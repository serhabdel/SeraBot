//! What to tell the owner when a chat send fails, and how to keep the text
//! they typed. The message is a sentence. Raw JSON is never shown.

use crate::ApiError;

/// The button next to a send failure.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChatAction {
    /// Send the same text again.
    Retry,
    /// The event stream or the server is unreachable.
    Reconnect,
    /// A provider key needs attention.
    OpenKeys,
    /// Nothing to press; the sentence is enough.
    None,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChatFailure {
    pub message: String,
    pub action: ChatAction,
}

/// A sentence and an action for a failed `POST /v1/messages`.
pub fn classify_send_error(error: &ApiError) -> ChatFailure {
    match error {
        ApiError::Transport => ChatFailure {
            message: "Can't reach SeraBot. Your message is still in the box. Check the connection, then send it again.".into(),
            action: ChatAction::Retry,
        },
        ApiError::Http(401 | 403) => ChatFailure {
            message: "Your session is no longer valid. Your message is still in the box. Reconnect, then send it again.".into(),
            action: ChatAction::Reconnect,
        },
        ApiError::Http(404) => ChatFailure {
            message: "This conversation could not be found. Your text is still in the box. Try switching chats or starting a new one.".into(),
            action: ChatAction::None,
        },
        ApiError::Http(413) => ChatFailure {
            message: "That message or attachment is too large for the server. Shorten it or remove a file, then try again.".into(),
            action: ChatAction::None,
        },
        ApiError::Http(408 | 504) => ChatFailure {
            message: "The request timed out. Your message is still in the box. Send it again.".into(),
            action: ChatAction::Retry,
        },
        ApiError::Http(500..=599) => ChatFailure {
            message: "SeraBot had a problem sending that message. Your text is still in the box. Try again.".into(),
            action: ChatAction::Retry,
        },
        ApiError::Http(429) => ChatFailure {
            message: "Too many requests right now. Your message is still in the box. Wait a moment, then send it again.".into(),
            action: ChatAction::Retry,
        },
        ApiError::Coded { message, code, .. } => from_sentence(message, Some(code)),
        ApiError::Rejected(message) => from_sentence(message, None),
        other => from_sentence(&other.to_string(), None),
    }
}

fn from_sentence(message: &str, code: Option<&str>) -> ChatFailure {
    if looks_like_payload(message) {
        return ChatFailure {
            message: "Something went wrong sending that message. Your text is still in the box. Try again.".into(),
            action: ChatAction::Retry,
        };
    }
    if keyish(message) || code.is_some_and(keyish) {
        return keys_failure();
    }
    ChatFailure {
        message: message.to_owned(),
        action: ChatAction::None,
    }
}

fn keys_failure() -> ChatFailure {
    ChatFailure {
        message: "The provider rejected the API key. Open AI keys and check it. Your message is still in the box.".into(),
        action: ChatAction::OpenKeys,
    }
}

/// Text to put back in an empty composer after a send that did not land.
/// `None` when the composer already has something, so a newer draft is kept.
pub fn restore_unsent_draft(composer: &str, optimistic: Option<&str>) -> Option<String> {
    if !composer.trim().is_empty() {
        return None;
    }
    let text = optimistic?.trim();
    if text.is_empty() {
        None
    } else {
        Some(text.to_owned())
    }
}

fn keyish(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    lower.contains("api key")
        || lower.contains("api_key")
        || lower.contains("provider key")
        || lower.contains("invalid key")
        || lower.contains("missing key")
}

fn looks_like_payload(text: &str) -> bool {
    let trimmed = text.trim_start();
    let lower = trimmed.to_ascii_lowercase();
    trimmed.starts_with('{')
        || trimmed.starts_with('[')
        || lower.starts_with("<!doctype")
        || lower.starts_with("<html")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transport_timeout_and_server_errors_keep_a_next_step() {
        let lost = classify_send_error(&ApiError::Transport);
        assert_eq!(lost.action, ChatAction::Retry);
        assert!(lost.message.contains("still in the box"));
        assert!(!lost.message.contains('{'));

        let timed = classify_send_error(&ApiError::Http(504));
        assert!(timed.message.contains("timed out"));
        assert_eq!(timed.action, ChatAction::Retry);

        let down = classify_send_error(&ApiError::Http(502));
        assert_eq!(down.action, ChatAction::Retry);
        assert!(down.message.contains("SeraBot"));
    }

    #[test]
    fn a_rejected_key_points_at_ai_keys_and_json_is_not_shown() {
        let key = classify_send_error(&ApiError::Rejected("missing api key".into()));
        assert_eq!(key.action, ChatAction::OpenKeys);
        assert!(key.message.contains("AI keys"));

        let dumped = classify_send_error(&ApiError::Rejected(
            "{\"error\":\"boom\",\"api_key\":\"x\"}".into(),
        ));
        assert!(!dumped.message.contains("api_key"));
        assert!(!dumped.message.contains('{'));
        assert_eq!(dumped.action, ChatAction::Retry);
    }

    #[test]
    fn an_empty_composer_gets_the_unsent_text_back() {
        assert_eq!(
            restore_unsent_draft("", Some("hello")),
            Some("hello".into())
        );
        assert_eq!(restore_unsent_draft("already typing", Some("hello")), None);
        assert_eq!(restore_unsent_draft("", Some("  ")), None);
        assert_eq!(restore_unsent_draft("", None), None);
    }

    #[test]
    fn auth_size_missing_and_html_errors_map_cleanly() {
        let auth = classify_send_error(&ApiError::Http(401));
        assert_eq!(auth.action, ChatAction::Reconnect);
        assert!(auth.message.contains("still in the box"));

        let forbidden = classify_send_error(&ApiError::Http(403));
        assert_eq!(forbidden.action, ChatAction::Reconnect);

        let missing = classify_send_error(&ApiError::Http(404));
        assert_eq!(missing.action, ChatAction::None);
        assert!(missing.message.contains("not be found"));

        let large = classify_send_error(&ApiError::Http(413));
        assert_eq!(large.action, ChatAction::None);
        assert!(large.message.contains("too large"));

        let html = classify_send_error(&ApiError::Rejected(
            "<!DOCTYPE html><html>502</html>".into(),
        ));
        assert_eq!(html.action, ChatAction::Retry);
        assert!(!html.message.contains('<'));
    }
}
