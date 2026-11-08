//! Live activity of the running turn, built from `turn.step` stream events.
//!
//! The server phrases every step in plain words; this module only validates
//! and orders them. Payloads are untrusted display data: strings are
//! length-bounded and control characters are dropped, and the approval id is
//! only ever handed back to the existing approvals API.

use serde_json::Value;

/// Most steps kept for one turn.
const MAX_STEPS: usize = 64;
const MAX_TEXT: usize = 120;
/// Longest reasoning text a Thinking step keeps.
const MAX_DETAIL: usize = 4_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StepPhase {
    Running,
    Done,
    Failed,
    WaitingApproval,
}

impl StepPhase {
    fn parse(value: &str) -> Option<Self> {
        match value {
            "running" => Some(Self::Running),
            "done" => Some(Self::Done),
            "failed" => Some(Self::Failed),
            "waiting_approval" => Some(Self::WaitingApproval),
            _ => None,
        }
    }
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
}

/// Steps of the most recent turn. Cleared when the owner sends again.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Activity {
    pub task_id: String,
    pub steps: Vec<Step>,
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

    /// Apply one event. Returns whether anything changed.
    pub fn apply(&mut self, kind: &str, payload: &Value) -> bool {
        if kind != "turn.step" {
            return false;
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
            self.task_id = task_id;
        }
        let icon = clean(payload, "icon").unwrap_or_else(|| "bolt".into());
        let approval_id = clean(payload, "approval_id");
        let detail = clean_detail(payload, "detail");
        if let Some(step) = self.steps.iter_mut().find(|step| step.call_id == call_id) {
            step.phase = phase;
            step.label = label;
            step.icon = icon;
            step.detail = detail.or(step.detail.take());
            step.approval_id = approval_id.or(step.approval_id.take());
            if phase != StepPhase::WaitingApproval {
                step.approval_id = None;
            }
        } else if self.steps.len() < MAX_STEPS {
            self.steps.push(Step {
                call_id,
                icon,
                label,
                phase,
                approval_id,
                detail,
            });
        }
        true
    }

    /// The step that waits on the owner, if any.
    pub fn waiting(&self) -> Option<&Step> {
        self.steps
            .iter()
            .find(|step| step.phase == StepPhase::WaitingApproval && step.approval_id.is_some())
    }

    /// "1 step" / "N steps" for the collapsed summary.
    pub fn summary(&self) -> String {
        match self.steps.len() {
            1 => "1 step".into(),
            n => format!("{n} steps"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn ev(task: &str, call: &str, phase: &str) -> Value {
        json!({"task_id": task, "call_id": call, "phase": phase,
               "label": "Opening example.com", "icon": "globe"})
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
}
