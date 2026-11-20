//! Alerts and backups cards (0.68) on the Usage tab.
//!
//! - **Alerts** (every member): where SeraBot's own alerts arrive (Telegram,
//!   and optionally one WhatsApp chat) and the last ones sent.
//! - **Backups** (owner): the last nightly backup, its test restore, the
//!   OneDrive copy, the files kept, and "Back up now".

use dioxus::prelude::*;
use serde_json::Value;

use crate::ui::Icon;
use crate::ui_widgets::StatusChip;
use serabot_desktop::ops_view::{self as ops, alert_kind_label, onedrive_line};
use serabot_desktop::session::Session;
use serabot_desktop::theme::Tone;
use serabot_desktop::usage_insights::human_bytes;
use serabot_desktop::worker::Credentials;

async fn call(
    session: Signal<Session>,
    job: impl FnOnce(&Credentials) -> Result<Value, String> + Send + 'static,
) -> Result<Value, String> {
    let Some(credentials) = session.read().credentials.clone() else {
        return Err("Not connected to the server.".into());
    };
    let (tx, rx) = tokio::sync::oneshot::channel();
    let spawned = std::thread::Builder::new()
        .name("serabot-ops".into())
        .spawn(move || {
            let _ = tx.send(job(&credentials));
        });
    if spawned.is_err() {
        return Err("Could not start the request.".into());
    }
    rx.await
        .unwrap_or_else(|_| Err("The request was interrupted.".into()))
}

fn short_time(at: &str) -> String {
    at.get(..16).unwrap_or(at).replace('T', " ")
}

#[component]
pub fn AlertsCard(session: Signal<Session>) -> Element {
    let mut view = use_signal(|| Option::<Value>::None);
    let mut error = use_signal(|| Option::<String>::None);
    let mut busy = use_signal(|| false);
    use_hook(move || {
        spawn(async move {
            match call(session, ops::get_alerts).await {
                Ok(value) => view.set(Some(value)),
                Err(message) => error.set(Some(message)),
            }
        });
    });
    let mut save = move |telegram: bool, whatsapp: Option<String>| {
        busy.set(true);
        spawn(async move {
            let result = call(session, move |credentials| {
                ops::put_alerts(credentials, telegram, whatsapp)
            })
            .await;
            busy.set(false);
            match result {
                Ok(value) => {
                    error.set(None);
                    view.set(Some(value));
                }
                Err(message) => error.set(Some(message)),
            }
        });
    };
    let Some(current) = view() else {
        return rsx! {
            div { class: "card insights-card",
                div { class: "card-head", h3 { class: "card-title", "Alerts" } }
                if let Some(message) = error() {
                    p { class: "tiny", role: "alert", "{message}" }
                } else {
                    p { class: "tiny", "Loading\u{2026}" }
                }
            }
        };
    };
    let telegram = current["settings"]["telegram"].as_bool().unwrap_or(true);
    let telegram_linked = current["telegram_linked"].as_bool().unwrap_or(false);
    let whatsapp = current["settings"]["whatsapp_conversation_id"]
        .as_str()
        .map(str::to_string);
    let choices: Vec<(String, String)> = current["whatsapp_choices"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|choice| {
            Some((
                choice["conversation_id"].as_str()?.to_string(),
                choice["label"]
                    .as_str()
                    .unwrap_or("WhatsApp chat")
                    .to_string(),
            ))
        })
        .collect();
    let recent: Vec<(String, String, String)> = current["recent"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|alert| {
            (
                alert["at"].as_str().unwrap_or_default().to_string(),
                alert["kind"].as_str().unwrap_or_default().to_string(),
                alert["text"].as_str().unwrap_or_default().to_string(),
            )
        })
        .collect();
    let thresholds = &current["thresholds"];
    let disk = thresholds["disk_percent"].as_u64().unwrap_or(90);
    let failovers = thresholds["failovers_in_15_min"].as_u64().unwrap_or(3);
    let enabled = current["enabled"].as_bool().unwrap_or(true);
    let whatsapp_for_toggle = whatsapp.clone();
    rsx! {
        div { class: "card insights-card",
            div { class: "card-head",
                h3 { class: "card-title", "Alerts" }
                if enabled {
                    StatusChip { tone: Tone::Ok, text: "On".to_string() }
                } else {
                    StatusChip { tone: Tone::Muted, text: "Off on this server".to_string() }
                }
            }
            p { class: "tiny",
                "SeraBot tells you when a model keeps failing over ({failovers} times in 15 minutes), a sign-in expires, the PC tunnel drops during a job, or (owner) the disk passes {disk}% or a backup fails."
            }
            label { class: "insights-label",
                input {
                    r#type: "checkbox",
                    checked: telegram,
                    disabled: busy(),
                    onchange: move |event| save(event.checked(), whatsapp_for_toggle.clone()),
                }
                if telegram_linked {
                    "Send to my Telegram chat"
                } else {
                    "Send to my Telegram chat (link Telegram first)"
                }
            }
            div { class: "insights-label",
                span { "WhatsApp chat" }
                select {
                    class: "input",
                    "aria-label": "WhatsApp chat for alerts",
                    disabled: busy() || choices.is_empty(),
                    onchange: move |event| {
                        let value = event.value();
                        save(telegram, (!value.is_empty()).then_some(value));
                    },
                    option { value: "", selected: whatsapp.is_none(), "None" }
                    for (id, label) in choices.clone() {
                        option { key: "{id}", value: "{id}", selected: whatsapp.as_deref() == Some(id.as_str()), "{label}" }
                    }
                }
            }
            if choices.is_empty() {
                p { class: "tiny", "WhatsApp appears here once one of your allowed chats has talked to the WhatsApp bot." }
            }
            if let Some(message) = error() {
                p { class: "tiny", role: "alert", "{message}" }
            }
            if recent.is_empty() {
                p { class: "tiny", "No alerts yet." }
            } else {
                div { class: "cat-list",
                    for (index, (at, kind, text)) in recent.into_iter().take(6).enumerate() {
                        div { key: "{index}-{at}", class: "cat-row",
                            span { class: "cat-label", "{alert_kind_label(&kind)}" }
                            span { class: "tiny", title: "{text}", "{text}" }
                            span { class: "cat-size", "{short_time(&at)}" }
                        }
                    }
                }
            }
        }
    }
}

#[component]
pub fn BackupsCard(session: Signal<Session>) -> Element {
    let mut view = use_signal(|| Option::<Value>::None);
    let mut error = use_signal(|| Option::<String>::None);
    let refresh = move || {
        spawn(async move {
            match call(session, ops::get_backups).await {
                Ok(value) => {
                    error.set(None);
                    view.set(Some(value));
                }
                Err(message) => error.set(Some(message)),
            }
        });
    };
    use_hook(refresh);
    let in_progress = view()
        .as_ref()
        .is_some_and(|value| value["in_progress"].as_bool().unwrap_or(false));
    // While a backup runs, check again every few seconds.
    use_effect(move || {
        let running = view
            .read()
            .as_ref()
            .is_some_and(|value| value["in_progress"].as_bool().unwrap_or(false));
        if running {
            spawn(async move {
                tokio::time::sleep(std::time::Duration::from_secs(4)).await;
                refresh();
            });
        }
    });
    let run_now = move |_| {
        spawn(async move {
            match call(session, ops::run_backup).await {
                Ok(value) => {
                    error.set(None);
                    view.set(Some(value));
                }
                Err(message) => error.set(Some(message)),
            }
        });
    };
    let Some(current) = view() else {
        return rsx! {
            div { class: "card insights-card",
                div { class: "card-head", h3 { class: "card-title", "Backups" } }
                if let Some(message) = error() {
                    p { class: "tiny", role: "alert", "{message}" }
                } else {
                    p { class: "tiny", "Loading\u{2026}" }
                }
            }
        };
    };
    let last = &current["last"];
    let has_last = last.is_object();
    let ok = last["ok"].as_bool().unwrap_or(false);
    let restore_ok = last["restore_ok"].as_bool().unwrap_or(false);
    let at = short_time(last["at"].as_str().unwrap_or_default());
    let bytes = last["bytes"].as_i64().unwrap_or(0);
    let detail = last["restore_detail"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    let onedrive = onedrive_line(last["onedrive"].as_str().unwrap_or_default());
    let failure = last["error"].as_str().map(str::to_string);
    let hour = current["hour_utc"].as_u64().unwrap_or(3);
    let keep = current["keep"].as_u64().unwrap_or(7);
    let enabled = current["enabled"].as_bool().unwrap_or(false);
    let files: Vec<(String, i64)> = current["files"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|file| {
            (
                file["file"].as_str().unwrap_or_default().to_string(),
                file["bytes"].as_i64().unwrap_or(0),
            )
        })
        .collect();
    rsx! {
        div { class: "card insights-card",
            div { class: "card-head",
                h3 { class: "card-title", "Backups" }
                if in_progress {
                    span { class: "spinner", title: "Backing up" }
                } else if !has_last {
                    StatusChip { tone: Tone::Muted, text: "None yet".to_string() }
                } else if ok {
                    StatusChip { tone: Tone::Ok, text: "Healthy".to_string() }
                } else {
                    StatusChip { tone: Tone::Danger, text: "Last one failed".to_string() }
                }
            }
            p { class: "tiny",
                if enabled {
                    "Every day at {hour}:00 UTC the database is copied, encrypted with the server key and test restored. The last {keep} are kept."
                } else {
                    "Nightly backups are off on this server ([ops] backup_enabled)."
                }
            }
            if has_last {
                div { class: "insights-kpis",
                    div { class: "insights-kpi",
                        span { class: "stat-tile-label", "Last backup" }
                        span { class: "stat-tile-value", "{at}" }
                        span { class: "stat-tile-sub", "{human_bytes(bytes)} \u{b7} {onedrive}" }
                    }
                    div { class: "insights-kpi",
                        span { class: "stat-tile-label", "Test restore" }
                        span { class: if restore_ok { "stat-tile-value" } else { "stat-tile-value warn-text" },
                            if restore_ok { "Passed" } else { "Failed" }
                        }
                        span { class: "stat-tile-sub", "{detail}" }
                    }
                }
            }
            if let Some(message) = failure {
                p { class: "tiny warn-text", role: "alert", "{message}" }
            }
            if let Some(message) = error() {
                p { class: "tiny", role: "alert", "{message}" }
            }
            div {
                button {
                    class: "btn sm",
                    r#type: "button",
                    disabled: in_progress,
                    onclick: run_now,
                    Icon { name: "refresh", size: 14 }
                    if in_progress { "Backing up\u{2026}" } else { "Back up and test now" }
                }
            }
            if !files.is_empty() {
                div { class: "cat-list",
                    for (file, size) in files {
                        div { key: "{file}", class: "cat-row",
                            span { class: "cat-label", "{file}" }
                            span { class: "tiny", "encrypted" }
                            span { class: "cat-size", "{human_bytes(size)}" }
                        }
                    }
                }
            }
        }
    }
}
