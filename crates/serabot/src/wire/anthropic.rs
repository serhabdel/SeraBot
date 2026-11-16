//! The native Anthropic Messages transport: `POST {base}/messages` over SSE.
//!
//! Used by the `anthropic` provider kind, for API-key accounts (`x-api-key`)
//! and, behind the admin `plan_oauth` flag, for Claude plan sign-ins (a bearer
//! token plus the `oauth-2025-04-20` beta).
//!
//! - `max_tokens` is required and always sent. `system` is one string.
//! - Content blocks: `text`, `tool_use` and `tool_result`. A tool result sits
//!   first in the user turn that answers the call. A result with no matching
//!   call is dropped; a call with no result gets an `is_error` placeholder, so
//!   a history that crossed providers never earns a hard 400.
//! - Extended thinking: `thinking: {type: enabled, budget_tokens}` with the
//!   budget clamped below `max_tokens` (and omitted under 1024); no
//!   `temperature` while it is on. Signed `thinking` and `redacted_thinking`
//!   blocks are captured from a reply that calls tools and replayed in the
//!   same order and byte for byte on the next request of the loop. They ride on
//!   the first tool call's `reasoning_signature` under a provider prefix, so
//!   another provider never sees them. When the loop has a tool turn whose
//!   thinking cannot be replayed (it came from elsewhere), thinking is left
//!   out of that request: Anthropic refuses the mix.
//! - Tool names are limited to `[A-Za-z0-9_-]`; others are mapped on the way
//!   out and back.
//!
//! Everything here is pure: the HTTP loop is in `llm.rs`.

use std::collections::BTreeMap;

use base64::Engine;
use serde_json::{Map, Value, json};

use crate::domain::{LlmMessage, Role, ToolCall};
use crate::error::{Result, SeraError};
use crate::gateway::ToolSpec;
use crate::llm::Usage;
use crate::llm_failure;
use crate::providers::{ModelMeta, official_model_id};

use super::sse;

/// The API version every request names.
pub const API_VERSION: &str = "2023-06-01";
/// The beta a Claude plan bearer token needs.
pub const OAUTH_BETA: &str = "oauth-2025-04-20";
/// Marks a `reasoning_signature` that holds Anthropic thinking blocks.
pub const SIGNATURE_PREFIX: &str = "anthropic.v1:";
/// Largest thinking replay kept on one tool call.
const MAX_REPLAY_BYTES: usize = 256 * 1024;
/// Smallest thinking budget the API accepts.
const MIN_BUDGET: u64 = 1024;
/// Placeholder body for a tool call whose result was never recorded.
const MISSING_RESULT: &str = "No result was recorded for this tool call.";

/// What a request needs beyond the conversation.
pub struct RequestOptions<'a> {
    pub model: &'a str,
    pub max_tokens: u32,
    pub temperature: Option<f32>,
    /// The thinking fields from `thinking::wire_fields`; only `thinking` is used.
    pub extra: Option<&'a Map<String, Value>>,
}

/// A tool name the API accepts: `[A-Za-z0-9_-]`, at most 64 characters.
pub fn wire_tool_name(name: &str) -> String {
    let mapped: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '_' | '-') {
                c
            } else {
                '_'
            }
        })
        .take(64)
        .collect();
    if mapped.is_empty() {
        "tool".into()
    } else {
        mapped
    }
}

/// Put the caller's own tool names back on a reply's tool calls.
pub fn restore_tool_names(calls: &mut [ToolCall], tools: &[ToolSpec]) {
    for call in calls {
        if let Some(tool) = tools
            .iter()
            .find(|tool| wire_tool_name(&tool.name) == call.name)
        {
            call.name = tool.name.clone();
        }
    }
}

fn clean_id(id: &str, fallback: usize) -> String {
    if !id.is_empty()
        && id.len() <= 64
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-'))
    {
        id.to_owned()
    } else {
        format!("toolu_{fallback}")
    }
}

/// Pack signed thinking blocks for `reasoning_signature`. `None` when there is
/// nothing to keep or it is too large to replay.
pub fn encode_thinking(blocks: &[Value]) -> Option<String> {
    if blocks.is_empty() {
        return None;
    }
    let json = serde_json::to_vec(blocks).ok()?;
    let packed = format!(
        "{SIGNATURE_PREFIX}{}",
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(json)
    );
    (packed.len() <= MAX_REPLAY_BYTES).then_some(packed)
}

/// The thinking blocks a `reasoning_signature` holds, when it is ours and
/// well-formed. Anything else (another provider's signature, a damaged one)
/// is ignored.
pub fn decode_thinking(signature: &str) -> Option<Vec<Value>> {
    let packed = signature.strip_prefix(SIGNATURE_PREFIX)?;
    if signature.len() > MAX_REPLAY_BYTES {
        return None;
    }
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(packed)
        .ok()?;
    let blocks: Vec<Value> = serde_json::from_slice(&bytes).ok()?;
    let valid = !blocks.is_empty()
        && blocks
            .iter()
            .all(|block| match block.get("type").and_then(Value::as_str) {
                Some("thinking") => {
                    block.get("thinking").is_some_and(Value::is_string)
                        && block.get("signature").is_some_and(Value::is_string)
                }
                Some("redacted_thinking") => block.get("data").is_some_and(Value::is_string),
                _ => false,
            });
    valid.then_some(blocks)
}

struct Turn {
    user: bool,
    blocks: Vec<Value>,
    /// The ids of the tool calls in an assistant turn.
    calls: Vec<String>,
    /// Whether an assistant turn carries tool calls but no replayable thinking.
    unsigned_calls: bool,
    /// Whether this user turn is real text, not only tool results.
    has_text: bool,
}

fn placeholder_results(ids: &[String]) -> Vec<Value> {
    ids.iter()
        .map(|id| {
            json!({
                "type": "tool_result",
                "tool_use_id": id,
                "content": MISSING_RESULT,
                "is_error": true,
            })
        })
        .collect()
}

/// Put `block` in a user turn: tool results first, everything else after.
fn push_user_block(blocks: &mut Vec<Value>, block: Value) {
    if block.get("type").and_then(Value::as_str) == Some("tool_result") {
        let at = blocks
            .iter()
            .take_while(|b| b.get("type").and_then(Value::as_str) == Some("tool_result"))
            .count();
        blocks.insert(at, block);
    } else {
        blocks.push(block);
    }
}

/// The conversation as Anthropic turns: alternating, starting with the user.
fn build_turns(messages: &[LlmMessage]) -> Vec<Turn> {
    let mut raw: Vec<Turn> = Vec::new();
    let mut counter = 0usize;
    let mut renamed: Vec<(String, String)> = Vec::new();
    for message in messages {
        match message.role {
            Role::System => {}
            Role::User => {
                if let Some(text) = message.content.as_deref().filter(|t| !t.trim().is_empty()) {
                    raw.push(Turn {
                        user: true,
                        blocks: vec![json!({"type": "text", "text": text})],
                        calls: Vec::new(),
                        unsigned_calls: false,
                        has_text: true,
                    });
                }
            }
            Role::Assistant => {
                let mut blocks = Vec::new();
                let mut calls = Vec::new();
                let mut replayed = false;
                if !message.tool_calls.is_empty()
                    && let Some(thinking) = message
                        .tool_calls
                        .iter()
                        .find_map(|call| call.reasoning_signature.as_deref())
                        .and_then(decode_thinking)
                {
                    blocks.extend(thinking);
                    replayed = true;
                }
                if let Some(text) = message.content.as_deref().filter(|t| !t.trim().is_empty()) {
                    blocks.push(json!({"type": "text", "text": text}));
                }
                for call in &message.tool_calls {
                    counter += 1;
                    let id = clean_id(&call.id, counter);
                    renamed.push((call.id.clone(), id.clone()));
                    calls.push(id.clone());
                    let input = call
                        .arguments
                        .as_object()
                        .cloned()
                        .map(Value::Object)
                        .unwrap_or_else(|| json!({}));
                    blocks.push(json!({
                        "type": "tool_use",
                        "id": id,
                        "name": wire_tool_name(&call.name),
                        "input": input,
                    }));
                }
                if !blocks.is_empty() {
                    let unsigned = !calls.is_empty() && !replayed;
                    raw.push(Turn {
                        user: false,
                        blocks,
                        calls,
                        unsigned_calls: unsigned,
                        has_text: false,
                    });
                }
            }
            Role::Tool => {
                let Some(original) = message.tool_call_id.as_deref() else {
                    continue;
                };
                let id = renamed
                    .iter()
                    .rev()
                    .find(|(from, _)| from == original)
                    .map(|(_, to)| to.clone())
                    .unwrap_or_else(|| original.to_owned());
                let text = message.content.clone().unwrap_or_default();
                let mut block = json!({
                    "type": "tool_result",
                    "tool_use_id": id,
                    "content": if text.is_empty() { "(empty)".to_owned() } else { text },
                });
                if message.is_error {
                    block["is_error"] = json!(true);
                }
                raw.push(Turn {
                    user: true,
                    blocks: vec![block],
                    calls: Vec::new(),
                    unsigned_calls: false,
                    has_text: false,
                });
            }
        }
    }
    // Pair every tool call with exactly one result.
    let mut paired: Vec<Turn> = Vec::new();
    let mut expected: Vec<String> = Vec::new();
    for mut turn in raw {
        if turn.user {
            let mut kept = Vec::new();
            for block in turn.blocks {
                if block.get("type").and_then(Value::as_str) == Some("tool_result") {
                    let id = block
                        .get("tool_use_id")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_owned();
                    if let Some(at) = expected.iter().position(|open| *open == id) {
                        expected.remove(at);
                        kept.push(block);
                    }
                    // A result for a call that was never made is dropped.
                } else {
                    kept.push(block);
                }
            }
            turn.blocks = kept;
            if turn.has_text && !expected.is_empty() {
                let mut blocks = placeholder_results(&expected);
                blocks.append(&mut turn.blocks);
                turn.blocks = blocks;
                expected.clear();
            }
            if turn.blocks.is_empty() {
                continue;
            }
        } else {
            if !expected.is_empty() {
                paired.push(Turn {
                    user: true,
                    blocks: placeholder_results(&expected),
                    calls: Vec::new(),
                    unsigned_calls: false,
                    has_text: false,
                });
                expected.clear();
            }
            expected = turn.calls.clone();
        }
        paired.push(turn);
    }
    if !expected.is_empty() {
        paired.push(Turn {
            user: true,
            blocks: placeholder_results(&expected),
            calls: Vec::new(),
            unsigned_calls: false,
            has_text: false,
        });
    }
    // Merge neighbours of the same role.
    let mut turns: Vec<Turn> = Vec::new();
    for turn in paired {
        match turns.last_mut() {
            Some(last) if last.user == turn.user => {
                if turn.user {
                    for block in turn.blocks {
                        push_user_block(&mut last.blocks, block);
                    }
                } else {
                    last.blocks.extend(turn.blocks);
                    last.calls.extend(turn.calls);
                }
                last.unsigned_calls |= turn.unsigned_calls;
                last.has_text |= turn.has_text;
            }
            _ => turns.push(turn),
        }
    }
    if turns.first().is_some_and(|turn| !turn.user) {
        turns.insert(
            0,
            Turn {
                user: true,
                blocks: vec![json!({"type": "text", "text": "..."})],
                calls: Vec::new(),
                unsigned_calls: false,
                has_text: true,
            },
        );
    }
    if turns.is_empty() {
        turns.push(Turn {
            user: true,
            blocks: vec![json!({"type": "text", "text": "..."})],
            calls: Vec::new(),
            unsigned_calls: false,
            has_text: true,
        });
    }
    turns
}

/// Whether the tool loop at the end of the conversation can carry thinking:
/// every assistant tool turn after the last real user message replays its
/// signed blocks.
fn loop_can_think(turns: &[Turn]) -> bool {
    let start = turns
        .iter()
        .rposition(|turn| turn.user && turn.has_text)
        .map_or(0, |at| at + 1);
    turns[start..].iter().all(|turn| !turn.unsigned_calls)
}

fn input_schema(parameters: &Value) -> Value {
    let mut schema = match parameters.as_object() {
        Some(map) => Value::Object(map.clone()),
        None => json!({}),
    };
    if schema.get("type").and_then(Value::as_str) != Some("object") {
        schema["type"] = json!("object");
    }
    if !schema.get("properties").is_some_and(Value::is_object) {
        schema["properties"] = json!({});
    }
    schema
}

/// The request body for one completion.
pub fn build_request(
    messages: &[LlmMessage],
    tools: &[ToolSpec],
    options: &RequestOptions<'_>,
) -> Value {
    let system = messages
        .iter()
        .filter(|message| message.role == Role::System)
        .filter_map(|message| message.content.as_deref())
        .filter(|text| !text.trim().is_empty())
        .collect::<Vec<_>>()
        .join("\n\n");
    let turns = build_turns(messages);
    let thinking_allowed = loop_can_think(&turns);
    let wire_messages: Vec<Value> = turns
        .into_iter()
        .map(|turn| {
            json!({
                "role": if turn.user { "user" } else { "assistant" },
                "content": turn.blocks,
            })
        })
        .collect();
    let max_tokens = options.max_tokens.max(1);
    let mut body = json!({
        "model": options.model,
        "max_tokens": max_tokens,
        "messages": wire_messages,
        "stream": true,
    });
    if !system.is_empty() {
        body["system"] = json!(system);
    }
    if !tools.is_empty() {
        body["tools"] = Value::Array(
            tools
                .iter()
                .map(|tool| {
                    json!({
                        "name": wire_tool_name(&tool.name),
                        "description": tool.description,
                        "input_schema": input_schema(&tool.parameters),
                    })
                })
                .collect(),
        );
    }
    let mut thinking_on = false;
    if let Some(thinking) = options.extra.and_then(|extra| extra.get("thinking")) {
        match thinking.get("type").and_then(Value::as_str) {
            Some("disabled") => body["thinking"] = json!({"type": "disabled"}),
            Some("enabled") if thinking_allowed => {
                let wanted = thinking
                    .get("budget_tokens")
                    .and_then(Value::as_u64)
                    .unwrap_or(0);
                // The budget must stay below `max_tokens`.
                let budget = wanted.min(u64::from(max_tokens).saturating_sub(1));
                if budget >= MIN_BUDGET {
                    body["thinking"] = json!({"type": "enabled", "budget_tokens": budget});
                    thinking_on = true;
                }
            }
            _ => {}
        }
    }
    if !thinking_on && let Some(temperature) = options.temperature {
        body["temperature"] = json!(temperature.clamp(0.0, 1.0));
    }
    body
}

/// The output ceiling a "max_tokens: 32768 > 8192" refusal names.
fn max_tokens_limit(text: &str) -> Option<u64> {
    let rest = &text[text.find("max_tokens")?..];
    let after = rest[rest.find('>')? + 1..].trim_start();
    let digits: String = after.chars().take_while(char::is_ascii_digit).collect();
    digits.parse().ok().filter(|limit| *limit >= 1)
}

/// Drop thinking, and the thinking blocks that only make sense with it.
fn remove_thinking(map: &mut Map<String, Value>) -> bool {
    let had = map.remove("thinking").is_some();
    if had && let Some(messages) = map.get_mut("messages").and_then(Value::as_array_mut) {
        for message in messages {
            if let Some(blocks) = message.get_mut("content").and_then(Value::as_array_mut) {
                blocks.retain(|block| {
                    !matches!(
                        block.get("type").and_then(Value::as_str),
                        Some("thinking" | "redacted_thinking")
                    )
                });
            }
        }
    }
    had
}

/// Repair `body` after a 400 so the request can be sent once more: remove the
/// thinking request when the error blamed it, or lower `max_tokens` to the
/// ceiling the model named. Returns whether anything changed; when nothing
/// did, the 400 is final.
pub fn strip_for_bad_request(body: &mut Value, error_text: &str) -> bool {
    let text = error_text.to_ascii_lowercase();
    let Some(map) = body.as_object_mut() else {
        return false;
    };
    let mut changed = false;
    if let Some(limit) = max_tokens_limit(&text)
        && map
            .get("max_tokens")
            .and_then(Value::as_u64)
            .is_some_and(|current| current > limit)
    {
        map.insert("max_tokens".into(), json!(limit));
        changed = true;
        // The thinking budget must stay below the new ceiling.
        let budget = map
            .get("thinking")
            .and_then(|thinking| thinking.get("budget_tokens"))
            .and_then(Value::as_u64);
        if let Some(budget) = budget
            && budget >= limit
        {
            let lowered = limit.saturating_sub(1);
            if lowered >= MIN_BUDGET {
                map["thinking"]["budget_tokens"] = json!(lowered);
            } else {
                remove_thinking(map);
            }
        }
    }
    if (text.contains("thinking") || text.contains("budget_tokens")) && remove_thinking(map) {
        changed = true;
    }
    changed
}

/// What a finished Messages stream said.
#[derive(Debug, Default)]
pub struct ParsedMessage {
    pub text: String,
    pub reasoning: String,
    pub tool_calls: Vec<ToolCall>,
    pub usage: Usage,
    pub finish_reason: Option<String>,
    /// The model declined to answer.
    pub refused: bool,
}

enum Block {
    Text(String),
    Tool {
        id: String,
        name: String,
        json: String,
    },
    Thinking {
        text: String,
        signature: String,
    },
    Redacted(String),
    Other,
}

fn error_status(kind: &str) -> u16 {
    match kind {
        "overloaded_error" => 529,
        "rate_limit_error" => 429,
        "authentication_error" => 401,
        "permission_error" => 403,
        "not_found_error" => 404,
        "request_too_large" => 413,
        "api_error" => 500,
        _ => 400,
    }
}

fn stream_failure(error: &Value) -> SeraError {
    let kind: String = error
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or("error")
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-'))
        .take(48)
        .collect();
    llm_failure::upstream_error(
        error_status(&kind),
        &json!({"error": error}).to_string(),
        None,
        format!("The model provider reported an error ({kind})"),
    )
}

fn stop_reason_word(reason: &str) -> String {
    match reason {
        "end_turn" | "stop_sequence" => "stop".into(),
        "tool_use" => "tool_calls".into(),
        "max_tokens" => "length".into(),
        other => other.chars().take(48).collect(),
    }
}

/// Read a finished SSE body into text, reasoning, tool calls and usage.
pub fn parse_stream(body: &[u8]) -> Result<ParsedMessage> {
    let mut blocks: BTreeMap<u64, Block> = BTreeMap::new();
    let mut parsed = ParsedMessage::default();
    let (mut input_tokens, mut output_tokens) = (0u64, 0u64);
    for event in sse::events(body) {
        let Ok(data) = serde_json::from_str::<Value>(&event.data) else {
            continue;
        };
        let kind = data
            .get("type")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .or(event.event.clone())
            .unwrap_or_default();
        let index = data.get("index").and_then(Value::as_u64).unwrap_or(0);
        match kind.as_str() {
            "message_start" => {
                if let Some(usage) = data.pointer("/message/usage") {
                    let count = |key: &str| usage.get(key).and_then(Value::as_u64).unwrap_or(0);
                    input_tokens = count("input_tokens")
                        .saturating_add(count("cache_creation_input_tokens"))
                        .saturating_add(count("cache_read_input_tokens"));
                    output_tokens = output_tokens.max(count("output_tokens"));
                }
            }
            "content_block_start" => {
                let block = data.get("content_block").unwrap_or(&Value::Null);
                let text = |key: &str| {
                    block
                        .get(key)
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_owned()
                };
                blocks.insert(
                    index,
                    match block.get("type").and_then(Value::as_str) {
                        Some("text") => Block::Text(text("text")),
                        Some("tool_use") => Block::Tool {
                            id: text("id"),
                            name: text("name"),
                            json: String::new(),
                        },
                        Some("thinking") => Block::Thinking {
                            text: text("thinking"),
                            signature: text("signature"),
                        },
                        Some("redacted_thinking") => Block::Redacted(text("data")),
                        _ => Block::Other,
                    },
                );
            }
            "content_block_delta" => {
                let delta = data.get("delta").unwrap_or(&Value::Null);
                let piece = |key: &str| delta.get(key).and_then(Value::as_str).unwrap_or_default();
                match (
                    delta.get("type").and_then(Value::as_str),
                    blocks.get_mut(&index),
                ) {
                    (Some("text_delta"), Some(Block::Text(text))) => text.push_str(piece("text")),
                    (Some("input_json_delta"), Some(Block::Tool { json, .. })) => {
                        json.push_str(piece("partial_json"));
                    }
                    (Some("thinking_delta"), Some(Block::Thinking { text, .. })) => {
                        text.push_str(piece("thinking"));
                    }
                    (Some("signature_delta"), Some(Block::Thinking { signature, .. })) => {
                        signature.push_str(piece("signature"));
                    }
                    _ => {}
                }
            }
            "message_delta" => {
                if let Some(reason) = data.pointer("/delta/stop_reason").and_then(Value::as_str) {
                    if reason == "refusal" {
                        parsed.refused = true;
                    }
                    parsed.finish_reason = Some(stop_reason_word(reason));
                }
                if let Some(out) = data.pointer("/usage/output_tokens").and_then(Value::as_u64) {
                    output_tokens = output_tokens.max(out);
                }
            }
            "error" => {
                let error = data.get("error").cloned().unwrap_or(data);
                return Err(stream_failure(&error));
            }
            _ => {}
        }
    }
    let mut replay: Vec<Value> = Vec::new();
    for (_, block) in blocks {
        match block {
            Block::Text(text) => parsed.text.push_str(&text),
            Block::Tool { id, name, json } => {
                if name.is_empty() {
                    return Err(SeraError::External("tool call missing name".into()));
                }
                let arguments = if json.trim().is_empty() {
                    json!({})
                } else {
                    serde_json::from_str(&json).map_err(|error| {
                        SeraError::External(format!("tool call arguments are not JSON: {error}"))
                    })?
                };
                let id = if id.is_empty() {
                    format!("toolu_{}", parsed.tool_calls.len())
                } else {
                    id
                };
                parsed.tool_calls.push(ToolCall {
                    id,
                    name,
                    arguments,
                    reasoning_signature: None,
                });
            }
            Block::Thinking { text, signature } => {
                parsed.reasoning.push_str(&text);
                replay.push(json!({"type": "thinking", "thinking": text, "signature": signature}));
            }
            Block::Redacted(data) => {
                replay.push(json!({"type": "redacted_thinking", "data": data}));
            }
            Block::Other => {}
        }
    }
    // Thinking only has to come back when the model called a tool.
    if let Some(first) = parsed.tool_calls.first_mut() {
        first.reasoning_signature = encode_thinking(&replay);
    }
    parsed.usage = Usage {
        prompt_tokens: input_tokens,
        completion_tokens: output_tokens,
        total_tokens: input_tokens.saturating_add(output_tokens),
    };
    if parsed.finish_reason.is_none() {
        parsed.finish_reason = Some(if parsed.tool_calls.is_empty() {
            "stop".into()
        } else {
            "tool_calls".into()
        });
    }
    if parsed.text.trim().is_empty() && parsed.tool_calls.is_empty() {
        if parsed.refused {
            return Err(SeraError::upstream(
                200,
                llm_failure::CODE_CONTENT_FILTERED,
                "The model provider declined to answer this request",
            ));
        }
        return Err(SeraError::External(
            "LLM returned neither content nor tool calls".into(),
        ));
    }
    Ok(parsed)
}

/// One page of `GET /v1/models`.
pub struct CatalogPage {
    pub models: Vec<(String, ModelMeta)>,
    /// The cursor for the next page, when there is one.
    pub next_after: Option<String>,
}

/// Read one page of the model list. The shape is not the `{object: list}` one,
/// so it has its own parser. Capability fields are read when present; the
/// current API documentation was not available to check them, so a missing
/// field only means fewer tags.
pub fn parse_catalog_page(bytes: &[u8]) -> Result<CatalogPage> {
    if bytes.len() > 1_048_576 {
        return Err(SeraError::External("model catalog is too large".into()));
    }
    let value: Value = serde_json::from_slice(bytes)
        .map_err(|_| SeraError::External("malformed model catalog".into()))?;
    let entries = value
        .get("data")
        .and_then(Value::as_array)
        .ok_or_else(|| SeraError::External("model catalog has no model list".into()))?;
    let mut models = Vec::new();
    for entry in entries {
        let Some(id) = entry.get("id").and_then(Value::as_str) else {
            continue;
        };
        if !official_model_id(id) {
            continue;
        }
        let supported =
            |pointer: &str| entry.pointer(pointer).and_then(Value::as_bool) == Some(true);
        let mut tags = vec!["tools".to_owned()];
        if supported("/capabilities/image_input/supported") {
            tags.push("vision".into());
        }
        if supported("/capabilities/thinking/supported") {
            tags.push("reasoning".into());
        }
        let name = entry
            .get("display_name")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|name| !name.is_empty() && name.len() <= 120 && *name != id)
            .filter(|name| !name.chars().any(char::is_control))
            .map(str::to_owned);
        models.push((
            id.to_owned(),
            ModelMeta {
                name,
                context_length: entry
                    .get("max_input_tokens")
                    .and_then(Value::as_u64)
                    .filter(|tokens| *tokens > 0),
                max_output_tokens: entry
                    .get("max_tokens")
                    .and_then(Value::as_u64)
                    .filter(|tokens| *tokens > 0),
                tags,
            },
        ));
    }
    let has_more = value.get("has_more").and_then(Value::as_bool) == Some(true);
    let next_after = has_more
        .then(|| {
            value
                .get("last_id")
                .and_then(Value::as_str)
                .filter(|id| official_model_id(id))
                .map(str::to_owned)
        })
        .flatten();
    Ok(CatalogPage { models, next_after })
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::*;

    fn tool() -> ToolSpec {
        ToolSpec {
            name: "plugin.web.lookup".into(),
            description: "look something up".into(),
            parameters: json!({"type": "object", "properties": {"q": {"type": "string"}}}),
            external_effect: false,
        }
    }

    fn options(extra: Option<&Map<String, Value>>, max_tokens: u32) -> RequestOptions<'_> {
        RequestOptions {
            model: "claude-sonnet-5-5",
            max_tokens,
            temperature: Some(0.2),
            extra,
        }
    }

    fn ids(page: &CatalogPage) -> HashSet<&str> {
        page.models.iter().map(|(id, _)| id.as_str()).collect()
    }

    fn call(id: &str, signature: Option<String>) -> ToolCall {
        ToolCall {
            id: id.into(),
            name: "plugin.web.lookup".into(),
            arguments: json!({"q": "x"}),
            reasoning_signature: signature,
        }
    }

    fn tool_result(id: &str, text: &str) -> LlmMessage {
        LlmMessage {
            role: Role::Tool,
            content: Some(text.into()),
            tool_call_id: Some(id.into()),
            ..Default::default()
        }
    }

    fn signed() -> String {
        encode_thinking(&[
            json!({"type": "thinking", "thinking": "hmm", "signature": "sig-AAA=="}),
            json!({"type": "redacted_thinking", "data": "opaque-BBB"}),
        ])
        .unwrap()
    }

    #[test]
    fn the_request_is_a_messages_body() {
        let body = build_request(
            &[LlmMessage::system("be brief"), LlmMessage::user("hi")],
            &[tool()],
            &options(None, 4096),
        );
        assert_eq!(body["model"], "claude-sonnet-5-5");
        assert_eq!(body["max_tokens"], 4096);
        assert_eq!(body["stream"], true);
        assert_eq!(body["system"], "be brief");
        assert_eq!(body["temperature"], json!(0.2f32));
        assert_eq!(
            body["messages"],
            json!([{"role": "user", "content": [{"type": "text", "text": "hi"}]}])
        );
        // The dotted name is mapped and the schema keeps its shape.
        assert_eq!(body["tools"][0]["name"], "plugin_web_lookup");
        assert_eq!(
            body["tools"][0]["input_schema"]["properties"]["q"]["type"],
            "string"
        );
    }

    #[test]
    fn a_tool_round_pairs_calls_with_results_first_in_the_user_turn() {
        let messages = vec![
            LlmMessage::user("look it up"),
            LlmMessage::assistant(Some("on it".into()), vec![call("toolu_1", None)]),
            tool_result("toolu_1", "found"),
            LlmMessage::user("and then?"),
        ];
        let body = build_request(&messages, &[tool()], &options(None, 4096));
        let turns = body["messages"].as_array().unwrap();
        assert_eq!(turns.len(), 3);
        assert_eq!(turns[1]["role"], "assistant");
        assert_eq!(turns[1]["content"][0]["type"], "text");
        assert_eq!(turns[1]["content"][1]["type"], "tool_use");
        assert_eq!(turns[1]["content"][1]["name"], "plugin_web_lookup");
        assert_eq!(turns[2]["role"], "user");
        assert_eq!(turns[2]["content"][0]["type"], "tool_result");
        assert_eq!(turns[2]["content"][0]["tool_use_id"], "toolu_1");
        assert_eq!(turns[2]["content"][1]["text"], "and then?");
    }

    #[test]
    fn orphans_are_dropped_and_missing_results_are_marked() {
        let messages = vec![
            LlmMessage::user("go"),
            LlmMessage::assistant(None, vec![call("toolu_a", None), call("toolu_b", None)]),
            tool_result("toolu_a", "ok"),
            tool_result("ghost", "stale"),
        ];
        let body = build_request(&messages, &[tool()], &options(None, 4096));
        let user = &body["messages"][2]["content"];
        assert_eq!(user.as_array().unwrap().len(), 2);
        assert_eq!(user[0]["tool_use_id"], "toolu_a");
        assert_eq!(user[1]["tool_use_id"], "toolu_b");
        assert_eq!(user[1]["is_error"], true);
        assert!(!body.to_string().contains("stale"));
    }

    #[test]
    fn consecutive_turns_merge_and_the_first_turn_is_the_users() {
        let messages = vec![
            LlmMessage::assistant(Some("hello".into()), Vec::new()),
            LlmMessage::user("one"),
            LlmMessage::user("two"),
        ];
        let body = build_request(&messages, &[], &options(None, 4096));
        let turns = body["messages"].as_array().unwrap();
        assert_eq!(turns[0]["role"], "user");
        assert_eq!(turns[0]["content"][0]["text"], "...");
        assert_eq!(turns[1]["role"], "assistant");
        assert_eq!(turns[2]["role"], "user");
        assert_eq!(turns[2]["content"].as_array().unwrap().len(), 2);
        // Never an empty conversation.
        let empty = build_request(&[LlmMessage::system("s")], &[], &options(None, 4096));
        assert_eq!(empty["messages"][0]["content"][0]["text"], "...");
    }

    #[test]
    fn thinking_clamps_below_max_tokens_and_drops_temperature() {
        let extra = json!({"thinking": {"type": "enabled", "budget_tokens": 24000}});
        let body = build_request(
            &[LlmMessage::user("x")],
            &[],
            &options(extra.as_object(), 8192),
        );
        assert_eq!(
            body["thinking"],
            json!({"type": "enabled", "budget_tokens": 8191})
        );
        assert!(body.get("temperature").is_none());
        // Too little room: thinking is left out and temperature stays.
        let body = build_request(
            &[LlmMessage::user("x")],
            &[],
            &options(extra.as_object(), 1000),
        );
        assert!(body.get("thinking").is_none());
        assert!(body.get("temperature").is_some());
        // `off` says so.
        let off = json!({"thinking": {"type": "disabled"}});
        let body = build_request(
            &[LlmMessage::user("x")],
            &[],
            &options(off.as_object(), 8192),
        );
        assert_eq!(body["thinking"], json!({"type": "disabled"}));
    }

    #[test]
    fn signed_thinking_is_replayed_byte_for_byte_ahead_of_the_call() {
        let messages = vec![
            LlmMessage::user("go"),
            LlmMessage::assistant(
                Some("calling".into()),
                vec![call("toolu_1", Some(signed()))],
            ),
            tool_result("toolu_1", "ok"),
        ];
        let extra = json!({"thinking": {"type": "enabled", "budget_tokens": 4000}});
        let body = build_request(&messages, &[tool()], &options(extra.as_object(), 16000));
        let assistant = &body["messages"][1]["content"];
        assert_eq!(
            assistant[0],
            json!({"type": "thinking", "thinking": "hmm", "signature": "sig-AAA=="})
        );
        assert_eq!(
            assistant[1],
            json!({"type": "redacted_thinking", "data": "opaque-BBB"})
        );
        assert_eq!(assistant[2]["text"], "calling");
        assert_eq!(assistant[3]["type"], "tool_use");
        assert_eq!(body["thinking"]["budget_tokens"], 4000);
    }

    #[test]
    fn a_tool_turn_that_cannot_replay_thinking_leaves_thinking_out() {
        // Another provider's signature is never sent to Anthropic.
        let messages = vec![
            LlmMessage::user("go"),
            LlmMessage::assistant(None, vec![call("toolu_1", Some("Zm9yZWlnbg==".into()))]),
            tool_result("toolu_1", "ok"),
        ];
        let extra = json!({"thinking": {"type": "enabled", "budget_tokens": 4000}});
        let body = build_request(&messages, &[tool()], &options(extra.as_object(), 16000));
        assert!(body.get("thinking").is_none());
        assert!(!body.to_string().contains("Zm9yZWlnbg"));
        assert!(!body.to_string().contains("thinking"));
        // A finished loop (a later real user message) can think again.
        let later = vec![
            LlmMessage::user("go"),
            LlmMessage::assistant(None, vec![call("toolu_1", None)]),
            tool_result("toolu_1", "ok"),
            LlmMessage::assistant(Some("done".into()), Vec::new()),
            LlmMessage::user("next"),
        ];
        let body = build_request(&later, &[tool()], &options(extra.as_object(), 16000));
        assert_eq!(body["thinking"]["type"], "enabled");
    }

    #[test]
    fn damaged_signatures_do_not_decode() {
        assert!(decode_thinking("anthropic.v1:!!!").is_none());
        assert!(decode_thinking("other:abc").is_none());
        let wrong = format!(
            "{SIGNATURE_PREFIX}{}",
            base64::engine::general_purpose::URL_SAFE_NO_PAD
                .encode(serde_json::to_vec(&json!([{"type": "text", "text": "x"}])).unwrap())
        );
        assert!(decode_thinking(&wrong).is_none());
        assert!(decode_thinking(&signed()).is_some());
    }

    #[test]
    fn a_thinking_400_strips_thinking_and_its_blocks() {
        let messages = vec![
            LlmMessage::user("go"),
            LlmMessage::assistant(None, vec![call("toolu_1", Some(signed()))]),
            tool_result("toolu_1", "ok"),
        ];
        let extra = json!({"thinking": {"type": "enabled", "budget_tokens": 4000}});
        let mut body = build_request(&messages, &[tool()], &options(extra.as_object(), 16000));
        assert!(strip_for_bad_request(
            &mut body,
            "thinking.type.enabled is not supported"
        ));
        assert!(body.get("thinking").is_none());
        assert!(!body.to_string().contains("redacted_thinking"));
        assert!(!strip_for_bad_request(&mut body, "thinking again"));
        let mut other = build_request(&messages, &[tool()], &options(extra.as_object(), 16000));
        assert!(!strip_for_bad_request(
            &mut other,
            "messages: roles must alternate"
        ));
        assert!(other.get("thinking").is_some());
    }

    #[test]
    fn every_thinking_level_maps_to_its_budget() {
        let tags = vec!["reasoning".to_owned()];
        for (level, budget) in [
            ("low", 4000),
            ("medium", 10000),
            ("high", 24000),
            ("xhigh", 32000),
        ] {
            let fields =
                crate::thinking::wire_fields("anthropic", "claude-sonnet-5-5", &tags, level)
                    .unwrap();
            let body = build_request(
                &[LlmMessage::user("x")],
                &[],
                &options(Some(&fields), 64000),
            );
            assert_eq!(
                body["thinking"],
                json!({"type": "enabled", "budget_tokens": budget}),
                "{level}"
            );
            assert!(body.get("temperature").is_none(), "{level}");
        }
        // `off` says so, and the temperature is back.
        let off =
            crate::thinking::wire_fields("anthropic", "claude-sonnet-5-5", &tags, "off").unwrap();
        let body = build_request(&[LlmMessage::user("x")], &[], &options(Some(&off), 64000));
        assert_eq!(body["thinking"], json!({"type": "disabled"}));
        assert!(body.get("temperature").is_some());
    }

    #[test]
    fn a_max_tokens_refusal_lowers_the_ceiling_and_the_budget() {
        let extra = json!({"thinking": {"type": "enabled", "budget_tokens": 24000}});
        let mut body = build_request(
            &[LlmMessage::user("x")],
            &[],
            &options(extra.as_object(), 32768),
        );
        let refusal = "max_tokens: 32768 > 8192, which is the maximum allowed number of output tokens for claude-haiku-5";
        assert!(strip_for_bad_request(&mut body, refusal));
        assert_eq!(body["max_tokens"], 8192);
        assert_eq!(body["thinking"]["budget_tokens"], 8191);
        // A ceiling too low for any thinking budget drops thinking.
        let mut body = build_request(
            &[LlmMessage::user("x")],
            &[],
            &options(extra.as_object(), 32768),
        );
        assert!(strip_for_bad_request(&mut body, "max_tokens: 32768 > 1000"));
        assert_eq!(body["max_tokens"], 1000);
        assert!(body.get("thinking").is_none());
        // Already within the ceiling: nothing to repair.
        assert!(!strip_for_bad_request(
            &mut body,
            "max_tokens: 32768 > 8192"
        ));
    }

    fn stream(events: &[(&str, Value)]) -> Vec<u8> {
        let mut out = String::new();
        for (name, data) in events {
            out.push_str(&format!("event: {name}\ndata: {data}\n\n"));
        }
        out.into_bytes()
    }

    #[test]
    fn a_text_stream_is_read_with_usage() {
        let body = stream(&[
            (
                "message_start",
                json!({"type": "message_start", "message": {"usage": {
                    "input_tokens": 10, "cache_read_input_tokens": 90, "output_tokens": 1}}}),
            ),
            ("ping", json!({"type": "ping"})),
            (
                "content_block_start",
                json!({"type": "content_block_start", "index": 0, "content_block": {"type": "text", "text": ""}}),
            ),
            (
                "content_block_delta",
                json!({"type": "content_block_delta", "index": 0, "delta": {"type": "text_delta", "text": "Hel"}}),
            ),
            (
                "content_block_delta",
                json!({"type": "content_block_delta", "index": 0, "delta": {"type": "text_delta", "text": "lo"}}),
            ),
            (
                "content_block_stop",
                json!({"type": "content_block_stop", "index": 0}),
            ),
            (
                "message_delta",
                json!({"type": "message_delta", "delta": {"stop_reason": "end_turn"}, "usage": {"output_tokens": 12}}),
            ),
            ("message_stop", json!({"type": "message_stop"})),
        ]);
        let parsed = parse_stream(&body).unwrap();
        assert_eq!(parsed.text, "Hello");
        assert_eq!(parsed.usage.prompt_tokens, 100);
        assert_eq!(parsed.usage.completion_tokens, 12);
        assert_eq!(parsed.finish_reason.as_deref(), Some("stop"));
        assert!(parsed.tool_calls.is_empty());
    }

    #[test]
    fn a_tool_use_reply_keeps_signed_thinking_for_the_next_request() {
        let body = stream(&[
            (
                "content_block_start",
                json!({"type": "content_block_start", "index": 0, "content_block": {"type": "thinking", "thinking": ""}}),
            ),
            (
                "content_block_delta",
                json!({"type": "content_block_delta", "index": 0, "delta": {"type": "thinking_delta", "thinking": "let me "}}),
            ),
            (
                "content_block_delta",
                json!({"type": "content_block_delta", "index": 0, "delta": {"type": "thinking_delta", "thinking": "see"}}),
            ),
            (
                "content_block_delta",
                json!({"type": "content_block_delta", "index": 0, "delta": {"type": "signature_delta", "signature": "sig-XYZ=="}}),
            ),
            (
                "content_block_start",
                json!({"type": "content_block_start", "index": 1, "content_block": {"type": "redacted_thinking", "data": "opaque"}}),
            ),
            (
                "content_block_start",
                json!({"type": "content_block_start", "index": 2, "content_block": {"type": "tool_use", "id": "toolu_77", "name": "plugin_web_lookup", "input": {}}}),
            ),
            (
                "content_block_delta",
                json!({"type": "content_block_delta", "index": 2, "delta": {"type": "input_json_delta", "partial_json": "{\"q\": \"ca"}}),
            ),
            (
                "content_block_delta",
                json!({"type": "content_block_delta", "index": 2, "delta": {"type": "input_json_delta", "partial_json": "ts\"}"}}),
            ),
            (
                "message_delta",
                json!({"type": "message_delta", "delta": {"stop_reason": "tool_use"}, "usage": {"output_tokens": 30}}),
            ),
        ]);
        let mut parsed = parse_stream(&body).unwrap();
        assert_eq!(parsed.reasoning, "let me see");
        assert_eq!(parsed.finish_reason.as_deref(), Some("tool_calls"));
        restore_tool_names(&mut parsed.tool_calls, &[tool()]);
        let call = &parsed.tool_calls[0];
        assert_eq!(call.id, "toolu_77");
        assert_eq!(call.name, "plugin.web.lookup");
        assert_eq!(call.arguments, json!({"q": "cats"}));
        let blocks = decode_thinking(call.reasoning_signature.as_deref().unwrap()).unwrap();
        assert_eq!(
            blocks,
            vec![
                json!({"type": "thinking", "thinking": "let me see", "signature": "sig-XYZ=="}),
                json!({"type": "redacted_thinking", "data": "opaque"}),
            ]
        );
    }

    #[test]
    fn stream_errors_are_typed_without_the_message() {
        for (kind, status_word) in [
            ("overloaded_error", llm_failure::CODE_OVERLOADED),
            ("rate_limit_error", llm_failure::CODE_RATE_LIMITED),
            (
                "authentication_error",
                llm_failure::CODE_CREDENTIALS_REJECTED,
            ),
            ("permission_error", llm_failure::CODE_ACCOUNT_FORBIDDEN),
            ("not_found_error", llm_failure::CODE_MODEL_UNAVAILABLE),
            ("api_error", llm_failure::CODE_PROVIDER_DOWN),
        ] {
            let body = stream(&[(
                "error",
                json!({"type": "error", "error": {"type": kind, "message": "key sk-secret-1234"}}),
            )]);
            match parse_stream(&body).unwrap_err() {
                SeraError::Upstream { code, message, .. } => {
                    assert_eq!(code, status_word, "{kind}");
                    assert!(!message.contains("sk-secret"), "{kind}");
                }
                other => panic!("{other:?}"),
            }
        }
    }

    #[test]
    fn a_refusal_with_no_content_is_a_content_failure() {
        let body = stream(&[(
            "message_delta",
            json!({"type": "message_delta", "delta": {"stop_reason": "refusal"}, "usage": {"output_tokens": 1}}),
        )]);
        assert!(matches!(
            parse_stream(&body).unwrap_err(),
            SeraError::Upstream {
                code: llm_failure::CODE_CONTENT_FILTERED,
                ..
            }
        ));
        assert!(matches!(
            parse_stream(&stream(&[("ping", json!({"type": "ping"}))])),
            Err(SeraError::External(_))
        ));
    }

    #[test]
    fn a_catalog_page_reads_capabilities_and_the_cursor() {
        let page = parse_catalog_page(
            json!({
                "data": [
                    {"type": "model", "id": "claude-sonnet-5-5", "display_name": "Claude Sonnet 5.5",
                     "max_input_tokens": 1_000_000, "max_tokens": 64000,
                     "capabilities": {"thinking": {"supported": true}, "image_input": {"supported": true}}},
                    {"type": "model", "id": "claude-haiku-5", "display_name": "claude-haiku-5"},
                    {"type": "model", "id": "bad id with spaces"}
                ],
                "has_more": true,
                "last_id": "claude-haiku-5"
            })
            .to_string()
            .as_bytes(),
        )
        .unwrap();
        assert_eq!(
            ids(&page),
            HashSet::from(["claude-sonnet-5-5", "claude-haiku-5"])
        );
        let sonnet = &page.models[0].1;
        assert_eq!(sonnet.context_length, Some(1_000_000));
        assert_eq!(sonnet.max_output_tokens, Some(64000));
        assert!(sonnet.tags.contains(&"reasoning".to_owned()));
        assert!(sonnet.tags.contains(&"vision".to_owned()));
        assert_eq!(sonnet.name.as_deref(), Some("Claude Sonnet 5.5"));
        assert_eq!(page.models[1].1.name, None);
        assert_eq!(page.next_after.as_deref(), Some("claude-haiku-5"));
        let last =
            parse_catalog_page(br#"{"data": [], "has_more": false, "last_id": "x"}"#).unwrap();
        assert!(last.next_after.is_none());
        assert!(parse_catalog_page(b"{}").is_err());
    }
}
