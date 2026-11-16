//! The composer's queue panel and the keyboard shortcuts sheet (v0.50).
//!
//! Messages typed while a reply runs wait here, in order, and go out one at a
//! time when the chat is idle. Each can be edited (the queue holds while it
//! is), moved, sent next (which stops the running reply) or deleted. The rules
//! live in `serabot_desktop::chat_compose`.

use super::*;

#[component]
pub(crate) fn QueuePanel(session: Signal<Session>) -> Element {
    let mut edit_text = use_signal(String::new);
    let items: Vec<chat_compose::QueuedMessage> = session.read().queue.items().to_vec();
    if items.is_empty() {
        return rsx! {};
    }
    let editing = session.read().queue.editing();
    let paused = session.read().queue_paused();
    let last = items.len() - 1;
    let count = items.len();
    rsx! {
        section { class: "queue-panel", "aria-label": "Queued messages",
            div { class: "queue-head",
                span { class: "queue-title", "Up next" }
                span { class: "queue-count", "{count}" }
                if editing.is_some() {
                    span { class: "queue-hold", role: "status", "Paused while you edit" }
                } else if paused {
                    span { class: "queue-hold", role: "status", "Paused: the last turn failed. Use the bolt to send one now." }
                }
            }
            ol { class: "queue-list",
                for (index, item) in items.into_iter().enumerate() {
                    {
                        let id = item.id;
                        let is_editing = editing == Some(id);
                        rsx! {
                            li {
                                key: "{id}",
                                class: if is_editing { "queue-item editing" } else { "queue-item" },
                                "data-steer": if item.steer { "true" } else { "false" },
                                if is_editing {
                                    textarea {
                                        class: "input queue-edit",
                                        rows: 2,
                                        "aria-label": "Edit queued message",
                                        value: "{edit_text()}",
                                        autofocus: true,
                                        oninput: move |event| edit_text.set(event.value()),
                                        onkeydown: move |event: KeyboardEvent| {
                                            if event.is_composing() {
                                                return;
                                            }
                                            if event.key() == Key::Escape {
                                                event.prevent_default();
                                                event.stop_propagation();
                                                session.write().queue_cancel_edit();
                                            } else if event.key() == Key::Enter && !event.modifiers().contains(Modifiers::SHIFT) {
                                                event.prevent_default();
                                                let text = edit_text();
                                                let _ = session.write().queue_commit_edit(id, &text);
                                            }
                                        },
                                    }
                                    div { class: "queue-actions",
                                        button {
                                            class: "btn primary sm",
                                            r#type: "button",
                                            onclick: move |_| {
                                                let text = edit_text();
                                                let result = session.write().queue_commit_edit(id, &text);
                                                if let Err(refusal) = result {
                                                    session.write().toast = Some(refusal.message().into());
                                                }
                                            },
                                            "Save"
                                        }
                                        button {
                                            class: "btn ghost sm",
                                            r#type: "button",
                                            onclick: move |_| session.write().queue_cancel_edit(),
                                            "Cancel"
                                        }
                                    }
                                } else {
                                    span { class: "queue-text", "{item.text}" }
                                    if let Some(bot) = item.target.as_deref() {
                                        span { class: "chip muted queue-target", "to @{bot}" }
                                    }
                                    if item.steer {
                                        span { class: "chip accent queue-target", "next" }
                                    }
                                    div { class: "queue-actions",
                                        button {
                                            class: "icon-btn",
                                            r#type: "button",
                                            title: "Move up",
                                            "aria-label": "Move up",
                                            disabled: index == 0,
                                            onclick: move |_| session.write().queue_move(id, true),
                                            Icon { name: "chevron-up", size: 14 }
                                        }
                                        button {
                                            class: "icon-btn",
                                            r#type: "button",
                                            title: "Move down",
                                            "aria-label": "Move down",
                                            disabled: index == last,
                                            onclick: move |_| session.write().queue_move(id, false),
                                            Icon { name: "chevron-down", size: 14 }
                                        }
                                        button {
                                            class: "icon-btn",
                                            r#type: "button",
                                            title: "Edit",
                                            "aria-label": "Edit queued message",
                                            onclick: {
                                                let text = item.text.clone();
                                                move |_| {
                                                    edit_text.set(text.clone());
                                                    let _ = session.write().queue_begin_edit(id);
                                                }
                                            },
                                            Icon { name: "edit", size: 14 }
                                        }
                                        button {
                                            class: "icon-btn",
                                            r#type: "button",
                                            title: "Stop the reply and send this next",
                                            "aria-label": "Stop the reply and send this next",
                                            onclick: move |_| session.write().queue_steer(id),
                                            Icon { name: "bolt", size: 14 }
                                        }
                                        button {
                                            class: "icon-btn",
                                            r#type: "button",
                                            title: "Delete",
                                            "aria-label": "Delete queued message",
                                            onclick: move |_| session.write().queue_delete(id),
                                            Icon { name: "trash", size: 14 }
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

/// F1, or the "Shortcuts" link under the composer.
#[component]
pub(crate) fn ShortcutsSheet(session: Signal<Session>, overlay: Signal<Overlay>) -> Element {
    let mut overlay = overlay;
    let rows = chat_compose::shortcut_rows(session.read().prefs.enter_sends);
    let mut groups: Vec<&'static str> = Vec::new();
    for (group, _, _) in &rows {
        if !groups.contains(group) {
            groups.push(group);
        }
    }
    rsx! {
        div { class: "scrim open", onclick: move |_| overlay.set(Overlay::None) }
        div {
            class: "sheet open",
            role: "dialog",
            "aria-label": "Keyboard shortcuts",
            onkeydown: move |event: KeyboardEvent| {
                if event.key() == Key::Escape {
                    overlay.set(Overlay::None);
                }
            },
            div { class: "sheet-head",
                h2 { class: "sheet-title", "Keyboard shortcuts" }
                button {
                    class: "icon-btn escape-close",
                    title: "Close",
                    "aria-label": "Close",
                    autofocus: true,
                    onclick: move |_| overlay.set(Overlay::None),
                    Icon { name: "x", size: 16 }
                }
            }
            div { class: "sheet-body shortcuts",
                for group in groups {
                    section { key: "{group}", class: "shortcut-group",
                        h3 { class: "shortcut-heading", "{group}" }
                        dl { class: "shortcut-list",
                            for (index, (_, keys, what)) in rows.iter().filter(|row| row.0 == group).enumerate() {
                                div { key: "{index}", class: "shortcut-row",
                                    dt { kbd { class: "kbd", "{keys}" } }
                                    dd { "{what}" }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}
