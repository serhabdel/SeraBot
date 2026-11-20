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
pub fn MemoryReviewCard(session: Signal<Session>) -> Element {
    let mut rows = use_signal(Vec::<ReviewRow>::new);
    let mut loaded = use_signal(|| false);
    let mut error = use_signal(|| Option::<String>::None);
    let mut busy = use_signal(|| false);
    let mut editing = use_signal(|| Option::<(String, String)>::None);

    let reload = move || {
        spawn(async move {
            match call(session, review::get_review).await {
                Ok(value) => {
                    error.set(None);
                    rows.set(review::rows(&value));
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

    if loaded() && rows().is_empty() && error().is_none() {
        // Nothing to check: stay out of the way.
        return rsx! {};
    }
    let summary = review::summary(rows().len());
    rsx! {
        div { class: "card insights-card", id: "memory-review",
            div { class: "card-head",
                h3 { class: "card-title", "Memory review" }
            }
            p { class: "tiny", "{summary} SeraBot also sends this list once a week." }
            if let Some(message) = error() {
                p { class: "tiny", role: "alert", "{message}" }
            }
            div { class: "cat-list",
                for row in rows() {
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
                        rsx! {
                            div { key: "{id}", class: "cat-row", style: "flex-direction: column; align-items: stretch; gap: 6px;",
                                div { class: "mem-meta",
                                    span { class: "chip", "{row.reason_label}" }
                                    span { class: "tiny", "{row.detail}" }
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
                                        p { class: "tiny", "Similar: {text}" }
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
                                            class: "btn ghost sm",
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
