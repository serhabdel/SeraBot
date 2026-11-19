//! The live WhatsApp linked-device clients, one per member.
//!
//! [`WhatsappLink`] starts a `whatsapp-rust` client for each member who linked
//! a number, pairs new ones (QR code or pairing code), turns allowed incoming
//! messages into turns for the member's WhatsApp bot, and delivers that bot's
//! replies back to the chat. It is a [`Notifier`]: replies in a WhatsApp
//! conversation go out over WhatsApp, every other conversation is ignored.
//!
//! Nothing here runs unless `[whatsapp_link] enabled = true`.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use serde::Serialize;
use tokio_util::sync::CancellationToken;
use whatsapp_rust::pair_code::PairCodeOptions;
use whatsapp_rust::prelude::*;
use whatsapp_rust::wacore::proto_helpers::MessageExt;
use whatsapp_rust::waproto::whatsapp as wa;

use super::{InboundDecision, decide_inbound, normalize_jid};
use crate::config::Config;
use crate::error::{Result, SeraError};
use crate::runtime::{ApprovalNotice, Notifier, Runtime};
use crate::store::Store;

/// Pairing state a console polls while the member links a number.
#[derive(Debug, Clone, Default, Serialize)]
pub struct PairingView {
    pub qr: Option<String>,
    pub code: Option<String>,
    pub expires_in_secs: Option<u64>,
    pub error: Option<String>,
}

struct Live {
    client: Arc<Client>,
    stop: CancellationToken,
}

#[derive(Default)]
struct Pairing {
    qr: Option<(String, Instant, Duration)>,
    code: Option<(String, Instant, Duration)>,
    error: Option<String>,
}

pub struct WhatsappLink {
    config: Arc<Config>,
    store: Store,
    runtime: OnceLock<Runtime>,
    live: Mutex<HashMap<String, Live>>,
    pairing: Mutex<HashMap<String, Pairing>>,
    sends: Mutex<HashMap<(String, String), Vec<Instant>>>,
}

impl WhatsappLink {
    pub fn new(config: Arc<Config>, store: Store) -> Arc<Self> {
        Arc::new(Self {
            config,
            store,
            runtime: OnceLock::new(),
            live: Mutex::new(HashMap::new()),
            pairing: Mutex::new(HashMap::new()),
            sends: Mutex::new(HashMap::new()),
        })
    }

    pub fn enabled(&self) -> bool {
        self.config.whatsapp_link.enabled
    }

    /// The runtime exists after the notifier list is built, so it is set late.
    pub fn set_runtime(&self, runtime: Runtime) {
        let _ = self.runtime.set(runtime);
    }

    fn session_path(&self, user_id: &str) -> Result<PathBuf> {
        if !crate::user_inbox::safe_user_segment(user_id) {
            return Err(SeraError::Forbidden("unsafe account id".into()));
        }
        let dir = self
            .config
            .ensure_writable_dir(&self.config.whatsapp_link.dir, "whatsapp")
            .map_err(|error| {
                SeraError::Validation(format!(
                    "WhatsApp session directory is not writable ({error}). Set [whatsapp_link] dir to a writable path."
                ))
            })?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700));
        }
        Ok(dir.join(format!("{user_id}.db")))
    }

    pub fn is_live(&self, user_id: &str) -> bool {
        self.live
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .contains_key(user_id)
    }

    pub fn pairing_view(&self, user_id: &str) -> PairingView {
        let pairing = self.pairing.lock().unwrap_or_else(|p| p.into_inner());
        let Some(state) = pairing.get(user_id) else {
            return PairingView::default();
        };
        let left = |entry: &Option<(String, Instant, Duration)>| {
            entry.as_ref().and_then(|(value, at, ttl)| {
                ttl.checked_sub(at.elapsed())
                    .map(|left| (value.clone(), left.as_secs()))
            })
        };
        let qr = left(&state.qr);
        let code = left(&state.code);
        PairingView {
            expires_in_secs: code.as_ref().or(qr.as_ref()).map(|(_, secs)| *secs),
            qr: qr.map(|(value, _)| value),
            code: code.map(|(value, _)| value),
            error: state.error.clone(),
        }
    }

    /// Start every member's linked client at boot.
    pub async fn start_linked(self: &Arc<Self>) {
        if !self.enabled() {
            return;
        }
        let users = match self.store.whatsapp_link_linked_users() {
            Ok(users) => users,
            Err(error) => {
                tracing::warn!(%error, "whatsapp link: cannot list linked members");
                return;
            }
        };
        for user in users {
            if let Err(error) = self.start(&user, None).await {
                tracing::warn!(%error, "whatsapp link: client did not start");
            }
        }
    }

    /// Start (or restart) a member's client. With `pair_phone`, a pairing
    /// code is requested for that number as well as a QR code.
    pub async fn start(self: &Arc<Self>, user_id: &str, pair_phone: Option<&str>) -> Result<()> {
        if !self.enabled() {
            return Err(SeraError::Validation(
                "WhatsApp linking is turned off on this server ([whatsapp_link] enabled)".into(),
            ));
        }
        self.stop(user_id).await;
        let path = self.session_path(user_id)?;
        let path_text = path
            .to_str()
            .ok_or_else(|| SeraError::Internal("session path is not text".into()))?;
        let backend = SqliteStore::new(path_text)
            .await
            .map_err(|error| SeraError::Internal(format!("whatsapp session store: {error}")))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
        }
        if self.store.whatsapp_link_account(user_id)?.map(|a| a.status) != Some("linked".into()) {
            self.store
                .set_whatsapp_link_status(user_id, "pairing", None)?;
        }
        self.pairing
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .insert(user_id.to_string(), Pairing::default());

        let user = user_id.to_string();
        let (link_qr, link_code, link_ok, link_out, link_msg) = (
            self.clone(),
            self.clone(),
            self.clone(),
            self.clone(),
            self.clone(),
        );
        let (user_qr, user_code, user_ok, user_out, user_msg) = (
            user.clone(),
            user.clone(),
            user.clone(),
            user.clone(),
            user.clone(),
        );
        // How the device shows in WhatsApp's Linked devices list: a browser
        // on a desktop, not the library default. Applied at pairing only.
        let os: String = self
            .config
            .whatsapp_link
            .device_os
            .chars()
            .filter(|c| c.is_ascii_alphanumeric() || *c == ' ')
            .take(24)
            .collect();
        let os = if os.trim().is_empty() {
            "Windows".to_string()
        } else {
            os
        };
        let mut builder = Bot::builder()
            .with_backend(backend)
            .with_device_props(
                whatsapp_rust::wacore::store::DevicePropsOverride::new()
                    .with_os(os)
                    .with_platform_type(wa::device_props::PlatformType::CHROME),
            )
            .on_qr_code(move |code, ttl| {
                let link = link_qr.clone();
                let user = user_qr.clone();
                async move { link.set_pairing(&user, Some(code), None, ttl) }
            })
            .on_pair_code(move |code, ttl| {
                let link = link_code.clone();
                let user = user_code.clone();
                async move { link.set_pairing(&user, None, Some(code), ttl) }
            })
            .on_connected(move |client| {
                let link = link_ok.clone();
                let user = user_ok.clone();
                async move { link.connected(&user, &client) }
            })
            .on_logged_out(move |_info| {
                let link = link_out.clone();
                let user = user_out.clone();
                async move { link.logged_out(&user).await }
            })
            .on_message(move |ctx| {
                let link = link_msg.clone();
                let user = user_msg.clone();
                async move { link.inbound(&user, ctx).await }
            });
        if let Some(phone) = pair_phone {
            let digits = normalize_jid(phone)?
                .split('@')
                .next()
                .unwrap_or_default()
                .to_string();
            builder = builder.with_pair_code(PairCodeOptions {
                phone_number: digits,
                ..Default::default()
            });
        }
        let bot = builder
            .build()
            .await
            .map_err(|error| SeraError::Internal(format!("whatsapp client: {error}")))?;
        let client = bot.client();
        let stop = CancellationToken::new();
        let mut handle = bot.spawn();
        let stopped = stop.clone();
        tokio::spawn(async move {
            tokio::select! {
                _ = &mut handle => {}
                _ = stopped.cancelled() => handle.shutdown().await,
            }
        });
        self.live
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .insert(user_id.to_string(), Live { client, stop });
        Ok(())
    }

    /// Stop a member's client without signing out.
    pub async fn stop(&self, user_id: &str) {
        let live = self
            .live
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .remove(user_id);
        if let Some(live) = live {
            live.stop.cancel();
        }
    }

    /// Sign out on WhatsApp's side, stop the client and delete the session.
    pub async fn sign_out(&self, user_id: &str) -> Result<()> {
        let live = self
            .live
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .remove(user_id);
        if let Some(live) = live {
            live.client.logout().await;
            live.stop.cancel();
        }
        self.pairing
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .remove(user_id);
        let path = self.session_path(user_id)?;
        for suffix in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{}{suffix}", path.display()));
        }
        self.store.delete_whatsapp_link(user_id)
    }

    fn set_pairing(&self, user_id: &str, qr: Option<String>, code: Option<String>, ttl: Duration) {
        let mut pairing = self.pairing.lock().unwrap_or_else(|p| p.into_inner());
        let state = pairing.entry(user_id.to_string()).or_default();
        if let Some(qr) = qr {
            state.qr = Some((qr, Instant::now(), ttl));
        }
        if let Some(code) = code {
            state.code = Some((code, Instant::now(), ttl));
        }
    }

    fn connected(&self, user_id: &str, client: &Arc<Client>) {
        self.pairing
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .remove(user_id);
        let phone = client.pn().map(|jid| jid.user.to_string());
        if let Err(error) = self
            .store
            .set_whatsapp_link_status(user_id, "linked", phone.as_deref())
        {
            tracing::warn!(%error, "whatsapp link: status not saved");
        }
        tracing::info!("whatsapp link: member's device connected");
    }

    async fn logged_out(&self, user_id: &str) {
        tracing::warn!("whatsapp link: device was logged out on the phone");
        let _ = self.sign_out(user_id).await;
    }

    /// One incoming message: allowlist, media, then a turn for the bot.
    async fn inbound(&self, user_id: &str, ctx: MessageContext) {
        if let Err(error) = self.inbound_inner(user_id, &ctx).await {
            tracing::warn!(%error, "whatsapp link: incoming message not handled");
            let _ = ctx
                .reply(&format!(
                    "Sorry, I could not handle that: {}",
                    short_error(&error)
                ))
                .await;
        }
    }

    async fn inbound_inner(&self, user_id: &str, ctx: &MessageContext) -> Result<()> {
        let allowlist = self
            .store
            .whatsapp_link_account(user_id)?
            .map(|account| account.allowlist)
            .unwrap_or_default();
        let source = &ctx.info.source;
        let chat = source.chat.to_string();
        let InboundDecision::Accept { conversation_id } = decide_inbound(
            user_id,
            &allowlist,
            &chat,
            source.is_from_me,
            source.is_group,
        ) else {
            return Ok(());
        };
        let jid = normalize_jid(&chat)?;
        let Some(runtime) = self.runtime.get() else {
            return Err(SeraError::Internal("runtime not ready".into()));
        };
        self.store
            .ensure_conversation_accepts_turns(user_id, &conversation_id)?;
        self.store
            .bind_whatsapp_link_chat(user_id, &conversation_id, &jid)?;

        let message = ctx.message.get_base_message();
        let max = self.config.whatsapp_link.max_media_bytes;
        let mut text = message
            .text_content()
            .unwrap_or_default()
            .trim()
            .to_string();
        let mut attachments = Vec::new();

        if let Some(audio) = message.audio_message.as_option() {
            check_size(audio.file_length, max)?;
            let bytes =
                ctx.client.download(audio).await.map_err(|error| {
                    SeraError::External(format!("voice note download: {error}"))
                })?;
            let transcript =
                crate::audio::transcribe_audio_bytes(&self.config, &bytes, "voice.ogg").await?;
            text = format!("[voice note] {}", transcript.trim());
        } else if let Some(document) = message.document_message.as_option() {
            check_size(document.file_length, max)?;
            let bytes = ctx
                .client
                .download(document)
                .await
                .map_err(|error| SeraError::External(format!("file download: {error}")))?;
            let name = document
                .file_name
                .clone()
                .unwrap_or_else(|| "document".into());
            let saved = crate::attachments::save_upload(
                &self.config,
                &self.store,
                user_id,
                &conversation_id,
                &name,
                &bytes,
            )?;
            attachments.push(saved.id);
            if let Some(caption) = document.caption.as_deref() {
                text = caption.trim().to_string();
            }
        } else if let Some(image) = message.image_message.as_option() {
            check_size(image.file_length, max)?;
            let bytes = ctx
                .client
                .download(image)
                .await
                .map_err(|error| SeraError::External(format!("image download: {error}")))?;
            let extension = match image.mimetype.as_deref() {
                Some("image/png") => "png",
                Some("image/webp") => "webp",
                _ => "jpg",
            };
            let saved = crate::attachments::save_upload(
                &self.config,
                &self.store,
                user_id,
                &conversation_id,
                &format!("whatsapp-image.{extension}"),
                &bytes,
            )?;
            attachments.push(saved.id);
            if let Some(caption) = image.caption.as_deref() {
                text = caption.trim().to_string();
            }
        }
        if text.is_empty() && attachments.is_empty() {
            return Ok(());
        }
        runtime.submit_text_attached(
            user_id,
            &conversation_id,
            &text,
            Some(&self.config.whatsapp_link.bot),
            &attachments,
        )?;
        Ok(())
    }

    /// Send text to one of the member's allowed chats, rate limited.
    pub async fn send_text_to(&self, user_id: &str, jid: &str, text: &str) -> Result<()> {
        let allowed = self
            .store
            .whatsapp_link_account(user_id)?
            .is_some_and(|account| account.allowlist.iter().any(|entry| entry == jid));
        if !allowed {
            return Err(SeraError::Forbidden(
                "that chat is not on your WhatsApp allowed list".into(),
            ));
        }
        self.take_send_slot(user_id, jid)?;
        let client = self
            .live
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .get(user_id)
            .map(|live| live.client.clone())
            .ok_or_else(|| SeraError::External("your WhatsApp number is not connected".into()))?;
        let to: Jid = jid
            .parse()
            .map_err(|_| SeraError::Validation("invalid WhatsApp chat".into()))?;
        client
            .send_message(to, wa::Message::text(text.to_string()))
            .await
            .map_err(|error| SeraError::External(format!("WhatsApp send failed: {error}")))?;
        Ok(())
    }

    /// Send a file to one of the member's allowed chats, rate limited like
    /// text. Images go as photos; everything else as a document.
    pub async fn send_file_to(
        &self,
        user_id: &str,
        jid: &str,
        filename: &str,
        bytes: Vec<u8>,
        caption: Option<String>,
    ) -> Result<()> {
        let allowed = self
            .store
            .whatsapp_link_account(user_id)?
            .is_some_and(|account| account.allowlist.iter().any(|entry| entry == jid));
        if !allowed {
            return Err(SeraError::Forbidden(
                "that chat is not on your WhatsApp allowed list".into(),
            ));
        }
        if bytes.len() as u64 > self.config.whatsapp_link.max_media_bytes {
            return Err(SeraError::Validation(format!(
                "'{filename}' is larger than {} MB",
                self.config.whatsapp_link.max_media_bytes / (1024 * 1024)
            )));
        }
        self.take_send_slot(user_id, jid)?;
        let client = self
            .live
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .get(user_id)
            .map(|live| live.client.clone())
            .ok_or_else(|| SeraError::External("your WhatsApp number is not connected".into()))?;
        let to: Jid = jid
            .parse()
            .map_err(|_| SeraError::Validation("invalid WhatsApp chat".into()))?;
        let mime = file_mime(filename);
        let image = mime.starts_with("image/");
        let kind = if image {
            whatsapp_rust::download::MediaType::Image
        } else {
            whatsapp_rust::download::MediaType::Document
        };
        let upload = client
            .upload(bytes, kind, Default::default())
            .await
            .map_err(|error| SeraError::External(format!("WhatsApp upload failed: {error}")))?;
        let message = if image {
            whatsapp_rust::media::image_message(
                upload,
                whatsapp_rust::media::ImageOptions {
                    caption,
                    mimetype: Some(mime.to_string()),
                    ..Default::default()
                },
            )
        } else {
            whatsapp_rust::media::document_message(
                upload,
                whatsapp_rust::media::DocumentOptions {
                    mimetype: Some(mime.to_string()),
                    file_name: Some(filename.to_string()),
                    caption,
                    ..Default::default()
                },
            )
        };
        client
            .send_message(to, message)
            .await
            .map_err(|error| SeraError::External(format!("WhatsApp send failed: {error}")))?;
        Ok(())
    }

    fn take_send_slot(&self, user_id: &str, jid: &str) -> Result<()> {
        let limit = self.config.whatsapp_link.sends_per_minute.max(1) as usize;
        let mut sends = self.sends.lock().unwrap_or_else(|p| p.into_inner());
        let window = sends
            .entry((user_id.to_string(), jid.to_string()))
            .or_default();
        window.retain(|at| at.elapsed() < Duration::from_secs(60));
        if window.len() >= limit {
            return Err(SeraError::Validation(
                "too many WhatsApp messages to this chat in the last minute".into(),
            ));
        }
        // Daily cap per member across all chats (0.63), so requested replies
        // and media can go out without a prompt and still stay bounded.
        let daily = self.config.whatsapp_link.sends_per_day.max(1) as usize;
        let day_key = (user_id.to_string(), String::new());
        let today_len = {
            let day = sends.entry(day_key.clone()).or_default();
            day.retain(|at| at.elapsed() < Duration::from_secs(24 * 60 * 60));
            day.len()
        };
        if today_len >= daily {
            return Err(SeraError::Validation(
                "the daily WhatsApp message limit is reached; try again tomorrow".into(),
            ));
        }
        sends
            .entry((user_id.to_string(), jid.to_string()))
            .or_default()
            .push(Instant::now());
        sends.entry(day_key).or_default().push(Instant::now());
        Ok(())
    }
}

/// MIME type from a file name, for the WhatsApp media message.
fn file_mime(filename: &str) -> &'static str {
    let lower = filename.to_ascii_lowercase();
    let ext = lower.rsplit('.').next().unwrap_or_default();
    match ext {
        "jpg" | "jpeg" => "image/jpeg",
        "png" => "image/png",
        "webp" => "image/webp",
        "pdf" => "application/pdf",
        "docx" => "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
        "xlsx" => "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
        "pptx" => "application/vnd.openxmlformats-officedocument.presentationml.presentation",
        "csv" => "text/csv",
        "txt" | "md" => "text/plain",
        "mp3" => "audio/mpeg",
        "ogg" => "audio/ogg",
        "mp4" => "video/mp4",
        "zip" => "application/zip",
        _ => "application/octet-stream",
    }
}

fn check_size(length: Option<u64>, max: u64) -> Result<()> {
    if length.unwrap_or(0) > max {
        return Err(SeraError::Validation(format!(
            "that file is larger than {} MB",
            max / (1024 * 1024)
        )));
    }
    Ok(())
}

/// A short, safe line for the chat: no paths, no provider bodies.
fn short_error(error: &SeraError) -> String {
    match error {
        SeraError::Validation(message) | SeraError::Forbidden(message) => message.clone(),
        _ => "an internal error; it was logged".into(),
    }
}

#[async_trait]
impl Notifier for WhatsappLink {
    async fn send_text(&self, user_id: &str, conversation_id: &str, text: &str) -> Result<()> {
        let Some(jid) = self.store.whatsapp_link_chat(user_id, conversation_id)? else {
            return Ok(());
        };
        self.send_text_to(user_id, &jid, text).await
    }

    async fn approval_requested(
        &self,
        user_id: &str,
        conversation_id: &str,
        notice: &ApprovalNotice,
    ) -> Result<()> {
        let Some(jid) = self.store.whatsapp_link_chat(user_id, conversation_id)? else {
            return Ok(());
        };
        let line = format!(
            "Waiting for your approval in SeraBot: {}",
            notice.action.tool
        );
        self.send_text_to(user_id, &jid, &line).await
    }
}

/// `whatsapp_link_reply_file` (0.63): send one of the member's files into the
/// WhatsApp chat the current conversation came from. Only that chat: the
/// tool takes no recipient, so it is a reply the member asked for (like the
/// automatic text reply), not a new message to someone. Not approval-gated;
/// bounded by the allowlist, the per-minute and daily send limits, the
/// artifact ownership check and the blocklist on the filename and caption.
pub struct WhatsappLinkReplyFileTool {
    link: Arc<WhatsappLink>,
    artifacts: Arc<crate::maker::ArtifactStore>,
    policy: Arc<crate::policy::Policy>,
}

impl WhatsappLinkReplyFileTool {
    pub fn new(
        link: Arc<WhatsappLink>,
        artifacts: Arc<crate::maker::ArtifactStore>,
        policy: Arc<crate::policy::Policy>,
    ) -> Self {
        Self {
            link,
            artifacts,
            policy,
        }
    }
}

#[async_trait]
impl crate::gateway::Tool for WhatsappLinkReplyFileTool {
    fn spec(&self) -> crate::gateway::ToolSpec {
        crate::gateway::ToolSpec {
            name: super::REPLY_FILE_TOOL.into(),
            description: "Send a file (a document, image or media the user's bots made or \
                          the user uploaded) back into this WhatsApp chat. Use it when the \
                          user asks over WhatsApp for a file. Find the hash with \
                          conversation_library, or ask the teammate who made it."
                .into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "artifact_hash": {"type": "string", "description": "SHA-256 of the file"},
                    "caption": {"type": "string", "description": "Optional short caption"}
                },
                "required": ["artifact_hash"],
                "additionalProperties": false
            }),
            external_effect: true,
        }
    }

    async fn execute(
        &self,
        args: serde_json::Value,
        context: &crate::gateway::ToolContext,
    ) -> Result<crate::gateway::ToolOutput> {
        let hash = args
            .get("artifact_hash")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| SeraError::Validation("artifact_hash is required".into()))?;
        let caption = args
            .get("caption")
            .and_then(serde_json::Value::as_str)
            .map(str::trim)
            .filter(|text| !text.is_empty())
            .map(|text| text.chars().take(1000).collect::<String>());
        let conversation = context.conversation_id.as_deref().ok_or_else(|| {
            SeraError::Validation("this only works inside a WhatsApp conversation".into())
        })?;
        let Some(jid) = self
            .link
            .store
            .whatsapp_link_chat(&context.user_id, conversation)?
        else {
            return Err(SeraError::Validation(
                "this conversation did not come from WhatsApp; the user can ask from \
                 WhatsApp, or use another delivery"
                    .into(),
            ));
        };
        if !self.link.store.user_owns_artifact(&context.user_id, hash)? {
            return Err(SeraError::NotFound("artifact".into()));
        }
        let artifact = self.artifacts.load(hash)?;
        if !self.policy.is_allowed(&artifact.filename)
            || caption
                .as_deref()
                .is_some_and(|text| !self.policy.is_allowed(text))
        {
            return Err(SeraError::PolicyViolation);
        }
        let filename = artifact.filename.clone();
        self.link
            .send_file_to(&context.user_id, &jid, &filename, artifact.bytes, caption)
            .await?;
        Ok(crate::gateway::ToolOutput {
            content: format!("Sent {filename} on WhatsApp."),
            artifact_hash: Some(artifact.hash),
            filename: Some(filename),
            destination: Some(format!("whatsapp:{jid}")),
            ..Default::default()
        })
    }
}

/// `whatsapp_link_send`: a bot sends text to one of the member's allowed
/// WhatsApp chats on its own initiative. On the gateway's approval list; the
/// approval binds the chat (destination) and the canonical text.
pub struct WhatsappLinkSendTool {
    link: Arc<WhatsappLink>,
}

impl WhatsappLinkSendTool {
    pub fn new(link: Arc<WhatsappLink>) -> Self {
        Self { link }
    }
}

fn send_args(args: &serde_json::Value) -> Result<(String, String)> {
    let to = args
        .get("to")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| SeraError::Validation("to is required".into()))?;
    let text = args
        .get("text")
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .ok_or_else(|| SeraError::Validation("text is required".into()))?;
    if text.chars().count() > 4000 {
        return Err(SeraError::Validation(
            "text is longer than 4000 characters".into(),
        ));
    }
    Ok((normalize_jid(to)?, text.to_string()))
}

#[async_trait]
impl crate::gateway::Tool for WhatsappLinkSendTool {
    fn spec(&self) -> crate::gateway::ToolSpec {
        crate::gateway::ToolSpec {
            name: super::SEND_TOOL.into(),
            description: "Send a WhatsApp message from the user's linked number to one of their \
                          allowed chats. Needs the user's approval. Replies to a WhatsApp \
                          conversation go back automatically; use this only to start a message."
                .into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "to": {"type": "string", "description": "Phone number with country code, on the user's allowed list"},
                    "text": {"type": "string", "description": "Message text"}
                },
                "required": ["to", "text"],
                "additionalProperties": false
            }),
            external_effect: true,
        }
    }

    fn approval_details(
        &self,
        args: &serde_json::Value,
    ) -> Result<crate::gateway::ApprovalDetails> {
        let (jid, text) = send_args(args)?;
        let preview: String = text.chars().take(120).collect();
        Ok(crate::gateway::ApprovalDetails {
            summary: format!("Send a WhatsApp message: \"{preview}\""),
            destination: Some(format!("whatsapp:{jid}")),
            artifact_hash: None,
        })
    }

    async fn execute(
        &self,
        args: serde_json::Value,
        context: &crate::gateway::ToolContext,
    ) -> Result<crate::gateway::ToolOutput> {
        let (jid, text) = send_args(&args)?;
        self.link
            .send_text_to(&context.user_id, &jid, &text)
            .await?;
        Ok(crate::gateway::ToolOutput {
            content: "Sent on WhatsApp.".into(),
            destination: Some(format!("whatsapp:{jid}")),
            ..Default::default()
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gateway::Tool;

    fn link() -> (tempfile::TempDir, Arc<WhatsappLink>, String) {
        let dir = tempfile::tempdir().unwrap();
        let mut config = Config {
            base_dir: dir.path().to_path_buf(),
            ..Config::default()
        };
        config.whatsapp_link.enabled = true;
        config.whatsapp_link.sends_per_minute = 2;
        let store = Store::open_in_memory(8).unwrap();
        let admin = store.admin_user_id().unwrap();
        (dir, WhatsappLink::new(Arc::new(config), store), admin)
    }

    #[tokio::test]
    async fn sends_only_reach_allowed_chats_and_are_rate_limited() {
        let (_dir, link, admin) = link();
        let jid = "33612345678@s.whatsapp.net";
        let refused = link.send_text_to(&admin, jid, "hi").await.unwrap_err();
        assert!(matches!(refused, SeraError::Forbidden(_)), "{refused:?}");

        link.store
            .set_whatsapp_link_allowlist(&admin, &[jid.to_string()])
            .unwrap();
        // Allowed, but no device is connected: it fails without sending.
        let offline = link.send_text_to(&admin, jid, "hi").await.unwrap_err();
        assert!(matches!(offline, SeraError::External(_)), "{offline:?}");
        link.send_text_to(&admin, jid, "hi").await.unwrap_err();
        let limited = link.send_text_to(&admin, jid, "hi").await.unwrap_err();
        assert!(
            matches!(&limited, SeraError::Validation(m) if m.contains("too many")),
            "{limited:?}"
        );
    }

    fn reply_context(admin: &str, conversation: &str) -> crate::gateway::ToolContext {
        crate::gateway::ToolContext {
            user_id: admin.to_string(),
            actor: admin.to_string(),
            bot: "whatsapp".into(),
            task_id: None,
            turn_id: None,
            conversation_id: Some(conversation.to_string()),
            allowed_tools: [super::super::REPLY_FILE_TOOL.to_string()]
                .into_iter()
                .collect(),
            require_approval: Default::default(),
        }
    }

    /// 0.63: a file goes back only into the WhatsApp chat the conversation
    /// came from, only if the member owns it, and never past the blocklist.
    #[tokio::test]
    async fn reply_file_stays_in_its_own_chat_and_checks_ownership() {
        let (dir, link, admin) = link();
        let artifacts = Arc::new(crate::maker::ArtifactStore::new(dir.path().join("a")).unwrap());
        let saved = artifacts
            .save(
                b"%PDF-1.4 test",
                "report.pdf",
                crate::maker::ArtifactMeta {
                    title: "report".into(),
                    filename: "report.pdf".into(),
                    language: crate::domain::Language::En,
                    source_artifact: None,
                    created_at: chrono::Utc::now(),
                },
            )
            .unwrap();
        let tool = WhatsappLinkReplyFileTool::new(
            link.clone(),
            artifacts,
            Arc::new(crate::policy::Policy::from_text("report")),
        );
        let args = serde_json::json!({"artifact_hash": saved.hash});
        // Not a WhatsApp conversation: refused.
        link.store
            .ensure_conversation_accepts_turns(&admin, "dir:main")
            .unwrap();
        let elsewhere = tool
            .execute(args.clone(), &reply_context(&admin, "dir:main"))
            .await
            .unwrap_err();
        assert!(
            matches!(elsewhere, SeraError::Validation(_)),
            "{elsewhere:?}"
        );
        // A WhatsApp conversation, but the file is not the member's.
        let jid = "33612345678@s.whatsapp.net";
        link.store
            .set_whatsapp_link_allowlist(&admin, &[jid.to_string()])
            .unwrap();
        let conversation = super::super::conversation_id_for(&admin, jid);
        link.store
            .bind_whatsapp_link_chat(&admin, &conversation, jid)
            .unwrap();
        let foreign = tool
            .execute(args.clone(), &reply_context(&admin, &conversation))
            .await
            .unwrap_err();
        assert!(matches!(foreign, SeraError::NotFound(_)), "{foreign:?}");
        // Owned, but the filename hits the blocklist.
        link.store
            .record_conversation_artifact(
                &admin,
                &conversation,
                None,
                "deck_build",
                &saved.hash,
                "report.pdf",
                None,
            )
            .unwrap();
        let blocked = tool
            .execute(args, &reply_context(&admin, &conversation))
            .await
            .unwrap_err();
        assert!(matches!(blocked, SeraError::PolicyViolation), "{blocked:?}");
        // The tool is a reply, not a new message: never approval-gated.
        assert!(!crate::gateway::is_destructive_tool(
            super::super::REPLY_FILE_TOOL
        ));
    }

    #[tokio::test]
    async fn the_daily_cap_spans_every_chat() {
        let (_dir, link, admin) = link();
        let mut config = (*link.config).clone();
        config.whatsapp_link.sends_per_day = 2;
        config.whatsapp_link.sends_per_minute = 10;
        let link = WhatsappLink::new(Arc::new(config), link.store.clone());
        link.take_send_slot(&admin, "a@s.whatsapp.net").unwrap();
        link.take_send_slot(&admin, "b@s.whatsapp.net").unwrap();
        let capped = link.take_send_slot(&admin, "c@s.whatsapp.net").unwrap_err();
        assert!(
            matches!(&capped, SeraError::Validation(m) if m.contains("daily")),
            "{capped:?}"
        );
    }

    #[tokio::test]
    async fn the_notifier_ignores_conversations_that_are_not_whatsapp_chats() {
        let (_dir, link, admin) = link();
        link.send_text(&admin, "dir:main", "hello").await.unwrap();
    }

    #[test]
    fn send_approval_binds_the_chat_and_the_text() {
        let (_dir, link, _admin) = link();
        let tool = WhatsappLinkSendTool::new(link);
        assert!(crate::gateway::is_destructive_tool(&tool.spec().name));
        let details = tool
            .approval_details(&serde_json::json!({"to": "+33 6 12 34 56 78", "text": "On my way"}))
            .unwrap();
        assert_eq!(
            details.destination.as_deref(),
            Some("whatsapp:33612345678@s.whatsapp.net")
        );
        assert!(details.summary.contains("On my way"));
        assert!(
            tool.approval_details(&serde_json::json!({"to": "120363@g.us", "text": "x"}))
                .is_err()
        );
    }

    #[tokio::test]
    async fn starting_is_refused_while_linking_is_off() {
        let (_dir, link, admin) = link();
        let mut config = (*link.config).clone();
        config.whatsapp_link.enabled = false;
        let off = WhatsappLink::new(Arc::new(config), link.store.clone());
        assert!(off.start(&admin, None).await.is_err());
        assert!(!off.is_live(&admin));
    }
}
