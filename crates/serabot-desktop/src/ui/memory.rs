use super::*;

/// Filter chips on the Memory page: `(filter key, label)`.
pub(super) const MEMORY_FILTERS: &[(&str, &str)] = &[
    ("all", "All"),
    ("saved", "Saved"),
    ("learned", "Learned"),
    ("superseded", "Superseded"),
    ("review", "To review"),
];

/// One memory card, prepared before rendering so the markup stays plain.
#[derive(Clone, PartialEq)]
pub(super) struct MemoryRow {
    id: String,
    content: String,
    kind: String,
    origin: &'static str,
    superseded: bool,
    pending: bool,
    delete_label: &'static str,
    bot: Option<String>,
    importance: String,
    created: String,
    recalled: String,
}

pub(super) fn memory_rows(memories: &[MemoryEntry]) -> Vec<MemoryRow> {
    memories
        .iter()
        .map(|memory| MemoryRow {
            id: memory.id.clone(),
            content: memory.content.clone(),
            kind: memory.kind.clone(),
            origin: if memory.origin == "extracted" {
                "Learned"
            } else {
                "Saved"
            },
            superseded: memory.status == "superseded",
            pending: memory.status == "pending",
            delete_label: if memory.status == "pending" {
                "Discard"
            } else {
                "Delete"
            },
            bot: memory.bot.clone(),
            importance: format!(
                "{:.0}% important",
                memory.importance.clamp(0.0, 1.0) * 100.0
            ),
            created: memory.created_at.clone(),
            recalled: match &memory.last_recalled_at {
                Some(at) => format!("last recalled {at} ({}×)", memory.recall_count),
                None => "not recalled yet".into(),
            },
        })
        .collect()
}

/// The header line: counts by origin and status, and the last extraction.
pub(super) fn memory_header(stats: Option<&MemoryStatsView>) -> String {
    match stats {
        None => "Owner-scoped memory. Loading counts…".into(),
        Some(stats) => {
            let count = |map: &std::collections::BTreeMap<String, i64>, key: &str| {
                map.get(key).copied().unwrap_or(0)
            };
            let last = stats
                .last_extraction_at
                .as_deref()
                .map_or("never".to_owned(), |at| at.to_owned());
            format!(
                "{} active · {} saved · {} learned · {} superseded · last learned: {last}",
                stats.active,
                count(&stats.by_origin, "explicit"),
                count(&stats.by_origin, "extracted"),
                count(&stats.by_status, "superseded"),
            )
        }
    }
}

#[component]
pub(super) fn MemoryView(session: Signal<Session>) -> Element {
    let memories = memory_rows(&session.read().state.memories);
    let memories_loaded = session.read().state.memories_loaded;
    let header = memory_header(session.read().state.memory_stats.as_ref());
    let kind = session.read().memory.kind.clone();
    let content = session.read().memory.content.clone();
    let source = session.read().memory.source.clone();
    let query = session.read().memory.query.clone();
    let filter = session.read().memory.filter.clone();
    let confirm = session.read().memory.delete_confirm.clone();
    let editing = session.read().memory.editing.clone();
    let edit_content = session.read().memory.edit_content.clone();
    let memories_empty = memories.is_empty();
    rsx! {
        div { class: "pane",
            div { class: "pane-head",
                div {
                    h1 { class: "pane-title", "Memory" }
                    p { class: "muted", "{header}" }
                }
                button {
                    class: "btn ghost",
                    onclick: move |_| session.write().load_memories(),
                    "Refresh"
                }
            }
            div { class: "pane-body",
                div { class: "card",
                    div { class: "form-grid",
                        div { class: "field",
                            label { class: "field-label", "Kind" }
                            select {
                                class: "select",
                                value: "{kind}",
                                onchange: move |event| session.write().memory.kind = event.value(),
                                for kind in Session::memory_kinds() {
                                    option { value: "{kind}", "{kind}" }
                                }
                            }
                        }
                        div { class: "field",
                            label { class: "field-label", "Source" }
                            input {
                                class: "input",
                                value: "{source}",
                                placeholder: "source",
                                oninput: move |event| session.write().memory.source = event.value(),
                            }
                        }
                    }
                    div { class: "field",
                        label { class: "field-label", "Content" }
                        textarea {
                            class: "textarea",
                            value: "{content}",
                            placeholder: "A durable preference, fact, decision, context or note",
                            oninput: move |event| session.write().memory.content = event.value(),
                        }
                    }
                    button {
                        class: "btn primary",
                        onclick: move |_| session.write().add_memory(),
                        "Add memory"
                    }
                }
                div { class: "field-row",
                    input {
                        class: "input",
                        value: "{query}",
                        placeholder: "Search all of your memory, including old facts",
                        oninput: move |event| session.write().memory.query = event.value(),
                    }
                    button {
                        class: "btn ghost",
                        onclick: move |_| session.write().load_memories(),
                        "Search"
                    }
                }
                div { class: "field-row",
                    for (key, label) in MEMORY_FILTERS.iter().copied() {
                        button {
                            key: "{key}",
                            class: if filter == key { "chip accent" } else { "chip" },
                            onclick: move |_| session.write().set_memory_filter(key),
                            "{label}"
                        }
                    }
                }
                div { class: "card-grid",
                    if !memories_loaded {
                        div { class: "card",
                            div { class: "skeleton title" }
                            div { class: "skeleton block" }
                        }
                    }
                    for memory in memories {
                        div { class: "card", key: "{memory.id}",
                            div { class: "card-head",
                                span { class: if memory.origin == "Learned" { "chip" } else { "chip accent" }, "{memory.origin}" }
                                if let Some(bot) = memory.bot.clone() {
                                    span { class: "chip", "{bot}" }
                                }
                                span { class: "chip", "{memory.kind}" }
                                if memory.superseded {
                                    span { class: "chip", "Superseded" }
                                }
                                if memory.pending {
                                    span { class: "chip accent", "To review" }
                                }
                                span { class: "tiny", "{memory.importance}" }
                            }
                            if editing.as_deref() == Some(memory.id.as_str()) {
                                textarea {
                                    class: "textarea",
                                    value: "{edit_content}",
                                    oninput: move |event| session.write().memory.edit_content = event.value(),
                                }
                                div { class: "field-row",
                                    button {
                                        class: "btn primary sm",
                                        onclick: move |_| session.write().save_memory_edit(),
                                        "Save"
                                    }
                                    button {
                                        class: "btn ghost sm",
                                        onclick: move |_| session.write().cancel_memory_edit(),
                                        "Cancel"
                                    }
                                }
                            } else {
                                p { "{memory.content}" }
                                p { class: "tiny", "{memory.created} · {memory.recalled}" }
                                if memory.pending {
                                    button {
                                        class: "btn primary sm",
                                        onclick: {
                                            let id = memory.id.clone();
                                            move |_| session.write().approve_memory(id.clone())
                                        },
                                        "Approve"
                                    }
                                }
                                button {
                                    class: "btn ghost sm",
                                    onclick: {
                                        let id = memory.id.clone();
                                        let text = memory.content.clone();
                                        move |_| session.write().begin_memory_edit(id.clone(), &text)
                                    },
                                    "Edit"
                                }
                            }
                            if confirm.as_deref() == Some(memory.id.as_str()) {
                                button {
                                    class: "btn danger sm",
                                    onclick: {
                                        let id = memory.id.clone();
                                        move |_| session.write().delete_memory(id.clone())
                                    },
                                    "Confirm delete"
                                }
                            } else {
                                button {
                                    class: "btn ghost sm",
                                    onclick: {
                                        let id = memory.id.clone();
                                        move |_| session.write().memory.delete_confirm = Some(id.clone())
                                    },
                                    "{memory.delete_label}"
                                }
                            }
                        }
                    }
                    if memories_loaded && memories_empty {
                        div { class: "empty",
                            div { class: "empty-icon", Icon { name: "bolt", size: 20 } }
                            h3 { class: "empty-title", "No memories yet" }
                            p { class: "empty-text", "Durable facts you add here reach the orchestrator." }
                        }
                    }
                }
            }
        }
    }
}
