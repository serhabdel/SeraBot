//! Thinking levels: which models can be told how hard to think, how each
//! provider wants that said on the wire, and where a provider puts the
//! reasoning text it returns.
//!
//! Detection is conservative. A level is offered only when a documented rule
//! (or, for OpenRouter, the catalog's own `reasoning` capability) says the
//! model accepts it; everything else is left to the provider's default, so a
//! request is never sent with a parameter the model would reject.
//!
//! Sources (checked against the official docs, 2026):
//! - OpenAI: `reasoning_effort` (`none`/`minimal`/`low`/`medium`/`high`/`xhigh`,
//!   model dependent).
//! - Mistral: `reasoning_effort` on Small 4 and Medium 3.5 (`none`/`high`),
//!   Large 4 (`low`/`medium`/`high`), GLM (`low`/`high`/`max`); the answer comes
//!   back as a chunk list with `thinking` chunks. Earlier Magistral reasons
//!   always and takes no level.
//! - OpenRouter: `reasoning: {effort}`; `reasoning` in `supported_parameters`.
//! - Gemini (OpenAI-compatible endpoint): `reasoning_effort`.
//! - Groq: `reasoning_effort` (GPT-OSS `low`/`medium`/`high`; Qwen adds
//!   `none`) and `reasoning_format: parsed` to get a separate reasoning field.
//! - xAI: `reasoning_effort` (`low`/`medium`/`high`, `xhigh` on 4.6+) for
//!   grok-3-mini and grok-4.5+; reasoning cannot be turned off.
//! - DeepSeek: `reasoning_effort` plus `thinking: {type}`; `deepseek-reasoner`
//!   takes no level and returns `reasoning_content`.

use serde_json::{Map, Value, json};

/// Every level a user can pick, weakest first. `off` turns thinking off.
pub const ALL_LEVELS: &[&str] = &["off", "minimal", "low", "medium", "high", "xhigh", "max"];

/// Whether `level` is a level name this server knows.
pub fn is_known_level(level: &str) -> bool {
    ALL_LEVELS.contains(&level)
}

/// The levels a model accepts, in picker order. Empty means the model has no
/// thinking control (it may still return reasoning text).
pub fn levels_for(kind: &str, model: &str, tags: &[String]) -> Vec<&'static str> {
    let id = model.to_ascii_lowercase();
    let id = id.rsplit('/').next().unwrap_or(&id).to_owned();
    let has_reasoning_tag = tags.iter().any(|tag| tag.eq_ignore_ascii_case("reasoning"));
    let rule: &[&'static str] = match kind {
        "openai" => {
            if id.starts_with("gpt-5-chat") || id.contains("chat-latest") {
                &[]
            } else if id.starts_with("o1") || id.starts_with("o3") || id.starts_with("o4") {
                &["low", "medium", "high"]
            } else if id.starts_with("gpt-5.") || id.starts_with("gpt-6") {
                &["off", "low", "medium", "high", "xhigh"]
            } else if id.starts_with("gpt-5") {
                &["minimal", "low", "medium", "high"]
            } else {
                &[]
            }
        }
        "mistral" => {
            if id.contains("mistral-small-2603") || id.contains("mistral-medium-3-5") {
                &["off", "high"]
            } else if id.contains("mistral-large-4") {
                &["low", "medium", "high"]
            } else if id.contains("zai-glm-5") {
                &["low", "high", "max"]
            } else {
                &[]
            }
        }
        "openrouter" => {
            if has_reasoning_tag {
                &["low", "medium", "high"]
            } else {
                &[]
            }
        }
        "gemini" => {
            if id.starts_with("gemini-2.5-pro") {
                &["low", "medium", "high"]
            } else if id.starts_with("gemini-2.5-flash") {
                &["off", "low", "medium", "high"]
            } else if id.starts_with("gemini-3") || id.starts_with("gemini-4") {
                &["low", "medium", "high"]
            } else {
                &[]
            }
        }
        "groq" => {
            if id.starts_with("gpt-oss") {
                &["low", "medium", "high"]
            } else if id.contains("qwen3") {
                &["off", "low", "medium", "high"]
            } else if id.contains("muse-spark") {
                &["low", "medium", "high", "xhigh"]
            } else {
                &[]
            }
        }
        "xai" => {
            if id.starts_with("grok-3-mini") {
                &["low", "high"]
            } else if id.starts_with("grok-4.6") || id.starts_with("grok-4.7") {
                &["low", "medium", "high", "xhigh"]
            } else if id.starts_with("grok-4.5") {
                &["low", "medium", "high"]
            } else {
                &[]
            }
        }
        "deepseek" => {
            if id == "deepseek-flash" || id.starts_with("deepseek-v4") {
                &["off", "low", "high", "max"]
            } else {
                // deepseek-reasoner always thinks and takes no level.
                &[]
            }
        }
        _ => &[],
    };
    rule.to_vec()
}

/// Whether `level` is offered for the model.
pub fn supports(kind: &str, model: &str, tags: &[String], level: &str) -> bool {
    levels_for(kind, model, tags).contains(&level)
}

/// The extra top-level request fields that apply `level`, or `None` when the
/// model does not support it (the request is then sent untouched).
pub fn wire_fields(
    kind: &str,
    model: &str,
    tags: &[String],
    level: &str,
) -> Option<Map<String, Value>> {
    if !supports(kind, model, tags, level) {
        return None;
    }
    let mut fields = Map::new();
    match kind {
        "openrouter" => {
            fields.insert("reasoning".into(), json!({"effort": level}));
        }
        "deepseek" => {
            if level == "off" {
                fields.insert("thinking".into(), json!({"type": "disabled"}));
            } else {
                fields.insert("thinking".into(), json!({"type": "enabled"}));
                fields.insert("reasoning_effort".into(), json!(level));
            }
        }
        _ => {
            // OpenAI-style `reasoning_effort`; `off` is spelled `none`.
            let value = if level == "off" { "none" } else { level };
            fields.insert("reasoning_effort".into(), json!(value));
            if kind == "groq" && level != "off" && !model.to_ascii_lowercase().contains("gpt-oss") {
                // Ask for the reasoning in its own field, not inline tags.
                fields.insert("reasoning_format".into(), json!("parsed"));
            }
        }
    }
    Some(fields)
}

/// Longest reasoning text kept from one model reply.
pub const MAX_REASONING_CHARS: usize = 8_000;

/// Pull the visible answer and any reasoning text out of one chat message.
///
/// Handles a plain string `content`, Mistral's chunk list (`thinking` and
/// `text` chunks), a separate `reasoning` / `reasoning_content` field
/// (OpenRouter, DeepSeek, xAI, Groq, Gemini), OpenRouter `reasoning_details`,
/// and inline `<think>` tags. Reasoning is untrusted display data.
pub fn split_message(message: &Value) -> (Option<String>, Option<String>) {
    let mut reasoning = String::new();

    let content = match message.get("content") {
        Some(Value::String(text)) => Some(text.clone()),
        Some(Value::Array(chunks)) => {
            let mut answer = String::new();
            for chunk in chunks {
                match chunk.get("type").and_then(Value::as_str) {
                    Some("thinking") => match chunk.get("thinking") {
                        Some(Value::String(text)) => push_text(&mut reasoning, text),
                        Some(Value::Array(parts)) => {
                            for part in parts {
                                if let Some(text) = part.get("text").and_then(Value::as_str) {
                                    push_text(&mut reasoning, text);
                                }
                            }
                        }
                        _ => {}
                    },
                    Some("text") | None => {
                        if let Some(text) = chunk.get("text").and_then(Value::as_str) {
                            answer.push_str(text);
                        }
                    }
                    _ => {}
                }
            }
            Some(answer)
        }
        _ => None,
    };
    for key in ["reasoning_content", "reasoning"] {
        if let Some(text) = message.get(key).and_then(Value::as_str) {
            push_text(&mut reasoning, text);
            break;
        }
    }
    if reasoning.is_empty()
        && let Some(details) = message.get("reasoning_details").and_then(Value::as_array)
    {
        for detail in details {
            for key in ["text", "summary"] {
                if let Some(text) = detail.get(key).and_then(Value::as_str) {
                    push_text(&mut reasoning, text);
                    break;
                }
            }
        }
    }
    let content = content.map(|text| strip_think_tags(&text, &mut reasoning));
    let reasoning = (!reasoning.is_empty()).then(|| {
        reasoning
            .chars()
            .filter(|c| !c.is_control() || matches!(c, '\n' | '\t'))
            .take(MAX_REASONING_CHARS)
            .collect()
    });
    (content, reasoning)
}

fn push_text(buffer: &mut String, text: &str) {
    let text = text.trim();
    if text.is_empty() {
        return;
    }
    if !buffer.is_empty() {
        buffer.push('\n');
    }
    buffer.push_str(text);
}

/// Remove inline `<think>…</think>` blocks from an answer, handing their text
/// to `push`. An unterminated block is left alone.
fn strip_think_tags(text: &str, reasoning: &mut String) -> String {
    let mut out = String::new();
    let mut rest = text;
    while let Some(start) = rest.find("<think>") {
        let after = &rest[start + "<think>".len()..];
        let Some(end) = after.find("</think>") else {
            break;
        };
        out.push_str(&rest[..start]);
        push_text(reasoning, &after[..end]);
        rest = &after[end + "</think>".len()..];
    }
    out.push_str(rest);
    if out.len() != text.len() {
        out = out.trim_start().to_owned();
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn none() -> Vec<String> {
        Vec::new()
    }

    #[test]
    fn rule_table_matches_the_documented_levels() {
        let cases: &[(&str, &str, &[&str])] = &[
            ("openai", "o3-mini", &["low", "medium", "high"]),
            ("openai", "gpt-5", &["minimal", "low", "medium", "high"]),
            (
                "openai",
                "gpt-5.5",
                &["off", "low", "medium", "high", "xhigh"],
            ),
            ("openai", "gpt-4o", &[]),
            ("openai", "gpt-5-chat-latest", &[]),
            ("mistral", "mistral-small-2603", &["off", "high"]),
            ("mistral", "mistral-medium-3-5", &["off", "high"]),
            ("mistral", "mistral-large-4-0", &["low", "medium", "high"]),
            ("mistral", "zai-glm-5-3", &["low", "high", "max"]),
            ("mistral", "magistral-medium-2509", &[]),
            ("mistral", "mistral-large-latest", &[]),
            ("gemini", "gemini-2.5-pro", &["low", "medium", "high"]),
            (
                "gemini",
                "gemini-2.5-flash",
                &["off", "low", "medium", "high"],
            ),
            ("gemini", "gemini-3-pro-preview", &["low", "medium", "high"]),
            ("groq", "openai/gpt-oss-20b", &["low", "medium", "high"]),
            ("groq", "qwen/qwen3-32b", &["off", "low", "medium", "high"]),
            ("groq", "llama-3.3-70b-versatile", &[]),
            ("xai", "grok-3-mini", &["low", "high"]),
            ("xai", "grok-4.5", &["low", "medium", "high"]),
            ("xai", "grok-4.7", &["low", "medium", "high", "xhigh"]),
            ("xai", "grok-4", &[]),
            ("deepseek", "deepseek-reasoner", &[]),
            ("deepseek", "deepseek-flash", &["off", "low", "high", "max"]),
            ("together", "anything", &[]),
        ];
        for (kind, model, expected) in cases {
            assert_eq!(
                levels_for(kind, model, &none()),
                *expected,
                "{kind} {model}"
            );
        }
    }

    #[test]
    fn openrouter_uses_the_catalog_reasoning_tag() {
        let tagged = vec!["tools".to_owned(), "reasoning".to_owned()];
        assert_eq!(
            levels_for("openrouter", "vendor/model", &tagged),
            ["low", "medium", "high"]
        );
        assert!(levels_for("openrouter", "vendor/model", &["tools".into()]).is_empty());
        // The tag alone grants nothing on a provider with no documented wire.
        assert!(levels_for("together", "m", &tagged).is_empty());
    }

    #[test]
    fn request_shaping_per_provider() {
        let shape = |kind: &str, model: &str, level: &str| {
            wire_fields(kind, model, &["reasoning".into()], level).map(Value::Object)
        };
        assert_eq!(
            shape("openai", "gpt-5.5", "high"),
            Some(json!({"reasoning_effort": "high"}))
        );
        assert_eq!(
            shape("openai", "gpt-5.5", "off"),
            Some(json!({"reasoning_effort": "none"}))
        );
        assert_eq!(
            shape("mistral", "mistral-small-2603", "off"),
            Some(json!({"reasoning_effort": "none"}))
        );
        assert_eq!(
            shape("openrouter", "a/b", "medium"),
            Some(json!({"reasoning": {"effort": "medium"}}))
        );
        assert_eq!(
            shape("groq", "qwen/qwen3-32b", "low"),
            Some(json!({"reasoning_effort": "low", "reasoning_format": "parsed"}))
        );
        assert_eq!(
            shape("groq", "openai/gpt-oss-120b", "low"),
            Some(json!({"reasoning_effort": "low"}))
        );
        assert_eq!(
            shape("xai", "grok-3-mini", "high"),
            Some(json!({"reasoning_effort": "high"}))
        );
        assert_eq!(
            shape("deepseek", "deepseek-flash", "off"),
            Some(json!({"thinking": {"type": "disabled"}}))
        );
        assert_eq!(
            shape("deepseek", "deepseek-flash", "max"),
            Some(json!({"thinking": {"type": "enabled"}, "reasoning_effort": "max"}))
        );
    }

    #[test]
    fn unsupported_levels_and_models_are_never_sent() {
        // xAI cannot turn reasoning off; Mistral Large 4 takes no `off`.
        assert!(wire_fields("xai", "grok-3-mini", &[], "off").is_none());
        assert!(wire_fields("mistral", "mistral-large-4-0", &[], "off").is_none());
        assert!(wire_fields("deepseek", "deepseek-reasoner", &[], "high").is_none());
        assert!(wire_fields("mistral", "mistral-large-latest", &[], "high").is_none());
        assert!(wire_fields("openai", "gpt-5.5", &[], "bogus").is_none());
    }

    #[test]
    fn splits_every_reasoning_shape() {
        // Mistral chunk list.
        let (content, reasoning) = split_message(&json!({
            "content": [
                {"type": "thinking", "thinking": [{"type": "text", "text": "step one"}]},
                {"type": "text", "text": "The answer."}
            ]
        }));
        assert_eq!(content.as_deref(), Some("The answer."));
        assert_eq!(reasoning.as_deref(), Some("step one"));
        // DeepSeek / xAI.
        let (content, reasoning) =
            split_message(&json!({"content": "42", "reasoning_content": "because"}));
        assert_eq!(
            (content.as_deref(), reasoning.as_deref()),
            (Some("42"), Some("because"))
        );
        // OpenRouter / Groq parsed.
        let (_, reasoning) = split_message(&json!({"content": "x", "reasoning": "hm"}));
        assert_eq!(reasoning.as_deref(), Some("hm"));
        // OpenRouter details.
        let (_, reasoning) = split_message(&json!({
            "content": "x",
            "reasoning_details": [{"type": "reasoning.text", "text": "detail"}]
        }));
        assert_eq!(reasoning.as_deref(), Some("detail"));
        // Inline tags are lifted out of the answer.
        let (content, reasoning) = split_message(&json!({"content": "<think>musing</think>Hello"}));
        assert_eq!(content.as_deref(), Some("Hello"));
        assert_eq!(reasoning.as_deref(), Some("musing"));
        // Plain answers carry no reasoning.
        let (content, reasoning) = split_message(&json!({"content": "hi"}));
        assert_eq!((content.as_deref(), reasoning), (Some("hi"), None));
        // Unterminated tags are left alone.
        let (content, _) = split_message(&json!({"content": "<think>open"}));
        assert_eq!(content.as_deref(), Some("<think>open"));
    }

    #[test]
    fn reasoning_text_is_bounded_and_stripped_of_control_characters() {
        let long = format!("a\u{7}{}", "x".repeat(MAX_REASONING_CHARS * 2));
        let (_, reasoning) = split_message(&json!({"content": "x", "reasoning": long}));
        let reasoning = reasoning.unwrap();
        assert!(reasoning.chars().count() <= MAX_REASONING_CHARS);
        assert!(!reasoning.contains('\u{7}'));
    }
}
