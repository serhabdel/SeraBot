//! Per-conversation goal and todo plan, and the per-conversation auto-approve
//! ("YOLO") setting.
//!
//! The model records a plan with the `plan_update` tool when the owner asks
//! for a multi-step task; the desktop shows it above the chat. Plan text is
//! model-written, so it is length-bounded and stripped of control characters
//! here, and it is only ever shown to its owner or fed back to the same
//! owner's bot as untrusted context.
//!
//! Every store method takes the acting user's id and filters on it. The
//! conversation id alone never selects a row: two accounts that both use
//! `dir:main` have separate plans and separate YOLO switches.

use async_trait::async_trait;
use chrono::{SecondsFormat, Utc};
use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::error::{Result, SeraError};
use crate::gateway::{Tool, ToolContext, ToolOutput, ToolSpec};
use crate::store::Store;

/// Most checklist items one plan may hold.
pub const MAX_PLAN_ITEMS: usize = 20;
pub const MAX_GOAL_CHARS: usize = 200;
pub const MAX_ITEM_CHARS: usize = 160;
const MAX_CONVERSATION_ID_CHARS: usize = 200;
pub const PLAN_TOOL: &str = "plan_update";
/// Event kind streamed to the desktop when a plan changes.
pub const PLAN_EVENT: &str = "plan.updated";

fn now() -> String {
    Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlanStatus {
    Pending,
    Doing,
    Done,
}

impl PlanStatus {
    fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "pending" | "todo" => Some(Self::Pending),
            "doing" | "in_progress" | "active" => Some(Self::Doing),
            "done" | "completed" | "complete" => Some(Self::Done),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanItem {
    pub text: String,
    pub status: PlanStatus,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Plan {
    pub conversation_id: String,
    pub goal: String,
    pub items: Vec<PlanItem>,
    pub updated_at: String,
}

impl Plan {
    pub fn done_count(&self) -> usize {
        self.items
            .iter()
            .filter(|item| item.status == PlanStatus::Done)
            .count()
    }

    /// True once there is at least one item and all of them are done.
    pub fn is_complete(&self) -> bool {
        !self.items.is_empty() && self.done_count() == self.items.len()
    }
}

fn clean_text(raw: &str, max_chars: usize) -> String {
    let flat: String = raw
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    let collapsed = flat.split_whitespace().collect::<Vec<_>>().join(" ");
    collapsed.chars().take(max_chars).collect()
}

/// Validate a model-supplied plan into its stored form.
pub fn parse_plan_args(args: &Value) -> Result<(String, Vec<PlanItem>)> {
    let goal = clean_text(
        args.get("goal").and_then(Value::as_str).unwrap_or_default(),
        MAX_GOAL_CHARS,
    );
    if goal.is_empty() {
        return Err(SeraError::Validation("goal is required".into()));
    }
    let raw_items = args
        .get("items")
        .and_then(Value::as_array)
        .ok_or_else(|| SeraError::Validation("items must be a list".into()))?;
    if raw_items.len() > MAX_PLAN_ITEMS {
        return Err(SeraError::Validation(format!(
            "a plan holds at most {MAX_PLAN_ITEMS} items"
        )));
    }
    let mut items = Vec::with_capacity(raw_items.len());
    for raw in raw_items {
        let text = clean_text(
            raw.get("text").and_then(Value::as_str).unwrap_or_default(),
            MAX_ITEM_CHARS,
        );
        if text.is_empty() {
            return Err(SeraError::Validation("plan items need text".into()));
        }
        let status = raw
            .get("status")
            .and_then(Value::as_str)
            .map_or(Some(PlanStatus::Pending), PlanStatus::parse)
            .ok_or_else(|| {
                SeraError::Validation("item status must be pending, doing or done".into())
            })?;
        items.push(PlanItem { text, status });
    }
    Ok((goal, items))
}

fn valid_conversation_id(id: &str) -> Result<()> {
    if id.is_empty() || id.chars().count() > MAX_CONVERSATION_ID_CHARS {
        return Err(SeraError::Validation("invalid conversation id".into()));
    }
    Ok(())
}

impl Store {
    /// Replace the acting user's plan for one conversation.
    pub fn set_plan(
        &self,
        user_id: &str,
        conversation_id: &str,
        goal: &str,
        items: &[PlanItem],
    ) -> Result<Plan> {
        valid_conversation_id(conversation_id)?;
        let updated_at = now();
        let connection = self.connection()?;
        connection.execute(
            "INSERT INTO conversation_plans (user_id, conversation_id, goal, items, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(user_id, conversation_id) DO UPDATE SET
                 goal = excluded.goal,
                 items = excluded.items,
                 updated_at = excluded.updated_at",
            params![
                user_id,
                conversation_id,
                goal,
                serde_json::to_string(items)?,
                updated_at
            ],
        )?;
        Ok(Plan {
            conversation_id: conversation_id.to_owned(),
            goal: goal.to_owned(),
            items: items.to_vec(),
            updated_at,
        })
    }

    /// The acting user's plan for one conversation, if any.
    pub fn get_plan(&self, user_id: &str, conversation_id: &str) -> Result<Option<Plan>> {
        valid_conversation_id(conversation_id)?;
        let connection = self.connection()?;
        let row: Option<(String, String, String)> = connection
            .query_row(
                "SELECT goal, items, updated_at FROM conversation_plans
                 WHERE user_id = ?1 AND conversation_id = ?2",
                params![user_id, conversation_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()?;
        Ok(row.map(|(goal, items, updated_at)| Plan {
            conversation_id: conversation_id.to_owned(),
            goal,
            // A corrupt row reads as an empty checklist, never an error.
            items: serde_json::from_str(&items).unwrap_or_default(),
            updated_at,
        }))
    }

    /// Remove the acting user's plan for one conversation.
    pub fn clear_plan(&self, user_id: &str, conversation_id: &str) -> Result<()> {
        valid_conversation_id(conversation_id)?;
        let connection = self.connection()?;
        connection.execute(
            "DELETE FROM conversation_plans WHERE user_id = ?1 AND conversation_id = ?2",
            params![user_id, conversation_id],
        )?;
        Ok(())
    }

    /// Whether the acting user turned auto-approve on for one conversation.
    /// Fails closed: a missing row, a malformed id or a read error is off.
    pub fn yolo_enabled(&self, user_id: &str, conversation_id: &str) -> bool {
        if valid_conversation_id(conversation_id).is_err() {
            return false;
        }
        let Ok(connection) = self.connection() else {
            return false;
        };
        connection
            .query_row(
                "SELECT enabled FROM conversation_yolo
                 WHERE user_id = ?1 AND conversation_id = ?2",
                params![user_id, conversation_id],
                |row| row.get::<_, i64>(0),
            )
            .optional()
            .ok()
            .flatten()
            == Some(1)
    }

    /// Turn auto-approve on or off for the acting user's conversation.
    pub fn set_yolo(&self, user_id: &str, conversation_id: &str, enabled: bool) -> Result<()> {
        valid_conversation_id(conversation_id)?;
        let connection = self.connection()?;
        connection.execute(
            "INSERT INTO conversation_yolo (user_id, conversation_id, enabled, updated_at)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(user_id, conversation_id) DO UPDATE SET
                 enabled = excluded.enabled,
                 updated_at = excluded.updated_at",
            params![user_id, conversation_id, i64::from(enabled), now()],
        )?;
        Ok(())
    }
}

/// JSON shape shared by the API and the `plan.updated` event.
pub fn plan_json(plan: &Plan) -> Value {
    json!({
        "conversation_id": plan.conversation_id,
        "goal": plan.goal,
        "items": plan.items,
        "done": plan.done_count(),
        "total": plan.items.len(),
        "updated_at": plan.updated_at,
    })
}

/// `plan_update`: record the goal and checklist of the current conversation.
pub struct PlanUpdateTool {
    store: Store,
}

impl PlanUpdateTool {
    pub fn new(store: Store) -> Self {
        Self { store }
    }
}

#[async_trait]
impl Tool for PlanUpdateTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: PLAN_TOOL.into(),
            description: "Record or update the goal and todo checklist of this conversation. Call it when the owner asks for a multi-step task, then again whenever an item changes status. Send the whole list every time.".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "goal": {
                        "type": "string",
                        "description": "One line stating what the owner wants done"
                    },
                    "items": {
                        "type": "array",
                        "maxItems": MAX_PLAN_ITEMS,
                        "items": {
                            "type": "object",
                            "properties": {
                                "text": {"type": "string"},
                                "status": {"type": "string", "enum": ["pending", "doing", "done"]}
                            },
                            "required": ["text", "status"],
                            "additionalProperties": false
                        }
                    }
                },
                "required": ["goal", "items"],
                "additionalProperties": false
            }),
            external_effect: false,
        }
    }

    async fn execute(&self, args: Value, context: &ToolContext) -> Result<ToolOutput> {
        let conversation_id = context
            .conversation_id
            .as_deref()
            .ok_or_else(|| SeraError::Validation("plan_update needs a conversation".into()))?;
        let (goal, items) = parse_plan_args(&args)?;
        let plan = self
            .store
            .set_plan(&context.user_id, conversation_id, &goal, &items)?;
        let payload = plan_json(&plan);
        // Best effort: the plan row is the record, the event is a push.
        let _ = self
            .store
            .append_event(&context.user_id, PLAN_EVENT, &payload);
        Ok(ToolOutput {
            content: format!(
                "Plan saved: {} of {} done.",
                plan.done_count(),
                plan.items.len()
            ),
            data: Some(payload),
            ..ToolOutput::default()
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store_with_users() -> (Store, String, String) {
        let store = Store::open_in_memory(384).unwrap();
        let admin = store.admin_user_id().unwrap();
        let other = store
            .create_user("microsoft:plan-other", "plan@example.example", "Plan")
            .unwrap()
            .id;
        (store, admin, other)
    }

    #[test]
    fn plan_args_are_validated_and_cleaned() {
        let (goal, items) = parse_plan_args(&json!({
            "goal": "  Ship\u{7} the report \n now ",
            "items": [{"text": "Draft", "status": "done"}, {"text": "Send"}]
        }))
        .unwrap();
        assert_eq!(goal, "Ship the report now");
        assert_eq!(items[0].status, PlanStatus::Done);
        assert_eq!(items[1].status, PlanStatus::Pending);
        for bad in [
            json!({"items": []}),
            json!({"goal": "g"}),
            json!({"goal": "g", "items": [{"text": ""}]}),
            json!({"goal": "g", "items": [{"text": "a", "status": "bogus"}]}),
            json!({"goal": "g", "items": vec![json!({"text": "a"}); MAX_PLAN_ITEMS + 1]}),
        ] {
            assert!(parse_plan_args(&bad).is_err(), "{bad}");
        }
        let long = parse_plan_args(&json!({
            "goal": "g".repeat(500),
            "items": [{"text": "t".repeat(500), "status": "doing"}]
        }))
        .unwrap();
        assert_eq!(long.0.chars().count(), MAX_GOAL_CHARS);
        assert_eq!(long.1[0].text.chars().count(), MAX_ITEM_CHARS);
    }

    #[test]
    fn plans_are_per_user_and_per_conversation() {
        let (store, admin, other) = store_with_users();
        let items = vec![
            PlanItem {
                text: "one".into(),
                status: PlanStatus::Done,
            },
            PlanItem {
                text: "two".into(),
                status: PlanStatus::Doing,
            },
        ];
        assert!(store.get_plan(&admin, "dir:main").unwrap().is_none());
        let plan = store.set_plan(&admin, "dir:main", "goal", &items).unwrap();
        assert_eq!((plan.done_count(), plan.items.len()), (1, 2));
        assert!(!plan.is_complete());
        assert_eq!(
            store.get_plan(&admin, "dir:main").unwrap().unwrap().goal,
            "goal"
        );
        // Same conversation id, other user and other conversation: untouched.
        assert!(store.get_plan(&other, "dir:main").unwrap().is_none());
        assert!(store.get_plan(&admin, "dir:other").unwrap().is_none());
        // Replace, then clear only the owner's row.
        store.set_plan(&other, "dir:main", "theirs", &[]).unwrap();
        store.clear_plan(&admin, "dir:main").unwrap();
        assert!(store.get_plan(&admin, "dir:main").unwrap().is_none());
        assert_eq!(
            store.get_plan(&other, "dir:main").unwrap().unwrap().goal,
            "theirs"
        );
    }

    #[test]
    fn yolo_defaults_off_and_is_scoped_to_one_user_and_conversation() {
        let (store, admin, other) = store_with_users();
        assert!(!store.yolo_enabled(&admin, "dir:main"));
        store.set_yolo(&admin, "dir:main", true).unwrap();
        assert!(store.yolo_enabled(&admin, "dir:main"));
        assert!(!store.yolo_enabled(&other, "dir:main"));
        assert!(!store.yolo_enabled(&admin, "dir:other"));
        store.set_yolo(&admin, "dir:main", false).unwrap();
        assert!(!store.yolo_enabled(&admin, "dir:main"));
        assert!(!store.yolo_enabled(&admin, ""));
    }

    #[tokio::test]
    async fn plan_tool_writes_only_the_acting_users_conversation() {
        let (store, admin, other) = store_with_users();
        let tool = PlanUpdateTool::new(store.clone());
        let context = |user: &str| ToolContext {
            user_id: user.into(),
            actor: user.into(),
            bot: "orchestrator".into(),
            task_id: None,
            turn_id: None,
            conversation_id: Some("dir:main".into()),
            allowed_tools: Default::default(),
            require_approval: Default::default(),
        };
        let args = json!({"goal": "g", "items": [{"text": "a", "status": "doing"}]});
        let out = tool.execute(args, &context(&admin)).await.unwrap();
        assert!(out.content.contains("0 of 1"));
        assert!(store.get_plan(&admin, "dir:main").unwrap().is_some());
        assert!(store.get_plan(&other, "dir:main").unwrap().is_none());
        let mut no_conversation = context(&admin);
        no_conversation.conversation_id = None;
        assert!(
            tool.execute(json!({"goal": "g", "items": []}), &no_conversation)
                .await
                .is_err()
        );
    }
}
