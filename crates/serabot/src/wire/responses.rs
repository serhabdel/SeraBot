//! The OpenAI Responses transport: `POST {base}/responses` over SSE.
//!
//! Meta Muse Code is served this way (reasoning only round-trips on Responses),
//! and so is the Codex (ChatGPT plan) endpoint, which also takes a
//! `prompt_cache_key`. The rules follow the owner's HielCowork client:
//!
//! - `store: false`; the system messages go in `instructions`; `stream: true`.
//! - Tool calls are `function_call` items and tool results `function_call_output`
//!   items. An output whose call was never sent is dropped (the endpoint
//!   rejects orphans).
//! - No `temperature`, no `prompt_cache_key`, no `include`, and no replay of
//!   reasoning items. Thinking is the nested `reasoning: {effort, summary}`.
//! - One 400 may be retried once with the part it blamed removed: the
//!   reasoning block, `max_output_tokens`, a recursive tool schema (replaced
//!   by a flat one) or the hosted `web_search` tool.
//!
//! Everything here is pure: the HTTP loop is in `llm.rs`.

use std::collections::HashSet;

use serde_json::{Map, Value, json};

use crate::domain::{LlmMessage, Role, ToolCall};
use crate::error::{Result, SeraError};
use crate::gateway::ToolSpec;
use crate::llm::Usage;
use crate::llm_failure;

use super::schema;
use super::sse;

/// Efforts the endpoint is known to take. `max` is clamped to `high`.
const EFFORTS: &[&str] = &["low", "medium", "high"];

/// What a request needs beyond the conversation.
pub struct RequestOptions<'a> {
    pub model: &'a str,
    pub max_output_tokens: u32,
    /// The thinking fields from `thinking::wire_fields`; only `reasoning` is used.
    pub extra: Option<&'a Map<String, Value>>,
    /// Offer the hosted `web_search` tool.
    pub web_search: bool,
    /// The conversation's cache key, sent as `prompt_cache_key`. Codex takes it
    /// (the Codex CLI names its conversation there); Muse rejects it, so it is
    /// `None` there.
    pub prompt_cache_key: Option<&'a str>,
}

fn clean_call_id(id: &str, fallback: usize) -> String {
    if !id.is_empty()
        && id.len() <= 64
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-'))
    {
        id.to_owned()
    } else {
        format!("call_{fallback}")
    }
}

/// The request body for one completion.
pub fn build_request(
    messages: &[LlmMessage],
    tools: &[ToolSpec],
    options: &RequestOptions<'_>,
) -> Value {
    let instructions = messages
        .iter()
        .filter(|message| message.role == Role::System)
        .filter_map(|message| message.content.as_deref())
        .filter(|text| !text.trim().is_empty())
        .collect::<Vec<_>>()
        .join("\n\n");
    let mut input: Vec<Value> = Vec::new();
    let mut emitted: HashSet<String> = HashSet::new();
    let mut counter = 0usize;
    // Ids as the conversation names them, to the ids put on the wire.
    let mut renamed: Vec<(String, String)> = Vec::new();
    for message in messages {
        match message.role {
            Role::System => {}
            Role::User => {
                if let Some(text) = message.content.as_deref().filter(|t| !t.is_empty()) {
                    input.push(json!({"role": "user", "content": text}));
                }
            }
            Role::Assistant => {
                if let Some(text) = message.content.as_deref().filter(|t| !t.trim().is_empty()) {
                    input.push(json!({"role": "assistant", "content": text}));
                }
                for call in &message.tool_calls {
                    counter += 1;
                    let wire_id = clean_call_id(&call.id, counter);
                    renamed.push((call.id.clone(), wire_id.clone()));
                    emitted.insert(wire_id.clone());
                    input.push(json!({
                        "type": "function_call",
                        "call_id": wire_id,
                        "name": call.name,
                        "arguments": serde_json::to_string(&call.arguments)
                            .unwrap_or_else(|_| "{}".into()),
                    }));
                }
            }
            Role::Tool => {
                let Some(original) = message.tool_call_id.as_deref() else {
                    continue;
                };
                let wire_id = renamed
                    .iter()
                    .rev()
                    .find(|(from, _)| from == original)
                    .map(|(_, to)| to.clone())
                    .unwrap_or_else(|| original.to_owned());
                // The endpoint rejects an output for a call it never saw.
                if !emitted.contains(&wire_id) {
                    continue;
                }
                input.push(json!({
                    "type": "function_call_output",
                    "call_id": wire_id,
                    "output": message.content.clone().unwrap_or_default(),
                }));
            }
        }
    }
    if input.is_empty() {
        input.push(json!({"role": "user", "content": "..."}));
    }
    let mut body = json!({
        "model": options.model,
        "input": input,
        "stream": true,
        "store": false,
        "max_output_tokens": options.max_output_tokens,
    });
    if !instructions.is_empty() {
        body["instructions"] = json!(instructions);
    }
    if let Some(key) = options.prompt_cache_key.filter(|key| !key.is_empty()) {
        body["prompt_cache_key"] = json!(key);
    }
    let mut wire_tools: Vec<Value> = tools
        .iter()
        .map(|tool| {
            json!({
                "type": "function",
                "name": tool.name,
                "description": tool.description,
                "parameters": schema::flatten(&tool.parameters),
            })
        })
        .collect();
    if options.web_search {
        wire_tools.push(json!({"type": "web_search"}));
    }
    if !wire_tools.is_empty() {
        body["tools"] = Value::Array(wire_tools);
    }
    if let Some(reasoning) = options.extra.and_then(|extra| extra.get("reasoning"))
        && let Some(effort) = reasoning.get("effort").and_then(Value::as_str)
    {
        let effort = if effort == "max" || effort == "xhigh" {
            "high"
        } else {
            effort
        };
        if EFFORTS.contains(&effort) {
            body["reasoning"] = json!({"effort": effort, "summary": "auto"});
        }
    }
    body
}

/// Remove from `body` whatever a 400 blamed, so the request can be sent once
/// more. `error_text` is the provider's own error text. Returns whether
/// anything changed; when nothing did, the 400 is final.
pub fn strip_for_bad_request(body: &mut Value, error_text: &str) -> bool {
    let text = error_text.to_ascii_lowercase();
    let mut changed = false;
    let Some(map) = body.as_object_mut() else {
        return false;
    };
    if (text.contains("reasoning") || text.contains("effort")) && map.remove("reasoning").is_some()
    {
        changed = true;
    }
    if (text.contains("max_output_tokens") || text.contains("max output token"))
        && map.remove("max_output_tokens").is_some()
    {
        changed = true;
    }
    let recursive_schema = text.contains("recursive json schema")
        || (text.contains("invalid_request_error") && text.contains("\"param\":\"parameters\""))
        || (text.contains("invalid_request_error") && text.contains("\"param\": \"parameters\""));
    if recursive_schema && let Some(tools) = map.get_mut("tools").and_then(Value::as_array_mut) {
        for tool in tools {
            if tool.get("type").and_then(Value::as_str) == Some("function")
                && let Some(parameters) = tool.get("parameters")
            {
                let flat = schema::flat_only(parameters);
                if flat != *parameters {
                    tool["parameters"] = flat;
                    changed = true;
                }
            }
        }
    }
    if text.contains("web_search")
        && ["unknown", "unsupported", "invalid", "not support"]
            .iter()
            .any(|word| text.contains(word))
        && let Some(tools) = map.get_mut("tools").and_then(Value::as_array_mut)
    {
        let before = tools.len();
        tools.retain(|tool| tool.get("type").and_then(Value::as_str) != Some("web_search"));
        if tools.len() != before {
            changed = true;
        }
        if tools.is_empty() {
            map.remove("tools");
        }
    }
    changed
}

/// What a finished Responses stream said.
#[derive(Debug, Default)]
pub struct ParsedResponse {
    pub text: String,
    pub reasoning: String,
    pub tool_calls: Vec<ToolCall>,
    pub usage: Usage,
    pub finish_reason: Option<String>,
}

#[derive(Default)]
struct PendingCall {
    item_id: String,
    output_index: Option<u64>,
    call_id: String,
    name: String,
    arguments: String,
}

fn find_call<'a>(
    calls: &'a mut [PendingCall],
    item_id: Option<&str>,
    output_index: Option<u64>,
) -> Option<&'a mut PendingCall> {
    let at = item_id
        .filter(|id| !id.is_empty())
        .and_then(|id| calls.iter().position(|call| call.item_id == id))
        .or_else(|| {
            output_index.and_then(|index| calls.iter().position(|c| c.output_index == Some(index)))
        })?;
    calls.get_mut(at)
}

fn stream_failure(error: &Value) -> SeraError {
    let code: String = error
        .get("code")
        .and_then(Value::as_str)
        .or_else(|| error.get("type").and_then(Value::as_str))
        .unwrap_or("error")
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-'))
        .take(48)
        .collect();
    let lowered = code.to_ascii_lowercase();
    let status = if lowered.contains("rate_limit") {
        429
    } else if lowered.contains("quota") || lowered.contains("billing") {
        402
    } else if lowered.contains("context_length") {
        413
    } else if lowered.contains("overload") {
        529
    } else if lowered.contains("server_error") || lowered.contains("internal") {
        500
    } else {
        400
    };
    // The body is read for its code only; it never reaches the message.
    llm_failure::upstream_error(
        status,
        &error.to_string(),
        None,
        format!("The model provider reported an error ({code})"),
    )
}

fn text_of(value: &Value, key: &str) -> Option<String> {
    value.get(key).and_then(Value::as_str).map(str::to_owned)
}

/// Read a finished SSE body into text, reasoning, tool calls and usage.
pub fn parse_stream(body: &[u8]) -> Result<ParsedResponse> {
    let mut parsed = ParsedResponse::default();
    let mut calls: Vec<PendingCall> = Vec::new();
    let mut completed: Option<Value> = None;
    for event in sse::events(body) {
        if event.data.trim() == "[DONE]" {
            continue;
        }
        let Ok(data) = serde_json::from_str::<Value>(&event.data) else {
            continue;
        };
        let kind = data
            .get("type")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .or(event.event.clone())
            .unwrap_or_default();
        match kind.as_str() {
            "response.output_text.delta" => {
                if let Some(delta) = data.get("delta").and_then(Value::as_str) {
                    parsed.text.push_str(delta);
                }
            }
            "response.reasoning_summary_text.delta" | "response.reasoning_text.delta" => {
                if let Some(delta) = data.get("delta").and_then(Value::as_str) {
                    parsed.reasoning.push_str(delta);
                }
            }
            "response.output_item.added" => {
                if let Some(item) = data.get("item")
                    && item.get("type").and_then(Value::as_str) == Some("function_call")
                {
                    calls.push(PendingCall {
                        item_id: text_of(item, "id").unwrap_or_default(),
                        output_index: data.get("output_index").and_then(Value::as_u64),
                        call_id: text_of(item, "call_id").unwrap_or_default(),
                        name: text_of(item, "name").unwrap_or_default(),
                        arguments: text_of(item, "arguments").unwrap_or_default(),
                    });
                }
            }
            "response.function_call_arguments.delta" => {
                let item_id = text_of(&data, "item_id");
                let index = data.get("output_index").and_then(Value::as_u64);
                if let (Some(call), Some(delta)) = (
                    find_call(&mut calls, item_id.as_deref(), index),
                    data.get("delta").and_then(Value::as_str),
                ) {
                    call.arguments.push_str(delta);
                }
            }
            "response.function_call_arguments.done" => {
                let item_id = text_of(&data, "item_id");
                let index = data.get("output_index").and_then(Value::as_u64);
                if let (Some(call), Some(full)) = (
                    find_call(&mut calls, item_id.as_deref(), index),
                    data.get("arguments").and_then(Value::as_str),
                ) {
                    call.arguments = full.to_owned();
                }
            }
            "response.output_item.done" => {
                if let Some(item) = data.get("item")
                    && item.get("type").and_then(Value::as_str) == Some("function_call")
                {
                    let item_id = text_of(item, "id");
                    let index = data.get("output_index").and_then(Value::as_u64);
                    match find_call(&mut calls, item_id.as_deref(), index) {
                        Some(call) => {
                            if let Some(name) = text_of(item, "name").filter(|n| !n.is_empty()) {
                                call.name = name;
                            }
                            if let Some(id) = text_of(item, "call_id").filter(|n| !n.is_empty()) {
                                call.call_id = id;
                            }
                            if let Some(full) = text_of(item, "arguments").filter(|a| !a.is_empty())
                            {
                                call.arguments = full;
                            }
                        }
                        None => calls.push(PendingCall {
                            item_id: item_id.unwrap_or_default(),
                            output_index: index,
                            call_id: text_of(item, "call_id").unwrap_or_default(),
                            name: text_of(item, "name").unwrap_or_default(),
                            arguments: text_of(item, "arguments").unwrap_or_default(),
                        }),
                    }
                }
            }
            "response.completed" | "response.done" | "response.incomplete" => {
                if kind == "response.incomplete" {
                    let reason = data
                        .pointer("/response/incomplete_details/reason")
                        .and_then(Value::as_str)
                        .unwrap_or("incomplete");
                    parsed.finish_reason = Some(if reason == "max_output_tokens" {
                        "length".into()
                    } else {
                        reason.chars().take(48).collect()
                    });
                }
                completed = data.get("response").cloned().or(Some(data));
            }
            "response.failed" => {
                let error = data
                    .pointer("/response/error")
                    .cloned()
                    .unwrap_or_else(|| json!({"code": "failed"}));
                return Err(stream_failure(&error));
            }
            "error" | "response.error" => {
                let error = data.get("error").cloned().unwrap_or(data);
                return Err(stream_failure(&error));
            }
            _ => {}
        }
    }
    if let Some(response) = &completed {
        if let Some(usage) = response.get("usage") {
            let input = usage
                .get("input_tokens")
                .and_then(Value::as_u64)
                .unwrap_or(0);
            let cached = usage
                .pointer("/input_tokens_details/cached_tokens")
                .and_then(Value::as_u64)
                .unwrap_or(0);
            let output = usage
                .get("output_tokens")
                .and_then(Value::as_u64)
                .unwrap_or(0);
            parsed.usage = Usage {
                prompt_tokens: input.saturating_sub(cached),
                completion_tokens: output,
                total_tokens: input.saturating_sub(cached).saturating_add(output),
            };
        }
        // A reply that came as one finished object, with no deltas.
        if parsed.text.is_empty()
            && calls.is_empty()
            && let Some(items) = response.get("output").and_then(Value::as_array)
        {
            for item in items {
                match item.get("type").and_then(Value::as_str) {
                    Some("message") => {
                        for part in item
                            .get("content")
                            .and_then(Value::as_array)
                            .into_iter()
                            .flatten()
                        {
                            if part.get("type").and_then(Value::as_str) == Some("output_text")
                                && let Some(text) = part.get("text").and_then(Value::as_str)
                            {
                                parsed.text.push_str(text);
                            }
                        }
                    }
                    Some("function_call") => calls.push(PendingCall {
                        item_id: text_of(item, "id").unwrap_or_default(),
                        output_index: None,
                        call_id: text_of(item, "call_id").unwrap_or_default(),
                        name: text_of(item, "name").unwrap_or_default(),
                        arguments: text_of(item, "arguments").unwrap_or_default(),
                    }),
                    _ => {}
                }
            }
        }
    }
    for (index, call) in calls.into_iter().enumerate() {
        if call.name.is_empty() {
            return Err(SeraError::External("tool call missing name".into()));
        }
        let arguments = if call.arguments.trim().is_empty() {
            json!({})
        } else {
            serde_json::from_str(&call.arguments).map_err(|error| {
                SeraError::External(format!("tool call arguments are not JSON: {error}"))
            })?
        };
        let id = if call.call_id.is_empty() {
            if call.item_id.is_empty() {
                format!("call_{index}")
            } else {
                call.item_id
            }
        } else {
            call.call_id
        };
        parsed.tool_calls.push(ToolCall {
            id,
            name: call.name,
            arguments,
            reasoning_signature: None,
        });
    }
    if parsed.finish_reason.is_none() {
        parsed.finish_reason = Some(if parsed.tool_calls.is_empty() {
            "stop".into()
        } else {
            "tool_calls".into()
        });
    }
    if parsed.text.trim().is_empty() && parsed.tool_calls.is_empty() {
        return Err(SeraError::External(
            "The subscription endpoint returned an empty response.".into(),
        ));
    }
    Ok(parsed)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tool() -> ToolSpec {
        ToolSpec {
            name: "lookup".into(),
            description: "look something up".into(),
            parameters: json!({"type": "object", "properties": {"q": {"type": "string"}}}),
            external_effect: false,
        }
    }

    fn options(extra: Option<&Map<String, Value>>) -> RequestOptions<'_> {
        RequestOptions {
            model: "muse-spark-1.3",
            max_output_tokens: 4096,
            extra,
            web_search: false,
            prompt_cache_key: None,
        }
    }

    #[test]
    fn a_cache_key_is_sent_only_when_asked_for() {
        let messages = [LlmMessage::user("hi")];
        let body = build_request(&messages, &[], &options(None));
        assert!(body.get("prompt_cache_key").is_none());
        let keyed = RequestOptions {
            prompt_cache_key: Some("conv-1"),
            ..options(None)
        };
        assert_eq!(
            build_request(&messages, &[], &keyed)["prompt_cache_key"],
            "conv-1"
        );
    }

    fn call(id: &str) -> ToolCall {
        ToolCall {
            id: id.into(),
            name: "lookup".into(),
            arguments: json!({"q": "x"}),
            reasoning_signature: None,
        }
    }

    fn tool_result(id: &str, text: &str) -> LlmMessage {
        LlmMessage {
            role: Role::Tool,
            content: Some(text.into()),
            tool_call_id: Some(id.into()),
            name: Some("lookup".into()),
            ..Default::default()
        }
    }

    #[test]
    fn the_request_follows_the_muse_rules() {
        let extra = json!({"reasoning": {"effort": "medium", "summary": "auto"}});
        let body = build_request(
            &[
                LlmMessage::system("be brief"),
                LlmMessage::system("answer in French"),
                LlmMessage::user("hi"),
            ],
            &[tool()],
            &options(extra.as_object()),
        );
        assert_eq!(body["instructions"], "be brief\n\nanswer in French");
        assert_eq!(body["stream"], true);
        assert_eq!(body["store"], false);
        assert_eq!(body["max_output_tokens"], 4096);
        assert_eq!(body["input"], json!([{"role": "user", "content": "hi"}]));
        assert_eq!(
            body["reasoning"],
            json!({"effort": "medium", "summary": "auto"})
        );
        assert_eq!(body["tools"][0]["type"], "function");
        assert_eq!(body["tools"][0]["name"], "lookup");
        for forbidden in [
            "temperature",
            "prompt_cache_key",
            "include",
            "reasoning_effort",
        ] {
            assert!(body.get(forbidden).is_none(), "{forbidden}");
        }
    }

    #[test]
    fn max_is_clamped_and_unknown_efforts_are_dropped() {
        let max = json!({"reasoning": {"effort": "max"}});
        let body = build_request(&[LlmMessage::user("x")], &[], &options(max.as_object()));
        assert_eq!(body["reasoning"]["effort"], "high");
        let odd = json!({"reasoning": {"effort": "ludicrous"}});
        let body = build_request(&[LlmMessage::user("x")], &[], &options(odd.as_object()));
        assert!(body.get("reasoning").is_none());
        let none = build_request(&[LlmMessage::user("x")], &[], &options(None));
        assert!(none.get("reasoning").is_none());
    }

    #[test]
    fn tool_rounds_become_function_call_items_and_orphans_are_pruned() {
        let messages = vec![
            LlmMessage::user("look it up"),
            LlmMessage::assistant(Some("on it".into()), vec![call("call_1")]),
            tool_result("call_1", "found"),
            // Its call was never sent (a history from another provider).
            tool_result("ghost", "stale"),
        ];
        let body = build_request(&messages, &[], &options(None));
        let input = body["input"].as_array().unwrap();
        assert_eq!(input.len(), 4);
        assert_eq!(input[1], json!({"role": "assistant", "content": "on it"}));
        assert_eq!(input[2]["type"], "function_call");
        assert_eq!(input[2]["call_id"], "call_1");
        assert_eq!(input[2]["arguments"], "{\"q\":\"x\"}");
        assert_eq!(
            input[3],
            json!({"type": "function_call_output", "call_id": "call_1", "output": "found"})
        );
        assert!(!body.to_string().contains("stale"));
    }

    #[test]
    fn an_odd_call_id_is_renamed_on_both_sides() {
        let messages = vec![
            LlmMessage::user("go"),
            LlmMessage::assistant(None, vec![call("call|weird id")]),
            tool_result("call|weird id", "ok"),
        ];
        let body = build_request(&messages, &[], &options(None));
        let input = body["input"].as_array().unwrap();
        let sent = input[1]["call_id"].as_str().unwrap().to_owned();
        assert_eq!(input[2]["call_id"], sent.as_str());
        assert!(sent.chars().all(|c| c.is_ascii_alphanumeric() || c == '_'));
    }

    #[test]
    fn an_empty_conversation_sends_a_placeholder_user_message() {
        let body = build_request(&[LlmMessage::system("only")], &[], &options(None));
        assert_eq!(body["input"], json!([{"role": "user", "content": "..."}]));
    }

    #[test]
    fn each_bad_request_blame_strips_only_its_part() {
        let extra = json!({"reasoning": {"effort": "high"}});
        let hostile = ToolSpec {
            name: "t".into(),
            description: "d".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "list": {"type": "array", "items": {"type": "string"}},
                    "n": {"type": "integer"}
                }
            }),
            external_effect: false,
        };
        let mut with_search = options(extra.as_object());
        with_search.web_search = true;
        let fresh = || {
            build_request(
                &[LlmMessage::user("x")],
                std::slice::from_ref(&hostile),
                &with_search,
            )
        };

        let mut body = fresh();
        assert!(strip_for_bad_request(
            &mut body,
            "Unsupported parameter: reasoning.effort"
        ));
        assert!(body.get("reasoning").is_none());
        assert!(body.get("max_output_tokens").is_some());

        let mut body = fresh();
        assert!(strip_for_bad_request(
            &mut body,
            "unknown parameter max_output_tokens"
        ));
        assert!(body.get("max_output_tokens").is_none());
        assert!(body.get("reasoning").is_some());

        let mut body = fresh();
        assert!(strip_for_bad_request(
            &mut body,
            "Recursive JSON schema is not allowed"
        ));
        assert_eq!(
            body["tools"][0]["parameters"]["properties"]["list"],
            json!({"type": "string"})
        );
        assert!(body.get("reasoning").is_some());

        let mut body = fresh();
        assert!(strip_for_bad_request(
            &mut body,
            "web_search is not supported for this model"
        ));
        assert_eq!(body["tools"].as_array().unwrap().len(), 1);
        assert!(body["tools"][0].get("name").is_some());

        // A 400 about something else is final.
        let mut body = fresh();
        let before = body.clone();
        assert!(!strip_for_bad_request(&mut body, "model not found"));
        assert_eq!(body, before);
        // And stripping is idempotent: nothing left to strip on the retry.
        let mut body = fresh();
        assert!(strip_for_bad_request(&mut body, "reasoning not allowed"));
        assert!(!strip_for_bad_request(&mut body, "reasoning not allowed"));
    }

    fn sse(events: &[(&str, Value)]) -> Vec<u8> {
        let mut out = String::new();
        for (name, data) in events {
            out.push_str(&format!("event: {name}\ndata: {data}\n\n"));
        }
        out.into_bytes()
    }

    #[test]
    fn a_text_stream_with_reasoning_and_usage_is_read() {
        let body = sse(&[
            (
                "response.created",
                json!({"type": "response.created", "response": {"model": "m"}}),
            ),
            (
                "response.reasoning_summary_text.delta",
                json!({"type": "response.reasoning_summary_text.delta", "delta": "thinking..."}),
            ),
            (
                "response.output_text.delta",
                json!({"type": "response.output_text.delta", "delta": "Bon"}),
            ),
            (
                "response.output_text.delta",
                json!({"type": "response.output_text.delta", "delta": "jour"}),
            ),
            (
                "response.completed",
                json!({"type": "response.completed", "response": {
                    "usage": {"input_tokens": 100, "output_tokens": 7,
                              "input_tokens_details": {"cached_tokens": 40}}
                }}),
            ),
        ]);
        let parsed = parse_stream(&body).unwrap();
        assert_eq!(parsed.text, "Bonjour");
        assert_eq!(parsed.reasoning, "thinking...");
        assert_eq!(parsed.usage.prompt_tokens, 60);
        assert_eq!(parsed.usage.completion_tokens, 7);
        assert_eq!(parsed.usage.total_tokens, 67);
        assert_eq!(parsed.finish_reason.as_deref(), Some("stop"));
    }

    #[test]
    fn a_function_call_is_assembled_from_deltas() {
        let body = sse(&[
            (
                "response.output_item.added",
                json!({"type": "response.output_item.added", "output_index": 0,
                       "item": {"type": "function_call", "id": "fc_1", "call_id": "call_9", "name": "lookup", "arguments": ""}}),
            ),
            (
                "response.function_call_arguments.delta",
                json!({"type": "response.function_call_arguments.delta", "item_id": "fc_1", "output_index": 0, "delta": "{\"q\":"}),
            ),
            (
                "response.function_call_arguments.delta",
                json!({"type": "response.function_call_arguments.delta", "item_id": "fc_1", "output_index": 0, "delta": "\"cats\"}"}),
            ),
            (
                "response.completed",
                json!({"type": "response.completed", "response": {}}),
            ),
        ]);
        let parsed = parse_stream(&body).unwrap();
        assert_eq!(parsed.tool_calls.len(), 1);
        assert_eq!(parsed.tool_calls[0].id, "call_9");
        assert_eq!(parsed.tool_calls[0].name, "lookup");
        assert_eq!(parsed.tool_calls[0].arguments, json!({"q": "cats"}));
        assert_eq!(parsed.finish_reason.as_deref(), Some("tool_calls"));
    }

    #[test]
    fn the_done_item_is_authoritative_for_arguments() {
        let body = sse(&[
            (
                "response.output_item.added",
                json!({"type": "response.output_item.added", "output_index": 1,
                       "item": {"type": "function_call", "id": "fc_2", "call_id": "c2", "name": "lookup"}}),
            ),
            (
                "response.output_item.done",
                json!({"type": "response.output_item.done", "output_index": 1,
                       "item": {"type": "function_call", "id": "fc_2", "call_id": "c2", "name": "lookup", "arguments": "{\"q\":\"dogs\"}"}}),
            ),
        ]);
        let parsed = parse_stream(&body).unwrap();
        assert_eq!(parsed.tool_calls[0].arguments, json!({"q": "dogs"}));
    }

    #[test]
    fn a_finished_object_with_no_deltas_is_still_read() {
        let body = sse(&[(
            "response.completed",
            json!({"type": "response.completed", "response": {
                "output": [
                    {"type": "message", "content": [{"type": "output_text", "text": "whole"}]}
                ]
            }}),
        )]);
        assert_eq!(parse_stream(&body).unwrap().text, "whole");
    }

    #[test]
    fn an_incomplete_response_reports_length() {
        let body = sse(&[
            (
                "response.output_text.delta",
                json!({"type": "response.output_text.delta", "delta": "cut"}),
            ),
            (
                "response.incomplete",
                json!({"type": "response.incomplete", "response": {"incomplete_details": {"reason": "max_output_tokens"}}}),
            ),
        ]);
        assert_eq!(
            parse_stream(&body).unwrap().finish_reason.as_deref(),
            Some("length")
        );
    }

    #[test]
    fn an_empty_reply_and_stream_errors_are_typed() {
        let empty = sse(&[(
            "response.completed",
            json!({"type": "response.completed", "response": {}}),
        )]);
        assert!(matches!(parse_stream(&empty), Err(SeraError::External(_))));
        let limited = sse(&[(
            "response.failed",
            json!({"type": "response.failed", "response": {"error": {"code": "rate_limit_exceeded", "message": "secret-detail"}}}),
        )]);
        match parse_stream(&limited).unwrap_err() {
            SeraError::Upstream { code, message, .. } => {
                assert_eq!(code, llm_failure::CODE_RATE_LIMITED);
                assert!(!message.contains("secret-detail"));
            }
            other => panic!("{other:?}"),
        }
        let quota = sse(&[(
            "error",
            json!({"type": "error", "code": "insufficient_quota", "message": "pay up"}),
        )]);
        assert!(matches!(
            parse_stream(&quota).unwrap_err(),
            SeraError::Upstream {
                code: llm_failure::CODE_QUOTA_EXHAUSTED,
                ..
            }
        ));
    }

    #[test]
    fn bad_tool_arguments_are_an_error_not_a_panic() {
        let body = sse(&[(
            "response.output_item.done",
            json!({"type": "response.output_item.done", "output_index": 0,
                   "item": {"type": "function_call", "id": "f", "call_id": "c", "name": "lookup", "arguments": "{nope"}}),
        )]);
        assert!(matches!(parse_stream(&body), Err(SeraError::External(_))));
    }
}
