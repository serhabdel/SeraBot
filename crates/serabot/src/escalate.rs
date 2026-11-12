//! Specialist → orchestrator (or peer) escalation when a turn lacks a tool.

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{Value, json};
use tracing::warn;

use crate::domain::new_id;
use crate::error::{Result, SeraError};
use crate::fleet::{Fleet, FleetRegistry};
use crate::gateway::{Tool, ToolContext, ToolOutput, ToolSpec};
use crate::model_router::ModelRouter;
use crate::orchestration::{BotRole, ORCHESTRATOR_BOT};
use crate::runtime::TurnPayload;
use crate::store::Store;
use crate::teams::enqueue_turn_task_with_id;

pub const ESCALATE_TOOL: &str = "escalate";

const MAX_OBJECTIVE_LEN: usize = 2000;
const MAX_TOOL_NAME_LEN: usize = 64;

#[derive(Clone)]
pub struct EscalateTool {
    store: Store,
    fleet: Arc<FleetRegistry>,
    router: ModelRouter,
}

impl EscalateTool {
    pub fn new(store: Store, fleet: Arc<FleetRegistry>, router: ModelRouter) -> Self {
        Self {
            store,
            fleet,
            router,
        }
    }

    fn orchestrator<'a>(&self, fleet: &'a Fleet) -> Result<&'a crate::config::LoadedBot> {
        fleet
            .get(ORCHESTRATOR_BOT)
            .filter(|bot| bot.config.enabled && bot.config.role == BotRole::Orchestrator)
            .ok_or_else(|| SeraError::Config("orchestrator bot is not configured".into()))
    }
}

#[async_trait]
impl Tool for EscalateTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: ESCALATE_TOOL.into(),
            description: "Queue the same request for main or another teammate that has the tools \
                          you lack. Use when the owner's task needs a capability not in your \
                          allowlist (web search, browser, another specialist). Say what is missing, \
                          then call this instead of refusing."
                .into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "objective": {
                        "type": "string",
                        "description": "What main or the peer should do for the owner (max 2000 chars)"
                    },
                    "needed_tool": {
                        "type": "string",
                        "description": "Optional tool or connector id you lack (e.g. tavily_search, browser_navigate)"
                    },
                    "to_bot": {
                        "type": "string",
                        "description": "Target bot: default main; or an enabled specialist main delegates to"
                    }
                },
                "required": ["objective"],
                "additionalProperties": false
            }),
            external_effect: false,
        }
    }

    async fn execute(&self, args: Value, context: &ToolContext) -> Result<ToolOutput> {
        if context.bot == ORCHESTRATOR_BOT {
            return Err(SeraError::Forbidden(
                "main should delegate with delegate, not escalate".into(),
            ));
        }
        let fleet = self.fleet.load(&context.user_id)?;
        let orchestrator = self.orchestrator(&fleet)?;
        let conversation_id = context
            .conversation_id
            .as_deref()
            .ok_or_else(|| SeraError::InvalidState("escalate requires a conversation".into()))?;
        self.store
            .ensure_conversation_accepts_turns(&context.user_id, conversation_id)?;

        let objective = args
            .get("objective")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .trim();
        if objective.is_empty() || objective.chars().count() > MAX_OBJECTIVE_LEN {
            return Err(SeraError::Validation("invalid escalation objective".into()));
        }
        let needed_tool = args
            .get("needed_tool")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty() && s.chars().count() <= MAX_TOOL_NAME_LEN)
            .map(str::to_owned);
        let to_bot = args
            .get("to_bot")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .unwrap_or(ORCHESTRATOR_BOT)
            .to_string();
        if to_bot == context.bot {
            return Err(SeraError::Validation(
                "escalate cannot target the same bot".into(),
            ));
        }
        let target = fleet
            .get(&to_bot)
            .ok_or_else(|| SeraError::NotFound(format!("bot {to_bot}")))?;
        if !target.config.enabled {
            return Err(SeraError::Validation(
                "escalation target is disabled".into(),
            ));
        }
        if to_bot != ORCHESTRATOR_BOT {
            if target.config.role != BotRole::Specialist {
                return Err(SeraError::Validation("invalid escalation target".into()));
            }
            if !orchestrator.config.delegates.iter().any(|d| d == &to_bot) {
                return Err(SeraError::Validation(
                    "escalation to_bot must be delegated by main".into(),
                ));
            }
        }

        let mut text = format!("[Escalation from {from}] {objective}", from = context.bot);
        if let Some(tool) = &needed_tool {
            text.push_str(&format!("\nMissing capability: {tool}."));
        }

        let selection = self.router.select_class(
            &context.user_id,
            Some("routine"),
            target.config.model.as_deref(),
            &target.config.name,
            &Default::default(),
        )?;
        let route = self.router.pin(&selection)?;
        let payload = TurnPayload {
            conversation_id: conversation_id.to_owned(),
            text,
            bot: to_bot.clone(),
            source: "escalation".into(),
            ask_mode: false,
            route: Some(route),
            ..TurnPayload::default()
        };

        let child_task_id = new_id();
        self.store.record_audit(
            &context.actor,
            "escalate.queued",
            Some(ESCALATE_TOOL),
            context.task_id.as_deref(),
            None,
            None,
            None,
            Some(&json!({
                "child_task_id": child_task_id,
                "conversation_id": conversation_id,
                "from_bot": context.bot,
                "to_bot": to_bot,
                "needed_tool": needed_tool,
            })),
        )?;
        let _ = self.store.append_event(
            &context.user_id,
            "escalate.queued",
            &json!({
                "child_task_id": child_task_id,
                "from_bot": context.bot,
                "to_bot": to_bot,
            }),
        );
        if let Err(error) = enqueue_turn_task_with_id(
            &self.store,
            &context.user_id,
            &child_task_id,
            &to_bot,
            &serde_json::to_value(&payload)?,
        ) {
            warn!(%to_bot, %error, "audited escalation was not queued");
            return Err(error);
        }

        Ok(ToolOutput {
            content: format!(
                "Queued escalation to {to_bot}. The owner will get a reply when that bot finishes."
            ),
            data: Some(json!({
                "escalation_queued": true,
                "child_task_id": child_task_id,
                "to_bot": to_bot,
                "needed_tool": needed_tool,
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
    use std::collections::{BTreeMap, HashSet};

    fn admin(store: &Store) -> String {
        store.admin_user_id().unwrap()
    }

    fn bot(name: &str, role: BotRole, delegates: Vec<String>, tools: Vec<String>) -> LoadedBot {
        LoadedBot {
            config: BotConfig {
                title: None,
                description: None,
                name: name.into(),
                trigger: "command".into(),
                model: None,
                system_prompt: format!("prompts/{name}.txt"),
                skills: vec![],
                tools,
                require_approval: vec![],
                enabled: true,
                temperature: None,
                thinking: None,
                role,
                delegates,
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
                    bot(
                        ORCHESTRATOR_BOT,
                        BotRole::Orchestrator,
                        vec!["legal".into()],
                        vec![ESCALATE_TOOL.into()],
                    ),
                    bot(
                        "legal",
                        BotRole::Specialist,
                        vec![],
                        vec![ESCALATE_TOOL.into()],
                    ),
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

    #[tokio::test]
    async fn specialist_queues_main() {
        let store = Store::open_in_memory(384).unwrap();
        let user = admin(&store);
        store
            .create_conversation(&user, "dir:esc", "direct", None)
            .unwrap();
        let task = store
            .enqueue_task(
                &user,
                "turn",
                "legal",
                &json!({"conversation_id":"dir:esc","text":"find law","bot":"legal"}),
                0,
                chrono::Utc::now(),
            )
            .unwrap();
        let turn = store
            .create_turn(&user, &task.id, "legal", "dir:esc", &json!({}))
            .unwrap();
        let tool = EscalateTool::new(store.clone(), fleet(&store), router());
        let context = ToolContext {
            user_id: user.clone(),
            actor: user,
            bot: "legal".into(),
            task_id: Some(task.id),
            turn_id: Some(turn.id),
            conversation_id: Some("dir:esc".into()),
            allowed_tools: HashSet::from([ESCALATE_TOOL.into()]),
            require_approval: HashSet::new(),
        };
        let out = tool
            .execute(
                json!({"objective":"Research statute online", "needed_tool":"tavily_search"}),
                &context,
            )
            .await
            .unwrap();
        assert!(out.data.as_ref().unwrap()["escalation_queued"].as_bool() == Some(true));
    }
}
