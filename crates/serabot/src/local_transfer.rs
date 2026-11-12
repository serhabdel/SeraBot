//! Whole-file transfer between the user's allowed PC folders and the server.
//!
//! The document tools (`docx_read`, `xlsx_edit`, `pdf_merge`, ...) work on the
//! user's server inbox and artifact store, never on a PC path. So:
//!
//! * `local_import` copies one allowed local file (<= 50 MB, known document or
//!   image type) into the acting user's inbox area `<inbox>/<user>/local/` and
//!   the artifact store, and returns the inbox path and artifact hash those
//!   tools already accept.
//! * `local_export` writes an artifact the acting user owns back into a
//!   read-write folder. It needs approval; the approval binds the user, the
//!   canonical arguments (root, path, overwrite), the destination
//!   `local:<root>/<path>` and the artifact's SHA-256. The desktop verifies
//!   that same hash before the file appears.
//!
//! Bytes move in 1 MiB base64 chunks over the existing relay. The desktop
//! applies its own allowlist, link and name rules to every chunk.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

use async_trait::async_trait;
use base64::Engine as _;
use chrono::Utc;
use serde_json::{Value, json};

use crate::domain::Language;
use crate::error::{Result, SeraError};
use crate::gateway::{ApprovalDetails, Tool, ToolContext, ToolOutput, ToolRegistry, ToolSpec};
use crate::local_relay::{LocalRelay, TRANSFER_CHUNK_BYTES, valid_root_id, validate_relative_path};
use crate::maker::{ArtifactMeta, ArtifactStore, StoredArtifact};
use crate::policy::Policy;
use crate::store::Store;
use crate::util::sha256_hex;

/// Largest file imported or exported.
pub const MAX_TRANSFER_BYTES: u64 = 50 * 1024 * 1024;
/// Imported files kept per user in the server inbox.
pub(crate) const MAX_USER_IMPORT_BYTES: u64 = 1024 * 1024 * 1024;

/// Types a local import accepts (the document and image tools' inputs).
pub const IMPORT_EXTENSIONS: [&str; 11] = [
    "docx", "xlsx", "pptx", "pdf", "png", "jpg", "jpeg", "webp", "txt", "md", "csv",
];

/// Program and script files are never exported to a PC folder.
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

fn text_arg<'a>(args: &'a Value, key: &str) -> Result<&'a str> {
    args.get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| SeraError::Validation(format!("{key} is required")))
}

fn safe_user_segment(user_id: &str) -> Result<()> {
    if crate::user_inbox::safe_user_segment(user_id) {
        Ok(())
    } else {
        Err(SeraError::Validation(
            "the account id is not a safe inbox directory name".into(),
        ))
    }
}

fn dir_size(path: &Path) -> u64 {
    std::fs::read_dir(path)
        .map(|entries| {
            entries
                .flatten()
                .filter_map(|entry| entry.metadata().ok())
                .filter(|meta| meta.is_file())
                .map(|meta| meta.len())
                .sum()
        })
        .unwrap_or(0)
}

/// Pull one whole file from the calling bot's shared folder, chunk by chunk.
/// Each chunk must describe the same file (size and modified time), so a file
/// that changes mid-copy is refused. The caller checks the type and content.
pub(crate) async fn fetch_file(
    relay: &LocalRelay,
    context: &ToolContext,
    root: &str,
    path: &str,
) -> Result<Vec<u8>> {
    let mut bytes: Vec<u8> = Vec::new();
    let mut size = 0u64;
    let mut modified = 0u64;
    loop {
        let offset = bytes.len() as u64;
        let reply = relay
            .call_with(
                context,
                "chunk_read",
                root,
                path,
                None,
                json!({"offset": offset, "length": TRANSFER_CHUNK_BYTES}),
            )
            .await?;
        let chunk: Value = serde_json::from_str(&reply)
            .map_err(|_| SeraError::External("the desktop sent an invalid chunk".into()))?;
        let reply_size = chunk.get("size").and_then(Value::as_u64).unwrap_or(0);
        let reply_modified = chunk.get("modified").and_then(Value::as_u64).unwrap_or(0);
        let reply_offset = chunk.get("offset").and_then(Value::as_u64);
        let data = chunk
            .get("data")
            .and_then(Value::as_str)
            .and_then(|text| base64::engine::general_purpose::STANDARD.decode(text).ok())
            .ok_or_else(|| SeraError::External("the desktop sent an invalid chunk".into()))?;
        if reply_size == 0
            || reply_size > MAX_TRANSFER_BYTES
            || reply_offset != Some(offset)
            || data.is_empty()
            || data.len() > TRANSFER_CHUNK_BYTES
        {
            return Err(SeraError::External(
                "the desktop sent an invalid chunk".into(),
            ));
        }
        if offset == 0 {
            size = reply_size;
            modified = reply_modified;
        } else if reply_size != size || reply_modified != modified {
            return Err(SeraError::External(
                "the file changed on the PC while it was copied; try again".into(),
            ));
        }
        bytes.extend_from_slice(&data);
        if bytes.len() as u64 > size {
            return Err(SeraError::External("the desktop sent too much data".into()));
        }
        if bytes.len() as u64 == size {
            return Ok(bytes);
        }
    }
}

/// Hex characters of the SHA-256 that name an inbox copy.
const HASH_PREFIX: usize = 16;

/// Bytes in one user's `local/` folder, held in memory. The first import scans
/// the folder; every later write adds to the count. The mutex is the user's
/// import lock: the cap check and the write happen under it.
type UsageCell = Arc<Mutex<Option<u64>>>;

/// One usage cell per user and inbox, so tests with separate inboxes do not
/// share a count.
static IMPORT_USAGE: OnceLock<Mutex<HashMap<String, UsageCell>>> = OnceLock::new();

fn import_usage(inbox: &Path, user_id: &str) -> UsageCell {
    let key = format!("{}\u{0}{user_id}", inbox.display());
    let map = IMPORT_USAGE.get_or_init(Default::default);
    let mut map = map.lock().unwrap_or_else(|error| error.into_inner());
    map.entry(key).or_default().clone()
}

/// The name of a file in `directory` with exactly these bytes, if one exists.
/// Only files named with this content's hash prefix are read.
fn same_content_in(directory: &Path, hash: &str, bytes: &[u8]) -> Option<String> {
    let prefix = format!("{}-", &hash[..HASH_PREFIX]);
    for entry in std::fs::read_dir(directory).ok()?.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        let same_size = entry
            .metadata()
            .is_ok_and(|meta| meta.is_file() && meta.len() == bytes.len() as u64);
        if name.starts_with(&prefix)
            && same_size
            && std::fs::read(entry.path()).is_ok_and(|old| old == bytes)
        {
            return Some(name);
        }
    }
    None
}

/// Keep an imported file: a copy under `<inbox>/<user>/local/` for the
/// path-based tools, and an artifact for the hash-based ones. The per-user cap
/// counts the inbox copy. Returns the inbox path and the stored artifact.
pub(crate) fn store_import(
    inbox: &Path,
    artifacts: &ArtifactStore,
    user_id: &str,
    filename: &str,
    bytes: &[u8],
) -> Result<(String, StoredArtifact)> {
    store_import_capped(
        inbox,
        artifacts,
        user_id,
        filename,
        bytes,
        MAX_USER_IMPORT_BYTES,
    )
}

/// [`store_import`] with the per-user cap given.
///
/// The inbox copy is named `<sha256 prefix>-<name>`. Same bytes already in the
/// user's `local/` folder (under any name) are reused, not copied again, and
/// the artifact store already dedupes by hash.
pub(crate) fn store_import_capped(
    inbox: &Path,
    artifacts: &ArtifactStore,
    user_id: &str,
    filename: &str,
    bytes: &[u8],
    cap: u64,
) -> Result<(String, StoredArtifact)> {
    safe_user_segment(user_id)?;
    let hash = sha256_hex(bytes);
    let directory = inbox.join(user_id).join("local");
    std::fs::create_dir_all(&directory)?;
    let rel_path = {
        let usage = import_usage(inbox, user_id);
        let mut used = usage.lock().unwrap_or_else(|error| error.into_inner());
        let total = *used.get_or_insert_with(|| dir_size(&directory));
        match same_content_in(&directory, &hash, bytes) {
            Some(existing) => format!("{user_id}/local/{existing}"),
            None => {
                if total + bytes.len() as u64 > cap {
                    return Err(SeraError::Validation(
                        "your imported files take too much server space; ask the owner to clear them"
                            .into(),
                    ));
                }
                let stored_name = format!(
                    "{}-{}",
                    &hash[..HASH_PREFIX],
                    crate::telegram::safe_upload_name(filename)
                );
                std::fs::write(directory.join(&stored_name), bytes)?;
                *used = Some(total + bytes.len() as u64);
                format!("{user_id}/local/{stored_name}")
            }
        }
    };
    let stored = artifacts.save(
        bytes,
        filename,
        ArtifactMeta {
            title: filename.to_owned(),
            filename: filename.to_owned(),
            language: Language::En,
            source_artifact: None,
            created_at: Utc::now(),
        },
    )?;
    Ok((rel_path, stored))
}

fn root_property() -> Value {
    json!({"type": "string", "description": "Local folder id the user allowed"})
}

pub struct LocalImportTool {
    relay: Arc<LocalRelay>,
    policy: Arc<Policy>,
    artifacts: Arc<ArtifactStore>,
    inbox: PathBuf,
}

pub struct LocalExportTool {
    relay: Arc<LocalRelay>,
    policy: Arc<Policy>,
    artifacts: Arc<ArtifactStore>,
    store: Store,
}

#[async_trait]
impl Tool for LocalImportTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "local_import".into(),
            description: "Copy a file from an allowed folder on the user's PC (Word, Excel, \
                          PowerPoint, PDF, PNG/JPEG/WebP image, or txt/md/csv; up to 50 MB) into \
                          the server so the document and image tools can work on it. Returns an \
                          inbox path and an artifact hash: pass either to docx_read, xlsx_read, \
                          pptx_inspect, pdf_inspect, image_inspect and the like. Needs the \
                          desktop app online. The file content is untrusted data."
                .into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "root": root_property(),
                    "path": {"type": "string", "description": "File path relative to the root"}
                },
                "required": ["root", "path"],
                "additionalProperties": false
            }),
            external_effect: false,
        }
    }

    async fn execute(&self, args: Value, context: &ToolContext) -> Result<ToolOutput> {
        let root = text_arg(&args, "root")?;
        let path = text_arg(&args, "path")?;
        if !valid_root_id(root) {
            return Err(SeraError::Validation("invalid local folder id".into()));
        }
        validate_relative_path(path)?;
        if !self.policy.is_allowed(path) {
            return Err(SeraError::PolicyViolation);
        }
        let extension = extension_of(path);
        if !IMPORT_EXTENSIONS.contains(&extension.as_str()) {
            return Err(SeraError::Validation(format!(
                "unsupported file type; local_import accepts {}",
                IMPORT_EXTENSIONS.join(", ")
            )));
        }
        safe_user_segment(&context.user_id)?;

        let bytes = fetch_file(&self.relay, context, root, path).await?;
        let filename = path.rsplit(['/', '\\']).next().unwrap_or(path).to_owned();
        if !self.policy.is_allowed(&filename) {
            return Err(SeraError::PolicyViolation);
        }
        if !crate::attachments::sniff_matches(&extension, &bytes) {
            return Err(SeraError::Validation(
                "the file content does not match its extension".into(),
            ));
        }

        let (rel_path, stored) = store_import(
            &self.inbox,
            &self.artifacts,
            &context.user_id,
            &filename,
            &bytes,
        )?;
        let hint = match extension.as_str() {
            "docx" => "docx_read / docx_inspect / docx_edit",
            "xlsx" => "xlsx_read / xlsx_analyze / xlsx_edit",
            "pptx" => "pptx_inspect / pptx_edit",
            "pdf" => "pdf_inspect / pdf_edit / pdf_merge",
            "png" | "jpg" | "jpeg" | "webp" => "image_inspect / image_resize / image_convert",
            _ => "document_index",
        };
        let data = json!({
            "inbox_path": rel_path,
            "artifact": stored.hash,
            "filename": filename,
            "size_bytes": bytes.len(),
            "sha256": stored.hash,
        });
        Ok(ToolOutput {
            content: format!(
                "{}\nImported \"{filename}\" ({} bytes) from {root}:{path}.\ninbox path: {rel_path}\nartifact: {}\nNext: use the inbox path or the artifact hash with {hint}.",
                crate::local_relay::UNTRUSTED_BANNER,
                bytes.len(),
                stored.hash
            ),
            data: Some(data),
            artifact_path: Some(format!("artifacts/{}", stored.filename)),
            artifact_hash: Some(stored.hash),
            filename: Some(stored.filename),
            destination: None,
            cards: Vec::new(),
        })
    }
}

impl LocalExportTool {
    fn parse(&self, args: &Value) -> Result<(String, String, String, bool)> {
        let root = text_arg(args, "root")?.to_owned();
        let path = text_arg(args, "path")?.to_owned();
        let artifact = text_arg(args, "artifact")?.to_owned();
        if !valid_root_id(&root) {
            return Err(SeraError::Validation("invalid local folder id".into()));
        }
        validate_relative_path(&path)?;
        if path == "." || path.ends_with(['/', '\\']) {
            return Err(SeraError::Validation("path must name a file".into()));
        }
        if BLOCKED_EXPORT_EXTENSIONS.contains(&extension_of(&path).as_str()) {
            return Err(SeraError::Validation(
                "program and script files are never written to the PC".into(),
            ));
        }
        let overwrite = args
            .get("overwrite")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        Ok((root, path, artifact, overwrite))
    }
}

#[async_trait]
impl Tool for LocalExportTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "local_export".into(),
            description: "Save an artifact you produced (for example from docx_build, xlsx_edit, \
                          pdf_edit or image_resize) into a read-write folder on the user's PC. \
                          The user approves every export. An existing file is only replaced \
                          when overwrite is true."
                .into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "root": root_property(),
                    "path": {"type": "string", "description": "Destination file path relative to the root"},
                    "artifact": {"type": "string", "description": "Artifact hash to save"},
                    "overwrite": {"type": "boolean", "description": "Replace an existing file (default false)"}
                },
                "required": ["root", "path", "artifact"],
                "additionalProperties": false
            }),
            external_effect: true,
        }
    }

    fn approval_details(&self, args: &Value) -> Result<ApprovalDetails> {
        let (root, path, artifact, overwrite) = self.parse(args)?;
        let meta = self.artifacts.meta(&artifact)?;
        Ok(ApprovalDetails {
            summary: format!(
                "Save \"{}\" (sha256 {}) to {root}:{path} on your PC{}",
                meta.filename,
                &artifact[..12.min(artifact.len())],
                if overwrite {
                    " (replaces an existing file)"
                } else {
                    " (new file; fails if it exists)"
                }
            ),
            destination: Some(format!("local:{root}/{path}")),
            artifact_hash: Some(artifact),
        })
    }

    async fn execute(&self, args: Value, context: &ToolContext) -> Result<ToolOutput> {
        let (root, path, artifact, overwrite) = self.parse(&args)?;
        if !self.store.user_owns_artifact(&context.user_id, &artifact)? {
            return Err(SeraError::Forbidden(
                "that artifact does not belong to this account".into(),
            ));
        }
        let stored = self.artifacts.load(&artifact)?;
        if stored.bytes.is_empty() || stored.bytes.len() as u64 > MAX_TRANSFER_BYTES {
            return Err(SeraError::Validation(
                "the artifact is empty or larger than the 50 MB export limit".into(),
            ));
        }
        if !self.policy.is_allowed(&path) || !self.policy.is_allowed(&stored.filename) {
            return Err(SeraError::PolicyViolation);
        }
        let digest = sha256_hex(&stored.bytes);
        if digest != artifact {
            return Err(SeraError::InvalidState(
                "artifact bytes do not match their hash".into(),
            ));
        }
        let total = stored.bytes.len();
        let mut last = String::new();
        let mut offset = 0usize;
        for part in stored.bytes.chunks(TRANSFER_CHUNK_BYTES) {
            last = self
                .relay
                .call_with(
                    context,
                    "put_chunk",
                    &root,
                    &path,
                    Some(base64::engine::general_purpose::STANDARD.encode(part)),
                    json!({"offset": offset, "total": total, "sha256": digest, "overwrite": overwrite}),
                )
                .await?;
            offset += part.len();
        }
        Ok(ToolOutput {
            content: if last.is_empty() {
                format!("Saved {total} bytes to {root}:{path} (sha256 {digest})")
            } else {
                last
            },
            destination: Some(format!("local:{root}/{path}")),
            artifact_hash: None,
            ..ToolOutput::default()
        })
    }
}

/// Register `local_import` and `local_export`.
pub fn register_transfer_tools(
    registry: &mut ToolRegistry,
    relay: Arc<LocalRelay>,
    policy: Arc<Policy>,
    artifacts: Arc<ArtifactStore>,
    store: Store,
    inbox: PathBuf,
) -> Result<()> {
    registry.register(LocalImportTool {
        relay: relay.clone(),
        policy: policy.clone(),
        artifacts: artifacts.clone(),
        inbox: inbox.clone(),
    })?;
    registry.register(LocalExportTool {
        relay: relay.clone(),
        policy: policy.clone(),
        artifacts: artifacts.clone(),
        store,
    })?;
    crate::local_bulk::register_bulk_tools(registry, relay, policy, artifacts, inbox)
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;
    use std::sync::Mutex;
    use std::time::Duration;

    use super::*;
    use crate::domain::new_id;
    use crate::gateway::{Decision, Gateway, ToolOutcome};
    use crate::local_relay::{LocalRoot, RelayResult};

    struct Fixture {
        relay: Arc<LocalRelay>,
        artifacts: Arc<ArtifactStore>,
        store: Store,
        inbox: PathBuf,
        user: String,
        base: PathBuf,
    }

    fn fixture() -> Fixture {
        let base = std::env::temp_dir().join(format!("serabot-xfer-{}", new_id()));
        std::fs::create_dir_all(&base).unwrap();
        let store = Store::open_in_memory(384).unwrap();
        let user = store
            .create_user("subject-xfer", "xfer@example.example", "Synthetic")
            .unwrap()
            .id;
        store
            .create_conversation(&user, "dir:main", "direct", None)
            .unwrap();
        Fixture {
            relay: LocalRelay::new(),
            artifacts: Arc::new(ArtifactStore::new(base.join("artifacts")).unwrap()),
            store,
            inbox: base.join("inbox"),
            user,
            base,
        }
    }

    fn roots(mode: &str) -> Vec<LocalRoot> {
        vec![LocalRoot {
            id: "docs".into(),
            label: "Docs".into(),
            mode: mode.into(),
            bots: Default::default(),
        }]
    }

    fn context(user: &str, tools: &[&str]) -> ToolContext {
        ToolContext {
            user_id: user.into(),
            actor: user.into(),
            bot: "main".into(),
            task_id: None,
            turn_id: None,
            conversation_id: Some("dir:main".into()),
            allowed_tools: tools
                .iter()
                .map(|t| (*t).to_owned())
                .collect::<HashSet<_>>(),
            require_approval: HashSet::new(),
        }
    }

    /// A stand-in for the desktop: serves `source` for chunk_read and
    /// collects put_chunk bytes (verifying the SHA-256 like the real one).
    fn fake_desktop(
        relay: Arc<LocalRelay>,
        user: String,
        mode: &str,
        source: Vec<u8>,
        received: Arc<Mutex<Vec<u8>>>,
        mutate_after_first: bool,
    ) -> tokio::task::JoinHandle<()> {
        let mode = mode.to_owned();
        tokio::spawn(async move {
            let mut served = 0;
            loop {
                let Ok(Some(request)) = relay
                    .poll(&user, roots(&mode), Duration::from_millis(300))
                    .await
                else {
                    if served > 0 {
                        return;
                    }
                    continue;
                };
                served += 1;
                let result = match request.op.as_str() {
                    "chunk_read" => {
                        let offset = request.args["offset"].as_u64().unwrap() as usize;
                        let end = (offset + TRANSFER_CHUNK_BYTES).min(source.len());
                        let modified = if mutate_after_first && offset > 0 {
                            2
                        } else {
                            1
                        };
                        RelayResult {
                            ok: true,
                            text: Some(
                                json!({
                                    "size": source.len(), "modified": modified, "offset": offset,
                                    "len": end - offset,
                                    "data": base64::engine::general_purpose::STANDARD.encode(&source[offset..end]),
                                })
                                .to_string(),
                            ),
                            error: None,
                        }
                    }
                    "put_chunk" => {
                        let part = base64::engine::general_purpose::STANDARD
                            .decode(request.content.as_deref().unwrap())
                            .unwrap();
                        let mut all = received.lock().unwrap();
                        assert_eq!(all.len() as u64, request.args["offset"].as_u64().unwrap());
                        all.extend(part);
                        let done = all.len() as u64 == request.args["total"].as_u64().unwrap();
                        if done && sha256_hex(&*all) != request.args["sha256"].as_str().unwrap() {
                            RelayResult {
                                ok: false,
                                text: None,
                                error: Some("hash mismatch".into()),
                            }
                        } else {
                            RelayResult {
                                ok: true,
                                text: Some(if done {
                                    "saved".into()
                                } else {
                                    "partial".into()
                                }),
                                error: None,
                            }
                        }
                    }
                    other => RelayResult {
                        ok: false,
                        text: None,
                        error: Some(format!("unexpected {other}")),
                    },
                };
                relay.complete(&user, &request.id, result);
            }
        })
    }

    fn pdf_bytes(len: usize) -> Vec<u8> {
        let mut bytes = b"%PDF-1.4\n".to_vec();
        bytes.extend((0..len).map(|i| (i % 253) as u8));
        bytes
    }

    fn import_tool(f: &Fixture) -> LocalImportTool {
        LocalImportTool {
            relay: f.relay.clone(),
            policy: Arc::new(Policy::from_text("forbiddenterm\n")),
            artifacts: f.artifacts.clone(),
            inbox: f.inbox.clone(),
        }
    }

    #[tokio::test]
    async fn import_copies_a_multi_chunk_file_into_the_users_inbox_and_artifacts() {
        let f = fixture();
        f.relay
            .poll(&f.user, roots("read"), Duration::ZERO)
            .await
            .unwrap();
        let source = pdf_bytes(2 * TRANSFER_CHUNK_BYTES + 99);
        let desktop = fake_desktop(
            f.relay.clone(),
            f.user.clone(),
            "read",
            source.clone(),
            Arc::default(),
            false,
        );
        let output = import_tool(&f)
            .execute(
                json!({"root": "docs", "path": "reports/q3.pdf"}),
                &context(&f.user, &["local_import"]),
            )
            .await
            .unwrap();
        desktop.abort();
        let data = output.data.unwrap();
        let rel = data["inbox_path"].as_str().unwrap();
        assert!(
            rel.starts_with(&format!("{}/local/", f.user)) && rel.ends_with("-q3.pdf"),
            "{rel}"
        );
        assert_eq!(std::fs::read(f.inbox.join(rel)).unwrap(), source);
        let hash = output.artifact_hash.unwrap();
        assert_eq!(f.artifacts.load(&hash).unwrap().bytes, source);
        assert_eq!(hash, sha256_hex(&source));
        assert!(output.content.contains("pdf_inspect"), "{}", output.content);
        std::fs::remove_dir_all(&f.base).ok();
    }

    #[tokio::test]
    async fn import_rejects_bad_requests_changed_files_and_wrong_content() {
        let f = fixture();
        let tool = import_tool(&f);
        let ctx = context(&f.user, &["local_import"]);
        for args in [
            json!({"root": "docs", "path": "../x.pdf"}),
            json!({"root": "docs", "path": "a.exe"}),
            json!({"root": "docs", "path": "a.pdf:ads"}),
            json!({"root": "docs", "path": "forbiddenterm.pdf"}),
            json!({"root": "../docs", "path": "a.pdf"}),
            json!({"root": "docs"}),
        ] {
            assert!(tool.execute(args.clone(), &ctx).await.is_err(), "{args}");
        }
        // Offline desktop.
        let offline = tool
            .execute(json!({"root": "docs", "path": "a.pdf"}), &ctx)
            .await
            .unwrap_err();
        assert!(
            offline.to_string().contains("Your PC app is closed"),
            "{offline}"
        );
        // The file changes between chunks.
        f.relay
            .poll(&f.user, roots("read"), Duration::ZERO)
            .await
            .unwrap();
        let desktop = fake_desktop(
            f.relay.clone(),
            f.user.clone(),
            "read",
            pdf_bytes(TRANSFER_CHUNK_BYTES + 10),
            Arc::default(),
            true,
        );
        let changed = tool
            .execute(json!({"root": "docs", "path": "a.pdf"}), &ctx)
            .await
            .unwrap_err();
        desktop.abort();
        assert!(
            changed.to_string().contains("changed on the PC"),
            "{changed}"
        );
        // Content that is not what the extension says.
        let desktop = fake_desktop(
            f.relay.clone(),
            f.user.clone(),
            "read",
            b"not a pdf at all".to_vec(),
            Arc::default(),
            false,
        );
        let wrong = tool
            .execute(json!({"root": "docs", "path": "a.pdf"}), &ctx)
            .await
            .unwrap_err();
        desktop.abort();
        assert!(wrong.to_string().contains("does not match"), "{wrong}");
        assert!(
            !f.inbox.join(&f.user).join("local").exists()
                || std::fs::read_dir(f.inbox.join(&f.user).join("local"))
                    .unwrap()
                    .next()
                    .is_none()
        );
        std::fs::remove_dir_all(&f.base).ok();
    }

    #[test]
    fn same_bytes_are_reused_and_the_cap_is_checked_before_any_write() {
        let f = fixture();
        let (first, _) =
            store_import_capped(&f.inbox, &f.artifacts, &f.user, "a.txt", b"hello", 100).unwrap();
        // The same bytes under another name reuse the copy already there.
        let (again, _) =
            store_import_capped(&f.inbox, &f.artifacts, &f.user, "copy.txt", b"hello", 100)
                .unwrap();
        assert_eq!(first, again);
        // A new file that would pass the cap is refused, and nothing is written.
        let refused = store_import_capped(
            &f.inbox,
            &f.artifacts,
            &f.user,
            "big.txt",
            &[b'x'; 200],
            100,
        )
        .unwrap_err();
        assert!(
            refused.to_string().contains("too much server space"),
            "{refused}"
        );
        let entries = std::fs::read_dir(f.inbox.join(&f.user).join("local"))
            .unwrap()
            .count();
        assert_eq!(entries, 1);
        // Another user's folder is counted separately: 60 bytes fit for them.
        let other = f
            .store
            .create_user("subject-cap", "cap@example.example", "Cap")
            .unwrap()
            .id;
        store_import_capped(&f.inbox, &f.artifacts, &other, "mine.txt", &[b'y'; 60], 100).unwrap();
        std::fs::remove_dir_all(&f.base).ok();
    }

    fn export_tool(f: &Fixture) -> LocalExportTool {
        LocalExportTool {
            relay: f.relay.clone(),
            policy: Arc::new(Policy::from_text("forbiddenterm\n")),
            artifacts: f.artifacts.clone(),
            store: f.store.clone(),
        }
    }

    fn make_artifact(f: &Fixture, bytes: &[u8], name: &str) -> String {
        let stored = f
            .artifacts
            .save(
                bytes,
                name,
                ArtifactMeta {
                    title: name.into(),
                    filename: name.into(),
                    language: Language::En,
                    source_artifact: None,
                    created_at: Utc::now(),
                },
            )
            .unwrap();
        f.store
            .record_conversation_artifact(
                &f.user,
                "dir:main",
                None,
                "pdf_edit",
                &stored.hash,
                name,
                None,
            )
            .unwrap();
        stored.hash
    }

    #[test]
    fn export_approval_binds_destination_artifact_hash_and_overwrite() {
        let f = fixture();
        let hash = make_artifact(&f, &pdf_bytes(500), "merged.pdf");
        let tool = export_tool(&f);
        assert!(tool.requires_approval(&json!({})));
        let base = json!({"root": "docs", "path": "out/merged.pdf", "artifact": hash});
        let details = tool.approval_details(&base).unwrap();
        assert_eq!(
            details.destination.as_deref(),
            Some("local:docs/out/merged.pdf")
        );
        assert_eq!(details.artifact_hash.as_deref(), Some(hash.as_str()));
        assert!(
            details.summary.contains("merged.pdf")
                && details.summary.contains("fails if it exists")
        );
        let overwrite = tool
            .approval_details(&json!({"root": "docs", "path": "out/merged.pdf", "artifact": hash, "overwrite": true}))
            .unwrap();
        assert!(overwrite.summary.contains("replaces"));
        for bad in [
            json!({"root": "docs", "path": "../merged.pdf", "artifact": hash}),
            json!({"root": "docs", "path": "x.exe", "artifact": hash}),
            json!({"root": "docs", "path": ".", "artifact": hash}),
            json!({"root": "docs", "path": "a.pdf", "artifact": "0".repeat(64)}),
            json!({"root": "docs", "path": "a.pdf"}),
        ] {
            assert!(tool.approval_details(&bad).is_err(), "{bad}");
        }
        std::fs::remove_dir_all(&f.base).ok();
    }

    #[tokio::test]
    async fn an_approved_export_reaches_the_desktop_and_a_changed_request_does_not() {
        let f = fixture();
        let source = pdf_bytes(TRANSFER_CHUNK_BYTES + 5000);
        let hash = make_artifact(&f, &source, "merged.pdf");
        let mut registry = ToolRegistry::default();
        registry.register(export_tool(&f)).unwrap();
        let gateway = Gateway::new(
            f.store.clone(),
            Arc::new(Policy::from_text("forbiddenterm\n")),
            registry,
            1,
            false,
        );
        let ctx = context(&f.user, &["local_export"]);
        let args = json!({"root": "docs", "path": "out/merged.pdf", "artifact": hash});
        let ToolOutcome::PendingApproval {
            approval_id,
            action_id,
            ..
        } = gateway
            .execute("local_export", args.clone(), &ctx)
            .await
            .unwrap()
        else {
            panic!("export must wait for approval");
        };
        // Nothing went to the desktop before approval.
        assert!(f.relay.roots(&f.user).is_empty());
        let action = f.store.get_action(&f.user, &action_id).unwrap();
        assert_eq!(
            action.destination.as_deref(),
            Some("local:docs/out/merged.pdf")
        );
        assert_eq!(action.artifact_hash.as_deref(), Some(hash.as_str()));
        gateway
            .decide(&f.user, &approval_id, Decision::Once)
            .unwrap();
        f.relay
            .poll(&f.user, roots("read_write"), Duration::ZERO)
            .await
            .unwrap();
        let received: Arc<Mutex<Vec<u8>>> = Arc::default();
        let desktop = fake_desktop(
            f.relay.clone(),
            f.user.clone(),
            "read_write",
            Vec::new(),
            received.clone(),
            false,
        );
        let outcome = gateway.execute_approved(&action_id, &ctx).await.unwrap();
        desktop.abort();
        let ToolOutcome::Completed(output) = outcome else {
            panic!("completed")
        };
        assert_eq!(
            output.destination.as_deref(),
            Some("local:docs/out/merged.pdf")
        );
        assert_eq!(*received.lock().unwrap(), source);
        // The same action cannot be replayed, and different arguments need a
        // fresh approval.
        assert!(gateway.execute_approved(&action_id, &ctx).await.is_err());
        let again = gateway
            .execute(
                "local_export",
                json!({"root": "docs", "path": "other/merged.pdf", "artifact": hash}),
                &ctx,
            )
            .await
            .unwrap();
        assert!(matches!(again, ToolOutcome::PendingApproval { .. }));
        std::fs::remove_dir_all(&f.base).ok();
    }

    #[tokio::test]
    async fn export_refuses_foreign_artifacts_read_only_folders_and_blocked_names() {
        let f = fixture();
        let hash = make_artifact(&f, &pdf_bytes(100), "mine.pdf");
        let tool = export_tool(&f);
        let other = f
            .store
            .create_user("subject-other", "other@example.example", "Other")
            .unwrap()
            .id;
        let args = json!({"root": "docs", "path": "x.pdf", "artifact": hash});
        let foreign = tool
            .execute(args.clone(), &context(&other, &["local_export"]))
            .await
            .unwrap_err();
        assert!(matches!(foreign, SeraError::Forbidden(_)), "{foreign}");
        f.relay
            .poll(&f.user, roots("read"), Duration::ZERO)
            .await
            .unwrap();
        let read_only = tool
            .execute(args, &context(&f.user, &["local_export"]))
            .await
            .unwrap_err();
        assert!(matches!(read_only, SeraError::Forbidden(_)), "{read_only}");
        let blocked = tool
            .execute(
                json!({"root": "docs", "path": "forbiddenterm.pdf", "artifact": hash}),
                &context(&f.user, &["local_export"]),
            )
            .await
            .unwrap_err();
        assert!(matches!(blocked, SeraError::PolicyViolation), "{blocked}");
        std::fs::remove_dir_all(&f.base).ok();
    }

    #[test]
    fn transfer_tools_are_classified_by_the_gateway() {
        assert!(crate::gateway::is_destructive_tool("local_export"));
        assert!(!crate::gateway::is_destructive_tool("local_import"));
        let f = fixture();
        let mut registry = ToolRegistry::default();
        register_transfer_tools(
            &mut registry,
            f.relay.clone(),
            Arc::new(Policy::empty()),
            f.artifacts.clone(),
            f.store.clone(),
            f.inbox.clone(),
        )
        .unwrap();
        std::fs::remove_dir_all(&f.base).ok();
    }
}
