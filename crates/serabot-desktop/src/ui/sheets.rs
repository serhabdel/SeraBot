use super::*;

/// Contextual right pane: bot profile, conversation details, provider and
/// connector summaries.
#[component]
pub(super) fn NewConversationSheet(session: Signal<Session>, overlay: Signal<Overlay>) -> Element {
    let mut overlay = overlay;
    // The kind the sheet works with. The caller sets it when the sheet
    // opens; the owner can change it with the Kind selector.
    let kind = session.read().fleet.conv_kind;
    // The team the owner picks before the room exists.
    let picked = use_signal(Vec::<String>::new);
    let parents = {
        let state = session.read();
        let parent_kinds: &[&str] = match kind {
            ConversationKind::Section => &[],
            ConversationKind::Group => &["section"],
            ConversationKind::Direct | ConversationKind::Room => &["section", "group"],
        };
        state
            .state
            .conversations
            .iter()
            .filter(|conversation| {
                parent_kinds
                    .iter()
                    .any(|parent| *parent == conversation.normalized_kind())
            })
            .map(|conversation| {
                (
                    conversation.id.clone(),
                    state.conversation_label(conversation),
                )
            })
            .collect::<Vec<_>>()
    };
    let title = session.read().fleet.conv_title.clone();
    let slug = session.read().fleet.conv_slug.clone();
    let parent = session.read().fleet.conv_parent.clone();
    // The picked bots as a draft team, so the shared team helpers can do the
    // membership maths (one to five members, no duplicates).
    let draft_team = Team {
        conversation_id: String::new(),
        mode: TeamMode::Selected,
        rules: String::new(),
        members: picked()
            .iter()
            .map(|bot| TeamMember {
                bot: bot.clone(),
                role: "specialist".into(),
                enabled: true,
                live: false,
            })
            .collect(),
    };
    // Current picks as removable rows at the top of the picker.
    let members: Vec<(String, String, String, String)> = {
        let state = session.read();
        picked()
            .iter()
            .filter_map(|id| {
                state
                    .state
                    .listed_bots
                    .iter()
                    .find(|bot| bot.id == *id)
                    .map(|bot| {
                        (
                            id.clone(),
                            state.bot_display_name(id),
                            state.bot_avatar(id),
                            bot.role.clone(),
                        )
                    })
            })
            .collect()
    };
    // Every enabled bot as a picker option; current picks are marked selected
    // and toggle off, the rest toggle on (capped by with_member).
    let picker_options: Vec<(String, String, String, String, bool)> = {
        let state = session.read();
        state
            .state
            .listed_bots
            .iter()
            .filter(|bot| bot.enabled)
            .map(|bot| {
                let id = bot.id.clone();
                (
                    id.clone(),
                    state.bot_display_name(&id),
                    state.bot_avatar(&id),
                    bot.role.clone(),
                    picked().contains(&id),
                )
            })
            .collect()
    };
    let mut picker_query = use_signal(String::new);
    let picker_needle = picker_query().trim().to_lowercase();
    let picker_rows: Vec<(String, String, String, String, bool)> = picker_options
        .into_iter()
        .filter(|(_, name, _, role, _)| {
            picker_needle.is_empty()
                || name.to_lowercase().contains(&picker_needle)
                || role.to_lowercase().contains(&picker_needle)
        })
        .collect();
    let picker_empty = picker_rows.is_empty();
    let picked_count = picked().len();
    let members_empty = members.is_empty();
    let team_full = picked_count >= MAX_TEAM_MEMBERS;
    rsx! {
        div {
            class: "scrim open",
            onclick: move |_| overlay.set(Overlay::None),
        }
        div {
            class: "sheet open",
            role: "dialog",
            onkeydown: move |event: KeyboardEvent| {
                if event.key() == Key::Escape {
                    overlay.set(Overlay::None);
                }
            },
            div { class: "sheet-head",
                h2 { class: "sheet-title", "New {kind.label().to_lowercase()}" }
                button {
                    class: "icon-btn escape-close",
                    title: "Close",
                    "aria-label": "Close",
                    onclick: move |_| overlay.set(Overlay::None),
                    Icon { name: "x", size: 16 }
                }
            }
            div { class: "sheet-body",
                div { class: "field",
                    label { class: "field-label", "Kind" }
                    select {
                        class: "select",
                        value: "{kind.as_str()}",
                        onchange: move |event| {
                            let next = ConversationKind::ALL
                                .into_iter()
                                .find(|candidate| candidate.as_str() == event.value())
                                .unwrap_or(ConversationKind::Section);
                            session.write().fleet.conv_kind = next;
                            session.write().fleet.conv_parent.clear();
                        },
                        for candidate in ConversationKind::ALL.into_iter().filter(|kind| *kind != ConversationKind::Room) {
                            option { value: "{candidate.as_str()}", "{candidate.label()}" }
                        }
                    }
                }
                div { class: "field",
                    label { class: "field-label", "Name" }
                    input {
                        class: "input",
                        autofocus: true,
                        value: "{title}",
                        placeholder: "name (for example Finance)",
                        oninput: move |event| session.write().fleet.conv_title = event.value(),
                    }
                }
                details { class: "advanced",
                    summary { "Advanced" }
                    div { class: "advanced-body",
                        div { class: "field",
                            label { class: "field-label", "Id" }
                            input {
                                class: "input",
                                value: "{slug}",
                                placeholder: "made from the name",
                                oninput: move |event| session.write().fleet.conv_slug = event.value(),
                            }
                            span { class: "hint", "Leave empty to make it from the name." }
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
                if kind == ConversationKind::Room {
                    div { class: "field",
                        label { class: "field-label", "Members" }
                        span { class: "member-count", "{picked_count} of {MAX_TEAM_MEMBERS} bots selected. The group is created first, then its members are written in the same step." }
                        div { class: "roster",
                            for (bot_id, bot_name, bot_avatar, bot_role) in members {
                                div { key: "{bot_id}", class: "roster-row",
                                    span { class: "avatar sm bot", style: avatar_style(&bot_id), "{bot_avatar}" }
                                    span { class: "thread-title", "{bot_name}" }
                                    span { class: "thread-meta", "{bot_role}" }
                                    button {
                                        class: "team-chip remove",
                                        title: "Remove from group",
                                        onclick: {
                                            let id = bot_id.clone();
                                            let team = draft_team.clone();
                                            move |_| {
                                                let mut picked = picked;
                                                let current = picked();
                                                let next = teams::without_member(&team, &id)
                                                    .unwrap_or_else(|| {
                                                        current
                                                            .iter()
                                                            .filter(|member| member.as_str() != id)
                                                            .cloned()
                                                            .collect()
                                                    });
                                                picked.set(next);
                                            }
                                        },
                                        Icon { name: "x", size: 13 }
                                    }
                                }
                            }
                            if members_empty {
                                p { class: "hint", "No bot picked yet." }
                            }
                        }
                        div { class: "member-picker",
                            input {
                                class: "input",
                                placeholder: "Search bots…",
                                value: "{picker_query}",
                                oninput: move |event| picker_query.set(event.value()),
                            }
                            div { class: "pop-list", style: "max-height: 220px;",
                                for (bot_id, bot_name, bot_avatar, bot_role, selected) in picker_rows {
                                    button {
                                        key: "{bot_id}",
                                        class: if selected { "member-option selected" } else { "member-option" },
                                        disabled: !selected && team_full,
                                        title: if selected { "Remove from group" } else if team_full { "The group is full" } else { "Add to group" },
                                        onclick: {
                                            let id = bot_id.clone();
                                            let team = draft_team.clone();
                                            move |_| {
                                                let mut picked = picked;
                                                let next = if selected {
                                                    teams::without_member(&team, &id)
                                                } else {
                                                    teams::with_member(&team, &id)
                                                };
                                                if let Some(next) = next {
                                                    picked.set(next);
                                                }
                                            }
                                        },
                                        span { class: "avatar sm bot", style: avatar_style(&bot_id), "{bot_avatar}" }
                                        div { class: "thread-copy",
                                            span { class: "thread-title", "{bot_name}" }
                                            span { class: "thread-meta", "{bot_role}" }
                                        }
                                        span { class: "member-check" }
                                    }
                                }
                                if picker_empty {
                                    p { class: "hint", "No enabled bot matches the search." }
                                }
                            }
                        }
                    }
                } else if kind == ConversationKind::Group {
                    div { class: "field",
                        label { class: "field-label", "Members" }
                        span { class: "hint", "Only group chats have members. Create a group chat instead, then pick its members." }
                    }
                }
            }
            div { class: "sheet-foot",
                button {
                    class: "btn ghost",
                    onclick: move |_| overlay.set(Overlay::None),
                    "Cancel"
                }
                button {
                    class: "btn primary",
                    onclick: move |_| {
                        let kind = session.read().fleet.conv_kind;
                        let title = session.read().fleet.conv_title.clone();
                        let slug = session.read().fleet.conv_slug.clone();
                        let slug = if slug.trim().is_empty() {
                            serabot_desktop::preview_id(&title, "item")
                        } else {
                            slug
                        };
                        let parent = session.read().fleet.conv_parent.trim().to_owned();
                        let parent = (!parent.is_empty()).then_some(parent);
                        let members = picked();
                        if kind == ConversationKind::Room && !members.is_empty() {
                            // The room and its team are created in one step.
                            session.write().create_conversation_with_team(
                                kind,
                                slug,
                                parent,
                                title,
                                members,
                            );
                        } else {
                            session.write().create_conversation(kind, slug, parent, title);
                        }
                        overlay.set(Overlay::None);
                    },
                    "Create"
                }
            }
        }
    }
}

#[component]
pub(super) fn TeamSheet(session: Signal<Session>, overlay: Signal<Overlay>) -> Element {
    let mut overlay = overlay;
    let mut bot_query = use_signal(String::new);
    let conversation = session.read().conversation.clone();
    let is_room = teams::is_room(&conversation);
    let rules_draft = session.read().team_rules_draft.clone();
    let (members, mode, loading, error) = {
        let state = session.read();
        let team = state.state.team.clone();
        let members = team
            .as_ref()
            .map(|team| {
                team.members
                    .iter()
                    .map(|member| {
                        (
                            member.bot.clone(),
                            state.bot_display_name(&member.bot),
                            state.bot_avatar(&member.bot),
                            member.role.clone(),
                            member.live,
                            member.enabled,
                        )
                    })
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        (
            members,
            team.as_ref().map(|team| team.mode),
            state.state.team_loading,
            state.state.team_error.clone(),
        )
    };
    // The live team is captured for the picker toggles (with_member /
    // without_member do the capped membership maths).
    let team = session.read().state.team.clone();
    let member_ids = members
        .iter()
        .map(|(id, ..)| id.clone())
        .collect::<Vec<_>>();
    // Every enabled bot is a picker option: current members show selected
    // and toggle off, the rest (teams::available_bots) toggle on.
    let picker_options: Vec<(String, String, String, String, bool)> = {
        let state = session.read();
        state
            .state
            .listed_bots
            .iter()
            .filter(|bot| bot.enabled)
            .map(|bot| {
                let id = bot.id.clone();
                (
                    id.clone(),
                    state.bot_display_name(&id),
                    state.bot_avatar(&id),
                    bot.role.clone(),
                    member_ids.contains(&id),
                )
            })
            .collect()
    };
    let needle = bot_query().trim().to_lowercase();
    let mode_value = mode
        .map(|mode| mode.as_str())
        .unwrap_or("selected")
        .to_owned();
    let picker_rows: Vec<(String, String, String, String, bool)> = picker_options
        .into_iter()
        .filter(|(_, name, _, role, _)| {
            needle.is_empty()
                || name.to_lowercase().contains(&needle)
                || role.to_lowercase().contains(&needle)
        })
        .collect();
    let picker_empty = picker_rows.is_empty();
    let team_full = member_ids.len() >= MAX_TEAM_MEMBERS;
    rsx! {
        div {
            class: "scrim open",
            onclick: move |_| overlay.set(Overlay::None),
        }
        div {
            class: "sheet open",
            role: "dialog",
            onkeydown: move |event: KeyboardEvent| {
                if event.key() == Key::Escape {
                    overlay.set(Overlay::None);
                }
            },
            div { class: "sheet-head",
                h2 { class: "sheet-title", "Group members" }
                div { class: "row",
                    if loading {
                        span { class: "spinner", title: "Loading members" }
                    }
                    button {
                        class: "icon-btn",
                        title: "Refresh members",
                        "aria-label": "Refresh members",
                        onclick: move |_| session.write().refresh_team(),
                        Icon { name: "refresh", size: 16 }
                    }
                    button {
                        class: "icon-btn escape-close",
                        title: "Close",
                        "aria-label": "Close",
                        onclick: move |_| overlay.set(Overlay::None),
                        Icon { name: "x", size: 16 }
                    }
                }
            }
            div { class: "sheet-body",
                p { class: "hint", "Group: {conversation}" }
                if let Some(error) = error {
                    div { class: "banner err", span { "{error}" } }
                }
                if !is_room {
                    div { class: "banner warn",
                        span { "Members are only available in group chats (room: conversations)." }
                    }
                }
                if is_room {
                    div { class: "field",
                        label { class: "field-label", "Mode" }
                        select {
                            class: "select",
                            value: "{mode_value}",
                            onchange: move |event| {
                                let next = if event.value() == "all" {
                                    TeamMode::All
                                } else {
                                    TeamMode::Selected
                                };
                                session.write().set_team_mode(next);
                            },
                            option { value: "selected", "Selected · the orchestrator picks who answers" }
                            option { value: "all", "All members · every member answers" }
                        }
                    }
                    div { class: "field",
                        label { class: "field-label", "Members" }
                        div { class: "roster",
                            for (id, name, avatar, role, live, enabled) in members {
                                div { key: "{id}", class: "roster-row",
                                    span { class: "avatar sm bot", style: avatar_style(&id), "{avatar}" }
                                    span { class: "thread-title", "{name}" }
                                    span { class: "thread-meta", "{role}" }
                                    span {
                                        class: if live { "chip ok" } else { "chip warn" },
                                        if live { "live" } else { "restart required" }
                                    }
                                    if !enabled {
                                        span { class: "chip danger", "disabled" }
                                    }
                                    button {
                                        class: "team-chip remove",
                                        title: "Remove from group",
                                        onclick: {
                                            let id = id.clone();
                                            move |_| session.write().remove_team_member(id.clone())
                                        },
                                        Icon { name: "x", size: 13 }
                                    }
                                }
                            }
                        }
                        span { class: "hint", "A group holds up to five enabled bots. The orchestrator is always implied." }
                    }
                    div { class: "field",
                        label { class: "field-label", "Bots" }
                        span { class: "member-count", "{member_ids.len()} of {MAX_TEAM_MEMBERS} bots selected" }
                        div { class: "member-picker",
                            input {
                                class: "input",
                                value: "{bot_query}",
                                placeholder: "Search bots…",
                                oninput: move |event| bot_query.set(event.value()),
                            }
                            div { class: "pop-list", style: "max-height: 220px;",
                                for (id, name, avatar, role, selected) in picker_rows {
                                    button {
                                        key: "{id}",
                                        class: if selected { "member-option selected" } else { "member-option" },
                                        disabled: !selected && team_full,
                                        title: if selected { "Remove from group" } else if team_full { "The group is full" } else { "Add to group" },
                                        onclick: {
                                            let id = id.clone();
                                            let team = team.clone();
                                            move |_| {
                                                if let Some(team) = team.clone() {
                                                    let next = if selected {
                                                        teams::without_member(&team, &id)
                                                    } else {
                                                        teams::with_member(&team, &id)
                                                    };
                                                    if let Some(next) = next {
                                                        session.write().set_team_members(next);
                                                    }
                                                }
                                            }
                                        },
                                        span { class: "avatar sm bot", style: avatar_style(&id), "{avatar}" }
                                        div { class: "thread-copy",
                                            span { class: "thread-title", "{name}" }
                                            span { class: "thread-meta", "{role}" }
                                        }
                                        span { class: "member-check" }
                                    }
                                }
                                if picker_empty {
                                    p { class: "hint", "No enabled bot matches the search." }
                                }
                            }
                        }
                    }
                    div { class: "field",
                        label { class: "field-label", "Group instructions" }
                        textarea {
                            class: "textarea",
                            value: "{rules_draft}",
                            placeholder: "Standing rules every member reads before answering here…",
                            oninput: move |event| session.write().team_rules_draft = event.value(),
                        }
                        div { class: "row",
                            button {
                                class: "btn primary sm",
                                onclick: move |_| session.write().save_team_rules(),
                                "Save instructions"
                            }
                            span { class: "hint", "Saved with the current members and mode." }
                        }
                    }
                }
            }
            div { class: "sheet-foot",
                button {
                    class: "btn ghost",
                    onclick: move |_| overlay.set(Overlay::None),
                    "Close"
                }
            }
        }
    }
}
