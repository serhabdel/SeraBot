//! Live activity of the running turn, built from `turn.step` stream events.
//!
//! The server phrases every step in plain words; this module only validates
//! and orders them. Payloads are untrusted display data: strings are
//! length-bounded and control characters are dropped, and the approval id is
//! only ever handed back to the existing approvals API.

use serde_json::Value;
use std::time::Instant;

/// Most steps kept for one turn.
const MAX_STEPS: usize = 64;
/// Dropped call ids remembered so repeat events of one step count once.
const MAX_TRACKED_DROPS: usize = 1_024;
const MAX_TEXT: usize = 120;
const MAX_INPUT: usize = 160;
const MAX_ERROR: usize = 200;
const MAX_RESULT_TEXT: usize = 400;
const MAX_CELL: usize = 80;
const MAX_COLS: usize = 8;
const MAX_ROWS: usize = 12;
/// A reported duration longer than this is ignored.
const MAX_DURATION_MS: u64 = 60 * 60 * 1000;
/// Longest reasoning text a Thinking step keeps.
const MAX_DETAIL: usize = 4_000;
const MEDIA_HOSTS: &[&str] = &["elevenlabs.io", "storage.googleapis.com"];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StepPhase {
    Running,
    /// The call returned and the work is still going (a generation).
    Progress,
    Done,
    Failed,
    WaitingApproval,
}

impl StepPhase {
    fn parse(value: &str) -> Option<Self> {
        match value {
            "running" => Some(Self::Running),
            "progress" => Some(Self::Progress),
            "done" => Some(Self::Done),
            "failed" => Some(Self::Failed),
            "waiting_approval" => Some(Self::WaitingApproval),
            _ => None,
        }
    }

    pub fn spinning(self) -> bool {
        matches!(self, Self::Running | Self::Progress)
    }
}

/// What a failed step offers. Unknown values are [`StepAction::None`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StepAction {
    None,
    Retry,
    Keys,
    Reconnect,
}

impl StepAction {
    /// The server sends a sentence, not a button. The words of that sentence
    /// decide whether Retry, Reconnect, or Open AI providers is offered.
    fn from_error(error: &str) -> Self {
        let lower = error.to_ascii_lowercase();
        if lower.contains("api key")
            || lower.contains("api_key")
            || lower.contains("provider key")
            || lower.contains("credential")
        {
            Self::Keys
        } else if lower.contains("timed out")
            || lower.contains("timeout")
            || lower.contains("you can retry")
            || lower.contains("busy")
        {
            Self::Retry
        } else if lower.contains("could not be reached")
            || lower.contains("check the connection")
            || lower.contains("reconnect")
        {
            Self::Reconnect
        } else {
            Self::None
        }
    }
}

/// The message "Try another way" sends after a failed step (0.69): it asks
/// the bot to change its approach once instead of repeating the same call.
pub fn retry_another_way_message(step_title: &str) -> String {
    let title: String = step_title
        .chars()
        .filter(|character| !character.is_control())
        .take(80)
        .collect();
    format!(
        "The step \"{}\" failed. Try once in a different way (a smaller part, another format \
         or another tool), following the error's hint. If it cannot work, tell me the one \
         thing I need to do.",
        title.trim()
    )
}

/// Plain connector name from a tool id. Older events omit `tool`.
pub fn tool_heading(tool: &str) -> String {
    let named = match tool {
        t if t.starts_with("elevenlabs_") || t.starts_with("audio_") => "ElevenLabs",
        t if t.starts_with("browser_") => "Browser",
        t if t.starts_with("slack_") => "Slack",
        t if t.starts_with("whatsapp_") => "WhatsApp",
        t if t.starts_with("github_") => "GitHub",
        t if t.starts_with("outlook_") || t.starts_with("agentmail_") => "Email",
        t if t.starts_with("onedrive_") => "OneDrive",
        t if t.starts_with("local_") => "This PC",
        t if t.starts_with("firecrawl_") || t.starts_with("tavily_") || t.starts_with("exa_") => {
            "Web search"
        }
        "media_job" => "Media",
        _ => "",
    };
    if !named.is_empty() {
        return named.to_owned();
    }
    let word = tool
        .split(['_', '.', '-'])
        .find(|part| !part.is_empty())
        .unwrap_or("Tool");
    let mut chars = word.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => "Tool".into(),
    }
}

/// How a tool result should be drawn. Unknown kinds fall back to text.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ResultKind {
    Audio,
    Image,
    File,
    Table,
    Link,
    Text,
}

impl ResultKind {
    fn parse(value: &str) -> Self {
        match value {
            "audio" => Self::Audio,
            "image" => Self::Image,
            "file" => Self::File,
            "table" => Self::Table,
            "link" => Self::Link,
            _ => Self::Text,
        }
    }
}

/// Redacted tool result. Every string is bounded. URLs are checked again
/// before the UI loads them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StepResult {
    pub kind: ResultKind,
    pub title: Option<String>,
    pub url: Option<String>,
    pub mime: Option<String>,
    pub voice: Option<String>,
    pub model: Option<String>,
    pub name: Option<String>,
    pub size: Option<u64>,
    pub text: Option<String>,
    pub artifact: Option<String>,
    pub columns: Vec<String>,
    pub rows: Vec<Vec<String>>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Step {
    pub call_id: String,
    pub icon: String,
    pub label: String,
    pub phase: StepPhase,
    pub approval_id: Option<String>,
    /// Reasoning text of a Thinking step, shown collapsed. Untrusted
    /// display data: bounded, control characters dropped, rendered as text.
    pub detail: Option<String>,
    /// Tool id. Absent on older servers.
    pub tool: Option<String>,
    /// Connector name in plain words, derived from `tool` when the server sends one.
    pub title: Option<String>,
    pub input_summary: Option<String>,
    pub result_summary: Option<String>,
    pub error: Option<String>,
    pub action: StepAction,
    pub duration_ms: Option<u64>,
    /// Typed cards from `cards` (`docs/tool-results.md`). Empty when absent.
    pub cards: Vec<StepResult>,
    /// When this step first started running (0.74.3): set on the first
    /// running event, never moved afterwards, so a running step can show a
    /// live elapsed time instead of the static word "Running". Client-side
    /// only; never sent anywhere.
    pub started_at: Option<Instant>,
}

impl Step {
    /// Milliseconds this step has been running, or `None` when it never
    /// started (or already finished: finished steps show `duration_ms`).
    pub fn elapsed_ms(&self, now: Instant) -> Option<u64> {
        self.started_at
            .filter(|_| self.phase.spinning())
            .map(|start| now.duration_since(start).as_millis() as u64)
    }
}

/// Steps of the most recent turn. Cleared when the owner sends again.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Activity {
    pub task_id: String,
    pub steps: Vec<Step>,
    /// When the current turn's first step event arrived (0.74.3): the
    /// turn's start for live elapsed times while no single step is running
    /// (the Thinking card). Client-side only.
    pub started_at: Option<Instant>,
    /// Elapsed seconds reported by the latest `turn.progress` heartbeat while
    /// a model call is in flight (0.74.11). Cleared when a tool step starts
    /// or the turn finishes.
    pub heartbeat_secs: Option<u64>,
    /// Beat counter from the latest `turn.progress` heartbeat (0.74.11).
    pub heartbeat_beat: u32,
    /// Steps past `MAX_STEPS` that were counted but not kept.
    pub dropped: usize,
    /// Call ids already counted in `dropped` (a step's later events must not
    /// count again). Bounded by the number of calls of one turn.
    dropped_calls: std::collections::BTreeSet<String>,
}

fn clean(value: &Value, key: &str) -> Option<String> {
    let text: String = value
        .get(key)?
        .as_str()?
        .chars()
        .filter(|c| !c.is_control())
        .take(MAX_TEXT)
        .collect();
    (!text.trim().is_empty()).then_some(text)
}

/// Like [`clean`] for long text: newlines and tabs survive.
fn clean_detail(value: &Value, key: &str) -> Option<String> {
    let text: String = value
        .get(key)?
        .as_str()?
        .chars()
        .filter(|c| !c.is_control() || matches!(c, '\n' | '\t'))
        .take(MAX_DETAIL)
        .collect();
    (!text.trim().is_empty()).then_some(text)
}

impl Activity {
    pub fn clear(&mut self) {
        *self = Self::default();
    }

    pub fn is_empty(&self) -> bool {
        self.steps.is_empty()
    }

    /// Clear any active model-call heartbeat.
    fn clear_heartbeat(&mut self) -> bool {
        let had = self.heartbeat_secs.is_some() || self.heartbeat_beat != 0;
        self.heartbeat_secs = None;
        self.heartbeat_beat = 0;
        had
    }

    /// Short live heartbeat note while a model call is in flight (e.g.
    /// `"Still thinking · 45s"` when `beat >= 1`).
    pub fn heartbeat_label(&self) -> Option<String> {
        if self.heartbeat_beat >= 1
            && let Some(secs) = self.heartbeat_secs
        {
            Some(format!("Still thinking \u{b7} {}s", secs))
        } else {
            None
        }
    }

    /// Apply one event. Returns whether anything changed.
    pub fn apply(&mut self, kind: &str, payload: &Value) -> bool {
        match kind {
            "turn.progress" => return self.apply_progress(payload),
            "turn.completed" | "task.failed" | "task.cancelled" => {
                let Some(task_id) = clean(payload, "task_id") else {
                    return false;
                };
                if self.task_id == task_id {
                    return self.clear_heartbeat();
                }
                return false;
            }
            "turn.step" => {}
            _ => return false,
        }
        let (Some(task_id), Some(call_id), Some(label), Some(phase)) = (
            clean(payload, "task_id"),
            clean(payload, "call_id"),
            clean(payload, "label"),
            payload
                .get("phase")
                .and_then(Value::as_str)
                .and_then(StepPhase::parse),
        ) else {
            return false;
        };
        if self.task_id != task_id {
            self.steps.clear();
            self.dropped = 0;
            self.dropped_calls.clear();
            self.task_id = task_id;
            self.started_at = Some(Instant::now());
            self.clear_heartbeat();
        } else if phase.spinning() && !label.trim().eq_ignore_ascii_case("thinking") {
            // A tool step started: the model call returned.
            self.clear_heartbeat();
        }
        let now = Instant::now();
        let icon = clean(payload, "icon").unwrap_or_else(|| "bolt".into());
        let approval_id = clean(payload, "approval_id");
        let detail = clean_detail(payload, "detail");
        let tool = clean(payload, "tool");
        let title = tool.as_deref().map(tool_heading);
        let input_summary = clean_max(payload, "input_summary", MAX_INPUT);
        let result_summary = clean_max(payload, "result_summary", MAX_TEXT);
        let error = clean_max(payload, "error", MAX_ERROR).map(present_error);
        let action = error
            .as_deref()
            .map(StepAction::from_error)
            .unwrap_or(StepAction::None);
        let duration_ms = duration_of(payload);
        let cards = parse_cards(payload.get("cards"));
        if let Some(step) = self.steps.iter_mut().find(|step| step.call_id == call_id) {
            step.phase = phase;
            if phase.spinning() && step.started_at.is_none() {
                step.started_at = Some(now);
            }
            step.label = label;
            step.icon = icon;
            step.detail = detail.or(step.detail.take());
            step.tool = tool.or(step.tool.take());
            step.title = title.or(step.title.take());
            step.input_summary = input_summary.or(step.input_summary.take());
            step.result_summary = result_summary.or(step.result_summary.take());
            if !cards.is_empty() {
                step.cards = cards;
            }
            if let Some(duration) = duration_ms {
                step.duration_ms = Some(duration);
            }
            if phase == StepPhase::WaitingApproval {
                step.approval_id = approval_id.or(step.approval_id.take());
                step.error = None;
                step.action = StepAction::None;
            } else if phase == StepPhase::Failed {
                step.approval_id = None;
                step.error = error.or(step.error.take());
                step.action = step
                    .error
                    .as_deref()
                    .map(StepAction::from_error)
                    .unwrap_or(StepAction::None);
            } else {
                step.approval_id = None;
                step.error = None;
                step.action = StepAction::None;
            }
        } else if self.steps.len() >= MAX_STEPS {
            // Not kept, but counted: the summary says how many are missing.
            if self.dropped_calls.len() >= MAX_TRACKED_DROPS || self.dropped_calls.insert(call_id) {
                self.dropped = self.dropped.saturating_add(1);
            }
        } else {
            self.steps.push(Step {
                call_id,
                icon,
                label,
                phase,
                approval_id: (phase == StepPhase::WaitingApproval)
                    .then_some(approval_id)
                    .flatten(),
                detail,
                tool,
                title,
                input_summary,
                result_summary,
                error: (phase == StepPhase::Failed).then_some(error).flatten(),
                action: if phase == StepPhase::Failed {
                    action
                } else {
                    StepAction::None
                },
                duration_ms,
                cards,
                started_at: if phase.spinning() { Some(now) } else { None },
            });
        }
        true
    }

    fn apply_progress(&mut self, payload: &Value) -> bool {
        let Some(task_id) = clean(payload, "task_id") else {
            return false;
        };
        let elapsed_secs = payload.get("elapsed_secs").and_then(Value::as_u64);
        let beat = payload
            .get("beat")
            .and_then(Value::as_u64)
            .and_then(|b| u32::try_from(b).ok());
        if elapsed_secs.is_none() && beat.is_none() {
            return false;
        }
        if self.task_id != task_id {
            self.steps.clear();
            self.dropped = 0;
            self.dropped_calls.clear();
            self.task_id = task_id;
            self.started_at = Some(Instant::now());
            self.clear_heartbeat();
        }
        let next_secs = elapsed_secs.or(self.heartbeat_secs);
        let next_beat = beat.unwrap_or(self.heartbeat_beat);
        let changed = self.heartbeat_secs != next_secs || self.heartbeat_beat != next_beat;
        self.heartbeat_secs = next_secs;
        self.heartbeat_beat = next_beat;
        changed
    }

    /// The step that waits on the owner, if any.
    pub fn waiting(&self) -> Option<&Step> {
        self.steps
            .iter()
            .find(|step| step.phase == StepPhase::WaitingApproval && step.approval_id.is_some())
    }

    /// The one step still running (0.74.3): the first spinning step. The
    /// feed's working line, the LiveCard and the step rows all derive from
    /// this single step, so there is never a second "working" indicator.
    pub fn running_step(&self) -> Option<&Step> {
        self.steps.iter().find(|step| step.phase.spinning())
    }

    /// Milliseconds the turn has been running, from its first step event.
    pub fn elapsed_ms(&self, now: Instant) -> Option<u64> {
        self.started_at
            .map(|start| now.duration_since(start).as_millis() as u64)
    }

    /// How long a finished turn took (0.74.3): the sum of the finished
    /// steps' reported durations when every finished step has one,
    /// otherwise the live elapsed time from the first step event.
    pub fn completed_elapsed_ms(&self, now: Instant) -> Option<u64> {
        self.total_duration_ms().or_else(|| self.elapsed_ms(now))
    }

    /// Steps that finished successfully.
    pub fn done_count(&self) -> usize {
        self.steps
            .iter()
            .filter(|step| step.phase == StepPhase::Done)
            .count()
    }

    /// "1 step" / "5 steps, 12s" for the collapsed summary. The time is the
    /// sum of reported durations, and it is left off when any finished step
    /// has none (an older server, or a lost event). Steps past the cap are
    /// counted in, so a long run is not under-reported.
    pub fn summary(&self) -> String {
        format_steps_summary(
            self.steps.len().saturating_add(self.dropped),
            self.total_duration_ms(),
        )
    }

    /// "+3 more steps not shown" once the cap was hit.
    pub fn dropped_note(&self) -> Option<String> {
        match self.dropped {
            0 => None,
            1 => Some("+1 more step not shown".into()),
            n => Some(format!("+{n} more steps not shown")),
        }
    }

    /// A step needs the owner (approval) or went wrong: the collapsed
    /// summary must not hide it.
    pub fn needs_attention(&self) -> bool {
        self.steps
            .iter()
            .any(|step| matches!(step.phase, StepPhase::WaitingApproval | StepPhase::Failed))
    }

    fn total_duration_ms(&self) -> Option<u64> {
        let finished: Vec<&Step> = self
            .steps
            .iter()
            .filter(|step| !step.phase.spinning())
            .collect();
        if finished.is_empty() || finished.iter().any(|step| step.duration_ms.is_none()) {
            return None;
        }
        Some(finished.iter().fold(0u64, |total, step| {
            total.saturating_add(step.duration_ms.unwrap_or(0))
        }))
    }
}

/// "under 1s", "12s", "1m 4s".
pub fn format_duration(ms: u64) -> String {
    if ms < 1_000 {
        "under 1s".into()
    } else if ms < 60_000 {
        format!("{}s", ms.div_ceil(1_000))
    } else {
        let minutes = ms / 60_000;
        let seconds = (ms % 60_000) / 1_000;
        if seconds == 0 {
            format!("{minutes}m")
        } else {
            format!("{minutes}m {seconds}s")
        }
    }
}

/// "5 steps" or "5 steps, 12s".
pub fn format_steps_summary(count: usize, duration_ms: Option<u64>) -> String {
    let steps = if count == 1 {
        "1 step".to_owned()
    } else {
        format!("{count} steps")
    };
    match duration_ms {
        Some(ms) => format!("{steps}, {}", format_duration(ms)),
        None => steps,
    }
}

/// https URL on a known media host, with no userinfo and no query.
pub fn safe_media_url(raw: &str) -> bool {
    let Ok(url) = url::Url::parse(raw.trim()) else {
        return false;
    };
    if url.scheme() != "https"
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
    {
        return false;
    }
    let Some(host) = url.host_str() else {
        return false;
    };
    MEDIA_HOSTS
        .iter()
        .any(|name| host == *name || host.ends_with(&format!(".{name}")))
}

/// https link with no credentials and no query. The UI uses this before
/// putting a URL in an href.
pub fn safe_link_url(raw: &str) -> bool {
    let Ok(url) = url::Url::parse(raw.trim()) else {
        return false;
    };
    url.scheme() == "https"
        && url.username().is_empty()
        && url.password().is_none()
        && url.query().is_none()
        && url.host_str().is_some()
}

fn clean_max(value: &Value, key: &str, max: usize) -> Option<String> {
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

/// A payload that looks like JSON is replaced. The server already sends a
/// sentence; this is the fallback when it does not.
fn present_error(text: String) -> String {
    let trimmed = text.trim_start();
    if trimmed.starts_with('{') || trimmed.starts_with('[') {
        "This step failed.".into()
    } else {
        text
    }
}

fn duration_of(value: &Value) -> Option<u64> {
    let ms = value.get("duration_ms")?.as_u64()?;
    (ms <= MAX_DURATION_MS).then_some(ms)
}

fn sha256_field(value: &Value, key: &str) -> Option<String> {
    value
        .get(key)
        .and_then(Value::as_str)
        .filter(|hash| hash.len() == 64 && hash.bytes().all(|byte| byte.is_ascii_hexdigit()))
        .map(|hash| hash.to_ascii_lowercase())
}

fn artifact_from_path(url: &str) -> Option<String> {
    let rest = url.trim().strip_prefix("/v1/artifacts/")?;
    let hash = rest.split(['?', '#']).next().unwrap_or(rest);
    (hash.len() == 64 && hash.bytes().all(|byte| byte.is_ascii_hexdigit()))
        .then(|| hash.to_ascii_lowercase())
}

fn parse_cards(value: Option<&Value>) -> Vec<StepResult> {
    let Some(items) = value.and_then(Value::as_array) else {
        return Vec::new();
    };
    items.iter().filter_map(parse_card).take(8).collect()
}

fn parse_card(value: &Value) -> Option<StepResult> {
    let kind_name = value.get("type").and_then(Value::as_str)?;
    let kind = ResultKind::parse(kind_name);
    if !matches!(kind_name, "audio" | "image" | "file" | "table" | "link") {
        return None;
    }
    parse_card_body(value, kind)
}

fn parse_card_body(value: &Value, kind: ResultKind) -> Option<StepResult> {
    let title = clean_max(value, "title", MAX_TEXT);
    let raw_url = value.get("url").and_then(Value::as_str).unwrap_or("");
    let url = clean_max(value, "url", 300).filter(|url| safe_link_url(url) || safe_media_url(url));
    let mime = clean_max(value, "mime", 120);
    let voice = clean_max(value, "voice", 80);
    let model = clean_max(value, "model", 80);
    let name = clean_max(value, "filename", 80).or_else(|| clean_max(value, "name", 80));
    let size = value
        .get("bytes")
        .and_then(Value::as_u64)
        .or_else(|| value.get("size").and_then(Value::as_u64));
    let text = clean_max(value, "description", MAX_RESULT_TEXT)
        .or_else(|| clean_max(value, "alt", MAX_TEXT))
        .or_else(|| clean_max(value, "text", MAX_RESULT_TEXT));
    let artifact = sha256_field(value, "artifact_hash")
        .or_else(|| sha256_field(value, "artifact"))
        .or_else(|| artifact_from_path(raw_url));
    let columns = value
        .get("columns")
        .and_then(Value::as_array)
        .map(|columns| {
            columns
                .iter()
                .filter_map(|cell| {
                    cell.as_str().and_then(|text| {
                        let text: String = text
                            .chars()
                            .filter(|c| !c.is_control())
                            .take(MAX_CELL)
                            .collect();
                        let text = text.trim().to_owned();
                        (!text.is_empty()).then_some(text)
                    })
                })
                .take(MAX_COLS)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let width = columns.len();
    let rows = value
        .get("rows")
        .and_then(Value::as_array)
        .map(|rows| {
            rows.iter()
                .filter_map(|row| row.as_array())
                .take(MAX_ROWS)
                .map(|row| {
                    (0..width)
                        .map(|index| {
                            row.get(index)
                                .and_then(Value::as_str)
                                .map(|text| {
                                    text.chars()
                                        .filter(|c| !c.is_control())
                                        .take(MAX_CELL)
                                        .collect::<String>()
                                        .trim()
                                        .to_owned()
                                })
                                .unwrap_or_default()
                        })
                        .collect::<Vec<_>>()
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let useful = title.is_some()
        || url.is_some()
        || text.is_some()
        || artifact.is_some()
        || name.is_some()
        || !columns.is_empty();
    if !useful && !matches!(kind, ResultKind::Text) {
        return None;
    }
    if !useful {
        return None;
    }
    Some(StepResult {
        kind: if matches!(kind, ResultKind::Table) && columns.is_empty() {
            ResultKind::Text
        } else {
            kind
        },
        title,
        url,
        mime,
        voice,
        model,
        name,
        size,
        text,
        artifact,
        columns,
        rows,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn another_way_message_names_the_step_only() {
        let text = retry_another_way_message("Cropping an image\n\u{7}");
        assert!(text.starts_with("The step \"Cropping an image\" failed."));
        assert!(!text.contains('\u{7}'));
    }
    use serde_json::json;

    fn ev(task: &str, call: &str, phase: &str) -> Value {
        json!({"task_id": task, "call_id": call, "phase": phase,
               "label": "Opening example.com", "icon": "globe"})
    }

    #[test]
    fn running_steps_carry_a_start_time_for_live_elapsed() {
        let mut a = Activity::default();
        assert!(a.apply("turn.step", &ev("t1", "c1", "running")));
        let started = a.steps[0].started_at;
        assert!(started.is_some(), "first running event stamps the step");
        assert!(a.started_at.is_some(), "first step event stamps the turn");
        assert!(a.steps[0].elapsed_ms(Instant::now()).is_some());
        assert_eq!(a.running_step().map(|s| s.call_id.as_str()), Some("c1"));
        // A later event of the same step does not move its start.
        assert!(a.apply("turn.step", &ev("t1", "c1", "running")));
        assert_eq!(a.steps[0].started_at, started);
        assert!(a.apply("turn.step", &ev("t1", "c1", "done")));
        assert!(a.steps[0].elapsed_ms(Instant::now()).is_none());
        assert!(a.running_step().is_none());
        // A step that arrives already done never gets a start time.
        assert!(a.apply("turn.step", &ev("t1", "c2", "done")));
        assert!(a.steps[1].started_at.is_none());
        // A new task restarts the turn clock.
        assert!(a.apply("turn.step", &ev("t2", "c9", "running")));
        assert!(a.started_at.is_some());
        assert_eq!(a.steps.len(), 1);
        // Finished durations still sum for the failed-turn line.
        let mut b = Activity::default();
        let mut d1 = ev("t1", "c1", "done");
        d1["duration_ms"] = json!(2_000);
        let mut d2 = ev("t1", "c2", "done");
        d2["duration_ms"] = json!(3_000);
        b.apply("turn.step", &d1);
        b.apply("turn.step", &d2);
        assert_eq!(b.completed_elapsed_ms(Instant::now()), Some(5_000));
        assert_eq!(b.done_count(), 2);
    }

    #[test]
    fn steps_progress_in_place_and_new_task_resets() {
        let mut a = Activity::default();
        assert!(a.apply("turn.step", &ev("t1", "c1", "running")));
        assert!(a.apply("turn.step", &ev("t1", "c1", "done")));
        assert_eq!(a.steps.len(), 1);
        assert_eq!(a.steps[0].phase, StepPhase::Done);
        assert!(a.apply("turn.step", &ev("t1", "c2", "running")));
        assert_eq!(a.summary(), "2 steps");
        assert!(a.apply("turn.step", &ev("t2", "c9", "running")));
        assert_eq!(a.steps.len(), 1);
        assert_eq!(a.summary(), "1 step");
    }

    #[test]
    fn steps_past_the_cap_are_counted_and_announced() {
        let mut a = Activity::default();
        for n in 0..(MAX_STEPS + 3) {
            a.apply("turn.step", &ev("t1", &format!("c{n}"), "running"));
        }
        // A later event of a dropped step does not count twice.
        a.apply("turn.step", &ev("t1", &format!("c{}", MAX_STEPS), "done"));
        assert_eq!(a.steps.len(), MAX_STEPS);
        assert_eq!(a.dropped, 3);
        assert_eq!(a.summary(), "67 steps");
        assert_eq!(a.dropped_note().as_deref(), Some("+3 more steps not shown"));
        a.apply("turn.step", &ev("t2", "c1", "running"));
        assert_eq!(a.dropped, 0);
        assert!(a.dropped_note().is_none());
    }

    #[test]
    fn waiting_or_failed_steps_need_attention() {
        let mut a = Activity::default();
        a.apply("turn.step", &ev("t1", "c1", "done"));
        assert!(!a.needs_attention());
        a.apply("turn.step", &ev("t1", "c2", "waiting_approval"));
        assert!(a.needs_attention());
        a.apply("turn.step", &ev("t1", "c2", "done"));
        assert!(!a.needs_attention());
        a.apply("turn.step", &ev("t1", "c3", "failed"));
        assert!(a.needs_attention());
    }

    #[test]
    fn approval_step_exposes_its_id_until_resolved() {
        let mut a = Activity::default();
        let mut p = ev("t1", "c1", "waiting_approval");
        p["approval_id"] = json!("appr-1");
        a.apply("turn.step", &p);
        assert_eq!(
            a.waiting().and_then(|s| s.approval_id.as_deref()),
            Some("appr-1")
        );
        a.apply("turn.step", &ev("t1", "c1", "done"));
        assert!(a.waiting().is_none());
    }

    #[test]
    fn thinking_step_keeps_bounded_reasoning_text() {
        let mut a = Activity::default();
        let mut p = ev("t1", "think-1", "done");
        p["label"] = json!("Thinking");
        p["detail"] = json!(format!("first\nsecond\u{7}{}", "x".repeat(MAX_DETAIL * 2)));
        assert!(a.apply("turn.step", &p));
        let detail = a.steps[0].detail.as_deref().unwrap();
        assert!(detail.starts_with("first\nsecond"));
        assert!(!detail.contains('\u{7}'));
        assert!(detail.chars().count() <= MAX_DETAIL);
        // A later update without detail keeps what was shown.
        assert!(a.apply("turn.step", &ev("t1", "think-1", "done")));
        assert!(a.steps[0].detail.is_some());
    }

    #[test]
    fn junk_and_other_kinds_are_ignored() {
        let mut a = Activity::default();
        assert!(!a.apply("task.updated", &ev("t", "c", "running")));
        assert!(!a.apply("turn.step", &json!({"task_id": "t"})));
        assert!(!a.apply("turn.step", &ev("t", "c", "bogus")));
        let mut long = ev("t", "c", "running");
        long["label"] = json!(format!("a\u{7}{}", "x".repeat(500)));
        assert!(a.apply("turn.step", &long));
        assert!(a.steps[0].label.chars().count() <= MAX_TEXT);
        assert!(!a.steps[0].label.contains('\u{7}'));
    }

    #[test]
    fn optional_fields_fill_in_and_older_events_still_parse() {
        let mut a = Activity::default();
        let mut running = ev("t1", "c1", "running");
        running["tool"] = json!("elevenlabs_mcp_call");
        running["input_summary"] = json!("Working with audio");
        assert!(a.apply("turn.step", &running));
        assert_eq!(a.steps[0].title.as_deref(), Some("ElevenLabs"));
        assert!(a.steps[0].duration_ms.is_none());
        assert_eq!(a.summary(), "1 step");

        let mut done = ev("t1", "c1", "done");
        done["duration_ms"] = json!(12_000);
        done["cards"] = json!([{
            "type": "audio",
            "voice": "Rachel",
            "model": "eleven_v2",
            "artifact_hash": "ab".repeat(32),
            "filename": "note.mp3",
            "url": "/v1/artifacts/".to_string() + &"ab".repeat(32)
        }]);
        assert!(a.apply("turn.step", &done));
        let step = &a.steps[0];
        assert_eq!(step.duration_ms, Some(12_000));
        let result = &step.cards[0];
        assert_eq!(result.kind, ResultKind::Audio);
        assert_eq!(result.voice.as_deref(), Some("Rachel"));
        assert_eq!(result.name.as_deref(), Some("note.mp3"));
        assert!(result.artifact.is_some());
        assert!(
            result.url.is_none(),
            "artifact paths are not loaded as media urls"
        );
        assert_eq!(a.summary(), "1 step, 12s");
    }

    #[test]
    fn failed_step_keeps_a_sentence_and_an_action() {
        let mut a = Activity::default();
        let mut failed = ev("t1", "c1", "failed");
        failed["error"] = json!("{\"error\":\"boom\"}");
        failed["duration_ms"] = json!(900);
        assert!(a.apply("turn.step", &failed));
        assert_eq!(a.steps[0].action, StepAction::None);
        let mut timed = ev("t1", "c2", "failed");
        timed["error"] = json!("This step timed out. You can retry.");
        assert!(a.apply("turn.step", &timed));
        assert_eq!(a.steps[1].action, StepAction::Retry);
        let mut keyed = ev("t1", "c3", "failed");
        keyed["error"] = json!("The provider rejected the API key.");
        assert!(a.apply("turn.step", &keyed));
        assert_eq!(a.steps[2].action, StepAction::Keys);
        assert_eq!(a.steps[0].error.as_deref(), Some("This step failed."));
        assert_eq!(a.summary(), "3 steps");
        assert!(safe_media_url("https://cdn.elevenlabs.io/note.mp3"));
        assert!(!safe_media_url("https://cdn.elevenlabs.io/note.mp3?sig=1"));
        assert!(safe_link_url("https://example.com/docs"));
        assert!(!safe_link_url("javascript:alert(1)"));
    }

    #[test]
    fn a_table_result_is_bounded() {
        let mut a = Activity::default();
        let mut done = ev("t1", "c1", "done");
        let mut rows = Vec::new();
        for index in 0..20 {
            rows.push(json!([format!("r{index}"), "x\u{7}"]));
        }
        done["cards"] = json!([{
            "type": "table",
            "columns": ["Name", "Note"],
            "rows": rows
        }]);
        assert!(a.apply("turn.step", &done));
        let result = &a.steps[0].cards[0];
        assert_eq!(result.kind, ResultKind::Table);
        assert!(result.rows.len() <= 12);
        assert!(!result.rows[0][1].contains('\u{7}'));
    }

    #[test]
    fn turn_progress_tracks_heartbeat_and_clears_on_tool_or_finish() {
        let mut a = Activity::default();
        assert_eq!(a.heartbeat_label(), None);

        // Beat 0 without beat >= 1 does not show the label yet.
        assert!(a.apply(
            "turn.progress",
            &json!({"task_id": "t1", "phase": "waiting_model", "elapsed_secs": 0, "beat": 0})
        ));
        assert_eq!(a.heartbeat_secs, Some(0));
        assert_eq!(a.heartbeat_beat, 0);
        assert_eq!(a.heartbeat_label(), None);

        // Beat 3 -> "Still thinking · 45s".
        assert!(a.apply(
            "turn.progress",
            &json!({"task_id": "t1", "phase": "waiting_model", "elapsed_secs": 45, "beat": 3})
        ));
        assert_eq!(a.heartbeat_secs, Some(45));
        assert_eq!(a.heartbeat_beat, 3);
        assert_eq!(a.heartbeat_label().as_deref(), Some("Still thinking · 45s"));

        // A Thinking step does not clear the heartbeat.
        let mut thinking = ev("t1", "c-think", "running");
        thinking["label"] = json!("Thinking");
        assert!(a.apply("turn.step", &thinking));
        assert_eq!(a.heartbeat_label().as_deref(), Some("Still thinking · 45s"));

        // A running tool step clears the heartbeat.
        assert!(a.apply("turn.step", &ev("t1", "c1", "running")));
        assert_eq!(a.heartbeat_secs, None);
        assert_eq!(a.heartbeat_beat, 0);
        assert_eq!(a.heartbeat_label(), None);

        // Heartbeat resumes during next model call, then clears on turn.completed.
        assert!(a.apply(
            "turn.progress",
            &json!({"task_id": "t1", "phase": "waiting_model", "elapsed_secs": 15, "beat": 1})
        ));
        assert_eq!(a.heartbeat_label().as_deref(), Some("Still thinking · 15s"));
        assert!(a.apply("turn.completed", &json!({"task_id": "t1"})));
        assert_eq!(a.heartbeat_secs, None);
        assert_eq!(a.heartbeat_beat, 0);
        assert_eq!(a.heartbeat_label(), None);

        // task.failed also clears it.
        assert!(a.apply(
            "turn.progress",
            &json!({"task_id": "t1", "phase": "waiting_model", "elapsed_secs": 30, "beat": 2})
        ));
        assert!(a.apply("task.failed", &json!({"task_id": "t1"})));
        assert_eq!(a.heartbeat_label(), None);
    }
}
