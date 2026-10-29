use std::sync::{Arc, Mutex};
use std::time::Duration;

use secrecy::{ExposeSecret, SecretString};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use zeroize::Zeroize;

use crate::providers::{
    OAuthMaterial, OAuthStyle, USER_AGENT, enforce_oauth_target, muse_mint_api_key,
    parse_model_ids, parse_official_catalog, refresh_token_grant,
};

use crate::domain::{LlmMessage, Role, ToolCall};
use crate::error::{Result, SeraError};
use crate::gateway::ToolSpec;

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

struct OAuthState {
    style: OAuthStyle,
    refresh_token: SecretString,
    token_url: String,
    client_id: String,
    client_secret: SecretString,
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
        })
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
            refresh_token: SecretString::new(material.refresh_token),
            token_url,
            client_id: material.client_id,
            client_secret: SecretString::new(material.client_secret),
        }));
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
        let model = model_override.unwrap_or(&self.model);
        if model.is_empty() {
            return Err(SeraError::Config("LLM model is not configured".into()));
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
                muse_mint_api_key(
                    &self.http,
                    &oauth.token_url,
                    oauth.refresh_token.expose_secret(),
                )
                .await?
            }
            OAuthStyle::RefreshToken => {
                refresh_token_grant(
                    &self.http,
                    &oauth.token_url,
                    oauth.refresh_token.expose_secret(),
                    &oauth.client_id,
                    oauth.client_secret.expose_secret(),
                )
                .await?
            }
        };
        *self
            .api_key
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = SecretString::new(token);
        Ok(())
    }
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
    if let Some(name) = &message.name {
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
    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

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
}
