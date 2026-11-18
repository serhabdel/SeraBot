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
    let can_create = !title.trim().is_empty();
    let sheet_title = format!("New {}", kind.label().to_lowercase());
    rsx! {
        div {
            class: "scrim open",
            onclick: move |_| overlay.set(Overlay::None),
        }
        div {
            class: "sheet open",
            role: "dialog",
            "aria-modal": "true",
            "aria-label": "{sheet_title}",
            tabindex: "-1",
            onkeydown: move |event: KeyboardEvent| {
                if event.key() == Key::Escape {
                    event.stop_propagation();
                    overlay.set(Overlay::None);
                }
            },
            div { class: "sheet-head",
                h2 { class: "sheet-title", "{sheet_title}" }
                button {
                    class: "icon-btn escape-close",
                    r#type: "button",
                    title: "Close",
                    "aria-label": "Close",
                    onclick: move |_| overlay.set(Overlay::None),
                    Icon { name: "x", size: 16 }
                }
            }
            div { class: "sheet-body",
                div { class: "field",
                    label { class: "field-label", r#for: "new-conv-kind", "Kind" }
                    select {
                        id: "new-conv-kind",
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
                    label { class: "field-label", r#for: "new-conv-title", "Name" }
                    input {
                        id: "new-conv-title",
                        class: "input",
                        autofocus: true,
                        value: "{title}",
                        placeholder: "name (for example Finance)",
                        oninput: move |event| session.write().fleet.conv_title = event.value(),
                        onkeydown: move |event: KeyboardEvent| {
                            if event.key() == Key::Enter && !session.read().fleet.conv_title.trim().is_empty() {
                                event.prevent_default();
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
                            }
                        },
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
                    r#type: "button",
                    onclick: move |_| overlay.set(Overlay::None),
                    "Cancel"
                }
                button {
                    class: "btn primary",
                    r#type: "button",
                    disabled: !can_create,
                    onclick: move |_| {
                        let title = session.read().fleet.conv_title.clone();
                        if title.trim().is_empty() {
                            return;
                        }
                        let kind = session.read().fleet.conv_kind;
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
    let saved_rules = team.as_ref().map(|t| t.rules.clone()).unwrap_or_default();
    let rules_dirty = rules_draft.trim() != saved_rules.trim();
    let rules_chars = rules_draft.trim().chars().count();
    let rules_too_long = rules_chars > 4_000;
    rsx! {
        div {
            class: "scrim open",
            onclick: move |_| overlay.set(Overlay::None),
        }
        div {
            class: "sheet open",
            role: "dialog",
            "aria-modal": "true",
            "aria-label": "Group members",
            tabindex: "-1",
            onkeydown: move |event: KeyboardEvent| {
                if event.key() == Key::Escape {
                    event.stop_propagation();
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
                        r#type: "button",
                        title: "Refresh members",
                        "aria-label": "Refresh members",
                        disabled: loading,
                        onclick: move |_| session.write().refresh_team(),
                        Icon { name: "refresh", size: 16 }
                    }
                    button {
                        class: "icon-btn escape-close",
                        r#type: "button",
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
                    div { class: "banner err", role: "alert", span { "{error}" } }
                }
                if !is_room {
                    div { class: "banner warn",
                        span { "Members are only available in group chats (room: conversations)." }
                    }
                }
                if is_room {
                    {
                        let title = session.read().conversation_title();
                        let letter = title
                            .chars()
                            .find(|c| c.is_alphanumeric())
                            .map(|c| c.to_uppercase().to_string())
                            .unwrap_or_else(|| "G".into());
                        let key = serabot_desktop::avatars::css_key("grp", &conversation);
                        let label = if serabot_desktop::avatars::has_image(&key) {
                            "Change image\u{2026}".to_owned()
                        } else {
                            "Set channel image\u{2026}".to_owned()
                        };
                        let conv_id = conversation.clone();
                        rsx! {
                            div { class: "field",
                                label { class: "field-label", "Channel image" }
                                crate::ui_bots::AvatarPicker {
                                    preview: None,
                                    letter,
                                    style: group_avatar_style(&conversation),
                                    has_image: false,
                                    label,
                                    on_pick: move |bytes: Vec<u8>| {
                                        session.write().pick_image(
                                            serabot_desktop::session::ImageSlot::Group(conv_id.clone()),
                                            bytes,
                                        );
                                    },
                                    on_clear: move |_| {},
                                }
                            }
                        }
                    }
                    div { class: "field",
                        label { class: "field-label", r#for: "team-mode-select", "Mode" }
                        select {
                            id: "team-mode-select",
                            class: "select",
                            value: "{mode_value}",
                            disabled: loading,
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
                        div { class: "card-head",
                            label { class: "field-label", r#for: "team-rules-input", "Group instructions" }
                            span { class: if rules_too_long { "tiny error-text" } else { "tiny" }, "{rules_chars}/4000" }
                        }
                        textarea {
                            id: "team-rules-input",
                            class: if rules_too_long { "textarea invalid" } else { "textarea" },
                            value: "{rules_draft}",
                            placeholder: "Standing rules every member reads before answering here\u{2026}",
                            oninput: move |event| session.write().team_rules_draft = event.value(),
                        }
                        div { class: "row",
                            button {
                                class: "btn primary sm",
                                r#type: "button",
                                disabled: loading || !rules_dirty || rules_too_long,
                                onclick: move |_| session.write().save_team_rules(),
                                if loading { "Saving\u{2026}" } else { "Save instructions" }
                            }
                            span { class: "hint", "Saved with the current members and mode." }
                        }
                    }
                }
            }
            div { class: "sheet-foot",
                button {
                    class: "btn ghost",
                    r#type: "button",
                    onclick: move |_| overlay.set(Overlay::None),
                    "Close"
                }
            }
        }
    }
}

#[derive(Clone, PartialEq)]
enum PaletteAction {
    Section(Section),
    Bot(String),
    Conversation(String),
    Overlay(Overlay),
    ToggleTheme,
}

#[derive(Clone)]
struct PaletteEntry {
    group: &'static str,
    title: String,
    subtitle: String,
    icon: &'static str,
    shortcut: &'static str,
    action: PaletteAction,
}

fn run_palette_action(
    action: PaletteAction,
    mut session: Signal<Session>,
    mut overlay: Signal<Overlay>,
) {
    overlay.set(Overlay::None);
    match action {
        PaletteAction::Section(section) => session.write().open_section(section),
        PaletteAction::Bot(id) => session.write().open_bot_chat(id),
        PaletteAction::Conversation(id) => session.write().open_conversation(id),
        PaletteAction::Overlay(next) => overlay.set(next),
        PaletteAction::ToggleTheme => {
            let next = match session.read().theme {
                Theme::Dark => Theme::Light,
                Theme::Light | Theme::System => Theme::Dark,
            };
            session.write().set_theme(next);
        }
    }
}

/// Studio command palette (`Ctrl+K`): quick jump to any workspace page, bot,
/// conversation, or action without touching the mouse.
#[component]
pub(super) fn CommandPaletteSheet(session: Signal<Session>, overlay: Signal<Overlay>) -> Element {
    let mut query = use_signal(String::new);
    let mut active_idx = use_signal(|| 0usize);

    let entries: Vec<PaletteEntry> = {
        let s = session.read();
        let mut out = Vec::new();
        for (section, icon, label, sub) in [
            (
                Section::Chat,
                "chat",
                "Chats",
                "Conversations & live agents",
            ),
            (
                Section::Workflows,
                "workflows",
                "Workflows",
                "Multi-step plans & schedules",
            ),
            (
                Section::Tasks,
                "list",
                "Tasks",
                "Approvals, deadlines & runs",
            ),
            (
                Section::Projects,
                "folder",
                "Projects",
                "Server project folders & files",
            ),
            (
                Section::Browser,
                "globe",
                "Browser",
                "Managed profiles & pages",
            ),
            (
                Section::Memory,
                "memory",
                "Memory",
                "Saved facts & retrieval",
            ),
            (Section::Vault, "lock", "Vault", "Sealed secrets & autofill"),
            (
                Section::Integrations,
                "connectors",
                "Integrations",
                "Plugins, MCP apps & bots",
            ),
            (
                Section::Providers,
                "providers",
                "AI providers",
                "Models, keys & sign-in accounts",
            ),
            (Section::Usage, "usage", "Usage", "Tokens, quotas & latency"),
            (Section::Logs, "logs", "Logs", "Runtime & audit logs"),
            (
                Section::Settings,
                "settings",
                "Settings",
                "Appearance, folders & server",
            ),
        ] {
            out.push(PaletteEntry {
                group: "Workspace",
                title: label.to_owned(),
                subtitle: sub.to_owned(),
                icon,
                shortcut: "",
                action: PaletteAction::Section(section),
            });
        }
        out.push(PaletteEntry {
            group: "Actions",
            title: "New conversation".to_owned(),
            subtitle: "Start a fresh thread or room".to_owned(),
            icon: "plus",
            shortcut: "",
            action: PaletteAction::Overlay(Overlay::NewConversation),
        });
        out.push(PaletteEntry {
            group: "Actions",
            title: "New bot".to_owned(),
            subtitle: "Create a custom specialist bot".to_owned(),
            icon: "bot",
            shortcut: "",
            action: PaletteAction::Overlay(Overlay::NewBot),
        });
        out.push(PaletteEntry {
            group: "Actions",
            title: "New group".to_owned(),
            subtitle: "Assemble bots into a room".to_owned(),
            icon: "group",
            shortcut: "",
            action: PaletteAction::Overlay(Overlay::NewGroup),
        });
        out.push(PaletteEntry {
            group: "Actions",
            title: "Toggle Light / Dark theme".to_owned(),
            subtitle: "Switch studio appearance".to_owned(),
            icon: "spark",
            shortcut: "",
            action: PaletteAction::ToggleTheme,
        });
        out.push(PaletteEntry {
            group: "Actions",
            title: "Keyboard shortcuts".to_owned(),
            subtitle: "View all keyboard shortcuts".to_owned(),
            icon: "sliders",
            shortcut: "F1",
            action: PaletteAction::Overlay(Overlay::Shortcuts),
        });
        for bot in s.state.listed_bots.iter().filter(|b| b.enabled) {
            let name = s.bot_display_name(&bot.id);
            let acp = s.bot_uses_claude_agent(&bot.id);
            let sub = if acp {
                format!("{} · Local Claude Code (ACP)", bot.role)
            } else {
                format!("{} · Server route", bot.role)
            };
            out.push(PaletteEntry {
                group: "Bots",
                title: name,
                subtitle: sub,
                icon: if acp { "code" } else { "bot" },
                shortcut: "",
                action: PaletteAction::Bot(bot.id.clone()),
            });
        }
        for conv in s
            .state
            .conversations
            .iter()
            .filter(|c| c.archived_at.is_none())
            .take(24)
        {
            let label = s.conversation_label(conv);
            let kind = conv.normalized_kind();
            out.push(PaletteEntry {
                group: "Conversations",
                title: label,
                subtitle: kind.to_owned(),
                icon: "chat",
                shortcut: "",
                action: PaletteAction::Conversation(conv.id.clone()),
            });
        }
        out
    };

    let needle = query().trim().to_lowercase();
    let filtered: Vec<PaletteEntry> = entries
        .into_iter()
        .filter(|entry| {
            needle.is_empty()
                || entry.title.to_lowercase().contains(&needle)
                || entry.subtitle.to_lowercase().contains(&needle)
                || entry.group.to_lowercase().contains(&needle)
        })
        .take(36)
        .collect();
    let count = filtered.len();
    let active = if count == 0 {
        0
    } else {
        active_idx().min(count - 1)
    };
    let keyed = filtered.clone();

    rsx! {
        div {
            class: "scrim open",
            onclick: move |_| overlay.set(Overlay::None),
        }
        div {
            class: "sheet sheet-palette open",
            role: "dialog",
            "aria-modal": "true",
            "aria-label": "Command palette",
            onkeydown: move |event: KeyboardEvent| {
                match event.key() {
                    Key::ArrowDown => {
                        event.prevent_default();
                        if count > 0 {
                            active_idx.set((active_idx() + 1).min(count - 1));
                        }
                    }
                    Key::ArrowUp => {
                        event.prevent_default();
                        active_idx.set(active_idx().saturating_sub(1));
                    }
                    Key::Enter => {
                        event.prevent_default();
                        if let Some(item) = keyed.get(active) {
                            run_palette_action(item.action.clone(), session, overlay);
                        }
                    }
                    Key::Escape => {
                        event.prevent_default();
                        overlay.set(Overlay::None);
                    }
                    _ => {}
                }
            },
            div { class: "palette-search-row",
                Icon { name: "search", size: 17 }
                input {
                    id: "cmd-palette-input",
                    class: "palette-input",
                    r#type: "search",
                    autofocus: true,
                    placeholder: "Jump to a screen, bot, conversation, or action\u{2026}",
                    value: "{query}",
                    oninput: move |event| {
                        query.set(event.value());
                        active_idx.set(0);
                    },
                }
                button {
                    class: "icon-btn escape-close",
                    r#type: "button",
                    title: "Close (Esc)",
                    "aria-label": "Close command palette",
                    onclick: move |_| overlay.set(Overlay::None),
                    Icon { name: "x", size: 15 }
                }
            }
            div { class: "palette-list", role: "listbox", "aria-label": "Command results",
                if filtered.is_empty() {
                    div { class: "palette-empty muted", "No matching screens, bots, or actions." }
                } else {
                    {
                        let mut prev_group = "";
                        rsx! {
                            for (idx, item) in filtered.into_iter().enumerate() {
                                {
                                    let show_group = item.group != prev_group;
                                    prev_group = item.group;
                                    let action = item.action.clone();
                                    rsx! {
                                        div { key: "{item.group}-{idx}-{item.title}",
                                            if show_group {
                                                div { class: "palette-group", "{item.group}" }
                                            }
                                            button {
                                                class: if idx == active { "palette-item active" } else { "palette-item" },
                                                r#type: "button",
                                                role: "option",
                                                "aria-selected": if idx == active { "true" } else { "false" },
                                                onclick: move |_| run_palette_action(action.clone(), session, overlay),
                                                span { class: "palette-item-icon",
                                                    Icon { name: item.icon, size: 16 }
                                                }
                                                span { class: "palette-item-copy",
                                                    span { class: "palette-item-title", "{item.title}" }
                                                    span { class: "palette-item-sub", "{item.subtitle}" }
                                                }
                                                if !item.shortcut.is_empty() {
                                                    kbd { class: "palette-kbd", "{item.shortcut}" }
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
            div { class: "palette-foot",
                span { class: "tiny muted", "Arrow keys to move · Enter to open · Esc to close" }
                span { class: "tiny muted", "Ctrl+Shift+F searches current page" }
            }
        }
    }
}
