//! The Logs tab (0.28.0): level filter chips plus a search box over
//! `GET /v1/logs?level=&q=`. Until the server ships the route (404) the tab
//! says so instead of showing an empty list; entries render as plain text
//! rows, newest last, in a scrollable pane.

use crate::ui::Icon;
use dioxus::prelude::*;
use serabot_desktop::logs::{self, LogEntry};
use serabot_desktop::session::Session;

/// One log row.
#[component]
fn LogRow(entry: LogEntry) -> Element {
    let class = logs::level_class(&entry.level);
    rsx! {
        div { class: "log-row {class}",
            span { class: "log-time", "{logs::time_of(&entry.ts)}" }
            span { class: "log-level", "{entry.level}" }
            span { class: "log-target", "{entry.target}" }
            span { class: "log-message", "{entry.message}" }
        }
    }
}

#[component]
pub fn LogsView(session: Signal<Session>) -> Element {
    let mut session = session;
    let mut query = use_signal(|| session.read().logs_query.clone());
    let (entries, supported, loaded) = {
        let state = session.read();
        (
            state.state.logs.clone(),
            state.state.logs_supported,
            state.state.logs_loaded,
        )
    };
    let level = session.read().logs_level.clone();
    let filtered = logs::filter_entries(&entries, level.as_deref(), &query());
    let mut level_buttons: Vec<(Option<&'static str>, String)> = vec![(None, "All".to_owned())];
    for l in logs::LEVELS {
        level_buttons.push((Some(l), l.to_owned()));
    }
    rsx! {
        div { class: "pane",
            div { class: "pane-head",
                div {
                    h1 { class: "pane-title", "Logs" }
                    p { class: "muted", "The server's recent log lines. Secrets are redacted server-side." }
                }
                button {
                    class: "btn ghost sm",
                    disabled: !loaded && !supported,
                    onclick: move |_| session.write().refresh_logs(),
                    Icon { name: "refresh", size: 14 }
                    "Refresh"
                }
            }
            div { class: "logs-toolbar", role: "group", "aria-label": "Log level filter",
                for (value, label) in level_buttons {
                    button {
                        key: "{label}",
                        class: if level.as_deref() == value { "seg-btn active" } else { "seg-btn" },
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
                input {
                    class: "input logs-search",
                    r#type: "search",
                    placeholder: "Search logs or tool name\u{2026}",
                    "aria-label": "Search logs or tool name",
                    value: "{query}",
                    oninput: move |event| query.set(event.value()),
                    onkeydown: move |event: KeyboardEvent| {
                        if event.key() == Key::Enter {
                            let q = query();
                            let level = session.read().logs_level.clone();
                            session.write().set_logs_filter(level, q);
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
                    "Search"
                }
            }
            div { class: "logs-body", role: "log", "aria-label": "Server logs",
                if !supported {
                    p { class: "muted", role: "status",
                        "This server does not expose logs yet (no /v1/logs route). Update SeraBot to see them."
                    }
                } else if !loaded {
                    p { class: "muted", role: "status", "Loading logs\u{2026}" }
                } else if filtered.is_empty() {
                    p { class: "muted", role: "status", "No log lines match." }
                } else {
                    for (index, entry) in filtered.into_iter().enumerate() {
                        LogRow { key: "{index}", entry: entry.clone() }
                    }
                }
            }
        }
    }
}
