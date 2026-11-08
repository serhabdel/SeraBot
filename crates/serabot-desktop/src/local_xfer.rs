//! Moving whole files between an allowed folder and the server (0.21).
//!
//! `local_import` pulls a file from an allowed folder in 1 MiB chunks so the
//! server's document and image tools can work on it; `local_export` pushes a
//! produced artifact back into a read-write folder. Both go through the same
//! path rules as every other request ([`crate::local_folders::resolve`]):
//! relative path under a canonical root, no links, no credential-like names.
//! Exports are staged next to the target, checked against the SHA-256 the
//! approved request named, and only then moved into place.

use std::io::{Read, Seek, SeekFrom, Write};
use std::path::Path;

use base64::Engine as _;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::local_folders::{is_link, resolve, sensitive_name};

/// Largest file moved in either direction.
pub const MAX_TRANSFER_BYTES: u64 = 50 * 1024 * 1024;
/// Raw bytes per chunk.
pub const CHUNK_BYTES: usize = 1024 * 1024;

/// Types the server's document and image tools understand.
pub const IMPORT_EXTENSIONS: [&str; 11] = [
    "docx", "xlsx", "pptx", "pdf", "png", "jpg", "jpeg", "webp", "txt", "md", "csv",
];

const BLOCKED_EXPORT_EXTENSIONS: [&str; 14] = [
    "exe", "dll", "bat", "cmd", "com", "msi", "scr", "ps1", "vbs", "lnk", "jar", "reg", "hta",
    "sys",
];

fn extension_of(name: &str) -> String {
    Path::new(name)
        .extension()
        .and_then(|ext| ext.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase()
}

fn name_of(path: &Path) -> String {
    path.file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_default()
        .to_owned()
}

fn number(args: &Value, key: &str) -> Option<u64> {
    args.get(key).and_then(Value::as_u64)
}

/// One chunk of a file, as JSON text: size and modified time so the server can
/// notice a file that changed mid-transfer.
pub fn read_chunk(root: &Path, rel: &str, args: &Value) -> Result<String, String> {
    let path = resolve(root, rel, true)?;
    let name = name_of(&path);
    if sensitive_name(&name) {
        return Err("credential-like files are never shared".into());
    }
    if !IMPORT_EXTENSIONS.contains(&extension_of(&name).as_str()) {
        return Err(format!(
            "only {} files can be imported",
            IMPORT_EXTENSIONS.join(", ")
        ));
    }
    let meta = std::fs::symlink_metadata(&path).map_err(|_| "could not be opened".to_string())?;
    if !meta.is_file() || is_link(&meta) {
        return Err("not a regular file".into());
    }
    let size = meta.len();
    if size == 0 {
        return Err("the file is empty".into());
    }
    if size > MAX_TRANSFER_BYTES {
        return Err(format!(
            "the file is {size} bytes; the import limit is {MAX_TRANSFER_BYTES} bytes (50 MB)"
        ));
    }
    let offset = number(args, "offset").unwrap_or(0);
    if offset >= size {
        return Err("offset is past the end of the file".into());
    }
    let length = number(args, "length")
        .unwrap_or(CHUNK_BYTES as u64)
        .clamp(1, CHUNK_BYTES as u64)
        .min(size - offset);
    let mut file = std::fs::File::open(&path).map_err(|_| "could not be opened".to_string())?;
    file.seek(SeekFrom::Start(offset))
        .map_err(|_| "could not be read".to_string())?;
    let mut buf = vec![0u8; length as usize];
    file.read_exact(&mut buf)
        .map_err(|_| "the file changed while it was read".to_string())?;
    let modified = meta
        .modified()
        .ok()
        .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_secs());
    Ok(json!({
        "size": size,
        "modified": modified,
        "offset": offset,
        "len": length,
        "data": base64::engine::general_purpose::STANDARD.encode(&buf),
    })
    .to_string())
}

fn hex_sha256(path: &Path) -> Result<String, String> {
    let mut file = std::fs::File::open(path).map_err(|_| "could not be read".to_string())?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let read = file
            .read(&mut buf)
            .map_err(|_| "could not be read".to_string())?;
        if read == 0 {
            break;
        }
        hasher.update(&buf[..read]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

/// Receive one chunk of an exported file. The last chunk verifies the whole
/// file against the approved SHA-256 and moves it into place.
pub fn put_chunk(root: &Path, rel: &str, content: &str, args: &Value) -> Result<String, String> {
    let path = resolve(root, rel, false)?;
    let name = name_of(&path);
    if sensitive_name(&name) {
        return Err("credential-like file names are never written".into());
    }
    if BLOCKED_EXPORT_EXTENSIONS.contains(&extension_of(&name).as_str()) {
        return Err("program and script files are never written".into());
    }
    if path == root.canonicalize().unwrap_or_default() {
        return Err("invalid path".into());
    }
    let existed = match std::fs::symlink_metadata(&path) {
        Ok(meta) if !meta.is_file() || is_link(&meta) => {
            return Err("the target is not a regular file".into());
        }
        Ok(_) => true,
        Err(_) => false,
    };
    let overwrite = args
        .get("overwrite")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if existed && !overwrite {
        return Err(format!(
            "{rel} already exists; ask the user, then repeat the export with overwrite=true"
        ));
    }
    let total = number(args, "total").ok_or("total is required")?;
    let offset = number(args, "offset").ok_or("offset is required")?;
    let expected = args
        .get("sha256")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_ascii_lowercase();
    if total == 0 || total > MAX_TRANSFER_BYTES {
        return Err("the file is empty or larger than the 50 MB export limit".into());
    }
    if expected.len() != 64 || !expected.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err("a SHA-256 of the whole file is required".into());
    }
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(content)
        .map_err(|_| "invalid chunk".to_string())?;
    if bytes.is_empty() || bytes.len() > CHUNK_BYTES || offset + bytes.len() as u64 > total {
        return Err("chunk does not fit the declared size".into());
    }
    let parent = path
        .parent()
        .ok_or_else(|| "invalid path".to_string())?
        .to_path_buf();
    let temp = parent.join(format!(".{name}.serabot-xfer"));
    if offset == 0 {
        let _ = std::fs::remove_file(&temp);
    } else {
        let staged = std::fs::metadata(&temp).map_err(|_| "no transfer in progress".to_string())?;
        if staged.len() != offset {
            let _ = std::fs::remove_file(&temp);
            return Err("transfer out of sequence".into());
        }
    }
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&temp)
        .map_err(|_| "could not be written".to_string())?;
    file.write_all(&bytes)
        .and_then(|()| file.flush())
        .map_err(|_| {
            let _ = std::fs::remove_file(&temp);
            "could not be written".to_string()
        })?;
    drop(file);
    let received = offset + bytes.len() as u64;
    if received < total {
        return Ok(format!("received {received} of {total} bytes"));
    }
    let actual = hex_sha256(&temp)?;
    if actual != expected {
        let _ = std::fs::remove_file(&temp);
        return Err("the file did not match the approved SHA-256; nothing was saved".into());
    }
    // Re-check just before the move: the target must still be a plain file.
    if let Ok(meta) = std::fs::symlink_metadata(&path)
        && (!meta.is_file() || is_link(&meta))
    {
        let _ = std::fs::remove_file(&temp);
        return Err("the target is not a regular file".into());
    }
    if let Err(error) = std::fs::rename(&temp, &path) {
        let _ = std::fs::remove_file(&temp);
        return Err(format!("could not be written: {}", error.kind()));
    }
    Ok(format!("Saved {total} bytes to {rel} (sha256 {actual})"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir() -> std::path::PathBuf {
        let base = std::env::temp_dir().join(format!("serabot-xfer-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&base).unwrap();
        base.canonicalize().unwrap()
    }

    fn sha(bytes: &[u8]) -> String {
        format!("{:x}", Sha256::digest(bytes))
    }

    fn b64(bytes: &[u8]) -> String {
        base64::engine::general_purpose::STANDARD.encode(bytes)
    }

    #[test]
    fn import_reads_a_big_file_in_consistent_chunks() {
        let base = dir();
        let data: Vec<u8> = (0..(2 * CHUNK_BYTES + 123))
            .map(|i| (i % 251) as u8)
            .collect();
        std::fs::write(base.join("big.pdf"), &data).unwrap();
        let mut offset = 0u64;
        let mut got = Vec::new();
        loop {
            let chunk = read_chunk(&base, "big.pdf", &json!({"offset": offset})).unwrap();
            let value: Value = serde_json::from_str(&chunk).unwrap();
            assert_eq!(value["size"], data.len() as u64);
            let part = base64::engine::general_purpose::STANDARD
                .decode(value["data"].as_str().unwrap())
                .unwrap();
            offset += part.len() as u64;
            got.extend(part);
            if offset >= data.len() as u64 {
                break;
            }
        }
        assert_eq!(got, data);
        assert!(read_chunk(&base, "big.pdf", &json!({"offset": data.len()})).is_err());
        std::fs::remove_dir_all(&base).ok();
    }

    #[test]
    fn import_refuses_other_types_secrets_empty_and_oversized_files() {
        let base = dir();
        std::fs::write(base.join("run.exe"), b"MZ").unwrap();
        std::fs::write(base.join(".env"), b"S=1").unwrap();
        std::fs::write(base.join("secret.pem"), b"k").unwrap();
        std::fs::write(base.join("empty.docx"), b"").unwrap();
        let huge = std::fs::File::create(base.join("huge.pdf")).unwrap();
        huge.set_len(MAX_TRANSFER_BYTES + 1).unwrap();
        drop(huge);
        for bad in [
            "run.exe",
            ".env",
            "secret.pem",
            "empty.docx",
            "huge.pdf",
            "../x.pdf",
            "missing.pdf",
            "a.pdf:s",
        ] {
            assert!(read_chunk(&base, bad, &json!({})).is_err(), "{bad}");
        }
        std::fs::remove_dir_all(&base).ok();
    }

    fn export(base: &Path, rel: &str, data: &[u8], overwrite: bool) -> Result<String, String> {
        let digest = sha(data);
        let mut last = Ok(String::new());
        let mut offset = 0usize;
        for part in data.chunks(CHUNK_BYTES) {
            last = put_chunk(
                base,
                rel,
                &b64(part),
                &json!({"offset": offset, "total": data.len(), "sha256": digest, "overwrite": overwrite}),
            );
            last.as_ref().map_err(|e| e.clone())?;
            offset += part.len();
        }
        last
    }

    #[test]
    fn export_stages_verifies_and_moves_into_place() {
        let base = dir();
        let data: Vec<u8> = (0..(CHUNK_BYTES + 77)).map(|i| (i % 200) as u8).collect();
        let done = export(&base, "out.pdf", &data, false).unwrap();
        assert!(done.contains(&sha(&data)), "{done}");
        assert_eq!(std::fs::read(base.join("out.pdf")).unwrap(), data);
        assert!(!base.join(".out.pdf.serabot-xfer").exists());
        // Existing files need overwrite=true, then are replaced atomically.
        assert!(
            export(&base, "out.pdf", b"new", false)
                .unwrap_err()
                .contains("already exists")
        );
        assert_eq!(std::fs::read(base.join("out.pdf")).unwrap(), data);
        export(&base, "out.pdf", b"new", true).unwrap();
        assert_eq!(std::fs::read(base.join("out.pdf")).unwrap(), b"new");
        std::fs::remove_dir_all(&base).ok();
    }

    #[test]
    fn export_rejects_a_hash_mismatch_bad_paths_and_out_of_order_chunks() {
        let base = dir();
        let wrong = put_chunk(
            &base,
            "x.pdf",
            &b64(b"data"),
            &json!({"offset": 0, "total": 4, "sha256": sha(b"other")}),
        )
        .unwrap_err();
        assert!(wrong.contains("did not match"), "{wrong}");
        assert!(!base.join("x.pdf").exists() && !base.join(".x.pdf.serabot-xfer").exists());
        let args = json!({"offset": 0, "total": 4, "sha256": sha(b"data")});
        for bad in [
            "../x.pdf",
            "/x.pdf",
            "a.exe",
            ".env",
            "id_rsa",
            "ghost/x.pdf",
            "con.pdf",
            "x.pdf:s",
            ".",
        ] {
            assert!(
                put_chunk(&base, bad, &b64(b"data"), &args).is_err(),
                "{bad}"
            );
        }
        // Skipping a chunk, oversize totals, bad base64 and missing hash.
        assert!(
            put_chunk(
                &base,
                "y.pdf",
                &b64(b"da"),
                &json!({"offset": 2, "total": 4, "sha256": sha(b"data")})
            )
            .is_err()
        );
        assert!(
            put_chunk(
                &base,
                "y.pdf",
                &b64(b"d"),
                &json!({"offset": 0, "total": MAX_TRANSFER_BYTES + 1, "sha256": sha(b"d")})
            )
            .is_err()
        );
        assert!(put_chunk(&base, "y.pdf", "***", &args).is_err());
        assert!(
            put_chunk(
                &base,
                "y.pdf",
                &b64(b"data"),
                &json!({"offset": 0, "total": 4})
            )
            .is_err()
        );
        assert!(
            put_chunk(
                &base,
                "y.pdf",
                &b64(b"data"),
                &json!({"offset": 0, "total": 3, "sha256": sha(b"data")})
            )
            .is_err()
        );
        std::fs::remove_dir_all(&base).ok();
    }

    #[cfg(unix)]
    #[test]
    fn export_never_writes_through_a_link() {
        let base = dir();
        let root = base.join("root");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(base.join("victim.pdf"), b"keep").unwrap();
        std::os::unix::fs::symlink(base.join("victim.pdf"), root.join("l.pdf")).unwrap();
        let args = json!({"offset": 0, "total": 4, "sha256": sha(b"evil"), "overwrite": true});
        assert!(put_chunk(&root, "l.pdf", &b64(b"evil"), &args).is_err());
        assert_eq!(std::fs::read(base.join("victim.pdf")).unwrap(), b"keep");
        std::fs::remove_dir_all(&base).ok();
    }

    #[cfg(windows)]
    #[test]
    fn export_never_writes_through_a_junction() {
        let base = dir();
        let root = base.join("root");
        let outside = base.join("outside");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        let made = std::process::Command::new("cmd")
            .args(["/C", "mklink", "/J"])
            .arg(root.join("jn"))
            .arg(&outside)
            .output()
            .map(|out| out.status.success())
            .unwrap_or(false);
        if made {
            let args = json!({"offset": 0, "total": 4, "sha256": sha(b"evil")});
            assert!(put_chunk(&root, "jn/x.pdf", &b64(b"evil"), &args).is_err());
            assert!(!outside.join("x.pdf").exists());
            assert!(read_chunk(&root, "jn/x.pdf", &json!({})).is_err());
            let _ = std::process::Command::new("cmd")
                .args(["/C", "rmdir"])
                .arg(root.join("jn"))
                .output();
        }
        std::fs::remove_dir_all(&base).ok();
    }
}
