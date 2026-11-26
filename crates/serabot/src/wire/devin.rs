//! Devin CLI / Codeium Connect-RPC JSON wire helpers.
//!
//! Endpoints:
//! - Chat: `POST {base}/exa.api_server_pb.ApiServerService/GetChatMessage`
//! - Catalog: `POST {base}/exa.api_server_pb.ApiServerService/GetCliModelConfigs`
//!
//! Allowed hosts are `server.codeium.com`, `api.devin.ai`, and exact loopback
//! (`http://127.0.0.1:<port>` / `http://localhost:<port>`) for tests or local
//! proxies.
//!
//! Supports both Connect-RPC 5-byte length-prefixed JSON streaming envelopes
//! (`application/connect+json`) and unary JSON (`application/json`).

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

use crate::domain::{LlmMessage, Role, ToolCall};
use crate::error::{Result, SeraError};
use crate::gateway::ToolSpec;
use crate::llm::Usage;
use crate::llm_failure;

use super::schema;

/// Default Codeium API server base URL.
pub const CODEIUM_BASE_URL: &str = "https://server.codeium.com";
/// Default Devin API base URL.
pub const DEVIN_BASE_URL: &str = "https://api.devin.ai";
/// Connect-RPC path for chat completions.
pub const CHAT_RPC_PATH: &str = "/exa.api_server_pb.ApiServerService/GetChatMessage";
/// Connect-RPC path for CLI model configuration catalog.
pub const MODELS_RPC_PATH: &str = "/exa.api_server_pb.ApiServerService/GetCliModelConfigs";

const CATALOG_MAX_BYTES: usize = 1_048_576;
const CATALOG_MAX_ENTRIES: usize = 4_000;
const RESPONSE_MAX_BYTES: usize = 16 * 1024 * 1024;

/// Build and validate a Connect-RPC URL for `server.codeium.com`, `api.devin.ai`,
/// or an exact loopback address (`127.0.0.1` / `localhost` with an explicit port).
pub fn rpc_url(base: &str, rpc_path: &str) -> Result<String> {
    let refuse = || {
        SeraError::Config(
            "Devin Connect-RPC base URL must be server.codeium.com, api.devin.ai, or loopback"
                .into(),
        )
    };
    let parsed = url::Url::parse(base.trim_end_matches('/')).map_err(|_| refuse())?;
    if !parsed.username().is_empty()
        || parsed.password().is_some()
        || parsed.query().is_some()
        || parsed.fragment().is_some()
    {
        return Err(refuse());
    }
    let host = parsed.host_str().unwrap_or("");
    let is_loopback = parsed.scheme() == "http"
        && matches!(host, "127.0.0.1" | "localhost")
        && parsed.port().is_some();
    let is_official = parsed.scheme() == "https"
        && (host.eq_ignore_ascii_case("server.codeium.com")
            || host.eq_ignore_ascii_case("api.devin.ai"));
    if !is_loopback && !is_official {
        return Err(refuse());
    }
    let base_str = parsed.as_str().trim_end_matches('/');
    let path = if rpc_path.starts_with('/') {
        rpc_path.to_owned()
    } else {
        format!("/{rpc_path}")
    };
    Ok(format!("{base_str}{path}"))
}

/// Parsed Connect-RPC response from `GetChatMessage`.
#[derive(Debug, Clone, Default)]
pub struct DevinResponse {
    pub text: String,
    pub reasoning: String,
    pub tool_calls: Vec<ToolCall>,
    pub usage: Usage,
    pub finish_reason: Option<String>,
}

/// One model entry returned by `GetCliModelConfigs`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CatalogModel {
    pub id: String,
    pub name: Option<String>,
    pub context_length: Option<u64>,
    pub max_output_tokens: Option<u64>,
    pub supports_tools: bool,
    pub supports_vision: bool,
    pub supports_reasoning: bool,
    pub tags: Vec<String>,
}

/// Build the Connect-RPC JSON request body for
/// `POST {base}/exa.api_server_pb.ApiServerService/GetChatMessage`.
pub fn build_chat_request(
    model: &str,
    system: &str,
    messages: &[LlmMessage],
    tools: &[ToolSpec],
    thinking_level: Option<&str>,
) -> Value {
    let mut system_parts: Vec<&str> = Vec::new();
    if !system.trim().is_empty() {
        system_parts.push(system.trim());
    }
    for msg in messages {
        if msg.role == Role::System
            && let Some(text) = msg.content.as_deref().map(str::trim)
            && !text.is_empty()
        {
            system_parts.push(text);
        }
    }
    let combined_system = system_parts.join("\n\n");

    let mut chat_messages: Vec<Value> = Vec::new();
    for msg in messages {
        match msg.role {
            Role::System => {}
            Role::User => {
                let mut entry = Map::new();
                entry.insert("source".into(), json!("CHAT_MESSAGE_SOURCE_USER"));
                if let Some(text) = msg.content.as_deref().filter(|t| !t.is_empty()) {
                    entry.insert("content".into(), json!(text));
                }
                if !msg.images.is_empty() {
                    let imgs: Vec<Value> = msg
                        .images
                        .iter()
                        .map(|img| {
                            json!({
                                "mimeType": img.mime,
                                "data": img.data_base64,
                            })
                        })
                        .collect();
                    entry.insert("images".into(), Value::Array(imgs));
                }
                if entry.contains_key("content") || entry.contains_key("images") {
                    chat_messages.push(Value::Object(entry));
                }
            }
            Role::Assistant => {
                let mut entry = Map::new();
                entry.insert("source".into(), json!("CHAT_MESSAGE_SOURCE_SYSTEM"));
                if let Some(text) = msg.content.as_deref().filter(|t| !t.is_empty()) {
                    entry.insert("content".into(), json!(text));
                }
                if !msg.tool_calls.is_empty() {
                    let calls: Vec<Value> = msg
                        .tool_calls
                        .iter()
                        .map(|call| {
                            json!({
                                "id": call.id,
                                "name": call.name,
                                "argumentsJson": serde_json::to_string(&call.arguments)
                                    .unwrap_or_else(|_| "{}".into()),
                            })
                        })
                        .collect();
                    entry.insert("toolCalls".into(), Value::Array(calls));
                }
                if entry.contains_key("content") || entry.contains_key("toolCalls") {
                    chat_messages.push(Value::Object(entry));
                }
            }
            Role::Tool => {
                let Some(call_id) = msg.tool_call_id.as_deref() else {
                    continue;
                };
                let mut result_obj = json!({
                    "toolCallId": call_id,
                    "result": msg.content.as_deref().unwrap_or_default(),
                    "isError": msg.is_error,
                });
                if let Some(name) = msg.name.as_deref().filter(|n| !n.is_empty()) {
                    result_obj["name"] = json!(name);
                }
                chat_messages.push(json!({
                    "source": "CHAT_MESSAGE_SOURCE_TOOL",
                    "toolCallResult": result_obj,
                }));
            }
        }
    }

    if chat_messages.is_empty() {
        chat_messages.push(json!({
            "source": "CHAT_MESSAGE_SOURCE_USER",
            "content": "...",
        }));
    }

    let mut body = json!({
        "metadata": {
            "ideName": "devin-cli",
            "ideVersion": "1.0.0",
        },
        "modelName": model,
        "chatMessages": chat_messages,
    });

    if !combined_system.is_empty() {
        body["systemPrompt"] = json!(combined_system);
    }

    if !tools.is_empty() {
        let wire_tools: Vec<Value> = tools
            .iter()
            .map(|tool| {
                json!({
                    "name": tool.name,
                    "description": tool.description,
                    "parameters": schema::flatten(&tool.parameters),
                })
            })
            .collect();
        body["tools"] = Value::Array(wire_tools);
    }

    if let Some(level) = thinking_level
        .map(str::trim)
        .filter(|l| !l.is_empty() && *l != "off")
    {
        body["thinkingConfig"] = json!({
            "enabled": true,
            "effort": level,
        });
    }

    body
}

fn connect_error_status(code: &str) -> u16 {
    match code.to_ascii_lowercase().as_str() {
        "unauthenticated" => 401,
        "permission_denied" => 403,
        "not_found" => 404,
        "resource_exhausted" => 429,
        "invalid_argument" | "failed_precondition" | "out_of_range" => 400,
        "deadline_exceeded" => 504,
        "unavailable" => 503,
        _ => 500,
    }
}

fn connect_error_to_sera(err: &Value) -> SeraError {
    let code: String = err
        .get("code")
        .and_then(Value::as_str)
        .unwrap_or("unknown")
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-'))
        .take(48)
        .collect();
    let status = connect_error_status(&code);
    llm_failure::upstream_error(
        status,
        &json!({"error": {"code": code}}).to_string(),
        None,
        format!("The model provider reported an error ({code})"),
    )
}

/// Unpack either Connect-RPC 5-byte length-prefixed frames, unary JSON, or
/// newline-delimited JSON into a sequence of JSON frames.
fn decode_connect_frames(bytes: &[u8]) -> Result<Vec<(u8, Value)>> {
    if bytes.is_empty() {
        return Err(SeraError::External(
            "empty response from Devin Connect-RPC endpoint".into(),
        ));
    }
    if bytes.len() > RESPONSE_MAX_BYTES {
        return Err(SeraError::External(
            "Devin Connect-RPC response exceeded size limit".into(),
        ));
    }

    // Unary JSON starts with `{` (after optional ASCII whitespace).
    let trimmed_start = bytes.iter().position(|b| !b.is_ascii_whitespace());
    if let Some(idx) = trimmed_start
        && bytes[idx] == b'{'
    {
        if let Ok(val) = serde_json::from_slice::<Value>(&bytes[idx..]) {
            return Ok(vec![(0x00, val)]);
        }
        // Fallback: NDJSON (one JSON object per line).
        let text = std::str::from_utf8(&bytes[idx..])
            .map_err(|_| SeraError::External("invalid UTF-8 in Connect-RPC response".into()))?;
        let mut frames = Vec::new();
        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            let val: Value = serde_json::from_str(line).map_err(|_| {
                SeraError::External("malformed JSON in Connect-RPC response".into())
            })?;
            frames.push((0x00, val));
        }
        if frames.is_empty() {
            return Err(SeraError::External(
                "empty JSON stream from Devin Connect-RPC endpoint".into(),
            ));
        }
        return Ok(frames);
    }

    // Connect-RPC streaming envelope: [flags: u8][length: u32 BE][payload: [u8; length]]
    let mut frames = Vec::new();
    let mut offset = 0usize;
    while offset < bytes.len() {
        if offset + 5 > bytes.len() {
            return Err(SeraError::External(
                "truncated Connect-RPC frame header".into(),
            ));
        }
        let flags = bytes[offset];
        let len = u32::from_be_bytes([
            bytes[offset + 1],
            bytes[offset + 2],
            bytes[offset + 3],
            bytes[offset + 4],
        ]) as usize;
        offset += 5;
        if flags & 0x01 != 0 {
            return Err(SeraError::External(
                "compressed Connect-RPC frames are not supported".into(),
            ));
        }
        if offset.checked_add(len).is_none_or(|end| end > bytes.len()) {
            return Err(SeraError::External(
                "truncated Connect-RPC frame payload".into(),
            ));
        }
        let payload = &bytes[offset..offset + len];
        offset += len;
        if payload.is_empty() && flags == 0x02 {
            frames.push((flags, Value::Null));
            continue;
        }
        let val: Value = serde_json::from_slice(payload)
            .map_err(|_| SeraError::External("malformed JSON in Connect-RPC frame".into()))?;
        frames.push((flags, val));
    }

    Ok(frames)
}

fn parse_tool_arguments(val: &Value) -> Value {
    match val {
        Value::Object(_) => val.clone(),
        Value::String(raw) => {
            let trimmed = raw.trim();
            if trimmed.is_empty() {
                json!({})
            } else {
                serde_json::from_str(trimmed).unwrap_or_else(|_| json!({"raw": raw}))
            }
        }
        _ => json!({}),
    }
}

fn extract_tool_calls(container: &Value, out: &mut Vec<ToolCall>) {
    let Some(calls) = container
        .get("toolCalls")
        .or_else(|| container.get("tool_calls"))
        .and_then(Value::as_array)
    else {
        return;
    };
    for call in calls {
        let Some(name) = call
            .get("name")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|n| !n.is_empty())
        else {
            continue;
        };
        let id = call
            .get("id")
            .or_else(|| call.get("toolCallId"))
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|id| !id.is_empty())
            .map(str::to_owned)
            .unwrap_or_else(|| format!("call_{}", out.len() + 1));
        let args_val = call
            .get("argumentsJson")
            .or_else(|| call.get("arguments_json"))
            .or_else(|| call.get("arguments"))
            .unwrap_or(&Value::Null);
        let arguments = parse_tool_arguments(args_val);
        out.push(ToolCall {
            id,
            name: name.to_owned(),
            arguments,
            reasoning_signature: None,
        });
    }
}

fn update_usage(container: &Value, usage: &mut Usage) {
    let Some(u) = container
        .get("usage")
        .or_else(|| container.get("tokenUsage"))
        .or_else(|| container.get("token_usage"))
    else {
        return;
    };
    let get_u64 = |keys: &[&str]| -> u64 {
        keys.iter()
            .find_map(|k| u.get(*k).and_then(Value::as_u64))
            .unwrap_or(0)
    };
    let prompt = get_u64(&[
        "inputTokens",
        "input_tokens",
        "promptTokens",
        "prompt_tokens",
    ]);
    let completion = get_u64(&[
        "outputTokens",
        "output_tokens",
        "completionTokens",
        "completion_tokens",
    ]);
    let cached = get_u64(&[
        "cachedTokens",
        "cached_tokens",
        "cacheReadInputTokens",
        "cache_read_input_tokens",
    ]);
    let total = get_u64(&["totalTokens", "total_tokens"])
        .max(prompt.saturating_add(completion).saturating_add(cached));

    usage.prompt_tokens = usage.prompt_tokens.max(prompt);
    usage.completion_tokens = usage.completion_tokens.max(completion);
    usage.cached_tokens = usage.cached_tokens.max(cached);
    usage.total_tokens = usage.total_tokens.max(total);
}

/// Parse a Connect-RPC `GetChatMessage` response (streaming frames or unary JSON).
pub fn parse_chat_response(bytes: &[u8]) -> Result<DevinResponse> {
    let frames = decode_connect_frames(bytes)?;
    let mut parsed = DevinResponse::default();

    for (flags, frame) in frames {
        // Connect-RPC end-of-stream trailer frame (`flags == 0x02`).
        if flags & 0x02 != 0 {
            if let Some(err) = frame.get("error").filter(|v| !v.is_null()) {
                return Err(connect_error_to_sera(err));
            }
            continue;
        }

        // Top-level Connect error in unary response (`{"code": "...", "message": "..."}`)
        // or nested `"error"` object.
        if let Some(err) = frame.get("error").filter(|v| v.is_object()) {
            return Err(connect_error_to_sera(err));
        }
        if frame.get("code").and_then(Value::as_str).is_some()
            && frame.get("chatMessage").is_none()
            && frame.get("content").is_none()
            && frame.get("text").is_none()
        {
            return Err(connect_error_to_sera(&frame));
        }

        let msg = frame
            .get("chatMessage")
            .or_else(|| frame.get("chat_message"))
            .or_else(|| frame.get("message"))
            .unwrap_or(&frame);

        if let Some(text) = msg
            .get("content")
            .or_else(|| msg.get("text"))
            .or_else(|| msg.pointer("/delta/text"))
            .or_else(|| msg.pointer("/intent/generic/text"))
            .and_then(Value::as_str)
        {
            parsed.text.push_str(text);
        }

        if let Some(thinking) = msg
            .get("thinking")
            .or_else(|| msg.get("reasoning"))
            .or_else(|| msg.get("reasoningContent"))
            .or_else(|| msg.get("reasoning_content"))
            .or_else(|| msg.get("thought"))
            .or_else(|| msg.pointer("/delta/thinking"))
            .and_then(Value::as_str)
        {
            parsed.reasoning.push_str(thinking);
        }

        extract_tool_calls(msg, &mut parsed.tool_calls);
        if !std::ptr::eq(msg, &frame) {
            extract_tool_calls(&frame, &mut parsed.tool_calls);
        }

        update_usage(msg, &mut parsed.usage);
        if !std::ptr::eq(msg, &frame) {
            update_usage(&frame, &mut parsed.usage);
        }

        if let Some(reason) = msg
            .get("finishReason")
            .or_else(|| msg.get("finish_reason"))
            .or_else(|| msg.get("stopReason"))
            .or_else(|| frame.get("finishReason"))
            .or_else(|| frame.get("finish_reason"))
            .and_then(Value::as_str)
        {
            parsed.finish_reason = Some(reason.to_owned());
        }
    }

    if parsed.finish_reason.is_none() {
        parsed.finish_reason = Some(if parsed.tool_calls.is_empty() {
            "stop".into()
        } else {
            "tool_calls".into()
        });
    }

    Ok(parsed)
}

fn catalog_id_ok(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 256
        && id.trim() == id
        && !id.chars().any(|c| c.is_control() || c.is_whitespace())
}

/// Parse a `GetCliModelConfigs` response (unary JSON or Connect-RPC framed JSON)
/// into a list of [`CatalogModel`].
pub fn parse_models_catalog(bytes: &[u8]) -> Result<Vec<CatalogModel>> {
    if bytes.len() > CATALOG_MAX_BYTES {
        return Err(SeraError::External("model catalog is too large".into()));
    }
    let frames = decode_connect_frames(bytes)?;
    let mut models = Vec::new();

    for (flags, frame) in frames {
        if flags & 0x02 != 0 {
            if let Some(err) = frame.get("error").filter(|v| !v.is_null()) {
                return Err(connect_error_to_sera(err));
            }
            continue;
        }
        if let Some(err) = frame.get("error").filter(|v| v.is_object()) {
            return Err(connect_error_to_sera(err));
        }

        let Some(entries) = frame
            .get("clientModelConfigs")
            .or_else(|| frame.get("client_model_configs"))
            .or_else(|| frame.get("models"))
            .or_else(|| frame.get("data"))
            .and_then(Value::as_array)
        else {
            continue;
        };

        if models.len() + entries.len() > CATALOG_MAX_ENTRIES {
            return Err(SeraError::External("model catalog is too large".into()));
        }

        for entry in entries {
            if entry.get("disabled").and_then(Value::as_bool) == Some(true) {
                continue;
            }
            let Some(id) = entry
                .get("model")
                .or_else(|| entry.pointer("/modelOrAlias/model"))
                .or_else(|| entry.get("id"))
                .or_else(|| entry.get("modelUid"))
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|id| catalog_id_ok(id))
            else {
                continue;
            };

            let name = entry
                .get("label")
                .or_else(|| entry.get("displayName"))
                .or_else(|| entry.get("name"))
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|n| !n.is_empty() && !n.chars().any(char::is_control))
                .map(str::to_owned);

            let context_length = entry
                .get("maxTokens")
                .or_else(|| entry.get("contextLength"))
                .or_else(|| entry.get("contextWindow"))
                .and_then(Value::as_u64)
                .filter(|v| *v > 0);

            let max_output_tokens = entry
                .get("maxOutputTokens")
                .or_else(|| entry.get("max_output_tokens"))
                .and_then(Value::as_u64)
                .filter(|v| *v > 0);

            let supports_tools = entry
                .get("supportsTools")
                .or_else(|| entry.get("supportsToolCalling"))
                .and_then(Value::as_bool)
                .unwrap_or(true);
            let supports_vision = entry
                .get("supportsImages")
                .or_else(|| entry.get("supportsVision"))
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let supports_reasoning = entry
                .get("supportsThinking")
                .or_else(|| entry.get("supportsReasoning"))
                .and_then(Value::as_bool)
                .unwrap_or(false);

            let mut tags = Vec::new();
            if supports_tools {
                tags.push("tools".to_owned());
            }
            if supports_vision {
                tags.push("vision".to_owned());
            }
            if supports_reasoning {
                tags.push("reasoning".to_owned());
            }

            models.push(CatalogModel {
                id: id.to_owned(),
                name,
                context_length,
                max_output_tokens,
                supports_tools,
                supports_vision,
                supports_reasoning,
                tags,
            });
        }
    }

    Ok(models)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::LlmImage;

    fn connect_frame(flags: u8, payload: &[u8]) -> Vec<u8> {
        let mut out = Vec::with_capacity(5 + payload.len());
        out.push(flags);
        out.extend_from_slice(&(payload.len() as u32).to_be_bytes());
        out.extend_from_slice(payload);
        out
    }

    #[test]
    fn rpc_url_allows_official_and_loopback_and_rejects_others() {
        assert_eq!(
            rpc_url(CODEIUM_BASE_URL, CHAT_RPC_PATH).unwrap(),
            "https://server.codeium.com/exa.api_server_pb.ApiServerService/GetChatMessage"
        );
        assert_eq!(
            rpc_url(DEVIN_BASE_URL, MODELS_RPC_PATH).unwrap(),
            "https://api.devin.ai/exa.api_server_pb.ApiServerService/GetCliModelConfigs"
        );
        assert_eq!(
            rpc_url("http://127.0.0.1:41234/", CHAT_RPC_PATH).unwrap(),
            "http://127.0.0.1:41234/exa.api_server_pb.ApiServerService/GetChatMessage"
        );
        assert!(rpc_url("http://server.codeium.com", CHAT_RPC_PATH).is_err());
        assert!(rpc_url("https://evil.example.com", CHAT_RPC_PATH).is_err());
        assert!(rpc_url("http://127.0.0.1", CHAT_RPC_PATH).is_err());
    }

    #[test]
    fn build_chat_request_formats_messages_tools_and_thinking() {
        let messages = vec![
            LlmMessage::user("Inspect main.rs"),
            LlmMessage {
                role: Role::Assistant,
                content: Some("Let me read it.".into()),
                tool_calls: vec![ToolCall {
                    id: "call_1".into(),
                    name: "read_file".into(),
                    arguments: json!({"path": "main.rs"}),
                    reasoning_signature: None,
                }],
                ..Default::default()
            },
            LlmMessage {
                role: Role::Tool,
                tool_call_id: Some("call_1".into()),
                name: Some("read_file".into()),
                content: Some("fn main() {}".into()),
                ..Default::default()
            },
            LlmMessage {
                role: Role::User,
                content: Some("And this image".into()),
                images: vec![LlmImage {
                    mime: "image/png".into(),
                    data_base64: "AAAA".into(),
                    filename: None,
                }],
                ..Default::default()
            },
        ];

        let tools = vec![ToolSpec {
            name: "read_file".into(),
            description: "Read a file".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string"}
                },
                "required": ["path"]
            }),
            external_effect: false,
        }];

        let body = build_chat_request(
            "claude-sonnet-4-6",
            "You are helpful.",
            &messages,
            &tools,
            Some("high"),
        );

        assert_eq!(body["modelName"], json!("claude-sonnet-4-6"));
        assert_eq!(body["systemPrompt"], json!("You are helpful."));
        assert_eq!(
            body["thinkingConfig"],
            json!({"enabled": true, "effort": "high"})
        );
        assert_eq!(body["tools"].as_array().unwrap().len(), 1);
        assert_eq!(body["tools"][0]["name"], json!("read_file"));

        let chat_msgs = body["chatMessages"].as_array().unwrap();
        assert_eq!(chat_msgs.len(), 4);
        assert_eq!(chat_msgs[0]["source"], json!("CHAT_MESSAGE_SOURCE_USER"));
        assert_eq!(chat_msgs[1]["source"], json!("CHAT_MESSAGE_SOURCE_SYSTEM"));
        assert_eq!(chat_msgs[1]["toolCalls"][0]["name"], json!("read_file"));
        assert_eq!(chat_msgs[2]["source"], json!("CHAT_MESSAGE_SOURCE_TOOL"));
        assert_eq!(
            chat_msgs[2]["toolCallResult"]["toolCallId"],
            json!("call_1")
        );
        assert_eq!(chat_msgs[3]["images"][0]["mimeType"], json!("image/png"));
    }

    #[test]
    fn parse_chat_response_handles_connect_rpc_streaming_frames() {
        let mut stream = Vec::new();
        stream.extend_from_slice(&connect_frame(
            0x00,
            br#"{"chatMessage":{"thinking":"Let me check... "}}"#,
        ));
        stream.extend_from_slice(&connect_frame(
            0x00,
            br#"{"chatMessage":{"content":"Hello ","toolCalls":[{"id":"tc_1","name":"search","argumentsJson":"{\"q\":\"rust\"}"}]}}"#,
        ));
        stream.extend_from_slice(&connect_frame(
            0x00,
            br#"{"chatMessage":{"content":"world!"},"usage":{"inputTokens":120,"outputTokens":30,"cachedTokens":10},"finishReason":"stop"}"#,
        ));
        stream.extend_from_slice(&connect_frame(0x02, br#"{}"#));

        let resp = parse_chat_response(&stream).unwrap();
        assert_eq!(resp.reasoning, "Let me check... ");
        assert_eq!(resp.text, "Hello world!");
        assert_eq!(resp.tool_calls.len(), 1);
        assert_eq!(resp.tool_calls[0].id, "tc_1");
        assert_eq!(resp.tool_calls[0].name, "search");
        assert_eq!(resp.tool_calls[0].arguments, json!({"q": "rust"}));
        assert_eq!(resp.usage.prompt_tokens, 120);
        assert_eq!(resp.usage.completion_tokens, 30);
        assert_eq!(resp.usage.cached_tokens, 10);
        assert_eq!(resp.usage.total_tokens, 160);
        assert_eq!(resp.finish_reason.as_deref(), Some("stop"));
    }

    #[test]
    fn parse_chat_response_handles_unary_json_and_errors() {
        let unary = br#"{"chatMessage":{"content":"Hi","thinking":"hmm"},"usage":{"inputTokens":5,"outputTokens":2}}"#;
        let resp = parse_chat_response(unary).unwrap();
        assert_eq!(resp.text, "Hi");
        assert_eq!(resp.reasoning, "hmm");
        assert_eq!(resp.finish_reason.as_deref(), Some("stop"));

        let mut err_stream = Vec::new();
        err_stream.extend_from_slice(&connect_frame(
            0x02,
            br#"{"error":{"code":"resource_exhausted","message":"rate limited"}}"#,
        ));
        let err = parse_chat_response(&err_stream).unwrap_err();
        match err {
            SeraError::Upstream { status, .. } => assert_eq!(status, 429),
            other => panic!("expected Upstream error, got {other:?}"),
        }
    }

    #[test]
    fn parse_models_catalog_extracts_cli_model_configs() {
        let payload = br#"{
            "clientModelConfigs": [
                {
                    "label": "Claude Sonnet 4.6",
                    "modelOrAlias": {"model": "claude-sonnet-4-6"},
                    "maxTokens": 200000,
                    "maxOutputTokens": 16384,
                    "supportsImages": true,
                    "supportsTools": true,
                    "supportsThinking": true
                },
                {
                    "label": "Disabled Model",
                    "model": "old-model",
                    "disabled": true
                },
                {
                    "label": "GPT-5 Mini",
                    "model": "gpt-5-mini",
                    "contextLength": 128000,
                    "supportsImages": false,
                    "supportsTools": true,
                    "supportsThinking": false
                }
            ]
        }"#;

        let models = parse_models_catalog(payload).unwrap();
        assert_eq!(models.len(), 2);

        assert_eq!(models[0].id, "claude-sonnet-4-6");
        assert_eq!(models[0].name.as_deref(), Some("Claude Sonnet 4.6"));
        assert_eq!(models[0].context_length, Some(200_000));
        assert_eq!(models[0].max_output_tokens, Some(16_384));
        assert!(models[0].supports_vision);
        assert!(models[0].supports_tools);
        assert!(models[0].supports_reasoning);
        assert_eq!(models[0].tags, vec!["tools", "vision", "reasoning"]);

        assert_eq!(models[1].id, "gpt-5-mini");
        assert_eq!(models[1].context_length, Some(128_000));
        assert!(!models[1].supports_vision);
        assert!(models[1].supports_tools);
        assert!(!models[1].supports_reasoning);
        assert_eq!(models[1].tags, vec!["tools"]);
    }
}
