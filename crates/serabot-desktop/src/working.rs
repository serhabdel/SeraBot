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
/// Tasks tracked at once.
const MAX_ENTRIES: usize = 32;
const MAX_ID: usize = 80;
const MAX_LABEL: usize = 80;
/// Shown under the name when the bot is busy without a named step.
pub const WORKING_LABEL: &str = "Working…";
/// Shown while a step waits for the owner's approval.
pub const WAITING_LABEL: &str = "Waiting for approval";

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
    seen: Instant,
}

/// Tasks that are running, by bot and conversation.
#[derive(Clone, Debug, Default)]
pub struct WorkingBots {
    entries: Vec<Entry>,
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
    }

    /// Apply one stream event. Returns whether anything changed.
    pub fn apply(&mut self, kind: &str, payload: &Value, now: Instant) -> bool {
        match kind {
            "turn.step" => self.apply_step(payload, now),
            "turn.completed" | "task.failed" => {
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

    fn apply_step(&mut self, payload: &Value, now: Instant) -> bool {
        // An older server sends no bot: nothing to attribute the step to.
        let (Some(task_id), Some(bot)) = (
            clean(payload, "task_id", MAX_ID),
            clean(payload, "bot", MAX_ID),
        ) else {
            return false;
        };
        let conversation = clean(payload, "conversation_id", MAX_ID).unwrap_or_default();
        let activity = match payload.get("phase").and_then(Value::as_str) {
            Some("running" | "progress") => {
                BotActivity::Working(clean(payload, "label", MAX_LABEL))
            }
            // The turn goes on between steps: keep the ring, drop the stale name.
            Some("done" | "failed") => BotActivity::Working(None),
            Some("waiting_approval") => BotActivity::WaitingApproval,
            _ => return false,
        };
        self.prune(now);
        if let Some(entry) = self.entries.iter_mut().find(|e| e.task_id == task_id) {
            entry.bot = bot;
            entry.conversation = conversation;
            entry.activity = activity;
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
                seen: now,
            });
        }
        true
    }

    /// Forget entries that have been quiet for [`STALE_AFTER`].
    pub fn prune(&mut self, now: Instant) {
        self.entries
            .retain(|entry| now.saturating_duration_since(entry.seen) < STALE_AFTER);
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
            entry.activity.label().hash(h);
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
}
