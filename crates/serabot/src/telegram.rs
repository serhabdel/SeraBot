use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use secrecy::{ExposeSecret, SecretString};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;
use tracing::{error, info, warn};

use crate::error::{Result, SeraError};
use crate::gateway::Decision;
use crate::maker::ArtifactStore;
use crate::memory;
use crate::runtime::{ApprovalNotice, Notifier, Runtime};
use crate::store::Store;

const MAX_MESSAGE_CHARS: usize = 3900;

#[derive(Clone)]
pub struct TelegramClient {
    http: reqwest::Client,
    base_url: String,
    token: SecretString,
    owner_user_id: i64,
    owner_chat_id: i64,
}

impl std::fmt::Debug for TelegramClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TelegramClient")
            .field("owner_user_id", &self.owner_user_id)
            .field("owner_chat_id", &self.owner_chat_id)
            .finish()
    }
}

impl TelegramClient {
    pub fn new(token: impl Into<String>, owner_user_id: i64, owner_chat_id: i64) -> Result<Self> {
        let token = token.into();
        if token.is_empty()
            || token.len() > 200
            || !token.contains(':')
            || !token
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, ':' | '-' | '_'))
        {
            return Err(SeraError::Config("invalid Telegram bot token".into()));
        }
        if owner_user_id <= 0 || owner_chat_id == 0 {
            return Err(SeraError::Config(
                "Telegram owner user and chat ids are required".into(),
            ));
        }
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(45))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|error| SeraError::External(error.to_string()))?;
        Ok(Self {
            http,
            base_url: "https://api.telegram.org".into(),
            token: SecretString::new(token),
            owner_user_id,
            owner_chat_id,
        })
    }

    pub fn with_base_url(mut self, base_url: impl Into<String>) -> Self {
        self.base_url = base_url.into();
        self
    }

    pub fn owner_chat_id(&self) -> i64 {
        self.owner_chat_id
    }

    fn url(&self, method: &str) -> String {
        format!(
            "{}/bot{}/{}",
            self.base_url,
            self.token.expose_secret(),
            method
        )
    }

    fn is_owner(&self, from_id: i64, chat_id: i64) -> bool {
        from_id == self.owner_user_id && chat_id == self.owner_chat_id
    }

    pub async fn get_updates(&self, offset: i64, timeout_secs: u64) -> Result<Vec<Update>> {
        let response: TelegramResponse<Vec<Update>> = self
            .http
            .post(self.url("getUpdates"))
            .json(&json!({
                "offset": offset,
                "timeout": timeout_secs,
                "allowed_updates": ["message", "callback_query"],
            }))
            .send()
            .await
            .map_err(|error| SeraError::External(format!("Telegram getUpdates failed: {error}")))?
            .json()
            .await
            .map_err(|error| {
                SeraError::External(format!("Telegram getUpdates response: {error}"))
            })?;
        if !response.ok {
            return Err(SeraError::External(format!(
                "Telegram getUpdates rejected: {}",
                response
                    .description
                    .unwrap_or_else(|| "unknown error".into())
            )));
        }
        Ok(response.result.unwrap_or_default())
    }

    pub async fn send_message(&self, chat_id: i64, text: &str) -> Result<()> {
        for part in split_message(text) {
            let response: TelegramResponse<Value> = self
                .http
                .post(self.url("sendMessage"))
                .json(&json!({
                    "chat_id": chat_id,
                    "text": part,
                    "disable_web_page_preview": true,
                }))
                .send()
                .await
                .map_err(|error| {
                    SeraError::External(format!("Telegram sendMessage failed: {error}"))
                })?
                .json()
                .await
                .map_err(|error| {
                    SeraError::External(format!("Telegram sendMessage response: {error}"))
                })?;
            if !response.ok {
                return Err(SeraError::External(format!(
                    "Telegram sendMessage rejected: {}",
                    response
                        .description
                        .unwrap_or_else(|| "unknown error".into())
                )));
            }
        }
        Ok(())
    }

    pub async fn send_approval(&self, notice: &ApprovalNotice) -> Result<()> {
        let action = &notice.action;
        let approval = &notice.approval;
        let mut text = String::new();
        text.push_str("Approval required\n\n");
        text.push_str(&approval.summary);
        text.push_str("\n\nTool: ");
        text.push_str(&approval.tool);
        if let Some(destination) = &action.destination {
            text.push_str("\nDestination: ");
            text.push_str(destination);
        }
        if let Some(hash) = &action.artifact_hash {
            text.push_str("\nArtifact SHA-256: ");
            text.push_str(&hash.chars().take(16).collect::<String>());
            text.push_str("...");
        }
        text.push_str(&format!(
            "\nExpires: {}",
            approval.expires_at.format("%Y-%m-%d %H:%M UTC")
        ));
        text.push_str(
            "\n\nOnce = this exact payload only. Always = revocable grant for this exact payload.",
        );

        let keyboard = json!({
            "inline_keyboard": [[
                {"text": "Approve once", "callback_data": format!("ap:once:{}", approval.id)},
                {"text": "Always allow", "callback_data": format!("ap:always:{}", approval.id)},
                {"text": "Deny", "callback_data": format!("ap:deny:{}", approval.id)}
            ]]
        });
        let response: TelegramResponse<Value> = self
            .http
            .post(self.url("sendMessage"))
            .json(&json!({
                "chat_id": self.owner_chat_id,
                "text": text,
                "reply_markup": keyboard,
                "disable_web_page_preview": true,
            }))
            .send()
            .await
            .map_err(|error| {
                SeraError::External(format!("Telegram approval send failed: {error}"))
            })?
            .json()
            .await
            .map_err(|error| SeraError::External(format!("Telegram approval response: {error}")))?;
        if !response.ok {
            return Err(SeraError::External(format!(
                "Telegram approval rejected: {}",
                response
                    .description
                    .unwrap_or_else(|| "unknown error".into())
            )));
        }
        Ok(())
    }

    pub async fn answer_callback(&self, callback_id: &str, text: &str) -> Result<()> {
        let _: TelegramResponse<Value> = self
            .http
            .post(self.url("answerCallbackQuery"))
            .json(&json!({
                "callback_query_id": callback_id,
                "text": text,
                "show_alert": false,
            }))
            .send()
            .await
            .map_err(|error| {
                SeraError::External(format!("Telegram callback answer failed: {error}"))
            })?
            .json()
            .await
            .map_err(|error| SeraError::External(format!("Telegram callback response: {error}")))?;
        Ok(())
    }

    pub async fn send_document(
        &self,
        chat_id: i64,
        filename: &str,
        bytes: Vec<u8>,
        caption: &str,
    ) -> Result<()> {
        let safe_name: String = filename
            .chars()
            .filter(|c| !c.is_control() && !matches!(c, '/' | '\\'))
            .take(180)
            .collect();
        let part = reqwest::multipart::Part::bytes(bytes)
            .file_name(if safe_name.is_empty() {
                "artifact.pdf".into()
            } else {
                safe_name
            })
            .mime_str("application/pdf")
            .map_err(|error| SeraError::External(error.to_string()))?;
        let form = reqwest::multipart::Form::new()
            .text("chat_id", chat_id.to_string())
            .text("caption", caption.chars().take(1000).collect::<String>())
            .part("document", part);
        let response: TelegramResponse<Value> = self
            .http
            .post(self.url("sendDocument"))
            .multipart(form)
            .send()
            .await
            .map_err(|error| SeraError::External(format!("Telegram sendDocument failed: {error}")))?
            .json()
            .await
            .map_err(|error| {
                SeraError::External(format!("Telegram sendDocument response: {error}"))
            })?;
        if !response.ok {
            return Err(SeraError::External(format!(
                "Telegram sendDocument rejected: {}",
                response
                    .description
                    .unwrap_or_else(|| "unknown error".into())
            )));
        }
        Ok(())
    }

    pub async fn run_polling(
        self: Arc<Self>,
        runtime: Runtime,
        store: Store,
        artifacts: Option<Arc<ArtifactStore>>,
        shutdown: CancellationToken,
    ) {
        let mut offset = store
            .kv_get("telegram_offset")
            .ok()
            .flatten()
            .and_then(|value| value.parse::<i64>().ok())
            .unwrap_or(0);
        info!("Telegram polling started");
        loop {
            if shutdown.is_cancelled() {
                break;
            }
            match self.get_updates(offset, 25).await {
                Ok(updates) => {
                    for update in updates {
                        let next = update.update_id + 1;
                        if next > offset {
                            offset = next;
                            if let Err(error) = store.kv_set("telegram_offset", &offset.to_string())
                            {
                                error!(%error, "failed to persist Telegram offset");
                            }
                        }
                        if let Err(error) = self
                            .handle_update(&runtime, &store, artifacts.as_deref(), update)
                            .await
                        {
                            warn!(%error, "Telegram update failed");
                            let _ = self
                                .send_message(
                                    self.owner_chat_id,
                                    "This request could not be processed. See the audit log.",
                                )
                                .await;
                        }
                    }
                }
                Err(error) => {
                    warn!(%error, "Telegram polling failed");
                    tokio::time::sleep(Duration::from_secs(5)).await;
                }
            }
        }
        info!("Telegram polling stopped");
    }

    async fn handle_update(
        &self,
        runtime: &Runtime,
        store: &Store,
        artifacts: Option<&ArtifactStore>,
        update: Update,
    ) -> Result<()> {
        if !store.record_telegram_update(update.update_id)? {
            return Ok(());
        }
        if let Some(message) = update.message {
            let Some(from) = &message.from else {
                return Ok(());
            };
            if !self.is_owner(from.id, message.chat.id) {
                warn!("ignored Telegram message from non-owner");
                return Ok(());
            }
            let text = message.text.unwrap_or_default();
            let text = text.trim();
            if text.is_empty() {
                let _ = self
                    .send_message(
                        self.owner_chat_id,
                        "Text messages are supported; document ingestion uses the owner API.",
                    )
                    .await;
                return Ok(());
            }
            if let Some(reply) = model_command(runtime, &message.chat.id.to_string(), text)? {
                self.send_message(self.owner_chat_id, &reply).await?;
                return Ok(());
            }
            match text {
                "/start" => {
                    self.send_message(
                        self.owner_chat_id,
                        "SeraBot ready. Use /ask <question>, /status, /approvals, or /revoke.\n\
                         All external actions require explicit approval.",
                    )
                    .await?;
                }
                "/status" => {
                    let pending = store.pending_task_count()?;
                    let approvals = store.list_pending_approvals(20)?;
                    let mut reply = format!(
                        "Pending tasks: {pending}\nPending approvals: {}\n",
                        approvals.len()
                    );
                    for approval in approvals {
                        reply.push_str(&format!(
                            "- {} [{}] expires {}\n",
                            approval.summary,
                            approval.id,
                            approval.expires_at.format("%Y-%m-%d %H:%M UTC")
                        ));
                    }
                    self.send_message(self.owner_chat_id, &reply).await?;
                }
                "/approvals" => {
                    let approvals = store.list_pending_approvals(20)?;
                    if approvals.is_empty() {
                        self.send_message(self.owner_chat_id, "No pending approvals.")
                            .await?;
                    } else {
                        for approval in approvals {
                            let action = store.get_action(&approval.action_id)?;
                            let notice = ApprovalNotice { approval, action };
                            self.send_approval(&notice).await?;
                        }
                    }
                }
                "/revoke" => {
                    let count = store.revoke_grants()?;
                    store.record_audit(
                        "owner",
                        "approval.grants_revoked",
                        None,
                        None,
                        None,
                        None,
                        None,
                        Some(&json!({"count": count})),
                    )?;
                    self.send_message(
                        self.owner_chat_id,
                        &format!("Revoked {count} standing grant(s)."),
                    )
                    .await?;
                }
                "/help" => {
                    self.send_message(
                        self.owner_chat_id,
                        "/ask <question> — cited document Q&A\n\
                         /deck <brief> — Italian deck draft\n\
                         /xlsx <request> — spreadsheet analysis\n\
                         /timesheets <request> — timesheet analysis\n\
                         /brief — morning brief\n\
                         /status — queue and approvals\n\
                         /models — configured providers and models\n\
                         /model <provider> <model> — select for this conversation\n\
                         /approvals — resend pending approvals\n\
                         /approve-deck <sha256> — approve Italian deck content for French\n\
                         /revoke — revoke all standing grants",
                    )
                    .await?;
                }
                _ if text.starts_with("/approve-deck") => {
                    let hash = text.trim_start_matches("/approve-deck").trim();
                    let Some(artifacts) = artifacts else {
                        self.send_message(self.owner_chat_id, "Artifact store is unavailable.")
                            .await?;
                        return Ok(());
                    };
                    artifacts.approve(store, hash)?;
                    self.send_message(
                        self.owner_chat_id,
                        "Italian deck approved for French generation.",
                    )
                    .await?;
                }
                _ if text.starts_with("/remember") => {
                    let content = text.trim_start_matches("/remember").trim();
                    if content.is_empty() {
                        self.send_message(self.owner_chat_id, "usage: /remember <fact>")
                            .await?;
                        return Ok(());
                    }
                    let memory = memory::add(store, "fact", content, "owner")?;
                    store.record_audit(
                        "owner",
                        "memory.added",
                        None,
                        None,
                        None,
                        None,
                        None,
                        Some(&json!({"memory_id": memory.id})),
                    )?;
                    self.send_message(self.owner_chat_id, &format!("Remembered as {}.", memory.id))
                        .await?;
                }
                "/memories" => {
                    let memories = memory::list(store, 20)?;
                    if memories.is_empty() {
                        self.send_message(self.owner_chat_id, "No stored facts.")
                            .await?;
                    } else {
                        let mut reply = String::from("Stored facts:\n");
                        for memory in memories {
                            reply.push_str(&format!(
                                "- [{}] {}\n",
                                memory.id,
                                memory.content.chars().take(300).collect::<String>()
                            ));
                        }
                        self.send_message(self.owner_chat_id, &reply).await?;
                    }
                }
                _ if text.starts_with("/forget") => {
                    let id = text.trim_start_matches("/forget").trim();
                    let deleted = memory::delete(store, id)?;
                    store.record_audit(
                        "owner",
                        "memory.deleted",
                        None,
                        None,
                        None,
                        None,
                        None,
                        Some(&json!({"memory_id": id, "deleted": deleted})),
                    )?;
                    self.send_message(
                        self.owner_chat_id,
                        if deleted {
                            "Fact deleted."
                        } else {
                            "No such fact."
                        },
                    )
                    .await?;
                }
                _ => {
                    runtime.submit_text(&message.chat.id.to_string(), text, None)?;
                }
            }
            return Ok(());
        }

        if let Some(callback) = update.callback_query {
            if callback.from.id != self.owner_user_id {
                let _ = self.answer_callback(&callback.id, "Not authorized.").await;
                return Ok(());
            }
            if let Some(message) = &callback.message {
                if message.chat.id != self.owner_chat_id {
                    let _ = self.answer_callback(&callback.id, "Not authorized.").await;
                    return Ok(());
                }
            }
            let data = callback.data.unwrap_or_default();
            let Some((decision, approval_id)) = parse_callback(&data) else {
                let _ = self.answer_callback(&callback.id, "Invalid action.").await;
                return Ok(());
            };
            match runtime.handle_approval(approval_id, decision).await {
                Ok(result) => {
                    let label = match result.approval.state.as_str() {
                        "denied" => "Denied",
                        _ => "Approved",
                    };
                    let _ = self.answer_callback(&callback.id, label).await;
                }
                Err(error) => {
                    warn!(%error, "approval callback failed");
                    let _ = self
                        .answer_callback(&callback.id, "Approval could not be applied.")
                        .await;
                }
            }
        }
        Ok(())
    }
}

#[async_trait]
impl Notifier for TelegramClient {
    async fn send_text(&self, _conversation_id: &str, text: &str) -> Result<()> {
        self.send_message(self.owner_chat_id, text).await
    }

    async fn approval_requested(
        &self,
        _conversation_id: &str,
        notice: &ApprovalNotice,
    ) -> Result<()> {
        self.send_approval(notice).await
    }
}

#[derive(Debug, Deserialize)]
struct TelegramResponse<T> {
    ok: bool,
    result: Option<T>,
    description: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Update {
    pub update_id: i64,
    pub message: Option<TelegramMessage>,
    pub callback_query: Option<CallbackQuery>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct TelegramMessage {
    pub message_id: i64,
    pub chat: Chat,
    pub from: Option<TelegramUser>,
    pub text: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Chat {
    pub id: i64,
    #[serde(rename = "type")]
    pub kind: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct TelegramUser {
    pub id: i64,
    #[serde(default)]
    pub is_bot: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CallbackQuery {
    pub id: String,
    pub from: TelegramUser,
    pub data: Option<String>,
    pub message: Option<TelegramMessage>,
}

fn model_command(runtime: &Runtime, conversation_id: &str, text: &str) -> Result<Option<String>> {
    let parts: Vec<_> = text.split_whitespace().collect();
    let reply = match parts.as_slice() {
        ["/models"] => {
            let mut reply = String::from("Configured providers and models:\n");
            for provider in runtime.model_catalog()? {
                let state = if provider.data_egress_approved {
                    "approved"
                } else {
                    "data sharing not approved"
                };
                reply.push_str(&format!(
                    "{} ({state}): {}\n",
                    provider.id,
                    provider.models.join(", ")
                ));
            }
            reply.push_str("\n/model <provider> <model> selects for this conversation. Future turns include this conversation's history; running turns keep their original selection.");
            reply
        }
        ["/model"] => {
            let selection = runtime.model_selection(conversation_id, "main")?;
            format!(
                "Current selection: {} / {}\nUse /models to list configured choices.",
                selection.provider, selection.model
            )
        }
        ["/model", provider, model] => {
            match runtime.select_model(conversation_id, crate::model_router::ModelSelection {
                provider: (*provider).to_owned(), model: (*model).to_owned(),
            }) {
                Ok(selection) => format!("Selected {} / {} for future turns in this conversation. Existing history will be sent to this provider. Running turns are unchanged.", selection.provider, selection.model),
                Err(SeraError::Validation(_) | SeraError::NotFound(_)) => "Unknown provider or model. Use /models to see configured choices.".into(),
                Err(SeraError::Forbidden(_)) => "This provider is not approved for data sharing. Configure approval on the server before selecting it.".into(),
                Err(SeraError::PolicyViolation) => "Selection blocked by content policy.".into(),
                Err(error) => return Err(error),
            }
        }
        ["/model", ..] | ["/models", ..] => {
            "Usage: /models, /model, or /model <provider> <model>".into()
        }
        [command, ..] if command.starts_with("/model@") || command.starts_with("/models@") => {
            "Use /model or /models without an @username in the configured owner chat.".into()
        }
        _ => return Ok(None),
    };
    runtime.scan_owner_output(&reply)?;
    Ok(Some(reply))
}

fn parse_callback(data: &str) -> Option<(Decision, &str)> {
    let mut parts = data.splitn(3, ':');
    if parts.next()? != "ap" {
        return None;
    }
    let decision = match parts.next()? {
        "once" => Decision::Once,
        "always" => Decision::Always,
        "deny" => Decision::Deny,
        _ => return None,
    };
    let approval_id = parts.next()?;
    if approval_id.len() < 8 || approval_id.len() > 64 {
        return None;
    }
    Some((decision, approval_id))
}

fn split_message(text: &str) -> Vec<String> {
    if text.chars().count() <= MAX_MESSAGE_CHARS {
        return vec![text.to_owned()];
    }
    let mut parts = Vec::new();
    let mut current = String::new();
    for line in text.lines() {
        if current.chars().count() + line.chars().count() + 1 > MAX_MESSAGE_CHARS {
            if !current.is_empty() {
                parts.push(std::mem::take(&mut current));
            }
            if line.chars().count() > MAX_MESSAGE_CHARS {
                let mut chunk = String::new();
                for character in line.chars() {
                    if chunk.chars().count() >= MAX_MESSAGE_CHARS {
                        parts.push(std::mem::take(&mut chunk));
                    }
                    chunk.push(character);
                }
                if !chunk.is_empty() {
                    current = chunk;
                }
            } else {
                current.push_str(line);
                current.push('\n');
            }
        } else {
            current.push_str(line);
            current.push('\n');
        }
    }
    if !current.trim().is_empty() {
        parts.push(current);
    }
    parts
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn model_runtime() -> (Runtime, Store) {
        use crate::config::{BotConfig, Config, LoadedBot};
        use crate::gateway::{Gateway, ToolRegistry};
        use crate::llm::LlmClient;
        use crate::model_router::ModelRouter;
        use crate::policy::Policy;
        let mut config = Config::default();
        config.llm.model = "test".into();
        config.llm.base_url = "https://example.com/v1".into();
        config.llm.models = vec!["alternative".into()];
        config.llm.data_egress_approved = true;
        let store = Store::open_in_memory(384).unwrap();
        let llm = LlmClient::new("https://example.com/v1", "synthetic-key", 5, 100)
            .unwrap()
            .with_model("test");
        let router = ModelRouter::new(
            &config.llm,
            std::collections::BTreeMap::from([("default".into(), llm)]),
        )
        .unwrap();
        let bot = LoadedBot {
            config: BotConfig {
                name: "main".into(),
                trigger: "command".into(),
                model: None,
                system_prompt: "unused".into(),
                skills: vec![],
                tools: vec![],
                require_approval: vec![],
                enabled: true,
                temperature: None,
            },
            prompt: "test".into(),
        };
        let runtime = Runtime::new(
            Arc::new(config),
            store.clone(),
            Gateway::new(
                store.clone(),
                Arc::new(Policy::empty()),
                ToolRegistry::default(),
                1,
                false,
            ),
            router,
            vec![bot],
            Arc::new(crate::runtime::NullNotifier),
            tokio_util::sync::CancellationToken::new(),
        )
        .unwrap();
        (runtime, store)
    }

    #[test]
    fn model_commands_list_select_and_keep_conversations_separate() {
        let (runtime, store) = model_runtime();
        let listing = model_command(&runtime, "20", "/models").unwrap().unwrap();
        assert!(listing.contains("default"));
        assert!(listing.contains("alternative"));
        assert!(!listing.contains("synthetic-key"));
        assert!(!listing.contains("example.com"));
        let reply = model_command(&runtime, "20", "/model default alternative")
            .unwrap()
            .unwrap();
        assert!(reply.contains("alternative"));
        assert_eq!(
            runtime.model_selection("20", "main").unwrap().model,
            "alternative"
        );
        assert_eq!(runtime.model_selection("21", "main").unwrap().model, "test");
        assert_eq!(store.pending_task_count().unwrap(), 0);
    }

    #[test]
    fn malformed_model_commands_do_not_change_selection_or_capture_other_commands() {
        let (runtime, _) = model_runtime();
        for command in [
            "/model missing unknown",
            "/model default",
            "/model default alternative extra",
        ] {
            assert!(model_command(&runtime, "20", command).unwrap().is_some());
            assert_eq!(runtime.model_selection("20", "main").unwrap().model, "test");
        }
        assert!(
            model_command(&runtime, "20", "/model@ExampleBot default alternative")
                .unwrap()
                .unwrap()
                .contains("without an @username")
        );
        assert_eq!(runtime.model_selection("20", "main").unwrap().model, "test");
        assert!(
            model_command(&runtime, "20", "/modeling")
                .unwrap()
                .is_none()
        );
        assert!(model_command(&runtime, "20", "hello").unwrap().is_none());
    }

    #[tokio::test]
    async fn unauthorized_model_command_cannot_change_selection() {
        let (runtime, store) = model_runtime();
        let client = TelegramClient::new("123:abc", 10, 20).unwrap();
        let update: Update = serde_json::from_value(json!({
            "update_id": 991,
            "message": {"message_id": 1, "chat": {"id":20}, "from":{"id":99}, "text":"/model default alternative"}
        })).unwrap();
        client
            .handle_update(&runtime, &store, None, update)
            .await
            .unwrap();
        assert_eq!(runtime.model_selection("20", "main").unwrap().model, "test");
    }

    #[test]
    fn parses_approval_callbacks() {
        assert_eq!(
            parse_callback("ap:once:12345678-abcd"),
            Some((Decision::Once, "12345678-abcd"))
        );
        assert_eq!(
            parse_callback("ap:always:12345678-abcd"),
            Some((Decision::Always, "12345678-abcd"))
        );
        assert_eq!(parse_callback("ap:everything:12345678"), None);
        assert_eq!(parse_callback("bad"), None);
    }

    #[test]
    fn only_exact_owner_identity_is_accepted() {
        let client = TelegramClient::new("123:abc_DEF", 10, 20).unwrap();
        assert!(client.is_owner(10, 20));
        assert!(!client.is_owner(11, 20));
        assert!(!client.is_owner(10, 21));
    }

    #[test]
    fn splits_long_messages_without_breaking_characters() {
        let text = "é".repeat(9000);
        let parts = split_message(&text);
        assert!(parts.len() >= 3);
        assert_eq!(parts.concat().chars().filter(|c| *c == 'é').count(), 9000);
    }

    #[test]
    fn rejects_malformed_tokens() {
        assert!(TelegramClient::new("bad token", 1, 1).is_err());
        assert!(TelegramClient::new("123:abc", 0, 1).is_err());
    }

    #[tokio::test]
    async fn sends_message_through_bot_api() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/bot123:abc/sendMessage"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(json!({"ok": true, "result": {}})),
            )
            .mount(&server)
            .await;
        let client = TelegramClient::new("123:abc", 10, 20)
            .unwrap()
            .with_base_url(server.uri());
        client.send_message(20, "hello").await.unwrap();
    }

    #[tokio::test]
    async fn rejects_unauthorized_updates() {
        let client = TelegramClient::new("123:abc", 10, 20).unwrap();
        let update: Update = serde_json::from_value(json!({
            "update_id": 1,
            "message": {
                "message_id": 2,
                "chat": {"id": 99, "type": "private"},
                "from": {"id": 99},
                "text": "hello"
            }
        }))
        .unwrap();
        assert!(!client.is_owner(
            update.message.as_ref().unwrap().from.as_ref().unwrap().id,
            update.message.as_ref().unwrap().chat.id
        ));
    }
}
