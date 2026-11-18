use super::*;
use serabot_desktop::row_menu::RowKind;

#[component]
pub(super) fn Shell(session: Signal<Session>) -> Element {
    let section = session.read().section;
    let mut session = session;
    let overlay = use_signal(|| Overlay::None);
    let chat = section == Section::Chat;
    let mut drawer_open = use_signal(|| false);
    let mut toast = use_signal(|| Option::<(String, bool)>::None);
    let undo = use_signal(|| Option::<String>::None);
    // The topbar's search button and the chat view's find bar share this flag.
    use_context_provider(|| ChatFindOpen(Signal::new(false)));
    // Claude Code on this computer keeps running while other pages are open.
    crate::ui_claude_code::use_claude_code_provider();
    // A sign-in the session starts by itself (right after a save, or when an
    // error asks for one) opens its sheet here.
    let mut overlay_for_signin = overlay;
    use_effect(move || {
        let wants = session.read().accounts_ui.sheet_provider.is_some();
        if wants && overlay_for_signin() == Overlay::None {
            overlay_for_signin.set(Overlay::AddAccount);
        }
    });
    let onboarding_open = chat && session.read().onboarding_ui.open;
    let settings_open = session.read().bot_settings.open;
    let preview_open = session.read().state.preview_dock_hash.is_some();
    // One stylesheet defines every avatar image; tiles only reference it.
    let avatar_css = serabot_desktop::avatars::stylesheet(&session.read().state.avatars);

    rsx! {
        style { "{avatar_css}" }
        div { class: "app-shell",
            IconRail { session, overlay, toast }
            if chat {
                Sidebar { session, overlay, drawer_open, toast, undo }
                div {
                    class: if drawer_open() { "scrim drawer-scrim open" } else { "scrim drawer-scrim" },
                    onclick: move |_| drawer_open.set(false),
                }
            }
            main { class: "main",
                if chat && !onboarding_open {
                    Topbar { session, overlay, drawer_open, toast, undo }
                }
                StatusBanner { session }
                UpdateBanner { session }
                if onboarding_open {
                    OnboardingView { session }
                } else {
                div { class: "pane",
                    {match section {
                        // Kept mounted across conversation switches so the DOM,
                        // observers, and WebView bridges stay warm and instant.
                        Section::Chat => rsx! {
                            ChatView { session, overlay }
                        },
                        Section::Conversations => rsx! { ConversationsView { session } },
                        Section::Memory => rsx! { MemoryView { session } },
                        Section::Providers => rsx! { ProvidersView { session, overlay } },
                        Section::Browser => rsx! { BrowserView { session, overlay } },

                        Section::Tasks => rsx! { crate::ui_tasks::TasksView { session } },
                        Section::Vault => rsx! { VaultPage { session } },
                        Section::Usage => rsx! { UsagePage { session } },
                        Section::Logs => rsx! { crate::ui_logs::LogsView { session } },
                        Section::Workflows => rsx! { crate::ui_workflows::WorkflowsView { session } },
                        Section::Integrations => rsx! { crate::ui_integrations::IntegrationsView { session } },
                        Section::Settings => rsx! { SettingsView { session } },
                    }}
                }
                }
            }
            div {
                class: if (chat && settings_open) || preview_open {
                    "scrim inspector-scrim open"
                } else {
                    "scrim inspector-scrim"
                },
                onclick: move |_| {
                    let mut s = session.write();
                    if s.bot_settings.open {
                        s.close_bot_settings();
                    }
                    if s.state.preview_dock_hash.is_some() {
                        s.close_preview_dock();
                    }
                },
            }
            if chat {
                SettingsPanel { session, overlay }
            }
            crate::ui_files::PreviewDock { session }
            BrowserDock { session }
            {match overlay() {
                Overlay::NewConversation => rsx! {
                    NewConversationSheet { session, overlay }
                },
                Overlay::NewBot => rsx! {
                    NewBotSheet { session, overlay }
                },
                Overlay::NewGroup => rsx! {
                    NewGroupSheet { session, overlay }
                },
                Overlay::AddProvider => rsx! {
                    AddProviderSheet { session, overlay }
                },
                Overlay::AddAccount => rsx! {
                    AddAccountSheet { session, overlay }
                },
                Overlay::Team => rsx! {
                    TeamSheet { session, overlay }
                },
                Overlay::MeetTeam => rsx! {
                    MeetTeamSheet { session, overlay }
                },
                Overlay::Shortcuts => rsx! {
                    ShortcutsSheet { session, overlay }
                },
                Overlay::CommandPalette => rsx! {
                    CommandPaletteSheet { session, overlay }
                },
                Overlay::None => rsx! {},
            }}
            UndoBar { session, undo }
            if let Some((message, failed)) = toast() {
                div { class: if failed { "toast err" } else { "toast" }, role: "status",
                    span { "{message}" }
                    button {
                        class: "icon-btn escape-close",
                        title: "Dismiss",
                        "aria-label": "Dismiss",
                        onclick: move |_| toast.set(None),
                        Icon { name: "x", size: 16 }
                    }
                }
            }
        }
    }
}

/// The 58 px studio navigation rail: quick command jump on top, grouped
/// destinations (Work, Data, Connect, System), Settings and the account below.
#[component]
pub(super) fn IconRail(
    session: Signal<Session>,
    overlay: Signal<Overlay>,
    toast: Signal<Option<(String, bool)>>,
) -> Element {
    let mut session = session;
    let mut overlay = overlay;
    let section = session.read().section;
    let work: [(Section, &str, &str); 3] = [
        (Section::Chat, "chat", "Chats"),
        (Section::Workflows, "workflows", "Workflows"),
        (Section::Tasks, "list", "Tasks"),
    ];
    let data: [(Section, &str, &str); 3] = [
        (Section::Browser, "globe", "Browser"),
        (Section::Memory, "memory", "Memory"),
        (Section::Vault, "lock", "Vault"),
    ];
    let connect: [(Section, &str, &str); 2] = [
        (Section::Integrations, "connectors", "Integrations"),
        (Section::Providers, "providers", "AI providers"),
    ];
    let system: [(Section, &str, &str); 2] = [
        (Section::Usage, "usage", "Usage"),
        (Section::Logs, "logs", "Logs"),
    ];
    rsx! {
        nav { class: "rail", "aria-label": "Main navigation",
            button {
                id: "cmd-palette-trigger",
                class: if overlay() == Overlay::CommandPalette { "rail-btn labeled rail-jump on" } else { "rail-btn labeled rail-jump" },
                r#type: "button",
                title: "Command palette (Ctrl+K)",
                "aria-label": "Command palette",
                onclick: move |_| {
                    let next = if overlay() == Overlay::CommandPalette {
                        Overlay::None
                    } else {
                        Overlay::CommandPalette
                    };
                    overlay.set(next);
                },
                Icon { name: "search", size: 17 }
                span { class: "rail-label", "Jump" }
            }
            div { class: "rail-divider", "aria-hidden": "true" }
            for group in [work.as_slice(), data.as_slice(), connect.as_slice(), system.as_slice()] {
                for (target, icon, label) in group.iter().copied() {
                    button {
                        key: "{label}",
                        class: if section == target { "rail-btn labeled on" } else { "rail-btn labeled" },
                        title: "{label}",
                        "aria-label": "{label}",
                        "aria-current": if section == target { "page" } else { "false" },
                        onclick: move |_| session.write().open_section(target),
                        Icon { name: icon, size: 18 }
                        span { class: "rail-label", "{label}" }
                    }
                }
                div { class: "rail-divider", "aria-hidden": "true" }
            }
            div { class: "rail-spacer" }
            button {
                class: if section == Section::Settings { "rail-btn labeled on" } else { "rail-btn labeled" },
                title: "Settings",
                "aria-label": "Settings",
                "aria-current": if section == Section::Settings { "page" } else { "false" },
                onclick: move |_| session.write().open_section(Section::Settings),
                Icon { name: "settings", size: 18 }
                span { class: "rail-label", "Settings" }
            }
            UserMenu { session, toast }
        }
    }
}

/// The new-chat menu shared by the sidebar head (a labelled "New chat"
/// button) and the composer row (a "+" icon). It opens on a "Choose a bot"
/// list; creating groups, folders and bots sits underneath.
#[component]
pub(super) fn NewMenu(
    session: Signal<Session>,
    overlay: Signal<Overlay>,
    #[props(default = false)] right: bool,
    #[props(default = false)] labelled: bool,
) -> Element {
    let mut overlay = overlay;
    let mut open = use_signal(|| false);
    let align = if right { " right" } else { "" };
    let bots: Vec<(String, String, String, String)> = {
        let state = session.read();
        state
            .state
            .listed_bots
            .iter()
            .filter(|bot| bot.enabled)
            .map(|bot| {
                (
                    bot.id.clone(),
                    state.bot_display_name(&bot.id),
                    state.bot_avatar(&bot.id),
                    bot.role.clone(),
                )
            })
            .collect()
    };
    rsx! {
        div { class: "menu-wrap",
            if labelled {
                button {
                    class: "btn primary sm new-chat-btn menu-trigger",
                    r#type: "button",
                    "aria-haspopup": "menu",
                    "aria-expanded": if open() { "true" } else { "false" },
                    onclick: move |_| open.set(!open()),
                    Icon { name: "plus", size: 14 }
                    "New chat"
                }
            } else {
                button {
                    class: "icon-btn primary menu-trigger",
                    r#type: "button",
                    title: "New chat, group or bot",
                    "aria-label": "New chat, group or bot",
                    "aria-haspopup": "menu",
                    "aria-expanded": if open() { "true" } else { "false" },
                    onclick: move |_| open.set(!open()),
                    Icon { name: "plus", size: 18 }
                }
            }
            div {
                class: if open() { format!("menu new-chat-menu open{align}") } else { format!("menu new-chat-menu{align}") },
                onkeydown: move |event: KeyboardEvent| {
                    if event.key() == Key::Escape {
                        open.set(false);
                    }
                },
                div { class: "menu-label", "Choose a bot" }
                div { class: "menu-scroll",
                    if bots.is_empty() {
                        span { class: "menu-empty caption", "No bots are available yet." }
                    }
                    for (id, name, avatar, role) in bots {
                        button {
                            key: "{id}",
                            class: "menu-item menu-bot",
                            r#type: "button",
                            onclick: {
                                let id = id.clone();
                                move |_| {
                                    open.set(false);
                                    session.write().open_bot_chat(id.clone());
                                }
                            },
                            span { class: "avatar sm bot", style: avatar_style(&id), "{avatar}" }
                            span { class: "menu-bot-copy",
                                span { class: "menu-bot-name", "{name}" }
                                span { class: "menu-bot-role", "{role}" }
                            }
                        }
                    }
                }
                div { class: "menu-sep" }
                div { class: "menu-label", "Or create" }
                // A team lives on a room: the room is created first and its
                // team is written in the same step (docs/desktop-redesign.md
                // §3.8).
                button {
                    class: "menu-item",
                    r#type: "button",
                    onclick: move |_| {
                        open.set(false);
                        overlay.set(Overlay::NewGroup);
                    },
                    Icon { name: "shield", size: 16 }
                    "New group chat"
                }
                button {
                    class: "menu-item",
                    r#type: "button",
                    onclick: move |_| {
                        open.set(false);
                        session.write().fleet.conv_kind = ConversationKind::Group;
                        overlay.set(Overlay::NewConversation);
                    },
                    Icon { name: "users", size: 16 }
                    "New folder"
                }
                button {
                    class: "menu-item",
                    r#type: "button",
                    onclick: move |_| {
                        open.set(false);
                        session.write().fleet.conv_kind = ConversationKind::Section;
                        overlay.set(Overlay::NewConversation);
                    },
                    Icon { name: "folder", size: 16 }
                    "New section"
                }
                button {
                    class: "menu-item",
                    r#type: "button",
                    onclick: move |_| {
                        open.set(false);
                        overlay.set(Overlay::NewBot);
                    },
                    Icon { name: "robot", size: 16 }
                    "New bot"
                }
                button {
                    class: "menu-item",
                    r#type: "button",
                    onclick: move |_| {
                        open.set(false);
                        session.write().open_meet_team();
                        overlay.set(Overlay::MeetTeam);
                    },
                    Icon { name: "users", size: 16 }
                    "Meet your team"
                }
            }
        }
    }
}
/// Inline SVG pin button: toggles a bot id or conversation id in the pin
/// list. Stops propagation so it never triggers the surrounding row.
#[component]
pub(crate) fn PinToggle(session: Signal<Session>, id: String, pinned: bool) -> Element {
    rsx! {
        button {
            class: if pinned { "pin-toggle pinned" } else { "pin-toggle" },
            title: if pinned { "Unpin" } else { "Pin" },
            "aria-label": if pinned { "Unpin" } else { "Pin" },
            onclick: move |event| {
                event.stop_propagation();
                session.write().toggle_pin(id.clone());
            },
            svg {
                width: 13,
                height: 13,
                "viewBox": "0 0 24 24",
                fill: "none",
                stroke: "currentColor",
                "stroke-width": 1.8,
                "stroke-linecap": "round",
                "stroke-linejoin": "round",
                path { d: "M9 4h6l1.2 7 2.3 2.3V16H5.5v-2.7L7.8 11 9 4zM12 16v4.5" }
            }
        }
    }
}

#[component]
pub(super) fn Sidebar(
    session: Signal<Session>,
    overlay: Signal<Overlay>,
    drawer_open: Signal<bool>,
    toast: Signal<Option<(String, bool)>>,
    undo: Signal<Option<String>>,
) -> Element {
    let mut overlay = overlay;
    let mut drawer_open = drawer_open;
    let mut toast = toast;
    let mut search = use_signal(String::new);
    let mut collapsed = use_signal(HashSet::<String>::new);
    let mut renaming = use_signal(|| Option::<(String, String)>::None);
    // Which row's "more actions" menu is open (by row id).
    let menu = use_signal(|| Option::<String>::None);
    let needle = search().trim().to_lowercase();
    let current = session.read().conversation.clone();
    let loaded = session.read().state.conversations_loaded;
    let pins = session.read().pins.clone();
    let items = {
        let state = session.read();
        let bots = &state.state.listed_bots;
        let conversations = &state.state.conversations;
        let matches = |label: &str, subtitle: &str| {
            needle.is_empty()
                || label.to_lowercase().contains(&needle)
                || subtitle.to_lowercase().contains(&needle)
        };
        let bot_chats: HashSet<String> = bots.iter().map(|bot| format!("dir:{}", bot.id)).collect();
        let mut items = Vec::new();

        // A bot shows when its name matches or one of its chats does, so the
        // search (and Ctrl+K, which focuses it) can jump straight to a chat.
        let bot_rows = bots
            .iter()
            .filter(|bot| {
                matches(
                    &state.bot_display_name(&bot.id),
                    &format!("bot {}", bot.role),
                ) || !serabot_desktop::bot_chats::chats_matching(
                    state.state.bot_chats.get(&bot.id),
                    &needle,
                )
                .is_empty()
            })
            .collect::<Vec<_>>();
        if needle.is_empty() || !bot_rows.is_empty() {
            let key = "group:bots".to_owned();
            let is_collapsed = collapsed().contains(&key);
            items.push(RailItem::Header {
                key: key.clone(),
                title: "Bots".into(),
                count: bots.len(),
                total: Some(state.state.shipped_bots.len()).filter(|total| *total > bots.len()),
                collapsed: is_collapsed,
            });
            if !is_collapsed {
                for bot in bot_rows {
                    let id = format!("dir:{}", bot.id);
                    let main_unread = conversations
                        .iter()
                        .find(|conversation| conversation.id == id)
                        .map(|conversation| conversation.unread_count)
                        .unwrap_or(0);
                    let chats = state
                        .state
                        .bot_chats
                        .get(&bot.id)
                        .map(serabot_desktop::bot_chats::live_chats)
                        .unwrap_or_default();
                    // The bot's badge covers its other chats too.
                    let other_unread: Vec<i64> = chats
                        .iter()
                        .filter(|chat| chat.id != id)
                        .filter_map(|chat| {
                            conversations
                                .iter()
                                .find(|conversation| conversation.id == chat.id)
                                .map(|conversation| conversation.unread_count)
                        })
                        .collect();
                    let unread = chat_feed::rolled_up_unread(main_unread, &other_unread);
                    let extra = chats.iter().filter(|chat| !chat.primary).count();
                    let has_active = chats.iter().any(|chat| chat.id == current);
                    // The latest message of the bot's main chat reads better
                    // than repeating its role on every row.
                    let subtitle = conversations
                        .iter()
                        .find(|conversation| conversation.id == id)
                        .filter(|conversation| {
                            conversation
                                .last_message
                                .as_ref()
                                .is_some_and(|last| !last.text.trim().is_empty())
                        })
                        .map(|conversation| state.conversation_list_subtitle(conversation))
                        .unwrap_or_else(|| capitalise_role(&bot.role));
                    items.push(RailItem::Bot {
                        active: id == current || has_active,
                        pinned: pins.contains(&bot.id),
                        id,
                        label: state.bot_display_name(&bot.id),
                        role: bot.role.clone(),
                        subtitle,
                        avatar: state.bot_avatar(&bot.id),
                        enabled: bot.enabled,
                        live: bot.live,
                        unread,
                        extra_chats: extra,
                    });
                }
                if needle.is_empty() && state.state.shipped_bots.len() > bots.len() {
                    items.push(RailItem::AddBots);
                }
            }
        }

        for section in conversations
            .iter()
            .filter(|conversation| conversation.normalized_kind() == "section")
        {
            let children = conversations
                .iter()
                .filter(|conversation| {
                    conversation.parent_id.as_deref() == Some(&section.id)
                        && !bot_chats.contains(&conversation.id)
                })
                .collect::<Vec<_>>();
            let mut rows = Vec::new();
            for child in &children {
                let label = state.conversation_label(child);
                let subtitle = state.conversation_list_subtitle(child);
                if matches(&label, &subtitle) {
                    rows.push(RailItem::Thread {
                        id: child.id.clone(),
                        label,
                        subtitle,
                        depth: 1,
                        active: child.id == current,
                        unread: child.unread_count,
                        archived: false,
                        pinned: pins.contains(&child.id),
                    });
                }
                if child.normalized_kind() == "group" {
                    for grand in conversations.iter().filter(|conversation| {
                        conversation.parent_id.as_deref() == Some(&child.id)
                            && !bot_chats.contains(&conversation.id)
                    }) {
                        let label = state.conversation_label(grand);
                        let subtitle = state.conversation_list_subtitle(grand);
                        if matches(&label, &subtitle) {
                            rows.push(RailItem::Thread {
                                id: grand.id.clone(),
                                label,
                                subtitle,
                                depth: 2,
                                active: grand.id == current,
                                unread: grand.unread_count,
                                archived: false,
                                pinned: pins.contains(&grand.id),
                            });
                        }
                    }
                }
            }
            if needle.is_empty() || !rows.is_empty() {
                let is_collapsed = collapsed().contains(&section.id);
                items.push(RailItem::Header {
                    key: section.id.clone(),
                    title: state.conversation_label(section),
                    count: rows.len(),
                    total: None,
                    collapsed: is_collapsed,
                });
                if !is_collapsed {
                    items.extend(rows);
                }
            }
        }

        let mut roots = conversations
            .iter()
            .filter(|conversation| {
                conversation.parent_id.is_none()
                    && conversation.normalized_kind() != "section"
                    && !bot_chats.contains(&conversation.id)
                    && !serabot_desktop::bot_chats::is_extra_chat_id(&conversation.id)
            })
            .collect::<Vec<_>>();
        roots.sort_by(|left, right| {
            let left_room = left.normalized_kind() == "room";
            let right_room = right.normalized_kind() == "room";
            right_room
                .cmp(&left_room)
                .then_with(|| right.updated_at.cmp(&left.updated_at))
        });
        let room_total = roots
            .iter()
            .filter(|conversation| conversation.normalized_kind() == "room")
            .count();
        let direct_total = roots.len().saturating_sub(room_total);
        let mut rooms_added = false;
        let mut directs_added = false;
        for conversation in roots {
            let is_room = conversation.normalized_kind() == "room";
            let label = state.conversation_label(conversation);
            let subtitle = state.conversation_list_subtitle(conversation);
            if !matches(&label, &subtitle) {
                continue;
            }
            let (key, title, added) = if is_room {
                ("group:rooms", "Groups", &mut rooms_added)
            } else {
                ("group:directs", "Chats", &mut directs_added)
            };
            if !*added {
                *added = true;
                let is_collapsed = collapsed().contains(key);
                items.push(RailItem::Header {
                    key: key.to_owned(),
                    title: title.to_owned(),
                    count: if is_room { room_total } else { direct_total },
                    total: None,
                    collapsed: is_collapsed,
                });
            }
            let is_collapsed = collapsed().contains(key);
            if !is_collapsed {
                items.push(RailItem::Thread {
                    id: conversation.id.clone(),
                    label,
                    subtitle,
                    depth: 0,
                    active: conversation.id == current,
                    unread: conversation.unread_count,
                    archived: false,
                    pinned: pins.contains(&conversation.id),
                });
            }
        }

        // An empty Archived section is chrome that announces its own emptiness;
        // it only appears once something is actually archived.
        if needle.is_empty() && !state.state.archived.is_empty() {
            let archived = &state.state.archived;
            let is_collapsed = collapsed().contains("group:archived");
            items.push(RailItem::Header {
                key: "group:archived".to_owned(),
                title: "Archived".into(),
                count: archived.len(),
                total: None,
                collapsed: is_collapsed,
            });
            if !is_collapsed {
                for conversation in archived {
                    items.push(RailItem::Thread {
                        id: conversation.id.clone(),
                        label: state.conversation_label(conversation),
                        subtitle: state.conversation_list_subtitle(conversation),
                        depth: 0,
                        active: false,
                        unread: 0,
                        archived: true,
                        pinned: pins.contains(&conversation.id),
                    });
                }
            }
        }
        items
    };
    let list_empty = items.is_empty();
    let searching = !needle.is_empty();
    rsx! {
        aside {
            class: if drawer_open() { "sidebar open" } else { "sidebar" },
            "aria-label": "Conversations",
            div { class: "sidebar-head",
                h2 { class: "list-title", "Chats" }
                NewMenu { session, overlay, right: true, labelled: true }
            }
            div { class: "sidebar-search",
                span { class: "search-glyph", Icon { name: "search", size: 15 } }
                input {
                    class: "input",
                    id: "global-search",
                    value: "{search}",
                    placeholder: "Search chats (Ctrl+K)",
                    "aria-label": "Search chats and bots",
                    oninput: move |event| search.set(event.value()),
                    onkeydown: move |event: KeyboardEvent| {
                        if event.key() == Key::Escape && !search().is_empty() {
                            event.stop_propagation();
                            search.set(String::new());
                        }
                    },
                }
                if searching {
                    button {
                        class: "icon-btn search-clear",
                        r#type: "button",
                        title: "Clear search",
                        "aria-label": "Clear search",
                        onclick: move |_| search.set(String::new()),
                        Icon { name: "x", size: 13 }
                    }
                }
            }
            div { class: "thread-list",
                if !loaded {
                    div { class: "skeleton row", " " }
                    div { class: "skeleton row", " " }
                    div { class: "skeleton row", " " }
                    div { class: "skeleton row", " " }
                }
                if loaded && list_empty {
                    div { class: "empty compact",
                        h2 { class: "empty-title",
                            if searching { "No chats match" } else { "No chats yet" }
                        }
                        p { class: "empty-text",
                            if searching { "Try a different word, or clear the search." } else { "Start a conversation with one of your bots." }
                        }
                        if searching {
                            button { class: "btn", onclick: move |_| search.set(String::new()), "Clear search" }
                        } else {
                            button {
                                class: "btn primary",
                                onclick: move |_| {
                                    session.write().fleet.conv_kind = ConversationKind::Direct;
                                    overlay.set(Overlay::NewConversation);
                                },
                                Icon { name: "plus", size: 16 }
                                "New chat"
                            }
                        }
                    }
                }
                for item in items {
                    {match item {
                        RailItem::Header { key, title, count, total, collapsed: closed } => {
                            let toggle = key.clone();
                            rsx! {
                                button {
                                    key: "{key}",
                                    class: "thread-group",
                                    r#type: "button",
                                    "aria-expanded": if closed { "false" } else { "true" },
                                    onclick: move |_| {
                                        let mut set = collapsed.write();
                                        let opening = set.contains(&toggle);
                                        if opening {
                                            set.remove(&toggle);
                                        } else {
                                            set.insert(toggle.clone());
                                        }
                                        // The first open of Archived fetches it.
                                        if opening && toggle == "group:archived" {
                                            session.write().load_archived();
                                        }
                                    },
                                    Icon { name: if closed { "chevron-right" } else { "chevron-down" }, size: 12 }
                                    span { class: "thread-title", "{title}" }
                                    if let Some(total) = total {
                                        span {
                                            class: "thread-count",
                                            title: "{count} of {total} built-in bots",
                                            "{count}"
                                        }
                                    } else {
                                        span { class: "thread-count", "{count}" }
                                    }
                                }
                            }
                        },
                        RailItem::AddBots => rsx! {
                            button {
                                class: "thread-add",
                                r#type: "button",
                                title: "See every built-in bot and add the ones you do not have",
                                onclick: move |_| {
                                    drawer_open.set(false);
                                    session.write().open_bots_tab();
                                },
                                span { class: "thread-add-icon", Icon { name: "plus", size: 14 } }
                                "Add more bots"
                            }
                        },
                        RailItem::Bot { id, label, role, subtitle, avatar, active, enabled, live, unread, pinned, extra_chats } => {
                            let pin_id = id.strip_prefix("dir:").unwrap_or(&id).to_owned();
                            let removable = serabot_desktop::can_remove_bot(&pin_id, &role);
                            let act = session.read().state.working.activity_of(&pin_id, Instant::now());
                            let act_label = act.as_ref().map(BotActivity::label).unwrap_or_default();
                            rsx! {
                                div {
                                    key: "{id}",
                                    class: if active { "thread active" } else { "thread" },
                                    "data-activity": activity_attr(act.as_ref()),
                                    oncontextmenu: {
                                        let id = id.clone();
                                        let mut menu = menu;
                                        move |event: MouseEvent| {
                                            event.prevent_default();
                                            menu.set(Some(id.clone()));
                                        }
                                    },
                                    button {
                                        class: "thread-main thread-bot",
                                        onclick: {
                                            let id = id.clone();
                                            move |_| {
                                                let bot = id.strip_prefix("dir:").unwrap_or(&id).to_owned();
                                                drawer_open.set(false);
                                                session.write().open_bot_chat(bot);
                                            }
                                        },
                                        span { class: "avatar-wrap", "data-activity": activity_attr(act.as_ref()),
                                            span { class: "avatar bot", style: conversation_avatar_style(&id), "{avatar}" }
                                        }
                                        div { class: "thread-copy",
                                            span { class: "thread-title", "{label}" }
                                            if act.is_some() {
                                                span { class: "thread-meta working-label", role: "status", "{act_label}" }
                                            }
                                            span { class: "thread-meta",
                                                if enabled { "{subtitle}" } else { "{subtitle} · disabled" }
                                            }
                                        }
                                        if extra_chats > 0 {
                                            span {
                                                class: "bot-chat-count",
                                                title: "{extra_chats + 1} chats",
                                                "{extra_chats + 1}"
                                            }
                                        }
                                        if let Some(badge) = chat_feed::unread_badge(unread) {
                                            span {
                                                class: "unread-badge",
                                                role: "img",
                                                "aria-label": "{chat_feed::unread_label(unread)}",
                                                "{badge}"
                                            }
                                        }
                                    }
                                    // Liveness dots only render for the
                                    // exception (restart required); five
                                    // identical green dots say nothing.
                                    if !live {
                                        span { class: "thread-dot off", title: "restart required" }
                                    }
                                    {
                                        let mut menu = menu;
                                        let row_id = id.clone();
                                        let open_id = id.clone();
                                        rsx! {
                                            button {
                                                class: "row-new",
                                                title: "New chat with {label}",
                                                "aria-label": "New chat with {label}",
                                                disabled: !enabled,
                                                onclick: {
                                                    let bot = pin_id.clone();
                                                    move |event| {
                                                        event.stop_propagation();
                                                        drawer_open.set(false);
                                                        session.write().new_bot_chat(bot.clone());
                                                    }
                                                },
                                                Icon { name: "plus", size: 15 }
                                            }
                                            button {
                                                class: "row-more",
                                                title: "More actions",
                                                "aria-label": "More actions for {label}",
                                                "aria-haspopup": "menu",
                                                "aria-expanded": if menu().as_deref() == Some(row_id.as_str()) { "true" } else { "false" },
                                                onclick: move |event| {
                                                    event.stop_propagation();
                                                    menu.set(Some(open_id.clone()));
                                                },
                                                Icon { name: "dots", size: 16 }
                                            }
                                            if menu().as_deref() == Some(row_id.as_str()) {
                                                RowMenu {
                                                    session,
                                                    toast,
                                                    undo,
                                                    renaming,
                                                    menu,
                                                    id: row_id.clone(),
                                                    pin_id: pin_id.clone(),
                                                    label: label.clone(),
                                                    kind: RowKind::Bot {
                                                        enabled,
                                                        removable,
                                                        needs_setup: !enabled && role != "orchestrator",
                                                    },
                                                    pinned,
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        },
                        RailItem::Thread { id, label, subtitle, depth, active, unread, archived, pinned } => {
                            let editing = renaming().as_ref().map(|(edit, _)| edit == &id).unwrap_or(false);
                            let act = session
                                .read()
                                .state
                                .working
                                .conversation_working(&id, Instant::now())
                                .then_some(BotActivity::Working(None));
                            let act_label = act.as_ref().map(BotActivity::label).unwrap_or_default();
                            rsx! {
                                div {
                                    key: "{id}",
                                    class: if active { "thread active" } else { "thread" },
                                    "data-activity": activity_attr(act.as_ref()),
                                    style: format!("--depth: {depth};"),
                                    oncontextmenu: {
                                        let id = id.clone();
                                        let mut menu = menu;
                                        move |event: MouseEvent| {
                                            event.prevent_default();
                                            menu.set(Some(id.clone()));
                                        }
                                    },
                                    if editing {
                                        div { class: "thread-rename",
                                            input {
                                                class: "input",
                                                autofocus: true,
                                                value: "{renaming().as_ref().map(|(_, text)| text.clone()).unwrap_or_default()}",
                                                oninput: move |event| {
                                                    if let Some((_, text)) = renaming.write().as_mut() {
                                                        *text = event.value();
                                                    }
                                                },
                                                onkeydown: {
                                                    let id = id.clone();
                                                    move |event: KeyboardEvent| {
                                                        if event.key() == Key::Enter {
                                                            event.prevent_default();
                                                            if let Some((_, text)) = renaming() {
                                                                if text.trim().is_empty() {
                                                                    toast.set(Some(("A name is required.".into(), true)));
                                                                } else {
                                                                    session.write().rename_conversation(id.clone(), text);
                                                                    toast.set(Some(("Renamed.".into(), false)));
                                                                }
                                                            }
                                                            renaming.set(None);
                                                        }
                                                        if event.key() == Key::Escape {
                                                            renaming.set(None);
                                                        }
                                                    }
                                                },
                                            }
                                            button {
                                                class: "icon-btn",
                                                title: "Save name",
                                                "aria-label": "Save name",
                                                onclick: {
                                                    let id = id.clone();
                                                    move |_| {
                                                        if let Some((_, text)) = renaming() {
                                                            if text.trim().is_empty() {
                                                                toast.set(Some(("A name is required.".into(), true)));
                                                            } else {
                                                                session.write().rename_conversation(id.clone(), text);
                                                                toast.set(Some(("Renamed.".into(), false)));
                                                            }
                                                        }
                                                        renaming.set(None);
                                                    }
                                                },
                                                Icon { name: "check", size: 16 }
                                            }
                                        }
                                    } else {
                                        button {
                                            class: "thread-main thread-bot",
                                            onclick: {
                                                let id = id.clone();
                                                move |_| {
                                                    drawer_open.set(false);
                                                    session.write().open_conversation(id.clone());
                                                }
                                            },
                                            span { class: "avatar-wrap", "data-activity": activity_attr(act.as_ref()),
                                                span {
                                                    class: if id.starts_with("room:") { "avatar bot group" } else { "avatar bot" },
                                                    style: conversation_avatar_style(&id),
                                                    "{initial_of(&label)}"
                                                }
                                            }
                                            div { class: "thread-copy",
                                                span { class: "thread-title", "{label}" }
                                                if act.is_some() {
                                                    span { class: "thread-meta working-label", role: "status", "{act_label}" }
                                                }
                                                span { class: "thread-meta", "{subtitle}" }
                                            }
                                            if !archived {
                                                if let Some(badge) = chat_feed::unread_badge(unread) {
                                                    span {
                                                        class: "unread-badge",
                                                        role: "img",
                                                        "aria-label": "{chat_feed::unread_label(unread)}",
                                                        "{badge}"
                                                    }
                                                }
                                            }
                                        }
                                        {
                                            let mut menu = menu;
                                            let row_id = id.clone();
                                            let open_id = id.clone();
                                            rsx! {
                                                button {
                                                    class: "row-more",
                                                    title: "More actions",
                                                    "aria-label": "More actions for {label}",
                                                    "aria-haspopup": "menu",
                                                    "aria-expanded": if menu().as_deref() == Some(row_id.as_str()) { "true" } else { "false" },
                                                    onclick: move |event| {
                                                        event.stop_propagation();
                                                        menu.set(Some(open_id.clone()));
                                                    },
                                                    Icon { name: "dots", size: 16 }
                                                }
                                                if menu().as_deref() == Some(row_id.as_str()) {
                                                    RowMenu {
                                                        session,
                                                        toast,
                                                        undo,
                                                        renaming,
                                                        menu,
                                                        id: row_id.clone(),
                                                        pin_id: row_id.clone(),
                                                        label: label.clone(),
                                                        kind: if row_id.starts_with("room:") {
                                                            RowKind::Group { archived }
                                                        } else {
                                                            RowKind::Thread { archived }
                                                        },
                                                        pinned,
                                                    }
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        },
                    }}
                }
            }
            RunsSidebar { session }
        }
    }
}

/// Server step icon names to the console's icon set.
pub(super) fn step_icon(name: &str) -> &'static str {
    match name {
        "globe" => "globe",
        "search" | "eye" => "search",
        "image" => "image",
        "file" => "folder",
        "brain" => "memory",
        "mail" => "send",
        "lock" => "lock",
        "chart" => "grid",
        "bot" => "robot",
        "mic" => "mic",
        _ => "bolt",
    }
}

/// One live step: connector name, a one-line summary, a spinner or a tick,
/// and Approve / Deny when it waits. Decisions use the approvals API, so the
/// gateway still binds and audits them. Inputs and results stay behind a
/// disclosure; secrets were masked before they reached this view.
#[component]
pub(super) fn ActivityRow(mut session: Signal<Session>, step: Step) -> Element {
    let state_class = match step.phase {
        StepPhase::Running | StepPhase::Progress => "step running",
        StepPhase::Done => "step done",
        StepPhase::Failed => "step failed",
        StepPhase::WaitingApproval => "step waiting",
    };
    let status_word = match step.phase {
        StepPhase::Running => "Running",
        StepPhase::Progress => "Still working",
        StepPhase::Done => "Done",
        StepPhase::Failed => "Failed",
        StepPhase::WaitingApproval => "Waiting for your approval",
    };
    let title = step.title.clone().unwrap_or_else(|| step.label.clone());
    let summary = (step.title.is_some() && step.label != title).then(|| step.label.clone());
    let duration = step
        .duration_ms
        .map(serabot_desktop::activity::format_duration);
    let approval = step.approval_id.clone();
    let can_retry = session.read().state.can_send();
    // Only a retryable failed step ever uses the last message: the other
    // rows of a long run skip the scan and the clone.
    let last_user = if step.phase == StepPhase::Failed && step.action == StepAction::Retry {
        session
            .read()
            .state
            .messages
            .iter()
            .rev()
            .find_map(|message| {
                (message.role == Role::User && !message.content.trim().is_empty())
                    .then(|| message.content.clone())
            })
    } else {
        None
    };
    let has_body = step.input_summary.is_some()
        || !step.cards.is_empty()
        || step.detail.is_some()
        || step.result_summary.is_some();
    let described = format!("{title}, {status_word}");
    rsx! {
        div { class: "{state_class}", role: "group", "aria-label": "{described}",
            div { class: "step-head",
                span { class: "step-status", "aria-hidden": "true",
                    match step.phase {
                        StepPhase::Running | StepPhase::Progress => rsx! { span { class: "step-spin" } },
                        StepPhase::Done => rsx! { Icon { name: "check", size: 13 } },
                        StepPhase::Failed => rsx! { Icon { name: "x", size: 13 } },
                        StepPhase::WaitingApproval => rsx! { Icon { name: "lock", size: 13 } },
                    }
                }
                span { class: "step-icon", "aria-hidden": "true",
                    if let Some(group) = step.icon.strip_prefix("file:") {
                        match file_icons::file_type_image_src(group) {
                            Some(src) => rsx! {
                                img {
                                    class: "step-file-icon",
                                    src: "{src}",
                                    width: "14",
                                    height: "14",
                                    draggable: "false",
                                    alt: "",
                                }
                            },
                            None => rsx! { Icon { name: "folder", size: 14 } },
                        }
                    } else {
                        Icon { name: step_icon(&step.icon), size: 14 }
                    }
                }
                div { class: "step-copy",
                    span { class: "step-title", "{title}" }
                    if let Some(summary) = summary.clone() {
                        span { class: "step-label", "{summary}" }
                    }
                }
                if let Some(duration) = duration.clone() {
                    span { class: "step-time", "{duration}" }
                } else if step.phase.spinning() {
                    span { class: "step-time", "Running" }
                }
            }
            if step.phase == StepPhase::WaitingApproval {
                div { class: "step-wait", role: "status", "Waiting for your approval" }
                if let Some(id) = approval {
                    span { class: "step-actions",
                        button {
                            class: "btn primary sm",
                            r#type: "button",
                            "aria-label": "Approve this step",
                            onclick: {
                                let id = id.clone();
                                move |_| session.write().decide(id.clone(), ApprovalDecision::Once)
                            },
                            "Approve"
                        }
                        button {
                            class: "btn danger sm",
                            r#type: "button",
                            "aria-label": "Deny this step",
                            onclick: move |_| session.write().decide(id.clone(), ApprovalDecision::Deny),
                            "Deny"
                        }
                    }
                }
            }
            if step.phase == StepPhase::Failed {
                if let Some(error) = step.error.clone() {
                    p { class: "step-error", role: "alert", "{error}" }
                }
                span { class: "step-actions",
                    match step.action {
                        StepAction::Retry => rsx! {
                            button {
                                class: "btn ghost sm",
                                r#type: "button",
                                disabled: !can_retry || last_user.is_none(),
                                "aria-label": "Retry this step",
                                title: if can_retry { "Send your last message again" } else { "Wait until this reply finishes" },
                                onclick: {
                                    let text = last_user.clone().unwrap_or_default();
                                    move |_| {
                                        if !text.is_empty() {
                                            session.write().regenerate(text.clone());
                                        }
                                    }
                                },
                                "Retry"
                            }
                        },
                        StepAction::Keys => rsx! {
                            button {
                                class: "btn ghost sm",
                                r#type: "button",
                                "aria-label": "Open AI providers",
                                onclick: move |_| session.write().open_section(Section::Providers),
                                "Open AI providers"
                            }
                        },
                        StepAction::Reconnect => rsx! {
                            button {
                                class: "btn ghost sm",
                                r#type: "button",
                                "aria-label": "Try the connection again",
                                onclick: move |_| session.write().retry_link(),
                                "Reconnect"
                            }
                        },
                        StepAction::None => rsx! {},
                    }
                }
            }
            if has_body {
                details { class: "step-more",
                    summary { class: "step-more-summary", "Details" }
                    if let Some(input) = step.input_summary.clone() {
                        div { class: "step-block",
                            span { class: "step-k", "Input" }
                            p { class: "step-v", "{input}" }
                        }
                    }
                    for (index, result) in step.cards.iter().cloned().enumerate() {
                        StepResultBody { key: "{index}", session, result }
                    }
                    if step.cards.is_empty()
                        && let Some(summary) = step.result_summary.clone()
                    {
                        div { class: "step-block",
                            span { class: "step-k", "Result" }
                            p { class: "step-v", "{summary}" }
                        }
                    }
                    if let Some(detail) = step.detail.clone() {
                        pre { class: "step-thinking-text", "{detail}" }
                    }
                }
            }
        }
    }
}

/// Audio, image, file, table, link, or plain text for one step result.
#[component]
fn StepResultBody(mut session: Signal<Session>, result: StepResult) -> Element {
    let link = result
        .url
        .clone()
        .filter(|url| serabot_desktop::activity::safe_link_url(url));
    let link_label = result.title.clone().or_else(|| link.clone());
    let caption = match (result.voice.clone(), result.model.clone()) {
        (Some(voice), Some(model)) => Some(format!("{voice} · {model}")),
        (Some(voice), None) => Some(voice),
        (None, Some(model)) => Some(model),
        (None, None) => None,
    };
    rsx! {
        div { class: "step-block",
            match result.kind {
                ResultKind::Audio => rsx! {
                    span { class: "step-k", "Audio" }
                    if let Some(caption) = caption.clone() {
                        span { class: "step-v", "{caption}" }
                    }
                    if let Some(hash) = result.artifact.clone() {
                        crate::ui_files::InlineMedia {
                            session,
                            hash,
                            mime: result.mime.clone().unwrap_or_else(|| "audio/mpeg".into()),
                        }
                    } else if let Some(url) = result.url.clone().filter(|url| serabot_desktop::activity::safe_media_url(url)) {
                        audio {
                            class: "media-player",
                            controls: true,
                            preload: "metadata",
                            src: "{url}",
                            "aria-label": "Audio",
                        }
                    }
                },
                ResultKind::Image => rsx! {
                    span { class: "step-k", "Image" }
                    if let Some(url) = result.url.clone().filter(|url| serabot_desktop::activity::safe_media_url(url)) {
                        img { class: "step-preview", src: "{url}", alt: "Image from this step" }
                    } else if let Some(hash) = result.artifact.clone() {
                        button {
                            class: "btn ghost sm",
                            r#type: "button",
                            "aria-label": "Open the image",
                            onclick: move |_| session.write().open_artifact(hash.clone()),
                            "Open image"
                        }
                    }
                },
                ResultKind::File => rsx! {
                    FileResult { session, result: result.clone() }
                },
                ResultKind::Table => rsx! {
                    div { class: "md-table-wrap",
                        table {
                            if !result.columns.is_empty() {
                                thead {
                                    tr {
                                        for (index, column) in result.columns.iter().cloned().enumerate() {
                                            th { key: "{index}", "{column}" }
                                        }
                                    }
                                }
                            }
                            tbody {
                                for (row_index, row) in result.rows.iter().cloned().enumerate() {
                                    tr { key: "{row_index}",
                                        for (cell_index, cell) in row.iter().cloned().enumerate() {
                                            td { key: "{cell_index}", "{cell}" }
                                        }
                                    }
                                }
                            }
                        }
                    }
                },
                ResultKind::Link => rsx! {
                    if let (Some(url), Some(label)) = (link.clone(), link_label.clone()) {
                        a {
                            class: "step-link",
                            href: "{url}",
                            target: "_blank",
                            rel: "noopener noreferrer",
                            "{label}"
                        }
                    } else if let Some(text) = result.text.clone() {
                        p { class: "step-v", "{text}" }
                    }
                },
                ResultKind::Text => rsx! {
                    if let Some(text) = result.text.clone() {
                        p { class: "step-v", "{text}" }
                    }
                },
            }
        }
    }
}

#[component]
fn FileResult(mut session: Signal<Session>, result: StepResult) -> Element {
    let name = result
        .name
        .clone()
        .or(result.title.clone())
        .unwrap_or_else(|| "File".into());
    let size = result.size.map(crate::ui_files::size_label);
    let open_hash = result.artifact.clone();
    let save_hash = result.artifact.clone();
    let link = result
        .artifact
        .is_none()
        .then(|| {
            result
                .url
                .clone()
                .filter(|url| serabot_desktop::activity::safe_link_url(url))
        })
        .flatten();
    let open_label = format!("Open {name}");
    let save_label = format!("Save {name}");
    rsx! {
        div { class: "step-file",
            span { class: "step-icon", "aria-hidden": "true",
                Icon { name: "paperclip", size: 16 }
            }
            div { class: "step-copy",
                span { class: "step-title", "{name}" }
                if let Some(size) = size {
                    span { class: "step-time", "{size}" }
                }
            }
            if let Some(hash) = open_hash {
                button {
                    class: "btn ghost sm",
                    r#type: "button",
                    "aria-label": "{open_label}",
                    onclick: move |_| session.write().open_artifact(hash.clone()),
                    "Open"
                }
            }
            if let Some(hash) = save_hash {
                button {
                    class: "btn ghost sm",
                    r#type: "button",
                    "aria-label": "{save_label}",
                    onclick: move |_| session.write().save_artifact_as(hash.clone()),
                    "Save"
                }
            }
            if let Some(url) = link {
                a {
                    class: "step-link",
                    href: "{url}",
                    target: "_blank",
                    rel: "noopener noreferrer",
                    "Open"
                }
            }
        }
    }
}

/// The goal and todo list of the chat. Collapsed to one line (goal and
/// "N of M done"); a click opens the checklist. The text is model-written and
/// rendered as plain text only.
#[component]
pub(super) fn PlanPanel(plan: serabot_desktop::chat_plan::ChatPlan) -> Element {
    use serabot_desktop::chat_plan::ItemStatus;
    let progress = plan.progress();
    let complete = plan.is_complete();
    let total = plan.items.len().max(1);
    let percent = plan.done() * 100 / total;
    rsx! {
        details { class: if complete { "plan-panel complete" } else { "plan-panel" },
            summary { class: "plan-summary", "aria-label": "{plan.goal}. {progress}",
                span { class: "plan-chevron", Icon { name: "chevron-right", size: 12 } }
                span { class: "plan-goal", title: "{plan.goal}", "{plan.goal}" }
                span { class: "plan-progress", "{progress}" }
                span { class: "plan-bar", "aria-hidden": "true",
                    span { class: "plan-bar-fill", style: "width: {percent}%" }
                }
            }
            ul { class: "plan-items", "aria-label": "Todo list",
                for (index, item) in plan.items.iter().enumerate() {
                    li {
                        key: "{index}",
                        class: match item.status {
                            ItemStatus::Done => "plan-item done",
                            ItemStatus::Doing => "plan-item doing",
                            ItemStatus::Pending => "plan-item",
                        },
                        span { class: "plan-mark",
                            match item.status {
                                ItemStatus::Done => rsx! { Icon { name: "check", size: 12 } },
                                ItemStatus::Doing => rsx! { span { class: "pill-dot live" } },
                                ItemStatus::Pending => rsx! { span { class: "plan-dot" } },
                            }
                        }
                        span { class: "plan-text", "{item.text}" }
                    }
                }
            }
        }
    }
}

/// First letter of a label, upper-cased, for avatar tiles.
pub(super) fn initial_of(label: &str) -> String {
    label
        .chars()
        .find(|c| c.is_alphanumeric())
        .map(|c| c.to_uppercase().collect())
        .unwrap_or_else(|| "C".to_owned())
}

/// "specialist" becomes "Specialist" for the one-line row subtitle.
pub(super) fn capitalise_role(role: &str) -> String {
    let role = role.trim();
    let mut chars = role.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
        None => "Bot".to_owned(),
    }
}

pub(super) enum RailItem {
    Header {
        key: String,
        title: String,
        count: usize,
        /// Set when the catalog has more bots than the user has ("N of M").
        total: Option<usize>,
        collapsed: bool,
    },
    Bot {
        id: String,
        label: String,
        role: String,
        /// One line under the name: the latest chat preview, else the role.
        subtitle: String,
        avatar: String,
        active: bool,
        enabled: bool,
        live: bool,
        unread: i64,
        pinned: bool,
        /// Chats beyond the primary one.
        extra_chats: usize,
    },
    /// "Add more bots", the quiet last row of the Bots group.
    AddBots,
    Thread {
        id: String,
        label: String,
        subtitle: String,
        depth: u8,
        active: bool,
        unread: i64,
        archived: bool,
        pinned: bool,
    },
}

/// The owner menu behind the sidebar's user chip: sections, theme and
/// disconnect live here so the rail stays a pure chat list.
#[component]
pub(super) fn UserMenu(session: Signal<Session>, toast: Signal<Option<(String, bool)>>) -> Element {
    let mut session = session;
    let mut toast = toast;
    let mut open = use_signal(|| false);
    let theme = session.read().theme;
    let offline = !session.read().state.connected;
    let (user_initial, user_label) = {
        let s = session.read();
        let label = s
            .state
            .user
            .as_ref()
            .map(|u| u.label())
            .filter(|text| !text.trim().is_empty())
            .unwrap_or_else(|| "Owner".to_owned());
        let initial = initial_of(&label);
        let full = if offline {
            format!("{label} \u{b7} offline")
        } else {
            label
        };
        (initial, full)
    };
    rsx! {
        div { class: "menu-wrap rail-account",
            button {
                class: "rail-btn rail-avatar menu-trigger",
                title: "{user_label}",
                "data-tip": if offline { "Account (offline)" } else { "Account" },
                "aria-label": if offline { "Account menu, offline" } else { "Account menu" },
                "aria-haspopup": "menu",
                "aria-expanded": if open() { "true" } else { "false" },
                onclick: move |_| open.set(!open()),
                span { class: "avatar user", "{user_initial}" }
                // Nominal connection state is silent; only the exception
                // (offline) gets a surface.
                if offline {
                    span { class: "rail-badge rail-dot", title: "Offline" }
                }
            }
            div {
                class: if open() { "menu rail-menu open" } else { "menu rail-menu" },
                onkeydown: move |event: KeyboardEvent| {
                    if event.key() == Key::Escape {
                        open.set(false);
                    }
                },
                div { class: "menu-label", "{user_label}" }
                button {
                    class: "menu-item",
                    onclick: move |_| {
                        open.set(false);
                        session.write().open_section(Section::Settings);
                    },
                    Icon { name: "settings", size: 16 }
                    "Settings"
                }
                button {
                    class: "menu-item",
                    onclick: move |_| {
                        open.set(false);
                        session.write().open_section(Section::Conversations);
                    },
                    Icon { name: "threads", size: 16 }
                    "All chats"
                }
                div { class: "menu-sep" }
                button {
                    class: "menu-item",
                    onclick: move |_| {
                        let next = theme.toggled();
                        session.write().set_theme(next);
                    },
                    Icon { name: if theme == Theme::Dark { "sun" } else { "moon" }, size: 16 }
                    if theme == Theme::Dark { "Switch to light" } else { "Switch to dark" }
                }
                button {
                    class: "menu-item danger",
                    onclick: move |_| {
                        open.set(false);
                        session.write().sign_out();
                        toast.set(Some(("Signing out…".into(), false)));
                    },
                    Icon { name: "x", size: 16 }
                    "Sign out"
                }
            }
        }
    }
}

/// The chat header: bot avatar, name and a one-line subtitle, the status
/// pill, the team stack and the Vault button. Only the chat shows it; every
/// other section carries its own large title.
#[component]
pub(super) fn Topbar(
    session: Signal<Session>,
    overlay: Signal<Overlay>,
    drawer_open: Signal<bool>,
    toast: Signal<Option<(String, bool)>>,
    undo: Signal<Option<String>>,
) -> Element {
    let mut header_menu = use_signal(|| Option::<String>::None);
    let header_rename = use_signal(|| Option::<(String, String)>::None);
    // The open chat's actions, same entries as its sidebar row.
    let header_kind = {
        let state = session.read();
        let conversation = state.conversation.clone();
        match state.conversation_bot(&conversation).as_deref() {
            _ if serabot_desktop::bot_chats::is_extra_chat_id(&conversation) => {
                Some(serabot_desktop::row_menu::RowKind::Thread { archived: false })
            }
            Some(bot) => state
                .state
                .listed_bots
                .iter()
                .find(|item| item.id == bot)
                .map(|item| serabot_desktop::row_menu::RowKind::Bot {
                    enabled: item.enabled,
                    removable: serabot_desktop::can_remove_bot(&item.id, &item.role),
                    needs_setup: !item.enabled && item.role != "orchestrator",
                }),
            None => state
                .state
                .conversations
                .iter()
                .find(|c| c.id == conversation)
                .map(|c| {
                    let archived = c.archived_at.is_some();
                    if c.id.starts_with("room:") {
                        serabot_desktop::row_menu::RowKind::Group { archived }
                    } else {
                        serabot_desktop::row_menu::RowKind::Thread { archived }
                    }
                }),
        }
    };
    let header_ids = {
        let state = session.read();
        let id = state.conversation.clone();
        let pin_id = id.strip_prefix("dir:").unwrap_or(&id).to_owned();
        let pinned = state.pins.contains(&pin_id);
        (id, pin_id, pinned)
    };
    let mut overlay = overlay;
    let mut drawer_open = drawer_open;
    let title = session.read().conversation_title();
    let active_bot = {
        let state = session.read();
        state.conversation_bot(&state.conversation)
    };
    let is_bot_chat = active_bot.is_some();
    let settings_open = session.read().bot_settings.open;
    let connected = session.read().state.connected;
    let (browser_active, browser_open) = {
        let state = session.read();
        (state.panel.session_active(), state.panel.open())
    };
    let task_status = session.read().state.task.as_ref().map(|task| task.status);
    let auto_approve = session.read().state.auto_approve;
    let auto_approve_confirming = session.read().auto_approve_confirming;
    let accepts_turns = session.read().conversation_accepts_turns();
    let (subtitle, avatar) = {
        let state = session.read();
        let summary = state
            .state
            .conversations
            .iter()
            .find(|conversation| conversation.id == state.conversation);
        let subtitle = match (&active_bot, summary) {
            (Some(bot), _) => state
                .state
                .listed_bots
                .iter()
                .find(|item| &item.id == bot)
                .map(|item| item.role.clone())
                .unwrap_or_else(|| "Bot".to_owned()),
            (None, Some(summary)) => state.conversation_subtitle(summary),
            (None, None) => "Your direct chat".to_owned(),
        };
        let avatar = match &active_bot {
            Some(bot) => state.bot_avatar(bot),
            None => initial_of(&title),
        };
        (subtitle, avatar)
    };
    // A chip only for what needs attention. "Working" shows as the live
    // status under the name; an idle bot says nothing.
    let status_pill = if !connected {
        Some(("pill danger", "offline"))
    } else if matches!(task_status, Some(TaskStatus::AwaitingApproval)) {
        Some(("pill warn", "awaiting approval"))
    } else {
        None
    };
    let mut find_open = use_context::<ChatFindOpen>().0;
    let can_search = session.read().state.messages.len() >= 2 || find_open();
    let team = {
        let state = session.read();
        let team_members = state
            .state
            .team
            .as_ref()
            .map(teams::member_ids)
            .unwrap_or_default();
        let source = if team_members.is_empty() {
            state
                .state
                .listed_bots
                .iter()
                .filter(|bot| bot.enabled)
                .map(|bot| bot.id.clone())
                .take(4)
                .collect::<Vec<_>>()
        } else {
            team_members
        };
        source
            .into_iter()
            .map(|id| (id.clone(), state.bot_avatar(&id)))
            .collect::<Vec<_>>()
    };
    // Who is working: the chat bot, or each room member. A queued or running
    // task covers the moment before the first step event arrives.
    let (header_act, team_acts) = {
        let state = session.read();
        let now = Instant::now();
        let busy = matches!(task_status, Some(TaskStatus::Running | TaskStatus::Queued));
        let header = match &active_bot {
            Some(bot) => state
                .state
                .working
                .activity_of(bot, now)
                .or_else(|| busy.then_some(BotActivity::Working(None))),
            None => (busy
                || state
                    .state
                    .working
                    .conversation_working(&state.conversation, now))
            .then_some(BotActivity::Working(None)),
        };
        let members = team
            .iter()
            .map(|(id, _)| (id.clone(), state.state.working.activity_of(id, now)))
            .collect::<HashMap<_, _>>();
        (header, members)
    };
    let header_label = header_act.as_ref().map(BotActivity::label);
    rsx! {
        header { class: "topbar chat-header",
            button {
                class: "icon-btn drawer-toggle",
                title: "Show chats",
                "aria-label": "Show chats",
                onclick: move |_| drawer_open.set(true),
                Icon { name: "list", size: 16 }
            }
            span { class: "avatar-wrap", "data-activity": activity_attr(header_act.as_ref()),
                span {
                    class: "avatar bot",
                    style: active_bot.as_deref().map(avatar_style).unwrap_or_else(|| avatar_style(&title)),
                    "{avatar}"
                }
            }
            div { class: "chat-header-copy",
                if header_rename().as_ref().is_some_and(|(edit, _)| edit == &header_ids.0) {
                    RenameField { session, toast, renaming: header_rename, id: header_ids.0.clone() }
                } else if let Some(bot) = active_bot.clone() {
                    ChatSwitcher { session, bot, title: title.clone() }
                } else {
                    strong { "{title}" }
                }
                if let Some(text) = header_label {
                    span { class: "topbar-sub topbar-status", role: "status",
                        span { class: "status-dot", "aria-hidden": "true" }
                        "{text}"
                    }
                } else {
                    span { class: "topbar-sub", "{subtitle}" }
                }
            }
            if let Some((pill_class, pill_label)) = status_pill {
                span { class: "{pill_class}", role: "status", "{pill_label}" }
            }
            div { class: "chat-header-actions",
                if let Some(bot) = active_bot.clone() {
                    {
                        let acp_on = session.read().bot_uses_claude_agent(&bot);
                        rsx! {
                            button {
                                class: if acp_on { "btn primary sm cc-mode-btn" } else { "btn ghost sm cc-mode-btn" },
                                r#type: "button",
                                title: if acp_on { "Switch back to server bot model" } else { "Run local Claude Code agent (ACP) in this bot chat" },
                                "aria-pressed": if acp_on { "true" } else { "false" },
                                onclick: move |_| session.write().toggle_bot_claude_agent(&bot),
                                Icon { name: "code", size: 13 }
                                span { if acp_on { "Claude Agent: On" } else { "Claude Agent" } }
                            }
                        }
                    }
                }
                // The member stack belongs to groups and rooms; in a one-to-one
                // chat it only repeated every enabled bot.
                if !is_bot_chat {
                  button {
                    class: "avatar-stack",
                    title: "Bots in this chat",
                    "aria-label": "Bots in this chat",
                    onclick: move |_| {
                        session.write().refresh_team();
                        overlay.set(Overlay::Team);
                    },
                    for (id, avatar) in team {
                        span {
                            key: "{id}",
                            class: "avatar-wrap sm",
                            "data-activity": activity_attr(team_acts.get(&id).and_then(|a| a.as_ref())),
                            title: team_acts.get(&id).and_then(|a| a.as_ref()).map(BotActivity::label).unwrap_or_default(),
                            span { class: "avatar sm bot", style: avatar_style(&id), "{avatar}" }
                        }
                    }
                }
                }
                if can_search {
                    button {
                        class: "icon-btn topbar-search",
                        r#type: "button",
                        title: "Search in conversation (Ctrl+F)",
                        "aria-label": "Search in conversation",
                        "aria-keyshortcuts": "Control+F",
                        "aria-pressed": if find_open() { "true" } else { "false" },
                        onclick: move |_| {
                            if find_open() {
                                find_open.set(false);
                            } else {
                                find_open.set(true);
                                let _ = dioxus::document::eval(
                                    "requestAnimationFrame(() => { const el = document.getElementById('chat-find-input'); if (el) el.focus(); })",
                                );
                            }
                        },
                        Icon { name: "search", size: 16 }
                    }
                }
                button {
                    class: "icon-btn",
                    title: "Reload",
                    "aria-label": "Reload",
                    onclick: move |_| session.write().refresh(),
                    Icon { name: "refresh", size: 16 }
                }
                if accepts_turns && connected {
                    div { class: "yolo-wrap",
                        button {
                            class: if auto_approve { "btn sm yolo-btn active" } else { "btn ghost sm yolo-btn" },
                            title: "Auto-approve: when on, this chat's bots run actions that normally ask you first (sending email, uploading or deleting files) without waiting for your OK. Policy checks and the audit log still apply. Off by default; applies to this chat only.",
                            "aria-label": "Auto-approve in this chat",
                            "aria-pressed": if auto_approve { "true" } else { "false" },
                            onclick: move |_| session.write().request_auto_approve(!auto_approve),
                            Icon { name: "bolt", size: 15 }
                            span { class: "btn-label", "Auto-approve" }
                        }
                        if auto_approve_confirming && !auto_approve {
                            div { class: "yolo-confirm", role: "alertdialog", "aria-label": "Turn on Auto-approve?",
                                p { class: "yolo-confirm-text",
                                    "Auto-approve lets this chat's bots send email, upload and delete files without asking you first. Policy checks and the audit log still apply."
                                }
                                div { class: "yolo-confirm-actions",
                                    button {
                                        class: "btn ghost sm",
                                        r#type: "button",
                                        onclick: move |_| session.write().cancel_auto_approve(),
                                        "Cancel"
                                    }
                                    button {
                                        class: "btn primary sm",
                                        r#type: "button",
                                        onclick: move |_| session.write().confirm_auto_approve(),
                                        "Turn on"
                                    }
                                }
                            }
                        }
                    }
                }
                if browser_active {
                    button {
                        class: if browser_open { "btn sm watch-btn active" } else { "btn sm watch-btn" },
                        title: if browser_open { "Hide the browser panel" } else { "Watch the bot's browser live" },
                        "aria-label": "Watch browser",
                        "aria-pressed": if browser_open { "true" } else { "false" },
                        onclick: move |_| {
                            if browser_open {
                                session.write().panel.hide();
                            } else {
                                session.write().panel.show();
                            }
                        },
                        Icon { name: "globe", size: 15 }
                        span { class: "btn-label", "Watch browser" }
                    }
                }
                if let Some(kind) = header_kind.clone() {
                    div { class: "header-more",
                        button {
                            class: "icon-btn",
                            title: "More actions for this chat",
                            "aria-label": "More actions for this chat",
                            "aria-haspopup": "menu",
                            "aria-expanded": if header_menu().is_some() { "true" } else { "false" },
                            onclick: {
                                let id = header_ids.0.clone();
                                move |_| header_menu.set(Some(id.clone()))
                            },
                            Icon { name: "dots", size: 16 }
                        }
                        if header_menu().is_some() {
                            RowMenu {
                                session,
                                toast,
                                undo,
                                renaming: header_rename,
                                menu: header_menu,
                                id: header_ids.0.clone(),
                                pin_id: header_ids.1.clone(),
                                label: title.clone(),
                                kind,
                                pinned: header_ids.2,
                                header: true,
                            }
                        }
                    }
                }
                button {
                    class: if settings_open { "btn sm details-btn active" } else { "btn ghost sm details-btn" },
                    title: if is_bot_chat { "Change this bot's name, picture and abilities" } else { "Members and files" },
                    "aria-label": if is_bot_chat { "Bot settings" } else { "Chat settings" },
                    "aria-pressed": if settings_open { "true" } else { "false" },
                    onclick: move |_| {
                        if session.read().bot_settings.open {
                            session.write().close_bot_settings();
                        } else {
                            session.write().open_settings_for_conversation();
                        }
                    },
                    Icon { name: "panel", size: 15 }
                    span { class: "btn-label", if is_bot_chat { "Bot settings" } else { "Settings" } }
                }
            }
        }
    }
}
#[component]
pub(super) fn StatusBanner(session: Signal<Session>) -> Element {
    let error = session.read().state.error.clone();
    let unknown = session.read().state.unknown_outcome;
    let admin_error = session.read().admin_error.clone();
    let admin_notice = session.read().admin_notice.clone();
    let restart_pending = session.read().restart_pending;
    let restarting = session.read().restarting;
    let can_restart = session.read().can_admin();
    let task = session.read().state.task.as_ref().map(|task| {
        (
            task.status,
            task.status.label().to_owned(),
            task.bot.clone(),
            task.id.clone(),
        )
    });
    let error_view = error.map(|message| {
        rsx! {
            div { class: "banner err", role: "alert",
                span { class: "grow", "{message}" }
                button {
                    class: "btn icon ghost sm banner-close",
                    r#type: "button",
                    title: "Dismiss",
                    "aria-label": "Dismiss error",
                    onclick: move |_| session.write().state.error = None,
                    Icon { name: "x", size: 13 }
                }
            }
        }
    });
    let credential_view = session.read().credential_save_failed.then(|| {
        rsx! {
            div { class: "banner warn", role: "alert",
                span {
                    "Signed in for this run only. Windows Credential Manager would not save your session, so you will need to sign in again after SeraBot restarts. "
                }
                button {
                    class: "btn ghost sm",
                    onclick: move |_| session.write().retry_save_credentials(),
                    "Try saving again"
                }
            }
        }
    });
    let unknown_view = unknown.then(|| {
        rsx! {
            div { class: "banner warn",
                span { "A previous action has an unknown outcome. Refresh reads before acting again." }
            }
        }
    });
    // The task banner exists for actionable states only. A "Done" row pinned
    // above the conversation is nominal state rendered as chrome; the per-turn
    // status in the message list already says it. The UUID never leaves the
    // Inspector.
    let task_view = task
        .filter(|(status, _, _, _)| {
            matches!(status, TaskStatus::AwaitingApproval | TaskStatus::Failed)
        })
        .map(|(status, label, bot, _id)| {
            rsx! {
                div { class: if status == TaskStatus::Failed { "banner err" } else { "banner" },
                    span { class: "chip", "{label}" }
                    span { class: "tiny", "{bot}" }
                    if status == TaskStatus::AwaitingApproval {
                        button {
                            class: "btn ghost sm",
                            onclick: move |_| session.write().open_approval_conversation(),
                            "Review approvals"
                        }
                    }
                }
            }
        });
    let admin_error_view = admin_error.map(|message| {
        rsx! {
            div { class: "banner err", role: "alert",
                span { class: "grow", "{message}" }
                button {
                    class: "btn icon ghost sm banner-close",
                    r#type: "button",
                    title: "Dismiss",
                    "aria-label": "Dismiss notice",
                    onclick: move |_| session.write().admin_error = None,
                    Icon { name: "x", size: 13 }
                }
            }
        }
    });
    let admin_notice_view = admin_notice.map(|message| {
        rsx! {
            div { class: "banner",
                span { class: "grow", "{message}" }
                if restart_pending {
                    button {
                        class: "btn primary sm",
                        disabled: !can_restart,
                        onclick: move |_| session.write().request_restart(),
                        "Restart server and apply"
                    }
                }
                button {
                    class: "btn icon ghost sm banner-close",
                    r#type: "button",
                    title: "Dismiss",
                    "aria-label": "Dismiss notice",
                    onclick: move |_| session.write().admin_notice = None,
                    Icon { name: "x", size: 13 }
                }
            }
        }
    });
    let link_down = session.read().link_down && !restarting;
    let link_view = link_down.then(|| {
        rsx! {
            div { class: "banner warn", role: "status",
                span { "Can't reach SeraBot right now. Retrying…" }
                button {
                    class: "btn ghost sm",
                    onclick: move |_| session.write().retry_link(),
                    "Try again"
                }
            }
        }
    });
    let restarting_view = restarting.then(|| {
        rsx! {
            div { class: "banner warn",
                span { "The server is restarting. Reconnecting automatically…" }
                button {
                    class: "btn ghost sm",
                    onclick: move |_| session.write().connect_now(),
                    "Reconnect now"
                }
            }
        }
    });
    rsx! {
        {admin_error_view}
        {admin_notice_view}
        {restarting_view}
        {link_view}
        {error_view}
        {credential_view}
        {unknown_view}
        {task_view}
    }
}

pub(super) fn format_size(bytes: u64) -> String {
    const MB: f64 = 1024.0 * 1024.0;
    if bytes as f64 >= MB {
        format!("{:.1} MB", bytes as f64 / MB)
    } else {
        format!("{} KB", bytes.div_ceil(1024))
    }
}

#[component]
pub(super) fn UpdateBanner(session: Signal<Session>) -> Element {
    let mut dismissed = use_signal(|| false);
    let (info, update_error) = {
        let s = session.read();
        (s.update.info.clone(), s.update.error.clone())
    };
    let Some(info) = info else {
        return rsx! {};
    };
    if dismissed() {
        return rsx! {};
    }
    let version = info.version.to_string();
    let size = format_size(info.size);
    let notes = info.notes.clone();
    let install = info.clone();
    rsx! {
        div { class: "banner",
            span { strong { "Update v{version} available" } span { class: "tiny", " · {size}" } }
            div { class: "row",
                button {
                    class: "btn primary sm",
                    onclick: move |_| {
                        let result = session.write().apply_update(&install);
                        match result {
                            Ok(()) => std::process::exit(0),
                            Err(error) => session.write().update.error = Some(error),
                        }
                    },
                    "Install and restart"
                }
                button {
                    class: "btn ghost sm",
                    onclick: move |_| dismissed.set(true),
                    "Later"
                }
            }
            if let Some(err) = update_error {
                p { class: "tiny error-text", role: "alert", "{err}" }
            }
            details {
                summary { "Release notes" }
                p { class: "tiny", "{notes}" }
            }
        }
    }
}

/// Moves the caret to the composer (after a suggestion fills it).
pub(super) fn focus_composer() {
    let _ = dioxus::document::eval("document.getElementById('composer-input')?.focus();");
}

#[cfg(test)]
mod tests {
    use super::capitalise_role;

    #[test]
    fn row_subtitles_capitalise_the_role_and_never_come_back_empty() {
        assert_eq!(capitalise_role("specialist"), "Specialist");
        assert_eq!(capitalise_role(" orchestrator "), "Orchestrator");
        assert_eq!(capitalise_role("élite"), "Élite");
        assert_eq!(capitalise_role(""), "Bot");
    }
}
