//! AWS Kiro (Amazon Q / CodeWhisperer) EventStream wire format.
//!
//! - Request builder: [`build_generate_request`] for
//!   `POST https://q.<region>.amazonaws.com/generateAssistantResponse`
//! - Binary decoder: [`parse_event_stream`] for `application/vnd.amazon.eventstream`
//!   frames (zero new dependencies, 4 MiB per-frame cap).
//! - Model catalog parser: [`parse_models_catalog`] for `ListAvailableModels`.
//!
//! Everything here is pure: the HTTP loop and credential handling live in `llm.rs`.

use std::collections::BTreeMap;

use serde_json::{Value, json};

use crate::auth_flows::kiro::KiroRegion;
use crate::domain::{LlmMessage, Role, ToolCall};
use crate::error::{Result, SeraError};
use crate::gateway::ToolSpec;
use crate::llm::Usage;
use crate::providers::{ModelCatalog, ModelMeta};

use super::schema;

/// Hard cap on a single AWS EventStream binary frame (4 MiB).
pub const MAX_FRAME_BYTES: usize = 4 * 1024 * 1024;
/// Minimum valid frame size: 12-byte prelude + 4-byte trailing message CRC.
const MIN_FRAME_BYTES: usize = 16;
const CATALOG_MAX_BYTES: usize = 1_048_576;
const CATALOG_MAX_ENTRIES: usize = 4_000;

/// Pinned inference URL for `generateAssistantResponse` in `region`.
pub fn generate_url(region: KiroRegion) -> Result<url::Url> {
    url::Url::parse(&format!(
        "https://{}/generateAssistantResponse",
        region.q_host()
    ))
    .map_err(|_| SeraError::Config("invalid AWS Kiro generate URL".into()))
}

/// Pinned model-list URL for `ListAvailableModels` in `region`.
pub fn models_url(region: KiroRegion) -> Result<url::Url> {
    url::Url::parse(&format!(
        "https://{}/ListAvailableModels?origin=AI_EDITOR",
        region.q_host()
    ))
    .map_err(|_| SeraError::Config("invalid AWS Kiro models URL".into()))
}

/// Decoded output from an AWS Kiro EventStream response.
#[derive(Debug, Clone, Default)]
pub struct KiroStreamResult {
    pub content: String,
    pub reasoning: Option<String>,
    pub tool_calls: Vec<ToolCall>,
    pub usage: Usage,
    pub conversation_id: Option<String>,
}

/// A parsed EventStream header map (string-valued headers such as `:event-type`
/// and `:message-type`).
#[derive(Debug, Default)]
struct FrameHeaders {
    message_type: Option<String>,
    event_type: Option<String>,
    exception_type: Option<String>,
}

/// Decode a complete `application/vnd.amazon.eventstream` byte buffer into
/// text, reasoning, tool calls, and usage.
pub fn parse_event_stream(bytes: &[u8]) -> Result<KiroStreamResult> {
    let mut result = KiroStreamResult::default();
    let mut reasoning_buf = String::new();
    let mut active_tools: Vec<PendingTool> = Vec::new();
    let mut offset = 0usize;

    while offset < bytes.len() {
        let remaining = &bytes[offset..];
        if remaining.len() < MIN_FRAME_BYTES {
            return Err(SeraError::External(
                "AWS Kiro EventStream frame is truncated".into(),
            ));
        }

        let total_len =
            u32::from_be_bytes([remaining[0], remaining[1], remaining[2], remaining[3]]) as usize;
        let headers_len =
            u32::from_be_bytes([remaining[4], remaining[5], remaining[6], remaining[7]]) as usize;
        // Bytes 8..12 are the prelude CRC (skipped).

        if total_len > MAX_FRAME_BYTES {
            return Err(SeraError::External(
                "AWS Kiro EventStream frame exceeds 4 MiB limit".into(),
            ));
        }
        if total_len < MIN_FRAME_BYTES || total_len > remaining.len() {
            return Err(SeraError::External(
                "AWS Kiro EventStream frame length is invalid".into(),
            ));
        }
        let header_end = 12usize.checked_add(headers_len).ok_or_else(|| {
            SeraError::External("AWS Kiro EventStream header length overflow".into())
        })?;
        let payload_end = total_len - 4; // Trailing 4-byte message CRC (skipped).
        if header_end > payload_end {
            return Err(SeraError::External(
                "AWS Kiro EventStream header length exceeds frame".into(),
            ));
        }

        let headers = parse_headers(&remaining[12..header_end])?;
        let payload = &remaining[header_end..payload_end];

        if matches!(headers.message_type.as_deref(), Some("exception" | "error")) {
            let kind = headers.exception_type.as_deref().unwrap_or("stream_error");
            let detail = serde_json::from_slice::<Value>(payload)
                .ok()
                .and_then(|v| {
                    v.get("message")
                        .or_else(|| v.get("Message"))
                        .and_then(Value::as_str)
                        .map(str::to_owned)
                })
                .unwrap_or_else(|| kind.to_owned());
            return Err(SeraError::External(format!(
                "AWS Kiro stream error: {detail}"
            )));
        }

        if !payload.is_empty() {
            let value: Value = serde_json::from_slice(payload).map_err(|_| {
                SeraError::External("AWS Kiro EventStream payload is not valid JSON".into())
            })?;
            apply_event_payload(
                headers.event_type.as_deref(),
                &value,
                &mut result,
                &mut reasoning_buf,
                &mut active_tools,
            );
        }

        offset += total_len;
    }

    for pending in active_tools {
        result.tool_calls.push(pending.into_tool_call());
    }
    if !reasoning_buf.is_empty() {
        result.reasoning = Some(reasoning_buf);
    }
    Ok(result)
}

struct PendingTool {
    id: String,
    name: String,
    input_chunks: String,
    input_value: Option<Value>,
}

impl PendingTool {
    fn into_tool_call(self) -> ToolCall {
        let arguments = if let Some(val) = self.input_value {
            val
        } else if self.input_chunks.trim().is_empty() {
            json!({})
        } else {
            serde_json::from_str(&self.input_chunks).unwrap_or_else(|_| json!({}))
        };
        ToolCall {
            id: self.id,
            name: self.name,
            arguments,
            reasoning_signature: None,
        }
    }
}

fn parse_headers(mut slice: &[u8]) -> Result<FrameHeaders> {
    let mut headers = FrameHeaders::default();
    while !slice.is_empty() {
        let name_len = slice[0] as usize;
        slice = &slice[1..];
        if slice.len() < name_len + 1 {
            return Err(SeraError::External(
                "AWS Kiro EventStream header name is truncated".into(),
            ));
        }
        let name = std::str::from_utf8(&slice[..name_len]).map_err(|_| {
            SeraError::External("AWS Kiro EventStream header name is not UTF-8".into())
        })?;
        slice = &slice[name_len..];
        let value_type = slice[0];
        slice = &slice[1..];

        let string_val: Option<String> = match value_type {
            // 0 = bool true, 1 = bool false
            0 | 1 => None,
            // 2 = byte (i8)
            2 => {
                if slice.is_empty() {
                    return Err(truncated_header());
                }
                slice = &slice[1..];
                None
            }
            // 3 = short (i16)
            3 => {
                if slice.len() < 2 {
                    return Err(truncated_header());
                }
                slice = &slice[2..];
                None
            }
            // 4 = int (i32)
            4 => {
                if slice.len() < 4 {
                    return Err(truncated_header());
                }
                slice = &slice[4..];
                None
            }
            // 5 = long (i64), 8 = timestamp (i64)
            5 | 8 => {
                if slice.len() < 8 {
                    return Err(truncated_header());
                }
                slice = &slice[8..];
                None
            }
            // 6 = bytes (u16 len + bytes)
            6 => {
                if slice.len() < 2 {
                    return Err(truncated_header());
                }
                let len = u16::from_be_bytes([slice[0], slice[1]]) as usize;
                slice = &slice[2..];
                if slice.len() < len {
                    return Err(truncated_header());
                }
                slice = &slice[len..];
                None
            }
            // 7 = string (u16 len + UTF-8)
            7 => {
                if slice.len() < 2 {
                    return Err(truncated_header());
                }
                let len = u16::from_be_bytes([slice[0], slice[1]]) as usize;
                slice = &slice[2..];
                if slice.len() < len {
                    return Err(truncated_header());
                }
                let text = std::str::from_utf8(&slice[..len])
                    .map_err(|_| {
                        SeraError::External("AWS Kiro EventStream header value is not UTF-8".into())
                    })?
                    .to_owned();
                slice = &slice[len..];
                Some(text)
            }
            // 9 = uuid (16 bytes)
            9 => {
                if slice.len() < 16 {
                    return Err(truncated_header());
                }
                slice = &slice[16..];
                None
            }
            _ => {
                return Err(SeraError::External(
                    "AWS Kiro EventStream header has unknown value type".into(),
                ));
            }
        };

        if let Some(val) = string_val {
            match name {
                ":message-type" => headers.message_type = Some(val),
                ":event-type" => headers.event_type = Some(val),
                ":exception-type" => headers.exception_type = Some(val),
                _ => {}
            }
        }
    }
    Ok(headers)
}

fn truncated_header() -> SeraError {
    SeraError::External("AWS Kiro EventStream header value is truncated".into())
}

fn apply_event_payload(
    event_type: Option<&str>,
    value: &Value,
    result: &mut KiroStreamResult,
    reasoning_buf: &mut String,
    active_tools: &mut Vec<PendingTool>,
) {
    // Unwrap `assistantResponseEvent` or `toolUseEvent` envelopes when present.
    let inner = value
        .get("assistantResponseEvent")
        .or_else(|| value.get("toolUseEvent"))
        .unwrap_or(value);

    if let Some(conv_id) = inner.get("conversationId").and_then(Value::as_str) {
        result.conversation_id = Some(conv_id.to_owned());
    }

    // Text content
    if let Some(text) = inner.get("content").and_then(Value::as_str) {
        result.content.push_str(text);
    }

    // Reasoning content (supports string, {"text": ...}, or {"reasoningText": {"text": ...}})
    if let Some(reasoning) = inner
        .get("reasoningContent")
        .or_else(|| inner.get("reasoning_content"))
    {
        extract_reasoning(reasoning, reasoning_buf);
    } else if event_type == Some("reasoningContentEvent")
        && let Some(text) = inner.get("text").and_then(Value::as_str)
    {
        reasoning_buf.push_str(text);
    }

    // Tool use event (either via :event-type == "toolUseEvent", `toolUseEvent` wrapper, or `toolUseId` field)
    let is_tool_event = event_type == Some("toolUseEvent")
        || value.get("toolUseEvent").is_some()
        || inner.get("toolUseId").is_some();
    if is_tool_event && let Some(tool_id) = inner.get("toolUseId").and_then(Value::as_str) {
        let name = inner
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let entry = match active_tools.iter_mut().find(|t| t.id == tool_id) {
            Some(existing) => {
                if existing.name.is_empty() && !name.is_empty() {
                    existing.name = name.to_owned();
                }
                existing
            }
            None => {
                active_tools.push(PendingTool {
                    id: tool_id.to_owned(),
                    name: name.to_owned(),
                    input_chunks: String::new(),
                    input_value: None,
                });
                active_tools.last_mut().expect("just pushed")
            }
        };
        if let Some(input) = inner.get("input") {
            match input {
                Value::String(chunk) => entry.input_chunks.push_str(chunk),
                Value::Object(_) => entry.input_value = Some(input.clone()),
                _ => {}
            }
        }
    }

    // Usage / metering metadata
    if let Some(usage) = inner
        .get("usage")
        .or_else(|| value.get("usageEvent"))
        .or_else(|| value.get("meteringEvent"))
    {
        if let Some(input) = usage
            .get("inputTokens")
            .or_else(|| usage.get("promptTokens"))
            .and_then(Value::as_u64)
        {
            result.usage.prompt_tokens = result.usage.prompt_tokens.max(input);
        }
        if let Some(output) = usage
            .get("outputTokens")
            .or_else(|| usage.get("completionTokens"))
            .and_then(Value::as_u64)
        {
            result.usage.completion_tokens = result.usage.completion_tokens.max(output);
        }
        result.usage.total_tokens = result
            .usage
            .prompt_tokens
            .saturating_add(result.usage.completion_tokens);
    }
}

fn extract_reasoning(val: &Value, buf: &mut String) {
    match val {
        Value::String(text) => buf.push_str(text),
        Value::Object(map) => {
            if let Some(text) = map.get("text").and_then(Value::as_str) {
                buf.push_str(text);
            } else if let Some(inner) = map.get("reasoningText") {
                extract_reasoning(inner, buf);
            }
        }
        _ => {}
    }
}

/// Build the JSON request body for
/// `POST https://q.<region>.amazonaws.com/generateAssistantResponse`.
pub fn build_generate_request(
    model: &str,
    system: Option<&str>,
    messages: &[LlmMessage],
    tools: &[ToolSpec],
    thinking_level: Option<&str>,
) -> Value {
    let tool_specs: Vec<Value> = tools
        .iter()
        .map(|tool| {
            json!({
                "toolSpecification": {
                    "name": tool.name,
                    "description": tool.description,
                    "inputSchema": {
                        "json": schema::flatten(&tool.parameters)
                    }
                }
            })
        })
        .collect();

    let system_text = system
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .or_else(|| {
            let joined = messages
                .iter()
                .filter(|m| m.role == Role::System)
                .filter_map(|m| m.content.as_deref())
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .collect::<Vec<_>>()
                .join("\n\n");
            (!joined.is_empty()).then_some(joined)
        });

    let non_system: Vec<&LlmMessage> = messages.iter().filter(|m| m.role != Role::System).collect();

    // Split into history (all but the final user/tool turn) and currentMessage.
    let mut history: Vec<Value> = Vec::new();
    let mut pending_tool_results: Vec<Value> = Vec::new();
    let mut current_user_content = String::new();
    let mut current_tool_results: Vec<Value> = Vec::new();

    // Find the last user or tool group to place in `currentMessage`.
    let split_idx = non_system
        .iter()
        .rposition(|m| m.role == Role::Assistant)
        .map(|idx| idx + 1)
        .unwrap_or(0);

    for (idx, msg) in non_system.iter().enumerate() {
        let is_current = idx >= split_idx;
        match msg.role {
            Role::System => {}
            Role::User => {
                let text = msg.content.as_deref().unwrap_or("");
                if is_current {
                    if !current_user_content.is_empty() && !text.is_empty() {
                        current_user_content.push_str("\n\n");
                    }
                    current_user_content.push_str(text);
                } else {
                    let mut ctx = serde_json::Map::new();
                    if !pending_tool_results.is_empty() {
                        ctx.insert(
                            "toolResults".into(),
                            Value::Array(std::mem::take(&mut pending_tool_results)),
                        );
                    }
                    history.push(json!({
                        "userInputMessage": {
                            "content": text,
                            "modelId": model,
                            "origin": "AI_EDITOR",
                            "userInputMessageContext": Value::Object(ctx),
                        }
                    }));
                }
            }
            Role::Assistant => {
                if !pending_tool_results.is_empty() {
                    history.push(json!({
                        "userInputMessage": {
                            "content": "",
                            "modelId": model,
                            "origin": "AI_EDITOR",
                            "userInputMessageContext": {
                                "toolResults": std::mem::take(&mut pending_tool_results),
                            }
                        }
                    }));
                }
                let tool_uses: Vec<Value> = msg
                    .tool_calls
                    .iter()
                    .map(|call| {
                        json!({
                            "toolUseId": call.id,
                            "name": call.name,
                            "input": call.arguments,
                        })
                    })
                    .collect();
                let mut assistant = json!({
                    "content": msg.content.as_deref().unwrap_or(""),
                });
                if !tool_uses.is_empty() {
                    assistant["toolUses"] = Value::Array(tool_uses);
                }
                history.push(json!({
                    "assistantResponseMessage": assistant
                }));
            }
            Role::Tool => {
                let Some(call_id) = msg.tool_call_id.as_deref() else {
                    continue;
                };
                let item = json!({
                    "toolUseId": call_id,
                    "content": [{ "text": msg.content.as_deref().unwrap_or("") }],
                    "status": if msg.is_error { "error" } else { "success" },
                });
                if is_current {
                    current_tool_results.push(item);
                } else {
                    pending_tool_results.push(item);
                }
            }
        }
    }

    if !pending_tool_results.is_empty() {
        current_tool_results.extend(pending_tool_results);
    }

    if let Some(sys) = system_text {
        if history.is_empty() {
            if current_user_content.is_empty() {
                current_user_content = sys;
            } else {
                current_user_content = format!("{sys}\n\n{current_user_content}");
            }
        } else if let Some(first_user) = history
            .iter_mut()
            .find_map(|item| item.get_mut("userInputMessage"))
        {
            let existing = first_user
                .get("content")
                .and_then(Value::as_str)
                .unwrap_or("");
            let combined = if existing.is_empty() {
                sys
            } else {
                format!("{sys}\n\n{existing}")
            };
            first_user["content"] = Value::String(combined);
        }
    }

    let mut current_ctx = serde_json::Map::new();
    if !tool_specs.is_empty() {
        current_ctx.insert("tools".into(), Value::Array(tool_specs));
    }
    if !current_tool_results.is_empty() {
        current_ctx.insert("toolResults".into(), Value::Array(current_tool_results));
    }

    let mut user_input = json!({
        "content": current_user_content,
        "modelId": model,
        "origin": "AI_EDITOR",
    });
    if !current_ctx.is_empty() {
        user_input["userInputMessageContext"] = Value::Object(current_ctx);
    }

    let mut state = json!({
        "chatTriggerType": "MANUAL",
        "conversationId": uuid::Uuid::new_v4().to_string(),
        "currentMessage": {
            "userInputMessage": user_input
        }
    });
    if !history.is_empty() {
        state["history"] = Value::Array(history);
    }

    let mut body = json!({
        "conversationState": state
    });
    if let Some(level) = thinking_level.filter(|l| *l != "off" && !l.is_empty()) {
        body["thinkingLevel"] = Value::String(level.to_owned());
    }
    body
}

/// Parse an AWS Kiro `ListAvailableModels` JSON response into [`ModelCatalog`].
pub fn parse_models_catalog(bytes: &[u8]) -> Result<ModelCatalog> {
    if bytes.len() > CATALOG_MAX_BYTES {
        return Err(SeraError::External(
            "AWS Kiro model catalog is too large".into(),
        ));
    }
    let value: Value = serde_json::from_slice(bytes)
        .map_err(|_| SeraError::External("AWS Kiro model catalog is malformed".into()))?;
    let list = value
        .get("models")
        .or_else(|| value.get("data"))
        .and_then(Value::as_array)
        .ok_or_else(|| SeraError::External("AWS Kiro model catalog has no models list".into()))?;

    let mut catalog = BTreeMap::new();
    if let Some(default_model) = value.get("defaultModel") {
        insert_catalog_entry(&mut catalog, default_model);
    }
    for entry in list.iter().take(CATALOG_MAX_ENTRIES) {
        insert_catalog_entry(&mut catalog, entry);
    }
    Ok(catalog)
}

fn insert_catalog_entry(catalog: &mut ModelCatalog, entry: &Value) {
    let Some(id) = entry
        .get("modelId")
        .or_else(|| entry.get("id"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
    else {
        return;
    };
    let name = entry
        .get("modelName")
        .or_else(|| entry.get("name"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty() && *s != id)
        .map(str::to_owned);
    let context_length = entry
        .pointer("/tokenLimits/maxInputTokens")
        .or_else(|| entry.get("contextLength"))
        .and_then(Value::as_u64)
        .filter(|v| *v > 0);
    let max_output_tokens = entry
        .pointer("/tokenLimits/maxOutputTokens")
        .and_then(Value::as_u64)
        .filter(|v| *v > 0);

    let mut tags = vec!["tools".to_owned()];
    let lower = id.to_ascii_lowercase();
    if lower.contains("3-7")
        || lower.contains("3.7")
        || lower.contains("sonnet-4")
        || lower.contains("opus-4")
        || lower.contains("thinking")
    {
        tags.push("reasoning".to_owned());
    }

    catalog.insert(
        id.to_owned(),
        ModelMeta {
            name,
            context_length,
            max_output_tokens,
            tags,
        },
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn encode_string_header(buf: &mut Vec<u8>, name: &str, value: &str) {
        buf.push(name.len() as u8);
        buf.extend_from_slice(name.as_bytes());
        buf.push(7); // string type
        buf.extend_from_slice(&(value.len() as u16).to_be_bytes());
        buf.extend_from_slice(value.as_bytes());
    }

    fn encode_frame(headers: &[(&str, &str)], payload: &[u8]) -> Vec<u8> {
        let mut hdr_bytes = Vec::new();
        for (name, val) in headers {
            encode_string_header(&mut hdr_bytes, name, val);
        }
        let total_len = (12 + hdr_bytes.len() + payload.len() + 4) as u32;
        let headers_len = hdr_bytes.len() as u32;

        let mut frame = Vec::with_capacity(total_len as usize);
        frame.extend_from_slice(&total_len.to_be_bytes());
        frame.extend_from_slice(&headers_len.to_be_bytes());
        frame.extend_from_slice(&0u32.to_be_bytes()); // prelude CRC (ignored)
        frame.extend_from_slice(&hdr_bytes);
        frame.extend_from_slice(payload);
        frame.extend_from_slice(&0u32.to_be_bytes()); // trailing CRC (ignored)
        frame
    }

    #[test]
    fn decodes_text_reasoning_and_tool_use_frames() {
        let mut stream = Vec::new();
        stream.extend_from_slice(&encode_frame(
            &[
                (":message-type", "event"),
                (":event-type", "assistantResponseEvent"),
            ],
            br#"{"assistantResponseEvent":{"reasoningContent":{"text":"Let me check."}}}"#,
        ));
        stream.extend_from_slice(&encode_frame(
            &[
                (":message-type", "event"),
                (":event-type", "assistantResponseEvent"),
            ],
            br#"{"assistantResponseEvent":{"content":"Hello "}}"#,
        ));
        stream.extend_from_slice(&encode_frame(
            &[
                (":message-type", "event"),
                (":event-type", "assistantResponseEvent"),
            ],
            br#"{"assistantResponseEvent":{"content":"world!"}}"#,
        ));
        stream.extend_from_slice(&encode_frame(
            &[(":message-type", "event"), (":event-type", "toolUseEvent")],
            br#"{"toolUseId":"call_1","name":"read_file","input":"{\"path\":"}"#,
        ));
        stream.extend_from_slice(&encode_frame(
            &[(":message-type", "event"), (":event-type", "toolUseEvent")],
            br#"{"toolUseId":"call_1","name":"read_file","input":"\"README.md\"}","stop":true}"#,
        ));

        let parsed = parse_event_stream(&stream).unwrap();
        assert_eq!(parsed.content, "Hello world!");
        assert_eq!(parsed.reasoning.as_deref(), Some("Let me check."));
        assert_eq!(parsed.tool_calls.len(), 1);
        assert_eq!(parsed.tool_calls[0].id, "call_1");
        assert_eq!(parsed.tool_calls[0].name, "read_file");
        assert_eq!(parsed.tool_calls[0].arguments, json!({"path": "README.md"}));
    }

    #[test]
    fn rejects_oversized_frame_over_4_mib() {
        let oversized_len = (MAX_FRAME_BYTES as u32) + 1;
        let mut frame = Vec::new();
        frame.extend_from_slice(&oversized_len.to_be_bytes());
        frame.extend_from_slice(&0u32.to_be_bytes());
        frame.extend_from_slice(&0u32.to_be_bytes());
        frame.extend_from_slice(&0u32.to_be_bytes());

        let err = parse_event_stream(&frame).unwrap_err();
        assert!(err.to_string().contains("4 MiB"));
    }

    #[test]
    fn builds_generate_request_with_system_tools_and_history() {
        let messages = vec![
            LlmMessage::user("Hi"),
            LlmMessage::assistant(
                Some("Checking".into()),
                vec![ToolCall {
                    id: "tc_1".into(),
                    name: "search".into(),
                    arguments: json!({"q": "rust"}),
                    reasoning_signature: None,
                }],
            ),
            LlmMessage {
                role: Role::Tool,
                content: Some("found 3 results".into()),
                tool_call_id: Some("tc_1".into()),
                ..Default::default()
            },
        ];
        let tools = vec![ToolSpec {
            name: "search".into(),
            description: "Search docs".into(),
            parameters: json!({
                "type": "object",
                "properties": { "q": { "type": "string" } },
                "required": ["q"]
            }),
            external_effect: false,
        }];

        let req = build_generate_request(
            "claude-sonnet-4",
            Some("Be concise."),
            &messages,
            &tools,
            Some("medium"),
        );

        let state = &req["conversationState"];
        assert_eq!(state["chatTriggerType"], "MANUAL");
        assert_eq!(
            state["history"][0]["userInputMessage"]["content"],
            "Be concise.\n\nHi"
        );
        assert_eq!(
            state["history"][1]["assistantResponseMessage"]["toolUses"][0]["toolUseId"],
            "tc_1"
        );
        assert_eq!(
            state["currentMessage"]["userInputMessage"]["modelId"],
            "claude-sonnet-4"
        );
        assert_eq!(
            state["currentMessage"]["userInputMessage"]["userInputMessageContext"]["toolResults"]
                [0]["toolUseId"],
            "tc_1"
        );
        assert_eq!(
            state["currentMessage"]["userInputMessage"]["userInputMessageContext"]["tools"][0]["toolSpecification"]
                ["name"],
            "search"
        );
        assert_eq!(req["thinkingLevel"], "medium");
    }

    #[test]
    fn parses_list_available_models_catalog() {
        let body = br#"{
            "defaultModel": {
                "modelId": "claude-sonnet-4",
                "modelName": "Claude Sonnet 4",
                "tokenLimits": { "maxInputTokens": 200000, "maxOutputTokens": 16384 }
            },
            "models": [
                {
                    "modelId": "claude-3-5-haiku",
                    "modelName": "Claude 3.5 Haiku",
                    "tokenLimits": { "maxInputTokens": 200000 }
                }
            ]
        }"#;
        let catalog = parse_models_catalog(body).unwrap();
        assert_eq!(catalog.len(), 2);
        let sonnet = &catalog["claude-sonnet-4"];
        assert_eq!(sonnet.name.as_deref(), Some("Claude Sonnet 4"));
        assert_eq!(sonnet.context_length, Some(200000));
        assert_eq!(sonnet.max_output_tokens, Some(16384));
        assert!(sonnet.tags.contains(&"reasoning".to_owned()));
        assert!(
            !catalog["claude-3-5-haiku"]
                .tags
                .contains(&"reasoning".to_owned())
        );
    }
}
