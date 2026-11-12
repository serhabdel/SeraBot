//! Detect assistant refusals and log them for the audit feed.

use serde_json::json;

use super::Runtime;
use crate::store::Store;

/// Heuristic: owner-facing replies that decline without using tools.
pub fn looks_like_refusal(text: &str) -> Option<String> {
    let lower = text.trim().to_ascii_lowercase();
    if lower.is_empty() {
        return None;
    }
    const PHRASES: &[&str] = &[
        "i can't",
        "i cannot",
        "i won't",
        "i will not",
        "i'm unable",
        "i am unable",
        "not able to",
        "cannot help",
        "can't help",
        "out of scope",
        "outside my",
        "i don't have access",
        "i do not have access",
        "tool is not available",
        "tools are not available",
        "say what is missing",
        "abstain instead of guessing",
    ];
    let hit = PHRASES
        .iter()
        .find(|phrase| lower.contains(**phrase))
        .map(|s| (*s).to_string());
    // Short refusals only — avoid false positives on long explanations.
    hit.filter(|_| lower.chars().count() <= 800)
}

impl Runtime {
    #[allow(clippy::too_many_arguments)]
    pub(super) fn log_refusal(
        &self,
        store: &Store,
        user_id: &str,
        bot: &str,
        task_id: &str,
        turn_id: &str,
        conversation_id: &str,
        reason: &str,
        snippet: &str,
    ) {
        let payload = json!({
            "bot": bot,
            "task_id": task_id,
            "turn_id": turn_id,
            "conversation_id": conversation_id,
            "reason": reason,
            "snippet": snippet,
        });
        let _ = store.append_event(user_id, "turn.refused", &payload);
        let _ = store.record_audit(
            user_id,
            "turn.refused",
            None,
            Some(task_id),
            None,
            None,
            Some(self.gateway.policy().version()),
            Some(&payload),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_common_refusal_phrases() {
        assert!(looks_like_refusal("I can't do that for you.").is_some());
        assert!(looks_like_refusal("That is outside my scope.").is_some());
        assert!(looks_like_refusal("Here is the table you asked for.").is_none());
    }
}
