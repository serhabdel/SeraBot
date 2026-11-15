//! The Workflows tab model (0.29.0, Wave A4): parsing responses from
//! `GET /v1/routines`, `GET /v1/monitoring/sources`, and `GET /v1/workflows/active`.
//!
//! Each endpoint is optional on older servers (404 means unsupported for that
//! panel). Strings are bounded and control characters dropped before display.

use serde_json::Value;

const MAX_NAME: usize = 120;
const MAX_TIME: usize = 64;
const MAX_GOAL: usize = 500;
const MAX_LABEL: usize = 120;
const MAX_PROMPT: usize = 2000;
const MAX_STEPS: usize = 50;
const MAX_ERROR: usize = 300;
const MAX_URL: usize = 512;
const MAX_KIND: usize = 64;
const MAX_CONVERSATION: usize = 128;
const MAX_ROUTINES: usize = 64;
const MAX_SOURCES: usize = 200;
const MAX_ACTIVE: usize = 200;

/// One scheduled routine row from `GET /v1/routines`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Routine {
    pub name: String,
    pub enabled: bool,
    pub next_run_at: String,
    pub last_run_for: Option<String>,
    /// Daily time (`HH:MM`) when present in server config.
    pub time: Option<String>,
    pub timezone: Option<String>,
    /// Optional display label from `config.label`.
    pub label: Option<String>,
    /// Optional custom instructions/prompt from `config.prompt`.
    pub prompt: Option<String>,
}

/// Last run snapshot nested under a monitoring source.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SourceLastRun {
    pub ok: bool,
    pub error: Option<String>,
    pub items_seen: i64,
    pub started_at: Option<String>,
    pub finished_at: Option<String>,
}

/// One configured watcher source from `GET /v1/monitoring/sources`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MonitoringSource {
    pub name: String,
    pub kind: String,
    pub enabled: bool,
    pub url: Option<String>,
    /// A feed this user added, as opposed to an admin-defined one (0.34.0).
    pub own: bool,
    pub last_run: Option<SourceLastRun>,
}

/// One step in an active multi-step workflow plan.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorkflowPlanStep {
    pub title: String,
    pub done: bool,
}

/// One incomplete multi-step plan from `GET /v1/workflows/active`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ActiveWorkflow {
    pub conversation_id: String,
    pub goal: String,
    pub done: usize,
    pub total: usize,
    pub updated_at: Option<String>,
    pub items: Vec<WorkflowPlanStep>,
}

/// All three Wave A reads in one worker round-trip.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct WorkflowsSnapshot {
    pub routines: Vec<Routine>,
    pub sources: Vec<MonitoringSource>,
    pub active: Vec<ActiveWorkflow>,
    pub routines_supported: bool,
    pub sources_supported: bool,
    pub active_supported: bool,
}

fn clean(value: &Value, key: &str, max: usize) -> Option<String> {
    let text: String = value
        .get(key)?
        .as_str()?
        .chars()
        .filter(|c| !c.is_control())
        .take(max)
        .collect();
    let text = text.trim().to_owned();
    (!text.is_empty()).then_some(text)
}

fn opt_str(value: &Value, key: &str, max: usize) -> Option<String> {
    value.get(key).and_then(|v| {
        if v.is_null() {
            None
        } else {
            clean(value, key, max)
        }
    })
}

fn parse_routine(row: &Value) -> Option<Routine> {
    let name = clean(row, "name", MAX_NAME)?;
    let enabled = row.get("enabled")?.as_bool()?;
    let next_run_at = clean(row, "next_run_at", MAX_TIME).unwrap_or_default();
    let config = row.get("config").unwrap_or(&Value::Null);
    Some(Routine {
        name,
        enabled,
        next_run_at,
        last_run_for: opt_str(row, "last_run_for", MAX_TIME),
        time: config
            .get("time")
            .and_then(|v| v.as_str())
            .map(|s| s.chars().take(16).collect()),
        timezone: config
            .get("timezone")
            .or_else(|| config.get("tz"))
            .and_then(|v| v.as_str())
            .map(|s| s.chars().take(64).collect()),
        label: opt_str(config, "label", MAX_LABEL),
        prompt: opt_str(config, "prompt", MAX_PROMPT),
    })
}

fn parse_last_run(row: &Value) -> Option<SourceLastRun> {
    let ok = row.get("ok")?.as_bool()?;
    let items_seen = row.get("items_seen").and_then(|v| v.as_i64()).unwrap_or(0);
    Some(SourceLastRun {
        ok,
        error: opt_str(row, "error", MAX_ERROR),
        items_seen,
        started_at: opt_str(row, "started_at", MAX_TIME),
        finished_at: opt_str(row, "finished_at", MAX_TIME),
    })
}

fn parse_source(row: &Value) -> Option<MonitoringSource> {
    let name = clean(row, "name", MAX_NAME)?;
    let kind = clean(row, "kind", MAX_KIND).unwrap_or_else(|| "unknown".into());
    let enabled = row.get("enabled").and_then(|v| v.as_bool()).unwrap_or(true);
    let last_run = row.get("last_run").and_then(parse_last_run);
    Some(MonitoringSource {
        name,
        kind,
        enabled,
        url: opt_str(row, "url", MAX_URL),
        own: row.get("own").and_then(|v| v.as_bool()).unwrap_or(false),
        last_run,
    })
}

fn parse_plan_step(row: &Value) -> Option<WorkflowPlanStep> {
    let title = clean(row, "title", MAX_GOAL)
        .or_else(|| clean(row, "text", MAX_GOAL))
        .or_else(|| clean(row, "label", MAX_GOAL))?;
    let done = row
        .get("done")
        .and_then(|v| v.as_bool())
        .unwrap_or_else(|| {
            row.get("status")
                .and_then(|v| v.as_str())
                .is_some_and(|s| s.eq_ignore_ascii_case("done"))
        });
    Some(WorkflowPlanStep { title, done })
}

fn parse_active(row: &Value) -> Option<ActiveWorkflow> {
    let conversation_id = clean(row, "conversation_id", MAX_CONVERSATION)?;
    let goal = clean(row, "goal", MAX_GOAL).unwrap_or_else(|| "(no goal)".into());
    let done = row.get("done").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
    let total = row.get("total").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
    let items: Vec<WorkflowPlanStep> = row
        .get("items")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .take(MAX_STEPS)
                .filter_map(parse_plan_step)
                .collect()
        })
        .unwrap_or_default();
    Some(ActiveWorkflow {
        conversation_id,
        goal,
        done,
        total,
        updated_at: opt_str(row, "updated_at", MAX_TIME),
        items,
    })
}

pub fn parse_routines(value: &Value) -> Option<Vec<Routine>> {
    let rows = value.get("routines")?.as_array()?;
    if rows.len() > MAX_ROUTINES {
        return None;
    }
    Some(rows.iter().filter_map(parse_routine).collect())
}

pub fn parse_sources(value: &Value) -> Option<Vec<MonitoringSource>> {
    let rows = value.get("sources")?.as_array()?;
    if rows.len() > MAX_SOURCES {
        return None;
    }
    Some(rows.iter().filter_map(parse_source).collect())
}

pub fn parse_active_workflows(value: &Value) -> Option<Vec<ActiveWorkflow>> {
    let rows = value.get("workflows")?.as_array()?;
    if rows.len() > MAX_ACTIVE {
        return None;
    }
    Some(rows.iter().filter_map(parse_active).collect())
}

/// Morning brief row and `watch_{n}` rows for the merged schedule panel (B2).
pub fn split_schedule_routines(routines: &[Routine]) -> (Option<Routine>, Vec<Routine>) {
    let mut brief = None;
    let mut watch = Vec::new();
    for routine in routines {
        if routine.name == "morning_brief" {
            brief = Some(routine.clone());
        } else if routine.name.starts_with("watch_") {
            watch.push(routine.clone());
        }
    }
    watch.sort_by(|left, right| left.name.cmp(&right.name));
    (brief, watch)
}

/// Placeholder watch slot names when the server has not created rows yet.
pub fn default_watch_slot_names() -> Vec<String> {
    (0..3).map(|index| format!("watch_{index}")).collect()
}

/// Every routine name a member can schedule: the morning brief and the watch
/// slots. Matches the server's limit of twelve watch slots.
pub fn member_routine_slots() -> Vec<String> {
    std::iter::once("morning_brief".to_owned())
        .chain((0..12).map(|index| format!("watch_{index}")))
        .collect()
}

/// Short label for the next run timestamp (shown as-is after trim).
pub fn format_next_run(raw: &str) -> String {
    let flat: String = raw
        .chars()
        .filter(|c| !c.is_control())
        .take(MAX_TIME)
        .collect();
    let flat = flat.trim();
    if flat.is_empty() {
        "—".into()
    } else {
        flat.to_owned()
    }
}

/// Seconds since the Unix epoch for an RFC 3339 UTC stamp such as
/// `2026-11-09T10:00:00Z` (fractions and a `+00:00` offset are accepted).
/// Other offsets are not guessed at: they return `None`.
pub fn epoch_secs(raw: &str) -> Option<i64> {
    let raw = raw.trim();
    let date = raw.get(..10)?;
    let time = raw.get(11..19)?;
    if raw.get(10..11)? != "T" && raw.get(10..11)? != " " {
        return None;
    }
    let rest = raw.get(19..)?;
    let rest = rest.trim_start_matches(|c: char| c == '.' || c.is_ascii_digit());
    if !(rest == "Z" || rest == "z" || rest == "+00:00" || rest.is_empty()) {
        return None;
    }
    let y: i64 = date.get(..4)?.parse().ok()?;
    let m: i64 = date.get(5..7)?.parse().ok()?;
    let d: i64 = date.get(8..10)?.parse().ok()?;
    let h: i64 = time.get(..2)?.parse().ok()?;
    let mi: i64 = time.get(3..5)?.parse().ok()?;
    let se: i64 = time.get(6..8)?.parse().ok()?;
    if !(1..=12).contains(&m) || !(1..=31).contains(&d) || h > 23 || mi > 59 || se > 60 {
        return None;
    }
    // Days from civil (Howard Hinnant's algorithm).
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    Some(days * 86_400 + h * 3_600 + mi * 60 + se)
}

fn span(secs: i64) -> String {
    match secs {
        0..=44 => "a moment".into(),
        45..=3_599 => format!("{}m", ((secs + 30) / 60).max(1)),
        3_600..=86_399 => format!("{}h", secs / 3_600),
        _ => format!("{}d", secs / 86_400),
    }
}

/// "5m ago" or "in 2h" for a stamp, given the current time. A stamp that
/// cannot be read is shown trimmed as-is, so nothing is hidden.
pub fn relative_time(raw: &str, now_secs: i64) -> String {
    let Some(at) = epoch_secs(raw) else {
        return format_next_run(raw);
    };
    let diff = at - now_secs;
    if diff.abs() < 45 {
        return "just now".into();
    }
    if diff < 0 {
        format!("{} ago", span(-diff))
    } else {
        format!("in {}", span(diff))
    }
}

/// The current time in whole seconds since the Unix epoch.
pub fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// The host of a feed URL for a short subtitle; `None` when it is not a URL.
pub fn source_host(url: &str) -> Option<String> {
    let parsed = url::Url::parse(url.trim()).ok()?;
    parsed.host_str().map(str::to_owned)
}

/// A conversation id cut short for display, ending in an ellipsis.
pub fn short_id(id: &str, max: usize) -> String {
    if id.chars().count() <= max {
        id.to_owned()
    } else {
        let head: String = id.chars().take(max.saturating_sub(1)).collect();
        format!("{head}\u{2026}")
    }
}

/// The label of an active plan's conversation: its name when known, else a
/// shortened id.
pub fn plan_conversation_label(name: Option<&str>, id: &str) -> String {
    match name.map(str::trim).filter(|n| !n.is_empty()) {
        Some(name) => name.to_owned(),
        None => short_id(id, 14),
    }
}

/// Progress text for an active plan row.
pub fn plan_progress(done: usize, total: usize) -> String {
    if total == 0 {
        "0 steps".into()
    } else {
        format!("{done}/{total}")
    }
}

/// True when a watcher/RSS feed URL parses as HTTP/HTTPS with a non-empty host.
#[allow(dead_code)]
pub fn is_valid_feed_url(raw: &str) -> bool {
    let Ok(parsed) = url::Url::parse(raw.trim()) else {
        return false;
    };
    matches!(parsed.scheme(), "http" | "https") && parsed.has_host()
}

/// Case-insensitive substring filter helper for the Workflows screen.
#[allow(dead_code)]
pub fn matches_workflow_filter(needle: &str, haystacks: &[&str]) -> bool {
    let needle = needle.trim().to_ascii_lowercase();
    if needle.is_empty() {
        return true;
    }
    haystacks
        .iter()
        .any(|text| text.to_ascii_lowercase().contains(&needle))
}

/// Kind of stage in a visual workflow node graph.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WorkflowNodeKind {
    Trigger,
    Source,
    Bot,
    Gate,
    Output,
    Step,
}

impl WorkflowNodeKind {
    /// Human-readable kind label for node headers.
    pub fn label(self) -> &'static str {
        match self {
            Self::Trigger => "Trigger",
            Self::Source => "Source",
            Self::Bot => "Bot",
            Self::Gate => "Gate",
            Self::Output => "Output",
            Self::Step => "Step",
        }
    }

    /// CSS modifier suffix (`wf-node--{modifier}`).
    pub fn css_modifier(self) -> &'static str {
        match self {
            Self::Trigger => "trigger",
            Self::Source => "source",
            Self::Bot => "bot",
            Self::Gate => "gate",
            Self::Output => "output",
            Self::Step => "step",
        }
    }
}

/// Live execution or readiness state of a workflow node.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WorkflowNodeStatus {
    Ready,
    Running,
    Done,
    Paused,
    Error,
}

impl WorkflowNodeStatus {
    /// Short status pill label.
    pub fn label(self) -> &'static str {
        match self {
            Self::Ready => "Ready",
            Self::Running => "Running",
            Self::Done => "Done",
            Self::Paused => "Paused",
            Self::Error => "Error",
        }
    }

    /// CSS modifier suffix (`wf-node-status--{modifier}`).
    pub fn css_modifier(self) -> &'static str {
        match self {
            Self::Ready => "ready",
            Self::Running => "running",
            Self::Done => "done",
            Self::Paused => "paused",
            Self::Error => "error",
        }
    }
}

/// One node card in the horizontal visual workflow canvas.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorkflowNode {
    pub id: String,
    pub kind: WorkflowNodeKind,
    pub title: String,
    pub subtitle: String,
    pub badge: String,
    pub status: WorkflowNodeStatus,
}

/// Complete visual node flow graph for a routine, source, or active plan.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorkflowGraph {
    pub title: String,
    pub subtitle: String,
    pub nodes: Vec<WorkflowNode>,
    pub active: bool,
}

/// Build the visual node graph for a scheduled routine (`morning_brief` or `watch_{n}`).
pub fn build_routine_graph(
    routine: &Routine,
    sources: &[MonitoringSource],
    timezone_fallback: &str,
) -> WorkflowGraph {
    let title = routine
        .label
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or(&routine.name)
        .to_owned();
    let time = routine
        .time
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("07:00");
    let tz = routine
        .timezone
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or(timezone_fallback);
    let schedule_text = if tz.is_empty() {
        time.to_owned()
    } else {
        format!("{time} {tz}")
    };
    let base_status = if routine.enabled {
        WorkflowNodeStatus::Ready
    } else {
        WorkflowNodeStatus::Paused
    };
    let enabled_sources = sources.iter().filter(|s| s.enabled).count();
    let total_sources = sources.len();
    let any_enabled_err = sources
        .iter()
        .any(|s| s.enabled && s.last_run.as_ref().is_some_and(|r| !r.ok));

    let nodes = if routine.name == "morning_brief" {
        let prompt_sub = routine
            .prompt
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(|s| short_id(s, 48))
            .unwrap_or_else(|| "Prompt / Default brief".into());
        vec![
            WorkflowNode {
                id: "trigger".into(),
                kind: WorkflowNodeKind::Trigger,
                title: "Daily Cron".into(),
                subtitle: schedule_text.clone(),
                badge: "Cron".into(),
                status: base_status,
            },
            WorkflowNode {
                id: "source".into(),
                kind: WorkflowNodeKind::Source,
                title: "Active Feeds".into(),
                subtitle: format!("{enabled_sources} sources"),
                badge: "Feeds".into(),
                status: if routine.enabled && enabled_sources == 0 {
                    WorkflowNodeStatus::Paused
                } else {
                    base_status
                },
            },
            WorkflowNode {
                id: "bot".into(),
                kind: WorkflowNodeKind::Bot,
                title: "Brief Digest Synthesizer".into(),
                subtitle: prompt_sub,
                badge: "Agent".into(),
                status: base_status,
            },
            WorkflowNode {
                id: "output".into(),
                kind: WorkflowNodeKind::Output,
                title: "Delivery".into(),
                subtitle: "Chat + Telegram".into(),
                badge: "Output".into(),
                status: base_status,
            },
        ]
    } else {
        let prompt_sub = routine
            .prompt
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(|s| short_id(s, 48))
            .unwrap_or_else(|| "Change detector".into());
        let source_status = if !routine.enabled {
            WorkflowNodeStatus::Paused
        } else if any_enabled_err {
            WorkflowNodeStatus::Error
        } else {
            WorkflowNodeStatus::Ready
        };
        vec![
            WorkflowNode {
                id: "trigger".into(),
                kind: WorkflowNodeKind::Trigger,
                title: "Watch Schedule".into(),
                subtitle: schedule_text.clone(),
                badge: "Cron".into(),
                status: base_status,
            },
            WorkflowNode {
                id: "source".into(),
                kind: WorkflowNodeKind::Source,
                title: "Monitored Feeds".into(),
                subtitle: format!("{enabled_sources}/{total_sources} feeds"),
                badge: "RSS".into(),
                status: source_status,
            },
            WorkflowNode {
                id: "bot".into(),
                kind: WorkflowNodeKind::Bot,
                title: "Watcher Summarizer".into(),
                subtitle: prompt_sub,
                badge: "Agent".into(),
                status: base_status,
            },
            WorkflowNode {
                id: "gate".into(),
                kind: WorkflowNodeKind::Gate,
                title: "Policy & Domain Scan".into(),
                subtitle: "Allowlist + blocklist".into(),
                badge: "Gate".into(),
                status: base_status,
            },
            WorkflowNode {
                id: "output".into(),
                kind: WorkflowNodeKind::Output,
                title: "Digest & Alerts".into(),
                subtitle: "Chat + Telegram".into(),
                badge: "Output".into(),
                status: base_status,
            },
        ]
    };

    let state_label = if routine.enabled { "Active" } else { "Paused" };
    WorkflowGraph {
        title,
        subtitle: format!("{state_label} · {schedule_text}"),
        nodes,
        active: routine.enabled,
    }
}

/// Build the visual node graph for a single monitoring source feed.
pub fn build_source_graph(source: &MonitoringSource) -> WorkflowGraph {
    let host_or_url = source
        .url
        .as_deref()
        .and_then(|u| source_host(u).or_else(|| Some(short_id(u.trim(), 40))))
        .unwrap_or_else(|| "No URL".into());
    let items_seen = source
        .last_run
        .as_ref()
        .map(|r| r.items_seen.max(0))
        .unwrap_or(0);
    let source_status = if !source.enabled {
        WorkflowNodeStatus::Paused
    } else if source.last_run.as_ref().is_some_and(|r| !r.ok) {
        WorkflowNodeStatus::Error
    } else if source.last_run.as_ref().is_some_and(|r| r.ok) {
        WorkflowNodeStatus::Done
    } else {
        WorkflowNodeStatus::Ready
    };
    let ready_or_paused = if source.enabled {
        WorkflowNodeStatus::Ready
    } else {
        WorkflowNodeStatus::Paused
    };

    WorkflowGraph {
        title: source.name.clone(),
        subtitle: format!("{} · {}", source.kind, host_or_url),
        active: source.enabled,
        nodes: vec![
            WorkflowNode {
                id: "trigger".into(),
                kind: WorkflowNodeKind::Trigger,
                title: "Scheduled Poll".into(),
                subtitle: "Watcher interval".into(),
                badge: "Poll".into(),
                status: ready_or_paused,
            },
            WorkflowNode {
                id: "source".into(),
                kind: WorkflowNodeKind::Source,
                title: format!("{} feed", source.kind),
                subtitle: host_or_url,
                badge: source.kind.clone(),
                status: source_status,
            },
            WorkflowNode {
                id: "bot".into(),
                kind: WorkflowNodeKind::Bot,
                title: "Diff & Item Extractor".into(),
                subtitle: format!("{items_seen} seen"),
                badge: "Extract".into(),
                status: source_status,
            },
            WorkflowNode {
                id: "output".into(),
                kind: WorkflowNodeKind::Output,
                title: "Opportunity Digest".into(),
                subtitle: "Brief + Alerts".into(),
                badge: "Output".into(),
                status: ready_or_paused,
            },
        ],
    }
}

/// Build the visual node graph for an active multi-step chat plan.
pub fn build_plan_graph(plan: &ActiveWorkflow) -> WorkflowGraph {
    let total = if plan.items.is_empty() {
        plan.total
    } else {
        plan.items.len()
    };
    let done = if plan.items.is_empty() {
        plan.done
    } else {
        plan.items.iter().filter(|s| s.done).count()
    };
    let all_done = total > 0 && done >= total;
    let mut nodes = Vec::with_capacity(plan.items.len() + 2);
    nodes.push(WorkflowNode {
        id: "trigger".into(),
        kind: WorkflowNodeKind::Trigger,
        title: "Chat Goal".into(),
        subtitle: short_id(&plan.goal, 48),
        badge: "Goal".into(),
        status: WorkflowNodeStatus::Done,
    });
    for (idx, step) in plan.items.iter().enumerate() {
        nodes.push(WorkflowNode {
            id: format!("step-{}", idx + 1),
            kind: WorkflowNodeKind::Step,
            title: format!("Step {}", idx + 1),
            subtitle: step.title.clone(),
            badge: if step.done {
                "Done".into()
            } else {
                "Pending".into()
            },
            status: if step.done {
                WorkflowNodeStatus::Done
            } else {
                WorkflowNodeStatus::Running
            },
        });
    }
    nodes.push(WorkflowNode {
        id: "output".into(),
        kind: WorkflowNodeKind::Output,
        title: if all_done {
            "Plan Complete".into()
        } else {
            "Next Action".into()
        },
        subtitle: if all_done {
            "All steps finished".into()
        } else {
            format!("{} remaining", total.saturating_sub(done))
        },
        badge: if all_done {
            "Complete".into()
        } else {
            "Active".into()
        },
        status: if all_done {
            WorkflowNodeStatus::Done
        } else {
            WorkflowNodeStatus::Ready
        },
    });
    WorkflowGraph {
        title: plan.goal.clone(),
        subtitle: plan_progress(done, total),
        nodes,
        active: !all_done,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parses_documented_routine_shape() {
        let rows = parse_routines(&json!({
            "routines": [{
                "name": "morning_brief",
                "enabled": true,
                "next_run_at": "2026-11-10T06:30:00Z",
                "last_run_for": "2026-11-09",
                "config": { "time": "07:30", "timezone": "Europe/Rome" }
            }]
        }))
        .unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].time.as_deref(), Some("07:30"));
    }

    #[test]
    fn parses_monitoring_and_active_shapes() {
        let sources = parse_sources(&json!({
            "sources": [{
                "name": "portal",
                "kind": "rss",
                "enabled": true,
                "url": "https://example.test/feed",
                "last_run": {
                    "ok": false,
                    "error": "timeout",
                    "items_seen": 0,
                    "started_at": "2026-11-09T10:00:00Z"
                }
            }]
        }))
        .unwrap();
        assert!(!sources[0].last_run.as_ref().unwrap().ok);

        let active = parse_active_workflows(&json!({
            "workflows": [{
                "conversation_id": "dir:main",
                "goal": "Ship",
                "done": 1,
                "total": 3,
                "updated_at": "2026-11-09T12:00:00Z"
            }]
        }))
        .unwrap();
        assert_eq!(active[0].done, 1);
    }

    #[test]
    fn epoch_parses_utc_stamps() {
        assert_eq!(epoch_secs("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(epoch_secs("2026-11-01T00:00:00Z"), Some(20_758 * 86_400));
        assert_eq!(
            epoch_secs("2026-11-01T10:30:15.250Z"),
            Some(20_758 * 86_400 + 37_815)
        );
        assert_eq!(
            epoch_secs("2026-11-01T00:00:00+00:00"),
            Some(20_758 * 86_400)
        );
        assert_eq!(epoch_secs("2026-11-01T00:00:00+02:00"), None);
        assert_eq!(epoch_secs("nope"), None);
        assert_eq!(epoch_secs("2026-13-01T00:00:00Z"), None);
    }

    #[test]
    fn relative_times_read_naturally() {
        let now = epoch_secs("2026-11-10T12:00:00Z").unwrap();
        assert_eq!(relative_time("2026-11-10T11:55:00Z", now), "5m ago");
        assert_eq!(relative_time("2026-11-10T09:00:00Z", now), "3h ago");
        assert_eq!(relative_time("2026-11-08T12:00:00Z", now), "2d ago");
        assert_eq!(relative_time("2026-11-10T14:00:00Z", now), "in 2h");
        assert_eq!(relative_time("2026-11-10T12:00:20Z", now), "just now");
        assert_eq!(relative_time("garbage", now), "garbage");
        assert_eq!(relative_time("", now), "\u{2014}");
    }

    #[test]
    fn source_host_and_short_id() {
        assert_eq!(
            source_host("https://example.test/feed?k=1").as_deref(),
            Some("example.test")
        );
        assert_eq!(source_host("not a url"), None);
        assert_eq!(short_id("abc", 14), "abc");
        assert_eq!(
            short_id("0123456789abcdefghij", 14),
            "0123456789abc\u{2026}"
        );
        assert_eq!(plan_conversation_label(Some("  Trip  "), "x"), "Trip");
        assert_eq!(plan_conversation_label(Some(" "), "dir:main"), "dir:main");
        assert_eq!(
            plan_conversation_label(None, "0123456789abcdefghij"),
            "0123456789abc\u{2026}"
        );
    }

    #[test]
    fn feed_url_and_filter_validation() {
        assert!(is_valid_feed_url("https://example.invalid/rss.xml"));
        assert!(is_valid_feed_url(" http://news.example.invalid/feed "));
        assert!(!is_valid_feed_url("ftp://example.invalid/feed"));
        assert!(!is_valid_feed_url("javascript:alert(1)"));
        assert!(!is_valid_feed_url("not a url"));
        assert!(matches_workflow_filter("", &["morning_brief"]));
        assert!(matches_workflow_filter("BRIEF", &["morning_brief", "rss"]));
        assert!(!matches_workflow_filter(
            "podcast",
            &["morning_brief", "rss"]
        ));
    }

    #[test]
    fn parses_routine_label_and_prompt_with_backwards_compat() {
        let rows = parse_routines(&json!({
            "routines": [
                {
                    "name": "morning_brief",
                    "enabled": true,
                    "next_run_at": "2026-11-10T06:30:00Z",
                    "config": {
                        "time": "07:30",
                        "timezone": "Europe/Paris",
                        "label": "Morning Executive Brief",
                        "prompt": "Summarize AI policy and tenders."
                    }
                },
                {
                    "name": "watch_0",
                    "enabled": false,
                    "next_run_at": "2026-11-10T09:00:00Z",
                    "config": { "time": "09:00", "timezone": "UTC" }
                }
            ]
        }))
        .unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].label.as_deref(), Some("Morning Executive Brief"));
        assert_eq!(
            rows[0].prompt.as_deref(),
            Some("Summarize AI policy and tenders.")
        );
        assert_eq!(rows[1].label, None);
        assert_eq!(rows[1].prompt, None);
    }

    #[test]
    fn parses_active_workflow_items_with_fallback() {
        let active = parse_active_workflows(&json!({
            "workflows": [
                {
                    "conversation_id": "conv-1",
                    "goal": "Launch v0.45",
                    "done": 1,
                    "total": 2,
                    "items": [
                        { "title": "Update backend", "done": true },
                        { "title": "Redesign desktop UI", "done": false }
                    ]
                },
                {
                    "conversation_id": "conv-2",
                    "goal": "Legacy plan",
                    "done": 0,
                    "total": 1
                }
            ]
        }))
        .unwrap();
        assert_eq!(active[0].items.len(), 2);
        assert_eq!(
            active[0].items[0],
            WorkflowPlanStep {
                title: "Update backend".into(),
                done: true
            }
        );
        assert!(!active[0].items[1].done);
        assert!(active[1].items.is_empty());
    }

    #[test]
    fn builds_morning_brief_and_watch_routine_graphs() {
        let sources = vec![
            MonitoringSource {
                name: "eu-tenders".into(),
                kind: "rss".into(),
                enabled: true,
                url: Some("https://tenders.example.invalid/rss".into()),
                own: true,
                last_run: Some(SourceLastRun {
                    ok: false,
                    error: Some("502".into()),
                    items_seen: 3,
                    started_at: None,
                    finished_at: None,
                }),
            },
            MonitoringSource {
                name: "disabled-feed".into(),
                kind: "rss".into(),
                enabled: false,
                url: None,
                own: false,
                last_run: None,
            },
        ];

        let brief = Routine {
            name: "morning_brief".into(),
            enabled: true,
            next_run_at: "2026-11-10T07:00:00Z".into(),
            last_run_for: None,
            time: Some("07:00".into()),
            timezone: None,
            label: None,
            prompt: None,
        };
        let brief_graph = build_routine_graph(&brief, &sources, "UTC");
        assert!(brief_graph.active);
        assert_eq!(brief_graph.nodes.len(), 4);
        assert_eq!(brief_graph.nodes[0].kind, WorkflowNodeKind::Trigger);
        assert_eq!(brief_graph.nodes[0].subtitle, "07:00 UTC");
        assert_eq!(brief_graph.nodes[1].subtitle, "1 sources");
        assert_eq!(brief_graph.nodes[2].subtitle, "Prompt / Default brief");
        assert_eq!(brief_graph.nodes[3].kind, WorkflowNodeKind::Output);

        let watch = Routine {
            name: "watch_0".into(),
            enabled: true,
            next_run_at: "2026-11-10T10:00:00Z".into(),
            last_run_for: None,
            time: Some("10:00".into()),
            timezone: Some("Europe/Paris".into()),
            label: Some("Tender Watch".into()),
            prompt: Some("Flag procurement deadlines".into()),
        };
        let watch_graph = build_routine_graph(&watch, &sources, "UTC");
        assert_eq!(watch_graph.title, "Tender Watch");
        assert_eq!(watch_graph.nodes.len(), 5);
        assert_eq!(watch_graph.nodes[1].subtitle, "1/2 feeds");
        assert_eq!(watch_graph.nodes[1].status, WorkflowNodeStatus::Error);
        assert_eq!(watch_graph.nodes[2].subtitle, "Flag procurement deadlines");
        assert_eq!(watch_graph.nodes[3].kind, WorkflowNodeKind::Gate);
        assert_eq!(watch_graph.nodes[4].kind, WorkflowNodeKind::Output);
    }

    #[test]
    fn builds_source_and_plan_graphs() {
        let source = MonitoringSource {
            name: "tech-news".into(),
            kind: "rss".into(),
            enabled: true,
            url: Some("https://news.example.invalid/feed.xml".into()),
            own: true,
            last_run: Some(SourceLastRun {
                ok: true,
                error: None,
                items_seen: 14,
                started_at: None,
                finished_at: None,
            }),
        };
        let source_graph = build_source_graph(&source);
        assert!(source_graph.active);
        assert_eq!(source_graph.nodes.len(), 4);
        assert_eq!(source_graph.nodes[1].subtitle, "news.example.invalid");
        assert_eq!(source_graph.nodes[1].status, WorkflowNodeStatus::Done);
        assert_eq!(source_graph.nodes[2].subtitle, "14 seen");

        let plan = ActiveWorkflow {
            conversation_id: "conv-42".into(),
            goal: "Prepare Q4 report".into(),
            done: 1,
            total: 2,
            updated_at: None,
            items: vec![
                WorkflowPlanStep {
                    title: "Collect metrics".into(),
                    done: true,
                },
                WorkflowPlanStep {
                    title: "Draft summary".into(),
                    done: false,
                },
            ],
        };
        let plan_graph = build_plan_graph(&plan);
        assert!(plan_graph.active);
        assert_eq!(plan_graph.nodes.len(), 4);
        assert_eq!(plan_graph.nodes[0].kind, WorkflowNodeKind::Trigger);
        assert_eq!(plan_graph.nodes[1].kind, WorkflowNodeKind::Step);
        assert_eq!(plan_graph.nodes[1].badge, "Done");
        assert_eq!(plan_graph.nodes[2].badge, "Pending");
        assert_eq!(plan_graph.nodes[3].title, "Next Action");
    }
}
