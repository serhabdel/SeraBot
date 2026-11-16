//! The runs sidebar's model (v0.50): the subagent tree of the open
//! conversation, its status chips, the hold badge and the wait/compaction
//! rows the orchestration events feed.
//!
//! The server owns the ledger (`GET /v1/handoffs`). Events only mark the list
//! stale and tell which child is waiting, so a burst of events costs one
//! refresh. Everything here is pure: the caller passes the clock, and every
//! text that came from the server is checked against a fixed vocabulary or
//! bounded before it is shown.

use std::collections::{BTreeMap, HashSet};

use serde::Deserialize;
use serde_json::Value;

/// Most runs the list keeps (the server sends at most 100).
pub const MAX_RUNS: usize = 100;
/// Siblings in the same state that collapse into one counted line.
pub const COLLAPSE_AT: usize = 3;
/// Deepest nesting drawn; deeper runs are drawn at this depth.
pub const MAX_DEPTH: u8 = 3;
/// Waiting children and holds the board remembers.
const MAX_OVERLAYS: usize = 64;
const MAX_HOLDS: usize = 16;
const MAX_COMPACTIONS: usize = 20;

/// One ledger row as `GET /v1/handoffs` sends it.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq)]
pub struct Handoff {
    pub id: String,
    #[serde(default)]
    pub parent_task_id: String,
    #[serde(default)]
    pub child_task_id: String,
    #[serde(default)]
    pub to_bot: String,
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub outcome_code: Option<String>,
    #[serde(default)]
    pub effects_unknown: bool,
    #[serde(default)]
    pub resume_count: i64,
    #[serde(default)]
    pub route: Option<Value>,
    #[serde(default)]
    pub conversation_id: Option<String>,
    #[serde(default)]
    pub created_at: String,
}

/// Reply of `GET /v1/handoffs`.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq)]
pub struct HandoffList {
    #[serde(default)]
    pub handoffs: Vec<Handoff>,
}

/// What a run is doing, in the words of the chip.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Chip {
    Queued,
    Running,
    WaitingApproval,
    WaitingModel,
    FailedResumable,
    Failed,
    Completed,
    /// Resumed on another route; the text names it.
    ResumedOn(String),
}

/// The chip's state without its text, so runs of one state can collapse.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ChipKind {
    Queued,
    Running,
    WaitingApproval,
    WaitingModel,
    FailedResumable,
    Failed,
    Completed,
    Resumed,
}

impl Chip {
    pub fn kind(&self) -> ChipKind {
        match self {
            Self::Queued => ChipKind::Queued,
            Self::Running => ChipKind::Running,
            Self::WaitingApproval => ChipKind::WaitingApproval,
            Self::WaitingModel => ChipKind::WaitingModel,
            Self::FailedResumable => ChipKind::FailedResumable,
            Self::Failed => ChipKind::Failed,
            Self::Completed => ChipKind::Completed,
            Self::ResumedOn(_) => ChipKind::Resumed,
        }
    }

    pub fn label(&self) -> String {
        match self {
            Self::Queued => "Queued".into(),
            Self::Running => "Running".into(),
            Self::WaitingApproval => "Waiting for approval".into(),
            Self::WaitingModel => "Waiting for a model".into(),
            Self::FailedResumable => "Failed, can resume".into(),
            Self::Failed => "Failed".into(),
            Self::Completed => "Done".into(),
            Self::ResumedOn(route) if route.is_empty() => "Resumed".into(),
            Self::ResumedOn(route) => format!("Resumed on {route}"),
        }
    }
}

impl ChipKind {
    /// The stylesheet tone: `live`, `wait`, `bad` or `ok`.
    pub fn tone(self) -> &'static str {
        match self {
            Self::Queued | Self::Running | Self::Resumed => "live",
            Self::WaitingApproval | Self::WaitingModel => "wait",
            Self::FailedResumable | Self::Failed => "bad",
            Self::Completed => "ok",
        }
    }

    /// The plural line of a collapsed group: `3 running`.
    fn group_word(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Running => "running",
            Self::WaitingApproval => "waiting for approval",
            Self::WaitingModel => "waiting for a model",
            Self::FailedResumable => "failed, can resume",
            Self::Failed => "failed",
            Self::Completed => "done",
            Self::Resumed => "resumed",
        }
    }

    /// Failed runs each need their own Retry, so they never collapse.
    fn collapses(self) -> bool {
        !matches!(self, Self::FailedResumable | Self::Failed)
    }
}

/// What the events say about a child the ledger still calls running.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Overlay {
    pub approval: bool,
    /// The child is parked until this stamp.
    pub model_until: Option<String>,
}

/// One run, ready to draw.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RunRow {
    pub id: String,
    pub child_task_id: String,
    pub parent_task_id: String,
    pub bot: String,
    pub chip: Chip,
    /// Reason words of a failed run, from a fixed vocabulary.
    pub reasons: Vec<String>,
    pub resume_count: i64,
    pub effects_unknown: bool,
    /// The stamp a waiting-model child is parked until.
    pub until: Option<String>,
    /// `provider/model` of the route the ledger recorded, if any.
    pub route: Option<String>,
}

impl RunRow {
    /// A queued or running child can be stopped.
    pub fn can_stop(&self) -> bool {
        matches!(
            self.chip.kind(),
            ChipKind::Queued
                | ChipKind::Running
                | ChipKind::WaitingApproval
                | ChipKind::WaitingModel
                | ChipKind::Resumed
        )
    }

    /// Retry is offered only for a run the server can resume. A run whose
    /// earlier attempt may have had an effect with an unknown outcome is not
    /// replayed from here; the owner checks the result first.
    pub fn can_retry(&self) -> bool {
        self.chip.kind() == ChipKind::FailedResumable && !self.effects_unknown
    }

    /// A failed run the sidebar will not replay, with the reason why.
    pub fn retry_blocked_note(&self) -> Option<&'static str> {
        match self.chip.kind() {
            ChipKind::Failed | ChipKind::FailedResumable if self.effects_unknown => {
                Some("It may have changed something. Check the result before trying again.")
            }
            ChipKind::Failed => Some("It has used all its retries."),
            _ => None,
        }
    }
}

/// The words a reason code may show as. Anything else is dropped, so
/// provider text never reaches a chip.
pub fn reason_word(code: &str) -> Option<&'static str> {
    Some(match code {
        "rate_limited" => "Rate limited",
        "overloaded" => "Overloaded",
        "model_unavailable" => "Model unavailable",
        "quota_exhausted" => "Quota used up",
        "credentials_rejected" => "Sign-in rejected",
        "account_forbidden" => "Account not allowed",
        "provider_down" => "Provider down",
        "transport" => "Connection problem",
        "context_too_long" => "Too long",
        "invalid_request" => "Request rejected",
        "content_filtered" => "Filtered",
        "policy_blocked" => "Blocked by policy",
        "egress_denied" => "Provider not approved",
        "cancelled" => "Stopped",
        _ => return None,
    })
}

/// Chips for a code text such as `rate_limited` or `rate_limited; quota_exhausted`.
/// Known words only, in order, without repeats, at most four.
pub fn reason_chips(text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for part in text.split(|c: char| !(c.is_ascii_alphanumeric() || c == '_')) {
        if let Some(word) = reason_word(part)
            && !out.iter().any(|seen| seen == word)
            && out.len() < 4
        {
            out.push(word.to_owned());
        }
    }
    out
}

/// `provider/model` of a ledger route (`{"selection": {"provider","model"}}`),
/// with control characters dropped and the length bounded.
fn route_label(route: &Value) -> Option<String> {
    let selection = route.get("selection").unwrap_or(route);
    let text = |key: &str| {
        selection
            .get(key)
            .and_then(Value::as_str)
            .map(|value| {
                value
                    .chars()
                    .filter(|c| !c.is_control())
                    .take(48)
                    .collect::<String>()
            })
            .filter(|value| !value.is_empty())
    };
    let provider = text("provider")?;
    match text("model") {
        Some(model) if provider != "default" => Some(format!("{provider}/{model}")),
        Some(model) => Some(model),
        None => Some(provider),
    }
}

fn clean_id(text: &str) -> String {
    text.chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | ':' | '.'))
        .take(96)
        .collect()
}

/// The chip of one ledger row, given what the events added.
pub fn chip_for(handoff: &Handoff, overlay: Option<&Overlay>) -> Chip {
    let waiting = |overlay: Option<&Overlay>| {
        overlay.and_then(|o| {
            if o.approval {
                Some(Chip::WaitingApproval)
            } else {
                o.model_until.as_ref().map(|_| Chip::WaitingModel)
            }
        })
    };
    match handoff.status.as_str() {
        "completed" => Chip::Completed,
        "failed" => Chip::Failed,
        "failed_resumable" => Chip::FailedResumable,
        "resumed" => waiting(overlay).unwrap_or_else(|| {
            Chip::ResumedOn(
                handoff
                    .route
                    .as_ref()
                    .and_then(route_label)
                    .unwrap_or_default(),
            )
        }),
        "running" => waiting(overlay).unwrap_or(Chip::Running),
        _ => Chip::Queued,
    }
}

/// The drawable row of a ledger row.
pub fn row_of(handoff: &Handoff, overlay: Option<&Overlay>) -> RunRow {
    let reasons = handoff
        .outcome_code
        .as_deref()
        .map(reason_chips)
        .unwrap_or_default();
    RunRow {
        id: clean_id(&handoff.id),
        child_task_id: clean_id(&handoff.child_task_id),
        parent_task_id: clean_id(&handoff.parent_task_id),
        bot: clean_id(&handoff.to_bot),
        chip: chip_for(handoff, overlay),
        reasons,
        resume_count: handoff.resume_count,
        effects_unknown: handoff.effects_unknown,
        until: overlay.and_then(|o| o.model_until.clone()),
        route: handoff.route.as_ref().and_then(route_label),
    }
}

/// One line of the drawn tree.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TreeLine {
    Run {
        row: RunRow,
        depth: u8,
    },
    /// Several siblings in one state, shown as one counted line.
    Group {
        key: String,
        label: String,
        kind: ChipKind,
        count: usize,
        open: bool,
        depth: u8,
    },
}

/// The tree of runs, oldest first. Children hang under the run that spawned
/// them; siblings in the same state collapse into a counted line once there
/// are `COLLAPSE_AT` of them, unless their key is in `open`.
pub fn tree(rows: &[RunRow], open: &HashSet<String>) -> Vec<TreeLine> {
    let children: HashSet<&str> = rows.iter().map(|r| r.child_task_id.as_str()).collect();
    let roots: Vec<&RunRow> = rows
        .iter()
        .filter(|row| !children.contains(row.parent_task_id.as_str()))
        .collect();
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    emit_siblings(rows, &roots, 0, open, &mut out, &mut seen);
    out
}

fn emit_siblings(
    all: &[RunRow],
    siblings: &[&RunRow],
    depth: u8,
    open: &HashSet<String>,
    out: &mut Vec<TreeLine>,
    seen: &mut HashSet<String>,
) {
    // The server lists newest first; the tree reads oldest first.
    let ordered: Vec<&RunRow> = siblings.iter().rev().copied().collect();
    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    let key_of = |row: &RunRow| {
        format!(
            "{}:{:?}:{}",
            row.parent_task_id,
            row.chip.kind(),
            depth.min(MAX_DEPTH)
        )
    };
    for row in &ordered {
        if row.chip.kind().collapses() {
            *counts.entry(key_of(row)).or_default() += 1;
        }
    }
    let mut headed: HashSet<String> = HashSet::new();
    for row in ordered {
        if !seen.insert(row.id.clone()) {
            continue;
        }
        let kind = row.chip.kind();
        let key = key_of(row);
        let grouped = kind.collapses() && counts.get(&key).copied().unwrap_or(0) >= COLLAPSE_AT;
        let draw_depth = depth.min(MAX_DEPTH);
        if grouped {
            let is_open = open.contains(&key);
            if headed.insert(key.clone()) {
                let count = counts[&key];
                out.push(TreeLine::Group {
                    label: format!("{count} {}", kind.group_word()),
                    key: key.clone(),
                    kind,
                    count,
                    open: is_open,
                    depth: draw_depth,
                });
            }
            if !is_open {
                continue;
            }
            out.push(TreeLine::Run {
                row: row.clone(),
                depth: (draw_depth + 1).min(MAX_DEPTH),
            });
        } else {
            out.push(TreeLine::Run {
                row: row.clone(),
                depth: draw_depth,
            });
        }
        let below: Vec<&RunRow> = all
            .iter()
            .filter(|child| child.parent_task_id == row.child_task_id)
            .collect();
        if !below.is_empty() {
            emit_siblings(all, &below, depth + 1, open, out, seen);
        }
    }
}

/// Runs that still need the owner or the server: the number the conversation
/// list shows next to a chat.
pub fn active_count(rows: &[RunRow]) -> usize {
    rows.iter()
        .filter(|row| {
            matches!(
                row.chip.kind(),
                ChipKind::Queued
                    | ChipKind::Running
                    | ChipKind::WaitingApproval
                    | ChipKind::WaitingModel
                    | ChipKind::Resumed
            )
        })
        .count()
}

/// Runs the owner can act on (a failed run that can resume).
pub fn needs_attention(rows: &[RunRow]) -> usize {
    rows.iter()
        .filter(|row| row.chip.kind() == ChipKind::FailedResumable)
        .count()
}

/// A provider hold from `provider.hold_set`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Hold {
    pub provider: String,
    pub account_id: String,
    pub model: String,
    pub kind: String,
    pub until: String,
}

/// The hold badge: a short line and a longer tooltip.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HoldBadge {
    pub label: String,
    pub title: String,
}

/// `4:07` under an hour, `1h 02m` above, `any moment` when due.
pub fn countdown(remaining_secs: i64) -> String {
    if remaining_secs <= 0 {
        return "any moment".into();
    }
    let hours = remaining_secs / 3_600;
    let minutes = (remaining_secs % 3_600) / 60;
    let seconds = remaining_secs % 60;
    if hours > 0 {
        format!("{hours}h {minutes:02}m")
    } else {
        format!("{minutes}:{seconds:02}")
    }
}

/// `14:05 UTC` for an RFC 3339 stamp.
fn clock_utc(until: &str) -> Option<String> {
    let secs = crate::workflows::epoch_secs(until)?;
    let of_day = secs.rem_euclid(86_400);
    Some(format!(
        "{:02}:{:02} UTC",
        of_day / 3_600,
        (of_day % 3_600) / 60
    ))
}

/// The badge for the holds still running at `now`, or `None` when there are
/// none. Holds that already expired are ignored even before their clear
/// event arrives.
pub fn hold_badge(holds: &[Hold], now_secs: i64) -> Option<HoldBadge> {
    let mut live: Vec<(i64, &Hold)> = holds
        .iter()
        .filter_map(|hold| {
            let until = crate::workflows::epoch_secs(&hold.until)?;
            (until > now_secs).then_some((until, hold))
        })
        .collect();
    live.sort_by_key(|(until, _)| *until);
    let (soonest, first) = *live.first()?;
    let in_words = countdown(soonest - now_secs);
    let clock = clock_utc(&first.until).unwrap_or_default();
    let provider: String = first
        .provider
        .chars()
        .filter(|c| !c.is_control())
        .take(32)
        .collect();
    let (label, title) = if live.len() == 1 {
        (
            format!("{provider} held, back in {in_words}"),
            format!("{provider} is on hold until {clock}. Runs on it wait or use a fallback."),
        )
    } else {
        (
            format!("{} holds, first back in {in_words}", live.len()),
            format!(
                "{} provider holds. The first ends at {clock}. Runs on them wait or use a fallback.",
                live.len()
            ),
        )
    };
    Some(HoldBadge { label, title })
}

/// A main turn parked in `waiting_model`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Waiting {
    pub task_id: String,
    pub until: String,
}

/// The waiting-model banner line: `Waiting for a model. Trying again in 4:07.`
pub fn waiting_line(waiting: &Waiting, now_secs: i64) -> String {
    match crate::workflows::epoch_secs(&waiting.until) {
        Some(until) => format!(
            "Waiting for a model. Trying again in {}.",
            countdown(until - now_secs)
        ),
        None => "Waiting for a model.".into(),
    }
}

/// An inline "earlier messages were summarized" row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CompactionRow {
    pub task_id: String,
    pub text: String,
    /// When the event arrived, as an RFC 3339 UTC stamp: the row sits before
    /// the first message made after it.
    pub at: String,
}

/// `2026-11-16T10:04:07Z` for Unix seconds (UTC).
pub fn rfc3339_utc(secs: i64) -> String {
    let days = secs.div_euclid(86_400);
    let of_day = secs.rem_euclid(86_400);
    // Civil from days (Howard Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        of_day / 3_600,
        (of_day % 3_600) / 60,
        of_day % 60
    )
}

/// What the orchestration events changed for the open conversation.
#[derive(Debug, Default)]
pub struct RunBoard {
    pub rows: Vec<RunRow>,
    pub loaded: bool,
    /// A handoff event arrived since the list was read.
    pub stale: bool,
    /// The list was asked for in this conversation.
    pub asked: bool,
    overlays: BTreeMap<String, Overlay>,
    pub holds: Vec<Hold>,
    pub waiting: Option<Waiting>,
    pub compactions: Vec<CompactionRow>,
    /// Handoff ledger rows as last read, so overlays can re-derive chips.
    ledger: Vec<Handoff>,
}

fn text<'a>(payload: &'a Value, key: &str) -> Option<&'a str> {
    payload
        .get(key)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
}

impl RunBoard {
    /// Take a fresh ledger read.
    pub fn set_ledger(&mut self, mut handoffs: Vec<Handoff>) {
        handoffs.truncate(MAX_RUNS);
        self.ledger = handoffs;
        self.loaded = true;
        self.rebuild();
    }

    fn rebuild(&mut self) {
        let overlays = &self.overlays;
        self.rows = self
            .ledger
            .iter()
            .map(|handoff| row_of(handoff, overlays.get(&handoff.child_task_id)))
            .collect();
    }

    fn overlay(&mut self, task: &str) -> &mut Overlay {
        if !self.overlays.contains_key(task)
            && self.overlays.len() >= MAX_OVERLAYS
            && let Some(oldest) = self.overlays.keys().next().cloned()
        {
            self.overlays.remove(&oldest);
        }
        self.overlays.entry(task.to_owned()).or_default()
    }

    /// Fold one stream event in. `conversation` is the open chat: events that
    /// name another conversation are ignored, and a handoff event of this one
    /// only marks the list stale. Returns whether anything drawn changed.
    pub fn apply_event(&mut self, kind: &str, payload: &Value, conversation: &str) -> bool {
        self.apply_event_at(kind, payload, conversation, crate::workflows::now_secs())
    }

    /// [`Self::apply_event`] with the clock passed in.
    pub fn apply_event_at(
        &mut self,
        kind: &str,
        payload: &Value,
        conversation: &str,
        now_secs: i64,
    ) -> bool {
        let here = text(payload, "conversation_id").is_none_or(|id| id == conversation);
        match kind {
            "handoff.queued" | "handoff.started" | "handoff.resumed" | "handoff.completed"
            | "handoff.failed"
                if here =>
            {
                if let Some(child) = text(payload, "child_task_id")
                    && kind != "handoff.started"
                {
                    self.overlays.remove(child);
                }
                self.stale = true;
                true
            }
            "task.waiting_model" if here => {
                let (Some(task), Some(until)) = (text(payload, "task_id"), text(payload, "until"))
                else {
                    return false;
                };
                let task = clean_id(task);
                let until: String = until.chars().take(40).collect();
                if self.ledger.iter().any(|h| h.child_task_id == task) {
                    self.overlay(&task).model_until = Some(until);
                    self.rebuild();
                } else {
                    self.waiting = Some(Waiting {
                        task_id: task,
                        until,
                    });
                }
                true
            }
            "turn.step" if here => {
                let Some(task) = text(payload, "task_id").map(clean_id) else {
                    return false;
                };
                let mut changed = false;
                if self.waiting.as_ref().is_some_and(|w| w.task_id == task) {
                    self.waiting = None;
                    changed = true;
                }
                if self.ledger.iter().any(|h| h.child_task_id == task) {
                    let phase = text(payload, "phase").unwrap_or("");
                    let overlay = self.overlay(&task);
                    let before = overlay.clone();
                    match phase {
                        "waiting_approval" => overlay.approval = true,
                        _ => {
                            overlay.approval = false;
                            overlay.model_until = None;
                        }
                    }
                    if *overlay != before {
                        self.rebuild();
                        changed = true;
                    }
                }
                changed
            }
            "turn.completed" | "task.failed" | "task.cancelled" | "task.completed" if here => {
                let cleared = self.waiting.take().is_some();
                if let Some(task) = text(payload, "task_id") {
                    self.overlays.remove(task);
                }
                cleared
            }
            "turn.compacted" if here => {
                let task = text(payload, "task_id").map(clean_id).unwrap_or_default();
                let reason = text(payload, "reason").unwrap_or("");
                let count = |key: &str| payload.get(key).and_then(Value::as_u64);
                let sizes = match (
                    count("prompt_tokens_est_before"),
                    count("prompt_tokens_est_after"),
                ) {
                    (Some(before), Some(after)) => {
                        format!(" ({before} to {after} estimated tokens)")
                    }
                    _ => String::new(),
                };
                let why = match reason {
                    "context_too_long" => "the conversation grew past the model's limit",
                    "rolling" | "window" | "threshold" => "it was nearing the model's limit",
                    _ => "to keep the conversation going",
                };
                self.compactions.push(CompactionRow {
                    task_id: task,
                    text: format!("Earlier messages were summarized because {why}{sizes}."),
                    at: rfc3339_utc(now_secs),
                });
                if self.compactions.len() > MAX_COMPACTIONS {
                    self.compactions.remove(0);
                }
                true
            }
            "provider.hold_set" => {
                let (Some(provider), Some(until)) =
                    (text(payload, "provider"), text(payload, "until"))
                else {
                    return false;
                };
                let hold = Hold {
                    provider: provider.chars().take(48).collect(),
                    account_id: text(payload, "account_id")
                        .unwrap_or("")
                        .chars()
                        .take(64)
                        .collect(),
                    model: text(payload, "model")
                        .unwrap_or("")
                        .chars()
                        .take(96)
                        .collect(),
                    kind: text(payload, "kind")
                        .unwrap_or("")
                        .chars()
                        .take(32)
                        .collect(),
                    until: until.chars().take(40).collect(),
                };
                self.holds.retain(|h| {
                    !(h.provider == hold.provider
                        && h.account_id == hold.account_id
                        && h.model == hold.model)
                });
                self.holds.push(hold);
                if self.holds.len() > MAX_HOLDS {
                    self.holds.remove(0);
                }
                true
            }
            "provider.hold_cleared" => {
                let provider = text(payload, "provider").unwrap_or("");
                let account = text(payload, "account_id").unwrap_or("");
                let model = text(payload, "model");
                let before = self.holds.len();
                self.holds.retain(|h| {
                    !(h.provider == provider
                        && h.account_id == account
                        && model.is_none_or(|m| h.model == m))
                });
                self.holds.len() != before
            }
            _ => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn handoff(id: &str, parent: &str, child: &str, bot: &str, status: &str) -> Handoff {
        Handoff {
            id: id.into(),
            parent_task_id: parent.into(),
            child_task_id: child.into(),
            to_bot: bot.into(),
            status: status.into(),
            ..Handoff::default()
        }
    }

    fn rows(handoffs: &[Handoff]) -> Vec<RunRow> {
        handoffs.iter().map(|h| row_of(h, None)).collect()
    }

    #[test]
    fn every_ledger_status_has_its_chip() {
        let mut h = handoff("h", "p", "c", "analyst", "queued");
        assert_eq!(chip_for(&h, None), Chip::Queued);
        h.status = "running".into();
        assert_eq!(chip_for(&h, None), Chip::Running);
        h.status = "failed_resumable".into();
        assert_eq!(chip_for(&h, None), Chip::FailedResumable);
        h.status = "failed".into();
        assert_eq!(chip_for(&h, None), Chip::Failed);
        h.status = "completed".into();
        assert_eq!(chip_for(&h, None), Chip::Completed);
        h.status = "resumed".into();
        h.route = Some(json!({"selection": {"provider": "anthropic", "model": "claude-sonnet"}}));
        assert_eq!(
            chip_for(&h, None),
            Chip::ResumedOn("anthropic/claude-sonnet".into())
        );
        assert_eq!(
            chip_for(&h, None).label(),
            "Resumed on anthropic/claude-sonnet"
        );
        // The default route is shown by its model alone.
        h.route = Some(json!({"selection": {"provider": "default", "model": "test"}}));
        assert_eq!(chip_for(&h, None), Chip::ResumedOn("test".into()));
        h.status = "something-new".into();
        assert_eq!(chip_for(&h, None), Chip::Queued);
    }

    #[test]
    fn waiting_overlays_only_show_on_a_run_that_is_still_going() {
        let mut h = handoff("h", "p", "c", "analyst", "running");
        let approval = Overlay {
            approval: true,
            model_until: None,
        };
        let parked = Overlay {
            approval: false,
            model_until: Some("2026-11-16T10:00:00Z".into()),
        };
        assert_eq!(chip_for(&h, Some(&approval)), Chip::WaitingApproval);
        assert_eq!(chip_for(&h, Some(&parked)), Chip::WaitingModel);
        h.status = "failed".into();
        assert_eq!(chip_for(&h, Some(&parked)), Chip::Failed);
        h.status = "completed".into();
        assert_eq!(chip_for(&h, Some(&approval)), Chip::Completed);
    }

    #[test]
    fn reason_chips_use_known_words_only() {
        assert_eq!(reason_chips("rate_limited"), vec!["Rate limited"]);
        assert_eq!(
            reason_chips(
                "No model route could answer (rate_limited; quota_exhausted; rate_limited)"
            ),
            vec!["Rate limited", "Quota used up"]
        );
        // Provider text and secrets never become a chip.
        assert!(reason_chips("sk-secret-key leaked: {\"error\":\"boom\"}").is_empty());
        assert_eq!(reason_chips("a b c d e").len(), 0);
        let many = "rate_limited overloaded transport provider_down quota_exhausted";
        assert_eq!(reason_chips(many).len(), 4);
    }

    #[test]
    fn retry_is_offered_for_a_resumable_run_without_unknown_effects() {
        let mut failed = handoff("h", "p", "c", "analyst", "failed_resumable");
        failed.outcome_code = Some("rate_limited".into());
        let row = row_of(&failed, None);
        assert!(row.can_retry());
        assert!(!row.can_stop());
        assert_eq!(row.reasons, vec!["Rate limited"]);
        assert!(row.retry_blocked_note().is_none());

        failed.effects_unknown = true;
        let row = row_of(&failed, None);
        assert!(
            !row.can_retry(),
            "an unknown effect is never replayed blindly"
        );
        assert!(
            row.retry_blocked_note()
                .unwrap()
                .contains("Check the result")
        );

        failed.status = "failed".into();
        failed.effects_unknown = false;
        let row = row_of(&failed, None);
        assert!(!row.can_retry());
        assert!(row.retry_blocked_note().unwrap().contains("retries"));

        let running = row_of(&handoff("h2", "p", "c2", "analyst", "running"), None);
        assert!(running.can_stop());
        assert!(!running.can_retry());
        assert!(!row_of(&handoff("h3", "p", "c3", "a", "completed"), None).can_stop());
    }

    #[test]
    fn six_parallel_specialists_collapse_with_a_count_and_failures_stay_apart() {
        // Newest first, as the server lists them.
        let mut list = Vec::new();
        for i in (0..4).rev() {
            list.push(handoff(
                &format!("r{i}"),
                "main-task",
                &format!("c{i}"),
                "analyst",
                "running",
            ));
        }
        list.push(handoff(
            "f2",
            "main-task",
            "cf2",
            "analyst",
            "failed_resumable",
        ));
        list.push(handoff(
            "f1",
            "main-task",
            "cf1",
            "analyst",
            "failed_resumable",
        ));
        let rows = rows(&list);
        let lines = tree(&rows, &HashSet::new());
        let groups: Vec<_> = lines
            .iter()
            .filter_map(|line| match line {
                TreeLine::Group { label, count, .. } => Some((label.clone(), *count)),
                _ => None,
            })
            .collect();
        assert_eq!(groups, vec![("4 running".to_owned(), 4)]);
        let runs = lines
            .iter()
            .filter(|l| matches!(l, TreeLine::Run { .. }))
            .count();
        assert_eq!(runs, 2, "collapsed runs are hidden; both failures stay");

        // Opening the group lists the runs below its header.
        let key = lines
            .iter()
            .find_map(|line| match line {
                TreeLine::Group { key, .. } => Some(key.clone()),
                _ => None,
            })
            .expect("a group line");
        let opened = tree(&rows, &HashSet::from([key]));
        assert_eq!(
            opened
                .iter()
                .filter(|l| matches!(l, TreeLine::Run { .. }))
                .count(),
            6
        );

        // Two siblings are below the threshold and stay as they are.
        let two = rows_two();
        assert!(
            tree(&two, &HashSet::new())
                .iter()
                .all(|l| matches!(l, TreeLine::Run { .. }))
        );
    }

    fn rows_two() -> Vec<RunRow> {
        rows(&[
            handoff("a", "p", "ca", "analyst", "running"),
            handoff("b", "p", "cb", "analyst", "running"),
        ])
    }

    #[test]
    fn children_hang_under_the_run_that_spawned_them() {
        let list = vec![
            handoff("grand", "c1", "c2", "maker", "running"),
            handoff("child", "main-task", "c1", "analyst", "running"),
        ];
        let lines = tree(&rows(&list), &HashSet::new());
        let depths: Vec<(String, u8)> = lines
            .iter()
            .filter_map(|l| match l {
                TreeLine::Run { row, depth } => Some((row.bot.clone(), *depth)),
                _ => None,
            })
            .collect();
        assert_eq!(depths, vec![("analyst".into(), 0), ("maker".into(), 1)]);
    }

    #[test]
    fn a_cycle_in_the_ledger_cannot_loop_the_tree() {
        let list = vec![
            handoff("a", "cb", "ca", "x", "running"),
            handoff("b", "ca", "cb", "y", "running"),
        ];
        // Neither is a root, so nothing draws, and nothing hangs.
        assert!(tree(&rows(&list), &HashSet::new()).is_empty());
    }

    #[test]
    fn counts_for_the_conversation_list() {
        let list = rows(&[
            handoff("a", "p", "ca", "x", "running"),
            handoff("b", "p", "cb", "y", "failed_resumable"),
            handoff("c", "p", "cc", "z", "completed"),
        ]);
        assert_eq!(active_count(&list), 1);
        assert_eq!(needs_attention(&list), 1);
    }

    #[test]
    fn handoff_events_mark_the_list_stale_once_for_this_conversation_only() {
        let mut board = RunBoard::default();
        let other = json!({"conversation_id": "dir:elsewhere", "child_task_id": "c"});
        assert!(!board.apply_event("handoff.started", &other, "dir:main"));
        assert!(!board.stale);
        let here = json!({"conversation_id": "dir:main", "child_task_id": "c"});
        assert!(board.apply_event("handoff.failed", &here, "dir:main"));
        assert!(board.stale);
    }

    #[test]
    fn a_waiting_child_shows_the_chip_until_it_steps_again() {
        let mut board = RunBoard::default();
        board.set_ledger(vec![handoff("h", "p", "c1", "analyst", "running")]);
        let parked = json!({"task_id": "c1", "conversation_id": "dir:main", "until": "2026-11-16T10:00:00Z"});
        board.apply_event("task.waiting_model", &parked, "dir:main");
        assert_eq!(board.rows[0].chip, Chip::WaitingModel);
        assert_eq!(board.rows[0].until.as_deref(), Some("2026-11-16T10:00:00Z"));

        let step = json!({"task_id": "c1", "conversation_id": "dir:main", "phase": "running"});
        board.apply_event("turn.step", &step, "dir:main");
        assert_eq!(board.rows[0].chip, Chip::Running);

        let ask =
            json!({"task_id": "c1", "conversation_id": "dir:main", "phase": "waiting_approval"});
        board.apply_event("turn.step", &ask, "dir:main");
        assert_eq!(board.rows[0].chip, Chip::WaitingApproval);

        // The ledger says it ended: the overlay is dropped with the event.
        let done = json!({"child_task_id": "c1", "conversation_id": "dir:main"});
        board.apply_event("handoff.completed", &done, "dir:main");
        board.set_ledger(vec![handoff("h", "p", "c1", "analyst", "completed")]);
        assert_eq!(board.rows[0].chip, Chip::Completed);
    }

    #[test]
    fn a_main_turn_parked_for_a_model_has_a_countdown_that_clears() {
        let mut board = RunBoard::default();
        let parked = json!({"task_id": "main-1", "conversation_id": "dir:main", "until": "2026-11-16T10:04:07Z"});
        assert!(board.apply_event("task.waiting_model", &parked, "dir:main"));
        let waiting = board.waiting.clone().unwrap();
        let until = crate::workflows::epoch_secs("2026-11-16T10:04:07Z").unwrap();
        assert_eq!(
            waiting_line(&waiting, until - 247),
            "Waiting for a model. Trying again in 4:07."
        );
        assert_eq!(
            waiting_line(&waiting, until + 5),
            "Waiting for a model. Trying again in any moment."
        );
        // Another conversation's park is not ours.
        let mut other = RunBoard::default();
        let elsewhere =
            json!({"task_id": "m", "conversation_id": "dir:x", "until": "2026-11-16T10:04:07Z"});
        assert!(!other.apply_event("task.waiting_model", &elsewhere, "dir:main"));
        assert!(other.waiting.is_none());
        // The next step of that task ends the wait.
        let step = json!({"task_id": "main-1", "conversation_id": "dir:main", "phase": "running"});
        assert!(board.apply_event("turn.step", &step, "dir:main"));
        assert!(board.waiting.is_none());
    }

    #[test]
    fn countdown_reads_as_a_clock() {
        assert_eq!(countdown(0), "any moment");
        assert_eq!(countdown(-5), "any moment");
        assert_eq!(countdown(9), "0:09");
        assert_eq!(countdown(247), "4:07");
        assert_eq!(countdown(3_720), "1h 02m");
    }

    #[test]
    fn the_hold_badge_names_the_reset_time_and_ignores_expired_holds() {
        let mut board = RunBoard::default();
        let set = json!({"provider": "anthropic", "account_id": "a1", "model": "m", "kind": "rate_limited", "until": "2026-11-16T14:05:00Z"});
        assert!(board.apply_event("provider.hold_set", &set, "dir:main"));
        let now = crate::workflows::epoch_secs("2026-11-16T14:00:00Z").unwrap();
        let badge = hold_badge(&board.holds, now).unwrap();
        assert_eq!(badge.label, "anthropic held, back in 5:00");
        assert!(badge.title.contains("14:05 UTC"));
        assert!(hold_badge(&board.holds, now + 600).is_none(), "expired");

        let second = json!({"provider": "openai", "account_id": "k", "model": "m2", "kind": "quota_exhausted", "until": "2026-11-16T15:00:00Z"});
        board.apply_event("provider.hold_set", &second, "dir:main");
        assert_eq!(
            hold_badge(&board.holds, now).unwrap().label,
            "2 holds, first back in 5:00"
        );

        // Setting the same hold again replaces it; clearing removes it.
        board.apply_event("provider.hold_set", &set, "dir:main");
        assert_eq!(board.holds.len(), 2);
        let clear = json!({"provider": "anthropic", "account_id": "a1", "model": "m"});
        assert!(board.apply_event("provider.hold_cleared", &clear, "dir:main"));
        assert_eq!(board.holds.len(), 1);
    }

    #[test]
    fn stamps_round_trip_through_the_clock_helpers() {
        for stamp in [
            "2026-11-16T10:04:07Z",
            "2024-02-29T23:59:59Z",
            "1970-01-01T00:00:00Z",
            "2000-03-01T00:00:00Z",
        ] {
            let secs = crate::workflows::epoch_secs(stamp).unwrap();
            assert_eq!(rfc3339_utc(secs), stamp);
        }
    }

    #[test]
    fn compaction_rows_carry_counts_never_the_summary() {
        let mut board = RunBoard::default();
        let event = json!({
            "task_id": "t", "conversation_id": "dir:main", "reason": "context_too_long",
            "prompt_tokens_est_before": 90000, "prompt_tokens_est_after": 20000,
            "summary": "SECRET SUMMARY BODY"
        });
        assert!(board.apply_event_at("turn.compacted", &event, "dir:main", 1_700_000_000));
        let row = &board.compactions[0];
        assert_eq!(row.at, "2023-11-14T22:13:20Z");
        assert!(row.text.contains("90000 to 20000"));
        assert!(!row.text.contains("SECRET"));
        for _ in 0..40 {
            board.apply_event("turn.compacted", &event, "dir:main");
        }
        assert!(board.compactions.len() <= MAX_COMPACTIONS);
    }

    #[test]
    fn ledger_text_is_cleaned_before_it_is_drawn() {
        let mut h = handoff("h<b>", "p", "c\u{7}1", "ana lyst", "running");
        h.route = Some(json!({"selection": {"provider": "x\u{7}y", "model": "m"}}));
        let row = row_of(&h, None);
        assert_eq!(row.id, "hb");
        assert_eq!(row.child_task_id, "c1");
        assert_eq!(row.bot, "analyst");
        assert_eq!(row.route.as_deref(), Some("xy/m"));
    }

    #[test]
    fn the_ledger_is_bounded() {
        let mut board = RunBoard::default();
        let many: Vec<Handoff> = (0..300)
            .map(|i| handoff(&format!("h{i}"), "p", &format!("c{i}"), "x", "running"))
            .collect();
        board.set_ledger(many);
        assert_eq!(board.rows.len(), MAX_RUNS);
    }
}
