//! "Job search" card (0.70) on the Usage page: the profile summary, the job
//! sources with their auto-watch switches, the latest matches and the
//! application pipeline counts. The profile itself is set in the chat with
//! JobFinder (its first conversation is a short interview).

use dioxus::prelude::*;
use serde_json::Value;

use crate::ui_widgets::StatusChip;
use serabot_desktop::jobs_view as jobs;
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
        .name("serabot-jobs".into())
        .spawn(move || {
            let _ = tx.send(job(&credentials));
        });
    if spawned.is_err() {
        return Err("Could not start the request.".into());
    }
    rx.await
        .unwrap_or_else(|_| Err("The request was interrupted.".into()))
}

#[component]
pub fn JobSearchCard(session: Signal<Session>) -> Element {
    let mut view = use_signal(|| Option::<Value>::None);
    let mut error = use_signal(|| Option::<String>::None);
    let mut busy = use_signal(|| false);
    use_hook(move || {
        spawn(async move {
            match call(session, jobs::get_overview).await {
                Ok(value) => view.set(Some(value)),
                Err(message) => error.set(Some(message)),
            }
        });
    });
    let mut switch = move |key: String, on: bool| {
        busy.set(true);
        spawn(async move {
            let result = call(session, move |credentials| {
                jobs::set_auto_watch(credentials, &key, on)
            })
            .await;
            match result {
                Ok(_) => match call(session, jobs::get_overview).await {
                    Ok(value) => {
                        error.set(None);
                        view.set(Some(value));
                    }
                    Err(message) => error.set(Some(message)),
                },
                Err(message) => error.set(Some(message)),
            }
            busy.set(false);
        });
    };
    let check_now = move |_| {
        busy.set(true);
        spawn(async move {
            match call(session, jobs::watch_now).await {
                Ok(value) => {
                    error.set(None);
                    view.set(Some(value["overview"].clone()));
                }
                Err(message) => error.set(Some(message)),
            }
            busy.set(false);
        });
    };
    let Some(current) = view() else {
        return rsx! {
            div { class: "card insights-card",
                div { class: "card-head", h3 { class: "card-title", "Job search" } }
                if let Some(message) = error() {
                    p { class: "tiny", role: "alert", "{message}" }
                } else {
                    p { class: "tiny", "Loading\u{2026}" }
                }
            }
        };
    };
    let rows = jobs::profile_rows(&current["profile"]);
    let has_profile = !rows.is_empty();
    let sources: Vec<(String, String, bool, bool)> = current["sources"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|source| {
            (
                source["key"].as_str().unwrap_or_default().to_string(),
                source["label"].as_str().unwrap_or_default().to_string(),
                source["auto_watch"].as_bool().unwrap_or(true),
                source["builtin"].as_bool().unwrap_or(true),
            )
        })
        .collect();
    let matches: Vec<(String, String)> = current["matches"]
        .as_array()
        .into_iter()
        .flatten()
        .take(6)
        .map(|item| {
            (
                jobs::match_line(item),
                item["url"].as_str().unwrap_or_default().to_string(),
            )
        })
        .collect();
    let counts = jobs::pipeline(&current["counts"]);
    let last = jobs::last_run_line(&current["last_run"]);
    rsx! {
        div { class: "card insights-card",
            div { class: "card-head",
                h3 { class: "card-title", "Job search" }
                if has_profile {
                    StatusChip { tone: Tone::Ok, text: "Profile set".to_string() }
                } else {
                    StatusChip { tone: Tone::Muted, text: "No profile yet".to_string() }
                }
            }
            if has_profile {
                div { class: "cat-list",
                    for (label, value) in rows {
                        div { key: "{label}", class: "cat-row",
                            span { class: "cat-label", "{label}" }
                            span { class: "tiny", title: "{value}", "{value}" }
                        }
                    }
                }
            } else {
                p { class: "tiny", "Open a chat with JobFinder. It asks a few short questions the first time and saves your profile." }
            }
            if has_profile {
                div { class: "usage-tiles",
                    for (label, count) in counts {
                        div { key: "{label}", class: "stat-tile",
                            span { class: "stat-value", "{count}" }
                            span { class: "stat-label", "{label}" }
                        }
                    }
                }
            }
            div { class: "cat-list",
                for (key, label, on, builtin) in sources {
                    label { key: "{key}", class: "insights-label",
                        input {
                            r#type: "checkbox",
                            checked: on,
                            disabled: busy(),
                            onchange: {
                                let key = key.clone();
                                move |event| switch(key.clone(), event.checked())
                            },
                        }
                        span { "{label}" }
                        span { class: "tiny", "{jobs::source_status_label(on, builtin)}" }
                    }
                }
            }
            if !matches.is_empty() {
                h4 { class: "tiny", "Latest matches" }
                div { class: "cat-list",
                    for (index, (line, url)) in matches.into_iter().enumerate() {
                        div { key: "{index}", class: "cat-row",
                            span { class: "tiny", title: "{line}", "{line}" }
                            if let Some(link) = jobs::safe_link(&url) {
                                span { class: "cat-size", title: "{link}", "{link}" }
                            }
                        }
                    }
                }
            }
            p { class: "tiny", "{last}" }
            if has_profile {
                div {
                    button {
                        class: "btn ghost sm",
                        r#type: "button",
                        disabled: busy(),
                        onclick: check_now,
                        if busy() { "Checking\u{2026}" } else { "Check now" }
                    }
                }
            }
            if let Some(message) = error() {
                p { class: "tiny", role: "alert", "{message}" }
            }
        }
    }
}
