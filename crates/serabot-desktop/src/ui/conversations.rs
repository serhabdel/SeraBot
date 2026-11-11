use super::*;

#[component]
pub(super) fn ConversationsView(session: Signal<Session>) -> Element {
    let mut rename = use_signal(|| None::<(String, String)>);
    let kind = session.read().fleet.conv_kind;
    let (items, parents) = {
        let state = session.read();
        let items = state
            .state
            .conversations
            .iter()
            .map(|conversation| {
                (
                    conversation.id.clone(),
                    state.conversation_label(conversation),
                    state.conversation_subtitle(conversation),
                    stamp(&conversation.updated_at),
                )
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
    let loaded = session.read().state.conversations_loaded;
    let list_empty = items.is_empty();
    // Inline validation with the same rule the session enforces on create.
    // Shown once the owner has typed something.
    let slug_invalid = !slug.is_empty() && !serabot_desktop::valid_slug(&slug);
    let can_create = serabot_desktop::valid_slug(&slug);
    rsx! {
        div { class: "pane",
            div { class: "pane-head",
                div {
                    h1 { class: "pane-title", "Conversations" }
                    p { class: "muted", "Sections, groups, direct threads and rooms." }
                }
                button {
                    class: "btn ghost",
                    onclick: move |_| session.write().load_conversations(),
                    "Refresh"
                }
            }
            div { class: "pane-body",
                div { class: "card",
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
                                oninput: move |event| session.write().fleet.conv_title = event.value(),
                            }
                        }
                        div { class: "field",
                            label { class: "field-label", "Id" }
                            input {
                                class: if slug_invalid { "input invalid" } else { "input" },
                                value: "{slug}",
                                placeholder: "id (lowercase slug)",
                                "aria-invalid": if slug_invalid { "true" } else { "false" },
                                oninput: move |event| session.write().fleet.conv_slug = event.value(),
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
                            disabled: !can_create,
                            onclick: move |_| {
                                let kind = session.read().fleet.conv_kind;
                                let slug = session.read().fleet.conv_slug.clone();
                                let title = session.read().fleet.conv_title.clone();
                                let parent = session.read().fleet.conv_parent.trim().to_owned();
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
                if !loaded {
                    div { class: "card",
                        div { class: "skeleton title" }
                        div { class: "skeleton block" }
                    }
                }
                if loaded && list_empty {
                    div { class: "empty",
                        div { class: "empty-icon", Icon { name: "threads", size: 20 } }
                        h2 { class: "empty-title", "No conversations yet" }
                        p { class: "empty-text", "Create a section, a group or a direct thread above, or start a chat from the Chat tab." }
                        button {
                            class: "btn primary",
                            onclick: move |_| session.write().open_section(Section::Chat),
                            "Go to Chat"
                        }
                    }
                }
                div { class: "card-grid",
                    for (id, title, subtitle, updated) in items {
                        div { class: "card", key: "{id}",
                            div { class: "card-head",
                                div {
                                    h3 { class: "card-title", "{title}" }
                                    span { class: "card-sub", "{subtitle} · {updated}" }
                                }
                                div { class: "card-actions",
                                    button {
                                        class: "btn ghost sm",
                                        onclick: {
                                            let id = id.clone();
                                            move |_| session.write().open_conversation(id.clone())
                                        },
                                        "Open"
                                    }
                                    button {
                                        class: "btn ghost sm",
                                        onclick: {
                                            let id = id.clone();
                                            let title = title.clone();
                                            move |_| rename.set(Some((id.clone(), title.clone())))
                                        },
                                        "Rename"
                                    }
                                    button {
                                        class: "btn danger sm",
                                        onclick: {
                                            let id = id.clone();
                                            move |_| session.write().archive_conversation(id.clone())
                                        },
                                        "Archive"
                                    }
                                }
                            }
                            if let Some((edit_id, text)) = editing.clone() {
                                if edit_id == id {
                                    div { class: "field-row",
                                        input {
                                            class: "input",
                                            value: "{text}",
                                            oninput: move |event| {
                                                if let Some((_, text)) = rename.write().as_mut() {
                                                    *text = event.value();
                                                }
                                            },
                                        }
                                        button {
                                            class: "btn primary sm",
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
