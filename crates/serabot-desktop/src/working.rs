//! Which bots are working right now, built from the live event stream.
//!
//! Every `turn.step` event names the bot, the room or chat it works in and
//! the task. A task is "working" from its first step until `turn.completed`
//! or `task.failed` names it, and it waits on the owner while a step asks for
//! approval. The rail, the chat header and the room member row read this to
//! show a ring around the right avatar and a short label under the name.
//!
//! Events are untrusted display data: ids and labels are length-bounded and
//! control characters are dropped. If the stream goes quiet (a lost event) an
//! entry expires after [`STALE_AFTER`], so a ring never spins forever.

use std::hash::{Hash, Hasher};
use std::time::{Duration, Instant};

use serde_json::Value;

/// An entry with no event for this long is treated as finished.
pub const STALE_AFTER: Duration = Duration::from_secs(15 * 60);
/// How long a bot keeps the brief `"done"` completion shimmer after `turn.completed`.
pub const DONE_SHIMMER: Duration = Duration::from_secs(2);
/// Tasks tracked at once.
const MAX_ENTRIES: usize = 32;
const MAX_ID: usize = 80;
const MAX_LABEL: usize = 80;
/// Shown under the name when the bot is busy without a named step.
pub const WORKING_LABEL: &str = "Working…";
/// Shown while a step waits for the owner's approval.
pub const WAITING_LABEL: &str = "Waiting for approval";

/// Fine-grained activity category for avatar presence rings (`data-activity`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum ActivityKind {
    #[default]
    Idle,
    Thinking,
    Searching,
    Browsing,
    Coding,
    Delegating,
    WaitingApproval,
    Done,
}

impl ActivityKind {
    /// Value for the DOM `data-activity` attribute.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::Thinking => "thinking",
            Self::Searching => "searching",
            Self::Browsing => "browsing",
            Self::Coding => "coding",
            Self::Delegating => "delegating",
            Self::WaitingApproval => "waiting_approval",
            Self::Done => "done",
        }
    }

    /// True when the avatar should animate (excludes `Idle`, `WaitingApproval`, `Done`).
    pub const fn animates(self) -> bool {
        matches!(
            self,
            Self::Thinking | Self::Searching | Self::Browsing | Self::Coding | Self::Delegating
        )
    }
}

/// Map a `turn.step` tool name (and optional display label fallback) to an [`ActivityKind`].
pub fn classify_step_activity(tool: Option<&str>, label: Option<&str>) -> ActivityKind {
    if let Some(raw_tool) = tool {
        let t = raw_tool.trim().to_ascii_lowercase();
        if t == "delegate" || t == "ask_teammate" || t.starts_with("delegate_") {
            return ActivityKind::Delegating;
        }
        if matches!(
            t.as_str(),
            "rag_search"
                | "job_search"
                | "job_watch_now"
                | "tavily_search"
                | "exa_search"
                | "firecrawl_search"
                | "github_search_repos"
                | "memory_search"
                | "skill_search"
        ) || t.ends_with("_search")
        {
            return ActivityKind::Searching;
        }
        if t.starts_with("browser_")
            || matches!(
                t.as_str(),
                "social_extract" | "fetch_url" | "exa_get_contents" | "watch_now"
            )
            || t.starts_with("firecrawl_")
        {
            return ActivityKind::Browsing;
        }
        if t.starts_with("project_")
            || t.starts_with("file_")
            || t.starts_with("mail_")
            || t.starts_with("outlook_")
            || t.starts_with("agentmail_")
            || t.starts_with("github_")
            || t.starts_with("local_")
            || t == "document_index"
        {
            return ActivityKind::Coding;
        }
    }
    if let Some(raw_label) = label {
        let l = raw_label.trim().to_ascii_lowercase();
        if l.contains("handing off") || l.contains("asking ") || l.contains("delegat") {
            return ActivityKind::Delegating;
        }
        if l.contains("brows")
            || l.contains("opening ")
            || l.contains("reading the page")
            || l.contains("clicking ")
            || l.contains("screenshot")
            || l.contains("scrolling the page")
        {
            return ActivityKind::Browsing;
        }
        if l.contains("search") || l.contains("checking job sites") {
            return ActivityKind::Searching;
        }
        if l.contains("project")
            || l.contains("file")
            || l.contains("email")
            || l.contains("inbox")
            || l.contains("code")
            || l.contains("spreadsheet")
            || l.contains("document")
        {
            return ActivityKind::Coding;
        }
    }
    ActivityKind::Thinking
}

/// What one bot is doing.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum BotActivity {
    /// Running a turn; the label is the current step ("Reading files"), if known.
    Working(Option<String>),
    /// Paused until the owner answers an approval.
    WaitingApproval,
}

impl BotActivity {
    /// The one-line label under the bot's name.
    pub fn label(&self) -> String {
        match self {
            Self::Working(Some(step)) => step.clone(),
            Self::Working(None) => WORKING_LABEL.to_owned(),
            Self::WaitingApproval => WAITING_LABEL.to_owned(),
        }
    }

    /// True when the avatar should show the moving ring.
    pub fn animates(&self) -> bool {
        matches!(self, Self::Working(_))
    }
}

#[derive(Clone, Debug)]
struct Entry {
    task_id: String,
    bot: String,
    conversation: String,
    activity: BotActivity,
    kind: ActivityKind,
    seen: Instant,
}

#[derive(Clone, Debug)]
struct DoneEntry {
    bot: String,
    conversation: String,
    completed_at: Instant,
}

/// Tasks that are running, by bot and conversation.
#[derive(Clone, Debug, Default)]
pub struct WorkingBots {
    entries: Vec<Entry>,
    done_entries: Vec<DoneEntry>,
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

/// Conversation ids are `dir:<bot>` for a direct chat; compare without it.
fn same_conversation(a: &str, b: &str) -> bool {
    a.strip_prefix("dir:").unwrap_or(a) == b.strip_prefix("dir:").unwrap_or(b)
}

impl WorkingBots {
    pub fn clear(&mut self) {
        self.entries.clear();
        self.done_entries.clear();
    }

    /// Apply one stream event. Returns whether anything changed.
    pub fn apply(&mut self, kind: &str, payload: &Value, now: Instant) -> bool {
        match kind {
            "turn.step" => self.apply_step(payload, now),
            "turn.progress" => self.apply_progress(payload, now),
            "turn.completed" => {
                let Some(task_id) = clean(payload, "task_id", MAX_ID) else {
                    return false;
                };
                self.prune(now);
                let mut completed = Vec::new();
                self.entries.retain(|entry| {
                    if entry.task_id == task_id {
                        completed.push((entry.bot.clone(), entry.conversation.clone()));
                        false
                    } else {
                        true
                    }
                });
                let changed = !completed.is_empty();
                for (bot, conversation) in completed {
                    self.done_entries.retain(|d| d.bot != bot);
                    if self.done_entries.len() >= MAX_ENTRIES {
                        self.done_entries.remove(0);
                    }
                    self.done_entries.push(DoneEntry {
                        bot,
                        conversation,
                        completed_at: now,
                    });
                }
                changed
            }
            "task.failed" => {
                let Some(task_id) = clean(payload, "task_id", MAX_ID) else {
                    return false;
                };
                let before = self.entries.len();
                self.entries.retain(|entry| entry.task_id != task_id);
                before != self.entries.len()
            }
            _ => false,
        }
    }

    fn apply_progress(&mut self, payload: &Value, now: Instant) -> bool {
        let Some(task_id) = clean(payload, "task_id", MAX_ID) else {
            return false;
        };
        let phase = payload.get("phase").and_then(Value::as_str);
        let elapsed_secs = payload.get("elapsed_secs").and_then(Value::as_u64);
        self.prune(now);
        let Some(entry) = self.entries.iter_mut().find(|e| e.task_id == task_id) else {
            // If no turn.step was seen yet (e.g. long initial model reasoning),
            // register the bot if `bot` is provided so the rail ring stays alive.
            if phase == Some("waiting_model")
                && let Some(bot) = clean(payload, "bot", MAX_ID)
            {
                let conversation = clean(payload, "conversation_id", MAX_ID).unwrap_or_default();
                let label = elapsed_secs.map(|s| format!("Thinking… ({s}s)"));
                self.done_entries.retain(|d| d.bot != bot);
                if self.entries.len() >= MAX_ENTRIES {
                    self.entries.remove(0);
                }
                self.entries.push(Entry {
                    task_id,
                    bot,
                    conversation,
                    activity: BotActivity::Working(label),
                    kind: ActivityKind::Thinking,
                    seen: now,
                });
                return true;
            }
            return false;
        };
        let mut changed = entry.seen != now;
        entry.seen = now;
        if phase == Some("waiting_model")
            && matches!(&entry.activity, BotActivity::Working(label) if label.is_none() || label.as_deref().is_some_and(|l| l.starts_with("Thinking…")))
        {
            let next_label = elapsed_secs.map(|s| format!("Thinking… ({s}s)"));
            let next_activity = BotActivity::Working(next_label);
            if entry.activity != next_activity || entry.kind != ActivityKind::Thinking {
                entry.activity = next_activity;
                entry.kind = ActivityKind::Thinking;
                changed = true;
            }
        }
        changed
    }

    fn apply_step(&mut self, payload: &Value, now: Instant) -> bool {
        // An older server sends no bot: nothing to attribute the step to.
        let (Some(task_id), Some(bot)) = (
            clean(payload, "task_id", MAX_ID),
            clean(payload, "bot", MAX_ID),
        ) else {
            return false;
        };
        let conversation = clean(payload, "conversation_id", MAX_ID).unwrap_or_default();
        let tool = clean(payload, "tool", MAX_ID);
        let (activity, kind) = match payload.get("phase").and_then(Value::as_str) {
            Some("running" | "progress") => {
                let label = clean(payload, "label", MAX_LABEL);
                let kind = classify_step_activity(tool.as_deref(), label.as_deref());
                (BotActivity::Working(label), kind)
            }
            // The turn goes on between steps: keep the ring, drop the stale name.
            Some("done" | "failed") => (BotActivity::Working(None), ActivityKind::Thinking),
            Some("waiting_approval") => {
                (BotActivity::WaitingApproval, ActivityKind::WaitingApproval)
            }
            _ => return false,
        };
        self.prune(now);
        self.done_entries.retain(|d| d.bot != bot);
        if let Some(entry) = self.entries.iter_mut().find(|e| e.task_id == task_id) {
            entry.bot = bot;
            entry.conversation = conversation;
            entry.activity = activity;
            entry.kind = kind;
            entry.seen = now;
        } else {
            if self.entries.len() >= MAX_ENTRIES {
                self.entries.remove(0);
            }
            self.entries.push(Entry {
                task_id,
                bot,
                conversation,
                activity,
                kind,
                seen: now,
            });
        }
        true
    }

    /// Forget entries that have been quiet for [`STALE_AFTER`] and expired done shimmers.
    pub fn prune(&mut self, now: Instant) {
        self.entries
            .retain(|entry| now.saturating_duration_since(entry.seen) < STALE_AFTER);
        self.done_entries
            .retain(|entry| now.saturating_duration_since(entry.completed_at) < DONE_SHIMMER);
    }

    fn live(&self, now: Instant) -> impl Iterator<Item = &Entry> {
        self.entries
            .iter()
            .filter(move |entry| now.saturating_duration_since(entry.seen) < STALE_AFTER)
    }

    /// What `bot` is doing, in any conversation. Working wins over waiting;
    /// among equals the most recent event wins.
    pub fn activity_of(&self, bot: &str, now: Instant) -> Option<BotActivity> {
        let bot = bot.strip_prefix("dir:").unwrap_or(bot);
        self.live(now)
            .filter(|entry| entry.bot == bot)
            .max_by_key(|entry| (entry.activity.animates(), entry.seen))
            .map(|entry| entry.activity.clone())
    }

    /// Fine-grained [`ActivityKind`] for `bot` (for `data-activity` on avatars).
    /// Returns [`ActivityKind::Done`] briefly after `turn.completed`.
    pub fn activity_kind_of(&self, bot: &str, now: Instant) -> ActivityKind {
        let bot = bot.strip_prefix("dir:").unwrap_or(bot);
        if let Some(entry) = self
            .live(now)
            .filter(|entry| entry.bot == bot)
            .max_by_key(|entry| (entry.activity.animates(), entry.seen))
        {
            return entry.kind;
        }
        if self
            .done_entries
            .iter()
            .rev()
            .any(|d| d.bot == bot && now.saturating_duration_since(d.completed_at) < DONE_SHIMMER)
        {
            return ActivityKind::Done;
        }
        ActivityKind::Idle
    }

    /// Fine-grained [`ActivityKind`] for a conversation (for `data-activity` in the rail).
    pub fn conversation_activity_kind(&self, conversation: &str, now: Instant) -> ActivityKind {
        if let Some(entry) = self
            .live(now)
            .filter(|entry| same_conversation(&entry.conversation, conversation))
            .max_by_key(|entry| (entry.activity.animates(), entry.seen))
        {
            return entry.kind;
        }
        if self.done_entries.iter().rev().any(|d| {
            same_conversation(&d.conversation, conversation)
                && now.saturating_duration_since(d.completed_at) < DONE_SHIMMER
        }) {
            return ActivityKind::Done;
        }
        ActivityKind::Idle
    }

    /// Whether anything in this chat or room is working (rail dot / ring).
    pub fn conversation_working(&self, conversation: &str, now: Instant) -> bool {
        self.live(now).any(|entry| {
            entry.activity.animates() && same_conversation(&entry.conversation, conversation)
        })
    }

    /// Live entries in one conversation as `(bot, activity)` pairs, for the
    /// Jobs/Activity panel (0.28.0). The conversation's own bot is included;
    /// callers decide what counts as a handoff.
    pub fn conversation_entries(
        &self,
        conversation: &str,
        now: Instant,
    ) -> Vec<(String, BotActivity)> {
        self.live(now)
            .filter(|entry| same_conversation(&entry.conversation, conversation))
            .map(|entry| (entry.bot.clone(), entry.activity.clone()))
            .collect()
    }

    /// Mix live entry ids and activities into `h` for UI dirty detection.
    pub fn hash_fingerprint<H: Hasher>(&self, h: &mut H) {
        self.entries.len().hash(h);
        for entry in &self.entries {
            entry.task_id.hash(h);
            entry.bot.hash(h);
            entry.conversation.hash(h);
            entry.activity.hash(h);
            entry.kind.hash(h);
            entry.activity.label().hash(h);
        }
        self.done_entries.len().hash(h);
        for d in &self.done_entries {
            d.bot.hash(h);
            d.conversation.hash(h);
        }
    }

    /// The bots that are working right now, sorted.
    pub fn working_bots(&self, now: Instant) -> Vec<String> {
        let mut bots: Vec<String> = self
            .live(now)
            .filter(|entry| entry.activity.animates())
            .map(|entry| entry.bot.clone())
            .collect();
        bots.sort();
        bots.dedup();
        bots
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn step(task: &str, bot: &str, conv: &str, phase: &str, label: &str) -> Value {
        json!({"task_id": task, "bot": bot, "conversation_id": conv,
               "phase": phase, "label": label, "call_id": "c1"})
    }

    #[test]
    fn a_running_step_marks_the_bot_with_its_label() {
        let now = Instant::now();
        let mut w = WorkingBots::default();
        assert!(w.apply(
            "turn.step",
            &step("t1", "analyst", "dir:analyst", "running", "Reading files"),
            now
        ));
        assert_eq!(
            w.activity_of("analyst", now),
            Some(BotActivity::Working(Some("Reading files".into())))
        );
        assert_eq!(w.working_bots(now), vec!["analyst"]);
        assert!(w.conversation_working("dir:analyst", now));
        assert!(w.conversation_working("analyst", now));
        assert_eq!(w.activity_of("main", now), None);
        assert_eq!(
            w.activity_of("analyst", now).unwrap().label(),
            "Reading files"
        );
    }

    #[test]
    fn the_ring_stops_when_the_turn_completes_or_the_task_fails() {
        let now = Instant::now();
        let mut w = WorkingBots::default();
        w.apply("turn.step", &step("t1", "a", "dir:a", "running", "x"), now);
        w.apply("turn.step", &step("t2", "b", "room:r", "running", "y"), now);
        assert_eq!(w.working_bots(now), vec!["a", "b"]);
        assert!(w.apply("turn.completed", &json!({"task_id": "t1"}), now));
        assert_eq!(w.working_bots(now), vec!["b"]);
        assert!(w.apply("task.failed", &json!({"task_id": "t2", "error": "e"}), now));
        assert!(w.working_bots(now).is_empty());
        // An unknown task changes nothing.
        assert!(!w.apply("turn.completed", &json!({"task_id": "zz"}), now));
    }

    #[test]
    fn between_steps_the_bot_keeps_working_without_a_stale_label() {
        let now = Instant::now();
        let mut w = WorkingBots::default();
        w.apply(
            "turn.step",
            &step("t1", "a", "dir:a", "running", "Reading"),
            now,
        );
        w.apply(
            "turn.step",
            &step("t1", "a", "dir:a", "done", "Reading"),
            now,
        );
        assert_eq!(w.activity_of("a", now), Some(BotActivity::Working(None)));
        assert_eq!(w.activity_of("a", now).unwrap().label(), WORKING_LABEL);
    }

    #[test]
    fn waiting_for_approval_is_not_animated() {
        let now = Instant::now();
        let mut w = WorkingBots::default();
        w.apply(
            "turn.step",
            &step("t1", "a", "dir:a", "waiting_approval", "Send"),
            now,
        );
        let activity = w.activity_of("a", now).unwrap();
        assert_eq!(activity, BotActivity::WaitingApproval);
        assert!(!activity.animates());
        assert!(w.working_bots(now).is_empty());
        assert!(!w.conversation_working("dir:a", now));
    }

    #[test]
    fn room_members_are_tracked_one_by_one() {
        let now = Instant::now();
        let mut w = WorkingBots::default();
        w.apply(
            "turn.step",
            &step("t1", "writer", "room:r", "running", "Writing"),
            now,
        );
        w.apply(
            "turn.step",
            &step("t2", "editor", "room:r", "running", "Checking"),
            now,
        );
        assert_eq!(w.working_bots(now), vec!["editor", "writer"]);
        assert!(w.conversation_working("room:r", now));
        assert!(!w.conversation_working("room:other", now));
        w.apply("turn.completed", &json!({"task_id": "t1"}), now);
        assert_eq!(w.working_bots(now), vec!["editor"]);
    }

    #[test]
    fn a_quiet_stream_expires_instead_of_spinning_forever() {
        let start = Instant::now();
        let mut w = WorkingBots::default();
        w.apply(
            "turn.step",
            &step("t1", "a", "dir:a", "running", "x"),
            start,
        );
        let later = start + STALE_AFTER + Duration::from_secs(1);
        assert!(w.working_bots(later).is_empty());
        assert_eq!(w.activity_of("a", later), None);
        w.prune(later);
        assert!(w.entries.is_empty());
    }

    #[test]
    fn steps_without_a_bot_or_with_junk_are_ignored() {
        let now = Instant::now();
        let mut w = WorkingBots::default();
        assert!(!w.apply(
            "turn.step",
            &json!({"task_id": "t", "phase": "running"}),
            now
        ));
        assert!(!w.apply("turn.step", &step("t", "a", "c", "exploding", "x"), now));
        assert!(!w.apply("task.queued", &json!({"task_id": "t"}), now));
        let long = "x".repeat(500);
        w.apply("turn.step", &step("t", "a", "c", "running", &long), now);
        let label = w.activity_of("a", now).unwrap().label();
        assert!(label.chars().count() <= MAX_LABEL);
        w.clear();
        assert!(w.working_bots(now).is_empty());
    }

    #[test]
    fn the_table_is_bounded() {
        let now = Instant::now();
        let mut w = WorkingBots::default();
        for n in 0..(MAX_ENTRIES + 10) {
            w.apply(
                "turn.step",
                &step(&format!("t{n}"), &format!("b{n}"), "c", "running", "x"),
                now,
            );
        }
        assert_eq!(w.entries.len(), MAX_ENTRIES);
    }

    #[test]
    fn turn_progress_refreshes_seen_and_updates_waiting_model_label() {
        let start = Instant::now();
        let mut w = WorkingBots::default();
        // Start with a completed step (no named tool step currently running).
        w.apply(
            "turn.step",
            &step("t1", "analyst", "dir:analyst", "done", "Reading"),
            start,
        );
        assert_eq!(
            w.activity_of("analyst", start),
            Some(BotActivity::Working(None))
        );

        // Halfway to STALE_AFTER, turn.progress arrives: updates seen and sets Thinking label.
        let mid = start + Duration::from_secs(10 * 60);
        assert!(w.apply(
            "turn.progress",
            &json!({
                "task_id": "t1",
                "bot": "analyst",
                "conversation_id": "dir:analyst",
                "phase": "waiting_model",
                "elapsed_secs": 45,
                "beat": 3
            }),
            mid
        ));
        assert_eq!(
            w.activity_of("analyst", mid).unwrap().label(),
            "Thinking… (45s)"
        );

        // Past STALE_AFTER from `start`, but still within STALE_AFTER from `mid`: entry stays live!
        let after_original_stale = start + STALE_AFTER + Duration::from_secs(60);
        assert_eq!(
            w.activity_of("analyst", after_original_stale)
                .unwrap()
                .label(),
            "Thinking… (45s)"
        );

        // Another beat updates the elapsed seconds.
        let later = mid + Duration::from_secs(15);
        assert!(w.apply(
            "turn.progress",
            &json!({
                "task_id": "t1",
                "phase": "waiting_model",
                "elapsed_secs": 60,
                "beat": 4
            }),
            later
        ));
        assert_eq!(
            w.activity_of("analyst", later).unwrap().label(),
            "Thinking… (60s)"
        );

        // If a named tool step is active, turn.progress refreshes `seen` without overwriting the tool label.
        let mut w2 = WorkingBots::default();
        w2.apply(
            "turn.step",
            &step("t2", "writer", "dir:writer", "running", "Drafting"),
            start,
        );
        assert!(w2.apply(
            "turn.progress",
            &json!({"task_id": "t2", "phase": "waiting_model", "elapsed_secs": 30}),
            mid
        ));
        assert_eq!(w2.activity_of("writer", mid).unwrap().label(), "Drafting");
        assert!(w2.activity_of("writer", after_original_stale).is_some());

        // A turn.progress before any turn.step also registers the bot if `bot` is present.
        let mut w3 = WorkingBots::default();
        assert!(w3.apply(
            "turn.progress",
            &json!({
                "task_id": "t3",
                "bot": "main",
                "conversation_id": "dir:main",
                "phase": "waiting_model",
                "elapsed_secs": 15,
                "beat": 1
            }),
            start
        ));
        assert_eq!(
            w3.activity_of("main", start).unwrap().label(),
            "Thinking… (15s)"
        );
    }

    #[test]
    fn activity_kind_maps_tools_and_labels_cleanly() {
        assert_eq!(
            classify_step_activity(Some("tavily_search"), None),
            ActivityKind::Searching
        );
        assert_eq!(
            classify_step_activity(Some("rag_search"), None),
            ActivityKind::Searching
        );
        assert_eq!(
            classify_step_activity(Some("job_search"), None),
            ActivityKind::Searching
        );
        assert_eq!(
            classify_step_activity(Some("browser_navigate"), None),
            ActivityKind::Browsing
        );
        assert_eq!(
            classify_step_activity(Some("social_extract"), None),
            ActivityKind::Browsing
        );
        assert_eq!(
            classify_step_activity(Some("project_check"), None),
            ActivityKind::Coding
        );
        assert_eq!(
            classify_step_activity(Some("file_write"), None),
            ActivityKind::Coding
        );
        assert_eq!(
            classify_step_activity(Some("mail_send"), None),
            ActivityKind::Coding
        );
        assert_eq!(
            classify_step_activity(Some("delegate"), None),
            ActivityKind::Delegating
        );
        assert_eq!(
            classify_step_activity(Some("ask_teammate"), None),
            ActivityKind::Delegating
        );
        assert_eq!(
            classify_step_activity(Some("memory_save"), None),
            ActivityKind::Thinking
        );
        // Label fallback when tool is absent.
        assert_eq!(
            classify_step_activity(None, Some("Searching the web")),
            ActivityKind::Searching
        );
        assert_eq!(
            classify_step_activity(None, Some("Handing off to Analyst")),
            ActivityKind::Delegating
        );
        assert_eq!(
            classify_step_activity(None, Some("Reading files")),
            ActivityKind::Coding
        );
        assert_eq!(
            classify_step_activity(None, Some("Thinking")),
            ActivityKind::Thinking
        );
    }

    #[test]
    fn activity_kind_tracks_steps_waiting_and_brief_done_shimmer() {
        let start = Instant::now();
        let mut w = WorkingBots::default();
        assert_eq!(w.activity_kind_of("main", start), ActivityKind::Idle);

        w.apply(
            "turn.step",
            &json!({
                "task_id": "t1",
                "bot": "main",
                "conversation_id": "dir:main",
                "phase": "running",
                "tool": "browser_snapshot",
                "label": "Reading the page",
                "call_id": "c1"
            }),
            start,
        );
        assert_eq!(w.activity_kind_of("main", start), ActivityKind::Browsing);
        assert_eq!(
            w.conversation_activity_kind("dir:main", start),
            ActivityKind::Browsing
        );
        assert_eq!(w.activity_kind_of("main", start).as_str(), "browsing");

        w.apply(
            "turn.step",
            &json!({
                "task_id": "t1",
                "bot": "main",
                "conversation_id": "dir:main",
                "phase": "waiting_approval",
                "tool": "mail_send",
                "label": "Sending an email",
                "call_id": "c2"
            }),
            start,
        );
        let kind = w.activity_kind_of("main", start);
        assert_eq!(kind, ActivityKind::WaitingApproval);
        assert_eq!(kind.as_str(), "waiting_approval");
        assert!(!kind.animates());

        // Completing the turn triggers a brief Done shimmer that does not animate as working.
        assert!(w.apply("turn.completed", &json!({"task_id": "t1"}), start));
        assert!(w.working_bots(start).is_empty());
        assert_eq!(w.activity_of("main", start), None);
        assert_eq!(w.activity_kind_of("main", start), ActivityKind::Done);
        assert_eq!(
            w.conversation_activity_kind("dir:main", start),
            ActivityKind::Done
        );
        assert!(!ActivityKind::Done.animates());

        // After DONE_SHIMMER, it settles back to Idle.
        let after_done = start + DONE_SHIMMER + Duration::from_millis(1);
        assert_eq!(w.activity_kind_of("main", after_done), ActivityKind::Idle);
        assert_eq!(
            w.conversation_activity_kind("dir:main", after_done),
            ActivityKind::Idle
        );
    }
}
