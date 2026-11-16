//! The runs sidebar (v0.50): the subagent tree of the open chat with a status
//! chip per run, parallel runs collapsed with a count, the provider hold badge
//! and a Stop or Retry on each run. The model lives in `serabot_desktop::runs`.
//!
//! Run text is server data: ids and chips are cleaned in the model, and every
//! string is drawn as plain text.

use super::*;
use serabot_desktop::runs::{self, ChipKind, RunRow, TreeLine};

/// Padding step of one tree level, in the stylesheet's `run-depth-N` classes.
fn depth_class(depth: u8) -> &'static str {
    match depth {
        0 => "run-row run-depth-0",
        1 => "run-row run-depth-1",
        2 => "run-row run-depth-2",
        _ => "run-row run-depth-3",
    }
}

/// "3 running" and "1 needs you" next to a conversation's name.
#[component]
pub(crate) fn RunCountChips(running: usize, failed: usize) -> Element {
    rsx! {
        if running > 0 {
            span { class: "chip run-count live", role: "status", "{running} running" }
        }
        if failed > 0 {
            span { class: "chip run-count bad", "{failed} to retry" }
        }
    }
}

#[component]
pub(crate) fn RunsSidebar(session: Signal<Session>) -> Element {
    let now = serabot_desktop::workflows::now_secs();
    let (lines, badge, active, attention, names) = {
        let state = session.read();
        let board = &state.state.runs;
        let lines = runs::tree(&board.rows, &state.runs_open);
        let names: std::collections::HashMap<String, String> = board
            .rows
            .iter()
            .map(|row| (row.bot.clone(), state.bot_display_name(&row.bot)))
            .collect();
        (
            lines,
            runs::hold_badge(&board.holds, now),
            runs::active_count(&board.rows),
            runs::needs_attention(&board.rows),
            names,
        )
    };
    // No runs and no hold: the sidebar has nothing to say and takes no room.
    if lines.is_empty() && badge.is_none() {
        return rsx! {};
    }
    rsx! {
        section { class: "runs-panel", "aria-label": "Subagent runs",
            div { class: "runs-head",
                h3 { class: "runs-title", "Runs" }
                if active > 0 {
                    span { class: "runs-summary", role: "status", "{active} running" }
                }
                if attention > 0 {
                    span { class: "runs-summary bad", "{attention} to retry" }
                }
            }
            if let Some(badge) = badge {
                div { class: "hold-badge", role: "status", title: "{badge.title}",
                    Icon { name: "alert-circle", size: 13 }
                    span { "{badge.label}" }
                }
            }
            ul { class: "runs-tree",
                for line in lines {
                    {match line {
                        TreeLine::Group { key, label, kind, open, depth, .. } => {
                            let toggle = key.clone();
                            rsx! {
                                li { key: "g-{key}", class: depth_class(depth),
                                    button {
                                        class: "run-group",
                                        r#type: "button",
                                        "data-tone": kind.tone(),
                                        "aria-expanded": if open { "true" } else { "false" },
                                        onclick: move |_| session.write().toggle_run_group(toggle.clone()),
                                        span { class: "run-chev", Icon { name: "chevron-right", size: 12 } }
                                        span { class: "run-chip", "data-tone": kind.tone(), "{label}" }
                                    }
                                }
                            }
                        }
                        TreeLine::Run { row, depth } => {
                            let name = names.get(&row.bot).cloned().unwrap_or_else(|| row.bot.clone());
                            rsx! { RunLine { key: "r-{row.id}", session, row, depth, name, now } }
                        }
                    }}
                }
            }
        }
    }
}

#[component]
fn RunLine(session: Signal<Session>, row: RunRow, depth: u8, name: String, now: i64) -> Element {
    let kind = row.chip.kind();
    let label = row.chip.label();
    let retry_id = row.id.clone();
    let stop_id = row.id.clone();
    let bot = row.bot.clone();
    let wait_note = row
        .until
        .as_deref()
        .filter(|_| kind == ChipKind::WaitingModel)
        .and_then(serabot_desktop::workflows::epoch_secs)
        .map(|until| format!("back in {}", runs::countdown(until - now)));
    let blocked = row.retry_blocked_note();
    let can_open = serabot_desktop::valid_id(&format!("dir:{bot}"));
    rsx! {
        li { class: depth_class(depth), "data-tone": kind.tone(),
            div { class: "run-main",
                button {
                    class: "run-open",
                    r#type: "button",
                    title: "Open {name}'s chat",
                    disabled: !can_open,
                    onclick: move |_| session.write().open_conversation(format!("dir:{bot}")),
                    span { class: "run-name", "{name}" }
                }
                span { class: "run-chip", "data-tone": kind.tone(), role: "status", "{label}" }
                if row.resume_count > 0 {
                    span { class: "run-resumes", title: "Resumed {row.resume_count} time(s)", "\u{21bb}{row.resume_count}" }
                }
                div { class: "run-actions",
                    if row.can_stop() {
                        button {
                            class: "btn ghost sm run-btn",
                            r#type: "button",
                            "aria-label": "Stop {name}",
                            onclick: move |_| session.write().stop_run(stop_id.clone()),
                            Icon { name: "stop", size: 11 }
                            "Stop"
                        }
                    }
                    if row.can_retry() {
                        button {
                            class: "btn sm run-btn",
                            r#type: "button",
                            "aria-label": "Retry {name}",
                            onclick: move |_| session.write().retry_run(retry_id.clone()),
                            Icon { name: "refresh", size: 11 }
                            "Retry"
                        }
                    }
                }
            }
            if !row.reasons.is_empty() || wait_note.is_some() || row.route.is_some() {
                div { class: "run-meta",
                    for reason in row.reasons.iter() {
                        span { key: "{reason}", class: "reason-chip", "{reason}" }
                    }
                    if let Some(note) = wait_note {
                        span { class: "run-note", "{note}" }
                    }
                    if let Some(route) = row.route.as_deref() {
                        span { class: "run-note", title: "Route", "{route}" }
                    }
                }
            }
            if let Some(note) = blocked {
                p { class: "run-blocked", "{note}" }
            }
        }
    }
}
