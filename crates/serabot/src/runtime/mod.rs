//! Turn execution: per-user bot routing, the durable turn loop, and the
//! schedules that feed it.
//!
//! Bots are resolved through the per-user [`FleetRegistry`] (phase 3 of the
//! multi-user reconstruction): a task's owning user is the only fleet that can
//! serve its turn, and the prompt, tool allowlist, delegation targets, and
//! memory injection all come from that user's rows. A user with an invalid
//! fleet fails closed without affecting any other user.
//!
//! Phase 5 enforces `[users].tasks_per_user_per_day` at the user-facing
//! enqueue points ([`Runtime::submit_text`]) through the store's atomic
//! quota check. Internal continuations — approval resumes, handoffs, team
//! fan-out — are not re-counted: they continue work that already consumed a
//! slot when the user submitted it. Admin-configured scheduled routines are
//! server work and are likewise not charged to the user's request budget.

use std::collections::HashSet;
use std::sync::Arc;

use async_trait::async_trait;
use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::sync::Semaphore;
use tokio_util::sync::CancellationToken;
use tracing::{error, info, warn};

use crate::ask::{ABSTENTION, validate_citations};
use crate::config::{Config, LoadedBot};
use crate::delegate::{DELEGATE_TOOL, HANDOFF_QUEUED_KEY};
use crate::domain::{Action, Approval, LlmMessage, Role, Task, ToolCall, Turn, new_id};
use crate::error::{Result, SeraError};
use crate::fleet::FleetRegistry;
use crate::gateway::{
    Decision, DecisionResult, Gateway, Tool, ToolContext, ToolOutcome, ToolOutput, ToolSpec,
};
use crate::memory::{self, MemoryHub};
use crate::model_router::{ModelRouter, ModelSelection, PinnedRoute, ProviderView};
use crate::orchestration::{
    BotRole, ConversationKind, HandoffContext, ORCHESTRATOR_BOT, assistant_speaker,
    parse_conversation_scope, safe_bot_label, validate_conversation_id,
};
use crate::retrieval::{RetrievedChunk, Retriever, evidence_block};
use crate::skills::on_demand_skill_clause;
use crate::store::Store;
use crate::teams::{
    AUDIT_ASKED, AUDIT_ROUND_COMPLETED, AUDIT_TURN_COMPLETED, TEAM_ASK_QUEUED_KEY, TEAM_ASK_TOOL,
    TEAM_REPLY_TOOL, TeamContext, TeamMode, enqueue_turn_task_with_id,
};
use crate::util::truncate_chars;

mod activity;
use activity::StepPhase;
mod extract;
mod models;
mod notifier;
mod prompts;
mod rag_tool;
mod recall;
mod routing;
mod schedules;
#[cfg(test)]
mod tests;
mod turn_loop;
mod worker;

pub use notifier::{ApprovalNotice, CompositeNotifier, Notifier, NullNotifier};
pub use rag_tool::RagSearchTool;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TurnPayload {
    pub conversation_id: String,
    pub text: String,
    pub bot: String,
    #[serde(default)]
    pub source: String,
    #[serde(default)]
    pub ask_mode: bool,
    #[serde(default)]
    pub route: Option<PinnedRoute>,
    #[serde(default)]
    pub handoff: Option<HandoffContext>,
    /// Present on turns created by `team_ask`, `team_reply`, or the room-mode
    /// `all` fan-out. Carries the room-turn frame used to enforce the bounds.
    #[serde(default)]
    pub team: Option<TeamContext>,
    /// Validated `@mention` bot ids parsed from the owner text in a `room:`
    /// conversation. Routing metadata only: never injected as instructions,
    /// and never routed to bots that are not room team members.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub mentions: Vec<String>,
    /// Chat attachment ids (already validated as the owner's) for this turn.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attachments: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResumePayload {
    pub turn_id: String,
    pub action_id: String,
    pub decision: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScheduledPayload {
    pub conversation_id: String,
    /// Owning user carried on the payload (phase 2): the task row's
    /// `user_id` remains authoritative — the worker always processes under
    /// the task row's owner.
    #[serde(default)]
    pub user_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct TurnState {
    #[serde(default)]
    messages: Vec<LlmMessage>,
    #[serde(default)]
    tool_calls: usize,
    #[serde(default)]
    pending: Option<PendingTool>,
    #[serde(default)]
    evidence: Vec<RetrievedChunk>,
    #[serde(default)]
    ask_mode: bool,
    #[serde(default)]
    route: Option<PinnedRoute>,
    #[serde(default)]
    handoff_queued: bool,
    #[serde(default)]
    handoff_to_bot: Option<String>,
    /// Present for specialist turns created by `delegate`. Kept on the turn so
    /// approval resumes, whose task payload is a `ResumePayload`, still finish
    /// the handoff instead of parsing the parent turn body.
    #[serde(default)]
    handoff: Option<HandoffContext>,
    /// Present for turns created by a room team ask or peer reply.
    #[serde(default)]
    team: Option<TeamContext>,
    #[serde(default)]
    team_asked: bool,
    /// History rows sent as context at the start of this turn (metadata for
    /// the `model.used` audit row).
    #[serde(default)]
    history_messages: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct PendingTool {
    call: ToolCall,
    remaining: Vec<ToolCall>,
    approval_id: String,
    action_id: String,
}

#[derive(Clone)]
pub struct Runtime {
    config: Arc<Config>,
    store: Store,
    gateway: Gateway,
    router: ModelRouter,
    fleet: Arc<FleetRegistry>,
    notifier: Arc<dyn Notifier>,
    semaphore: Arc<Semaphore>,
    worker_id: String,
    shutdown: CancellationToken,
    memory: MemoryHub,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TurnOutcome {
    Completed,
    Suspended,
}

impl Runtime {
    /// Build a runtime from the repository seed templates. The templates are
    /// copied into each user's fleet rows lazily; production uses
    /// [`Runtime::with_fleet`] so the skill and delegation tools share the same
    /// registry.
    pub fn new(
        config: Arc<Config>,
        store: Store,
        gateway: Gateway,
        router: ModelRouter,
        bots: Vec<LoadedBot>,
        notifier: Arc<dyn Notifier>,
        shutdown: CancellationToken,
    ) -> Result<Self> {
        let catalog = Arc::new(crate::skills::SkillCatalog::load(&config)?);
        let fleet = Arc::new(FleetRegistry::new(
            config.clone(),
            store.clone(),
            bots,
            catalog,
        )?);
        Self::with_fleet(config, store, gateway, router, fleet, notifier, shutdown)
    }

    /// Build a runtime around an existing per-user fleet registry.
    pub fn with_fleet(
        config: Arc<Config>,
        store: Store,
        gateway: Gateway,
        mut router: ModelRouter,
        fleet: Arc<FleetRegistry>,
        notifier: Arc<dyn Notifier>,
        shutdown: CancellationToken,
    ) -> Result<Self> {
        if config.runtime.max_concurrent_turns == 0 {
            return Err(SeraError::Config(
                "runtime.max_concurrent_turns must be positive".into(),
            ));
        }
        for bot in fleet.templates() {
            if let Some(model) = &bot.config.model {
                router.add_default_model(model)?;
            }
        }
        let semaphore = Arc::new(Semaphore::new(config.runtime.max_concurrent_turns));
        let memory = MemoryHub::new(&config.memory, gateway.clone())?;
        Ok(Self {
            config,
            store,
            gateway,
            router,
            fleet,
            notifier,
            semaphore,
            worker_id: format!("worker-{}", &uuid::Uuid::new_v4().to_string()[..8]),
            shutdown,
            memory,
        })
    }

    /// The agent memory provider: local SQLite, plus the optional third-party
    /// recall source when `[memory] provider = "mem0"`.
    pub fn memory(&self) -> &MemoryHub {
        &self.memory
    }

    pub fn store(&self) -> &Store {
        &self.store
    }

    pub fn config(&self) -> &Config {
        &self.config
    }

    /// The per-user fleet registry shared with the skill and delegation
    /// tools.
    pub fn fleet(&self) -> Arc<FleetRegistry> {
        self.fleet.clone()
    }

    /// Whether the acting user's fleet currently contains this bot. A fleet
    /// that fails validation reports false (fail closed).
    pub fn bot_loaded(&self, user_id: &str, name: &str) -> bool {
        self.fleet
            .load(user_id)
            .map(|fleet| fleet.contains_key(name))
            .unwrap_or(false)
    }

    /// The acting user's loaded bot registry entry, if the bot is live.
    pub fn bot(&self, user_id: &str, name: &str) -> Option<LoadedBot> {
        self.fleet
            .load(user_id)
            .ok()
            .and_then(|fleet| fleet.get(name).cloned())
    }

    pub fn router(&self) -> &ModelRouter {
        &self.router
    }

    pub fn gateway(&self) -> &Gateway {
        &self.gateway
    }

    pub fn policy_version(&self) -> &str {
        self.gateway.policy().version()
    }

    pub fn policy(&self) -> &crate::policy::Policy {
        self.gateway.policy()
    }

    /// Scan owner/console output against the policy and attribute a block to
    /// the acting user in the audit ledger.
    pub fn scan_owner_output(&self, user_id: &str, text: &str) -> Result<()> {
        self.gateway.scan_egress(user_id, text)
    }
}

/// Manual (regex-free) parse of `@` mentions from owner text: `@` followed by
/// `[a-z0-9][a-z0-9-_]{0,31}`. A candidate is kept only when it is a configured
/// bot id (and passes `safe_bot_label`), so mentions can never carry arbitrary
/// strings. Order is preserved, duplicates removed.
fn parse_mentions(text: &str, known_bots: &HashSet<String>) -> Vec<String> {
    let bytes = text.as_bytes();
    let mut out: Vec<String> = Vec::new();
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] != b'@' {
            index += 1;
            continue;
        }
        let start = index + 1;
        let mut end = start;
        while end < bytes.len() && end - start < 32 {
            let byte = bytes[end];
            let valid = byte.is_ascii_lowercase()
                || byte.is_ascii_digit()
                || (end > start && (byte == b'-' || byte == b'_'));
            if !valid {
                break;
            }
            end += 1;
        }
        if end > start {
            let first = bytes[start];
            if first.is_ascii_lowercase() || first.is_ascii_digit() {
                let candidate = &text[start..end];
                if safe_bot_label(candidate)
                    && known_bots.contains(candidate)
                    && !out.iter().any(|mention| mention == candidate)
                {
                    out.push(candidate.to_owned());
                }
            }
        }
        index = if end > start { end } else { start };
    }
    out
}

/// Opt-in silence rule for a watch run: silent only when every source reported
/// zero new items, zero changed items, zero failures, and no source error.
fn watch_run_is_silent(silent_when_empty: bool, reports: &[crate::watcher::WatchReport]) -> bool {
    silent_when_empty
        && reports
            .iter()
            .all(|report| report.new == 0 && report.changed == 0 && report.failed == 0 && report.ok)
}

/// Rough prompt-size estimate recorded on `model.used` audit rows: the summed
/// content chars of the messages sent this round, divided by 4, rounded up.
/// Pure metadata; it never changes routing or payloads.
pub(crate) fn estimate_prompt_chars(messages: &[LlmMessage]) -> usize {
    let chars: usize = messages
        .iter()
        .map(|message| message.content.as_deref().map_or(0, str::len))
        .sum();
    chars.div_ceil(4)
}

const TOOL_NOTICE_PREFIX: &str = "AVAILABLE TOOLS THIS TURN:";

/// One-line description: the first sentence, capped in length.
fn one_line(description: &str) -> String {
    let first = description.split(". ").next().unwrap_or(description);
    let flat = first.split_whitespace().collect::<Vec<_>>().join(" ");
    let flat = flat.trim_end_matches('.');
    if flat.chars().count() > 160 {
        let cut: String = flat.chars().take(157).collect();
        format!("{cut}...")
    } else {
        flat.to_owned()
    }
}

/// The per-turn notice body: every tool with a one-line description, browser
/// tools and `vault_list` grouped under a short browser line.
fn render_tool_list(tools: &[ToolSpec]) -> String {
    let mut lines = Vec::new();
    let browser: Vec<&str> = tools
        .iter()
        .filter(|tool| tool.name.starts_with("browser_") || tool.name == "vault_list")
        .map(|tool| tool.name.as_str())
        .collect();
    if !browser.is_empty() {
        lines.push(format!(
            "- You can control a real web browser on the server: {}. Open pages with browser_navigate, read them with browser_snapshot, act with browser_click, browser_type and browser_press; vault_list names the owner's stored logins, which browser_type can fill without ever showing you the secret. Element refs such as e12 are internal handles for your tool calls: never write them in a reply to the user; name an element by its visible text (for example the Sign in button) instead.",
            browser.join(", ")
        ));
    }
    for tool in tools {
        if tool.name.starts_with("browser_") || tool.name == "vault_list" {
            continue;
        }
        let gate = if tool.destructive() {
            " [needs owner approval]"
        } else {
            ""
        };
        lines.push(format!(
            "- {}: {}{gate}",
            tool.name,
            one_line(&tool.description)
        ));
    }
    lines.join("\n")
}

/// Keeps exactly one authoritative tool-list notice in the conversation.
/// An existing notice is refreshed in place so stale lists from earlier
/// turns never survive; otherwise the notice is inserted after the first
/// system message.
fn upsert_tool_notice(messages: &mut Vec<LlmMessage>, tools: &[ToolSpec]) {
    let tool_list = render_tool_list(tools);
    let content = format!(
        "{TOOL_NOTICE_PREFIX}\n{tool_list}\nThis list is authoritative for the current turn and overrides any earlier assistant statement in the conversation about which tools exist. If the owner asks which tools are available, answer only from this list. These tools are callable now; use them on your own judgement. Only tools marked [needs owner approval] wait for the owner; for those, request the approval instead of saying the tool is unavailable."
    );
    if let Some(existing) = messages.iter_mut().find(|message| {
        message.role == Role::System
            && message
                .content
                .as_deref()
                .is_some_and(|text| text.starts_with(TOOL_NOTICE_PREFIX))
    }) {
        existing.content = Some(content);
    } else {
        messages.insert(1, LlmMessage::system(content));
    }
}
