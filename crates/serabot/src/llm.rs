use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use secrecy::{ExposeSecret, SecretString};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use zeroize::Zeroize;

use crate::cloud_code::{self, CLOUD_CODE_API_CLIENT, CLOUD_CODE_USER_AGENT, CloudCodeCompletion};
use crate::providers::{
    OAuthMaterial, OAuthStyle, USER_AGENT, enforce_oauth_target, muse_mint_api_key,
    parse_model_ids, parse_official_catalog, refresh_token_grant,
};

use crate::domain::{LlmMessage, Role, ToolCall};
use crate::error::{Result, SeraError};
use crate::gateway::ToolSpec;
use crate::secrets::SharedSecrets;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Usage {
    #[serde(default)]
    pub prompt_tokens: u64,
    #[serde(default)]
    pub completion_tokens: u64,
    #[serde(default)]
    pub total_tokens: u64,
}

#[derive(Debug, Clone)]
pub struct LlmResponse {
    pub message: LlmMessage,
    pub usage: Usage,
    pub finish_reason: Option<String>,
}

struct RefreshPersistence {
    secrets: SharedSecrets,
    refresh_secret_name: String,
    access_secret_name: String,
}

struct OAuthState {
    style: OAuthStyle,
    refresh_token: Mutex<SecretString>,
    token_url: String,
    client_id: String,
    client_secret: SecretString,
    persistence: Mutex<Option<RefreshPersistence>>,
    /// Set when a provider rotated the refresh token and the age store does not
    /// yet contain that value. Stays set until `persist` succeeds.
    refresh_unpersisted: AtomicBool,
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
}

impl std::fmt::Debug for LlmClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LlmClient")
            .field("base_url", &self.base_url)
            .field("model", &self.model)
            .finish()
    }
}

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
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(timeout_secs.max(1)))
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
        })
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
        let refresh_secret_name = refresh_secret_name.into();
        if refresh_secret_name.trim().is_empty() {
            return Err(SeraError::Config(
                "oauth refresh secret name is missing".into(),
            ));
        }
        let oauth = self
            .oauth
            .as_ref()
            .ok_or_else(|| SeraError::Config("oauth refresh persistence requires oauth".into()))?;
        *oauth
            .persistence
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = Some(RefreshPersistence {
            secrets,
            refresh_secret_name,
            access_secret_name: access_secret_name.into(),
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
        let model = model_override.unwrap_or(&self.model).to_owned();
        if model.is_empty() {
            return Err(SeraError::Config("LLM model is not configured".into()));
        }
        if self.cloud_code.is_some() {
            return self
                .complete_cloud_code(messages, tools, &model, temperature)
                .await;
        }
        let mut body = json!({
            "model": model,
            "messages": messages.iter().map(message_to_json).collect::<Vec<_>>(),
            "max_tokens": self.max_output_tokens,
        });
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

        let url = format!("{}/chat/completions", self.base_url);
        let mut last_error = None;
        let mut refreshed = false;
        for attempt in 0..=self.max_retries + 1 {
            let mut key = self.bearer();
            let response = self
                .http
                .post(&url)
                .bearer_auth(&key)
                .json(&body)
                .send()
                .await;
            key.zeroize();
            match response {
                Ok(response) => {
                    let status = response.status();
                    if status.as_u16() == 401 && self.oauth.is_some() && !refreshed {
                        refreshed = true;
                        self.refresh_oauth().await?;
                        continue;
                    }
                    if status.is_success() {
                        let bytes = response.bytes().await.map_err(|error| {
                            SeraError::External(format!("LLM response read failed: {error}"))
                        })?;
                        return parse_response(&bytes);
                    }
                    let retryable = status.as_u16() == 429 || status.is_server_error();
                    let text = response.text().await.unwrap_or_default();
                    let sanitized = sanitize_provider_error(&text);
                    last_error = Some(SeraError::External(format!(
                        "LLM request failed with status {}: {sanitized}",
                        status.as_u16()
                    )));
                    if !retryable || attempt == self.max_retries {
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(500 * (1 << attempt))).await;
                }
                Err(error) => {
                    last_error = Some(SeraError::External(format!("LLM request failed: {error}")));
                    if attempt == self.max_retries {
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(500 * (1 << attempt))).await;
                }
            }
        }
        Err(last_error.unwrap_or_else(|| SeraError::External("LLM request failed".into())))
    }

    pub async fn list_models(&self) -> Result<std::collections::BTreeSet<String>> {
        self.fetch_model_catalog(&format!("{}/models", self.base_url), false)
            .await
    }

    pub(crate) async fn fetch_model_catalog(
        &self,
        catalog_url: &str,
        strict: bool,
    ) -> Result<std::collections::BTreeSet<String>> {
        if url::Url::parse(catalog_url)
            .ok()
            .is_some_and(|url| url.path().ends_with(":fetchAvailableModels"))
        {
            return self.fetch_cloud_code_catalog(catalog_url).await;
        }
        let url = validate_base_url(catalog_url)?;
        let mut refreshed = false;
        let mut last_error = None;
        for attempt in 0..=self.max_retries + 1 {
            let mut key = self.bearer();
            let response = self
                .http
                .get(&url)
                .bearer_auth(&key)
                .header("user-agent", USER_AGENT)
                .header("accept", "application/json")
                .send()
                .await;
            key.zeroize();
            match response {
                Ok(response) => {
                    let status = response.status();
                    if status.as_u16() == 401 && self.oauth.is_some() && !refreshed {
                        refreshed = true;
                        self.refresh_oauth().await?;
                        continue;
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
                        return if strict {
                            parse_official_catalog(&bytes)
                        } else {
                            parse_model_ids(&bytes)
                        };
                    }
                    // Drop the body. Provider errors can echo credentials; the status is enough.
                    let _ = response.bytes().await;
                    let retryable = status.as_u16() == 429 || status.is_server_error();
                    last_error = Some(SeraError::External(format!(
                        "model catalog failed with status {}",
                        status.as_u16()
                    )));
                    if !retryable || attempt == self.max_retries {
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(200 * (1 << attempt))).await;
                }
                Err(_) => {
                    last_error = Some(SeraError::External("model catalog request failed".into()));
                    if attempt == self.max_retries {
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(200 * (1 << attempt))).await;
                }
            }
        }
        Err(last_error
            .unwrap_or_else(|| SeraError::External("model catalog request failed".into())))
    }

    async fn refresh_oauth(&self) -> Result<()> {
        let oauth = self
            .oauth
            .as_ref()
            .ok_or_else(|| SeraError::External("oauth refresh is not configured".into()))?;
        let token = match oauth.style {
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
        let (secrets, refresh_name, access_name) = {
            let persistence = oauth
                .persistence
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            let Some(persistence) = persistence.as_ref() else {
                return Err(SeraError::Secrets(
                    "rotated oauth refresh token was not persisted".into(),
                ));
            };
            let refresh_name = persistence.refresh_secret_name.trim().to_owned();
            if refresh_name.is_empty() {
                return Err(SeraError::Secrets(
                    "rotated oauth refresh token was not persisted".into(),
                ));
            }
            (
                persistence.secrets.clone(),
                refresh_name,
                persistence.access_secret_name.trim().to_owned(),
            )
        };
        let mut updates = vec![(refresh_name.as_str(), refresh)];
        if !access_name.is_empty() && access_name != refresh_name {
            updates.push((access_name.as_str(), access));
        }
        match secrets.persist(&updates) {
            Ok(()) => Ok(()),
            Err(error) => Err(redact_secret_error(error, &[refresh, access])),
        }
    }

    async fn complete_cloud_code(
        &self,
        messages: &[LlmMessage],
        tools: &[ToolSpec],
        model: &str,
        temperature: Option<f32>,
    ) -> Result<LlmResponse> {
        let project = self.cloud_code.clone().unwrap_or_default();
        let targets = cloud_code::chat_targets(&self.base_url)?;
        let mut refreshed = false;
        let mut last_status = None;
        for _ in 0..2 {
            let mut restart = false;
            for target in &targets {
                let mut session_bumped = false;
                loop {
                    let envelope = cloud_code::build_cloud_code_envelope(
                        model,
                        &project,
                        messages,
                        tools,
                        self.max_output_tokens,
                        temperature,
                    )?;
                    match self
                        .post_cloud(&target.stream, &envelope, model, true)
                        .await?
                    {
                        CloudPost::Ready(bytes) => {
                            return cloud_completion(cloud_code::parse_cloud_code_completion(
                                &bytes,
                            )?);
                        }
                        CloudPost::Unauthorized if !refreshed && self.oauth.is_some() => {
                            refreshed = true;
                            self.refresh_oauth().await?;
                            restart = true;
                            break;
                        }
                        CloudPost::SessionLimited if !session_bumped => {
                            session_bumped = true;
                            continue;
                        }
                        CloudPost::NotFound => {
                            match self
                                .post_cloud(&target.generate, &envelope, model, false)
                                .await?
                            {
                                CloudPost::Ready(bytes) => {
                                    return cloud_completion(
                                        cloud_code::parse_cloud_code_completion(&bytes)?,
                                    );
                                }
                                CloudPost::Unauthorized if !refreshed && self.oauth.is_some() => {
                                    refreshed = true;
                                    self.refresh_oauth().await?;
                                    restart = true;
                                    break;
                                }
                                CloudPost::SessionLimited if !session_bumped => {
                                    session_bumped = true;
                                    continue;
                                }
                                CloudPost::NotFound => {
                                    last_status = Some(404);
                                    break;
                                }
                                CloudPost::Retryable(status) => {
                                    last_status = Some(status);
                                    break;
                                }
                                CloudPost::Failed(message) => {
                                    return Err(SeraError::External(message));
                                }
                                CloudPost::Unauthorized => {
                                    return Err(SeraError::External(
                                        "Cloud Code request failed with status 401".into(),
                                    ));
                                }
                                CloudPost::SessionLimited | CloudPost::Transport => break,
                            }
                        }
                        CloudPost::Retryable(status) => {
                            last_status = Some(status);
                            break;
                        }
                        CloudPost::Failed(message) => return Err(SeraError::External(message)),
                        CloudPost::Unauthorized => {
                            return Err(SeraError::External(
                                "Cloud Code request failed with status 401".into(),
                            ));
                        }
                        CloudPost::SessionLimited | CloudPost::Transport => break,
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
        Err(match last_status {
            Some(status) => {
                SeraError::External(format!("Cloud Code request failed with status {status}"))
            }
            None => SeraError::External("Cloud Code request failed".into()),
        })
    }

    async fn fetch_cloud_code_catalog(
        &self,
        catalog_url: &str,
    ) -> Result<std::collections::BTreeSet<String>> {
        let urls = cloud_code::catalog_urls(catalog_url)?;
        let project = self.cloud_code.clone().unwrap_or_default();
        let body = if project.is_empty() {
            json!({})
        } else {
            json!({"project": project})
        };
        let mut refreshed = false;
        let mut last_status = None;
        for _ in 0..2 {
            let mut restart = false;
            for url in &urls {
                match self.post_cloud(url, &body, "", false).await? {
                    CloudPost::Ready(bytes) => return cloud_code::parse_cloud_code_catalog(&bytes),
                    CloudPost::Unauthorized if !refreshed && self.oauth.is_some() => {
                        refreshed = true;
                        self.refresh_oauth().await?;
                        restart = true;
                        break;
                    }
                    CloudPost::Unauthorized => {
                        return Err(SeraError::External(
                            "model catalog failed with status 401".into(),
                        ));
                    }
                    CloudPost::NotFound => last_status = Some(404),
                    CloudPost::Retryable(status) => last_status = Some(status),
                    CloudPost::SessionLimited => last_status = Some(400),
                    CloudPost::Failed(message) => return Err(SeraError::External(message)),
                    CloudPost::Transport => {}
                }
            }
            if !restart {
                break;
            }
        }
        Err(match last_status {
            Some(status) => {
                SeraError::External(format!("model catalog failed with status {status}"))
            }
            None => SeraError::External("model catalog request failed".into()),
        })
    }

    async fn post_cloud(
        &self,
        url: &str,
        body: &Value,
        model: &str,
        stream: bool,
    ) -> Result<CloudPost> {
        let mut key = self.bearer();
        let mut request = self
            .http
            .post(url)
            .bearer_auth(&key)
            .header("user-agent", CLOUD_CODE_USER_AGENT)
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
        if cloud_code::requests_interleaved_thinking(model) {
            request = request.header("anthropic-beta", "interleaved-thinking-2025-05-14");
        }
        let response = request.send().await;
        key.zeroize();
        let response = match response {
            Ok(response) => response,
            Err(_) => return Ok(CloudPost::Transport),
        };
        Ok(classify_cloud(response, stream).await)
    }
}

enum CloudPost {
    Ready(Vec<u8>),
    Unauthorized,
    NotFound,
    SessionLimited,
    Retryable(u16),
    Failed(String),
    Transport,
}

async fn classify_cloud(response: reqwest::Response, allow_session: bool) -> CloudPost {
    let status = response.status().as_u16();
    if status == 200 {
        if response.content_length().is_some_and(|len| len > 4_194_304) {
            return CloudPost::Failed("Cloud Code response is too large".into());
        }
        return match response.bytes().await {
            Ok(bytes) if bytes.len() > 4_194_304 => {
                CloudPost::Failed("Cloud Code response is too large".into())
            }
            Ok(bytes) => CloudPost::Ready(bytes.to_vec()),
            Err(_) => CloudPost::Transport,
        };
    }
    if status == 401 {
        return CloudPost::Unauthorized;
    }
    if status == 404 {
        return CloudPost::NotFound;
    }
    if status == 400 && allow_session {
        let bytes = response.bytes().await.unwrap_or_default();
        let limited = bytes.len().min(65_536);
        let text = String::from_utf8_lossy(&bytes[..limited]);
        if text
            .to_ascii_lowercase()
            .contains("exceeds the maximum number of tokens")
        {
            return CloudPost::SessionLimited;
        }
        return CloudPost::Failed("Cloud Code request failed with status 400".into());
    }
    if status == 403 || status == 429 || status >= 500 {
        return CloudPost::Retryable(status);
    }
    CloudPost::Failed(format!("Cloud Code request failed with status {status}"))
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
        })
        .collect::<Vec<_>>();
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
        },
        usage: Usage {
            prompt_tokens: parsed.prompt_tokens,
            completion_tokens: parsed.completion_tokens,
            total_tokens: parsed
                .prompt_tokens
                .saturating_add(parsed.completion_tokens),
        },
        finish_reason: parsed.finish_reason,
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

fn message_to_json(message: &LlmMessage) -> Value {
    let mut value = json!({
        "role": message.role.as_str(),
        "content": message.content,
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
    let content = message
        .get("content")
        .and_then(Value::as_str)
        .map(str::to_owned);
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
        .and_then(|usage| serde_json::from_value(usage.clone()).ok())
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
        },
        usage,
        finish_reason,
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
            .and(header("user-agent", CLOUD_CODE_USER_AGENT))
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
