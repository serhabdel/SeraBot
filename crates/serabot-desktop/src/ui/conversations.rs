use super::*;
use std::collections::HashMap;

/// Derive a valid slug draft (`[a-z][a-z0-9-]{0,31}`) from a conversation title.
fn slugify_title(title: &str) -> String {
    let mut out = String::new();
    let mut prev_dash = false;
    for ch in title.chars() {
        let lower = ch.to_ascii_lowercase();
        if out.is_empty() {
            if lower.is_ascii_lowercase() {
                out.push(lower);
            }
            continue;
        }
        if lower.is_ascii_lowercase() || lower.is_ascii_digit() {
            out.push(lower);
            prev_dash = false;
        } else if !prev_dash {
            out.push('-');
            prev_dash = true;
        }
        if out.len() >= 32 {
            break;
        }
    }
    while out.ends_with('-') {
        out.pop();
    }
    out
}

struct ConvCardRow {
    id: String,
    title: String,
    subtitle: String,
    /// The latest message ("You: ..." for the owner's), else the subtitle.
    preview: String,
    avatar: String,
    updated: String,
    updated_raw: String,
    kind_key: String,
    kind_label: &'static str,
    parent_label: Option<String>,
    unread: i64,
    active: bool,
    /// Subagent runs going and failed runs waiting for the owner. Known for
    /// the open chat only: the runs list is read per conversation.
    runs: (usize, usize),
}

#[component]
pub(super) fn ConversationsView(session: Signal<Session>) -> Element {
    let mut rename = use_signal(|| None::<(String, String)>);
    let mut confirm_archive = use_signal(|| None::<String>);
    let mut search = use_signal(String::new);
    let mut kind_filter = use_signal(|| Option::<&'static str>::None);
    let mut auto_slug = use_signal(|| true);
    let mut creating = use_signal(|| false);

    let kind = session.read().fleet.conv_kind;
    let current = session.read().conversation.clone();
    let (items, parents) = {
        let state = session.read();
        let labels_by_id: HashMap<&str, String> = state
            .state
            .conversations
            .iter()
            .map(|c| (c.id.as_str(), state.conversation_label(c)))
            .collect();
        let items = state
            .state
            .conversations
            .iter()
            .map(|conversation| {
                let norm_kind = conversation.normalized_kind().to_owned();
                let kind_label = match norm_kind.as_str() {
                    "section" => "Section",
                    "group" => "Group",
                    "room" => "Room",
                    _ => "Direct",
                };
                let parent_label = conversation
                    .parent_id
                    .as_deref()
                    .and_then(|pid| labels_by_id.get(pid).cloned());
                ConvCardRow {
                    id: conversation.id.clone(),
                    title: state.conversation_label(conversation),
                    subtitle: state.conversation_subtitle(conversation),
                    preview: state.conversation_list_subtitle(conversation),
                    avatar: match conversation.id.strip_prefix("dir:") {
                        Some(bot) => state.bot_avatar(bot),
                        None => initial_of(&state.conversation_label(conversation)),
                    },
                    updated: msg_stamp(&conversation.updated_at),
                    updated_raw: conversation.updated_at.clone(),
                    kind_key: norm_kind,
                    kind_label,
                    parent_label,
                    unread: conversation.unread_count,
                    active: conversation.id == current,
                    runs: if conversation.id == current {
                        (
                            serabot_desktop::runs::active_count(&state.state.runs.rows),
                            serabot_desktop::runs::needs_attention(&state.state.runs.rows),
                        )
                    } else {
                        (0, 0)
                    },
                }
            })
            .collect::<Vec<_>>();
        let parent_kinds: &[&str] = match kind {
            ConversationKind::Section => &[],
            ConversationKind::Group => &["section"],
            ConversationKind::Direct | ConversationKind::Room => &["section", "group"],
        };
        let parents = state
            .state
            .conversations
            .iter()
            .filter(|conversation| {
                parent_kinds
                    .iter()
                    .any(|kind| *kind == conversation.normalized_kind())
            })
            .map(|conversation| {
                (
                    conversation.id.clone(),
                    state.conversation_label(conversation),
                )
            })
            .collect::<Vec<_>>();
        (items, parents)
    };
    let title = session.read().fleet.conv_title.clone();
    let slug = session.read().fleet.conv_slug.clone();
    let parent = session.read().fleet.conv_parent.clone();
    let editing = rename();
    let archiving = confirm_archive();
    let loaded = session.read().state.conversations_loaded;
    let total_count = items.len();
    let needle = search().trim().to_lowercase();
    let active_kind = kind_filter();
    let kind_count = |key: Option<&str>| {
        items
            .iter()
            .filter(|row| key.is_none_or(|k| row.kind_key == k))
            .count()
    };
    let filter_chips: Vec<(Option<&'static str>, &'static str, usize)> = [
        (None, "All"),
        (Some("direct"), "Direct"),
        (Some("room"), "Room"),
        (Some("group"), "Group"),
        (Some("section"), "Section"),
    ]
    .into_iter()
    .map(|(key, label)| (key, label, kind_count(key)))
    .filter(|(key, _, count)| key.is_none() || *count > 0 || active_kind == *key)
    .collect();
    let filtered_items: Vec<ConvCardRow> = items
        .into_iter()
        .filter(|row| {
            let kind_ok = active_kind.is_none_or(|k| row.kind_key == k);
            let text_ok = needle.is_empty()
                || row.title.to_lowercase().contains(&needle)
                || row.subtitle.to_lowercase().contains(&needle)
                || row.preview.to_lowercase().contains(&needle)
                || row.id.to_lowercase().contains(&needle)
                || row
                    .parent_label
                    .as_deref()
                    .is_some_and(|p| p.to_lowercase().contains(&needle));
            kind_ok && text_ok
        })
        .collect();
    let list_empty = filtered_items.is_empty();
    // Inline validation with the same rule the session enforces on create.
    // Shown once the owner has typed something.
    let slug_invalid = !slug.is_empty() && !serabot_desktop::valid_slug(&slug);
    let can_create = serabot_desktop::valid_slug(&slug);
    rsx! {
        div { class: "pane",
            div { class: "pane-head",
                div {
                    h1 { class: "pane-title", "All chats" }
                    p { class: "muted", "Every conversation: direct chats, group chats, folders and sections." }
                }
                div { class: "pane-head-actions",
                    button {
                        class: "btn ghost",
                        r#type: "button",
                        onclick: move |_| session.write().load_conversations(),
                        Icon { name: "refresh", size: 14 }
                        " Refresh"
                    }
                    button {
                        class: "btn primary",
                        r#type: "button",
                        "aria-expanded": if creating() { "true" } else { "false" },
                        onclick: move |_| creating.set(!creating()),
                        Icon { name: if creating() { "x" } else { "plus" }, size: 14 }
                        if creating() { " Close" } else { " New conversation" }
                    }
                }
            }
            div { class: "pane-body conv-body",
                if creating() {
                div { class: "card conv-new",
                    div { class: "card-head",
                        h3 { class: "card-title", "New conversation" }
                    }
                    div { class: "form-grid",
                        div { class: "field",
                            label { class: "field-label", "Kind" }
                            select {
                                class: "select",
                                value: "{kind.as_str()}",
                                onchange: move |event| {
                                    let next = ConversationKind::ALL
                                        .into_iter()
                                        .find(|kind| kind.as_str() == event.value())
                                        .unwrap_or(ConversationKind::Section);
                                    session.write().fleet.conv_kind = next;
                                    session.write().fleet.conv_parent.clear();
                                },
                                for kind in ConversationKind::ALL {
                                    option { value: "{kind.as_str()}", "{kind.label()}" }
                                }
                            }
                        }
                        div { class: "field",
                            label { class: "field-label", "Name" }
                            input {
                                class: "input",
                                value: "{title}",
                                placeholder: "name (for example Finance)",
                                oninput: move |event| {
                                    let val = event.value();
                                    let mut guard = session.write();
                                    guard.fleet.conv_title = val.clone();
                                    if auto_slug() || guard.fleet.conv_slug.is_empty() {
                                        guard.fleet.conv_slug = slugify_title(&val);
                                        auto_slug.set(true);
                                    }
                                },
                                onkeydown: move |event: KeyboardEvent| {
                                    if event.key() == Key::Enter && !event.is_composing() && can_create {
                                        let kind = session.read().fleet.conv_kind;
                                        let slug = session.read().fleet.conv_slug.clone();
                                        let title = session.read().fleet.conv_title.clone();
                                        let parent = session.read().fleet.conv_parent.trim().to_owned();
                                        auto_slug.set(true);
                                        creating.set(false);
                                        session.write().create_conversation(
                                            kind,
                                            slug,
                                            (!parent.is_empty()).then_some(parent),
                                            title,
                                        );
                                    }
                                },
                            }
                        }
                        div { class: "field",
                            label { class: "field-label", "Id" }
                            input {
                                class: if slug_invalid { "input invalid" } else { "input" },
                                value: "{slug}",
                                placeholder: "id (lowercase slug)",
                                "aria-invalid": if slug_invalid { "true" } else { "false" },
                                oninput: move |event| {
                                    let val = event.value();
                                    auto_slug.set(val.is_empty());
                                    session.write().fleet.conv_slug = val;
                                },
                                onkeydown: move |event: KeyboardEvent| {
                                    if event.key() == Key::Enter && !event.is_composing() && can_create {
                                        let kind = session.read().fleet.conv_kind;
                                        let slug = session.read().fleet.conv_slug.clone();
                                        let title = session.read().fleet.conv_title.clone();
                                        let parent = session.read().fleet.conv_parent.trim().to_owned();
                                        auto_slug.set(true);
                                        creating.set(false);
                                        session.write().create_conversation(
                                            kind,
                                            slug,
                                            (!parent.is_empty()).then_some(parent),
                                            title,
                                        );
                                    }
                                },
                            }
                            if slug_invalid {
                                p { class: "field-error", role: "alert",
                                    "Use lowercase letters, numbers and dashes only, starting with a letter (32 at most)."
                                }
                            }
                        }
                        if kind != ConversationKind::Section {
                            div { class: "field",
                                label { class: "field-label", "Parent" }
                                select {
                                    class: "select",
                                    value: "{parent}",
                                    onchange: move |event| session.write().fleet.conv_parent = event.value(),
                                    option { value: "", "No parent" }
                                    for (id, label) in parents {
                                        option { value: "{id}", "{label}" }
                                    }
                                }
                            }
                        }
                    }
                    div { class: "card-actions",
                        button {
                            class: "btn primary",
                            r#type: "button",
                            disabled: !can_create,
                            onclick: move |_| {
                                let kind = session.read().fleet.conv_kind;
                                let slug = session.read().fleet.conv_slug.clone();
                                let title = session.read().fleet.conv_title.clone();
                                let parent = session.read().fleet.conv_parent.trim().to_owned();
                                auto_slug.set(true);
                                creating.set(false);
                                session.write().create_conversation(
                                    kind,
                                    slug,
                                    (!parent.is_empty()).then_some(parent),
                                    title,
                                );
                            },
                            "Create"
                        }
                    }
                }
                }
                if total_count > 0 {
                    div { class: "conv-toolbar",
                        div { class: "conv-search",
                            span { class: "search-glyph", Icon { name: "search", size: 15 } }
                            input {
                                class: "input",
                                r#type: "search",
                                "aria-label": "Search conversations",
                                placeholder: "Search by name, message, id or parent\u{2026}",
                                value: "{search}",
                                oninput: move |event| search.set(event.value()),
                                onkeydown: move |event: KeyboardEvent| {
                                    if event.key() == Key::Escape && !search.read().is_empty() {
                                        event.prevent_default();
                                        search.set(String::new());
                                    }
                                },
                            }
                        }
                        div { class: "conv-filter", role: "group", "aria-label": "Filter by kind",
                            for (key, label, count) in filter_chips {
                                button {
                                    key: "{label}",
                                    class: if active_kind == key { "conv-chip active" } else { "conv-chip" },
                                    r#type: "button",
                                    "aria-pressed": if active_kind == key { "true" } else { "false" },
                                    onclick: move |_| kind_filter.set(key),
                                    "{label}"
                                    span { class: "conv-chip-count", "{count}" }
                                }
                            }
                        }
                    }
                }
                if !loaded {
                    div { class: "card",
                        div { class: "skeleton title" }
                        div { class: "skeleton block" }
                    }
                }
                if loaded && list_empty {
                    div { class: "empty",
                        div { class: "empty-icon", Icon { name: "threads", size: 20 } }
                        h2 { class: "empty-title",
                            if total_count == 0 { "No conversations yet" } else { "No matching conversations" }
                        }
                        p { class: "empty-text",
                            if total_count == 0 {
                                "Create a section, a group or a direct thread above, or start a chat from the Chat tab."
                            } else {
                                "Try clearing your search or kind filter."
                            }
                        }
                        if total_count == 0 {
                            button {
                                class: "btn primary",
                                r#type: "button",
                                onclick: move |_| session.write().open_section(Section::Chat),
                                "Go to Chat"
                            }
                        } else {
                            button {
                                class: "btn ghost",
                                r#type: "button",
                                onclick: move |_| {
                                    search.set(String::new());
                                    kind_filter.set(None);
                                },
                                "Reset filters"
                            }
                        }
                    }
                }
                if !list_empty {
                    div { class: "conv-list", role: "list", "aria-label": "Conversations",
                        for row in filtered_items {
                            {
                                let id = row.id;
                                let title = row.title;
                                let preview = row.preview;
                                let updated = row.updated;
                                let updated_raw = row.updated_raw;
                                let is_confirming = archiving.as_deref() == Some(id.as_str());
                                let is_editing = editing.as_ref().is_some_and(|(edit_id, _)| edit_id == &id);
                                let busy = is_confirming || is_editing;
                                let tile = conversation_avatar_style(&id);
                                let tile_class = if id.starts_with("room:") { "avatar bot group" } else { "avatar bot" };
                                rsx! {
                                    div {
                                        class: match (row.active, busy) {
                                            (true, true) => "conv-row active busy",
                                            (true, false) => "conv-row active",
                                            (false, true) => "conv-row busy",
                                            (false, false) => "conv-row",
                                        },
                                        key: "{id}",
                                        role: "listitem",
                                        button {
                                            class: "conv-main",
                                            r#type: "button",
                                            title: "Open {title}",
                                            onclick: {
                                                let id = id.clone();
                                                move |_| session.write().open_conversation(id.clone())
                                            },
                                            span { class: "{tile_class}", style: "{tile}", "{row.avatar}" }
                                            span { class: "conv-copy",
                                                span { class: "conv-title-line",
                                                    span { class: "conv-title", "{title}" }
                                                    span { class: "conv-kind", "{row.kind_label}" }
                                                    if let Some(parent_name) = row.parent_label.as_deref() {
                                                        span { class: "conv-parent", "in {parent_name}" }
                                                    }
                                                    RunCountChips { running: row.runs.0, failed: row.runs.1 }
                                                }
                                                span { class: "conv-preview", "{preview}" }
                                            }
                                            span { class: "conv-side",
                                                span { class: "conv-time", title: "{updated_raw}", "{updated}" }
                                                if let Some(badge) = chat_feed::unread_badge(row.unread) {
                                                    span {
                                                        class: "unread-badge",
                                                        role: "img",
                                                        "aria-label": "{chat_feed::unread_label(row.unread)}",
                                                        "{badge}"
                                                    }
                                                }
                                            }
                                        }
                                        div { class: "conv-actions",
                                            if is_confirming {
                                                div { class: "conv-confirm",
                                                    button {
                                                        class: "btn danger sm",
                                                        r#type: "button",
                                                        onclick: {
                                                            let id = id.clone();
                                                            move |_| {
                                                                confirm_archive.set(None);
                                                                session.write().archive_conversation(id.clone());
                                                            }
                                                        },
                                                        "Confirm archive"
                                                    }
                                                    button {
                                                        class: "btn ghost sm",
                                                        r#type: "button",
                                                        onclick: move |_| confirm_archive.set(None),
                                                        "Cancel"
                                                    }
                                                }
                                            } else {
                                                button {
                                                    class: "btn ghost sm",
                                                    r#type: "button",
                                                    onclick: {
                                                        let id = id.clone();
                                                        move |_| session.write().open_conversation(id.clone())
                                                    },
                                                    "Open"
                                                }
                                                button {
                                                    class: "btn ghost sm",
                                                    r#type: "button",
                                                    onclick: {
                                                        let id = id.clone();
                                                        let title = title.clone();
                                                        move |_| {
                                                            confirm_archive.set(None);
                                                            rename.set(Some((id.clone(), title.clone())));
                                                        }
                                                    },
                                                    "Rename"
                                                }
                                                button {
                                                    class: "btn ghost sm danger-text",
                                                    r#type: "button",
                                                    onclick: {
                                                        let id = id.clone();
                                                        move |_| {
                                                            rename.set(None);
                                                            confirm_archive.set(Some(id.clone()));
                                                        }
                                                    },
                                                    "Archive"
                                                }
                                            }
                                        }
                                        if let Some((edit_id, text)) = editing.clone() {
                                            if edit_id == id {
                                                div { class: "field-row conv-rename",
                                                    input {
                                                        class: "input",
                                                        "aria-label": "Conversation name",
                                                        value: "{text}",
                                                        oninput: move |event| {
                                                            if let Some((_, text)) = rename.write().as_mut() {
                                                                *text = event.value();
                                                            }
                                                        },
                                                        onkeydown: {
                                                            let id = id.clone();
                                                            move |event: KeyboardEvent| {
                                                                if event.is_composing() {
                                                                    return;
                                                                }
                                                                if event.key() == Key::Enter {
                                                                    event.prevent_default();
                                                                    if let Some((_, new_title)) = rename() {
                                                                        session.write().rename_conversation(id.clone(), new_title);
                                                                        rename.set(None);
                                                                    }
                                                                } else if event.key() == Key::Escape {
                                                                    event.prevent_default();
                                                                    rename.set(None);
                                                                }
                                                            }
                                                        },
                                                    }
                                                    button {
                                                        class: "btn primary sm",
                                                        r#type: "button",
                                                        onclick: {
                                                            let id = id.clone();
                                                            move |_| {
                                                                if let Some((_, title)) = rename() {
                                                                    session.write().rename_conversation(id.clone(), title);
                                                                    rename.set(None);
                                                                }
                                                            }
                                                        },
                                                        "Save"
                                                    }
                                                    button {
                                                        class: "btn ghost sm",
                                                        r#type: "button",
                                                        onclick: move |_| rename.set(None),
                                                        "Cancel"
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
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slugify_title_creates_valid_slugs() {
        assert_eq!(slugify_title("Finance & Legal"), "finance-legal");
        assert_eq!(slugify_title("  123 Launch Plan! "), "launch-plan");
        assert_eq!(slugify_title("Q4-Ops_2026"), "q4-ops-2026");
        assert!(serabot_desktop::valid_slug(&slugify_title(
            "My Awesome Room"
        )));
        assert_eq!(slugify_title("12345"), "");
    }
}
