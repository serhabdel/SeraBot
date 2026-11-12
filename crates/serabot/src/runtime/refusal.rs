//! Spot bare refusals in a bot's final reply and record them in the audit feed.
//!
//! This is observation only: it never changes or blocks the reply. It exists so
//! the owner can see when a specialist bot stopped without trying (the 0.37 base
//! tools and `ask_teammate` are meant to make that rare) and tune prompts.

use serde_json::json;

use super::Runtime;
use crate::store::Store;

/// Replies longer than this are explanations, not bare refusals.
const MAX_REFUSAL_CHARS: usize = 400;

/// Phrases that open a bare refusal. Deliberately specific: "I can't find any
/// matches" is an ordinary answer, "I can't help with that" is a refusal.
const PHRASES: &[&str] = &[
    "i can't help",
    "i cannot help",
    "i can't do that",
    "i cannot do that",
    "i can't assist",
    "i cannot assist",
    "i won't do",
    "i will not do",
    "i'm unable to",
    "i am unable to",
    "i don't have access to",
    "i do not have access to",
    "tool is not available",
    "tools are not available",
    "say what is missing",
    "abstain instead of guessing",
];

/// The matched phrase when `text` reads as a bare refusal, else `None`.
///
/// `tool_calls` is how many tools the turn already ran: a reply after real work
/// is a result, not a refusal, so it is never flagged.
pub(super) fn looks_like_refusal(text: &str, tool_calls: usize) -> Option<&'static str> {
    if tool_calls > 0 {
        return None;
    }
    let lower = text.trim().to_lowercase().replace('\u{2019}', "'");
    if lower.is_empty() || lower.chars().count() > MAX_REFUSAL_CHARS {
        return None;
    }
    PHRASES
        .iter()
        .copied()
        .find(|phrase| lower.contains(phrase))
}

impl Runtime {
    /// Append a `turn.refused` event and audit row. Best effort: a failure to
    /// log must never fail the turn.
    pub(super) fn log_refusal(
        &self,
        store: &Store,
        user_id: &str,
        task_id: &str,
        details: &serde_json::Value,
    ) {
        if let Err(error) = store.append_event(user_id, "turn.refused", details) {
            tracing::warn!(%error, "could not append turn.refused event");
        }
        if let Err(error) = store.record_audit(
            user_id,
            "turn.refused",
            None,
            Some(task_id),
            None,
            None,
            Some(self.gateway.policy().version()),
            Some(details),
        ) {
            tracing::warn!(%error, "could not record turn.refused audit");
        }
    }

    /// Build the audit payload: ids plus a short snippet, nothing else.
    pub(super) fn refusal_details(
        bot: &str,
        turn_id: &str,
        conversation_id: &str,
        phrase: &str,
        text: &str,
    ) -> serde_json::Value {
        json!({
            "bot": bot,
            "turn_id": turn_id,
            "conversation_id": conversation_id,
            "phrase": phrase,
            "snippet": crate::util::truncate_chars(text, 160),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flags_a_bare_refusal_with_no_tool_work() {
        assert_eq!(
            looks_like_refusal("I can't help with that.", 0),
            Some("i can't help")
        );
        assert_eq!(
            looks_like_refusal("I’m unable to look that up.", 0),
            Some("i'm unable to")
        );
    }

    #[test]
    fn ordinary_answers_are_not_refusals() {
        assert!(looks_like_refusal("I can't find any matches for that name.", 0).is_none());
        assert!(looks_like_refusal("Here is the table you asked for.", 0).is_none());
        assert!(looks_like_refusal("", 0).is_none());
    }

    #[test]
    fn a_reply_after_tool_work_is_never_flagged() {
        assert!(looks_like_refusal("I can't help with the third item.", 2).is_none());
    }

    #[test]
    fn long_explanations_are_not_flagged() {
        let long = format!("I can't help with that. {}", "Context. ".repeat(80));
        assert!(looks_like_refusal(&long, 0).is_none());
    }

    #[test]
    fn details_carry_ids_and_a_short_snippet_only() {
        let text = "I can't help with that. ".repeat(30);
        let details = Runtime::refusal_details("legal", "turn-1", "conv-1", "i can't help", &text);
        assert_eq!(details["bot"], "legal");
        assert_eq!(details["turn_id"], "turn-1");
        assert!(details["snippet"].as_str().unwrap().chars().count() <= 160);
        assert_eq!(details.as_object().unwrap().len(), 5);
    }
}
