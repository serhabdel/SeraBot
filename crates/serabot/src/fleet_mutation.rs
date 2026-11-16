//! The one path that changes a user's bot row (v0.50).
//!
//! The owner's `PUT /v1/bots/{id}` and `PUT /v1/bots/{id}/route`, and the
//! main bot's management tools (`tools_bots.rs`), all go through [`apply`], so
//! both run the same validation, the same policy scan, the same catalog and
//! egress checks, write the same row, and leave the same `bot.profile_changed`
//! audit record with the before and after values.
//!
//! A change started by a bot (`Origin::tool` is set) is checked once more by
//! [`guard_no_widening`]: it can never grow `tools`, `skills`, the prompt or
//! cross-provider egress consent, never drop a `require_approval` entry, and
//! never change a role. Every row read or written is the acting user's.

use std::collections::HashSet;
use std::time::Duration;

use serde::Deserialize;
use serde_json::{Value, json};

use crate::browser_profiles::{
    BotSettings, DEFAULT_PROFILE, MAX_BOT_DESCRIPTION_CHARS, MAX_BOT_DISPLAY_NAME_CHARS,
};
use crate::config::{BotConfig, BotRoute, FailoverPolicy};
use crate::error::{Result, SeraError};
use crate::fleet::FleetRegistry;
use crate::model_router::{ModelRouter, ModelSelection};
use crate::orchestration::{BotRole, safe_bot_label};
use crate::policy::Policy;
use crate::store::{Store, StoredBot};

/// Every console ability on at once is ~95 tools plus `delegate`.
pub const MAX_BOT_CONFIG_TOOLS: usize = 128;
pub const MAX_BOT_CONFIG_SKILLS: usize = 16;
/// Spec cap for a bot system prompt.
pub const MAX_BOT_PROMPT_BYTES: usize = 100_000;
/// Most delegates the orchestrator may list.
pub const MAX_DELEGATES: usize = 32;
/// How long a live model-catalog check may wait before a save.
const CATALOG_TIMEOUT: Duration = Duration::from_secs(8);

/// An explicit `{provider, model}` choice.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelChoice {
    pub provider: String,
    pub model: String,
}

/// The fields a change may set. `None` leaves a field alone; the nested
/// options mean "set, or clear back to the default" where the API does.
#[derive(Debug, Clone, Default)]
pub struct BotPatch {
    pub enabled: Option<bool>,
    pub prompt: Option<String>,
    pub tools: Option<Vec<String>>,
    pub skills: Option<Vec<String>>,
    pub require_approval: Option<Vec<String>>,
    pub display_name: Option<String>,
    pub description: Option<String>,
    pub model: Option<Option<ModelChoice>>,
    pub thinking: Option<Option<String>>,
    pub browser_profile: Option<Option<String>>,
    pub route: Option<Option<BotRoute>>,
    pub fallbacks: Option<Vec<BotRoute>>,
    pub failover: Option<FailoverPolicy>,
    /// The orchestrator's delegate list. Refused for any other bot.
    pub delegates: Option<Vec<String>>,
}

impl BotPatch {
    fn is_empty(&self) -> bool {
        self.enabled.is_none()
            && self.prompt.is_none()
            && self.tools.is_none()
            && self.skills.is_none()
            && self.require_approval.is_none()
            && self.display_name.is_none()
            && self.description.is_none()
            && self.model.is_none()
            && self.thinking.is_none()
            && self.browser_profile.is_none()
            && self.route.is_none()
            && self.fallbacks.is_none()
            && self.failover.is_none()
            && self.delegates.is_none()
    }

    fn touches_route(&self) -> bool {
        self.route.is_some() || self.fallbacks.is_some() || self.failover.is_some()
    }
}

/// Who asked for the change. The audit row records it in its `actor`, `tool`
/// and `task_id` columns; the details stay identical for both paths.
#[derive(Debug, Clone, Copy)]
pub struct Origin<'a> {
    pub actor: &'a str,
    /// The tool that started the change; `None` for the owner's own request.
    pub tool: Option<&'a str>,
    pub task_id: Option<&'a str>,
}

impl<'a> Origin<'a> {
    pub fn owner(actor: &'a str) -> Self {
        Self {
            actor,
            tool: None,
            task_id: None,
        }
    }

    pub fn tool(actor: &'a str, tool: &'a str, task_id: Option<&'a str>) -> Self {
        Self {
            actor,
            tool: Some(tool),
            task_id,
        }
    }

    fn model_initiated(&self) -> bool {
        self.tool.is_some()
    }
}

/// What the change needs from the running server.
pub struct MutationEnv<'a> {
    pub store: &'a Store,
    pub fleet: &'a FleetRegistry,
    pub router: &'a ModelRouter,
    pub policy: &'a Policy,
    /// Whether a tool is registered in the gateway.
    pub tool_registered: &'a (dyn Fn(&str) -> bool + Send + Sync),
}

impl MutationEnv<'_> {
    /// Scan owner or model text against the policy, attributing a block to the
    /// acting user in the audit ledger (the same rule as
    /// `Gateway::scan_egress`).
    fn scan(&self, user_id: &str, text: &str) -> Result<()> {
        if !self.policy.is_allowed(text) {
            self.store.record_audit(
                user_id,
                "policy.blocked_egress",
                None,
                None,
                None,
                None,
                Some(self.policy.version()),
                None,
            )?;
            return Err(SeraError::PolicyViolation);
        }
        Ok(())
    }
}

/// The row after a successful change.
#[derive(Debug, Clone)]
pub struct Mutation {
    pub config: BotConfig,
    pub prompt: String,
    pub enabled: bool,
    pub settings: BotSettings,
    pub role: String,
}

/// Parse a stored bot row's config; the row's own columns are authoritative
/// for the name and the enabled flag.
pub fn parse_bot_config(row: &StoredBot) -> Result<BotConfig> {
    let mut config: BotConfig = serde_json::from_value(row.config.clone())
        .map_err(|_| SeraError::InvalidState(format!("bot '{}' config is invalid", row.name)))?;
    config.name = row.name.clone();
    config.enabled = row.enabled;
    Ok(config)
}

/// Tool list rules matching the desktop `validate_tool_list` semantics on top
/// of the server's tool registry: unique safe ids, `delegate` only for the
/// orchestrator, and every other tool must be a registered tool -- or one the
/// bot already has (`kept`). A tool whose service is switched off (for example
/// `tavily_search` while Tavily is disabled) is unregistered, and without the
/// `kept` exception every later edit of that bot's tools was refused (0.14.0).
pub fn validate_tool_list(
    tools: &[String],
    role: BotRole,
    kept: &[String],
    is_registered: &dyn Fn(&str) -> bool,
) -> Result<Vec<String>> {
    if tools.len() > MAX_BOT_CONFIG_TOOLS {
        return Err(SeraError::Validation(format!(
            "at most {MAX_BOT_CONFIG_TOOLS} tools are allowed"
        )));
    }
    let mut seen = HashSet::with_capacity(tools.len());
    let mut out = Vec::with_capacity(tools.len());
    for tool in tools {
        if !safe_bot_label(tool) {
            return Err(SeraError::Validation(format!("invalid tool id '{tool}'")));
        }
        if tool == crate::delegate::DELEGATE_TOOL {
            if role != BotRole::Orchestrator {
                return Err(SeraError::Validation(
                    "specialists cannot be granted delegate".into(),
                ));
            }
        } else if crate::tools_bots::is_management_tool(tool) {
            if role != BotRole::Orchestrator {
                return Err(SeraError::Validation(format!(
                    "specialists cannot be granted {tool}"
                )));
            }
        } else if !is_registered(tool)
            && !kept.iter().any(|existing| existing == tool)
            && !crate::tools::known_tool_names().contains(&tool.as_str())
        {
            return Err(SeraError::Validation(format!("unknown tool '{tool}'")));
        }
        if !seen.insert(tool.clone()) {
            return Err(SeraError::Validation(format!("duplicate tool '{tool}'")));
        }
        out.push(tool.clone());
    }
    Ok(out)
}

/// Skill list rules matching the desktop `parse_skill_list` semantics plus the
/// acting user's effective catalog check: at most 16 unique, valid skill ids
/// that exist in the shipped catalog or the user's own rows.
pub fn validate_skill_list(
    fleet: &FleetRegistry,
    user_id: &str,
    skills: &[String],
) -> Result<Vec<String>> {
    if skills.len() > MAX_BOT_CONFIG_SKILLS {
        return Err(SeraError::Validation(format!(
            "at most {MAX_BOT_CONFIG_SKILLS} skills are allowed"
        )));
    }
    let known = fleet.effective_skill_ids(user_id)?;
    let mut seen = HashSet::with_capacity(skills.len());
    let mut out = Vec::with_capacity(skills.len());
    for skill in skills {
        crate::skills::validate_skill_id(skill)?;
        if !known.iter().any(|id| id == skill) {
            return Err(SeraError::Validation(format!("unknown skill '{skill}'")));
        }
        if !seen.insert(skill.clone()) {
            return Err(SeraError::Validation(format!("duplicate skill '{skill}'")));
        }
        out.push(skill.clone());
    }
    Ok(out)
}

/// `require_approval` entries must be safe ids contained in the bot's
/// effective tool list (the new tools when the request sets them).
pub fn validate_require_approval_list(list: &[String], tools: &[String]) -> Result<Vec<String>> {
    if list.len() > MAX_BOT_CONFIG_TOOLS {
        return Err(SeraError::Validation(format!(
            "at most {MAX_BOT_CONFIG_TOOLS} require_approval entries are allowed"
        )));
    }
    let mut seen = HashSet::with_capacity(list.len());
    let mut out = Vec::with_capacity(list.len());
    for item in list {
        if !safe_bot_label(item) {
            return Err(SeraError::Validation(format!(
                "invalid require_approval entry '{item}'"
            )));
        }
        if !tools.iter().any(|tool| tool == item) {
            return Err(SeraError::Validation(format!(
                "require_approval entry '{item}' is not in the bot's tools"
            )));
        }
        if !seen.insert(item.clone()) {
            return Err(SeraError::Validation(format!(
                "duplicate require_approval entry '{item}'"
            )));
        }
        out.push(item.clone());
    }
    Ok(out)
}

/// Refuse a bot-initiated change that would give a bot more power: new tools
/// or skills, a changed prompt or role, a dropped approval entry, or consent
/// to send a turn to another provider. Narrowing is allowed.
pub fn guard_no_widening(
    before: &BotConfig,
    before_prompt: &str,
    after: &BotConfig,
    after_prompt: &str,
) -> Result<()> {
    let widened = |what: &str| {
        Err(SeraError::Rejected {
            code: "widening_refused",
            message: format!("a bot cannot widen {what}; ask the owner to change it"),
        })
    };
    if before.role != after.role {
        return widened("a bot's role");
    }
    if !after.tools.iter().all(|tool| before.tools.contains(tool)) {
        return widened("a bot's tools");
    }
    if !after
        .skills
        .iter()
        .all(|skill| before.skills.contains(skill))
    {
        return widened("a bot's skills");
    }
    if !before
        .require_approval
        .iter()
        .all(|tool| after.require_approval.contains(tool))
    {
        return widened("what runs without approval");
    }
    if after.failover.allow_cross_provider && !before.failover.allow_cross_provider {
        return widened("cross-provider consent");
    }
    if before_prompt != after_prompt {
        return widened("a bot's instructions");
    }
    Ok(())
}

fn snapshot(config: &BotConfig, settings: &BotSettings, enabled: bool) -> Value {
    json!({
        "enabled": enabled,
        "model": config.model,
        "thinking": config.thinking,
        "route": config.route,
        "fallbacks": config.fallbacks,
        "failover": config.failover,
        "delegates": config.delegates,
        "tools": config.tools,
        "skills": config.skills,
        "require_approval": config.require_approval,
        "display_name": settings.display_name,
        "description": settings.description,
        "browser_profile": settings.browser_profile,
    })
}

/// Apply one change to the acting user's bot. Everything is validated and
/// scanned before the first write.
pub async fn apply(
    env: &MutationEnv<'_>,
    user_id: &str,
    bot_id: &str,
    patch: BotPatch,
    origin: Origin<'_>,
) -> Result<Mutation> {
    if !safe_bot_label(bot_id) {
        return Err(SeraError::NotFound(format!("bot {bot_id}")));
    }
    if patch.is_empty() {
        return Err(SeraError::Validation(
            "at least one of enabled, prompt, tools, skills, require_approval, display_name, description, model, thinking, browser_profile, route, fallbacks, failover, delegates is required".into(),
        ));
    }
    if patch.model.as_ref().is_some_and(Option::is_some) || patch.touches_route() {
        // A live-only model must be checked against the live list.
        env.router.ensure_catalogs(user_id, CATALOG_TIMEOUT).await;
    }
    // Untrusted input is scanned before it is validated or stored.
    for list in [
        &patch.tools,
        &patch.skills,
        &patch.require_approval,
        &patch.delegates,
    ]
    .into_iter()
    .flatten()
    {
        for item in list {
            env.scan(user_id, item)?;
        }
    }
    let row = env
        .fleet
        .row(user_id, bot_id)?
        .ok_or_else(|| SeraError::NotFound(format!("bot {bot_id}")))?;
    let before_config = parse_bot_config(&row)?;
    let before_settings = env.store.bot_settings(user_id, bot_id)?;
    let mut config = before_config.clone();

    // Owner-editable profile fields (0.16): validated and scanned before any write.
    let mut settings = before_settings.clone();
    let settings_changed = patch.display_name.is_some()
        || patch.description.is_some()
        || patch.browser_profile.is_some();
    if let Some(name) = &patch.display_name {
        let name = name.trim();
        let count = name.chars().count();
        if count == 0 || count > MAX_BOT_DISPLAY_NAME_CHARS || name.chars().any(char::is_control) {
            return Err(SeraError::Validation(format!(
                "display_name must be 1 to {MAX_BOT_DISPLAY_NAME_CHARS} characters"
            )));
        }
        env.scan(user_id, name)?;
        settings.display_name = Some(name.to_owned());
    }
    if let Some(description) = &patch.description {
        let description = description.trim();
        if description.chars().count() > MAX_BOT_DESCRIPTION_CHARS
            || description
                .chars()
                .any(|c| c.is_control() && !matches!(c, '\n' | '\t'))
        {
            return Err(SeraError::Validation(format!(
                "description must be at most {MAX_BOT_DESCRIPTION_CHARS} characters"
            )));
        }
        env.scan(user_id, description)?;
        settings.description = (!description.is_empty()).then(|| description.to_owned());
    }
    if let Some(choice) = &patch.browser_profile {
        settings.browser_profile = match choice.as_deref() {
            None | Some(DEFAULT_PROFILE) => None,
            Some(profile) => {
                if !crate::browser_profiles::valid_profile_id(profile)
                    || env.store.get_browser_profile(user_id, profile)?.is_none()
                {
                    return Err(SeraError::Validation("unknown browser profile".into()));
                }
                Some(profile.to_owned())
            }
        };
    }
    if let Some(choice) = &patch.model {
        config.model = match choice {
            None => None,
            Some(choice) => {
                env.scan(user_id, &choice.provider)?;
                env.scan(user_id, &choice.model)?;
                env.router.validate_selection(
                    user_id,
                    &ModelSelection {
                        provider: choice.provider.clone(),
                        model: choice.model.clone(),
                        account: None,
                    },
                )?;
                Some(choice.model.clone())
            }
        };
        if config.route.is_some() {
            // The route head is what the bot runs; a model edit moves it.
            match choice {
                Some(choice) => {
                    if let Some(route) = config.route.as_mut() {
                        route.provider = choice.provider.clone();
                        route.model = choice.model.clone();
                        route.account = None;
                        route.thinking = None;
                    }
                }
                None => {
                    config.route = None;
                    config.fallbacks.clear();
                }
            }
        }
    }

    if let Some(choice) = &patch.thinking {
        config.thinking = match choice.as_deref() {
            None | Some("") => None,
            Some(level) if crate::thinking::is_known_level(level) => Some(level.to_owned()),
            Some(_) => {
                return Err(SeraError::Rejected {
                    code: "thinking_unavailable",
                    message: "That thinking level isn't available".into(),
                });
            }
        };
    }

    if patch.touches_route() {
        if let Some(route) = patch.route.clone() {
            config.route = route;
        }
        if let Some(fallbacks) = patch.fallbacks.clone() {
            config.fallbacks = fallbacks;
        }
        if let Some(failover) = patch.failover {
            config.failover = failover;
        }
        // Shape first (counts, duplicates), then the scan, then the live checks.
        crate::config::validate_route(config.route.as_ref(), &config.fallbacks, &config.failover)?;
        for entry in config.route.iter().chain(&config.fallbacks) {
            for text in [
                Some(&entry.provider),
                Some(&entry.model),
                entry.account.as_ref(),
                entry.thinking.as_ref(),
            ]
            .into_iter()
            .flatten()
            {
                env.scan(user_id, text)?;
            }
            env.router.validate_route_entry(user_id, entry)?;
        }
        // The legacy fields stay in step with the head, for older readers.
        if let Some(head) = &config.route {
            config.model = Some(head.model.clone());
            config.thinking = head.thinking.clone();
        }
    }

    let prompt_changed = patch.prompt.is_some();
    let prompt = match patch.prompt {
        Some(prompt) => {
            if prompt.trim().is_empty() {
                return Err(SeraError::Validation("bot prompt must not be empty".into()));
            }
            if prompt.len() > MAX_BOT_PROMPT_BYTES {
                return Err(SeraError::Validation(format!(
                    "bot prompt exceeds {MAX_BOT_PROMPT_BYTES} bytes"
                )));
            }
            if prompt
                .chars()
                .any(|c| c.is_control() && !matches!(c, '\n' | '\r' | '\t'))
            {
                return Err(SeraError::Validation(
                    "bot prompt contains unsupported control characters".into(),
                ));
            }
            env.scan(user_id, &prompt)?;
            prompt
        }
        None => row.prompt.clone(),
    };
    let tools = match &patch.tools {
        Some(tools) => validate_tool_list(tools, config.role, &config.tools, env.tool_registered)?,
        None => config.tools.clone(),
    };
    let skills = match &patch.skills {
        Some(skills) => validate_skill_list(env.fleet, user_id, skills)?,
        None => config.skills.clone(),
    };
    let require_approval = match &patch.require_approval {
        Some(list) => validate_require_approval_list(list, &tools)?,
        None => {
            // The tools were replaced without a new approval list: drop the
            // entries the new list no longer covers instead of refusing the
            // whole save (0.26.0 — capability toggles used to break Save).
            let mut kept_list = config.require_approval.clone();
            if patch.tools.is_some() {
                kept_list.retain(|item| tools.iter().any(|tool| tool == item));
            }
            validate_require_approval_list(&kept_list, &tools)?
        }
    };
    if let Some(delegates) = &patch.delegates {
        if config.role != BotRole::Orchestrator {
            return Err(SeraError::Validation(
                "only the orchestrator has delegates".into(),
            ));
        }
        if delegates.len() > MAX_DELEGATES {
            return Err(SeraError::Validation(format!(
                "at most {MAX_DELEGATES} delegates are allowed"
            )));
        }
        let mut seen = HashSet::with_capacity(delegates.len());
        for delegate in delegates {
            if !safe_bot_label(delegate) || !seen.insert(delegate.as_str()) {
                return Err(SeraError::Validation(format!(
                    "invalid or duplicate delegate '{delegate}'"
                )));
            }
        }
        config.delegates = delegates.clone();
    }
    let enabled = patch.enabled.unwrap_or(row.enabled);
    if !enabled && config.role == BotRole::Orchestrator {
        return Err(SeraError::Validation(
            "the orchestrator cannot be disabled".into(),
        ));
    }

    // Validate the fleet the write would produce, not just the row.
    let mut candidate = env.fleet.candidates(user_id)?;
    let Some(bot) = candidate.iter_mut().find(|bot| bot.config.name == bot_id) else {
        return Err(SeraError::NotFound(format!("bot {bot_id}")));
    };
    bot.config.enabled = enabled;
    bot.config.tools = tools.clone();
    bot.config.skills = skills.clone();
    bot.config.require_approval = require_approval.clone();
    bot.config.model = config.model.clone();
    bot.config.delegates = config.delegates.clone();
    bot.prompt = prompt.clone();
    FleetRegistry::validate_fleet(&candidate)?;

    config.enabled = enabled;
    config.tools = tools;
    config.skills = skills;
    config.require_approval = require_approval;
    if origin.model_initiated() {
        guard_no_widening(&before_config, &row.prompt, &config, &prompt)?;
    }

    let value = serde_json::to_value(&config)?;
    if !env
        .store
        .update_bot(user_id, bot_id, &value, &prompt, enabled)?
    {
        return Err(SeraError::NotFound(format!("bot {bot_id}")));
    }
    if settings_changed {
        env.store.put_bot_settings(user_id, bot_id, &settings)?;
    }
    env.fleet.invalidate(user_id);
    env.store.record_audit(
        origin.actor,
        "bot.profile_changed",
        origin.tool,
        origin.task_id,
        None,
        None,
        Some(env.policy.version()),
        Some(&json!({
            "id": bot_id,
            "role": config.role.as_str(),
            "prompt_changed": prompt_changed,
            "before": snapshot(&before_config, &before_settings, row.enabled),
            "after": snapshot(&config, &settings, enabled),
        })),
    )?;
    Ok(Mutation {
        role: config.role.as_str().to_owned(),
        config,
        prompt,
        enabled,
        settings,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base() -> BotConfig {
        BotConfig {
            title: None,
            description: None,
            name: "analyst".into(),
            trigger: "command".into(),
            model: None,
            system_prompt: "prompts/analyst.txt".into(),
            skills: vec!["a".into()],
            tools: vec!["rag_search".into()],
            require_approval: vec!["rag_search".into()],
            enabled: true,
            temperature: None,
            thinking: None,
            role: BotRole::Specialist,
            delegates: vec![],
            route: None,
            fallbacks: vec![],
            failover: FailoverPolicy::default(),
        }
    }

    #[test]
    fn widening_is_refused_and_narrowing_is_allowed() {
        let before = base();
        let refuse = |after: &BotConfig, prompt: &str| {
            let error = guard_no_widening(&before, "p", after, prompt).unwrap_err();
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
        };
        let mut more_tools = base();
        more_tools.tools.push("fetch_url".into());
        refuse(&more_tools, "p");
        let mut more_skills = base();
        more_skills.skills.push("b".into());
        refuse(&more_skills, "p");
        let mut less_approval = base();
        less_approval.require_approval.clear();
        refuse(&less_approval, "p");
        let mut consent = base();
        consent.failover.allow_cross_provider = true;
        refuse(&consent, "p");
        let mut role = base();
        role.role = BotRole::Orchestrator;
        refuse(&role, "p");
        refuse(&base(), "a different prompt");

        let mut narrower = base();
        narrower.tools.clear();
        narrower.skills.clear();
        narrower.failover.max_switches = 1;
        guard_no_widening(&before, "p", &narrower, "p").unwrap();
    }
}
