use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::error::{Result, SeraError};

pub fn sha256_hex(bytes: impl AsRef<[u8]>) -> String {
    hex::encode(Sha256::digest(bytes.as_ref()))
}

/// Content type for a stored artifact, derived from its filename.
///
/// Every delivery path used to hardcode `application/pdf`, which mislabelled
/// Word, Excel, and PowerPoint files in Telegram, OneDrive, and the owner API.
pub fn mime_for_filename(filename: &str) -> &'static str {
    let extension = std::path::Path::new(filename)
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    match extension.as_str() {
        "pdf" => "application/pdf",
        "docx" => "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
        "xlsx" => "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
        "pptx" => "application/vnd.openxmlformats-officedocument.presentationml.presentation",
        "csv" => "text/csv",
        "txt" | "md" | "markdown" => "text/plain; charset=utf-8",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "mp3" => "audio/mpeg",
        "wav" => "audio/wav",
        "mp4" => "video/mp4",
        _ => "application/octet-stream",
    }
}

/// Produce a stable representation for hashing approval-bound arguments.
///
/// Object keys are sorted recursively. JSON numbers and strings retain their
/// exact serde representation, so callers must normalize semantic values
/// (absolute paths, timestamps) before binding them to an approval.
pub fn canonical_json(value: &Value) -> Result<String> {
    fn write(value: &Value, out: &mut String) -> Result<()> {
        match value {
            Value::Null => out.push_str("null"),
            Value::Bool(v) => out.push_str(if *v { "true" } else { "false" }),
            Value::Number(v) => out.push_str(&v.to_string()),
            Value::String(v) => {
                out.push_str(&serde_json::to_string(v)?);
            }
            Value::Array(values) => {
                out.push('[');
                for (index, value) in values.iter().enumerate() {
                    if index > 0 {
                        out.push(',');
                    }
                    write(value, out)?;
                }
                out.push(']');
            }
            Value::Object(map) => {
                let mut keys: Vec<&String> = map.keys().collect();
                keys.sort();
                out.push('{');
                for (index, key) in keys.into_iter().enumerate() {
                    if index > 0 {
                        out.push(',');
                    }
                    out.push_str(&serde_json::to_string(key)?);
                    out.push(':');
                    write(&map[key], out)?;
                }
                out.push('}');
            }
        }
        Ok(())
    }

    let mut output = String::new();
    write(value, &mut output)?;
    Ok(output)
}

pub fn canonical_hash(value: &Value) -> Result<String> {
    Ok(sha256_hex(canonical_json(value)?))
}

pub fn constant_time_eq(left: &str, right: &str) -> bool {
    use subtle::ConstantTimeEq;
    left.as_bytes().ct_eq(right.as_bytes()).into()
}

pub fn validate_id(value: &str) -> Result<()> {
    if value.len() > 128
        || value.is_empty()
        || !value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | ':'))
    {
        return Err(SeraError::Validation("invalid identifier".into()));
    }
    Ok(())
}

pub fn truncate_chars(value: &str, max: usize) -> String {
    value.chars().take(max).collect()
}

/// Remove the query string and fragment from every `http(s)://` URL in `text`.
/// Feed URLs can carry access keys in their query, so any text that may be
/// shown to a member is passed through this first. A redacted query is
/// replaced with `?[redacted]` so the reader still sees one was there.
pub fn redact_url_queries(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = next_scheme(rest) {
        out.push_str(&rest[..start]);
        let tail = &rest[start..];
        let end = tail
            .find(|c: char| {
                c.is_whitespace() || matches!(c, '"' | '\'' | '<' | '>' | '(' | ')' | ',')
            })
            .unwrap_or(tail.len());
        // Sentence punctuation after the URL is not part of it.
        let url_len = tail[..end].trim_end_matches(['.', ':', ';', '!']).len();
        let url = &tail[..url_len];
        match url.find(['?', '#']) {
            Some(cut) => {
                out.push_str(&url[..cut]);
                out.push_str("?[redacted]");
            }
            None => out.push_str(url),
        }
        rest = &tail[url_len..];
    }
    out.push_str(rest);
    out
}

fn next_scheme(text: &str) -> Option<usize> {
    match (text.find("https://"), text.find("http://")) {
        (Some(https), Some(http)) => Some(https.min(http)),
        (https, http) => https.or(http),
    }
}

const MAX_SMART_TITLE_CHARS: usize = 56;
const MAX_SMART_TITLE_WORDS: usize = 7;

/// Whether a conversation title is absent or a generic placeholder (`"New chat"`,
/// `"Chat"`, `"Chat <hex>"`, or `"dir:chat-..."`).
pub fn is_placeholder_chat_title(title: Option<&str>) -> bool {
    let Some(raw) = title.map(str::trim).filter(|s| !s.is_empty()) else {
        return true;
    };
    let lower = raw.to_ascii_lowercase();
    if matches!(lower.as_str(), "new chat" | "chat") || lower.starts_with("dir:chat-") {
        return true;
    }
    if let Some(rest) = lower.strip_prefix("chat ") {
        let rest = rest.trim();
        if !rest.is_empty()
            && rest.len() >= 6
            && rest.chars().all(|c| c.is_ascii_hexdigit() || c == '-')
        {
            return true;
        }
    }
    false
}

fn strip_code_blocks(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut rest = input;
    while let Some(start) = rest.find("```") {
        out.push_str(&rest[..start]);
        out.push(' ');
        let after_start = &rest[start + 3..];
        if let Some(end) = after_start.find("```") {
            rest = &after_start[end + 3..];
        } else {
            rest = "";
            break;
        }
    }
    out.push_str(rest);
    out
}

fn strip_urls(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut rest = input;
    while let Some(start) = next_scheme(rest) {
        out.push_str(&rest[..start]);
        out.push(' ');
        let tail = &rest[start..];
        let end = tail
            .find(|c: char| {
                c.is_whitespace() || matches!(c, '"' | '\'' | '<' | '>' | '(' | ')' | ']')
            })
            .unwrap_or(tail.len());
        rest = &tail[end..];
    }
    out.push_str(rest);
    out
}

fn strip_markdown_links(input: &str) -> String {
    // Convert `[label](url)` into `label` and drop the URL.
    let mut out = String::with_capacity(input.len());
    let mut rest = input;
    while let Some(open) = rest.find('[') {
        out.push_str(&rest[..open]);
        let after_open = &rest[open + 1..];
        if let Some(mid) = after_open.find("](") {
            let label = &after_open[..mid];
            let after_mid = &after_open[mid + 2..];
            if let Some(close) = after_mid.find(')') {
                out.push_str(label);
                rest = &after_mid[close + 1..];
                continue;
            }
        }
        out.push('[');
        rest = after_open;
    }
    out.push_str(rest);
    out
}

fn clean_title_source(raw: &str, bot_name: &str) -> String {
    let no_blocks = strip_code_blocks(raw);
    let no_links = strip_markdown_links(&no_blocks);
    let no_urls = strip_urls(&no_links);
    let cleaned: String = no_urls
        .chars()
        .map(|c| {
            if matches!(c, '*' | '`' | '#' | '~' | '>' | '|') || c.is_control() {
                ' '
            } else {
                c
            }
        })
        .collect();

    let mut collapsed = cleaned.split_whitespace().collect::<Vec<_>>().join(" ");
    let bot_lower = bot_name.trim().to_ascii_lowercase();
    let filler_prefixes: &[&str] = &[
        "hey there",
        "hello there",
        "hi there",
        "good morning",
        "good afternoon",
        "good evening",
        "hey",
        "hello",
        "hi",
        "bonjour",
        "salut",
        "salam",
        "please",
        "could you please",
        "can you please",
        "would you please",
        "could you",
        "can you",
        "would you",
        "help me",
        "i need you to",
        "i want you to",
        "i need",
        "i want",
        "tell me",
        "let's",
        "lets",
        "serabot",
    ];

    loop {
        let trimmed = collapsed
            .trim_start_matches([',', ':', ';', '-', '!', '?', '.'])
            .trim_start();
        if trimmed.len() != collapsed.len() {
            collapsed = trimmed.to_owned();
            continue;
        }
        let lower = collapsed.to_ascii_lowercase();
        let mut matched = false;
        if !bot_lower.is_empty()
            && lower.starts_with(&bot_lower)
            && lower[bot_lower.len()..]
                .chars()
                .next()
                .is_none_or(|c| !c.is_alphanumeric())
        {
            collapsed = collapsed[bot_lower.len()..].trim_start().to_owned();
            matched = true;
        }
        if !matched {
            for prefix in filler_prefixes {
                if lower.starts_with(prefix)
                    && lower[prefix.len()..]
                        .chars()
                        .next()
                        .is_none_or(|c| !c.is_alphanumeric())
                {
                    collapsed = collapsed[prefix.len()..].trim_start().to_owned();
                    matched = true;
                    break;
                }
            }
        }
        if !matched {
            break;
        }
    }

    collapsed
        .trim_matches(|c: char| matches!(c, ',' | ':' | ';' | '-' | '!' | '?' | '.' | '"' | '\''))
        .trim()
        .to_owned()
}

fn format_concise_topic(cleaned: &str) -> Option<String> {
    let words: Vec<&str> = cleaned.split_whitespace().collect();
    if words.is_empty() {
        return None;
    }

    let mut chosen: Vec<String> = Vec::new();
    let mut current_len = 0usize;
    for word in words.into_iter().take(MAX_SMART_TITLE_WORDS) {
        let word_len = word.chars().count();
        let added_len = if chosen.is_empty() {
            word_len
        } else {
            1 + word_len
        };
        if current_len + added_len > MAX_SMART_TITLE_CHARS {
            break;
        }
        chosen.push(word.to_owned());
        current_len += added_len;
    }

    if chosen.is_empty() {
        let truncated: String = cleaned.chars().take(MAX_SMART_TITLE_CHARS).collect();
        let trimmed = truncated.trim();
        if trimmed.is_empty() {
            return None;
        }
        chosen.push(trimmed.to_owned());
    }

    while chosen.len() >= 4
        && matches!(
            chosen.last().map(|w| w.to_ascii_lowercase()).as_deref(),
            Some("a" | "an" | "the" | "to" | "for" | "of" | "in" | "on" | "with" | "and" | "or")
        )
    {
        chosen.pop();
    }

    let mut topic = chosen.join(" ");
    topic = topic
        .trim_end_matches([',', ':', ';', '-', '.', '!', '?'])
        .trim()
        .to_owned();

    if topic.chars().count() < 3 {
        return None;
    }

    let mut chars = topic.chars();
    let first = chars.next()?;
    let capitalized = first.to_uppercase().collect::<String>() + chars.as_str();

    if is_placeholder_chat_title(Some(&capitalized)) {
        return None;
    }
    Some(capitalized)
}

/// Derive a concise, context-aware 3–7 word conversation title (max 56 chars)
/// from the user's prompt and optional assistant reply. Never returns `"New chat"`,
/// `"Chat"`, or hex conversation IDs.
pub fn derive_smart_chat_title(
    user_text: &str,
    assistant_text: Option<&str>,
    bot_name: &str,
) -> Option<String> {
    let cleaned_user = clean_title_source(user_text, bot_name);
    if let Some(title) = format_concise_topic(&cleaned_user) {
        return Some(title);
    }
    if let Some(reply) = assistant_text {
        let cleaned_reply = clean_title_source(reply, bot_name);
        if let Some(title) = format_concise_topic(&cleaned_reply) {
            return Some(title);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn url_queries_and_fragments_are_redacted_everywhere_in_text() {
        assert_eq!(
            redact_url_queries("fetch https://host.example/feed?access=KEY: timeout"),
            "fetch https://host.example/feed?[redacted]: timeout"
        );
        assert_eq!(
            redact_url_queries("a http://h.example/x#frag and https://h.example/y?k=1&j=2)"),
            "a http://h.example/x?[redacted] and https://h.example/y?[redacted])"
        );
        // Text without a URL, and URLs without a query, pass through unchanged.
        assert_eq!(redact_url_queries("plain text"), "plain text");
        assert_eq!(
            redact_url_queries("see https://h.example/feed."),
            "see https://h.example/feed."
        );
    }

    #[test]
    fn mime_matches_the_extension() {
        assert_eq!(mime_for_filename("a.pdf"), "application/pdf");
        assert_eq!(
            mime_for_filename("A.DOCX"),
            "application/vnd.openxmlformats-officedocument.wordprocessingml.document"
        );
        assert_eq!(
            mime_for_filename("b.xlsx"),
            "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet"
        );
        assert_eq!(
            mime_for_filename("c.pptx"),
            "application/vnd.openxmlformats-officedocument.presentationml.presentation"
        );
        assert_eq!(mime_for_filename("d.csv"), "text/csv");
        assert_eq!(mime_for_filename("e.unknown"), "application/octet-stream");
        assert_eq!(
            mime_for_filename("no-extension"),
            "application/octet-stream"
        );
    }

    #[test]
    fn placeholder_chat_title_detects_generic_and_hex_names() {
        assert!(is_placeholder_chat_title(None));
        assert!(is_placeholder_chat_title(Some("")));
        assert!(is_placeholder_chat_title(Some("   ")));
        assert!(is_placeholder_chat_title(Some("New chat")));
        assert!(is_placeholder_chat_title(Some("new chat")));
        assert!(is_placeholder_chat_title(Some("Chat")));
        assert!(is_placeholder_chat_title(Some("Chat 01a1dfd96ab2")));
        assert!(is_placeholder_chat_title(Some("dir:chat-01a1dfd96ab2")));
        assert!(!is_placeholder_chat_title(Some("Q3 Revenue Forecast")));
        assert!(!is_placeholder_chat_title(Some("Chat about Morocco jobs")));
    }

    #[test]
    fn derive_smart_chat_title_strips_markdown_urls_and_fillers() {
        let title = derive_smart_chat_title(
            "Hey, please can you help me review [this contract](https://example.com/doc) for **Acme Corp**? ```rust\nfn main(){}\n```",
            None,
            "legal",
        )
        .unwrap();
        assert!(!title.contains('*'));
        assert!(!title.contains("https://"));
        assert!(!title.contains("Hey"));
        assert!(title.len() <= 56);
        assert!(title.starts_with("Review this contract for Acme Corp"));

        // Falls back to assistant reply when user prompt is only greeting/filler.
        let fallback = derive_smart_chat_title(
            "Hi!",
            Some("## Moroccan Public Sector Job Checklist\nHere are the required documents."),
            "jobfinder",
        )
        .unwrap();
        assert!(fallback.starts_with("Moroccan Public Sector Job Checklist"));
        assert!(fallback.len() <= 56);

        // Never returns placeholder titles.
        assert_eq!(derive_smart_chat_title("New chat", None, "main"), None);
        assert_eq!(
            derive_smart_chat_title("Chat 01a1dfd96ab2", None, "main"),
            None
        );
    }
}
