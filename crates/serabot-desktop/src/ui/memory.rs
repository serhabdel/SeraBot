use super::*;

/// Maximum characters accepted in one memory entry on the client.
const MAX_MEMORY_CHARS: usize = 4_000;

/// Filter chips on the Memory page: `(filter key, label)`.
pub(super) const MEMORY_FILTERS: &[(&str, &str)] = &[
    ("all", "All"),
    ("saved", "Saved"),
    ("learned", "Learned"),
    ("superseded", "Superseded"),
    ("review", "To review"),
];

pub(super) fn validate_memory_content(text: &str) -> Result<(), &'static str> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Err("Write something to remember first.");
    }
    if trimmed.chars().count() > MAX_MEMORY_CHARS {
        return Err("Memories are limited to 4,000 characters.");
    }
    Ok(())
}

pub(super) fn memory_empty_copy(filter: &str, query: &str) -> (&'static str, String, bool) {
    let has_filter = filter != "all" || !query.trim().is_empty();
    if has_filter {
        (
            "No matching memories",
            "Nothing matched that filter or search. Clear filters to see everything.".to_owned(),
            true,
        )
    } else {
        (
            "No memories saved yet",
            "Durable facts you add here reach the orchestrator.".to_owned(),
            false,
        )
    }
}

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
    importance_pct: u32,
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
            importance_pct: (memory.importance.clamp(0.0, 1.0) * 100.0).round() as u32,
            created: stamp(&memory.created_at),
            recalled: match &memory.last_recalled_at {
                Some(at) => format!(
                    "last recalled {} ({}\u{d7})",
                    stamp(at),
                    memory.recall_count
                ),
                None => "not recalled yet".into(),
            },
        })
        .collect()
}

/// The header line: counts by origin and status, and the last extraction.
pub(super) fn memory_header(stats: Option<&MemoryStatsView>) -> String {
    match stats {
        None => "Owner-scoped memory. Loading counts\u{2026}".into(),
        Some(stats) => {
            let count = |map: &std::collections::BTreeMap<String, i64>, key: &str| {
                map.get(key).copied().unwrap_or(0)
            };
            let last = stats
                .last_extraction_at
                .as_deref()
                .map_or("never".to_owned(), stamp);
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
    let mut draft_error = use_signal(|| Option::<&'static str>::None);
    let mut composing = use_signal(|| false);
    let (memories, memories_loaded, header, app_error) = {
        let s = session.read();
        (
            memory_rows(&s.state.memories),
            s.state.memories_loaded,
            memory_header(s.state.memory_stats.as_ref()),
            s.state.error.clone(),
        )
    };
    let kind = session.read().memory.kind.clone();
    let content = session.read().memory.content.clone();
    let source = session.read().memory.source.clone();
    let query = session.read().memory.query.clone();
    let filter = session.read().memory.filter.clone();
    let confirm = session.read().memory.delete_confirm.clone();
    let editing = session.read().memory.editing.clone();
    let edit_content = session.read().memory.edit_content.clone();
    let memories_empty = memories.is_empty();
    let content_chars = content.trim().chars().count();
    let can_add = validate_memory_content(&content).is_ok();
    let (empty_title, empty_text, empty_filtered) = memory_empty_copy(&filter, &query);
    let is_composing = composing();
    rsx! {
        div { class: "pane memory-page",
            div { class: "pane-head",
                div {
                    h1 { class: "pane-title", "Memory" }
                    p { class: "muted", "{header}" }
                }
                div { class: "page-actions",
                    button {
                        class: "btn ghost",
                        r#type: "button",
                        onclick: move |_| session.write().load_memories(),
                        Icon { name: "refresh", size: 14 }
                        "Refresh"
                    }
                    button {
                        class: if is_composing { "btn" } else { "btn primary" },
                        r#type: "button",
                        "aria-expanded": if is_composing { "true" } else { "false" },
                        "aria-controls": "mem-composer",
                        onclick: move |_| {
                            draft_error.set(None);
                            composing.set(!is_composing);
                        },
                        if is_composing {
                            "Close"
                        } else {
                            Icon { name: "plus", size: 15 }
                            "Add memory"
                        }
                    }
                }
            }
            div { class: "pane-body",
                if let Some(error) = app_error {
                    div { class: "banner err", role: "alert", span { "{error}" } }
                }
                crate::ui_memory_review::MemoryReviewCard { session }
                if is_composing {
                    div { class: "card mem-composer", id: "mem-composer",
                        div { class: "field",
                            label { class: "field-label", r#for: "mem-content", "What should be remembered?" }
                            textarea {
                                id: "mem-content",
                                class: if draft_error().is_some() || content_chars > MAX_MEMORY_CHARS { "textarea invalid" } else { "textarea" },
                                rows: "2",
                                autofocus: true,
                                value: "{content}",
                                placeholder: "A durable preference, fact, decision, context or note",
                                oninput: move |event| {
                                    draft_error.set(None);
                                    session.write().memory.content = event.value();
                                },
                                onkeydown: move |event: KeyboardEvent| {
                                    if event.key() == Key::Escape {
                                        event.stop_propagation();
                                        composing.set(false);
                                    } else if event.key() == Key::Enter
                                        && (event.modifiers().contains(Modifiers::CONTROL)
                                            || event.modifiers().contains(Modifiers::META))
                                    {
                                        event.prevent_default();
                                        let current = session.read().memory.content.clone();
                                        match validate_memory_content(&current) {
                                            Ok(()) => {
                                                draft_error.set(None);
                                                session.write().add_memory();
                                                composing.set(false);
                                            }
                                            Err(msg) => draft_error.set(Some(msg)),
                                        }
                                    }
                                },
                            }
                            if let Some(msg) = draft_error() {
                                span { class: "field-error", role: "alert", "{msg}" }
                            } else if content_chars > 3_500 {
                                span { class: "tiny", "{content_chars}/{MAX_MEMORY_CHARS}" }
                            }
                        }
                        div { class: "mem-composer-row",
                            div { class: "field",
                                label { class: "field-label", r#for: "mem-kind", "Kind" }
                                select {
                                    id: "mem-kind",
                                    class: "select",
                                    value: "{kind}",
                                    onchange: move |event| session.write().memory.kind = event.value(),
                                    for kind in Session::memory_kinds() {
                                        option { value: "{kind}", "{kind}" }
                                    }
                                }
                            }
                            div { class: "field",
                                label { class: "field-label", r#for: "mem-source", "Source" }
                                input {
                                    id: "mem-source",
                                    class: "input",
                                    value: "{source}",
                                    placeholder: "source",
                                    oninput: move |event| session.write().memory.source = event.value(),
                                }
                            }
                            div { class: "mem-composer-actions",
                                button {
                                    class: "btn ghost",
                                    r#type: "button",
                                    onclick: move |_| composing.set(false),
                                    "Cancel"
                                }
                                button {
                                    class: "btn primary",
                                    r#type: "button",
                                    disabled: !can_add,
                                    onclick: move |_| {
                                        let current = session.read().memory.content.clone();
                                        match validate_memory_content(&current) {
                                            Ok(()) => {
                                                draft_error.set(None);
                                                session.write().add_memory();
                                                composing.set(false);
                                            }
                                            Err(msg) => draft_error.set(Some(msg)),
                                        }
                                    },
                                    "Save memory"
                                }
                            }
                        }
                    }
                }
                div { class: "mem-toolbar",
                    div { class: "search-wrap mem-search",
                        input {
                            class: "input",
                            r#type: "search",
                            value: "{query}",
                            placeholder: "Search all of your memory, including old facts",
                            "aria-label": "Search memories",
                            oninput: move |event| session.write().memory.query = event.value(),
                            onkeydown: move |event: KeyboardEvent| {
                                if event.key() == Key::Enter {
                                    event.prevent_default();
                                    session.write().load_memories();
                                } else if event.key() == Key::Escape && !session.read().memory.query.is_empty() {
                                    event.stop_propagation();
                                    session.write().memory.query.clear();
                                    session.write().load_memories();
                                }
                            },
                        }
                        if !query.trim().is_empty() {
                            button {
                                class: "btn ghost sm",
                                r#type: "button",
                                onclick: move |_| {
                                    session.write().memory.query.clear();
                                    session.write().load_memories();
                                },
                                "Clear"
                            }
                        }
                        button {
                            class: "btn sm",
                            r#type: "button",
                            onclick: move |_| session.write().load_memories(),
                            "Search"
                        }
                    }
                    div { class: "seg", role: "group", "aria-label": "Filter memories",
                        for (key, label) in MEMORY_FILTERS.iter().copied() {
                            button {
                                key: "{key}",
                                r#type: "button",
                                class: if filter == key { "seg-btn active" } else { "seg-btn" },
                                "aria-pressed": if filter == key { "true" } else { "false" },
                                onclick: move |_| session.write().set_memory_filter(key),
                                "{label}"
                            }
                        }
                    }
                }
                div { class: "card-grid mem-grid",
                    if !memories_loaded {
                        for n in 0..3 {
                            div { key: "{n}", class: "card mem-card", "aria-busy": "true",
                                div { class: "skeleton title" }
                                div { class: "skeleton block" }
                            }
                        }
                    }
                    for memory in memories {
                        div {
                            class: if memory.superseded { "card mem-card superseded" } else if memory.pending { "card mem-card pending" } else { "card mem-card" },
                            key: "{memory.id}",
                            div { class: "mem-meta",
                                span { class: if memory.origin == "Learned" { "chip" } else { "chip accent" }, "{memory.origin}" }
                                span { class: "chip", "{memory.kind}" }
                                if let Some(bot) = memory.bot.clone() {
                                    span { class: "chip", "{bot}" }
                                }
                                if memory.superseded {
                                    span { class: "chip", "Superseded" }
                                }
                                if memory.pending {
                                    span { class: "chip warn", "To review" }
                                }
                            }
                            if editing.as_deref() == Some(memory.id.as_str()) {
                                {
                                    let edit_invalid = validate_memory_content(&edit_content).is_err();
                                    rsx! {
                                        textarea {
                                            class: if edit_invalid && !edit_content.trim().is_empty() { "textarea invalid" } else { "textarea" },
                                            autofocus: true,
                                            value: "{edit_content}",
                                            oninput: move |event| session.write().memory.edit_content = event.value(),
                                            onkeydown: move |event: KeyboardEvent| {
                                                if event.key() == Key::Escape {
                                                    event.stop_propagation();
                                                    session.write().cancel_memory_edit();
                                                } else if event.key() == Key::Enter
                                                    && (event.modifiers().contains(Modifiers::CONTROL)
                                                        || event.modifiers().contains(Modifiers::META))
                                                {
                                                    event.prevent_default();
                                                    let cur = session.read().memory.edit_content.clone();
                                                    if validate_memory_content(&cur).is_ok() {
                                                        session.write().save_memory_edit();
                                                    }
                                                }
                                            },
                                        }
                                        div { class: "row-actions",
                                            button {
                                                class: "btn primary sm",
                                                r#type: "button",
                                                disabled: edit_invalid,
                                                onclick: move |_| session.write().save_memory_edit(),
                                                "Save"
                                            }
                                            button {
                                                class: "btn ghost sm",
                                                r#type: "button",
                                                onclick: move |_| session.write().cancel_memory_edit(),
                                                "Cancel"
                                            }
                                        }
                                    }
                                }
                            } else {
                                p { class: "mem-text", "{memory.content}" }
                                div { class: "mem-foot",
                                    div { class: "mem-importance", title: "{memory.importance}",
                                        div { class: "meter",
                                            div { class: "meter-fill", style: "width: {memory.importance_pct}%;" }
                                        }
                                        span { class: "tiny", "{memory.importance_pct}%" }
                                    }
                                    p { class: "tiny", "{memory.created} \u{b7} {memory.recalled}" }
                                }
                                div { class: "row-actions mem-actions",
                                    if memory.pending {
                                        button {
                                            class: "btn primary sm",
                                            r#type: "button",
                                            onclick: {
                                                let id = memory.id.clone();
                                                move |_| session.write().approve_memory(id.clone())
                                            },
                                            "Approve"
                                        }
                                    }
                                    button {
                                        class: "btn ghost sm",
                                        r#type: "button",
                                        onclick: {
                                            let id = memory.id.clone();
                                            let text = memory.content.clone();
                                            move |_| session.write().begin_memory_edit(id.clone(), &text)
                                        },
                                        "Edit"
                                    }
                                    if confirm.as_deref() == Some(memory.id.as_str()) {
                                        div { class: "vault-confirm", role: "alertdialog", "aria-label": "Confirm delete memory",
                                            button {
                                                class: "btn danger sm",
                                                r#type: "button",
                                                onclick: {
                                                    let id = memory.id.clone();
                                                    move |_| session.write().delete_memory(id.clone())
                                                },
                                                "Confirm delete"
                                            }
                                            button {
                                                class: "btn ghost sm",
                                                r#type: "button",
                                                onclick: move |_| session.write().memory.delete_confirm = None,
                                                "Cancel"
                                            }
                                        }
                                    } else {
                                        button {
                                            class: "btn ghost sm",
                                            r#type: "button",
                                            onclick: {
                                                let id = memory.id.clone();
                                                move |_| session.write().memory.delete_confirm = Some(id.clone())
                                            },
                                            "{memory.delete_label}"
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
                if memories_loaded && memories_empty {
                    div { class: "empty",
                        div { class: "empty-icon", Icon { name: if empty_filtered { "search" } else { "bolt" }, size: 20 } }
                        h3 { class: "empty-title", "{empty_title}" }
                        p { class: "empty-text", "{empty_text}" }
                        if empty_filtered {
                            button {
                                class: "btn sm",
                                r#type: "button",
                                onclick: move |_| {
                                    session.write().memory.query.clear();
                                    session.write().set_memory_filter("all");
                                },
                                "Clear filters"
                            }
                        } else {
                            button {
                                class: "btn primary sm",
                                r#type: "button",
                                onclick: move |_| composing.set(true),
                                "Add memory"
                            }
                        }
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn memory_content_validation_checks_empty_and_length() {
        assert!(validate_memory_content("   \n ").is_err());
        assert!(validate_memory_content("Prefer concise replies").is_ok());
        let too_long = "a".repeat(MAX_MEMORY_CHARS + 1);
        assert!(validate_memory_content(&too_long).is_err());
    }

    #[test]
    fn memory_empty_copy_distinguishes_filtered_from_empty_vault() {
        let (title, _, filtered) = memory_empty_copy("all", "");
        assert_eq!(title, "No memories saved yet");
        assert!(!filtered);

        let (title, _, filtered) = memory_empty_copy("learned", "");
        assert_eq!(title, "No matching memories");
        assert!(filtered);

        let (title, _, filtered) = memory_empty_copy("all", "budget");
        assert_eq!(title, "No matching memories");
        assert!(filtered);
    }
}
