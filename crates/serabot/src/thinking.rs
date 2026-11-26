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
            } else if id.starts_with("gpt-6-astra") || id.starts_with("gpt-6.1-sol") {
                // OpenAI's models page: no `none`, and `max` on top.
                &["low", "medium", "high", "xhigh", "max"]
            } else if id.starts_with("gpt-6.1-terra") || id.starts_with("gpt-6-luna") {
                &["off", "low", "medium", "high", "xhigh", "max"]
            } else if id.starts_with("gpt-5.") || id.starts_with("gpt-6") {
                &["off", "low", "medium", "high", "xhigh"]
            } else if id.starts_with("gpt-5") {
                &["minimal", "low", "medium", "high"]
            } else {
                &[]
            }
        }
        "mistral" => {
            if id.contains("mistral-small-26") || id.contains("mistral-medium-3") {
                &["off", "high"]
            } else if id.contains("mistral-large-4") {
                &["low", "medium", "high"]
            } else if id.contains("zai-glm") {
                &["low", "high", "max"]
            } else if id.starts_with("deepseek-v4") || id == "deepseek-flash" {
                &["off", "low", "high", "max"]
            } else if has_reasoning_tag && !id.contains("magistral") {
                &["low", "medium", "high"]
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
            if is_gemini_pro_or_argon(&id) {
                &["low", "medium", "high"]
            } else if id.starts_with("gemini-2.5-flash")
                || id.starts_with("gemini-3-flash")
                || id.starts_with("gemini-3.8-flash")
                || id.starts_with("gemini-4-flash")
            {
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
            } else if id.starts_with("grok-4.6")
                || id.starts_with("grok-4.7")
                || id.starts_with("grok-4.8")
                || id.starts_with("grok-5")
            {
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
        "anthropic" | "claude-agent-sdk" => {
            if is_always_on_claude(&id) {
                // Anthropic marks these "always on": thinking cannot be off.
                &["low", "medium", "high", "xhigh", "max"]
            } else if is_non_thinking_claude(&id) {
                &[]
            } else if anthropic_thinks(&id) || (id.starts_with("claude-") && has_reasoning_tag) {
                &["off", "low", "medium", "high", "xhigh"]
            } else {
                &[]
            }
        }
        "google-antigravity" => {
            if id == "gpt-oss-120b" {
                &["medium"]
            } else if id.contains("gpt-oss") {
                &[]
            } else if id.starts_with("claude-sonnet-4-6") || id.starts_with("claude-opus-4-6") {
                &["high"]
            } else if id.starts_with("claude") {
                if is_always_on_claude(&id) {
                    &["low", "medium", "high", "max"]
                } else if is_non_thinking_claude(&id) {
                    &[]
                } else if id.ends_with("-thinking")
                    || id.starts_with("claude-sonnet-4-5")
                    || id.starts_with("claude-opus-4-5")
                    || id.starts_with("claude-sonnet-5-")
                    || id.starts_with("claude-haiku-4-5")
                    || id.starts_with("claude-haiku-5-")
                    || anthropic_thinks(&id)
                {
                    // `off` sends no thinking config; the rest are token budgets.
                    &["off", "low", "medium", "high", "max"]
                } else {
                    &[]
                }
            } else if is_gemini_suffixed(&id) {
                // The level is baked into the id; no thinking config is sent.
                &[]
            } else if id.ends_with("-tiered") {
                &["off", "low", "medium", "high"]
            } else if id.starts_with("gemini-3.1-pro") {
                &["low", "high"]
            } else if id.starts_with("gemini-3.8-flash")
                || id.starts_with("gemini-3.7-flash")
                || id.starts_with("gemini-3.6-flash")
                || id.starts_with("gemini-3.5-flash")
            {
                &["low", "medium", "high"]
            } else if id.starts_with("gemini-4") {
                &["low", "medium", "high", "max"]
            } else if is_gemini_pro_or_argon(&id) {
                &["low", "medium", "high"]
            } else if id.starts_with("gemini-2.5-flash") || id.starts_with("gemini-3") {
                &["off", "low", "medium", "high"]
            } else {
                &[]
            }
        }
        "github-copilot" => {
            if id.starts_with("o1") || id.starts_with("o3") || id.starts_with("o4") {
                &["low", "medium", "high"]
            } else if id.starts_with("gpt-5.") || id.starts_with("gpt-6") {
                &["off", "low", "medium", "high"]
            } else if id.starts_with("gpt-5") && !id.starts_with("gpt-5-chat") {
                &["minimal", "low", "medium", "high"]
            } else if is_gemini_pro_or_argon(&id) {
                &["low", "medium", "high"]
            } else if id.starts_with("gemini-2.5-flash")
                || id.starts_with("gemini-3")
                || id.starts_with("gemini-4")
            {
                &["off", "low", "medium", "high"]
            } else if !is_non_thinking_claude(&id) && (anthropic_thinks(&id) || has_reasoning_tag) {
                &["low", "medium", "high"]
            } else {
                &[]
            }
        }
        // Both take the Responses `reasoning.effort`; `max` clamps to high.
        "muse-code" | "openai-codex" => &["off", "low", "medium", "high"],
        "kiro" => {
            if id.starts_with("claude-")
                && !is_non_thinking_claude(&id)
                && (anthropic_thinks(&id) || id.contains("thinking") || has_reasoning_tag)
            {
                &["off", "low", "medium", "high", "max"]
            } else if has_reasoning_tag {
                &["off", "low", "medium", "high", "xhigh", "max"]
            } else {
                &[]
            }
        }
        "devin" => {
            if id.starts_with("swe-2") || is_always_on_claude(&id) {
                &["low", "medium", "high", "max"]
            } else if is_non_thinking_claude(&id) {
                &[]
            } else if anthropic_thinks(&id) {
                &["off", "low", "medium", "high", "max"]
            } else if id.starts_with("gpt-5-chat") || id.contains("chat-latest") {
                &[]
            } else if id.starts_with("o1") || id.starts_with("o3") || id.starts_with("o4") {
                &["low", "medium", "high"]
            } else if id.starts_with("gpt-6-astra") || id.starts_with("gpt-6.1-sol") {
                &["low", "medium", "high", "xhigh", "max"]
            } else if id.starts_with("gpt-6.1-terra") || id.starts_with("gpt-6-luna") {
                &["off", "low", "medium", "high", "xhigh", "max"]
            } else if id.starts_with("gpt-5.") || id.starts_with("gpt-6") {
                &["off", "low", "medium", "high", "xhigh"]
            } else if id.starts_with("gpt-5") {
                &["minimal", "low", "medium", "high"]
            } else if is_gemini_pro_or_argon(&id) {
                &["low", "medium", "high"]
            } else if id.starts_with("gemini-2.5-flash")
                || id.starts_with("gemini-3")
                || id.starts_with("gemini-4")
            {
                &["off", "low", "medium", "high"]
            } else if has_reasoning_tag {
                &["low", "medium", "high"]
            } else {
                &[]
            }
        }
        "moonshot" => {
            if id.starts_with("kimi-k2-thinking")
                || id.starts_with("kimi-k2.5")
                || id.starts_with("kimi-k2-5")
            {
                &["off", "low", "medium", "high"]
            } else {
                &[]
            }
        }
        "inception" => {
            if id.starts_with("mercury-2") || id.starts_with("mercury-3") {
                &["off", "low", "medium", "high"]
            } else {
                &[]
            }
        }
        "ollama" => {
            if id.starts_with("deepseek-r1")
                || id.starts_with("qwen3")
                || id.starts_with("qwq")
                || id.starts_with("gpt-oss")
                || has_reasoning_tag
            {
                &["off", "low", "medium", "high"]
            } else {
                &[]
            }
        }
        "cerebras" | "together" | "fireworks" | "minimax" | "minimax-cn" | "nebius"
        | "sambanova" => {
            if id.contains("qwen3") {
                &["off", "low", "medium", "high"]
            } else if has_reasoning_tag
                || id.contains("deepseek-r1")
                || id.contains("gpt-oss")
                || id.contains("kimi-k2")
                || id.contains("minimax-m2")
                || id.contains("minimax-m3")
            {
                &["low", "medium", "high"]
            } else {
                &[]
            }
        }
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

fn is_always_on_claude(id: &str) -> bool {
    id.starts_with("claude-opus-5-5")
        || id.starts_with("claude-fable-5-2")
        || id.starts_with("claude-opus-4-6")
}

fn is_non_thinking_claude(id: &str) -> bool {
    id.starts_with("claude-3-5")
        || id.starts_with("claude-3.5")
        || id.starts_with("claude-haiku-3")
        || id.starts_with("claude-3-haiku")
        || id.starts_with("claude-3-sonnet")
        || id.starts_with("claude-3-opus")
        || id.starts_with("claude-2")
}

fn is_gemini_pro_or_argon(id: &str) -> bool {
    id.starts_with("gemini-2.5-pro")
        || id.starts_with("gemini-3-pro")
        || id.starts_with("gemini-3.1-pro")
        || id.starts_with("gemini-4-pro")
        || id.starts_with("gemini-4-argon")
}

fn is_gemini_suffixed(id: &str) -> bool {
    id.ends_with("-low")
        || id.ends_with("-medium")
        || id.ends_with("-high")
        || id.ends_with("-minimal")
        || id.ends_with("-xhigh")
        || id.ends_with("-max")
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
        "anthropic" | "claude-agent-sdk" => {
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
        "muse-code" | "openai-codex" => {
            // Responses-style effort. Off omits it; `max` is not offered.
            if level != "off" {
                fields.insert(
                    "reasoning".into(),
                    json!({"effort": level, "summary": "auto"}),
                );
            }
        }
        "deepseek" | "moonshot" => {
            if level == "off" {
                fields.insert("thinking".into(), json!({"type": "disabled"}));
            } else {
                fields.insert("thinking".into(), json!({"type": "enabled"}));
                fields.insert("reasoning_effort".into(), json!(level));
            }
        }
        "groq" => {
            let value = if level == "off" { "none" } else { level };
            fields.insert("reasoning_effort".into(), json!(value));
            if level != "off" && !model.to_ascii_lowercase().contains("gpt-oss") {
                // Ask for the reasoning in its own field, not inline tags.
                fields.insert("reasoning_format".into(), json!("parsed"));
            }
        }
        "kiro" => {
            if level != "off" {
                fields.insert("thinkingLevel".into(), json!(level));
            }
        }
        "devin" => {
            if level != "off" {
                fields.insert(
                    "thinkingConfig".into(),
                    json!({"enabled": true, "effort": level}),
                );
            }
        }
        "openai" | "gemini" | "mistral" | "xai" | "github-copilot" | "inception" | "ollama"
        | "cerebras" | "together" | "fireworks" | "minimax" | "minimax-cn" | "nebius"
        | "sambanova" => {
            // OpenAI-style `reasoning_effort`; `off` is spelled `none`.
            let value = if level == "off" { "none" } else { level };
            fields.insert("reasoning_effort".into(), json!(value));
        }
        _ => {
            // OpenAI-style `reasoning_effort`; `off` is spelled `none`.
            let value = if level == "off" { "none" } else { level };
            fields.insert("reasoning_effort".into(), json!(value));
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
            (
                "openai",
                "gpt-6.1-sol",
                &["low", "medium", "high", "xhigh", "max"],
            ),
            (
                "openai",
                "gpt-6-astra",
                &["low", "medium", "high", "xhigh", "max"],
            ),
            (
                "openai",
                "gpt-6.1-terra",
                &["off", "low", "medium", "high", "xhigh", "max"],
            ),
            (
                "openai",
                "gpt-6-luna",
                &["off", "low", "medium", "high", "xhigh", "max"],
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
            "claude-sonnet-5-5",
        ] {
            assert_eq!(levels_for("anthropic", model, &none()), full, "{model}");
        }
        // "Always on" models never offer `off`, and include `max`.
        for model in [
            "claude-fable-5-2",
            "claude-opus-5-5",
            "claude-opus-4-6",
            "claude-opus-4-6-20260101",
        ] {
            assert_eq!(
                levels_for("anthropic", model, &none()),
                ["low", "medium", "high", "xhigh", "max"],
                "{model}"
            );
            assert_eq!(
                levels_for("claude-agent-sdk", model, &none()),
                ["low", "medium", "high", "xhigh", "max"],
                "{model}"
            );
            assert!(wire_fields("anthropic", model, &[], "off").is_none());
            assert!(wire_fields("claude-agent-sdk", model, &[], "off").is_none());
        }
        for model in [
            "claude-3-5-sonnet-20241022",
            "claude-haiku-3-5",
            "claude-3-opus-20240229",
            "claude-3-haiku-20240307",
            "claude-2.1",
            "gpt-4o",
        ] {
            assert!(
                levels_for("anthropic", model, &none()).is_empty(),
                "{model}"
            );
            assert!(
                levels_for("anthropic", model, &["reasoning".into()]).is_empty(),
                "{model}"
            );
            assert!(
                levels_for("claude-agent-sdk", model, &none()).is_empty(),
                "{model}"
            );
        }
        // An unknown Claude id is offered levels only when the catalog says so.
        assert!(levels_for("anthropic", "claude-next", &none()).is_empty());
        assert_eq!(
            levels_for("anthropic", "claude-next", &["reasoning".into()]),
            full
        );
        assert_eq!(
            levels_for("claude-agent-sdk", "claude-sonnet-4-5", &none()),
            full
        );

        let agy_always_on = ["low", "medium", "high", "max"];
        for model in [
            "claude-opus-5-5",
            "claude-opus-5-5-thinking",
            "claude-fable-5-2",
        ] {
            assert_eq!(
                levels_for("google-antigravity", model, &none()),
                agy_always_on,
                "{model}"
            );
            assert!(wire_fields("google-antigravity", model, &[], "off").is_none());
        }
        for model in [
            "claude-sonnet-4-6",
            "claude-opus-4-6",
            "claude-opus-4-6-thinking",
        ] {
            assert_eq!(
                levels_for("google-antigravity", model, &none()),
                ["high"],
                "{model}"
            );
        }
        assert_eq!(
            levels_for("google-antigravity", "gpt-oss-120b", &none()),
            ["medium"]
        );
        let agy = ["off", "low", "medium", "high", "max"];
        for model in [
            "claude-sonnet-4-5-thinking",
            "claude-sonnet-4-5",
            "claude-opus-4-5",
            "claude-sonnet-5-5",
            "claude-haiku-4-5",
            "claude-haiku-5-5",
        ] {
            assert_eq!(
                levels_for("google-antigravity", model, &none()),
                agy,
                "{model}"
            );
        }
        for model in ["claude-3-5-sonnet", "claude-haiku-3-5"] {
            assert!(
                levels_for("google-antigravity", model, &none()).is_empty(),
                "{model}"
            );
        }
        assert_eq!(
            levels_for("google-antigravity", "gemini-3.1-pro", &none()),
            ["low", "high"]
        );
        for model in [
            "gemini-2.5-pro",
            "gemini-3-pro",
            "gemini-3.8-flash",
            "gemini-3.7-flash",
            "gemini-3.6-flash",
            "gemini-3.5-flash",
        ] {
            assert_eq!(
                levels_for("google-antigravity", model, &none()),
                ["low", "medium", "high"],
                "{model}"
            );
        }
        for model in ["gemini-4-pro", "gemini-4-argon", "gemini-4-flash"] {
            assert_eq!(
                levels_for("google-antigravity", model, &none()),
                ["low", "medium", "high", "max"],
                "{model}"
            );
        }
        for model in ["gemini-2.5-flash", "gemini-3-flash", "gemini-3-pro-tiered"] {
            assert_eq!(
                levels_for("google-antigravity", model, &none()),
                ["off", "low", "medium", "high"],
                "{model}"
            );
        }
        for model in [
            "gemini-3-pro-low",
            "gemini-3-pro-medium",
            "gemini-3-pro-high",
            "gemini-3-flash-minimal",
            "gemini-4-pro-xhigh",
            "gemini-4-argon-max",
            "gpt-oss-120b-medium",
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
            shape("claude-agent-sdk", "claude-opus-4-6", "max"),
            Some(json!({"thinking": {"type": "enabled", "budget_tokens": 32000}}))
        );
        assert_eq!(
            wire_fields("anthropic", "claude-3-5-sonnet", &[], "high"),
            None
        );
        assert_eq!(
            shape("google-antigravity", "gemini-3-pro", "high"),
            Some(json!({"thinking_level": "high"}))
        );
        assert_eq!(shape("google-antigravity", "gemini-3-pro", "max"), None);
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
        // Codex takes the same Responses effort for every model.
        assert_eq!(
            levels_for("openai-codex", "gpt-6.1-sol", &none()),
            ["off", "low", "medium", "high"]
        );
        assert_eq!(
            shape("openai-codex", "gpt-6.1-sol", "medium"),
            Some(json!({"reasoning": {"effort": "medium", "summary": "auto"}}))
        );
        assert_eq!(shape("openai-codex", "gpt-6.1-sol", "off"), Some(json!({})));
        assert_eq!(
            shape("opencode-go", "glm-5", "medium"),
            Some(json!({"reasoning_effort": "medium"}))
        );
        assert_eq!(
            wire_fields("opencode-zen", "glm-5", &[], "medium"),
            None,
            "no reasoning tag, no parameter"
        );
        assert_eq!(
            shape("moonshot", "kimi-k2.5", "off"),
            Some(json!({"thinking": {"type": "disabled"}}))
        );
        assert_eq!(
            shape("moonshot", "kimi-k2-thinking", "high"),
            Some(json!({"thinking": {"type": "enabled"}, "reasoning_effort": "high"}))
        );
        assert_eq!(
            shape("inception", "mercury-2", "off"),
            Some(json!({"reasoning_effort": "none"}))
        );
        assert_eq!(
            shape("ollama", "qwen3:32b", "medium"),
            Some(json!({"reasoning_effort": "medium"}))
        );
        assert_eq!(
            shape("kiro", "claude-sonnet-4-5", "max"),
            Some(json!({"thinkingLevel": "max"}))
        );
        assert_eq!(shape("kiro", "claude-sonnet-4-5", "off"), Some(json!({})));
        assert_eq!(
            shape("devin", "swe-2-pro", "high"),
            Some(json!({"thinkingConfig": {"enabled": true, "effort": "high"}}))
        );
        assert_eq!(shape("devin", "claude-sonnet-4-5", "off"), Some(json!({})));
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
        // Together uses the reasoning tag too; an undocumented provider does not.
        assert_eq!(
            levels_for("together", "m", &tagged),
            ["low", "medium", "high"]
        );
        assert!(levels_for("huggingface", "m", &tagged).is_empty());
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
    fn copilot_mistral_and_antigravity_thinking_levels_are_accurate() {
        assert_eq!(
            levels_for("github-copilot", "o3-mini", &[]),
            vec!["low", "medium", "high"]
        );
        assert_eq!(
            levels_for("github-copilot", "gpt-5.2", &[]),
            vec!["off", "low", "medium", "high"]
        );
        assert_eq!(
            levels_for("github-copilot", "gemini-2.5-pro", &[]),
            vec!["low", "medium", "high"]
        );
        assert_eq!(
            levels_for("github-copilot", "gemini-3-pro", &[]),
            vec!["low", "medium", "high"]
        );
        assert!(levels_for("github-copilot", "gpt-4o", &[]).is_empty());

        assert_eq!(
            levels_for("google-antigravity", "gemini-2.5-pro", &[]),
            vec!["low", "medium", "high"]
        );
        assert_eq!(
            levels_for("google-antigravity", "gemini-2.5-flash", &[]),
            vec!["off", "low", "medium", "high"]
        );
        assert_eq!(
            levels_for("google-antigravity", "gemini-3-pro", &[]),
            vec!["low", "medium", "high"]
        );
        assert_eq!(
            levels_for("mistral", "deepseek-v4-flash", &[]),
            vec!["off", "low", "high", "max"]
        );
        assert_eq!(
            levels_for("mistral", "custom-reasoner", &["reasoning".into()]),
            vec!["low", "medium", "high"]
        );
    }

    #[test]
    fn kiro_devin_moonshot_inception_ollama_and_inference_providers() {
        // Kiro: Claude thinking + reasoning-tagged.
        assert_eq!(
            levels_for("kiro", "claude-sonnet-4-5", &none()),
            vec!["off", "low", "medium", "high", "max"]
        );
        assert!(levels_for("kiro", "claude-3-5-sonnet", &none()).is_empty());
        assert_eq!(
            levels_for("kiro", "custom-reasoner", &["reasoning".into()]),
            vec!["off", "low", "medium", "high", "xhigh", "max"]
        );
        assert!(levels_for("kiro", "plain-model", &none()).is_empty());

        // Devin: swe-2* + Claude/GPT/Gemini reasoning.
        assert_eq!(
            levels_for("devin", "swe-2-pro", &none()),
            vec!["low", "medium", "high", "max"]
        );
        assert_eq!(
            levels_for("devin", "claude-opus-4-6", &none()),
            vec!["low", "medium", "high", "max"]
        );
        assert_eq!(
            levels_for("devin", "claude-sonnet-4-5", &none()),
            vec!["off", "low", "medium", "high", "max"]
        );
        assert_eq!(
            levels_for("devin", "gpt-5.4", &none()),
            vec!["off", "low", "medium", "high", "xhigh"]
        );
        assert_eq!(
            levels_for("devin", "gemini-3-pro", &none()),
            vec!["low", "medium", "high"]
        );
        assert_eq!(
            levels_for("devin", "gemini-2.5-flash", &none()),
            vec!["off", "low", "medium", "high"]
        );

        // Moonshot: kimi-k2-thinking and kimi-k2.5.
        for model in ["kimi-k2-thinking", "kimi-k2.5"] {
            assert_eq!(
                levels_for("moonshot", model, &none()),
                vec!["off", "low", "medium", "high"],
                "{model}"
            );
        }
        assert!(levels_for("moonshot", "moonshot-v1-8k", &none()).is_empty());

        // Inception: mercury-2* and mercury-3*.
        for model in ["mercury-2", "mercury-3-pro"] {
            assert_eq!(
                levels_for("inception", model, &none()),
                vec!["off", "low", "medium", "high"],
                "{model}"
            );
        }
        assert!(levels_for("inception", "mercury-coder-small", &none()).is_empty());

        // Ollama: deepseek-r1*, qwen3*, qwq*, gpt-oss*.
        for model in ["deepseek-r1:70b", "qwen3:32b", "qwq:32b", "gpt-oss:20b"] {
            assert_eq!(
                levels_for("ollama", model, &none()),
                vec!["off", "low", "medium", "high"],
                "{model}"
            );
        }
        assert!(levels_for("ollama", "llama3.3:70b", &none()).is_empty());

        // Inference providers: cerebras, together, fireworks, minimax, nebius, sambanova.
        for kind in [
            "cerebras",
            "together",
            "fireworks",
            "minimax",
            "nebius",
            "sambanova",
        ] {
            assert_eq!(
                levels_for(kind, "qwen/qwen3-235b", &none()),
                vec!["off", "low", "medium", "high"],
                "{kind} qwen3"
            );
            for model in [
                "deepseek-r1-0528",
                "openai/gpt-oss-120b",
                "kimi-k2-instruct",
                "MiniMax-M3.1-Flash",
            ] {
                assert_eq!(
                    levels_for(kind, model, &none()),
                    vec!["low", "medium", "high"],
                    "{kind} {model}"
                );
            }
            assert_eq!(
                levels_for(kind, "custom", &["reasoning".into()]),
                vec!["low", "medium", "high"],
                "{kind} tagged"
            );
            assert!(levels_for(kind, "llama-3.3-70b", &none()).is_empty());
        }
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
