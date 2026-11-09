//! The Workflows tab (0.29.0, Wave A4): scheduled routines, monitoring source
//! health and active multi-step plans. Data comes from three read-only APIs
//! loaded through the session worker, not from the chat composer.

use crate::ui::Icon;
use dioxus::prelude::*;
use serabot_desktop::session::Session;
use serabot_desktop::workflows::{
    self, ActiveWorkflow, MonitoringSource, Routine,
};

#[component]
fn RoutineRow(routine: Routine) -> Element {
    let schedule = match (&routine.time, &routine.timezone) {
        (Some(time), Some(tz)) => format!("{time} ({tz})"),
        (Some(time), None) => time.clone(),
        _ => "—".into(),
    };
    let status = if routine.enabled { "On" } else { "Off" };
    rsx! {
        div { class: "wf-row", role: "listitem",
            span { class: "wf-name", title: "{routine.name}", "{routine.name}" }
            span { class: "wf-meta", "{schedule}" }
            span { class: if routine.enabled { "wf-pill ok" } else { "wf-pill muted" }, "{status}" }
            span { class: "wf-meta", "Next {workflows::format_next_run(&routine.next_run_at)}" }
            if let Some(last) = routine.last_run_for.as_deref() {
                span { class: "wf-meta", "Last {last}" }
            }
        }
    }
}

#[component]
fn SourceRow(source: MonitoringSource) -> Element {
    let status = match &source.last_run {
        Some(run) if run.ok => format!("OK · {} items", run.items_seen.max(0)),
        Some(run) => run
            .error
            .clone()
            .unwrap_or_else(|| "Error".into()),
        None => "No run yet".into(),
    };
    let status_class = match source.last_run.as_ref().map(|r| r.ok) {
        Some(true) => "wf-pill ok",
        Some(false) => "wf-pill err",
        None => "wf-pill muted",
    };
    rsx! {
        div { class: "wf-row", role: "listitem",
            span { class: "wf-name", title: "{source.name}", "{source.name}" }
            span { class: "wf-meta", "{source.kind}" }
            span { class: if source.enabled { "wf-pill ok" } else { "wf-pill muted" },
                if source.enabled { "Enabled" } else { "Disabled" }
            }
            span { class: "{status_class}", title: "{status}", "{status}" }
        }
    }
}

#[component]
fn ActiveRow(plan: ActiveWorkflow) -> Element {
    let progress = workflows::plan_progress(plan.done, plan.total);
    rsx! {
        div { class: "wf-row", role: "listitem",
            span { class: "wf-name", title: "{plan.goal}", "{plan.goal}" }
            span { class: "wf-meta", title: "{plan.conversation_id}", "{plan.conversation_id}" }
            span { class: "wf-pill", "{progress}" }
            if let Some(at) = plan.updated_at.as_deref() {
                span { class: "wf-meta", "Updated {at}" }
            }
        }
    }
}

#[component]
pub fn WorkflowsView(session: Signal<Session>) -> Element {
    let mut session = session;
    let state = session.read();
    let snapshot = state.state.workflows.clone();
    let loaded = state.state.workflows_loaded;
    let error = state.state.workflows_error.clone();
    drop(state);
    rsx! {
        div { class: "pane",
            div { class: "pane-head",
                div {
                    h1 { class: "pane-title", "Workflows" }
                    p { class: "muted",
                        "Your scheduled routines, watcher source health and in-progress plans."
                    }
                }
                button {
                    class: "btn ghost sm",
                    onclick: move |_| session.write().refresh_workflows(),
                    Icon { name: "refresh", size: 14 }
                    "Refresh"
                }
            }
            if let Some(message) = error.as_ref() {
                p { class: "wf-banner err", role: "alert", "{message}" }
            }
            if !loaded && error.is_none() {
                p { class: "muted", role: "status", "Loading workflows\u{2026}" }
            } else {
                div { class: "wf-body",
                    section { class: "wf-section",
                        h2 { class: "wf-section-title", "Scheduled routines" }
                        if !snapshot.routines_supported {
                            p { class: "muted", role: "status",
                                "This server does not expose /v1/routines yet."
                            }
                        } else if snapshot.routines.is_empty() {
                            p { class: "muted", role: "status", "No routines configured." }
                        } else {
                            div { class: "wf-list", role: "list",
                                for routine in snapshot.routines.clone() {
                                    RoutineRow { key: "{routine.name}", routine }
                                }
                            }
                        }
                    }
                    section { class: "wf-section",
                        h2 { class: "wf-section-title", "Source health" }
                        if !snapshot.sources_supported {
                            p { class: "muted", role: "status",
                                "This server does not expose /v1/monitoring/sources yet."
                            }
                        } else if snapshot.sources.is_empty() {
                            p { class: "muted", role: "status", "No watcher sources configured." }
                        } else {
                            div { class: "wf-list", role: "list",
                                for source in snapshot.sources.clone() {
                                    SourceRow { key: "{source.name}", source }
                                }
                            }
                        }
                    }
                    section { class: "wf-section",
                        h2 { class: "wf-section-title", "Active plans" }
                        if !snapshot.active_supported {
                            p { class: "muted", role: "status",
                                "This server does not expose /v1/workflows/active yet."
                            }
                        } else if snapshot.active.is_empty() {
                            p { class: "muted", role: "status", "No incomplete plans." }
                        } else {
                            div { class: "wf-list", role: "list",
                                for plan in snapshot.active.clone() {
                                    ActiveRow { key: "{plan.conversation_id}", plan }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}
