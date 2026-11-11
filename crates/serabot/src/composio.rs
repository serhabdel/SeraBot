//! Composio apps (<https://composio.dev>), per user.
//!
//! Each user pastes their own Composio API key in the console. It is
//! validated with a cheap authenticated read, then sealed in the per-user
//! store (`user_secrets`) and never returned, logged, or put in a prompt.
//! Composio's own `user_id` is the SeraBot user id (stable, not the email), so
//! one user's linked apps are never visible to or usable by another.
//!
//! Endpoints (REST API v3, base `https://backend.composio.dev`, header
//! `x-api-key`, or `x-user-api-key` for a user-level key):
//! `GET /api/v3/toolkits`, `GET /api/v3/tools`,
//! `POST /api/v3/tools/execute/{slug}`, `GET|POST /api/v3/auth_configs`,
//! `POST /api/v3/connected_accounts/link`, `GET /api/v3/connected_accounts`,
//! `DELETE /api/v3/connected_accounts/{id}`. See `docs/composio.md`.
//!
//! Everything Composio returns is untrusted data. Bodies are read with a hard
//! byte limit, requests have a timeout, and tool output is truncated.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use secrecy::ExposeSecret;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::composio_mcp::McpClient;
use crate::config::{ComposioConfig, Config};
use crate::connectors::plausible_secret;
use crate::error::{Result, SeraError};
use crate::gateway::{ApprovalDetails, Tool, ToolContext, ToolOutput, ToolRegistry, ToolSpec};
use crate::user_secrets::UserSecrets;
use crate::util::truncate_chars;

/// The per-user credential row name.
pub const CREDENTIAL_ID: &str = "conn.composio";
pub const HELP_URL: &str = "https://platform.composio.dev";
/// Where a Connect (`ck_`) key is created and its apps are managed.
pub const CONNECT_URL: &str = "https://dashboard.composio.dev";
const MAX_KEY_BYTES: usize = 512;
const MAX_ARGS_BYTES: usize = 64 * 1024;
const MAX_OUTPUT_CHARS: usize = 24_000;
const MAX_LIST: usize = 50;

/// Slug words that make a Composio tool destructive (needs approval): it
/// deletes, sends, publishes, or moves something that cannot be recalled.
/// Composio's tool schema does not expose read-only/destructive hints, so the
/// rule is a conservative verb match on the `_`-separated slug words.
const DESTRUCTIVE_WORDS: &[&str] = &[
    "DELETE",
    "REMOVE",
    "SEND",
    "TRASH",
    "ARCHIVE",
    "TRANSFER",
    "PAY",
    "PUBLISH",
    "DESTROY",
    "PURGE",
    "REVOKE",
    "CANCEL",
    "TERMINATE",
    "REPLY",
    "FORWARD",
    "INVITE",
    "SHARE",
    "POST",
];

/// True when running this Composio tool needs approval. Matches whole words
/// of the slug (`GMAIL_SEND_EMAIL`, `SLACK_POST_MESSAGE`, `GMAIL_CREATE_EMAIL_DRAFT`).
pub fn slug_is_destructive(slug: &str) -> bool {
    let words: Vec<String> = slug
        .split('_')
        .filter(|word| !word.is_empty())
        .map(str::to_ascii_uppercase)
        .collect();
    if words
        .iter()
        .any(|word| DESTRUCTIVE_WORDS.contains(&word.as_str()))
    {
        return true;
    }
    words.windows(2).any(|pair| {
        matches!(
            (pair[0].as_str(), pair[1].as_str()),
            ("POST", "MESSAGE") | ("CREATE", "EMAIL") | ("DRAFT", "SEND")
        )
    })
}

fn valid_slug(slug: &str) -> bool {
    !slug.is_empty()
        && slug.len() <= 128
        && slug
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
}

fn valid_toolkit(toolkit: &str) -> bool {
    !toolkit.is_empty()
        && toolkit.len() <= 64
        && toolkit
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
}

/// A toolkit version as Composio writes it: `latest`, or a dated id such as
/// `00000000_00`. Bounded and plain, since it is sent in a request body.
fn valid_tool_version(version: &str) -> bool {
    !version.is_empty()
        && version.len() <= 32
        && version
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'))
}

fn valid_account_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 128
        && id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
}

pub(crate) fn clean(value: &str, max: usize) -> String {
    truncate_chars(
        &value
            .chars()
            .filter(|c| !c.is_control() || *c == ' ')
            .collect::<String>(),
        max,
    )
}

fn text(value: &Value, key: &str, max: usize) -> String {
    value
        .get(key)
        .and_then(Value::as_str)
        .map(|s| clean(s, max))
        .unwrap_or_default()
}

/// Which kind of key this is, and so where it is sent. Project keys (`ak_`)
/// use `x-api-key` on the REST API; user-level keys (`uak_`) use
/// `x-user-api-key`; Composio Connect consumer keys (`ck_`) use
/// `x-consumer-api-key` on the Connect MCP endpoint.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum KeyKind {
    Project,
    User,
    Consumer,
}

impl KeyKind {
    /// The kind a key's prefix announces, or `None` for an unknown prefix.
    /// `uak_` is checked before `ak_` only for clarity; they never overlap.
    pub fn detect(key: &str) -> Option<Self> {
        if key.starts_with("uak_") {
            Some(Self::User)
        } else if key.starts_with("ak_") {
            Some(Self::Project)
        } else if key.starts_with("ck_") {
            Some(Self::Consumer)
        } else {
            None
        }
    }

    /// The REST header for this kind. A consumer key has none: it is never
    /// sent to the REST API.
    fn name(self) -> &'static str {
        match self {
            Self::Project => "x-api-key",
            Self::User => "x-user-api-key",
            Self::Consumer => "x-consumer-api-key",
        }
    }

    /// The word used in the connector list and the console.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Project => "project",
            Self::User => "user",
            Self::Consumer => "consumer",
        }
    }
}

/// The sealed per-user value: the key and the header it was accepted with.
/// Rows saved before the header was recorded hold the bare key (a project key).
#[derive(Serialize, Deserialize)]
struct SealedKey {
    key: String,
    header: KeyKind,
}

impl SealedKey {
    fn parse(raw: &str) -> Self {
        serde_json::from_str(raw).unwrap_or_else(|_| Self {
            key: raw.to_owned(),
            header: KeyKind::Project,
        })
    }
}

/// The key as it appears inside a URL: every byte outside the unreserved set
/// is written as `%XX`.
fn percent_encoded(value: &str) -> String {
    use std::fmt::Write as _;
    let mut out = String::with_capacity(value.len());
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~') {
            out.push(char::from(byte));
        } else {
            let _ = write!(out, "%{byte:02X}");
        }
    }
    out
}

/// Replace every ASCII case-insensitive occurrence of `needle` in `text`.
/// Lowercasing ASCII keeps byte offsets, so the match positions are valid
/// in the original text.
fn replace_ascii_ci(text: &str, needle: &str, with: &str) -> String {
    if needle.is_empty() {
        return text.to_owned();
    }
    let lower_text = text.to_ascii_lowercase();
    let lower_needle = needle.to_ascii_lowercase();
    let mut out = String::with_capacity(text.len());
    let mut last = 0;
    for (start, matched) in lower_text.match_indices(&lower_needle) {
        out.push_str(&text[last..start]);
        out.push_str(with);
        last = start + matched.len();
    }
    out.push_str(&text[last..]);
    out
}

/// Remove the key from text in every form it can take: as sent, percent
/// encoded (as in a URL), and in any letter case.
pub(crate) fn scrub_key(text: &str, key: &str) -> String {
    let mut out = text.to_owned();
    for form in [key.to_owned(), percent_encoded(key)] {
        out = replace_ascii_ci(&out, &form, "[redacted]");
    }
    out
}

/// A failed Composio response, reduced to what is safe to show. The key is
/// removed from every text first; control characters are dropped and each
/// text is capped. `status` is 0 when no response arrived.
pub(crate) fn upstream_error(status: u16, body: &Value, key: &str) -> SeraError {
    let text = |pointer: &str, max: usize| {
        let raw = body
            .pointer(pointer)
            .and_then(Value::as_str)
            .unwrap_or_default();
        clean(&scrub_key(raw, key), max)
    };
    let message = text("/error/message", 240);
    let slug = text("/error/slug", 120);
    let fix = text("/error/suggested_fix", 240);
    let detail = if message.is_empty() { slug } else { message };
    let (code, base) = match status {
        401 | 403 => (
            "composio_rejected",
            format!(
                "Composio rejected this API key{}. {}",
                parenthesized(&detail),
                rejection_hint(key)
            ),
        ),
        402 => (
            "composio_plan_required",
            format!(
                "Composio needs a plan or credits for this request{}.",
                parenthesized(&detail)
            ),
        ),
        404 => (
            "composio_not_found",
            format!("Composio could not find that{}.", parenthesized(&detail)),
        ),
        429 => (
            "composio_rate_limited",
            "Composio is rate limiting requests. Try again in a minute.".to_owned(),
        ),
        500..=599 => (
            "composio_unavailable",
            format!("Composio could not be reached (HTTP {status})."),
        ),
        _ => (
            "composio_bad_request",
            format!(
                "Composio refused the request (HTTP {status}){}.",
                parenthesized(&detail)
            ),
        ),
    };
    let message = if fix.is_empty() {
        base
    } else {
        format!("{base} Composio suggests: {fix}")
    };
    SeraError::Upstream {
        status,
        code,
        message,
    }
}

/// What to do about a rejected key, by the kind its prefix announces. Never
/// includes the key itself, only its (public) prefix.
fn rejection_hint(key: &str) -> &'static str {
    match KeyKind::detect(key) {
        Some(KeyKind::Consumer) => {
            "This looks like a Composio Connect key (ck_). Copy it again from Composio Connect → Settings → Sessions & API Key; regenerating it there invalidates the old one."
        }
        Some(KeyKind::Project) => {
            "This looks like a project API key (ak_). Copy a current one from platform.composio.dev → Settings → API keys. A Connect key starts with ck_."
        }
        Some(KeyKind::User) => {
            "This looks like a user API key (uak_). Use a current one, or the project API key (ak_) from platform.composio.dev → Settings → API keys."
        }
        None => {
            "Use the project API key from platform.composio.dev → Settings → API keys. SeraBot accepts project keys (ak_), user keys (uak_) and Connect keys (ck_)."
        }
    }
}

fn parenthesized(detail: &str) -> String {
    if detail.is_empty() {
        String::new()
    } else {
        format!(" ({detail})")
    }
}

/// A request that never got a response (timeout, refused connection, reset).
pub(crate) fn transport_failure(error: reqwest::Error) -> SeraError {
    let detail = if error.is_timeout() {
        "timed out"
    } else if error.is_connect() {
        "could not connect"
    } else {
        "request failed"
    };
    SeraError::Upstream {
        status: 0,
        code: "composio_unavailable",
        message: format!("Composio could not be reached ({detail})."),
    }
}

pub struct ComposioService {
    users: Arc<UserSecrets>,
    http: reqwest::Client,
    base: String,
    mcp: McpClient,
    max_bytes: usize,
    tool_version: String,
}

impl std::fmt::Debug for ComposioService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ComposioService")
            .field("base", &self.base)
            .field("mcp_base", &self.mcp.url())
            .finish_non_exhaustive()
    }
}

impl ComposioService {
    pub fn new(config: &Config, users: Arc<UserSecrets>) -> Result<Self> {
        Self::with_settings(&config.composio, users)
    }

    pub fn with_settings(settings: &ComposioConfig, users: Arc<UserSecrets>) -> Result<Self> {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(settings.timeout_secs.clamp(5, 120)))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|error| SeraError::Internal(format!("Composio HTTP client: {error}")))?;
        let tool_version = settings
            .tool_version
            .as_deref()
            .map(str::trim)
            .filter(|version| !version.is_empty())
            .unwrap_or("latest");
        if !valid_tool_version(tool_version) {
            return Err(SeraError::Config(
                "composio tool_version must be 1-32 letters, digits, '.', '-', or '_'".into(),
            ));
        }
        let max_bytes = settings.max_response_bytes.clamp(4 * 1024, 4 * 1024 * 1024);
        let mcp = McpClient::new(
            &settings.mcp_base,
            Duration::from_secs(settings.timeout_secs.clamp(5, 120)),
            max_bytes,
        )?;
        Ok(Self {
            users,
            http,
            base: settings.api_base.trim().trim_end_matches('/').to_owned(),
            mcp,
            max_bytes,
            tool_version: tool_version.to_owned(),
        })
    }

    pub fn api_base(&self) -> &str {
        &self.base
    }

    pub fn mcp_base(&self) -> &str {
        self.mcp.url()
    }

    /// The outbound destination for this user's Composio calls: the Connect
    /// MCP endpoint for a Connect key, the REST API otherwise.
    fn destination(&self, user_id: &str) -> &str {
        match self.key_kind(user_id) {
            Ok(Some(KeyKind::Consumer)) => self.mcp.url(),
            _ => &self.base,
        }
    }

    /// Which kind of key this user saved, if any.
    pub fn key_kind(&self, user_id: &str) -> Result<Option<KeyKind>> {
        Ok(self
            .users
            .get(user_id, CREDENTIAL_ID)?
            .map(|secret| SealedKey::parse(secret.expose_secret()).header))
    }

    /// Whether this user has saved a Composio key.
    pub fn connected(&self, user_id: &str) -> Result<bool> {
        Ok(self.users.get(user_id, CREDENTIAL_ID)?.is_some())
    }

    fn key(&self, user_id: &str) -> Result<SealedKey> {
        self.users
            .get(user_id, CREDENTIAL_ID)?
            .map(|secret| SealedKey::parse(secret.expose_secret()))
            .ok_or_else(|| {
                SeraError::Forbidden(
                    "Composio is not connected; add your Composio key in Connectors first".into(),
                )
            })
    }

    /// Check the key with a cheap authenticated read, then seal it for this
    /// user with the header Composio accepted. The key is never returned.
    pub async fn connect_key(&self, user_id: &str, key: &str) -> Result<()> {
        let key = key.trim();
        // Shorter keys could not be redacted from Composio's error text.
        if key.len() < 8 || key.len() > MAX_KEY_BYTES || !plausible_secret(key) {
            return Err(SeraError::Validation(
                "that does not look like a Composio API key".into(),
            ));
        }
        let header = match KeyKind::detect(key) {
            // A Connect key is checked against the MCP endpoint, never REST.
            Some(KeyKind::Consumer) => {
                self.mcp.list_tools(key).await?;
                KeyKind::Consumer
            }
            // The prefix names the header: send it there and nowhere else.
            Some(kind) => {
                self.check(key, kind).await?;
                kind
            }
            // No known prefix: try the project header, then once the user one.
            None => match self.check(key, KeyKind::Project).await {
                Ok(()) => KeyKind::Project,
                // Composio refuses some user-level keys sent as x-api-key.
                // Only a 401 earns the second try; a key that fails both is
                // still refused.
                Err(error @ SeraError::Upstream { status: 401, .. }) => {
                    if self.check(key, KeyKind::User).await.is_ok() {
                        KeyKind::User
                    } else {
                        return Err(error);
                    }
                }
                Err(error) => return Err(error),
            },
        };
        let sealed = serde_json::to_string(&SealedKey {
            key: key.to_owned(),
            header,
        })?;
        self.users.set(user_id, CREDENTIAL_ID, &sealed)
    }

    /// One cheap authenticated read, to see whether Composio takes the key
    /// under `header`.
    async fn check(&self, key: &str, header: KeyKind) -> Result<()> {
        let candidate = SealedKey {
            key: key.to_owned(),
            header,
        };
        self.call(
            &candidate,
            "GET",
            "/api/v3/toolkits",
            &[("limit", "1")],
            None,
        )
        .await
        .map(|_| ())
    }

    /// Forget this user's key. Their linked apps stay in Composio.
    pub fn disconnect_key(&self, user_id: &str) -> Result<bool> {
        self.users.delete(user_id, CREDENTIAL_ID)
    }

    async fn call(
        &self,
        key: &SealedKey,
        method: &str,
        path: &str,
        query: &[(&str, &str)],
        body: Option<&Value>,
    ) -> Result<Value> {
        if key.header == KeyKind::Consumer {
            // A Connect key is for the MCP endpoint only; sending it to the
            // REST API would just leak it to the wrong place.
            return Err(SeraError::Internal(
                "a Connect key cannot call the REST API".into(),
            ));
        }
        let url = format!("{}{path}", self.base);
        let mut request = match method {
            "GET" => self.http.get(&url),
            "POST" => self.http.post(&url),
            "DELETE" => self.http.delete(&url),
            _ => return Err(SeraError::Internal("unsupported method".into())),
        }
        .header(key.header.name(), key.key.as_str())
        .header("accept", "application/json");
        if !query.is_empty() {
            request = request.query(query);
        }
        if let Some(body) = body {
            request = request.json(body);
        }
        // Retry only where repeating the request is safe; see `crate::retry`.
        let mut response = crate::retry::send(request, crate::retry::idempotent_method(method))
            .await
            .map_err(transport_failure)?;
        let status = response.status();
        let mut bytes: Vec<u8> = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(transport_failure)? {
            if bytes.len() + chunk.len() > self.max_bytes {
                return Err(SeraError::External(
                    "Composio response exceeded the size limit".into(),
                ));
            }
            bytes.extend_from_slice(&chunk);
        }
        let value: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
        if status.is_success() {
            return Ok(value);
        }
        Err(upstream_error(status.as_u16(), &value, &key.key))
    }

    /// Find tool slugs for a task. Each result says whether running it would
    /// need approval.
    pub async fn search_tools(&self, user_id: &str, query: &str, limit: usize) -> Result<Value> {
        let key = self.key(user_id)?;
        let query = clean(query, 200);
        if query.trim().is_empty() {
            return Err(SeraError::Validation("query is required".into()));
        }
        if key.header == KeyKind::Consumer {
            return self.consumer_search_tools(&key, &query, limit).await;
        }
        let limit = limit.clamp(1, 20).to_string();
        let reply = self
            .call(
                &key,
                "GET",
                "/api/v3/tools",
                &[("query", query.as_str()), ("limit", limit.as_str())],
                None,
            )
            .await?;
        let tools: Vec<Value> = reply
            .get("items")
            .and_then(Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .take(MAX_LIST)
                    .map(|item| {
                        let slug = text(item, "slug", 128);
                        json!({
                            "slug": slug,
                            "name": text(item, "name", 120),
                            "description": text(item, "description", 240),
                            "toolkit": item.pointer("/toolkit/slug").and_then(Value::as_str)
                                .map(|s| clean(s, 64)).unwrap_or_default(),
                            "needs_approval": slug_needs_approval(&slug),
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();
        Ok(json!({"tools": tools}))
    }

    /// Search the app catalogue (toolkits) by name.
    pub async fn search_toolkits(&self, user_id: &str, query: &str) -> Result<Value> {
        let key = self.key(user_id)?;
        let query = clean(query, 80);
        if key.header == KeyKind::Consumer {
            return self.consumer_apps(&key, &query).await;
        }
        let mut params = vec![("limit", "24"), ("sort_by", "usage")];
        if !query.trim().is_empty() {
            params.push(("search", query.as_str()));
        }
        let reply = self
            .call(&key, "GET", "/api/v3/toolkits", &params, None)
            .await?;
        let toolkits: Vec<Value> = reply
            .get("items")
            .and_then(Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .take(MAX_LIST)
                    .filter(|item| valid_toolkit(&text(item, "slug", 64)))
                    .map(|item| {
                        json!({
                            "slug": text(item, "slug", 64),
                            "name": text(item, "name", 80),
                            "description": item.pointer("/meta/description")
                                .and_then(Value::as_str).map(|s| clean(s, 160)).unwrap_or_default(),
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();
        Ok(json!({"toolkits": toolkits}))
    }

    /// This user's linked apps (Composio connected accounts).
    pub async fn accounts(&self, user_id: &str) -> Result<Value> {
        let key = self.key(user_id)?;
        if key.header == KeyKind::Consumer {
            // Connect keeps linked apps on Composio's side.
            return Ok(json!({
                "accounts": [],
                "managed_externally": true,
                "manage_url": CONNECT_URL,
            }));
        }
        let reply = self
            .call(
                &key,
                "GET",
                "/api/v3/connected_accounts",
                &[("user_ids", user_id), ("limit", "100")],
                None,
            )
            .await?;
        let accounts: Vec<Value> = reply
            .get("items")
            .and_then(Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .take(100)
                    .filter(|item| valid_account_id(&text(item, "id", 128)))
                    .map(|item| {
                        json!({
                            "id": text(item, "id", 128),
                            "toolkit": item.pointer("/toolkit/slug").and_then(Value::as_str)
                                .map(|s| clean(s, 64)).unwrap_or_default(),
                            "status": text(item, "status", 32),
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();
        Ok(json!({"accounts": accounts}))
    }

    /// Unlink one app. The account must be in this user's own list first, so
    /// a guessed id from another user is refused.
    pub async fn disconnect_account(&self, user_id: &str, account_id: &str) -> Result<()> {
        if !valid_account_id(account_id) {
            return Err(SeraError::Validation("invalid account id".into()));
        }
        let key = self.key(user_id)?;
        if key.header == KeyKind::Consumer {
            return Err(SeraError::Validation(
                "your apps are managed in Composio Connect".into(),
            ));
        }
        let own = self.accounts(user_id).await?;
        let owned = own["accounts"]
            .as_array()
            .is_some_and(|list| list.iter().any(|a| a["id"] == account_id));
        if !owned {
            return Err(SeraError::NotFound("that app is not linked to you".into()));
        }
        self.call(
            &key,
            "DELETE",
            &format!("/api/v3/connected_accounts/{account_id}"),
            &[],
            None,
        )
        .await?;
        Ok(())
    }

    /// A hosted sign-in link for one app, for this user.
    pub async fn connect_link(&self, user_id: &str, toolkit: &str) -> Result<Value> {
        let toolkit = toolkit.trim().to_ascii_lowercase();
        if !valid_toolkit(&toolkit) {
            return Err(SeraError::Validation("invalid app name".into()));
        }
        let key = self.key(user_id)?;
        if key.header == KeyKind::Consumer {
            // Apps for a Connect key are linked on Composio's own site.
            return Ok(json!({"url": CONNECT_URL, "toolkit": toolkit, "managed_externally": true}));
        }
        let configs = self
            .call(
                &key,
                "GET",
                "/api/v3/auth_configs",
                &[("toolkit_slug", toolkit.as_str()), ("limit", "20")],
                None,
            )
            .await?;
        let items = configs
            .get("items")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let enabled =
            |item: &&Value| item.get("status").and_then(Value::as_str) != Some("DISABLED");
        let existing = items
            .iter()
            .filter(enabled)
            .find(|item| item["is_composio_managed"] == true)
            .or_else(|| items.iter().find(enabled))
            .and_then(|item| item.get("id").and_then(Value::as_str))
            .filter(|id| valid_account_id(id))
            .map(str::to_owned);
        let auth_config_id = match existing {
            Some(id) => id,
            None => {
                let created = self
                    .call(
                        &key,
                        "POST",
                        "/api/v3/auth_configs",
                        &[],
                        Some(&json!({
                            "toolkit": {"slug": toolkit},
                            "auth_config": {"type": "use_composio_managed_auth"},
                        })),
                    )
                    .await?;
                created
                    .pointer("/auth_config/id")
                    .and_then(Value::as_str)
                    .filter(|id| valid_account_id(id))
                    .map(str::to_owned)
                    .ok_or_else(|| {
                        SeraError::External("Composio could not set up that app".into())
                    })?
            }
        };
        let link = self
            .call(
                &key,
                "POST",
                "/api/v3/connected_accounts/link",
                &[],
                Some(&json!({"auth_config_id": auth_config_id, "user_id": user_id})),
            )
            .await?;
        let url = link
            .get("redirect_url")
            .and_then(Value::as_str)
            .filter(|url| url.len() <= 2048 && url.starts_with("https://"))
            .ok_or_else(|| SeraError::External("Composio returned no sign-in link".into()))?;
        Ok(json!({"url": url, "toolkit": toolkit}))
    }

    /// Run one Composio tool for this user.
    pub async fn execute(&self, user_id: &str, slug: &str, arguments: &Value) -> Result<Value> {
        if !valid_slug(slug) {
            return Err(SeraError::Validation("invalid tool slug".into()));
        }
        if !arguments.is_object() {
            return Err(SeraError::Validation("arguments must be an object".into()));
        }
        if serde_json::to_vec(arguments)?.len() > MAX_ARGS_BYTES {
            return Err(SeraError::Validation("arguments are too large".into()));
        }
        let key = self.key(user_id)?;
        if key.header == KeyKind::Consumer {
            return self.consumer_execute(&key, slug, arguments).await;
        }
        self.call(
            &key,
            "POST",
            &format!("/api/v3/tools/execute/{slug}"),
            &[],
            Some(&json!({
                "user_id": user_id,
                "version": self.tool_version,
                "arguments": arguments,
            })),
        )
        .await
    }
}

/// Tool names that wrap other tools (run many, run code or shell). The slug
/// does not say what the inner call does, so these always need approval.
fn is_wrapper_tool(slug: &str) -> bool {
    let upper = slug.to_ascii_uppercase();
    upper.starts_with("COMPOSIO_")
        && !upper.starts_with("COMPOSIO_SEARCH")
        && !upper.starts_with("COMPOSIO_GET_")
        && !upper.starts_with("COMPOSIO_MANAGE_CONNECTIONS")
}

/// True when running this tool needs approval: a destructive verb in the
/// slug, or a Connect meta tool that can run anything.
pub fn slug_needs_approval(slug: &str) -> bool {
    slug_is_destructive(slug) || is_wrapper_tool(slug)
}

/// The app a Connect tool belongs to: the slug text before its first `_`.
fn tool_app(slug: &str) -> String {
    slug.split('_')
        .next()
        .unwrap_or_default()
        .to_ascii_lowercase()
}

impl ComposioService {
    /// Tools of a Connect key matching a query. The Connect server may list
    /// only a few meta tools, so when nothing matches the whole (bounded)
    /// list is returned with a note on how to discover app actions.
    async fn consumer_search_tools(
        &self,
        key: &SealedKey,
        query: &str,
        limit: usize,
    ) -> Result<Value> {
        let tools = self.mcp.list_tools(&key.key).await?;
        let words: Vec<String> = query
            .split_whitespace()
            .map(str::to_ascii_lowercase)
            .collect();
        let describe = |item: &Value| {
            let slug = text(item, "name", 128);
            json!({
                "slug": slug,
                "name": text(item, "title", 120),
                "description": text(item, "description", 240),
                "toolkit": tool_app(&slug),
                "needs_approval": slug_needs_approval(&slug),
            })
        };
        let mut scored: Vec<(usize, Value)> = tools
            .iter()
            .filter(|item| valid_slug(&text(item, "name", 129)))
            .map(|item| {
                let haystack = format!(
                    "{} {} {}",
                    text(item, "name", 128),
                    text(item, "title", 120),
                    text(item, "description", 600)
                )
                .to_ascii_lowercase()
                .replace('_', " ");
                let score = words
                    .iter()
                    .filter(|w| haystack.contains(w.as_str()))
                    .count();
                (score, describe(item))
            })
            .collect();
        let matched = scored.iter().any(|(score, _)| *score > 0);
        scored.sort_by_key(|(score, _)| std::cmp::Reverse(*score));
        let picked: Vec<Value> = scored
            .into_iter()
            .filter(|(score, _)| !matched || *score > 0)
            .take(limit.clamp(1, 20))
            .map(|(_, tool)| tool)
            .collect();
        let mut reply = json!({"tools": picked});
        if !matched {
            reply["note"] = json!(
                "No tool matched. These are the tools this Composio Connect key offers; use COMPOSIO_SEARCH_TOOLS through composio_execute to find app actions."
            );
        }
        Ok(reply)
    }

    /// The apps behind a Connect key's tools, grouped by slug prefix.
    async fn consumer_apps(&self, key: &SealedKey, query: &str) -> Result<Value> {
        let tools = self.mcp.list_tools(&key.key).await?;
        let needle = query.trim().to_ascii_lowercase();
        let mut toolkits: Vec<Value> = Vec::new();
        for item in &tools {
            let app = tool_app(&text(item, "name", 128));
            if !valid_toolkit(&app)
                || toolkits.iter().any(|t| t["slug"] == app.as_str())
                || (!needle.is_empty() && !app.contains(&needle))
            {
                continue;
            }
            toolkits.push(json!({
                "slug": app,
                "name": app,
                "description": text(item, "description", 160),
            }));
            if toolkits.len() >= MAX_LIST {
                break;
            }
        }
        Ok(json!({
            "toolkits": toolkits,
            "managed_externally": true,
            "manage_url": CONNECT_URL,
        }))
    }

    /// Run one tool on the Connect endpoint. The reply has the same shape as
    /// a REST execution (`successful`, `error`, `data`).
    async fn consumer_execute(
        &self,
        key: &SealedKey,
        slug: &str,
        arguments: &Value,
    ) -> Result<Value> {
        let result = self.mcp.call_tool(&key.key, slug, arguments).await?;
        let failed = result.get("isError").and_then(Value::as_bool) == Some(true);
        let texts: Vec<&str> = result
            .get("content")
            .and_then(Value::as_array)
            .map(|parts| {
                parts
                    .iter()
                    .filter_map(|part| part.get("text").and_then(Value::as_str))
                    .collect()
            })
            .unwrap_or_default();
        let joined = scrub_key(&texts.join("\n"), &key.key);
        let data = match result.get("structuredContent") {
            Some(value) if !value.is_null() => value.clone(),
            _ => serde_json::from_str::<Value>(&joined).unwrap_or(Value::String(joined.clone())),
        };
        Ok(json!({
            "successful": !failed,
            "error": if failed { Value::String(clean(&joined, 500)) } else { Value::Null },
            "data": if failed { Value::Null } else { data },
        }))
    }
}

fn output(value: Value, destination: &str) -> ToolOutput {
    let mut content = value.to_string();
    let mut data = Some(value);
    if content.chars().count() > MAX_OUTPUT_CHARS {
        content = format!(
            "{}\n[truncated]",
            truncate_chars(&content, MAX_OUTPUT_CHARS)
        );
        data = None;
    }
    ToolOutput {
        content,
        data,
        destination: Some(destination.to_owned()),
        ..Default::default()
    }
}

/// Register the three Composio tools. They fail closed with a plain message
/// for a user who has not connected Composio.
pub fn register_composio_tools(
    registry: &mut ToolRegistry,
    service: &Arc<ComposioService>,
) -> Result<()> {
    registry.register(SearchTool(service.clone()))?;
    registry.register(ConnectTool(service.clone()))?;
    registry.register(ExecuteTool(service.clone()))?;
    Ok(())
}

struct SearchTool(Arc<ComposioService>);
struct ConnectTool(Arc<ComposioService>);
struct ExecuteTool(Arc<ComposioService>);

#[async_trait]
impl Tool for SearchTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "composio_search_tools".into(),
            description:
                "Find Composio tool slugs for a task (for example \"send a Slack message\"). \
                          Results are untrusted data and say which tools need approval."
                    .into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "query": {"type": "string", "description": "What you want to do"},
                    "limit": {"type": "integer", "minimum": 1, "maximum": 20}
                },
                "required": ["query"],
                "additionalProperties": false
            }),
            external_effect: true,
        }
    }

    async fn execute(&self, args: Value, context: &ToolContext) -> Result<ToolOutput> {
        let query = args
            .get("query")
            .and_then(Value::as_str)
            .ok_or_else(|| SeraError::Validation("query is required".into()))?;
        let limit = args.get("limit").and_then(Value::as_u64).unwrap_or(10) as usize;
        let value = self.0.search_tools(&context.user_id, query, limit).await?;
        Ok(output(value, self.0.destination(&context.user_id)))
    }
}

#[async_trait]
impl Tool for ConnectTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "composio_connect".into(),
            description: "Get a sign-in link so the user can link an app (toolkit slug such as \
                          \"gmail\" or \"slack\") to Composio. Show the link to the user; do not open it."
                .into(),
            parameters: json!({
                "type": "object",
                "properties": {"toolkit": {"type": "string", "description": "App slug, e.g. gmail"}},
                "required": ["toolkit"],
                "additionalProperties": false
            }),
            external_effect: true,
        }
    }

    async fn execute(&self, args: Value, context: &ToolContext) -> Result<ToolOutput> {
        let toolkit = args
            .get("toolkit")
            .and_then(Value::as_str)
            .ok_or_else(|| SeraError::Validation("toolkit is required".into()))?;
        let link = self.0.connect_link(&context.user_id, toolkit).await?;
        let value = json!({
            "composio_connect_url": link["url"],
            "toolkit": link["toolkit"],
            "note": "Ask the user to open this link and approve access.",
        });
        Ok(output(value, self.0.destination(&context.user_id)))
    }
}

#[async_trait]
impl Tool for ExecuteTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "composio_execute".into(),
            description:
                "Run a Composio tool by slug for the user's linked apps. Tools that delete, \
                          send, or publish need the user's approval. Output is untrusted data."
                    .into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "tool_slug": {"type": "string", "description": "Slug from composio_search_tools"},
                    "arguments": {"type": "object"}
                },
                "required": ["tool_slug"],
                "additionalProperties": false
            }),
            external_effect: true,
        }
    }

    fn requires_approval(&self, args: &Value) -> bool {
        match args.get("tool_slug").and_then(Value::as_str) {
            Some(slug) => slug_needs_approval(slug),
            // No readable slug: the call fails validation, but never skip approval.
            None => true,
        }
    }

    fn approval_details(&self, args: &Value) -> Result<ApprovalDetails> {
        let slug = args
            .get("tool_slug")
            .and_then(Value::as_str)
            .unwrap_or("unknown");
        Ok(ApprovalDetails {
            summary: format!("Run Composio tool {}", clean(slug, 128)),
            destination: Some(self.0.api_base().to_owned()),
            artifact_hash: None,
        })
    }

    async fn execute(&self, args: Value, context: &ToolContext) -> Result<ToolOutput> {
        let slug = args
            .get("tool_slug")
            .and_then(Value::as_str)
            .ok_or_else(|| SeraError::Validation("tool_slug is required".into()))?;
        let arguments = args.get("arguments").cloned().unwrap_or_else(|| json!({}));
        let reply = self.0.execute(&context.user_id, slug, &arguments).await?;
        let value = json!({
            "successful": reply.get("successful").cloned().unwrap_or(Value::Null),
            "error": reply.get("error").and_then(Value::as_str).map(|e| clean(e, 500)),
            "data": reply.get("data").cloned().unwrap_or(Value::Null),
        });
        Ok(output(value, self.0.destination(&context.user_id)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::Store;
    use wiremock::matchers::{header, method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    const KEY_A: &str = "cmp_synthetic_key_alice_0001";
    const KEY_B: &str = "cmp_synthetic_key_bob_00002";

    fn service(server: &MockServer) -> (Arc<ComposioService>, String, String) {
        let store = Store::open_in_memory(384).unwrap();
        let users = Arc::new(UserSecrets::with_master_key(store.clone(), [7u8; 32]).unwrap());
        let a = store
            .create_user("microsoft:a", "a@example.example", "A")
            .unwrap()
            .id;
        let b = store
            .create_user("microsoft:b", "b@example.example", "B")
            .unwrap()
            .id;
        let settings = ComposioConfig {
            api_base: server.uri(),
            mcp_base: format!("{}/mcp", server.uri()),
            timeout_secs: 5,
            max_response_bytes: 8 * 1024,
            tool_version: None,
        };
        (
            Arc::new(ComposioService::with_settings(&settings, users).unwrap()),
            a,
            b,
        )
    }

    async fn mock_toolkits_ok(server: &MockServer, key: &str) {
        Mock::given(method("GET"))
            .and(path("/api/v3/toolkits"))
            .and(header("x-api-key", key))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"items": []})))
            .mount(server)
            .await;
    }

    fn context(user: &str) -> ToolContext {
        ToolContext {
            user_id: user.into(),
            actor: user.into(),
            bot: "main".into(),
            task_id: None,
            turn_id: None,
            conversation_id: None,
            allowed_tools: Default::default(),
            require_approval: Default::default(),
        }
    }

    #[test]
    fn destructive_rule() {
        for slug in [
            "GMAIL_SEND_EMAIL",
            "SLACK_POST_MESSAGE",
            "GMAIL_CREATE_EMAIL_DRAFT",
            "GITHUB_DELETE_A_REPOSITORY",
            "NOTION_ARCHIVE_NOTION_PAGE",
            "stripe_pay_invoice",
            "X_PUBLISH_POST",
            "DRIVE_TRASH_FILE",
            "BANK_TRANSFER_FUNDS",
            "GITHUB_REMOVE_LABEL",
            "GMAIL_REPLY_TO_THREAD",
            "GMAIL_FORWARD_MESSAGE",
            "GMAIL_DRAFT_SEND",
            "GOOGLEDRIVE_SHARE_FILE",
            "SLACK_INVITE_USER_TO_CHANNEL",
            "X_CREATE_POST",
        ] {
            assert!(slug_is_destructive(slug), "{slug}");
        }
        for slug in [
            "GMAIL_FETCH_EMAILS",
            "GITHUB_LIST_REPOSITORIES",
            "SLACK_LIST_CHANNELS",
            "CALENDAR_FIND_EVENT",
            "SENDGRID_LIST_TEMPLATES",
        ] {
            assert!(!slug_is_destructive(slug), "{slug}");
        }
    }

    #[tokio::test]
    async fn key_is_validated_sealed_and_isolated() {
        let server = MockServer::start().await;
        mock_toolkits_ok(&server, KEY_A).await;
        Mock::given(method("GET"))
            .and(path("/api/v3/toolkits"))
            .and(header("x-api-key", KEY_B))
            .respond_with(ResponseTemplate::new(401).set_body_json(
                json!({"error": {"message": "bad key", "code": 401, "slug": "x", "status": 401}}),
            ))
            .mount(&server)
            .await;
        let (svc, a, b) = service(&server);
        assert!(svc.connect_key(&a, KEY_B).await.is_err());
        assert!(!svc.connected(&a).unwrap());
        svc.connect_key(&a, KEY_A).await.unwrap();
        assert!(svc.connected(&a).unwrap());
        assert!(!svc.connected(&b).unwrap());
        // B cannot use A's key.
        let error = svc.search_tools(&b, "send mail", 5).await.unwrap_err();
        assert!(matches!(error, SeraError::Forbidden(_)));
        assert!(svc.disconnect_key(&a).unwrap());
        assert!(!svc.connected(&a).unwrap());
    }

    #[tokio::test]
    async fn execute_uses_the_acting_users_id_and_key() {
        let server = MockServer::start().await;
        mock_toolkits_ok(&server, KEY_A).await;
        let (svc, a, _b) = service(&server);
        svc.connect_key(&a, KEY_A).await.unwrap();
        Mock::given(method("POST"))
            .and(path("/api/v3/tools/execute/GMAIL_FETCH_EMAILS"))
            .and(header("x-api-key", KEY_A))
            .and(wiremock::matchers::body_json(
                json!({"user_id": a, "version": "latest", "arguments": {"max": 1}}),
            ))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(json!({"data": {"n": 1}, "error": null, "successful": true})),
            )
            .expect(1)
            .mount(&server)
            .await;
        let value = svc
            .execute(&a, "GMAIL_FETCH_EMAILS", &json!({"max": 1}))
            .await
            .unwrap();
        assert_eq!(value["successful"], true);
        assert!(svc.execute(&a, "../x", &json!({})).await.is_err());
        assert!(svc.execute(&a, "OK_SLUG", &json!("str")).await.is_err());
    }

    #[tokio::test]
    async fn responses_are_bounded_and_errors_never_carry_the_key() {
        let server = MockServer::start().await;
        mock_toolkits_ok(&server, KEY_A).await;
        let (svc, a, _b) = service(&server);
        svc.connect_key(&a, KEY_A).await.unwrap();
        Mock::given(method("POST"))
            .and(path("/api/v3/tools/execute/BIG_LIST"))
            .respond_with(ResponseTemplate::new(200).set_body_string("x".repeat(64 * 1024)))
            .mount(&server)
            .await;
        let error = svc.execute(&a, "BIG_LIST", &json!({})).await.unwrap_err();
        assert!(error.to_string().contains("size limit"));
        Mock::given(method("POST"))
            .and(path("/api/v3/tools/execute/BAD"))
            .respond_with(ResponseTemplate::new(500).set_body_json(
                json!({"error": {"message": "boom", "code": 500, "slug": "s", "status": 500}}),
            ))
            .mount(&server)
            .await;
        let error = svc.execute(&a, "BAD", &json!({})).await.unwrap_err();
        assert!(!error.to_string().contains(KEY_A));
        assert!(!format!("{svc:?}").contains(KEY_A));
    }

    #[tokio::test]
    async fn connect_link_and_account_ownership() {
        let server = MockServer::start().await;
        mock_toolkits_ok(&server, KEY_A).await;
        let (svc, a, _b) = service(&server);
        svc.connect_key(&a, KEY_A).await.unwrap();
        Mock::given(method("GET"))
            .and(path("/api/v3/auth_configs"))
            .and(query_param("toolkit_slug", "gmail"))
            .respond_with(ResponseTemplate::new(200).set_body_json(
                json!({"items": [{"id": "ac_1", "status": "ENABLED", "is_composio_managed": true}]}),
            ))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/api/v3/connected_accounts/link"))
            .and(wiremock::matchers::body_json(
                json!({"auth_config_id": "ac_1", "user_id": a}),
            ))
            .respond_with(ResponseTemplate::new(201).set_body_json(json!({
                "redirect_url": "https://connect.example.example/l/abc",
                "link_token": "t", "expires_at": "x", "connected_account_id": "ca_1"
            })))
            .mount(&server)
            .await;
        let link = svc.connect_link(&a, "Gmail").await.unwrap();
        assert_eq!(link["url"], "https://connect.example.example/l/abc");
        assert!(svc.connect_link(&a, "bad slug!").await.is_err());

        Mock::given(method("GET"))
            .and(path("/api/v3/connected_accounts"))
            .and(query_param("user_ids", a.as_str()))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "items": [{"id": "ca_1", "status": "ACTIVE", "toolkit": {"slug": "gmail"}}]
            })))
            .mount(&server)
            .await;
        Mock::given(method("DELETE"))
            .and(path("/api/v3/connected_accounts/ca_1"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"success": true})))
            .expect(1)
            .mount(&server)
            .await;
        let listed = svc.accounts(&a).await.unwrap();
        assert_eq!(listed["accounts"][0]["toolkit"], "gmail");
        // Someone else's account id is refused before any DELETE.
        let error = svc.disconnect_account(&a, "ca_other").await.unwrap_err();
        assert!(matches!(error, SeraError::NotFound(_)));
        svc.disconnect_account(&a, "ca_1").await.unwrap();
    }

    #[tokio::test]
    async fn tools_gate_destructive_slugs() {
        let server = MockServer::start().await;
        mock_toolkits_ok(&server, KEY_A).await;
        let (svc, a, _b) = service(&server);
        svc.connect_key(&a, KEY_A).await.unwrap();
        let tool = ExecuteTool(svc.clone());
        assert!(tool.requires_approval(&json!({"tool_slug": "GMAIL_SEND_EMAIL"})));
        assert!(!tool.requires_approval(&json!({"tool_slug": "GMAIL_FETCH_EMAILS"})));
        assert!(tool.requires_approval(&json!({})));
        Mock::given(method("GET"))
            .and(path("/api/v3/tools"))
            .and(query_param("query", "mail"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"items": [
                {"slug": "GMAIL_SEND_EMAIL", "name": "Send", "description": "d", "toolkit": {"slug": "gmail"}},
                {"slug": "GMAIL_FETCH_EMAILS", "name": "Fetch", "description": "d", "toolkit": {"slug": "gmail"}}
            ]})))
            .mount(&server)
            .await;
        let out = SearchTool(svc)
            .execute(json!({"query": "mail"}), &context(&a))
            .await
            .unwrap();
        let data = out.data.unwrap();
        assert_eq!(data["tools"][0]["needs_approval"], true);
        assert_eq!(data["tools"][1]["needs_approval"], false);
    }

    /// The typed upstream error, with its status and code checked. Returns
    /// the user-facing text.
    fn assert_upstream(error: &SeraError, status: u16, code: &str) -> String {
        match error {
            SeraError::Upstream {
                status: got_status,
                code: got_code,
                message,
            } => {
                assert_eq!(*got_status, status, "{error:?}");
                assert_eq!(*got_code, code, "{error:?}");
                message.clone()
            }
            other => panic!("expected an upstream error, got {other:?}"),
        }
    }

    /// No error text, Display or Debug, may carry the key.
    fn assert_no_key(error: &SeraError, key: &str) {
        assert!(!error.to_string().contains(key), "{error}");
        assert!(!format!("{error:?}").contains(key), "{error:?}");
    }

    /// Mount one canned reply for `GET /api/v3/toolkits` under one key header.
    async fn toolkits_reply(
        server: &MockServer,
        header_name: &'static str,
        key: &str,
        status: u16,
        body: Value,
    ) {
        Mock::given(method("GET"))
            .and(path("/api/v3/toolkits"))
            .and(header(header_name, key))
            .respond_with(ResponseTemplate::new(status).set_body_json(body))
            .mount(server)
            .await;
    }

    #[tokio::test]
    async fn user_level_key_is_retried_once_with_its_own_header_and_kept() {
        let server = MockServer::start().await;
        toolkits_reply(
            &server,
            "x-api-key",
            KEY_A,
            401,
            json!({"error": {"message": "Invalid API key"}}),
        )
        .await;
        toolkits_reply(&server, "x-user-api-key", KEY_A, 200, json!({"items": []})).await;
        let (svc, a, _b) = service(&server);
        svc.connect_key(&a, KEY_A).await.unwrap();
        let raw = svc.users.get(&a, CREDENTIAL_ID).unwrap().unwrap();
        assert_eq!(SealedKey::parse(raw.expose_secret()).header, KeyKind::User);
        // Later calls use the stored header, not x-api-key.
        Mock::given(method("POST"))
            .and(path("/api/v3/tools/execute/GMAIL_FETCH_EMAILS"))
            .and(header("x-user-api-key", KEY_A))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"successful": true})))
            .expect(1)
            .mount(&server)
            .await;
        let value = svc
            .execute(&a, "GMAIL_FETCH_EMAILS", &json!({}))
            .await
            .unwrap();
        assert_eq!(value["successful"], true);
    }

    #[tokio::test]
    async fn project_key_is_never_retried_with_the_user_header() {
        let server = MockServer::start().await;
        mock_toolkits_ok(&server, KEY_A).await;
        Mock::given(method("GET"))
            .and(path("/api/v3/toolkits"))
            .and(header("x-user-api-key", KEY_A))
            .respond_with(ResponseTemplate::new(200))
            .expect(0)
            .mount(&server)
            .await;
        let (svc, a, _b) = service(&server);
        svc.connect_key(&a, KEY_A).await.unwrap();
        let raw = svc.users.get(&a, CREDENTIAL_ID).unwrap().unwrap();
        assert_eq!(
            SealedKey::parse(raw.expose_secret()).header,
            KeyKind::Project
        );
    }

    #[tokio::test]
    async fn key_refused_under_both_headers_reports_the_rejection() {
        let server = MockServer::start().await;
        toolkits_reply(
            &server,
            "x-api-key",
            KEY_A,
            401,
            json!({"error": {"message": "Invalid API key", "suggested_fix": "Check the key"}}),
        )
        .await;
        toolkits_reply(
            &server,
            "x-user-api-key",
            KEY_A,
            401,
            json!({"error": {"message": "Invalid API key"}}),
        )
        .await;
        let (svc, a, _b) = service(&server);
        let error = svc.connect_key(&a, KEY_A).await.unwrap_err();
        let text = assert_upstream(&error, 401, "composio_rejected");
        assert_eq!(
            text,
            "Composio rejected this API key (Invalid API key). Use the project API key from platform.composio.dev → Settings → API keys. SeraBot accepts project keys (ak_), user keys (uak_) and Connect keys (ck_). Composio suggests: Check the key"
        );
        assert_no_key(&error, KEY_A);
        assert!(!svc.connected(&a).unwrap());
    }

    #[tokio::test]
    async fn forbidden_is_a_rejection_and_is_not_retried() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v3/toolkits"))
            .and(header("x-api-key", KEY_A))
            .respond_with(ResponseTemplate::new(403).set_body_json(json!({
                "error": {"message": "Access denied", "suggested_fix": "Link the app first"}
            })))
            .expect(1)
            .mount(&server)
            .await;
        let (svc, a, _b) = service(&server);
        svc.users.set(&a, CREDENTIAL_ID, KEY_A).unwrap();
        let error = svc.search_toolkits(&a, "mail").await.unwrap_err();
        let text = assert_upstream(&error, 403, "composio_rejected");
        assert!(text.starts_with("Composio rejected this API key (Access denied)."));
        assert!(text.ends_with("Composio suggests: Link the app first"));
        assert_no_key(&error, KEY_A);
    }

    #[tokio::test]
    async fn payment_required_names_the_plan_or_credits() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v3/tools"))
            .respond_with(
                ResponseTemplate::new(402)
                    .set_body_json(json!({"error": {"message": "Upgrade your plan"}})),
            )
            .mount(&server)
            .await;
        let (svc, a, _b) = service(&server);
        svc.users.set(&a, CREDENTIAL_ID, KEY_A).unwrap();
        let error = svc.search_tools(&a, "mail", 5).await.unwrap_err();
        assert_eq!(
            assert_upstream(&error, 402, "composio_plan_required"),
            "Composio needs a plan or credits for this request (Upgrade your plan)."
        );
        assert_no_key(&error, KEY_A);
    }

    #[tokio::test]
    async fn not_found_keeps_the_composio_message() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v3/tools/execute/NO_SUCH_TOOL"))
            .respond_with(
                ResponseTemplate::new(404)
                    .set_body_json(json!({"error": {"message": "Tool not found"}})),
            )
            .mount(&server)
            .await;
        let (svc, a, _b) = service(&server);
        svc.users.set(&a, CREDENTIAL_ID, KEY_A).unwrap();
        let error = svc
            .execute(&a, "NO_SUCH_TOOL", &json!({}))
            .await
            .unwrap_err();
        assert_eq!(
            assert_upstream(&error, 404, "composio_not_found"),
            "Composio could not find that (Tool not found)."
        );
    }

    #[tokio::test]
    async fn rate_limit_says_to_try_again_later() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v3/tools/execute/BUSY_TOOL"))
            .respond_with(ResponseTemplate::new(429).set_body_json(json!({"error": {}})))
            .mount(&server)
            .await;
        let (svc, a, _b) = service(&server);
        svc.users.set(&a, CREDENTIAL_ID, KEY_A).unwrap();
        let error = svc.execute(&a, "BUSY_TOOL", &json!({})).await.unwrap_err();
        assert_eq!(
            assert_upstream(&error, 429, "composio_rate_limited"),
            "Composio is rate limiting requests. Try again in a minute."
        );
    }

    #[tokio::test]
    async fn server_error_is_unavailable_with_the_suggested_fix() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v3/tools/execute/DOWN_TOOL"))
            .respond_with(ResponseTemplate::new(500).set_body_json(json!({
                "error": {"message": "boom", "suggested_fix": "Retry later"}
            })))
            .mount(&server)
            .await;
        let (svc, a, _b) = service(&server);
        svc.users.set(&a, CREDENTIAL_ID, KEY_A).unwrap();
        let error = svc.execute(&a, "DOWN_TOOL", &json!({})).await.unwrap_err();
        assert_eq!(
            assert_upstream(&error, 500, "composio_unavailable"),
            "Composio could not be reached (HTTP 500). Composio suggests: Retry later"
        );
        assert_no_key(&error, KEY_A);
    }

    #[tokio::test]
    async fn no_response_is_unavailable_with_status_zero() {
        // A port that was just free: nothing listens there, so the connect fails.
        let port = std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        let settings = ComposioConfig {
            api_base: format!("http://127.0.0.1:{port}"),
            mcp_base: format!("http://127.0.0.1:{port}/mcp"),
            timeout_secs: 5,
            max_response_bytes: 8 * 1024,
            tool_version: None,
        };
        let store = Store::open_in_memory(384).unwrap();
        let users = Arc::new(UserSecrets::with_master_key(store.clone(), [7u8; 32]).unwrap());
        let a = store
            .create_user("microsoft:a", "a@example.example", "A")
            .unwrap()
            .id;
        let svc = ComposioService::with_settings(&settings, users).unwrap();
        svc.users.set(&a, CREDENTIAL_ID, KEY_A).unwrap();
        let error = svc.search_toolkits(&a, "mail").await.unwrap_err();
        assert!(matches!(
            error,
            SeraError::Upstream {
                status: 0,
                code: "composio_unavailable",
                ..
            }
        ));
        assert_no_key(&error, KEY_A);
    }

    #[tokio::test]
    async fn echoed_key_is_redacted_from_upstream_text() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v3/tools/execute/ECHO_TOOL"))
            .respond_with(ResponseTemplate::new(400).set_body_json(json!({
                "error": {
                    "message": format!("unexpected {KEY_A}"),
                    "suggested_fix": format!("send {KEY_A} once"),
                }
            })))
            .mount(&server)
            .await;
        let (svc, a, _b) = service(&server);
        svc.users.set(&a, CREDENTIAL_ID, KEY_A).unwrap();
        let error = svc.execute(&a, "ECHO_TOOL", &json!({})).await.unwrap_err();
        let text = assert_upstream(&error, 400, "composio_bad_request");
        assert!(text.contains("[redacted]"), "{text}");
        assert_no_key(&error, KEY_A);
    }

    #[test]
    fn stored_rows_read_both_formats() {
        let sealed = SealedKey::parse(r#"{"key":"cmp_x","header":"user"}"#);
        assert_eq!(
            (sealed.key.as_str(), sealed.header),
            ("cmp_x", KeyKind::User)
        );
        // A row from before the header was recorded is a bare project key.
        let legacy = SealedKey::parse(KEY_A);
        assert_eq!(
            (legacy.key.as_str(), legacy.header),
            (KEY_A, KeyKind::Project)
        );
    }

    /// A service pinned by `tool_version` (None keeps the default), with KEY_A
    /// stored for one fresh user.
    fn pinned_service(
        server: &MockServer,
        tool_version: Option<&str>,
    ) -> (ComposioService, String) {
        let store = Store::open_in_memory(384).unwrap();
        let users = Arc::new(UserSecrets::with_master_key(store.clone(), [7u8; 32]).unwrap());
        let a = store
            .create_user("microsoft:v", "v@example.example", "V")
            .unwrap()
            .id;
        let settings = ComposioConfig {
            api_base: server.uri(),
            mcp_base: format!("{}/mcp", server.uri()),
            timeout_secs: 5,
            max_response_bytes: 8 * 1024,
            tool_version: tool_version.map(str::to_owned),
        };
        let svc = ComposioService::with_settings(&settings, users).unwrap();
        svc.users.set(&a, CREDENTIAL_ID, KEY_A).unwrap();
        (svc, a)
    }

    #[tokio::test]
    async fn execute_sends_latest_by_default_and_the_configured_pin_otherwise() {
        let server = MockServer::start().await;
        let (default_svc, a) = pinned_service(&server, None);
        Mock::given(method("POST"))
            .and(path("/api/v3/tools/execute/GMAIL_FETCH_EMAILS"))
            .and(wiremock::matchers::body_json(
                json!({"user_id": a, "version": "latest", "arguments": {}}),
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"successful": true})))
            .expect(1)
            .mount(&server)
            .await;
        default_svc
            .execute(&a, "GMAIL_FETCH_EMAILS", &json!({}))
            .await
            .unwrap();

        let (pinned_svc, p) = pinned_service(&server, Some(" 00000000_00 "));
        Mock::given(method("POST"))
            .and(path("/api/v3/tools/execute/GMAIL_FETCH_EMAILS"))
            .and(wiremock::matchers::body_json(
                json!({"user_id": p, "version": "00000000_00", "arguments": {}}),
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"successful": true})))
            .expect(1)
            .mount(&server)
            .await;
        pinned_svc
            .execute(&p, "GMAIL_FETCH_EMAILS", &json!({}))
            .await
            .unwrap();
    }

    #[test]
    fn a_malformed_tool_version_is_refused_at_startup() {
        let store = Store::open_in_memory(384).unwrap();
        let users = Arc::new(UserSecrets::with_master_key(store, [7u8; 32]).unwrap());
        let long = "1".repeat(33);
        for bad in ["bad version!", "x/../y", long.as_str()] {
            let settings = ComposioConfig {
                tool_version: Some(bad.to_owned()),
                ..ComposioConfig::default()
            };
            assert!(
                ComposioService::with_settings(&settings, users.clone()).is_err(),
                "{bad}"
            );
        }
    }

    #[test]
    fn redaction_covers_percent_encoding_and_letter_case() {
        const PLUS_KEY: &str = "cmp+synthetic/key=0004";
        assert_eq!(percent_encoded(PLUS_KEY), "cmp%2Bsynthetic%2Fkey%3D0004");
        let text =
            "a cmp+synthetic/key=0004 b cmp%2bsynthetic%2Fkey%3d0004 c CMP+SYNTHETIC/KEY=0004 d";
        assert_eq!(
            scrub_key(text, PLUS_KEY),
            "a [redacted] b [redacted] c [redacted] d"
        );
        // Text without the key is left exactly as it was.
        assert_eq!(scrub_key("nothing here", PLUS_KEY), "nothing here");
    }

    #[test]
    fn upstream_text_never_carries_the_key_in_any_form() {
        const PLUS_KEY: &str = "cmp+synthetic/key=0004";
        let body = json!({"error": {
            "message": "bad cmp%2Bsynthetic%2Fkey%3D0004",
            "slug": "CMP+SYNTHETIC/KEY=0004",
            "suggested_fix": "send cmp%2bsynthetic%2fkey%3d0004 again",
        }});
        let error = upstream_error(400, &body, PLUS_KEY);
        let text = assert_upstream(&error, 400, "composio_bad_request");
        assert!(!text.to_ascii_lowercase().contains("synthetic"), "{text}");
        assert!(text.contains("[redacted]"), "{text}");
        assert_no_key(&error, PLUS_KEY);
    }
}

#[cfg(test)]
mod consumer_tests {
    use super::*;
    use crate::store::Store;
    use wiremock::matchers::{body_partial_json, header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    const CK: &str = "ck_synthetic_key_carol_0003";
    const AK: &str = "ak_synthetic_key_dave_00004";
    const UAK: &str = "uak_synthetic_key_erin_00005";

    fn service(server: &MockServer) -> (Arc<ComposioService>, String) {
        let store = Store::open_in_memory(384).unwrap();
        let users = Arc::new(UserSecrets::with_master_key(store.clone(), [7u8; 32]).unwrap());
        let user = store
            .create_user("microsoft:c", "c@example.example", "C")
            .unwrap()
            .id;
        let settings = ComposioConfig {
            api_base: server.uri(),
            mcp_base: format!("{}/mcp", server.uri()),
            timeout_secs: 5,
            max_response_bytes: 8 * 1024,
            tool_version: None,
        };
        (
            Arc::new(ComposioService::with_settings(&settings, users).unwrap()),
            user,
        )
    }

    fn sse(message: Value) -> ResponseTemplate {
        // A progress event first, so the answer is not the first event.
        let body = format!(
            "event: message\ndata: {{\"jsonrpc\":\"2.0\",\"method\":\"notifications/message\",\"params\":{{}}}}\n\nevent: message\ndata: {message}\n\n"
        );
        ResponseTemplate::new(200).set_body_raw(body.into_bytes(), "text/event-stream")
    }

    fn rpc(id: u64, result: Value) -> Value {
        json!({"jsonrpc": "2.0", "id": id, "result": result})
    }

    /// A Connect server: initialize (plain JSON, with a session id), the
    /// initialized notification, a `tools/list` that answers as an event
    /// stream, and the session close. Every request must carry the key, and
    /// every request after initialize the session id.
    async fn mount_connect(server: &MockServer, key: &'static str) {
        Mock::given(method("POST"))
            .and(path("/mcp"))
            .and(header("x-consumer-api-key", key))
            .and(body_partial_json(json!({"method": "initialize"})))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("mcp-session-id", "sess-1")
                    .set_body_json(rpc(
                        1,
                        json!({"protocolVersion": "2025-03-26", "capabilities": {},
                               "serverInfo": {"name": "synthetic", "version": "0"}}),
                    )),
            )
            .expect(1..)
            .mount(server)
            .await;
        Mock::given(method("POST"))
            .and(path("/mcp"))
            .and(header("x-consumer-api-key", key))
            .and(header("mcp-session-id", "sess-1"))
            .and(body_partial_json(
                json!({"method": "notifications/initialized"}),
            ))
            .respond_with(ResponseTemplate::new(202))
            .expect(1..)
            .mount(server)
            .await;
        Mock::given(method("POST"))
            .and(path("/mcp"))
            .and(header("x-consumer-api-key", key))
            .and(header("mcp-session-id", "sess-1"))
            .and(body_partial_json(json!({"method": "tools/list"})))
            .respond_with(sse(rpc(
                2,
                json!({"tools": [
                    {"name": "COMPOSIO_SEARCH_TOOLS", "description": "Find app actions"},
                    {"name": "COMPOSIO_MULTI_EXECUTE_TOOL", "description": "Run tools"},
                    {"name": "GMAIL_FETCH_EMAILS", "description": "Fetch mail from gmail"},
                    {"name": "GMAIL_SEND_EMAIL", "description": "Send mail from gmail"},
                    {"name": "bad name!", "description": "ignored"}
                ]}),
            )))
            .expect(1..)
            .mount(server)
            .await;
        Mock::given(method("DELETE"))
            .and(path("/mcp"))
            .and(header("mcp-session-id", "sess-1"))
            .respond_with(ResponseTemplate::new(200))
            .mount(server)
            .await;
    }

    async fn rest_untouched(server: &MockServer) {
        Mock::given(wiremock::matchers::path_regex("^/api/"))
            .respond_with(ResponseTemplate::new(500))
            .expect(0)
            .mount(server)
            .await;
    }

    #[test]
    fn prefixes_pick_the_kind() {
        assert_eq!(KeyKind::detect("ak_abc"), Some(KeyKind::Project));
        assert_eq!(KeyKind::detect("uak_abc"), Some(KeyKind::User));
        assert_eq!(KeyKind::detect("ck_abc"), Some(KeyKind::Consumer));
        assert_eq!(KeyKind::detect("cmp_abc"), None);
        let sealed = SealedKey::parse(r#"{"key":"ck_x","header":"consumer"}"#);
        assert_eq!(sealed.header, KeyKind::Consumer);
    }

    #[tokio::test]
    async fn connect_key_is_checked_over_mcp_and_sealed_as_consumer() {
        let server = MockServer::start().await;
        mount_connect(&server, CK).await;
        rest_untouched(&server).await;
        let (svc, user) = service(&server);
        svc.connect_key(&user, CK).await.unwrap();
        assert_eq!(svc.key_kind(&user).unwrap(), Some(KeyKind::Consumer));
        assert!(svc.connected(&user).unwrap());
        assert!(!format!("{svc:?}").contains(CK));
    }

    #[tokio::test]
    async fn tools_are_searched_and_listed_from_tools_list() {
        let server = MockServer::start().await;
        mount_connect(&server, CK).await;
        rest_untouched(&server).await;
        let (svc, user) = service(&server);
        svc.connect_key(&user, CK).await.unwrap();

        let found = svc.search_tools(&user, "send gmail", 5).await.unwrap();
        let tools = found["tools"].as_array().unwrap();
        assert!(tools.iter().all(|t| t["slug"] != "bad name!"));
        assert_eq!(tools[0]["slug"], "GMAIL_SEND_EMAIL");
        assert_eq!(tools[0]["needs_approval"], true);
        assert!(found.get("note").is_none());

        // Nothing matches: the whole list comes back, with a pointer.
        let none = svc.search_tools(&user, "zzzz", 20).await.unwrap();
        assert_eq!(none["tools"].as_array().unwrap().len(), 4);
        assert!(
            none["note"]
                .as_str()
                .unwrap()
                .contains("COMPOSIO_SEARCH_TOOLS")
        );

        let apps = svc.search_toolkits(&user, "").await.unwrap();
        let slugs: Vec<&str> = apps["toolkits"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|t| t["slug"].as_str())
            .collect();
        assert_eq!(slugs, ["composio", "gmail"]);
        assert_eq!(apps["managed_externally"], true);
        let gmail = svc.search_toolkits(&user, "gma").await.unwrap();
        assert_eq!(gmail["toolkits"].as_array().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn apps_are_managed_in_connect() {
        let server = MockServer::start().await;
        mount_connect(&server, CK).await;
        rest_untouched(&server).await;
        let (svc, user) = service(&server);
        svc.connect_key(&user, CK).await.unwrap();
        let accounts = svc.accounts(&user).await.unwrap();
        assert_eq!(accounts["accounts"].as_array().unwrap().len(), 0);
        assert_eq!(accounts["manage_url"], CONNECT_URL);
        let link = svc.connect_link(&user, "gmail").await.unwrap();
        assert_eq!(link["url"], CONNECT_URL);
        let error = svc.disconnect_account(&user, "ca_1").await.unwrap_err();
        assert!(matches!(error, SeraError::Validation(_)));
    }

    #[tokio::test]
    async fn execute_calls_the_tool_and_reads_an_event_stream_answer() {
        let server = MockServer::start().await;
        mount_connect(&server, CK).await;
        rest_untouched(&server).await;
        let (svc, user) = service(&server);
        svc.connect_key(&user, CK).await.unwrap();
        Mock::given(method("POST"))
            .and(path("/mcp"))
            .and(header("x-consumer-api-key", CK))
            .and(header("mcp-session-id", "sess-1"))
            .and(body_partial_json(json!({
                "method": "tools/call",
                "params": {"name": "GMAIL_FETCH_EMAILS", "arguments": {"max": 1}}
            })))
            .respond_with(sse(rpc(
                2,
                json!({"content": [{"type": "text", "text": "{\"n\":1}"}], "isError": false}),
            )))
            .expect(1)
            .mount(&server)
            .await;
        let reply = svc
            .execute(&user, "GMAIL_FETCH_EMAILS", &json!({"max": 1}))
            .await
            .unwrap();
        assert_eq!(reply["successful"], true);
        assert_eq!(reply["data"]["n"], 1);
        // Slug and arguments are still checked before anything is sent.
        assert!(svc.execute(&user, "../x", &json!({})).await.is_err());
        assert!(svc.execute(&user, "OK_SLUG", &json!("s")).await.is_err());
    }

    #[tokio::test]
    async fn a_tool_error_is_a_failed_result_with_the_key_removed() {
        let server = MockServer::start().await;
        mount_connect(&server, CK).await;
        let (svc, user) = service(&server);
        svc.connect_key(&user, CK).await.unwrap();
        Mock::given(method("POST"))
            .and(path("/mcp"))
            .and(body_partial_json(json!({"method": "tools/call"})))
            .respond_with(ResponseTemplate::new(200).set_body_json(rpc(
                2,
                json!({"content": [{"type": "text", "text": format!("denied for {CK}")}],
                       "isError": true}),
            )))
            .mount(&server)
            .await;
        let reply = svc
            .execute(&user, "GMAIL_FETCH_EMAILS", &json!({}))
            .await
            .unwrap();
        assert_eq!(reply["successful"], false);
        let text = reply.to_string();
        assert!(!text.contains(CK), "{text}");
        assert!(text.contains("[redacted]"), "{text}");
    }

    #[tokio::test]
    async fn json_rpc_errors_never_carry_the_key() {
        let server = MockServer::start().await;
        mount_connect(&server, CK).await;
        let (svc, user) = service(&server);
        svc.connect_key(&user, CK).await.unwrap();
        Mock::given(method("POST"))
            .and(path("/mcp"))
            .and(body_partial_json(json!({"method": "tools/call"})))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "jsonrpc": "2.0", "id": 2,
                "error": {"code": -32602, "message": format!("bad {CK}")}
            })))
            .mount(&server)
            .await;
        let error = svc
            .execute(&user, "GMAIL_FETCH_EMAILS", &json!({}))
            .await
            .unwrap_err();
        assert!(matches!(
            error,
            SeraError::Upstream {
                code: "composio_bad_request",
                ..
            }
        ));
        assert!(!error.to_string().contains(CK));
        assert!(!format!("{error:?}").contains(CK));
    }

    #[tokio::test]
    async fn a_rejected_connect_key_says_which_kind_it_looks_like() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/mcp"))
            .respond_with(
                ResponseTemplate::new(401)
                    .set_body_json(json!({"error": {"message": "Invalid API key: ck_**-o3k"}})),
            )
            .mount(&server)
            .await;
        rest_untouched(&server).await;
        let (svc, user) = service(&server);
        let error = svc.connect_key(&user, CK).await.unwrap_err();
        let SeraError::Upstream {
            status,
            code,
            message,
        } = &error
        else {
            panic!("{error:?}");
        };
        assert_eq!((*status, *code), (401, "composio_rejected"));
        assert!(message.contains("Composio Connect key (ck_)"), "{message}");
        assert!(message.contains("Sessions & API Key"), "{message}");
        assert!(!message.contains(CK));
        assert!(!svc.connected(&user).unwrap());
    }

    #[tokio::test]
    async fn rejected_project_and_user_keys_get_their_own_hint() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v3/toolkits"))
            .respond_with(
                ResponseTemplate::new(401)
                    .set_body_json(json!({"error": {"message": "Invalid API key"}})),
            )
            .mount(&server)
            .await;
        let (svc, user) = service(&server);
        let error = svc.connect_key(&user, AK).await.unwrap_err().to_string();
        assert!(error.contains("project API key (ak_)"), "{error}");
        assert!(error.contains("starts with ck_"), "{error}");
        let error = svc.connect_key(&user, UAK).await.unwrap_err().to_string();
        assert!(error.contains("user API key (uak_)"), "{error}");
        assert!(!error.contains(UAK));
    }

    #[tokio::test]
    async fn prefixed_keys_go_only_to_their_own_header() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v3/toolkits"))
            .and(header("x-api-key", AK))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"items": []})))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/v3/toolkits"))
            .and(header("x-user-api-key", UAK))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"items": []})))
            .expect(1)
            .mount(&server)
            .await;
        // Any other header on REST, or any MCP call, would hit these.
        Mock::given(wiremock::matchers::path("/mcp"))
            .respond_with(ResponseTemplate::new(500))
            .expect(0)
            .mount(&server)
            .await;
        let (svc, user) = service(&server);
        svc.connect_key(&user, AK).await.unwrap();
        assert_eq!(svc.key_kind(&user).unwrap(), Some(KeyKind::Project));
        svc.connect_key(&user, UAK).await.unwrap();
        assert_eq!(svc.key_kind(&user).unwrap(), Some(KeyKind::User));
    }

    #[tokio::test]
    async fn mcp_responses_are_bounded() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/mcp"))
            .respond_with(ResponseTemplate::new(200).set_body_string("x".repeat(64 * 1024)))
            .mount(&server)
            .await;
        let (svc, user) = service(&server);
        let error = svc.connect_key(&user, CK).await.unwrap_err();
        assert!(error.to_string().contains("size limit"), "{error}");
    }

    #[tokio::test]
    async fn mcp_outage_is_unavailable() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/mcp"))
            .respond_with(ResponseTemplate::new(503))
            .mount(&server)
            .await;
        let (svc, user) = service(&server);
        let error = svc.connect_key(&user, CK).await.unwrap_err();
        assert!(matches!(
            error,
            SeraError::Upstream {
                status: 503,
                code: "composio_unavailable",
                ..
            }
        ));
    }

    #[test]
    fn wrapper_tools_always_need_approval() {
        for slug in [
            "COMPOSIO_MULTI_EXECUTE_TOOL",
            "COMPOSIO_REMOTE_WORKBENCH",
            "COMPOSIO_REMOTE_BASH_TOOL",
            "GMAIL_SEND_EMAIL",
        ] {
            assert!(slug_needs_approval(slug), "{slug}");
        }
        for slug in [
            "COMPOSIO_SEARCH_TOOLS",
            "COMPOSIO_GET_TOOL_SCHEMAS",
            "COMPOSIO_MANAGE_CONNECTIONS",
            "GMAIL_FETCH_EMAILS",
        ] {
            assert!(!slug_needs_approval(slug), "{slug}");
        }
    }

    #[test]
    fn bad_mcp_base_is_refused_at_startup() {
        let store = Store::open_in_memory(384).unwrap();
        let users = Arc::new(UserSecrets::with_master_key(store, [7u8; 32]).unwrap());
        for bad in ["", "ftp://x/mcp", "https://u:p@x/mcp"] {
            let settings = ComposioConfig {
                mcp_base: bad.to_owned(),
                ..ComposioConfig::default()
            };
            assert!(
                ComposioService::with_settings(&settings, users.clone()).is_err(),
                "{bad}"
            );
        }
    }
}
