//! "Route model traffic through this PC" (0.66): turn the desktop network
//! relay on or off, choose providers, and see whether it is moving traffic.
//! Talks to `/v1/relay/net`; the relay threads themselves live in
//! `net_relay.rs` and run while signed in.

use std::sync::atomic::Ordering;

use dioxus::prelude::*;
use serde_json::Value;

use crate::ui::Icon;
use crate::ui_widgets::StatusChip;
use serabot_desktop::net_relay as relay;
use serabot_desktop::session::Session;
use serabot_desktop::theme::Tone;
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
        .name("serabot-relay-settings".into())
        .spawn(move || {
            let _ = tx.send(job(&credentials));
        });
    if spawned.is_err() {
        return Err("Could not start the request.".into());
    }
    rx.await
        .unwrap_or_else(|_| Err("The request was interrupted.".into()))
}

fn human_bytes(bytes: u64) -> String {
    if bytes >= 1024 * 1024 {
        format!("{:.1} MB", bytes as f64 / (1024.0 * 1024.0))
    } else if bytes >= 1024 {
        format!("{} KB", bytes / 1024)
    } else {
        format!("{bytes} B")
    }
}

#[component]
pub fn NetRelayCard(session: Signal<Session>) -> Element {
    let mut view = use_signal(|| Option::<Value>::None);
    let mut error = use_signal(|| Option::<String>::None);
    let mut busy = use_signal(|| false);
    let refresh = move || {
        spawn(async move {
            match call(session, relay::get_settings).await {
                Ok(value) => view.set(Some(value)),
                Err(message) => error.set(Some(message)),
            }
        });
    };
    use_hook(refresh);
    let mut save = move |enabled: bool, providers: Vec<String>| {
        busy.set(true);
        spawn(async move {
            let result = call(session, move |credentials| {
                relay::put_settings(credentials, enabled, &providers)
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
    let current = view();
    let enabled = current
        .as_ref()
        .and_then(|v| v["enabled"].as_bool())
        .unwrap_or(false);
    let providers: Vec<(String, String, bool)> = current
        .as_ref()
        .and_then(|v| v["providers"].as_array().cloned())
        .unwrap_or_default()
        .iter()
        .map(|p| {
            (
                p["kind"].as_str().unwrap_or_default().to_owned(),
                p["label"].as_str().unwrap_or_default().to_owned(),
                p["selected"].as_bool().unwrap_or(false),
            )
        })
        .collect();
    let selected: Vec<String> = providers
        .iter()
        .filter(|(_, _, on)| *on)
        .map(|(kind, _, _)| kind.clone())
        .collect();
    let stats = session.read().net_relay_stats();
    let (streams, up, down) = stats
        .as_ref()
        .map(|s| {
            (
                s.streams.load(Ordering::Relaxed),
                s.bytes_up.load(Ordering::Relaxed),
                s.bytes_down.load(Ordering::Relaxed),
            )
        })
        .unwrap_or_default();
    let (tone, label) = if !enabled {
        (Tone::Muted, "Off")
    } else if stats.is_none() {
        (Tone::Warn, "On, this PC is not connected")
    } else if streams > 0 {
        (Tone::Ok, "Relaying now")
    } else {
        (Tone::Ok, "On, ready")
    };
    rsx! {
        div { class: "card net-relay-card",
            div { class: "settings-card-head",
                div { class: "settings-card-head-copy",
                    h3 { class: "card-title", "Route model traffic through this PC" }
                    p { class: "settings-card-desc",
                        "Selected providers reach the internet from this PC while the app is open, encrypted end-to-end."
                    }
                }
                div { class: "settings-card-head-actions",
                    StatusChip { tone, text: label.to_string() }
                }
            }
            p { class: "tiny muted",
                "Only the listed provider hosts can be reached, never your home network. When this PC is off, those providers fail over to the next route."
            }
            if let Some(message) = error() {
                p { class: "field-error", role: "alert", "{message}" }
            }
            label { class: "settings-toggle-row",
                div { class: "settings-toggle-copy",
                    span { class: "settings-toggle-title", "Use this PC's internet for selected providers" }
                    span { class: "settings-toggle-desc", "This PC only relays encrypted TLS bytes and never sees keys or messages." }
                }
                input {
                    class: "switch",
                    r#type: "checkbox",
                    checked: enabled,
                    disabled: busy() || current.is_none(),
                    onchange: {
                        let selected = selected.clone();
                        move |event: FormEvent| {
                            let on = event.checked();
                            let mut chosen = selected.clone();
                            // Turning it on with nothing picked starts with
                            // Antigravity, the case this exists for.
                            if on && chosen.is_empty() {
                                chosen.push("google-antigravity".to_string());
                            }
                            save(on, chosen);
                        }
                    },
                }
            }
            div { class: "net-relay-providers", role: "group", "aria-label": "Providers",
                for (kind, name, on) in providers.clone() {
                    label { key: "{kind}", class: "toggle-row",
                        input {
                            r#type: "checkbox",
                            checked: on,
                            disabled: busy() || !enabled,
                            onchange: {
                                let selected = selected.clone();
                                let kind = kind.clone();
                                move |event: FormEvent| {
                                    let mut chosen: Vec<String> = selected
                                        .iter()
                                        .filter(|k| **k != kind)
                                        .cloned()
                                        .collect();
                                    if event.checked() {
                                        chosen.push(kind.clone());
                                    }
                                    save(true, chosen);
                                }
                            },
                        }
                        span { "{name}" }
                    }
                }
            }
            div { class: "settings-card-foot",
                if enabled && stats.is_some() {
                    span { class: "tiny muted mono", role: "status",
                        "{streams} open \u{b7} {human_bytes(up)} sent \u{b7} {human_bytes(down)} received"
                    }
                } else {
                    span { class: "tiny muted", "Provider terms apply to how you use your account." }
                }
                div { class: "settings-card-foot-actions",
                    button {
                        class: "btn ghost sm",
                        r#type: "button",
                        onclick: move |_| refresh(),
                        Icon { name: "refresh", size: 13 }
                        "Refresh"
                    }
                    if enabled {
                        button {
                            class: "btn ghost sm",
                            r#type: "button",
                            disabled: busy(),
                            onclick: {
                                let selected = selected.clone();
                                move |_| save(false, selected.clone())
                            },
                            "Stop relaying"
                        }
                    }
                }
            }
        }
    }
}
