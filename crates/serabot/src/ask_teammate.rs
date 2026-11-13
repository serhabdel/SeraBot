//! `ask_teammate`: a specialist hands a step to main or another enabled bot.
//!
//! `delegate` stays orchestrator-only. This tool is the other direction: a
//! bot that does not have the tool the step needs queues one follow-up turn
//! instead of telling the owner it cannot do the work. The child turn runs
//! as the same user, through the gateway, so destructive actions still wait
//! for approval. Depth is capped at [`MAX_HELP_DEPTH`] so the work cannot
//! bounce forever.

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{Value, json};

use crate::collaboration::{ASK_TEAMMATE_TOOL, MAX_HELP_DEPTH};
use crate::domain::new_id;
use crate::error::{Result, SeraError};
use crate::fleet::FleetRegistry;
use crate::gateway::{Tool, ToolContext, ToolOutput, ToolSpec};
use crate::model_router::ModelRouter;
use crate::store::Store;
use crate::teams::enqueue_turn_task_with_id;
use crate::util::truncate_chars;

pub const ASK_QUEUED_KEY: &str = "ask_queued";
const MAX_OBJECTIVE: usize = 2000;
const MAX_REASON: usize = 300;

#[derive(Clone)]
pub struct AskTeammateTool {
    store: Store,
    fleet: Arc<FleetRegistry>,
    router: ModelRouter,
}

impl AskTeammateTool {
    pub fn new(store: Store, fleet: Arc<FleetRegistry>, router: ModelRouter) -> Self {
        Self {
            store,
            fleet,
            router,
        }
    }
}

fn plain(text: &str, max: usize) -> String {
    let cleaned: String = text
        .chars()
        .map(|character| {
            if character.is_control() {
                ' '
            } else {
                character
            }
        })
        .collect();
    truncate_chars(cleaned.trim(), max)
}

fn objective_text(from: &str, reason: &str, objective: &str) -> String {
    let mut text = format!(
        "Teammate {from} asked you to finish this work. Complete it with your tools. Do not hand the same work back to {from}."
    );
    if !reason.is_empty() {
        text.push_str("\nWhy: ");
        text.push_str(reason);
    }
    text.push_str("\n\n");
    text.push_str(objective);
    text
}

#[async_trait]
impl Tool for AskTeammateTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: ASK_TEAMMATE_TOOL.into(),
            description: "Hand one step to main or another enabled teammate who has the tool you lack. \
                          Use it instead of telling the owner you cannot do the task. One handoff per \
                          turn. The teammate runs as this owner; sends, deletes, payments and uploads \
                          still wait for approval. Do not use it to retry a call the owner denied."
                .into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "to_bot": {"type": "string", "description": "Enabled bot id, usually main"},
                    "objective": {"type": "string", "description": "What the teammate should finish"},
                    "reason": {"type": "string", "description": "Which tool or fact you could not reach"}
                },
                "required": ["to_bot", "objective"],
                "additionalProperties": false
            }),
            external_effect: false,
        }
    }

    async fn execute(&self, args: Value, context: &ToolContext) -> Result<ToolOutput> {
        let task_id = context.task_id.as_deref().ok_or_else(|| {
            SeraError::InvalidState("ask_teammate requires an active task".into())
        })?;
        let turn_id = context.turn_id.as_deref().ok_or_else(|| {
            SeraError::InvalidState("ask_teammate requires an active turn".into())
        })?;
        let conversation_id = context.conversation_id.as_deref().ok_or_else(|| {
            SeraError::InvalidState("ask_teammate requires a conversation id".into())
        })?;
        self.store
            .ensure_conversation_accepts_turns(&context.user_id, conversation_id)?;

        let parent = self.store.get_task(&context.user_id, task_id)?;
        let help_depth = parent
            .payload
            .get("help_depth")
            .and_then(Value::as_u64)
            .unwrap_or(0);
        if help_depth >= u64::from(MAX_HELP_DEPTH) {
            return Err(SeraError::Validation(
                "this turn already came from a teammate; finish it with the tools you have".into(),
            ));
        }
        let help_from = parent
            .payload
            .get("help_from")
            .and_then(Value::as_str)
            .unwrap_or("");

        let to_bot = plain(args.get("to_bot").and_then(Value::as_str).unwrap_or(""), 64);
        if to_bot.is_empty() || to_bot == context.bot || to_bot == help_from {
            return Err(SeraError::Validation(
                "to_bot must be a different enabled teammate".into(),
            ));
        }
        let fleet = self.fleet.load(&context.user_id)?;
        let target = fleet
            .get(&to_bot)
            .ok_or_else(|| SeraError::NotFound(format!("bot {to_bot}")))?;
        if !target.config.enabled {
            return Err(SeraError::Validation(
                "that teammate is turned off; pick an enabled bot or main".into(),
            ));
        }

        let objective = plain(
            args.get("objective").and_then(Value::as_str).unwrap_or(""),
            MAX_OBJECTIVE,
        );
        if objective.is_empty() {
            return Err(SeraError::Validation(
                "objective must say what the teammate should finish".into(),
            ));
        }
        let reason = plain(
            args.get("reason").and_then(Value::as_str).unwrap_or(""),
            MAX_REASON,
        );

        let selection = self.router.select_class(
            &context.user_id,
            Some("routine"),
            target.config.model.as_deref(),
            &target.config.name,
            &Default::default(),
        )?;
        let route = self.router.pin(&selection)?;
        let child_task_id = new_id();
        let text = objective_text(&context.bot, &reason, &objective);
        let payload = json!({
            "conversation_id": conversation_id,
            "text": text,
            "bot": to_bot,
            "source": "help",
            "ask_mode": false,
            "route": route,
            "help_depth": help_depth + 1,
            "help_from": context.bot,
        });
        self.store.record_audit(
            &context.actor,
            "help.queued",
            Some(ASK_TEAMMATE_TOOL),
            Some(task_id),
            None,
            None,
            None,
            Some(&json!({
                "child_task_id": child_task_id,
                "parent_turn_id": turn_id,
                "conversation_id": conversation_id,
                "from_bot": context.bot,
                "to_bot": to_bot,
            })),
        )?;
        self.store.append_event(
            &context.user_id,
            "help.queued",
            &json!({
                "parent_task_id": task_id,
                "child_task_id": child_task_id,
                "from_bot": context.bot,
                "to_bot": to_bot,
            }),
        )?;
        enqueue_turn_task_with_id(
            &self.store,
            &context.user_id,
            &child_task_id,
            &to_bot,
            &payload,
        )?;
        Ok(ToolOutput {
            content: format!(
                "Asked {to_bot} to finish this. Tell the owner you handed it on and end your turn. Do not redo the step."
            ),
            data: Some(json!({
                ASK_QUEUED_KEY: true,
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
    use crate::orchestration::{BotRole, ORCHESTRATOR_BOT};
    use rusqlite::params;
    use std::collections::{BTreeMap, HashSet};

    const ROOM: &str = "room:review";

    fn admin(store: &Store) -> String {
        store.admin_user_id().unwrap()
    }

    fn store() -> (Store, String) {
        let store = Store::open_in_memory(384).unwrap();
        let admin = admin(&store);
        store
            .create_conversation(&admin, ROOM, "room", None)
            .unwrap();
        let parent = store
            .enqueue_task(
                &admin,
                "turn",
                "legal",
                &json!({"conversation_id": ROOM, "text": "synthetic", "bot": "legal", "help_depth": 0}),
                0,
                chrono::Utc::now(),
            )
            .unwrap();
        (store, parent.id)
    }

    fn bot(name: &str, role: BotRole) -> LoadedBot {
        LoadedBot {
            config: BotConfig {
                title: None,
                description: None,
                name: name.into(),
                trigger: "command".into(),
                model: None,
                system_prompt: format!("prompts/{name}.txt"),
                skills: vec![],
                tools: vec![],
                require_approval: vec![],
                enabled: true,
                temperature: None,
                thinking: None,
                role,
                delegates: vec![],
            },
            prompt: name.into(),
        }
    }

    fn fleet(store: &Store) -> Arc<FleetRegistry> {
        let config = Arc::new(Config::default());
        let catalog = Arc::new(crate::skills::SkillCatalog::load(&config).unwrap());
        Arc::new(
            FleetRegistry::new(
                config,
                store.clone(),
                vec![
                    bot(ORCHESTRATOR_BOT, BotRole::Orchestrator),
                    bot("legal", BotRole::Specialist),
                ],
                catalog,
            )
            .unwrap(),
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

    fn context(store: &Store, task_id: &str, turn_id: &str) -> ToolContext {
        ToolContext {
            user_id: admin(store),
            actor: admin(store),
            bot: "legal".into(),
            task_id: Some(task_id.into()),
            turn_id: Some(turn_id.into()),
            conversation_id: Some(ROOM.into()),
            allowed_tools: HashSet::from([ASK_TEAMMATE_TOOL.to_string()]),
            require_approval: HashSet::new(),
        }
    }

    #[tokio::test]
    async fn a_specialist_can_hand_a_step_to_main() {
        let (store, parent) = store();
        let tool = AskTeammateTool::new(store.clone(), fleet(&store), router());
        let turn = store
            .create_turn(&admin(&store), &parent, "legal", ROOM, &json!({}))
            .unwrap();
        let output = tool
            .execute(
                json!({"to_bot": "main", "objective": "Search the public statute.", "reason": "no browser"}),
                &context(&store, &parent, &turn.id),
            )
            .await
            .unwrap();
        assert_eq!(
            output
                .data
                .as_ref()
                .and_then(|data| data.get(ASK_QUEUED_KEY)),
            Some(&json!(true))
        );
        let child = output
            .data
            .as_ref()
            .and_then(|data| data.get("child_task_id"))
            .and_then(Value::as_str)
            .unwrap();
        let task = store.get_task(&admin(&store), child).unwrap();
        assert_eq!(task.bot, "main");
        assert_eq!(task.payload["help_depth"], 1);
        assert_eq!(task.payload["help_from"], "legal");
        assert!(
            task.payload["text"]
                .as_str()
                .unwrap()
                .contains("public statute")
        );
    }

    #[tokio::test]
    async fn a_turn_that_already_came_from_a_teammate_cannot_hand_it_on() {
        let (store, parent) = store();
        store
            .connection()
            .unwrap()
            .execute(
                "UPDATE tasks SET payload_json = ?1 WHERE id = ?2",
                params![
                    json!({"conversation_id": ROOM, "help_depth": 1, "help_from": "main"})
                        .to_string(),
                    parent
                ],
            )
            .unwrap();
        let tool = AskTeammateTool::new(store.clone(), fleet(&store), router());
        let turn = store
            .create_turn(&admin(&store), &parent, "legal", ROOM, &json!({}))
            .unwrap();
        let error = tool
            .execute(
                json!({"to_bot": "main", "objective": "again"}),
                &context(&store, &parent, &turn.id),
            )
            .await
            .unwrap_err();
        assert!(
            error.to_string().contains("already came from a teammate"),
            "{error}"
        );
    }

    #[tokio::test]
    async fn a_bot_cannot_ask_itself() {
        let (store, parent) = store();
        let tool = AskTeammateTool::new(store.clone(), fleet(&store), router());
        let turn = store
            .create_turn(&admin(&store), &parent, "legal", ROOM, &json!({}))
            .unwrap();
        let error = tool
            .execute(
                json!({"to_bot": "legal", "objective": "same"}),
                &context(&store, &parent, &turn.id),
            )
            .await
            .unwrap_err();
        assert!(
            error.to_string().contains("different enabled teammate"),
            "{error}"
        );
    }
}
