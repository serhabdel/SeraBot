//! Google Antigravity / Cloud Code Private Assist.
//!
//! One-time owner login is OAuth 2.0 PKCE on a loopback redirect. Chat and the
//! model catalog then stay on the documented Cloud Code hosts. Tokens are
//! returned to the caller for the age store and are never written into errors.

use std::collections::BTreeSet;
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};

use rand::RngCore;
use secrecy::{ExposeSecret, SecretString};
use serde_json::{Map, Value, json};
use zeroize::Zeroize;

// The PKCE helpers moved to the shared sign-in framework.
pub use crate::auth_flows::pkce::pkce_s256;
use crate::auth_flows::pkce::{pkce_token_ok, random_state as random_token, state_ok};
use crate::domain::{LlmMessage, Role};
use crate::error::{Result, SeraError};
use crate::gateway::ToolSpec;
use crate::llm::validate_base_url;

pub const ANTIGRAVITY_CLIENT_ID: &str =
    "1071006060591-tmhssin2h21lcre235vtolojh4g403ep.apps.googleusercontent.com";
/// The Antigravity desktop client's secret. This is Google's published
/// installed-application credential, the same one the Antigravity app and the
/// owner's HielCowork ship as a default; it is not a private secret (Google's
/// installed-app flow treats it as public, and PKCE protects the exchange).
/// It is compiled in from the git-ignored `config/agy.local.toml` (see
/// `build.rs`; GitHub push protection rejects it as a literal) so owner builds
/// need no setup step. Without that file the default is empty.
pub const ANTIGRAVITY_DEFAULT_CLIENT_SECRET: &str = env!("SERABOT_AGY_CLIENT_SECRET");
/// Environment override for the client secret (the same name HielCowork reads).
pub const ANTIGRAVITY_CLIENT_SECRET_ENV: &str = "AGY_CLIENT_SECRET";

/// Choose the Antigravity client secret: the age-store / console-saved value,
/// then the `AGY_CLIENT_SECRET` environment value, then the compiled-in default.
/// Blank values are skipped at every step.
pub fn pick_antigravity_client_secret(stored: Option<&str>, env: Option<&str>) -> String {
    [stored, env]
        .into_iter()
        .flatten()
        .map(str::trim)
        .find(|value| !value.is_empty())
        .unwrap_or(ANTIGRAVITY_DEFAULT_CLIENT_SECRET)
        .to_owned()
}

/// The Antigravity client secret for a provider whose override lives under
/// `stored_name` in the age store (empty or absent means no override).
pub fn antigravity_client_secret(
    secrets: &crate::secrets::SharedSecrets,
    stored_name: &str,
) -> String {
    let stored = Some(stored_name.trim())
        .filter(|name| !name.is_empty())
        .and_then(|name| secrets.get(name));
    let env = std::env::var(ANTIGRAVITY_CLIENT_SECRET_ENV).ok();
    pick_antigravity_client_secret(stored.as_deref(), env.as_deref())
}

/// Environment override for the client id (the same name HielCowork reads).
pub const ANTIGRAVITY_CLIENT_ID_ENV: &str = "AGY_CLIENT_ID";

/// Choose the Antigravity client id: the age-store value the provider names,
/// then `AGY_CLIENT_ID`, then the published Antigravity client id. Sign-in and
/// refresh read the same order, so one account always uses one client.
pub fn pick_antigravity_client_id(stored: Option<&str>, env: Option<&str>) -> String {
    [stored, env]
        .into_iter()
        .flatten()
        .map(str::trim)
        .find(|value| !value.is_empty())
        .unwrap_or(ANTIGRAVITY_CLIENT_ID)
        .to_owned()
}

/// The Antigravity client id for a provider whose override lives under
/// `stored_name` in the age store (empty or absent means no override).
pub fn antigravity_client_id(secrets: &crate::secrets::SharedSecrets, stored_name: &str) -> String {
    let stored = Some(stored_name.trim())
        .filter(|name| !name.is_empty())
        .and_then(|name| secrets.get(name));
    let env = std::env::var(ANTIGRAVITY_CLIENT_ID_ENV).ok();
    pick_antigravity_client_id(stored.as_deref(), env.as_deref())
}

/// Where the Antigravity Hub publishes its current release, as HielCowork
/// reads it. Asked once per process, anonymously, for its `version` only.
pub const ANTIGRAVITY_UPDATER_URL: &str =
    "https://antigravity-hub-auto-updater-971186067036.us-central1.run.app/releases/latest";
/// Environment override for the Antigravity Hub version (HielCowork's name).
pub const ANTIGRAVITY_VERSION_ENV: &str = "AGY_VERSION";
/// The live version, once asked (`None` when the updater did not answer).
static LIVE_VERSION: tokio::sync::OnceCell<Option<String>> = tokio::sync::OnceCell::const_new();

/// The version in an updater answer (`{"version": "2.31.4", ...}`), when it
/// is a plain dotted version.
pub fn parse_updater_version(bytes: &[u8]) -> Option<String> {
    if bytes.len() > 16_384 {
        return None;
    }
    let value: Value = serde_json::from_slice(bytes).ok()?;
    let version = value.get("version")?.as_str()?.trim();
    client_version_ok(version).then(|| version.to_owned())
}

/// Ask the Antigravity updater for the current Hub version: one GET, three
/// seconds, no redirects, no credentials. `None` on any failure.
#[cfg(not(test))]
async fn fetch_live_version() -> Option<String> {
    let http = reqwest::Client::builder()
        .timeout(Duration::from_secs(3))
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .ok()?;
    let response = http
        .get(ANTIGRAVITY_UPDATER_URL)
        .header("accept", "application/json")
        .send()
        .await
        .ok()?;
    if !response.status().is_success() {
        return None;
    }
    let bytes = response.bytes().await.ok()?;
    parse_updater_version(&bytes)
}

/// Tests never reach the network: the live version is "not known".
#[cfg(test)]
async fn fetch_live_version() -> Option<String> {
    None
}

/// The Antigravity Hub version this server presents (user agent and
/// `ideVersion`), in HielCowork's order: the provider's own `client_version`,
/// then `AGY_VERSION`, then the live updater's (asked once per process), then
/// the pinned [`IDE_VERSION`]. Google retires old client versions, so a server
/// that never updates its pin still presents the current one.
pub async fn antigravity_version(configured: Option<&str>) -> String {
    let env = std::env::var(ANTIGRAVITY_VERSION_ENV).ok();
    if let Some(version) = [configured, env.as_deref()]
        .into_iter()
        .flatten()
        .map(str::trim)
        .find(|version| client_version_ok(version))
    {
        return version.to_owned();
    }
    LIVE_VERSION
        .get_or_init(fetch_live_version)
        .await
        .clone()
        .unwrap_or_else(|| IDE_VERSION.to_owned())
}

/// The model Cloud Code serves when a Claude 5.5 name is not aliased on the
/// cluster that answered, as HielCowork maps it: plain Opus 5.5 is served as
/// its thinking variant. `None` when there is no other id to try.
pub fn wire_fallback_model(model: &str) -> Option<&'static str> {
    match model.trim().to_ascii_lowercase().as_str() {
        "claude-opus-5-5" => Some("claude-opus-5-5-thinking"),
        _ => None,
    }
}

/// Add the provider's configured models to an account's live Cloud Code list,
/// as HielCowork does: a model Google leaves out of `fetchAvailableModels` (a
/// plain Claude id served through its wire alias) stays selectable. Live
/// entries keep their own metadata.
pub fn with_configured_models<'a>(
    mut catalog: crate::providers::ModelCatalog,
    configured: impl IntoIterator<Item = &'a String>,
) -> crate::providers::ModelCatalog {
    for id in configured {
        if cloud_code_model_id(id) && !legacy_model(id) && !catalog.contains_key(id) {
            catalog.insert(id.clone(), cloud_code_model_meta(id, &Value::Null));
        }
    }
    catalog
}

pub const ANTIGRAVITY_SCOPES: &str = "https://www.googleapis.com/auth/cloud-platform https://www.googleapis.com/auth/userinfo.email https://www.googleapis.com/auth/userinfo.profile https://www.googleapis.com/auth/cclog https://www.googleapis.com/auth/experimentsandconfigs";
pub const CLOUD_CODE_USER_AGENT: &str =
    "antigravity/hub/2.25.0 (aidev_client; os_type=windows; arch=amd64; cl=1003475615)";
pub const CLOUD_CODE_API_CLIENT: &str = "google-cloud-sdk vscode/1.96.0";
pub const THOUGHT_SIGNATURE_SENTINEL: &str = "skip_thought_signature_validator";

/// The Antigravity Hub version the user agent and `ideVersion` carry when the
/// provider sets no `client_version`.
pub const IDE_VERSION: &str = "2.25.0";
const CALLBACK_PATH: &str = "/oauth-callback";
const PREFERRED_PORT: u16 = 51121;
const SESSION_SPACE: u64 = 9_000_000_000_000_000_000;

/// The two hosts project discovery asks, in order (daily, then prod).
const DISCOVERY_HOSTS: &[&str] = &[
    "daily-cloudcode-pa.googleapis.com",
    "cloudcode-pa.googleapis.com",
];

pub const CLOUD_CODE_HOSTS: &[&str] = &[
    "daily-cloudcode-pa.googleapis.com",
    "cloudcode-pa.googleapis.com",
    "daily-cloudcode-pa.sandbox.googleapis.com",
    "autopush-cloudcode-pa.sandbox.googleapis.com",
];

#[derive(Clone, Debug)]
pub struct ChatTarget {
    pub stream: String,
    pub generate: String,
}

#[derive(Debug, Clone)]
pub struct CloudCodeToolCall {
    pub id: String,
    pub name: String,
    pub arguments: Value,
    /// The `thoughtSignature` Google sent with this call, to replay with it.
    pub signature: Option<String>,
}

#[derive(Debug, Default)]
pub struct CloudCodeCompletion {
    pub text: String,
    /// Text of the `thought: true` parts: untrusted display data.
    pub reasoning: String,
    pub tool_calls: Vec<CloudCodeToolCall>,
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub finish_reason: Option<String>,
}

impl CloudCodeCompletion {
    /// Google gave up on the model's tool call and returned nothing else. The
    /// request is worth sending again.
    pub fn is_malformed_call(&self) -> bool {
        self.text.trim().is_empty()
            && self.tool_calls.is_empty()
            && self.finish_reason.as_deref() == Some("MALFORMED_FUNCTION_CALL")
    }
}

/// Times a request that came back as a malformed tool call is sent in all.
pub const MALFORMED_CALL_ATTEMPTS: usize = 3;

pub struct AntigravityLogin {
    pub access_token: SecretString,
    pub refresh_token: SecretString,
    pub project_id: Option<String>,
}

impl std::fmt::Debug for AntigravityLogin {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AntigravityLogin")
            .field("access_token", &"<redacted>")
            .field("refresh_token", &"<redacted>")
            .field("project_id", &self.project_id)
            .finish()
    }
}

/// A version the operator may set in `client_version`: digits and dots, like
/// `2.25.0`.
pub fn client_version_ok(version: &str) -> bool {
    (1..=16).contains(&version.len())
        && version.starts_with(|c: char| c.is_ascii_digit())
        && version.chars().all(|c| c.is_ascii_digit() || c == '.')
        && !version.contains("..")
        && !version.ends_with('.')
}

/// The Cloud Code user agent for `version`; the pinned default when none is set
/// (or the one given is not a plain version).
pub fn cloud_code_user_agent(version: Option<&str>) -> String {
    match version.map(str::trim).filter(|v| client_version_ok(v)) {
        Some(version) => format!(
            "antigravity/hub/{version} (aidev_client; os_type=windows; arch=amd64; cl=1003475615)"
        ),
        None => CLOUD_CODE_USER_AGENT.to_owned(),
    }
}

pub fn validate_project_id(kind: &str, project_id: &str) -> Result<()> {
    if project_id.is_empty() {
        return Ok(());
    }
    let kind = kind.trim();
    let kind = if kind.is_empty() || kind == "openai-compatible" {
        "openai-compatible"
    } else {
        kind
    };
    if kind != "google-antigravity" {
        return Err(SeraError::Config(
            "project_id is only valid for google-antigravity".into(),
        ));
    }
    if !project_id_ok(project_id) {
        return Err(SeraError::Config("invalid Cloud Code project id".into()));
    }
    Ok(())
}

pub fn cloud_code_origins(base: &str) -> Result<Vec<String>> {
    let endpoint = validate_base_url(base)?;
    let parsed = url::Url::parse(&endpoint)
        .map_err(|_| SeraError::Config("invalid provider base url".into()))?;
    if parsed.path() != "/" && !parsed.path().is_empty() {
        return Err(SeraError::Config(
            "google-antigravity base_url must be the Cloud Code origin".into(),
        ));
    }
    let origin = origin_string(&parsed);
    if is_exact_loopback(&parsed) {
        return Ok(vec![origin]);
    }
    let host = parsed.host_str().unwrap_or("");
    if !pinned_cloud_code_host(host) {
        return Err(SeraError::Config(
            "google-antigravity only contacts documented Cloud Code hosts".into(),
        ));
    }
    let mut origins = vec![origin];
    for host in CLOUD_CODE_HOSTS {
        let candidate = format!("https://{host}");
        if !origins
            .iter()
            .any(|item| item.eq_ignore_ascii_case(&candidate))
        {
            origins.push(candidate);
        }
    }
    Ok(origins)
}

pub fn primary_catalog_url(base: &str) -> Result<String> {
    let origin = cloud_code_origins(base)?
        .into_iter()
        .next()
        .ok_or_else(|| SeraError::Config("invalid provider base url".into()))?;
    validate_base_url(&format!("{origin}/v1internal:fetchAvailableModels"))
}

pub fn catalog_urls(catalog_or_base: &str) -> Result<Vec<String>> {
    let parsed = url::Url::parse(catalog_or_base)
        .map_err(|_| SeraError::Config("invalid catalog base url".into()))?;
    let origin = origin_string(&parsed);
    Ok(cloud_code_origins(&origin)?
        .into_iter()
        .map(|item| format!("{item}/v1internal:fetchAvailableModels"))
        .collect())
}

pub fn chat_targets(base: &str) -> Result<Vec<ChatTarget>> {
    Ok(cloud_code_origins(base)?
        .into_iter()
        .map(|origin| ChatTarget {
            stream: format!("{origin}/v1internal:streamGenerateContent?alt=sse"),
            generate: format!("{origin}/v1internal:generateContent"),
        })
        .collect())
}

pub fn authorize_url(redirect_uri: &str, challenge: &str, state: &str) -> Result<String> {
    validate_loopback_redirect(redirect_uri)?;
    if !pkce_token_ok(challenge) || !state_ok(state) {
        return Err(SeraError::External("oauth login could not start".into()));
    }
    let mut url = url::Url::parse("https://accounts.google.com/o/oauth2/v2/auth")
        .map_err(|_| SeraError::External("oauth login could not start".into()))?;
    let env_id = std::env::var(ANTIGRAVITY_CLIENT_ID_ENV).ok();
    let client_id = pick_antigravity_client_id(None, env_id.as_deref());
    url.query_pairs_mut()
        .append_pair("client_id", &client_id)
        .append_pair("response_type", "code")
        .append_pair("redirect_uri", redirect_uri)
        .append_pair("scope", ANTIGRAVITY_SCOPES)
        .append_pair("code_challenge", challenge)
        .append_pair("code_challenge_method", "S256")
        .append_pair("state", state)
        .append_pair("access_type", "offline")
        .append_pair("prompt", "consent");
    if url.host_str() != Some("accounts.google.com") || url.path() != "/o/oauth2/v2/auth" {
        return Err(SeraError::External("oauth login could not start".into()));
    }
    Ok(url.to_string())
}

pub async fn login_antigravity<F>(client_secret: &str, on_url: F) -> Result<AntigravityLogin>
where
    F: FnOnce(&str),
{
    let http = reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|_| SeraError::External("oauth login could not start".into()))?;
    let bases: Vec<String> = CLOUD_CODE_HOSTS
        .iter()
        .map(|host| format!("https://{host}"))
        .collect();
    login_antigravity_at(
        &http,
        client_secret,
        "https://oauth2.googleapis.com/token",
        &bases,
        Duration::from_secs(300),
        Duration::from_secs(2),
        on_url,
    )
    .await
}

pub fn parse_cloud_code_catalog(bytes: &[u8]) -> Result<BTreeSet<String>> {
    parse_cloud_code_models(bytes).map(|models| models.into_keys().collect())
}

/// Ids Google still lists but that are retired: the 4.6 Claude models left
/// Antigravity with the 5.5 generation.
fn legacy_model(id: &str) -> bool {
    id.starts_with("claude-") && (id.ends_with("-4-6") || id.contains("-4-6-"))
}

/// The usable models of a `fetchAvailableModels` answer with what it says about
/// each. Tab-completion, image, audio and deprecated ids and the legacy `4-6`
/// Claude ids are left out. Context and output limits come from `maxTokens` and
/// `maxOutputTokens`, with the family defaults when absent.
pub fn parse_cloud_code_models(bytes: &[u8]) -> Result<crate::providers::ModelCatalog> {
    if bytes.len() > 1_048_576 {
        return Err(SeraError::External("model catalog is too large".into()));
    }
    let value: Value = serde_json::from_slice(bytes)
        .map_err(|_| SeraError::External("malformed model catalog".into()))?;
    let models = value
        .get("models")
        .and_then(Value::as_object)
        .ok_or_else(|| {
            SeraError::External("model catalog was not a Cloud Code model map".into())
        })?;
    if models.len() > 4_000 {
        return Err(SeraError::External("model catalog is too large".into()));
    }
    let excluded = id_set(value.get("tabModelIds"))
        .into_iter()
        .chain(id_set(value.get("imageGenerationModelIds")))
        .chain(id_set(value.get("audioTranscriptionModelIds")))
        .chain(id_set(value.get("deprecatedModelIds")))
        .collect::<BTreeSet<_>>();
    let mut catalog = crate::providers::ModelCatalog::new();
    for (id, entry) in models {
        if excluded.contains(id) || !cloud_code_model_id(id) || legacy_model(id) {
            continue;
        }
        catalog.insert(id.clone(), cloud_code_model_meta(id, entry));
    }
    if catalog.is_empty() {
        return Err(SeraError::External(
            "model catalog contained no usable model ids".into(),
        ));
    }
    Ok(catalog)
}

fn cloud_code_model_meta(id: &str, entry: &Value) -> crate::providers::ModelMeta {
    let limit = |key: &str, fallback: u64| {
        entry
            .get(key)
            .and_then(json_u64)
            .filter(|tokens| *tokens > 0)
            .unwrap_or(fallback)
    };
    let mut tags = vec!["tools".to_owned(), "reasoning".to_owned()];
    if !id.contains("gpt-oss") {
        tags.push("vision".to_owned());
    }
    crate::providers::ModelMeta {
        name: entry
            .get("displayName")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|name| !name.is_empty() && name.len() <= 120 && *name != id)
            .filter(|name| !name.chars().any(char::is_control))
            .map(str::to_owned),
        context_length: Some(limit("maxTokens", 1_048_576)),
        max_output_tokens: Some(limit("maxOutputTokens", u64::from(output_cap(id)))),
        tags,
    }
}

pub fn parse_cloud_code_completion(bytes: &[u8]) -> Result<CloudCodeCompletion> {
    let mut completion = parse_cloud_code_stream(bytes)?;
    if completion.text.trim().is_empty() && completion.tool_calls.is_empty() {
        return Err(SeraError::External(
            "Cloud Code response contained no content".into(),
        ));
    }
    if completion.text.trim().is_empty() {
        completion.text.clear();
    }
    Ok(completion)
}

/// Streaming parser mode for Cloud Code 200 responses.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum StreamMode {
    #[default]
    Undecided,
    Sse,
    RawJson,
}

/// Incremental SSE / JSON accumulator for Cloud Code responses.
/// Reads chunks up to 4 MiB and parses complete `data:` lines immediately instead
/// of buffering a full multi-megabyte SSE stream in memory.
#[derive(Debug, Default)]
pub struct CloudCodeStreamReader {
    completion: CloudCodeCompletion,
    total_bytes: usize,
    line_buf: Vec<u8>,
    raw_json: Vec<u8>,
    mode: StreamMode,
    saw_event: bool,
}

impl CloudCodeStreamReader {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push_chunk(&mut self, chunk: &[u8]) -> Result<()> {
        self.total_bytes = self.total_bytes.saturating_add(chunk.len());
        if self.total_bytes > 4_194_304 {
            return Err(SeraError::External(
                "Cloud Code response is too large".into(),
            ));
        }
        if self.mode == StreamMode::RawJson {
            self.raw_json.extend_from_slice(chunk);
            return Ok(());
        }
        let mut start = 0usize;
        while let Some(pos) = chunk[start..].iter().position(|&b| b == b'\n') {
            let end = start + pos;
            self.line_buf.extend_from_slice(&chunk[start..end]);
            let line = std::mem::take(&mut self.line_buf);
            self.process_line(&line)?;
            if self.mode == StreamMode::RawJson {
                self.raw_json.extend_from_slice(&chunk[end + 1..]);
                return Ok(());
            }
            start = end + 1;
        }
        if start < chunk.len() {
            self.line_buf.extend_from_slice(&chunk[start..]);
        }
        Ok(())
    }

    fn process_line(&mut self, raw_line: &[u8]) -> Result<()> {
        let line = std::str::from_utf8(raw_line)
            .map_err(|_| SeraError::External("malformed Cloud Code response".into()))?;
        let trimmed = line.trim();
        if self.mode == StreamMode::Undecided {
            if trimmed.is_empty()
                || trimmed.starts_with(':')
                || trimmed.starts_with("id:")
                || trimmed.starts_with("retry:")
            {
                return Ok(());
            }
            if trimmed.starts_with("event:") || trimmed.starts_with("data:") {
                self.mode = StreamMode::Sse;
            } else if trimmed.starts_with('{') {
                if let Ok(value) = serde_json::from_str::<Value>(trimmed) {
                    self.mode = StreamMode::Sse;
                    absorb_completion(&mut self.completion, &value)?;
                    self.saw_event = true;
                    return Ok(());
                }
                // Pretty-printed multi-line JSON body rather than NDJSON/SSE.
                self.mode = StreamMode::RawJson;
                self.raw_json.extend_from_slice(raw_line);
                self.raw_json.push(b'\n');
                return Ok(());
            } else {
                self.mode = StreamMode::Sse;
            }
        }
        let payload = if let Some(data) = trimmed.strip_prefix("data:") {
            data.trim()
        } else if trimmed.starts_with('{') {
            trimmed
        } else {
            return Ok(());
        };
        if payload.is_empty() || payload == "[DONE]" {
            return Ok(());
        }
        let Ok(value) = serde_json::from_str::<Value>(payload) else {
            return Ok(());
        };
        absorb_completion(&mut self.completion, &value)?;
        self.saw_event = true;
        Ok(())
    }

    pub fn finish(mut self) -> Result<CloudCodeCompletion> {
        if self.mode == StreamMode::RawJson {
            self.raw_json.extend_from_slice(&self.line_buf);
            let text = std::str::from_utf8(&self.raw_json)
                .map_err(|_| SeraError::External("malformed Cloud Code response".into()))?;
            let value: Value = serde_json::from_str(text.trim())
                .map_err(|_| SeraError::External("malformed Cloud Code response".into()))?;
            absorb_completion(&mut self.completion, &value)?;
            return Ok(self.completion);
        }
        if !self.line_buf.is_empty() {
            let line = std::mem::take(&mut self.line_buf);
            self.process_line(&line)?;
            if self.mode == StreamMode::RawJson {
                let text = std::str::from_utf8(&self.raw_json)
                    .map_err(|_| SeraError::External("malformed Cloud Code response".into()))?;
                let value: Value = serde_json::from_str(text.trim())
                    .map_err(|_| SeraError::External("malformed Cloud Code response".into()))?;
                absorb_completion(&mut self.completion, &value)?;
                return Ok(self.completion);
            }
        }
        if !self.saw_event {
            return Err(SeraError::External("malformed Cloud Code response".into()));
        }
        Ok(self.completion)
    }
}

/// Read a Cloud Code answer (an SSE stream or one JSON body) without judging
/// whether it said anything; see [`CloudCodeCompletion::is_malformed_call`].
pub fn parse_cloud_code_stream(bytes: &[u8]) -> Result<CloudCodeCompletion> {
    let mut reader = CloudCodeStreamReader::new();
    reader.push_chunk(bytes)?;
    reader.finish()
}

pub fn sanitize_schema(value: &Value) -> Value {
    if !value.is_object() {
        return placeholder_object();
    }
    let mut value = value.clone();
    let defs = value
        .get("$defs")
        .cloned()
        .or_else(|| value.get("definitions").cloned())
        .unwrap_or(Value::Null);
    let mut stack = Vec::new();
    hoist_refs(&mut value, &defs, 0, &mut stack);
    sanitize_schema_in(&mut value, 0);
    if !value.is_object() {
        placeholder_object()
    } else {
        value
    }
}

pub fn build_cloud_code_envelope(
    model: &str,
    project_id: &str,
    messages: &[LlmMessage],
    tools: &[ToolSpec],
    max_output_tokens: u32,
    temperature: Option<f32>,
) -> Result<Value> {
    build_cloud_code_envelope_with(
        model,
        project_id,
        messages,
        tools,
        max_output_tokens,
        temperature,
        &EnvelopeOptions::default(),
    )
}

pub fn build_cloud_code_envelope_with(
    model: &str,
    project_id: &str,
    messages: &[LlmMessage],
    tools: &[ToolSpec],
    max_output_tokens: u32,
    temperature: Option<f32>,
    options: &EnvelopeOptions<'_>,
) -> Result<Value> {
    if !cloud_code_model_id(model) {
        return Err(SeraError::Config("invalid configured model id".into()));
    }
    if !project_id.is_empty() && !project_id_ok(project_id) {
        return Err(SeraError::Config("invalid Cloud Code project id".into()));
    }
    if tools.len() > 128 {
        return Err(SeraError::Validation("too many tools".into()));
    }
    let mut system = Vec::new();
    let mut names = std::collections::BTreeMap::new();
    for message in messages {
        if message.role == Role::System
            && let Some(text) = message
                .content
                .as_deref()
                .map(str::trim)
                .filter(|text| !text.is_empty())
        {
            system.push(text.to_owned());
        }
        if message.role == Role::Assistant {
            for call in &message.tool_calls {
                names.insert(call.id.clone(), wire_tool_name(&call.name));
            }
        }
    }
    let mut turns: Vec<DraftTurn> = Vec::new();
    for message in messages {
        match message.role {
            Role::System => {}
            Role::User => {
                let mut parts = Vec::new();
                if let Some(text) = non_empty(message.content.as_deref()) {
                    parts.push(json!({"text": text}));
                }
                for img in &message.images {
                    parts.push(json!({
                        "inlineData": {
                            "mimeType": img.mime,
                            "data": img.data_base64,
                        }
                    }));
                }
                if !parts.is_empty() {
                    push_turn(&mut turns, "user", parts, false);
                }
            }
            Role::Assistant => {
                let mut parts = Vec::new();
                if let Some(text) = non_empty(message.content.as_deref()) {
                    parts.push(json!({"text": text}));
                }
                for call in &message.tool_calls {
                    let id = if call_id_ok(&call.id) {
                        call.id.clone()
                    } else {
                        format!("call_{}", parts.len())
                    };
                    let arguments = call
                        .arguments
                        .as_object()
                        .cloned()
                        .map(Value::Object)
                        .unwrap_or_else(|| json!({}));
                    // The signature Google gave this call goes back with it; the
                    // sentinel stands in only when there is none.
                    let signature = call
                        .reasoning_signature
                        .as_deref()
                        .filter(|signature| signature_ok(signature))
                        .unwrap_or(THOUGHT_SIGNATURE_SENTINEL);
                    parts.push(json!({
                        "thoughtSignature": signature,
                        "functionCall": {
                            "id": id,
                            "name": wire_tool_name(&call.name),
                            "args": arguments,
                        }
                    }));
                }
                push_turn(&mut turns, "model", parts, false);
            }
            Role::Tool => {
                let id = message
                    .tool_call_id
                    .as_deref()
                    .filter(|id| call_id_ok(id))
                    .map(str::to_owned)
                    .unwrap_or_else(|| format!("call_{}", turns.len()));
                let name = message
                    .name
                    .as_deref()
                    .map(wire_tool_name)
                    .filter(|name| tool_name_ok(name))
                    .or_else(|| names.get(&id).cloned())
                    .unwrap_or_else(|| sanitize_tool_name(&id));
                let result = message.content.clone().unwrap_or_default();
                push_turn(
                    &mut turns,
                    "user",
                    vec![json!({
                        "functionResponse": {
                            "id": id,
                            "name": name,
                            "response": {"result": result, "is_error": message.is_error}
                        }
                    })],
                    true,
                );
            }
        }
    }
    // Claude rejects an assistant prefill: the request must end on the user.
    if claude_model(model) {
        while turns.last().is_some_and(|turn| turn.role == "model") {
            turns.pop();
        }
    }
    if turns.is_empty() {
        push_turn(&mut turns, "user", vec![json!({"text": "Hello"})], false);
    }
    let contents = turns
        .into_iter()
        .map(|turn| json!({"role": turn.role, "parts": turn.parts}))
        .collect::<Vec<_>>();
    let mut request = json!({
        "sessionId": session_id(project_id, options.conversation, options.session_generation),
        "contents": contents,
        "generationConfig": generation_config(
            model,
            max_output_tokens,
            temperature,
            options.thinking_level,
        ),
    });
    if !system.is_empty() {
        request["systemInstruction"] = json!({"parts": [{"text": system.join("\n\n")}]});
    }
    let declarations = tool_declarations(tools);
    if !declarations.is_empty() {
        request["tools"] = json!([{"functionDeclarations": declarations}]);
        request["toolConfig"] = json!({"functionCallingConfig": {"mode": "VALIDATED"}});
    }
    let mut envelope = json!({
        "model": model,
        "userAgent": "antigravity",
        "requestType": "agent",
        "requestId": fresh_request_id(),
        "request": request,
    });
    if !project_id.is_empty() {
        envelope["project"] = json!(project_id);
    }
    Ok(envelope)
}

/// Finish a PKCE sign-in whose browser leg ran on the owner's machine: the
/// console holds the verifier and loopback listener, this side holds the client
/// secret. Tokens go back to the caller only as `SecretString`.
pub async fn complete_antigravity_login(
    client_secret: &str,
    code: &str,
    redirect_uri: &str,
    verifier: &str,
) -> Result<AntigravityLogin> {
    let http = reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|_| SeraError::External("oauth login could not start".into()))?;
    let bases: Vec<String> = CLOUD_CODE_HOSTS
        .iter()
        .map(|host| format!("https://{host}"))
        .collect();
    complete_antigravity_login_at(
        &http,
        client_secret,
        "https://oauth2.googleapis.com/token",
        &bases,
        Duration::from_secs(2),
        code,
        redirect_uri,
        verifier,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
async fn complete_antigravity_login_at(
    http: &reqwest::Client,
    client_secret: &str,
    token_url: &str,
    bases: &[String],
    pause: Duration,
    code: &str,
    redirect_uri: &str,
    verifier: &str,
) -> Result<AntigravityLogin> {
    if !credential_ok(client_secret) {
        return Err(SeraError::Config("oauth client secret is missing".into()));
    }
    if !credential_ok(code) || code.len() > 2_048 || !pkce_token_ok(verifier) {
        return Err(SeraError::Validation(
            "sign-in code or verifier is not valid".into(),
        ));
    }
    let token_url = pin_token_url(token_url)?;
    let (access_token, refresh_token) = exchange_code(
        http,
        &token_url,
        client_secret,
        code,
        redirect_uri,
        verifier,
    )
    .await?;
    let project_id = discover_project(http, access_token.expose_secret(), bases, pause).await;
    Ok(AntigravityLogin {
        access_token,
        refresh_token,
        project_id,
    })
}

async fn login_antigravity_at<F>(
    http: &reqwest::Client,
    client_secret: &str,
    token_url: &str,
    bases: &[String],
    timeout: Duration,
    pause: Duration,
    on_url: F,
) -> Result<AntigravityLogin>
where
    F: FnOnce(&str),
{
    if !credential_ok(client_secret) {
        return Err(SeraError::Config("oauth client secret is missing".into()));
    }
    let token_url = pin_token_url(token_url)?;
    let listeners = bind_loopback().await?;
    // The Antigravity registration names `localhost` (as HielCowork signs in);
    // the browser reaches the IPv4 or IPv6 listener through it.
    let redirect_uri = format!("http://localhost:{}{CALLBACK_PATH}", listeners.port);
    let (mut verifier, challenge) = pkce_s256();
    let state = random_token();
    let url = authorize_url(&redirect_uri, &challenge, &state)?;
    on_url(&url);
    let code = match tokio::time::timeout(timeout, accept_callback(listeners, &state)).await {
        Ok(result) => result,
        Err(_) => {
            verifier.zeroize();
            return Err(SeraError::External("Google sign-in timed out".into()));
        }
    };
    let mut code = match code {
        Ok(code) => code,
        Err(error) => {
            verifier.zeroize();
            return Err(error);
        }
    };
    let mut secret = client_secret.to_owned();
    let grant = exchange_code(http, &token_url, &secret, &code, &redirect_uri, &verifier).await;
    verifier.zeroize();
    code.zeroize();
    secret.zeroize();
    let (access_token, refresh_token) = grant?;
    let project_id = discover_project(http, access_token.expose_secret(), bases, pause).await;
    Ok(AntigravityLogin {
        access_token,
        refresh_token,
        project_id,
    })
}

/// What the bot asks of one Cloud Code request beyond the messages.
#[derive(Debug, Clone, Default)]
pub struct EnvelopeOptions<'a> {
    /// The conversation the request belongs to; part of the session id.
    pub conversation: &'a str,
    /// How many times the account's session was rotated after Google's
    /// 1M-token wall (see [`session_id`]).
    pub session_generation: i64,
    /// The thinking level the bot chose (`off`, `low`, `medium`, `high`, `max`).
    pub thinking_level: Option<&'a str>,
}

/// FNV-1a, 64 bit.
pub fn fnv1a64(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 14_695_981_039_346_656_037;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(1_099_511_628_211);
    }
    hash
}

/// The Cloud Code `sessionId`: stable for one account and conversation so the
/// server-side cache and the 1M-token session accumulator behave, and rotated
/// by `generation` when Google says the session is full.
///
/// FNV-1a 64 over `"{project}:{conversation}"` (`":gen{N}"` appended when `N`
/// is above zero), folded with `% 9e18` and prefixed with `-`. The project is
/// `default` when the account has none.
pub fn session_id(project: &str, conversation: &str, generation: i64) -> String {
    let account = if project.is_empty() {
        "default"
    } else {
        project
    };
    let key = if generation > 0 {
        format!("{account}:{conversation}:gen{generation}")
    } else {
        format!("{account}:{conversation}")
    };
    let folded = fnv1a64(key.as_bytes()) % SESSION_SPACE;
    format!("-{}", folded.max(1))
}

fn claude_model(model: &str) -> bool {
    model.to_ascii_lowercase().contains("claude")
}

/// The largest reply Cloud Code accepts for a model family.
fn output_cap(model: &str) -> u32 {
    if claude_model(model) { 64_000 } else { 65_536 }
}

/// The reply size to ask for. The server's default of 4096 (or less) means "no
/// opinion" and takes the model's cap; anything larger is kept. Always inside
/// `[1024, cap]`.
pub fn effective_max_output(model: &str, requested: u32) -> u32 {
    let cap = output_cap(model);
    let wanted = if requested <= 4_096 { cap } else { requested };
    wanted.clamp(1_024, cap)
}

/// The `thinkingConfig` for one request, or `None` when none is sent.
///
/// - Level models (Gemini 3 and 4, `-tiered`): `off` becomes
///   `thinkingBudget: 0`; `low`, `medium` and `high` map to `thinkingLevel`,
///   `max` to `high`. An id that already ends in `-low`, `-medium` or `-high`
///   carries its level and gets no config.
/// - Claude: `off` sends nothing; the others are budgets of 4000, 10000, 24000
///   and 32000 tokens, kept inside `[1024, max_out - 1024]` and sent only when
///   `max_out` is above the budget. With no level, a `-thinking` id uses
///   `medium` and any other id sends nothing.
pub fn antigravity_thinking(model: &str, level: Option<&str>, max_out: u32) -> Option<Value> {
    let lower = model.to_ascii_lowercase();
    if lower.contains("claude") {
        let level = match level {
            Some(level) => level,
            None if lower.contains("thinking") => "medium",
            None => return None,
        };
        let budget = match level {
            "low" => 4_000u32,
            "medium" => 10_000,
            "high" => 24_000,
            "max" | "xhigh" => 32_000,
            "off" => return None,
            _ => 10_000,
        };
        let ceiling = max_out.saturating_sub(1_024).max(1_024);
        let budget = budget.clamp(1_024, ceiling);
        return (max_out > budget).then(|| {
            json!({
                "thinkingBudget": budget,
                "includeThoughts": true,
            })
        });
    }
    if lower.starts_with("gemini-2.5") {
        let level = level?;
        if level == "off" {
            return (!lower.contains("pro")).then(|| json!({"thinkingBudget": 0}));
        }
        let budget = match level {
            "low" => 4_096u32,
            "high" | "max" | "xhigh" => 24_576,
            _ => 10_240,
        };
        let ceiling = max_out.saturating_sub(1_024).max(1_024);
        let budget = budget.clamp(1_024, ceiling);
        return (max_out > budget).then(|| {
            json!({
                "thinkingBudget": budget,
                "includeThoughts": true,
            })
        });
    }
    if !level_thinking_model(&lower) {
        return None;
    }
    let level = level?;
    Some(match level {
        "off" => json!({"thinkingBudget": 0}),
        "low" => json!({"thinkingLevel": "low", "includeThoughts": true}),
        "high" | "max" | "xhigh" => json!({"thinkingLevel": "high", "includeThoughts": true}),
        _ => json!({"thinkingLevel": "medium", "includeThoughts": true}),
    })
}

fn generation_config(
    model: &str,
    max_output_tokens: u32,
    temperature: Option<f32>,
    thinking_level: Option<&str>,
) -> Value {
    let max_out = effective_max_output(model, max_output_tokens);
    let mut config = json!({"maxOutputTokens": max_out});
    let thinking = antigravity_thinking(model, thinking_level, max_out);
    // Claude rejects a temperature while it thinks; other models keep it.
    let drop_temperature = claude_model(model) && thinking.is_some();
    if let Some(temperature) = temperature.filter(|_| !drop_temperature) {
        config["temperature"] = json!(temperature.clamp(0.0, 2.0));
    }
    if let Some(thinking) = thinking {
        config["thinkingConfig"] = thinking;
    }
    config
}

fn level_thinking_model(model: &str) -> bool {
    if model.contains("claude") || model.contains("gpt-oss") {
        return false;
    }
    if model.ends_with("-tiered") {
        return true;
    }
    if model.ends_with("-low") || model.ends_with("-medium") || model.ends_with("-high") {
        return false;
    }
    model.starts_with("gemini-3") || model.starts_with("gemini-4")
}

struct DraftTurn {
    role: &'static str,
    parts: Vec<Value>,
    function_only: bool,
}

fn push_turn(
    turns: &mut Vec<DraftTurn>,
    role: &'static str,
    parts: Vec<Value>,
    function_only: bool,
) {
    if parts.is_empty() {
        return;
    }
    if let Some(last) = turns.last_mut()
        && last.role == role
        && last.function_only == function_only
    {
        last.parts.extend(parts);
        return;
    }
    turns.push(DraftTurn {
        role,
        parts,
        function_only,
    });
}

fn non_empty(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|text| !text.is_empty())
}

fn absorb_completion(completion: &mut CloudCodeCompletion, value: &Value) -> Result<()> {
    // A stream can end with an error object instead of a candidate.
    if let Some(error) = value.get("error").filter(|error| error.is_object()) {
        let message = error
            .get("message")
            .and_then(Value::as_str)
            .map(|text| clean_text(text, 200))
            .unwrap_or_default();
        return Err(match error.get("code").and_then(json_u64) {
            Some(code @ 400..=599) => SeraError::External(format!("HTTP {code}: {message}")),
            _ => {
                let status = error
                    .get("status")
                    .and_then(Value::as_str)
                    .map(|text| clean_text(text, 40))
                    .unwrap_or_default();
                SeraError::External(format!("Antigravity error ({status}): {message}"))
            }
        });
    }
    let root = value.get("response").unwrap_or(value);
    if let Some(usage) = root.get("usageMetadata") {
        if let Some(prompt) = usage.get("promptTokenCount").and_then(json_u64) {
            let cached = usage
                .get("cachedContentTokenCount")
                .and_then(json_u64)
                .unwrap_or(0);
            completion.prompt_tokens = prompt.saturating_sub(cached);
        }
        // Output counts what the model thought as well as what it said.
        let output = usage
            .get("candidatesTokenCount")
            .and_then(json_u64)
            .unwrap_or(0);
        let thoughts = usage
            .get("thoughtsTokenCount")
            .and_then(json_u64)
            .unwrap_or(0);
        if output > 0 || thoughts > 0 {
            completion.completion_tokens = output.saturating_add(thoughts);
        }
    }
    let Some(candidate) = root
        .get("candidates")
        .and_then(Value::as_array)
        .and_then(|items| items.first())
    else {
        return Ok(());
    };
    if let Some(reason) = candidate
        .get("finishReason")
        .and_then(Value::as_str)
        .and_then(clean_finish)
    {
        completion.finish_reason = Some(reason);
    }
    let Some(parts) = candidate
        .pointer("/content/parts")
        .and_then(Value::as_array)
    else {
        return Ok(());
    };
    for part in parts {
        if part.get("thought").and_then(Value::as_bool) == Some(true) {
            if let Some(text) = part.get("text").and_then(Value::as_str)
                && completion.reasoning.len() < crate::thinking::MAX_REASONING_CHARS * 4
            {
                completion.reasoning.push_str(text);
            }
            continue;
        }
        if let Some(text) = part.get("text").and_then(Value::as_str) {
            completion.text.push_str(text);
        }
        if let Some(call) = part.get("functionCall") {
            let Some(name) = call.get("name").and_then(Value::as_str) else {
                continue;
            };
            if !tool_name_ok(name) {
                continue;
            }
            let id = call
                .get("id")
                .and_then(Value::as_str)
                .filter(|id| call_id_ok(id))
                .map(str::to_owned)
                .unwrap_or_else(|| format!("call_{}", completion.tool_calls.len()));
            let arguments = match call.get("args") {
                Some(Value::Object(map)) => Value::Object(map.clone()),
                Some(Value::String(text)) => {
                    serde_json::from_str(text).unwrap_or_else(|_| json!({}))
                }
                _ => json!({}),
            };
            let signature = part
                .get("thoughtSignature")
                .and_then(Value::as_str)
                .filter(|signature| signature_ok(signature))
                .map(str::to_owned);
            if let Some(existing) = completion.tool_calls.iter_mut().find(|item| item.id == id) {
                existing.name = name.to_owned();
                existing.arguments = arguments;
                if signature.is_some() {
                    existing.signature = signature;
                }
            } else {
                completion.tool_calls.push(CloudCodeToolCall {
                    id,
                    name: name.to_owned(),
                    arguments,
                    signature,
                });
            }
        }
    }
    Ok(())
}

fn json_u64(value: &Value) -> Option<u64> {
    value.as_u64().or_else(|| {
        value
            .as_i64()
            .filter(|number| *number >= 0)
            .map(|number| number as u64)
    })
}

fn clean_finish(value: &str) -> Option<String> {
    if value.eq_ignore_ascii_case("FINISH_REASON_UNSPECIFIED") {
        return None;
    }
    if (1..=64).contains(&value.len())
        && value.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
    {
        Some(value.to_owned())
    } else {
        None
    }
}

fn id_set(value: Option<&Value>) -> BTreeSet<String> {
    value
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .filter(|id| cloud_code_model_id(id))
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

fn hoist_refs(value: &mut Value, defs: &Value, depth: u8, stack: &mut Vec<String>) {
    if depth > 8 {
        *value = json!({"type": "STRING"});
        return;
    }
    let reference = value.get("$ref").and_then(Value::as_str).map(str::to_owned);
    if let Some(reference) = reference {
        if stack.iter().any(|item| item == &reference) {
            *value = json!({"type": "STRING"});
            return;
        }
        let name = reference
            .strip_prefix("#/$defs/")
            .or_else(|| reference.strip_prefix("#/definitions/"));
        if let Some(name) = name
            && let Some(target) = defs.get(name).cloned()
        {
            stack.push(reference);
            *value = target;
            hoist_refs(value, defs, depth + 1, stack);
            stack.pop();
            return;
        }
    }
    match value {
        Value::Array(items) => {
            for item in items {
                hoist_refs(item, defs, depth + 1, stack);
            }
        }
        Value::Object(map) => {
            let keys: Vec<String> = map.keys().cloned().collect();
            for key in keys {
                if let Some(child) = map.get_mut(&key) {
                    hoist_refs(child, defs, depth + 1, stack);
                }
            }
        }
        _ => {}
    }
}

fn sanitize_schema_in(value: &mut Value, depth: u8) {
    if depth > 12 {
        *value = json!({"type": "STRING"});
        return;
    }
    let Value::Object(obj) = value else {
        // A bare `true` or a string where a schema belongs: any string will do.
        *value = json!({"type": "STRING"});
        return;
    };
    if let Some(Value::Array(branches)) = obj.remove("allOf") {
        for branch in branches {
            if let Value::Object(map) = branch {
                for (key, child) in map {
                    obj.entry(key).or_insert(child);
                }
            }
        }
    }
    if let Some(Value::Array(branches)) = obj.remove("anyOf").or_else(|| obj.remove("oneOf")) {
        for branch in branches {
            let Value::Object(map) = branch else {
                continue;
            };
            let is_null = map
                .get("type")
                .and_then(Value::as_str)
                .is_some_and(|kind| kind.eq_ignore_ascii_case("null"));
            if is_null {
                continue;
            }
            for (key, child) in map {
                obj.entry(key).or_insert(child);
            }
            break;
        }
    }
    if let Some(Value::Array(types)) = obj.get("type").cloned() {
        let mut nullable = false;
        let mut chosen = None;
        for item in types {
            let Some(name) = item.as_str() else {
                continue;
            };
            if name.eq_ignore_ascii_case("null") {
                nullable = true;
            } else if chosen.is_none() {
                chosen = Some(name.to_owned());
            }
        }
        if nullable {
            obj.insert("nullable".into(), Value::Bool(true));
        }
        match chosen {
            Some(chosen) => {
                obj.insert("type".into(), Value::String(chosen));
            }
            None => {
                obj.remove("type");
            }
        }
    }
    if let Some(constant) = obj.remove("const")
        && !obj.contains_key("enum")
    {
        obj.insert("enum".into(), Value::Array(vec![json_string(&constant)]));
    }
    if let Some(Value::Array(items)) = obj.get_mut("enum") {
        for item in items.iter_mut() {
            if !item.is_string() {
                *item = json_string(item);
            }
        }
    }
    append_hints(obj);
    const STRIPPED: &[&str] = &[
        "$schema",
        "$defs",
        "definitions",
        "$id",
        "$ref",
        "additionalProperties",
        "patternProperties",
        "unevaluatedProperties",
        "minLength",
        "maxLength",
        "minimum",
        "maximum",
        "exclusiveMinimum",
        "exclusiveMaximum",
        "pattern",
        "format",
        "default",
        "examples",
        "title",
        "const",
        "anyOf",
        "oneOf",
        "allOf",
        "not",
        "if",
        "then",
        "else",
        "dependentRequired",
        "dependentSchemas",
        "propertyNames",
        "contentMediaType",
        "contentEncoding",
    ];
    for key in STRIPPED {
        obj.remove(*key);
    }
    if let Some(name) = obj.get("type").and_then(Value::as_str).map(str::to_owned) {
        let upper = match name.to_ascii_lowercase().as_str() {
            "object" => "OBJECT",
            "string" => "STRING",
            "integer" => "INTEGER",
            "number" => "NUMBER",
            "boolean" => "BOOLEAN",
            "array" => "ARRAY",
            _ => "STRING",
        };
        obj.insert("type".into(), Value::String(upper.to_owned()));
    }
    let property_names = obj
        .get("properties")
        .and_then(Value::as_object)
        .map(|map| map.keys().cloned().collect::<Vec<_>>())
        .unwrap_or_default();
    if let Some(Value::Object(properties)) = obj.get_mut("properties") {
        for name in property_names {
            if let Some(child) = properties.get_mut(&name) {
                sanitize_schema_in(child, depth + 1);
            }
        }
    }
    // Tuple-style `items` keeps only its first schema.
    if let Some(Value::Array(items)) = obj.get("items").cloned() {
        match items.into_iter().next() {
            Some(first) => {
                obj.insert("items".into(), first);
            }
            None => {
                obj.remove("items");
            }
        }
    }
    if let Some(items) = obj.get_mut("items") {
        sanitize_schema_in(items, depth + 1);
    }
    // A missing type is inferred from the shape.
    if obj.get("type").and_then(Value::as_str).is_none() {
        let inferred = if obj.contains_key("properties") {
            "OBJECT"
        } else if obj.contains_key("items") {
            "ARRAY"
        } else {
            "STRING"
        };
        obj.insert("type".into(), Value::String(inferred.to_owned()));
    }
    if obj.get("type").and_then(Value::as_str) == Some("ARRAY") && !obj.contains_key("items") {
        obj.insert("items".into(), json!({"type": "STRING"}));
    }
    let is_object = obj.get("type").and_then(Value::as_str) == Some("OBJECT");
    if is_object {
        let empty = match obj.get("properties") {
            None => true,
            Some(Value::Object(map)) => map.is_empty(),
            Some(_) => true,
        };
        if empty {
            obj.insert(
                "properties".into(),
                json!({"_placeholder": {"type": "BOOLEAN", "description": "unused"}}),
            );
        }
    }
    // `required` names only properties that exist; Cloud Code answers 400 for
    // anything else.
    let existing = obj
        .get("properties")
        .and_then(Value::as_object)
        .map(|map| map.keys().cloned().collect::<BTreeSet<_>>())
        .unwrap_or_default();
    if let Some(Value::Array(names)) = obj.get("required").cloned() {
        let kept = names
            .into_iter()
            .filter(|name| name.as_str().is_some_and(|name| existing.contains(name)))
            .collect::<Vec<_>>();
        if kept.is_empty() {
            obj.remove("required");
        } else {
            obj.insert("required".into(), Value::Array(kept));
        }
    } else {
        obj.remove("required");
    }
    // An allow-list: any key Cloud Code does not know is a 400 INVALID_ARGUMENT.
    obj.retain(|key, _| SCHEMA_KEYS.contains(&key.as_str()));
}

/// The schema keys Cloud Code accepts that this sanitizer keeps.
const SCHEMA_KEYS: &[&str] = &[
    "type",
    "description",
    "nullable",
    "items",
    "enum",
    "properties",
    "required",
];

fn append_hints(obj: &mut Map<String, Value>) {
    const HINTS: &[&str] = &[
        "minLength",
        "maxLength",
        "minimum",
        "maximum",
        "exclusiveMinimum",
        "exclusiveMaximum",
        "pattern",
        "format",
    ];
    let mut hints = Vec::new();
    for key in HINTS {
        if let Some(text) = obj.get(*key).and_then(hint_text) {
            hints.push(format!("{key}={text}"));
        }
    }
    if hints.is_empty() {
        return;
    }
    let extra = hints.join(", ");
    let description = obj.get("description").and_then(Value::as_str).unwrap_or("");
    let mut combined = if description.is_empty() {
        extra
    } else {
        format!("{description} ({extra})")
    };
    combined.truncate(512);
    obj.insert("description".into(), Value::String(combined));
}

fn hint_text(value: &Value) -> Option<String> {
    match value {
        Value::String(text) if text.len() <= 80 && !text.chars().any(char::is_control) => {
            Some(text.clone())
        }
        Value::Bool(flag) => Some(flag.to_string()),
        Value::Number(number) => Some(number.to_string()),
        _ => None,
    }
}

fn json_string(value: &Value) -> Value {
    Value::String(match value {
        Value::String(text) => text.clone(),
        Value::Bool(flag) => flag.to_string(),
        Value::Number(number) => number.to_string(),
        Value::Null => "null".into(),
        other => other.to_string(),
    })
}

fn placeholder_object() -> Value {
    json!({
        "type": "OBJECT",
        "properties": {"_placeholder": {"type": "BOOLEAN", "description": "unused"}}
    })
}

/// Tool names Cloud Code reserves for its own built-ins; ours travel under an
/// alias and are mapped back when the model calls them.
const TOOL_ALIASES: &[(&str, &str)] = &[
    ("web_search", "agy_web_search"),
    ("search_web", "agy_search_web"),
];

/// The name a tool travels under: sanitized, and aliased when it would clash
/// with one of Cloud Code's own.
fn wire_tool_name(name: &str) -> String {
    let clean = sanitize_tool_name(name);
    TOOL_ALIASES
        .iter()
        .find(|(ours, _)| *ours == clean)
        .map(|(_, wire)| (*wire).to_owned())
        .unwrap_or(clean)
}

/// The other half of [`wire_tool_name`] for the aliased names.
fn from_wire_tool_name(name: &str) -> &str {
    TOOL_ALIASES
        .iter()
        .find(|(_, wire)| *wire == name)
        .map(|(ours, _)| *ours)
        .unwrap_or(name)
}

/// Give a completion's tool calls back the names the runtime knows: the
/// aliases are undone, and a name the sanitizer rewrote (`plugin.x.y` became
/// `plugin_x_y`) is matched to the tool it came from.
pub fn restore_tool_names(completion: &mut CloudCodeCompletion, tools: &[ToolSpec]) {
    for call in &mut completion.tool_calls {
        let wire = call.name.clone();
        if let Some(tool) = tools.iter().find(|tool| wire_tool_name(&tool.name) == wire) {
            call.name = tool.name.clone();
        } else {
            call.name = from_wire_tool_name(&wire).to_owned();
        }
    }
}

/// The function declarations for `tools`. Cloud Code's own search tools are
/// skipped, names are made unique ignoring case, and a schema that sanitizes
/// to nothing is left out.
fn tool_declarations(tools: &[ToolSpec]) -> Vec<Value> {
    let mut seen = BTreeSet::new();
    let mut declarations = Vec::new();
    for tool in tools {
        if matches!(tool.name.as_str(), "google_search" | "googleSearch") {
            continue;
        }
        let name = wire_tool_name(&tool.name);
        if !seen.insert(name.to_ascii_lowercase()) {
            continue;
        }
        let description = if tool.description.trim().is_empty() {
            name.clone()
        } else {
            clean_text(&tool.description, 1_024)
        };
        declarations.push(json!({
            "name": name,
            "description": description,
            "parameters": sanitize_schema(&tool.parameters),
        }));
    }
    declarations
}

/// A thought signature is opaque base64-ish text; refuse anything that is not
/// a bounded printable token.
fn signature_ok(value: &str) -> bool {
    // Another provider's replay data (Anthropic thinking blocks) is not a
    // Google signature and is never sent to Google.
    (1..=16_384).contains(&value.len())
        && value.chars().all(|c| c.is_ascii_graphic())
        && !value.starts_with(crate::wire::anthropic::SIGNATURE_PREFIX)
}

fn sanitize_tool_name(name: &str) -> String {
    let mut out = String::new();
    for c in name.chars() {
        if c.is_ascii_alphanumeric() || c == '_' {
            out.push(c);
        } else if !out.is_empty() {
            out.push('_');
        }
        if out.len() == 64 {
            break;
        }
    }
    if out.is_empty() || !out.chars().next().is_some_and(|c| c.is_ascii_alphabetic()) {
        out.insert(0, 't');
    }
    out.truncate(64);
    out
}

fn clean_text(value: &str, max: usize) -> String {
    value
        .chars()
        .filter(|c| !c.is_control())
        .take(max)
        .collect()
}

pub(crate) fn pinned_cloud_code_host(host: &str) -> bool {
    CLOUD_CODE_HOSTS
        .iter()
        .any(|item| host.eq_ignore_ascii_case(item))
}

fn origin_string(url: &url::Url) -> String {
    match url.port() {
        Some(port) => format!("{}://{}:{port}", url.scheme(), url.host_str().unwrap_or("")),
        None => format!("{}://{}", url.scheme(), url.host_str().unwrap_or("")),
    }
}

fn is_exact_loopback(url: &url::Url) -> bool {
    match url.host() {
        Some(url::Host::Ipv4(ip)) => ip == std::net::Ipv4Addr::LOCALHOST,
        Some(url::Host::Ipv6(ip)) => ip == std::net::Ipv6Addr::LOCALHOST,
        _ => false,
    }
}

fn project_id_ok(value: &str) -> bool {
    (1..=256).contains(&value.len())
        && value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-' | ':'))
}

fn cloud_code_model_id(model: &str) -> bool {
    !model.is_empty()
        && model.len() <= 256
        && model.trim() == model
        && model
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-' | ':' | '+'))
}

fn tool_name_ok(name: &str) -> bool {
    (1..=64).contains(&name.len())
        && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
        && name.chars().next().is_some_and(|c| c.is_ascii_alphabetic())
}

fn call_id_ok(value: &str) -> bool {
    (1..=128).contains(&value.len())
        && value
            .chars()
            .all(|c| c.is_ascii_graphic() && c != '"' && c != '\\')
}

fn credential_ok(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 8_192
        && value.trim() == value
        && !value.chars().any(char::is_control)
}

/// `agent/{unix ms}/{8 hex}`.
fn fresh_request_id() -> String {
    let mut bytes = [0u8; 4];
    rand::rngs::OsRng.fill_bytes(&mut bytes);
    format!(
        "agent/{}/{}",
        chrono::Utc::now().timestamp_millis(),
        hex::encode(bytes)
    )
}

fn validate_loopback_redirect(redirect: &str) -> Result<()> {
    let url = url::Url::parse(redirect)
        .map_err(|_| SeraError::External("oauth redirect was rejected".into()))?;
    if url.scheme() != "http"
        || !matches!(url.host_str(), Some("127.0.0.1" | "localhost"))
        || url.port().is_none()
        || url.path() != CALLBACK_PATH
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(SeraError::External("oauth redirect was rejected".into()));
    }
    Ok(())
}

fn pin_token_url(url: &str) -> Result<String> {
    let endpoint = validate_base_url(url)?;
    let parsed = url::Url::parse(&endpoint)
        .map_err(|_| SeraError::Config("invalid oauth token url".into()))?;
    let loopback = is_exact_loopback(&parsed) && parsed.path() == "/token";
    let google = parsed
        .host_str()
        .is_some_and(|host| host.eq_ignore_ascii_case("oauth2.googleapis.com"))
        && parsed.path() == "/token";
    if loopback || google {
        Ok(endpoint)
    } else {
        Err(SeraError::Config(
            "oauth token url is not the documented endpoint for this provider".into(),
        ))
    }
}

/// The loopback listeners for the `localhost` redirect: IPv4 always, and
/// IPv6 on the same port when this computer has it, because a browser may
/// resolve `localhost` to either address.
struct LoopbackListeners {
    v4: tokio::net::TcpListener,
    v6: Option<tokio::net::TcpListener>,
    port: u16,
}

async fn bind_loopback() -> Result<LoopbackListeners> {
    let listener = match tokio::net::TcpListener::bind(("127.0.0.1", PREFERRED_PORT)).await {
        Ok(listener) => listener,
        Err(_) => tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .map_err(|_| {
                SeraError::External("oauth callback could not listen on loopback".into())
            })?,
    };
    let port = listener
        .local_addr()
        .map_err(|_| SeraError::External("oauth callback could not listen on loopback".into()))?
        .port();
    let v6 = tokio::net::TcpListener::bind(("::1", port)).await.ok();
    Ok(LoopbackListeners {
        v4: listener,
        v6,
        port,
    })
}

async fn accept_callback(listeners: LoopbackListeners, state: &str) -> Result<String> {
    let accepted = match &listeners.v6 {
        Some(v6) => tokio::select! {
            accepted = listeners.v4.accept() => accepted,
            accepted = v6.accept() => accepted,
        },
        None => listeners.v4.accept().await,
    };
    let (mut stream, _) =
        accepted.map_err(|_| SeraError::External("oauth callback was rejected".into()))?;
    let mut buf = vec![0u8; 8_192];
    let mut filled = 0usize;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    loop {
        if filled >= buf.len() {
            return Err(SeraError::External("oauth callback was rejected".into()));
        }
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            return Err(SeraError::External("oauth callback was rejected".into()));
        }
        let read = tokio::time::timeout(remaining, stream.read(&mut buf[filled..]))
            .await
            .map_err(|_| SeraError::External("oauth callback was rejected".into()))?
            .map_err(|_| SeraError::External("oauth callback was rejected".into()))?;
        if read == 0 {
            break;
        }
        filled += read;
        if buf[..filled].windows(4).any(|window| window == b"\r\n\r\n") {
            break;
        }
    }
    let request = String::from_utf8_lossy(&buf[..filled]);
    let code = match callback_code(&request, state) {
        Ok(code) => code,
        Err(error) => {
            let _ = write_callback_response(&mut stream, false).await;
            return Err(error);
        }
    };
    let _ = write_callback_response(&mut stream, true).await;
    Ok(code)
}

fn callback_code(request: &str, expected_state: &str) -> Result<String> {
    let target = request
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .ok_or_else(|| SeraError::External("oauth callback was rejected".into()))?;
    if target.len() > 4_096 || !target.starts_with(CALLBACK_PATH) {
        return Err(SeraError::External("oauth callback was rejected".into()));
    }
    let parsed = url::Url::parse(&format!("http://127.0.0.1{target}"))
        .map_err(|_| SeraError::External("oauth callback was rejected".into()))?;
    if parsed.path() != CALLBACK_PATH {
        return Err(SeraError::External("oauth callback was rejected".into()));
    }
    let mut code = None;
    let mut state = None;
    let mut error = None;
    for (key, value) in parsed.query_pairs() {
        match key.as_ref() {
            "code" => code = Some(value.into_owned()),
            "state" => state = Some(value.into_owned()),
            "error" => error = Some(value.into_owned()),
            _ => {}
        }
    }
    if state.as_deref() != Some(expected_state) {
        return Err(SeraError::External("Google sign-in was rejected".into()));
    }
    if error.as_deref() == Some("access_denied") {
        return Err(SeraError::External("Google sign-in was denied".into()));
    }
    if error.is_some() {
        return Err(SeraError::External("Google sign-in was rejected".into()));
    }
    let code = code.ok_or_else(|| SeraError::External("oauth callback was rejected".into()))?;
    if !credential_ok(&code) {
        return Err(SeraError::External("oauth callback was rejected".into()));
    }
    Ok(code)
}

async fn write_callback_response(stream: &mut tokio::net::TcpStream, ok: bool) -> Result<()> {
    let body = if ok {
        "SeraBot received the Google redirect. You can close this window.\n"
    } else {
        "SeraBot rejected the Google redirect. You can close this window.\n"
    };
    let status = if ok { "200 OK" } else { "400 Bad Request" };
    let header = format!(
        "HTTP/1.1 {status}\r\nContent-Type: text/plain; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    stream
        .write_all(header.as_bytes())
        .await
        .map_err(|_| SeraError::External("oauth callback was rejected".into()))?;
    stream
        .write_all(body.as_bytes())
        .await
        .map_err(|_| SeraError::External("oauth callback was rejected".into()))?;
    let _ = stream.shutdown().await;
    Ok(())
}

async fn exchange_code(
    http: &reqwest::Client,
    token_url: &str,
    client_secret: &str,
    code: &str,
    redirect_uri: &str,
    verifier: &str,
) -> Result<(SecretString, SecretString)> {
    validate_loopback_redirect(redirect_uri)?;
    let env_id = std::env::var(ANTIGRAVITY_CLIENT_ID_ENV).ok();
    let client_id = pick_antigravity_client_id(None, env_id.as_deref());
    let response = http
        .post(token_url)
        .header("user-agent", "SeraBot/0.1")
        .header("accept", "application/json")
        .form(&[
            ("client_id", client_id.as_str()),
            ("client_secret", client_secret),
            ("code", code),
            ("grant_type", "authorization_code"),
            ("redirect_uri", redirect_uri),
            ("code_verifier", verifier),
        ])
        .send()
        .await
        .map_err(|_| SeraError::External("oauth token exchange failed".into()))?;
    let status = response.status();
    let bytes = response
        .bytes()
        .await
        .map_err(|_| SeraError::External("oauth token exchange failed".into()))?;
    if bytes.len() > 65_536 || !status.is_success() {
        return Err(SeraError::External(format!(
            "oauth token exchange failed with status {}",
            status.as_u16()
        )));
    }
    let value: Value = serde_json::from_slice(&bytes)
        .map_err(|_| SeraError::External("oauth token exchange failed".into()))?;
    let access = required_token(&value, "access_token")?;
    let refresh = required_token(&value, "refresh_token")?;
    Ok((SecretString::new(access), SecretString::new(refresh)))
}

fn required_token(value: &Value, field: &str) -> Result<String> {
    value
        .get(field)
        .and_then(Value::as_str)
        .filter(|token| credential_ok(token))
        .map(str::to_owned)
        .ok_or_else(|| SeraError::External("oauth token exchange failed".into()))
}

async fn discover_project(
    http: &reqwest::Client,
    access_token: &str,
    bases: &[String],
    pause: Duration,
) -> Option<String> {
    discover_project_with(http, access_token, bases, pause, None)
        .await
        .project
}

/// What project discovery learned about an account.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Discovery {
    /// The Cloud Code companion project; `None` means the envelope must omit
    /// `project` (an empty one is answered with 404).
    pub project: Option<String>,
    /// The tier Google reported, for display.
    pub tier: Option<String>,
}

/// The origins project discovery asks: daily, then prod. A loopback origin (a
/// test server) is asked alone.
pub fn discovery_origins(base: &str) -> Result<Vec<String>> {
    let endpoint = validate_base_url(base)?;
    let parsed = url::Url::parse(&endpoint)
        .map_err(|_| SeraError::Config("invalid provider base url".into()))?;
    if is_exact_loopback(&parsed) {
        return Ok(vec![origin_string(&parsed)]);
    }
    Ok(DISCOVERY_HOSTS
        .iter()
        .map(|host| format!("https://{host}"))
        .collect())
}

/// Find (or create) the Cloud Code companion project of the signed-in Google
/// account: `loadCodeAssist`, then up to three `onboardUser` calls `pause`
/// apart, each followed by another `loadCodeAssist`. A failing origin moves on
/// to the next. Never fails: an account with no project simply has none.
pub async fn discover_project_with(
    http: &reqwest::Client,
    access_token: &str,
    bases: &[String],
    pause: Duration,
    version: Option<&str>,
) -> Discovery {
    let pause = pause.min(Duration::from_secs(5));
    let agent = cloud_code_user_agent(version);
    let metadata = json!({
        "ideType": "ANTIGRAVITY",
        "ideVersion": version
            .map(str::trim)
            .filter(|v| client_version_ok(v))
            .unwrap_or(IDE_VERSION),
        "platform": "WINDOWS_AMD64"
    });
    let mut found = Discovery::default();
    for base in bases {
        let Ok(origin) = one_origin(base) else {
            continue;
        };
        let Ok((status, body)) = post_cloud_json(
            http,
            &format!("{origin}/v1internal:loadCodeAssist"),
            access_token,
            &json!({"metadata": metadata}),
            &agent,
        )
        .await
        else {
            continue;
        };
        if !status.is_success() {
            continue;
        }
        found.tier = found.tier.or_else(|| shown_tier(&body));
        if let Some(project) = extract_project(&body) {
            found.project = Some(project);
            return found;
        }
        let tier = default_tier(&body).unwrap_or_else(|| "free-tier".into());
        for _ in 0..3 {
            let Ok((status, onboard)) = post_cloud_json(
                http,
                &format!("{origin}/v1internal:onboardUser"),
                access_token,
                &json!({"tierId": tier, "metadata": metadata}),
                &agent,
            )
            .await
            else {
                break;
            };
            if status.is_success()
                && let Some(project) = extract_project(&onboard)
                    .or_else(|| onboard.get("response").and_then(extract_project))
            {
                found.project = Some(project);
                return found;
            }
            sleep_pause(pause).await;
            let Ok((status, again)) = post_cloud_json(
                http,
                &format!("{origin}/v1internal:loadCodeAssist"),
                access_token,
                &json!({"metadata": metadata}),
                &agent,
            )
            .await
            else {
                break;
            };
            if status.is_success()
                && let Some(project) = extract_project(&again)
            {
                found.project = Some(project);
                return found;
            }
        }
    }
    found
}

async fn sleep_pause(pause: Duration) {
    tokio::time::sleep(pause).await;
}

fn one_origin(base: &str) -> Result<String> {
    let endpoint = validate_base_url(base)?;
    let parsed = url::Url::parse(&endpoint)
        .map_err(|_| SeraError::Config("invalid provider base url".into()))?;
    if parsed.path() != "/" && !parsed.path().is_empty() {
        return Err(SeraError::Config(
            "google-antigravity base_url must be the Cloud Code origin".into(),
        ));
    }
    if is_exact_loopback(&parsed) || pinned_cloud_code_host(parsed.host_str().unwrap_or("")) {
        Ok(origin_string(&parsed))
    } else {
        Err(SeraError::Config(
            "google-antigravity only contacts documented Cloud Code hosts".into(),
        ))
    }
}

async fn post_cloud_json(
    http: &reqwest::Client,
    url: &str,
    access_token: &str,
    body: &Value,
    user_agent: &str,
) -> Result<(reqwest::StatusCode, Value)> {
    let response = http
        .post(url)
        .header("user-agent", user_agent)
        .header("x-goog-api-client", CLOUD_CODE_API_CLIENT)
        .header("accept", "application/json")
        .bearer_auth(access_token)
        .json(body)
        .send()
        .await
        .map_err(|_| SeraError::External("Cloud Code request failed".into()))?;
    let status = response.status();
    let bytes = response
        .bytes()
        .await
        .map_err(|_| SeraError::External("Cloud Code request failed".into()))?;
    if bytes.len() > 1_048_576 {
        return Err(SeraError::External(
            "Cloud Code response is too large".into(),
        ));
    }
    let value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    Ok((status, value))
}

fn extract_project(value: &Value) -> Option<String> {
    let node = value.get("cloudaicompanionProject")?;
    let id = node
        .as_str()
        .or_else(|| node.get("id").and_then(Value::as_str))?;
    project_id_ok(id).then(|| id.to_owned())
}

fn tier_id_ok(id: &str) -> bool {
    (1..=64).contains(&id.len())
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// The tier new accounts are onboarded on: the default of `allowedTiers`, else
/// the first with a usable id.
fn default_tier(value: &Value) -> Option<String> {
    let tiers = value.get("allowedTiers")?.as_array()?;
    let mut first = None;
    for tier in tiers {
        let Some(id) = tier.get("id").and_then(Value::as_str) else {
            continue;
        };
        if !tier_id_ok(id) {
            continue;
        }
        if first.is_none() {
            first = Some(id.to_owned());
        }
        if tier.get("isDefault").and_then(Value::as_bool) == Some(true) {
            return Some(id.to_owned());
        }
    }
    first
}

/// The tier to show: the account's current one when Google says, else the
/// default it would be onboarded on.
fn shown_tier(value: &Value) -> Option<String> {
    value
        .pointer("/currentTier/id")
        .and_then(Value::as_str)
        .filter(|id| tier_id_ok(id))
        .map(str::to_owned)
        .or_else(|| default_tier(value))
}

/// Most quota windows one snapshot keeps.
const MAX_QUOTA_WINDOWS: usize = 16;

/// A window label a person recognizes: `week`, `5h`, or the provider's own
/// short word.
fn quota_label(bucket: &Value) -> String {
    let raw = ["window", "bucketId"]
        .iter()
        .find_map(|key| bucket.get(*key).and_then(Value::as_str))
        .unwrap_or("");
    let lower = raw.to_ascii_lowercase();
    if lower.contains("week") {
        "week".into()
    } else if lower.contains("5h") || lower.contains("hour") {
        "5h".into()
    } else if raw.is_empty() {
        "window".into()
    } else {
        clean_text(raw, 24)
    }
}

fn quota_window(
    scope: &str,
    label: &str,
    remaining: &Value,
    reset: Option<&Value>,
) -> Option<Value> {
    let remaining = remaining
        .as_f64()
        .filter(|r| r.is_finite())?
        .clamp(0.0, 1.0);
    let used = ((1.0 - remaining) * 1000.0).round() / 10.0;
    let resets_at = reset
        .and_then(Value::as_str)
        .and_then(|text| chrono::DateTime::parse_from_rfc3339(text).ok())
        .map(|time| {
            time.with_timezone(&chrono::Utc)
                .to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
        });
    Some(json!({
        "scope": clean_text(scope, 48),
        "label": label,
        "used_percent": used.clamp(0.0, 100.0),
        "remaining": remaining,
        "resets_at": resets_at,
    }))
}

/// Windows from `retrieveUserQuotaSummary`: groups of buckets, each with a
/// remaining fraction and a reset time.
pub fn parse_quota_summary(body: &Value) -> Vec<Value> {
    let mut windows = Vec::new();
    let Some(groups) = body.get("groups").and_then(Value::as_array) else {
        return windows;
    };
    for group in groups {
        let scope = ["displayName", "groupId"]
            .iter()
            .find_map(|key| group.get(*key).and_then(Value::as_str))
            .unwrap_or("quota");
        for bucket in group
            .get("buckets")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            let Some(remaining) = bucket.get("remainingFraction") else {
                continue;
            };
            if let Some(window) = quota_window(
                scope,
                &quota_label(bucket),
                remaining,
                bucket.get("resetTime"),
            ) {
                windows.push(window);
            }
            if windows.len() >= MAX_QUOTA_WINDOWS {
                return windows;
            }
        }
    }
    windows
}

/// Windows from the `quotaInfo` each model of `fetchAvailableModels` may carry
/// (the fallback when the summary call is not available).
pub fn parse_quota_models(body: &Value) -> Vec<Value> {
    let mut windows = Vec::new();
    let Some(models) = body.get("models").and_then(Value::as_object) else {
        return windows;
    };
    for (id, entry) in models {
        if !cloud_code_model_id(id) {
            continue;
        }
        let Some(info) = entry.get("quotaInfo") else {
            continue;
        };
        let Some(remaining) = info.get("remainingFraction") else {
            continue;
        };
        if let Some(window) = quota_window(id, "5h", remaining, info.get("resetTime")) {
            windows.push(window);
        }
        if windows.len() >= MAX_QUOTA_WINDOWS {
            break;
        }
    }
    windows
}

/// Read an account's remaining quota: the summary call, else the model list's
/// `quotaInfo`. Any failure is `None` ("no reading"), never an error.
pub async fn read_quota(
    http: &reqwest::Client,
    access_token: &str,
    project: &str,
    origins: &[String],
    version: Option<&str>,
) -> Option<Value> {
    let agent = cloud_code_user_agent(version);
    let body = if project_id_ok(project) {
        json!({"project": project})
    } else {
        json!({})
    };
    for base in origins {
        let Ok(origin) = one_origin(base) else {
            continue;
        };
        for (path, source) in [
            ("retrieveUserQuotaSummary", "summary"),
            ("fetchAvailableModels", "models"),
        ] {
            let Ok((status, reply)) = post_cloud_json(
                http,
                &format!("{origin}/v1internal:{path}"),
                access_token,
                &body,
                &agent,
            )
            .await
            else {
                continue;
            };
            if !status.is_success() {
                continue;
            }
            let windows = if source == "summary" {
                parse_quota_summary(&reply)
            } else {
                parse_quota_models(&reply)
            };
            if !windows.is_empty() {
                return Some(json!({
                    "sampled_at": chrono::Utc::now()
                        .to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
                    "source": source,
                    "windows": windows,
                }));
            }
        }
    }
    None
}

#[cfg(test)]
mod parity_tests;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::ToolCall;
    use base64::Engine;
    use secrecy::ExposeSecret;
    use serde_json::json;
    use sha2::{Digest, Sha256};
    use std::sync::{Arc, Mutex};
    use wiremock::matchers::{body_string_contains, header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[test]
    fn pkce_challenge_is_s256_and_authorize_url_stays_on_google() {
        let (verifier, challenge) = pkce_s256();
        let expected = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(Sha256::digest(verifier.as_bytes()));
        assert_eq!(challenge, expected);
        let url = authorize_url(
            "http://127.0.0.1:51121/oauth-callback",
            &challenge,
            &random_token(),
        )
        .unwrap();
        let parsed = url::Url::parse(&url).unwrap();
        assert_eq!(parsed.host_str(), Some("accounts.google.com"));
        assert_eq!(parsed.path(), "/o/oauth2/v2/auth");
        let query = parsed.query().unwrap_or("");
        assert!(query.contains("code_challenge_method=S256"));
        assert!(query.contains("access_type=offline"));
        assert!(query.contains("prompt=consent"));
        assert!(!url.contains(&verifier));
        assert!(!url.contains("code_verifier"));
        assert!(
            authorize_url(
                "https://evil.example/oauth-callback",
                &challenge,
                &random_token()
            )
            .is_err()
        );
        assert!(validate_project_id("mistral", "proj").is_err());
        assert!(validate_project_id("google-antigravity", "proj-1").is_ok());
        assert!(validate_project_id("google-antigravity", "bad id").is_err());
    }

    #[test]
    fn origins_pin_documented_hosts_and_keep_loopback_alone() {
        let origins = cloud_code_origins("https://cloudcode-pa.googleapis.com").unwrap();
        assert_eq!(origins[0], "https://cloudcode-pa.googleapis.com");
        assert_eq!(origins.len(), 4);
        assert!(origins.iter().all(|origin| {
            CLOUD_CODE_HOSTS
                .iter()
                .any(|host| origin == &format!("https://{host}"))
        }));
        let loopback = cloud_code_origins("http://127.0.0.1:9").unwrap();
        assert_eq!(loopback, vec!["http://127.0.0.1:9".to_string()]);
        assert!(cloud_code_origins("https://evil.example").is_err());
        assert!(cloud_code_origins("https://daily-cloudcode-pa.googleapis.com/v1").is_err());
        assert_eq!(
            primary_catalog_url("https://daily-cloudcode-pa.googleapis.com").unwrap(),
            "https://daily-cloudcode-pa.googleapis.com/v1internal:fetchAvailableModels"
        );
    }

    #[test]
    fn catalog_parser_is_a_model_map_and_drops_non_chat_ids() {
        let secret = "synthetic-catalog-secret";
        let ids = parse_cloud_code_catalog(
            serde_json::to_vec(&json!({
                "models": {
                    "gemini-3.8-flash": {"displayName": "Flash"},
                    "imagen-dropped": {"note": secret},
                    "bad/id": {"note": secret},
                    "claude-sonnet-5-5-thinking": {}
                },
                "imageGenerationModelIds": ["imagen-dropped"],
                "deprecatedModelIds": ["claude-sonnet-5-5-thinking"]
            }))
            .unwrap()
            .as_slice(),
        )
        .unwrap();
        assert_eq!(
            ids.into_iter().collect::<Vec<_>>(),
            vec!["gemini-3.8-flash".to_string()]
        );
        let rejected = parse_cloud_code_catalog(
            serde_json::to_vec(
                &json!({"object": "list", "data": [{"id": "kept"}], "note": secret}),
            )
            .unwrap()
            .as_slice(),
        )
        .unwrap_err();
        let message = rejected.to_string();
        assert!(message.contains("Cloud Code model map"));
        assert!(!message.contains(secret));
    }

    #[test]
    fn schema_sanitizer_uppercases_types_and_strips_rejected_keywords() {
        let sanitized = sanitize_schema(&json!({
            "type": "object",
            "additionalProperties": false,
            "$defs": {"q": {"type": "string", "minLength": 2}},
            "properties": {
                "q": {"$ref": "#/$defs/q"},
                "flag": {"type": ["string", "null"], "const": "yes"}
            }
        }));
        assert!(sanitized.get("$defs").is_none());
        assert_eq!(sanitized["type"], "OBJECT");
        assert_eq!(sanitized["properties"]["q"]["type"], "STRING");
        assert!(sanitized["properties"]["q"].get("$ref").is_none());
        assert!(
            sanitized["properties"]["q"]["description"]
                .as_str()
                .unwrap_or("")
                .contains("minLength=2")
        );
        assert_eq!(sanitized["properties"]["flag"]["type"], "STRING");
        assert_eq!(sanitized["properties"]["flag"]["nullable"], true);
        assert_eq!(sanitized["properties"]["flag"]["enum"], json!(["yes"]));
        let empty = sanitize_schema(&json!({"type": "object", "properties": {}}));
        assert_eq!(empty["properties"]["_placeholder"]["type"], "BOOLEAN");
    }

    #[test]
    fn envelope_omits_empty_project_and_uses_the_signature_sentinel() {
        let envelope = build_cloud_code_envelope(
            "gemini-3.8-flash",
            "",
            &[
                LlmMessage::system("be brief"),
                LlmMessage::user("hello"),
                LlmMessage::assistant(
                    None,
                    vec![ToolCall {
                        id: "call_1".into(),
                        name: "rag_search".into(),
                        arguments: json!({"query": "q"}),
                        reasoning_signature: None,
                    }],
                ),
            ],
            &[ToolSpec {
                name: "rag_search".into(),
                description: "search".into(),
                parameters: json!({"type": "object", "properties": {}}),
                external_effect: false,
            }],
            100,
            None,
        )
        .unwrap();
        assert!(envelope.get("project").is_none());
        assert_eq!(
            envelope["request"]["contents"][1]["parts"][0]["thoughtSignature"],
            THOUGHT_SIGNATURE_SENTINEL
        );
        assert_eq!(
            envelope["request"]["tools"][0]["functionDeclarations"][0]["parameters"]["type"],
            "OBJECT"
        );
        assert_eq!(
            envelope["request"]["systemInstruction"]["parts"][0]["text"],
            "be brief"
        );
        let with_project = build_cloud_code_envelope(
            "gemini-3.8-flash",
            "proj-synthetic",
            &[LlmMessage::user("hello")],
            &[],
            100,
            None,
        )
        .unwrap();
        assert_eq!(with_project["project"], "proj-synthetic");
    }

    #[test]
    fn completion_parser_skips_thoughts_and_concatenates_sse() {
        let secret = "synthetic-thought-secret";
        let body = format!(
            "data: {{\"response\":{{\"candidates\":[{{\"content\":{{\"parts\":[{{\"thought\":true,\"text\":\"{secret}\"}},{{\"text\":\"hello \"}}]}}}}]}}}}\n\ndata: {{\"response\":{{\"candidates\":[{{\"content\":{{\"parts\":[{{\"text\":\"world\"}},{{\"functionCall\":{{\"id\":\"call_1\",\"name\":\"rag_search\",\"args\":{{\"query\":\"q\"}}}}}}]}},\"finishReason\":\"STOP\"}}],\"usageMetadata\":{{\"promptTokenCount\":3,\"candidatesTokenCount\":2}}}}}}\n\n"
        );
        let parsed = parse_cloud_code_completion(body.as_bytes()).unwrap();
        assert_eq!(parsed.text, "hello world");
        assert_eq!(parsed.tool_calls.len(), 1);
        assert_eq!(parsed.tool_calls[0].name, "rag_search");
        assert_eq!(parsed.prompt_tokens, 3);
        assert!(!parsed.text.contains(secret));
    }

    #[tokio::test]
    async fn pkce_login_exchanges_on_loopback_and_does_not_echo_secrets() {
        let token_server = MockServer::start().await;
        let api_server = MockServer::start().await;
        let client_secret = "synthetic-client-secret";
        Mock::given(method("POST"))
            .and(path("/token"))
            .and(body_string_contains("grant_type=authorization_code"))
            .and(body_string_contains("code_verifier="))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "access_token": "access-synthetic",
                "refresh_token": "refresh-synthetic",
                "expires_in": 3600
            })))
            .expect(1)
            .mount(&token_server)
            .await;
        Mock::given(method("POST"))
            .and(path("/v1internal:loadCodeAssist"))
            .and(header("authorization", "Bearer access-synthetic"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "cloudaicompanionProject": "proj-synthetic"
            })))
            .expect(1)
            .mount(&api_server)
            .await;
        let seen = Arc::new(Mutex::new(String::new()));
        let seen_url = seen.clone();
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(10))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .unwrap();
        let login = login_antigravity_at(
            &http,
            client_secret,
            &format!("{}/token", token_server.uri()),
            &[api_server.uri()],
            Duration::from_secs(10),
            Duration::from_millis(1),
            move |url| {
                *seen_url.lock().unwrap() = url.to_owned();
                let url = url.to_owned();
                tokio::spawn(async move {
                    let parsed = url::Url::parse(&url).unwrap();
                    let redirect = parsed
                        .query_pairs()
                        .find(|(key, _)| key == "redirect_uri")
                        .unwrap()
                        .1
                        .into_owned();
                    let state = parsed
                        .query_pairs()
                        .find(|(key, _)| key == "state")
                        .unwrap()
                        .1
                        .into_owned();
                    let callback = format!("{redirect}?code=synthetic-auth-code&state={state}");
                    let _ = reqwest::Client::builder()
                        .redirect(reqwest::redirect::Policy::none())
                        .build()
                        .unwrap()
                        .get(callback)
                        .send()
                        .await;
                });
            },
        )
        .await
        .unwrap();
        let url = seen.lock().unwrap().clone();
        assert!(url.starts_with("https://accounts.google.com/o/oauth2/v2/auth?"));
        assert!(!url.contains(client_secret));
        assert!(!url.contains("code_verifier"));
        assert_eq!(login.access_token.expose_secret(), "access-synthetic");
        assert_eq!(login.refresh_token.expose_secret(), "refresh-synthetic");
        assert_eq!(login.project_id.as_deref(), Some("proj-synthetic"));
        let rendered = format!("{login:?}");
        assert!(!rendered.contains("access-synthetic"));
        assert!(!rendered.contains("refresh-synthetic"));
        assert!(!rendered.contains(client_secret));
    }

    #[tokio::test]
    async fn complete_login_exchanges_a_client_supplied_code_without_echoing_it() {
        let token_server = MockServer::start().await;
        let api_server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/token"))
            .and(body_string_contains("code=synthetic-auth-code"))
            .and(body_string_contains(
                "code_verifier=synthetic-verifier-synthetic-verifier-0123456789",
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "access_token": "access-synthetic",
                "refresh_token": "refresh-synthetic"
            })))
            .expect(1)
            .mount(&token_server)
            .await;
        Mock::given(method("POST"))
            .and(path("/v1internal:loadCodeAssist"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "cloudaicompanionProject": "proj-synthetic"
            })))
            .mount(&api_server)
            .await;
        let http = reqwest::Client::new();
        let login = complete_antigravity_login_at(
            &http,
            "synthetic-client-secret",
            &format!("{}/token", token_server.uri()),
            &[api_server.uri()],
            Duration::from_millis(1),
            "synthetic-auth-code",
            "http://127.0.0.1:51121/oauth-callback",
            "synthetic-verifier-synthetic-verifier-0123456789",
        )
        .await
        .unwrap();
        assert_eq!(login.refresh_token.expose_secret(), "refresh-synthetic");
        assert_eq!(login.project_id.as_deref(), Some("proj-synthetic"));
        assert!(!format!("{login:?}").contains("access-synthetic"));
    }

    #[test]
    fn the_updater_version_is_read_only_when_it_is_a_plain_version() {
        assert_eq!(
            parse_updater_version(br#"{"version":"2.31.4","url":"x"}"#).as_deref(),
            Some("2.31.4")
        );
        assert_eq!(
            parse_updater_version(br#"{"version":" 3.0.1 "}"#).as_deref(),
            Some("3.0.1")
        );
        for bad in [
            &br#"{"version":"v2.31"}"#[..],
            br#"{"version":"2..1"}"#,
            br#"{"name":"2.31.4"}"#,
            b"not json",
        ] {
            assert_eq!(parse_updater_version(bad), None);
        }
        let mut large = br#"{"version":"2.31.4","pad":""#.to_vec();
        large.extend(std::iter::repeat_n(b'x', 20_000));
        large.extend(br#""}"#);
        assert_eq!(parse_updater_version(&large), None);
    }

    #[tokio::test]
    async fn the_presented_version_prefers_the_providers_own_then_the_pin() {
        assert_eq!(antigravity_version(Some("2.40.1")).await, "2.40.1");
        if std::env::var(ANTIGRAVITY_VERSION_ENV).is_err() {
            // Tests never ask the updater, so the pin stands in for it.
            assert_eq!(antigravity_version(None).await, IDE_VERSION);
            assert_eq!(antigravity_version(Some("latest")).await, IDE_VERSION);
        }
    }

    #[test]
    fn the_client_id_prefers_the_store_then_the_environment_then_the_published_one() {
        assert_eq!(
            pick_antigravity_client_id(Some(" stored "), Some("env")),
            "stored"
        );
        assert_eq!(pick_antigravity_client_id(None, Some(" env ")), "env");
        assert_eq!(
            pick_antigravity_client_id(Some(" "), Some("")),
            ANTIGRAVITY_CLIENT_ID
        );
        assert_eq!(
            pick_antigravity_client_id(None, None),
            ANTIGRAVITY_CLIENT_ID
        );
    }

    #[test]
    fn only_plain_opus_has_a_wire_id() {
        assert_eq!(
            wire_fallback_model("claude-opus-5-5"),
            Some("claude-opus-5-5-thinking")
        );
        assert_eq!(
            wire_fallback_model(" CLAUDE-OPUS-5-5 "),
            Some("claude-opus-5-5-thinking")
        );
        for none in [
            "claude-opus-5-5-thinking",
            "claude-sonnet-5-5",
            "gemini-4-argon-high",
        ] {
            assert_eq!(wire_fallback_model(none), None, "{none}");
        }
    }

    #[test]
    fn configured_models_join_the_live_list_without_replacing_it() {
        let live = parse_cloud_code_models(
            br#"{"models": {"gemini-4-argon-high": {"displayName": "Gemini 4 Argon", "maxTokens": 2000000}}}"#,
        )
        .unwrap();
        let configured = vec![
            "gemini-4-argon-high".to_owned(),
            "claude-opus-5-5".to_owned(),
            "claude-opus-4-6-thinking".to_owned(),
        ];
        let merged = with_configured_models(live.clone(), &configured);
        assert!(merged.contains_key("claude-opus-5-5"));
        assert!(
            !merged.contains_key("claude-opus-4-6-thinking"),
            "legacy ids stay out"
        );
        assert_eq!(merged["gemini-4-argon-high"], live["gemini-4-argon-high"]);
    }

    #[test]
    fn the_operator_redirect_takes_localhost_like_hielcowork() {
        for ok in [
            "http://localhost:51121/oauth-callback",
            "http://localhost:53811/oauth-callback",
            "http://127.0.0.1:51121/oauth-callback",
        ] {
            assert!(validate_loopback_redirect(ok).is_ok(), "{ok}");
        }
        for bad in [
            "https://localhost:51121/oauth-callback",
            "http://localhost/oauth-callback",
            "http://localhost:51121/callback",
            "http://example.com:51121/oauth-callback",
            "http://localhost:51121/oauth-callback?x=1",
        ] {
            assert!(validate_loopback_redirect(bad).is_err(), "{bad}");
        }
    }

    #[tokio::test]
    async fn complete_login_rejects_bad_redirect_and_verifier_before_any_request() {
        let http = reqwest::Client::new();
        for (redirect, verifier) in [
            (
                "https://evil.example/oauth-callback",
                "synthetic-verifier-synthetic-verifier-0123456789",
            ),
            ("http://127.0.0.1:51121/oauth-callback", "short"),
        ] {
            let error = complete_antigravity_login_at(
                &http,
                "synthetic-client-secret",
                "http://127.0.0.1:9/token",
                &[],
                Duration::from_millis(1),
                "synthetic-auth-code",
                redirect,
                verifier,
            )
            .await
            .unwrap_err()
            .to_string();
            assert!(!error.contains("synthetic-client-secret"));
        }
    }

    #[tokio::test]
    async fn token_exchange_error_omits_the_response_body() {
        let server = MockServer::start().await;
        let secret = "synthetic-client-secret";
        Mock::given(method("POST"))
            .and(path("/token"))
            .respond_with(ResponseTemplate::new(400).set_body_string(secret))
            .mount(&server)
            .await;
        let http = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .unwrap();
        let error = exchange_code(
            &http,
            &format!("{}/token", server.uri()),
            secret,
            "synthetic-auth-code",
            "http://127.0.0.1:9/oauth-callback",
            &pkce_s256().0,
        )
        .await
        .unwrap_err();
        let message = error.to_string();
        assert!(message.contains("400"));
        assert!(!message.contains(secret));
        assert!(pin_token_url("https://evil.example/token").is_err());
    }

    #[test]
    fn stream_parser_skips_non_json_lines_and_subtracts_cached_tokens() {
        let sse = b"data: keepalive\n\ndata: {\"response\":{\"candidates\":[{\"content\":{\"parts\":[{\"text\":\"hello \"}]}}],\"usageMetadata\":{\"promptTokenCount\":120,\"cachedContentTokenCount\":80,\"candidatesTokenCount\":5}}}\n{\"response\":{\"candidates\":[{\"content\":{\"parts\":[{\"text\":\"world\"}]},\"finishReason\":\"STOP\"}]}}\ndata: [DONE]\n";
        let parsed = parse_cloud_code_stream(sse).expect("sse parses");
        assert_eq!(parsed.text, "hello world");
        assert_eq!(parsed.prompt_tokens, 40);
        assert_eq!(parsed.completion_tokens, 5);
        assert_eq!(parsed.finish_reason.as_deref(), Some("STOP"));
    }

    #[test]
    fn empty_messages_insert_fallback_user_turn() {
        let envelope = build_cloud_code_envelope_with(
            "gemini-4-argon-high",
            "proj-1",
            &[LlmMessage::system("Be helpful.")],
            &[],
            1024,
            None,
            &EnvelopeOptions {
                conversation: "c1",
                session_generation: 0,
                thinking_level: None,
            },
        )
        .expect("envelope builds");
        let contents = envelope["request"]["contents"].as_array().unwrap();
        assert_eq!(contents.len(), 1);
        assert_eq!(contents[0]["role"], "user");
        assert_eq!(contents[0]["parts"][0]["text"], "Hello");
    }

    #[test]
    fn incremental_reader_handles_split_utf8_chunks_and_ignores_unspecified_finish() {
        let sse = concat!(
            ": keepalive\n",
            "data: {\"response\":{\"candidates\":[{\"content\":{\"parts\":[{\"thought\":true,\"text\":\"Plan \"}]},\"finishReason\":\"STOP\"}]}}\n",
            "data: {\"response\":{\"candidates\":[{\"content\":{\"parts\":[{\"text\":\"Caf\\u00e9 — ready\"}]},\"finishReason\":\"FINISH_REASON_UNSPECIFIED\"}],\"usageMetadata\":{\"promptTokenCount\":20,\"candidatesTokenCount\":4}}}\n",
            "data: [DONE]\n"
        );
        // Feed small odd-sized byte chunks, splitting multi-byte UTF-8 em-dash across chunks.
        let mut reader = CloudCodeStreamReader::new();
        for chunk in sse.as_bytes().chunks(7) {
            reader.push_chunk(chunk).expect("chunk accepted");
        }
        let parsed = reader.finish().expect("incremental sse succeeds");
        assert_eq!(parsed.reasoning, "Plan ");
        assert_eq!(parsed.text, "Café — ready");
        assert_eq!(parsed.finish_reason.as_deref(), Some("STOP"));
        assert_eq!(parsed.prompt_tokens, 20);
        assert_eq!(parsed.completion_tokens, 4);
    }

    #[test]
    fn incremental_reader_rejects_oversized_stream_immediately() {
        let mut reader = CloudCodeStreamReader::new();
        let chunk = vec![b' '; 1_048_576];
        for _ in 0..4 {
            reader.push_chunk(&chunk).unwrap();
        }
        let err = reader.push_chunk(b"x").unwrap_err();
        assert!(err.to_string().contains("too large"));
    }
}
