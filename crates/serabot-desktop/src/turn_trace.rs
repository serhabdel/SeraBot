//! What each finished turn did, kept per turn so the reply owns it (0.61).
//!
//! The live `Activity` shows only the running turn and is cleared on the next
//! send. This ledger keeps, per `turn_id`, the same steps plus the memories the
//! turn used and saved (`turn.memory`), fed by the live stream and by
//! `GET /v1/conversations/{id}/activity` after a reload. The chat draws a
//! turn's trace inside the reply that carries the same `turn_id`.
//!
//! Everything here is display data from the server: bounded, control
//! characters stripped, never instructions.

use std::collections::{BTreeMap, VecDeque};

use serde_json::Value;

use crate::activity::Activity;

/// Turns kept per open conversation. Older traces fall off; their replies
/// simply show no trace.
const MAX_TURNS: usize = 64;
/// Memories kept per side of one turn (the server sends at most 8).
const MAX_MEMORY_ITEMS: usize = 8;
/// Longest excerpt shown.
const MAX_EXCERPT: usize = 120;

/// One memory a turn used or saved.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MemoryItem {
    pub id: String,
    pub kind: String,
    /// `explicit` or `extracted`.
    pub origin: String,
    /// Empty when the server withheld the text.
    pub excerpt: String,
    /// Saved but held for review (the turn read untrusted content, or it
    /// disagreed with something the owner saved).
    pub pending: bool,
    /// Older facts this one replaced (0.62), at most 2.
    pub replaces: Vec<String>,
}

/// The memory side of one turn.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TurnMemory {
    pub recalled: Vec<MemoryItem>,
    pub saved: Vec<MemoryItem>,
    pub recalled_total: usize,
    pub saved_total: usize,
}

impl TurnMemory {
    pub fn is_empty(&self) -> bool {
        self.recalled_total == 0 && self.saved_total == 0
    }

    /// Merge one `turn.memory` event: recall arrives at turn start, saves after
    /// the reply, so each side replaces only itself when present.
    fn apply(&mut self, payload: &Value) {
        let recalled = items(payload.get("recalled"));
        let saved = items(payload.get("saved"));
        let total = |key: &str, fallback: usize| {
            payload
                .get(key)
                .and_then(Value::as_u64)
                .map(|n| (n as usize).max(fallback))
                .unwrap_or(fallback)
        };
        if !recalled.is_empty() || total("recalled_total", 0) > 0 {
            self.recalled_total = total("recalled_total", recalled.len());
            self.recalled = recalled;
        }
        if !saved.is_empty() || total("saved_total", 0) > 0 {
            self.saved_total = total("saved_total", saved.len());
            self.saved = saved;
        }
    }

    /// "Used 2 memories · Remembered 1", or `None` when nothing happened.
    pub fn summary(&self) -> Option<String> {
        let mut parts = Vec::new();
        match self.recalled_total {
            0 => {}
            1 => parts.push("Used 1 memory".to_owned()),
            n => parts.push(format!("Used {n} memories")),
        }
        if self.saved_total > 0 {
            parts.push(format!("Remembered {}", self.saved_total));
        }
        let replaced: usize = self.saved.iter().map(|item| item.replaces.len()).sum();
        if replaced > 0 {
            parts.push(format!("Replaced {replaced}"));
        }
        (!parts.is_empty()).then(|| parts.join(" \u{b7} "))
    }
}

fn text(value: &Value, key: &str, max: usize) -> String {
    value
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .chars()
        .filter(|c| !c.is_control())
        .take(max)
        .collect::<String>()
        .trim()
        .to_owned()
}

fn items(value: Option<&Value>) -> Vec<MemoryItem> {
    value
        .and_then(Value::as_array)
        .map(|rows| {
            rows.iter()
                .filter_map(|row| {
                    let id = text(row, "id", 64);
                    (!id.is_empty()).then(|| MemoryItem {
                        id,
                        kind: text(row, "kind", 24),
                        origin: text(row, "origin", 24),
                        excerpt: text(row, "excerpt", MAX_EXCERPT),
                        pending: row.get("pending").and_then(Value::as_bool) == Some(true),
                        replaces: row
                            .get("replaces")
                            .and_then(Value::as_array)
                            .map(|olds| {
                                olds.iter()
                                    .filter_map(Value::as_str)
                                    .map(|old| {
                                        old.chars()
                                            .filter(|c| !c.is_control())
                                            .take(MAX_EXCERPT)
                                            .collect::<String>()
                                            .trim()
                                            .to_owned()
                                    })
                                    .filter(|old| !old.is_empty())
                                    .take(2)
                                    .collect()
                            })
                            .unwrap_or_default(),
                    })
                })
                .take(MAX_MEMORY_ITEMS)
                .collect()
        })
        .unwrap_or_default()
}

/// Context fill at turn start (0.73, `turn.context` event): estimated prompt
/// size, the routed model's window and the percent used.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TurnContext {
    pub prompt_chars_est: u64,
    pub window: Option<u64>,
    pub percent: Option<u32>,
}

/// Steps and memories of one turn.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TurnTrace {
    pub activity: Activity,
    pub memory: TurnMemory,
    pub context: Option<TurnContext>,
}

impl TurnTrace {
    pub fn is_empty(&self) -> bool {
        self.activity.is_empty() && self.memory.is_empty()
    }

    /// "4 steps, 12s · Used 2 memories · Remembered 1".
    pub fn summary(&self) -> String {
        let mut parts = Vec::new();
        if !self.activity.is_empty() {
            parts.push(self.activity.summary());
        }
        if let Some(memory) = self.memory.summary() {
            parts.push(memory);
        }
        parts.join(" \u{b7} ")
    }
}

/// Per-turn traces of the open conversation.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TurnLedger {
    conversation: String,
    turns: BTreeMap<String, TurnTrace>,
    order: VecDeque<String>,
    /// Bumped on every change, so views can key a cache on it.
    revision: u64,
}

fn turn_id(payload: &Value) -> Option<String> {
    let id = payload.get("turn_id")?.as_str()?;
    (!id.is_empty() && id.len() <= 64 && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-'))
        .then(|| id.to_owned())
}

impl TurnLedger {
    /// Start over for `conversation` (a switch, or a fresh history load).
    pub fn reset(&mut self, conversation: &str) {
        self.conversation = conversation.to_owned();
        self.turns.clear();
        self.order.clear();
        self.revision = self.revision.wrapping_add(1);
    }

    pub fn conversation(&self) -> &str {
        &self.conversation
    }

    pub fn revision(&self) -> u64 {
        self.revision
    }

    pub fn get(&self, turn_id: &str) -> Option<&TurnTrace> {
        self.turns.get(turn_id).filter(|trace| !trace.is_empty())
    }

    /// Context of the newest turn (0.73, 0.74.3): `None` when the newest
    /// turn reported none, even when an older turn did. Showing an older
    /// turn's context under a newer reply would describe the wrong turn.
    pub fn latest_context(&self) -> Option<&TurnContext> {
        let newest = self.order.back()?;
        self.turns.get(newest).and_then(|trace| trace.context.as_ref())
    }

    /// Apply one stream or history event. Returns whether anything changed.
    /// Events of another conversation, child tasks and unknown kinds are
    /// ignored.
    pub fn apply(&mut self, kind: &str, payload: &Value) -> bool {
        if kind != "turn.step" && kind != "turn.memory" && kind != "turn.context" {
            return false;
        }
        if payload.get("parent_task_id").is_some()
            || payload.get("conversation_id").and_then(Value::as_str)
                != Some(self.conversation.as_str())
        {
            return false;
        }
        let Some(turn) = turn_id(payload) else {
            return false;
        };
        if !self.turns.contains_key(&turn) {
            while self.order.len() >= MAX_TURNS {
                if let Some(oldest) = self.order.pop_front() {
                    self.turns.remove(&oldest);
                }
            }
            self.order.push_back(turn.clone());
        }
        let trace = self.turns.entry(turn).or_default();
        let changed = if kind == "turn.step" {
            trace.activity.apply(kind, payload)
        } else if kind == "turn.context" {
            let before = trace.context.clone();
            let est = payload
                .get("prompt_chars_est")
                .and_then(Value::as_u64)
                .unwrap_or(0);
            let window = payload.get("window").and_then(Value::as_u64);
            let percent = payload
                .get("percent")
                .and_then(Value::as_u64)
                .map(|p| p as u32);
            trace.context = (est > 0 || window.is_some()).then_some(TurnContext {
                prompt_chars_est: est,
                window,
                percent,
            });
            trace.context != before
        } else {
            let before = trace.memory.clone();
            trace.memory.apply(payload);
            trace.memory != before
        };
        if changed {
            self.revision = self.revision.wrapping_add(1);
        }
        changed
    }

    /// Replace the ledger with `GET /v1/conversations/{id}/activity`
    /// (`{"steps": [{"kind", "payload"}, ...]}`, oldest first).
    pub fn load(&mut self, conversation: &str, body: &Value) {
        self.reset(conversation);
        let Some(events) = body.get("steps").and_then(Value::as_array) else {
            return;
        };
        for event in events {
            let kind = event
                .get("kind")
                .and_then(Value::as_str)
                .unwrap_or_default();
            if let Some(payload) = event.get("payload") {
                self.apply(kind, payload);
            }
        }
    }
}

/// For each message index, the turn whose trace that message shows: the last
/// non-user message of each turn (a turn that sent several assistant messages
/// shows its trace once, on the final one).
pub fn anchors<'a>(messages: impl Iterator<Item = (bool, Option<&'a str>)>) -> Vec<Option<String>> {
    let rows: Vec<(bool, Option<&str>)> = messages.collect();
    let mut out = vec![None; rows.len()];
    let mut seen = std::collections::HashSet::new();
    for (index, (user, turn)) in rows.iter().enumerate().rev() {
        if *user {
            continue;
        }
        if let Some(turn) = turn
            && seen.insert(*turn)
        {
            out[index] = Some((*turn).to_owned());
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn step(turn: &str, call: &str, phase: &str) -> Value {
        json!({
            "task_id": format!("task-{turn}"),
            "turn_id": turn,
            "conversation_id": "dir:main",
            "call_id": call,
            "label": "Searching the web",
            "phase": phase,
            "duration_ms": 1200,
        })
    }

    #[test]
    fn latest_context_is_the_newest_turns_or_nothing() {
        let mut ledger = TurnLedger::default();
        ledger.reset("dir:main");
        assert!(ledger.latest_context().is_none());
        let base = json!({"turn_id": "t1", "conversation_id": "dir:main", "prompt_chars_est": 1000, "window": 200_000, "percent": 1});
        assert!(ledger.apply("turn.context", &base));
        assert_eq!(
            ledger.latest_context().map(|c| c.prompt_chars_est),
            Some(1000)
        );
        // A newer turn with no context hides the older turn's: the context
        // line must describe the reply it sits under, not a previous turn.
        assert!(ledger.apply("turn.step", &step("t2", "a", "done")));
        assert!(ledger.latest_context().is_none());
        let mut newer = base.clone();
        newer["turn_id"] = json!("t2");
        newer["prompt_chars_est"] = json!(2000);
        assert!(ledger.apply("turn.context", &newer));
        assert_eq!(
            ledger.latest_context().map(|c| c.prompt_chars_est),
            Some(2000)
        );
    }

    #[test]
    fn steps_stay_with_their_own_turn() {
        let mut ledger = TurnLedger::default();
        ledger.reset("dir:main");
        assert!(ledger.apply("turn.step", &step("t1", "a", "done")));
        assert!(ledger.apply("turn.step", &step("t2", "b", "running")));
        assert!(ledger.apply("turn.step", &step("t2", "b", "done")));
        assert_eq!(ledger.get("t1").unwrap().activity.steps.len(), 1);
        assert_eq!(ledger.get("t2").unwrap().activity.steps.len(), 1);
        assert!(ledger.get("t3").is_none());
    }

    #[test]
    fn other_conversations_and_child_tasks_are_ignored() {
        let mut ledger = TurnLedger::default();
        ledger.reset("dir:main");
        let mut other = step("t1", "a", "done");
        other["conversation_id"] = json!("dir:other");
        assert!(!ledger.apply("turn.step", &other));
        let mut child = step("t1", "a", "done");
        child["parent_task_id"] = json!("p");
        assert!(!ledger.apply("turn.step", &child));
        assert!(!ledger.apply("turn.completed", &step("t1", "a", "done")));
    }

    #[test]
    fn memory_sides_merge_and_summarize() {
        let mut ledger = TurnLedger::default();
        ledger.reset("dir:main");
        let base = json!({"turn_id": "t1", "conversation_id": "dir:main", "task_id": "x"});
        let mut recall = base.clone();
        recall["recalled"] = json!([
            {"id": "m1", "kind": "fact", "origin": "explicit", "excerpt": "Likes tea\u{7}"},
            {"id": "m2", "kind": "fact", "origin": "extracted"}
        ]);
        recall["recalled_total"] = json!(2);
        let mut save = base;
        save["saved"] = json!([{"id": "m3", "kind": "fact", "origin": "extracted", "excerpt": "Lives in Porto", "pending": true, "replaces": ["Lives in Lisbon", "", 7]}]);
        save["saved_total"] = json!(1);
        ledger.apply("turn.memory", &recall);
        ledger.apply("turn.memory", &save);
        let trace = ledger.get("t1").unwrap();
        assert_eq!(trace.memory.recalled[0].excerpt, "Likes tea");
        assert!(trace.memory.recalled[1].excerpt.is_empty());
        assert!(trace.memory.saved[0].pending);
        assert_eq!(
            trace.memory.saved[0].replaces,
            vec!["Lives in Lisbon".to_owned()]
        );
        assert_eq!(
            trace.summary(),
            "Used 2 memories \u{b7} Remembered 1 \u{b7} Replaced 1"
        );
    }

    #[test]
    fn the_ledger_is_bounded() {
        let mut ledger = TurnLedger::default();
        ledger.reset("dir:main");
        for n in 0..(MAX_TURNS + 5) {
            ledger.apply("turn.step", &step(&format!("t{n}"), "a", "done"));
        }
        assert!(ledger.get("t0").is_none());
        assert!(ledger.get(&format!("t{}", MAX_TURNS + 4)).is_some());
    }

    #[test]
    fn history_load_rebuilds_traces() {
        let mut ledger = TurnLedger::default();
        ledger.reset("dir:old");
        ledger.load(
            "dir:main",
            &json!({"steps": [
                {"id": 1, "kind": "turn.step", "payload": step("t1", "a", "done")},
                {"id": 2, "kind": "turn.memory", "payload": {"turn_id": "t1", "conversation_id": "dir:main", "recalled": [{"id": "m1"}], "recalled_total": 1}}
            ]}),
        );
        let trace = ledger.get("t1").unwrap();
        assert_eq!(trace.activity.steps.len(), 1);
        assert_eq!(trace.memory.recalled_total, 1);
    }

    #[test]
    fn a_turn_anchors_on_its_last_reply() {
        let rows = vec![
            (true, Some("t1")),
            (false, Some("t1")),
            (false, Some("t1")),
            (true, Some("t2")),
            (false, None),
        ];
        let got = anchors(rows.into_iter());
        assert_eq!(got, vec![None, None, Some("t1".to_owned()), None, None]);
    }

    #[test]
    fn unsafe_turn_ids_are_rejected() {
        let mut ledger = TurnLedger::default();
        ledger.reset("dir:main");
        assert!(!ledger.apply("turn.step", &step("bad id!", "a", "done")));
    }
}
