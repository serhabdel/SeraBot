//! External service connectors (OAuth scaffolds + native AgentMail).

use std::fmt;
use std::sync::Arc;

use async_trait::async_trait;
use reqwest::header::{AUTHORIZATION, CONTENT_TYPE};
use reqwest::Client;
use serde_json::{Value, json};

use crate::config::ConnectorsConfig;
use crate::error::{Result, SeraError};
use crate::gateway::{ApprovalDetails, Tool, ToolContext, ToolOutput, ToolRegistry, ToolSpec};
use crate::util::truncate_chars;

const MAX_API_BYTES: usize = 512 * 1024;
const MAX_SUBJECT_LEN: usize = 500;
const MAX_BODY_LEN: usize = 50_000;
const MAX_INBOX_ID_LEN: usize = 128;

pub fn register_connector_tools(
    registry: &mut ToolRegistry,
    config: &ConnectorsConfig,
    api_key: Option<&str>,
    http: Client,
) -> Result<()> {
    if config.google_drive.oauth.enabled {
        registry.register(ConnectorScaffoldTool::new(
            "google_drive_list",
            "List Google Drive files (scaffold — OAuth pending).",
            "Google Drive",
        ))?;
    }
    if config.google_sheets.oauth.enabled {
        registry.register(ConnectorScaffoldTool::new(
            "google_sheets_read",
            "Read a Google Sheets range (scaffold — OAuth pending).",
            "Google Sheets",
        ))?;
    }
    if config.google_calendar.oauth.enabled {
        registry.register(ConnectorScaffoldTool::new(
            "google_calendar_list_events",
            "List Google Calendar events (scaffold — OAuth pending).",
            "Google Calendar",
        ))?;
    }
    if config.gmail.oauth.enabled {
        registry.register(ConnectorScaffoldTool::new(
            "gmail_list_messages",
            "List Gmail messages (scaffold — OAuth pending).",
            "Gmail",
        ))?;
    }
    if config.outlook.oauth.enabled {
        registry.register(ConnectorScaffoldTool::new(
            "outlook_list_messages",
            "List Outlook mail (scaffold — OAuth pending).",
            "Outlook",
        ))?;
    }
    if config.outlook_calendar.oauth.enabled {
        registry.register(ConnectorScaffoldTool::new(
            "outlook_calendar_list_events",
            "List Outlook Calendar events (scaffold — OAuth pending).",
            "Outlook Calendar",
        ))?;
    }
    if config.notion.oauth.enabled {
        registry.register(ConnectorScaffoldTool::new(
            "notion_query_database",
            "Query Notion (scaffold — OAuth pending).",
            "Notion",
        ))?;
    }
    if config.agentmail.enabled {
        let api_key = api_key.ok_or_else(|| {
            SeraError::Config("agentmail enabled without API key in secret store".into())
        })?;
        let client = Arc::new(AgentMailClient::new(
            http,
            &config.agentmail.api_base,
            api_key,
        )?);
        registry.register(AgentMailInboxesListTool::new(client.clone()))?;
        registry.register(AgentMailSendTool::new(
            client,
            config.agentmail.default_inbox_id.clone(),
        ))?;
    }
    Ok(())
}

struct ConnectorScaffoldTool {
    name: String,
    description: String,
    label: String,
}

impl ConnectorScaffoldTool {
    fn new(name: &str, description: &str, label: &str) -> Self {
        Self {
            name: name.into(),
            description: description.into(),
            label: label.into(),
        }
    }
}

#[async_trait]
impl Tool for ConnectorScaffoldTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: self.name.clone(),
            description: self.description.clone(),
            parameters: json!({
                "type": "object",
                "properties": {},
                "additionalProperties": false
            }),
            external_effect: false,
        }
    }

    async fn execute(&self, _args: Value, _context: &ToolContext) -> Result<ToolOutput> {
        let payload = json!({
            "status": "scaffold",
            "connector": self.label,
            "message": "OAuth/API handler not implemented yet."
        });
        Ok(ToolOutput {
            content: payload.to_string(),
            data: Some(payload),
            ..Default::default()
        })
    }
}

#[derive(Clone)]
pub struct AgentMailClient {
    http: Client,
    api_base: String,
    api_key: String,
}

impl fmt::Debug for AgentMailClient {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AgentMailClient")
            .field("api_base", &self.api_base)
            .field("api_key", &"<redacted>")
            .finish_non_exhaustive()
    }
}

impl AgentMailClient {
    pub fn new(http: Client, api_base: &str, api_key: impl Into<String>) -> Result<Self> {
        let api_key = api_key.into();
        if api_key.trim().is_empty() {
            return Err(SeraError::Config("agentmail API key must not be empty".into()));
        }
        let api_base = api_base.trim().trim_end_matches('/').to_owned();
        let loopback_http = api_base.starts_with("http://127.0.0.1")
            || api_base.starts_with("http://localhost");
        if !api_base.starts_with("https://") && !loopback_http {
            return Err(SeraError::Config(
                "agentmail.api_base must be HTTPS (http loopback allowed for tests)".into(),
            ));
        }
        Ok(Self {
            http,
            api_base,
            api_key,
        })
    }

    pub async fn list_inboxes(&self, limit: u32) -> Result<Value> {
        let limit = limit.clamp(1, 50);
        self.get_json(&format!("{}/inboxes?limit={limit}", self.api_base))
            .await
    }

    pub async fn send_message(
        &self,
        inbox_id: &str,
        to: &[String],
        subject: &str,
        text: &str,
    ) -> Result<Value> {
        let inbox_id = inbox_id.trim();
        if inbox_id.is_empty() || inbox_id.len() > MAX_INBOX_ID_LEN {
            return Err(SeraError::Validation(
                "inbox_id must be 1..128 characters".into(),
            ));
        }
        if to.is_empty() || to.len() > 20 {
            return Err(SeraError::Validation("to must have 1..20 addresses".into()));
        }
        let subject = subject.trim();
        if subject.is_empty() || subject.len() > MAX_SUBJECT_LEN {
            return Err(SeraError::Validation(
                "subject must be 1..500 characters".into(),
            ));
        }
        let text = text.trim();
        if text.is_empty() || text.len() > MAX_BODY_LEN {
            return Err(SeraError::Validation(
                "text must be 1..50000 characters".into(),
            ));
        }
        self.post_json(
            &format!("{}/inboxes/{inbox_id}/messages/send", self.api_base),
            &json!({ "to": to, "subject": subject, "text": text }),
        )
        .await
    }

    async fn get_json(&self, url: &str) -> Result<Value> {
        let response = self
            .http
            .get(url)
            .header(AUTHORIZATION, format!("Bearer {}", self.api_key))
            .send()
            .await
            .map_err(map_http)?;
        parse_api_response(response).await
    }

    async fn post_json(&self, url: &str, body: &Value) -> Result<Value> {
        let response = self
            .http
            .post(url)
            .header(AUTHORIZATION, format!("Bearer {}", self.api_key))
            .header(CONTENT_TYPE, "application/json")
            .json(body)
            .send()
            .await
            .map_err(map_http)?;
        parse_api_response(response).await
    }
}

async fn parse_api_response(response: reqwest::Response) -> Result<Value> {
    let status = response.status();
    let bytes = response.bytes().await.map_err(map_http)?.to_vec();
    if bytes.len() > MAX_API_BYTES {
        return Err(SeraError::External(
            "agentmail response exceeded size limit".into(),
        ));
    }
    if !status.is_success() {
        let detail = truncate_chars(&String::from_utf8_lossy(&bytes), 400);
        return Err(SeraError::External(format!(
            "agentmail HTTP {status}: {detail}"
        )));
    }
    serde_json::from_slice(&bytes)
        .map_err(|error| SeraError::External(format!("malformed agentmail JSON: {error}")))
}

struct AgentMailInboxesListTool {
    client: Arc<AgentMailClient>,
}

impl AgentMailInboxesListTool {
    fn new(client: Arc<AgentMailClient>) -> Self {
        Self { client }
    }
}

#[async_trait]
impl Tool for AgentMailInboxesListTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "agentmail_inboxes_list".into(),
            description: "List AgentMail inboxes (untrusted metadata).".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "limit": {"type": "integer"}
                },
                "additionalProperties": false
            }),
            external_effect: true,
        }
    }

    fn approval_details(&self, _args: &Value) -> Result<ApprovalDetails> {
        Ok(ApprovalDetails {
            summary: "List AgentMail inboxes".into(),
            destination: Some(self.client.api_base.clone()),
            artifact_hash: None,
        })
    }

    async fn execute(&self, args: Value, _context: &ToolContext) -> Result<ToolOutput> {
        let limit = args.get("limit").and_then(Value::as_u64).unwrap_or(10) as u32;
        let value = self.client.list_inboxes(limit).await?;
        Ok(ToolOutput {
            content: value.to_string(),
            data: Some(value),
            destination: Some(self.client.api_base.clone()),
            ..Default::default()
        })
    }
}

struct AgentMailSendTool {
    client: Arc<AgentMailClient>,
    default_inbox_id: String,
}

impl AgentMailSendTool {
    fn new(client: Arc<AgentMailClient>, default_inbox_id: String) -> Self {
        Self {
            client,
            default_inbox_id,
        }
    }
}

#[async_trait]
impl Tool for AgentMailSendTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "agentmail_send".into(),
            description: "Send email via AgentMail (owner approval required).".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "inbox_id": {"type": "string"},
                    "to": {"type": "array", "items": {"type": "string"}},
                    "subject": {"type": "string"},
                    "text": {"type": "string"}
                },
                "required": ["to", "subject", "text"],
                "additionalProperties": false
            }),
            external_effect: true,
        }
    }

    fn approval_details(&self, args: &Value) -> Result<ApprovalDetails> {
        let subject = args
            .get("subject")
            .and_then(Value::as_str)
            .unwrap_or("(no subject)");
        Ok(ApprovalDetails {
            summary: format!("Send AgentMail message: {subject}"),
            destination: Some(self.client.api_base.clone()),
            artifact_hash: None,
        })
    }

    async fn execute(&self, args: Value, _context: &ToolContext) -> Result<ToolOutput> {
        let inbox_id = args
            .get("inbox_id")
            .and_then(Value::as_str)
            .filter(|v| !v.trim().is_empty())
            .unwrap_or(self.default_inbox_id.as_str());
        if inbox_id.trim().is_empty() {
            return Err(SeraError::Validation(
                "inbox_id required (or set connectors.agentmail.default_inbox_id)".into(),
            ));
        }
        let to = args
            .get("to")
            .and_then(Value::as_array)
            .ok_or_else(|| SeraError::Validation("to is required".into()))?
            .iter()
            .filter_map(Value::as_str)
            .map(str::to_owned)
            .collect::<Vec<_>>();
        let subject = args
            .get("subject")
            .and_then(Value::as_str)
            .ok_or_else(|| SeraError::Validation("subject is required".into()))?;
        let text = args
            .get("text")
            .and_then(Value::as_str)
            .ok_or_else(|| SeraError::Validation("text is required".into()))?;
        let value = self
            .client
            .send_message(inbox_id, &to, subject, text)
            .await?;
        Ok(ToolOutput {
            content: value.to_string(),
            data: Some(value),
            destination: Some(self.client.api_base.clone()),
            ..Default::default()
        })
    }
}

fn map_http(error: reqwest::Error) -> SeraError {
    SeraError::External(format!("agentmail HTTP error: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[tokio::test]
    async fn agentmail_lists_inboxes() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v0/inboxes"))
            .and(header("authorization", "Bearer test-key"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "count": 1,
                "inboxes": [{"inbox_id": "in_1", "email": "a@agentmail.to"}]
            })))
            .mount(&server)
            .await;
        let http = Client::builder().build().unwrap();
        let client =
            AgentMailClient::new(http, &format!("{}/v0", server.uri()), "test-key").unwrap();
        assert_eq!(client.list_inboxes(10).await.unwrap()["count"], 1);
    }
}
