use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{Value, json};
use tracing::warn;

use crate::config::LoadedBot;
use crate::domain::new_id;
use crate::error::{Result, SeraError};
use crate::gateway::{Tool, ToolContext, ToolOutput, ToolSpec};
use crate::memory;
use crate::model_router::ModelRouter;
use crate::orchestration::HandoffContext;
use crate::orchestration::{BotRole, Handoff, ModelClass, ORCHESTRATOR_BOT, validate_handoff};
use crate::store::Store;
use crate::teams::enqueue_turn_task_with_id;

pub const DELEGATE_TOOL: &str = "delegate";
pub const HANDOFF_QUEUED_KEY: &str = "handoff_queued";

#[derive(Clone)]
pub struct DelegateTool {
    store: Store,
    bots: Arc<HashMap<String, LoadedBot>>,
    router: ModelRouter,
}

impl DelegateTool {
    pub fn new(store: Store, bots: Arc<HashMap<String, LoadedBot>>, router: ModelRouter) -> Self {
        Self {
            store,
            bots,
            router,
        }
    }

    fn orchestrator(&self) -> Result<&LoadedBot> {
        self.bots
            .get(ORCHESTRATOR_BOT)
            .filter(|bot| bot.config.enabled && bot.config.role == BotRole::Orchestrator)
            .ok_or_else(|| SeraError::Config("orchestrator bot is not configured".into()))
    }
}

#[async_trait]
impl Tool for DelegateTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: DELEGATE_TOOL.into(),
            description: "Queue one bounded handoff from the orchestrator to an enabled specialist. Only main may call this; at most one handoff per turn.".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "to_bot": {"type": "string", "description": "Enabled specialist listed in main.delegates"},
                    "objective": {"type": "string", "description": "Non-empty specialist objective (max 2000 characters)"},
                    "skill": {"type": "string", "description": "Optional skill id from the target bot allowlist"},
                    "memory_ids": {
                        "type": "array",
                        "items": {"type": "string"},
                        "maxItems": 5,
                        "description": "Existing memory ids to attach (max 5)"
                    },
                    "model_class": {
                        "type": "string",
                        "enum": ["routine", "synthesis"],
                        "description": "Logical model class for the specialist turn"
                    }
                },
                "required": ["to_bot", "objective", "model_class"],
                "additionalProperties": false
            }),
            external_effect: false,
        }
    }

    async fn execute(&self, args: Value, context: &ToolContext) -> Result<ToolOutput> {
        if context.bot != ORCHESTRATOR_BOT {
            return Err(SeraError::Forbidden(
                "only the orchestrator may delegate".into(),
            ));
        }
        let orchestrator = self.orchestrator()?;
        let task_id = context
            .task_id
            .as_deref()
            .ok_or_else(|| SeraError::InvalidState("delegate requires an active task".into()))?;
        let turn_id = context
            .turn_id
            .as_deref()
            .ok_or_else(|| SeraError::InvalidState("delegate requires an active turn".into()))?;
        let conversation_id = context
            .conversation_id
            .as_deref()
            .ok_or_else(|| SeraError::InvalidState("delegate requires a conversation id".into()))?;
        self.store
            .ensure_conversation_accepts_turns(conversation_id)?;

        let to_bot = args
            .get("to_bot")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .trim()
            .to_string();
        if !orchestrator.config.delegates.iter().any(|d| d == &to_bot) {
            return Err(SeraError::Validation(
                "handoff to_bot is not delegated by main".into(),
            ));
        }
        let specialist = self
            .bots
            .get(&to_bot)
            .ok_or_else(|| SeraError::NotFound(format!("bot {to_bot}")))?;
        if !specialist.config.enabled || specialist.config.role != BotRole::Specialist {
            return Err(SeraError::Validation("invalid handoff to_bot".into()));
        }

        let objective = args
            .get("objective")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .trim()
            .to_string();
        let skill = args
            .get("skill")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_owned);
        let memory_ids: Vec<String> = args
            .get("memory_ids")
            .and_then(Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(str::to_owned)
                    .collect()
            })
            .unwrap_or_default();
        for memory_id in &memory_ids {
            memory::get(&self.store, memory_id)?;
        }
        let model_class = match args
            .get("model_class")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .trim()
        {
            "routine" => ModelClass::Routine,
            "synthesis" => ModelClass::Synthesis,
            _ => {
                return Err(SeraError::Validation(
                    "model_class must be routine or synthesis".into(),
                ));
            }
        };

        let handoff = Handoff {
            id: new_id(),
            parent_task_id: task_id.to_owned(),
            conversation_id: conversation_id.to_owned(),
            from_bot: ORCHESTRATOR_BOT.into(),
            to_bot: to_bot.clone(),
            skill,
            objective: objective.clone(),
            memory_ids: memory_ids.clone(),
            model_class,
        };
        validate_handoff(&handoff, &specialist.config.skills)?;

        let class_name = match model_class {
            ModelClass::Routine => "routine",
            ModelClass::Synthesis => "synthesis",
        };
        let selection = self.router.select_class(
            Some(class_name),
            specialist.config.model.as_deref(),
            &specialist.config.name,
            &Default::default(),
        )?;
        let route = self.router.pin(&selection)?;

        let payload = json!({
            "conversation_id": conversation_id,
            "text": handoff.objective,
            "bot": to_bot,
            "source": "handoff",
            "ask_mode": false,
            "route": route,
            "handoff": HandoffContext {
                handoff_id: handoff.id.clone(),
                parent_task_id: task_id.to_owned(),
                parent_turn_id: turn_id.to_owned(),
                memory_ids: memory_ids.clone(),
                skill: handoff.skill.clone(),
            },
        });
        // The handoff is audited before the specialist turn is queued: a failed
        // audit must never leave an untracked queued turn behind. The audit row
        // needs the child task id first and `Store::enqueue_task` allocates that
        // id itself, so the id is allocated here and the turn is queued under it.
        let child_task_id = new_id();
        self.store.record_audit(
            &context.actor,
            "handoff.queued",
            Some(DELEGATE_TOOL),
            Some(task_id),
            None,
            None,
            None,
            Some(&json!({
                "handoff_id": handoff.id,
                "child_task_id": child_task_id,
                "conversation_id": conversation_id,
                "from_bot": ORCHESTRATOR_BOT,
                "to_bot": to_bot,
                "skill": handoff.skill,
                "memory_ids": memory_ids,
                "model_class": class_name,
            })),
        )?;
        self.store.append_event(
            "handoff.queued",
            &json!({
                "handoff_id": handoff.id,
                "parent_task_id": task_id,
                "child_task_id": child_task_id,
                "to_bot": to_bot,
            }),
        )?;
        if let Err(error) =
            enqueue_turn_task_with_id(&self.store, &child_task_id, &to_bot, &payload)
        {
            warn!(bot = %to_bot, %error, "audited handoff was not queued");
            return Err(error);
        }

        Ok(ToolOutput {
            content: format!("Queued handoff {id} to {to_bot}.", id = handoff.id),
            data: Some(json!({
                HANDOFF_QUEUED_KEY: true,
                "handoff_id": handoff.id,
                "child_task_id": child_task_id,
                "to_bot": to_bot,
            })),
            ..ToolOutput::default()
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{BotConfig, Config, LoadedBot};
    use crate::llm::LlmClient;
    use rusqlite::params;
    use std::collections::{BTreeMap, HashSet};

    const ROOM: &str = "room:review";

    /// A room conversation with the orchestrator task that asked for a handoff.
    fn store() -> (Store, String) {
        let store = Store::open_in_memory(384).unwrap();
        store.create_conversation(ROOM, "room", None).unwrap();
        let parent = store
            .enqueue_task(
                "turn",
                ORCHESTRATOR_BOT,
                &json!({"conversation_id": ROOM, "text": "synthetic", "bot": ORCHESTRATOR_BOT}),
                0,
                chrono::Utc::now(),
            )
            .unwrap();
        (store, parent.id)
    }

    fn bot(name: &str, role: BotRole, delegates: Vec<String>) -> LoadedBot {
        LoadedBot {
            config: BotConfig {
                name: name.into(),
                trigger: "command".into(),
                model: None,
                system_prompt: format!("prompts/{name}.txt"),
                skills: vec![],
                tools: vec![],
                require_approval: vec![],
                enabled: true,
                temperature: None,
                role,
                delegates,
            },
            prompt: name.into(),
        }
    }

    fn bots() -> Arc<HashMap<String, LoadedBot>> {
        Arc::new(
            [
                bot(
                    ORCHESTRATOR_BOT,
                    BotRole::Orchestrator,
                    vec!["analyst".to_string()],
                ),
                bot("analyst", BotRole::Specialist, vec![]),
            ]
            .into_iter()
            .map(|bot| (bot.config.name.clone(), bot))
            .collect(),
        )
    }

    fn router() -> ModelRouter {
        let mut config = Config::default();
        config.llm.data_egress_approved = true;
        config.llm.model = "test".into();
        config.llm.base_url = "https://example.com/v1".into();
        config.llm.models = vec!["test-alternative".into()];
        ModelRouter::new(
            &config.llm,
            BTreeMap::from([(
                "default".into(),
                LlmClient::new("https://example.com/v1", "test", 5, 50)
                    .unwrap()
                    .with_model("test"),
            )]),
        )
        .unwrap()
    }

    fn context(task_id: &str, turn_id: &str) -> ToolContext {
        ToolContext {
            actor: "owner".into(),
            bot: ORCHESTRATOR_BOT.into(),
            task_id: Some(task_id.into()),
            turn_id: Some(turn_id.into()),
            conversation_id: Some(ROOM.into()),
            allowed_tools: HashSet::from([DELEGATE_TOOL.to_string()]),
            require_approval: HashSet::new(),
        }
    }

    fn args() -> Value {
        json!({
            "to_bot": "analyst",
            "objective": "Summarize the synthetic cash position.",
            "model_class": "routine",
        })
    }

    /// Specialist turns that are queued or running.
    fn queued_specialist_tasks(store: &Store) -> i64 {
        let connection = store.connection().unwrap();
        connection
            .query_row(
                "SELECT COUNT(*) FROM tasks
                 WHERE bot='analyst' AND status IN ('queued','running','awaiting_approval')",
                params![],
                |row| row.get(0),
            )
            .unwrap()
    }

    #[tokio::test]
    async fn delegate_audits_the_child_task_it_queues() {
        let (store, parent) = store();
        let tool = DelegateTool::new(store.clone(), bots(), router());
        let turn = store
            .create_turn(&parent, ORCHESTRATOR_BOT, ROOM, &json!({}))
            .unwrap();
        let context = context(&parent, &turn.id);
        let output = tool.execute(args(), &context).await.unwrap();
        let child = output
            .data
            .as_ref()
            .and_then(|data| data.get("child_task_id"))
            .and_then(Value::as_str)
            .expect("child task id")
            .to_owned();
        // The audit row names the specialist turn that was queued.
        let entry = store
            .list_audit(20)
            .unwrap()
            .into_iter()
            .find(|entry| entry.event == "handoff.queued")
            .expect("handoff.queued audit row");
        let details = entry.details.expect("audit details");
        assert_eq!(
            details.get("child_task_id").and_then(Value::as_str),
            Some(child.as_str())
        );
        assert_eq!(
            details.get("to_bot").and_then(Value::as_str),
            Some("analyst")
        );
        assert_eq!(
            details.get("from_bot").and_then(Value::as_str),
            Some(ORCHESTRATOR_BOT)
        );
        assert_eq!(
            details.get("conversation_id").and_then(Value::as_str),
            Some(ROOM)
        );
        assert_eq!(
            details.get("model_class").and_then(Value::as_str),
            Some("routine")
        );
        // The queued task is the task the audit names.
        let task = store.get_task(&child).unwrap();
        assert_eq!(task.bot, "analyst");
        assert_eq!(task.kind, "turn");
        assert_eq!(task.status, "queued");
        assert_eq!(queued_specialist_tasks(&store), 1);
    }

    #[tokio::test]
    async fn delegate_queues_no_child_task_when_the_audit_fails() {
        let (store, parent) = store();
        let tool = DelegateTool::new(store.clone(), bots(), router());
        let turn = store
            .create_turn(&parent, ORCHESTRATOR_BOT, ROOM, &json!({}))
            .unwrap();
        let context = context(&parent, &turn.id);
        // The event stream write fails, so the handoff is not audited.
        {
            let connection = store.connection().unwrap();
            connection.execute_batch("DROP TABLE events").unwrap();
        }
        let error = tool.execute(args(), &context).await.unwrap_err();
        assert!(
            error.to_string().contains("no such table: events"),
            "{error}"
        );
        // No specialist turn was queued: the audit came first and failed.
        assert_eq!(queued_specialist_tasks(&store), 0);
        assert_eq!(store.pending_task_count().unwrap(), 1);
    }
}
