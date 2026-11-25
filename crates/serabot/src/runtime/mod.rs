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

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use async_trait::async_trait;
use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::sync::Semaphore;
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info, warn};

use crate::ask::{ABSTENTION, validate_citations};
use crate::config::{Config, LoadedBot};
use crate::delegate::{DELEGATE_TOOL, HANDOFF_QUEUED_KEY, MAX_HANDOFFS_PER_TURN};
use crate::domain::{Action, Approval, LlmMessage, Role, Task, ToolCall, Turn, new_id};
use crate::error::{Result, SeraError};
use crate::fleet::FleetRegistry;
use crate::gateway::{
    Decision, DecisionResult, Gateway, Tool, ToolContext, ToolOutcome, ToolOutput, ToolSpec,
};
use crate::memory::{self, MemoryHub};
use crate::model_router::{ModelRouter, ModelSelection, PinnedRoute, ProviderView, RouterHandle};
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

pub(crate) mod activity;
mod auto_resume;
mod compaction;
use activity::StepPhase;
mod conflicts;
mod extract;
mod failover;
mod models;
mod notifier;
mod prompts;
mod rag_tool;
mod recall;
mod refusal;
mod routing;
mod schedules;
#[cfg(test)]
mod tests;
mod turn_loop;
mod worker;

pub use notifier::{ApprovalNotice, CompositeNotifier, Notifier, NullNotifier};
pub use rag_tool::RagSearchTool;
pub(crate) use turn_loop::LIMIT_REACHED_STATUS;

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
    /// How many `ask_teammate` hops produced this turn. `delegate` copies it
    /// onto specialist handoffs so a handed-on task cannot bounce forever.
    #[serde(default)]
    pub help_depth: u8,
    /// Bot that asked for this turn, when `help_depth` is greater than zero.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub help_from: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResumePayload {
    pub turn_id: String,
    pub action_id: String,
    pub decision: String,
    /// Set by "Continue after limit": the turn ended at a tool limit, has no
    /// pending approval, and carries on with a fresh tool budget.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub continue_after_limit: bool,
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
    /// Every specialist the orchestrator queued a handoff to this turn (0.27:
    /// several handoffs per turn fan out to parallel subagents).
    #[serde(default)]
    handoffs: Vec<String>,
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
    /// Copied from the task payload so a resumed turn keeps the handoff cap.
    #[serde(default)]
    help_depth: u8,
    /// One `ask_teammate` per turn.
    #[serde(default)]
    help_asked: bool,
    /// History rows sent as context at the start of this turn (metadata for
    /// the `model.used` audit row).
    #[serde(default)]
    history_messages: usize,
    /// Context compactions this turn has used (at most `MAX_COMPACTIONS`).
    #[serde(default)]
    compactions: u8,
    /// Seconds this turn has spent parked in `waiting_model`.
    #[serde(default)]
    waited_secs: u64,
    /// Effectful tool calls that already ran, with their stored outcome: a
    /// retried turn replays the stored result instead of repeating the effect.
    #[serde(default)]
    completed_effects: Vec<CompletedEffect>,
    /// One short line per tool step this turn ran, for the persisted digest.
    #[serde(default)]
    tool_digest: Vec<String>,
}

/// One effectful tool call whose outcome is already stored, so a retried turn
/// can skip it without repeating the effect.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct CompletedEffect {
    call_id: String,
    tool: String,
    content: String,
    is_error: bool,
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
    /// The live model router: a console provider save swaps it in place.
    router: RouterHandle,
    fleet: Arc<FleetRegistry>,
    notifier: Arc<dyn Notifier>,
    semaphore: Arc<Semaphore>,
    worker_id: String,
    shutdown: CancellationToken,
    memory: MemoryHub,
    extract_gate: Arc<extract::ExtractGate>,
    /// Turn ids the owner asked to stop. The turn loop checks this between
    /// model and tool steps; a marked turn never starts another step.
    stop_requests: Arc<std::sync::Mutex<HashSet<String>>>,
    /// Per-turn cancellation tokens fired by `request_stop` / `request_stop_task`
    /// so in-flight model/compaction calls abort immediately without waiting for
    /// upstream timeouts.
    stop_tokens: Arc<std::sync::Mutex<HashMap<String, CancellationToken>>>,
    /// Task ids whose stop arrived while claimed before the turn row was inserted.
    stop_tasks: Arc<std::sync::Mutex<HashSet<String>>>,
}

pub(super) struct TurnStopGuard {
    turn_id: String,
    tokens: Arc<std::sync::Mutex<HashMap<String, CancellationToken>>>,
}

impl Drop for TurnStopGuard {
    fn drop(&mut self) {
        self.tokens
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(&self.turn_id);
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TurnOutcome {
    Completed,
    Suspended,
    /// No model route is ready: the task parks until `until`, and the turn
    /// resumes from its stored state.
    Waiting {
        until: chrono::DateTime<Utc>,
    },
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
        router: impl Into<RouterHandle>,
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

    /// Turn on semantic memory recall with the shared local embedder
    /// (0.62). A no-op when `[memory] semantic_recall = false`.
    pub fn with_semantic_memory(mut self, embedder: Arc<dyn crate::embeddings::Embedder>) -> Self {
        if self.config.memory.semantic_recall {
            let index = crate::memory_vectors::SemanticIndex::new(
                embedder,
                self.config.memory.semantic_min_similarity,
            );
            self.memory = self.memory.clone().with_semantic(index);
        }
        self
    }

    /// Build a runtime around an existing per-user fleet registry.
    pub fn with_fleet(
        config: Arc<Config>,
        store: Store,
        gateway: Gateway,
        router: impl Into<RouterHandle>,
        fleet: Arc<FleetRegistry>,
        notifier: Arc<dyn Notifier>,
        shutdown: CancellationToken,
    ) -> Result<Self> {
        if config.runtime.max_concurrent_turns == 0 {
            return Err(SeraError::Config(
                "runtime.max_concurrent_turns must be positive".into(),
            ));
        }
        let router: RouterHandle = router.into();
        router.update(|router| {
            for bot in fleet.templates() {
                if let Some(model) = &bot.config.model {
                    router.add_default_model(model)?;
                }
            }
            Ok(())
        })?;
        let semaphore = Arc::new(Semaphore::new(config.runtime.max_concurrent_turns));
        let memory = MemoryHub::new(&config.memory, gateway.clone())?;
        if let Some(model) = &config.memory.extract_model
            && router.get().provider_for_model(model).is_none()
        {
            return Err(SeraError::Config(format!(
                "memory.extract_model {model} is not served by any configured provider"
            )));
        }
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
            extract_gate: Arc::new(extract::ExtractGate::default()),
            stop_requests: Arc::new(std::sync::Mutex::new(HashSet::new())),
            stop_tokens: Arc::new(std::sync::Mutex::new(HashMap::new())),
            stop_tasks: Arc::new(std::sync::Mutex::new(HashSet::new())),
        })
    }

    /// The agent memory provider: local SQLite, plus the optional third-party
    /// recall source when `[memory] provider = "mem0"`.
    pub fn memory(&self) -> &MemoryHub {
        &self.memory
    }

    /// Ask the acting user's running or pending turns in one conversation to
    /// stop immediately: in-flight model calls are cancelled, queued/parked
    /// tasks are cancelled in the database, and any in-flight turn ends at its
    /// next safe point with `"Stopped."`. Returns how many turns/tasks were
    /// stopped or signalled.
    pub fn request_stop(&self, user_id: &str, conversation_id: &str) -> Result<usize> {
        validate_conversation_id(conversation_id)?;
        let mut count = self
            .store
            .cancel_pending_conversation_tasks(user_id, conversation_id)?;
        let turn_ids = self.store.running_turn_ids(user_id, conversation_id)?;
        let mut seen_turns: HashSet<String> = turn_ids.iter().cloned().collect();
        if !turn_ids.is_empty() {
            {
                let mut marks = self
                    .stop_requests
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                marks.extend(turn_ids.iter().cloned());
            }
            let tokens: Vec<CancellationToken> = {
                let map = self
                    .stop_tokens
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                turn_ids
                    .iter()
                    .filter_map(|id| map.get(id).cloned())
                    .collect()
            };
            for token in tokens {
                token.cancel();
            }
            count += turn_ids.len();
        }
        let early_tasks = self
            .store
            .running_task_ids_without_turn(user_id, conversation_id)?;
        let mut seen_tasks: HashSet<String> = HashSet::new();
        if !early_tasks.is_empty() {
            let mut pending_tasks = self
                .stop_tasks
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            for task_id in &early_tasks {
                seen_tasks.insert(task_id.clone());
                if pending_tasks.insert(task_id.clone()) {
                    count += 1;
                }
            }
        }
        if let Ok(handoffs) = self
            .store
            .list_handoffs(user_id, Some(conversation_id), 200)
        {
            for handoff in handoffs {
                if !matches!(handoff.status, "queued" | "running" | "resumed") {
                    continue;
                }
                let child_turns = self
                    .store
                    .running_turn_ids_for_task(user_id, &handoff.child_task_id)?;
                let mut cancelled_child_turn = false;
                for tid in child_turns {
                    if !seen_turns.insert(tid.clone()) {
                        cancelled_child_turn = true;
                        continue;
                    }
                    cancelled_child_turn = true;
                    self.stop_requests
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner())
                        .insert(tid.clone());
                    let token = self
                        .stop_tokens
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner())
                        .get(&tid)
                        .cloned();
                    if let Some(token) = token {
                        token.cancel();
                    }
                    count += 1;
                }
                if !cancelled_child_turn
                    && seen_tasks.insert(handoff.child_task_id.clone())
                    && let Ok(task) = self.store.get_task(user_id, &handoff.child_task_id)
                    && matches!(task.status.as_str(), "running" | "queued" | "waiting_model")
                    && self
                        .stop_tasks
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner())
                        .insert(handoff.child_task_id)
                {
                    count += 1;
                }
            }
        }
        Ok(count)
    }

    /// Ask the acting user's running turn of one task (a subagent's child)
    /// to stop immediately. Returns how many turns were marked.
    pub fn request_stop_task(&self, user_id: &str, task_id: &str) -> Result<usize> {
        let turn_ids = self.store.running_turn_ids_for_task(user_id, task_id)?;
        if !turn_ids.is_empty() {
            {
                let mut marks = self
                    .stop_requests
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                marks.extend(turn_ids.iter().cloned());
            }
            let tokens: Vec<CancellationToken> = {
                let map = self
                    .stop_tokens
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                turn_ids
                    .iter()
                    .filter_map(|id| map.get(id).cloned())
                    .collect()
            };
            for token in tokens {
                token.cancel();
            }
            return Ok(turn_ids.len());
        }
        if let Ok(task) = self.store.get_task(user_id, task_id)
            && matches!(task.status.as_str(), "running" | "queued" | "waiting_model")
        {
            self.stop_tasks
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .insert(task_id.to_owned());
            return Ok(1);
        }
        Ok(0)
    }

    pub(super) fn register_stop_token(&self, turn_id: &str) -> (CancellationToken, TurnStopGuard) {
        let token = CancellationToken::new();
        if self.stop_requested(turn_id) {
            token.cancel();
        }
        self.stop_tokens
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(turn_id.to_owned(), token.clone());
        let guard = TurnStopGuard {
            turn_id: turn_id.to_owned(),
            tokens: Arc::clone(&self.stop_tokens),
        };
        (token, guard)
    }

    pub(super) fn take_task_stop(&self, task_id: &str) -> bool {
        self.stop_tasks
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(task_id)
    }

    fn stop_requested(&self, turn_id: &str) -> bool {
        self.stop_requests
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .contains(turn_id)
    }

    fn clear_stop(&self, turn_id: &str) {
        self.stop_requests
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(turn_id);
        self.stop_tokens
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(turn_id);
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

    /// The model router as it stands now. Hold the returned `Arc` for one
    /// piece of work; a console provider save may replace it for the next.
    pub fn router(&self) -> Arc<ModelRouter> {
        self.router.get()
    }

    /// The shared handle, for tools that read the live router.
    pub fn router_handle(&self) -> &RouterHandle {
        &self.router
    }

    /// Serve `router` from now on (a console provider save). Cached
    /// per-user clients of changed providers are dropped with the old one.
    pub fn replace_router(&self, router: ModelRouter) {
        self.router.replace(router);
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
/// Tool-call arguments and signed reasoning count too (a call with a huge
/// argument list is a huge prompt), and each inline image carries a flat
/// allowance: the provider charges vision tokens the text never shows.
/// Pure metadata; it never changes routing or payloads.
pub(crate) fn estimate_prompt_chars(messages: &[LlmMessage]) -> usize {
    const IMAGE_TOKENS_EST: usize = 1_000;
    let chars: usize = messages
        .iter()
        .map(|message| {
            let mut chars = message.content.as_deref().map_or(0, str::len);
            for call in &message.tool_calls {
                chars += call.name.len();
                chars += serde_json::to_string(&call.arguments).map_or(0, |text| text.len());
                chars += call.reasoning_signature.as_deref().map_or(0, str::len);
            }
            chars += message.images.len() * IMAGE_TOKENS_EST * 4;
            chars
        })
        .sum();
    chars.div_ceil(4)
}

const TOOL_NOTICE_PREFIX: &str = "AVAILABLE TOOLS THIS TURN:";
const APP_NOTICE_PREFIX: &str = "MARKETPLACE APPS NEEDING ATTENTION:";

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
            "- You can control a real web browser on the server: {}. Open pages with browser_navigate, read them with browser_snapshot, act with browser_click, browser_type and browser_press; vault_list names the owner's stored logins, which browser_type can fill without ever showing you the secret. Use browser_snapshot with mode interactive (actionable elements only) or text (readable text) to keep reads small, browser_find to locate an element by role and name or text, browser_wait to wait for text or loading, and browser_dismiss_banners for a cookie banner (navigation already tries it); a click that fails because the element is covered, disabled or hidden says why, so fix that and retry. browser_upload sends an owner's file to the site and needs approval. Element refs such as e12 are internal handles for your tool calls: never write them in a reply to the user; name an element by its visible text (for example the Sign in button) instead.",
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
/// Keep one system line naming the marketplace apps the owner enabled that could
/// not be used this turn (for example "Canva needs sign-in"), or remove it when
/// every app works. The wording is fixed, so no server text reaches the prompt.
fn upsert_app_notice(messages: &mut Vec<LlmMessage>, notes: &[String]) {
    let existing = messages.iter().position(|message| {
        message.role == Role::System
            && message
                .content
                .as_deref()
                .is_some_and(|text| text.starts_with(APP_NOTICE_PREFIX))
    });
    if notes.is_empty() {
        if let Some(index) = existing {
            messages.remove(index);
        }
        return;
    }
    let content = format!(
        "{APP_NOTICE_PREFIX} {}. Tell the owner if they ask for it; they fix it in Integrations.",
        notes.join("; ")
    );
    match existing {
        Some(index) => messages[index].content = Some(content),
        None => {
            let at = 2.min(messages.len());
            messages.insert(at, LlmMessage::system(content));
        }
    }
}

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
