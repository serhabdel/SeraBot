//! The Workflows tab (0.29.0, Wave A4; manage actions 0.34.0; Visual Workflow
//! Studio 0.45.0 Wave 8; redesigned for clarity and scannability):
//! hero header with primary action, stat tiles, segmented view switch
//! (Workflows / Routines / History), workflow/routine cards with bot avatar,
//! human schedule, status pill, next/last run, inline actions, visual node
//! canvas, and vertical step timeline for multi-step plans.

use crate::ui::{Icon, avatar_style};
use crate::ui_files::{CONFIRM_RESET_JS, copy_script};
use dioxus::prelude::*;
use serabot_desktop::session::Session;
use serabot_desktop::workflows::{
    self, ActiveWorkflow, MonitoringSource, Routine, WorkflowGraph, WorkflowHistoryEntry,
    WorkflowNodeKind, WorkflowPlanStep, WorkflowStatus, member_routine_slots,
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
enum WorkflowTab {
    Workflows,
    Routines,
    History,
}

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
        WorkflowNodeKind::Trigger => "workflows",
        WorkflowNodeKind::Source => "globe",
        WorkflowNodeKind::Bot => "robot",
        WorkflowNodeKind::Gate => "shield",
        WorkflowNodeKind::Output => "send",
        WorkflowNodeKind::Step => "chevron-right",
    }
}

fn status_icon_name(status: WorkflowStatus) -> &'static str {
    match status {
        WorkflowStatus::Active | WorkflowStatus::Complete => "check",
        WorkflowStatus::Paused => "workflows",
        WorkflowStatus::Failed => "alert-circle",
    }
}

/// Reusable status pill with icon + text (never color alone).
#[component]
fn WfStatusPill(status: WorkflowStatus, #[props(default)] detail: Option<String>) -> Element {
    let cls = status.css_class();
    let icon = status_icon_name(status);
    let label = detail.unwrap_or_else(|| status.label().to_owned());
    rsx! {
        span { class: "{cls}", role: "status",
            Icon { name: icon, size: 12 }
            span { "{label}" }
        }
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
    let status = if graph.active {
        WorkflowStatus::Active
    } else {
        WorkflowStatus::Paused
    };
    let node_count = graph.nodes.len();
    rsx! {
        section { class: "wf-canvas", "aria-label": "Visual workflow node canvas",
            div { class: "wf-canvas-head",
                div { class: "wf-canvas-title-wrap",
                    span { class: "wf-canvas-eyebrow", "Flow Preview" }
                    h2 { class: "wf-canvas-title", "{graph.title}" }
                    span { class: "wf-meta", "{graph.subtitle}" }
                }
                WfStatusPill { status }
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

/// Calm empty state with optional primary CTA button.
#[component]
fn WfEmpty(
    icon: &'static str,
    title: &'static str,
    text: &'static str,
    #[props(default)] cta_label: Option<&'static str>,
    #[props(default)] on_cta: Option<EventHandler<()>>,
) -> Element {
    rsx! {
        div { class: "empty compact wf-empty", role: "status",
            span { class: "empty-icon", Icon { name: icon, size: 20 } }
            h3 { class: "empty-title", "{title}" }
            p { class: "empty-text", "{text}" }
            if let (Some(label), Some(handler)) = (cta_label, on_cta) {
                button {
                    class: "btn primary sm",
                    r#type: "button",
                    onclick: move |_| handler.call(()),
                    Icon { name: "plus", size: 14 }
                    "{label}"
                }
            }
        }
    }
}

/// New Routine / New Feed creation modal sheet using existing `save_workflow_routine`
/// and `add_feed` operations, with labelled fields, help text, and validation.
#[component]
fn CreateWorkflowSheet(
    session: Signal<Session>,
    free_slots: Vec<String>,
    default_tz: String,
    initial_mode: String,
    on_close: EventHandler<()>,
    on_created_routine: EventHandler<String>,
    on_created_source: EventHandler<String>,
) -> Element {
    let mut session = session;
    let mut kind = use_signal(|| {
        if initial_mode == "source" || free_slots.is_empty() {
            "source".to_owned()
        } else {
            "routine".to_owned()
        }
    });
    let mut slot = use_signal(|| free_slots.first().cloned().unwrap_or_default());
    let mut label = use_signal(String::new);
    let mut time = use_signal(|| "08:00".to_owned());
    let mut timezone = use_signal(|| {
        if default_tz.trim().is_empty() {
            "UTC".to_owned()
        } else {
            default_tz.clone()
        }
    });
    let mut prompt = use_signal(String::new);
    let mut feed_name = use_signal(String::new);
    let mut feed_url = use_signal(String::new);
    let mut form_error = use_signal(|| Option::<String>::None);

    let selected_slot = if free_slots.contains(&slot.read()) {
        slot.read().clone()
    } else {
        free_slots.first().cloned().unwrap_or_default()
    };
    let tz_preset = if TIMEZONE_PRESETS.contains(&timezone.read().as_str()) {
        timezone.read().clone()
    } else {
        "__custom__".to_owned()
    };
    let is_routine = kind.read().as_str() == "routine";

    rsx! {
        div {
            class: "wf-modal-scrim",
            onclick: move |_| on_close.call(()),
        }
        div {
            class: "wf-modal",
            role: "dialog",
            "aria-modal": "true",
            "aria-label": "Create scheduled routine or monitoring feed",
            tabindex: "-1",
            onkeydown: move |e: KeyboardEvent| {
                if e.key() == Key::Escape {
                    e.stop_propagation();
                    on_close.call(());
                }
            },
            div { class: "wf-modal-head",
                div {
                    h2 { class: "wf-modal-title", "New Automation" }
                    p { class: "wf-modal-sub", "Schedule a daily routine or register an RSS monitoring feed." }
                }
                button {
                    class: "icon-btn escape-close",
                    r#type: "button",
                    "aria-label": "Close dialog",
                    onclick: move |_| on_close.call(()),
                    Icon { name: "x", size: 16 }
                }
            }
            div { class: "wf-modal-body",
                div { class: "seg wf-modal-seg", role: "group", "aria-label": "Automation type",
                    button {
                        class: if is_routine { "seg-btn active" } else { "seg-btn" },
                        r#type: "button",
                        disabled: free_slots.is_empty(),
                        onclick: move |_| {
                            form_error.set(None);
                            kind.set("routine".into());
                        },
                        "Scheduled Routine"
                    }
                    button {
                        class: if !is_routine { "seg-btn active" } else { "seg-btn" },
                        r#type: "button",
                        onclick: move |_| {
                            form_error.set(None);
                            kind.set("source".into());
                        },
                        "RSS Feed Source"
                    }
                }

                if is_routine {
                    if free_slots.is_empty() {
                        p { class: "wf-field-help", "All routine slots (Morning Brief and 12 Watch slots) are already configured. Edit or pause an existing routine, or add an RSS feed instead." }
                    } else {
                        div { class: "wf-editor-grid",
                            label { class: "wf-field",
                                span { class: "wf-field-label", "Routine slot" }
                                select {
                                    class: "select",
                                    value: "{selected_slot}",
                                    onchange: move |e| {
                                        form_error.set(None);
                                        slot.set(e.value());
                                    },
                                    for item in free_slots.clone() {
                                        option {
                                            value: "{item}",
                                            selected: item == selected_slot,
                                            "{workflows::slot_display_label(&item)}"
                                        }
                                    }
                                }
                                span { class: "wf-field-help", "Runs under your Watcher bot on the server." }
                            }
                            label { class: "wf-field",
                                span { class: "wf-field-label", "Display name (optional)" }
                                input {
                                    class: "input",
                                    r#type: "text",
                                    maxlength: "120",
                                    placeholder: "e.g. Daily Market Radar",
                                    value: "{label}",
                                    oninput: move |e| {
                                        form_error.set(None);
                                        label.set(e.value());
                                    },
                                }
                                span { class: "wf-field-help", "Shown on cards instead of the slot id." }
                            }
                            label { class: "wf-field",
                                span { class: "wf-field-label", "Daily run time (24h)" }
                                input {
                                    class: "input",
                                    r#type: "time",
                                    value: "{time}",
                                    oninput: move |e| {
                                        form_error.set(None);
                                        time.set(e.value());
                                    },
                                }
                                span { class: "wf-field-help", "Uses HH:MM in the chosen timezone." }
                            }
                            label { class: "wf-field",
                                span { class: "wf-field-label", "Timezone preset" }
                                select {
                                    class: "select",
                                    value: "{tz_preset}",
                                    onchange: move |e| {
                                        form_error.set(None);
                                        let v = e.value();
                                        if v != "__custom__" {
                                            timezone.set(v);
                                        }
                                    },
                                    for tz in TIMEZONE_PRESETS {
                                        option { value: "{tz}", selected: tz_preset == *tz, "{tz}" }
                                    }
                                    option { value: "__custom__", selected: tz_preset == "__custom__", "Custom IANA…" }
                                }
                                span { class: "wf-field-help", "Pick a preset or enter an IANA zone below." }
                            }
                        }
                        label { class: "wf-field wf-field-full",
                            span { class: "wf-field-label", "IANA Timezone" }
                            input {
                                class: "input",
                                r#type: "text",
                                maxlength: "64",
                                placeholder: "Europe/Paris",
                                value: "{timezone}",
                                oninput: move |e| {
                                    form_error.set(None);
                                    timezone.set(e.value());
                                },
                            }
                        }
                        label { class: "wf-field wf-field-full",
                            span { class: "wf-field-label", "Instructions / Prompt (optional)" }
                            textarea {
                                class: "wf-textarea textarea",
                                rows: "3",
                                maxlength: "240",
                                placeholder: "Summarize key updates and highlight deadlines…",
                                value: "{prompt}",
                                oninput: move |e| {
                                    form_error.set(None);
                                    prompt.set(e.value());
                                },
                            }
                            span { class: "wf-field-help", "Up to 240 characters, no control characters." }
                        }
                    }
                } else {
                    div { class: "wf-editor",
                        label { class: "wf-field wf-field-full",
                            span { class: "wf-field-label", "Feed name" }
                            input {
                                class: "input",
                                r#type: "text",
                                maxlength: "128",
                                placeholder: "e.g. EU Public Tenders",
                                value: "{feed_name}",
                                oninput: move |e| {
                                    form_error.set(None);
                                    feed_name.set(e.value());
                                },
                            }
                            span { class: "wf-field-help", "A short unique label for your RSS/Atom source." }
                        }
                        label { class: "wf-field wf-field-full",
                            span { class: "wf-field-label", "Feed URL" }
                            input {
                                class: "input",
                                r#type: "url",
                                placeholder: "https://example.invalid/feed.xml",
                                value: "{feed_url}",
                                oninput: move |e| {
                                    form_error.set(None);
                                    feed_url.set(e.value());
                                },
                            }
                            span { class: "wf-field-help", "Must start with http:// or https://." }
                        }
                    }
                }

                if let Some(err) = form_error.read().as_ref() {
                    p { class: "wf-inline-err", role: "alert", "{err}" }
                }
            }
            div { class: "wf-modal-foot",
                button {
                    class: "btn ghost",
                    r#type: "button",
                    onclick: move |_| on_close.call(()),
                    "Cancel"
                }
                if is_routine {
                    button {
                        class: "btn primary",
                        r#type: "button",
                        disabled: free_slots.is_empty(),
                        onclick: move |_| {
                            let target_slot = selected_slot.clone();
                            let t = time.read().trim().to_owned();
                            let tz = timezone.read().trim().to_owned();
                            let lbl = label.read().trim().to_owned();
                            let pr = prompt.read().trim().to_owned();
                            if target_slot.is_empty() {
                                form_error.set(Some("No routine slots available.".into()));
                                return;
                            }
                            if !t.is_empty() && !workflows::is_valid_routine_time(&t) {
                                form_error.set(Some("Enter a valid 24-hour time (HH:MM).".into()));
                                return;
                            }
                            if !tz.is_empty() && !workflows::is_valid_timezone_name(&tz) {
                                form_error.set(Some("Enter a valid IANA timezone with no spaces.".into()));
                                return;
                            }
                            if lbl.chars().count() > 120 || pr.chars().count() > 240 {
                                form_error.set(Some("Label max 120 chars; prompt max 240 chars.".into()));
                                return;
                            }
                            session.write().update_routine(
                                target_slot.clone(),
                                Some(true),
                                (!t.is_empty()).then_some(t),
                                (!tz.is_empty()).then_some(tz),
                                (!lbl.is_empty()).then_some(lbl),
                                (!pr.is_empty()).then_some(pr),
                            );
                            on_created_routine.call(target_slot);
                            on_close.call(());
                        },
                        Icon { name: "plus", size: 14 }
                        "Create Routine"
                    }
                } else {
                    button {
                        class: "btn primary",
                        r#type: "button",
                        onclick: move |_| {
                            let name = feed_name.read().trim().to_owned();
                            let url = feed_url.read().trim().to_owned();
                            if name.is_empty() {
                                form_error.set(Some("Enter a feed name.".into()));
                                return;
                            }
                            if !workflows::is_valid_feed_url(&url) {
                                form_error.set(Some("Enter a valid http:// or https:// feed URL.".into()));
                                return;
                            }
                            session.write().add_feed(name.clone(), url);
                            on_created_source.call(name);
                            on_close.call(());
                        },
                        Icon { name: "plus", size: 14 }
                        "Add Feed"
                    }
                }
            }
        }
    }
}

/// Routine card with bot avatar/name, human schedule, status pill, next/last run,
/// inline actions (Run Now, Pause/Resume, Edit, Delete), and structured editor.
#[component]
fn RoutineCard(
    session: Signal<Session>,
    routine: Routine,
    sources: Vec<MonitoringSource>,
    bot_name: String,
    bot_avatar: String,
    selected: bool,
    editing_routine: Signal<Option<String>>,
    on_select: EventHandler<String>,
) -> Element {
    let mut session = session;
    let mut editing_routine = editing_routine;
    let mut confirming = use_signal(|| false);
    let mut editor_error = use_signal(|| Option::<String>::None);

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
        editor_error.set(None);
    }

    let is_editing = editing_routine.read().as_deref() == Some(routine.name.as_str());
    let display_title = workflows::routine_display_title(&routine);
    let human_schedule =
        workflows::format_human_schedule(routine.time.as_deref(), routine.timezone.as_deref());
    let status = workflows::routine_status(&routine, &sources);
    let bot_id = workflows::routine_bot_id(&routine);
    let now = workflows::now_secs();
    let next_run = workflows::relative_time(&routine.next_run_at, now);

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
        "wf-card wf-routine-card selected"
    } else {
        "wf-card wf-routine-card"
    };

    rsx! {
        div { class: "{card_cls}", role: "listitem",
            div { class: "wf-card-head",
                div { class: "wf-card-identity",
                    span {
                        class: "avatar bot wf-bot-avatar",
                        style: avatar_style(bot_id),
                        title: "{bot_name}",
                        "{bot_avatar}"
                    }
                    div { class: "wf-card-copy",
                        div { class: "wf-card-title-row",
                            span { class: "wf-name", title: "{routine.name}", "{display_title}" }
                            span { class: "wf-slot-tag mono", "{routine.name}" }
                        }
                        div { class: "wf-meta-row",
                            span { class: "wf-bot-name", "{bot_name}" }
                            span { "·" }
                            span { class: "wf-schedule-text", title: "{human_schedule}", "{human_schedule}" }
                        }
                    }
                }
                div { class: "wf-card-right",
                    WfStatusPill { status }
                }
            }
            if let Some(prompt_text) = routine.prompt.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
                p { class: "wf-meta", "{prompt_text}" }
            }
            div { class: "wf-telemetry",
                span { class: "wf-telemetry-item",
                    span { class: "wf-telemetry-label", "Next run" }
                    strong { class: "wf-telemetry-val", title: "{routine.next_run_at}", "{next_run}" }
                }
                span { class: "wf-telemetry-item",
                    span { class: "wf-telemetry-label", "Last run" }
                    strong { class: "wf-telemetry-val",
                        {routine.last_run_for.as_deref().unwrap_or("Not yet")}
                    }
                }
            }
            div { class: "wf-card-actions",
                button {
                    class: "btn primary sm",
                    r#type: "button",
                    onclick: move |_| session.write().run_workflow_routine(run_name.clone()),
                    Icon { name: "bolt", size: 12 }
                    "Run Now"
                }
                button {
                    class: "btn ghost sm",
                    r#type: "button",
                    onclick: move |_| session.write().save_workflow_routine(toggle_name.clone(), Some(toggle_to), None, None),
                    if routine.enabled { "Pause" } else { "Resume" }
                }
                button {
                    class: if is_editing { "btn sm wf-btn-active" } else { "btn ghost sm" },
                    r#type: "button",
                    onclick: move |_| {
                        editor_error.set(None);
                        if is_editing {
                            editing_routine.set(None);
                        } else {
                            editing_routine.set(Some(edit_name.clone()));
                        }
                    },
                    Icon { name: "edit", size: 12 }
                    if is_editing { "Close" } else { "Edit" }
                }
                button {
                    class: if selected { "btn sm wf-btn-active" } else { "btn ghost sm" },
                    r#type: "button",
                    onclick: move |_| on_select.call(select_name.clone()),
                    "Flow"
                }
                div { class: "wf-confirm-row",
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
                            class: "btn ghost sm wf-del-btn",
                            r#type: "button",
                            title: "Delete routine",
                            "aria-label": "Delete {display_title}",
                            onclick: move |_| confirming.set(true),
                            Icon { name: "trash", size: 13 }
                        }
                    }
                }
            }
            if is_editing {
                div { class: "wf-editor",
                    div { class: "wf-editor-head",
                        strong { "Edit Routine Settings" }
                        span { class: "wf-field-help", "Changes update the server schedule immediately." }
                    }
                    div { class: "wf-editor-grid",
                        label { class: "wf-field",
                            span { class: "wf-field-label", "Custom label" }
                            input {
                                class: "input",
                                r#type: "text",
                                maxlength: "120",
                                placeholder: "{routine.name}",
                                value: "{draft_label}",
                                oninput: move |e| {
                                    editor_error.set(None);
                                    draft_label.set(e.value());
                                },
                            }
                            span { class: "wf-field-help", "Leave blank to use slot name." }
                        }
                        label { class: "wf-field",
                            span { class: "wf-field-label", "Daily time (HH:MM)" }
                            input {
                                class: "input wf-time",
                                r#type: "time",
                                value: "{draft_time}",
                                oninput: move |e| {
                                    editor_error.set(None);
                                    draft_time.set(e.value());
                                },
                            }
                            span { class: "wf-field-help", "24-hour local schedule time." }
                        }
                        label { class: "wf-field",
                            span { class: "wf-field-label", "Timezone preset" }
                            select {
                                class: "select",
                                value: "{preset_value}",
                                onchange: move |e| {
                                    editor_error.set(None);
                                    let val = e.value();
                                    if val != "__custom__" {
                                        draft_tz.set(val);
                                    }
                                },
                                for tz in TIMEZONE_PRESETS {
                                    option { value: "{tz}", selected: preset_value == *tz, "{tz}" }
                                }
                                option { value: "__custom__", selected: preset_value == "__custom__", "Custom IANA…" }
                            }
                        }
                        label { class: "wf-field",
                            span { class: "wf-field-label", "IANA Timezone" }
                            input {
                                class: "input",
                                r#type: "text",
                                maxlength: "64",
                                placeholder: "Europe/Paris",
                                value: "{draft_tz}",
                                oninput: move |e| {
                                    editor_error.set(None);
                                    draft_tz.set(e.value());
                                },
                            }
                        }
                    }
                    label { class: "wf-field wf-field-full",
                        span { class: "wf-field-label", "Custom instructions / prompt" }
                        textarea {
                            class: "wf-textarea textarea",
                            rows: "2",
                            maxlength: "240",
                            placeholder: "Optional instructions for this routine…",
                            value: "{draft_prompt}",
                            oninput: move |e| {
                                editor_error.set(None);
                                draft_prompt.set(e.value());
                            },
                        }
                        span { class: "wf-field-help", "Up to 240 characters." }
                    }
                    if let Some(err) = editor_error.read().as_ref() {
                        p { class: "wf-inline-err", role: "alert", "{err}" }
                    }
                    div { class: "wf-editor-foot",
                        label { class: "wf-toggle",
                            input {
                                r#type: "checkbox",
                                checked: *draft_enabled.read(),
                                onchange: move |e| draft_enabled.set(e.checked()),
                            }
                            span { "Routine active" }
                        }
                        div { class: "row-actions",
                            button {
                                class: "btn ghost sm",
                                r#type: "button",
                                onclick: move |_| editing_routine.set(None),
                                "Cancel"
                            }
                            button {
                                class: "btn primary sm",
                                r#type: "button",
                                onclick: move |_| {
                                    let t = draft_time.read().trim().to_owned();
                                    let tz = draft_tz.read().trim().to_owned();
                                    let lbl = draft_label.read().trim().to_owned();
                                    let pr = draft_prompt.read().trim().to_owned();
                                    let en = *draft_enabled.read();
                                    if !t.is_empty() && !workflows::is_valid_routine_time(&t) {
                                        editor_error.set(Some("Enter a valid 24-hour time (HH:MM).".into()));
                                        return;
                                    }
                                    if !tz.is_empty() && !workflows::is_valid_timezone_name(&tz) {
                                        editor_error.set(Some("Enter a valid IANA timezone with no spaces.".into()));
                                        return;
                                    }
                                    if pr.chars().count() > 240 {
                                        editor_error.set(Some("Prompt must be 240 characters or fewer.".into()));
                                        return;
                                    }
                                    session.write().update_routine(
                                        save_name.clone(),
                                        Some(en),
                                        (!t.is_empty()).then_some(t),
                                        (!tz.is_empty()).then_some(tz),
                                        (!lbl.is_empty()).then_some(lbl),
                                        (!pr.is_empty()).then_some(pr),
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
}

/// Monitoring source card with status pill, host summary, copy link, toggle, URL edit, and Delete.
#[component]
fn SourceCard(
    session: Signal<Session>,
    source: MonitoringSource,
    bot_name: String,
    bot_avatar: String,
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
    let now = workflows::now_secs();
    let when = source
        .last_run
        .as_ref()
        .and_then(|run| run.finished_at.as_deref().or(run.started_at.as_deref()))
        .map(|at| workflows::relative_time(at, now));
    let items_seen = source
        .last_run
        .as_ref()
        .map(|r| r.items_seen.max(0))
        .unwrap_or(0);
    let status = workflows::source_status(&source);
    let telemetry = match &source.last_run {
        Some(run) if run.ok => match &when {
            Some(w) => format!("{items_seen} items · {w}"),
            None => format!("{items_seen} items"),
        },
        Some(run) => run.error.clone().unwrap_or_else(|| "Run error".into()),
        None => "Not checked yet".into(),
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
                div { class: "wf-card-identity",
                    span {
                        class: "avatar bot wf-bot-avatar",
                        style: avatar_style("watcher"),
                        title: "{bot_name}",
                        "{bot_avatar}"
                    }
                    div { class: "wf-card-copy",
                        div { class: "wf-card-title-row",
                            span { class: "wf-name", title: "{source.name}", "{source.name}" }
                            span { class: "wf-slot-tag", "{source.kind}" }
                        }
                        match source.url.clone() {
                            Some(url) => {
                                let host = workflows::source_host(&url).unwrap_or_else(|| url.clone());
                                rsx! {
                                    div { class: "wf-meta-row",
                                        span { class: "wf-bot-name", "{bot_name}" }
                                        span { "·" }
                                        span { class: "wf-schedule-text", title: "{url}", "{host}" }
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
                                div { class: "wf-meta-row",
                                    span { class: "wf-bot-name", "{bot_name}" }
                                    span { "·" }
                                    span { class: "wf-schedule-text", "Server-managed URL" }
                                }
                            },
                        }
                    }
                }
                div { class: "wf-card-right",
                    WfStatusPill { status }
                }
            }
            div { class: "wf-telemetry",
                span {
                    class: if status == WorkflowStatus::Failed { "wf-telemetry-item err" } else { "wf-telemetry-item" },
                    span { class: "wf-telemetry-label", "Last check" }
                    strong { class: "wf-telemetry-val", title: "{telemetry}", "{telemetry}" }
                }
                span { class: "wf-telemetry-item",
                    span { class: "wf-telemetry-label", "Owner" }
                    strong { class: "wf-telemetry-val", if source.own { "Your feed" } else { "Shared feed" } }
                }
            }
            div { class: "wf-card-actions",
                button {
                    class: "btn ghost sm",
                    r#type: "button",
                    onclick: move |_| session.write().patch_source(toggle_name.clone(), Some(toggle_to), None),
                    if source.enabled { "Pause" } else { "Resume" }
                }
                button {
                    class: if selected { "btn sm wf-btn-active" } else { "btn ghost sm" },
                    r#type: "button",
                    onclick: move |_| on_select.call(select_name.clone()),
                    "Flow"
                }
                if source.own {
                    button {
                        class: if is_editing { "btn sm wf-btn-active" } else { "btn ghost sm" },
                        r#type: "button",
                        onclick: move |_| {
                            url_error.set(None);
                            if is_editing {
                                editing_source.set(None);
                            } else {
                                editing_source.set(Some(edit_name.clone()));
                            }
                        },
                        Icon { name: "edit", size: 12 }
                        if is_editing { "Close" } else { "Edit URL" }
                    }
                    div { class: "wf-confirm-row",
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
                                class: "btn ghost sm wf-del-btn",
                                r#type: "button",
                                title: "Delete feed",
                                "aria-label": "Delete {source.name}",
                                onclick: move |_| confirming.set(true),
                                Icon { name: "trash", size: 13 }
                            }
                        }
                    }
                }
            }
            if source.own && is_editing {
                div { class: "wf-editor",
                    label { class: "wf-field wf-field-full",
                        span { class: "wf-field-label", "Feed URL" }
                        div { class: "wf-editor-row",
                            input {
                                class: "input wf-url-input",
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
                        span { class: "wf-field-help", "Only http:// and https:// feed endpoints are allowed." }
                    }
                    if let Some(err) = url_error.read().as_ref() {
                        p { class: "wf-inline-err", role: "alert", "{err}" }
                    }
                }
            }
        }
    }
}

/// Interactive multi-step workflow plan card with vertical stepper/timeline,
/// status icons, goal rename, add-step form, and Open in Chat.
#[component]
fn ActivePlanCard(
    session: Signal<Session>,
    plan: ActiveWorkflow,
    conversation: Option<String>,
    bot_id: String,
    bot_name: String,
    bot_avatar: String,
    selected: bool,
    on_select: EventHandler<String>,
) -> Element {
    let mut session = session;
    let mut draft_steps = use_signal(|| plan.items.clone());
    let mut draft_goal = use_signal(|| plan.goal.clone());
    let mut editing_goal = use_signal(|| false);
    let mut new_step_text = use_signal(String::new);
    let mut last_conv = use_signal(|| {
        (
            plan.conversation_id.clone(),
            plan.goal.clone(),
            plan.items.clone(),
        )
    });
    let cur_conv = (
        plan.conversation_id.clone(),
        plan.goal.clone(),
        plan.items.clone(),
    );
    if *last_conv.read() != cur_conv {
        last_conv.set(cur_conv);
        draft_steps.set(plan.items.clone());
        draft_goal.set(plan.goal.clone());
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
    let status = workflows::plan_status(&plan);
    let first_pending = steps.iter().position(|s| !s.done);
    let step_count = steps.len();
    let conv_id_open = plan.conversation_id.clone();
    let conv_id_select = plan.conversation_id.clone();
    let conv_id_goal = plan.conversation_id.clone();
    let conv_id_add = plan.conversation_id.clone();
    let conv_id_toggle = plan.conversation_id.clone();
    let card_cls = if selected {
        "wf-card wf-plan-card selected"
    } else {
        "wf-card wf-plan-card"
    };

    rsx! {
        div { class: "{card_cls}", role: "listitem",
            div { class: "wf-card-head",
                div { class: "wf-card-identity",
                    span {
                        class: "avatar bot wf-bot-avatar",
                        style: avatar_style(&bot_id),
                        title: "{bot_name}",
                        "{bot_avatar}"
                    }
                    div { class: "wf-card-copy",
                        if *editing_goal.read() {
                            div { class: "wf-editor-row",
                                input {
                                    class: "input wf-input",
                                    r#type: "text",
                                    maxlength: "240",
                                    value: "{draft_goal}",
                                    oninput: move |e| draft_goal.set(e.value()),
                                }
                                button {
                                    class: "btn primary sm",
                                    r#type: "button",
                                    disabled: draft_goal.read().trim().is_empty(),
                                    onclick: move |_| {
                                        let g = draft_goal.read().trim().to_owned();
                                        if !g.is_empty() {
                                            session.write().update_active_workflow(
                                                conv_id_goal.clone(),
                                                Some(g),
                                                None,
                                            );
                                            editing_goal.set(false);
                                        }
                                    },
                                    "Save"
                                }
                            }
                        } else {
                            span { class: "wf-name wf-name-wrap", title: "{plan.goal}", "{plan.goal}" }
                        }
                        div { class: "wf-meta-row",
                            span { class: "wf-bot-name", "{bot_name}" }
                            span { "·" }
                            span { class: "wf-meta", "{label}" }
                            if let Some(at) = plan.updated_at.as_deref() {
                                span { "·" }
                                span { class: "wf-meta", title: "{at}",
                                    "Updated {workflows::relative_time(at, workflows::now_secs())}"
                                }
                            }
                        }
                    }
                }
                div { class: "wf-card-right",
                    span { class: "wf-section-count mono", "{progress}" }
                    WfStatusPill { status }
                }
            }
            div { class: "wf-progress-bar", "aria-hidden": "true",
                div { class: "wf-progress-fill", style: "width: {pct}%;" }
            }
            if !steps.is_empty() {
                ol { class: "wf-stepper", "aria-label": "Workflow plan steps",
                    for (idx, step) in steps.iter().enumerate() {
                        {
                            let step_title = step.title.clone();
                            let step_done = step.done;
                            let is_current = !step_done && first_pending == Some(idx);
                            let step_cls = if step_done {
                                "wf-step done"
                            } else if is_current {
                                "wf-step current"
                            } else {
                                "wf-step"
                            };
                            let step_badge = if step_done {
                                "Done"
                            } else if is_current {
                                "Up next"
                            } else {
                                "Queued"
                            };
                            let conv_id_step = conv_id_toggle.clone();
                            rsx! {
                                li { key: "{idx}", class: "{step_cls}",
                                    div { class: "wf-step-rail", "aria-hidden": "true",
                                        span { class: "wf-step-marker",
                                            if step_done {
                                                Icon { name: "check", size: 12 }
                                            } else {
                                                "{idx + 1}"
                                            }
                                        }
                                        if idx + 1 < step_count {
                                            span { class: "wf-step-line" }
                                        }
                                    }
                                    button {
                                        class: "wf-step-toggle",
                                        r#type: "button",
                                        role: "checkbox",
                                        "aria-checked": if step_done { "true" } else { "false" },
                                        title: if step_done { "Mark step pending" } else { "Mark step done" },
                                        onclick: move |_| {
                                            let mut next = draft_steps.read().clone();
                                            if let Some(item) = next.get_mut(idx) {
                                                item.done = !item.done;
                                            }
                                            let payload: Vec<(String, bool)> = next
                                                .iter()
                                                .map(|s| (s.title.clone(), s.done))
                                                .collect();
                                            draft_steps.set(next);
                                            session.write().update_active_workflow(
                                                conv_id_step.clone(),
                                                None,
                                                Some(payload),
                                            );
                                        },
                                        span { class: "wf-step-text", "{step_title}" }
                                        span { class: "wf-step-badge", "{step_badge}" }
                                    }
                                }
                            }
                        }
                    }
                }
            }
            div { class: "wf-plan-add-row",
                input {
                    class: "input wf-input",
                    r#type: "text",
                    maxlength: "240",
                    placeholder: "Add step to this plan…",
                    "aria-label": "New step title",
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
                            let payload: Vec<(String, bool)> = next
                                .iter()
                                .map(|s| (s.title.clone(), s.done))
                                .collect();
                            draft_steps.set(next);
                            new_step_text.set(String::new());
                            session.write().update_active_workflow(
                                conv_id_add.clone(),
                                None,
                                Some(payload),
                            );
                        }
                    },
                    Icon { name: "plus", size: 12 }
                    "Add Step"
                }
            }
            div { class: "wf-card-actions",
                button {
                    class: if selected { "btn sm wf-btn-active" } else { "btn ghost sm" },
                    r#type: "button",
                    onclick: move |_| on_select.call(conv_id_select.clone()),
                    "Flow"
                }
                button {
                    class: "btn ghost sm",
                    r#type: "button",
                    onclick: move |_| editing_goal.toggle(),
                    Icon { name: "edit", size: 12 }
                    if *editing_goal.read() { "Cancel" } else { "Rename Goal" }
                }
                button {
                    class: "btn ghost sm wf-chat-link",
                    r#type: "button",
                    title: "Open conversation ({plan.conversation_id})",
                    onclick: move |_| session.write().open_conversation(conv_id_open.clone()),
                    Icon { name: "chat", size: 12 }
                    "Open in Chat"
                }
            }
        }
    }
}

/// History row rendered in the History tab from existing source & routine snapshots.
#[component]
fn HistoryRow(entry: WorkflowHistoryEntry) -> Element {
    rsx! {
        div { class: "wf-history-row", role: "listitem",
            div { class: "wf-history-main",
                div { class: "wf-card-title-row",
                    span { class: "wf-slot-tag", "{entry.kind_label}" }
                    span { class: "wf-name", "{entry.title}" }
                }
                span { class: "wf-meta", "{entry.summary}" }
            }
            div { class: "wf-history-right",
                span { class: "wf-meta mono", "{entry.when_label}" }
                WfStatusPill { status: entry.status }
            }
        }
    }
}

#[component]
pub fn WorkflowsView(session: Signal<Session>) -> Element {
    let mut session = session;
    let mut active_tab = use_signal(|| WorkflowTab::Workflows);
    let mut create_open = use_signal(|| false);
    let mut create_mode = use_signal(|| "routine".to_owned());
    let mut query = use_signal(String::new);
    let mut filter_mode = use_signal(|| WorkflowFilter::All);
    let mut selected = use_signal(|| Option::<SelectedWorkflow>::None);
    let mut selected_node_id = use_signal(|| Option::<String>::None);
    let mut editing_routine = use_signal(|| Option::<String>::None);
    let mut editing_source = use_signal(|| Option::<String>::None);

    let (snapshot, loaded, error, names, conv_bots, watcher_name, watcher_avatar) = {
        let state = session.read();
        let names: std::collections::HashMap<String, String> = state
            .state
            .conversations
            .iter()
            .map(|c| (c.id.clone(), state.conversation_label(c)))
            .collect();
        let conv_bots: std::collections::HashMap<String, (String, String, String)> = state
            .state
            .workflows
            .active
            .iter()
            .map(|p| {
                let bot_id = state
                    .conversation_bot(&p.conversation_id)
                    .unwrap_or_else(|| "main".to_owned());
                let name = state.bot_display_name(&bot_id);
                let avatar = state.bot_avatar(&bot_id);
                (p.conversation_id.clone(), (bot_id, name, avatar))
            })
            .collect();
        (
            state.state.workflows.clone(),
            state.state.workflows_loaded,
            state.state.workflows_error.clone(),
            names,
            conv_bots,
            state.bot_display_name("watcher"),
            state.bot_avatar("watcher"),
        )
    };

    let now_secs = workflows::now_secs();
    let overview = workflows::compute_overview_stats(&snapshot, now_secs);
    let history_entries = workflows::collect_history_entries(&snapshot, now_secs);
    let has_history = !history_entries.is_empty();

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
        } else if let Some(p) = snapshot.active.first() {
            Some(SelectedWorkflow::Plan(p.conversation_id.clone()))
        } else {
            snapshot
                .sources
                .first()
                .map(|s| SelectedWorkflow::Source(s.name.clone()))
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

    let needle = query.read().clone();
    let mode = *filter_mode.read();
    let filtered_routines: Vec<Routine> = snapshot
        .routines
        .iter()
        .filter(|r| {
            let status = workflows::routine_status(r, &snapshot.sources);
            let mode_ok = match mode {
                WorkflowFilter::All => true,
                WorkflowFilter::Enabled => r.enabled,
                WorkflowFilter::Failing => status == WorkflowStatus::Failed,
            };
            let tz = r.timezone.as_deref().unwrap_or("");
            let lbl = r.label.as_deref().unwrap_or("");
            let title = workflows::routine_display_title(r);
            mode_ok && workflows::matches_workflow_filter(&needle, &[&r.name, tz, lbl, &title])
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
            let mode_ok = mode == WorkflowFilter::All || mode == WorkflowFilter::Enabled;
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
    let filtered_history: Vec<WorkflowHistoryEntry> = history_entries
        .iter()
        .filter(|h| {
            let mode_ok = match mode {
                WorkflowFilter::All => true,
                WorkflowFilter::Enabled => h.status != WorkflowStatus::Paused,
                WorkflowFilter::Failing => h.status == WorkflowStatus::Failed,
            };
            mode_ok
                && workflows::matches_workflow_filter(
                    &needle,
                    &[&h.title, h.kind_label, &h.summary],
                )
        })
        .cloned()
        .collect();

    let current_tab = if !has_history && *active_tab.read() == WorkflowTab::History {
        WorkflowTab::Workflows
    } else {
        *active_tab.read()
    };

    let no_free_slots = free_slots.is_empty();
    let open_routine_creator = move |_| {
        create_mode.set(if no_free_slots {
            "source".into()
        } else {
            "routine".into()
        });
        create_open.set(true);
    };

    rsx! {
        div { class: "pane wf-page",
            div { class: "pane-head wf-hero",
                div { class: "wf-hero-copy",
                    h1 { class: "pane-title", "Workflows" }
                    p { class: "muted",
                        "Scheduled routines, monitoring feeds and multi-step plans running across your bots."
                    }
                }
                div { class: "pane-actions wf-head-actions",
                    button {
                        class: "btn primary sm",
                        r#type: "button",
                        disabled: !loaded,
                        onclick: open_routine_creator,
                        Icon { name: "plus", size: 14 }
                        if no_free_slots { "New Feed" } else { "New Routine" }
                    }
                    if has_brief {
                        button {
                            class: "btn sm",
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
                        "aria-label": "Refresh workflows",
                        onclick: move |_| session.write().refresh_workflows(),
                        Icon { name: "refresh", size: 14 }
                        "Refresh"
                    }
                }
            }
            div { class: "pane-body",
                if let Some(message) = error.as_ref() {
                    div { class: "wf-banner err", role: "alert",
                        span { "{message}" }
                        button {
                            class: "btn ghost sm",
                            r#type: "button",
                            onclick: move |_| session.write().refresh_workflows(),
                            "Retry"
                        }
                    }
                }
                if !loaded && error.is_none() {
                    div { class: "wf-skeleton", role: "status", "aria-label": "Loading workflows",
                        div { class: "wf-hero-stats",
                            for n in 0..4 {
                                div { key: "{n}", class: "skeleton wf-skeleton-tile" }
                            }
                        }
                        div { class: "skeleton wf-skeleton-canvas" }
                        div { class: "wf-skeleton-cards",
                            for n in 0..4 {
                                div { key: "{n}", class: "skeleton wf-skeleton-card" }
                            }
                        }
                    }
                } else {
                    div { class: "wf-hero-stats", role: "region", "aria-label": "Workflow metrics",
                        div { class: "stat-tile wf-stat-card",
                            span { class: "stat-tile-value wf-stat-value", "{overview.active_routines}" }
                            span { class: "stat-tile-label wf-stat-label", "Active routines" }
                            span { class: "stat-tile-sub", "{overview.total_routines} configured" }
                        }
                        div { class: "stat-tile wf-stat-card",
                            span { class: "stat-tile-value wf-stat-value", "{overview.next_run_relative}" }
                            span { class: "stat-tile-label wf-stat-label", "Next scheduled run" }
                            span { class: "stat-tile-sub", "{overview.next_run_target}" }
                        }
                        div {
                            class: if overview.failing_sources > 0 { "stat-tile wf-stat-card wf-stat-err" } else { "stat-tile wf-stat-card" },
                            span { class: "stat-tile-value wf-stat-value", "{overview.failing_sources}" }
                            span { class: "stat-tile-label wf-stat-label", "Last failures" }
                            span { class: "stat-tile-sub", "{overview.healthy_sources}/{overview.total_sources} sources healthy" }
                        }
                        div { class: "stat-tile wf-stat-card",
                            span { class: "stat-tile-value wf-stat-value", "{overview.active_plans}" }
                            span { class: "stat-tile-label wf-stat-label", "Active plans" }
                            span { class: "stat-tile-sub", "TZ: {tz_display}" }
                        }
                    }

                    div { class: "wf-toolbar",
                        div { class: "seg wf-view-seg", role: "group", "aria-label": "Workflows section",
                            button {
                                class: if current_tab == WorkflowTab::Workflows { "seg-btn active" } else { "seg-btn" },
                                r#type: "button",
                                "aria-pressed": if current_tab == WorkflowTab::Workflows { "true" } else { "false" },
                                onclick: move |_| active_tab.set(WorkflowTab::Workflows),
                                "Workflows"
                            }
                            button {
                                class: if current_tab == WorkflowTab::Routines { "seg-btn active" } else { "seg-btn" },
                                r#type: "button",
                                "aria-pressed": if current_tab == WorkflowTab::Routines { "true" } else { "false" },
                                onclick: move |_| active_tab.set(WorkflowTab::Routines),
                                "Routines ({snapshot.routines.len()})"
                            }
                            if has_history {
                                button {
                                    class: if current_tab == WorkflowTab::History { "seg-btn active" } else { "seg-btn" },
                                    r#type: "button",
                                    "aria-pressed": if current_tab == WorkflowTab::History { "true" } else { "false" },
                                    onclick: move |_| active_tab.set(WorkflowTab::History),
                                    "History ({history_entries.len()})"
                                }
                            }
                        }
                        input {
                            class: "input wf-search",
                            r#type: "search",
                            placeholder: "Filter workflows, routines or feeds…",
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
                                "Active"
                            }
                            button {
                                class: if mode == WorkflowFilter::Failing { "seg-btn active" } else { "seg-btn" },
                                r#type: "button",
                                onclick: move |_| filter_mode.set(WorkflowFilter::Failing),
                                "Failing"
                            }
                        }
                    }

                    if current_tab == WorkflowTab::Workflows {
                        if let Some(graph) = active_graph {
                            {
                                let sel_for_click = active_selection.clone();
                                let active_for_click = snapshot.active.clone();
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
                                                Some(SelectedWorkflow::Plan(id)) if node_id.starts_with("step-") => {
                                                    if let Some(idx) = node_id
                                                        .strip_prefix("step-")
                                                        .and_then(|s| s.parse::<usize>().ok())
                                                        .and_then(|one_based| one_based.checked_sub(1))
                                                        && let Some(plan) = active_for_click.iter().find(|p| &p.conversation_id == id)
                                                        && !plan.items.is_empty()
                                                    {
                                                        let items_payload: Vec<(String, bool)> = plan
                                                            .items
                                                            .iter()
                                                            .enumerate()
                                                            .map(|(i, s)| {
                                                                (
                                                                    s.title.clone(),
                                                                    if i == idx { !s.done } else { s.done },
                                                                )
                                                            })
                                                            .collect();
                                                        session.write().update_active_workflow(
                                                            id.clone(),
                                                            None,
                                                            Some(items_payload),
                                                        );
                                                    }
                                                }
                                                _ => {}
                                            }
                                        },
                                    }
                                }
                            }
                        }

                        div { class: "wf-studio-grid",
                            section { class: "wf-section wf-section-wide",
                                div { class: "wf-section-head",
                                    h2 { class: "wf-section-title", "Multi-step workflow plans" }
                                    span { class: "wf-section-count", "{filtered_active.len()}" }
                                }
                                if !snapshot.active_supported {
                                    WfEmpty {
                                        icon: "alert",
                                        title: "Plans unavailable",
                                        text: "This server does not expose /v1/workflows/active yet. Update SeraBot to track multi-step plans."
                                    }
                                } else if snapshot.active.is_empty() {
                                    WfEmpty {
                                        icon: "check",
                                        title: "No active multi-step plans",
                                        text: "When you ask a bot to handle a multi-step goal in Chat, its progress timeline appears here."
                                    }
                                } else if filtered_active.is_empty() {
                                    WfEmpty {
                                        icon: "search",
                                        title: "No plans match",
                                        text: "Clear the search or switch the state filter."
                                    }
                                } else {
                                    div { class: "wf-cards card-grid", role: "list",
                                        for plan in filtered_active {
                                            {
                                                let is_sel = active_selection.as_ref()
                                                    == Some(&SelectedWorkflow::Plan(plan.conversation_id.clone()));
                                                let (bot_id, bot_name, bot_avatar) = conv_bots
                                                    .get(&plan.conversation_id)
                                                    .cloned()
                                                    .unwrap_or_else(|| ("main".into(), "Assistant".into(), "A".into()));
                                                rsx! {
                                                    ActivePlanCard {
                                                        key: "{plan.conversation_id}",
                                                        session,
                                                        conversation: names.get(&plan.conversation_id).cloned(),
                                                        plan,
                                                        bot_id,
                                                        bot_name,
                                                        bot_avatar,
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

                            section { class: "wf-section",
                                div { class: "wf-section-head",
                                    h2 { class: "wf-section-title", "Scheduled routines" }
                                    div { class: "row-actions",
                                        span { class: "wf-section-count", "{filtered_routines.len()}" }
                                        if !free_slots.is_empty() {
                                            button {
                                                class: "btn ghost sm",
                                                r#type: "button",
                                                onclick: move |_| {
                                                    create_mode.set("routine".into());
                                                    create_open.set(true);
                                                },
                                                Icon { name: "plus", size: 12 }
                                                "Add"
                                            }
                                        }
                                    }
                                }
                                if !snapshot.routines_supported {
                                    WfEmpty {
                                        icon: "alert",
                                        title: "Routines unavailable",
                                        text: "This server does not expose /v1/routines yet. Update SeraBot to manage schedules."
                                    }
                                } else if snapshot.routines.is_empty() {
                                    WfEmpty {
                                        icon: "workflows",
                                        title: "No routines scheduled yet",
                                        text: "Schedule a morning brief or recurring watch digest to run automatically.",
                                        cta_label: (!free_slots.is_empty()).then_some("New Routine"),
                                        on_cta: EventHandler::new(move |_| {
                                            create_mode.set("routine".into());
                                            create_open.set(true);
                                        }),
                                    }
                                } else if filtered_routines.is_empty() {
                                    WfEmpty {
                                        icon: "search",
                                        title: "No routines match",
                                        text: "Clear the search or switch the state filter."
                                    }
                                } else {
                                    div { class: "wf-cards card-grid", role: "list",
                                        for routine in filtered_routines {
                                            {
                                                let is_sel = active_selection.as_ref()
                                                    == Some(&SelectedWorkflow::Routine(routine.name.clone()));
                                                rsx! {
                                                    RoutineCard {
                                                        key: "{routine.name}",
                                                        session,
                                                        routine,
                                                        sources: snapshot.sources.clone(),
                                                        bot_name: watcher_name.clone(),
                                                        bot_avatar: watcher_avatar.clone(),
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
                            }

                            section { class: "wf-section",
                                div { class: "wf-section-head",
                                    h2 { class: "wf-section-title", "Monitoring sources" }
                                    div { class: "row-actions",
                                        span { class: "wf-section-count", "{filtered_sources.len()}" }
                                        if snapshot.sources_supported {
                                            button {
                                                class: "btn ghost sm",
                                                r#type: "button",
                                                onclick: move |_| {
                                                    create_mode.set("source".into());
                                                    create_open.set(true);
                                                },
                                                Icon { name: "plus", size: 12 }
                                                "Add Feed"
                                            }
                                        }
                                    }
                                }
                                if !snapshot.sources_supported {
                                    WfEmpty {
                                        icon: "alert",
                                        title: "Sources unavailable",
                                        text: "This server does not expose /v1/monitoring/sources yet. Update SeraBot to watch feeds."
                                    }
                                } else if snapshot.sources.is_empty() {
                                    WfEmpty {
                                        icon: "globe",
                                        title: "No monitoring sources yet",
                                        text: "Add an RSS or Atom feed and Watcher will scan it on schedule.",
                                        cta_label: Some("Add RSS Feed"),
                                        on_cta: EventHandler::new(move |_| {
                                            create_mode.set("source".into());
                                            create_open.set(true);
                                        }),
                                    }
                                } else if filtered_sources.is_empty() {
                                    WfEmpty {
                                        icon: "search",
                                        title: "No sources match",
                                        text: "Clear the search or switch the state filter."
                                    }
                                } else {
                                    div { class: "wf-cards card-grid", role: "list",
                                        for source in filtered_sources {
                                            {
                                                let is_sel = active_selection.as_ref()
                                                    == Some(&SelectedWorkflow::Source(source.name.clone()));
                                                rsx! {
                                                    SourceCard {
                                                        key: "{source.name}",
                                                        session,
                                                        source,
                                                        bot_name: watcher_name.clone(),
                                                        bot_avatar: watcher_avatar.clone(),
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
                            }
                        }
                    } else if current_tab == WorkflowTab::Routines {
                        section { class: "wf-section",
                            div { class: "wf-section-head",
                                h2 { class: "wf-section-title", "All scheduled routines" }
                                div { class: "row-actions",
                                    span { class: "wf-section-count", "{filtered_routines.len()}" }
                                    if !free_slots.is_empty() {
                                        button {
                                            class: "btn primary sm",
                                            r#type: "button",
                                            onclick: move |_| {
                                                create_mode.set("routine".into());
                                                create_open.set(true);
                                            },
                                            Icon { name: "plus", size: 13 }
                                            "New Routine"
                                        }
                                    }
                                }
                            }
                            if !snapshot.routines_supported {
                                WfEmpty {
                                    icon: "alert",
                                    title: "Routines unavailable",
                                    text: "This server does not expose /v1/routines yet."
                                }
                            } else if snapshot.routines.is_empty() {
                                WfEmpty {
                                    icon: "workflows",
                                    title: "No routines scheduled yet",
                                    text: "Create your morning brief or a watch routine to automate recurring digests.",
                                    cta_label: (!free_slots.is_empty()).then_some("New Routine"),
                                    on_cta: EventHandler::new(move |_| {
                                        create_mode.set("routine".into());
                                        create_open.set(true);
                                    }),
                                }
                            } else if filtered_routines.is_empty() {
                                WfEmpty {
                                    icon: "search",
                                    title: "No routines match",
                                    text: "Clear the search or switch the state filter."
                                }
                            } else {
                                div { class: "wf-routines-grid card-grid", role: "list",
                                    for routine in filtered_routines {
                                        {
                                            let is_sel = active_selection.as_ref()
                                                == Some(&SelectedWorkflow::Routine(routine.name.clone()));
                                            rsx! {
                                                RoutineCard {
                                                    key: "{routine.name}",
                                                    session,
                                                    routine,
                                                    sources: snapshot.sources.clone(),
                                                    bot_name: watcher_name.clone(),
                                                    bot_avatar: watcher_avatar.clone(),
                                                    selected: is_sel,
                                                    editing_routine,
                                                    on_select: move |name: String| {
                                                        selected_node_id.set(None);
                                                        selected.set(Some(SelectedWorkflow::Routine(name)));
                                                        active_tab.set(WorkflowTab::Workflows);
                                                    },
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    } else {
                        section { class: "wf-section",
                            div { class: "wf-section-head",
                                h2 { class: "wf-section-title", "Recent runs & checks" }
                                span { class: "wf-section-count", "{filtered_history.len()}" }
                            }
                            if filtered_history.is_empty() {
                                WfEmpty {
                                    icon: "search",
                                    title: "No matching history",
                                    text: "Clear the filter or run a routine or source check."
                                }
                            } else {
                                div { class: "wf-history-list", role: "list",
                                    for entry in filtered_history {
                                        HistoryRow { key: "{entry.id}", entry }
                                    }
                                }
                            }
                        }
                    }
                }
            }

            if *create_open.read() {
                CreateWorkflowSheet {
                    session,
                    free_slots,
                    default_tz: tz_display,
                    initial_mode: create_mode.read().clone(),
                    on_close: move |_| create_open.set(false),
                    on_created_routine: move |slot: String| {
                        selected_node_id.set(None);
                        selected.set(Some(SelectedWorkflow::Routine(slot)));
                    },
                    on_created_source: move |name: String| {
                        selected_node_id.set(None);
                        selected.set(Some(SelectedWorkflow::Source(name)));
                    },
                }
            }
        }
    }
}
