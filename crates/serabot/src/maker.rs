use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::deck::{Brand, DeckSpec};
use crate::domain::Language;
use crate::error::{Result, SeraError};
use crate::gateway::{ApprovalDetails, Tool, ToolContext, ToolOutput, ToolSpec};
use crate::onedrive::OneDriveRegistry;
use crate::render::render_deck;
use crate::store::Store;
use crate::telegram::TelegramClient;
use crate::util::sha256_hex;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArtifactMeta {
    pub title: String,
    pub filename: String,
    pub language: Language,
    pub source_artifact: Option<String>,
    pub created_at: chrono::DateTime<Utc>,
}

/// Content type for a stored artifact, derived from its filename.
pub use crate::util::mime_for_filename;

#[derive(Debug, Clone)]
pub struct StoredArtifact {
    pub hash: String,
    pub filename: String,
    pub bytes: Vec<u8>,
    pub meta: ArtifactMeta,
}

/// Largest artifact the store accepts, and the largest file a media job saves.
pub const MAX_ARTIFACT_BYTES: usize = 100 * 1024 * 1024;

/// The artifacts one tool call saved, and the ones it lists for the owner
/// beside its primary artifact (0.24).
#[derive(Debug, Default)]
pub struct CallArtifacts {
    pub saved: HashSet<String>,
    pub listed: Vec<crate::gateway::ExtraArtifact>,
}

tokio::task_local! {
    /// Scoped by the gateway around each tool call. A save made on a blocking
    /// thread is not seen here, so such a file cannot be listed.
    pub static CALL_ARTIFACTS: Arc<Mutex<CallArtifacts>>;
}

/// List a file this call saved in the owner's conversation, beside the call's
/// primary artifact (0.24). A file the call did not save is not listed. Returns
/// whether it was listed.
pub fn list_saved_artifact(stored: &StoredArtifact) -> bool {
    list_saved_hash(&stored.hash, &stored.filename, &stored.meta.title)
}

/// The same as [`list_saved_artifact`], for a save known only by its hash,
/// filename and title (a tool result that already holds them).
pub fn list_saved_hash(hash: &str, filename: &str, title: &str) -> bool {
    CALL_ARTIFACTS
        .try_with(|calls| {
            let Ok(mut calls) = calls.lock() else {
                return false;
            };
            if !calls.saved.contains(hash) {
                return false;
            }
            calls.listed.push(crate::gateway::ExtraArtifact {
                hash: hash.to_owned(),
                filename: filename.to_owned(),
                title: Some(title.to_owned()),
            });
            true
        })
        .unwrap_or(false)
}

pub struct ArtifactStore {
    root: PathBuf,
}

impl ArtifactStore {
    pub fn new(root: impl Into<PathBuf>) -> Result<Self> {
        let root = root.into();
        std::fs::create_dir_all(&root)?;
        Ok(Self { root })
    }

    pub fn save(&self, bytes: &[u8], filename: &str, meta: ArtifactMeta) -> Result<StoredArtifact> {
        if bytes.is_empty() || bytes.len() > MAX_ARTIFACT_BYTES {
            return Err(SeraError::Validation("invalid artifact size".into()));
        }
        let hash = sha256_hex(bytes);
        let filename = safe_filename(filename)?;
        let data_path = artifact_bytes_path(&self.root, &hash, &filename);
        let meta_path = self.root.join(format!("{hash}.json"));
        atomic_write(&data_path, bytes)?;
        let meta_bytes = serde_json::to_vec_pretty(&meta)?;
        atomic_write(&meta_path, &meta_bytes)?;
        let _ = CALL_ARTIFACTS.try_with(|calls| {
            if let Ok(mut calls) = calls.lock() {
                calls.saved.insert(hash.clone());
            }
        });
        Ok(StoredArtifact {
            hash,
            filename,
            bytes: bytes.to_vec(),
            meta,
        })
    }

    /// Delete artifact files whose modification time is older than `cutoff`.
    ///
    /// Retention is opt-in: the caller passes a cutoff only when the operator
    /// configured a positive retention window. Both the bytes and the sidecar
    /// are removed, so a pruned hash fails closed instead of returning stale
    /// metadata.
    pub fn prune_older_than(&self, cutoff: chrono::DateTime<Utc>) -> Result<usize> {
        let mut removed = 0usize;
        for entry in std::fs::read_dir(&self.root)? {
            let entry = entry?;
            if !entry.file_type()?.is_file() {
                continue;
            }
            let modified = entry.metadata()?.modified()?;
            let modified: chrono::DateTime<Utc> = modified.into();
            if modified < cutoff {
                std::fs::remove_file(entry.path())?;
                removed = removed.saturating_add(1);
            }
        }
        Ok(removed)
    }

    /// Metadata only, without loading the artifact bytes — for listings.
    pub fn meta(&self, hash: &str) -> Result<ArtifactMeta> {
        validate_hash(hash)?;
        let meta_raw = std::fs::read(self.root.join(format!("{hash}.json")))
            .map_err(|_| SeraError::NotFound(format!("artifact metadata {hash}")))?;
        Ok(serde_json::from_slice(&meta_raw)?)
    }

    pub fn load(&self, hash: &str) -> Result<StoredArtifact> {
        validate_hash(hash)?;
        let meta_raw = std::fs::read(self.root.join(format!("{hash}.json")))
            .map_err(|_| SeraError::NotFound(format!("artifact metadata {hash}")))?;
        let meta: ArtifactMeta = serde_json::from_slice(&meta_raw)?;
        let data_path = artifact_bytes_path(&self.root, hash, &meta.filename);
        let bytes = std::fs::read(&data_path)
            .map_err(|_| SeraError::NotFound(format!("artifact {hash}")))?;
        if sha256_hex(&bytes) != hash {
            return Err(SeraError::InvalidState(
                "artifact bytes do not match their content hash".into(),
            ));
        }
        Ok(StoredArtifact {
            hash: hash.to_owned(),
            filename: meta.filename.clone(),
            bytes,
            meta,
        })
    }

    /// Approve the Italian artifact's content for the acting user. The
    /// approval marker is scoped to `(user_id, hash)` in `kv`, and the audit
    /// row records the acting user's id.
    pub fn approve(&self, store: &Store, user_id: &str, hash: &str) -> Result<()> {
        let artifact = self.load(hash)?;
        if artifact.meta.language != Language::It {
            return Err(SeraError::Validation(
                "only an Italian draft can be approved for French generation".into(),
            ));
        }
        store.kv_set(&format!("deck_approved:{user_id}:{hash}"), "1")?;
        store.record_audit(
            user_id,
            "deck.italian_content_approved",
            None,
            None,
            None,
            None,
            None,
            Some(&json!({"artifact_hash": hash})),
        )?;
        Ok(())
    }

    pub fn is_approved(&self, store: &Store, user_id: &str, hash: &str) -> Result<bool> {
        Ok(store
            .kv_get(&format!("deck_approved:{user_id}:{hash}"))?
            .as_deref()
            == Some("1"))
    }

    /// Copy one file from the acting user's inbox area into the store.
    ///
    /// This is the single, format-agnostic inbox → artifact path. The path is
    /// resolved inside `<inbox>/<user_id>/` by [`crate::user_inbox::resolve`]
    /// (another user's file is not found), then the caller's extension
    /// allowlist and size cap apply, and it must be a regular file. `inspect`
    /// runs on the bytes before anything is written, so a caller can reject a
    /// file without leaving an artifact behind; only then are the bytes hashed
    /// into the store. The artifact records English because ingest does not
    /// infer a language.
    pub fn ingest_inbox_file<T>(
        &self,
        inbox: &Path,
        user_id: &str,
        relative: &str,
        extensions: &[&str],
        max_bytes: usize,
        inspect: impl FnOnce(&[u8]) -> Result<T>,
    ) -> Result<(StoredArtifact, T)> {
        let candidate = crate::user_inbox::resolve(inbox, user_id, relative)?;
        let extension = candidate
            .extension()
            .and_then(|value| value.to_str())
            .unwrap_or_default()
            .to_ascii_lowercase();
        if !extensions.iter().any(|allowed| *allowed == extension) {
            return Err(SeraError::Validation(format!(
                "unsupported file type; use {}",
                extensions.join(", ")
            )));
        }
        let metadata = std::fs::metadata(&candidate)?;
        if !metadata.is_file() {
            return Err(SeraError::Validation("inbox path is not a file".into()));
        }
        if metadata.len() == 0 {
            return Err(SeraError::Validation("inbox file is empty".into()));
        }
        if metadata.len() > max_bytes as u64 {
            return Err(SeraError::Validation(format!(
                "inbox file is {} bytes; the limit is {max_bytes} bytes",
                metadata.len()
            )));
        }
        let bytes = std::fs::read(&candidate)?;
        if bytes.is_empty() || bytes.len() > max_bytes {
            return Err(SeraError::Validation(
                "inbox file changed while it was read".into(),
            ));
        }
        let inspected = inspect(&bytes)?;
        let filename = candidate
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("file")
            .to_owned();
        let stored = self.save(
            &bytes,
            &filename,
            ArtifactMeta {
                title: filename.clone(),
                filename: filename.clone(),
                language: Language::En,
                source_artifact: None,
                created_at: Utc::now(),
            },
        )?;
        Ok((stored, inspected))
    }
}

fn artifact_bytes_path(root: &Path, hash: &str, filename: &str) -> PathBuf {
    let extension = Path::new(filename)
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or("pdf");
    root.join(format!("{hash}.{extension}"))
}

fn validate_hash(hash: &str) -> Result<()> {
    if hash.len() != 64 || !hash.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err(SeraError::Validation("invalid artifact hash".into()));
    }
    Ok(())
}

fn safe_filename(value: &str) -> Result<String> {
    let stem = value
        .rsplit('/')
        .next()
        .unwrap_or(value)
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.') {
                c
            } else {
                '-'
            }
        })
        .collect::<String>();
    let stem = stem.trim_matches(['.', '-']).to_owned();
    if stem.is_empty() {
        return Err(SeraError::Validation("invalid artifact filename".into()));
    }
    Ok(stem.chars().take(180).collect())
}

fn atomic_write(path: &Path, bytes: &[u8]) -> Result<()> {
    let temporary = path.with_extension("tmp");
    std::fs::write(&temporary, bytes)?;
    std::fs::rename(&temporary, path)?;
    Ok(())
}

pub struct DeckBuildTool {
    brand: Arc<Brand>,
    artifacts: Arc<ArtifactStore>,
    store: Store,
    max_pages: usize,
}

impl DeckBuildTool {
    pub fn new(
        brand: Arc<Brand>,
        artifacts: Arc<ArtifactStore>,
        store: Store,
        max_pages: usize,
    ) -> Self {
        Self {
            brand,
            artifacts,
            store,
            max_pages,
        }
    }
}

#[async_trait]
impl Tool for DeckBuildTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "deck_build".into(),
            description:
                "Render a bounded structured deck specification into a deterministic PDF and a \
                text-only PPTX (text, shapes, and brand theme colours; no images, links, notes, or \
                embedded files). Italian is the default. French requires source_artifact to be an \
                Italian artifact that the owner has explicitly approved."
                    .into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "spec": {
                        "type": "object",
                        "description": "DeckSpec: title, optional subtitle/date_label/author/footer, language (it|fr|en), and sections[{part_title?, title, cards[{heading, body, bullets[], monogram?}], bullets[]}]"
                    },
                    "source_artifact": {"type": "string", "description": "Approved Italian artifact hash, required for French"}
                },
                "required": ["spec"],
                "additionalProperties": false
            }),
            external_effect: false,
        }
    }

    async fn execute(&self, args: Value, context: &ToolContext) -> Result<ToolOutput> {
        let spec: DeckSpec = serde_json::from_value(
            args.get("spec")
                .cloned()
                .ok_or_else(|| SeraError::Validation("spec is required".into()))?,
        )
        .map_err(|error| SeraError::Validation(format!("invalid deck spec: {error}")))?;
        let source = args
            .get("source_artifact")
            .and_then(Value::as_str)
            .map(str::to_owned);
        match spec.language {
            Language::It => {}
            Language::Fr => {
                let source = source.as_deref().ok_or_else(|| {
                    SeraError::Validation(
                        "French generation requires an approved Italian source_artifact".into(),
                    )
                })?;
                let source_artifact = self.artifacts.load(source)?;
                if source_artifact.meta.language != Language::It {
                    return Err(SeraError::Validation(
                        "source_artifact must be an Italian deck".into(),
                    ));
                }
                if !self
                    .artifacts
                    .is_approved(&self.store, &context.user_id, source)?
                {
                    return Err(SeraError::Forbidden(
                        "the Italian draft has not been approved by the owner".into(),
                    ));
                }
            }
            Language::En => {}
        }
        let rendered = render_deck(&spec, &self.brand, self.max_pages)?;
        // Both deliverables are rendered before either is stored, so a spec the
        // renderer rejects never leaves half a deck on disk.
        #[cfg(feature = "pptx")]
        let pptx = crate::deck_pptx::render_deck_pptx(&spec, &self.brand, self.max_pages)?;

        let meta = ArtifactMeta {
            title: spec.title.clone(),
            filename: rendered.filename.clone(),
            language: spec.language,
            source_artifact: source.clone(),
            created_at: Utc::now(),
        };
        let stored = self
            .artifacts
            .save(&rendered.pdf, &rendered.filename, meta)?;

        // The PPTX is a second artifact with its own hash, stored through the
        // same store so approval and audit bind it exactly like the PDF.
        #[cfg(feature = "pptx")]
        let pptx_stored = self.artifacts.save(
            &pptx.bytes,
            &pptx.filename,
            ArtifactMeta {
                title: spec.title.clone(),
                filename: pptx.filename.clone(),
                language: spec.language,
                source_artifact: source,
                created_at: Utc::now(),
            },
        )?;

        #[cfg(feature = "pptx")]
        let pptx_summary = Some((
            pptx_stored.hash.clone(),
            pptx_stored.filename.clone(),
            pptx.slides,
        ));
        #[cfg(not(feature = "pptx"))]
        let pptx_summary: Option<(String, String, usize)> = None;

        self.store.record_audit(
            &context.actor,
            "deck.rendered",
            Some("deck_build"),
            context.task_id.as_deref(),
            None,
            None,
            None,
            Some(&json!({
                "artifact_hash": stored.hash,
                "language": spec.language.code(),
                "pages": rendered.pages,
                "source_artifact": stored.meta.source_artifact,
                "pptx_artifact_hash": pptx_summary.as_ref().map(|(hash, _, _)| hash.clone()),
                "pptx_slides": pptx_summary.as_ref().map(|(_, _, slides)| *slides),
            })),
        )?;
        Ok(ToolOutput {
            content: format!(
                "title: {}\nsubtitle: {}\nauthor: {}\nfooter: {}\nbrand: {}\nfilename: {}\nartifact: {}\npptx: {}\npptx_artifact: {}\n\n{}",
                spec.title,
                spec.subtitle.as_deref().unwrap_or(""),
                spec.author.as_deref().unwrap_or(""),
                spec.footer.as_deref().unwrap_or(""),
                self.brand.name,
                stored.filename,
                stored.hash,
                pptx_summary
                    .as_ref()
                    .map(|(_, filename, _)| filename.as_str())
                    .unwrap_or(""),
                pptx_summary
                    .as_ref()
                    .map(|(hash, _, _)| hash.as_str())
                    .unwrap_or(""),
                rendered.text
            ),
            data: Some(json!({
                "artifact_hash": stored.hash,
                "filename": stored.filename,
                "pages": rendered.pages,
                "language": spec.language.code(),
                "pptx_artifact_hash": pptx_summary.as_ref().map(|(hash, _, _)| hash.clone()),
                "pptx_filename": pptx_summary.as_ref().map(|(_, filename, _)| filename.clone()),
                "pptx_slides": pptx_summary.as_ref().map(|(_, _, slides)| *slides),
            })),
            artifact_path: Some(format!("artifacts/{}", stored.filename)),
            artifact_hash: Some(stored.hash),
            filename: Some(stored.filename),
            destination: None,
            cards: Vec::new(),
        })
    }
}

pub const TELEGRAM_SEND_DOCUMENT_TOOL: &str = "telegram_send_document";

pub struct TelegramDocumentTool {
    client: Arc<TelegramClient>,
    artifacts: Arc<ArtifactStore>,
    policy: Arc<crate::policy::Policy>,
}

impl TelegramDocumentTool {
    pub fn new(
        client: Arc<TelegramClient>,
        artifacts: Arc<ArtifactStore>,
        policy: Arc<crate::policy::Policy>,
    ) -> Self {
        Self {
            client,
            artifacts,
            policy,
        }
    }
}

#[async_trait]
impl Tool for TelegramDocumentTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: TELEGRAM_SEND_DOCUMENT_TOOL.into(),
            description:
                "Deliver an existing rendered artifact to the owner's private Telegram chat.".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "artifact_hash": {"type": "string"},
                    "caption": {"type": "string"}
                },
                "required": ["artifact_hash"],
                "additionalProperties": false
            }),
            external_effect: true,
        }
    }

    fn approval_details(&self, args: &Value) -> Result<ApprovalDetails> {
        let hash = args
            .get("artifact_hash")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let artifact = self.artifacts.load(hash)?;
        Ok(ApprovalDetails {
            summary: format!(
                "Send '{}' to the acting user's private Telegram chat",
                artifact.filename
            ),
            // The chat is resolved from the acting user's binding at
            // execution time; the destination binds the approval to that
            // user, not to a chat id that can change on re-link.
            destination: Some("telegram:user".into()),
            artifact_hash: Some(artifact.hash),
        })
    }

    async fn execute(&self, args: Value, context: &ToolContext) -> Result<ToolOutput> {
        let hash = args
            .get("artifact_hash")
            .and_then(Value::as_str)
            .ok_or_else(|| SeraError::Validation("artifact_hash is required".into()))?;
        let caption = args
            .get("caption")
            .and_then(Value::as_str)
            .unwrap_or("SeraBot deliverable");
        // The gateway scans `output.content` after the tool returns, but the
        // caption is model-authored text that leaves the host here. Scan it
        // before the upload, not after.
        if !self.policy.is_allowed(caption) {
            return Err(SeraError::PolicyViolation);
        }
        let artifact = self.artifacts.load(hash)?;
        // Telegram's Bot API caps sendDocument at 50 MB. The artifact store
        // allows 100 MiB, so fail locally with a clear message instead of
        // surfacing an opaque provider error.
        const TELEGRAM_MAX_DOCUMENT_BYTES: usize = 50 * 1024 * 1024;
        if artifact.bytes.len() > TELEGRAM_MAX_DOCUMENT_BYTES {
            return Err(SeraError::Validation(format!(
                "'{}' is {} bytes; Telegram accepts at most {} bytes. Upload it to OneDrive instead.",
                artifact.filename,
                artifact.bytes.len(),
                TELEGRAM_MAX_DOCUMENT_BYTES
            )));
        }
        let chat_id = self.client.chat_for(&context.user_id)?;
        self.client
            .send_document(chat_id, &artifact.filename, artifact.bytes, caption)
            .await?;
        Ok(ToolOutput {
            content: format!("Delivered {} to Telegram.", artifact.filename),
            data: Some(json!({"destination": "telegram", "filename": artifact.filename})),
            artifact_hash: Some(artifact.hash),
            filename: Some(artifact.filename),
            destination: Some("telegram:user".into()),
            ..ToolOutput::default()
        })
    }
}

pub const TELEGRAM_REPLY_FILE_TOOL: &str = "telegram_reply_file";

/// `telegram_reply_file` (0.63): send one of the member's files into their
/// own Telegram chat, as the reply to a conversation that came from
/// Telegram. The member asked for it in that chat, so it is not
/// approval-gated (the approval-gated `telegram_send_document` stays for
/// deliveries from any other conversation). Checks artifact ownership, the
/// Telegram size cap and the blocklist on the filename and caption.
pub struct TelegramReplyFileTool {
    client: Arc<TelegramClient>,
    artifacts: Arc<ArtifactStore>,
    policy: Arc<crate::policy::Policy>,
    store: crate::store::Store,
}

impl TelegramReplyFileTool {
    pub fn new(
        client: Arc<TelegramClient>,
        artifacts: Arc<ArtifactStore>,
        policy: Arc<crate::policy::Policy>,
        store: crate::store::Store,
    ) -> Self {
        Self {
            client,
            artifacts,
            policy,
            store,
        }
    }
}

#[async_trait]
impl Tool for TelegramReplyFileTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: TELEGRAM_REPLY_FILE_TOOL.into(),
            description: "Send a file (a document, image or media made by the user's bots or \
                          uploaded by the user) back into this Telegram chat, when the user \
                          asked for it over Telegram. Find the hash with conversation_library."
                .into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "artifact_hash": {"type": "string"},
                    "caption": {"type": "string"}
                },
                "required": ["artifact_hash"],
                "additionalProperties": false
            }),
            external_effect: true,
        }
    }

    async fn execute(&self, args: Value, context: &ToolContext) -> Result<ToolOutput> {
        let hash = args
            .get("artifact_hash")
            .and_then(Value::as_str)
            .ok_or_else(|| SeraError::Validation("artifact_hash is required".into()))?;
        let caption = args
            .get("caption")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|text| !text.is_empty())
            .unwrap_or("");
        let conversation = context.conversation_id.as_deref().unwrap_or_default();
        let from_telegram = !conversation.is_empty()
            && self
                .store
                .conversation_origin(&context.user_id, conversation)?
                .as_deref()
                == Some(crate::telegram::ORIGIN_TELEGRAM);
        if !from_telegram {
            return Err(SeraError::Validation(
                "this conversation did not come from Telegram; use telegram_send_document \
                 (it asks the user first)"
                    .into(),
            ));
        }
        if !self.store.user_owns_artifact(&context.user_id, hash)? {
            return Err(SeraError::NotFound("artifact".into()));
        }
        let artifact = self.artifacts.load(hash)?;
        if !self.policy.is_allowed(caption) || !self.policy.is_allowed(&artifact.filename) {
            return Err(SeraError::PolicyViolation);
        }
        const TELEGRAM_MAX_DOCUMENT_BYTES: usize = 50 * 1024 * 1024;
        if artifact.bytes.len() > TELEGRAM_MAX_DOCUMENT_BYTES {
            return Err(SeraError::Validation(format!(
                "'{}' is larger than Telegram's 50 MB limit",
                artifact.filename
            )));
        }
        let chat_id = self.client.chat_for(&context.user_id)?;
        self.client
            .send_document(chat_id, &artifact.filename, artifact.bytes, caption)
            .await?;
        Ok(ToolOutput {
            content: format!("Sent {} on Telegram.", artifact.filename),
            artifact_hash: Some(artifact.hash),
            filename: Some(artifact.filename),
            destination: Some("telegram:user".into()),
            ..ToolOutput::default()
        })
    }
}

pub struct OneDriveUploadTool {
    registry: Arc<OneDriveRegistry>,
    artifacts: Arc<ArtifactStore>,
    folder: String,
}

impl OneDriveUploadTool {
    pub fn new(
        registry: Arc<OneDriveRegistry>,
        artifacts: Arc<ArtifactStore>,
        folder: impl Into<String>,
    ) -> Self {
        Self {
            registry,
            artifacts,
            folder: folder.into(),
        }
    }

    fn resolve_destination(&self, args: &Value) -> Result<String> {
        let folder = args.get("folder").and_then(Value::as_str);
        let subfolder = args.get("subfolder").and_then(Value::as_str);
        if folder.is_some() && subfolder.is_some() {
            return Err(SeraError::Validation(
                "provide either folder or subfolder, not both".into(),
            ));
        }
        if let Some(folder) = folder {
            let trimmed = folder.trim();
            if matches!(trimmed, "" | "/" | "." | "root") {
                return Ok("/".to_owned());
            }
            let inner = trimmed.strip_prefix('/').unwrap_or(trimmed);
            let inner = inner.strip_suffix('/').unwrap_or(inner);
            if inner.is_empty() || inner == "." || inner == "root" {
                return Ok("/".to_owned());
            }
            validate_onedrive_path_segments(inner, 12, "folder")?;
            return Ok(format!("/{inner}"));
        }
        if let Some(subfolder) = subfolder {
            let trimmed = subfolder.trim();
            validate_onedrive_path_segments(trimmed, 4, "subfolder")?;
            return Ok(format!("/{}/{}", self.folder.trim_matches('/'), trimmed));
        }
        Ok(format!("/{}", self.folder.trim_matches('/')))
    }
}

fn validate_onedrive_path_segments(value: &str, max_depth: usize, label: &str) -> Result<()> {
    if value.is_empty() {
        return Err(SeraError::Validation(format!("{label} must not be empty")));
    }
    if value.starts_with('/') || value.ends_with('/') {
        return Err(SeraError::Validation(format!(
            "{label} must not start or end with '/'"
        )));
    }
    let parts: Vec<&str> = value.split('/').collect();
    if parts.len() > max_depth {
        return Err(SeraError::Validation(format!(
            "{label} exceeds the maximum depth of {max_depth} segments"
        )));
    }
    for part in &parts {
        if part.is_empty() || *part == "." || *part == ".." || part.trim() != *part {
            return Err(SeraError::Validation(format!(
                "{label} contains an invalid path segment"
            )));
        }
        if part
            .chars()
            .any(|c| c.is_control() || matches!(c, '\\' | ':' | '*' | '<' | '>' | '?' | '"' | '|'))
        {
            return Err(SeraError::Validation(format!(
                "{label} contains an unsupported character"
            )));
        }
    }
    Ok(())
}

#[async_trait]
impl Tool for OneDriveUploadTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "onedrive_upload".into(),
            description: format!(
                "Upload an existing stored artifact to the acting user's OneDrive (requires approval). \
                 By default uploads into the configured dossier folder ('{}', optionally under `subfolder`); \
                 pass `folder` to upload to any path from the drive root (e.g. 'Documents/Jobs', or '/' for root). \
                 Never overwrites existing files (renames or fails according to server policy).",
                self.folder
            ),
            parameters: json!({
                "type": "object",
                "properties": {
                    "artifact_hash": {"type": "string", "description": "SHA-256 hash of the stored artifact to upload"},
                    "folder": {"type": "string", "maxLength": 300, "description": "Optional destination folder path from the drive root (e.g. 'Documents/Jobs' or '/')"},
                    "subfolder": {"type": "string", "maxLength": 300, "description": "Optional subfolder below the configured dossier folder"}
                },
                "required": ["artifact_hash"],
                "additionalProperties": false
            }),
            external_effect: true,
        }
    }

    fn approval_details(&self, args: &Value) -> Result<ApprovalDetails> {
        let hash = args
            .get("artifact_hash")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let artifact = self.artifacts.load(hash)?;
        let destination_folder = self.resolve_destination(args)?;
        let exact_path = if destination_folder == "/" {
            format!("/{}", artifact.filename)
        } else {
            format!("{}/{}", destination_folder, artifact.filename)
        };
        Ok(ApprovalDetails {
            summary: format!(
                "Upload '{}' to OneDrive path '{}'",
                artifact.filename, exact_path
            ),
            destination: Some(format!("onedrive:{exact_path}")),
            artifact_hash: Some(artifact.hash),
        })
    }

    async fn execute(&self, args: Value, context: &ToolContext) -> Result<ToolOutput> {
        let hash = args
            .get("artifact_hash")
            .and_then(Value::as_str)
            .ok_or_else(|| SeraError::Validation("artifact_hash is required".into()))?;
        let folder = args.get("folder").and_then(Value::as_str);
        let subfolder = args.get("subfolder").and_then(Value::as_str);
        let artifact = self.artifacts.load(hash)?;
        // The acting user's own client: no connected drive is an explicit
        // error, never the admin's (or any other account's) drive.
        let client = self.registry.client_for(&context.user_id)?;
        let mut client = client.lock().await;
        let result = client
            .upload_to_folder(
                &artifact.filename,
                artifact.bytes,
                mime_for_filename(&artifact.filename),
                folder,
                subfolder,
            )
            .await?;
        Ok(ToolOutput {
            content: format!("Uploaded to OneDrive: {}", result.path),
            data: Some(json!({
                "id": result.id,
                "name": result.name,
                "web_url": result.web_url,
                "path": result.path,
                "size": result.size,
                "sha256": result.sha256,
            })),
            artifact_hash: Some(result.sha256),
            filename: Some(result.name),
            destination: Some(format!("onedrive:{}", result.path)),
            ..ToolOutput::default()
        })
    }
}

/// Largest file `onedrive_download` imports.
const MAX_ONEDRIVE_DOWNLOAD_BYTES: usize = 25 * 1024 * 1024;

fn next_reader_hint(filename: &str) -> &'static str {
    let ext = Path::new(filename)
        .extension()
        .and_then(|value| value.to_str())
        .map(str::to_ascii_lowercase);
    match ext.as_deref() {
        Some("pdf") => {
            "pdf_inspect (or document_index for OCR/text search, pdf_to_images for scanned pages)"
        }
        Some("xlsx" | "xlsm" | "xls" | "ods") => {
            "xlsx_import then xlsx_read / xlsx_inspect / xlsx_edit"
        }
        Some("csv") => "xlsx_analyze or document_index",
        Some("docx") => "docx_import then docx_read / docx_inspect / docx_edit",
        Some("pptx") => "pptx_import then pptx_inspect / pptx_edit",
        Some("png" | "jpg" | "jpeg" | "webp" | "gif") => {
            "image_inspect / image_crop / image_adjust / image_compress"
        }
        _ => "document_index",
    }
}

/// Read-only: copy one file from the acting member's own OneDrive into an
/// artifact (for example their Excel tracker, before `xlsx_edit`).
pub struct OneDriveDownloadTool {
    registry: Arc<OneDriveRegistry>,
    artifacts: Arc<ArtifactStore>,
}

impl OneDriveDownloadTool {
    pub fn new(registry: Arc<OneDriveRegistry>, artifacts: Arc<ArtifactStore>) -> Self {
        Self {
            registry,
            artifacts,
        }
    }
}

#[async_trait]
impl Tool for OneDriveDownloadTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "onedrive_download".into(),
            description: "Import one file from the acting user's own OneDrive into an artifact (read-only; max 25 MB; path from the drive root as shown by onedrive_list, onedrive_search, or onedrive_recent, e.g. 'Documents/Jobs/tracker.xlsx'). Returns an artifact hash and recommends the matching reader tool by file type (pdf -> pdf_inspect/document_index; xlsx/csv -> xlsx_import/xlsx_read; docx -> docx_import; pptx -> pptx_import; images -> image_inspect).".into(),
            parameters: json!({
                "type": "object",
                "properties": {"path": {"type": "string", "maxLength": 300, "description": "File path from the drive root (e.g. 'Documents/Jobs/tracker.xlsx')"}},
                "required": ["path"],
                "additionalProperties": false
            }),
            external_effect: false,
        }
    }

    async fn execute(&self, args: Value, context: &ToolContext) -> Result<ToolOutput> {
        let path = args
            .get("path")
            .and_then(Value::as_str)
            .ok_or_else(|| SeraError::Validation("path is required".into()))?
            .trim()
            .trim_matches('/')
            .to_owned();
        let client = self.registry.client_for(&context.user_id)?;
        let client = client.lock().await;
        let (name, bytes) = client.download(&path, MAX_ONEDRIVE_DOWNLOAD_BYTES).await?;
        drop(client);
        let stored = self.artifacts.save(
            &bytes,
            &name,
            ArtifactMeta {
                title: name.clone(),
                filename: name.clone(),
                language: Language::En,
                source_artifact: None,
                created_at: Utc::now(),
            },
        )?;
        let hint = next_reader_hint(&stored.filename);
        Ok(ToolOutput {
            content: format!(
                "Imported '{}' from your OneDrive ({} bytes) as artifact {}.\nNext: pass this artifact hash to {}.",
                stored.filename,
                bytes.len(),
                stored.hash,
                hint
            ),
            data: Some(json!({
                "artifact_hash": stored.hash,
                "filename": stored.filename,
                "size_bytes": bytes.len(),
                "next_tool_hint": hint,
            })),
            artifact_hash: Some(stored.hash),
            filename: Some(stored.filename),
            ..ToolOutput::default()
        })
    }
}

fn format_drive_item_line(item: &crate::onedrive::DriveListItem) -> String {
    let mut meta = Vec::new();
    if item.folder {
        if let Some(count) = item.child_count {
            meta.push(format!("{count} items"));
        }
    } else {
        meta.push(format!("{} bytes", item.size));
    }
    if let Some(modified) = &item.last_modified {
        meta.push(format!("modified {modified}"));
    }
    if let Some(url) = &item.web_url {
        meta.push(url.clone());
    }
    let kind = if item.folder { "folder" } else { "file" };
    if meta.is_empty() {
        format!("- [{kind}] {} ({})", item.name, item.path)
    } else {
        format!(
            "- [{kind}] {} ({} — {})",
            item.name,
            item.path,
            meta.join(", ")
        )
    }
}

fn drive_items_json(items: &[crate::onedrive::DriveListItem]) -> Vec<Value> {
    items
        .iter()
        .map(|item| {
            json!({
                "id": item.id,
                "name": item.name,
                "folder": item.folder,
                "size": item.size,
                "path": item.path,
                "last_modified": item.last_modified,
                "web_url": item.web_url,
                "child_count": item.child_count,
            })
        })
        .collect()
}

pub struct OneDriveListTool {
    registry: Arc<OneDriveRegistry>,
    folder: String,
}

impl OneDriveListTool {
    pub fn new(registry: Arc<OneDriveRegistry>, folder: impl Into<String>) -> Self {
        Self {
            registry,
            folder: folder.into(),
        }
    }
}

#[async_trait]
impl Tool for OneDriveListTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "onedrive_list".into(),
            description: format!(
                "List files and folders in the acting user's own OneDrive (read-only). \
                 By default lists the drive root ('/'); pass `path` for any folder from the drive root \
                 (e.g. 'Documents/Jobs'), or pass `subfolder` to list inside the configured dossier folder ('{}'). \
                 Returns name, path, size, folder flag, child count, last modified time, and web URL.",
                self.folder
            ),
            parameters: json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string", "maxLength": 300, "description": "Folder path from the drive root (default '/' for drive root, e.g. 'Documents/Jobs')"},
                    "subfolder": {"type": "string", "maxLength": 300, "description": "Optional subfolder relative to the configured dossier folder"},
                    "max_pages": {"type": "integer", "minimum": 1, "maximum": 10}
                },
                "additionalProperties": false
            }),
            external_effect: false,
        }
    }

    async fn execute(&self, args: Value, context: &ToolContext) -> Result<ToolOutput> {
        let path = args.get("path").and_then(Value::as_str);
        let subfolder = args.get("subfolder").and_then(Value::as_str);
        if path.is_some() && subfolder.is_some() {
            return Err(SeraError::Validation(
                "provide either path or subfolder, not both".into(),
            ));
        }
        let max_pages = args
            .get("max_pages")
            .and_then(Value::as_u64)
            .map(|pages| pages as usize)
            .unwrap_or(5);
        let client = self.registry.client_for(&context.user_id)?;
        let client = client.lock().await;
        let (listed_scope, result) = if let Some(subfolder) = subfolder {
            let scope = format!(
                "/{}/{}",
                self.folder.trim_matches('/'),
                subfolder.trim_matches('/')
            );
            (scope, client.list_folder(Some(subfolder), max_pages).await?)
        } else {
            let scope = match path.map(str::trim) {
                None | Some("" | "/" | "." | "root") => "/ (OneDrive root)".to_owned(),
                Some(p) => format!("/{}", p.trim_matches('/')),
            };
            (scope, client.list_path(path, max_pages).await?)
        };
        let header = format!(
            "OneDrive listing ({listed_scope}; configured dossier folder: /{}): {} item(s){}",
            self.folder.trim_matches('/'),
            result.items.len(),
            if result.truncated { " (truncated)" } else { "" }
        );
        let content = if result.items.is_empty() {
            header
        } else {
            let lines = result
                .items
                .iter()
                .map(format_drive_item_line)
                .collect::<Vec<_>>()
                .join("\n");
            format!("{header}\n{lines}")
        };
        Ok(ToolOutput {
            content,
            data: Some(json!({
                "scope": listed_scope,
                "dossier_folder": format!("/{}", self.folder.trim_matches('/')),
                "items": drive_items_json(&result.items),
                "truncated": result.truncated,
            })),
            ..ToolOutput::default()
        })
    }
}

/// Read-only search across the acting member's own OneDrive.
pub struct OneDriveSearchTool {
    registry: Arc<OneDriveRegistry>,
}

impl OneDriveSearchTool {
    pub fn new(registry: Arc<OneDriveRegistry>) -> Self {
        Self { registry }
    }
}

#[async_trait]
impl Tool for OneDriveSearchTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "onedrive_search".into(),
            description: "Search files and folders across the acting user's own OneDrive by name or content query (read-only). Returns matching drive-root paths, sizes, modified times, and web URLs.".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "query": {"type": "string", "minLength": 1, "maxLength": 200, "description": "Search text"},
                    "max_results": {"type": "integer", "minimum": 1, "maximum": 50, "description": "Maximum results to return (default 20, max 50)"}
                },
                "required": ["query"],
                "additionalProperties": false
            }),
            external_effect: false,
        }
    }

    async fn execute(&self, args: Value, context: &ToolContext) -> Result<ToolOutput> {
        let query = args
            .get("query")
            .and_then(Value::as_str)
            .ok_or_else(|| SeraError::Validation("query is required".into()))?;
        let max_results = args
            .get("max_results")
            .and_then(Value::as_u64)
            .map(|n| n as usize)
            .unwrap_or(20);
        let client = self.registry.client_for(&context.user_id)?;
        let client = client.lock().await;
        let result = client.search(query, max_results).await?;
        let header = format!(
            "OneDrive search for '{}': {} item(s){}",
            query.trim(),
            result.items.len(),
            if result.truncated { " (truncated)" } else { "" }
        );
        let content = if result.items.is_empty() {
            header
        } else {
            let lines = result
                .items
                .iter()
                .map(format_drive_item_line)
                .collect::<Vec<_>>()
                .join("\n");
            format!("{header}\n{lines}")
        };
        Ok(ToolOutput {
            content,
            data: Some(json!({
                "query": query.trim(),
                "items": drive_items_json(&result.items),
                "truncated": result.truncated,
            })),
            ..ToolOutput::default()
        })
    }
}

/// Read-only list of recently accessed items in the acting member's own OneDrive.
pub struct OneDriveRecentTool {
    registry: Arc<OneDriveRegistry>,
}

impl OneDriveRecentTool {
    pub fn new(registry: Arc<OneDriveRegistry>) -> Self {
        Self { registry }
    }
}

#[async_trait]
impl Tool for OneDriveRecentTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "onedrive_recent".into(),
            description: "List recently accessed files in the acting user's own OneDrive (read-only). Useful when the user asks about a file they worked on recently without knowing its folder.".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "max_results": {"type": "integer", "minimum": 1, "maximum": 50, "description": "Maximum items to return (default 20, max 50)"}
                },
                "additionalProperties": false
            }),
            external_effect: false,
        }
    }

    async fn execute(&self, args: Value, context: &ToolContext) -> Result<ToolOutput> {
        let max_results = args
            .get("max_results")
            .and_then(Value::as_u64)
            .map(|n| n as usize)
            .unwrap_or(20);
        let client = self.registry.client_for(&context.user_id)?;
        let client = client.lock().await;
        let result = client.recent(max_results).await?;
        let header = format!(
            "OneDrive recent files: {} item(s){}",
            result.items.len(),
            if result.truncated { " (truncated)" } else { "" }
        );
        let content = if result.items.is_empty() {
            header
        } else {
            let lines = result
                .items
                .iter()
                .map(format_drive_item_line)
                .collect::<Vec<_>>()
                .join("\n");
            format!("{header}\n{lines}")
        };
        Ok(ToolOutput {
            content,
            data: Some(json!({
                "items": drive_items_json(&result.items),
                "truncated": result.truncated,
            })),
            ..ToolOutput::default()
        })
    }
}

/// Read-only item metadata lookup in the acting member's own OneDrive.
pub struct OneDriveInfoTool {
    registry: Arc<OneDriveRegistry>,
}

impl OneDriveInfoTool {
    pub fn new(registry: Arc<OneDriveRegistry>) -> Self {
        Self { registry }
    }
}

#[async_trait]
impl Tool for OneDriveInfoTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "onedrive_info".into(),
            description: "Inspect metadata for one file or folder in the acting user's own OneDrive by path from the drive root (read-only): size, created/modified timestamps, MIME type, child count, SHA-256 hash when provided, and web URL.".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string", "maxLength": 300, "description": "File or folder path from the drive root (or '/' for the root)"}
                },
                "required": ["path"],
                "additionalProperties": false
            }),
            external_effect: false,
        }
    }

    async fn execute(&self, args: Value, context: &ToolContext) -> Result<ToolOutput> {
        let path = args
            .get("path")
            .and_then(Value::as_str)
            .ok_or_else(|| SeraError::Validation("path is required".into()))?;
        let client = self.registry.client_for(&context.user_id)?;
        let client = client.lock().await;
        let info = client.item_info(path).await?;
        let mut lines = vec![
            format!(
                "OneDrive {}: {} ({})",
                if info.folder { "folder" } else { "file" },
                info.name,
                info.path
            ),
            format!("id: {}", info.id),
            format!("size: {} bytes", info.size),
        ];
        if let Some(count) = info.child_count {
            lines.push(format!("children: {count}"));
        }
        if let Some(mime) = &info.mime_type {
            lines.push(format!("mime: {mime}"));
        }
        if let Some(created) = &info.created_at {
            lines.push(format!("created: {created}"));
        }
        if let Some(modified) = &info.last_modified {
            lines.push(format!("modified: {modified}"));
        }
        if let Some(sha) = &info.sha256 {
            lines.push(format!("sha256: {sha}"));
        }
        if let Some(url) = &info.web_url {
            lines.push(format!("web_url: {url}"));
        }
        Ok(ToolOutput {
            content: lines.join("\n"),
            data: Some(json!({
                "id": info.id,
                "name": info.name,
                "folder": info.folder,
                "size": info.size,
                "path": info.path,
                "created_at": info.created_at,
                "last_modified": info.last_modified,
                "web_url": info.web_url,
                "mime_type": info.mime_type,
                "child_count": info.child_count,
                "sha256": info.sha256,
            })),
            ..ToolOutput::default()
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::deck::{BrandPalette, DeckCard, DeckSection};
    use crate::domain::Language;
    use crate::gateway::ToolRegistry;
    use crate::policy::Policy;
    use crate::store::Store;
    use std::collections::HashSet;

    fn brand() -> Brand {
        Brand {
            name: "Example".into(),
            palette: BrandPalette {
                background: "#FFFFFF".into(),
                foreground: "#111111".into(),
                accent: "#1B5E5A".into(),
                muted: "#777777".into(),
            },
            monogram_text: "EX".into(),
            footer: None,
            font_family: Some("helvetica".into()),
        }
    }

    fn deck(language: Language) -> DeckSpec {
        DeckSpec {
            title: "Synthetic deck".into(),
            subtitle: None,
            language,
            date_label: None,
            author: None,
            sections: vec![DeckSection {
                part_title: None,
                title: "Section".into(),
                cards: vec![DeckCard {
                    heading: "Card".into(),
                    body: "Body".into(),
                    bullets: vec![],
                    monogram: Some("EX".into()),
                }],
                bullets: vec![],
            }],
            footer: None,
        }
    }

    fn context() -> ToolContext {
        ToolContext {
            user_id: crate::store::ADMIN_USER_ID.to_owned(),
            actor: "owner".into(),
            bot: "maker".into(),
            task_id: None,
            turn_id: None,
            conversation_id: None,
            allowed_tools: ["deck_build".to_string()].into_iter().collect(),
            require_approval: HashSet::new(),
        }
    }

    #[tokio::test]
    async fn renders_and_stores_artifact() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in_memory(384).unwrap();
        let artifacts = Arc::new(ArtifactStore::new(dir.path()).unwrap());
        let tool = DeckBuildTool::new(Arc::new(brand()), artifacts.clone(), store, 10);
        let args = json!({"spec": deck(Language::It)});
        let output = tool.execute(args, &context()).await.unwrap();
        let hash = output.artifact_hash.clone().unwrap();
        let stored = artifacts.load(&hash).unwrap();
        assert_eq!(stored.meta.language, Language::It);
        assert!(stored.bytes.starts_with(b"%PDF"));
        // The model only sees `content`; the hash must be in it or the deck
        // cannot be delivered.
        assert!(
            output.content.contains(&format!("artifact: {hash}")),
            "content must carry the PDF hash: {}",
            output.content
        );

        #[cfg(feature = "pptx")]
        {
            let data = output.data.clone().unwrap();
            let pptx_hash = data["pptx_artifact_hash"].as_str().unwrap().to_owned();
            assert_ne!(pptx_hash, hash);
            assert!(
                output
                    .content
                    .contains(&format!("pptx_artifact: {pptx_hash}")),
                "content must carry the PPTX hash: {}",
                output.content
            );
            let pptx = artifacts.load(&pptx_hash).unwrap();
            assert_eq!(pptx.meta.language, Language::It);
            assert!(pptx.filename.ends_with(".pptx"));
            assert!(pptx.bytes.starts_with(b"PK"));
            assert_eq!(data["pptx_slides"], 3);
        }
    }

    #[tokio::test]
    async fn rejected_spec_stores_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in_memory(384).unwrap();
        let artifacts = Arc::new(ArtifactStore::new(dir.path()).unwrap());
        // Cover, section divider, and card page need three pages.
        let tool = DeckBuildTool::new(Arc::new(brand()), artifacts, store, 1);
        assert!(
            tool.execute(json!({"spec": deck(Language::It)}), &context())
                .await
                .is_err()
        );
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
    }

    #[tokio::test]
    async fn french_requires_approved_italian_source() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in_memory(384).unwrap();
        let artifacts = Arc::new(ArtifactStore::new(dir.path()).unwrap());
        let tool = DeckBuildTool::new(Arc::new(brand()), artifacts.clone(), store.clone(), 10);
        let italian = tool
            .execute(json!({"spec": deck(Language::It)}), &context())
            .await
            .unwrap()
            .artifact_hash
            .unwrap();
        let args = json!({"spec": deck(Language::Fr), "source_artifact": italian});
        assert!(tool.execute(args.clone(), &context()).await.is_err());
        artifacts
            .approve(&store, &store.admin_user_id().unwrap(), &italian)
            .unwrap();
        assert!(tool.execute(args, &context()).await.is_ok());
    }

    #[tokio::test]
    async fn deck_approval_is_scoped_per_user() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in_memory(384).unwrap();
        let user_a = store
            .create_user("sub-deck-a", "deck-a@example.example", "Deck A")
            .unwrap();
        let user_b = store
            .create_user("sub-deck-b", "deck-b@example.example", "Deck B")
            .unwrap();
        let artifacts = Arc::new(ArtifactStore::new(dir.path()).unwrap());
        let tool = DeckBuildTool::new(Arc::new(brand()), artifacts.clone(), store.clone(), 10);

        let mut ctx_a = context();
        ctx_a.user_id = user_a.id.clone();
        let mut ctx_b = context();
        ctx_b.user_id = user_b.id.clone();

        let italian = tool
            .execute(json!({"spec": deck(Language::It)}), &ctx_a)
            .await
            .unwrap()
            .artifact_hash
            .unwrap();

        artifacts.approve(&store, &user_a.id, &italian).unwrap();
        assert!(artifacts.is_approved(&store, &user_a.id, &italian).unwrap());
        assert!(!artifacts.is_approved(&store, &user_b.id, &italian).unwrap());

        let args = json!({"spec": deck(Language::Fr), "source_artifact": italian});
        assert!(tool.execute(args.clone(), &ctx_a).await.is_ok());
        assert!(matches!(
            tool.execute(args.clone(), &ctx_b).await,
            Err(SeraError::Forbidden(_))
        ));

        artifacts.approve(&store, &user_b.id, &italian).unwrap();
        assert!(tool.execute(args, &ctx_b).await.is_ok());
    }

    #[test]
    fn artifact_paths_reject_traversal() {
        let store = ArtifactStore::new(tempfile::tempdir().unwrap().path()).unwrap();
        assert!(store.load("../../etc/passwd").is_err());
        assert_eq!(safe_filename("../../x.pdf").unwrap(), "x.pdf");
    }

    #[test]
    fn ingest_inbox_file_enforces_the_boundary() {
        let dir = tempfile::tempdir().unwrap();
        let inbox = dir.path().join("inbox");
        std::fs::create_dir_all(inbox.join("u1")).unwrap();
        std::fs::create_dir_all(inbox.join("u2")).unwrap();
        let artifacts = ArtifactStore::new(dir.path().join("artifacts")).unwrap();
        std::fs::write(inbox.join("u1").join("note.txt"), b"hello").unwrap();
        std::fs::write(dir.path().join("outside.txt"), b"hello").unwrap();

        let (stored, seen) = artifacts
            .ingest_inbox_file(&inbox, "u1", "u1/note.txt", &["txt"], 16, |bytes| {
                Ok(bytes.len())
            })
            .unwrap();
        assert_eq!(seen, 5);
        assert_eq!(stored.bytes, b"hello");
        assert_eq!(stored.filename, "note.txt");

        let absolute = inbox
            .join("u1")
            .join("note.txt")
            .to_string_lossy()
            .to_string();
        for path in [
            "u1/../outside.txt",
            "u1/missing.txt",
            "u1/note.md",
            absolute.as_str(),
        ] {
            assert!(
                artifacts
                    .ingest_inbox_file(&inbox, "u1", path, &["txt"], 16, |_| Ok(()))
                    .is_err(),
                "accepted {path}"
            );
        }
        // Another user's file, named by its prefix, is not found.
        assert!(matches!(
            artifacts
                .ingest_inbox_file(&inbox, "u2", "u1/note.txt", &["txt"], 16, |_| Ok(()))
                .unwrap_err(),
            SeraError::NotFound(_)
        ));
        // The size cap is enforced before the bytes are read.
        assert!(
            artifacts
                .ingest_inbox_file(&inbox, "u1", "u1/note.txt", &["txt"], 4, |_| Ok(()))
                .is_err()
        );
        // A rejected inspection stores nothing.
        assert!(
            artifacts
                .ingest_inbox_file(
                    &inbox,
                    "u1",
                    "u1/note.txt",
                    &["txt"],
                    16,
                    |_| -> Result<()> { Err(SeraError::Validation("rejected".into())) }
                )
                .is_err()
        );
        let stored_artifacts = std::fs::read_dir(dir.path().join("artifacts"))
            .unwrap()
            .filter_map(|entry| entry.ok())
            .filter(|entry| entry.path().extension().is_some_and(|ext| ext == "json"))
            .count();
        assert_eq!(stored_artifacts, 1);
    }

    #[allow(dead_code)]
    fn registry_smoke() {
        let mut registry = ToolRegistry::default();
        let store = Store::open_in_memory(384).unwrap();
        let artifacts =
            Arc::new(ArtifactStore::new(std::env::temp_dir().join("serabot-smoke")).unwrap());
        registry
            .register(DeckBuildTool::new(Arc::new(brand()), artifacts, store, 5))
            .unwrap();
        let _ = Policy::empty();
    }

    fn onedrive_registry_for_server(
        server: &wiremock::MockServer,
    ) -> (
        tempfile::TempDir,
        Arc<crate::onedrive::OneDriveRegistry>,
        crate::config::Config,
    ) {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in_memory(384).unwrap();
        let users =
            Arc::new(crate::user_secrets::UserSecrets::with_master_key(store, [7u8; 32]).unwrap());
        users
            .set(
                crate::store::ADMIN_USER_ID,
                crate::onedrive::ONEDRIVE_PROVIDER,
                "refresh-1",
            )
            .unwrap();

        let secrets = crate::secrets::SharedSecrets::new(crate::secrets::SecretStore::from_map(
            std::collections::BTreeMap::from([("onedrive_client_id".into(), "client-1".into())]),
        ));

        let mut config = crate::config::Config::default();
        config.onedrive.enabled = true;
        config.onedrive.folder = "SeraBot".into();
        config.onedrive.graph_base = format!("{}/v1.0", server.uri());

        let registry = Arc::new(
            crate::onedrive::OneDriveRegistry::new(&config, &secrets, users)
                .unwrap()
                .with_token_url(format!("{}/token", server.uri()))
                .unwrap(),
        );
        (dir, registry, config)
    }

    async fn mount_maker_onedrive_token(server: &wiremock::MockServer) {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, ResponseTemplate};
        Mock::given(method("POST"))
            .and(path("/token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "access_token": "access-1",
                "expires_in": 3600,
                "token_type": "Bearer",
            })))
            .mount(server)
            .await;
    }

    #[test]
    fn onedrive_read_tools_are_non_destructive_and_upload_binds_destination() {
        let art_dir = tempfile::tempdir().unwrap();
        let artifacts = Arc::new(ArtifactStore::new(art_dir.path()).unwrap());
        let stored = artifacts
            .save(
                b"%PDF-1.7 test",
                "brief.pdf",
                ArtifactMeta {
                    title: "Brief".into(),
                    filename: "brief.pdf".into(),
                    language: Language::En,
                    source_artifact: None,
                    created_at: Utc::now(),
                },
            )
            .unwrap();

        assert!(!crate::gateway::is_destructive_tool("onedrive_list"));
        assert!(!crate::gateway::is_destructive_tool("onedrive_search"));
        assert!(!crate::gateway::is_destructive_tool("onedrive_recent"));
        assert!(!crate::gateway::is_destructive_tool("onedrive_info"));
        assert!(!crate::gateway::is_destructive_tool("onedrive_download"));
        assert!(crate::gateway::is_destructive_tool("onedrive_upload"));

        let server_uri = "https://graph.microsoft.com/v1.0".to_owned();
        let store = Store::open_in_memory(384).unwrap();
        let users =
            Arc::new(crate::user_secrets::UserSecrets::with_master_key(store, [8u8; 32]).unwrap());
        let secrets = crate::secrets::SharedSecrets::new(crate::secrets::SecretStore::from_map(
            std::collections::BTreeMap::from([("onedrive_client_id".into(), "client-1".into())]),
        ));
        let mut config = crate::config::Config::default();
        config.onedrive.enabled = true;
        config.onedrive.folder = "SeraBot".into();
        config.onedrive.graph_base = server_uri;
        let registry =
            Arc::new(crate::onedrive::OneDriveRegistry::new(&config, &secrets, users).unwrap());

        let upload = OneDriveUploadTool::new(registry.clone(), artifacts.clone(), "SeraBot");
        assert!(upload.spec().external_effect);
        assert!(
            !OneDriveListTool::new(registry.clone(), "SeraBot")
                .spec()
                .external_effect
        );
        assert!(
            !OneDriveSearchTool::new(registry.clone())
                .spec()
                .external_effect
        );
        assert!(
            !OneDriveRecentTool::new(registry.clone())
                .spec()
                .external_effect
        );
        assert!(
            !OneDriveInfoTool::new(registry.clone())
                .spec()
                .external_effect
        );
        assert!(
            !OneDriveDownloadTool::new(registry, artifacts)
                .spec()
                .external_effect
        );

        let default_dest = upload
            .approval_details(&json!({"artifact_hash": stored.hash}))
            .unwrap();
        assert_eq!(
            default_dest.destination.as_deref(),
            Some("onedrive:/SeraBot/brief.pdf")
        );

        let sub_dest = upload
            .approval_details(&json!({"artifact_hash": stored.hash, "subfolder": "Q4"}))
            .unwrap();
        assert_eq!(
            sub_dest.destination.as_deref(),
            Some("onedrive:/SeraBot/Q4/brief.pdf")
        );

        let folder_dest = upload
            .approval_details(&json!({"artifact_hash": stored.hash, "folder": "Documents/Jobs"}))
            .unwrap();
        assert_eq!(
            folder_dest.destination.as_deref(),
            Some("onedrive:/Documents/Jobs/brief.pdf")
        );

        let root_dest = upload
            .approval_details(&json!({"artifact_hash": stored.hash, "folder": "/"}))
            .unwrap();
        assert_eq!(
            root_dest.destination.as_deref(),
            Some("onedrive:/brief.pdf")
        );

        assert!(
            upload
                .approval_details(&json!({
                    "artifact_hash": stored.hash,
                    "folder": "Documents",
                    "subfolder": "Q4"
                }))
                .is_err()
        );
    }

    #[test]
    fn onedrive_download_hints_match_file_extension() {
        assert!(next_reader_hint("cv.pdf").contains("pdf_inspect"));
        assert!(next_reader_hint("tracker.xlsx").contains("xlsx_import"));
        assert!(next_reader_hint("data.csv").contains("xlsx_analyze"));
        assert!(next_reader_hint("letter.docx").contains("docx_import"));
        assert!(next_reader_hint("slides.pptx").contains("pptx_import"));
        assert!(next_reader_hint("scan.png").contains("image_inspect"));
    }

    #[tokio::test]
    async fn onedrive_list_search_recent_info_and_download_execute_against_mock() {
        use wiremock::matchers::{method, path, query_param};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        mount_maker_onedrive_token(&server).await;

        Mock::given(method("GET"))
            .and(path("/v1.0/me/drive/root/children"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "value": [{
                    "id": "d1",
                    "name": "Documents",
                    "folder": { "childCount": 3 },
                    "lastModifiedDateTime": "2026-11-20T09:00:00Z"
                }]
            })))
            .mount(&server)
            .await;

        Mock::given(method("GET"))
            .and(path("/v1.0/me/drive/root:/Documents/Jobs:/children"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "value": [{
                    "id": "f1",
                    "name": "cv.pdf",
                    "size": 100,
                    "file": {},
                    "parentReference": { "path": "/drive/root:/Documents/Jobs" }
                }]
            })))
            .mount(&server)
            .await;

        Mock::given(method("GET"))
            .and(path("/v1.0/me/drive/root:/SeraBot/Out:/children"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "value": []
            })))
            .mount(&server)
            .await;

        Mock::given(method("GET"))
            .and(path("/v1.0/me/drive/root/search(q='tracker')"))
            .and(query_param("$top", "5"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "value": [{
                    "id": "f2",
                    "name": "tracker.xlsx",
                    "size": 200,
                    "file": {},
                    "parentReference": { "path": "/drive/root:/Documents/Jobs" }
                }]
            })))
            .mount(&server)
            .await;

        Mock::given(method("GET"))
            .and(path("/v1.0/me/drive/recent"))
            .and(query_param("$top", "4"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "value": [{
                    "id": "f2",
                    "name": "tracker.xlsx",
                    "size": 200,
                    "file": {},
                    "parentReference": { "path": "/drive/root:/Documents/Jobs" }
                }]
            })))
            .mount(&server)
            .await;

        Mock::given(method("GET"))
            .and(path("/v1.0/me/drive/root:/Documents/Jobs/tracker.xlsx"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "id": "f2",
                "name": "tracker.xlsx",
                "size": 200,
                "parentReference": { "path": "/drive/root:/Documents/Jobs" },
                "file": { "mimeType": "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet" }
            })))
            .mount(&server)
            .await;

        Mock::given(method("GET"))
            .and(path(
                "/v1.0/me/drive/root:/Documents/Jobs/tracker.xlsx:/content",
            ))
            .respond_with(
                ResponseTemplate::new(302)
                    .insert_header("location", format!("{}/dl/tracker.xlsx", server.uri())),
            )
            .mount(&server)
            .await;

        Mock::given(method("GET"))
            .and(path("/dl/tracker.xlsx"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(b"fake-xlsx".to_vec()))
            .mount(&server)
            .await;

        let (_dir, registry, _config) = onedrive_registry_for_server(&server);
        let art_dir = tempfile::tempdir().unwrap();
        let artifacts = Arc::new(ArtifactStore::new(art_dir.path()).unwrap());
        let ctx = context();

        let list = OneDriveListTool::new(registry.clone(), "SeraBot");
        let root_out = list.execute(json!({}), &ctx).await.unwrap();
        assert!(
            root_out
                .content
                .contains("configured dossier folder: /SeraBot")
        );
        assert!(root_out.content.contains("/Documents"));

        let path_out = list
            .execute(json!({"path": "Documents/Jobs"}), &ctx)
            .await
            .unwrap();
        assert!(path_out.content.contains("/Documents/Jobs/cv.pdf"));

        let sub_out = list
            .execute(json!({"subfolder": "Out"}), &ctx)
            .await
            .unwrap();
        assert!(sub_out.content.contains("/SeraBot/Out"));

        assert!(
            list.execute(json!({"path": "Documents", "subfolder": "Out"}), &ctx)
                .await
                .is_err()
        );

        let search = OneDriveSearchTool::new(registry.clone());
        let search_out = search
            .execute(json!({"query": "tracker", "max_results": 5}), &ctx)
            .await
            .unwrap();
        assert!(search_out.content.contains("/Documents/Jobs/tracker.xlsx"));

        let recent = OneDriveRecentTool::new(registry.clone());
        let recent_out = recent
            .execute(json!({"max_results": 4}), &ctx)
            .await
            .unwrap();
        assert!(recent_out.content.contains("tracker.xlsx"));

        let info = OneDriveInfoTool::new(registry.clone());
        let info_out = info
            .execute(json!({"path": "Documents/Jobs/tracker.xlsx"}), &ctx)
            .await
            .unwrap();
        assert!(info_out.content.contains("size: 200 bytes"));

        let download = OneDriveDownloadTool::new(registry.clone(), artifacts);
        let dl_out = download
            .execute(json!({"path": "/Documents/Jobs/tracker.xlsx"}), &ctx)
            .await
            .unwrap();
        assert!(dl_out.artifact_hash.is_some());
        assert!(dl_out.content.contains("xlsx_import"));

        // Non-connected member fails closed rather than falling back to admin.
        let mut other_ctx = ctx.clone();
        other_ctx.user_id = "other-member".into();
        assert!(list.execute(json!({}), &other_ctx).await.is_err());
    }
}
