//! External service connectors.
//!
//! OAuth products (Google Drive, Sheets, Calendar, Gmail, Outlook mail,
//! Outlook Calendar, Notion) are configure-later stubs:
//! - secret **names** live in config; values stay in the age store
//! - authorization URLs and token exchange are implemented
//! - no loopback callback server is started
//! - read tools fail closed until a refresh token is stored
//! - reads are approval-gated once a refresh token is present
//!
//! AgentMail is a native HTTPS client. A new `refresh_token` from either the
//! refresh grant or the authorization-code grant is written to the configured
//! age-store key immediately. A failed write fails the call. Token values are
//! never logged.

use std::fmt;
use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use base64::Engine;
use rand::RngCore;
use reqwest::Client;
use reqwest::header::{AUTHORIZATION, CONTENT_TYPE};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tokio::sync::Mutex;
use url::Url;

use crate::config::{ConnectorsConfig, OAuthConnectorSettings, valid_secret_name};
use crate::error::{Result, SeraError};
use crate::gateway::{ApprovalDetails, Tool, ToolContext, ToolOutput, ToolRegistry, ToolSpec};
use crate::secrets::SharedSecrets;
use crate::util::truncate_chars;

const MAX_API_BYTES: usize = 512 * 1024;
const MAX_TOKEN_BYTES: usize = 64 * 1024;
const MAX_SUBJECT_LEN: usize = 500;
const MAX_BODY_LEN: usize = 50_000;
const MAX_INBOX_ID_LEN: usize = 128;
const MAX_PAGE_SIZE: u32 = 20;
const REFRESH_SKEW: Duration = Duration::from_secs(30);
const NOTION_VERSION: &str = "2022-06-28";

const GOOGLE_AUTHORIZE: &str = "https://accounts.google.com/o/oauth2/v2/auth";
const GOOGLE_TOKEN: &str = "https://oauth2.googleapis.com/token";
const MICROSOFT_AUTHORIZE: &str = "https://login.microsoftonline.com/common/oauth2/v2.0/authorize";
const MICROSOFT_TOKEN: &str = "https://login.microsoftonline.com/common/oauth2/v2.0/token";
const NOTION_AUTHORIZE: &str = "https://api.notion.com/v1/oauth/authorize";
const NOTION_TOKEN: &str = "https://api.notion.com/v1/oauth/token";

pub fn register_connector_tools(
    registry: &mut ToolRegistry,
    config: &ConnectorsConfig,
    secrets: &SharedSecrets,
    http: Client,
) -> Result<()> {
    register_oauth(
        registry,
        secrets,
        &config.google_drive,
        ConnectorKind::GoogleDrive,
        http.clone(),
    )?;
    register_oauth(
        registry,
        secrets,
        &config.google_sheets,
        ConnectorKind::GoogleSheets,
        http.clone(),
    )?;
    register_oauth(
        registry,
        secrets,
        &config.google_calendar,
        ConnectorKind::GoogleCalendar,
        http.clone(),
    )?;
    register_oauth(
        registry,
        secrets,
        &config.gmail,
        ConnectorKind::Gmail,
        http.clone(),
    )?;
    register_oauth(
        registry,
        secrets,
        &config.outlook,
        ConnectorKind::OutlookMail,
        http.clone(),
    )?;
    register_oauth(
        registry,
        secrets,
        &config.outlook_calendar,
        ConnectorKind::OutlookCalendar,
        http.clone(),
    )?;
    register_oauth(
        registry,
        secrets,
        &config.notion,
        ConnectorKind::Notion,
        http.clone(),
    )?;
    if config.agentmail.enabled {
        let api_key = require_named_secret(
            secrets,
            "agentmail",
            "api_key_secret",
            &config.agentmail.api_key_secret,
        )?;
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

fn register_oauth(
    registry: &mut ToolRegistry,
    secrets: &SharedSecrets,
    settings: &dyn OAuthConnectorSettings,
    kind: ConnectorKind,
    http: Client,
) -> Result<()> {
    if !settings.oauth_enabled() {
        return Ok(());
    }
    let section = kind.section();
    let client_id = require_named_secret(
        secrets,
        section,
        "client_id_secret",
        settings.client_id_secret_name(),
    )?;
    let client_secret = require_named_secret(
        secrets,
        section,
        "client_secret_secret",
        settings.client_secret_secret_name(),
    )?;
    let refresh_name = settings.refresh_token_secret_name().trim();
    if !valid_secret_name(refresh_name) {
        return Err(SeraError::Config(format!(
            "connectors.{section}.refresh_token_secret must be a secret-store key name"
        )));
    }
    let refresh_token = match secrets
        .get(refresh_name)
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        Some(value) if plausible_secret(value) => Some(value.to_owned()),
        Some(_) => {
            return Err(SeraError::Config(format!(
                "connectors.{section} secret `{refresh_name}` is invalid"
            )));
        }
        None => None,
    };
    let client = OAuthClient::new(OAuthClientParts {
        http,
        kind,
        client_id,
        client_secret,
        refresh_token,
        redirect_uri: settings.redirect_uri().trim().to_owned(),
        refresh_secret_name: refresh_name.to_owned(),
        secrets: Some(secrets.clone()),
        allow_loopback: false,
        authorize_url: kind.authorize_url().to_owned(),
        token_url: kind.token_url().to_owned(),
        api_base: kind.api_base().to_owned(),
    })?;
    registry.register(OAuthReadTool::new(Arc::new(client)))?;
    Ok(())
}

fn require_named_secret(
    secrets: &SharedSecrets,
    section: &str,
    field: &str,
    name: &str,
) -> Result<String> {
    let name = name.trim();
    if !valid_secret_name(name) {
        return Err(SeraError::Config(format!(
            "connectors.{section}.{field} must be a secret-store key name"
        )));
    }
    match secrets
        .get(name)
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        Some(value) if plausible_secret(value) => Ok(value.to_owned()),
        Some(_) => Err(SeraError::Config(format!(
            "connectors.{section} secret `{name}` is invalid"
        ))),
        None => Err(SeraError::Config(format!(
            "connectors.{section} enabled without secret `{name}`; run `serabot secrets set {name}`"
        ))),
    }
}

fn plausible_secret(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 16 * 1024
        && value.bytes().all(|byte| byte.is_ascii_graphic())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum OAuthProvider {
    Google,
    Microsoft,
    Notion,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ConnectorKind {
    GoogleDrive,
    GoogleSheets,
    GoogleCalendar,
    Gmail,
    OutlookMail,
    OutlookCalendar,
    Notion,
}

impl ConnectorKind {
    fn section(self) -> &'static str {
        match self {
            Self::GoogleDrive => "google_drive",
            Self::GoogleSheets => "google_sheets",
            Self::GoogleCalendar => "google_calendar",
            Self::Gmail => "gmail",
            Self::OutlookMail => "outlook",
            Self::OutlookCalendar => "outlook_calendar",
            Self::Notion => "notion",
        }
    }

    fn tool_name(self) -> &'static str {
        match self {
            Self::GoogleDrive => "google_drive_list",
            Self::GoogleSheets => "google_sheets_read",
            Self::GoogleCalendar => "google_calendar_list_events",
            Self::Gmail => "gmail_list_messages",
            Self::OutlookMail => "outlook_list_messages",
            Self::OutlookCalendar => "outlook_calendar_list_events",
            Self::Notion => "notion_query_database",
        }
    }

    fn summary(self) -> &'static str {
        match self {
            Self::GoogleDrive => "List Google Drive files",
            Self::GoogleSheets => "Read a Google Sheets range",
            Self::GoogleCalendar => "List Google Calendar events",
            Self::Gmail => "List Gmail messages",
            Self::OutlookMail => "List Outlook mail",
            Self::OutlookCalendar => "List Outlook Calendar events",
            Self::Notion => "Query a Notion database",
        }
    }

    fn description(self) -> &'static str {
        match self {
            Self::GoogleDrive => {
                "List Google Drive file ids and names (read-only, untrusted metadata)."
            }
            Self::GoogleSheets => "Read one Google Sheets range (read-only, untrusted cells).",
            Self::GoogleCalendar => "List upcoming Google Calendar events (read-only, untrusted).",
            Self::Gmail => "List Gmail message ids (read-only, untrusted metadata).",
            Self::OutlookMail => "List Outlook message ids and subjects (read-only, untrusted).",
            Self::OutlookCalendar => "List Outlook calendar events (read-only, untrusted).",
            Self::Notion => "Query one Notion database (read-only, untrusted pages).",
        }
    }

    fn provider(self) -> OAuthProvider {
        match self {
            Self::GoogleDrive | Self::GoogleSheets | Self::GoogleCalendar | Self::Gmail => {
                OAuthProvider::Google
            }
            Self::OutlookMail | Self::OutlookCalendar => OAuthProvider::Microsoft,
            Self::Notion => OAuthProvider::Notion,
        }
    }

    fn authorize_url(self) -> &'static str {
        match self.provider() {
            OAuthProvider::Google => GOOGLE_AUTHORIZE,
            OAuthProvider::Microsoft => MICROSOFT_AUTHORIZE,
            OAuthProvider::Notion => NOTION_AUTHORIZE,
        }
    }

    fn token_url(self) -> &'static str {
        match self.provider() {
            OAuthProvider::Google => GOOGLE_TOKEN,
            OAuthProvider::Microsoft => MICROSOFT_TOKEN,
            OAuthProvider::Notion => NOTION_TOKEN,
        }
    }

    fn api_base(self) -> &'static str {
        match self {
            Self::GoogleDrive | Self::GoogleCalendar => "https://www.googleapis.com",
            Self::GoogleSheets => "https://sheets.googleapis.com",
            Self::Gmail => "https://gmail.googleapis.com",
            Self::OutlookMail | Self::OutlookCalendar => "https://graph.microsoft.com",
            Self::Notion => "https://api.notion.com",
        }
    }

    fn scope(self) -> &'static str {
        match self {
            Self::GoogleDrive => "https://www.googleapis.com/auth/drive.readonly",
            Self::GoogleSheets => "https://www.googleapis.com/auth/spreadsheets.readonly",
            Self::GoogleCalendar => "https://www.googleapis.com/auth/calendar.readonly",
            Self::Gmail => "https://www.googleapis.com/auth/gmail.readonly",
            Self::OutlookMail => "offline_access https://graph.microsoft.com/Mail.Read",
            Self::OutlookCalendar => "offline_access https://graph.microsoft.com/Calendars.Read",
            Self::Notion => "",
        }
    }

    fn parameters(self) -> Value {
        match self {
            Self::GoogleSheets => json!({
                "type": "object",
                "properties": {
                    "spreadsheet_id": {"type": "string"},
                    "range": {"type": "string"}
                },
                "required": ["spreadsheet_id", "range"],
                "additionalProperties": false
            }),
            Self::Notion => json!({
                "type": "object",
                "properties": {
                    "database_id": {"type": "string"},
                    "page_size": {"type": "integer"}
                },
                "required": ["database_id"],
                "additionalProperties": false
            }),
            _ => json!({
                "type": "object",
                "properties": {
                    "page_size": {"type": "integer"}
                },
                "additionalProperties": false
            }),
        }
    }
}

struct OAuthClientParts {
    http: Client,
    kind: ConnectorKind,
    client_id: String,
    client_secret: String,
    refresh_token: Option<String>,
    redirect_uri: String,
    refresh_secret_name: String,
    secrets: Option<SharedSecrets>,
    allow_loopback: bool,
    authorize_url: String,
    token_url: String,
    api_base: String,
}

struct OAuthMemory {
    refresh_token: Option<String>,
    access_token: Option<String>,
    access_expires: Option<Instant>,
    /// The provider issued a new refresh token that is not in the age store yet.
    refresh_unpersisted: bool,
}

struct OAuthClient {
    http: Client,
    kind: ConnectorKind,
    authorize_url: String,
    token_url: String,
    api_base: String,
    client_id: String,
    client_secret: String,
    redirect_uri: String,
    refresh_secret_name: String,
    secrets: Option<SharedSecrets>,
    refresh_configured: bool,
    memory: Mutex<OAuthMemory>,
}

impl fmt::Debug for OAuthClient {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("OAuthClient")
            .field("connector", &self.kind.section())
            .field("api_base", &self.api_base)
            .field("client_id", &"<redacted>")
            .field("client_secret", &"<redacted>")
            .field("refresh_token", &"<redacted>")
            .field("access_token", &"<redacted>")
            .finish_non_exhaustive()
    }
}

impl OAuthClient {
    fn new(parts: OAuthClientParts) -> Result<Self> {
        if !plausible_secret(&parts.client_id) || !plausible_secret(&parts.client_secret) {
            return Err(SeraError::Config(format!(
                "connectors.{} client credentials are missing or invalid",
                parts.kind.section()
            )));
        }
        if let Some(refresh) = &parts.refresh_token {
            if !plausible_secret(refresh) {
                return Err(SeraError::Config(format!(
                    "connectors.{} secret `{}` is invalid",
                    parts.kind.section(),
                    parts.refresh_secret_name
                )));
            }
        }
        if !parts.redirect_uri.is_empty() {
            crate::config::validate_oauth_redirect_uri(&parts.redirect_uri)?;
        }
        let authorize_url = pin_endpoint(
            &parts.authorize_url,
            parts.kind.authorize_url(),
            parts.allow_loopback,
        )?;
        let token_url = pin_endpoint(
            &parts.token_url,
            parts.kind.token_url(),
            parts.allow_loopback,
        )?;
        let api_base = pin_endpoint(&parts.api_base, parts.kind.api_base(), parts.allow_loopback)?;
        let refresh_configured = parts.refresh_token.is_some();
        Ok(Self {
            http: parts.http,
            kind: parts.kind,
            authorize_url,
            token_url,
            api_base,
            client_id: parts.client_id,
            client_secret: parts.client_secret,
            redirect_uri: parts.redirect_uri,
            refresh_secret_name: parts.refresh_secret_name,
            secrets: parts.secrets,
            refresh_configured,
            memory: Mutex::new(OAuthMemory {
                refresh_token: parts.refresh_token,
                access_token: None,
                access_expires: None,
                refresh_unpersisted: false,
            }),
        })
    }

    /// Provider authorize URL. No callback listener is started.
    #[cfg_attr(not(test), allow(dead_code))]
    fn authorization_url(&self, state: &str, code_challenge: &str) -> Result<String> {
        if self.redirect_uri.is_empty() {
            return Err(SeraError::Config(format!(
                "connectors.{} redirect_uri is unset; set a loopback http://127.0.0.1:<port>/<path> before building an authorization URL",
                self.kind.section()
            )));
        }
        crate::config::validate_oauth_redirect_uri(&self.redirect_uri)?;
        if !oauth_token_ok(state, 16, 128) {
            return Err(SeraError::Config(
                "oauth state must be 16..=128 characters of letters, digits, '_' or '-'".into(),
            ));
        }
        let mut url = Url::parse(&self.authorize_url)
            .map_err(|_| SeraError::Config("oauth authorize URL was rejected".into()))?;
        {
            let mut query = url.query_pairs_mut();
            query.append_pair("client_id", &self.client_id);
            query.append_pair("response_type", "code");
            query.append_pair("redirect_uri", &self.redirect_uri);
            query.append_pair("state", state);
            match self.kind.provider() {
                OAuthProvider::Google => {
                    if !oauth_token_ok(code_challenge, 43, 128) {
                        return Err(SeraError::Config(
                            "oauth code_challenge must be 43..=128 unpadded base64url characters"
                                .into(),
                        ));
                    }
                    query.append_pair("scope", self.kind.scope());
                    query.append_pair("code_challenge", code_challenge);
                    query.append_pair("code_challenge_method", "S256");
                    query.append_pair("access_type", "offline");
                    query.append_pair("prompt", "consent");
                }
                OAuthProvider::Microsoft => {
                    if !oauth_token_ok(code_challenge, 43, 128) {
                        return Err(SeraError::Config(
                            "oauth code_challenge must be 43..=128 unpadded base64url characters"
                                .into(),
                        ));
                    }
                    query.append_pair("scope", self.kind.scope());
                    query.append_pair("response_mode", "query");
                    query.append_pair("code_challenge", code_challenge);
                    query.append_pair("code_challenge_method", "S256");
                }
                OAuthProvider::Notion => {
                    query.append_pair("owner", "user");
                }
            }
        }
        if url.host_str().is_none() {
            return Err(SeraError::Config("oauth authorize URL was rejected".into()));
        }
        let rendered = url.to_string();
        if self.client_secret.len() >= 8 && rendered.contains(&self.client_secret) {
            return Err(SeraError::Config("authorization URL was rejected".into()));
        }
        Ok(rendered)
    }

    /// Authorization-code grant. The caller keeps the verifier. A refresh token
    /// in the response is written to the configured secret key before this returns.
    #[cfg_attr(not(test), allow(dead_code))]
    async fn exchange_authorization_code(
        &self,
        code: &str,
        code_verifier: Option<&str>,
    ) -> Result<()> {
        if !plausible_secret(code) || code.len() < 8 {
            return Err(SeraError::Validation(
                "authorization code must be 8..=16384 ascii graphic characters".into(),
            ));
        }
        if self.redirect_uri.is_empty() {
            return Err(SeraError::Config(format!(
                "connectors.{} redirect_uri is unset",
                self.kind.section()
            )));
        }
        let verifier = match self.kind.provider() {
            OAuthProvider::Notion => None,
            OAuthProvider::Google | OAuthProvider::Microsoft => {
                let verifier = code_verifier.unwrap_or("");
                if !oauth_token_ok(verifier, 43, 128) {
                    return Err(SeraError::Validation(
                        "code_verifier must be 43..=128 unpadded base64url characters".into(),
                    ));
                }
                Some(verifier.to_owned())
            }
        };
        let payload = self
            .request_token(TokenGrant::Code {
                code: code.to_owned(),
                verifier,
            })
            .await?;
        self.store_token(payload).await
    }

    async fn execute_read(&self, args: &Value) -> Result<Value> {
        match self.kind {
            ConnectorKind::GoogleDrive => self.google_drive_list(page_size(args)).await,
            ConnectorKind::GoogleSheets => {
                let spreadsheet_id = required_str(args, "spreadsheet_id")?;
                let range = required_str(args, "range")?;
                if !segment_ok(spreadsheet_id, 10, 128) {
                    return Err(SeraError::Validation(
                        "spreadsheet_id must be 10..=128 letters, digits, '_' or '-'".into(),
                    ));
                }
                if !sheet_range_ok(range) {
                    return Err(SeraError::Validation(
                        "range must be 1..=128 characters without URL separators".into(),
                    ));
                }
                self.google_sheets_read(spreadsheet_id, range).await
            }
            ConnectorKind::GoogleCalendar => self.google_calendar_list(page_size(args)).await,
            ConnectorKind::Gmail => self.gmail_list(page_size(args)).await,
            ConnectorKind::OutlookMail => self.outlook_messages(page_size(args)).await,
            ConnectorKind::OutlookCalendar => self.outlook_events(page_size(args)).await,
            ConnectorKind::Notion => {
                let database_id = required_str(args, "database_id")?;
                if !notion_id_ok(database_id) {
                    return Err(SeraError::Validation(
                        "database_id must be a Notion UUID".into(),
                    ));
                }
                self.notion_query(database_id, page_size(args)).await
            }
        }
    }

    async fn google_drive_list(&self, page_size: u32) -> Result<Value> {
        let mut url = self.child_url("/drive/v3/files")?;
        url.query_pairs_mut()
            .append_pair("pageSize", &page_size.to_string())
            .append_pair("fields", "files(id,name,mimeType),nextPageToken");
        self.api_get(url).await
    }

    async fn google_sheets_read(&self, spreadsheet_id: &str, range: &str) -> Result<Value> {
        let path = format!(
            "/v4/spreadsheets/{}/values/{}",
            encode_component(spreadsheet_id),
            encode_component(range)
        );
        self.api_get(self.child_url(&path)?).await
    }

    async fn google_calendar_list(&self, page_size: u32) -> Result<Value> {
        let mut url = self.child_url("/calendar/v3/calendars/primary/events")?;
        let time_min = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string();
        url.query_pairs_mut()
            .append_pair("maxResults", &page_size.to_string())
            .append_pair("singleEvents", "true")
            .append_pair("orderBy", "startTime")
            .append_pair("timeMin", &time_min);
        self.api_get(url).await
    }

    async fn gmail_list(&self, page_size: u32) -> Result<Value> {
        let mut url = self.child_url("/gmail/v1/users/me/messages")?;
        url.query_pairs_mut()
            .append_pair("maxResults", &page_size.to_string());
        self.api_get(url).await
    }

    async fn outlook_messages(&self, page_size: u32) -> Result<Value> {
        let mut url = self.child_url("/v1.0/me/messages")?;
        url.query_pairs_mut()
            .append_pair("$top", &page_size.to_string())
            .append_pair("$select", "id,subject,receivedDateTime");
        self.api_get(url).await
    }

    async fn outlook_events(&self, page_size: u32) -> Result<Value> {
        let mut url = self.child_url("/v1.0/me/events")?;
        url.query_pairs_mut()
            .append_pair("$top", &page_size.to_string())
            .append_pair("$select", "id,subject,start,end");
        self.api_get(url).await
    }

    async fn notion_query(&self, database_id: &str, page_size: u32) -> Result<Value> {
        let url = self.child_url(&format!(
            "/v1/databases/{}/query",
            encode_component(database_id)
        ))?;
        let token = self.access_token().await?;
        let response = self
            .http
            .post(url)
            .header(AUTHORIZATION, format!("Bearer {token}"))
            .header(CONTENT_TYPE, "application/json")
            .header("Notion-Version", NOTION_VERSION)
            .body(json!({ "page_size": page_size }).to_string())
            .send()
            .await
            .map_err(|error| map_transport(self.kind.section(), error))?;
        self.parse_api(response, &token).await
    }

    fn child_url(&self, path: &str) -> Result<Url> {
        let base = Url::parse(&self.api_base)
            .map_err(|_| SeraError::Config("connector API base was rejected".into()))?;
        let joined = base
            .join(path.trim_start_matches('/'))
            .map_err(|_| SeraError::Config("connector URL was rejected".into()))?;
        if joined.scheme() != base.scheme() || joined.host_str() != base.host_str() {
            return Err(SeraError::Config("connector URL was rejected".into()));
        }
        Ok(joined)
    }

    async fn api_get(&self, url: Url) -> Result<Value> {
        let token = self.access_token().await?;
        let response = self
            .http
            .get(url)
            .header(AUTHORIZATION, format!("Bearer {token}"))
            .send()
            .await
            .map_err(|error| map_transport(self.kind.section(), error))?;
        self.parse_api(response, &token).await
    }

    async fn access_token(&self) -> Result<String> {
        let mut memory = self.memory.lock().await;
        if memory.refresh_unpersisted {
            self.persist_rotated_refresh(&memory)?;
            memory.refresh_unpersisted = false;
        }
        if let Some(token) = fresh_access(&memory) {
            return Ok(token);
        }
        let refresh = memory
            .refresh_token
            .clone()
            .filter(|value| !value.is_empty())
            .ok_or_else(|| {
                SeraError::Config(format!(
                    "connectors.{} refresh token unset; set secret `{}`",
                    self.kind.section(),
                    self.refresh_secret_name
                ))
            })?;
        let payload = self.request_token(TokenGrant::Refresh(refresh)).await?;
        let access = payload.access_token.clone();
        if payload
            .refresh_token
            .as_ref()
            .is_some_and(|value| memory.refresh_token.as_deref() != Some(value.as_str()))
        {
            memory.refresh_unpersisted = true;
        }
        store_token_in(&mut memory, payload);
        if memory.refresh_unpersisted {
            self.persist_rotated_refresh(&memory)?;
            memory.refresh_unpersisted = false;
        }
        Ok(access)
    }

    fn persist_rotated_refresh(&self, memory: &OAuthMemory) -> Result<()> {
        let Some(refresh) = memory
            .refresh_token
            .as_deref()
            .filter(|value| !value.is_empty())
        else {
            return Err(SeraError::Secrets(format!(
                "connectors.{} rotated refresh token was not persisted",
                self.kind.section()
            )));
        };
        let Some(secrets) = &self.secrets else {
            return Err(SeraError::Secrets(format!(
                "connectors.{} rotated refresh token was not persisted",
                self.kind.section()
            )));
        };
        secrets
            .persist(&[(&self.refresh_secret_name, refresh)])
            .map_err(|error| redact_persist_error(error, self.kind.section(), refresh))
    }

    async fn store_token(&self, payload: TokenPayload) -> Result<()> {
        let mut memory = self.memory.lock().await;
        let changed = payload
            .refresh_token
            .as_deref()
            .is_some_and(|value| memory.refresh_token.as_deref() != Some(value));
        store_token_in(&mut memory, payload);
        if changed {
            memory.refresh_unpersisted = true;
            self.persist_rotated_refresh(&memory)?;
            memory.refresh_unpersisted = false;
        }
        Ok(())
    }

    async fn request_token(&self, grant: TokenGrant) -> Result<TokenPayload> {
        let secrets = match &grant {
            TokenGrant::Refresh(token) => vec![self.client_secret.clone(), token.clone()],
            TokenGrant::Code { code, verifier } => {
                let mut secrets = vec![self.client_secret.clone(), code.clone()];
                if let Some(verifier) = verifier {
                    secrets.push(verifier.clone());
                }
                secrets
            }
        };
        let response = match self.kind.provider() {
            OAuthProvider::Notion => self.notion_token_request(&grant),
            OAuthProvider::Google | OAuthProvider::Microsoft => self.form_token_request(&grant),
        }
        .send()
        .await
        .map_err(|error| map_transport("oauth token", error))?;
        read_token_response(response, &secrets).await
    }

    fn form_token_request(&self, grant: &TokenGrant) -> reqwest::RequestBuilder {
        let mut pairs = vec![
            ("client_id".to_owned(), self.client_id.clone()),
            ("client_secret".to_owned(), self.client_secret.clone()),
        ];
        match grant {
            TokenGrant::Refresh(token) => {
                pairs.push(("grant_type".into(), "refresh_token".into()));
                pairs.push(("refresh_token".into(), token.clone()));
                if self.kind.provider() == OAuthProvider::Microsoft {
                    pairs.push(("scope".into(), self.kind.scope().to_owned()));
                }
            }
            TokenGrant::Code { code, verifier } => {
                pairs.push(("grant_type".into(), "authorization_code".into()));
                pairs.push(("code".into(), code.clone()));
                pairs.push(("redirect_uri".into(), self.redirect_uri.clone()));
                if let Some(verifier) = verifier {
                    pairs.push(("code_verifier".into(), verifier.clone()));
                }
            }
        }
        self.http.post(&self.token_url).form(&pairs)
    }

    fn notion_token_request(&self, grant: &TokenGrant) -> reqwest::RequestBuilder {
        let mut body = serde_json::Map::new();
        match grant {
            TokenGrant::Refresh(token) => {
                body.insert("grant_type".into(), json!("refresh_token"));
                body.insert("refresh_token".into(), json!(token));
            }
            TokenGrant::Code { code, .. } => {
                body.insert("grant_type".into(), json!("authorization_code"));
                body.insert("code".into(), json!(code));
                body.insert("redirect_uri".into(), json!(self.redirect_uri));
            }
        }
        let raw = format!("{}:{}", self.client_id, self.client_secret);
        let encoded = base64::engine::general_purpose::STANDARD.encode(raw);
        self.http
            .post(&self.token_url)
            .header(AUTHORIZATION, format!("Basic {encoded}"))
            .header(CONTENT_TYPE, "application/json")
            .body(Value::Object(body).to_string())
    }

    async fn parse_api(&self, response: reqwest::Response, access_token: &str) -> Result<Value> {
        let status = response.status();
        let bytes = response
            .bytes()
            .await
            .map_err(|error| map_transport(self.kind.section(), error))?
            .to_vec();
        if bytes.len() > MAX_API_BYTES {
            return Err(SeraError::External(format!(
                "{} response exceeded size limit",
                self.kind.section()
            )));
        }
        let mut secrets = vec![self.client_secret.clone(), access_token.to_owned()];
        if let Some(refresh) = self.memory.lock().await.refresh_token.clone() {
            secrets.push(refresh);
        }
        if !status.is_success() {
            let code = error_code(&bytes, &secrets);
            return Err(SeraError::External(redact(
                &format!("{} HTTP {}: {code}", self.kind.section(), status.as_u16()),
                &secrets,
            )));
        }
        let mut value: Value = serde_json::from_slice(&bytes).map_err(|_| {
            SeraError::External(format!("{} returned malformed JSON", self.kind.section()))
        })?;
        redact_json(&mut value, &secrets);
        Ok(value)
    }
}

fn store_token_in(memory: &mut OAuthMemory, payload: TokenPayload) {
    if let Some(refresh) = payload.refresh_token {
        memory.refresh_token = Some(refresh);
    }
    memory.access_expires = payload.expires_in.and_then(|seconds| {
        if seconds <= 60 {
            None
        } else {
            Some(Instant::now() + Duration::from_secs(seconds))
        }
    });
    memory.access_token = Some(payload.access_token);
}

fn redact_persist_error(error: SeraError, section: &str, refresh: &str) -> SeraError {
    let message = error.to_string();
    if refresh.len() >= 8 && message.contains(refresh) {
        SeraError::Secrets(format!(
            "connectors.{section} rotated refresh token was not persisted"
        ))
    } else {
        error
    }
}

fn fresh_access(memory: &OAuthMemory) -> Option<String> {
    let (Some(token), Some(expires)) = (&memory.access_token, memory.access_expires) else {
        return None;
    };
    if Instant::now() + REFRESH_SKEW < expires {
        Some(token.clone())
    } else {
        None
    }
}

enum TokenGrant {
    Refresh(String),
    Code {
        code: String,
        verifier: Option<String>,
    },
}

struct TokenPayload {
    access_token: String,
    refresh_token: Option<String>,
    expires_in: Option<u64>,
}

async fn read_token_response(
    response: reqwest::Response,
    secrets: &[String],
) -> Result<TokenPayload> {
    let status = response.status();
    let bytes = response
        .bytes()
        .await
        .map_err(|error| map_transport("oauth token", error))?
        .to_vec();
    if bytes.len() > MAX_TOKEN_BYTES {
        return Err(SeraError::External(
            "oauth token response exceeded size limit".into(),
        ));
    }
    if !status.is_success() {
        let code = error_code(&bytes, secrets);
        return Err(SeraError::External(redact(
            &format!(
                "oauth token request failed (HTTP {}): {code}",
                status.as_u16()
            ),
            secrets,
        )));
    }
    let value: Value = serde_json::from_slice(&bytes)
        .map_err(|_| SeraError::External("oauth token response was rejected".into()))?;
    if let Some(error) = value.get("error").and_then(Value::as_str) {
        let code = sanitize_code(error, secrets);
        return Err(SeraError::External(format!(
            "oauth token request failed (HTTP {}): {code}",
            status.as_u16()
        )));
    }
    let access = value
        .get("access_token")
        .and_then(Value::as_str)
        .unwrap_or("");
    if !plausible_secret(access) {
        return Err(SeraError::External(
            "oauth token response was rejected".into(),
        ));
    }
    let refresh = value
        .get("refresh_token")
        .and_then(Value::as_str)
        .map(str::to_owned);
    if let Some(refresh) = &refresh {
        if !plausible_secret(refresh) {
            return Err(SeraError::External(
                "oauth token response was rejected".into(),
            ));
        }
    }
    let expires_in = value.get("expires_in").and_then(|item| {
        item.as_u64()
            .or_else(|| item.as_str().and_then(|text| text.parse().ok()))
            .filter(|seconds| *seconds <= 86_400 * 7)
    });
    Ok(TokenPayload {
        access_token: access.to_owned(),
        refresh_token: refresh,
        expires_in,
    })
}

fn error_code(bytes: &[u8], secrets: &[String]) -> String {
    let Ok(value) = serde_json::from_slice::<Value>(bytes) else {
        return "request_failed".into();
    };
    if let Some(code) = value.get("error").and_then(Value::as_str) {
        return sanitize_code(code, secrets);
    }
    if let Some(status) = value.pointer("/error/status").and_then(Value::as_str) {
        return sanitize_code(status, secrets);
    }
    "request_failed".into()
}

fn sanitize_code(value: &str, secrets: &[String]) -> String {
    let cleaned: String = value
        .chars()
        .filter(|character| character.is_ascii_graphic())
        .take(64)
        .collect();
    if cleaned.is_empty()
        || secrets
            .iter()
            .any(|secret| secret.len() >= 8 && cleaned.contains(secret.as_str()))
    {
        "request_failed".into()
    } else {
        cleaned
    }
}

fn redact(message: &str, secrets: &[String]) -> String {
    let mut message = message.to_owned();
    for secret in secrets {
        if secret.len() >= 8 {
            message = message.replace(secret.as_str(), "[redacted]");
        }
    }
    message
}

fn redact_json(value: &mut Value, secrets: &[String]) {
    match value {
        Value::String(text) => *text = redact(text, secrets),
        Value::Array(items) => {
            for item in items {
                redact_json(item, secrets);
            }
        }
        Value::Object(map) => {
            for item in map.values_mut() {
                redact_json(item, secrets);
            }
        }
        _ => {}
    }
}

fn pin_endpoint(value: &str, official: &str, allow_loopback: bool) -> Result<String> {
    let value = value.trim().trim_end_matches('/').to_owned();
    let official = official.trim_end_matches('/');
    if value == official {
        return Ok(value);
    }
    if !allow_loopback {
        return Err(SeraError::Config(
            "connector endpoint must be the documented https URL".into(),
        ));
    }
    let url = Url::parse(&value)
        .map_err(|_| SeraError::Config("connector endpoint is not a valid URL".into()))?;
    let host = url.host_str().unwrap_or("");
    let loopback = url.scheme() == "http"
        && (host == "127.0.0.1" || host.eq_ignore_ascii_case("localhost"))
        && url.port().is_some();
    if !loopback
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(SeraError::Config(
            "connector endpoint must be the documented https URL or loopback http for tests".into(),
        ));
    }
    Ok(value)
}

fn oauth_token_ok(value: &str, min: usize, max: usize) -> bool {
    (min..=max).contains(&value.len())
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
}

#[cfg_attr(not(test), allow(dead_code))]
fn pkce_s256() -> (String, String) {
    let mut bytes = [0u8; 32];
    rand::rngs::OsRng.fill_bytes(&mut bytes);
    let verifier = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes);
    let challenge = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .encode(Sha256::digest(verifier.as_bytes()));
    (verifier, challenge)
}

fn page_size(args: &Value) -> u32 {
    let value = args.get("page_size").and_then(Value::as_u64).unwrap_or(10);
    u32::try_from(value)
        .unwrap_or(MAX_PAGE_SIZE)
        .clamp(1, MAX_PAGE_SIZE)
}

fn required_str<'a>(args: &'a Value, key: &str) -> Result<&'a str> {
    args.get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| SeraError::Validation(format!("{key} is required")))
}

fn segment_ok(value: &str, min: usize, max: usize) -> bool {
    (min..=max).contains(&value.len())
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-')
}

fn sheet_range_ok(value: &str) -> bool {
    !value.is_empty()
        && value.chars().count() <= 128
        && !value.chars().any(|character| {
            character.is_control() || matches!(character, '/' | '\\' | '?' | '#' | '%' | '&' | '@')
        })
}

fn notion_id_ok(value: &str) -> bool {
    let hex = |character: char| character.is_ascii_hexdigit();
    (value.len() == 32 && value.chars().all(hex))
        || (value.len() == 36
            && value
                .chars()
                .all(|character| hex(character) || character == '-'))
}

fn encode_component(value: &str) -> String {
    let mut encoded = String::new();
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                encoded.push(byte as char);
            }
            _ => encoded.push_str(&format!("%{byte:02X}")),
        }
    }
    encoded
}

fn map_transport(context: &str, error: reqwest::Error) -> SeraError {
    let detail = if error.is_timeout() {
        "timed out"
    } else if error.is_connect() {
        "could not connect"
    } else {
        "request failed"
    };
    SeraError::External(format!("{context} {detail}"))
}

struct OAuthReadTool {
    client: Arc<OAuthClient>,
}

impl OAuthReadTool {
    fn new(client: Arc<OAuthClient>) -> Self {
        Self { client }
    }
}

#[async_trait]
impl Tool for OAuthReadTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: self.client.kind.tool_name().into(),
            description: self.client.kind.description().into(),
            parameters: self.client.kind.parameters(),
            external_effect: self.client.refresh_configured,
        }
    }

    fn approval_details(&self, args: &Value) -> Result<ApprovalDetails> {
        let summary = match self.client.kind {
            ConnectorKind::GoogleSheets => {
                let id = args
                    .get("spreadsheet_id")
                    .and_then(Value::as_str)
                    .unwrap_or("unset");
                format!("Read Google Sheets {}", truncate_chars(id, 80))
            }
            ConnectorKind::Notion => {
                let id = args
                    .get("database_id")
                    .and_then(Value::as_str)
                    .unwrap_or("unset");
                format!("Query Notion database {}", truncate_chars(id, 80))
            }
            _ => self.client.kind.summary().to_owned(),
        };
        Ok(ApprovalDetails {
            summary,
            destination: Some(self.client.api_base.clone()),
            artifact_hash: None,
        })
    }

    async fn execute(&self, args: Value, _context: &ToolContext) -> Result<ToolOutput> {
        let value = self.client.execute_read(&args).await?;
        Ok(ToolOutput {
            content: value.to_string(),
            data: Some(value),
            destination: Some(self.client.api_base.clone()),
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
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AgentMailClient")
            .field("api_base", &self.api_base)
            .field("api_key", &"<redacted>")
            .finish_non_exhaustive()
    }
}

impl AgentMailClient {
    pub fn new(http: Client, api_base: &str, api_key: impl Into<String>) -> Result<Self> {
        let api_key = api_key.into();
        if !plausible_secret(api_key.trim()) {
            return Err(SeraError::Config(
                "agentmail API key must not be empty".into(),
            ));
        }
        let api_base = api_base.trim().trim_end_matches('/').to_owned();
        let loopback_http =
            api_base.starts_with("http://127.0.0.1") || api_base.starts_with("http://localhost");
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
        if !segment_ok(inbox_id, 1, MAX_INBOX_ID_LEN) {
            return Err(SeraError::Validation(
                "inbox_id must be 1..=128 letters, digits, '_' or '-'".into(),
            ));
        }
        if to.is_empty() || to.len() > 20 || to.iter().any(|address| !email_ok(address)) {
            return Err(SeraError::Validation(
                "to must have 1..=20 plain email addresses".into(),
            ));
        }
        let subject = subject.trim();
        if subject.is_empty()
            || subject.len() > MAX_SUBJECT_LEN
            || subject.chars().any(char::is_control)
        {
            return Err(SeraError::Validation(
                "subject must be 1..=500 characters without control characters".into(),
            ));
        }
        let text = text.trim();
        if text.is_empty() || text.len() > MAX_BODY_LEN {
            return Err(SeraError::Validation(
                "text must be 1..=50000 characters".into(),
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
            .map_err(|error| map_transport("agentmail", error))?;
        parse_api_response(response, &self.api_key).await
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
            .map_err(|error| map_transport("agentmail", error))?;
        parse_api_response(response, &self.api_key).await
    }
}

fn email_ok(value: &str) -> bool {
    let value = value.trim();
    (3..=254).contains(&value.len())
        && value
            .bytes()
            .all(|byte| byte.is_ascii_graphic() && byte != b'<' && byte != b'>')
        && value.matches('@').count() == 1
        && !value.starts_with('@')
        && !value.ends_with('@')
        && !value.contains("..")
}

async fn parse_api_response(response: reqwest::Response, api_key: &str) -> Result<Value> {
    let status = response.status();
    let bytes = response
        .bytes()
        .await
        .map_err(|error| map_transport("agentmail", error))?
        .to_vec();
    if bytes.len() > MAX_API_BYTES {
        return Err(SeraError::External(
            "agentmail response exceeded size limit".into(),
        ));
    }
    let secrets = vec![api_key.to_owned()];
    if !status.is_success() {
        let detail = redact(
            &truncate_chars(&String::from_utf8_lossy(&bytes), 400),
            &secrets,
        );
        return Err(SeraError::External(format!(
            "agentmail HTTP {status}: {detail}"
        )));
    }
    let mut value: Value = serde_json::from_slice(&bytes)
        .map_err(|error| SeraError::External(format!("malformed agentmail JSON: {error}")))?;
    redact_json(&mut value, &secrets);
    Ok(value)
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
            summary: format!("Send AgentMail message: {}", truncate_chars(subject, 120)),
            destination: Some(self.client.api_base.clone()),
            artifact_hash: None,
        })
    }

    async fn execute(&self, args: Value, _context: &ToolContext) -> Result<ToolOutput> {
        let inbox_id = args
            .get("inbox_id")
            .and_then(Value::as_str)
            .filter(|value| !value.trim().is_empty())
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

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, HashSet};

    use super::*;
    use crate::secrets::SecretStore;
    use wiremock::matchers::{body_string_contains, header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn http() -> Client {
        Client::builder().build().unwrap()
    }

    fn context() -> ToolContext {
        ToolContext {
            actor: "owner".into(),
            bot: "main".into(),
            task_id: None,
            turn_id: None,
            conversation_id: None,
            allowed_tools: HashSet::new(),
            require_approval: HashSet::new(),
        }
    }

    fn test_client(
        kind: ConnectorKind,
        refresh: Option<&str>,
        redirect: &str,
        authorize_url: &str,
        token_url: &str,
        api_base: &str,
    ) -> OAuthClient {
        OAuthClient::new(OAuthClientParts {
            http: http(),
            kind,
            client_id: "client-id-value".into(),
            client_secret: "client-secret-value".into(),
            refresh_token: refresh.map(str::to_owned),
            redirect_uri: redirect.into(),
            refresh_secret_name: format!("{}_refresh_token", kind.section()),
            secrets: None,
            allow_loopback: true,
            authorize_url: authorize_url.into(),
            token_url: token_url.into(),
            api_base: api_base.into(),
        })
        .unwrap()
    }

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
        let client =
            AgentMailClient::new(http(), &format!("{}/v0", server.uri()), "test-key").unwrap();
        assert_eq!(client.list_inboxes(10).await.unwrap()["count"], 1);
    }

    #[tokio::test]
    async fn agentmail_send_posts_json_and_rejects_path_segments() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v0/inboxes/in_1/messages/send"))
            .and(header("authorization", "Bearer test-key"))
            .and(body_string_contains("owner@example.com"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"message_id": "m_1"})))
            .mount(&server)
            .await;
        let client =
            AgentMailClient::new(http(), &format!("{}/v0", server.uri()), "test-key").unwrap();
        let sent = client
            .send_message(
                "in_1",
                &["owner@example.com".into()],
                "Hello",
                "Synthetic body",
            )
            .await
            .unwrap();
        assert_eq!(sent["message_id"], "m_1");
        assert!(
            client
                .send_message("..", &["owner@example.com".into()], "Hello", "Body")
                .await
                .is_err()
        );
        assert!(AgentMailClient::new(http(), "https://api.agentmail.to/v0", "  ").is_err());
    }

    #[test]
    fn disabled_connectors_register_nothing() {
        let mut registry = ToolRegistry::default();
        let secrets = SharedSecrets::new(SecretStore::from_map(BTreeMap::new()));
        register_connector_tools(
            &mut registry,
            &ConnectorsConfig::default(),
            &secrets,
            http(),
        )
        .unwrap();
        assert!(registry.specs().is_empty());
    }

    #[test]
    fn enabled_oauth_without_client_credentials_fails_closed() {
        let cases = [
            ("google_drive", "google_oauth_client_id"),
            ("google_sheets", "google_oauth_client_id"),
            ("google_calendar", "google_oauth_client_id"),
            ("gmail", "google_oauth_client_id"),
            ("outlook", "outlook_client_id"),
            ("outlook_calendar", "outlook_client_id"),
            ("notion", "notion_oauth_client_id"),
            ("agentmail", "agentmail_api_key"),
        ];
        for (section, secret_name) in cases {
            let mut config = ConnectorsConfig::default();
            match section {
                "google_drive" => config.google_drive.enabled = true,
                "google_sheets" => config.google_sheets.enabled = true,
                "google_calendar" => config.google_calendar.enabled = true,
                "gmail" => config.gmail.enabled = true,
                "outlook" => config.outlook.enabled = true,
                "outlook_calendar" => config.outlook_calendar.enabled = true,
                "notion" => config.notion.enabled = true,
                "agentmail" => config.agentmail.enabled = true,
                _ => unreachable!(),
            }
            let mut registry = ToolRegistry::default();
            let error = register_connector_tools(
                &mut registry,
                &config,
                &SharedSecrets::new(SecretStore::from_map(BTreeMap::new())),
                http(),
            )
            .unwrap_err();
            let message = error.to_string();
            assert!(message.contains(secret_name), "{section}: {message}");
            assert!(registry.specs().is_empty(), "{section}");
        }
    }

    #[test]
    fn missing_refresh_token_registers_and_does_not_call_out() {
        let mut config = ConnectorsConfig::default();
        config.google_drive.enabled = true;
        let mut values = BTreeMap::new();
        values.insert("google_oauth_client_id".into(), "client-id-value".into());
        values.insert(
            "google_oauth_client_secret".into(),
            "client-secret-value".into(),
        );
        let mut registry = ToolRegistry::default();
        register_connector_tools(
            &mut registry,
            &config,
            &SharedSecrets::new(SecretStore::from_map(values)),
            http(),
        )
        .unwrap();
        let spec = registry.specs().pop().unwrap();
        assert_eq!(spec.name, "google_drive_list");
        assert!(!spec.external_effect);
    }

    #[tokio::test]
    async fn read_without_refresh_token_fails_closed() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(500))
            .expect(0)
            .mount(&server)
            .await;
        let client = test_client(
            ConnectorKind::GoogleDrive,
            None,
            "",
            &format!("{}/authorize", server.uri()),
            &format!("{}/token", server.uri()),
            &server.uri(),
        );
        let tool = OAuthReadTool::new(Arc::new(client));
        let error = tool.execute(json!({}), &context()).await.unwrap_err();
        let message = error.to_string();
        assert!(message.contains("google_drive_refresh_token"), "{message}");
        assert!(!message.contains("client-secret-value"), "{message}");
    }

    #[test]
    fn authorization_urls_pin_provider_hosts_and_omit_the_client_secret() {
        let redirect = "http://127.0.0.1:8765/callback";
        let (verifier, challenge) = pkce_s256();
        assert!(oauth_token_ok(&verifier, 43, 128));
        let google = OAuthClient::new(OAuthClientParts {
            http: http(),
            kind: ConnectorKind::GoogleDrive,
            client_id: "client-id-value".into(),
            client_secret: "client-secret-value".into(),
            refresh_token: None,
            redirect_uri: redirect.into(),
            refresh_secret_name: "google_drive_refresh_token".into(),
            secrets: None,
            allow_loopback: false,
            authorize_url: GOOGLE_AUTHORIZE.into(),
            token_url: GOOGLE_TOKEN.into(),
            api_base: ConnectorKind::GoogleDrive.api_base().into(),
        })
        .unwrap();
        let google_url = google
            .authorization_url("state-state-state1", &challenge)
            .unwrap();
        assert!(google_url.starts_with(GOOGLE_AUTHORIZE), "{google_url}");
        assert!(google_url.contains("client_id=client-id-value"));
        assert!(google_url.contains("code_challenge_method=S256"));
        assert!(google_url.contains("access_type=offline"));
        assert!(!google_url.contains("client-secret-value"));

        let microsoft = OAuthClient::new(OAuthClientParts {
            http: http(),
            kind: ConnectorKind::OutlookMail,
            client_id: "client-id-value".into(),
            client_secret: "client-secret-value".into(),
            refresh_token: None,
            redirect_uri: redirect.into(),
            refresh_secret_name: "outlook_refresh_token".into(),
            secrets: None,
            allow_loopback: false,
            authorize_url: MICROSOFT_AUTHORIZE.into(),
            token_url: MICROSOFT_TOKEN.into(),
            api_base: ConnectorKind::OutlookMail.api_base().into(),
        })
        .unwrap();
        let microsoft_url = microsoft
            .authorization_url("state-state-state1", &challenge)
            .unwrap();
        assert!(
            microsoft_url.starts_with(MICROSOFT_AUTHORIZE),
            "{microsoft_url}"
        );
        assert!(!microsoft_url.contains("client-secret-value"));

        let notion = OAuthClient::new(OAuthClientParts {
            http: http(),
            kind: ConnectorKind::Notion,
            client_id: "client-id-value".into(),
            client_secret: "client-secret-value".into(),
            refresh_token: None,
            redirect_uri: redirect.into(),
            refresh_secret_name: "notion_refresh_token".into(),
            secrets: None,
            allow_loopback: false,
            authorize_url: NOTION_AUTHORIZE.into(),
            token_url: NOTION_TOKEN.into(),
            api_base: ConnectorKind::Notion.api_base().into(),
        })
        .unwrap();
        let notion_url = notion
            .authorization_url("state-state-state1", "unused")
            .unwrap();
        assert!(notion_url.starts_with(NOTION_AUTHORIZE), "{notion_url}");
        assert!(notion_url.contains("owner=user"));
        assert!(!notion_url.contains("client-secret-value"));
        assert!(google.authorization_url("short", &challenge).is_err());
        let mut public = google;
        public.redirect_uri = "https://example.com/callback".into();
        assert!(
            public
                .authorization_url("state-state-state1", &challenge)
                .is_err()
        );
    }

    #[tokio::test]
    async fn google_drive_refreshes_then_lists_and_caches_the_access_token() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/token"))
            .and(body_string_contains("grant_type=refresh_token"))
            .and(body_string_contains("refresh-token-value"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "access_token": "access-token-value",
                "expires_in": 3600,
                "token_type": "Bearer"
            })))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/drive/v3/files"))
            .and(header("authorization", "Bearer access-token-value"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "files": [{"id": "file-1", "name": "Notes", "mimeType": "text/plain"}]
            })))
            .expect(2)
            .mount(&server)
            .await;
        let client = test_client(
            ConnectorKind::GoogleDrive,
            Some("refresh-token-value"),
            "http://127.0.0.1:8765/callback",
            &format!("{}/authorize", server.uri()),
            &format!("{}/token", server.uri()),
            &server.uri(),
        );
        let tool = OAuthReadTool::new(Arc::new(client));
        let first = tool
            .execute(json!({"page_size": 5}), &context())
            .await
            .unwrap();
        let second = tool.execute(json!({}), &context()).await.unwrap();
        assert_eq!(first.data.unwrap()["files"][0]["id"], "file-1");
        assert_eq!(second.data.unwrap()["files"][0]["name"], "Notes");
        assert!(first.destination.unwrap().starts_with("http://127.0.0.1"));
    }

    #[tokio::test]
    async fn token_errors_do_not_echo_the_refresh_token() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/token"))
            .respond_with(ResponseTemplate::new(400).set_body_json(json!({
                "error": "invalid_grant",
                "error_description": "rejected refresh-token-value"
            })))
            .mount(&server)
            .await;
        let client = test_client(
            ConnectorKind::Gmail,
            Some("refresh-token-value"),
            "",
            &format!("{}/authorize", server.uri()),
            &format!("{}/token", server.uri()),
            &server.uri(),
        );
        let tool = OAuthReadTool::new(Arc::new(client));
        let message = tool
            .execute(json!({}), &context())
            .await
            .unwrap_err()
            .to_string();
        assert!(message.contains("invalid_grant"), "{message}");
        assert!(!message.contains("refresh-token-value"), "{message}");
        assert!(!message.contains("client-secret-value"), "{message}");
    }

    #[tokio::test]
    async fn notion_code_exchange_uses_basic_auth_and_json() {
        let server = MockServer::start().await;
        let basic =
            base64::engine::general_purpose::STANDARD.encode("client-id-value:client-secret-value");
        Mock::given(method("POST"))
            .and(path("/token"))
            .and(header("authorization", format!("Basic {basic}")))
            .and(body_string_contains("authorization_code"))
            .and(body_string_contains("synthetic-code"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "access_token": "access-token-value",
                "refresh_token": "rotated-refresh-value",
                "token_type": "bearer"
            })))
            .mount(&server)
            .await;
        let (_dir, _identity, path, secrets) = age_store(&[("other_secret", "other-value")]);
        let client = exchange_client(ConnectorKind::Notion, &server, Some(secrets.clone()));
        client
            .exchange_authorization_code("synthetic-code", None)
            .await
            .unwrap();
        assert_eq!(
            secrets.get("notion_refresh_token").as_deref(),
            Some("rotated-refresh-value")
        );
        assert_eq!(secrets.get("other_secret").as_deref(), Some("other-value"));
        let raw = String::from_utf8_lossy(&std::fs::read(&path).unwrap()).into_owned();
        assert!(!raw.contains("rotated-refresh-value"));
        assert!(!raw.contains("access-token-value"));
        assert!(!raw.contains("other-value"));
        assert!(!raw.contains("client-secret-value"));
        let rendered = format!("{client:?}");
        assert!(!rendered.contains("client-secret-value"));
        assert!(!rendered.contains("rotated-refresh-value"));
        assert!(!rendered.contains("access-token-value"));
    }

    #[tokio::test]
    async fn google_code_exchange_sends_the_verifier() {
        let server = MockServer::start().await;
        let (verifier, _) = pkce_s256();
        Mock::given(method("POST"))
            .and(path("/token"))
            .and(body_string_contains("grant_type=authorization_code"))
            .and(body_string_contains(&verifier))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "access_token": "access-token-value",
                "expires_in": 3600
            })))
            .mount(&server)
            .await;
        let client = test_client(
            ConnectorKind::GoogleCalendar,
            None,
            "http://127.0.0.1:8765/callback",
            &format!("{}/authorize", server.uri()),
            &format!("{}/token", server.uri()),
            &server.uri(),
        );
        client
            .exchange_authorization_code("synthetic-code", Some(&verifier))
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn sheets_and_notion_reject_unsafe_ids_before_http() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(500))
            .expect(0)
            .mount(&server)
            .await;
        let sheets = test_client(
            ConnectorKind::GoogleSheets,
            Some("refresh-token-value"),
            "",
            &format!("{}/authorize", server.uri()),
            &format!("{}/token", server.uri()),
            &server.uri(),
        );
        let error = sheets
            .execute_read(&json!({"spreadsheet_id": "../secret", "range": "A1"}))
            .await
            .unwrap_err();
        assert!(matches!(error, SeraError::Validation(_)), "{error}");
        let notion = test_client(
            ConnectorKind::Notion,
            Some("refresh-token-value"),
            "",
            &format!("{}/authorize", server.uri()),
            &format!("{}/token", server.uri()),
            &server.uri(),
        );
        let error = notion
            .execute_read(&json!({"database_id": "not-a-uuid"}))
            .await
            .unwrap_err();
        assert!(matches!(error, SeraError::Validation(_)), "{error}");
    }

    #[test]
    fn rejects_non_loopback_endpoint_overrides() {
        let error = OAuthClient::new(OAuthClientParts {
            http: http(),
            kind: ConnectorKind::OutlookCalendar,
            client_id: "client-id-value".into(),
            client_secret: "client-secret-value".into(),
            refresh_token: None,
            redirect_uri: String::new(),
            refresh_secret_name: "outlook_calendar_refresh_token".into(),
            secrets: None,
            allow_loopback: true,
            authorize_url: "https://evil.example/authorize".into(),
            token_url: MICROSOFT_TOKEN.into(),
            api_base: ConnectorKind::OutlookCalendar.api_base().into(),
        })
        .unwrap_err();
        assert!(!error.to_string().contains("client-secret-value"));
    }

    fn age_store(
        pairs: &[(&str, &str)],
    ) -> (
        tempfile::TempDir,
        std::path::PathBuf,
        std::path::PathBuf,
        SharedSecrets,
    ) {
        let dir = tempfile::tempdir().unwrap();
        let identity = dir.path().join("identity.txt");
        let path = dir.path().join("secrets.age");
        crate::secrets::ensure_identity(&identity).unwrap();
        let mut store = SecretStore::open_paths(&path, &identity).unwrap();
        for (name, value) in pairs {
            store.set(name, value).unwrap();
        }
        store.save().unwrap();
        let secrets = SharedSecrets::new(SecretStore::open_paths(&path, &identity).unwrap());
        (dir, identity, path, secrets)
    }

    fn client_with_store(
        kind: ConnectorKind,
        refresh: &str,
        secret_name: &str,
        server: &MockServer,
        secrets: SharedSecrets,
    ) -> OAuthClient {
        OAuthClient::new(OAuthClientParts {
            http: http(),
            kind,
            client_id: "client-id-value".into(),
            client_secret: "client-secret-value".into(),
            refresh_token: Some(refresh.into()),
            redirect_uri: String::new(),
            refresh_secret_name: secret_name.into(),
            secrets: Some(secrets),
            allow_loopback: true,
            authorize_url: format!("{}/authorize", server.uri()),
            token_url: format!("{}/token", server.uri()),
            api_base: server.uri(),
        })
        .unwrap()
    }

    #[tokio::test]
    async fn rotated_refresh_tokens_are_written_to_the_age_store() {
        let kinds = [
            ConnectorKind::GoogleDrive,
            ConnectorKind::GoogleSheets,
            ConnectorKind::GoogleCalendar,
            ConnectorKind::Gmail,
            ConnectorKind::OutlookMail,
            ConnectorKind::OutlookCalendar,
            ConnectorKind::Notion,
        ];
        for kind in kinds {
            let server = MockServer::start().await;
            let secret_name = format!("{}_refresh_token", kind.section());
            Mock::given(method("POST"))
                .and(path("/token"))
                .and(body_string_contains("refresh_token"))
                .and(body_string_contains("refresh-old-value"))
                .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                    "access_token": "access-new-value",
                    "refresh_token": "refresh-new-value",
                    "expires_in": 3600,
                    "token_type": "Bearer"
                })))
                .expect(1)
                .mount(&server)
                .await;
            let (_dir, _identity, path, secrets) = age_store(&[
                (&secret_name, "refresh-old-value"),
                ("other_secret", "other-value"),
            ]);
            let client = client_with_store(
                kind,
                "refresh-old-value",
                &secret_name,
                &server,
                secrets.clone(),
            );
            assert_eq!(client.access_token().await.unwrap(), "access-new-value");
            assert_eq!(
                secrets.get(&secret_name).as_deref(),
                Some("refresh-new-value")
            );
            assert_eq!(secrets.get("other_secret").as_deref(), Some("other-value"));
            let raw = std::fs::read(&path).unwrap();
            let text = String::from_utf8_lossy(&raw);
            assert!(!text.contains("refresh-new-value"), "{}", kind.section());
            assert!(!text.contains("refresh-old-value"), "{}", kind.section());
            assert!(!text.contains("access-new-value"), "{}", kind.section());
            assert!(!text.contains("other-value"), "{}", kind.section());
        }
    }

    #[tokio::test]
    async fn store_write_failure_surfaces_and_keeps_the_rotated_token() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/token"))
            .and(body_string_contains("refresh-old-value"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "access_token": "access-new-value",
                "refresh_token": "refresh-new-value",
                "expires_in": 3600
            })))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/drive/v3/files"))
            .and(header("authorization", "Bearer access-new-value"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "files": [{"id": "file-1", "name": "Notes"}]
            })))
            .expect(1)
            .mount(&server)
            .await;
        let (_dir, identity, path, secrets) = age_store(&[
            ("google_drive_refresh_token", "refresh-old-value"),
            ("other_secret", "other-value"),
        ]);
        let backup = std::fs::read(&identity).unwrap();
        std::fs::write(&identity, "not-an-identity").unwrap();
        let client = Arc::new(client_with_store(
            ConnectorKind::GoogleDrive,
            "refresh-old-value",
            "google_drive_refresh_token",
            &server,
            secrets.clone(),
        ));
        let tool = OAuthReadTool::new(client.clone());
        let error = tool.execute(json!({}), &context()).await.unwrap_err();
        let message = error.to_string();
        assert!(
            message.contains("secret store") || message.contains("not persisted"),
            "{message}"
        );
        assert!(!message.contains("refresh-new-value"), "{message}");
        assert!(!message.contains("refresh-old-value"), "{message}");
        assert!(!message.contains("access-new-value"), "{message}");
        assert!(!message.contains("client-secret-value"), "{message}");
        assert_eq!(
            client.memory.lock().await.refresh_token.as_deref(),
            Some("refresh-new-value")
        );
        assert_eq!(
            secrets.get("google_drive_refresh_token").as_deref(),
            Some("refresh-old-value")
        );

        std::fs::write(&identity, backup).unwrap();
        let on_disk = SecretStore::open_paths(&path, &identity).unwrap();
        assert_eq!(
            on_disk.get("google_drive_refresh_token"),
            Some("refresh-old-value")
        );
        assert_eq!(on_disk.get("other_secret"), Some("other-value"));

        let value = tool.execute(json!({}), &context()).await.unwrap();
        assert_eq!(value.data.unwrap()["files"][0]["id"], "file-1");
        assert_eq!(
            secrets.get("google_drive_refresh_token").as_deref(),
            Some("refresh-new-value")
        );
        assert_eq!(secrets.get("other_secret").as_deref(), Some("other-value"));
        let raw = String::from_utf8_lossy(&std::fs::read(&path).unwrap()).into_owned();
        assert!(!raw.contains("refresh-new-value"));
        assert!(!raw.contains("other-value"));
    }

    #[tokio::test]
    async fn missing_writer_fails_closed_without_dropping_the_new_token() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "access_token": "access-new-value",
                "refresh_token": "refresh-new-value",
                "expires_in": 3600
            })))
            .expect(1)
            .mount(&server)
            .await;
        let client = test_client(
            ConnectorKind::OutlookMail,
            Some("refresh-old-value"),
            "",
            &format!("{}/authorize", server.uri()),
            &format!("{}/token", server.uri()),
            &server.uri(),
        );
        let error = client.access_token().await.unwrap_err();
        let message = error.to_string();
        assert!(message.contains("not persisted"), "{message}");
        assert!(!message.contains("refresh-new-value"), "{message}");
        assert!(!message.contains("client-secret-value"), "{message}");
        assert_eq!(
            client.memory.lock().await.refresh_token.as_deref(),
            Some("refresh-new-value")
        );
        let again = client.access_token().await.unwrap_err();
        assert!(!again.to_string().contains("refresh-new-value"));
    }

    #[tokio::test]
    async fn omitted_or_repeated_refresh_token_does_not_rewrite_the_store() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "access_token": "access-new-value",
                "expires_in": 3600
            })))
            .expect(1)
            .mount(&server)
            .await;
        let (_dir, _identity, _path, secrets) =
            age_store(&[("gmail_refresh_token", "refresh-old-value")]);
        let client = client_with_store(
            ConnectorKind::Gmail,
            "refresh-old-value",
            "gmail_refresh_token",
            &server,
            secrets.clone(),
        );
        assert_eq!(client.access_token().await.unwrap(), "access-new-value");
        assert_eq!(
            secrets.get("gmail_refresh_token").as_deref(),
            Some("refresh-old-value")
        );

        let memory = SharedSecrets::new(SecretStore::from_map(BTreeMap::from([(
            "notion_refresh_token".into(),
            "refresh-old-value".into(),
        )])));
        let echo = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "access_token": "access-new-value",
                "refresh_token": "refresh-old-value",
                "expires_in": 3600
            })))
            .expect(1)
            .mount(&echo)
            .await;
        let client = client_with_store(
            ConnectorKind::Notion,
            "refresh-old-value",
            "notion_refresh_token",
            &echo,
            memory,
        );
        assert_eq!(client.access_token().await.unwrap(), "access-new-value");
    }

    fn exchange_client(
        kind: ConnectorKind,
        server: &MockServer,
        secrets: Option<SharedSecrets>,
    ) -> OAuthClient {
        OAuthClient::new(OAuthClientParts {
            http: http(),
            kind,
            client_id: "client-id-value".into(),
            client_secret: "client-secret-value".into(),
            refresh_token: None,
            redirect_uri: "http://127.0.0.1:8765/callback".into(),
            refresh_secret_name: format!("{}_refresh_token", kind.section()),
            secrets,
            allow_loopback: true,
            authorize_url: format!("{}/authorize", server.uri()),
            token_url: format!("{}/token", server.uri()),
            api_base: server.uri(),
        })
        .unwrap()
    }

    fn code_verifier_for(kind: ConnectorKind) -> Option<String> {
        match kind.provider() {
            OAuthProvider::Notion => None,
            OAuthProvider::Google | OAuthProvider::Microsoft => Some(pkce_s256().0),
        }
    }

    #[tokio::test]
    async fn authorization_code_exchange_persists_the_first_refresh_token() {
        let kinds = [
            ConnectorKind::GoogleDrive,
            ConnectorKind::GoogleSheets,
            ConnectorKind::GoogleCalendar,
            ConnectorKind::Gmail,
            ConnectorKind::OutlookMail,
            ConnectorKind::OutlookCalendar,
            ConnectorKind::Notion,
        ];
        for kind in kinds {
            let server = MockServer::start().await;
            let secret_name = format!("{}_refresh_token", kind.section());
            Mock::given(method("POST"))
                .and(path("/token"))
                .and(body_string_contains("authorization_code"))
                .and(body_string_contains("synthetic-code"))
                .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                    "access_token": "access-new-value",
                    "refresh_token": "refresh-first-value",
                    "expires_in": 3600,
                    "token_type": "Bearer"
                })))
                .expect(1)
                .mount(&server)
                .await;
            let (_dir, _identity, path, secrets) = age_store(&[("other_secret", "other-value")]);
            let verifier = code_verifier_for(kind);
            let client = exchange_client(kind, &server, Some(secrets.clone()));
            client
                .exchange_authorization_code("synthetic-code", verifier.as_deref())
                .await
                .unwrap();
            assert_eq!(
                secrets.get(&secret_name).as_deref(),
                Some("refresh-first-value"),
                "{}",
                kind.section()
            );
            assert_eq!(secrets.get("other_secret").as_deref(), Some("other-value"));
            assert!(secrets.get("access_token").is_none());
            let raw = String::from_utf8_lossy(&std::fs::read(&path).unwrap()).into_owned();
            for secret in [
                "refresh-first-value",
                "access-new-value",
                "other-value",
                "client-secret-value",
                "synthetic-code",
            ] {
                assert!(!raw.contains(secret), "{} {secret}", kind.section());
            }
            let rendered = format!("{client:?}");
            assert!(!rendered.contains("refresh-first-value"));
            assert!(!rendered.contains("access-new-value"));
        }
    }

    #[tokio::test]
    async fn code_exchange_without_a_refresh_token_does_not_touch_the_store() {
        let server = MockServer::start().await;
        let (verifier, _) = pkce_s256();
        Mock::given(method("POST"))
            .and(path("/token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "access_token": "access-new-value",
                "expires_in": 3600
            })))
            .expect(1)
            .mount(&server)
            .await;
        let secrets = SharedSecrets::new(SecretStore::from_map(BTreeMap::from([(
            "other_secret".into(),
            "other-value".into(),
        )])));
        let client = exchange_client(
            ConnectorKind::GoogleCalendar,
            &server,
            Some(secrets.clone()),
        );
        client
            .exchange_authorization_code("synthetic-code", Some(&verifier))
            .await
            .unwrap();
        assert_eq!(secrets.get("google_calendar_refresh_token"), None);
        assert_eq!(secrets.get("other_secret").as_deref(), Some("other-value"));
    }

    #[tokio::test]
    async fn first_refresh_without_a_writer_fails_closed() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "access_token": "access-new-value",
                "refresh_token": "refresh-first-value",
                "expires_in": 3600
            })))
            .expect(1)
            .mount(&server)
            .await;
        let client = exchange_client(ConnectorKind::Gmail, &server, None);
        let error = client
            .exchange_authorization_code("synthetic-code", Some(&pkce_s256().0))
            .await
            .unwrap_err();
        let message = error.to_string();
        assert!(message.contains("not persisted"), "{message}");
        assert!(!message.contains("refresh-first-value"), "{message}");
        assert!(!message.contains("access-new-value"), "{message}");
        assert!(!message.contains("client-secret-value"), "{message}");
        assert!(!message.contains("synthetic-code"), "{message}");
        assert_eq!(
            client.memory.lock().await.refresh_token.as_deref(),
            Some("refresh-first-value")
        );
        assert!(client.memory.lock().await.refresh_unpersisted);
    }

    #[tokio::test]
    async fn first_refresh_store_failure_keeps_the_token_for_retry() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "access_token": "access-new-value",
                "refresh_token": "refresh-first-value",
                "expires_in": 3600
            })))
            .expect(1)
            .mount(&server)
            .await;
        let (_dir, identity, path, secrets) = age_store(&[("other_secret", "other-value")]);
        let backup = std::fs::read(&identity).unwrap();
        let before = std::fs::read(&path).unwrap();
        std::fs::write(&identity, "not-an-identity").unwrap();
        let client = exchange_client(ConnectorKind::OutlookMail, &server, Some(secrets.clone()));
        let verifier = pkce_s256().0;
        let error = client
            .exchange_authorization_code("synthetic-code", Some(&verifier))
            .await
            .unwrap_err();
        let message = error.to_string();
        assert!(
            message.contains("identity") || message.contains("not persisted"),
            "{message}"
        );
        for secret in [
            "refresh-first-value",
            "access-new-value",
            "client-secret-value",
            "synthetic-code",
            "other-value",
        ] {
            assert!(!message.contains(secret), "{message}");
        }
        assert_eq!(std::fs::read(&path).unwrap(), before);
        assert_eq!(secrets.get("outlook_refresh_token"), None);
        assert_eq!(
            client.memory.lock().await.refresh_token.as_deref(),
            Some("refresh-first-value")
        );
        assert!(client.memory.lock().await.refresh_unpersisted);

        std::fs::write(&identity, backup).unwrap();
        assert_eq!(client.access_token().await.unwrap(), "access-new-value");
        assert_eq!(
            secrets.get("outlook_refresh_token").as_deref(),
            Some("refresh-first-value")
        );
        assert_eq!(secrets.get("other_secret").as_deref(), Some("other-value"));
        let raw = String::from_utf8_lossy(&std::fs::read(&path).unwrap()).into_owned();
        assert!(!raw.contains("refresh-first-value"));
        assert!(!raw.contains("access-new-value"));
        assert!(!raw.contains("other-value"));
        assert!(!client.memory.lock().await.refresh_unpersisted);
        server.verify().await;
    }
}
