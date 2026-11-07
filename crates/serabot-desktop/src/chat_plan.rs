//! The goal and todo list of the open conversation.
//!
//! The server records the plan (`plan_update` tool) and pushes it as a
//! `plan.updated` event; the same shape comes back from
//! `GET /v1/conversations/{id}/plan`. The text is model-written and therefore
//! untrusted display data: it is length-bounded, control characters are
//! dropped, and nothing here is ever sent back to the server.

use serde_json::Value;

const MAX_ITEMS: usize = 20;
const MAX_GOAL: usize = 200;
const MAX_ITEM: usize = 160;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ItemStatus {
    Pending,
    Doing,
    Done,
}

impl ItemStatus {
    fn parse(value: &str) -> Option<Self> {
        match value {
            "pending" => Some(Self::Pending),
            "doing" => Some(Self::Doing),
            "done" => Some(Self::Done),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlanItem {
    pub text: String,
    pub status: ItemStatus,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChatPlan {
    pub goal: String,
    pub items: Vec<PlanItem>,
}

fn clean(value: &str, max: usize) -> String {
    value
        .chars()
        .filter(|c| !c.is_control())
        .take(max)
        .collect::<String>()
        .trim()
        .to_owned()
}

impl ChatPlan {
    /// Parse the server shape. `None` for a missing goal or malformed items.
    pub fn from_value(value: &Value) -> Option<Self> {
        let goal = clean(value.get("goal")?.as_str()?, MAX_GOAL);
        if goal.is_empty() {
            return None;
        }
        let raw = value.get("items")?.as_array()?;
        if raw.len() > MAX_ITEMS {
            return None;
        }
        let mut items = Vec::with_capacity(raw.len());
        for item in raw {
            let text = clean(item.get("text")?.as_str()?, MAX_ITEM);
            if text.is_empty() {
                return None;
            }
            let status = ItemStatus::parse(item.get("status")?.as_str()?)?;
            items.push(PlanItem { text, status });
        }
        Some(Self { goal, items })
    }

    /// Apply a `plan.updated` event for `conversation`. Events of another
    /// conversation, other kinds and malformed payloads are ignored. Returns
    /// whether the event was a plan event (handled, not a transcript change).
    pub fn apply_event(
        current: &mut Option<Self>,
        conversation: &str,
        kind: &str,
        payload: &Value,
    ) -> bool {
        if kind != "plan.updated" {
            return false;
        }
        if payload.get("conversation_id").and_then(Value::as_str) == Some(conversation)
            && let Some(plan) = Self::from_value(payload)
        {
            *current = Some(plan);
        }
        true
    }

    pub fn done(&self) -> usize {
        self.items
            .iter()
            .filter(|item| item.status == ItemStatus::Done)
            .count()
    }

    /// "2 of 5 done".
    pub fn progress(&self) -> String {
        format!("{} of {} done", self.done(), self.items.len())
    }

    pub fn is_complete(&self) -> bool {
        !self.items.is_empty() && self.done() == self.items.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn sample() -> Value {
        json!({
            "conversation_id": "dir:main",
            "goal": "Prepare the summary",
            "items": [
                {"text": "Collect figures", "status": "done"},
                {"text": "Write it", "status": "doing"},
                {"text": "Send it", "status": "pending"}
            ]
        })
    }

    #[test]
    fn parses_the_server_shape_and_counts_progress() {
        let plan = ChatPlan::from_value(&sample()).unwrap();
        assert_eq!(plan.goal, "Prepare the summary");
        assert_eq!(plan.progress(), "1 of 3 done");
        assert!(!plan.is_complete());
    }

    #[test]
    fn rejects_junk_and_bounds_text() {
        assert!(ChatPlan::from_value(&json!({"goal": "", "items": []})).is_none());
        assert!(ChatPlan::from_value(&json!({"goal": "g"})).is_none());
        let bad_status = json!({"goal": "g", "items": [{"text": "a", "status": "x"}]});
        assert!(ChatPlan::from_value(&bad_status).is_none());
        let many = json!({"goal": "g", "items": vec![json!({"text": "a", "status": "done"}); 21]});
        assert!(ChatPlan::from_value(&many).is_none());
        let long = json!({"goal": format!("a\u{7}{}", "x".repeat(500)),
                          "items": [{"text": "y".repeat(500), "status": "done"}]});
        let plan = ChatPlan::from_value(&long).unwrap();
        assert!(plan.goal.chars().count() <= MAX_GOAL && !plan.goal.contains('\u{7}'));
        assert!(plan.items[0].text.chars().count() <= MAX_ITEM);
        assert!(plan.is_complete());
    }

    #[test]
    fn events_apply_only_to_their_own_conversation() {
        let mut current = None;
        assert!(!ChatPlan::apply_event(
            &mut current,
            "dir:main",
            "turn.step",
            &sample()
        ));
        assert!(ChatPlan::apply_event(
            &mut current,
            "dir:other",
            "plan.updated",
            &sample()
        ));
        assert!(current.is_none(), "another conversation's plan is dropped");
        assert!(ChatPlan::apply_event(
            &mut current,
            "dir:main",
            "plan.updated",
            &sample()
        ));
        assert_eq!(current.as_ref().unwrap().items.len(), 3);
        // A malformed update keeps the last good plan.
        ChatPlan::apply_event(
            &mut current,
            "dir:main",
            "plan.updated",
            &json!({"conversation_id": "dir:main", "goal": ""}),
        );
        assert!(current.is_some());
    }
}
