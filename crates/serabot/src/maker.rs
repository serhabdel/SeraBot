use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::sync::Mutex;

use crate::deck::{Brand, DeckSpec};
use crate::domain::Language;
use crate::error::{Result, SeraError};
use crate::gateway::{ApprovalDetails, Tool, ToolContext, ToolOutput, ToolSpec};
use crate::onedrive::OneDriveClient;
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

#[derive(Debug, Clone)]
pub struct StoredArtifact {
    pub hash: String,
    pub filename: String,
    pub bytes: Vec<u8>,
    pub meta: ArtifactMeta,
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
        if bytes.is_empty() || bytes.len() > 100 * 1024 * 1024 {
            return Err(SeraError::Validation("invalid artifact size".into()));
        }
        let hash = sha256_hex(bytes);
        let filename = safe_filename(filename)?;
        let data_path = artifact_bytes_path(&self.root, &hash, &filename);
        let meta_path = self.root.join(format!("{hash}.json"));
        atomic_write(&data_path, bytes)?;
        let meta_bytes = serde_json::to_vec_pretty(&meta)?;
        atomic_write(&meta_path, &meta_bytes)?;
        Ok(StoredArtifact {
            hash,
            filename,
            bytes: bytes.to_vec(),
            meta,
        })
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

    pub fn approve(&self, store: &Store, hash: &str) -> Result<()> {
        let artifact = self.load(hash)?;
        if artifact.meta.language != Language::It {
            return Err(SeraError::Validation(
                "only an Italian draft can be approved for French generation".into(),
            ));
        }
        store.kv_set(&format!("deck_approved:{hash}"), "1")?;
        store.record_audit(
            "owner",
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

    pub fn is_approved(&self, store: &Store, hash: &str) -> Result<bool> {
        Ok(store.kv_get(&format!("deck_approved:{hash}"))?.as_deref() == Some("1"))
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
                if !self.artifacts.is_approved(&self.store, source)? {
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
                "title: {}\nsubtitle: {}\nauthor: {}\nfooter: {}\nbrand: {}\nfilename: {}\npptx: {}\n\n{}",
                spec.title,
                spec.subtitle.as_deref().unwrap_or(""),
                spec.author.as_deref().unwrap_or(""),
                spec.footer.as_deref().unwrap_or(""),
                self.brand.name,
                stored.filename,
                pptx_summary
                    .as_ref()
                    .map(|(_, filename, _)| filename.as_str())
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
        })
    }
}

pub struct TelegramDocumentTool {
    client: Arc<TelegramClient>,
    artifacts: Arc<ArtifactStore>,
}

impl TelegramDocumentTool {
    pub fn new(client: Arc<TelegramClient>, artifacts: Arc<ArtifactStore>) -> Self {
        Self { client, artifacts }
    }
}

#[async_trait]
impl Tool for TelegramDocumentTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "telegram_send_document".into(),
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
                "Send '{}' to the owner's private Telegram chat",
                artifact.filename
            ),
            destination: Some(format!("telegram:owner:{}", self.client.owner_chat_id())),
            artifact_hash: Some(artifact.hash),
        })
    }

    async fn execute(&self, args: Value, _context: &ToolContext) -> Result<ToolOutput> {
        let hash = args
            .get("artifact_hash")
            .and_then(Value::as_str)
            .ok_or_else(|| SeraError::Validation("artifact_hash is required".into()))?;
        let caption = args
            .get("caption")
            .and_then(Value::as_str)
            .unwrap_or("SeraBot deliverable");
        let artifact = self.artifacts.load(hash)?;
        self.client
            .send_document(
                self.client.owner_chat_id(),
                &artifact.filename,
                artifact.bytes,
                caption,
            )
            .await?;
        Ok(ToolOutput {
            content: format!("Delivered {} to Telegram.", artifact.filename),
            data: Some(json!({"destination": "telegram", "filename": artifact.filename})),
            artifact_hash: Some(artifact.hash),
            filename: Some(artifact.filename),
            destination: Some("telegram:owner".into()),
            ..ToolOutput::default()
        })
    }
}

pub struct OneDriveUploadTool {
    client: Arc<Mutex<OneDriveClient>>,
    artifacts: Arc<ArtifactStore>,
    folder: String,
}

impl OneDriveUploadTool {
    pub fn new(
        client: Arc<Mutex<OneDriveClient>>,
        artifacts: Arc<ArtifactStore>,
        folder: impl Into<String>,
    ) -> Self {
        Self {
            client,
            artifacts,
            folder: folder.into(),
        }
    }
}

#[async_trait]
impl Tool for OneDriveUploadTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "onedrive_upload".into(),
            description:
                "Upload an existing rendered artifact to the approved OneDrive dossier folder."
                    .into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "artifact_hash": {"type": "string"},
                    "subfolder": {"type": "string"}
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
        let destination = match args.get("subfolder").and_then(Value::as_str) {
            Some(subfolder) => format!("{}/{}", self.folder, subfolder),
            None => self.folder.clone(),
        };
        Ok(ApprovalDetails {
            summary: format!(
                "Upload '{}' to OneDrive folder '{}'",
                artifact.filename, destination
            ),
            destination: Some(format!("onedrive:{destination}")),
            artifact_hash: Some(artifact.hash),
        })
    }

    async fn execute(&self, args: Value, _context: &ToolContext) -> Result<ToolOutput> {
        let hash = args
            .get("artifact_hash")
            .and_then(Value::as_str)
            .ok_or_else(|| SeraError::Validation("artifact_hash is required".into()))?;
        let subfolder = args.get("subfolder").and_then(Value::as_str);
        let artifact = self.artifacts.load(hash)?;
        let mut client = self.client.lock().await;
        let result = client
            .upload(
                &artifact.filename,
                artifact.bytes,
                "application/pdf",
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

pub struct OneDriveListTool {
    client: Arc<Mutex<OneDriveClient>>,
    folder: String,
}

impl OneDriveListTool {
    pub fn new(client: Arc<Mutex<OneDriveClient>>, folder: impl Into<String>) -> Self {
        Self {
            client,
            folder: folder.into(),
        }
    }
}

#[async_trait]
impl Tool for OneDriveListTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "onedrive_list".into(),
            description:
                "List files and folders in the approved OneDrive dossier folder (read-only).".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "subfolder": {"type": "string"},
                    "max_pages": {"type": "integer", "minimum": 1, "maximum": 10}
                },
                "additionalProperties": false
            }),
            external_effect: false,
        }
    }

    fn approval_details(&self, args: &Value) -> Result<ApprovalDetails> {
        let destination = match args.get("subfolder").and_then(Value::as_str) {
            Some(subfolder) => format!("{}/{}", self.folder, subfolder),
            None => self.folder.clone(),
        };
        Ok(ApprovalDetails {
            summary: format!("List OneDrive folder '{destination}'"),
            destination: Some(format!("onedrive:{destination}")),
            artifact_hash: None,
        })
    }

    async fn execute(&self, args: Value, _context: &ToolContext) -> Result<ToolOutput> {
        let subfolder = args.get("subfolder").and_then(Value::as_str);
        let max_pages = args
            .get("max_pages")
            .and_then(Value::as_u64)
            .map(|pages| pages as usize)
            .unwrap_or(5);
        let client = self.client.lock().await;
        let result = client.list_folder(subfolder, max_pages).await?;
        Ok(ToolOutput {
            content: format!(
                "OneDrive listing: {} item(s){}",
                result.items.len(),
                if result.truncated { " (truncated)" } else { "" }
            ),
            data: Some(json!({
                "items": result.items.iter().map(|item| json!({
                    "id": item.id,
                    "name": item.name,
                    "folder": item.folder,
                    "size": item.size,
                    "path": item.path,
                })).collect::<Vec<_>>(),
                "truncated": result.truncated,
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

        #[cfg(feature = "pptx")]
        {
            let data = output.data.clone().unwrap();
            let pptx_hash = data["pptx_artifact_hash"].as_str().unwrap().to_owned();
            assert_ne!(pptx_hash, hash);
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
        artifacts.approve(&store, &italian).unwrap();
        assert!(tool.execute(args, &context()).await.is_ok());
    }

    #[test]
    fn artifact_paths_reject_traversal() {
        let store = ArtifactStore::new(tempfile::tempdir().unwrap().path()).unwrap();
        assert!(store.load("../../etc/passwd").is_err());
        assert_eq!(safe_filename("../../x.pdf").unwrap(), "x.pdf");
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
}
