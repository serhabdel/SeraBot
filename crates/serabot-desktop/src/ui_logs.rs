//! The Logs tab (0.28.0): level filter chips plus a search box over
//! `GET /v1/logs?level=&q=`. Until the server ships the route (404) the tab
//! says so instead of showing an empty list; entries render as plain text
//! rows, newest last, in a scrollable pane.

use crate::ui::Icon;
use crate::ui_files::{CONFIRM_RESET_JS, copy_script};
use dioxus::prelude::*;
use serabot_desktop::logs::{self, LogEntry};
use serabot_desktop::session::Session;

/// One log row with click-to-copy line feedback.
#[component]
fn LogRow(entry: LogEntry) -> Element {
    let mut copied = use_signal(|| false);
    let class = logs::level_class(&entry.level);
    let plain = logs::format_line(&entry);
    rsx! {
        div { class: "log-row {class}",
            span { class: "log-time", title: "{entry.ts}", "{logs::time_of(&entry.ts)}" }
            span { class: "log-level", "{entry.level}" }
            span { class: "log-target", title: "{entry.target}", "{entry.target}" }
            span { class: "log-message", "{entry.message}" }
            button {
                class: "btn ghost sm log-copy",
                title: "Copy log line",
                "aria-label": "Copy log line",
                onclick: move |_| {
                    let _ = document::eval(&copy_script(&plain));
                    copied.set(true);
                    spawn(async move {
                        let mut eval = document::eval(CONFIRM_RESET_JS);
                        let _ = eval.recv::<bool>().await;
                        copied.set(false);
                    });
                },
                if copied() {
                    Icon { name: "check", size: 12 }
                } else {
                    Icon { name: "copy", size: 12 }
                }
            }
        }
    }
}

#[component]
pub fn LogsView(session: Signal<Session>) -> Element {
    let mut session = session;
    let mut query = use_signal(|| session.read().logs_query.clone());
    let mut wrap = use_signal(|| false);
    let mut copied_all = use_signal(|| false);
    let (entries, supported, loaded, logs_error) = {
        let state = session.read();
        (
            state.state.logs.clone(),
            state.state.logs_supported,
            state.state.logs_loaded,
            state.state.logs_error.clone(),
        )
    };
    let level = session.read().logs_level.clone();
    let filtered = logs::filter_entries(&entries, level.as_deref(), &query());
    let total_count = entries.len();
    let shown_count = filtered.len();
    let has_filters = level.is_some() || !query().trim().is_empty();
    let mut level_buttons: Vec<(Option<&'static str>, String, &'static str)> =
        vec![(None, "All".to_owned(), "Show every log level")];
    for l in logs::LEVELS {
        let tip = match l {
            "trace" => "Show trace and above",
            "debug" => "Show debug and above",
            "info" => "Show info and above",
            "warn" => "Show warn and error",
            "error" => "Show errors only",
            _ => "Filter by minimum level",
        };
        level_buttons.push((Some(l), l.to_owned(), tip));
    }
    let body_class = if wrap() {
        "logs-body logs-wrap"
    } else {
        "logs-body"
    };
    let copy_all_text = if filtered.is_empty() {
        String::new()
    } else {
        logs::format_plain_lines(&filtered)
    };
    rsx! {
        div { class: "pane",
            div { class: "pane-head",
                div {
                    h1 { class: "pane-title", "Logs" }
                    p { class: "muted", "The server's recent log lines. Secrets are redacted server-side." }
                }
                div { class: "row-actions",
                    if loaded && total_count > 0 {
                        span { class: "pill muted", "{shown_count} of {total_count}" }
                    }
                    button {
                        class: "btn ghost sm",
                        disabled: !loaded && !supported,
                        onclick: move |_| session.write().refresh_logs(),
                        Icon { name: "refresh", size: 14 }
                        "Refresh"
                    }
                }
            }
            div { class: "pane-body",
                div { class: "logs-toolbar",
                    div { class: "seg logs-levels", role: "group", "aria-label": "Log level filter",
                        for (value, label, tip) in level_buttons {
                            button {
                                key: "{label}",
                                class: if level.as_deref() == value { "seg-btn active" } else { "seg-btn" },
                                r#type: "button",
                                title: "{tip}",
                                "aria-pressed": if level.as_deref() == value { "true" } else { "false" },
                                onclick: {
                                    let query = query();
                                    move |_| {
                                        let level = value.map(str::to_owned);
                                        session.write().set_logs_filter(level, query.clone());
                                    }
                                },
                                "{label}"
                            }
                        }
                    }
                    input {
                        class: "input logs-search",
                        r#type: "search",
                        placeholder: "Filter message or target (Enter fetches server)\u{2026}",
                        "aria-label": "Search logs or tool name",
                        value: "{query}",
                        oninput: move |event| query.set(event.value()),
                        onkeydown: move |event: KeyboardEvent| {
                            match event.key() {
                                Key::Enter => {
                                    let q = query();
                                    let level = session.read().logs_level.clone();
                                    session.write().set_logs_filter(level, q);
                                }
                                Key::Escape => {
                                    query.set(String::new());
                                    let level = session.read().logs_level.clone();
                                    session.write().set_logs_filter(level, String::new());
                                }
                                _ => {}
                            }
                        },
                    }
                    button {
                        class: "btn primary sm",
                        onclick: move |_| {
                            let q = query();
                            let level = session.read().logs_level.clone();
                            session.write().set_logs_filter(level, q);
                        },
                        "Fetch"
                    }
                    if has_filters {
                        button {
                            class: "btn ghost sm",
                            onclick: move |_| {
                                query.set(String::new());
                                session.write().set_logs_filter(None, String::new());
                            },
                            "Clear"
                        }
                    }
                    button {
                        class: if wrap() { "btn sm" } else { "btn ghost sm" },
                        "aria-pressed": if wrap() { "true" } else { "false" },
                        title: "Wrap long log lines",
                        onclick: move |_| wrap.toggle(),
                        "Wrap"
                    }
                    if !filtered.is_empty() {
                        button {
                            class: "btn ghost sm",
                            title: "Copy visible log lines",
                            onclick: move |_| {
                                let _ = document::eval(&copy_script(&copy_all_text));
                                copied_all.set(true);
                                spawn(async move {
                                    let mut eval = document::eval(CONFIRM_RESET_JS);
                                    let _ = eval.recv::<bool>().await;
                                    copied_all.set(false);
                                });
                            },
                            Icon { name: if copied_all() { "check" } else { "copy" }, size: 13 }
                            if copied_all() { "Copied" } else { "Copy" }
                        }
                        button {
                            class: "btn ghost sm",
                            title: "Jump to newest lines",
                            onclick: move |_| {
                                let _ = document::eval(
                                    "const el = document.getElementById('logs-body'); if (el) el.scrollTop = el.scrollHeight;"
                                );
                            },
                            Icon { name: "arrow-down", size: 13 }
                            "Latest"
                        }
                    }
                }
                div { id: "logs-body", class: "{body_class}", role: "log", "aria-label": "Server logs",
                    if !supported {
                        div { class: "empty logs-empty", role: "status",
                            span { class: "empty-icon", Icon { name: "alert", size: 20 } }
                            h2 { class: "empty-title", "Logs are not available" }
                            p { class: "empty-text",
                                "This server does not expose logs yet (no /v1/logs route). Update SeraBot to see them."
                            }
                        }
                    } else if !loaded {
                        div { class: "logs-skeleton", role: "status", "aria-label": "Loading logs",
                            for n in 0..8 {
                                div { key: "{n}", class: "skeleton logs-skeleton-row" }
                            }
                        }
                    } else if let Some(err) = logs_error.as_deref().filter(|_| entries.is_empty()) {
                        div { class: "empty logs-empty", role: "alert",
                            span { class: "empty-icon", Icon { name: "alert", size: 20 } }
                            h2 { class: "empty-title", "Could not load logs" }
                            p { class: "empty-text", "{err}" }
                            button {
                                class: "btn ghost sm",
                                onclick: move |_| session.write().refresh_logs(),
                                "Retry"
                            }
                        }
                    } else if entries.is_empty() {
                        div { class: "empty logs-empty",
                            span { class: "empty-icon", Icon { name: "logs", size: 20 } }
                            h2 { class: "empty-title", "No log entries yet" }
                            p { class: "empty-text",
                                "Logs appear once the server starts a task, a routine or a sign-in."
                            }
                            button {
                                class: "btn ghost sm",
                                onclick: move |_| session.write().refresh_logs(),
                                "Refresh"
                            }
                        }
                    } else if filtered.is_empty() {
                        if let Some(err) = logs_error.as_deref() {
                            div { class: "banner err", role: "alert", "{err}" }
                        }
                        div { class: "empty logs-empty",
                            span { class: "empty-icon", Icon { name: "search", size: 20 } }
                            h2 { class: "empty-title", "No lines match these filters" }
                            p { class: "empty-text", "Clear the level or search to see every line." }
                            button {
                                class: "btn ghost sm",
                                onclick: move |_| {
                                    query.set(String::new());
                                    session.write().set_logs_filter(None, String::new());
                                },
                                "Clear filters"
                            }
                        }
                    } else {
                        if let Some(err) = logs_error.as_deref() {
                            div { class: "banner err", role: "alert", "{err}" }
                        }
                        for (index, entry) in filtered.into_iter().enumerate() {
                            LogRow { key: "{index}", entry: entry.clone() }
                        }
                    }
                }
            }
        }
    }
}
