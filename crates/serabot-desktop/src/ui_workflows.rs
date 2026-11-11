//! The Workflows tab (0.29.0, Wave A4; manage actions 0.34.0): scheduled
//! routines, monitoring source health and active multi-step plans. Reads and
//! writes both go through the session worker.

use crate::ui::Icon;
use crate::ui_files::copy_script;
use dioxus::prelude::*;
use serabot_desktop::session::Session;
use serabot_desktop::workflows::{
    self, ActiveWorkflow, MonitoringSource, Routine, member_routine_slots,
};

/// Turn-on, turn-off, run, edit time and delete for one routine.
#[component]
fn RoutineRow(session: Signal<Session>, routine: Routine) -> Element {
    let mut session = session;
    let mut confirming = use_signal(|| false);
    let mut time = use_signal(|| routine.time.clone().unwrap_or_default());
    let schedule = match (&routine.time, &routine.timezone) {
        (Some(time), Some(tz)) => format!("{time} ({tz})"),
        (Some(time), None) => time.clone(),
        _ => "—".into(),
    };
    let status = if routine.enabled { "On" } else { "Off" };
    let toggle_name = routine.name.clone();
    let toggle_text_name = routine.name.clone();
    let toggle_to = !routine.enabled;
    let run_name = routine.name.clone();
    let time_name = routine.name.clone();
    let delete_name = routine.name.clone();
    rsx! {
        div { class: "wf-row wf-row-routine", role: "listitem",
            span { class: "wf-name", title: "{routine.name}", "{routine.name}" }
            span { class: "wf-meta", "{schedule}" }
            button {
                class: if routine.enabled { "wf-switch on" } else { "wf-switch" },
                r#type: "button",
                role: "switch",
                "aria-checked": if routine.enabled { "true" } else { "false" },
                "aria-label": "{routine.name} enabled",
                title: "{status}",
                onclick: move |_| session.write().save_workflow_routine(toggle_name.clone(), Some(toggle_to), None, None),
                span { class: "wf-switch-track", "aria-hidden": "true", span { class: "wf-switch-knob" } }
                span { class: "wf-switch-label", if routine.enabled { "Enabled" } else { "Paused" } }
            }
            span { class: "wf-meta", title: "{routine.next_run_at}",
                "Next {workflows::relative_time(&routine.next_run_at, workflows::now_secs())}"
            }
            if let Some(last) = routine.last_run_for.as_deref() {
                span { class: "wf-meta", "Last {last}" }
            } else {
                span { class: "wf-meta", "No run yet" }
            }
            div { class: "wf-actions",
                button { class: "btn ghost sm", r#type: "button",
                    onclick: move |_| session.write().save_workflow_routine(toggle_text_name.clone(), Some(toggle_to), None, None),
                    if routine.enabled { "Turn off" } else { "Turn on" }
                }
                button { class: "btn ghost sm", r#type: "button",
                    onclick: move |_| session.write().run_workflow_routine(run_name.clone()),
                    "Run now"
                }
                input {
                    class: "wf-time",
                    r#type: "time",
                    "aria-label": "Time for {routine.name}",
                    value: "{time}",
                    oninput: move |e| time.set(e.value()),
                }
                button { class: "btn ghost sm", r#type: "button",
                    onclick: move |_| {
                        let value = time.read().trim().to_owned();
                        if !value.is_empty() {
                            session.write().save_workflow_routine(time_name.clone(), None, Some(value), None);
                        }
                    },
                    "Save time"
                }
                if *confirming.read() {
                    button { class: "btn danger sm", r#type: "button",
                        onclick: move |_| {
                            confirming.set(false);
                            session.write().delete_workflow_routine(delete_name.clone());
                        },
                        "Confirm delete"
                    }
                    button { class: "btn ghost sm", r#type: "button",
                        onclick: move |_| confirming.set(false),
                        "Keep"
                    }
                } else {
                    button { class: "btn ghost sm", r#type: "button",
                        onclick: move |_| confirming.set(true),
                        "Delete"
                    }
                }
            }
        }
    }
}

/// Enable or disable a source. The caller's own feed also takes a new URL and
/// can be removed.
#[component]
fn SourceRow(session: Signal<Session>, source: MonitoringSource) -> Element {
    let mut session = session;
    let mut confirming = use_signal(|| false);
    let mut editing = use_signal(|| false);
    let mut url_draft = use_signal(|| source.url.clone().unwrap_or_default());
    let when = source
        .last_run
        .as_ref()
        .and_then(|run| run.finished_at.as_deref().or(run.started_at.as_deref()))
        .map(|at| workflows::relative_time(at, workflows::now_secs()));
    let status = match &source.last_run {
        Some(run) if run.ok => match &when {
            Some(when) => format!("OK · {} items · {when}", run.items_seen.max(0)),
            None => format!("OK · {} items", run.items_seen.max(0)),
        },
        Some(run) => run.error.clone().unwrap_or_else(|| "Error".into()),
        None => "No run yet".into(),
    };
    let status_class = match source.last_run.as_ref().map(|r| r.ok) {
        Some(true) => "wf-pill ok",
        Some(false) => "wf-pill err",
        None => "wf-pill muted",
    };
    let toggle_name = source.name.clone();
    let toggle_to = !source.enabled;
    let edit_name = source.name.clone();
    let delete_name = source.name.clone();
    rsx! {
        div { class: "wf-row wf-row-source", role: "listitem",
            div { class: "wf-name-col",
                span { class: "wf-name", title: "{source.name}", "{source.name}" }
                match source.url.clone() {
                    Some(url) => {
                        let host = workflows::source_host(&url).unwrap_or_else(|| url.clone());
                        rsx! {
                            span { class: "wf-sub",
                                span { class: "wf-url", title: "{url}", "{host}" }
                                button {
                                    class: "btn ghost sm wf-copy",
                                    r#type: "button",
                                    "aria-label": "Copy feed URL",
                                    title: "Copy feed URL",
                                    onclick: move |_| {
                                        let _ = dioxus::document::eval(&copy_script(&url));
                                    },
                                    Icon { name: "copy", size: 12 }
                                }
                            }
                        }
                    }
                    None => rsx! {
                        span { class: "wf-sub wf-url-missing", "URL unavailable" }
                    },
                }
            }
            span { class: "wf-meta", "{source.kind}" }
            button {
                class: if source.enabled { "wf-switch on" } else { "wf-switch" },
                r#type: "button",
                role: "switch",
                "aria-checked": if source.enabled { "true" } else { "false" },
                "aria-label": "{source.name} enabled",
                onclick: move |_| session.write().patch_source(toggle_name.clone(), Some(toggle_to), None),
                span { class: "wf-switch-track", "aria-hidden": "true", span { class: "wf-switch-knob" } }
                span { class: "wf-switch-label", if source.enabled { "Enabled" } else { "Disabled" } }
            }
            span { class: "{status_class}", title: "{status}", "{status}" }
            div { class: "wf-actions",
                if source.own {
                    button { class: "btn ghost sm", r#type: "button",
                        onclick: move |_| editing.toggle(),
                        if *editing.read() { "Close" } else { "Edit URL" }
                    }
                    if *editing.read() {
                        input {
                            class: "wf-url-input",
                            r#type: "url",
                            "aria-label": "Feed URL for {edit_name}",
                            value: "{url_draft}",
                            oninput: move |e| url_draft.set(e.value()),
                        }
                        button { class: "btn primary sm", r#type: "button",
                            onclick: move |_| {
                                let value = url_draft.read().trim().to_owned();
                                if !value.is_empty() {
                                    editing.set(false);
                                    session.write().patch_source(edit_name.clone(), None, Some(value));
                                }
                            },
                            "Save URL"
                        }
                    }
                    if *confirming.read() {
                        button { class: "btn danger sm", r#type: "button",
                            onclick: move |_| {
                                confirming.set(false);
                                session.write().delete_feed(delete_name.clone());
                            },
                            "Confirm delete"
                        }
                        button { class: "btn ghost sm", r#type: "button",
                            onclick: move |_| confirming.set(false),
                            "Keep"
                        }
                    } else {
                        button { class: "btn ghost sm", r#type: "button",
                            onclick: move |_| confirming.set(true),
                            "Delete"
                        }
                    }
                }
            }
        }
    }
}

#[component]
fn ActiveRow(plan: ActiveWorkflow, conversation: Option<String>) -> Element {
    let progress = workflows::plan_progress(plan.done, plan.total);
    let label = workflows::plan_conversation_label(conversation.as_deref(), &plan.conversation_id);
    rsx! {
        div { class: "wf-row wf-row-plan", role: "listitem",
            span { class: "wf-name wf-name-wrap", title: "{plan.goal}", "{plan.goal}" }
            span { class: "wf-meta", title: "{plan.conversation_id}", "{label}" }
            span { class: "wf-pill", "{progress}" }
            if let Some(at) = plan.updated_at.as_deref() {
                span { class: "wf-meta", title: "{at}",
                    "Updated {workflows::relative_time(at, workflows::now_secs())}"
                }
            }
        }
    }
}

#[component]
pub fn WorkflowsView(session: Signal<Session>) -> Element {
    let mut session = session;
    let mut new_slot = use_signal(String::new);
    let mut new_feed_name = use_signal(String::new);
    let mut new_feed_url = use_signal(String::new);
    let state = session.read();
    let snapshot = state.state.workflows.clone();
    let loaded = state.state.workflows_loaded;
    let error = state.state.workflows_error.clone();
    // Conversation names for the plan rows (id -> label), from the list the
    // app already holds.
    let names: std::collections::HashMap<String, String> = state
        .state
        .conversations
        .iter()
        .map(|c| (c.id.clone(), state.conversation_label(c)))
        .collect();
    drop(state);
    // Routine slots not yet scheduled, offered in the add form.
    let free_slots: Vec<String> = member_routine_slots()
        .into_iter()
        .filter(|slot| {
            !snapshot
                .routines
                .iter()
                .any(|routine| &routine.name == slot)
        })
        .collect();
    let selected_slot = if free_slots.contains(&new_slot.read()) {
        new_slot.read().clone()
    } else {
        free_slots.first().cloned().unwrap_or_default()
    };
    rsx! {
        div { class: "pane",
            div { class: "pane-head",
                div {
                    h1 { class: "pane-title", "Workflows" }
                    p { class: "muted",
                        "Your scheduled routines, watcher source health and in-progress plans."
                    }
                }
                div { class: "pane-actions",
                    button {
                        class: "btn ghost sm",
                        r#type: "button",
                        onclick: move |_| session.write().run_all_watch_sources(),
                        "Check sources now"
                    }
                    button {
                        class: "btn ghost sm",
                        r#type: "button",
                        onclick: move |_| session.write().refresh_workflows(),
                        Icon { name: "refresh", size: 14 }
                        "Refresh"
                    }
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
                        } else {
                            if snapshot.routines.is_empty() {
                                p { class: "muted", role: "status",
                                    "No routines yet. Add the morning brief or a watch below."
                                }
                            } else {
                                div { class: "wf-list", role: "list",
                                    for routine in snapshot.routines.clone() {
                                        RoutineRow { key: "{routine.name}", session, routine }
                                    }
                                }
                            }
                            if !free_slots.is_empty() {
                                form {
                                    class: "wf-form",
                                    "aria-label": "Add a routine",
                                    onsubmit: move |event| {
                                        event.prevent_default();
                                        let slot = selected_slot.clone();
                                        if !slot.is_empty() {
                                            session.write().save_workflow_routine(slot, Some(true), None, None);
                                        }
                                    },
                                    label { class: "wf-field",
                                        span { "Add a routine" }
                                        select {
                                            "aria-label": "Routine to add",
                                            onchange: move |e| new_slot.set(e.value()),
                                            for slot in free_slots.clone() {
                                                option { value: "{slot}", selected: slot == selected_slot, "{slot}" }
                                            }
                                        }
                                    }
                                    button { class: "btn primary sm", r#type: "submit", "Turn on" }
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
                        } else {
                            if snapshot.sources.is_empty() {
                                p { class: "muted", role: "status",
                                    "No watcher sources yet. Add an RSS feed below."
                                }
                            } else {
                                div { class: "wf-list", role: "list",
                                    for source in snapshot.sources.clone() {
                                        SourceRow { key: "{source.name}", session, source }
                                    }
                                }
                            }
                            form {
                                class: "wf-form",
                                "aria-label": "Add an RSS feed",
                                onsubmit: move |event| {
                                    event.prevent_default();
                                    let name = new_feed_name.read().trim().to_owned();
                                    let url = new_feed_url.read().trim().to_owned();
                                    if !name.is_empty() && !url.is_empty() {
                                        session.write().add_feed(name, url);
                                        new_feed_name.set(String::new());
                                        new_feed_url.set(String::new());
                                    }
                                },
                                label { class: "wf-field",
                                    span { "Feed name" }
                                    input {
                                        r#type: "text",
                                        maxlength: "128",
                                        value: "{new_feed_name}",
                                        oninput: move |e| new_feed_name.set(e.value()),
                                    }
                                }
                                label { class: "wf-field wf-field-wide",
                                    span { "RSS URL" }
                                    input {
                                        r#type: "url",
                                        value: "{new_feed_url}",
                                        oninput: move |e| new_feed_url.set(e.value()),
                                    }
                                }
                                button { class: "btn primary sm", r#type: "submit", "Add feed" }
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
                                    ActiveRow {
                                        key: "{plan.conversation_id}",
                                        conversation: names.get(&plan.conversation_id).cloned(),
                                        plan,
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}
