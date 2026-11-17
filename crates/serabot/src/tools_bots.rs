//! Bot management tools for the orchestrator (v0.50).
//!
//! `main` can read the fleet and its model options, follow its subagents, and
//! (with the owner's approval) change a bot's route, pause a specialist, set
//! its delegates, or resume a failed subagent. Every change goes through
//! `fleet_mutation::apply`, the same path as the owner's own edits.
//!
//! Guards, adapted from openbot's `widensAgentSettings`:
//! - only `main` holds these tools, and never in a handoff, team or help turn
//!   (a prompt-injection guard: a specialist's output cannot steer the fleet);
//! - the acting user comes from the tool context, never from the arguments;
//!   an argument named `user_id` is rejected like any other unknown field;
//! - a bot never widens `tools`, `skills`, `require_approval`, its prompt or
//!   cross-provider egress consent (see `fleet_mutation::guard_no_widening`).
//!   Delegates are not covered by that guard: adding one gives main that
//!   specialist's tools indirectly, so `bot_set_delegates` states this on its
//!   approval card and the owner's approval is the control;
//! - `main`'s role and enabled state are never touched;
//! - targets are `main` itself (profile only) and the bots in `main.delegates`;
//! - a route change or a pause needs an idle target: no running task and no
//!   approval pending;
//! - the three setters always need the owner's approval, never a standing
//!   grant, never YOLO.

use std::sync::Arc;

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Map, Value, json};

use crate::config::{BotConfig, BotRoute, FailoverPolicy};
use crate::delegate::{MAX_RESUMES, ResumeOptions, resume_handoff};
use crate::error::{Result, SeraError};
use crate::fleet::{Fleet, FleetRegistry};
use crate::fleet_mutation::{self, BotPatch, MutationEnv, Origin};
use crate::gateway::{ApprovalDetails, Tool, ToolContext, ToolOutput, ToolRegistry, ToolSpec};
use crate::model_router::ModelRouter;
use crate::orchestration::{BotRole, ORCHESTRATOR_BOT, safe_bot_label};
use crate::policy::Policy;
use crate::store::Store;

pub const BOT_LIST_TOOL: &str = "bot_list";
pub const BOT_READ_TOOL: &str = "bot_read";
pub const MODEL_LIST_TOOL: &str = "model_list";
pub const HANDOFF_STATUS_TOOL: &str = "handoff_status";
pub const BOT_SET_ROUTE_TOOL: &str = "bot_set_route";
pub const BOT_SET_ENABLED_TOOL: &str = "bot_set_enabled";
pub const BOT_SET_DELEGATES_TOOL: &str = "bot_set_delegates";
pub const SUBAGENT_RETRY_TOOL: &str = "subagent_retry";

/// Every management tool, for the orchestrator only.
pub const MANAGEMENT_TOOLS: [&str; 8] = [
    BOT_LIST_TOOL,
    BOT_READ_TOOL,
    MODEL_LIST_TOOL,
    HANDOFF_STATUS_TOOL,
    BOT_SET_ROUTE_TOOL,
    BOT_SET_ENABLED_TOOL,
    BOT_SET_DELEGATES_TOOL,
    SUBAGENT_RETRY_TOOL,
];

pub fn is_management_tool(name: &str) -> bool {
    MANAGEMENT_TOOLS.contains(&name)
}

/// Whether a turn may use the management tools, judged from its task
/// payload: an owner turn may; a handoff, team, help or routine turn may not.
pub fn turn_may_manage(payload: &Value) -> bool {
    let present = |key: &str| payload.get(key).is_some_and(|value| !value.is_null());
    if present("handoff") || present("team") || present("help_from") {
        return false;
    }
    !matches!(
        payload.get("source").and_then(Value::as_str),
        Some("handoff" | "team" | "help" | "routine" | "schedule" | "brief" | "watch")
    )
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    BotList,
    BotRead,
    ModelList,
    HandoffStatus,
    SetRoute,
    SetEnabled,
    SetDelegates,
    SubagentRetry,
}

struct Deps {
    store: Store,
    fleet: Arc<FleetRegistry>,
    router: ModelRouter,
    policy: Arc<Policy>,
    child_daily_limit: u64,
}

/// One management tool. All eight share their dependencies.
#[derive(Clone)]
pub struct BotTool {
    kind: Kind,
    deps: Arc<Deps>,
}

/// Register the eight management tools.
pub fn register_bot_tools(
    registry: &mut ToolRegistry,
    store: &Store,
    fleet: &Arc<FleetRegistry>,
    router: &ModelRouter,
    policy: &Arc<Policy>,
    child_daily_limit: u64,
) -> Result<()> {
    let deps = Arc::new(Deps {
        store: store.clone(),
        fleet: fleet.clone(),
        router: router.clone(),
        policy: policy.clone(),
        child_daily_limit,
    });
    for kind in [
        Kind::BotList,
        Kind::BotRead,
        Kind::ModelList,
        Kind::HandoffStatus,
        Kind::SetRoute,
        Kind::SetEnabled,
        Kind::SetDelegates,
        Kind::SubagentRetry,
    ] {
        registry.register(BotTool {
            kind,
            deps: deps.clone(),
        })?;
    }
    Ok(())
}

fn double_option<'de, T, D>(deserializer: D) -> std::result::Result<Option<Option<T>>, D::Error>
where
    T: Deserialize<'de>,
    D: serde::Deserializer<'de>,
{
    Option::<T>::deserialize(deserializer).map(Some)
}

fn parse_args<T: serde::de::DeserializeOwned>(args: &Value) -> Result<T> {
    serde_json::from_value(args.clone())
        .map_err(|error| SeraError::Validation(format!("invalid arguments: {error}")))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct NoArgs {}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct BotArgs {
    bot: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SetRouteArgs {
    bot: String,
    #[serde(default, deserialize_with = "double_option")]
    route: Option<Option<BotRoute>>,
    #[serde(default)]
    fallbacks: Option<Vec<BotRoute>>,
    /// Only the fields to change; the rest keep their current value.
    #[serde(default)]
    failover: Option<Map<String, Value>>,
    #[serde(default, deserialize_with = "double_option")]
    thinking: Option<Option<String>>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SetEnabledArgs {
    bot: String,
    enabled: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SetDelegatesArgs {
    delegates: Vec<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct HandoffStatusArgs {
    #[serde(default)]
    handoff_id: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RetryArgs {
    handoff_id: String,
    #[serde(default)]
    route_override: Option<RouteOverride>,
}

/// Where a retry runs: a slot of the bot's own approved chain, or a full
/// route (which needs approval, since it may name a new provider).
#[derive(Deserialize)]
#[serde(untagged)]
enum RouteOverride {
    Slot(SlotArg),
    Route(BotRoute),
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SlotArg {
    /// 0 is the primary route, 1 and 2 the fallbacks.
    chain_index: usize,
}

fn route_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "provider": {"type": "string"},
            "model": {"type": "string"},
            "account": {"type": "string", "description": "Optional account id from model_list"},
            "thinking": {"type": "string"}
        },
        "required": ["provider", "model"],
        "additionalProperties": false
    })
}

impl BotTool {
    fn name(&self) -> &'static str {
        match self.kind {
            Kind::BotList => BOT_LIST_TOOL,
            Kind::BotRead => BOT_READ_TOOL,
            Kind::ModelList => MODEL_LIST_TOOL,
            Kind::HandoffStatus => HANDOFF_STATUS_TOOL,
            Kind::SetRoute => BOT_SET_ROUTE_TOOL,
            Kind::SetEnabled => BOT_SET_ENABLED_TOOL,
            Kind::SetDelegates => BOT_SET_DELEGATES_TOOL,
            Kind::SubagentRetry => SUBAGENT_RETRY_TOOL,
        }
    }

    fn is_setter(&self) -> bool {
        matches!(
            self.kind,
            Kind::SetRoute | Kind::SetEnabled | Kind::SetDelegates
        )
    }

    /// Refuse anything but an owner turn of `main`, and load the user's
    /// fleet. The user is the context's, never an argument.
    fn caller(&self, context: &ToolContext) -> Result<(Arc<Fleet>, String)> {
        let refuse = |reason: &str| {
            let _ = self.deps.store.record_audit(
                &context.actor,
                "bot_tools.refused",
                Some(self.name()),
                context.task_id.as_deref(),
                None,
                None,
                Some(self.deps.policy.version()),
                Some(&json!({"reason": reason, "bot": context.bot})),
            );
            Err(SeraError::Forbidden(
                "bot management is only available to main in a turn the owner started".into(),
            ))
        };
        if context.bot != ORCHESTRATOR_BOT {
            return refuse("not_orchestrator");
        }
        let task_id = context.task_id.as_deref().ok_or_else(|| {
            SeraError::InvalidState("bot management requires an active task".into())
        })?;
        let task = self.deps.store.get_task(&context.user_id, task_id)?;
        if !turn_may_manage(&task.payload) {
            return refuse("turn_source");
        }
        if let Some(turn_id) = &context.turn_id
            && let Ok(turn) = self.deps.store.get_turn(&context.user_id, turn_id)
            && ["team", "handoff"]
                .iter()
                .any(|key| turn.state.get(key).is_some_and(|value| !value.is_null()))
        {
            return refuse("turn_source");
        }
        let fleet = self.deps.fleet.load(&context.user_id)?;
        let main_ok = fleet
            .get(ORCHESTRATOR_BOT)
            .is_some_and(|bot| bot.config.enabled && bot.config.role == BotRole::Orchestrator);
        if !main_ok {
            return Err(SeraError::Config(
                "orchestrator bot is not configured".into(),
            ));
        }
        Ok((fleet, task_id.to_owned()))
    }

    /// `main` itself, or a bot in `main.delegates`.
    fn target<'a>(&self, fleet: &'a Fleet, bot: &str) -> Result<&'a BotConfig> {
        if !safe_bot_label(bot) {
            return Err(SeraError::NotFound(format!("bot {bot}")));
        }
        let main = fleet
            .get(ORCHESTRATOR_BOT)
            .ok_or_else(|| SeraError::Config("orchestrator bot is not configured".into()))?;
        if bot != ORCHESTRATOR_BOT && !main.config.delegates.iter().any(|name| name == bot) {
            return Err(SeraError::Forbidden(
                "that bot is not main or one of main's delegates".into(),
            ));
        }
        fleet
            .get(bot)
            .map(|loaded| &loaded.config)
            .ok_or_else(|| SeraError::NotFound(format!("bot {bot}")))
    }

    fn require_idle(&self, context: &ToolContext, task_id: &str, bot: &str) -> Result<()> {
        if self
            .deps
            .store
            .bot_is_busy(&context.user_id, bot, Some(task_id))?
        {
            return Err(SeraError::Rejected {
                code: "bot_busy",
                message: format!(
                    "{bot} has a task running or waiting for approval; try again when it is idle"
                ),
            });
        }
        Ok(())
    }

    async fn mutate(
        &self,
        context: &ToolContext,
        task_id: &str,
        bot: &str,
        patch: BotPatch,
    ) -> Result<fleet_mutation::Mutation> {
        let registered = |_: &str| false;
        let env = MutationEnv {
            store: &self.deps.store,
            fleet: &self.deps.fleet,
            router: &self.deps.router,
            policy: &self.deps.policy,
            tool_registered: &registered,
        };
        fleet_mutation::apply(
            &env,
            &context.user_id,
            bot,
            patch,
            Origin::tool(&context.actor, self.name(), Some(task_id)),
        )
        .await
    }

    fn bot_summary(
        &self,
        context: &ToolContext,
        config: &BotConfig,
        task_id: &str,
    ) -> Result<Value> {
        let settings = self
            .deps
            .store
            .bot_settings(&context.user_id, &config.name)?;
        let chain = self.deps.router.bot_chain(&context.user_id, config);
        Ok(json!({
            "id": config.name,
            "role": config.role.as_str(),
            "enabled": config.enabled,
            "display_name": settings.display_name,
            "model": config.model,
            "route": config.route,
            "fallbacks": config.fallbacks,
            "failover": config.failover,
            "thinking": config.thinking,
            "effective": chain.head.map(|head| json!({
                "provider": head.provider,
                "model": head.model,
                "account": head.account,
            })),
            "idle": !self.deps.store.bot_is_busy(&context.user_id, &config.name, Some(task_id))?,
        }))
    }

    fn output(content: Value) -> Result<ToolOutput> {
        Ok(ToolOutput {
            content: serde_json::to_string_pretty(&content)?,
            data: Some(content),
            ..ToolOutput::default()
        })
    }

    fn list(&self, context: &ToolContext) -> Result<ToolOutput> {
        let (fleet, task_id) = self.caller(context)?;
        let main = &fleet[ORCHESTRATOR_BOT];
        let mut bots = vec![self.bot_summary(context, &main.config, &task_id)?];
        for name in &main.config.delegates {
            if let Some(bot) = fleet.get(name) {
                bots.push(self.bot_summary(context, &bot.config, &task_id)?);
            }
        }
        Self::output(json!({ "bots": bots }))
    }

    fn read(&self, args: &Value, context: &ToolContext) -> Result<ToolOutput> {
        let (fleet, task_id) = self.caller(context)?;
        let args: BotArgs = parse_args(args)?;
        let config = self.target(&fleet, &args.bot)?;
        let mut summary = self.bot_summary(context, config, &task_id)?;
        summary["tools"] = json!(config.tools);
        summary["skills"] = json!(config.skills);
        summary["require_approval"] = json!(config.require_approval);
        summary["delegates"] = json!(config.delegates);
        Self::output(summary)
    }

    fn models(&self, context: &ToolContext) -> Result<ToolOutput> {
        self.caller(context)?;
        let entries = self
            .deps
            .router
            .catalog_entries(&context.user_id)
            .into_iter()
            .filter(|entry| entry.data_egress_approved)
            .take(300)
            .map(|entry| {
                json!({
                    "provider": entry.provider,
                    "model": entry.model,
                    "thinking_levels": entry.thinking_levels,
                    "context_length": entry.context_length,
                })
            })
            .collect::<Vec<_>>();
        // Accounts are listed by id, label and state only: never an identity
        // blob, a fingerprint or a credential.
        let accounts = self
            .deps
            .store
            .provider_accounts(&context.user_id, None)?
            .into_iter()
            .map(|account| {
                json!({
                    "provider": account.provider,
                    "id": account.account_id,
                    "label": account.label,
                    "state": account.status.as_str(),
                })
            })
            .collect::<Vec<_>>();
        Self::output(json!({ "models": entries, "accounts": accounts }))
    }

    fn handoffs(&self, args: &Value, context: &ToolContext) -> Result<ToolOutput> {
        self.caller(context)?;
        let args: HandoffStatusArgs = parse_args(args)?;
        match args.handoff_id {
            Some(id) => {
                let view = self.deps.store.handoff_view(&context.user_id, &id)?;
                Self::output(json!({ "handoff": view }))
            }
            None => {
                let views = self.deps.store.list_handoffs(
                    &context.user_id,
                    context.conversation_id.as_deref(),
                    50,
                )?;
                Self::output(json!({ "handoffs": views }))
            }
        }
    }

    async fn set_route(&self, args: &Value, context: &ToolContext) -> Result<ToolOutput> {
        let (fleet, task_id) = self.caller(context)?;
        let args: SetRouteArgs = parse_args(args)?;
        let config = self.target(&fleet, &args.bot)?;
        self.require_idle(context, &task_id, &args.bot)?;
        let mut patch = BotPatch {
            route: args.route,
            fallbacks: args.fallbacks,
            ..BotPatch::default()
        };
        if let Some(changes) = args.failover {
            let mut merged = serde_json::to_value(config.failover)?;
            for (key, value) in changes {
                merged[key] = value;
            }
            patch.failover = Some(serde_json::from_value::<FailoverPolicy>(merged).map_err(
                |error| SeraError::Validation(format!("invalid failover policy: {error}")),
            )?);
        }
        if let Some(level) = args.thinking {
            // The level rides on the head route; without one it is the bot's own.
            match patch.route.take() {
                Some(Some(mut route)) => {
                    route.thinking = level;
                    patch.route = Some(Some(route));
                }
                Some(None) => patch.route = Some(None),
                None => match config.route.clone() {
                    Some(mut route) => {
                        route.thinking = level;
                        patch.route = Some(Some(route));
                    }
                    None => patch.thinking = Some(level),
                },
            }
        }
        let mutation = self.mutate(context, &task_id, &args.bot, patch).await?;
        Self::output(json!({
            "bot": args.bot,
            "route": mutation.config.route,
            "fallbacks": mutation.config.fallbacks,
            "failover": mutation.config.failover,
            "thinking": mutation.config.thinking,
        }))
    }

    async fn set_enabled(&self, args: &Value, context: &ToolContext) -> Result<ToolOutput> {
        let (fleet, task_id) = self.caller(context)?;
        let args: SetEnabledArgs = parse_args(args)?;
        if args.bot == ORCHESTRATOR_BOT {
            return Err(SeraError::Forbidden(
                "main's role and enabled state are not changed by a bot".into(),
            ));
        }
        let target = self.target(&fleet, &args.bot)?;
        self.require_idle(context, &task_id, &args.bot)?;
        if args.enabled {
            // A delegate is always enabled, and a paused bot is no longer a
            // delegate, so a bot has no way to switch a bot on: that stays
            // with the owner.
            return Self::output(json!({
                "bot": args.bot,
                "enabled": target.enabled,
                "changed": false,
            }));
        }
        // Pause: leave main's delegate list first, so the fleet stays valid at
        // every step, then disable the bot.
        let remaining = fleet[ORCHESTRATOR_BOT]
            .config
            .delegates
            .iter()
            .filter(|name| **name != args.bot)
            .cloned()
            .collect::<Vec<_>>();
        self.mutate(
            context,
            &task_id,
            ORCHESTRATOR_BOT,
            BotPatch {
                delegates: Some(remaining),
                ..BotPatch::default()
            },
        )
        .await?;
        self.mutate(
            context,
            &task_id,
            &args.bot,
            BotPatch {
                enabled: Some(false),
                ..BotPatch::default()
            },
        )
        .await?;
        Self::output(json!({ "bot": args.bot, "enabled": false, "changed": true }))
    }

    async fn set_delegates(&self, args: &Value, context: &ToolContext) -> Result<ToolOutput> {
        let (_fleet, task_id) = self.caller(context)?;
        let args: SetDelegatesArgs = parse_args(args)?;
        self.require_idle(context, &task_id, ORCHESTRATOR_BOT)?;
        let mutation = self
            .mutate(
                context,
                &task_id,
                ORCHESTRATOR_BOT,
                BotPatch {
                    delegates: Some(args.delegates),
                    ..BotPatch::default()
                },
            )
            .await?;
        Self::output(json!({ "delegates": mutation.config.delegates }))
    }

    async fn retry(&self, args: &Value, context: &ToolContext) -> Result<ToolOutput> {
        let (fleet, task_id) = self.caller(context)?;
        let args: RetryArgs = parse_args(args)?;
        // A route the bot supplies is scanned and checked against the live
        // catalog, like the owner's own retry.
        if let Some(RouteOverride::Route(route)) = &args.route_override {
            for text in [&route.provider, &route.model]
                .into_iter()
                .chain(route.account.as_ref())
            {
                if !self.deps.policy.is_allowed(text) {
                    self.deps.store.record_audit(
                        &context.user_id,
                        "policy.blocked_egress",
                        None,
                        Some(&task_id),
                        None,
                        None,
                        Some(self.deps.policy.version()),
                        None,
                    )?;
                    return Err(SeraError::PolicyViolation);
                }
            }
            self.deps
                .router
                .ensure_catalogs(&context.user_id, std::time::Duration::from_secs(8))
                .await;
        }
        // The handoff is looked up under the acting user, so another user's id
        // is a plain not-found.
        let view = self
            .deps
            .store
            .handoff_view(&context.user_id, &args.handoff_id)?;
        let config = self.target(&fleet, &view.to_bot)?;
        let route = match &args.route_override {
            None => None,
            Some(RouteOverride::Route(route)) => Some(route.clone()),
            Some(RouteOverride::Slot(slot)) => {
                let chain = config
                    .route
                    .iter()
                    .chain(&config.fallbacks)
                    .collect::<Vec<_>>();
                Some(
                    chain
                        .get(slot.chain_index)
                        .map(|route| (*route).clone())
                        .ok_or_else(|| {
                            SeraError::Validation(
                                "chain_index is not a route of that bot's chain".into(),
                            )
                        })?,
                )
            }
        };
        let resumed = resume_handoff(
            &self.deps.store,
            &self.deps.router,
            config,
            &context.user_id,
            &args.handoff_id,
            &ResumeOptions {
                actor: &context.actor,
                tool: Some(SUBAGENT_RETRY_TOOL),
                task_id: Some(&task_id),
                route: route.as_ref(),
                max_resumes: Some(MAX_RESUMES),
                accept_unknown_effects: false,
                daily_limit: self.deps.child_daily_limit,
            },
        )?;
        Self::output(json!({ "handoff": resumed }))
    }
}

#[async_trait]
impl Tool for BotTool {
    fn spec(&self) -> ToolSpec {
        let (description, parameters) = match self.kind {
            Kind::BotList => (
                "List main and its delegates with their route, state and whether each is idle.",
                json!({"type": "object", "properties": {}, "additionalProperties": false}),
            ),
            Kind::BotRead => (
                "Read one bot's settings (main or one of its delegates): route, tools, skills, delegates. Never its instructions.",
                json!({
                    "type": "object",
                    "properties": {"bot": {"type": "string"}},
                    "required": ["bot"],
                    "additionalProperties": false
                }),
            ),
            Kind::ModelList => (
                "List the models a route may use and the owner's provider accounts (id, label and state only).",
                json!({"type": "object", "properties": {}, "additionalProperties": false}),
            ),
            Kind::HandoffStatus => (
                "Show the subagent handoffs of this conversation, or one by id: status, route, outcome and resume count.",
                json!({
                    "type": "object",
                    "properties": {"handoff_id": {"type": "string"}},
                    "additionalProperties": false
                }),
            ),
            Kind::SetRoute => (
                "Change a bot's model route, fallbacks, failover policy or thinking level (owner approval required). Never grants cross-provider consent.",
                json!({
                    "type": "object",
                    "properties": {
                        "bot": {"type": "string", "description": "main or one of its delegates; the bot must be idle"},
                        "route": {"description": "Primary route, or null to follow the default", "anyOf": [route_schema(), {"type": "null"}]},
                        "fallbacks": {"type": "array", "items": route_schema(), "maxItems": 2},
                        "failover": {
                            "type": "object",
                            "properties": {
                                "enabled": {"type": "boolean"},
                                "switch_account_first": {"type": "boolean"},
                                "max_switches": {"type": "integer"}
                            },
                            "additionalProperties": false
                        },
                        "thinking": {"type": ["string", "null"]}
                    },
                    "required": ["bot"],
                    "additionalProperties": false
                }),
            ),
            Kind::SetEnabled => (
                "Pause one of main's delegates (owner approval required). It cannot switch a bot on or touch main.",
                json!({
                    "type": "object",
                    "properties": {"bot": {"type": "string"}, "enabled": {"type": "boolean"}},
                    "required": ["bot", "enabled"],
                    "additionalProperties": false
                }),
            ),
            Kind::SetDelegates => (
                "Set main's list of delegate specialists (owner approval required). The list must name enabled specialists.",
                json!({
                    "type": "object",
                    "properties": {"delegates": {"type": "array", "items": {"type": "string"}, "maxItems": 32}},
                    "required": ["delegates"],
                    "additionalProperties": false
                }),
            ),
            Kind::SubagentRetry => (
                "Run a failed subagent again, at most twice per handoff, never after an unknown effect. route_override {chain_index} stays inside the bot's own chain; a full route needs owner approval.",
                json!({
                    "type": "object",
                    "properties": {
                        "handoff_id": {"type": "string"},
                        "route_override": {"anyOf": [
                            {"type": "object", "properties": {"chain_index": {"type": "integer", "minimum": 0, "maximum": 2}}, "required": ["chain_index"], "additionalProperties": false},
                            route_schema()
                        ]}
                    },
                    "required": ["handoff_id"],
                    "additionalProperties": false
                }),
            ),
        };
        ToolSpec {
            name: self.name().into(),
            description: description.into(),
            parameters,
            external_effect: false,
        }
    }

    fn requires_approval(&self, args: &Value) -> bool {
        match self.kind {
            Kind::SetRoute | Kind::SetEnabled | Kind::SetDelegates => true,
            // Only a route outside the bot's own chain needs the owner.
            Kind::SubagentRetry => args
                .get("route_override")
                .is_some_and(|route| route.get("provider").is_some()),
            _ => false,
        }
    }

    fn allow_standing_grant(&self, _args: &Value) -> bool {
        !self.is_setter() && self.kind != Kind::SubagentRetry
    }

    async fn auto_approve_allowed(&self, _args: &Value, _context: &ToolContext) -> bool {
        false
    }

    fn approval_details(&self, args: &Value) -> Result<ApprovalDetails> {
        let text = |key: &str| args.get(key).and_then(Value::as_str).unwrap_or("?");
        let route_text = |route: &Value| {
            format!(
                "{}/{}",
                route.get("provider").and_then(Value::as_str).unwrap_or("?"),
                route.get("model").and_then(Value::as_str).unwrap_or("?")
            )
        };
        let summary = match self.kind {
            Kind::SetRoute => {
                let mut parts = Vec::new();
                match args.get("route") {
                    Some(Value::Null) => parts.push("primary: default".to_owned()),
                    Some(route) => parts.push(format!("primary: {}", route_text(route))),
                    None => {}
                }
                if let Some(fallbacks) = args.get("fallbacks").and_then(Value::as_array) {
                    let list = fallbacks.iter().map(route_text).collect::<Vec<_>>();
                    parts.push(format!("fallbacks: [{}]", list.join(", ")));
                }
                if args.get("failover").is_some() {
                    parts.push("failover policy".into());
                }
                if args.get("thinking").is_some() {
                    parts.push("thinking level".into());
                }
                format!("Change the route of {}: {}", text("bot"), parts.join("; "))
            }
            Kind::SetEnabled => format!(
                "Set {} enabled = {}",
                text("bot"),
                args.get("enabled")
                    .and_then(Value::as_bool)
                    .unwrap_or(false)
            ),
            Kind::SetDelegates => {
                let list = args
                    .get("delegates")
                    .and_then(Value::as_array)
                    .map(|items| {
                        items
                            .iter()
                            .filter_map(Value::as_str)
                            .collect::<Vec<_>>()
                            .join(", ")
                    })
                    .unwrap_or_default();
                format!(
                    "Set main's delegates to [{list}]. Adding a delegate gives main that specialist's tools indirectly"
                )
            }
            Kind::SubagentRetry => format!(
                "Run subagent {} again on {}",
                text("handoff_id"),
                args.get("route_override")
                    .map(route_text)
                    .unwrap_or_else(|| "its own route".into())
            ),
            _ => "Read bot settings".into(),
        };
        Ok(ApprovalDetails {
            summary: crate::util::truncate_chars(&summary, 300),
            destination: Some(format!("fleet:{}", self.name())),
            artifact_hash: None,
        })
    }

    async fn execute(&self, args: Value, context: &ToolContext) -> Result<ToolOutput> {
        match self.kind {
            Kind::BotList => {
                parse_args::<NoArgs>(&args)?;
                self.list(context)
            }
            Kind::BotRead => self.read(&args, context),
            Kind::ModelList => {
                parse_args::<NoArgs>(&args)?;
                self.models(context)
            }
            Kind::HandoffStatus => self.handoffs(&args, context),
            Kind::SetRoute => self.set_route(&args, context).await,
            Kind::SetEnabled => self.set_enabled(&args, context).await,
            Kind::SetDelegates => self.set_delegates(&args, context).await,
            Kind::SubagentRetry => self.retry(&args, context).await,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Config, LoadedBot};
    use crate::delegate::DelegateTool;
    use crate::gateway::{Decision, Gateway, ToolOutcome};
    use crate::llm::LlmClient;
    use rusqlite::params;
    use std::collections::{BTreeMap, HashSet};

    fn bot(name: &str, role: BotRole, delegates: Vec<String>) -> LoadedBot {
        LoadedBot {
            config: BotConfig {
                title: None,
                description: None,
                name: name.into(),
                trigger: "command".into(),
                model: None,
                system_prompt: format!("prompts/{name}.txt"),
                skills: vec![],
                tools: vec!["rag_search".into()],
                require_approval: vec!["rag_search".into()],
                enabled: true,
                temperature: None,
                thinking: None,
                role,
                delegates,
                route: None,
                fallbacks: Vec::new(),
                failover: Default::default(),
                catalog_only: false,
            },
            prompt: name.into(),
        }
    }

    struct Rig {
        store: Store,
        fleet: Arc<FleetRegistry>,
        router: ModelRouter,
        gateway: Gateway,
        policy: Arc<Policy>,
        admin: String,
        member: String,
    }

    fn rig() -> Rig {
        let store = Store::open_in_memory(384).unwrap();
        let config = Arc::new(Config::default());
        let catalog = Arc::new(crate::skills::SkillCatalog::load(&config).unwrap());
        let bots = vec![
            bot(
                ORCHESTRATOR_BOT,
                BotRole::Orchestrator,
                vec!["analyst".into(), "maker".into()],
            ),
            bot("analyst", BotRole::Specialist, vec![]),
            bot("maker", BotRole::Specialist, vec![]),
        ];
        let fleet = Arc::new(FleetRegistry::new(config, store.clone(), bots, catalog).unwrap());
        let mut llm = Config::default().llm;
        llm.data_egress_approved = true;
        llm.model = "test".into();
        llm.base_url = "https://example.com/v1".into();
        llm.models = vec!["test-alternative".into(), "test-third".into()];
        let router = ModelRouter::new(
            &llm,
            BTreeMap::from([(
                "default".into(),
                LlmClient::new("https://example.com/v1", "test", 5, 50)
                    .unwrap()
                    .with_model("test"),
            )]),
        )
        .unwrap();
        let policy = Arc::new(Policy::from_text("TopSecret"));
        let mut registry = ToolRegistry::default();
        register_bot_tools(&mut registry, &store, &fleet, &router, &policy, 500).unwrap();
        registry
            .register(DelegateTool::new(
                store.clone(),
                fleet.clone(),
                router.clone(),
            ))
            .unwrap();
        let gateway = Gateway::new(store.clone(), policy.clone(), registry, 1, false);
        let admin = store.admin_user_id().unwrap();
        let member = store
            .create_user("microsoft:fleet-b", "fleet-b@example.example", "B")
            .unwrap()
            .id;
        Rig {
            store,
            fleet,
            router,
            gateway,
            policy,
            admin,
            member,
        }
    }

    fn route(model: &str) -> Value {
        json!({"provider": "default", "model": model})
    }

    /// A tool context for `bot` inside a turn whose task carries `payload`.
    fn context_with(rig: &Rig, user: &str, bot: &str, payload: Value) -> ToolContext {
        let task = rig
            .store
            .enqueue_task(user, "turn", bot, &payload, 0, chrono::Utc::now())
            .unwrap();
        ToolContext {
            user_id: user.into(),
            actor: user.into(),
            bot: bot.into(),
            task_id: Some(task.id),
            turn_id: None,
            conversation_id: Some("dir:main".into()),
            allowed_tools: MANAGEMENT_TOOLS.iter().map(|t| (*t).to_owned()).collect(),
            require_approval: HashSet::new(),
        }
    }

    fn owner_context(rig: &Rig, user: &str) -> ToolContext {
        context_with(
            rig,
            user,
            ORCHESTRATOR_BOT,
            json!({"conversation_id": "dir:main", "text": "synthetic", "bot": "main"}),
        )
    }

    async fn run(rig: &Rig, name: &str, args: Value, context: &ToolContext) -> Result<ToolOutput> {
        match rig.gateway.execute(name, args, context).await? {
            ToolOutcome::Completed(output) => Ok(output),
            other => panic!("expected completion, got {other:?}"),
        }
    }

    /// Run an approval-gated tool: it must wait, then run once the owner says yes.
    async fn approve_and_run(
        rig: &Rig,
        name: &str,
        args: Value,
        context: &ToolContext,
    ) -> Result<ToolOutput> {
        let ToolOutcome::PendingApproval {
            approval_id,
            action_id,
            ..
        } = rig.gateway.execute(name, args, context).await?
        else {
            panic!("{name} must wait for approval");
        };
        rig.gateway
            .decide(&context.user_id, &approval_id, Decision::Once)
            .unwrap();
        match rig.gateway.execute_approved(&action_id, context).await? {
            ToolOutcome::Completed(output) => Ok(output),
            other => panic!("expected completion, got {other:?}"),
        }
    }

    fn config_json(rig: &Rig, user: &str, bot: &str) -> Value {
        // `row` seeds the user's fleet on first use, like any request would.
        rig.fleet.row(user, bot).unwrap().unwrap().config
    }

    fn set_status(rig: &Rig, task: &str, status: &str) {
        rig.store
            .connection()
            .unwrap()
            .execute(
                "UPDATE tasks SET status = ?2 WHERE id = ?1",
                params![task, status],
            )
            .unwrap();
    }

    /// A ledger row for the analyst whose child task has `status`.
    fn add_handoff(rig: &Rig, user: &str, objective: &str, status: &str) -> (String, String) {
        let parent = rig
            .store
            .enqueue_task(user, "turn", "main", &json!({}), 0, chrono::Utc::now())
            .unwrap();
        let child = rig
            .store
            .enqueue_task(
                user,
                "turn",
                "analyst",
                &json!({"conversation_id": "dir:main", "text": objective, "bot": "analyst", "source": "handoff"}),
                0,
                chrono::Utc::now(),
            )
            .unwrap();
        let id = crate::domain::new_id();
        let hash = crate::delegate::handoff_key_hash("turn", "analyst", objective);
        let pinned = rig
            .router
            .pin(&crate::model_router::ModelSelection::new("default", "test"))
            .unwrap();
        // The child task already exists; only the ledger row is new here.
        rig.store
            .connection()
            .unwrap()
            .execute(
                "INSERT INTO handoffs(id, user_id, parent_task_id, parent_turn_id, child_task_id,
                     to_bot, key_hash, status, effects_unknown, resume_count, route_json,
                     created_at, updated_at)
                 VALUES (?1, ?2, ?3, 'turn', ?4, 'analyst', ?5, 'queued', 0, 0, ?6, 'now', 'now')",
                params![
                    id,
                    user,
                    parent.id,
                    child.id,
                    hash,
                    serde_json::to_string(&pinned).unwrap()
                ],
            )
            .unwrap();
        set_status(rig, &child.id, status);
        (id, child.id)
    }

    #[test]
    fn management_turn_sources() {
        assert!(turn_may_manage(&json!({"text": "hi"})));
        assert!(turn_may_manage(&json!({"source": "owner"})));
        for source in ["handoff", "team", "help", "routine", "schedule"] {
            assert!(!turn_may_manage(&json!({"source": source})), "{source}");
        }
        assert!(!turn_may_manage(&json!({"handoff": {"handoff_id": "h"}})));
        assert!(!turn_may_manage(&json!({"team": {"round": 0}})));
        assert!(is_management_tool("bot_set_route"));
        assert!(!is_management_tool("delegate"));
        for name in MANAGEMENT_TOOLS {
            assert!(crate::tools::known_tool_names().contains(&name), "{name}");
        }
    }

    #[tokio::test]
    async fn reads_cover_main_and_its_delegates_only() {
        let rig = rig();
        let context = owner_context(&rig, &rig.admin);
        let listed = run(&rig, BOT_LIST_TOOL, json!({}), &context).await.unwrap();
        let ids = listed.data.unwrap()["bots"]
            .as_array()
            .unwrap()
            .iter()
            .map(|bot| bot["id"].as_str().unwrap().to_owned())
            .collect::<Vec<_>>();
        assert_eq!(ids, vec!["main", "analyst", "maker"]);

        let read = run(&rig, BOT_READ_TOOL, json!({"bot": "analyst"}), &context)
            .await
            .unwrap();
        let data = read.data.unwrap();
        assert_eq!(data["id"], "analyst");
        assert!(data.get("prompt").is_none() && data.get("instructions").is_none());

        // A bot outside main's delegates is out of reach.
        rig.store
            .insert_bot(
                &rig.admin,
                "stranger",
                &serde_json::to_value(&bot("stranger", BotRole::Specialist, vec![]).config)
                    .unwrap(),
                "p",
                false,
            )
            .unwrap();
        rig.fleet.invalidate(&rig.admin);
        let error = run(&rig, BOT_READ_TOOL, json!({"bot": "stranger"}), &context)
            .await
            .unwrap_err();
        assert!(matches!(error, SeraError::Forbidden(_)), "{error}");
    }

    #[tokio::test]
    async fn model_list_shows_accounts_as_id_label_and_state_only() {
        let rig = rig();
        rig.store
            .connection()
            .unwrap()
            .execute(
                "INSERT INTO provider_accounts(user_id, provider, account_id, label, auth_kind,
                     fingerprint, identity_json, status, created_at, updated_at)
                 VALUES (?1, 'default', 'acct1', 'Work key', 'api_key', 'fp-secret-ish',
                     '{\"email\":\"hidden@example.example\"}', 'ok', 'now', 'now')",
                params![rig.admin],
            )
            .unwrap();
        let context = owner_context(&rig, &rig.admin);
        let output = run(&rig, MODEL_LIST_TOOL, json!({}), &context)
            .await
            .unwrap();
        assert!(!output.content.contains("fp-secret-ish"));
        assert!(!output.content.contains("hidden@example.example"));
        let data = output.data.unwrap();
        assert_eq!(
            data["accounts"],
            json!([{"provider": "default", "id": "acct1", "label": "Work key", "state": "ok"}])
        );
        assert!(
            data["models"]
                .as_array()
                .unwrap()
                .iter()
                .any(|entry| entry["model"] == "test-alternative")
        );
        // Another user's accounts are not listed.
        let other = owner_context(&rig, &rig.member);
        let theirs = run(&rig, MODEL_LIST_TOOL, json!({}), &other).await.unwrap();
        assert_eq!(theirs.data.unwrap()["accounts"], json!([]));
    }

    #[tokio::test]
    async fn one_users_main_cannot_read_or_change_another_users_bots() {
        let rig = rig();
        // The member's own custom bot.
        rig.store
            .insert_bot(
                &rig.member,
                "bonly",
                &serde_json::to_value(&bot("bonly", BotRole::Specialist, vec![]).config).unwrap(),
                "p",
                false,
            )
            .unwrap();
        rig.fleet.invalidate(&rig.member);
        let admin_context = owner_context(&rig, &rig.admin);
        let error = run(&rig, BOT_READ_TOOL, json!({"bot": "bonly"}), &admin_context)
            .await
            .unwrap_err();
        assert!(
            matches!(error, SeraError::Forbidden(_) | SeraError::NotFound(_)),
            "{error}"
        );

        // A route change by the admin's main moves only the admin's analyst.
        rig.fleet.row(&rig.member, "analyst").unwrap();
        let before = config_json(&rig, &rig.member, "analyst");
        approve_and_run(
            &rig,
            BOT_SET_ROUTE_TOOL,
            json!({"bot": "analyst", "route": route("test-alternative")}),
            &admin_context,
        )
        .await
        .unwrap();
        assert_eq!(config_json(&rig, &rig.member, "analyst"), before);
        assert_eq!(
            config_json(&rig, &rig.admin, "analyst")["route"]["model"],
            "test-alternative"
        );
        // The member's own handoffs are not the admin's.
        let (handoff, _) = add_handoff(&rig, &rig.member, "member objective", "failed");
        let error = run(
            &rig,
            HANDOFF_STATUS_TOOL,
            json!({"handoff_id": handoff}),
            &admin_context,
        )
        .await
        .unwrap_err();
        assert!(matches!(error, SeraError::NotFound(_)), "{error}");
    }

    #[tokio::test]
    async fn no_model_supplied_user_id_is_accepted() {
        let rig = rig();
        let context = owner_context(&rig, &rig.admin);
        for (name, args) in [
            (BOT_LIST_TOOL, json!({"user_id": rig.member})),
            (
                BOT_READ_TOOL,
                json!({"bot": "analyst", "user_id": rig.member}),
            ),
            (MODEL_LIST_TOOL, json!({"user": rig.member})),
            (HANDOFF_STATUS_TOOL, json!({"uid": rig.member})),
        ] {
            let error = run(&rig, name, args, &context).await.unwrap_err();
            assert!(matches!(error, SeraError::Validation(_)), "{name}: {error}");
        }
        // A setter refuses it too, so an approved call changes nothing.
        let error = approve_and_run(
            &rig,
            BOT_SET_ROUTE_TOOL,
            json!({"bot": "analyst", "route": route("test-alternative"), "user_id": rig.member}),
            &context,
        )
        .await;
        assert!(error.is_err());
        assert!(
            config_json(&rig, &rig.admin, "analyst")
                .get("route")
                .is_none()
        );
        assert!(
            config_json(&rig, &rig.member, "analyst")
                .get("route")
                .is_none()
        );
    }

    #[tokio::test]
    async fn widening_is_refused() {
        let rig = rig();
        let context = owner_context(&rig, &rig.admin);
        // Consent to cross-provider failover is the owner's to give.
        let error = approve_and_run(
            &rig,
            BOT_SET_ROUTE_TOOL,
            json!({
                "bot": "analyst",
                "route": route("test-alternative"),
                "failover": {"allow_cross_provider": true}
            }),
            &context,
        )
        .await
        .unwrap_err();
        assert!(
            matches!(
                error,
                SeraError::Rejected {
                    code: "widening_refused",
                    ..
                }
            ),
            "{error}"
        );
        assert!(
            config_json(&rig, &rig.admin, "analyst")
                .get("route")
                .is_none()
        );

        // The shared path refuses a bot-started change to tools, approvals or
        // the prompt, whatever tool asks.
        let registered = |_: &str| true;
        let env = MutationEnv {
            store: &rig.store,
            fleet: &rig.fleet,
            router: &rig.router,
            policy: &rig.policy,
            tool_registered: &registered,
        };
        let origin = Origin::tool(&rig.admin, BOT_SET_ROUTE_TOOL, None);
        for patch in [
            BotPatch {
                tools: Some(vec!["rag_search".into(), "fetch_url".into()]),
                ..BotPatch::default()
            },
            BotPatch {
                require_approval: Some(vec![]),
                ..BotPatch::default()
            },
            BotPatch {
                prompt: Some("a new prompt".into()),
                ..BotPatch::default()
            },
        ] {
            let error = fleet_mutation::apply(&env, &rig.admin, "analyst", patch, origin)
                .await
                .unwrap_err();
            assert!(
                matches!(
                    error,
                    SeraError::Rejected {
                        code: "widening_refused",
                        ..
                    }
                ),
                "{error}"
            );
        }
        // The same tool grant from the owner is allowed.
        fleet_mutation::apply(
            &env,
            &rig.admin,
            "analyst",
            BotPatch {
                tools: Some(vec!["rag_search".into(), "fetch_url".into()]),
                ..BotPatch::default()
            },
            Origin::owner(&rig.admin),
        )
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn main_is_never_paused_and_a_pause_leaves_the_fleet_valid() {
        let rig = rig();
        let context = owner_context(&rig, &rig.admin);
        let error = approve_and_run(
            &rig,
            BOT_SET_ENABLED_TOOL,
            json!({"bot": "main", "enabled": false}),
            &context,
        )
        .await
        .unwrap_err();
        assert!(matches!(error, SeraError::Forbidden(_)), "{error}");

        rig.fleet.row(&rig.member, "maker").unwrap();
        approve_and_run(
            &rig,
            BOT_SET_ENABLED_TOOL,
            json!({"bot": "maker", "enabled": false}),
            &context,
        )
        .await
        .unwrap();
        let maker = rig.store.get_bot(&rig.admin, "maker").unwrap().unwrap();
        assert!(!maker.enabled);
        let main = config_json(&rig, &rig.admin, "main");
        assert_eq!(main["delegates"], json!(["analyst"]));
        assert_eq!(main["role"], "orchestrator");
        rig.fleet.load(&rig.admin).unwrap();
        // The member's fleet is untouched.
        assert!(
            rig.store
                .get_bot(&rig.member, "maker")
                .unwrap()
                .unwrap()
                .enabled
        );
        // A bot cannot switch a bot on; that is the owner's.
        let error = approve_and_run(
            &rig,
            BOT_SET_ENABLED_TOOL,
            json!({"bot": "maker", "enabled": true}),
            &context,
        )
        .await
        .unwrap_err();
        assert!(matches!(error, SeraError::Forbidden(_)), "{error}");
    }

    #[tokio::test]
    async fn delegates_are_set_through_the_shared_path() {
        let rig = rig();
        let context = owner_context(&rig, &rig.admin);
        approve_and_run(
            &rig,
            BOT_SET_DELEGATES_TOOL,
            json!({"delegates": ["analyst"]}),
            &context,
        )
        .await
        .unwrap();
        assert_eq!(
            config_json(&rig, &rig.admin, "main")["delegates"],
            json!(["analyst"])
        );
        // A name that is not an enabled bot leaves the fleet invalid, so it is refused.
        let error = approve_and_run(
            &rig,
            BOT_SET_DELEGATES_TOOL,
            json!({"delegates": ["analyst", "ghost"]}),
            &context,
        )
        .await
        .unwrap_err();
        assert!(!error.to_string().is_empty());
        assert_eq!(
            config_json(&rig, &rig.admin, "main")["delegates"],
            json!(["analyst"])
        );
    }

    #[tokio::test]
    async fn a_route_change_needs_an_idle_target() {
        let rig = rig();
        let context = owner_context(&rig, &rig.admin);
        let busy = rig
            .store
            .enqueue_task(
                &rig.admin,
                "turn",
                "analyst",
                &json!({}),
                0,
                chrono::Utc::now(),
            )
            .unwrap();
        for status in ["running", "awaiting_approval"] {
            set_status(&rig, &busy.id, status);
            let error = approve_and_run(
                &rig,
                BOT_SET_ROUTE_TOOL,
                json!({"bot": "analyst", "route": route("test-alternative")}),
                &context,
            )
            .await
            .unwrap_err();
            assert!(
                matches!(
                    error,
                    SeraError::Rejected {
                        code: "bot_busy",
                        ..
                    }
                ),
                "{status}: {error}"
            );
        }
        set_status(&rig, &busy.id, "done");
        approve_and_run(
            &rig,
            BOT_SET_ROUTE_TOOL,
            json!({"bot": "analyst", "route": route("test-alternative")}),
            &context,
        )
        .await
        .unwrap();
        // Main may change its own profile while its own turn runs.
        approve_and_run(
            &rig,
            BOT_SET_ROUTE_TOOL,
            json!({"bot": "main", "thinking": null}),
            &context,
        )
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn handoff_team_and_routine_turns_and_other_bots_are_refused() {
        let rig = rig();
        for payload in [
            json!({"source": "handoff", "handoff": {"handoff_id": "h"}}),
            json!({"source": "team", "team": {"round": 0}}),
            json!({"source": "routine"}),
        ] {
            let context = context_with(&rig, &rig.admin, ORCHESTRATOR_BOT, payload);
            for (name, args) in [
                (BOT_LIST_TOOL, json!({})),
                (HANDOFF_STATUS_TOOL, json!({})),
                (
                    BOT_SET_ROUTE_TOOL,
                    json!({"bot": "analyst", "route": route("test-alternative")}),
                ),
            ] {
                // A read refuses at once; a setter waits for approval and is
                // then refused when it runs. Neither changes a row.
                match rig.gateway.execute(name, args, &context).await {
                    Err(error) => assert!(matches!(error, SeraError::Forbidden(_)), "{error}"),
                    Ok(ToolOutcome::PendingApproval {
                        approval_id,
                        action_id,
                        ..
                    }) => {
                        rig.gateway
                            .decide(&rig.admin, &approval_id, Decision::Once)
                            .unwrap();
                        let result = rig.gateway.execute_approved(&action_id, &context).await;
                        assert!(
                            !matches!(result, Ok(ToolOutcome::Completed(_))),
                            "{name} ran from a refused turn"
                        );
                    }
                    Ok(other) => panic!("{name} was not refused: {other:?}"),
                }
            }
        }
        assert!(
            config_json(&rig, &rig.admin, "analyst")
                .get("route")
                .is_none()
        );
        assert!(
            rig.store
                .list_audit(100)
                .unwrap()
                .iter()
                .any(|entry| entry.event == "bot_tools.refused")
        );
        // A specialist never holds the tools.
        let specialist = context_with(&rig, &rig.admin, "analyst", json!({}));
        let error = run(&rig, BOT_LIST_TOOL, json!({}), &specialist)
            .await
            .unwrap_err();
        assert!(matches!(error, SeraError::Forbidden(_)), "{error}");
        // And a specialist's tool list cannot be granted them.
        let error = fleet_mutation::validate_tool_list(
            &[BOT_SET_ROUTE_TOOL.to_owned()],
            BotRole::Specialist,
            &[],
            &|_| true,
        )
        .unwrap_err();
        assert!(matches!(error, SeraError::Validation(_)), "{error}");
    }

    async fn pending_hash(rig: &Rig, context: &ToolContext, args: Value) -> (String, String) {
        let ToolOutcome::PendingApproval {
            approval_id,
            action_id,
            ..
        } = rig
            .gateway
            .execute(BOT_SET_ROUTE_TOOL, args, context)
            .await
            .unwrap()
        else {
            panic!("must wait for approval");
        };
        let action = rig.store.get_action(&rig.admin, &action_id).unwrap();
        (approval_id, action.canonical_hash)
    }

    #[tokio::test]
    async fn the_approval_binds_the_canonical_arguments() {
        let rig = rig();
        let context = owner_context(&rig, &rig.admin);
        let (approval, hash) = pending_hash(
            &rig,
            &context,
            json!({"bot": "analyst", "route": {"provider": "default", "model": "test-alternative"}}),
        )
        .await;
        // Key order does not matter: the hash is over the canonical form.
        let (_, reordered) = pending_hash(
            &rig,
            &context,
            json!({"route": {"model": "test-alternative", "provider": "default"}, "bot": "analyst"}),
        )
        .await;
        assert_eq!(hash, reordered);
        // Another model is another action with another hash.
        let (_, other) = pending_hash(
            &rig,
            &context,
            json!({"bot": "analyst", "route": {"provider": "default", "model": "test-third"}}),
        )
        .await;
        assert_ne!(hash, other);
        // Nothing changed before the owner decided, and only the approval's own
        // user can decide it.
        assert!(
            config_json(&rig, &rig.admin, "analyst")
                .get("route")
                .is_none()
        );
        assert!(
            rig.gateway
                .decide(&rig.member, &approval, Decision::Once)
                .is_err()
        );
        // No standing grant and never YOLO: even after "always" and with YOLO
        // on, the same call asks again.
        rig.store.set_yolo(&rig.admin, "dir:main", true).unwrap();
        rig.gateway
            .decide(&rig.admin, &approval, Decision::Always)
            .unwrap();
        let again = rig
            .gateway
            .execute(
                BOT_SET_ROUTE_TOOL,
                json!({"bot": "analyst", "route": route("test-alternative")}),
                &context,
            )
            .await
            .unwrap();
        assert!(matches!(again, ToolOutcome::PendingApproval { .. }));
    }

    #[tokio::test]
    async fn the_owner_path_and_the_tool_path_write_identical_rows_and_audit() {
        let rig = rig();
        let registered = |_: &str| false;
        let env = MutationEnv {
            store: &rig.store,
            fleet: &rig.fleet,
            router: &rig.router,
            policy: &rig.policy,
            tool_registered: &registered,
        };
        let entry = |model: &str| BotRoute {
            provider: "default".into(),
            model: model.into(),
            thinking: None,
            account: None,
        };
        let patch = || BotPatch {
            route: Some(Some(entry("test-alternative"))),
            fallbacks: Some(vec![entry("test-third")]),
            ..BotPatch::default()
        };
        // The owner's REST handler runs `apply` with an owner origin.
        fleet_mutation::apply(
            &env,
            &rig.admin,
            "analyst",
            patch(),
            Origin::owner(&rig.admin),
        )
        .await
        .unwrap();
        // The tool runs the same call for the member, as a bot.
        let task = rig
            .store
            .enqueue_task(
                &rig.member,
                "turn",
                "main",
                &json!({}),
                0,
                chrono::Utc::now(),
            )
            .unwrap();
        fleet_mutation::apply(
            &env,
            &rig.member,
            "analyst",
            patch(),
            Origin::tool(&rig.member, BOT_SET_ROUTE_TOOL, Some(&task.id)),
        )
        .await
        .unwrap();
        assert_eq!(
            config_json(&rig, &rig.admin, "analyst"),
            config_json(&rig, &rig.member, "analyst")
        );
        let audit = |user: &str| {
            rig.store
                .list_audit_for_user(user, 20)
                .unwrap()
                .into_iter()
                .find(|entry| entry.event == "bot.profile_changed")
                .expect("bot.profile_changed audit row")
        };
        let (owner, tool) = (audit(&rig.admin), audit(&rig.member));
        assert_eq!(owner.details, tool.details);
        assert_eq!(tool.tool.as_deref(), Some(BOT_SET_ROUTE_TOOL));
        assert_eq!(tool.task_id.as_deref(), Some(task.id.as_str()));
        assert_eq!(owner.tool, None);
        let details = owner.details.unwrap();
        assert_eq!(details["before"]["route"], Value::Null);
        assert_eq!(details["after"]["route"]["model"], "test-alternative");
        assert_eq!(details["after"]["fallbacks"][0]["model"], "test-third");
    }

    #[tokio::test]
    async fn policy_blocked_text_is_scanned_and_nothing_is_written() {
        let rig = rig();
        let context = owner_context(&rig, &rig.admin);
        let result = approve_and_run(
            &rig,
            BOT_SET_ROUTE_TOOL,
            json!({"bot": "analyst", "route": {"provider": "default", "model": "test-alternative", "account": "TopSecret"}}),
            &context,
        )
        .await;
        assert!(result.is_err());
        assert!(
            config_json(&rig, &rig.admin, "analyst")
                .get("route")
                .is_none()
        );
    }

    #[tokio::test]
    async fn subagent_retry_stays_in_its_chain_unless_approved() {
        let rig = rig();
        let context = owner_context(&rig, &rig.admin);
        // Give the analyst a chain: primary plus one fallback.
        approve_and_run(
            &rig,
            BOT_SET_ROUTE_TOOL,
            json!({
                "bot": "analyst",
                "route": route("test-alternative"),
                "fallbacks": [route("test-third")]
            }),
            &context,
        )
        .await
        .unwrap();
        let (handoff, child) = add_handoff(&rig, &rig.admin, "first objective", "failed");
        // A slot of the chain needs no approval.
        let output = run(
            &rig,
            SUBAGENT_RETRY_TOOL,
            json!({"handoff_id": handoff, "route_override": {"chain_index": 1}}),
            &context,
        )
        .await
        .unwrap();
        let data = output.data.unwrap();
        let view = &data["handoff"];
        assert_eq!(view["resume_count"], 1);
        assert_eq!(view["status"], "resumed");
        let new_child = view["child_task_id"].as_str().unwrap().to_owned();
        assert_ne!(new_child, child);
        let task = rig.store.get_task(&rig.admin, &new_child).unwrap();
        assert_eq!(task.payload["route"]["selection"]["model"], "test-third");
        // A slot outside the chain is refused.
        set_status(&rig, &new_child, "failed");
        let error = run(
            &rig,
            SUBAGENT_RETRY_TOOL,
            json!({"handoff_id": handoff, "route_override": {"chain_index": 2}}),
            &context,
        )
        .await
        .unwrap_err();
        assert!(matches!(error, SeraError::Validation(_)), "{error}");
        // A full route names a provider, so it waits for the owner.
        let output = approve_and_run(
            &rig,
            SUBAGENT_RETRY_TOOL,
            json!({"handoff_id": handoff, "route_override": route("test")}),
            &context,
        )
        .await
        .unwrap();
        assert_eq!(output.data.unwrap()["handoff"]["resume_count"], 2);
        // The cap: a third resume is refused.
        let latest = rig.store.handoff_view(&rig.admin, &handoff).unwrap();
        set_status(&rig, &latest.child_task_id, "failed");
        let error = run(
            &rig,
            SUBAGENT_RETRY_TOOL,
            json!({"handoff_id": handoff}),
            &context,
        )
        .await
        .unwrap_err();
        assert!(error.to_string().contains("resumed"), "{error}");
        // The resumes are audited with the tool that asked.
        let resumed = rig
            .store
            .list_audit_for_user(&rig.admin, 100)
            .unwrap()
            .into_iter()
            .filter(|entry| entry.event == "handoff.resumed")
            .count();
        assert_eq!(resumed, 2);
    }

    #[tokio::test]
    async fn subagent_retry_scans_a_route_the_bot_supplies() {
        let rig = rig();
        let context = owner_context(&rig, &rig.admin);
        let (handoff, _) = add_handoff(&rig, &rig.admin, "scan objective", "failed");
        let result = approve_and_run(
            &rig,
            SUBAGENT_RETRY_TOOL,
            json!({"handoff_id": handoff, "route_override": {"provider": "default", "model": "test", "account": "TopSecret"}}),
            &context,
        )
        .await;
        assert!(result.is_err());
        let view = rig.store.handoff_view(&rig.admin, &handoff).unwrap();
        assert_eq!(view.resume_count, 0);
    }

    #[tokio::test]
    async fn subagent_retry_refuses_unknown_effects_and_other_users_handoffs() {
        let rig = rig();
        let context = owner_context(&rig, &rig.admin);
        let (handoff, child) = add_handoff(&rig, &rig.admin, "risky objective", "failed");
        rig.store
            .connection()
            .unwrap()
            .execute(
                "UPDATE tasks SET uncertain = 1 WHERE id = ?1",
                params![child],
            )
            .unwrap();
        let error = run(
            &rig,
            SUBAGENT_RETRY_TOOL,
            json!({"handoff_id": handoff}),
            &context,
        )
        .await
        .unwrap_err();
        assert!(
            matches!(
                error,
                SeraError::Rejected {
                    code: "effects_unknown",
                    ..
                }
            ),
            "{error}"
        );
        let (theirs, _) = add_handoff(&rig, &rig.member, "member objective", "failed");
        let error = run(
            &rig,
            SUBAGENT_RETRY_TOOL,
            json!({"handoff_id": theirs}),
            &context,
        )
        .await
        .unwrap_err();
        assert!(matches!(error, SeraError::NotFound(_)), "{error}");
    }

    #[tokio::test]
    async fn handoff_status_lists_the_conversations_subagents() {
        let rig = rig();
        let context = owner_context(&rig, &rig.admin);
        let (id, _) = add_handoff(&rig, &rig.admin, "status objective", "running");
        let output = run(&rig, HANDOFF_STATUS_TOOL, json!({}), &context)
            .await
            .unwrap();
        let listed = output.data.unwrap();
        assert_eq!(listed["handoffs"][0]["id"], json!(id));
        assert_eq!(listed["handoffs"][0]["status"], "running");
        assert_eq!(listed["handoffs"][0]["to_bot"], "analyst");
        let one = run(
            &rig,
            HANDOFF_STATUS_TOOL,
            json!({"handoff_id": id}),
            &context,
        )
        .await
        .unwrap();
        assert_eq!(one.data.unwrap()["handoff"]["resume_count"], 0);
    }
}
