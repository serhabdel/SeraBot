//! The failure and wait rows of the chat (v0.50): the failed-turn card with
//! its reason chips and ways forward, the waiting-for-a-model banner with a
//! countdown, and the inline compaction note.
//!
//! The live turn has one working indicator (0.74.3): [`LiveCard`], derived
//! from the live [`Activity`](serabot_desktop::activity::Activity). At most
//! one card is ever on screen; when the turn completes it goes away and the
//! reply's TurnTrace carries the same steps.
//!
//! Every word here is fixed text or a chip from a fixed vocabulary; provider
//! text never reaches these rows.

use super::*;
use serabot_desktop::activity::{Activity, format_duration, tool_heading};
use serabot_desktop::chat_failure::FailedCard;
use std::time::Instant;

/// What the one working indicator says right now (0.74.3). Plain display
/// data only: bot and connector names, step counts, elapsed times. No
/// handoff objectives, tool arguments, queries, file contents or provider
/// error text ever reach a card.
#[derive(Clone, Debug)]
pub enum LiveCardData {
    /// No step yet, or the running step is the Thinking step: "{Bot} is
    /// thinking…" with the model chip, a live elapsed time and Stop.
    Thinking {
        bot: String,
        model: Option<String>,
        elapsed: Option<String>,
    },
    /// A tool or connection step is running: "{Bot} is {label}" with the
    /// connector chip, "Step n" or "n/total", and a live elapsed time.
    UsingTool {
        bot: String,
        label: String,
        connector: Option<String>,
        position: String,
        elapsed: Option<String>,
    },
    /// All steps are done and the reply is streaming: "{Bot} is writing
    /// your reply…" with "Drafting from N steps".
    Writing { bot: String, steps: usize },
    /// Another bot is working in this conversation: "Handed off to {bot}"
    /// with "Running in Runs — tap to open". No objective.
    Delegating { bot: String },
    /// The turn is parked waiting for a model: the existing wait line with
    /// its countdown, plus a reason chip when the last switch gave one.
    WaitingModel { line: String, reason: Option<String> },
    /// A step waits on the owner: "Needs your approval" with Approve/Deny.
    Approval { label: String, approval_id: String },
    /// A route just failed over (transient, ~6s): "{from} failed — switched
    /// to {to}". The caller only passes switches from the last few seconds.
    Failover { from: String, to: String },
    /// The turn failed: the FailedTurnCard copy, plus "Failed after
    /// {elapsed} · {n} steps done".
    Failed {
        elapsed: Option<String>,
        done: usize,
    },
}

/// Inputs for [`derive_live_card`]. Everything is already display data.
pub struct LiveCardInputs<'a> {
    pub bot_name: &'a str,
    pub model_label: Option<&'a str>,
    pub activity: &'a Activity,
    pub waiting_line: Option<&'a str>,
    pub waiting_reason: Option<&'a str>,
    /// `(from, to)` route labels of a switch that landed in the last few
    /// seconds, if any.
    pub failover: Option<(&'a str, &'a str)>,
    /// Display name of another bot working in this conversation, if any.
    pub delegate_bot: Option<&'a str>,
    /// The reply is already streaming (a live assistant message exists).
    pub streaming_reply: bool,
    /// The turn failed.
    pub failed: bool,
    pub now: Instant,
}

/// Derive the one live card from the live turn (0.74.3). Priority, so two
/// states never show at once: Failed, Approval, Waiting for a model,
/// Failover, Delegating, the running step (Thinking or Using tool), Writing,
/// then Thinking while nothing has started yet.
pub fn derive_live_card(inputs: LiveCardInputs<'_>) -> LiveCardData {
    let activity = inputs.activity;
    let now = inputs.now;
    if inputs.failed {
        return LiveCardData::Failed {
            elapsed: activity
                .completed_elapsed_ms(now)
                .map(format_duration),
            done: activity.done_count(),
        };
    }
    if let Some(step) = activity.waiting() {
        return LiveCardData::Approval {
            label: step.label.clone(),
            approval_id: step.approval_id.clone().unwrap_or_default(),
        };
    }
    if let Some(line) = inputs.waiting_line {
        return LiveCardData::WaitingModel {
            line: line.to_owned(),
            reason: inputs.waiting_reason.map(str::to_owned),
        };
    }
    if let Some((from, to)) = inputs.failover {
        return LiveCardData::Failover {
            from: from.to_owned(),
            to: to.to_owned(),
        };
    }
    if let Some(bot) = inputs.delegate_bot {
        return LiveCardData::Delegating {
            bot: bot.to_owned(),
        };
    }
    if let Some(step) = activity.running_step() {
        let elapsed = step
            .elapsed_ms(now)
            .or_else(|| activity.elapsed_ms(now))
            .map(format_duration);
        if step.label.trim().eq_ignore_ascii_case("thinking") {
            return LiveCardData::Thinking {
                bot: inputs.bot_name.to_owned(),
                model: inputs.model_label.map(str::to_owned),
                elapsed,
            };
        }
        let total = activity.steps.len().saturating_add(activity.dropped);
        let index = activity
            .steps
            .iter()
            .position(|other| other.call_id == step.call_id)
            .map(|index| index + 1)
            .unwrap_or(1);
        let position = if total > 1 {
            format!("{index}/{total}")
        } else {
            format!("Step {index}")
        };
        return LiveCardData::UsingTool {
            bot: inputs.bot_name.to_owned(),
            label: step.label.clone(),
            connector: step.title.clone().or_else(|| {
                step.tool.as_deref().map(tool_heading)
            }),
            position,
            elapsed,
        };
    }
    if inputs.streaming_reply {
        return LiveCardData::Writing {
            bot: inputs.bot_name.to_owned(),
            steps: activity.steps.len().saturating_add(activity.dropped),
        };
    }
    LiveCardData::Thinking {
        bot: inputs.bot_name.to_owned(),
        model: inputs.model_label.map(str::to_owned),
        elapsed: activity.elapsed_ms(now).map(format_duration),
    }
}

/// Scroll the Runs sidebar into view. The Runs sidebar stays the drill-down:
/// the card never repeats what a run is doing.
const RUNS_OPEN_JS: &str =
    "document.querySelector('.runs-panel')?.scrollIntoView({block:'nearest'});";

/// The one working indicator of the live turn (0.74.3). Replaces the old
/// working line and the wait banner; `failed_card` is present only for the
/// Failed state, which reuses [`FailedTurnCard`] with its own dismissal.
#[component]
pub(crate) fn LiveCard(
    mut session: Signal<Session>,
    data: LiveCardData,
    failed_card: Option<FailedCard>,
    failed_can_act: bool,
    failed_ondismiss: EventHandler<()>,
) -> Element {
    match data {
        LiveCardData::Thinking {
            bot,
            model,
            elapsed,
        } => rsx! {
            div { class: "live-card", role: "status", "aria-live": "polite",
                span { class: "live-dot", "aria-hidden": "true" }
                span { class: "live-text", "{bot} is thinking\u{2026}" }
                if let Some(model) = model {
                    span { class: "chip live-chip", "{model}" }
                }
                if let Some(elapsed) = elapsed {
                    span { class: "live-time", "{elapsed}" }
                }
                button {
                    class: "btn ghost sm",
                    r#type: "button",
                    "aria-label": "Stop generating",
                    onclick: move |_| session.write().stop_turn_or_runs(),
                    Icon { name: "stop", size: 12 }
                    "Stop"
                }
            }
        },
        LiveCardData::UsingTool {
            bot,
            label,
            connector,
            position,
            elapsed,
        } => rsx! {
            div { class: "live-card", role: "status", "aria-live": "polite",
                span { class: "live-dot", "aria-hidden": "true" }
                span { class: "live-text", "{bot} is {label}" }
                if let Some(connector) = connector {
                    span { class: "chip live-chip", "{connector}" }
                }
                span { class: "chip live-chip", "{position}" }
                if let Some(elapsed) = elapsed {
                    span { class: "live-time", "{elapsed}" }
                }
            }
        },
        LiveCardData::Writing { bot, steps } => rsx! {
            div { class: "live-card", role: "status", "aria-live": "polite",
                span { class: "live-dot", "aria-hidden": "true" }
                span { class: "live-text", "{bot} is writing your reply\u{2026}" }
                span { class: "live-sub",
                    if steps == 1 {
                        "Drafting from 1 step"
                    } else {
                        "Drafting from {steps} steps"
                    }
                }
            }
        },
        LiveCardData::Delegating { bot } => rsx! {
            div { class: "live-card", role: "status", "aria-live": "polite",
                span { class: "live-dot", "aria-hidden": "true" }
                span { class: "live-text", "Handed off to {bot}" }
                button {
                    class: "btn ghost sm",
                    r#type: "button",
                    onclick: move |_| {
                        let _ = dioxus::document::eval(RUNS_OPEN_JS);
                    },
                    "Running in Runs \u{2014} tap to open"
                }
            }
        },
        LiveCardData::WaitingModel { line, reason } => rsx! {
            div { class: "live-card", role: "status", "aria-live": "polite",
                WaitBanner { line }
                if let Some(reason) = reason {
                    span { class: "reason-chip", "{reason}" }
                }
            }
        },
        LiveCardData::Approval {
            label,
            approval_id,
        } => rsx! {
            div { class: "live-card", role: "status", "aria-live": "polite",
                span { class: "live-dot", "aria-hidden": "true" }
                span { class: "live-text", "Needs your approval" }
                span { class: "live-sub", "{label}" }
                button {
                    class: "btn primary sm",
                    r#type: "button",
                    "aria-label": "Approve this step",
                    onclick: {
                        let id = approval_id.clone();
                        move |_| session.write().decide(id.clone(), ApprovalDecision::Once)
                    },
                    "Approve"
                }
                button {
                    class: "btn danger sm",
                    r#type: "button",
                    "aria-label": "Deny this step",
                    onclick: move |_| session.write().decide(approval_id.clone(), ApprovalDecision::Deny),
                    "Deny"
                }
            }
        },
        LiveCardData::Failover { from, to } => rsx! {
            div { class: "live-card", role: "status", "aria-live": "polite",
                span { class: "live-dot", "aria-hidden": "true" }
                span { class: "live-text", "{from} failed \u{2014} switched to {to}" }
            }
        },
        LiveCardData::Failed { elapsed, done } => rsx! {
            div { class: "live-card failed", role: "status",
                p { class: "live-failed-line",
                    match elapsed {
                        Some(elapsed) => rsx! {
                            "Failed after {elapsed} \u{b7} {done} steps done"
                        },
                        None => rsx! {
                            "{done} steps done"
                        },
                    }
                }
                if let Some(card) = failed_card {
                    FailedTurnCard {
                        session,
                        card,
                        can_act: failed_can_act,
                        ondismiss: failed_ondismiss,
                    }
                }
            }
        },
    }
}

/// The failed-turn card: why it failed, and Retry on the next route, Retry
/// on another route, or Ask main. One primary action: the fallback when
/// there is one, otherwise Ask main.
#[component]
pub(crate) fn FailedTurnCard(
    session: Signal<Session>,
    card: FailedCard,
    can_act: bool,
    ondismiss: EventHandler<()>,
) -> Element {
    let mut picking = use_signal(|| false);
    let has_fallback = card.fallback.is_some();
    let line = if has_fallback {
        chat_failure::FAILED_TURN_WITH_FALLBACK
    } else {
        chat_failure::FAILED_TURN
    };
    let has_alternatives = !card.alternatives.is_empty();
    rsx! {
        div { class: "banner err failed-card", role: "alert",
            div { class: "failed-body",
                span { class: "failed-line", "{line}" }
                if !card.reasons.is_empty() {
                    div { class: "reason-chips", "aria-label": "Why it failed",
                        for reason in card.reasons.iter() {
                            span { key: "{reason}", class: "reason-chip", "{reason}" }
                        }
                    }
                }
            }
            div { class: "failed-actions",
                if let Some(offer) = card.fallback.clone() {
                    button {
                        class: "btn primary sm",
                        r#type: "button",
                        disabled: !can_act,
                        onclick: {
                            let pick = offer.clone();
                            move |_| session.write().retry_on_fallback(pick.clone())
                        },
                        "{offer.label}"
                    }
                }
                if has_alternatives {
                    button {
                        class: "btn ghost sm",
                        r#type: "button",
                        "aria-expanded": if picking() { "true" } else { "false" },
                        "aria-haspopup": "listbox",
                        disabled: !can_act,
                        onclick: move |_| picking.set(!picking()),
                        "Retry on\u{2026}"
                    }
                }
                if card.ask_main {
                    button {
                        class: if has_fallback { "btn ghost sm" } else { "btn primary sm" },
                        r#type: "button",
                        disabled: !can_act,
                        onclick: move |_| session.write().ask_main(),
                        "Ask main"
                    }
                }
                button {
                    class: "btn ghost sm chat-banner-close",
                    r#type: "button",
                    title: "Dismiss",
                    "aria-label": "Dismiss",
                    onclick: move |_| ondismiss.call(()),
                    Icon { name: "x", size: 13 }
                }
            }
            if picking() && has_alternatives {
                ul { class: "failed-alt", role: "listbox", "aria-label": "Retry on another route",
                    for offer in card.alternatives.iter().cloned() {
                        li { key: "{offer.label}", role: "option",
                            button {
                                class: "failed-alt-item",
                                r#type: "button",
                                onclick: {
                                    let offer = offer.clone();
                                    move |_| {
                                        picking.set(false);
                                        session.write().retry_on_fallback(offer.clone());
                                    }
                                },
                                "{offer.label}"
                            }
                        }
                    }
                }
            }
        }
    }
}

/// "Waiting for a model. Trying again in 4:07." The line is rebuilt every
/// second by the session's repaint, and goes away when the turn moves on.
#[component]
pub(crate) fn WaitBanner(line: String) -> Element {
    rsx! {
        div { class: "banner wait-banner", role: "status", "aria-live": "polite",
            span { class: "wait-dot", "aria-hidden": "true" }
            span { "{line}" }
        }
    }
}

/// One quiet "Earlier messages were summarized" line in the transcript.
#[component]
pub(crate) fn CompactionNote(text: String) -> Element {
    rsx! {
        div { class: "compaction-note", role: "status",
            Icon { name: "sparkles", size: 12 }
            span { "{text}" }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn activity_with(steps: &[(&str, &str, &str)]) -> Activity {
        let mut activity = Activity::default();
        for (call, phase, label) in steps {
            let mut event = json!({"task_id": "t1", "call_id": call, "phase": phase,
                "label": label, "icon": "bolt"});
            if *call == "audio" {
                event["tool"] = json!("elevenlabs_mcp_call");
            }
            activity.apply("turn.step", &event);
        }
        activity
    }

    fn inputs<'a>(activity: &'a Activity, now: Instant) -> LiveCardInputs<'a> {
        LiveCardInputs {
            bot_name: "Main",
            model_label: Some("Auto"),
            activity,
            waiting_line: None,
            waiting_reason: None,
            failover: None,
            delegate_bot: None,
            streaming_reply: false,
            failed: false,
            now,
        }
    }

    #[test]
    fn thinking_has_the_model_chip_and_stop_before_any_step() {
        let activity = Activity::default();
        let card = derive_live_card(inputs(&activity, Instant::now()));
        assert!(matches!(card, LiveCardData::Thinking { bot, model, elapsed }
            if bot == "Main" && model.as_deref() == Some("Auto") && elapsed.is_none()));
    }

    #[test]
    fn a_running_tool_step_names_its_connector_and_position() {
        let activity = activity_with(&[
            ("c1", "done", "Reading the ledger"),
            ("audio", "running", "Reading the note aloud"),
        ]);
        let card = derive_live_card(inputs(&activity, Instant::now()));
        match card {
            LiveCardData::UsingTool {
                bot,
                label,
                connector,
                position,
                elapsed,
            } => {
                assert_eq!(bot, "Main");
                assert_eq!(label, "Reading the note aloud");
                assert_eq!(connector.as_deref(), Some("ElevenLabs"));
                assert_eq!(position, "2/2");
                assert!(elapsed.is_some(), "a running step shows live elapsed");
            }
            other => panic!("expected UsingTool, got {other:?}"),
        }
    }

    #[test]
    fn a_thinking_step_is_thinking_not_a_tool() {
        let activity = activity_with(&[("think", "running", "Thinking")]);
        let card = derive_live_card(inputs(&activity, Instant::now()));
        assert!(matches!(card, LiveCardData::Thinking { .. }));
    }

    #[test]
    fn finished_steps_with_a_streaming_reply_are_writing() {
        let activity = activity_with(&[("c1", "done", "Searching the web")]);
        let mut with_reply = inputs(&activity, Instant::now());
        with_reply.streaming_reply = true;
        match derive_live_card(with_reply) {
            LiveCardData::Writing { bot, steps } => {
                assert_eq!(bot, "Main");
                assert_eq!(steps, 1);
            }
            other => panic!("expected Writing, got {other:?}"),
        }
    }

    #[test]
    fn approval_and_waiting_beat_the_running_step() {
        let mut activity = activity_with(&[("c1", "running", "Opening the vault")]);
        let waiting = json!({"task_id": "t1", "call_id": "c2", "phase": "waiting_approval",
            "label": "Opening the vault", "icon": "lock", "approval_id": "appr-1"});
        activity.apply("turn.step", &waiting);
        let card = derive_live_card(inputs(&activity, Instant::now()));
        match card {
            LiveCardData::Approval { label, approval_id } => {
                assert_eq!(label, "Opening the vault");
                assert_eq!(approval_id, "appr-1");
            }
            other => panic!("expected Approval, got {other:?}"),
        }
        let activity = activity_with(&[("c1", "running", "Searching the web")]);
        let mut with_wait = inputs(&activity, Instant::now());
        with_wait.waiting_line = Some("Waiting for a model. Trying again in 4:07.");
        with_wait.waiting_reason = Some("rate limited");
        match derive_live_card(with_wait) {
            LiveCardData::WaitingModel { line, reason } => {
                assert!(line.contains("Trying again in 4:07."));
                assert_eq!(reason.as_deref(), Some("rate limited"));
            }
            other => panic!("expected WaitingModel, got {other:?}"),
        }
    }

    #[test]
    fn failover_and_delegation_use_names_only() {
        let activity = activity_with(&[("c1", "running", "Searching the web")]);
        let mut with_failover = inputs(&activity, Instant::now());
        with_failover.failover = Some(("Anthropic", "OpenAI"));
        match derive_live_card(with_failover) {
            LiveCardData::Failover { from, to } => {
                assert_eq!(from, "Anthropic");
                assert_eq!(to, "OpenAI");
            }
            other => panic!("expected Failover, got {other:?}"),
        }
        let activity = Activity::default();
        let mut with_delegate = inputs(&activity, Instant::now());
        with_delegate.delegate_bot = Some("Analyst");
        assert!(matches!(
            derive_live_card(with_delegate),
            LiveCardData::Delegating { bot } if bot == "Analyst"
        ));
    }

    #[test]
    fn failed_reports_elapsed_and_steps_done() {
        let mut activity = Activity::default();
        let d1 = json!({"task_id": "t1", "call_id": "c1", "phase": "done",
            "label": "Searching the web", "icon": "globe", "duration_ms": 2_000});
        activity.apply("turn.step", &d1);
        let failed = json!({"task_id": "t1", "call_id": "c2", "phase": "failed",
            "label": "Reading the ledger", "icon": "bolt", "error": "timed out", "duration_ms": 3_000});
        activity.apply("turn.step", &failed);
        let mut with_failed = inputs(&activity, Instant::now());
        with_failed.failed = true;
        match derive_live_card(with_failed) {
            LiveCardData::Failed { elapsed, done } => {
                assert_eq!(elapsed.as_deref(), Some("5s"));
                assert_eq!(done, 1);
            }
            other => panic!("expected Failed, got {other:?}"),
        }
    }
}
