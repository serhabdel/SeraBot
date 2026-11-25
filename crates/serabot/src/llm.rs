use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures::StreamExt;
use secrecy::{ExposeSecret, SecretString};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use zeroize::Zeroize;

use crate::accounts::{AccountRefresher, CredentialStamp, RefreshError, RefreshGrant, Refreshed};
use crate::cloud_code::{
    self, CLOUD_CODE_API_CLIENT, CloudCodeCompletion, CloudCodeStreamReader, Discovery,
};
use crate::providers::{
    ModelCatalog, OAuthMaterial, OAuthStyle, USER_AGENT, enforce_oauth_target, muse_mint_api_key,
    parse_model_catalog, refresh_token_exchange, refresh_token_grant,
};

use crate::domain::{LlmMessage, Role, ToolCall};
use crate::error::{Result, SeraError};
use crate::gateway::ToolSpec;
use crate::llm_failure;
use crate::retry;
use crate::secrets::{AgeCredentialWriter, CredentialWriter, SharedSecrets};
use crate::wire::Wire;

mod wire_calls;
pub(crate) use wire_calls::opencode_session_id;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Usage {
    /// Input tokens billed at the normal rate (cache reads excluded).
    #[serde(default)]
    pub prompt_tokens: u64,
    #[serde(default)]
    pub completion_tokens: u64,
    #[serde(default)]
    pub total_tokens: u64,
    /// Input tokens read from the provider's prompt cache (0.70), billed at
    /// the cache price.
    #[serde(default)]
    pub cached_tokens: u64,
}

impl Usage {
    /// Chat-completions usage (0.70): OpenAI-style `prompt_tokens` includes
    /// cached tokens, reported in `prompt_tokens_details.cached_tokens`
    /// (DeepSeek: `prompt_cache_hit_tokens`). Split them so `prompt_tokens`
    /// is the uncached part, like the other transports.
    pub fn from_chat_json(usage: &Value) -> Self {
        let get = |pointer: &str| usage.pointer(pointer).and_then(Value::as_u64).unwrap_or(0);
        let prompt = get("/prompt_tokens");
        let completion = get("/completion_tokens");
        let cached = get("/prompt_tokens_details/cached_tokens")
            .max(get("/prompt_cache_hit_tokens"))
            .min(prompt);
        let total = get("/total_tokens").max(prompt.saturating_add(completion));
        Self {
            prompt_tokens: prompt - cached,
            completion_tokens: completion,
            total_tokens: total,
            cached_tokens: cached,
        }
    }
}

#[derive(Debug, Clone)]
pub struct LlmResponse {
    pub message: LlmMessage,
    pub usage: Usage,
    pub finish_reason: Option<String>,
    /// Reasoning text the provider returned with the answer, when it did.
    /// Untrusted display data; never fed back as instructions.
    pub reasoning: Option<String>,
}

struct RefreshPersistence {
    writer: Arc<dyn CredentialWriter>,
}

/// Where a client's token refresh goes when its credential is a member's
/// account: through the shared single-flight refresher, which re-reads the
/// stored credential and writes the result back to the account row.
#[derive(Clone)]
pub struct AccountRefreshTarget {
    pub refresher: Arc<AccountRefresher>,
    pub user_id: String,
    pub provider: String,
    pub account_id: String,
}

/// What a client knows about the stored credential it was built from.
struct AccountRefreshState {
    target: AccountRefreshTarget,
    generation: Option<String>,
    expires_at: Option<i64>,
}

struct OAuthState {
    style: OAuthStyle,
    refresh_token: Mutex<SecretString>,
    token_url: String,
    client_id: String,
    client_secret: SecretString,
    persistence: Mutex<Option<RefreshPersistence>>,
    /// Set for a member's account: refreshes go through the account refresher
    /// and `persistence` is not used.
    account: Mutex<Option<AccountRefreshState>>,
    /// Set when a provider rotated the refresh token and the age store does not
    /// yet contain that value. Stays set until `persist` succeeds.
    refresh_unpersisted: AtomicBool,
}

tokio::task_local! {
    static CONVERSATION: String;
}

/// Run `future` with `conversation_id` as the conversation its model calls
/// belong to. Cloud Code derives its session id from it, so the same
/// conversation keeps one server-side session.
pub async fn with_conversation<F: std::future::Future>(
    conversation_id: &str,
    future: F,
) -> F::Output {
    CONVERSATION.scope(conversation_id.to_owned(), future).await
}

fn current_conversation() -> String {
    CONVERSATION.try_with(Clone::clone).unwrap_or_default()
}

/// Where a member account's discovered Cloud Code project is remembered.
pub trait ProjectMemo: Send + Sync {
    /// Record what discovery found (and that it ran).
    fn record(&self, discovery: &Discovery);
}

/// How long a failed project discovery is not tried again for an account.
const DISCOVERY_RETRY_SECS: i64 = 3_600;

struct ProjectState {
    known: Mutex<Option<String>>,
    memo: Arc<dyn ProjectMemo>,
    /// One attempt per client, and none while the last one is recent.
    tried: AtomicBool,
}

#[derive(Clone)]
pub struct LlmClient {
    http: reqwest::Client,
    base_url: String,
    api_key: Arc<Mutex<SecretString>>,
    model: String,
    max_output_tokens: u32,
    max_retries: usize,
    oauth: Option<Arc<OAuthState>>,
    /// `Some` selects the Cloud Code transport. The string is the project id, or
    /// empty when the envelope must omit `project`.
    cloud_code: Option<String>,
    /// A header the provider wants on every request (Muse's `x-api-version`).
    extra_headers: Vec<(&'static str, &'static str)>,
    /// The member account this client acts for, when it has one.
    account: Option<AccountRefreshTarget>,
    /// A 401 with nothing to refresh marks the account `needs_reauth`.
    reauth_on_401: bool,
    /// Cloud Code session rotations (see `cloud_code::session_id`).
    session_generation: Arc<AtomicI64>,
    /// Lazy project discovery for a member's Cloud Code account.
    project: Option<Arc<ProjectState>>,
    /// The Antigravity Hub version for the user agent, when configured.
    cloud_version: Option<String>,
    /// The request and response format (chat completions unless set).
    wire: Wire,
    /// OpenCode Go wants `x-opencode-session` on every request. Holds the
    /// seed used when no conversation is running (one per client).
    opencode_session: Option<Arc<str>>,
    /// A Codex (ChatGPT plan) account's request identity.
    codex: Option<Arc<CodexRequest>>,
    /// A GitHub Copilot account: the editor headers are on every request, and
    /// the model list comes from the plan's own host.
    copilot: bool,
}

/// What a Codex request carries beyond the bearer: the ChatGPT workspace the
/// sign-in named and a session seed for use outside a conversation.
struct CodexRequest {
    account_id: Option<String>,
    seed: String,
}

impl std::fmt::Debug for LlmClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LlmClient")
            .field("base_url", &self.base_url)
            .field("model", &self.model)
            .finish()
    }
}

/// The longest one model call may take while the provider keeps sending.
const MAX_CALL: Duration = Duration::from_secs(15 * 60);

impl LlmClient {
    pub fn new(
        base_url: impl Into<String>,
        api_key: impl Into<String>,
        timeout_secs: u64,
        max_output_tokens: u32,
    ) -> Result<Self> {
        let base_url = validate_base_url(&base_url.into())?;
        let api_key = api_key.into();
        if api_key.trim().is_empty() {
            return Err(SeraError::Config("LLM credential is missing".into()));
        }
        // `timeout_secs` is how long the provider may go quiet, not how long
        // an answer may take: streamed thinking answers routinely run past
        // it while sending all along (HielCowork never cuts an open stream).
        // A whole call is still bounded, at the larger of a quarter hour and
        // the configured value.
        let idle = Duration::from_secs(timeout_secs.max(1));
        let http = reqwest::Client::builder()
            .read_timeout(idle)
            .connect_timeout(idle.min(Duration::from_secs(30)))
            .timeout(idle.max(MAX_CALL))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|error| SeraError::External(error.to_string()))?;
        Ok(Self {
            http,
            base_url,
            api_key: Arc::new(Mutex::new(SecretString::new(api_key))),
            model: String::new(),
            max_output_tokens,
            max_retries: 2,
            oauth: None,
            cloud_code: None,
            extra_headers: Vec::new(),
            account: None,
            reauth_on_401: false,
            session_generation: Arc::new(AtomicI64::new(0)),
            project: None,
            cloud_version: None,
            wire: Wire::ChatCompletions,
            opencode_session: None,
            codex: None,
            copilot: false,
        })
    }

    /// Send this client's traffic through `proxy` (the desktop relay, 0.66),
    /// with the same timeouts as [`Self::new`].
    pub fn with_proxy(mut self, proxy: reqwest::Proxy, timeout_secs: u64) -> Result<Self> {
        let idle = Duration::from_secs(timeout_secs.max(1));
        self.http = reqwest::Client::builder()
            .read_timeout(idle)
            .connect_timeout(idle.min(Duration::from_secs(30)))
            .timeout(idle.max(MAX_CALL))
            .redirect(reqwest::redirect::Policy::none())
            .proxy(proxy)
            .build()
            .map_err(|error| SeraError::External(error.to_string()))?;
        Ok(self)
    }

    /// Send the editor identity GitHub Copilot requires on every request
    /// (`User-Agent`, `Editor-Version`, `Editor-Plugin-Version`,
    /// `Copilot-Integration-Id`, `X-GitHub-Api-Version`), as the owner's
    /// HielCowork client does.
    pub fn with_copilot(mut self) -> Self {
        self.copilot = true;
        for (name, value) in crate::auth_flows::copilot::EDITOR_HEADERS {
            self.extra_headers.push((name, value));
        }
        self
    }

    /// Send the Codex request identity (`ChatGPT-Account-Id`, `originator`,
    /// `OpenAI-Beta`, `session_id` and the body's `prompt_cache_key`) on the
    /// Responses transport. `account_id` is the ChatGPT workspace the member's
    /// sign-in named, when it named one.
    pub fn with_codex(mut self, account_id: Option<String>) -> Self {
        use rand::RngCore;
        let mut seed = [0u8; 16];
        rand::rngs::OsRng.fill_bytes(&mut seed);
        self.codex = Some(Arc::new(CodexRequest {
            account_id,
            seed: hex::encode(seed),
        }));
        self
    }

    /// Speak another wire format (Muse chats on Responses, Anthropic on
    /// Messages).
    pub fn with_wire(mut self, wire: Wire) -> Self {
        self.wire = wire;
        self
    }

    /// Send `x-opencode-session` on every request (OpenCode Go). The id comes
    /// from the running conversation, so a retry reuses it; outside one, from
    /// a seed made for this client.
    pub fn with_opencode_session(mut self) -> Self {
        use rand::RngCore;
        let mut seed = [0u8; 16];
        rand::rngs::OsRng.fill_bytes(&mut seed);
        self.opencode_session = Some(Arc::from(hex::encode(seed)));
        self
    }

    /// The headers every request carries: the provider's fixed ones and the
    /// OpenCode session id.
    fn decorate(&self, mut request: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        for (name, value) in &self.extra_headers {
            request = request.header(*name, *value);
        }
        if let Some(seed) = &self.opencode_session {
            let conversation = current_conversation();
            let basis = if conversation.is_empty() {
                seed.as_ref()
            } else {
                conversation.as_str()
            };
            request = request.header("x-opencode-session", opencode_session_id(basis));
        }
        request
    }

    /// Send `name: value` on every request.
    pub fn with_header(mut self, name: &'static str, value: &'static str) -> Self {
        self.extra_headers.push((name, value));
        self
    }

    /// Act for a member's account: session rotations are written to its row,
    /// and (with `reauth_on_401`) a rejected credential parks it.
    pub fn with_account(
        mut self,
        target: AccountRefreshTarget,
        session_generation: i64,
        reauth_on_401: bool,
    ) -> Self {
        self.account = Some(target);
        self.session_generation = Arc::new(AtomicI64::new(session_generation));
        self.reauth_on_401 = reauth_on_401;
        self
    }

    /// Pin the Antigravity Hub version the user agent reports.
    pub fn with_cloud_version(mut self, version: &str) -> Self {
        let version = version.trim();
        if cloud_code::client_version_ok(version) {
            self.cloud_version = Some(version.to_owned());
        }
        self
    }

    /// Find this Cloud Code account's project on first use when it has none.
    /// `last_attempt` is when discovery last ran for the account (Unix
    /// seconds); a recent failure is not repeated.
    pub fn with_project_discovery(
        mut self,
        known: Option<String>,
        last_attempt: Option<i64>,
        memo: Arc<dyn ProjectMemo>,
    ) -> Self {
        let recent = last_attempt
            .is_some_and(|at| chrono::Utc::now().timestamp() - at < DISCOVERY_RETRY_SECS);
        self.project = Some(Arc::new(ProjectState {
            tried: AtomicBool::new(recent || known.is_some()),
            known: Mutex::new(known),
            memo,
        }));
        self
    }

    pub fn with_cloud_code(mut self, project_id: impl Into<String>) -> Result<Self> {
        let project_id = project_id.into();
        cloud_code::validate_project_id("google-antigravity", &project_id)?;
        self.cloud_code = Some(project_id);
        Ok(self)
    }

    pub fn with_oauth(self, material: OAuthMaterial) -> Result<Self> {
        let token_url = enforce_oauth_target(material.style, &material.token_url)?;
        self.install_oauth(material, token_url)
    }

    #[cfg(test)]
    pub fn with_oauth_unpinned(self, material: OAuthMaterial) -> Result<Self> {
        let token_url = validate_base_url(&material.token_url)?;
        self.install_oauth(material, token_url)
    }

    fn install_oauth(mut self, material: OAuthMaterial, token_url: String) -> Result<Self> {
        if material.refresh_token.trim().is_empty() {
            return Err(SeraError::Config(
                "oauth refresh credential is missing".into(),
            ));
        }
        self.oauth = Some(Arc::new(OAuthState {
            style: material.style,
            refresh_token: Mutex::new(SecretString::new(material.refresh_token)),
            token_url,
            client_id: material.client_id,
            client_secret: SecretString::new(material.client_secret),
            persistence: Mutex::new(None),
            account: Mutex::new(None),
            refresh_unpersisted: AtomicBool::new(false),
        }));
        Ok(self)
    }

    /// Record where a rotated Google refresh token (and its access token) must
    /// be written. Antigravity login stores both in the age store.
    pub fn with_refresh_persistence(
        self,
        secrets: SharedSecrets,
        refresh_secret_name: impl Into<String>,
        access_secret_name: impl Into<String>,
    ) -> Result<Self> {
        let writer = AgeCredentialWriter::new(secrets, refresh_secret_name, access_secret_name)?;
        self.with_credential_writer(Arc::new(writer))
    }

    /// Record a custom write-back target for a rotated credential. The
    /// per-user provider factory passes a writer that merges into the acting
    /// user's (or grantor's) credential row.
    pub fn with_credential_writer(self, writer: Arc<dyn CredentialWriter>) -> Result<Self> {
        let oauth = self
            .oauth
            .as_ref()
            .ok_or_else(|| SeraError::Config("oauth refresh persistence requires oauth".into()))?;
        *oauth
            .persistence
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = Some(RefreshPersistence { writer });
        Ok(self)
    }

    /// Route this client's refreshes through the account refresher.
    /// `generation` and `expires_at` describe the credential the client was
    /// built from, so a refresh can tell whether someone else already renewed it.
    pub fn with_account_refresh(
        self,
        target: AccountRefreshTarget,
        generation: Option<String>,
        expires_at: Option<i64>,
    ) -> Result<Self> {
        let oauth = self
            .oauth
            .as_ref()
            .ok_or_else(|| SeraError::Config("account refresh requires oauth".into()))?;
        *oauth
            .account
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = Some(AccountRefreshState {
            target,
            generation,
            expires_at,
        });
        Ok(self)
    }

    fn bearer(&self) -> String {
        self.api_key
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .expose_secret()
            .to_owned()
    }

    pub fn with_model(mut self, model: impl Into<String>) -> Self {
        self.model = model.into();
        self
    }

    pub(crate) fn base_url(&self) -> &str {
        &self.base_url
    }

    pub async fn complete(
        &self,
        messages: &[LlmMessage],
        tools: &[ToolSpec],
        model_override: Option<&str>,
        temperature: Option<f32>,
    ) -> Result<LlmResponse> {
        self.complete_with(messages, tools, model_override, temperature, None)
            .await
    }

    /// Like [`Self::complete`], merging `extra` (thinking-level fields built
    /// by `thinking::wire_fields`) into the request body.
    pub async fn complete_with(
        &self,
        messages: &[LlmMessage],
        tools: &[ToolSpec],
        model_override: Option<&str>,
        temperature: Option<f32>,
        extra: Option<&serde_json::Map<String, Value>>,
    ) -> Result<LlmResponse> {
        let model = model_override.unwrap_or(&self.model).to_owned();
        if model.is_empty() {
            return Err(SeraError::Config("LLM model is not configured".into()));
        }
        self.ensure_fresh_token().await?;
        if self.cloud_code.is_some() {
            let level = extra
                .and_then(|fields| fields.get("thinking_level"))
                .and_then(Value::as_str);
            return self
                .complete_cloud_code(messages, tools, &model, temperature, level)
                .await;
        }
        if self.copilot && copilot_uses_responses(&model) {
            return self
                .complete_responses(messages, tools, &model, extra)
                .await;
        }
        match self.wire {
            Wire::Responses => {
                return self
                    .complete_responses(messages, tools, &model, extra)
                    .await;
            }
            Wire::AnthropicKey | Wire::AnthropicPlan => {
                return self
                    .complete_anthropic(messages, tools, &model, temperature, extra)
                    .await;
            }
            Wire::ChatCompletions => {}
        }
        let mut body = json!({
            "model": model,
            "messages": messages.iter().map(message_to_json).collect::<Vec<_>>(),
            "max_tokens": self.max_output_tokens,
        });
        if self.copilot
            && let Some(effort) = extra
                .and_then(|fields| fields.get("reasoning"))
                .and_then(|r| r.get("effort"))
                .and_then(Value::as_str)
            && matches!(effort, "low" | "medium" | "high")
        {
            body["reasoning_effort"] = json!(effort);
        }
        if !tools.is_empty() {
            body["tools"] = Value::Array(
                tools
                    .iter()
                    .map(|tool| {
                        json!({
                            "type": "function",
                            "function": {
                                "name": tool.name,
                                "description": tool.description,
                                "parameters": tool.parameters,
                            }
                        })
                    })
                    .collect(),
            );
            body["tool_choice"] = json!("auto");
        }
        if let Some(temperature) = temperature {
            body["temperature"] = json!(temperature.clamp(0.0, 2.0));
        }
        if let Some(extra) = extra {
            for (key, value) in extra {
                body[key.as_str()] = value.clone();
            }
        }

        let url = format!("{}/chat/completions", self.base_url);
        let mut last_error = None;
        let mut refreshed = false;
        for attempt in 0..=self.max_retries + 1 {
            let mut key = self.bearer();
            let used = zeroize::Zeroizing::new(key.clone());
            let request = self.http.post(&url).bearer_auth(&key).json(&body);
            let response = self.decorate(request).send().await;
            key.zeroize();
            match response {
                Ok(response) => {
                    let status = response.status();
                    if status.as_u16() == 401 && self.oauth.is_some() && !refreshed {
                        refreshed = true;
                        self.refresh_oauth(&used).await?;
                        continue;
                    }
                    if status.as_u16() == 401 && self.oauth.is_none() {
                        self.park_for_reauth();
                    }
                    if status.is_success() {
                        let bytes = response.bytes().await.map_err(|error| {
                            llm_failure::transport_error(format!(
                                "LLM response read failed: {error}"
                            ))
                        })?;
                        return parse_response(&bytes);
                    }
                    // The provider answered, so the completion did not run: safe to retry.
                    let hint = retry::retry_after(response.headers());
                    let hold = llm_failure::retry_after_hold(response.headers());
                    let text = response.text().await.unwrap_or_default();
                    let sanitized = sanitize_provider_error(&text);
                    let error = llm_failure::upstream_error(
                        status.as_u16(),
                        &text,
                        hold,
                        format!(
                            "LLM request failed with status {}: {sanitized}",
                            status.as_u16()
                        ),
                    );
                    // A spent quota does not clear in seconds: retrying only
                    // delays the failover that can help.
                    let retryable = retry::transient_status(status.as_u16())
                        && !matches!(
                            &error,
                            SeraError::Upstream {
                                code: llm_failure::CODE_QUOTA_EXHAUSTED,
                                ..
                            }
                        );
                    last_error = Some(error);
                    if !retryable || attempt == self.max_retries {
                        break;
                    }
                    tokio::time::sleep(retry::delay(
                        Duration::from_millis(500),
                        attempt as u32,
                        hint,
                    ))
                    .await;
                }
                Err(error) => {
                    // A completion POST that timed out may still have run, so only a
                    // connect failure (the request never left) is retried here.
                    last_error = Some(llm_failure::transport_error(format!(
                        "LLM request failed: {error}"
                    )));
                    if !retry::transport_retryable(&error, false) || attempt == self.max_retries {
                        break;
                    }
                    tokio::time::sleep(retry::delay(
                        Duration::from_millis(500),
                        attempt as u32,
                        None,
                    ))
                    .await;
                }
            }
        }
        Err(last_error.unwrap_or_else(|| llm_failure::transport_error("LLM request failed".into())))
    }

    pub async fn list_models(&self) -> Result<std::collections::BTreeSet<String>> {
        self.fetch_model_catalog(&format!("{}/models", self.base_url), false)
            .await
            .map(|catalog| catalog.into_keys().collect())
    }

    pub(crate) async fn fetch_model_catalog(
        &self,
        catalog_url: &str,
        strict: bool,
    ) -> Result<ModelCatalog> {
        self.ensure_fresh_token().await?;
        if url::Url::parse(catalog_url)
            .ok()
            .is_some_and(|url| url.path().ends_with(":fetchAvailableModels"))
        {
            return self.fetch_cloud_code_catalog(catalog_url).await;
        }
        if self.wire.is_anthropic() {
            return self.fetch_anthropic_catalog(catalog_url).await;
        }
        // A Copilot plan lists its models on its own host, which is this
        // client's base (the configured one is the individual plan's).
        let url = if self.copilot {
            validate_base_url(&format!("{}/models", self.base_url))?
        } else {
            validate_base_url(catalog_url)?
        };
        let mut refreshed = false;
        let mut last_error = None;
        for attempt in 0..=self.max_retries + 1 {
            let mut key = self.bearer();
            let used = zeroize::Zeroizing::new(key.clone());
            let mut request = self
                .http
                .get(&url)
                .bearer_auth(&key)
                .header("accept", "application/json");
            if self.copilot {
                // The editor identity (its own user agent) comes from `decorate`.
                let (name, value) = crate::auth_flows::copilot::CATALOG_INTENT;
                request = request.header(name, value);
            } else {
                request = request.header("user-agent", USER_AGENT);
            }
            let response = self.decorate(request).send().await;
            key.zeroize();
            match response {
                Ok(response) => {
                    let status = response.status();
                    if status.as_u16() == 401 && self.oauth.is_some() && !refreshed {
                        refreshed = true;
                        self.refresh_oauth(&used).await?;
                        continue;
                    }
                    if status.as_u16() == 401 && self.oauth.is_none() {
                        self.park_for_reauth();
                    }
                    if catalog_body_too_large(&response) {
                        last_error = Some(SeraError::External("model catalog is too large".into()));
                        break;
                    }
                    if status.is_success() {
                        let bytes = response
                            .bytes()
                            .await
                            .map_err(|_| SeraError::External("model catalog read failed".into()))?;
                        let mut catalog = parse_model_catalog(&bytes, strict)?;
                        if self.copilot {
                            catalog.entry("auto".into()).or_default();
                        }
                        return Ok(catalog);
                    }
                    // Drop the body. Provider errors can echo credentials; the status is enough.
                    let hint = retry::retry_after(response.headers());
                    let hold = llm_failure::retry_after_hold(response.headers());
                    let _ = response.bytes().await;
                    let retryable = retry::transient_status(status.as_u16());
                    last_error = Some(llm_failure::upstream_error(
                        status.as_u16(),
                        "",
                        hold,
                        format!("model catalog failed with status {}", status.as_u16()),
                    ));
                    if !retryable || attempt == self.max_retries {
                        break;
                    }
                    tokio::time::sleep(retry::delay(
                        Duration::from_millis(200),
                        attempt as u32,
                        hint,
                    ))
                    .await;
                }
                Err(error) => {
                    // A catalog GET is idempotent, so timeouts are retried too.
                    last_error = Some(llm_failure::transport_error(
                        "model catalog request failed".into(),
                    ));
                    if !retry::transport_retryable(&error, true) || attempt == self.max_retries {
                        break;
                    }
                    tokio::time::sleep(retry::delay(
                        Duration::from_millis(200),
                        attempt as u32,
                        None,
                    ))
                    .await;
                }
            }
        }
        Err(last_error
            .unwrap_or_else(|| llm_failure::transport_error("model catalog request failed".into())))
    }

    /// Renew the access token after a request made with `used` was rejected
    /// (or is about to expire). A member's account refreshes through the
    /// single-flight refresher; other clients refresh on their own.
    async fn refresh_oauth(&self, used: &str) -> Result<()> {
        let oauth = self
            .oauth
            .as_ref()
            .ok_or_else(|| SeraError::External("oauth refresh is not configured".into()))?;
        let account = oauth
            .account
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .as_ref()
            .map(|state| (state.target.clone(), state.generation.clone()));
        match account {
            Some((target, generation)) => {
                self.refresh_account(oauth, &target, generation, used).await
            }
            None => self.refresh_unshared().await,
        }
    }

    async fn refresh_account(
        &self,
        oauth: &OAuthState,
        target: &AccountRefreshTarget,
        generation: Option<String>,
        used: &str,
    ) -> Result<()> {
        // A sibling request on this client already replaced the token that
        // was rejected: retry with it instead of refreshing again.
        if self.bearer() != used {
            return Ok(());
        }
        let refresh = oauth
            .refresh_token
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .expose_secret()
            .to_owned();
        let seen = CredentialStamp::new(generation, &refresh, used);
        let style = oauth.style;
        let http = self.http.clone();
        let token_url = oauth.token_url.clone();
        let client_id = oauth.client_id.clone();
        let client_secret = oauth.client_secret.expose_secret().to_owned();
        let exchange = move |stored_refresh: String| async move {
            exchange_for_grant(
                &http,
                style,
                &token_url,
                &stored_refresh,
                &client_id,
                &client_secret,
            )
            .await
        };
        let Refreshed { credential, .. } = target
            .refresher
            .refresh(
                &target.user_id,
                &target.provider,
                &target.account_id,
                &seen,
                exchange,
            )
            .await?;
        self.adopt_credential(oauth, &credential);
        Ok(())
    }

    /// Take over a credential the refresher stored (ours, or a sibling's).
    fn adopt_credential(
        &self,
        oauth: &OAuthState,
        credential: &crate::user_secrets::ProviderCredential,
    ) {
        *self
            .api_key
            .lock()
            .unwrap_or_else(|error| error.into_inner()) =
            SecretString::new(credential.api_key.clone());
        if !credential.refresh_token.is_empty() {
            *oauth
                .refresh_token
                .lock()
                .unwrap_or_else(|error| error.into_inner()) =
                SecretString::new(credential.refresh_token.clone());
        }
        if let Some(state) = oauth
            .account
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .as_mut()
        {
            state.generation = credential.generation.clone();
            state.expires_at = credential.expires_at;
        }
    }

    /// Renew this client's token now when it expires within five minutes.
    pub async fn refresh_if_stale(&self) -> Result<()> {
        self.ensure_fresh_token().await
    }

    /// Refresh ahead of expiry: when this account's access token expires within
    /// five minutes, renew it before the request instead of after a 401. A
    /// transient failure is ignored (the current token still works for now);
    /// a refused sign-in is reported.
    async fn ensure_fresh_token(&self) -> Result<()> {
        let Some(oauth) = self.oauth.as_ref() else {
            return Ok(());
        };
        let near = oauth
            .account
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .as_ref()
            .and_then(|state| state.expires_at)
            .is_some_and(|at| {
                at - chrono::Utc::now().timestamp() <= crate::accounts::REFRESH_SKEW_SECS
            });
        if !near {
            return Ok(());
        }
        let used = zeroize::Zeroizing::new(self.bearer());
        match self.refresh_oauth(&used).await {
            Err(
                error @ SeraError::Rejected {
                    code: "needs_reauth",
                    ..
                },
            ) => Err(error),
            Err(error) => {
                tracing::debug!(%error, "early token refresh failed; using the current token");
                Ok(())
            }
            Ok(()) => Ok(()),
        }
    }

    async fn refresh_unshared(&self) -> Result<()> {
        let oauth = self
            .oauth
            .as_ref()
            .ok_or_else(|| SeraError::External("oauth refresh is not configured".into()))?;
        let token = match oauth.style {
            // A rotating token cannot be refreshed outside the single-flight
            // account path: a lost write would lock the account out.
            OAuthStyle::ClaudeRefresh => {
                return Err(SeraError::Forbidden(
                    "Claude plan tokens refresh only through a member account".into(),
                ));
            }
            OAuthStyle::CodexRefresh => {
                return Err(SeraError::Forbidden(
                    "Codex plan tokens refresh only through a member account".into(),
                ));
            }
            OAuthStyle::CopilotExchange => {
                return Err(SeraError::Forbidden(
                    "Copilot tokens refresh only through a member account".into(),
                ));
            }
            OAuthStyle::MuseMint => {
                let refresh = oauth
                    .refresh_token
                    .lock()
                    .unwrap_or_else(|error| error.into_inner())
                    .expose_secret()
                    .to_owned();
                muse_mint_api_key(&self.http, &oauth.token_url, &refresh).await?
            }
            OAuthStyle::RefreshToken => {
                let mut refresh = oauth
                    .refresh_token
                    .lock()
                    .unwrap_or_else(|error| error.into_inner())
                    .expose_secret()
                    .to_owned();
                let mut client_secret = oauth.client_secret.expose_secret().to_owned();
                let grant = refresh_token_grant(
                    &self.http,
                    &oauth.token_url,
                    &refresh,
                    &oauth.client_id,
                    &client_secret,
                )
                .await;
                refresh.zeroize();
                client_secret.zeroize();
                let grant = grant?;
                if let Some(mut rotated) = grant.refresh_token {
                    let mut slot = oauth
                        .refresh_token
                        .lock()
                        .unwrap_or_else(|error| error.into_inner());
                    if slot.expose_secret().as_str() != rotated {
                        *slot = SecretString::new(rotated.clone());
                        oauth.refresh_unpersisted.store(true, Ordering::SeqCst);
                    }
                    rotated.zeroize();
                }
                let mut access = grant.access_token;
                if oauth.refresh_unpersisted.load(Ordering::SeqCst) {
                    let mut current = oauth
                        .refresh_token
                        .lock()
                        .unwrap_or_else(|error| error.into_inner())
                        .expose_secret()
                        .to_owned();
                    let persisted = self.persist_rotated(&current, &access);
                    current.zeroize();
                    if let Err(error) = persisted {
                        access.zeroize();
                        return Err(error);
                    }
                    oauth.refresh_unpersisted.store(false, Ordering::SeqCst);
                }
                access
            }
        };
        *self
            .api_key
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = SecretString::new(token);
        Ok(())
    }

    fn persist_rotated(&self, refresh: &str, access: &str) -> Result<()> {
        let oauth = self.oauth.as_ref().ok_or_else(|| {
            SeraError::Secrets("rotated oauth refresh token was not persisted".into())
        })?;
        let writer = {
            let persistence = oauth
                .persistence
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            let Some(persistence) = persistence.as_ref() else {
                return Err(SeraError::Secrets(
                    "rotated oauth refresh token was not persisted".into(),
                ));
            };
            persistence.writer.clone()
        };
        match writer.persist_rotation(refresh, Some(access)) {
            Ok(()) => Ok(()),
            Err(error) => Err(redact_secret_error(error, &[refresh, access])),
        }
    }

    /// A rejected credential nothing can refresh: park the member's account so
    /// routing skips it until they sign in again.
    fn park_for_reauth(&self) {
        if !self.reauth_on_401 {
            return;
        }
        let Some(target) = &self.account else {
            return;
        };
        if let Err(error) = target.refresher.accounts().set_status(
            &target.user_id,
            &target.provider,
            &target.account_id,
            crate::accounts::AccountStatus::NeedsReauth,
            Some("sign in again"),
            None,
        ) {
            tracing::warn!(%error, "could not mark an account as needing sign-in");
        }
    }

    /// Rotate the Cloud Code session after Google's token wall. `seen` is the
    /// generation the failed request used; a sibling that already rotated it is
    /// not rotated twice.
    fn bump_session(&self, seen: i64) {
        let next = match &self.account {
            Some(target) => target
                .refresher
                .accounts()
                .store()
                .bump_session_generation(
                    &target.user_id,
                    &target.provider,
                    &target.account_id,
                    seen,
                )
                .unwrap_or(seen + 1),
            None => seen + 1,
        };
        self.session_generation.fetch_max(next, Ordering::SeqCst);
    }

    /// The Cloud Code project for this request: the configured or remembered
    /// one, else (once per client, and not within an hour of a failed attempt)
    /// what discovery finds for the signed-in account.
    async fn cloud_project(&self) -> String {
        let configured = self.cloud_code.clone().unwrap_or_default();
        let Some(state) = &self.project else {
            return configured;
        };
        if let Some(known) = state
            .known
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clone()
        {
            return known;
        }
        if state.tried.swap(true, Ordering::SeqCst) {
            return configured;
        }
        let Ok(origins) = cloud_code::discovery_origins(&self.base_url) else {
            return configured;
        };
        let version = cloud_code::antigravity_version(self.cloud_version.as_deref()).await;
        let token = zeroize::Zeroizing::new(self.bearer());
        let found = cloud_code::discover_project_with(
            &self.http,
            &token,
            &origins,
            Duration::from_secs(3),
            Some(&version),
        )
        .await;
        state.memo.record(&found);
        match found.project {
            Some(project) => {
                *state
                    .known
                    .lock()
                    .unwrap_or_else(|error| error.into_inner()) = Some(project.clone());
                project
            }
            None => configured,
        }
    }

    fn clear_negative_project_discovery(&self) {
        if let Some(state) = &self.project {
            let has_known = state
                .known
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .is_some();
            if !has_known {
                state.tried.store(false, Ordering::SeqCst);
            }
        }
    }

    /// Read a finished Cloud Code answer. `None` means Google returned a
    /// malformed tool call and nothing else: send the request again.
    fn cloud_answer(&self, bytes: &[u8], tools: &[ToolSpec]) -> Result<Option<LlmResponse>> {
        let parsed = cloud_code::parse_cloud_code_stream(bytes)?;
        self.finish_cloud_answer(parsed, tools)
    }

    fn finish_cloud_answer(
        &self,
        mut parsed: CloudCodeCompletion,
        tools: &[ToolSpec],
    ) -> Result<Option<LlmResponse>> {
        if parsed.is_malformed_call() {
            return Ok(None);
        }
        if parsed.text.trim().is_empty() && parsed.tool_calls.is_empty() {
            return Err(SeraError::External(
                "Cloud Code response contained no content".into(),
            ));
        }
        cloud_code::restore_tool_names(&mut parsed, tools);
        cloud_completion(parsed).map(Some)
    }

    async fn complete_cloud_code(
        &self,
        messages: &[LlmMessage],
        tools: &[ToolSpec],
        model: &str,
        temperature: Option<f32>,
        thinking_level: Option<&str>,
    ) -> Result<LlmResponse> {
        let mut project = self.cloud_project().await;
        let conversation = current_conversation();
        let targets = cloud_code::chat_targets(&self.base_url)?;
        let mut refreshed = false;
        let mut version_retried = false;
        let mut malformed = 0usize;
        // The id sent on the wire: the asked model, or once its HielCowork wire
        // alias when a cluster does not know the plain name.
        let mut wire_model = model;
        let mut tried_wire = false;
        let mut effective_thinking = thinking_level;
        let mut stripped_thinking = false;
        let mut last_error: Option<SeraError> = None;
        for _ in 0..3 {
            let mut restart = false;
            for target in &targets {
                let mut session_bumped = false;
                loop {
                    let generation = self.session_generation.load(Ordering::SeqCst);
                    let envelope = cloud_code::build_cloud_code_envelope_with(
                        wire_model,
                        &project,
                        messages,
                        tools,
                        self.max_output_tokens,
                        temperature,
                        &cloud_code::EnvelopeOptions {
                            conversation: &conversation,
                            session_generation: generation,
                            thinking_level: effective_thinking,
                        },
                    )?;
                    let mut used = zeroize::Zeroizing::new(self.bearer());
                    let mut post = self
                        .post_cloud(&target.stream, &envelope, wire_model, true)
                        .await?;
                    if matches!(post, CloudPost::NotFound) {
                        used = zeroize::Zeroizing::new(self.bearer());
                        post = self
                            .post_cloud(&target.generate, &envelope, wire_model, false)
                            .await?;
                        if matches!(post, CloudPost::NotFound) {
                            if !tried_wire
                                && let Some(alias) = cloud_code::wire_fallback_model(wire_model)
                            {
                                tried_wire = true;
                                wire_model = alias;
                                continue;
                            }
                            last_error = Some(cloud_status_error(404));
                            break;
                        }
                    }
                    match post {
                        CloudPost::Ready(bytes) => match self.cloud_answer(&bytes, tools)? {
                            Some(response) => return Ok(response),
                            None => {
                                malformed += 1;
                                if malformed >= cloud_code::MALFORMED_CALL_ATTEMPTS {
                                    return Err(SeraError::External(
                                        "The model repeatedly emitted a malformed tool call (MALFORMED_FUNCTION_CALL)."
                                            .into(),
                                    ));
                                }
                                continue;
                            }
                        },
                        CloudPost::Parsed(parsed) => {
                            match self.finish_cloud_answer(parsed, tools)? {
                                Some(response) => return Ok(response),
                                None => {
                                    malformed += 1;
                                    if malformed >= cloud_code::MALFORMED_CALL_ATTEMPTS {
                                        return Err(SeraError::External(
                                        "The model repeatedly emitted a malformed tool call (MALFORMED_FUNCTION_CALL)."
                                            .into(),
                                    ));
                                    }
                                    continue;
                                }
                            }
                        }
                        CloudPost::Unauthorized if !refreshed && self.oauth.is_some() => {
                            refreshed = true;
                            self.refresh_oauth(&used).await?;
                            restart = true;
                            break;
                        }
                        CloudPost::SessionLimited if !session_bumped => {
                            session_bumped = true;
                            self.bump_session(generation);
                            continue;
                        }
                        CloudPost::Retryable(error) => {
                            if !version_retried
                                && matches!(&error, SeraError::Upstream { status: 403, .. })
                            {
                                version_retried = true;
                                cloud_code::invalidate_live_version().await;
                                if project.is_empty() {
                                    self.clear_negative_project_discovery();
                                    project = self.cloud_project().await;
                                }
                                restart = true;
                                break;
                            }
                            if !target.stream.contains(".sandbox.googleapis.com")
                                || last_error.is_none()
                            {
                                last_error = Some(error);
                            }
                            break;
                        }
                        CloudPost::BadRequest(error) => {
                            if !stripped_thinking && effective_thinking.is_some() {
                                stripped_thinking = true;
                                effective_thinking = None;
                                continue;
                            }
                            if !tried_wire
                                && let Some(alias) = cloud_code::wire_fallback_model(wire_model)
                            {
                                tried_wire = true;
                                wire_model = alias;
                                continue;
                            }
                            if !version_retried {
                                version_retried = true;
                                cloud_code::invalidate_live_version().await;
                                if project.is_empty() {
                                    self.clear_negative_project_discovery();
                                    project = self.cloud_project().await;
                                }
                                restart = true;
                                break;
                            }
                            if !target.stream.contains(".sandbox.googleapis.com") {
                                last_error = Some(error);
                                break;
                            }
                            return Err(last_error.unwrap_or(error));
                        }
                        CloudPost::Failed(error) => {
                            if !version_retried
                                && matches!(
                                    &error,
                                    SeraError::Upstream {
                                        status: 400 | 403,
                                        ..
                                    }
                                )
                            {
                                version_retried = true;
                                cloud_code::invalidate_live_version().await;
                                if project.is_empty() {
                                    self.clear_negative_project_discovery();
                                    project = self.cloud_project().await;
                                }
                                restart = true;
                                break;
                            }
                            return Err(error);
                        }
                        CloudPost::Unauthorized => return Err(cloud_status_error(401)),
                        CloudPost::NotFound | CloudPost::SessionLimited | CloudPost::Transport => {
                            break;
                        }
                    }
                }
                if restart {
                    break;
                }
            }
            if !restart {
                break;
            }
        }
        Err(last_error
            .unwrap_or_else(|| llm_failure::transport_error("Cloud Code request failed".into())))
    }

    async fn fetch_cloud_code_catalog(&self, catalog_url: &str) -> Result<ModelCatalog> {
        let urls = cloud_code::catalog_urls(catalog_url)?;
        let mut project = self.cloud_project().await;
        let mut refreshed = false;
        let mut version_retried = false;
        let mut last_error: Option<SeraError> = None;
        for _ in 0..3 {
            let body = if project.is_empty() {
                json!({})
            } else {
                json!({"project": project})
            };
            let mut restart = false;
            for url in &urls {
                let used = zeroize::Zeroizing::new(self.bearer());
                match self.post_cloud(url, &body, "", false).await? {
                    CloudPost::Ready(bytes) => {
                        return cloud_code::parse_cloud_code_models(&bytes);
                    }
                    CloudPost::Parsed(_) => return Ok(ModelCatalog::new()),
                    CloudPost::Unauthorized if !refreshed && self.oauth.is_some() => {
                        refreshed = true;
                        self.refresh_oauth(&used).await?;
                        restart = true;
                        break;
                    }
                    CloudPost::Unauthorized => return Err(catalog_status_error(401)),
                    CloudPost::NotFound => last_error = Some(catalog_status_error(404)),
                    CloudPost::Retryable(error) => {
                        if !version_retried
                            && matches!(&error, SeraError::Upstream { status: 403, .. })
                        {
                            version_retried = true;
                            cloud_code::invalidate_live_version().await;
                            if project.is_empty() {
                                self.clear_negative_project_discovery();
                                project = self.cloud_project().await;
                            }
                            restart = true;
                            break;
                        }
                        last_error = Some(catalog_retag(error));
                    }
                    CloudPost::SessionLimited => last_error = Some(catalog_status_error(400)),
                    CloudPost::BadRequest(error) => {
                        if !version_retried {
                            version_retried = true;
                            cloud_code::invalidate_live_version().await;
                            if project.is_empty() {
                                self.clear_negative_project_discovery();
                                project = self.cloud_project().await;
                            }
                            restart = true;
                            break;
                        }
                        last_error = Some(catalog_retag(error));
                    }
                    CloudPost::Failed(error) => {
                        if !version_retried
                            && matches!(
                                &error,
                                SeraError::Upstream {
                                    status: 400 | 403,
                                    ..
                                }
                            )
                        {
                            version_retried = true;
                            cloud_code::invalidate_live_version().await;
                            if project.is_empty() {
                                self.clear_negative_project_discovery();
                                project = self.cloud_project().await;
                            }
                            restart = true;
                            break;
                        }
                        return Err(catalog_retag(error));
                    }
                    CloudPost::Transport => {}
                }
            }
            if !restart {
                break;
            }
        }
        Err(last_error
            .unwrap_or_else(|| llm_failure::transport_error("model catalog request failed".into())))
    }

    async fn post_cloud(
        &self,
        url: &str,
        body: &Value,
        _model: &str,
        stream: bool,
    ) -> Result<CloudPost> {
        let version = cloud_code::antigravity_version(self.cloud_version.as_deref()).await;
        let mut key = self.bearer();
        let request = self
            .http
            .post(url)
            .bearer_auth(&key)
            .header(
                "user-agent",
                cloud_code::cloud_code_user_agent(Some(&version)),
            )
            .header("x-goog-api-client", CLOUD_CODE_API_CLIENT)
            .header(
                "accept",
                if stream {
                    "text/event-stream"
                } else {
                    "application/json"
                },
            )
            .json(body);
        // HielCowork sends no `anthropic-beta` header to Cloud Code, and its
        // chats work; the model id alone selects thinking.
        let response = request.send().await;
        key.zeroize();
        let response = match response {
            Ok(response) => response,
            Err(_) => return Ok(CloudPost::Transport),
        };
        Ok(classify_cloud(response, stream).await)
    }
}

/// Run one token exchange for an account: a Google-style refresh grant, or a
/// Muse re-mint from the identity token.
async fn exchange_for_grant(
    http: &reqwest::Client,
    style: OAuthStyle,
    token_url: &str,
    refresh_token: &str,
    client_id: &str,
    client_secret: &str,
) -> std::result::Result<RefreshGrant, RefreshError> {
    match style {
        OAuthStyle::MuseMint => {
            match muse_mint_api_key(http, token_url, refresh_token).await {
                Ok(key) => Ok(RefreshGrant {
                    access_token: key,
                    refresh_token: None,
                    expires_at: None,
                }),
                // A refused identity token cannot mint again: sign in again.
                Err(SeraError::Upstream {
                    status: 401 | 403, ..
                }) => Err(RefreshError::InvalidGrant),
                Err(error) => Err(RefreshError::Failed(error)),
            }
        }
        OAuthStyle::ClaudeRefresh => {
            crate::auth_flows::claude::refresh_exchange(http, token_url, refresh_token, client_id)
                .await
        }
        OAuthStyle::CodexRefresh => {
            crate::auth_flows::codex::refresh_exchange(http, token_url, refresh_token, client_id)
                .await
        }
        OAuthStyle::CopilotExchange => {
            crate::auth_flows::copilot::refresh_exchange(http, token_url, refresh_token, client_id)
                .await
        }
        OAuthStyle::RefreshToken => {
            let grant =
                refresh_token_exchange(http, token_url, refresh_token, client_id, client_secret)
                    .await?;
            Ok(RefreshGrant {
                access_token: grant.access_token,
                refresh_token: grant.refresh_token,
                expires_at: grant
                    .expires_in
                    .map(|seconds| chrono::Utc::now().timestamp() + seconds as i64),
            })
        }
    }
}

enum CloudPost {
    Ready(Vec<u8>),
    Parsed(CloudCodeCompletion),
    Unauthorized,
    NotFound,
    SessionLimited,
    /// A chat 400 that is not the session wall. When the body names the model
    /// as unknown or unavailable the typed code is `model_unavailable` and the
    /// route chain moves to the next candidate; HielCowork additionally retries
    /// its one wire id once, which the caller does before giving up.
    BadRequest(SeraError),
    /// A status worth trying the next endpoint for, already typed.
    Retryable(SeraError),
    Failed(SeraError),
    Transport,
}

/// A Cloud Code status failure with no body to classify.
fn cloud_status_error(status: u16) -> SeraError {
    llm_failure::upstream_error(
        status,
        "",
        None,
        format!("Cloud Code request failed with status {status}"),
    )
}

fn catalog_status_error(status: u16) -> SeraError {
    llm_failure::upstream_error(
        status,
        "",
        None,
        format!("model catalog failed with status {status}"),
    )
}

/// Word a Cloud Code failure as a catalog failure, keeping its typed code.
fn catalog_retag(error: SeraError) -> SeraError {
    match error {
        SeraError::Upstream {
            status,
            code,
            retry_after,
            ambiguous,
            ..
        } if status != 0 => SeraError::Upstream {
            status,
            code,
            message: format!("model catalog failed with status {status}"),
            retry_after,
            ambiguous,
        },
        other => other,
    }
}

/// A bounded error body, read only to pick the failure code.
async fn cloud_error_body(response: reqwest::Response) -> String {
    if response.content_length().is_some_and(|len| len > 65_536) {
        return String::new();
    }
    llm_failure::bounded_body(&response.bytes().await.unwrap_or_default())
}

const CLOUD_IDLE_TIMEOUT: Duration = Duration::from_secs(120);
const CLOUD_MAX_BYTES: usize = 4_194_304;

async fn read_cloud_sse(response: reqwest::Response) -> CloudPost {
    let mut reader = CloudCodeStreamReader::new();
    let mut stream = response.bytes_stream();
    loop {
        let next = match tokio::time::timeout(CLOUD_IDLE_TIMEOUT, stream.next()).await {
            Ok(Some(Ok(chunk))) => chunk,
            Ok(Some(Err(_))) | Err(_) => return CloudPost::Transport,
            Ok(None) => break,
        };
        if let Err(error) = reader.push_chunk(&next) {
            return CloudPost::Failed(error);
        }
    }
    match reader.finish() {
        Ok(completion) => CloudPost::Parsed(completion),
        Err(error) => CloudPost::Failed(error),
    }
}

async fn read_cloud_bytes(response: reqwest::Response) -> CloudPost {
    let mut bytes = Vec::new();
    let mut stream = response.bytes_stream();
    loop {
        let next = match tokio::time::timeout(CLOUD_IDLE_TIMEOUT, stream.next()).await {
            Ok(Some(Ok(chunk))) => chunk,
            Ok(Some(Err(_))) | Err(_) => return CloudPost::Transport,
            Ok(None) => break,
        };
        if bytes.len().saturating_add(next.len()) > CLOUD_MAX_BYTES {
            return CloudPost::Failed(SeraError::External(
                "Cloud Code response is too large".into(),
            ));
        }
        bytes.extend_from_slice(&next);
    }
    CloudPost::Ready(bytes)
}

async fn classify_cloud(response: reqwest::Response, allow_session: bool) -> CloudPost {
    let status = response.status().as_u16();
    if status == 200 {
        if response
            .content_length()
            .is_some_and(|len| len > CLOUD_MAX_BYTES as u64)
        {
            return CloudPost::Failed(SeraError::External(
                "Cloud Code response is too large".into(),
            ));
        }
        if allow_session {
            return read_cloud_sse(response).await;
        }
        return read_cloud_bytes(response).await;
    }
    if status == 401 {
        return CloudPost::Unauthorized;
    }
    if status == 404 {
        return CloudPost::NotFound;
    }
    let hold = llm_failure::retry_after_hold(response.headers());
    let text = cloud_error_body(response).await;
    let detail = extract_cloud_error_detail(&text);
    tracing::warn!(
        status,
        detail = detail.as_deref().unwrap_or(""),
        "Cloud Code upstream error"
    );
    let error = llm_failure::upstream_error(
        status,
        &text,
        hold,
        format!("Cloud Code request failed with status {status}"),
    );
    if status == 400 && allow_session {
        if text
            .to_ascii_lowercase()
            .contains("exceeds the maximum number of tokens")
        {
            return CloudPost::SessionLimited;
        }
        return CloudPost::BadRequest(error);
    }
    if status == 403 || status == 429 || status >= 500 {
        return CloudPost::Retryable(error);
    }
    CloudPost::Failed(error)
}

fn extract_cloud_error_detail(body: &str) -> Option<String> {
    let value: Value = serde_json::from_str(body).ok()?;
    let err = value.get("error").unwrap_or(&value);
    let status = err.get("status").and_then(Value::as_str).map(str::trim);
    let msg = err.get("message").and_then(Value::as_str).map(str::trim);
    let combined = match (status, msg) {
        (Some(s), Some(m)) if !s.is_empty() && !m.is_empty() => format!("{s}: {m}"),
        (_, Some(m)) if !m.is_empty() => m.to_owned(),
        (Some(s), _) if !s.is_empty() => s.to_owned(),
        _ => return None,
    };
    let clean: String = combined
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .take(220)
        .collect();
    let clean = clean.trim().to_owned();
    (!clean.is_empty()).then_some(clean)
}

fn cloud_completion(parsed: CloudCodeCompletion) -> Result<LlmResponse> {
    let content = if parsed.text.is_empty() {
        None
    } else {
        Some(parsed.text)
    };
    let tool_calls = parsed
        .tool_calls
        .into_iter()
        .map(|call| ToolCall {
            id: call.id,
            name: call.name,
            arguments: call.arguments,
            reasoning_signature: call.signature,
        })
        .collect::<Vec<_>>();
    let reasoning = {
        let text: String = parsed
            .reasoning
            .chars()
            .filter(|c| !c.is_control() || matches!(c, '\n' | '\t'))
            .take(crate::thinking::MAX_REASONING_CHARS)
            .collect();
        let text = text.trim().to_owned();
        (!text.is_empty()).then_some(text)
    };
    if content.as_deref().unwrap_or_default().trim().is_empty() && tool_calls.is_empty() {
        return Err(SeraError::External(
            "Cloud Code response contained no content".into(),
        ));
    }
    Ok(LlmResponse {
        message: LlmMessage {
            role: Role::Assistant,
            content,
            tool_calls,
            tool_call_id: None,
            name: None,
            is_error: false,
            images: Vec::new(),
        },
        usage: Usage {
            prompt_tokens: parsed.prompt_tokens,
            completion_tokens: parsed.completion_tokens,
            total_tokens: parsed
                .prompt_tokens
                .saturating_add(parsed.cached_tokens)
                .saturating_add(parsed.completion_tokens),
            cached_tokens: parsed.cached_tokens,
        },
        finish_reason: parsed.finish_reason,
        reasoning,
    })
}

fn catalog_body_too_large(response: &reqwest::Response) -> bool {
    response.content_length().is_some_and(|len| len > 1_048_576)
}

pub(crate) fn validate_base_url(value: &str) -> Result<String> {
    let invalid = || {
        SeraError::Config("LLM endpoint must be HTTPS (exact loopback HTTP is allowed for fixtures), without credentials, query or fragment".into())
    };
    let url = url::Url::parse(value).map_err(|_| invalid())?;
    // URL parsing normalizes aliases and drops empty userinfo; reject those in the
    // original authority as well, rather than relying on normalized host prefixes.
    let authority = value
        .split_once("://")
        .ok_or_else(invalid)?
        .1
        .split(['/', '?', '#'])
        .next()
        .ok_or_else(invalid)?;
    let exact_loopback = authority == "127.0.0.1"
        || authority.starts_with("127.0.0.1:")
        || authority == "[::1]"
        || authority.starts_with("[::1]:");
    let loopback = matches!(url.host(), Some(url::Host::Ipv4(ip)) if ip == std::net::Ipv4Addr::LOCALHOST)
        || matches!(url.host(), Some(url::Host::Ipv6(ip)) if ip == std::net::Ipv6Addr::LOCALHOST);
    if url.host().is_none()
        || value.trim() != value
        || value.chars().any(char::is_control)
        || value.contains('\\')
        || authority.contains('@')
        || !(url.scheme() == "https" || (url.scheme() == "http" && loopback && exact_loopback))
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(invalid());
    }
    Ok(url.as_str().trim_end_matches('/').to_owned())
}

/// Copilot models that are only served on `/responses` (not `/chat/completions`).
pub(crate) fn copilot_uses_responses(model: &str) -> bool {
    let lower = model.to_ascii_lowercase();
    lower.contains("codex")
        || lower.starts_with("gpt-5.3-")
        || lower.starts_with("gpt-5.4-")
        || lower.starts_with("gpt-6")
}

fn message_to_json(message: &LlmMessage) -> Value {
    let content = if message.role == Role::User && !message.images.is_empty() {
        let mut parts = Vec::with_capacity(message.images.len() + 1);
        if let Some(text) = message.content.as_deref().filter(|t| !t.is_empty()) {
            parts.push(json!({"type": "text", "text": text}));
        }
        for img in &message.images {
            parts.push(json!({
                "type": "image_url",
                "image_url": {
                    "url": format!("data:{};base64,{}", img.mime, img.data_base64),
                }
            }));
        }
        Value::Array(parts)
    } else {
        json!(message.content)
    };
    let mut value = json!({
        "role": message.role.as_str(),
        "content": content,
    });
    if !message.tool_calls.is_empty() {
        value["tool_calls"] = Value::Array(
            message
                .tool_calls
                .iter()
                .map(|call| {
                    json!({
                        "id": call.id,
                        "type": "function",
                        "function": {
                            "name": call.name,
                            "arguments": serde_json::to_string(&call.arguments).unwrap_or_else(|_| "{}".into()),
                        }
                    })
                })
                .collect(),
        );
        value["content"] = Value::Null;
    }
    if let Some(tool_call_id) = &message.tool_call_id {
        value["tool_call_id"] = json!(tool_call_id);
    }
    // Only tool results carry `name` on the wire. Assistant, system and user
    // messages never send it: Mistral rejects `body.messages.N.assistant.name`
    // with HTTP 422. Stored messages keep their bot identity; only the
    // outgoing JSON is affected.
    if message.role == Role::Tool
        && let Some(name) = &message.name
    {
        value["name"] = json!(name);
    }
    value
}

fn parse_response(bytes: &[u8]) -> Result<LlmResponse> {
    let value: Value = serde_json::from_slice(bytes)
        .map_err(|error| SeraError::External(format!("malformed LLM response: {error}")))?;
    let choice = value
        .get("choices")
        .and_then(Value::as_array)
        .and_then(|choices| choices.first())
        .ok_or_else(|| SeraError::External("LLM response has no choices".into()))?;
    let message = choice
        .get("message")
        .ok_or_else(|| SeraError::External("LLM response has no message".into()))?;
    let (content, reasoning) = crate::thinking::split_message(message);
    let mut tool_calls = Vec::new();
    if let Some(calls) = message.get("tool_calls").and_then(Value::as_array) {
        for call in calls {
            let id = call
                .get("id")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned();
            let function = call
                .get("function")
                .ok_or_else(|| SeraError::External("tool call missing function".into()))?;
            let name = function
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned();
            if name.is_empty() {
                return Err(SeraError::External("tool call missing name".into()));
            }
            let arguments = function
                .get("arguments")
                .and_then(Value::as_str)
                .unwrap_or("{}");
            let arguments: Value = serde_json::from_str(arguments).map_err(|error| {
                SeraError::External(format!("tool call arguments are not JSON: {error}"))
            })?;
            tool_calls.push(ToolCall {
                id: if id.is_empty() {
                    format!("call_{}", tool_calls.len())
                } else {
                    id
                },
                name,
                arguments,
                reasoning_signature: None,
            });
        }
    }
    if content.as_deref().unwrap_or_default().trim().is_empty() && tool_calls.is_empty() {
        return Err(SeraError::External(
            "LLM returned neither content nor tool calls".into(),
        ));
    }
    let usage = value
        .get("usage")
        .map(Usage::from_chat_json)
        .unwrap_or_default();
    let finish_reason = choice
        .get("finish_reason")
        .and_then(Value::as_str)
        .map(str::to_owned);
    Ok(LlmResponse {
        message: LlmMessage {
            role: Role::Assistant,
            content,
            tool_calls,
            tool_call_id: None,
            name: None,
            is_error: false,
            images: Vec::new(),
        },
        usage,
        finish_reason,
        reasoning,
    })
}

fn redact_secret_error(error: SeraError, secrets: &[&str]) -> SeraError {
    let message = error.to_string();
    if secrets
        .iter()
        .any(|secret| secret.len() >= 8 && message.contains(secret))
    {
        SeraError::Secrets("rotated oauth refresh token was not persisted".into())
    } else {
        error
    }
}

fn sanitize_provider_error(body: &str) -> String {
    let parsed: Option<Value> = serde_json::from_str(body).ok();
    let message = parsed
        .as_ref()
        .and_then(|value| value.pointer("/error/message"))
        .and_then(Value::as_str)
        .or_else(|| {
            parsed
                .as_ref()
                .and_then(|value| value.get("message"))
                .and_then(Value::as_str)
        })
        .unwrap_or(body);
    message.chars().take(500).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::secrets::{SecretStore, SharedSecrets, ensure_identity};
    use wiremock::matchers::{body_string_contains, header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[test]
    fn assistant_and_system_messages_never_send_name() {
        let assistant = LlmMessage {
            role: Role::Assistant,
            content: Some("hello".into()),
            tool_calls: Vec::new(),
            tool_call_id: None,
            name: Some("sera-bot".into()),
            is_error: false,
            images: Vec::new(),
        };
        let value = message_to_json(&assistant);
        assert!(value.get("name").is_none());

        let system = LlmMessage {
            role: Role::System,
            name: Some("sera-bot".into()),
            ..LlmMessage::system("rules")
        };
        assert!(message_to_json(&system).get("name").is_none());

        let tool = LlmMessage {
            role: Role::Tool,
            content: Some("ok".into()),
            tool_calls: Vec::new(),
            tool_call_id: Some("call_1".into()),
            name: Some("rag_search".into()),
            is_error: false,
            images: Vec::new(),
        };
        assert_eq!(message_to_json(&tool)["name"], json!("rag_search"));
    }

    fn tool() -> ToolSpec {
        ToolSpec {
            name: "rag_search".into(),
            description: "search".into(),
            parameters: json!({"type": "object"}),
            external_effect: false,
        }
    }

    #[tokio::test]
    async fn parses_text_response_and_sends_bearer_token() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .and(header("authorization", "Bearer secret"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "choices": [{"message": {"role": "assistant", "content": "bonjour"}, "finish_reason": "stop"}],
                "usage": {"prompt_tokens": 3, "completion_tokens": 2, "total_tokens": 5}
            })))
            .mount(&server)
            .await;
        let client = LlmClient::new(server.uri(), "secret", 10, 100)
            .unwrap()
            .with_model("test");
        let response = client
            .complete(&[LlmMessage::user("salut")], &[], None, None)
            .await
            .unwrap();
        assert_eq!(response.message.content.as_deref(), Some("bonjour"));
        assert_eq!(response.usage.total_tokens, 5);
        let split = Usage::from_chat_json(&json!({
            "prompt_tokens": 100, "completion_tokens": 10, "total_tokens": 110,
            "prompt_tokens_details": {"cached_tokens": 70}
        }));
        assert_eq!(
            (split.prompt_tokens, split.cached_tokens, split.total_tokens),
            (30, 70, 110)
        );
        let deepseek = Usage::from_chat_json(&json!({
            "prompt_tokens": 50, "completion_tokens": 5, "prompt_cache_hit_tokens": 20
        }));
        assert_eq!((deepseek.prompt_tokens, deepseek.cached_tokens), (30, 20));
    }

    #[tokio::test]
    async fn parses_tool_calls() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "choices": [{
                    "message": {
                        "role": "assistant",
                        "content": null,
                        "tool_calls": [{
                            "id": "call_1",
                            "type": "function",
                            "function": {"name": "rag_search", "arguments": "{\"query\":\"test\"}"}
                        }]
                    },
                    "finish_reason": "tool_calls"
                }]
            })))
            .mount(&server)
            .await;
        let client = LlmClient::new(server.uri(), "secret", 10, 100)
            .unwrap()
            .with_model("test");
        let response = client
            .complete(&[LlmMessage::user("salut")], &[tool()], None, None)
            .await
            .unwrap();
        assert_eq!(response.message.tool_calls.len(), 1);
        assert_eq!(response.message.tool_calls[0].name, "rag_search");
    }

    #[test]
    fn rejects_unsafe_endpoints_and_empty_credentials() {
        for url in [
            "http://api.example.com",
            "http://127.0.0.1.evil",
            "https://user:password@example.com",
            "https://@example.com",
            "http://127.1",
            "http://2130706433",
            "https://example.com?key=secret",
            "https://example.com#fragment",
        ] {
            assert!(
                LlmClient::new(url, "secret", 10, 100).is_err(),
                "accepted {url}"
            );
        }
        assert!(LlmClient::new("https://example.com", "", 10, 100).is_err());
        assert!(LlmClient::new("http://127.0.0.1:8080/v1", "fixture", 10, 100).is_ok());
        assert!(LlmClient::new("http://[::1]:8080/v1", "fixture", 10, 100).is_ok());
    }

    #[tokio::test]
    async fn malformed_json_is_an_error() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_string("not json"))
            .mount(&server)
            .await;
        let client = LlmClient::new(server.uri(), "secret", 10, 100)
            .unwrap()
            .with_model("test");
        assert!(
            client
                .complete(&[LlmMessage::user("salut")], &[], None, None)
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn catalog_errors_omit_response_bodies() {
        let server = MockServer::start().await;
        let secret = "synthetic-catalog-secret";
        Mock::given(method("GET"))
            .and(path("/models"))
            .respond_with(ResponseTemplate::new(500).set_body_string(secret))
            .mount(&server)
            .await;
        let client = LlmClient::new(server.uri(), "synthetic-key", 10, 100).unwrap();
        let error = client.list_models().await.unwrap_err();
        let message = error.to_string();
        assert!(message.contains("500"));
        assert!(!message.contains(secret));
        assert!(!message.contains("synthetic-key"));
    }

    /// A failing chat reply as the typed error the failover engine reads.
    async fn chat_failure(status: u16, body: &str, retry_after: Option<&str>) -> SeraError {
        let server = MockServer::start().await;
        let mut response = ResponseTemplate::new(status).set_body_string(body);
        if let Some(value) = retry_after {
            response = response.insert_header("retry-after", value);
        }
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(response)
            .mount(&server)
            .await;
        let client = LlmClient::new(server.uri(), "synthetic-key", 10, 100)
            .unwrap()
            .with_model("test");
        client
            .complete(&[LlmMessage::user("salut")], &[], None, None)
            .await
            .unwrap_err()
    }

    #[tokio::test]
    async fn chat_failures_are_typed_and_classified() {
        use crate::llm_failure::{FailKind, NoSwitch, classify_llm};
        let quota = r#"{"error":{"type":"insufficient_quota","code":"insufficient_quota","message":"sk-leaky-marker"}}"#;
        let error = chat_failure(429, quota, None).await;
        assert!(matches!(
            error,
            SeraError::Upstream {
                status: 429,
                code: "quota_exhausted",
                ambiguous: false,
                ..
            }
        ));
        assert!(matches!(
            classify_llm(&error),
            Ok(FailKind::QuotaExhausted { .. })
        ));

        let error = chat_failure(401, "{}", None).await;
        assert_eq!(classify_llm(&error), Ok(FailKind::CredentialsRejected));
        let error = chat_failure(403, "{}", None).await;
        assert_eq!(classify_llm(&error), Ok(FailKind::AccountForbidden));
        let error = chat_failure(404, "{}", None).await;
        assert_eq!(classify_llm(&error), Ok(FailKind::ModelUnavailable));
        let error =
            chat_failure(400, r#"{"error":{"code":"context_length_exceeded"}}"#, None).await;
        assert_eq!(classify_llm(&error), Ok(FailKind::ContextTooLong));
        let error = chat_failure(400, r#"{"error":{"message":"bad field"}}"#, None).await;
        assert_eq!(classify_llm(&error), Err(NoSwitch::InvalidRequest));
    }

    #[tokio::test]
    async fn a_rate_limit_keeps_the_servers_retry_hint() {
        use crate::llm_failure::{FailKind, classify_llm};
        let error = chat_failure(429, "{}", Some("1")).await;
        assert_eq!(
            classify_llm(&error),
            Ok(FailKind::RateLimited {
                retry_after: Some(Duration::from_secs(1))
            })
        );
    }

    #[tokio::test]
    async fn an_unreachable_provider_is_a_transport_failure() {
        use crate::llm_failure::{FailKind, classify_llm};
        // A port nothing listens on: bind, note it, close.
        let port = std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        let client = LlmClient::new(format!("http://127.0.0.1:{port}"), "synthetic-key", 2, 100)
            .unwrap()
            .with_model("test");
        let error = client
            .complete(&[LlmMessage::user("salut")], &[], None, None)
            .await
            .unwrap_err();
        assert!(matches!(
            error,
            SeraError::Upstream {
                status: 0,
                code: "transport",
                ..
            }
        ));
        assert_eq!(classify_llm(&error), Ok(FailKind::Transport));
    }

    #[tokio::test]
    async fn cloud_code_statuses_are_typed() {
        use crate::llm_failure::{FailKind, classify_llm};
        for (status, body, expected) in [
            (403, "{}", FailKind::AccountForbidden),
            (404, "", FailKind::ModelUnavailable),
            (401, "", FailKind::CredentialsRejected),
            (500, "", FailKind::ProviderDown),
            (429, "{}", FailKind::RateLimited { retry_after: None }),
        ] {
            let server = MockServer::start().await;
            Mock::given(method("POST"))
                .respond_with(ResponseTemplate::new(status).set_body_string(body))
                .mount(&server)
                .await;
            let client = LlmClient::new(server.uri(), "synthetic-key", 10, 128)
                .unwrap()
                .with_cloud_code("proj-synthetic")
                .unwrap()
                .with_model("gemini-test");
            let error = client
                .complete(&[LlmMessage::user("hi")], &[], None, None)
                .await
                .unwrap_err();
            assert_eq!(classify_llm(&error), Ok(expected), "status {status}");
        }
        // A spent quota says when it resets.
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(429).set_body_string(
                r#"{"error":{"code":429,"message":"You have exhausted your capacity on this model. Your quota will reset after 1h.","status":"RESOURCE_EXHAUSTED"}}"#,
            ))
            .mount(&server)
            .await;
        let client = LlmClient::new(server.uri(), "synthetic-key", 10, 128)
            .unwrap()
            .with_cloud_code("proj-synthetic")
            .unwrap()
            .with_model("gemini-test");
        let error = client
            .complete(&[LlmMessage::user("hi")], &[], None, None)
            .await
            .unwrap_err();
        let Ok(FailKind::QuotaExhausted {
            resets_at: Some(at),
        }) = classify_llm(&error)
        else {
            panic!("expected a quota reset, got {error:?}");
        };
        assert!((at - chrono::Utc::now()).num_seconds() > 3500);
        assert!(!error.to_string().contains("exhausted your capacity"));
    }

    #[tokio::test]
    async fn catalog_401_mints_a_muse_key_without_echoing_it() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/models"))
            .and(header("authorization", "Bearer minted-key"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "data": [{"id": "muse-spark"}]
            })))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/models"))
            .and(header("authorization", "Bearer old-key"))
            .respond_with(ResponseTemplate::new(401).set_body_string("old-key identity-token"))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/muse-code/key"))
            .and(header("authorization", "Bearer identity-token"))
            .and(header("user-agent", USER_AGENT))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "api_key": "minted-key"
            })))
            .expect(1)
            .mount(&server)
            .await;
        let client = LlmClient::new(server.uri(), "old-key", 10, 100)
            .unwrap()
            .with_oauth_unpinned(OAuthMaterial {
                style: OAuthStyle::MuseMint,
                refresh_token: "identity-token".into(),
                token_url: format!("{}/muse-code/key", server.uri()),
                client_id: String::new(),
                client_secret: String::new(),
            })
            .unwrap();
        let ids = client.list_models().await.unwrap();
        assert!(ids.contains("muse-spark"));
        assert!(!format!("{ids:?}").contains("identity-token"));
        assert!(!format!("{ids:?}").contains("old-key"));
    }

    #[tokio::test]
    async fn cloud_code_chat_buffers_sse_without_echoing_the_bearer() {
        let server = MockServer::start().await;
        let secret = "synthetic-cloud-token";
        Mock::given(method("POST"))
            .and(path("/v1internal:streamGenerateContent"))
            .and(header("authorization", "Bearer synthetic-cloud-token"))
            .and(header("user-agent", cloud_code::CLOUD_CODE_USER_AGENT))
            .respond_with(ResponseTemplate::new(200).set_body_string(
                "data: {\"response\":{\"candidates\":[{\"content\":{\"parts\":[{\"thought\":true,\"text\":\"synthetic-thought\"},{\"text\":\"hello\"}]},\"finishReason\":\"STOP\"}],\"usageMetadata\":{\"promptTokenCount\":2,\"candidatesTokenCount\":1}}}\n\n",
            ))
            .expect(1)
            .mount(&server)
            .await;
        let client = LlmClient::new(server.uri(), secret, 10, 128)
            .unwrap()
            .with_cloud_code("proj-synthetic")
            .unwrap()
            .with_model("gemini-3.8-flash");
        let response = client
            .complete(&[LlmMessage::user("hello")], &[tool()], None, None)
            .await
            .unwrap();
        assert_eq!(response.message.content.as_deref(), Some("hello"));
        assert!(
            !response
                .message
                .content
                .unwrap()
                .contains("synthetic-thought")
        );
        assert_eq!(response.usage.total_tokens, 3);
        let requests = server.received_requests().await.unwrap();
        let body: Value = requests[0].body_json().unwrap();
        assert_eq!(body["project"], "proj-synthetic");
        assert_eq!(body["model"], "gemini-3.8-flash");
        assert_eq!(
            body["request"]["tools"][0]["functionDeclarations"][0]["parameters"]["type"],
            "OBJECT"
        );
        assert!(body.get("access_token").is_none());
        assert!(!body.to_string().contains(secret));
    }

    #[tokio::test]
    async fn cloud_code_catalog_is_a_post_and_drops_error_bodies() {
        let server = MockServer::start().await;
        let secret = "synthetic-catalog-secret";
        Mock::given(method("POST"))
            .and(path("/v1internal:fetchAvailableModels"))
            .respond_with(ResponseTemplate::new(500).set_body_string(secret))
            .mount(&server)
            .await;
        let client = LlmClient::new(server.uri(), "synthetic-key", 10, 100)
            .unwrap()
            .with_cloud_code("")
            .unwrap();
        let error = client
            .fetch_model_catalog(
                &format!("{}/v1internal:fetchAvailableModels", server.uri()),
                true,
            )
            .await
            .unwrap_err();
        let message = error.to_string();
        assert!(message.contains("500"));
        assert!(!message.contains(secret));
        assert!(!message.contains("synthetic-key"));
        let evil = client
            .fetch_model_catalog("https://evil.example/v1internal:fetchAvailableModels", true)
            .await
            .unwrap_err();
        assert!(!evil.to_string().contains("evil.example"));
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
        ensure_identity(&identity).unwrap();
        let mut store = SecretStore::open_paths(&path, &identity).unwrap();
        for (name, value) in pairs {
            store.set(name, value).unwrap();
        }
        store.save().unwrap();
        let secrets = SharedSecrets::new(SecretStore::open_paths(&path, &identity).unwrap());
        (dir, identity, path, secrets)
    }

    fn antigravity_client(server: &MockServer, secrets: Option<SharedSecrets>) -> LlmClient {
        let client = LlmClient::new(server.uri(), "access-old-value", 10, 128)
            .unwrap()
            .with_cloud_code("proj-synthetic")
            .unwrap()
            .with_oauth_unpinned(OAuthMaterial {
                style: OAuthStyle::RefreshToken,
                refresh_token: "refresh-old-value".into(),
                token_url: format!("{}/token", server.uri()),
                client_id: "client-id-value".into(),
                client_secret: "client-secret-value".into(),
            })
            .unwrap()
            .with_model("gemini-test");
        match secrets {
            Some(secrets) => client
                .with_refresh_persistence(
                    secrets,
                    "antigravity_refresh_token",
                    "antigravity_access_token",
                )
                .unwrap(),
            None => client,
        }
    }

    const CLOUD_OK: &str = "data: {\"response\":{\"candidates\":[{\"content\":{\"parts\":[{\"text\":\"hello\"}]},\"finishReason\":\"STOP\"}],\"usageMetadata\":{\"promptTokenCount\":2,\"candidatesTokenCount\":1}}}\n\n";

    #[tokio::test]
    async fn antigravity_persists_rotated_refresh_and_access_tokens() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1internal:streamGenerateContent"))
            .and(header("authorization", "Bearer access-old-value"))
            .respond_with(ResponseTemplate::new(401))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/v1internal:streamGenerateContent"))
            .and(header("authorization", "Bearer access-new-value"))
            .respond_with(ResponseTemplate::new(200).set_body_string(CLOUD_OK))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/token"))
            .and(body_string_contains("refresh_token=refresh-old-value"))
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
            ("antigravity_refresh_token", "refresh-old-value"),
            ("antigravity_access_token", "access-old-value"),
            ("other_secret", "other-value"),
        ]);
        let client = antigravity_client(&server, Some(secrets.clone()));
        let response = client
            .complete(&[LlmMessage::user("hello")], &[], None, None)
            .await
            .unwrap();
        assert_eq!(response.message.content.as_deref(), Some("hello"));
        assert_eq!(
            secrets.get("antigravity_refresh_token").as_deref(),
            Some("refresh-new-value")
        );
        assert_eq!(
            secrets.get("antigravity_access_token").as_deref(),
            Some("access-new-value")
        );
        assert_eq!(secrets.get("other_secret").as_deref(), Some("other-value"));
        let raw = String::from_utf8_lossy(&std::fs::read(&path).unwrap()).into_owned();
        for secret in [
            "refresh-new-value",
            "refresh-old-value",
            "access-new-value",
            "access-old-value",
            "other-value",
            "client-secret-value",
        ] {
            assert!(!raw.contains(secret), "{secret}");
        }
        assert!(!format!("{response:?}").contains("refresh-new-value"));
        assert!(!format!("{response:?}").contains("access-new-value"));
    }

    #[tokio::test]
    async fn antigravity_store_failure_fails_closed_and_keeps_the_refresh_token() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1internal:streamGenerateContent"))
            .and(header("authorization", "Bearer access-old-value"))
            .respond_with(ResponseTemplate::new(401))
            .expect(2)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/v1internal:streamGenerateContent"))
            .and(header("authorization", "Bearer access-new-value"))
            .respond_with(ResponseTemplate::new(200).set_body_string(CLOUD_OK))
            .expect(0)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "access_token": "access-new-value",
                "refresh_token": "refresh-new-value",
                "expires_in": 3600
            })))
            .expect(2)
            .mount(&server)
            .await;
        let client = antigravity_client(&server, None);
        let first = client
            .complete(&[LlmMessage::user("hello")], &[], None, None)
            .await
            .unwrap_err();
        let message = first.to_string();
        assert!(message.contains("not persisted"), "{message}");
        assert!(!message.contains("refresh-new-value"), "{message}");
        assert!(!message.contains("refresh-old-value"), "{message}");
        assert!(!message.contains("access-new-value"), "{message}");
        assert!(!message.contains("client-secret-value"), "{message}");
        let second = client
            .complete(&[LlmMessage::user("hello")], &[], None, None)
            .await
            .unwrap_err();
        assert!(!second.to_string().contains("refresh-new-value"));
        let requests = server.received_requests().await.unwrap();
        let token_bodies: Vec<String> = requests
            .iter()
            .filter(|request| request.url.path() == "/token")
            .map(|request| String::from_utf8(request.body.clone()).unwrap_or_default())
            .collect();
        assert_eq!(token_bodies.len(), 2);
        assert!(token_bodies[0].contains("refresh-old-value"));
        assert!(token_bodies[1].contains("refresh-new-value"));
        assert!(!token_bodies[1].contains("refresh-old-value"));
    }

    #[tokio::test]
    async fn antigravity_without_a_new_refresh_token_does_not_rewrite_the_store() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1internal:streamGenerateContent"))
            .and(header("authorization", "Bearer access-old-value"))
            .respond_with(ResponseTemplate::new(401))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/v1internal:streamGenerateContent"))
            .and(header("authorization", "Bearer access-new-value"))
            .respond_with(ResponseTemplate::new(200).set_body_string(CLOUD_OK))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/token"))
            .and(body_string_contains("refresh_token=refresh-old-value"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "access_token": "access-new-value",
                "expires_in": 3600
            })))
            .expect(1)
            .mount(&server)
            .await;
        let (_dir, _identity, _path, secrets) = age_store(&[
            ("antigravity_refresh_token", "refresh-old-value"),
            ("antigravity_access_token", "access-old-value"),
        ]);
        let client = antigravity_client(&server, Some(secrets.clone()));
        client
            .complete(&[LlmMessage::user("hello")], &[], None, None)
            .await
            .unwrap();
        assert_eq!(
            secrets.get("antigravity_refresh_token").as_deref(),
            Some("refresh-old-value")
        );
        assert_eq!(
            secrets.get("antigravity_access_token").as_deref(),
            Some("access-old-value")
        );
    }
}
