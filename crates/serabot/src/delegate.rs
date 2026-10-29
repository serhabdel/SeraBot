use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use chrono::Utc;
use serde_json::{Value, json};

use crate::config::LoadedBot;
use crate::domain::new_id;
use crate::error::{Result, SeraError};
use crate::gateway::{Tool, ToolContext, ToolOutput, ToolSpec};
use crate::memory;
use crate::model_router::ModelRouter;
use crate::orchestration::HandoffContext;
use crate::orchestration::{BotRole, Handoff, ModelClass, ORCHESTRATOR_BOT, validate_handoff};
use crate::store::Store;

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
        let child = self
            .store
            .enqueue_task("turn", &to_bot, &payload, 0, Utc::now())?;
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
                "child_task_id": child.id,
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
                "child_task_id": child.id,
                "to_bot": to_bot,
            }),
        )?;

        Ok(ToolOutput {
            content: format!("Queued handoff {id} to {to_bot}.", id = handoff.id),
            data: Some(json!({
                HANDOFF_QUEUED_KEY: true,
                "handoff_id": handoff.id,
                "child_task_id": child.id,
                "to_bot": to_bot,
            })),
            ..ToolOutput::default()
        })
    }
}
