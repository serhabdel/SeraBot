//! Dioxus Desktop shell. Windows-only; the Rust session owns all network work.

#![cfg_attr(not(windows), allow(dead_code))]

use dioxus::prelude::*;
use serabot_desktop::{
    ApprovalDecision, ConversationKind, Role, SPECIALIST_TOOLS, Selection, TaskStatus,
    parse_skill_list,
    session::{Section, Session, Theme},
};
use std::collections::HashSet;

const CSS: &str = include_str!("../assets/app.css");

fn stamp(raw: &str) -> String {
    raw.chars().take(16).collect::<String>().replace('T', " ")
}

const NAV: [(Section, &str, &str); 7] = [
    (Section::Chat, "Chat", "chat"),
    (Section::Conversations, "Threads", "threads"),
    (Section::Memory, "Memory", "memory"),
    (Section::Providers, "Providers", "providers"),
    (Section::Connectors, "Connectors", "connectors"),
    (Section::Approvals, "Approvals", "approvals"),
    (Section::Fleet, "Fleet", "fleet"),
];

#[derive(Clone)]
struct BotCard {
    id: String,
    role: String,
    enabled: bool,
    live: bool,
    display_name: String,
    description: String,
    avatar: String,
    model: Option<String>,
    tools: Vec<String>,
    skills: Vec<String>,
    require_approval: Vec<String>,
}

fn avatar_hue(id: &str) -> u16 {
    let mut hash: u32 = 0;
    for byte in id.bytes() {
        hash = hash.wrapping_mul(31).wrapping_add(u32::from(byte));
    }
    (hash % 360) as u16
}

fn avatar_style(id: &str) -> String {
    format!(
        "--avatar-hue: {}; --avatar-a: hsl({} 58% 52%); --avatar-b: hsl({} 62% 38%);",
        avatar_hue(id),
        avatar_hue(id),
        (avatar_hue(id) + 28) % 360
    )
}

fn human_model(model: &str) -> String {
    let base = model.strip_suffix("-latest").unwrap_or(model);
    base.split(['-', '_'])
        .filter(|word| !word.is_empty())
        .map(|word| {
            let mut chars = word.chars();
            match chars.next() {
                Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn human_provider(provider: &str) -> String {
    if provider == "default" {
        "Default provider".into()
    } else {
        human_model(provider)
    }
}

fn capability_label(tool: &str) -> Option<&'static str> {
    match tool {
        "deck_build" => Some("Slide decks"),
        "xlsx_analyze" | "timesheet_analyze" => Some("Spreadsheet analysis"),
        "rag_search" | "document_index" => Some("Document search"),
        "fetch_url" | "watch_now" | "firecrawl_scrape" | "firecrawl_map" | "firecrawl_search"
        | "firecrawl_crawl" => Some("Web research"),
        "audio_transcribe" | "audio_understand" => Some("Audio notes"),
        "elevenlabs_text_to_speech"
        | "elevenlabs_sound_effect"
        | "elevenlabs_transcribe"
        | "elevenlabs_generate_image"
        | "elevenlabs_generate_video" => Some("Voice and media"),
        "onedrive_upload" | "onedrive_list" => Some("OneDrive files"),
        "skill_list" | "skill_search" | "skill_load" => Some("Skills"),
        "delegate" => Some("Team handoffs"),
        _ => None,
    }
}

pub fn run() {
    #[cfg(windows)]
    {
        let config = dioxus::desktop::Config::new()
            .with_menu(Option::<dioxus::desktop::muda::Menu>::None)
            .with_window(
                dioxus::desktop::WindowBuilder::new()
                    .with_title("SeraBot Owner")
                    .with_inner_size(dioxus::desktop::LogicalSize::new(1280.0, 820.0))
                    .with_min_inner_size(dioxus::desktop::LogicalSize::new(920.0, 620.0)),
            );
        dioxus::LaunchBuilder::desktop()
            .with_cfg(config)
            .launch(App);
    }
}

#[component]
fn App() -> Element {
    let mut session = use_signal(Session::new);
    let mut revision = use_signal(|| 0u64);
    use_coroutine(move |_: UnboundedReceiver<()>| async move {
        loop {
            #[cfg(windows)]
            tokio::time::sleep(std::time::Duration::from_millis(120)).await;
            #[cfg(not(windows))]
            std::future::pending::<()>().await;
            session.with_mut(|session| session.tick());
            revision.set(revision() + 1);
        }
    });
    let connected = session.read().state.connected;
    let restarting = session.read().restarting;
    let theme = session.read().theme.as_str();
    let _revision = revision();
    rsx! {
        style { {CSS} }
        div { class: "root", "data-theme": theme,
            if connected || restarting {
                Shell { session }
            } else {
                ConnectView { session }
            }
        }
    }
}

#[component]
fn Icon(name: String) -> Element {
    let path = match name.as_str() {
        "chat" => "M4 5h16v11H8l-4 4V5z",
        "threads" => "M4 5h11v9H4zM9 9h11v9H9z",
        "memory" => "M12 3c-4 0-7 2-7 5s3 5 7 5 7-2 7-5-3-5-7-5zM5 8v8c0 3 3 5 7 5s7-2 7-5V8",
        "providers" => "M4 6h16M4 12h16M4 18h10",
        "connectors" => "M8 12a4 4 0 1 1 4 4M16 12a4 4 0 1 1-4-4",
        "approvals" => "M12 3a9 9 0 1 0 0 18 9 9 0 0 0 0-18zM8 12l3 3 5-6",
        "fleet" => "M4 4h6v6H4zM14 4h6v6h-6zM4 14h6v6H4zM14 14h6v6h-6z",
        "settings" => {
            "M12 8a4 4 0 1 0 0 8 4 4 0 0 0 0-8zM12 2v3M12 19v3M2 12h3M19 12h3M5 5l2 2M17 17l2 2M19 5l-2 2M7 17l-2 2"
        }
        "theme" => {
            "M12 4v2M12 18v2M4 12h2M18 12h2M6.3 6.3l1.4 1.4M16.3 16.3l1.4 1.4M17.7 6.3l-1.4 1.4M7.7 16.3l-1.4 1.4M16 12a4 4 0 1 1-8 0 4 4 0 0 1 8 0z"
        }
        "send" => "M4 12l16-8-6 16-2-6-8-2z",
        "refresh" => {
            "M4 12a8 8 0 0 1 13.5-5.8L20 8M20 4v4h-4M20 12a8 8 0 0 1-13.5 5.8L4 16M4 20v-4h4"
        }
        _ => "M5 5h14v14H5z",
    };
    rsx! {
        svg {
            "viewBox": "0 0 24 24",
            fill: "none",
            stroke: "currentColor",
            "stroke-width": "1.7",
            "stroke-linecap": "round",
            "stroke-linejoin": "round",
            path { d: "{path}" }
        }
    }
}

#[component]
fn ConnectView(session: Signal<Session>) -> Element {
    let server = session.read().server.clone();
    let token = session.read().token.clone();
    let error = session.read().connect_error.clone();
    let connecting = session.read().connecting;
    let theme = session.read().theme;
    let version = env!("CARGO_PKG_VERSION");
    rsx! {
        div { class: "connect",
            div { class: "connect-card",
                div { class: "brand brand-lg", "S" }
                h1 { "SeraBot" }
                p { class: "muted", "Owner console · connect to your host" }
                label { "Server" }
                input {
                    class: "input",
                    value: "{server}",
                    placeholder: "https://bot.seradyx.tech",
                    oninput: move |event| session.write().server = event.value(),
                }
                label { "Token" }
                input {
                    class: "input",
                    "type": "password",
                    value: "{token}",
                    placeholder: "token minted on the server",
                    oninput: move |event| session.write().token = event.value(),
                }
                if let Some(message) = error {
                    div { class: "error", "{message}" }
                }
                div { class: "row",
                    button {
                        class: "btn primary",
                        disabled: connecting,
                        onclick: move |_| session.write().connect_now(),
                        if connecting { "Connecting…" } else { "Connect" }
                    }
                    button {
                        class: "btn ghost",
                        onclick: move |_| {
                            let next = session.read().theme.toggled();
                            session.write().set_theme(next);
                        },
                        if theme == Theme::Dark { "Light theme" } else { "Dark theme" }
                    }
                }
                p { class: "muted small",
                    "The token is kept in Windows Credential Manager. Only the server origin and theme are written to settings.json."
                }
            }
            p { class: "muted small", "v{version}" }
        }
    }
}

#[component]
fn Shell(session: Signal<Session>) -> Element {
    let section = session.read().section;
    let theme = session.read().theme;
    let chat = section == Section::Chat;
    let shell_class = if chat { "shell" } else { "shell dense" };
    rsx! {
        div { class: "{shell_class}",
            nav { class: "rail",
                div { class: "brand", "S" }
                div { class: "nav",
                    for (target, label, icon) in NAV {
                        button {
                            class: if section == target { "nav-btn active" } else { "nav-btn" },
                            onclick: move |_| session.write().open_section(target),
                            Icon { name: icon.to_string() }
                            span { "{label}" }
                        }
                    }
                }
                div { class: "rail-foot",
                    button {
                        class: "nav-btn",
                        onclick: move |_| {
                            let next = session.read().theme.toggled();
                            session.write().set_theme(next);
                        },
                        Icon { name: "theme".to_string() }
                        span { if theme == Theme::Dark { "Dark" } else { "Light" } }
                    }
                    button {
                        class: if section == Section::Settings { "nav-btn active" } else { "nav-btn" },
                        onclick: move |_| session.write().open_section(Section::Settings),
                        Icon { name: "settings".to_string() }
                        span { "Settings" }
                    }
                    span { class: "version", "v{env!(\"CARGO_PKG_VERSION\")}" }
                }
            }
            if chat {
                ConversationsPane { session }
            }
            main { class: "main",
                Header { session }
                StatusBanner { session }
                UpdateBanner { session }
                div { class: "content",
                    {match section {
                        Section::Chat => rsx! { ChatView { session } },
                        Section::Conversations => rsx! { ConversationsView { session } },
                        Section::Memory => rsx! { MemoryView { session } },
                        Section::Providers => rsx! { ProvidersView { session } },
                        Section::Connectors => rsx! { ConnectorsView { session } },
                        Section::Approvals => rsx! { ApprovalsView { session } },
                        Section::Fleet => {
                            if session.read().selected_bot.is_some() {
                                rsx! { BotProfileView { session } }
                            } else {
                                rsx! { FleetView { session } }
                            }
                        }
                        Section::Settings => rsx! { SettingsView { session } },
                    }}
                }
            }
        }
    }
}

#[component]
fn Header(session: Signal<Session>) -> Element {
    let section = session.read().section;
    let title = if section == Section::Chat {
        session.read().conversation_title()
    } else {
        section.label().to_owned()
    };
    let kind = session.read().conversation_kind().map(str::to_owned);
    let active_bot = session
        .read()
        .conversation
        .strip_prefix("dir:")
        .map(str::to_owned);
    let active_bot_avatar = active_bot
        .as_deref()
        .map(|bot| session.read().bot_avatar(bot))
        .unwrap_or_default();
    let connected = session.read().state.connected;
    let bots = session.read().state.listed_bots.len();
    let bots_loaded = session.read().state.bots_loaded;
    let bots_label = if bots_loaded {
        format!("Bots {bots}")
    } else {
        "Bots".to_owned()
    };
    let theme = session.read().theme;
    rsx! {
        header { class: "header",
            div { class: "header-title",
                strong { "{title}" }
                if let Some(kind) = kind {
                    span { class: "pill", "{kind}" }
                }
                if let Some(bot) = active_bot.clone() {
                    span { class: "pill accent", "bot chat" }
                    button {
                        class: "btn ghost small",
                        onclick: move |_| session.write().open_bot(bot.clone()),
                        span { class: "avatar avatar-xs", "{active_bot_avatar}" }
                        "Profile"
                    }
                }
            }
            div { class: "header-actions",
                if section == Section::Chat {
                    button {
                        class: "btn ghost small",
                        onclick: move |_| session.write().open_section(Section::Fleet),
                        "Bots {bots_label}"
                    }
                }
                button {
                    class: "btn ghost small",
                    onclick: move |_| {
                        let next = session.read().theme.toggled();
                        session.write().set_theme(next);
                    },
                    if theme == Theme::Dark { "Light" } else { "Dark" }
                }
                button {
                    class: "btn ghost small",
                    onclick: move |_| session.write().refresh(),
                    Icon { name: "refresh".to_string() }
                    "Refresh"
                }
                span { class: if connected { "status on" } else { "status" },
                    if connected { "Online" } else { "Offline" }
                }
            }
        }
    }
}

#[component]
fn StatusBanner(session: Signal<Session>) -> Element {
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
            div { class: "banner error", span { "{message}" } }
        }
    });
    let unknown_view = unknown.then(|| {
        rsx! {
            div { class: "banner warn",
                span { "A previous action has an unknown outcome. Refresh reads before acting again." }
            }
        }
    });
    let task_view = task.map(|(status, label, bot, id)| {
        rsx! {
            div { class: "banner",
                span { class: "pill", "{label}" }
                span { class: "muted small", "{bot} · {id}" }
                if status == TaskStatus::AwaitingApproval {
                    button {
                        class: "btn ghost small",
                        onclick: move |_| session.write().open_section(Section::Approvals),
                        "Review approvals"
                    }
                }
            }
        }
    });
    let admin_error_view = admin_error.map(|message| {
        rsx! {
            div { class: "banner error",
                span { "{message}" }
            }
        }
    });
    let admin_notice_view = admin_notice.map(|message| {
        rsx! {
            div { class: if restart_pending { "banner accent" } else { "banner" },
                span { "{message}" }
                if restart_pending {
                    button {
                        class: "btn primary small",
                        disabled: !can_restart,
                        onclick: move |_| session.write().request_restart(),
                        "Restart server and apply"
                    }
                }
            }
        }
    });
    let restarting_view = restarting.then(|| {
        rsx! {
            div { class: "banner warn",
                span { "The server is restarting. Reconnecting automatically…" }
                button {
                    class: "btn ghost small",
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
        {error_view}
        {unknown_view}
        {task_view}
    }
}

fn format_size(bytes: u64) -> String {
    const MB: f64 = 1024.0 * 1024.0;
    if bytes as f64 >= MB {
        format!("{:.1} MB", bytes as f64 / MB)
    } else {
        format!("{} KB", bytes.div_ceil(1024))
    }
}

#[component]
fn UpdateBanner(session: Signal<Session>) -> Element {
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
        div { class: "banner update",
            span { strong { "Update v{version} available" } span { class: "muted small", " · {size}" } }
            div { class: "row",
                button {
                    class: "btn primary small",
                    onclick: move |_| {
                        if session.write().apply_update(&install).is_ok() {
                            std::process::exit(0);
                        }
                    },
                    "Install and restart"
                }
                button {
                    class: "btn ghost small",
                    onclick: move |_| dismissed.set(true),
                    "Later"
                }
            }
            details {
                summary { "Release notes" }
                p { class: "muted small", "{notes}" }
            }
        }
    }
}

enum RailItem {
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
    },
    Thread {
        id: String,
        label: String,
        subtitle: String,
        depth: u8,
        active: bool,
    },
}

#[component]
fn ConversationsPane(session: Signal<Session>) -> Element {
    let mut search = use_signal(String::new);
    let mut collapsed = use_signal(HashSet::<String>::new);
    let needle = search().trim().to_lowercase();
    let current = session.read().conversation.clone();
    let loaded = session.read().state.conversations_loaded;
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
                    items.push(RailItem::Bot {
                        active: id == current,
                        id,
                        label: state.bot_display_name(&bot.id),
                        role: bot.role.clone(),
                        avatar: state.bot_avatar(&bot.id),
                        enabled: bot.enabled,
                    });
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
                let subtitle = state.conversation_subtitle(child);
                if matches(&label, &subtitle) {
                    rows.push(RailItem::Thread {
                        id: child.id.clone(),
                        label,
                        subtitle,
                        depth: 1,
                        active: child.id == current,
                    });
                }
                if child.normalized_kind() == "group" {
                    for grand in conversations.iter().filter(|conversation| {
                        conversation.parent_id.as_deref() == Some(&child.id)
                            && !bot_chats.contains(&conversation.id)
                    }) {
                        let label = state.conversation_label(grand);
                        let subtitle = state.conversation_subtitle(grand);
                        if matches(&label, &subtitle) {
                            rows.push(RailItem::Thread {
                                id: grand.id.clone(),
                                label,
                                subtitle,
                                depth: 2,
                                active: grand.id == current,
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
            let subtitle = state.conversation_subtitle(conversation);
            if !matches(&label, &subtitle) {
                continue;
            }
            let (key, title, added) = if is_room {
                ("group:rooms", "Rooms", &mut rooms_added)
            } else {
                ("group:directs", "Direct chats", &mut directs_added)
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
                });
            }
        }
        items
    };
    rsx! {
        aside { class: "sidebar",
            div { class: "sidebar-head",
                h2 { "Chats" }
                button {
                    class: "btn primary small",
                    onclick: move |_| session.write().open_section(Section::Conversations),
                    "New"
                }
            }
            input {
                class: "input",
                value: "{search}",
                placeholder: "Search chats and bots",
                oninput: move |event| search.set(event.value()),
            }
            div { class: "thread-list",
                if !loaded {
                    div { class: "muted small", "Loading chats…" }
                }
                for item in items {
                    {match item {
                        RailItem::Header { key, title, count, collapsed: closed } => {
                            let toggle = key.clone();
                            rsx! {
                                button {
                                    key: "{key}",
                                    class: "rail-header",
                                    onclick: move |_| {
                                        let mut set = collapsed.write();
                                        if set.contains(&toggle) {
                                            set.remove(&toggle);
                                        } else {
                                            set.insert(toggle.clone());
                                        }
                                    },
                                    span { class: "rail-chevron", if closed { "▸" } else { "▾" } }
                                    span { class: "rail-title", "{title}" }
                                    span { class: "rail-count", "{count}" }
                                }
                            }
                        },
                        RailItem::Bot { id, label, role, avatar, active, enabled } => rsx! {
                            button {
                                key: "{id}",
                                class: if active { "thread bot-thread active" } else { "thread bot-thread" },
                                onclick: {
                                    let id = id.clone();
                                    move |_| {
                                        let bot = id.strip_prefix("dir:").unwrap_or(&id).to_owned();
                                        session.write().open_bot_chat(bot);
                                    }
                                },
                                span { class: "avatar bot-avatar", style: avatar_style(&id), "{avatar}" }
                                span { class: "thread-copy",
                                    span { class: "thread-title", "{label}" }
                                    span { class: "thread-meta", if enabled { "{role}" } else { "{role} · disabled" } }
                                }
                            }
                        },
                        RailItem::Thread { id, label, subtitle, depth, active } => rsx! {
                            button {
                                key: "{id}",
                                class: if active { "thread indent active" } else { "thread indent" },
                                style: format!("--depth: {depth};"),
                                onclick: {
                                    let id = id.clone();
                                    move |_| session.write().open_conversation(id.clone())
                                },
                                span { class: "thread-title", "{label}" }
                                span { class: "thread-meta", "{subtitle}" }
                            }
                        },
                    }}
                }
            }
            button {
                class: "btn ghost small",
                onclick: move |_| session.write().open_section(Section::Conversations),
                "Manage sections and groups"
            }
        }
    }
}

#[component]
fn ChatView(session: Signal<Session>) -> Element {
    let messages: Vec<(String, bool, String, String, Option<String>, String, String)> = {
        let state = session.read();
        state
            .state
            .messages
            .iter()
            .map(|message| {
                let user = message.role == Role::User;
                let (speaker, avatar, bot) = state.message_speaker(message);
                (
                    message.id.clone(),
                    user,
                    speaker,
                    avatar,
                    bot,
                    stamp(&message.created_at),
                    message.content.clone(),
                )
            })
            .collect()
    };
    let task = session
        .read()
        .state
        .task
        .as_ref()
        .map(|task| (task.status, task.status.label().to_owned()));
    let composer = session.read().composer.clone();
    let can_send = session.read().state.can_send();
    let accepts = session.read().conversation_accepts_turns();
    let (active_bot, active_name, active_avatar, roster) = {
        let state = session.read();
        let active = state
            .conversation
            .strip_prefix("dir:")
            .map(str::to_owned)
            .or_else(|| {
                state
                    .state
                    .listed_bots
                    .iter()
                    .find(|bot| bot.role == "orchestrator")
                    .map(|bot| bot.id.clone())
            });
        let name = active
            .as_deref()
            .map(|id| state.bot_display_name(id))
            .unwrap_or_else(|| "Assistant".into());
        let avatar = active
            .as_deref()
            .map(|id| state.bot_avatar(id))
            .unwrap_or_else(|| "A".into());
        let roster = state
            .state
            .listed_bots
            .iter()
            .map(|bot| {
                (
                    bot.id.clone(),
                    state.bot_display_name(&bot.id),
                    state.bot_avatar(&bot.id),
                )
            })
            .collect::<Vec<_>>();
        (active, name, avatar, roster)
    };
    use_effect(move || {
        let _ = session.read().state.messages.len();
        let _ = dioxus::document::eval(
            "const el=document.getElementById('messages'); if(el){el.scrollTop=el.scrollHeight;}",
        );
    });
    let empty = messages.is_empty();
    let is_one_to_one = session.read().conversation.starts_with("dir:");
    rsx! {
        div { class: "chat",
            ModelBar { session }
            if let Some((status, label)) = task {
                if status == TaskStatus::AwaitingApproval {
                    div { class: "banner warn",
                        span { "{label}" }
                        button {
                            class: "btn ghost small",
                            onclick: move |_| session.write().open_section(Section::Approvals),
                            "Review approvals"
                        }
                    }
                }
            }
            div { class: "messages", id: "messages",
                if empty {
                    div { class: "hero",
                        div { class: "hero-mark", style: active_bot.as_deref().map(avatar_style).unwrap_or_default(), "{active_avatar}" }
                        h2 {
                            if !accepts {
                                "This node is read-only."
                            } else if is_one_to_one {
                                "Chat with {active_name}"
                            } else {
                                "Start a focused turn."
                            }
                        }
                        p { class: "muted",
                            if !accepts {
                                "Sections and groups organize the tree. Open a direct thread or a room to talk."
                            } else if is_one_to_one {
                                "A one-to-one conversation. Replies come from {active_name}, not the orchestrator."
                            } else {
                                "Ask the orchestrator, or open a room with your specialists."
                            }
                        }
                        div { class: "roster",
                            for (bot_id, bot_name, bot_avatar) in roster {
                                button {
                                    class: "roster-item",
                                    onclick: {
                                        let bot_id = bot_id.clone();
                                        move |_| session.write().open_bot_chat(bot_id.clone())
                                    },
                                    span { class: "avatar", style: avatar_style(&bot_id), "{bot_avatar}" }
                                    span { class: "muted small", "{bot_name}" }
                                }
                            }
                        }
                    }
                }
                for (id, user, speaker, avatar, bot, stamp, content) in messages {
                    div { class: if user { "msg user" } else { "msg" }, key: "{id}",
                        span {
                            class: "avatar",
                            style: bot.as_deref().map(avatar_style).unwrap_or_default(),
                            "{avatar}"
                        }
                        div { class: "bubble",
                            div { class: "bubble-meta",
                                strong { "{speaker}" }
                                span { "{stamp}" }
                            }
                            div { class: "bubble-text", "{content}" }
                        }
                    }
                }
            }
            div { class: "composer",
                textarea {
                    class: "input composer-input",
                    value: "{composer}",
                    placeholder: if accepts { format!("Message {active_name}…") } else { "This node does not accept turns".into() },
                    disabled: !accepts,
                    oninput: move |event| session.write().composer = event.value(),
                }
                div { class: "composer-row",
                    button {
                        class: "btn primary",
                        disabled: !can_send || composer.trim().is_empty(),
                        onclick: move |_| session.write().submit(),
                        Icon { name: "send".to_string() }
                        "Send"
                    }
                    span { class: "muted small", "Ctrl+Enter in the old client · 8,000 character limit" }
                }
            }
        }
    }
}

#[component]
fn ModelBar(session: Signal<Session>) -> Element {
    let (options, current, allowed, saved) = {
        let session = session.read();
        let catalog = session.state.catalog.as_ref();
        let mut options = vec![("auto".to_owned(), String::new())];
        let mut current = "auto".to_owned();
        let mut allowed = false;
        let mut saved = "auto".to_owned();
        if let Some(catalog) = catalog {
            if !catalog.selection.is_auto() {
                current = format!("{}|{}", catalog.selection.provider, catalog.selection.model);
            }
            for entry in &catalog.entries {
                options.push((entry.provider.clone(), entry.model.clone()));
            }
            if catalog.entries.is_empty() {
                for provider in &catalog.providers {
                    for model in &provider.models {
                        options.push((provider.id.clone(), model.clone()));
                    }
                }
            }
            allowed = catalog.allows(&session.state.draft);
            saved = if catalog.selection.is_auto() {
                "Auto-routing across approved models".into()
            } else {
                format!(
                    "Using {} via {}",
                    human_model(&catalog.selection.model),
                    human_provider(&catalog.selection.provider)
                )
            };
        }
        (options, current, allowed, saved)
    };
    let value = {
        let draft = session.read().state.draft.clone();
        if draft.is_auto() {
            "auto".to_owned()
        } else {
            format!("{}|{}", draft.provider, draft.model)
        }
    };
    let changed = value != current;
    rsx! {
        div { class: "model-bar",
            label { class: "muted small", "Model" }
            select {
                class: "input model-select",
                value: "{value}",
                onchange: move |event| {
                    let raw = event.value();
                    let next = if raw == "auto" {
                        Selection::auto()
                    } else {
                        let (provider, model) = raw.split_once('|').unwrap_or(("auto", ""));
                        if provider == "auto" {
                            Selection::auto()
                        } else {
                            Selection { provider: provider.to_owned(), model: model.to_owned() }
                        }
                    };
                    session.write().state.draft = next;
                },
                for (provider, model) in options {
                    if provider == "auto" {
                        option { value: "auto", "Auto · approved route" }
                    } else {
                        option { value: "{provider}|{model}", "{human_model(&model)} · {human_provider(&provider)}" }
                    }
                }
            }
            if changed {
                span { class: "muted small", "Egress-approved providers only" }
                button {
                    class: "btn primary small",
                    disabled: !allowed,
                    onclick: move |_| {
                        let draft = session.read().state.draft.clone();
                        session.write().apply_model(draft);
                    },
                    "Apply"
                }
            } else {
                span { class: "muted small", "Saved {saved}" }
            }
        }
    }
}

#[component]
fn FleetView(session: Signal<Session>) -> Element {
    let bots: Vec<BotCard> = {
        let state = session.read();
        state
            .state
            .listed_bots
            .iter()
            .map(|bot| BotCard {
                id: bot.id.clone(),
                role: bot.role.clone(),
                enabled: bot.enabled,
                live: bot.live,
                display_name: state.bot_display_name(&bot.id),
                description: bot.description.clone().unwrap_or_default(),
                avatar: state.bot_avatar(&bot.id),
                model: bot.model.clone(),
                tools: bot.tools.clone(),
                skills: bot.skills.clone(),
                require_approval: bot.require_approval.clone(),
            })
            .collect()
    };
    let bots_empty = bots.is_empty();
    let bots_loaded = session.read().state.bots_loaded;
    let bot_binding = session.read().state.bot_chat_binding == Some(true);
    let mut bot_id = use_signal(String::new);
    let mut bot_desc = use_signal(String::new);
    let mut bot_skills = use_signal(String::new);
    let mut skill_id = use_signal(String::new);
    let mut skill_desc = use_signal(String::new);
    let mut skill_body = use_signal(String::new);
    rsx! {
        div { class: "page",
            div { class: "page-head",
                div {
                    h1 { "Fleet" }
                    p { class: "muted", "Specialists, tools, and approval rules." }
                }
                button {
                    class: "btn ghost",
                    onclick: move |_| session.write().refresh_bots(),
                    Icon { name: "refresh".to_string() }
                    "Refresh"
                }
            }
            if !bot_binding {
                div { class: "banner warn",
                    span { "This server does not route one-to-one bot chats yet. Update the server to 0.5.0 or later." }
                }
            }
            details { class: "card form-card",
                summary { "New specialist" }
                div { class: "form-grid",
                    input {
                        class: "input",
                        value: "{bot_id}",
                        placeholder: "bot id (lowercase slug)",
                        oninput: move |event| bot_id.set(event.value()),
                    }
                    input {
                        class: "input",
                        value: "{bot_skills}",
                        placeholder: "skills: ask, notes",
                        oninput: move |event| bot_skills.set(event.value()),
                    }
                }
                textarea {
                    class: "input",
                    value: "{bot_desc}",
                    placeholder: "What this specialist is for",
                    oninput: move |event| bot_desc.set(event.value()),
                }
                details {
                    summary { class: "muted small", "Tools" }
                    div { class: "tool-grid",
                        for (index, name) in SPECIALIST_TOOLS.iter().enumerate() {
                            label { key: "{index}",
                                input {
                                    "type": "checkbox",
                                    checked: session.read().fleet.bot_tools.get(index).copied().unwrap_or(false),
                                    onchange: move |event| {
                                        if let Some(flag) = session.write().fleet.bot_tools.get_mut(index) {
                                            *flag = event.checked();
                                        }
                                    },
                                }
                                "{name}"
                            }
                        }
                    }
                }
                button {
                    class: "btn primary",
                    onclick: move |_| {
                        match parse_skill_list(&bot_skills()) {
                            Ok(skills) => {
                                let tools = session.read().selected_tools();
                                session.write().create_bot(bot_id(), bot_desc(), tools, skills);
                                bot_id.set(String::new());
                                bot_desc.set(String::new());
                                bot_skills.set(String::new());
                            }
                            Err(error) => session.write().state.error = Some(error.to_string()),
                        }
                    },
                    "Create disabled bot"
                }
                p { class: "muted small", "The bot stays disabled until serabot.service restarts." }
            }
            details { class: "card form-card",
                summary { "New skill pack" }
                div { class: "form-grid",
                    input {
                        class: "input",
                        value: "{skill_id}",
                        placeholder: "skill id",
                        oninput: move |event| skill_id.set(event.value()),
                    }
                    input {
                        class: "input",
                        value: "{skill_desc}",
                        placeholder: "One-line description",
                        oninput: move |event| skill_desc.set(event.value()),
                    }
                }
                textarea {
                    class: "input",
                    value: "{skill_body}",
                    placeholder: "Optional markdown body",
                    oninput: move |event| skill_body.set(event.value()),
                }
                button {
                    class: "btn primary",
                    onclick: move |_| {
                        session.write().create_skill(skill_id(), skill_desc(), skill_body());
                        skill_id.set(String::new());
                        skill_desc.set(String::new());
                        skill_body.set(String::new());
                    },
                    "Create skill"
                }
            }
            div { class: "cards",
                if !bots_loaded {
                    div { class: "card empty", h3 { "Loading bots…" } }
                }
                for bot in bots {
                    div { class: "card bot-card", key: "{bot.id}",
                        div { class: "bot-head",
                            span { class: "avatar avatar-lg", style: avatar_style(&bot.id), "{bot.avatar}" }
                            div {
                                h3 { "{bot.display_name}" }
                                span { class: "muted small", "{bot.id} · {bot.role}" }
                            }
                            span { class: if bot.enabled { "pill ok" } else { "pill" }, if bot.enabled { "enabled" } else { "disabled" } }
                            if !bot.live {
                                span { class: "pill warn", "restart required" }
                            }
                        }
                        if !bot.description.is_empty() {
                            p { class: "muted small", "{bot.description}" }
                        }
                        div { class: "chips",
                            span { class: "chip", if let Some(model) = bot.model.clone() { "Model: {human_model(&model)}" } else { "Model: conversation route" } }
                        }
                        div { class: "chips",
                            for tool in bot.tools {
                                span { class: if bot.require_approval.contains(&tool) { "chip warn" } else { "chip" }, "{tool}" }
                            }
                        }
                        div { class: "chips",
                            for skill in bot.skills {
                                span { class: "chip accent", "{skill}" }
                            }
                        }
                        div { class: "row",
                            button {
                                class: "btn primary small",
                                disabled: !bot.enabled || !bot_binding,
                                onclick: {
                                    let id = bot.id.clone();
                                    move |_| session.write().open_bot_chat(id.clone())
                                },
                                "1:1 chat"
                            }
                            button {
                                class: "btn ghost small",
                                onclick: {
                                    let id = bot.id.clone();
                                    move |_| session.write().open_bot(id.clone())
                                },
                                "Open profile"
                            }
                        }
                    }
                }
                if bots_loaded && bots_empty {
                    div { class: "card empty",
                        h3 { "No bots loaded yet" }
                        p { class: "muted", "Refresh, or create a specialist on the server." }
                    }
                }
            }
        }
    }
}

#[component]
fn BotProfileView(session: Signal<Session>) -> Element {
    let Some(id) = session.read().selected_bot.clone() else {
        return rsx! {};
    };
    let bot = session
        .read()
        .state
        .listed_bots
        .iter()
        .find(|bot| bot.id == id)
        .cloned();
    let Some(bot) = bot else {
        return rsx! {
            div { class: "page",
                div { class: "page-head",
                    div { h1 { "Bot profile" } p { class: "muted", "Loading {id}…" } }
                    button {
                        class: "btn ghost",
                        onclick: move |_| session.write().close_bot(),
                        "Back to fleet"
                    }
                }
                div { class: "card empty", h3 { "This bot is not in the roster yet." } }
            }
        };
    };
    let display_name = session.read().bot_display_name(&bot.id);
    let avatar = session.read().bot_avatar(&bot.id);
    let bot_description = bot.description.clone().unwrap_or_default();
    let bot_model = bot.model.clone();
    let bot_binding = session.read().state.bot_chat_binding == Some(true);
    let draft_name = session.read().bot_profile.display_name.clone();
    let draft_description = session.read().bot_profile.description.clone();
    let draft_avatar = session.read().bot_profile.avatar.clone();
    let can_save = session.read().can_admin();
    let skills = {
        let state = session.read();
        bot.skills
            .iter()
            .map(|id| {
                let summary = state
                    .state
                    .listed_skills
                    .iter()
                    .find(|skill| &skill.id == id);
                (
                    summary
                        .map(|skill| skill.name.clone())
                        .unwrap_or_else(|| id.clone()),
                    summary
                        .map(|skill| skill.description.clone())
                        .unwrap_or_default(),
                )
            })
            .collect::<Vec<_>>()
    };
    let capabilities = {
        let mut labels = bot
            .tools
            .iter()
            .filter_map(|tool| capability_label(tool))
            .collect::<Vec<_>>();
        labels.sort_unstable();
        labels.dedup();
        labels
    };
    let is_watcher = bot.role == "watcher" || bot.id == "watcher";
    rsx! {
        div { class: "page",
            div { class: "page-head",
                div {
                    h1 { "{display_name}" }
                    p { class: "muted", "{bot.id} · {bot.role}" }
                }
                div { class: "row",
                    button {
                        class: "btn ghost",
                        onclick: move |_| session.write().close_bot(),
                        "Back to fleet"
                    }
                    button {
                        class: "btn primary",
                        disabled: !bot.enabled || !bot_binding,
                        onclick: {
                            let id = bot.id.clone();
                            move |_| session.write().open_bot_chat(id.clone())
                        },
                        "Open 1:1 chat"
                    }
                }
            }
            div { class: "profile-grid",
                div { class: "card profile-identity",
                    span { class: "avatar avatar-xl", style: avatar_style(&bot.id), "{avatar}" }
                    h2 { "{display_name}" }
                    div { class: "chips",
                        span { class: if bot.enabled { "pill ok" } else { "pill" }, if bot.enabled { "enabled" } else { "disabled" } }
                        span { class: if bot.live { "pill ok" } else { "pill warn" }, if bot.live { "live" } else { "restart required" } }
                        span { class: "pill accent", "{bot.role}" }
                    }
                    if !bot_description.is_empty() {
                        p { class: "muted", "{bot_description}" }
                    } else {
                        p { class: "muted small", "No description yet. Add one below so the roster explains itself." }
                    }
                    if !bot.enabled {
                        p { class: "muted small", "This bot is disabled in its bot file. Enable it there and restart the server before chatting." }
                    }
                    if !bot_binding {
                        p { class: "muted small", "This server does not route one-to-one bot chats yet. Update the server to 0.5.0 or later." }
                    }
                    div { class: "chips",
                        span { class: "chip", if let Some(model) = bot_model.clone() { "Model: {human_model(&model)}" } else { "Model: conversation route" } }
                    }
                    p { class: "muted small", "Fallback models are not available yet; each turn uses the conversation route." }
                }
                div { class: "card",
                    h3 { "Profile" }
                    p { class: "muted small", "Display name, avatar and description belong to the console. Behavior, tools and model stay in the bot file." }
                    p { class: "muted small", "Use an emoji or initials for the avatar. Image uploads are not supported yet." }
                    div { class: "form-grid",
                        input {
                            class: "input",
                            value: "{draft_name}",
                            placeholder: "Display name",
                            oninput: move |event| session.write().bot_profile.display_name = event.value(),
                        }
                        input {
                            class: "input",
                            value: "{draft_avatar}",
                            placeholder: "Avatar emoji or initials",
                            oninput: move |event| session.write().bot_profile.avatar = event.value(),
                        }
                    }
                    textarea {
                        class: "input",
                        value: "{draft_description}",
                        placeholder: "What this bot is for",
                        oninput: move |event| session.write().bot_profile.description = event.value(),
                    }
                    button {
                        class: "btn primary",
                        disabled: !can_save,
                        onclick: move |_| session.write().save_bot_profile(),
                        "Save profile"
                    }
                }
                div { class: "card",
                    h3 { "Skills" }
                    if skills.is_empty() {
                        p { class: "muted small", "No skills attached to this bot." }
                    } else {
                        div { class: "stack",
                            for (name, description) in skills {
                                div { class: "stack-row",
                                    strong { "{name}" }
                                    if !description.is_empty() {
                                        span { class: "muted small", "{description}" }
                                    }
                                }
                            }
                        }
                    }
                }
                div { class: "card",
                    h3 { "Can produce" }
                    if capabilities.is_empty() {
                        p { class: "muted small", "No output tools are attached to this bot." }
                    } else {
                        div { class: "chips",
                            for label in capabilities {
                                span { class: "chip accent", "{label}" }
                            }
                        }
                        p { class: "muted small", "Generated decks and media are downloadable from their approvals and artifact links; per-bot generation history is not indexed yet." }
                    }
                }
                div { class: "card",
                    h3 { "Routines" }
                    if is_watcher {
                        p { class: "muted small", "The watcher runs the morning brief and watched-source checks from the server schedule." }
                    } else {
                        p { class: "muted small", "No per-bot routine yet. Routines currently run on the Watcher schedule only." }
                    }
                }
            }
        }
    }
}

#[component]
fn ApprovalsView(session: Signal<Session>) -> Element {
    let approvals_loaded = session.read().state.approvals_loaded;
    let approvals: Vec<(String, String, String, String)> = session
        .read()
        .state
        .approvals
        .iter()
        .map(|approval| {
            (
                approval.id.clone(),
                approval.tool.clone(),
                approval.summary.clone(),
                approval.expires_at.clone(),
            )
        })
        .collect();
    let approvals_empty = approvals.is_empty();
    rsx! {
        div { class: "page",
            div { class: "page-head",
                div {
                    h1 { "Approvals" }
                    p { class: "muted", "Each decision binds the exact payload, destination, hash, policy and expiry." }
                }
                button {
                    class: "btn ghost",
                    onclick: move |_| session.write().refresh_approvals(),
                    "Refresh list"
                }
            }
            div { class: "cards",
                if !approvals_loaded {
                    div { class: "card empty", h3 { "Loading approvals…" } }
                }
                for (id, tool, summary, expires) in approvals {
                    div { class: "card", key: "{id}",
                        div { class: "row between",
                            span { class: "pill accent", "{tool}" }
                            span { class: "muted small", "expires {expires}" }
                        }
                        p { class: "approval-text", "{summary}" }
                        div { class: "row",
                            button {
                                class: "btn primary",
                                onclick: {
                                    let id = id.clone();
                                    move |_| session.write().decide(id.clone(), ApprovalDecision::Once)
                                },
                                "Approve once"
                            }
                            button {
                                class: "btn ghost",
                                onclick: {
                                    let id = id.clone();
                                    move |_| session.write().decide(id.clone(), ApprovalDecision::Always)
                                },
                                "Always"
                            }
                            button {
                                class: "btn danger",
                                onclick: {
                                    let id = id.clone();
                                    move |_| session.write().decide(id.clone(), ApprovalDecision::Deny)
                                },
                                "Deny"
                            }
                        }
                    }
                }
                if approvals_loaded && approvals_empty {
                    div { class: "card empty",
                        h3 { "Nothing pending" }
                        p { class: "muted", "Gateway requests appear here." }
                    }
                }
            }
        }
    }
}

#[component]
fn ProvidersView(session: Signal<Session>) -> Element {
    let providers = session.read().state.providers.clone();
    let loaded = session.read().state.providers_loaded;
    let id = session.read().provider.id.clone();
    let kind = session.read().provider.kind.clone();
    let base_url = session.read().provider.base_url.clone();
    let models = session.read().provider.models.clone();
    let secret_name = session.read().provider.secret_name.clone();
    let api_key = session.read().provider.api_key.clone();
    let catalog_fetch = session.read().provider.catalog_fetch;
    let can_save = session.read().can_admin();
    let mut confirm = use_signal(|| Option::<String>::None);
    let kinds = [
        "openai-compatible",
        "mistral",
        "deepseek",
        "minimax",
        "minimax-cn",
        "openai",
        "groq",
        "together",
        "fireworks",
        "openrouter",
        "xai",
        "cerebras",
        "moonshot",
    ];
    rsx! {
        div { class: "page",
            div { class: "page-head",
                div {
                    h1 { "Providers" }
                    p { class: "muted", "Add a provider, pull its live models, and pick the route per conversation." }
                }
                div { class: "row",
                    button {
                        class: "btn ghost",
                        onclick: move |_| session.write().fetch_models(),
                        "Fetch models"
                    }
                    button {
                        class: "btn ghost",
                        onclick: move |_| session.write().refresh_providers(),
                        "Refresh"
                    }
                }
            }
            div { class: "card",
                ModelBar { session }
                p { class: "muted small", "Applies to the next turn; a running turn keeps its pinned route." }
            }
            if !loaded {
                div { class: "card empty", h3 { "Loading providers…" } }
            }
            div { class: "cards",
                for provider in providers {
                    div { class: "card", key: "{provider.id}",
                        div { class: "row between",
                            div {
                                h3 { "{provider.id}" }
                                if provider.managed {
                                    span { class: "pill accent", "managed here" }
                                }
                            }
                            span { class: "pill", "{provider.kind}" }
                        }
                        div { class: "chips",
                            if provider.catalog_verified {
                                span { class: "pill ok", "catalog verified" }
                            } else if provider.catalog_fetch {
                                span { class: "pill warn", "catalog fetch on" }
                            } else {
                                span { class: "pill", "static models" }
                            }
                            if provider.data_egress_approved {
                                span { class: "pill ok", "ready" }
                            } else {
                                span { class: "pill warn", "egress blocked" }
                            }
                        }
                        div { class: "chips",
                            for model in provider.models {
                                span { class: "chip", "{model}" }
                            }
                        }
                        p { class: "muted small", "Routing: {provider.routing_classes.join(\", \")}" }
                        div { class: "chips",
                            for (role, secret) in provider.secrets {
                                span { class: if secret.configured { "chip accent" } else { "chip warn" },
                                    "{role}: {secret.name.clone().unwrap_or_default()}"
                                }
                            }
                        }
                        if provider.managed {
                            if confirm() == Some(provider.id.clone()) {
                                div { class: "row",
                                    span { class: "muted small", "Remove {provider.id}? Conversations pinned to it must pick another route." }
                                    button {
                                        class: "btn danger small",
                                        disabled: !can_save,
                                        onclick: {
                                            let id = provider.id.clone();
                                            move |_| {
                                                session.write().remove_provider(id.clone());
                                                confirm.set(None);
                                            }
                                        },
                                        "Remove"
                                    }
                                    button {
                                        class: "btn ghost small",
                                        onclick: move |_| confirm.set(None),
                                        "Cancel"
                                    }
                                }
                            } else {
                                button {
                                    class: "btn ghost small",
                                    onclick: {
                                        let id = provider.id.clone();
                                        move |_| confirm.set(Some(id.clone()))
                                    },
                                    "Remove provider"
                                }
                            }
                        }
                    }
                }
            }
            div { class: "card form-card",
                h3 { "Add or update a provider" }
                p { class: "muted small",
                    "Saved next to the database, never in serabot.toml. Keys go straight to the encrypted store and are never shown again. Any OpenAI-compatible /chat/completions endpoint works, including gateways that front Anthropic models."
                }
                div { class: "form-grid",
                    input {
                        class: "input",
                        value: "{id}",
                        placeholder: "provider id (for example work)",
                        oninput: move |event| session.write().provider.id = event.value(),
                    }
                    select {
                        class: "input",
                        value: "{kind}",
                        onchange: move |event| session.write().provider.kind = event.value(),
                        for item in kinds {
                            option { value: "{item}", "{item}" }
                        }
                    }
                    input {
                        class: "input",
                        value: "{base_url}",
                        placeholder: "https://api.example.com/v1",
                        oninput: move |event| session.write().provider.base_url = event.value(),
                    }
                    input {
                        class: "input",
                        value: "{models}",
                        placeholder: "model ids, comma separated",
                        oninput: move |event| session.write().provider.models = event.value(),
                    }
                    input {
                        class: "input",
                        value: "{secret_name}",
                        placeholder: "secret name (for example WORK_API_KEY)",
                        oninput: move |event| session.write().provider.secret_name = event.value(),
                    }
                    input {
                        class: "input",
                        type: "password",
                        value: "{api_key}",
                        placeholder: "API key (leave empty if already stored)",
                        oninput: move |event| session.write().provider.api_key = event.value(),
                    }
                }
                label { class: "check",
                    input {
                        "type": "checkbox",
                        checked: catalog_fetch,
                        onchange: move |event| session.write().provider.catalog_fetch = event.checked(),
                    }
                    span { "Fetch the live model list (/models)" }
                }
                button {
                    class: "btn primary",
                    disabled: !can_save,
                    onclick: move |_| session.write().save_provider(),
                    "Save provider"
                }
            }
        }
    }
}

#[component]
fn ConnectorsView(session: Signal<Session>) -> Element {
    let connectors = session.read().state.connectors.clone();
    let loaded = session.read().state.connectors_loaded;
    let can_save = session.read().can_admin();
    rsx! {
        div { class: "page",
            div { class: "page-head",
                div {
                    h1 { "Connectors" }
                    p { class: "muted", "Enable Google, Microsoft, Notion and AgentMail, and store their credentials." }
                }
                button {
                    class: "btn ghost",
                    onclick: move |_| session.write().refresh_connectors(),
                    "Refresh"
                }
            }
            if !loaded {
                div { class: "card empty", h3 { "Loading connectors…" } }
            }
            div { class: "cards",
                for connector in connectors {
                    {
                        let id = connector.id.clone();
                        let enabled = session
                            .read()
                            .connector_drafts
                            .get(&id)
                            .map(|draft| draft.enabled)
                            .unwrap_or(connector.enabled);
                        let values = session
                            .read()
                            .connector_drafts
                            .get(&id)
                            .map(|draft| draft.values.clone())
                            .unwrap_or_default();
                        rsx! {
                            div { class: "card", key: "{id}",
                                div { class: "row between",
                                    div {
                                        h3 { "{id}" }
                                        span { class: "pill", "{connector.kind}" }
                                    }
                                    span { class: if enabled { "pill ok" } else { "pill" },
                                        if enabled { "enabled" } else { "disabled" }
                                    }
                                }
                                if let Some(uri) = connector.redirect_uri.clone() {
                                    code { "{uri}" }
                                }
                                div { class: "chips",
                                    for (role, secret) in &connector.secrets {
                                        span { class: if secret.configured { "chip accent" } else { "chip warn" },
                                            "{role}: {secret.name.clone().unwrap_or_default()}"
                                        }
                                    }
                                }
                                div { class: "form-grid",
                                    for (role, secret) in &connector.secrets {
                                        {
                                            let role = role.clone();
                                            let name = secret.name.clone().unwrap_or_default();
                                            let value = values.get(&role).cloned().unwrap_or_default();
                                            rsx! {
                                                input {
                                                    class: "input",
                                                    "type": "password",
                                                    value: "{value}",
                                                    placeholder: if secret.configured { format!("replace {name}") } else { format!("set {name}") },
                                                    oninput: {
                                                        let id = id.clone();
                                                        let role = role.clone();
                                                        move |event| session.write().set_connector_value(&id, &role, event.value())
                                                    },
                                                }
                                            }
                                        }
                                    }
                                }
                                p { class: "muted small",
                                    "Enabling makes the connector available; grant its tools to a bot in Fleet. Reads and sends stay approval-gated."
                                }
                                div { class: "row",
                                    button {
                                        class: if enabled { "btn ghost" } else { "btn primary" },
                                        onclick: {
                                            let id = id.clone();
                                            move |_| session.write().toggle_connector(&id)
                                        },
                                        if enabled { "Disable" } else { "Enable" }
                                    }
                                    button {
                                        class: "btn ghost",
                                        disabled: !can_save,
                                        onclick: {
                                            let id = id.clone();
                                            move |_| session.write().save_connector(id.clone())
                                        },
                                        "Save connector"
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

#[component]
fn MemoryView(session: Signal<Session>) -> Element {
    let memories = session.read().state.memories.clone();
    let memories_loaded = session.read().state.memories_loaded;
    let kind = session.read().memory.kind.clone();
    let content = session.read().memory.content.clone();
    let source = session.read().memory.source.clone();
    let query = session.read().memory.query.clone();
    let confirm = session.read().memory.delete_confirm.clone();
    rsx! {
        div { class: "page",
            div { class: "page-head",
                div {
                    h1 { "Memory" }
                    p { class: "muted", "Owner-scoped facts. The orchestrator reads the 20 most recent." }
                }
                button {
                    class: "btn ghost",
                    onclick: move |_| session.write().load_memories(),
                    "Refresh"
                }
            }
            div { class: "card",
                div { class: "row",
                    select {
                        class: "input",
                        value: "{kind}",
                        onchange: move |event| session.write().memory.kind = event.value(),
                        for kind in Session::memory_kinds() {
                            option { value: "{kind}", "{kind}" }
                        }
                    }
                    input {
                        class: "input",
                        value: "{source}",
                        placeholder: "source",
                        oninput: move |event| session.write().memory.source = event.value(),
                    }
                }
                textarea {
                    class: "input",
                    value: "{content}",
                    placeholder: "A durable preference, fact, decision, context or note",
                    oninput: move |event| session.write().memory.content = event.value(),
                }
                button {
                    class: "btn primary",
                    onclick: move |_| session.write().add_memory(),
                    "Add memory"
                }
            }
            div { class: "row",
                input {
                    class: "input",
                    value: "{query}",
                    placeholder: "Filter content, kind or source",
                    oninput: move |event| session.write().memory.query = event.value(),
                }
                button {
                    class: "btn ghost",
                    onclick: move |_| session.write().load_memories(),
                    "Search"
                }
            }
            div { class: "cards",
                if !memories_loaded {
                    div { class: "card empty", h3 { "Loading memories…" } }
                }
                for memory in memories {
                    div { class: "card", key: "{memory.id}",
                        div { class: "row between",
                            span { class: "pill accent", "{memory.kind}" }
                            span { class: "muted small", "{memory.created_at} · {memory.source}" }
                        }
                        p { "{memory.content}" }
                        if confirm.as_deref() == Some(memory.id.as_str()) {
                            button {
                                class: "btn danger small",
                                onclick: {
                                    let id = memory.id.clone();
                                    move |_| session.write().delete_memory(id.clone())
                                },
                                "Confirm delete"
                            }
                        } else {
                            button {
                                class: "btn ghost small",
                                onclick: {
                                    let id = memory.id.clone();
                                    move |_| session.write().memory.delete_confirm = Some(id.clone())
                                },
                                "Delete"
                            }
                        }
                    }
                }
            }
        }
    }
}

#[component]
fn ConversationsView(session: Signal<Session>) -> Element {
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
    rsx! {
        div { class: "page",
            div { class: "page-head",
                div {
                    h1 { "Conversations" }
                    p { class: "muted", "Sections, groups, direct threads and rooms." }
                }
                button {
                    class: "btn ghost",
                    onclick: move |_| session.write().load_conversations(),
                    "Refresh"
                }
            }
            div { class: "card",
                h3 { "New conversation" }
                div { class: "row",
                    select {
                        class: "input",
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
                    input {
                        class: "input",
                        value: "{title}",
                        placeholder: "name (for example Finance)",
                        oninput: move |event| session.write().fleet.conv_title = event.value(),
                    }
                    input {
                        class: "input",
                        value: "{slug}",
                        placeholder: "id (lowercase slug)",
                        oninput: move |event| session.write().fleet.conv_slug = event.value(),
                    }
                    if kind != ConversationKind::Section {
                        select {
                            class: "input",
                            value: "{parent}",
                            onchange: move |event| session.write().fleet.conv_parent = event.value(),
                            option { value: "", "No parent" }
                            for (id, label) in parents {
                                option { value: "{id}", "{label}" }
                            }
                        }
                    }
                    button {
                        class: "btn primary",
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
            div { class: "cards",
                for (id, title, subtitle, updated) in items {
                    div { class: "card", key: "{id}",
                        div { class: "row between",
                            div {
                                h3 { "{title}" }
                                span { class: "muted small", "{subtitle} · {updated}" }
                            }
                            div { class: "row",
                                button {
                                    class: "btn ghost small",
                                    onclick: {
                                        let id = id.clone();
                                        move |_| session.write().open_conversation(id.clone())
                                    },
                                    "Open"
                                }
                                button {
                                    class: "btn ghost small",
                                    onclick: {
                                        let id = id.clone();
                                        let title = title.clone();
                                        move |_| rename.set(Some((id.clone(), title.clone())))
                                    },
                                    "Rename"
                                }
                                button {
                                    class: "btn danger small",
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
                                div { class: "row",
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
                                        class: "btn primary small",
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
                                        class: "btn ghost small",
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

#[component]
fn SettingsView(session: Signal<Session>) -> Element {
    let theme = session.read().theme;
    let server = session.read().server.clone();
    let version = session.read().state.version.clone();
    let update = session.read().update.info.clone();
    let update_error = session.read().update.error.clone();
    let checking = session.read().update.checking;
    let audio_enabled = session.read().state.audio_enabled;
    let transcription = session
        .read()
        .state
        .audio_transcription_model
        .clone()
        .unwrap_or_default();
    let understanding = session
        .read()
        .state
        .audio_understanding_model
        .clone()
        .unwrap_or_default();
    let firecrawl_enabled = session.read().state.firecrawl_enabled;
    let telegram_enabled = session.read().state.telegram_enabled;
    let catalog_fetch = session.read().state.catalog_fetch;
    let audio_on = session.read().audio.enabled;
    let audio_transcription = session.read().audio.transcription_model.clone();
    let audio_understanding = session.read().audio.understanding_model.clone();
    let can_admin = session.read().can_admin();
    rsx! {
        div { class: "page",
            div { class: "page-head",
                div {
                    h1 { "Settings" }
                    p { class: "muted", "Connection, capabilities, appearance and updates." }
                }
            }
            div { class: "card",
                h3 { "Connection" }
                p { class: "muted small", "{server}" }
                if !version.is_empty() {
                    p { class: "muted small", "Server {version}" }
                }
                div { class: "row",
                    button {
                        class: "btn ghost",
                        onclick: move |_| session.write().forget_token(),
                        "Forget token"
                    }
                    button {
                        class: "btn danger",
                        onclick: move |_| session.write().disconnect(),
                        "Disconnect"
                    }
                }
            }
            div { class: "card",
                h3 { "Capabilities" }
                div { class: "chips",
                    span {
                        class: match audio_enabled { Some(true) => "chip accent", Some(false) => "chip warn", None => "chip" },
                        match audio_enabled { Some(true) => "Audio on", Some(false) => "Audio off", None => "Audio unknown" }
                    }
                    span {
                        class: match firecrawl_enabled { Some(true) => "chip accent", Some(false) => "chip", None => "chip" },
                        match firecrawl_enabled { Some(true) => "Firecrawl on", Some(false) => "Firecrawl off", None => "Firecrawl unknown" }
                    }
                    span {
                        class: match telegram_enabled { Some(true) => "chip accent", Some(false) => "chip", None => "chip" },
                        match telegram_enabled { Some(true) => "Telegram on", Some(false) => "Telegram off", None => "Telegram unknown" }
                    }
                    span {
                        class: match catalog_fetch { Some(true) => "chip accent", Some(false) => "chip", None => "chip" },
                        match catalog_fetch { Some(true) => "Model catalogs live", Some(false) => "Model catalogs static", None => "Model catalogs unknown" }
                    }
                }
                if audio_enabled == Some(true) {
                    p { class: "muted small",
                        "Transcription: {transcription}. Understanding: {understanding}."
                    }
                    p { class: "muted small",
                        "Voice notes sent to the bot's Telegram intake are transcribed with the configured Mistral audio model before the turn runs."
                    }
                } else if audio_enabled == Some(false) {
                    p { class: "muted small", "Audio is disabled on the server (audio.enabled = false)." }
                } else {
                    p { class: "muted small",
                        "This server build does not report capabilities. Update the server to see live status."
                    }
                }
                if catalog_fetch == Some(false) {
                    p { class: "muted small",
                        "Enable catalog_fetch in [llm] and use Fetch models on the Providers page to pull live model lists."
                    }
                }
                div { class: "form-grid",
                    input {
                        class: "input",
                        value: "{audio_transcription}",
                        placeholder: "transcription model",
                        oninput: move |event| session.write().audio.transcription_model = event.value(),
                    }
                    input {
                        class: "input",
                        value: "{audio_understanding}",
                        placeholder: "understanding model",
                        oninput: move |event| session.write().audio.understanding_model = event.value(),
                    }
                }
                p { class: "muted small",
                    "Audio uses the Mistral credentials already stored for the provider. Add a separate audio key in serabot.toml only if you need one."
                }
                label { class: "check",
                    input {
                        "type": "checkbox",
                        checked: audio_on,
                        onchange: move |event| session.write().audio.enabled = event.checked(),
                    }
                    span { "Enable audio transcription and understanding" }
                }
                button {
                    class: "btn primary",
                    disabled: !can_admin,
                    onclick: move |_| session.write().save_audio(),
                    "Save audio settings"
                }
            }
            div { class: "card",
                h3 { "Appearance" }
                div { class: "row",
                    for mode in [Theme::Dark, Theme::Light] {
                        button {
                            class: if theme == mode { "btn primary" } else { "btn ghost" },
                            onclick: move |_| session.write().set_theme(mode),
                            "{mode:?}"
                        }
                    }
                }
            }
            div { class: "card",
                h3 { "Updates" }
                button {
                    class: "btn ghost",
                    disabled: checking,
                    onclick: move |_| session.write().check_updates(),
                    if checking { "Checking…" } else { "Check for updates" }
                }
                if let Some(info) = update {
                    p { class: "accent", "v{info.version} available" }
                    button {
                        class: "btn primary",
                        onclick: {
                            let info = info.clone();
                            move |_| {
                                let _ = session.write().apply_update(&info);
                            }
                        },
                        "Install and restart"
                    }
                }
                if let Some(error) = update_error {
                    p { class: "error", "{error}" }
                }
            }
            div { class: "card",
                h3 { "Keyboard" }
                p { class: "muted small", "Ctrl+Enter sends in the previous shell; the webview uses the Send button." }
            }
        }
    }
}
