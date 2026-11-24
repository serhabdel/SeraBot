//! The Jobs/Activity panel model (0.28.0): one unified, ordered list of the
//! ongoing work in the open conversation — the queued/running task, background
//! media jobs (pending/finishing/done/failed) and handoffs to other bots in a
//! room — built from the state the event stream and the media-jobs poll
//! already keep. Pure logic only; the components in the console binary draw it.
//!
//! Everything here is untrusted display data: labels come from the server and
//! are already bounded upstream, and this module never re-widens them.

use crate::activity::Activity;
use crate::api::{MediaJob, Task, TaskStatus};
use crate::jobs_reliability::JobsReliabilitySnapshot;
use crate::media_view::placeholder_label;
use crate::runs::{ChipKind, RunRow};

/// Most rows the panel keeps.
pub const MAX_ITEMS: usize = 24;

/// What kind of work one row is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JobKind {
    /// The conversation's own turn task (queued, running, awaiting approval).
    Task,
    /// A background media render (image/video/audio).
    Media,
    /// A handoff to another bot working in this room.
    Handoff,
    /// Uncertain task or unknown-outcome action from `GET /v1/jobs`.
    Reliability,
}

/// Where one row sits in its lifecycle.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum JobState {
    Queued,
    Running,
    Waiting,
    Finishing,
    Done,
    Failed,
}

impl JobState {
    pub fn label(self) -> &'static str {
        match self {
            Self::Queued => "Queued",
            Self::Running => "Working",
            Self::Waiting => "Waiting for approval",
            Self::Finishing => "Finishing",
            Self::Done => "Done",
            Self::Failed => "Failed",
        }
    }

    /// Active rows sort before finished ones; within a group, running first.
    fn rank(self) -> u8 {
        match self {
            Self::Running => 0,
            Self::Waiting => 1,
            Self::Finishing => 2,
            Self::Queued => 3,
            Self::Done => 4,
            Self::Failed => 5,
        }
    }

    pub fn active(self) -> bool {
        matches!(
            self,
            Self::Queued | Self::Running | Self::Waiting | Self::Finishing
        )
    }
}

/// One row of the panel.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct JobItem {
    pub kind: JobKind,
    pub id: String,
    pub bot: String,
    pub title: String,
    pub state: JobState,
    /// Optional longer detail (error message or destination target).
    pub detail: Option<String>,
    /// When set, the console can open this chat for trace context (B6).
    pub trace_conversation: Option<String>,
}

impl JobItem {
    pub fn icon(&self) -> &'static str {
        match self.kind {
            JobKind::Task => "bolt",
            JobKind::Media => "image",
            JobKind::Handoff => "users",
            JobKind::Reliability => "shield",
        }
    }
}

fn media_state(status: &str) -> Option<JobState> {
    match status {
        "pending" => Some(JobState::Queued),
        "finishing" => Some(JobState::Finishing),
        "done" => Some(JobState::Done),
        "failed" | "expired" => Some(JobState::Failed),
        _ => None,
    }
}

/// Build the panel's rows from the live state. Active work sorts first
/// (running, waiting, finishing, queued), then finished rows; the list is
/// capped at [`MAX_ITEMS`].
///
/// Handoffs come from the handoff ledger rows (`runs`), never from the
/// WorkingBots rings: a ring also fires for the chat's own turn and for
/// steps that already ended, while a ledger row is a handoff that started
/// and has not completed yet.
pub fn build(
    task: Option<&Task>,
    media_jobs: &[MediaJob],
    runs: &[RunRow],
    activity: &Activity,
    conversation: &str,
    reliability: &JobsReliabilitySnapshot,
) -> Vec<JobItem> {
    let mut items: Vec<JobItem> = Vec::new();

    if let Some(task) = task {
        let state = match task.status {
            TaskStatus::Queued => JobState::Queued,
            TaskStatus::Running => JobState::Running,
            TaskStatus::AwaitingApproval | TaskStatus::WaitingModel | TaskStatus::NeedsReview => {
                JobState::Waiting
            }
            TaskStatus::Done => JobState::Done,
            TaskStatus::Failed | TaskStatus::Cancelled => JobState::Failed,
        };
        let step = activity
            .steps
            .iter()
            .rev()
            .find(|step| step.phase == crate::activity::StepPhase::Running)
            .map(|step| step.label.clone());
        let title = match step {
            Some(label) if state == JobState::Running => label,
            _ => task.status.label().to_owned(),
        };
        items.push(JobItem {
            kind: JobKind::Task,
            id: format!("task:{}", task.id),
            bot: task.bot.clone(),
            title,
            state,
            detail: None,
            trace_conversation: None,
        });
    }

    for job in media_jobs {
        let Some(state) = media_state(&job.status) else {
            continue;
        };
        let detail = job
            .error
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_owned);
        items.push(JobItem {
            kind: JobKind::Media,
            id: format!("media:{}", job.id),
            bot: job.bot.clone(),
            title: placeholder_label(job),
            state,
            detail,
            trace_conversation: None,
        });
    }

    // Handoffs: ledger rows that started and have not completed yet, other
    // than the one the chat is addressed to. One row per handoff, so
    // parallel handoffs to the same bot stay visible as separate rows.
    let own = conversation.strip_prefix("dir:").unwrap_or(conversation);
    for row in runs {
        let state = match row.chip.kind() {
            ChipKind::Queued => JobState::Queued,
            ChipKind::Running | ChipKind::Resumed => JobState::Running,
            ChipKind::WaitingApproval | ChipKind::WaitingModel => JobState::Waiting,
            ChipKind::FailedResumable | ChipKind::Failed | ChipKind::Completed => continue,
        };
        if row.bot == own {
            continue;
        }
        items.push(JobItem {
            kind: JobKind::Handoff,
            id: format!("handoff:{}", row.child_task_id),
            bot: row.bot.clone(),
            title: format!("Handed off to {}", row.bot),
            state,
            detail: None,
            trace_conversation: None,
        });
    }

    for row in &reliability.uncertain_tasks {
        let title = row
            .last_error
            .clone()
            .filter(|text| !text.is_empty())
            .unwrap_or_else(|| format!("Uncertain {} task", row.kind));
        items.push(JobItem {
            kind: JobKind::Reliability,
            id: format!("uncertain:{}", row.id),
            bot: row.bot.clone(),
            title,
            state: JobState::Failed,
            detail: row.last_error.clone(),
            trace_conversation: row.conversation_id.clone(),
        });
    }
    for row in &reliability.unknown_actions {
        items.push(JobItem {
            kind: JobKind::Reliability,
            id: format!("unknown:{}", row.id),
            bot: row.tool.clone(),
            title: format!("Unknown outcome: {}", row.tool),
            state: JobState::Failed,
            detail: row
                .destination
                .as_ref()
                .map(|dest| format!("Destination: {dest}")),
            trace_conversation: row.conversation_id.clone(),
        });
    }

    items.sort_by(|a, b| {
        a.state
            .rank()
            .cmp(&b.state.rank())
            .then_with(|| a.kind.sort_rank().cmp(&b.kind.sort_rank()))
            .then_with(|| a.id.cmp(&b.id))
    });
    items.truncate(MAX_ITEMS);
    items
}

impl JobKind {
    /// The chat's own turn sorts before media and handoffs within a state.
    fn sort_rank(self) -> u8 {
        match self {
            Self::Task => 0,
            Self::Media => 1,
            Self::Handoff => 2,
            Self::Reliability => 3,
        }
    }
}

impl JobItem {
    /// Rows with active work, for the panel's headline count.
    pub fn is_active(&self) -> bool {
        self.state.active()
    }
}

/// "3 jobs running" style headline for the collapsed panel.
pub fn headline(items: &[JobItem]) -> String {
    let active = items.iter().filter(|item| item.is_active()).count();
    match active {
        0 => format!("{} finished", items.len()),
        1 => "1 job running".into(),
        n => format!("{n} jobs running"),
    }
}

/// Auto-expand the Jobs panel only when active work starts (`0 -> >0`), so a
/// manual collapse stays respected while jobs continue running.
#[allow(dead_code)]
pub fn should_auto_open(prev_active: usize, next_active: usize) -> bool {
    prev_active == 0 && next_active > 0
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::activity::StepPhase;
    use serde_json::json;

    fn task(id: &str, bot: &str, status: TaskStatus) -> Task {
        Task {
            id: id.into(),
            kind: "turn".into(),
            bot: bot.into(),
            status,
        }
    }

    fn job(id: &str, bot: &str, kind: &str, status: &str) -> MediaJob {
        MediaJob {
            id: id.into(),
            bot: bot.into(),
            kind: kind.into(),
            status: status.into(),
            artifact_hash: None,
            error: None,
            created_at: "2026-11-08T10:00:00.000Z".into(),
        }
    }

    fn step(task_id: &str, call: &str, phase: &str, label: &str) -> serde_json::Value {
        json!({"task_id": task_id, "call_id": call, "phase": phase, "label": label, "icon": "bolt"})
    }

    fn handoff_row(child: &str, bot: &str, status: &str) -> RunRow {
        crate::runs::row_of(
            &crate::runs::Handoff {
                id: format!("h-{child}"),
                parent_task_id: "p".into(),
                child_task_id: child.into(),
                to_bot: bot.into(),
                status: status.into(),
                ..Default::default()
            },
            None,
        )
    }

    #[test]
    fn running_task_media_and_handoffs_merge_active_first() {
        let runs = vec![handoff_row("t9", "analyst", "running")];
        let mut activity = Activity::default();
        activity.apply(
            "turn.step",
            &step("t1", "c1", "running", "Searching the web"),
        );

        let items = build(
            Some(&task("t1", "main", TaskStatus::Running)),
            &[
                job("m2", "maker", "video", "done"),
                job("m1", "maker", "image", "pending"),
                job("m3", "maker", "audio", "failed"),
            ],
            &runs,
            &activity,
            "room:ops",
            &JobsReliabilitySnapshot::default(),
        );
        let states: Vec<JobState> = items.iter().map(|i| i.state).collect();
        assert_eq!(
            states,
            vec![
                JobState::Running, // the task (its own turn sorts first)
                JobState::Running, // handoff to analyst
                JobState::Queued,  // pending media
                JobState::Done,    // done media
                JobState::Failed,  // failed media
            ]
        );
        // The running task carries the current step label.
        assert_eq!(items[0].title, "Searching the web");
        assert_eq!(items[0].kind, JobKind::Task);
        // The handoff names the other bot, not the room's own bot.
        assert_eq!(items[1].kind, JobKind::Handoff);
        assert_eq!(items[1].bot, "analyst");
        assert_eq!(items[1].title, "Handed off to analyst");
        // The pending media job keeps its placeholder wording.
        assert_eq!(items[2].title, "Generating an image\u{2026}");
    }

    #[test]
    fn the_conversations_own_bot_is_not_a_handoff() {
        // A ledger row to the chat's own bot is the turn itself, not a handoff.
        let runs = vec![handoff_row("t1", "main", "running")];
        let items = build(
            Some(&task("t1", "main", TaskStatus::Running)),
            &[],
            &runs,
            &Activity::default(),
            "dir:main",
            &JobsReliabilitySnapshot::default(),
        );
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].kind, JobKind::Task);
    }

    #[test]
    fn finished_handoffs_leave_the_panel_and_parallel_ones_stay() {
        let runs = vec![
            handoff_row("c1", "analyst", "running"),
            handoff_row("c2", "analyst", "running"),
            handoff_row("c3", "writer", "completed"),
            handoff_row("c4", "legal", "failed"),
        ];
        let items = build(
            None,
            &[],
            &runs,
            &Activity::default(),
            "dir:main",
            &JobsReliabilitySnapshot::default(),
        );
        assert_eq!(items.len(), 2);
        assert!(items.iter().all(|item| item.kind == JobKind::Handoff));
        assert_eq!(items[0].id, "handoff:c1");
        assert_eq!(items[1].id, "handoff:c2");
    }

    #[test]
    fn a_task_needing_review_waits_for_the_owner_not_a_retry() {
        let items = build(
            Some(&task("t1", "main", TaskStatus::NeedsReview)),
            &[],
            &[],
            &Activity::default(),
            "dir:main",
            &JobsReliabilitySnapshot::default(),
        );
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].state, JobState::Waiting);
        assert_eq!(items[0].title, "Needs review");
    }

    #[test]
    fn awaiting_approval_and_finishing_sort_before_done() {
        let items = build(
            Some(&task("t1", "main", TaskStatus::AwaitingApproval)),
            &[
                job("m1", "maker", "video", "finishing"),
                job("m2", "maker", "image", "done"),
            ],
            &[],
            &Activity::default(),
            "dir:main",
            &JobsReliabilitySnapshot::default(),
        );
        let states: Vec<JobState> = items.iter().map(|i| i.state).collect();
        assert_eq!(
            states,
            vec![JobState::Waiting, JobState::Finishing, JobState::Done]
        );
        assert_eq!(items[0].title, "Awaiting approval");
    }

    #[test]
    fn unknown_media_statuses_are_skipped_and_the_list_is_capped() {
        let mut jobs = Vec::new();
        for n in 0..40 {
            let status = if n % 4 == 3 { "weird" } else { "pending" };
            jobs.push(job(&format!("m{n}"), "maker", "image", status));
        }
        let items = build(
            None,
            &jobs,
            &[],
            &Activity::default(),
            "dir:main",
            &JobsReliabilitySnapshot::default(),
        );
        // 30 valid pending jobs, capped at MAX_ITEMS.
        assert_eq!(items.len(), MAX_ITEMS);
        assert!(items.iter().all(|i| i.state == JobState::Queued));
    }

    #[test]
    fn headline_counts_only_active_rows() {
        let items = vec![
            JobItem {
                kind: JobKind::Task,
                id: "t".into(),
                bot: "main".into(),
                title: "x".into(),
                state: JobState::Running,
                detail: None,
                trace_conversation: None,
            },
            JobItem {
                kind: JobKind::Media,
                id: "m".into(),
                bot: "maker".into(),
                title: "y".into(),
                state: JobState::Done,
                detail: None,
                trace_conversation: None,
            },
        ];
        assert_eq!(headline(&items), "1 job running");
        assert_eq!(headline(&items[1..]), "1 finished");
        assert_eq!(headline(&[]), "0 finished");
        let mut two = items.clone();
        two.push(JobItem {
            kind: JobKind::Handoff,
            id: "h".into(),
            bot: "a".into(),
            title: "z".into(),
            state: JobState::Queued,
            detail: None,
            trace_conversation: None,
        });
        assert_eq!(headline(&two), "2 jobs running");
    }

    #[test]
    fn reliability_preserves_destination_and_auto_open_only_on_zero_to_active() {
        let snap = crate::jobs_reliability::parse_jobs_reliability(&json!({
            "reliability": {
                "uncertain_tasks": [{
                    "id": "u1",
                    "kind": "turn",
                    "bot": "main",
                    "status": "outcome_unknown",
                    "last_error": "gateway timed out",
                    "conversation_id": "conv-1"
                }],
                "unknown_actions": [{
                    "id": "a1",
                    "tool": "email.send",
                    "conversation_id": "conv-2",
                    "destination": "ops@example.invalid"
                }]
            }
        }))
        .unwrap();
        let items = build(None, &[], &[], &Activity::default(), "dir:main", &snap);
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].detail.as_deref(), Some("gateway timed out"));
        assert_eq!(
            items[1].detail.as_deref(),
            Some("Destination: ops@example.invalid")
        );
        assert!(should_auto_open(0, 1));
        assert!(!should_auto_open(1, 2));
        assert!(!should_auto_open(1, 0));
    }

    #[test]
    fn done_step_falls_back_to_the_task_status_label() {
        let mut activity = Activity::default();
        activity.apply("turn.step", &step("t1", "c1", "done", "Searching the web"));
        let items = build(
            Some(&task("t1", "main", TaskStatus::Running)),
            &[],
            &[],
            &activity,
            "dir:main",
            &JobsReliabilitySnapshot::default(),
        );
        assert_eq!(items[0].title, "Working");
        assert_eq!(items[0].state, JobState::Running);
        let _ = StepPhase::Running; // the phase is part of the public surface
    }
}
