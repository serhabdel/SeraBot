//! The HTTP side of the non-chat-completions transports: the shared request
//! loop (credential, 401 refresh, retry, typed failures, one 400 repair), the
//! Responses and Messages completions built on it, and the Anthropic model
//! list.
//!
//! The pure halves (request bodies, stream parsers) are in `crate::wire`.

use serde_json::{Map, Value};
use sha2::{Digest, Sha256};

use super::*;
use crate::wire::{anthropic, devin, kiro, responses};

/// Largest completion body read. Streams are buffered whole.
const MAX_WIRE_BODY: usize = 16 * 1024 * 1024;
/// Most of an error body read; it is inspected for a code, never shown.
const MAX_ERROR_BODY: usize = 64 * 1024;
/// Pages of the Anthropic model list followed.
const MAX_CATALOG_PAGES: usize = 10;

const BASE62: &[u8; 62] = b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz";

/// `ses_` plus 12 hex and 14 base62 characters, derived from `basis` (a
/// conversation id, or a per-client seed). The same basis always gives the
/// same id, so a retry reuses it.
pub(crate) fn opencode_session_id(basis: &str) -> String {
    let digest = Sha256::digest(format!("opencode-session:{basis}").as_bytes());
    let hex_part = hex::encode(&digest[..6]);
    let tail: String = digest[6..20]
        .iter()
        .map(|byte| BASE62[usize::from(*byte) % 62] as char)
        .collect();
    format!("ses_{hex_part}{tail}")
}

/// How a request proves who sent it.
#[derive(Clone, Copy)]
pub(super) enum WireAuth {
    Bearer,
    /// Anthropic's `x-api-key` header.
    ApiKey,
}

/// A repair for a 400: edit the body from the provider's error text and say
/// whether anything changed.
pub(super) type BadRequestFix<'a> = &'a (dyn Fn(&mut Value, &str) -> bool + Send + Sync);

pub(super) struct WireCall<'a> {
    pub method: reqwest::Method,
    pub url: String,
    pub query: Vec<(&'static str, String)>,
    pub auth: WireAuth,
    pub headers: Vec<(&'static str, String)>,
    pub body: Option<Value>,
    pub fix_bad_request: Option<BadRequestFix<'a>>,
    /// Whether the provider's own error message may appear in the failure.
    pub echo_error: bool,
    /// Whether repeating the request is harmless even after a timeout.
    pub idempotent: bool,
}

async fn read_limited(mut response: reqwest::Response, limit: usize) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    loop {
        match response.chunk().await {
            Ok(Some(chunk)) => {
                if out.len() + chunk.len() > limit {
                    return None;
                }
                out.extend_from_slice(&chunk);
            }
            Ok(None) => return Some(out),
            Err(_) => return None,
        }
    }
}

/// An error body, cut at [`MAX_ERROR_BODY`] rather than refused.
async fn read_error_body(mut response: reqwest::Response) -> String {
    let mut out = Vec::new();
    while out.len() < MAX_ERROR_BODY {
        match response.chunk().await {
            Ok(Some(chunk)) => out.extend_from_slice(&chunk),
            _ => break,
        }
    }
    llm_failure::bounded_body(&out)
}

fn is_anthropic_oauth_gate(body: &str) -> bool {
    let lower = body.to_ascii_lowercase();
    lower.contains("claude code") || lower.contains("only authorized") || lower.contains("oauth")
}

/// The reasoning text a reply carried, bounded and stripped of control
/// characters. Untrusted display data.
fn bounded_reasoning(text: &str) -> Option<String> {
    let text: String = text
        .chars()
        .filter(|c| !c.is_control() || matches!(c, '\n' | '\t'))
        .take(crate::thinking::MAX_REASONING_CHARS)
        .collect();
    let text = text.trim().to_owned();
    (!text.is_empty()).then_some(text)
}

impl LlmClient {
    /// Send one wire request and return the whole response body.
    pub(super) async fn send_wire(&self, mut call: WireCall<'_>) -> Result<Vec<u8>> {
        let mut last_error = None;
        let mut refreshed = false;
        let mut repaired = false;
        for attempt in 0..=self.max_retries + 1 {
            let mut key = self.bearer();
            let used = zeroize::Zeroizing::new(key.clone());
            let mut request = self.http.request(call.method.clone(), &call.url);
            request = match call.auth {
                WireAuth::Bearer => request.bearer_auth(&key),
                WireAuth::ApiKey => request.header("x-api-key", &key),
            };
            key.zeroize();
            for (name, value) in &call.headers {
                request = request.header(*name, value);
            }
            if !call.query.is_empty() {
                request = request.query(&call.query);
            }
            if let Some(body) = &call.body {
                if !call
                    .headers
                    .iter()
                    .any(|(name, _)| name.eq_ignore_ascii_case("accept"))
                {
                    request = request.header("accept", "text/event-stream");
                }
                request = request.json(body);
            } else {
                if !call
                    .headers
                    .iter()
                    .any(|(name, _)| name.eq_ignore_ascii_case("user-agent"))
                {
                    request = request.header("user-agent", USER_AGENT);
                }
                request = request.header("accept", "application/json");
            }
            let response = self.decorate(request).send().await;
            match response {
                Ok(response) => {
                    let status = response.status();
                    if status.as_u16() == 401 && self.oauth.is_some() && !refreshed {
                        refreshed = true;
                        self.refresh_oauth(&used).await?;
                        continue;
                    }
                    if status.as_u16() == 401 && (self.oauth.is_none() || refreshed) {
                        self.park_for_reauth();
                    }
                    if status.is_success() {
                        return read_limited(response, MAX_WIRE_BODY).await.ok_or_else(|| {
                            llm_failure::transport_error("LLM response read failed".into())
                        });
                    }
                    let hint = retry::retry_after(response.headers());
                    let hold = llm_failure::retry_after_hold(response.headers());
                    let text = read_error_body(response).await;
                    if status.as_u16() == 400
                        && !repaired
                        && let (Some(fix), Some(body)) = (call.fix_bad_request, call.body.as_mut())
                        && fix(body, &text)
                    {
                        repaired = true;
                        continue;
                    }
                    let message = if self.wire == Wire::AnthropicPlan
                        && matches!(status.as_u16(), 401 | 403)
                        && is_anthropic_oauth_gate(&text)
                    {
                        format!(
                            "Anthropic rejected the Claude plan OAuth token (HTTP {}): {}. Use a Console API key (sk-ant-api03-...) if OAuth is blocked.",
                            status.as_u16(),
                            sanitize_provider_error(&text)
                        )
                    } else if call.echo_error {
                        format!(
                            "LLM request failed with status {}: {}",
                            status.as_u16(),
                            sanitize_provider_error(&text)
                        )
                    } else {
                        format!("model catalog failed with status {}", status.as_u16())
                    };
                    let error = llm_failure::upstream_error(status.as_u16(), &text, hold, message);
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
                    let reason = crate::llm::transport_reason(&error);
                    last_error = Some(llm_failure::transport_error(format!(
                        "LLM request failed ({reason})"
                    )));
                    if !retry::transport_retryable(&error, call.idempotent)
                        || attempt == self.max_retries
                    {
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

    /// The Codex session id: the running conversation's (so every request of
    /// one conversation hits one cache), else this client's seed. Only header-
    /// and key-safe characters are kept.
    fn codex_session(&self) -> Option<String> {
        let codex = self.codex.as_ref()?;
        let conversation: String = current_conversation()
            .chars()
            .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'))
            .take(64)
            .collect();
        Some(if conversation.is_empty() {
            codex.seed.clone()
        } else {
            conversation
        })
    }

    /// The headers a Codex request adds to the bearer.
    fn codex_headers(&self, session: &str) -> Vec<(&'static str, String)> {
        let Some(codex) = self.codex.as_ref() else {
            return Vec::new();
        };
        let mut headers = vec![
            (
                "OpenAI-Beta",
                crate::auth_flows::codex::OPENAI_BETA.to_owned(),
            ),
            (
                "originator",
                crate::auth_flows::codex::ORIGINATOR.to_owned(),
            ),
            ("session_id", session.to_owned()),
        ];
        if let Some(account) = &codex.account_id {
            headers.push(("ChatGPT-Account-Id", account.clone()));
        }
        headers
    }

    /// One completion on the Responses transport (Meta Muse Code, Codex).
    pub(super) async fn complete_responses(
        &self,
        messages: &[LlmMessage],
        tools: &[ToolSpec],
        model: &str,
        extra: Option<&Map<String, Value>>,
    ) -> Result<LlmResponse> {
        let session = self.codex_session();
        let mut body = responses::build_request(
            messages,
            tools,
            &responses::RequestOptions {
                model,
                max_output_tokens: self.max_output_tokens,
                extra,
                web_search: false,
                prompt_cache_key: session.as_deref(),
            },
        );
        // The ChatGPT backend refuses a request with no `instructions`.
        if session.is_some() && body.get("instructions").is_none() {
            body["instructions"] = Value::String("You are a helpful assistant.".into());
        }
        let fix = |body: &mut Value, text: &str| responses::strip_for_bad_request(body, text);
        let bytes = self
            .send_wire(WireCall {
                method: reqwest::Method::POST,
                url: format!("{}/responses", self.base_url),
                query: Vec::new(),
                auth: WireAuth::Bearer,
                headers: session
                    .as_deref()
                    .map(|session| self.codex_headers(session))
                    .unwrap_or_default(),
                body: Some(body),
                fix_bad_request: Some(&fix),
                echo_error: true,
                idempotent: false,
            })
            .await?;
        let parsed = responses::parse_stream(&bytes)?;
        Ok(LlmResponse {
            message: LlmMessage {
                role: Role::Assistant,
                content: (!parsed.text.trim().is_empty()).then_some(parsed.text),
                tool_calls: parsed.tool_calls,
                tool_call_id: None,
                name: None,
                is_error: false,
                images: Vec::new(),
            },
            usage: parsed.usage,
            finish_reason: parsed.finish_reason,
            reasoning: bounded_reasoning(&parsed.reasoning),
        })
    }

    fn anthropic_headers(&self) -> Vec<(&'static str, String)> {
        let mut headers = vec![("anthropic-version", anthropic::API_VERSION.to_owned())];
        if self.wire == Wire::AnthropicPlan {
            headers.push(("anthropic-beta", anthropic::OAUTH_BETA.to_owned()));
            headers.push((
                "user-agent",
                crate::auth_flows::claude::PLAN_USER_AGENT.to_owned(),
            ));
        }
        headers
    }

    fn anthropic_auth(&self) -> WireAuth {
        if self.wire == Wire::AnthropicPlan {
            WireAuth::Bearer
        } else {
            WireAuth::ApiKey
        }
    }

    /// One completion on the native Messages transport.
    pub(super) async fn complete_anthropic(
        &self,
        messages: &[LlmMessage],
        tools: &[ToolSpec],
        model: &str,
        temperature: Option<f32>,
        extra: Option<&Map<String, Value>>,
    ) -> Result<LlmResponse> {
        let body = anthropic::build_request(
            messages,
            tools,
            &anthropic::RequestOptions {
                model,
                max_tokens: self.max_output_tokens,
                temperature,
                extra,
            },
        );
        let fix = |body: &mut Value, text: &str| anthropic::strip_for_bad_request(body, text);
        let bytes = self
            .send_wire(WireCall {
                method: reqwest::Method::POST,
                url: format!("{}/messages", self.base_url),
                query: Vec::new(),
                auth: self.anthropic_auth(),
                headers: self.anthropic_headers(),
                body: Some(body),
                fix_bad_request: Some(&fix),
                echo_error: true,
                idempotent: false,
            })
            .await?;
        let mut parsed = anthropic::parse_stream(&bytes)?;
        anthropic::restore_tool_names(&mut parsed.tool_calls, tools);
        Ok(LlmResponse {
            message: LlmMessage {
                role: Role::Assistant,
                content: (!parsed.text.trim().is_empty()).then_some(parsed.text),
                tool_calls: parsed.tool_calls,
                tool_call_id: None,
                name: None,
                is_error: false,
                images: Vec::new(),
            },
            usage: parsed.usage,
            finish_reason: parsed.finish_reason,
            reasoning: bounded_reasoning(&parsed.reasoning),
        })
    }

    /// The Anthropic model list, following its cursor.
    pub(super) async fn fetch_anthropic_catalog(&self, catalog_url: &str) -> Result<ModelCatalog> {
        let url = validate_base_url(catalog_url)?;
        let mut catalog = ModelCatalog::new();
        let mut after: Option<String> = None;
        for _ in 0..MAX_CATALOG_PAGES {
            let mut query = vec![("limit", "1000".to_owned())];
            if let Some(cursor) = &after {
                query.push(("after_id", cursor.clone()));
            }
            let bytes = self
                .send_wire(WireCall {
                    method: reqwest::Method::GET,
                    url: url.clone(),
                    query,
                    auth: self.anthropic_auth(),
                    headers: self.anthropic_headers(),
                    body: None,
                    fix_bad_request: None,
                    echo_error: false,
                    idempotent: true,
                })
                .await?;
            let page = anthropic::parse_catalog_page(&bytes)?;
            for (id, meta) in page.models {
                catalog.insert(id, meta);
            }
            if catalog.len() > 4_000 {
                return Err(SeraError::External("model catalog is too large".into()));
            }
            match page.next_after {
                Some(next) if after.as_deref() != Some(next.as_str()) => after = Some(next),
                _ => break,
            }
        }
        if catalog.is_empty() {
            return Err(SeraError::External(
                "model catalog contained no usable model ids".into(),
            ));
        }
        Ok(catalog)
    }

    /// One completion on the local loopback Claude Agent SDK gateway (`POST {base}/v1/turn`).
    pub(super) async fn complete_claude_gateway(
        &self,
        messages: &[LlmMessage],
        tools: &[ToolSpec],
        model: &str,
        extra: Option<&Map<String, Value>>,
    ) -> Result<LlmResponse> {
        let base = crate::config::validate_loopback_gateway_url(&self.base_url)?;
        let root = base.strip_suffix("/v1").unwrap_or(&base);
        let system = messages
            .iter()
            .filter(|m| m.role == Role::System)
            .filter_map(|m| m.content.as_deref())
            .filter(|text| !text.trim().is_empty())
            .collect::<Vec<_>>()
            .join("\n\n");
        let non_system: Vec<LlmMessage> = messages
            .iter()
            .filter(|m| m.role != Role::System)
            .cloned()
            .collect();
        let tools_json: Vec<Value> = tools
            .iter()
            .map(|tool| {
                json!({
                    "name": tool.name,
                    "description": tool.description,
                    "parameters": tool.parameters,
                })
            })
            .collect();
        let mut body = json!({
            "model": model,
            "system": system,
            "messages": messages_to_json(&non_system),
            "tools": tools_json,
            "max_tokens": self.max_output_tokens,
        });
        let mut headers = vec![("accept", "application/json".to_owned())];
        if let Some(profile) = &self.claude_profile {
            headers.push(("X-Claude-Profile", profile.clone()));
            body["profile"] = json!(profile);
        }
        if let Some(effort) = Self::claude_gateway_effort(extra) {
            body["effort"] = json!(effort);
        }
        let bytes = self
            .send_wire(WireCall {
                method: reqwest::Method::POST,
                url: format!("{root}/v1/turn"),
                query: Vec::new(),
                auth: WireAuth::Bearer,
                headers,
                body: Some(body),
                fix_bad_request: None,
                echo_error: true,
                idempotent: false,
            })
            .await?;
        parse_claude_gateway_response(&bytes)
    }

    /// Extract an `effort` level (`"low"`, `"medium"`, `"high"`, `"max"`) from
    /// thinking extra fields for the Claude Agent SDK gateway.
    fn claude_gateway_effort(extra: Option<&Map<String, Value>>) -> Option<&'static str> {
        let fields = extra?;
        if let Some(level) = fields
            .get("effort")
            .or_else(|| fields.get("thinking_level"))
            .or_else(|| fields.get("thinkingLevel"))
            .or_else(|| fields.get("reasoning").and_then(|r| r.get("effort")))
            .and_then(Value::as_str)
        {
            return match level {
                "low" => Some("low"),
                "medium" => Some("medium"),
                "high" => Some("high"),
                "xhigh" | "max" => Some("max"),
                _ => None,
            };
        }
        let thinking = fields.get("thinking")?;
        if thinking.get("type").and_then(Value::as_str) != Some("enabled") {
            return None;
        }
        let budget = thinking.get("budget_tokens").and_then(Value::as_u64)?;
        Some(if budget <= 4_000 {
            "low"
        } else if budget <= 10_000 {
            "medium"
        } else if budget <= 24_000 {
            "high"
        } else {
            "max"
        })
    }

    /// One completion on the AWS Kiro EventStream transport.
    pub(super) async fn complete_kiro(
        &self,
        messages: &[LlmMessage],
        tools: &[ToolSpec],
        model: &str,
        extra: Option<&Map<String, Value>>,
    ) -> Result<LlmResponse> {
        let thinking_level = extra
            .and_then(|fields| {
                fields
                    .get("thinking_level")
                    .or_else(|| fields.get("thinkingLevel"))
            })
            .and_then(Value::as_str);
        let body = kiro::build_generate_request(model, None, messages, tools, thinking_level);
        let url = format!("{}/generateAssistantResponse", self.base_url);
        let bytes = self
            .send_wire(WireCall {
                method: reqwest::Method::POST,
                url,
                query: Vec::new(),
                auth: WireAuth::Bearer,
                headers: vec![("accept", "application/vnd.amazon.eventstream".to_owned())],
                body: Some(body),
                fix_bad_request: None,
                echo_error: true,
                idempotent: false,
            })
            .await?;
        let parsed = kiro::parse_event_stream(&bytes)?;
        let finish_reason = Some(if parsed.tool_calls.is_empty() {
            "stop".to_owned()
        } else {
            "tool_calls".to_owned()
        });
        Ok(LlmResponse {
            message: LlmMessage {
                role: Role::Assistant,
                content: (!parsed.content.trim().is_empty()).then_some(parsed.content),
                tool_calls: parsed.tool_calls,
                tool_call_id: None,
                name: None,
                is_error: false,
                images: Vec::new(),
            },
            usage: parsed.usage,
            finish_reason,
            reasoning: parsed.reasoning.as_deref().and_then(bounded_reasoning),
        })
    }

    /// AWS Kiro `ListAvailableModels` catalog.
    pub(super) async fn fetch_kiro_catalog(&self, catalog_url: &str) -> Result<ModelCatalog> {
        let raw = catalog_url
            .strip_suffix("?origin=AI_EDITOR")
            .unwrap_or(catalog_url);
        let url = validate_base_url(raw)?;
        let bytes = self
            .send_wire(WireCall {
                method: reqwest::Method::GET,
                url,
                query: vec![("origin", "AI_EDITOR".to_owned())],
                auth: WireAuth::Bearer,
                headers: Vec::new(),
                body: None,
                fix_bad_request: None,
                echo_error: false,
                idempotent: true,
            })
            .await?;
        let catalog = kiro::parse_models_catalog(&bytes)?;
        if catalog.is_empty() {
            return Err(SeraError::External(
                "model catalog contained no usable model ids".into(),
            ));
        }
        Ok(catalog)
    }

    /// One completion on the Devin CLI / Codeium Connect-RPC transport.
    pub(super) async fn complete_devin(
        &self,
        messages: &[LlmMessage],
        tools: &[ToolSpec],
        model: &str,
        extra: Option<&Map<String, Value>>,
    ) -> Result<LlmResponse> {
        let thinking_level = extra
            .and_then(|fields| {
                fields
                    .get("thinking_level")
                    .or_else(|| fields.get("thinkingConfig").and_then(|c| c.get("effort")))
            })
            .and_then(Value::as_str);
        let body = devin::build_chat_request(model, "", messages, tools, thinking_level);
        let url = devin::rpc_url(&self.base_url, devin::CHAT_RPC_PATH)?;
        let bytes = self
            .send_wire(WireCall {
                method: reqwest::Method::POST,
                url,
                query: Vec::new(),
                auth: WireAuth::Bearer,
                headers: vec![
                    ("connect-protocol-version", "1".to_owned()),
                    ("accept", "application/json".to_owned()),
                ],
                body: Some(body),
                fix_bad_request: None,
                echo_error: true,
                idempotent: false,
            })
            .await?;
        let parsed = devin::parse_chat_response(&bytes)?;
        Ok(LlmResponse {
            message: LlmMessage {
                role: Role::Assistant,
                content: (!parsed.text.trim().is_empty()).then_some(parsed.text),
                tool_calls: parsed.tool_calls,
                tool_call_id: None,
                name: None,
                is_error: false,
                images: Vec::new(),
            },
            usage: parsed.usage,
            finish_reason: parsed.finish_reason,
            reasoning: bounded_reasoning(&parsed.reasoning),
        })
    }

    /// Devin CLI / Codeium `GetCliModelConfigs` catalog.
    pub(super) async fn fetch_devin_catalog(&self, catalog_url: &str) -> Result<ModelCatalog> {
        let url = if catalog_url.ends_with(devin::MODELS_RPC_PATH) {
            let base = catalog_url
                .strip_suffix(devin::MODELS_RPC_PATH)
                .unwrap_or(catalog_url);
            devin::rpc_url(base, devin::MODELS_RPC_PATH)?
        } else {
            devin::rpc_url(&self.base_url, devin::MODELS_RPC_PATH)?
        };
        let bytes = self
            .send_wire(WireCall {
                method: reqwest::Method::POST,
                url,
                query: Vec::new(),
                auth: WireAuth::Bearer,
                headers: vec![
                    ("connect-protocol-version", "1".to_owned()),
                    ("accept", "application/json".to_owned()),
                ],
                body: Some(serde_json::json!({})),
                fix_bad_request: None,
                echo_error: false,
                idempotent: true,
            })
            .await?;
        let models = devin::parse_models_catalog(&bytes)?;
        let mut catalog = ModelCatalog::new();
        for model in models {
            catalog.insert(
                model.id,
                crate::providers::ModelMeta {
                    name: model.name,
                    context_length: model.context_length,
                    max_output_tokens: model.max_output_tokens,
                    tags: model.tags,
                },
            );
        }
        if catalog.is_empty() {
            return Err(SeraError::External(
                "model catalog contained no usable model ids".into(),
            ));
        }
        Ok(catalog)
    }
}

fn parse_claude_gateway_response(bytes: &[u8]) -> Result<LlmResponse> {
    let value: Value = serde_json::from_slice(bytes).map_err(|error| {
        SeraError::External(format!("malformed claude-gateway response: {error}"))
    })?;
    let content = value
        .get("content")
        .and_then(Value::as_str)
        .filter(|text| !text.trim().is_empty())
        .map(str::to_owned);
    let mut tool_calls = Vec::new();
    if let Some(calls) = value.get("tool_calls").and_then(Value::as_array) {
        for (idx, call) in calls.iter().enumerate() {
            let id = call
                .get("id")
                .and_then(Value::as_str)
                .filter(|id| !id.trim().is_empty())
                .map(str::to_owned)
                .unwrap_or_else(|| format!("call_{idx}"));
            let (name, raw_args) = if let Some(func) = call.get("function") {
                (
                    func.get("name").and_then(Value::as_str).unwrap_or_default(),
                    func.get("arguments"),
                )
            } else {
                (
                    call.get("name").and_then(Value::as_str).unwrap_or_default(),
                    call.get("arguments"),
                )
            };
            if name.trim().is_empty() {
                continue;
            }
            let arguments = match raw_args {
                Some(Value::String(text)) => {
                    serde_json::from_str(text).unwrap_or_else(|_| json!({}))
                }
                Some(other) if !other.is_null() => other.clone(),
                _ => json!({}),
            };
            tool_calls.push(ToolCall {
                id,
                name: name.to_owned(),
                arguments,
                reasoning_signature: None,
            });
        }
    }
    let usage = match value.get("usage") {
        Some(u) if !u.is_null() => {
            let get = |key: &str| u.get(key).and_then(Value::as_u64).unwrap_or(0);
            let prompt = get("prompt_tokens").max(get("input_tokens"));
            let completion = get("completion_tokens").max(get("output_tokens"));
            let cached = get("cached_tokens")
                .max(get("cache_read_input_tokens"))
                .min(prompt);
            let total = get("total_tokens").max(prompt.saturating_add(completion));
            Usage {
                prompt_tokens: prompt.saturating_sub(cached),
                completion_tokens: completion,
                total_tokens: total,
                cached_tokens: cached,
            }
        }
        _ => Usage::default(),
    };
    let finish_reason = value
        .get("finish_reason")
        .and_then(Value::as_str)
        .map(str::to_owned);
    let reasoning = value
        .get("reasoning")
        .and_then(Value::as_str)
        .and_then(bounded_reasoning);
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
