//! Local folders (0.18, `docs/specs/local-folders.md`): the folders on this PC
//! the user lets the bot see, and the code that enforces that allowlist.
//!
//! The allowlist lives only on this device (`local_folders.json`). The server
//! relays tool calls here and never sees an absolute path. Every request is
//! checked again in this module, whatever the server says: root id lookup,
//! relative-path rules, no `..`, no symlinks / junctions / reparse points on
//! the way, the result must stay inside the canonical root, writes need a
//! read-write root, and credential-like file names are refused.

use std::path::{Path, PathBuf};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::local_fs::Budget;
use crate::{ApiClient, ApiError};

/// Write ceiling per request, and read ceiling per call.
pub const MAX_FILE_BYTES: u64 = 256 * 1024;
/// Folders a user can allow.
pub const MAX_FOLDERS: usize = 16;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FolderMode {
    Read,
    ReadWrite,
}

impl FolderMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Read => "read",
            Self::ReadWrite => "read_write",
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            Self::Read => "Read only",
            Self::ReadWrite => "Read and write",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LocalFolder {
    /// Short slug the model sees. Never the path.
    pub id: String,
    /// Absolute, canonical path. Stays on this device.
    pub path: String,
    pub mode: FolderMode,
}

/// What the server receives with each poll: no path.
#[derive(Serialize)]
struct RootView<'a> {
    id: &'a str,
    label: &'a str,
    mode: &'a str,
}

/// A request relayed by the server.
#[derive(Clone, Debug, Deserialize)]
pub struct LocalRequest {
    pub id: String,
    pub op: String,
    pub root: String,
    pub path: String,
    #[serde(default)]
    pub content: Option<String>,
    /// Operation parameters (ranges, filters, cursors). Clamped here again.
    #[serde(default)]
    pub args: serde_json::Value,
}

const FILE_NAME: &str = "local_folders.json";

pub fn load(dir: &Path) -> Vec<LocalFolder> {
    let Ok(raw) = std::fs::read_to_string(dir.join(FILE_NAME)) else {
        return Vec::new();
    };
    let mut folders: Vec<LocalFolder> = serde_json::from_str(&raw).unwrap_or_default();
    folders.retain(|folder| valid_id(&folder.id));
    folders.truncate(MAX_FOLDERS);
    folders
}

pub fn save(dir: &Path, folders: &[LocalFolder]) -> Result<(), String> {
    std::fs::create_dir_all(dir).map_err(|error| error.to_string())?;
    let json = serde_json::to_string_pretty(folders).map_err(|error| error.to_string())?;
    std::fs::write(dir.join(FILE_NAME), json).map_err(|error| error.to_string())
}

pub fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 32
        && id
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '-' | '_'))
}

fn slug(name: &str) -> String {
    let mut out: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect();
    out = out.trim_matches('-').chars().take(24).collect();
    if out.is_empty() { "folder".into() } else { out }
}

/// Add a folder to the allowlist. `app_dir` (the SeraBot settings directory)
/// and the user's home directory itself can never be allowed.
pub fn add_folder(
    folders: &mut Vec<LocalFolder>,
    path: &Path,
    mode: FolderMode,
    app_dir: Option<&Path>,
) -> Result<String, String> {
    if folders.len() >= MAX_FOLDERS {
        return Err(format!("You can allow at most {MAX_FOLDERS} folders."));
    }
    let canonical = path
        .canonicalize()
        .map_err(|_| "That folder could not be opened.".to_string())?;
    if !canonical.is_dir() {
        return Err("Pick a folder, not a file.".into());
    }
    if canonical.parent().is_none() {
        return Err("A whole drive cannot be shared. Pick a folder.".into());
    }
    if let Some(home) = std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME"))
        && let Ok(home) = Path::new(&home).canonicalize()
        && canonical == home
    {
        return Err("Your whole user folder cannot be shared. Pick a folder inside it.".into());
    }
    if let Some(app_dir) = app_dir
        && let Ok(app_dir) = app_dir.canonicalize()
        && (canonical.starts_with(&app_dir) || app_dir.starts_with(&canonical))
    {
        return Err("That folder holds SeraBot's own settings and cannot be shared.".into());
    }
    let text = canonical.to_string_lossy().into_owned();
    if folders.iter().any(|folder| folder.path == text) {
        return Err("That folder is already on the list.".into());
    }
    let base = slug(
        canonical
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("folder"),
    );
    let mut id = base.clone();
    let mut counter = 2;
    while folders.iter().any(|folder| folder.id == id) {
        id = format!("{}-{counter}", base.chars().take(28).collect::<String>());
        counter += 1;
    }
    folders.push(LocalFolder {
        id: id.clone(),
        path: text,
        mode,
    });
    Ok(id)
}

// ---------------------------------------------------------------------------
// Enforcement
// ---------------------------------------------------------------------------

const RESERVED: [&str; 22] = [
    "con", "prn", "aux", "nul", "com1", "com2", "com3", "com4", "com5", "com6", "com7", "com8",
    "com9", "lpt1", "lpt2", "lpt3", "lpt4", "lpt5", "lpt6", "lpt7", "lpt8", "lpt9",
];

const BLOCKED_WRITE_EXTENSIONS: [&str; 14] = [
    "exe", "dll", "bat", "cmd", "com", "msi", "scr", "ps1", "vbs", "lnk", "jar", "reg", "hta",
    "sys",
];

/// Credential-like names are never read, written or listed.
pub fn sensitive_name(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    lower == ".env"
        || lower.starts_with(".env.")
        || lower.starts_with("id_rsa")
        || lower.starts_with("id_ed25519")
        || lower.starts_with("id_ecdsa")
        || lower.starts_with("credentials")
        || lower == ".netrc"
        || lower == ".git-credentials"
        || [".pem", ".key", ".pfx", ".p12", ".kdbx", ".ppk"]
            .iter()
            .any(|ext| lower.ends_with(ext))
}

fn reserved_device(component: &str) -> bool {
    let stem = component
        .split('.')
        .next()
        .unwrap_or_default()
        .to_ascii_lowercase();
    RESERVED.contains(&stem.as_str())
}

/// Symlink, junction or any other reparse point.
pub(crate) fn is_link(meta: &std::fs::Metadata) -> bool {
    if meta.file_type().is_symlink() {
        return true;
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
        if meta.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            return true;
        }
    }
    false
}

/// Split and vet the relative path the server sent. `.` alone means the root.
fn components(rel: &str) -> Result<Vec<String>, String> {
    if rel.is_empty() || rel.len() > 512 || rel.contains('\0') || rel.contains(':') {
        return Err("invalid path".into());
    }
    if rel.starts_with('/') || rel.starts_with('\\') {
        return Err("absolute paths are not allowed".into());
    }
    let mut out = Vec::new();
    for part in rel.split(['/', '\\']) {
        match part {
            "." => continue,
            "" => return Err("empty path component".into()),
            ".." => return Err("'..' is not allowed".into()),
            _ => {}
        }
        if part.ends_with('.') || part.ends_with(' ') {
            return Err("invalid path component".into());
        }
        if reserved_device(part) {
            return Err("reserved device name".into());
        }
        if part
            .chars()
            .any(|c| c.is_control() || matches!(c, '<' | '>' | '"' | '|' | '?' | '*'))
        {
            return Err("invalid character in path".into());
        }
        out.push(part.to_owned());
    }
    Ok(out)
}

/// Resolve `rel` under `root` without ever leaving it. Every component that
/// exists is checked to not be a link; the canonical result must start with
/// the canonical root. `must_exist = false` allows a missing final component
/// (a file about to be created).
pub(crate) fn resolve(root: &Path, rel: &str, must_exist: bool) -> Result<PathBuf, String> {
    let root = root
        .canonicalize()
        .map_err(|_| "the allowed folder is no longer available".to_string())?;
    let parts = components(rel)?;
    let mut current = root.clone();
    for (index, part) in parts.iter().enumerate() {
        current.push(part);
        let last = index + 1 == parts.len();
        match std::fs::symlink_metadata(&current) {
            Ok(meta) => {
                if is_link(&meta) {
                    return Err("links and junctions are not followed".into());
                }
                if !last && !meta.is_dir() {
                    return Err("not a folder".into());
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                if must_exist || !last {
                    return Err("not found".into());
                }
            }
            Err(_) => return Err("could not be opened".into()),
        }
    }
    // Defence in depth: canonicalise what exists and re-check containment.
    let check = if current.exists() {
        current
            .canonicalize()
            .map_err(|_| "could not be opened".to_string())?
    } else {
        current
            .parent()
            .ok_or_else(|| "invalid path".to_string())?
            .canonicalize()
            .map_err(|_| "could not be opened".to_string())?
    };
    if !crate::local_fs::path_within(&root, &check) {
        return Err("the path leaves the allowed folder".into());
    }
    Ok(current)
}

fn file_name_of(path: &Path) -> String {
    path.file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_default()
        .to_owned()
}

/// Time one relayed request may run before the agent answers without it.
const REQUEST_LIMIT: Duration = Duration::from_secs(22);

/// Run one relayed request against the local allowlist.
pub fn handle(folders: &[LocalFolder], request: &LocalRequest) -> Result<String, String> {
    handle_with(folders, request, &Budget::new(crate::local_fs::WALK_BUDGET))
}

pub fn handle_with(
    folders: &[LocalFolder],
    request: &LocalRequest,
    budget: &Budget,
) -> Result<String, String> {
    let folder = folders
        .iter()
        .find(|folder| folder.id == request.root)
        .ok_or_else(|| "that folder is not on the allowed list".to_string())?;
    let root = Path::new(&folder.path);
    let args = if request.args.is_object() {
        request.args.clone()
    } else {
        serde_json::json!({})
    };
    match request.op.as_str() {
        "list" => crate::local_fs::list(root, &request.path, &args, budget),
        "search" => crate::local_fs::search(root, &request.path, &args, budget),
        "stat" => crate::local_fs::stat(root, &request.path),
        "read" => crate::local_fs::read(root, &request.path, &args, budget),
        "write" => {
            if folder.mode != FolderMode::ReadWrite {
                return Err("that folder is read-only".into());
            }
            write(
                root,
                &request.path,
                request.content.as_deref().unwrap_or_default(),
            )
        }
        _ => Err("unknown operation".into()),
    }
}

/// Run a request on a worker thread so a slow disk or a huge tree can never
/// stall the long-poll loop. Past [`REQUEST_LIMIT`] the walk is cancelled and
/// the agent answers with an error.
fn handle_in_worker(folders: Vec<LocalFolder>, request: LocalRequest) -> Result<String, String> {
    let cancel = Arc::new(AtomicBool::new(false));
    let budget = Budget::with_cancel(crate::local_fs::WALK_BUDGET, cancel.clone());
    let (sender, receiver) = std::sync::mpsc::channel();
    std::thread::Builder::new()
        .name("serabot-local-worker".into())
        .spawn(move || {
            let _ = sender.send(handle_with(&folders, &request, &budget));
        })
        .map_err(|_| "could not start a worker".to_string())?;
    match receiver.recv_timeout(REQUEST_LIMIT) {
        Ok(outcome) => outcome,
        Err(_) => {
            cancel.store(true, Ordering::SeqCst);
            Err("the folder took too long to read; try a smaller path or add filters".into())
        }
    }
}
fn write(root: &Path, rel: &str, content: &str) -> Result<String, String> {
    if content.len() as u64 > MAX_FILE_BYTES {
        return Err("content is larger than 256 KiB".into());
    }
    let path = resolve(root, rel, false)?;
    let name = file_name_of(&path);
    if sensitive_name(&name) {
        return Err("credential-like file names are never written".into());
    }
    let extension = Path::new(&name)
        .extension()
        .and_then(|ext| ext.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    if BLOCKED_WRITE_EXTENSIONS.contains(&extension.as_str()) {
        return Err("program and script files are never written".into());
    }
    if path == root.canonicalize().unwrap_or_default() {
        return Err("invalid path".into());
    }
    if let Ok(meta) = std::fs::symlink_metadata(&path)
        && (!meta.is_file() || is_link(&meta))
    {
        return Err("the target is not a regular file".into());
    }
    let parent = path
        .parent()
        .ok_or_else(|| "invalid path".to_string())?
        .to_path_buf();
    // Write next to the target, then move into place, so a half-written file
    // never replaces the original.
    let temp = parent.join(format!(".{name}.serabot-tmp"));
    std::fs::write(&temp, content).map_err(|_| "could not be written".to_string())?;
    if let Err(error) = std::fs::rename(&temp, &path) {
        let _ = std::fs::remove_file(&temp);
        return Err(format!("could not be written: {}", error.kind()));
    }
    Ok(format!("Wrote {} bytes to {rel}", content.len()))
}

// ---------------------------------------------------------------------------
// Agent: long-polls the server and answers relayed requests.
// ---------------------------------------------------------------------------

/// Stops its thread when dropped. The allowlist is shared with the thread, so
/// removing a folder takes effect on the next request, not the next poll.
pub struct LocalAgent {
    stop: Arc<AtomicBool>,
    folders: Arc<std::sync::RwLock<Vec<LocalFolder>>>,
}

impl Drop for LocalAgent {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
    }
}

impl LocalAgent {
    /// Start polling for these folders. Nothing runs for an empty list.
    pub fn start(client: ApiClient, folders: Vec<LocalFolder>) -> Option<Self> {
        if folders.is_empty() {
            return None;
        }
        let stop = Arc::new(AtomicBool::new(false));
        let shared = Arc::new(std::sync::RwLock::new(folders));
        let flag = stop.clone();
        let list = shared.clone();
        std::thread::Builder::new()
            .name("serabot-local-folders".into())
            .spawn(move || {
                let mut backoff = Duration::from_secs(2);
                while !flag.load(Ordering::SeqCst) {
                    let current = list.read().map(|guard| guard.clone()).unwrap_or_default();
                    let views: Vec<(String, String, &'static str)> = current
                        .iter()
                        .map(|folder| {
                            let label = Path::new(&folder.path)
                                .file_name()
                                .and_then(|name| name.to_str())
                                .unwrap_or("folder")
                                .to_owned();
                            (folder.id.clone(), label, folder.mode.as_str())
                        })
                        .collect();
                    let roots: Vec<RootView<'_>> = views
                        .iter()
                        .map(|(id, label, mode)| RootView { id, label, mode })
                        .collect();
                    match client.local_poll(&roots_json(&roots), 12) {
                        Ok(Some(request)) => {
                            backoff = Duration::from_secs(2);
                            // Re-read the allowlist: a folder removed while the
                            // poll was open must not be served.
                            let live = list.read().map(|guard| guard.clone()).unwrap_or_default();
                            let outcome = if flag.load(Ordering::SeqCst) || live.is_empty() {
                                Err("local folders are switched off on this PC".to_string())
                            } else {
                                handle_in_worker(live, request.clone())
                            };
                            let _ = client.local_result(&request.id, outcome);
                        }
                        Ok(None) => backoff = Duration::from_secs(2),
                        Err(ApiError::Http(401)) | Err(ApiError::Http(403)) => {
                            // Signed out: wait for the session to restart us.
                            std::thread::sleep(Duration::from_secs(30));
                        }
                        Err(_) => {
                            std::thread::sleep(backoff);
                            backoff = (backoff * 2).min(Duration::from_secs(30));
                        }
                    }
                }
            })
            .ok()?;
        Some(Self {
            stop,
            folders: shared,
        })
    }

    /// Replace the allowlist the running thread serves.
    pub fn update(&self, folders: Vec<LocalFolder>) {
        if let Ok(mut guard) = self.folders.write() {
            *guard = folders;
        }
    }
}
fn roots_json(roots: &[RootView<'_>]) -> serde_json::Value {
    serde_json::to_value(roots).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> (PathBuf, Vec<LocalFolder>) {
        let base = std::env::temp_dir().join(format!("serabot-lf-{}", uuid::Uuid::new_v4()));
        let root = base.join("docs");
        std::fs::create_dir_all(root.join("sub")).unwrap();
        std::fs::write(root.join("a.txt"), "hello").unwrap();
        std::fs::write(root.join("sub").join("b.md"), "# b").unwrap();
        std::fs::write(root.join(".env"), "SECRET=synthetic").unwrap();
        std::fs::write(root.join("bin.dat"), [0u8, 1, 2]).unwrap();
        std::fs::write(base.join("outside.txt"), "outside").unwrap();
        let folders = vec![
            LocalFolder {
                id: "docs".into(),
                path: root.canonicalize().unwrap().to_string_lossy().into_owned(),
                mode: FolderMode::ReadWrite,
            },
            LocalFolder {
                id: "ro".into(),
                path: root.canonicalize().unwrap().to_string_lossy().into_owned(),
                mode: FolderMode::Read,
            },
        ];
        (base, folders)
    }

    fn request(op: &str, root: &str, path: &str, content: Option<&str>) -> LocalRequest {
        LocalRequest {
            id: "r".into(),
            op: op.into(),
            root: root.into(),
            path: path.into(),
            content: content.map(str::to_owned),
            args: serde_json::Value::Null,
        }
    }

    #[test]
    fn lists_reads_and_writes_inside_the_root() {
        let (base, folders) = fixture();
        let listing = handle(&folders, &request("list", "docs", ".", None)).unwrap();
        assert!(listing.contains("a.txt (5 bytes") && listing.contains("sub/"));
        // Credential-like files are not even listed.
        assert!(!listing.contains(".env"));
        assert_eq!(
            handle(&folders, &request("read", "docs", "sub/b.md", None)).unwrap(),
            "# b"
        );
        handle(
            &folders,
            &request("write", "docs", "new.txt", Some("written")),
        )
        .unwrap();
        assert_eq!(
            std::fs::read_to_string(Path::new(&folders[0].path).join("new.txt")).unwrap(),
            "written"
        );
        // Overwrite keeps no temp file around.
        handle(&folders, &request("write", "docs", "new.txt", Some("v2"))).unwrap();
        assert!(
            !Path::new(&folders[0].path)
                .join(".new.txt.serabot-tmp")
                .exists()
        );
        std::fs::remove_dir_all(base).ok();
    }

    #[test]
    fn traversal_absolute_and_odd_paths_are_refused() {
        let (base, folders) = fixture();
        for bad in [
            "../outside.txt",
            "sub/../../outside.txt",
            "..\\outside.txt",
            "/etc/passwd",
            "\\\\server\\share\\x",
            "C:\\Windows\\win.ini",
            "a.txt:stream",
            "sub//b.md",
            "CON",
            "nul.txt",
            "a.txt.",
            "",
        ] {
            assert!(
                handle(&folders, &request("read", "docs", bad, None)).is_err(),
                "{bad:?} should be refused"
            );
        }
        // The file outside the root was never returned.
        assert!(handle(&folders, &request("read", "docs", "../outside.txt", None)).is_err());
        std::fs::remove_dir_all(base).ok();
    }

    #[test]
    fn modes_unknown_roots_and_sensitive_files_are_enforced() {
        let (base, folders) = fixture();
        assert_eq!(
            handle(&folders, &request("write", "ro", "x.txt", Some("x"))).unwrap_err(),
            "that folder is read-only"
        );
        assert!(!Path::new(&folders[0].path).join("x.txt").exists());
        assert!(handle(&folders, &request("read", "nope", "a.txt", None)).is_err());
        assert!(handle(&folders, &request("read", "docs", ".env", None)).is_err());
        assert!(handle(&folders, &request("write", "docs", ".env", Some("x"))).is_err());
        assert!(handle(&folders, &request("write", "docs", "id_rsa", Some("x"))).is_err());
        assert!(handle(&folders, &request("write", "docs", "run.bat", Some("x"))).is_err());
        assert!(
            handle(
                &folders,
                &request("write", "docs", "ghost/new.txt", Some("x"))
            )
            .is_err()
        );
        assert!(handle(&folders, &request("read", "docs", "bin.dat", None)).is_err());
        let big = "a".repeat(MAX_FILE_BYTES as usize + 1);
        assert!(handle(&folders, &request("write", "docs", "big.txt", Some(&big))).is_err());
        assert!(handle(&folders, &request("delete", "docs", "a.txt", None)).is_err());
        std::fs::remove_dir_all(base).ok();
    }

    /// A junction (or symlink) inside the root that points outside it must
    /// not be followed for reads, writes or listings.
    #[cfg(windows)]
    #[test]
    fn junctions_pointing_out_of_the_root_are_not_followed() {
        let (base, folders) = fixture();
        let link = Path::new(&folders[0].path).join("escape");
        let target = base.join("outside_dir");
        std::fs::create_dir_all(&target).unwrap();
        std::fs::write(target.join("secret.txt"), "outside secret").unwrap();
        let status = std::process::Command::new("cmd")
            .args(["/C", "mklink", "/J"])
            .arg(&link)
            .arg(&target)
            .output();
        let created = status.map(|out| out.status.success()).unwrap_or(false);
        if !created {
            // Junction creation is unavailable on this host; nothing to test.
            std::fs::remove_dir_all(base).ok();
            return;
        }
        assert!(
            handle(
                &folders,
                &request("read", "docs", "escape/secret.txt", None)
            )
            .is_err()
        );
        assert!(handle(&folders, &request("list", "docs", "escape", None)).is_err());
        assert!(
            handle(
                &folders,
                &request("write", "docs", "escape/new.txt", Some("x"))
            )
            .is_err()
        );
        assert!(!target.join("new.txt").exists());
        // The junction itself is hidden from listings.
        let listing = handle(&folders, &request("list", "docs", ".", None)).unwrap();
        assert!(!listing.contains("escape"));
        let _ = std::process::Command::new("cmd")
            .args(["/C", "rmdir"])
            .arg(&link)
            .output();
        std::fs::remove_dir_all(base).ok();
    }

    #[test]
    fn add_folder_rejects_roots_apps_and_duplicates() {
        let (base, _) = fixture();
        let mut list = Vec::new();
        let docs = base.join("docs");
        let id = add_folder(&mut list, &docs, FolderMode::Read, None).unwrap();
        assert_eq!(id, "docs");
        assert!(add_folder(&mut list, &docs, FolderMode::Read, None).is_err());
        assert!(add_folder(&mut list, &docs.join("a.txt"), FolderMode::Read, None).is_err());
        assert!(add_folder(&mut list, &base.join("missing"), FolderMode::Read, None).is_err());
        // The app settings directory (and anything containing it) is refused.
        assert!(add_folder(&mut list, &base, FolderMode::Read, Some(&docs.join("sub"))).is_err());
        // A drive root is refused.
        let root = base.ancestors().last().unwrap().to_path_buf();
        assert!(add_folder(&mut list, &root, FolderMode::Read, None).is_err());
        // Same folder name gets a distinct id.
        let other = base.join("x").join("docs");
        std::fs::create_dir_all(&other).unwrap();
        assert_eq!(
            add_folder(&mut list, &other, FolderMode::ReadWrite, None).unwrap(),
            "docs-2"
        );
        std::fs::remove_dir_all(base).ok();
    }

    #[test]
    fn allowlist_round_trips_on_disk() {
        let (base, folders) = fixture();
        save(&base, &folders).unwrap();
        assert_eq!(load(&base), folders);
        std::fs::remove_dir_all(base).ok();
    }

    #[test]
    fn components_helper_flags_dot_and_traversal() {
        assert_eq!(components("./a/b").unwrap(), vec!["a", "b"]);
        assert!(components("a/./../b").is_err());
    }
}
