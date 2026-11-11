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
    let onboarding_open = chat && session.read().onboarding_ui.open;
    let settings_open = session.read().bot_settings.open;
    // One stylesheet defines every avatar image; tiles only reference it.
    let avatar_css = serabot_desktop::avatars::stylesheet(&session.read().state.avatars);

    rsx! {
        style { "{avatar_css}" }
        div { class: "app-shell",
            IconRail { session, toast }
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
                        // Keyed by conversation: switching chats remounts the
                        // view, so scroll position and unseen counts start fresh.
                        Section::Chat => rsx! {
                            ChatView { key: "{session.read().conversation}", session, overlay }
                        },
                        Section::Conversations => rsx! { ConversationsView { session } },
                        Section::Memory => rsx! { MemoryView { session } },
                        Section::Providers => rsx! { ProvidersView { session, overlay } },
                        Section::Connectors => rsx! { ConnectorsView { session } },
                        Section::Approvals => rsx! { ApprovalsView { session } },
                        Section::Browser => rsx! { BrowserView { session, overlay } },

                        Section::Tasks => rsx! { crate::ui_tasks::TasksView { session } },
                        Section::Vault => rsx! { VaultPage { session } },
                        Section::Usage => rsx! { UsagePage { session } },
                        Section::Logs => rsx! { crate::ui_logs::LogsView { session } },
                        Section::Workflows => rsx! { crate::ui_workflows::WorkflowsView { session } },
                        Section::Settings => rsx! { SettingsView { session } },
                    }}
                }
                }
            }
            if chat {
                div {
                    class: if settings_open { "scrim inspector-scrim open" } else { "scrim inspector-scrim" },
                    onclick: move |_| session.write().close_bot_settings(),
                }
                SettingsPanel { session, overlay }
            }
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
                Overlay::Team => rsx! {
                    TeamSheet { session, overlay }
                },
                Overlay::MeetTeam => rsx! {
                    MeetTeamSheet { session, overlay }
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

/// The 58 px icon rail: primary destinations on top, Settings and the account
/// at the bottom. Labels live in tooltips that show on hover and on keyboard
/// focus; the count on Approvals is the number of pending requests.
#[component]
pub(super) fn IconRail(session: Signal<Session>, toast: Signal<Option<(String, bool)>>) -> Element {
    let mut session = session;
    let section = session.read().section;
    let pending = session.read().state.approvals.len();
    let destinations: [(Section, &str, &str); 11] = [
        (Section::Chat, "chat", "Chats"),
        (Section::Browser, "globe", "Browser"),
        (Section::Tasks, "list", "Tasks"),
        (Section::Approvals, "approvals", "Approvals"),
        (Section::Memory, "memory", "Memory"),
        (Section::Connectors, "connectors", "Connectors"),
        (Section::Vault, "lock", "Vault"),
        (Section::Usage, "usage", "Usage"),
        (Section::Workflows, "workflows", "Workflows"),
        (Section::Providers, "providers", "AI keys"),
        (Section::Logs, "logs", "Logs"),
    ];
    rsx! {
        nav { class: "rail", "aria-label": "Main navigation",
            for (target, icon, label) in destinations {
                button {
                    key: "{label}",
                    class: if section == target { "rail-btn labeled on" } else { "rail-btn labeled" },
                    "aria-label": if target == Section::Approvals && pending > 0 { format!("{label}, {pending} pending") } else { label.to_owned() },
                    "aria-current": if section == target { "page" } else { "false" },
                    onclick: move |_| session.write().open_section(target),
                    Icon { name: icon, size: 18 }
                    span { class: "rail-label", "{label}" }
                    if target == Section::Approvals && pending > 0 {
                        span { class: "rail-badge", if pending > 99 { "99+" } else { "{pending}" } }
                    }
                }
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
                    "aria-haspopup": "menu",
                    "aria-expanded": if open() { "true" } else { "false" },
                    onclick: move |_| open.set(!open()),
                    Icon { name: "plus", size: 14 }
                    "New chat"
                }
            } else {
                button {
                    class: "icon-btn primary menu-trigger",
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
                    onclick: move |_| {
                        open.set(false);
                        overlay.set(Overlay::NewGroup);
                    },
                    Icon { name: "shield", size: 16 }
                    "New group chat"
                }
                button {
                    class: "menu-item",
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
                    onclick: move |_| {
                        open.set(false);
                        overlay.set(Overlay::NewBot);
                    },
                    Icon { name: "robot", size: 16 }
                    "New bot"
                }
                button {
                    class: "menu-item",
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
    let mut toast = toast;
    let mut search = use_signal(String::new);
    let mut collapsed = use_signal(HashSet::<String>::new);
    let mut renaming = use_signal(|| Option::<(String, String)>::None);
    // Which row's "more actions" menu is open (by row id).
    let menu = use_signal(|| Option::<String>::None);
    // A bot's chat list: the owner's explicit open or closed choice, and the
    // bots whose whole list is shown instead of the first five.
    let mut chats_open = use_signal(HashMap::<String, bool>::new);
    let mut chats_all = use_signal(HashSet::<String>::new);
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

        let bot_rows = bots
            .iter()
            .filter(|bot| {
                matches(
                    &state.bot_display_name(&bot.id),
                    &format!("bot {}", bot.role),
                )
            })
            .collect::<Vec<_>>();
        if needle.is_empty() || !bot_rows.is_empty() {
            let key = "group:bots".to_owned();
            let is_collapsed = collapsed().contains(&key);
            items.push(RailItem::Header {
                key: key.clone(),
                title: "Bots".into(),
                count: bots.len(),
                collapsed: is_collapsed,
            });
            if !is_collapsed {
                for bot in bot_rows {
                    let id = format!("dir:{}", bot.id);
                    let unread = conversations
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
                    let extra = chats.iter().filter(|chat| !chat.primary).count();
                    let has_active = chats.iter().any(|chat| chat.id == current);
                    // Open when the reader chose so; else when an extra chat
                    // exists and one of them is the open chat.
                    let open = chats_open()
                        .get(&bot.id)
                        .copied()
                        .unwrap_or(extra > 0 && has_active);
                    items.push(RailItem::Bot {
                        active: id == current,
                        pinned: pins.contains(&bot.id),
                        id,
                        label: state.bot_display_name(&bot.id),
                        role: bot.role.clone(),
                        avatar: state.bot_avatar(&bot.id),
                        enabled: bot.enabled,
                        live: bot.live,
                        unread,
                        extra_chats: extra,
                        chats_open: open,
                    });
                    if open && extra > 0 {
                        let all = chats_all().contains(&bot.id);
                        let (shown, hidden) =
                            serabot_desktop::bot_chats::visible_chats(&chats, all, &current);
                        for chat in shown {
                            let chat_unread = conversations
                                .iter()
                                .find(|conversation| conversation.id == chat.id)
                                .map(|conversation| conversation.unread_count)
                                .unwrap_or(0);
                            items.push(RailItem::BotChat {
                                id: chat.id.clone(),
                                title: serabot_desktop::bot_chats::chat_title(chat),
                                primary: chat.primary,
                                active: chat.id == current,
                                unread: chat_unread,
                            });
                        }
                        if hidden > 0 || all {
                            items.push(RailItem::ShowMore {
                                bot: bot.id.clone(),
                                hidden,
                                all,
                            });
                        }
                    }
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
                NewMenu { session, overlay, labelled: true }
            }
            div { class: "sidebar-search",
                span { class: "search-glyph", Icon { name: "search", size: 15 } }
                input {
                    class: "input",
                    id: "global-search",
                    value: "{search}",
                    placeholder: "Search chats",
                    "aria-label": "Search chats and bots",
                    oninput: move |event| search.set(event.value()),
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
                        RailItem::Header { key, title, count, collapsed: closed } => {
                            let toggle = key.clone();
                            rsx! {
                                button {
                                    key: "{key}",
                                    class: "thread-group",
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
                                    Icon { name: if closed { "chevron-right" } else { "chevron-down" }, size: 14 }
                                    span { class: "thread-title", "{title}" }
                                    span { class: "thread-meta", "{count}" }
                                }
                            }
                        },
                        RailItem::Bot { id, label, role, avatar, active, enabled, live, unread, pinned, extra_chats, chats_open: is_open } => {
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
                                    {
                                        let toggle_bot = pin_id.clone();
                                        rsx! {
                                            button {
                                                class: if extra_chats > 0 { "row-chevron" } else { "row-chevron placeholder" },
                                                title: if is_open { "Hide this bot\u{2019}s chats" } else { "Show this bot\u{2019}s chats" },
                                                "aria-label": if is_open { "Hide chats of {label}" } else { "Show chats of {label}" },
                                                "aria-expanded": if is_open { "true" } else { "false" },
                                                disabled: extra_chats == 0,
                                                onclick: move |event| {
                                                    event.stop_propagation();
                                                    chats_open.write().insert(toggle_bot.clone(), !is_open);
                                                },
                                                Icon { name: if is_open { "chevron-down" } else { "chevron-right" }, size: 14 }
                                            }
                                        }
                                    }
                                    button {
                                        class: "thread-main thread-bot",
                                        onclick: {
                                            let id = id.clone();
                                            move |_| {
                                                let bot = id.strip_prefix("dir:").unwrap_or(&id).to_owned();
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
                                            span { class: "thread-meta", if enabled { "{role}" } else { "{role} · disabled" } }
                                        }
                                        if unread > 0 {
                                            span { class: "unread-badge", "{unread}" }
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
                        RailItem::BotChat { id, title, primary, active, unread } => {
                            let editing = renaming().as_ref().map(|(edit, _)| edit == &id).unwrap_or(false);
                            let open_id = id.clone();
                            let row_id = id.clone();
                            let pinned = session.read().pins.contains(&id);
                            rsx! {
                                div {
                                    key: "{id}",
                                    class: if active { "thread chat-row active" } else { "thread chat-row" },
                                    oncontextmenu: {
                                        let id = id.clone();
                                        let mut menu = menu;
                                        move |event: MouseEvent| {
                                            event.prevent_default();
                                            if !primary {
                                                menu.set(Some(id.clone()));
                                            }
                                        }
                                    },
                                    if editing {
                                        RenameField { session, toast, renaming, id: id.clone() }
                                    } else {
                                        button {
                                            class: "thread-main",
                                            "aria-current": if active { "true" } else { "false" },
                                            onclick: {
                                                let id = id.clone();
                                                move |_| session.write().open_conversation(id.clone())
                                            },
                                            div { class: "thread-copy",
                                                span { class: if unread > 0 { "thread-title unread" } else { "thread-title" }, "{title}" }
                                            }
                                            if unread > 0 {
                                                span { class: "unread-badge", "{unread}" }
                                            }
                                        }
                                        if !primary {
                                            {
                                                let mut menu = menu;
                                                rsx! {
                                                    button {
                                                        class: "row-more",
                                                        title: "More actions",
                                                        "aria-label": "More actions for {title}",
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
                                                            label: title.clone(),
                                                            kind: RowKind::Thread { archived: false },
                                                            pinned,
                                                        }
                                                    }
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        },
                        RailItem::ShowMore { bot, hidden, all } => {
                            rsx! {
                                button {
                                    key: "more-{bot}",
                                    class: "thread-group chat-more",
                                    onclick: move |_| {
                                        let mut set = chats_all.write();
                                        if all {
                                            set.remove(&bot);
                                        } else {
                                            set.insert(bot.clone());
                                        }
                                    },
                                    if all { "Show fewer" } else { "Show {hidden} more" }
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
                                                move |_| session.write().open_conversation(id.clone())
                                            },
                                            span { class: "avatar-wrap", "data-activity": activity_attr(act.as_ref()),
                                                span { class: "avatar bot", style: conversation_avatar_style(&id), "{initial_of(&label)}" }
                                            }
                                            div { class: "thread-copy",
                                                span { class: "thread-title", "{label}" }
                                                if act.is_some() {
                                                    span { class: "thread-meta working-label", role: "status", "{act_label}" }
                                                }
                                                span { class: "thread-meta", "{subtitle}" }
                                            }
                                            if !archived && unread > 0 {
                                                span { class: "unread-badge", "{unread}" }
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
                                                        kind: RowKind::Thread { archived },
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

/// One live step: status icon, plain-language label, and inline Approve and
/// Deny when it waits on the owner. Decisions use the approvals API, so the
/// gateway still binds and audits them.
#[component]
pub(super) fn ActivityRow(mut session: Signal<Session>, step: Step) -> Element {
    let state_class = match step.phase {
        StepPhase::Running => "step running",
        StepPhase::Done => "step done",
        StepPhase::Failed => "step failed",
        StepPhase::WaitingApproval => "step waiting",
    };
    let status_icon = match step.phase {
        StepPhase::Done => "check",
        StepPhase::Failed => "x",
        StepPhase::Running => "",
        StepPhase::WaitingApproval => "lock",
    };
    let approval = step.approval_id.clone();
    rsx! {
        div { class: "{state_class}",
            span { class: "step-status",
                if status_icon.is_empty() {
                    span { class: "pill-dot live" }
                } else {
                    Icon { name: status_icon, size: 13 }
                }
            }
            span { class: "step-icon",
                // File steps carry the real file-type icon (vscode-icons):
                // "file:pdf" renders the vendored PDF glyph, anything else
                // falls back to the console's folder icon.
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
            if let Some(detail) = step.detail.clone() {
                // The reasoning text a model returned: collapsed, plain text.
                details { class: "step-thinking",
                    summary { class: "step-label", "{step.label}" }
                    pre { class: "step-thinking-text", "{detail}" }
                }
            } else {
                span { class: "step-label", "{step.label}" }
            }
            if let Some(id) = approval {
                span { class: "step-actions",
                    button {
                        class: "btn primary sm",
                        onclick: {
                            let id = id.clone();
                            move |_| session.write().decide(id.clone(), ApprovalDecision::Once)
                        },
                        "Approve"
                    }
                    button {
                        class: "btn danger sm",
                        onclick: move |_| session.write().decide(id.clone(), ApprovalDecision::Deny),
                        "Deny"
                    }
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
            summary { class: "plan-summary",
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

pub(super) enum RailItem {
    Header {
        key: String,
        title: String,
        count: usize,
        collapsed: bool,
    },
    Bot {
        id: String,
        label: String,
        role: String,
        avatar: String,
        active: bool,
        enabled: bool,
        live: bool,
        unread: i64,
        pinned: bool,
        /// Chats beyond the primary one, and whether the list is open.
        extra_chats: usize,
        chats_open: bool,
    },
    BotChat {
        id: String,
        title: String,
        primary: bool,
        active: bool,
        unread: i64,
    },
    ShowMore {
        bot: String,
        hidden: usize,
        all: bool,
    },
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
    rsx! {
        div { class: "menu-wrap rail-account",
            button {
                class: "rail-btn rail-avatar menu-trigger",
                "data-tip": if offline { "Account (offline)" } else { "Account" },
                "aria-label": if offline { "Account menu, offline" } else { "Account menu" },
                "aria-haspopup": "menu",
                "aria-expanded": if open() { "true" } else { "false" },
                onclick: move |_| open.set(!open()),
                span { class: "avatar user", "Y" }
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
                div { class: "menu-label", if offline { "Owner · offline" } else { "Owner" } }
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
                .map(|c| serabot_desktop::row_menu::RowKind::Thread {
                    archived: c.archived_at.is_some(),
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
    let (pill_class, pill_label) = if !connected {
        ("pill danger", "offline")
    } else {
        match task_status {
            Some(TaskStatus::Running | TaskStatus::Queued) => ("pill ok", "working"),
            Some(TaskStatus::AwaitingApproval) => ("pill warn", "awaiting approval"),
            _ => ("pill", "idle"),
        }
    };
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
                strong { "{title}" }
                if let Some(text) = header_label {
                    span { class: "working-label", role: "status", "{text}" }
                } else {
                    span { "{subtitle}" }
                }
            }
            span { class: "{pill_class}", role: "status",
                if pill_label == "working" {
                    span { class: "pill-dot live" }
                }
                "{pill_label}"
            }
            div { class: "chat-header-actions",
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
                button {
                    class: "icon-btn",
                    title: "Reload",
                    "aria-label": "Reload",
                    onclick: move |_| session.write().refresh(),
                    Icon { name: "refresh", size: 16 }
                }
                if accepts_turns && connected {
                    button {
                        class: if auto_approve { "btn sm yolo-btn active" } else { "btn ghost sm yolo-btn" },
                        title: "Auto-approve: when on, this chat's bots run actions that normally ask you first (sending email, uploading or deleting files) without waiting for your OK. Policy checks and the audit log still apply. Off by default; applies to this chat only.",
                        "aria-label": "Auto-approve in this chat",
                        "aria-pressed": if auto_approve { "true" } else { "false" },
                        onclick: move |_| session.write().set_auto_approve(!auto_approve),
                        Icon { name: "bolt", size: 15 }
                        span { class: "btn-label", "Auto-approve" }
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
            div { class: "banner err", span { "{message}" } }
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
                            onclick: move |_| session.write().open_section(Section::Approvals),
                            "Review approvals"
                        }
                    }
                }
            }
        });
    let admin_error_view = admin_error.map(|message| {
        rsx! {
            div { class: "banner err",
                span { "{message}" }
            }
        }
    });
    let admin_notice_view = admin_notice.map(|message| {
        rsx! {
            div { class: "banner",
                span { "{message}" }
                if restart_pending {
                    button {
                        class: "btn primary sm",
                        disabled: !can_restart,
                        onclick: move |_| session.write().request_restart(),
                        "Restart server and apply"
                    }
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
    let info = session.read().update.info.clone();
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
