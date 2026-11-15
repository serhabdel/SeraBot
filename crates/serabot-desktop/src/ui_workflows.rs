//! The Workflows tab (0.29.0, Wave A4; manage actions 0.34.0; Visual Workflow
//! Studio 0.45.0 Wave 8): interactive node-flow canvas, scheduled routines,
//! monitoring source health and active multi-step plans.

use crate::ui::Icon;
use crate::ui_files::{CONFIRM_RESET_JS, copy_script};
use dioxus::prelude::*;
use serabot_desktop::session::Session;
use serabot_desktop::workflows::{
    self, ActiveWorkflow, MonitoringSource, Routine, WorkflowGraph, WorkflowNodeKind,
    WorkflowPlanStep, member_routine_slots,
};

const TIMEZONE_PRESETS: &[&str] = &[
    "UTC",
    "Europe/Paris",
    "Africa/Casablanca",
    "America/New_York",
    "Europe/London",
    "Asia/Tokyo",
];

#[derive(Clone, Copy, PartialEq, Eq)]
enum WorkflowFilter {
    All,
    Enabled,
    Failing,
}

#[derive(Clone, PartialEq, Eq)]
enum SelectedWorkflow {
    Routine(String),
    Source(String),
    Plan(String),
}

fn node_icon_name(kind: WorkflowNodeKind) -> &'static str {
    match kind {
        WorkflowNodeKind::Trigger => "settings",
        WorkflowNodeKind::Source => "search",
        WorkflowNodeKind::Bot => "robot",
        WorkflowNodeKind::Gate => "check",
        WorkflowNodeKind::Output => "send",
        WorkflowNodeKind::Step => "chevron-right",
    }
}

/// Horizontal visual node flow diagram with SVG directional connectors.
#[component]
fn WorkflowCanvas(
    graph: WorkflowGraph,
    selected_node_id: Signal<Option<String>>,
    on_node_click: EventHandler<String>,
) -> Element {
    let mut selected_node_id = selected_node_id;
    let status_cls = if graph.active {
        "wf-pill ok"
    } else {
        "wf-pill muted"
    };
    let status_text = if graph.active { "Active" } else { "Paused" };
    let node_count = graph.nodes.len();
    rsx! {
        section { class: "wf-canvas", "aria-label": "Visual workflow node canvas",
            div { class: "wf-canvas-head",
                div { class: "wf-canvas-title-wrap",
                    span { class: "wf-canvas-eyebrow", "Visual Flow" }
                    h2 { class: "wf-canvas-title", "{graph.title}" }
                    span { class: "wf-meta", "{graph.subtitle}" }
                }
                span { class: "{status_cls}", "{status_text}" }
            }
            div { class: "wf-flow-row", role: "list",
                for (idx, node) in graph.nodes.into_iter().enumerate() {
                    {
                        let node_id = node.id.clone();
                        let click_id = node.id.clone();
                        let is_selected = selected_node_id.read().as_deref() == Some(node.id.as_str());
                        let kind_mod = node.kind.css_modifier();
                        let status_mod = node.status.css_modifier();
                        let node_cls = if is_selected {
                            format!("wf-node wf-node--{kind_mod} selected")
                        } else {
                            format!("wf-node wf-node--{kind_mod}")
                        };
                        let status_pill_cls = format!("wf-node-status wf-node-status--{status_mod}");
                        let icon = node_icon_name(node.kind);
                        let kind_label = node.kind.label();
                        let status_label = node.status.label();
                        rsx! {
                            button {
                                key: "{node_id}",
                                class: "{node_cls}",
                                r#type: "button",
                                role: "listitem",
                                onclick: move |_| {
                                    selected_node_id.set(Some(click_id.clone()));
                                    on_node_click.call(click_id.clone());
                                },
                                div { class: "wf-node-head",
                                    span { class: "wf-node-badge",
                                        Icon { name: icon, size: 13 }
                                        "{kind_label}"
                                    }
                                    span { class: "{status_pill_cls}", "{status_label}" }
                                }
                                div { class: "wf-node-title", "{node.title}" }
                                div { class: "wf-node-sub", title: "{node.subtitle}", "{node.subtitle}" }
                                div { class: "wf-node-foot",
                                    span { class: "wf-node-tag", "{node.badge}" }
                                }
                            }
                            if idx + 1 < node_count {
                                div { class: "wf-connector", "aria-hidden": "true",
                                    svg {
                                        class: "wf-connector-svg",
                                        view_box: "0 0 36 20",
                                        width: "36",
                                        height: "20",
                                        path {
                                            d: "M2 10h26M22 4l6 6-6 6",
                                            fill: "none",
                                            stroke: "currentColor",
                                            "stroke-width": "1.8",
                                            "stroke-linecap": "round",
                                            "stroke-linejoin": "round",
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
}

/// Routine card with Visual Flow selection, structured editor, Run Now, and Delete.
#[component]
fn RoutineCard(
    session: Signal<Session>,
    routine: Routine,
    selected: bool,
    editing_routine: Signal<Option<String>>,
    on_select: EventHandler<String>,
) -> Element {
    let mut session = session;
    let mut editing_routine = editing_routine;
    let mut confirming = use_signal(|| false);

    let server_time = routine.time.clone().unwrap_or_default();
    let server_tz = routine.timezone.clone().unwrap_or_else(|| "UTC".into());
    let server_label = routine.label.clone().unwrap_or_default();
    let server_prompt = routine.prompt.clone().unwrap_or_default();

    let mut last_sig = use_signal(|| {
        (
            server_time.clone(),
            server_tz.clone(),
            server_label.clone(),
            server_prompt.clone(),
            routine.enabled,
        )
    });
    let mut draft_time = use_signal(|| server_time.clone());
    let mut draft_tz = use_signal(|| server_tz.clone());
    let mut draft_label = use_signal(|| server_label.clone());
    let mut draft_prompt = use_signal(|| server_prompt.clone());
    let mut draft_enabled = use_signal(|| routine.enabled);

    let cur_sig = (
        server_time.clone(),
        server_tz.clone(),
        server_label.clone(),
        server_prompt.clone(),
        routine.enabled,
    );
    if *last_sig.read() != cur_sig {
        last_sig.set(cur_sig);
        draft_time.set(server_time.clone());
        draft_tz.set(server_tz.clone());
        draft_label.set(server_label.clone());
        draft_prompt.set(server_prompt.clone());
        draft_enabled.set(routine.enabled);
    }

    let is_editing = editing_routine.read().as_deref() == Some(routine.name.as_str());
    let display_title = routine
        .label
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or(&routine.name)
        .to_owned();
    let schedule = match (&routine.time, &routine.timezone) {
        (Some(t), Some(tz)) => format!("{t} ({tz})"),
        (Some(t), None) => t.clone(),
        _ => "Unscheduled".into(),
    };
    let preset_value = if TIMEZONE_PRESETS.contains(&draft_tz.read().as_str()) {
        draft_tz.read().clone()
    } else {
        "__custom__".into()
    };

    let select_name = routine.name.clone();
    let edit_name = routine.name.clone();
    let toggle_name = routine.name.clone();
    let toggle_to = !routine.enabled;
    let run_name = routine.name.clone();
    let save_name = routine.name.clone();
    let delete_name = routine.name.clone();
    let card_cls = if selected {
        "wf-card selected"
    } else {
        "wf-card"
    };

    rsx! {
        div { class: "{card_cls}", role: "listitem",
            div { class: "wf-card-head",
                div { class: "wf-name-col",
                    div { class: "wf-card-title-row",
                        span {
                            class: if routine.enabled { "wf-dot ok" } else { "wf-dot muted" },
                            "aria-hidden": "true",
                        }
                        span { class: "wf-name", title: "{routine.name}", "{display_title}" }
                        if routine.label.is_some() {
                            span { class: "wf-pill muted", "{routine.name}" }
                        }
                    }
                    span { class: "wf-meta",
                        "{schedule} · Next {workflows::relative_time(&routine.next_run_at, workflows::now_secs())}"
                        if let Some(last) = routine.last_run_for.as_deref() {
                            " · Last {last}"
                        }
                    }
                }
                button {
                    class: if routine.enabled { "wf-switch on" } else { "wf-switch" },
                    r#type: "button",
                    role: "switch",
                    "aria-checked": if routine.enabled { "true" } else { "false" },
                    "aria-label": "{routine.name} enabled",
                    onclick: move |_| session.write().save_workflow_routine(toggle_name.clone(), Some(toggle_to), None, None),
                    span { class: "wf-switch-track", "aria-hidden": "true", span { class: "wf-switch-knob" } }
                    span { class: "wf-switch-label", if routine.enabled { "Enabled" } else { "Paused" } }
                }
            }
            div { class: "wf-card-actions",
                button {
                    class: if selected { "btn secondary sm" } else { "btn ghost sm" },
                    r#type: "button",
                    onclick: move |_| on_select.call(select_name.clone()),
                    "Visual Flow"
                }
                button {
                    class: if is_editing { "btn primary sm" } else { "btn ghost sm" },
                    r#type: "button",
                    onclick: move |_| {
                        if is_editing {
                            editing_routine.set(None);
                        } else {
                            editing_routine.set(Some(edit_name.clone()));
                        }
                    },
                    if is_editing { "Close Editor" } else { "Edit" }
                }
                button {
                    class: "btn ghost sm",
                    r#type: "button",
                    onclick: move |_| session.write().run_workflow_routine(run_name.clone()),
                    "Run Now"
                }
                if *confirming.read() {
                    button {
                        class: "btn danger sm",
                        r#type: "button",
                        onclick: move |_| {
                            confirming.set(false);
                            session.write().delete_workflow_routine(delete_name.clone());
                        },
                        "Confirm delete"
                    }
                    button {
                        class: "btn ghost sm",
                        r#type: "button",
                        onclick: move |_| confirming.set(false),
                        "Keep"
                    }
                } else {
                    button {
                        class: "btn ghost sm",
                        r#type: "button",
                        onclick: move |_| confirming.set(true),
                        "Delete"
                    }
                }
            }
            if is_editing {
                div { class: "wf-editor",
                    div { class: "wf-editor-grid",
                        label { class: "wf-field",
                            span { "Custom Label" }
                            input {
                                r#type: "text",
                                maxlength: "120",
                                placeholder: "{routine.name}",
                                value: "{draft_label}",
                                oninput: move |e| draft_label.set(e.value()),
                            }
                        }
                        label { class: "wf-field",
                            span { "Daily Time (HH:MM)" }
                            input {
                                class: "wf-time",
                                r#type: "time",
                                value: "{draft_time}",
                                oninput: move |e| draft_time.set(e.value()),
                            }
                        }
                        label { class: "wf-field",
                            span { "Timezone Preset" }
                            select {
                                value: "{preset_value}",
                                onchange: move |e| {
                                    let val = e.value();
                                    if val != "__custom__" {
                                        draft_tz.set(val);
                                    }
                                },
                                for tz in TIMEZONE_PRESETS {
                                    option { value: "{tz}", selected: preset_value == *tz, "{tz}" }
                                }
                                option { value: "__custom__", selected: preset_value == "__custom__", "Custom IANA\u{2026}" }
                            }
                        }
                        label { class: "wf-field",
                            span { "IANA Timezone" }
                            input {
                                r#type: "text",
                                placeholder: "Europe/Paris",
                                value: "{draft_tz}",
                                oninput: move |e| draft_tz.set(e.value()),
                            }
                        }
                    }
                    label { class: "wf-field wf-field-full",
                        span { "Custom Instructions / Prompt" }
                        textarea {
                            class: "wf-textarea",
                            rows: "2",
                            placeholder: "Optional instructions for this routine\u{2026}",
                            value: "{draft_prompt}",
                            oninput: move |e| draft_prompt.set(e.value()),
                        }
                    }
                    div { class: "wf-editor-foot",
                        label { class: "wf-toggle",
                            input {
                                r#type: "checkbox",
                                checked: *draft_enabled.read(),
                                onchange: move |e| draft_enabled.set(e.checked()),
                            }
                            span { "Routine enabled" }
                        }
                        button {
                            class: "btn primary sm",
                            r#type: "button",
                            onclick: move |_| {
                                let t = draft_time.read().trim().to_owned();
                                let tz = draft_tz.read().trim().to_owned();
                                let en = *draft_enabled.read();
                                session.write().save_workflow_routine(
                                    save_name.clone(),
                                    Some(en),
                                    (!t.is_empty()).then_some(t),
                                    (!tz.is_empty()).then_some(tz),
                                );
                                editing_routine.set(None);
                            },
                            "Save Changes"
                        }
                    }
                }
            }
        }
    }
}

/// Monitoring source card with status dot, Visual Flow select, inline URL edit, and Delete.
#[component]
fn SourceCard(
    session: Signal<Session>,
    source: MonitoringSource,
    selected: bool,
    editing_source: Signal<Option<String>>,
    on_select: EventHandler<String>,
) -> Element {
    let mut session = session;
    let mut editing_source = editing_source;
    let mut confirming = use_signal(|| false);
    let mut copied = use_signal(|| false);
    let mut url_error = use_signal(|| Option::<String>::None);

    let server_url = source.url.clone().unwrap_or_default();
    let mut last_server_url = use_signal(|| server_url.clone());
    let mut url_draft = use_signal(|| server_url.clone());
    if *last_server_url.read() != server_url {
        last_server_url.set(server_url.clone());
        url_draft.set(server_url.clone());
        url_error.set(None);
    }

    let is_editing = editing_source.read().as_deref() == Some(source.name.as_str());
    let when = source
        .last_run
        .as_ref()
        .and_then(|run| run.finished_at.as_deref().or(run.started_at.as_deref()))
        .map(|at| workflows::relative_time(at, workflows::now_secs()));
    let items_seen = source
        .last_run
        .as_ref()
        .map(|r| r.items_seen.max(0))
        .unwrap_or(0);
    let (dot_cls, status_cls, status_text) = match &source.last_run {
        Some(run) if run.ok => (
            "wf-dot ok",
            "wf-pill ok",
            match &when {
                Some(w) => format!("{items_seen} items · {w}"),
                None => format!("{items_seen} items"),
            },
        ),
        Some(run) => (
            "wf-dot err",
            "wf-pill err",
            run.error.clone().unwrap_or_else(|| "Error".into()),
        ),
        None => ("wf-dot muted", "wf-pill muted", "No run yet".into()),
    };

    let select_name = source.name.clone();
    let toggle_name = source.name.clone();
    let toggle_to = !source.enabled;
    let edit_name = source.name.clone();
    let save_name = source.name.clone();
    let delete_name = source.name.clone();
    let draft_trimmed = url_draft.read().trim().to_owned();
    let draft_valid = workflows::is_valid_feed_url(&draft_trimmed);
    let draft_dirty = draft_trimmed != server_url;
    let card_cls = if selected {
        "wf-card selected"
    } else {
        "wf-card"
    };

    rsx! {
        div { class: "{card_cls}", role: "listitem",
            div { class: "wf-card-head",
                div { class: "wf-name-col",
                    div { class: "wf-card-title-row",
                        span { class: "{dot_cls}", "aria-hidden": "true" }
                        span { class: "wf-name", title: "{source.name}", "{source.name}" }
                        span { class: "wf-pill muted", "{source.kind}" }
                    }
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
                                        title: if copied() { "Copied" } else { "Copy feed URL" },
                                        onclick: move |_| {
                                            let _ = document::eval(&copy_script(&url));
                                            copied.set(true);
                                            spawn(async move {
                                                let mut eval = document::eval(CONFIRM_RESET_JS);
                                                let _ = eval.recv::<bool>().await;
                                                copied.set(false);
                                            });
                                        },
                                        Icon { name: if copied() { "check" } else { "copy" }, size: 12 }
                                    }
                                }
                            }
                        }
                        None => rsx! {
                            span { class: "wf-sub wf-url-missing", "URL unavailable" }
                        },
                    }
                }
                div { class: "wf-card-right",
                    span { class: "{status_cls}", title: "{status_text}", "{status_text}" }
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
                }
            }
            div { class: "wf-card-actions",
                button {
                    class: if selected { "btn secondary sm" } else { "btn ghost sm" },
                    r#type: "button",
                    onclick: move |_| on_select.call(select_name.clone()),
                    "Visual Flow"
                }
                if source.own {
                    button {
                        class: if is_editing { "btn primary sm" } else { "btn ghost sm" },
                        r#type: "button",
                        onclick: move |_| {
                            url_error.set(None);
                            if is_editing {
                                editing_source.set(None);
                            } else {
                                editing_source.set(Some(edit_name.clone()));
                            }
                        },
                        if is_editing { "Close" } else { "Edit URL" }
                    }
                    if *confirming.read() {
                        button {
                            class: "btn danger sm",
                            r#type: "button",
                            onclick: move |_| {
                                confirming.set(false);
                                session.write().delete_feed(delete_name.clone());
                            },
                            "Confirm delete"
                        }
                        button {
                            class: "btn ghost sm",
                            r#type: "button",
                            onclick: move |_| confirming.set(false),
                            "Keep"
                        }
                    } else {
                        button {
                            class: "btn ghost sm",
                            r#type: "button",
                            onclick: move |_| confirming.set(true),
                            "Delete"
                        }
                    }
                }
            }
            if source.own && is_editing {
                div { class: "wf-editor",
                    div { class: "wf-editor-row",
                        input {
                            class: "wf-url-input",
                            r#type: "url",
                            placeholder: "https://...",
                            "aria-label": "Feed URL for {save_name}",
                            value: "{url_draft}",
                            oninput: move |e| {
                                url_error.set(None);
                                url_draft.set(e.value());
                            },
                        }
                        button {
                            class: "btn primary sm",
                            r#type: "button",
                            disabled: !draft_dirty || !draft_valid,
                            onclick: move |_| {
                                let value = url_draft.read().trim().to_owned();
                                if !workflows::is_valid_feed_url(&value) {
                                    url_error.set(Some("Enter an http:// or https:// feed URL.".into()));
                                    return;
                                }
                                editing_source.set(None);
                                url_error.set(None);
                                session.write().patch_source(save_name.clone(), None, Some(value));
                            },
                            "Save URL"
                        }
                    }
                    if let Some(err) = url_error.read().as_ref() {
                        p { class: "wf-inline-err", role: "alert", "{err}" }
                    }
                }
            }
        }
    }
}

/// Interactive multi-step plan card with progress bar, step checklist, and Open in Chat.
#[component]
fn ActivePlanCard(
    session: Signal<Session>,
    plan: ActiveWorkflow,
    conversation: Option<String>,
    selected: bool,
    on_select: EventHandler<String>,
) -> Element {
    let mut session = session;
    let mut draft_steps = use_signal(|| plan.items.clone());
    let mut new_step_text = use_signal(String::new);
    let mut last_conv = use_signal(|| plan.conversation_id.clone());
    if *last_conv.read() != plan.conversation_id {
        last_conv.set(plan.conversation_id.clone());
        draft_steps.set(plan.items.clone());
    }

    let steps = draft_steps.read().clone();
    let total = if steps.is_empty() {
        plan.total
    } else {
        steps.len()
    };
    let done = if steps.is_empty() {
        plan.done
    } else {
        steps.iter().filter(|s| s.done).count()
    };
    let pct = (done * 100).checked_div(total).unwrap_or(0).min(100);
    let progress = workflows::plan_progress(done, total);
    let label = workflows::plan_conversation_label(conversation.as_deref(), &plan.conversation_id);
    let conv_id_open = plan.conversation_id.clone();
    let conv_id_select = plan.conversation_id.clone();
    let card_cls = if selected {
        "wf-card selected"
    } else {
        "wf-card"
    };

    rsx! {
        div { class: "{card_cls}", role: "listitem",
            div { class: "wf-card-head",
                div { class: "wf-name-col",
                    span { class: "wf-name wf-name-wrap", title: "{plan.goal}", "{plan.goal}" }
                    if let Some(at) = plan.updated_at.as_deref() {
                        span { class: "wf-meta", title: "{at}",
                            "Updated {workflows::relative_time(at, workflows::now_secs())}"
                        }
                    }
                }
                span { class: "wf-pill", "{progress}" }
            }
            div { class: "wf-progress-bar", "aria-hidden": "true",
                div { class: "wf-progress-fill", style: "width: {pct}%;" }
            }
            if !steps.is_empty() {
                div { class: "wf-plan-steps", role: "list",
                    for (idx, step) in steps.iter().enumerate() {
                        {
                            let step_title = step.title.clone();
                            let step_done = step.done;
                            rsx! {
                                label { key: "{idx}", class: if step_done { "wf-plan-step done" } else { "wf-plan-step" }, role: "listitem",
                                    input {
                                        r#type: "checkbox",
                                        checked: step_done,
                                        onchange: move |_| {
                                            let mut next = draft_steps.read().clone();
                                            if let Some(item) = next.get_mut(idx) {
                                                item.done = !item.done;
                                            }
                                            draft_steps.set(next);
                                        },
                                    }
                                    span { "{step_title}" }
                                }
                            }
                        }
                    }
                }
            }
            div { class: "wf-plan-add-row",
                input {
                    class: "wf-input",
                    r#type: "text",
                    placeholder: "Add step\u{2026}",
                    value: "{new_step_text}",
                    oninput: move |e| new_step_text.set(e.value()),
                }
                button {
                    class: "btn ghost sm",
                    r#type: "button",
                    disabled: new_step_text.read().trim().is_empty(),
                    onclick: move |_| {
                        let title = new_step_text.read().trim().to_owned();
                        if !title.is_empty() {
                            let mut next = draft_steps.read().clone();
                            next.push(WorkflowPlanStep { title, done: false });
                            draft_steps.set(next);
                            new_step_text.set(String::new());
                        }
                    },
                    "Add Step"
                }
            }
            div { class: "wf-card-actions",
                button {
                    class: if selected { "btn secondary sm" } else { "btn ghost sm" },
                    r#type: "button",
                    onclick: move |_| on_select.call(conv_id_select.clone()),
                    "Visual Flow"
                }
                button {
                    class: "btn ghost sm wf-chat-link",
                    r#type: "button",
                    title: "Open conversation ({plan.conversation_id})",
                    onclick: move |_| session.write().open_conversation(conv_id_open.clone()),
                    Icon { name: "chat", size: 12 }
                    "Open in Chat ({label})"
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
    let mut feed_error = use_signal(|| Option::<String>::None);
    let mut query = use_signal(String::new);
    let mut filter_mode = use_signal(|| WorkflowFilter::All);
    let mut selected = use_signal(|| Option::<SelectedWorkflow>::None);
    let mut selected_node_id = use_signal(|| Option::<String>::None);
    let mut editing_routine = use_signal(|| Option::<String>::None);
    let mut editing_source = use_signal(|| Option::<String>::None);

    let state = session.read();
    let snapshot = state.state.workflows.clone();
    let loaded = state.state.workflows_loaded;
    let error = state.state.workflows_error.clone();
    let names: std::collections::HashMap<String, String> = state
        .state
        .conversations
        .iter()
        .map(|c| (c.id.clone(), state.conversation_label(c)))
        .collect();
    drop(state);

    let enabled_routines = snapshot.routines.iter().filter(|r| r.enabled).count();
    let total_routines = snapshot.routines.len();
    let healthy_sources = snapshot
        .sources
        .iter()
        .filter(|s| s.enabled && !s.last_run.as_ref().is_some_and(|r| !r.ok))
        .count();
    let total_sources = snapshot.sources.len();
    let active_plans_count = snapshot.active.len();
    let tz_display = snapshot
        .routines
        .iter()
        .find(|r| r.name == "morning_brief")
        .or_else(|| snapshot.routines.first())
        .and_then(|r| r.timezone.clone())
        .unwrap_or_else(|| "UTC".into());
    let has_brief = snapshot.routines.iter().any(|r| r.name == "morning_brief");

    // Auto-select the first available workflow if nothing is selected or the selection was removed.
    let active_selection = {
        let cur = selected.read().clone();
        let valid = match &cur {
            Some(SelectedWorkflow::Routine(n)) => snapshot.routines.iter().any(|r| &r.name == n),
            Some(SelectedWorkflow::Source(n)) => snapshot.sources.iter().any(|s| &s.name == n),
            Some(SelectedWorkflow::Plan(id)) => {
                snapshot.active.iter().any(|p| &p.conversation_id == id)
            }
            None => false,
        };
        if valid {
            cur
        } else if let Some(r) = snapshot.routines.first() {
            Some(SelectedWorkflow::Routine(r.name.clone()))
        } else if let Some(s) = snapshot.sources.first() {
            Some(SelectedWorkflow::Source(s.name.clone()))
        } else {
            snapshot
                .active
                .first()
                .map(|p| SelectedWorkflow::Plan(p.conversation_id.clone()))
        }
    };

    let active_graph: Option<WorkflowGraph> = match &active_selection {
        Some(SelectedWorkflow::Routine(name)) => snapshot
            .routines
            .iter()
            .find(|r| &r.name == name)
            .map(|r| workflows::build_routine_graph(r, &snapshot.sources, &tz_display)),
        Some(SelectedWorkflow::Source(name)) => snapshot
            .sources
            .iter()
            .find(|s| &s.name == name)
            .map(workflows::build_source_graph),
        Some(SelectedWorkflow::Plan(id)) => snapshot
            .active
            .iter()
            .find(|p| &p.conversation_id == id)
            .map(workflows::build_plan_graph),
        None => None,
    };

    let free_slots: Vec<String> = member_routine_slots()
        .into_iter()
        .filter(|slot| !snapshot.routines.iter().any(|r| &r.name == slot))
        .collect();
    let selected_slot = if free_slots.contains(&new_slot.read()) {
        new_slot.read().clone()
    } else {
        free_slots.first().cloned().unwrap_or_default()
    };

    let needle = query.read().clone();
    let mode = *filter_mode.read();
    let filtered_routines: Vec<Routine> = snapshot
        .routines
        .iter()
        .filter(|r| {
            let mode_ok = match mode {
                WorkflowFilter::All => true,
                WorkflowFilter::Enabled => r.enabled,
                WorkflowFilter::Failing => false,
            };
            let tz = r.timezone.as_deref().unwrap_or("");
            let lbl = r.label.as_deref().unwrap_or("");
            mode_ok && workflows::matches_workflow_filter(&needle, &[&r.name, tz, lbl])
        })
        .cloned()
        .collect();
    let filtered_sources: Vec<MonitoringSource> = snapshot
        .sources
        .iter()
        .filter(|s| {
            let mode_ok = match mode {
                WorkflowFilter::All => true,
                WorkflowFilter::Enabled => s.enabled,
                WorkflowFilter::Failing => s.last_run.as_ref().is_some_and(|r| !r.ok),
            };
            let url = s.url.as_deref().unwrap_or("");
            let err = s
                .last_run
                .as_ref()
                .and_then(|r| r.error.as_deref())
                .unwrap_or("");
            mode_ok && workflows::matches_workflow_filter(&needle, &[&s.name, &s.kind, url, err])
        })
        .cloned()
        .collect();
    let filtered_active: Vec<ActiveWorkflow> = snapshot
        .active
        .iter()
        .filter(|p| {
            let mode_ok = mode == WorkflowFilter::All;
            let label = names
                .get(&p.conversation_id)
                .map(String::as_str)
                .unwrap_or("");
            mode_ok
                && workflows::matches_workflow_filter(
                    &needle,
                    &[&p.goal, &p.conversation_id, label],
                )
        })
        .cloned()
        .collect();

    let feed_name_ready = !new_feed_name.read().trim().is_empty();
    let feed_url_ready = workflows::is_valid_feed_url(&new_feed_url.read());

    rsx! {
        div { class: "pane",
            div { class: "pane-head wf-hero",
                div { class: "wf-hero-copy",
                    h1 { class: "pane-title", "Workflow Studio" }
                    p { class: "muted",
                        "Visual node flows, scheduled routines, watcher feeds and multi-step plans."
                    }
                    div { class: "wf-hero-stats",
                        div { class: "wf-stat-card",
                            span { class: "wf-stat-label", "Active Schedules" }
                            span { class: "wf-stat-value", "{enabled_routines}/{total_routines}" }
                        }
                        div { class: "wf-stat-card",
                            span { class: "wf-stat-label", "Healthy Sources" }
                            span { class: "wf-stat-value", "{healthy_sources}/{total_sources}" }
                        }
                        div { class: "wf-stat-card",
                            span { class: "wf-stat-label", "Active Multi-Step Plans" }
                            span { class: "wf-stat-value", "{active_plans_count}" }
                        }
                        span { class: "wf-tz-pill", "TZ: {tz_display}" }
                    }
                }
                div { class: "pane-actions wf-head-actions",
                    if has_brief {
                        button {
                            class: "btn secondary sm",
                            r#type: "button",
                            disabled: !loaded,
                            onclick: move |_| session.write().run_workflow_routine("morning_brief".into()),
                            "Run Brief"
                        }
                    }
                    button {
                        class: "btn ghost sm",
                        r#type: "button",
                        disabled: !loaded,
                        onclick: move |_| session.write().run_all_watch_sources(),
                        "Check All Feeds"
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
            div { class: "pane-body",
                if let Some(message) = error.as_ref() {
                    p { class: "wf-banner err", role: "alert", "{message}" }
                }
                if !loaded && error.is_none() {
                    div { class: "wf-skeleton", role: "status", "aria-label": "Loading workflows",
                        for n in 0..5 {
                            div { key: "{n}", class: "skeleton", style: "height: 44px; margin-bottom: 10px;" }
                        }
                    }
                } else {
                    if let Some(graph) = active_graph {
                        {
                            let sel_for_click = active_selection.clone();
                            rsx! {
                                WorkflowCanvas {
                                    graph,
                                    selected_node_id,
                                    on_node_click: move |node_id: String| {
                                        match &sel_for_click {
                                            Some(SelectedWorkflow::Routine(name)) if node_id == "trigger" || node_id == "bot" => {
                                                editing_routine.set(Some(name.clone()));
                                            }
                                            Some(SelectedWorkflow::Source(name)) if node_id == "source" => {
                                                editing_source.set(Some(name.clone()));
                                            }
                                            _ => {}
                                        }
                                    },
                                }
                            }
                        }
                    }
                    div { class: "wf-toolbar",
                        input {
                            class: "input wf-search",
                            r#type: "search",
                            placeholder: "Filter routines, sources, or plans\u{2026}",
                            "aria-label": "Filter workflows",
                            value: "{query}",
                            oninput: move |e| query.set(e.value()),
                            onkeydown: move |e: KeyboardEvent| {
                                if e.key() == Key::Escape {
                                    query.set(String::new());
                                }
                            },
                        }
                        div { class: "seg", role: "group", "aria-label": "Filter workflow state",
                            button {
                                class: if mode == WorkflowFilter::All { "seg-btn active" } else { "seg-btn" },
                                r#type: "button",
                                onclick: move |_| filter_mode.set(WorkflowFilter::All),
                                "All"
                            }
                            button {
                                class: if mode == WorkflowFilter::Enabled { "seg-btn active" } else { "seg-btn" },
                                r#type: "button",
                                onclick: move |_| filter_mode.set(WorkflowFilter::Enabled),
                                "Enabled"
                            }
                            button {
                                class: if mode == WorkflowFilter::Failing { "seg-btn active" } else { "seg-btn" },
                                r#type: "button",
                                onclick: move |_| filter_mode.set(WorkflowFilter::Failing),
                                "Failing"
                            }
                        }
                    }
                    div { class: "wf-studio-grid",
                        section { class: "wf-section",
                            h2 { class: "wf-section-title", "Scheduled Routines" }
                            if !snapshot.routines_supported {
                                p { class: "muted", role: "status",
                                    "This server does not expose /v1/routines yet."
                                }
                            } else {
                                if snapshot.routines.is_empty() {
                                    p { class: "muted", role: "status",
                                        "No routines yet. Add the morning brief or a watch schedule below."
                                    }
                                } else if filtered_routines.is_empty() {
                                    p { class: "muted", role: "status", "No routines match this filter." }
                                } else {
                                    div { class: "wf-cards", role: "list",
                                        for routine in filtered_routines {
                                            {
                                                let is_sel = active_selection.as_ref()
                                                    == Some(&SelectedWorkflow::Routine(routine.name.clone()));
                                                rsx! {
                                                    RoutineCard {
                                                        key: "{routine.name}",
                                                        session,
                                                        routine,
                                                        selected: is_sel,
                                                        editing_routine,
                                                        on_select: move |name: String| {
                                                            selected_node_id.set(None);
                                                            selected.set(Some(SelectedWorkflow::Routine(name)));
                                                        },
                                                    }
                                                }
                                            }
                                        }
                                    }
                                }
                                if !free_slots.is_empty() {
                                    form {
                                        class: "wf-form",
                                        "aria-label": "Add watch schedule",
                                        onsubmit: move |event| {
                                            event.prevent_default();
                                            let slot = selected_slot.clone();
                                            if !slot.is_empty() {
                                                session.write().save_workflow_routine(slot.clone(), Some(true), None, None);
                                                selected.set(Some(SelectedWorkflow::Routine(slot)));
                                            }
                                        },
                                        label { class: "wf-field",
                                            span { "Schedule slot" }
                                            select {
                                                "aria-label": "Routine to add",
                                                onchange: move |e| new_slot.set(e.value()),
                                                for slot in free_slots.clone() {
                                                    option { value: "{slot}", selected: slot == selected_slot, "{slot}" }
                                                }
                                            }
                                        }
                                        button { class: "btn primary sm", r#type: "submit", "+ Add Watch Schedule" }
                                    }
                                }
                            }
                        }
                        section { class: "wf-section",
                            h2 { class: "wf-section-title", "Monitoring Sources" }
                            if !snapshot.sources_supported {
                                p { class: "muted", role: "status",
                                    "This server does not expose /v1/monitoring/sources yet."
                                }
                            } else {
                                if snapshot.sources.is_empty() {
                                    p { class: "muted", role: "status",
                                        "No watcher sources yet. Add an RSS feed below."
                                    }
                                } else if filtered_sources.is_empty() {
                                    p { class: "muted", role: "status", "No watcher sources match this filter." }
                                } else {
                                    div { class: "wf-cards", role: "list",
                                        for source in filtered_sources {
                                            {
                                                let is_sel = active_selection.as_ref()
                                                    == Some(&SelectedWorkflow::Source(source.name.clone()));
                                                rsx! {
                                                    SourceCard {
                                                        key: "{source.name}",
                                                        session,
                                                        source,
                                                        selected: is_sel,
                                                        editing_source,
                                                        on_select: move |name: String| {
                                                            selected_node_id.set(None);
                                                            selected.set(Some(SelectedWorkflow::Source(name)));
                                                        },
                                                    }
                                                }
                                            }
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
                                        if name.is_empty() {
                                            feed_error.set(Some("Enter a feed name.".into()));
                                            return;
                                        }
                                        if !workflows::is_valid_feed_url(&url) {
                                            feed_error.set(Some("Enter a valid http:// or https:// URL.".into()));
                                            return;
                                        }
                                        feed_error.set(None);
                                        session.write().add_feed(name, url);
                                        new_feed_name.set(String::new());
                                        new_feed_url.set(String::new());
                                    },
                                    label { class: "wf-field",
                                        span { "Feed name" }
                                        input {
                                            r#type: "text",
                                            maxlength: "128",
                                            placeholder: "e.g. EU tenders",
                                            value: "{new_feed_name}",
                                            oninput: move |e| {
                                                feed_error.set(None);
                                                new_feed_name.set(e.value());
                                            },
                                        }
                                    }
                                    label { class: "wf-field wf-field-wide",
                                        span { "RSS URL" }
                                        input {
                                            r#type: "url",
                                            placeholder: "https://example.invalid/feed.xml",
                                            value: "{new_feed_url}",
                                            oninput: move |e| {
                                                feed_error.set(None);
                                                new_feed_url.set(e.value());
                                            },
                                        }
                                    }
                                    button {
                                        class: "btn primary sm",
                                        r#type: "submit",
                                        disabled: !feed_name_ready || !feed_url_ready,
                                        "+ Add Feed"
                                    }
                                }
                                if let Some(msg) = feed_error.read().as_ref() {
                                    p { class: "wf-inline-err", role: "alert", "{msg}" }
                                }
                            }
                        }
                        section { class: "wf-section wf-section-wide",
                            h2 { class: "wf-section-title", "Active Multi-Step Plans" }
                            if !snapshot.active_supported {
                                p { class: "muted", role: "status",
                                    "This server does not expose /v1/workflows/active yet."
                                }
                            } else if snapshot.active.is_empty() {
                                p { class: "muted", role: "status", "No incomplete plans." }
                            } else if filtered_active.is_empty() {
                                p { class: "muted", role: "status", "No active plans match this filter." }
                            } else {
                                div { class: "wf-cards", role: "list",
                                    for plan in filtered_active {
                                        {
                                            let is_sel = active_selection.as_ref()
                                                == Some(&SelectedWorkflow::Plan(plan.conversation_id.clone()));
                                            rsx! {
                                                ActivePlanCard {
                                                    key: "{plan.conversation_id}",
                                                    session,
                                                    conversation: names.get(&plan.conversation_id).cloned(),
                                                    plan,
                                                    selected: is_sel,
                                                    on_select: move |id: String| {
                                                        selected_node_id.set(None);
                                                        selected.set(Some(SelectedWorkflow::Plan(id)));
                                                    },
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
        }
    }
}
