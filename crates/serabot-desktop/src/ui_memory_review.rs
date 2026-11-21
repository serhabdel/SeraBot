//! "Memory review" card (0.70) at the top of the Memory page: up to ten
//! memories that may be contradicted, duplicated or long unchecked, each with
//! Keep, Edit, Merge and Delete.

use dioxus::prelude::*;
use serde_json::Value;

use serabot_desktop::memory_review_view::{self as review, ReviewRow};
use serabot_desktop::session::Session;
use serabot_desktop::worker::Credentials;

async fn call<T: Send + 'static>(
    session: Signal<Session>,
    job: impl FnOnce(&Credentials) -> Result<T, String> + Send + 'static,
) -> Result<T, String> {
    let Some(credentials) = session.read().credentials.clone() else {
        return Err("Not connected to the server.".into());
    };
    let (tx, rx) = tokio::sync::oneshot::channel();
    let spawned = std::thread::Builder::new()
        .name("serabot-memory-review".into())
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
pub fn MemoryReviewCard(
    session: Signal<Session>,
    #[props(default)] mut review_summary: Option<Signal<Option<(usize, usize)>>>,
) -> Element {
    let mut rows = use_signal(Vec::<ReviewRow>::new);
    let mut loaded = use_signal(|| false);
    let mut error = use_signal(|| Option::<String>::None);
    let mut busy = use_signal(|| false);
    let mut expanded = use_signal(|| true);
    let mut editing = use_signal(|| Option::<(String, String)>::None);

    let reload = move || {
        spawn(async move {
            match call(session, review::get_review).await {
                Ok(value) => {
                    error.set(None);
                    let parsed = review::rows(&value);
                    let counts = review::review_counts(&parsed);
                    rows.set(parsed);
                    if let Some(mut sig) = review_summary {
                        sig.set(Some(counts));
                    }
                    loaded.set(true);
                }
                Err(message) => {
                    error.set(Some(message));
                    loaded.set(true);
                }
            }
        });
    };
    use_hook(reload);

    let mut act = move |id: String, action: String| {
        busy.set(true);
        spawn(async move {
            let result: Result<Value, String> = call(session, move |credentials| {
                review::act(credentials, &id, &action)
            })
            .await;
            busy.set(false);
            match result {
                Ok(_) => {
                    session.write().load_memories();
                    reload();
                }
                Err(message) => error.set(Some(message)),
            }
        });
    };
    let mut save_edit = move |id: String, text: String| {
        busy.set(true);
        spawn(async move {
            let result = call(session, move |credentials| {
                review::edit(credentials, &id, &text)
            })
            .await;
            busy.set(false);
            match result {
                Ok(()) => {
                    editing.set(None);
                    session.write().load_memories();
                    reload();
                }
                Err(message) => error.set(Some(message)),
            }
        });
    };

    let current_rows = rows();
    let (total_count, conflict_count) = review::review_counts(&current_rows);
    let summary = review::summary(total_count);
    let is_empty_ok = loaded() && current_rows.is_empty() && error().is_none();
    let show_body = expanded() && !is_empty_ok;

    rsx! {
        section {
            class: if is_empty_ok { "card mem-review-card is-clear" } else { "card mem-review-card" },
            id: "memory-review",
            "aria-label": "Weekly memory review",
            div { class: "card-head mem-review-head",
                div { class: "mem-review-title-wrap",
                    h2 { class: "card-title mem-review-title", "Weekly memory review" }
                    p { class: "mem-review-sub",
                        if !loaded() {
                            "Checking recent facts for contradictions, duplicates, or stale items\u{2026}"
                        } else if is_empty_ok {
                            "{summary} Your memory graph looks consistent — SeraBot refreshes this weekly."
                        } else {
                            "{summary} Contradictions, duplicates, and older facts surfaced for a quick check."
                        }
                    }
                }
                div { class: "mem-review-badges",
                    if conflict_count > 0 {
                        span { class: "chip warn", "{conflict_count} conflict" }
                    }
                    if !loaded() {
                        span { class: "chip", "Checking\u{2026}" }
                    } else if is_empty_ok {
                        span { class: "chip ok", "All caught up" }
                    } else {
                        span { class: "chip accent", "{total_count} to review" }
                    }
                    button {
                        class: "btn ghost sm",
                        r#type: "button",
                        disabled: busy(),
                        title: "Refresh review list",
                        onclick: move |_| reload(),
                        "Refresh"
                    }
                    if !is_empty_ok && loaded() {
                        button {
                            class: "btn ghost sm",
                            r#type: "button",
                            "aria-expanded": if show_body { "true" } else { "false" },
                            onclick: move |_| expanded.set(!expanded()),
                            if show_body { "Collapse" } else { "Show items" }
                        }
                    }
                }
            }
            if let Some(message) = error() {
                p { class: "tiny mem-review-error", role: "alert", "{message}" }
            }
            if !loaded() {
                div { class: "mem-review-skeletons", "aria-busy": "true",
                    div { class: "skeleton row" }
                    div { class: "skeleton row" }
                }
            } else if show_body {
                div { class: "mem-review-list",
                    for row in current_rows {
                        {
                            let id = row.id.clone();
                            let keep_id = row.id.clone();
                            let delete_id = row.id.clone();
                            let edit_id = row.id.clone();
                            let edit_text = row.content.clone();
                            let is_editing = editing().as_ref().is_some_and(|(open, _)| *open == row.id);
                            let current_edit = editing().map(|(_, text)| text).unwrap_or_default();
                            let save_id = row.id.clone();
                            let merge = row.merge_target.clone();
                            let reason_chip_class = match row.reason.as_str() {
                                "contradicted" => "chip warn",
                                "duplicate" => "chip accent",
                                _ => "chip",
                            };
                            rsx! {
                                div {
                                    key: "{id}",
                                    class: if row.reason == "contradicted" { "mem-review-item is-conflict" } else { "mem-review-item" },
                                    div { class: "mem-meta",
                                        span { class: "{reason_chip_class}", "{row.reason_label}" }
                                        if !row.detail.is_empty() {
                                            span { class: "tiny", "{row.detail}" }
                                        }
                                    }
                                    if is_editing {
                                        textarea {
                                            class: "textarea",
                                            value: "{current_edit}",
                                            oninput: move |event| {
                                                let open = editing().map(|(open, _)| open);
                                                if let Some(open) = open {
                                                    editing.set(Some((open, event.value())));
                                                }
                                            },
                                        }
                                        div { class: "row-actions",
                                            button {
                                                class: "btn primary sm",
                                                r#type: "button",
                                                disabled: busy() || current_edit.trim().is_empty(),
                                                onclick: move |_| save_edit(save_id.clone(), current_edit.clone()),
                                                "Save"
                                            }
                                            button {
                                                class: "btn ghost sm",
                                                r#type: "button",
                                                onclick: move |_| editing.set(None),
                                                "Cancel"
                                            }
                                        }
                                    } else {
                                        p { class: "mem-text", "{row.content}" }
                                        if let Some((_, text)) = merge.clone() {
                                            div { class: "mem-review-compare",
                                                span { class: "mem-review-compare-label", "Similar fact" }
                                                p { class: "mem-review-compare-text", "{text}" }
                                            }
                                        }
                                        div { class: "row-actions",
                                            button {
                                                class: "btn sm",
                                                r#type: "button",
                                                disabled: busy(),
                                                onclick: move |_| act(keep_id.clone(), "keep".into()),
                                                "Keep"
                                            }
                                            button {
                                                class: "btn ghost sm",
                                                r#type: "button",
                                                disabled: busy(),
                                                onclick: move |_| editing.set(Some((edit_id.clone(), edit_text.clone()))),
                                                "Edit"
                                            }
                                            if let Some((target, _)) = merge {
                                                {
                                                    let merge_id = row.id.clone();
                                                    rsx! {
                                                        button {
                                                            class: "btn ghost sm",
                                                            r#type: "button",
                                                            title: "Delete this one and keep the similar memory",
                                                            disabled: busy(),
                                                            onclick: move |_| act(merge_id.clone(), review::merge_action(&target)),
                                                            "Merge"
                                                        }
                                                    }
                                                }
                                            }
                                            button {
                                                class: "btn ghost sm danger",
                                                r#type: "button",
                                                disabled: busy(),
                                                onclick: move |_| act(delete_id.clone(), "delete".into()),
                                                "Delete"
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
