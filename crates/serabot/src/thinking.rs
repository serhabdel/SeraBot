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
//! - Anthropic: `thinking: {type: enabled, budget_tokens}` on Claude 3.7 and
//!   4.x or later; levels map to budgets of 4k/10k/24k/32k.
//! - Antigravity (Cloud Code): Gemini 3/4 level models and Claude; the level is
//!   carried to the envelope builder, which maps it as HielCowork does.
//! - Muse Code: `reasoning: {effort, summary: auto}`, `off` omits it.
//! - OpenCode Go/Zen: `reasoning_effort`, only on models whose catalog tags
//!   say reasoning.

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
        "anthropic" => {
            if anthropic_thinks(&id) || (id.starts_with("claude-") && has_reasoning_tag) {
                &["off", "low", "medium", "high", "xhigh"]
            } else {
                &[]
            }
        }
        "google-antigravity" => {
            if id.contains("gpt-oss") {
                &[]
            } else if id.starts_with("claude") {
                // `off` sends no thinking config; the rest are token budgets.
                &["off", "low", "medium", "high", "max"]
            } else if id.ends_with("-tiered") {
                &["off", "low", "medium", "high", "max"]
            } else if id.ends_with("-low") || id.ends_with("-medium") || id.ends_with("-high") {
                // The level is baked into the id; no thinking config is sent.
                &[]
            } else if id.starts_with("gemini-3") || id.starts_with("gemini-4") {
                &["off", "low", "medium", "high", "max"]
            } else {
                &[]
            }
        }
        "muse-code" => &["off", "low", "medium", "high"],
        kind if kind.starts_with("opencode-") => {
            // Only what the catalog tags as reasoning; OpenCode serves many
            // vendors' models behind one endpoint.
            if has_reasoning_tag {
                &["low", "medium", "high"]
            } else {
                &[]
            }
        }
        _ => &[],
    };
    rule.to_vec()
}

/// Whether a Claude model id accepts extended thinking: Claude 3.7 and every
/// 4.x or later model. The first number in the id is the major version and a
/// second small number the minor (`claude-3-7-sonnet`, `claude-sonnet-4-5-2025…`).
fn anthropic_thinks(id: &str) -> bool {
    if !id.starts_with("claude-") {
        return false;
    }
    let mut numbers = Vec::new();
    for part in id.split('-').skip(1) {
        match part.parse::<u32>() {
            Ok(value) if value < 100 => numbers.push(value),
            // A date stamp ends the version.
            Ok(_) => break,
            // A family name (`sonnet`) or suffix sits between the numbers.
            Err(_) => {}
        }
        if numbers.len() == 2 {
            break;
        }
    }
    match numbers.as_slice() {
        [major, ..] if *major >= 4 => true,
        [3, 7] => true,
        _ => false,
    }
}

/// The token budget Anthropic's extended thinking gets for `level`. The
/// transport clamps it below `max_tokens`.
pub fn anthropic_budget(level: &str) -> Option<u32> {
    match level {
        "low" => Some(4_000),
        "medium" => Some(10_000),
        "high" => Some(24_000),
        "xhigh" | "max" => Some(32_000),
        _ => None,
    }
}

/// The level a model uses when none is chosen or the old one is unsupported:
/// `medium`, else `high`, else `low`, else the first it offers.
pub fn default_level(levels: &[&'static str]) -> Option<&'static str> {
    ["medium", "high", "low"]
        .into_iter()
        .find_map(|wanted| levels.iter().copied().find(|level| *level == wanted))
        .or_else(|| levels.first().copied())
}

/// The level to keep after the model changed. A level the new model accepts is
/// kept; otherwise the new model's default; a model with no levels clears it.
pub fn level_after_switch(levels: &[&'static str], current: &str) -> Option<&'static str> {
    levels
        .iter()
        .copied()
        .find(|level| *level == current)
        .or_else(|| default_level(levels))
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
        "anthropic" => {
            // The Messages transport clamps the budget below `max_tokens`
            // and drops `temperature` while thinking is on.
            match anthropic_budget(level) {
                Some(budget) => fields.insert(
                    "thinking".into(),
                    json!({"type": "enabled", "budget_tokens": budget}),
                ),
                None => fields.insert("thinking".into(), json!({"type": "disabled"})),
            };
        }
        "google-antigravity" => {
            // A carrier for the Cloud Code envelope builder, which turns it
            // into `thinkingConfig` for the model. It is never sent as is.
            fields.insert("thinking_level".into(), json!(level));
        }
        "muse-code" => {
            // Responses-style effort. Off omits it; `max` is not offered.
            if level != "off" {
                fields.insert(
                    "reasoning".into(),
                    json!({"effort": level, "summary": "auto"}),
                );
            }
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
    fn anthropic_antigravity_muse_and_opencode_rules() {
        let full = ["off", "low", "medium", "high", "xhigh"];
        for model in [
            "claude-3-7-sonnet-20250219",
            "claude-sonnet-4-5-20250929",
            "claude-opus-4-1",
            "claude-haiku-4-5",
            "claude-opus-5-5",
        ] {
            assert_eq!(levels_for("anthropic", model, &none()), full, "{model}");
        }
        for model in [
            "claude-3-5-sonnet-20241022",
            "claude-3-opus-20240229",
            "claude-3-haiku-20240307",
            "claude-2.1",
            "gpt-4o",
        ] {
            assert!(
                levels_for("anthropic", model, &none()).is_empty(),
                "{model}"
            );
        }
        // An unknown Claude id is offered levels only when the catalog says so.
        assert!(levels_for("anthropic", "claude-next", &none()).is_empty());
        assert_eq!(
            levels_for("anthropic", "claude-next", &["reasoning".into()]),
            full
        );

        let agy = ["off", "low", "medium", "high", "max"];
        for model in [
            "gemini-3-pro",
            "gemini-4-flash",
            "gemini-3-pro-tiered",
            "claude-sonnet-4-5-thinking",
            "claude-opus-4-6-thinking",
        ] {
            assert_eq!(
                levels_for("google-antigravity", model, &none()),
                agy,
                "{model}"
            );
        }
        for model in [
            "gemini-3-pro-low",
            "gemini-3-pro-high",
            "gpt-oss-120b-medium",
            "gpt-oss-120b",
            "gemini-2.5-flash",
        ] {
            assert!(
                levels_for("google-antigravity", model, &none()).is_empty(),
                "{model}"
            );
        }

        assert_eq!(
            levels_for("muse-code", "muse-spark", &none()),
            ["off", "low", "medium", "high"]
        );

        let tagged = vec!["reasoning".to_owned()];
        for kind in ["opencode-go", "opencode-zen"] {
            assert_eq!(
                levels_for(kind, "glm-5", &tagged),
                ["low", "medium", "high"]
            );
            assert!(levels_for(kind, "glm-5", &none()).is_empty());
        }
        assert!(levels_for("opencode", "glm-5", &tagged).is_empty());
    }

    #[test]
    fn new_provider_wire_shapes() {
        let shape = |kind: &str, model: &str, level: &str| {
            wire_fields(kind, model, &["reasoning".into()], level).map(Value::Object)
        };
        assert_eq!(
            shape("anthropic", "claude-sonnet-4-5", "low"),
            Some(json!({"thinking": {"type": "enabled", "budget_tokens": 4000}}))
        );
        assert_eq!(
            shape("anthropic", "claude-sonnet-4-5", "medium"),
            Some(json!({"thinking": {"type": "enabled", "budget_tokens": 10000}}))
        );
        assert_eq!(
            shape("anthropic", "claude-sonnet-4-5", "high"),
            Some(json!({"thinking": {"type": "enabled", "budget_tokens": 24000}}))
        );
        assert_eq!(
            shape("anthropic", "claude-sonnet-4-5", "xhigh"),
            Some(json!({"thinking": {"type": "enabled", "budget_tokens": 32000}}))
        );
        assert_eq!(
            shape("anthropic", "claude-sonnet-4-5", "off"),
            Some(json!({"thinking": {"type": "disabled"}}))
        );
        assert_eq!(
            wire_fields("anthropic", "claude-3-5-sonnet", &[], "high"),
            None
        );
        assert_eq!(
            shape("google-antigravity", "gemini-3-pro", "max"),
            Some(json!({"thinking_level": "max"}))
        );
        assert_eq!(
            shape("google-antigravity", "gemini-3-pro-high", "high"),
            None
        );
        assert_eq!(
            shape("muse-code", "muse-spark", "high"),
            Some(json!({"reasoning": {"effort": "high", "summary": "auto"}}))
        );
        // Muse `off` omits reasoning; `max` is never offered.
        assert_eq!(shape("muse-code", "muse-spark", "off"), Some(json!({})));
        assert_eq!(shape("muse-code", "muse-spark", "max"), None);
        assert_eq!(
            shape("opencode-go", "glm-5", "medium"),
            Some(json!({"reasoning_effort": "medium"}))
        );
        assert_eq!(
            wire_fields("opencode-zen", "glm-5", &[], "medium"),
            None,
            "no reasoning tag, no parameter"
        );
    }

    #[test]
    fn a_model_switch_keeps_a_supported_level_or_takes_the_default() {
        let claude = levels_for("anthropic", "claude-sonnet-4-5", &none());
        assert_eq!(level_after_switch(&claude, "high"), Some("high"));
        let deepseek = levels_for("deepseek", "deepseek-flash", &none());
        // `medium` is not offered there: the default is the next best.
        assert_eq!(level_after_switch(&deepseek, "medium"), Some("high"));
        assert_eq!(level_after_switch(&deepseek, "max"), Some("max"));
        let mistral = levels_for("mistral", "mistral-small-2603", &none());
        assert_eq!(level_after_switch(&mistral, "xhigh"), Some("high"));
        let xai = levels_for("xai", "grok-3-mini", &none());
        assert_eq!(level_after_switch(&xai, "medium"), Some("high"));
        assert_eq!(level_after_switch(&xai, "low"), Some("low"));
        // A model with no levels clears the choice.
        assert_eq!(level_after_switch(&[], "high"), None);
        assert_eq!(default_level(&["off"]), Some("off"));
        assert_eq!(default_level(&["minimal", "low", "high"]), Some("high"));
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
