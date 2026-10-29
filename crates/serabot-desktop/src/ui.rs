//! Dioxus Desktop shell. Windows-only; the Rust session owns all network work.
//!
//! Layout: a three-pane macOS-style shell (glass sidebar, sticky glass topbar,
//! centered chat column, optional contextual inspector). The design system lives
//! in `assets/app.css`; this file owns the markup and the chat ergonomics.

#![cfg_attr(not(windows), allow(dead_code))]

use dioxus::prelude::*;
use serabot_desktop::{
    ApprovalDecision, ConversationKind, Role, SPECIALIST_TOOLS, Selection, TaskStatus, Team,
    TeamMember, TeamMode,
    audio_capture::MicRecorder,
    parse_skill_list, provider_presets,
    session::{Section, Session, Theme},
    teams,
};
use std::collections::{BTreeMap, HashMap, HashSet};

const CSS: &str = include_str!("../assets/app.css");

/// A chat row: id, owner flag, speaker, avatar, bot, time, body, peer flag.
type ChatRow = (
    String,
    bool,
    String,
    String,
    Option<String>,
    String,
    String,
    bool,
);

/// A roster row: bot id, display name, avatar, enabled, live, role.
type RosterRow = (String, String, String, bool, bool, String);

/// A provider row: id, kind, managed, catalog verified, catalog fetch, data
/// egress approved, models, routing classes, secrets.
type ProviderRow = (
    String,
    String,
    bool,
    bool,
    bool,
    bool,
    Vec<String>,
    String,
    Vec<(String, String, bool)>,
);

/// The provider kinds the server understands.
const PROVIDER_KINDS: [&str; 13] = [
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

/// The provider authorization pages the console can open.
///
/// These are the same fixed endpoints the server uses
/// (`crates/serabot/src/connectors.rs`). The console never builds a URL from
/// unvalidated server data: the endpoint comes from this table and only a
/// checked redirect URI is appended.
const CONNECTOR_AUTH_URLS: [(&str, &str); 8] = [
    (
        "google_drive",
        "https://accounts.google.com/o/oauth2/v2/auth",
    ),
    (
        "google_sheets",
        "https://accounts.google.com/o/oauth2/v2/auth",
    ),
    (
        "google_calendar",
        "https://accounts.google.com/o/oauth2/v2/auth",
    ),
    ("gmail", "https://accounts.google.com/o/oauth2/v2/auth"),
    (
        "outlook",
        "https://login.microsoftonline.com/common/oauth2/v2.0/authorize",
    ),
    (
        "outlook_calendar",
        "https://login.microsoftonline.com/common/oauth2/v2.0/authorize",
    ),
    ("notion", "https://api.notion.com/v1/oauth/authorize"),
    ("agentmail", "https://app.agentmail.to/"),
];

/// The authorization page of a connector, with its redirect URI appended.
///
/// `None` when the connector has no known authorization page or when the
/// redirect URI is not a plain HTTP(S) address. The result is always an
/// HTTPS URL built from the fixed table above.
fn connector_auth_url(connector_id: &str, redirect_uri: Option<&str>) -> Option<String> {
    let (_, endpoint) = CONNECTOR_AUTH_URLS
        .iter()
        .find(|(id, _)| *id == connector_id)?;
    let mut url = url::Url::parse(endpoint).ok()?;
    if url.scheme() != "https" {
        return None;
    }
    if let Some(redirect) = redirect_uri
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        if !redirect.starts_with("https://") && !redirect.starts_with("http://") {
            return None;
        }
        // Only a plain address is accepted: no shell metacharacters and no
        // query string, so the opened URL can carry nothing but the address.
        if !redirect
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || "-._~:/?@#%".contains(character))
        {
            return None;
        }
        url.query_pairs_mut().append_pair("redirect_uri", redirect);
    }
    Some(url.to_string())
}

/// Global key bindings, installed once in the WebView.
/// Cmd/Ctrl+K focuses the sidebar search; Esc closes the topmost sheet or menu.
const GLOBAL_KEYS_JS: &str = r#"(function () {
  if (window.__serabotKeys) { return; }
  window.__serabotKeys = true;
  window.addEventListener('keydown', function (event) {
    var mod = event.ctrlKey || event.metaKey;
    if (mod && (event.key === 'k' || event.key === 'K')) {
      var search = document.getElementById('global-search');
      if (search) {
        event.preventDefault();
        search.focus();
        if (search.select) { search.select(); }
      }
      return;
    }
    if (event.key === 'Escape') {
      var open = document.querySelectorAll('.sheet.open, .menu.open');
      for (var i = open.length - 1; i >= 0; i -= 1) {
        var overlay = open[i];
        var closer = overlay.querySelector('.escape-close');
        if (closer) { closer.click(); return; }
        var parent = overlay.parentElement;
        var trigger = parent ? parent.querySelector('.menu-trigger') : null;
        if (trigger) { trigger.click(); return; }
        overlay.classList.remove('open');
      }
    }
  });
})();"#;

fn stamp(raw: &str) -> String {
    raw.chars().take(16).collect::<String>().replace('T', " ")
}

const NAV: [(Section, &str, &str); 8] = [
    (Section::Chat, "Chat", "chat"),
    (Section::Fleet, "Fleet", "robot"),
    (Section::Memory, "Memory", "bolt"),
    (Section::Approvals, "Approvals", "shield"),
    (Section::Providers, "Providers", "grid"),
    (Section::Connectors, "Connectors", "external"),
    (Section::Settings, "Settings", "settings"),
    (Section::Conversations, "Threads", "list"),
];

/// Overlay surfaces that sit above the shell.
#[derive(Clone, Copy, PartialEq)]
enum Overlay {
    None,
    /// The new-conversation sheet. The kind lives in the session draft, so
    /// the menu, the sheet and the Threads form always agree.
    NewConversation,
    /// The guided "Add provider" sheet.
    AddProvider,
    Team,
}

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

/// Only the hue travels to the DOM; the colors themselves are CSS tokens.
fn avatar_style(id: &str) -> String {
    format!("--avatar-hue: {};", avatar_hue(id))
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
    use_effect(move || {
        let _ = dioxus::document::eval(GLOBAL_KEYS_JS);
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

/// Inline SVG icon set. No image assets ship with the console.
#[component]
fn Icon(name: String, #[props(default = 20)] size: u32) -> Element {
    let path = match name.as_str() {
        "chat" => "M4 5h16v11H8l-4 4V5z",
        "search" => "M11 4a7 7 0 1 0 0 14 7 7 0 0 0 0-14zM20 20l-4-4",
        "plus" => "M12 5v14M5 12h14",
        "send" => "M4 12l16-8-6 16-2-6-8-2z",
        "mic" => "M12 3a3 3 0 0 1 3 3v6a3 3 0 0 1-6 0V6a3 3 0 0 1 3-3zM5 11a7 7 0 0 0 14 0M12 18v3",
        "paperclip" => {
            "M20 11l-8.5 8.5a5 5 0 0 1-7-7L13 4a3.5 3.5 0 0 1 5 5l-8.5 8.5a2 2 0 0 1-3-3L15 6"
        }
        "image" => "M4 5h16v14H4zM4 16l5-5 4 4 3-3 4 4M9 9.5a1 1 0 1 1-2 0 1 1 0 0 1 2 0z",
        "settings" => {
            "M12 8a4 4 0 1 0 0 8 4 4 0 0 0 0-8zM12 2v3M12 19v3M2 12h3M19 12h3M5 5l2 2M17 17l2 2M19 5l-2 2M7 17l-2 2"
        }
        "users" => {
            "M9 11a3 3 0 1 0 0-6 3 3 0 0 0 0 6zM3 19a6 6 0 0 1 12 0M17 11a3 3 0 0 0 0-6M16 14a5 5 0 0 1 5 5"
        }
        "robot" => {
            "M7 8h10a2 2 0 0 1 2 2v6a2 2 0 0 1-2 2H7a2 2 0 0 1-2-2v-6a2 2 0 0 1 2-2zM12 4v4M9 13h.01M15 13h.01"
        }
        "check" => "M5 13l4 4 10-10",
        "x" => "M6 6l12 12M18 6L6 18",
        "chevron-down" => "M6 9l6 6 6-6",
        "chevron-right" => "M9 6l6 6-6 6",
        "dots" => "M6 12h.01M12 12h.01M18 12h.01",
        "sparkles" => {
            "M12 3l1.6 4.4L18 9l-4.4 1.6L12 15l-1.6-4.4L6 9l4.4-1.6L12 3zM18 15l.8 2.2L21 18l-2.2.8L18 21l-.8-2.2L15 18l2.2-.8L18 15z"
        }
        "shield" => "M12 3l8 3v6c0 5-3.5 8-8 9-4.5-1-8-4-8-9V6l8-3zM9 12l2 2 4-4",
        "folder" => "M3 6h6l2 2h10v10H3z",
        "bolt" => "M13 3L5 14h6l-1 7 8-11h-6l1-7z",
        "refresh" => {
            "M4 12a8 8 0 0 1 13.5-5.8L20 8M20 4v4h-4M20 12a8 8 0 0 1-13.5 5.8L4 16M4 20v-4h4"
        }
        "download" => "M12 4v10M8 11l4 4 4-4M5 19h14",
        "external" => "M14 5h5v5M19 5l-8 8M18 14v5H5V6h5",
        "edit" => "M4 20h4l10-10-4-4L4 16v4zM14 6l4 4",
        "trash" => "M4 7h16M9 7V5h6v2M6 7l1 13h10l1-13M10 11v6M14 11v6",
        "moon" => "M20 14a8 8 0 1 1-10-10 7 7 0 0 0 10 10z",
        "sun" => {
            "M12 8a4 4 0 1 0 0 8 4 4 0 0 0 0-8zM12 2v2M12 20v2M2 12h2M20 12h2M5 5l1.5 1.5M17.5 17.5L19 19M19 5l-1.5 1.5M6.5 17.5L5 19"
        }
        "list" => "M8 6h12M8 12h12M8 18h12M4 6h.01M4 12h.01M4 18h.01",
        "grid" => "M4 4h7v7H4zM13 4h7v7h-7zM4 13h7v7H4zM13 13h7v7h-7z",
        "user" => "M12 12a4 4 0 1 0 0-8 4 4 0 0 0 0 8zM4 20a8 8 0 0 1 16 0",
        "threads" => "M4 5h11v9H4zM9 9h11v9H9z",
        "memory" => "M12 3c-4 0-7 2-7 5s3 5 7 5 7-2 7-5-3-5-7-5zM5 8v8c0 3 3 5 7 5s7-2 7-5V8",
        "providers" => "M4 6h16M4 12h16M4 18h10",
        "connectors" => "M8 12a4 4 0 1 1 4 4M16 12a4 4 0 1 1-4-4",
        "approvals" => "M12 3a9 9 0 1 0 0 18 9 9 0 0 0 0-18zM8 12l3 3 5-6",
        "fleet" => "M4 4h6v6H4zM14 4h6v6h-6zM4 14h6v6H4zM14 14h6v6h-6z",
        "theme" => {
            "M12 4v2M12 18v2M4 12h2M18 12h2M6.3 6.3l1.4 1.4M16.3 16.3l1.4 1.4M17.7 6.3l-1.4 1.4M7.7 16.3l-1.4 1.4M16 12a4 4 0 1 1-8 0 4 4 0 0 1 8 0z"
        }
        _ => "M5 5h14v14H5z",
    };
    rsx! {
        svg {
            width: "{size}",
            height: "{size}",
            "viewBox": "0 0 24 24",
            fill: "none",
            stroke: "currentColor",
            "stroke-width": "1.6",
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
                div { class: "field",
                    label { class: "field-label", "Server" }
                    input {
                        class: "input",
                        value: "{server}",
                        placeholder: "https://bot.seradyx.tech",
                        oninput: move |event| session.write().server = event.value(),
                    }
                }
                div { class: "field",
                    label { class: "field-label", "Token" }
                    input {
                        class: "input",
                        "type": "password",
                        value: "{token}",
                        placeholder: "token minted on the server",
                        oninput: move |event| session.write().token = event.value(),
                    }
                }
                if let Some(message) = error {
                    div { class: "banner err", span { "{message}" } }
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
                p { class: "tiny",
                    "The token is kept in Windows Credential Manager. Only the server origin and theme are written to settings.json."
                }
            }
            p { class: "tiny", "v{version}" }
        }
    }
}

#[component]
fn Shell(session: Signal<Session>) -> Element {
    let section = session.read().section;
    let overlay = use_signal(|| Overlay::None);
    let mut inspector_open = use_signal(|| true);
    let mut drawer_open = use_signal(|| false);
    let mut toast = use_signal(|| Option::<(String, bool)>::None);

    rsx! {
        div { class: "app-shell",
            Sidebar { session, overlay, drawer_open, toast }
            div {
                class: if drawer_open() { "scrim drawer-scrim open" } else { "scrim drawer-scrim" },
                onclick: move |_| drawer_open.set(false),
            }
            main { class: "main",
                Topbar { session, overlay, inspector_open, drawer_open }
                StatusBanner { session }
                UpdateBanner { session }
                div { class: "pane",
                    {match section {
                        Section::Chat => rsx! { ChatView { session, overlay } },
                        Section::Conversations => rsx! { ConversationsView { session } },
                        Section::Memory => rsx! { MemoryView { session } },
                        Section::Providers => rsx! { ProvidersView { session, overlay } },
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
            div {
                class: if inspector_open() { "scrim inspector-scrim open" } else { "scrim inspector-scrim" },
                onclick: move |_| inspector_open.set(false),
            }
            Inspector { session, overlay, open: inspector_open }
            {match overlay() {
                Overlay::NewConversation => rsx! {
                    NewConversationSheet { session, overlay }
                },
                Overlay::AddProvider => rsx! {
                    AddProviderSheet { session, overlay }
                },
                Overlay::Team => rsx! {
                    TeamSheet { session, overlay }
                },
                Overlay::None => rsx! {},
            }}
            if let Some((message, failed)) = toast() {
                div { class: if failed { "toast err" } else { "toast" }, role: "status",
                    span { "{message}" }
                    button {
                        class: "icon-btn escape-close",
                        title: "Dismiss",
                        onclick: move |_| toast.set(None),
                        Icon { name: "x", size: 16 }
                    }
                }
            }
        }
    }
}

/// The "+" menu shared by the sidebar head and the chat column head.
#[component]
fn NewMenu(
    session: Signal<Session>,
    overlay: Signal<Overlay>,
    #[props(default = false)] right: bool,
) -> Element {
    let mut overlay = overlay;
    let mut open = use_signal(|| false);
    let align = if right { " right" } else { "" };
    rsx! {
        div { class: "menu-wrap",
            button {
                class: "icon-btn primary menu-trigger",
                title: "New",
                onclick: move |_| open.set(!open()),
                Icon { name: "plus", size: 18 }
            }
            div {
                class: if open() { format!("menu open{align}") } else { format!("menu{align}") },
                onkeydown: move |event: KeyboardEvent| {
                    if event.key() == Key::Escape {
                        open.set(false);
                    }
                },
                div { class: "menu-label", "Create" }
                button {
                    class: "menu-item",
                    onclick: move |_| {
                        open.set(false);
                        session.write().fleet.conv_kind = ConversationKind::Direct;
                        overlay.set(Overlay::NewConversation);
                    },
                    Icon { name: "chat", size: 16 }
                    "New direct chat"
                }
                button {
                    class: "menu-item",
                    onclick: move |_| {
                        open.set(false);
                        session.write().fleet.conv_kind = ConversationKind::Group;
                        overlay.set(Overlay::NewConversation);
                    },
                    Icon { name: "users", size: 16 }
                    "New group"
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
                div { class: "menu-sep" }
                button {
                    class: "menu-item",
                    onclick: move |_| {
                        open.set(false);
                        session.write().open_section(Section::Fleet);
                    },
                    Icon { name: "robot", size: 16 }
                    "New bot"
                }
                // A team lives on a room: the room is created first and its
                // team is written in the same step (docs/desktop-redesign.md
                // §3.8).
                button {
                    class: "menu-item",
                    onclick: move |_| {
                        open.set(false);
                        session.write().fleet.conv_kind = ConversationKind::Room;
                        overlay.set(Overlay::NewConversation);
                    },
                    Icon { name: "shield", size: 16 }
                    "New team"
                }
            }
        }
    }
}

#[component]
fn Sidebar(
    session: Signal<Session>,
    overlay: Signal<Overlay>,
    drawer_open: Signal<bool>,
    toast: Signal<Option<(String, bool)>>,
) -> Element {
    let mut overlay = overlay;
    let mut toast = toast;
    let section = session.read().section;
    let theme = session.read().theme;
    let connected = session.read().state.connected;
    let mut search = use_signal(String::new);
    let mut collapsed = use_signal(HashSet::<String>::new);
    let mut renaming = use_signal(|| Option::<(String, String)>::None);
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
                        live: bot.live,
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
        aside { class: if drawer_open() { "sidebar glass open" } else { "sidebar glass" },
            div { class: "sidebar-head",
                span { class: "brand", "S" }
                span { class: "brand-name", "SeraBot" }
                NewMenu { session, overlay }
            }
            button {
                class: "btn primary block",
                onclick: move |_| {
                    session.write().fleet.conv_kind = ConversationKind::Direct;
                    overlay.set(Overlay::NewConversation);
                },
                Icon { name: "plus", size: 16 }
                "New chat"
            }
            div { class: "sidebar-search",
                span { class: "search-glyph", Icon { name: "search", size: 15 } }
                input {
                    class: "input",
                    id: "global-search",
                    value: "{search}",
                    placeholder: "Search chats and bots",
                    oninput: move |event| search.set(event.value()),
                }
            }
            nav { class: "nav-list",
                for (target, label, icon) in NAV {
                    button {
                        class: if section == target { "nav-item active" } else { "nav-item" },
                        onclick: move |_| session.write().open_section(target),
                        Icon { name: icon.to_string(), size: 16 }
                        span { "{label}" }
                    }
                }
            }
            div { class: "thread-list",
                if !loaded {
                    div { class: "skeleton", " " }
                    div { class: "skeleton", " " }
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
                                        if set.contains(&toggle) {
                                            set.remove(&toggle);
                                        } else {
                                            set.insert(toggle.clone());
                                        }
                                    },
                                    Icon { name: if closed { "chevron-right" } else { "chevron-down" }, size: 14 }
                                    span { class: "thread-title", "{title}" }
                                    span { class: "thread-meta", "{count}" }
                                }
                            }
                        },
                        RailItem::Bot { id, label, role, avatar, active, enabled, live } => rsx! {
                            div { key: "{id}", class: if active { "thread active" } else { "thread" },
                                button {
                                    class: "thread-main",
                                    onclick: {
                                        let id = id.clone();
                                        move |_| {
                                            let bot = id.strip_prefix("dir:").unwrap_or(&id).to_owned();
                                            session.write().open_bot_chat(bot);
                                        }
                                    },
                                    span { class: "avatar sm bot", style: avatar_style(&id), "{avatar}" }
                                    span { class: "thread-title", "{label}" }
                                    span { class: "thread-meta", if enabled { "{role}" } else { "{role} · disabled" } }
                                }
                                span {
                                    class: if live { "thread-dot live" } else { "thread-dot off" },
                                    title: if live { "live" } else { "restart required" }
                                }
                            }
                        },
                        RailItem::Thread { id, label, subtitle, depth, active } => {
                            let editing = renaming().as_ref().map(|(edit, _)| edit == &id).unwrap_or(false);
                            rsx! {
                                div {
                                    key: "{id}",
                                    class: if active { "thread active" } else { "thread" },
                                    style: format!("--depth: {depth};"),
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
                                            class: "thread-main",
                                            onclick: {
                                                let id = id.clone();
                                                move |_| session.write().open_conversation(id.clone())
                                            },
                                            span { class: "thread-title", "{label}" }
                                            span { class: "thread-meta", "{subtitle}" }
                                        }
                                        div { class: "thread-actions",
                                            button {
                                                class: "btn icon",
                                                title: "New message",
                                                onclick: {
                                                    let id = id.clone();
                                                    move |_| session.write().open_conversation(id.clone())
                                                },
                                                Icon { name: "chat", size: 15 }
                                            }
                                            button {
                                                class: "btn icon",
                                                title: "Rename",
                                                onclick: {
                                                    let id = id.clone();
                                                    let label = label.clone();
                                                    move |_| renaming.set(Some((id.clone(), label.clone())))
                                                },
                                                Icon { name: "edit", size: 15 }
                                            }
                                            button {
                                                class: "btn icon",
                                                title: "Archive",
                                                onclick: {
                                                    let id = id.clone();
                                                    move |_| {
                                                        session.write().archive_conversation(id.clone());
                                                        toast.set(Some(("Archived.".into(), false)));
                                                    }
                                                },
                                                Icon { name: "trash", size: 15 }
                                            }
                                        }
                                    }
                                }
                            }
                        },
                    }}
                }
            }
            div { class: "sidebar-foot",
                span {
                    class: if connected { "chip ok" } else { "chip danger" },
                    if connected { "Connected" } else { "Offline" }
                }
                button {
                    class: "icon-btn",
                    title: if theme == Theme::Dark { "Light theme" } else { "Dark theme" },
                    onclick: move |_| {
                        let next = session.read().theme.toggled();
                        session.write().set_theme(next);
                    },
                    Icon { name: if theme == Theme::Dark { "sun" } else { "moon" }, size: 16 }
                }
                div { class: "user-chip",
                    span { class: "avatar sm user", "Y" }
                    span { "Owner" }
                }
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
        live: bool,
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
fn Topbar(
    session: Signal<Session>,
    overlay: Signal<Overlay>,
    inspector_open: Signal<bool>,
    drawer_open: Signal<bool>,
) -> Element {
    let mut overlay = overlay;
    let mut inspector_open = inspector_open;
    let mut drawer_open = drawer_open;
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
    let connected = session.read().state.connected;
    let bots = session.read().state.listed_bots.len();
    let bots_loaded = session.read().state.bots_loaded;
    let theme = session.read().theme;
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
                .take(5)
                .collect::<Vec<_>>()
        } else {
            team_members
        };
        source
            .into_iter()
            .map(|id| {
                (
                    id.clone(),
                    state.bot_display_name(&id),
                    state.bot_avatar(&id),
                )
            })
            .collect::<Vec<_>>()
    };
    rsx! {
        header { class: "topbar glass",
            div { class: "topbar-title",
                button {
                    class: "icon-btn drawer-toggle",
                    title: "Open navigation",
                    onclick: move |_| drawer_open.set(true),
                    Icon { name: "list", size: 16 }
                }
                strong { "{title}" }
                if let Some(kind) = kind {
                    span { class: "chip muted", "{kind}" }
                }
                if let Some(bot) = active_bot.clone() {
                    span { class: "chip accent", "bot chat" }
                    button {
                        class: "btn ghost sm",
                        onclick: move |_| session.write().open_bot(bot.clone()),
                        "Profile"
                    }
                }
                button {
                    class: "avatar-stack",
                    title: "Team",
                    onclick: move |_| {
                        session.write().refresh_team();
                        overlay.set(Overlay::Team);
                    },
                    for (id, _name, avatar) in team {
                        span { key: "{id}", class: "avatar sm bot", style: avatar_style(&id), "{avatar}" }
                    }
                }
            }
            div { class: "topbar-actions",
                if session.read().state.task.is_some() {
                    span { class: "spinner", title: "A turn is running" }
                }
                if section == Section::Chat {
                    ModelSelect { session }
                }
                button {
                    class: "icon-btn",
                    title: if theme == Theme::Dark { "Light theme" } else { "Dark theme" },
                    onclick: move |_| {
                        let next = session.read().theme.toggled();
                        session.write().set_theme(next);
                    },
                    Icon { name: if theme == Theme::Dark { "sun" } else { "moon" }, size: 16 }
                }
                button {
                    class: "icon-btn",
                    title: "Refresh",
                    onclick: move |_| session.write().refresh(),
                    Icon { name: "refresh", size: 16 }
                }
                button {
                    class: "icon-btn",
                    title: "Inspector",
                    onclick: move |_| {
                        let next = !inspector_open();
                        inspector_open.set(next);
                    },
                    Icon { name: "dots", size: 16 }
                }
                if section == Section::Chat {
                    button {
                        class: "btn ghost sm",
                        onclick: move |_| session.write().open_section(Section::Fleet),
                        if bots_loaded { "Bots {bots}" } else { "Bots" }
                    }
                }
                span { class: if connected { "chip ok" } else { "chip danger" },
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
            div { class: "banner err", span { "{message}" } }
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
                span { class: "chip", "{label}" }
                span { class: "tiny", "{bot} · {id}" }
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
        div { class: "banner",
            span { strong { "Update v{version} available" } span { class: "tiny", " · {size}" } }
            div { class: "row",
                button {
                    class: "btn primary sm",
                    onclick: move |_| {
                        if session.write().apply_update(&install).is_ok() {
                            std::process::exit(0);
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

#[component]
fn ChatView(session: Signal<Session>, overlay: Signal<Overlay>) -> Element {
    let conversation_kind = session.read().conversation_kind().map(str::to_owned);
    let group_like = matches!(conversation_kind.as_deref(), Some("group") | Some("room"));
    let orchestrator = session
        .read()
        .state
        .listed_bots
        .iter()
        .find(|bot| bot.role == "orchestrator")
        .map(|bot| bot.id.clone());
    let messages: Vec<ChatRow> = {
        let state = session.read();
        state
            .state
            .messages
            .iter()
            .map(|message| {
                let user = message.role == Role::User;
                let (speaker, avatar, bot) = state.message_speaker(message);
                let peer = !user
                    && group_like
                    && bot.as_deref().is_some()
                    && bot.as_deref() != orchestrator.as_deref();
                (
                    message.id.clone(),
                    user,
                    speaker,
                    avatar,
                    bot,
                    stamp(&message.created_at),
                    message.content.clone(),
                    peer,
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
                    bot.enabled,
                    bot.live,
                    bot.role.clone(),
                )
            })
            .collect::<Vec<RosterRow>>();
        (active, name, avatar, roster)
    };
    let bot_meta: HashMap<String, (String, bool)> = roster
        .iter()
        .map(|(id, _, _, _, live, role)| (id.clone(), (role.clone(), *live)))
        .collect();
    let talking_to: Vec<(String, String, String)> = {
        let state = session.read();
        let member_ids = state
            .state
            .team
            .as_ref()
            .map(teams::member_ids)
            .unwrap_or_default();
        let ids = if member_ids.is_empty() {
            roster
                .iter()
                .filter(|(_, _, _, enabled, _, _)| *enabled)
                .map(|(id, _, _, _, _, _)| id.clone())
                .take(5)
                .collect::<Vec<_>>()
        } else {
            member_ids
        };
        ids.into_iter()
            .map(|id| {
                (
                    id.clone(),
                    state.bot_display_name(&id),
                    state.bot_avatar(&id),
                )
            })
            .collect::<Vec<_>>()
    };
    let mut team_collapsed = use_signal(|| false);
    let mut recording = use_signal(|| false);
    // The recorder is owned by the signal but taken out for every capture, so
    // no borrow of the signal is alive across an await point.
    let mut recorder = use_signal(MicRecorder::new);
    let transcript_error = session.read().state.transcript_error.clone();
    use_effect(move || {
        let transcript = session.read().state.transcript.clone();
        let Some(text) = transcript else {
            return;
        };
        let mut session = session;
        let mut composer = session.write();
        composer.state.transcript = None;
        if composer.composer.trim().is_empty() {
            composer.composer = text;
        } else {
            composer.composer = format!("{}\n{}", composer.composer, text);
        }
    });
    use_effect(move || {
        let _ = session.read().state.messages.len();
        let _ = dioxus::document::eval(
            "const el=document.getElementById('messages'); if(el){el.scrollTop=el.scrollHeight;}",
        );
    });
    let empty = messages.is_empty();
    let is_one_to_one = session.read().conversation.starts_with("dir:");
    let room = teams::is_room(&session.read().conversation);
    let in_team: Vec<String> = session
        .read()
        .state
        .team
        .as_ref()
        .map(teams::member_ids)
        .unwrap_or_default();
    rsx! {
        div { class: "pane-body chat-body",
            div { class: "chat-column",
                div { class: "chat-head",
                    div { class: "row",
                        span { class: "micro", "Conversation" }
                        ModelBar { session }
                    }
                    NewMenu { session, overlay, right: true }
                }
                if let Some((status, label)) = task {
                    if status == TaskStatus::AwaitingApproval {
                        div { class: "banner warn",
                            span { "{label}" }
                            button {
                                class: "btn ghost sm",
                                onclick: move |_| session.write().open_section(Section::Approvals),
                                "Review approvals"
                            }
                        }
                    }
                }
                div { class: "messages", id: "messages",
                    if empty {
                        div { class: "empty",
                            div {
                                class: "empty-icon",
                                style: active_bot.as_deref().map(avatar_style).unwrap_or_default(),
                                span { class: "avatar lg bot", style: active_bot.as_deref().map(avatar_style).unwrap_or_default(), "{active_avatar}" }
                            }
                            h2 { class: "empty-title",
                                if !accepts {
                                    "This node is read-only."
                                } else if is_one_to_one {
                                    "Chat with {active_name}"
                                } else {
                                    "Start a focused turn."
                                }
                            }
                            p { class: "empty-text",
                                if !accepts {
                                    "Sections and groups organize the tree. Open a direct thread or a room to talk."
                                } else if is_one_to_one {
                                    "A one-to-one conversation. Replies come from {active_name}, not the orchestrator."
                                } else {
                                    "Ask the orchestrator, or open a room with your specialists."
                                }
                            }
                            div { class: "roster",
                                for (bot_id, bot_name, bot_avatar, bot_enabled, bot_live, bot_role) in roster.clone() {
                                    div {
                                        key: "{bot_id}",
                                        class: "roster-row",
                                        span { class: "avatar bot", style: avatar_style(&bot_id), "{bot_avatar}" }
                                        span { class: "thread-title", "{bot_name}" }
                                        span { class: "thread-meta", "{bot_role}" }
                                        if !bot_enabled {
                                            span { class: "chip warn", "disabled" }
                                        } else if !bot_live {
                                            span { class: "chip warn", "restart required" }
                                        }
                                        button {
                                            class: "btn ghost sm",
                                            onclick: {
                                                let bot_id = bot_id.clone();
                                                move |_| session.write().open_bot_chat(bot_id.clone())
                                            },
                                            "1:1"
                                        }
                                        if room {
                                            button {
                                                class: "btn ghost sm",
                                                disabled: !bot_enabled || in_team.contains(&bot_id),
                                                title: if in_team.contains(&bot_id) { "Already in the team" } else { "Add to the team" },
                                                onclick: {
                                                    let bot_id = bot_id.clone();
                                                    move |_| session.write().add_team_member(bot_id.clone())
                                                },
                                                "add to team"
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                    for (id, user, speaker, avatar, bot, stamp, content, peer) in messages {
                        div {
                            key: "{id}",
                            class: if user {
                                "msg user"
                            } else if peer {
                                "msg bot peer"
                            } else {
                                "msg bot"
                            },
                            span {
                                class: if user { "msg-avatar avatar user" } else { "msg-avatar avatar bot" },
                                style: bot.as_deref().map(avatar_style).unwrap_or_default(),
                                "{avatar}"
                            }
                            div { class: "msg-main",
                                div { class: "msg-head",
                                    span { class: "msg-name", "{speaker}" }
                                    span { class: "msg-time", "{stamp}" }
                                    if let Some(id) = bot.as_ref() {
                                        if let Some((role, live)) = bot_meta.get(id) {
                                            span { class: "team-chip", "{role}" }
                                            span {
                                                class: if *live { "chip ok" } else { "chip warn" },
                                                if *live { "live" } else { "not live" }
                                            }
                                        }
                                    }
                                    if peer {
                                        span { class: "team-chip", "peer bot" }
                                    }
                                }
                                div { class: "msg-body", "{content}" }
                            }
                        }
                    }
                }
                div { class: "composer-wrap",
                    if let Some(message) = transcript_error {
                        div { class: "banner err", span { "{message}" } }
                    }
                    div { class: "composer",
                        textarea {
                            class: "composer-input",
                            id: "composer-input",
                            value: "{composer}",
                            placeholder: if accepts { format!("Message {active_name}…") } else { "This node does not accept turns".into() },
                            disabled: !accepts,
                            oninput: move |event| session.write().composer = event.value(),
                            onkeydown: move |event: KeyboardEvent| {
                                if event.key() == Key::Enter
                                    && !event.modifiers().contains(Modifiers::SHIFT)
                                {
                                    event.prevent_default();
                                    session.write().submit();
                                }
                            },
                        }
                        div { class: "composer-row",
                            NewMenu { session, overlay }
                            div { class: "team-row",
                                if group_like {
                                    span { class: "chip muted", "Talking to" }
                                    button {
                                        class: "icon-btn",
                                        title: if team_collapsed() { "Show the team" } else { "Hide the team" },
                                        onclick: move |_| team_collapsed.set(!team_collapsed()),
                                        Icon { name: if team_collapsed() { "chevron-right" } else { "chevron-down" }, size: 14 }
                                    }
                                    if !team_collapsed() {
                                        for (bot_id, bot_name, bot_avatar) in talking_to.iter() {
                                            span { key: "{bot_id}", class: "team-chip",
                                                span { class: "avatar sm bot", style: avatar_style(bot_id), "{bot_avatar}" }
                                                "{bot_name}"
                                            }
                                        }
                                    }
                                }
                            }
                            div { class: "composer-actions",
                                button {
                                    title: if recording() { "Stop recording" } else { "Record a voice note" },
                                    class: if recording() { "composer-mic recording" } else { "composer-mic" },
                                    onclick: move |_| async move {
                                        if recording() {
                                            // Take the recorder out of the signal
                                            // for the capture and put it back
                                            // afterwards: a borrow that spans
                                            // an await is not allowed.
                                            let mut device = recorder.take();
                                            let result = device.stop().await;
                                            let filename = device.filename().to_owned();
                                            recorder.set(device);
                                            recording.set(false);
                                            match result {
                                                Ok(bytes) => {
                                                    session.write().transcribe_audio(filename, bytes);
                                                }
                                                Err(message) => {
                                                    session.write().state.transcript_error = Some(message);
                                                }
                                            }
                                        } else {
                                            let mut device = recorder.take();
                                            let result = device.start().await;
                                            recorder.set(device);
                                            match result {
                                                Ok(()) => {
                                                    recording.set(true);
                                                    session.write().state.transcript_error = None;
                                                }
                                                Err(message) => {
                                                    session.write().state.transcript_error = Some(message);
                                                }
                                            }
                                        }
                                    },
                                    Icon { name: if recording() { "check" } else { "mic" }, size: 16 }
                                }
                                // TODO(desktop-features): attachments need `session::Session::attach_file(&mut self, path: String) -> Option<String>` — the client has no inbox upload path yet, so the button stays disabled.
                                button {
                                    class: "composer-mic",
                                    title: "Attachments are not available yet",
                                    disabled: true,
                                    Icon { name: "paperclip", size: 16 }
                                }
                                button {
                                    class: "btn primary",
                                    disabled: !can_send || composer.trim().is_empty(),
                                    onclick: move |_| session.write().submit(),
                                    Icon { name: "send", size: 16 }
                                    "Send"
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
fn ModelSelect(session: Signal<Session>) -> Element {
    let (options, value, allowed, changed) = {
        let session = session.read();
        let catalog = session.state.catalog.as_ref();
        let mut options = vec![("auto".to_owned(), String::new())];
        if let Some(catalog) = catalog {
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
        }
        let draft = session.state.draft.clone();
        let value = if draft.is_auto() {
            "auto".to_owned()
        } else {
            format!("{}|{}", draft.provider, draft.model)
        };
        let saved = catalog
            .map(|catalog| {
                if catalog.selection.is_auto() {
                    "auto".to_owned()
                } else {
                    format!("{}|{}", catalog.selection.provider, catalog.selection.model)
                }
            })
            .unwrap_or_else(|| "auto".to_owned());
        let allowed = catalog.is_some_and(|catalog| catalog.allows(&draft));
        let changed = value != saved;
        (options, value, allowed, changed)
    };
    rsx! {
        div { class: "row",
            select {
                class: "select model-select",
                value: "{value}",
                title: "Model for the next turn",
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
                button {
                    class: "btn primary sm",
                    disabled: !allowed,
                    onclick: move |_| {
                        let draft = session.read().state.draft.clone();
                        session.write().apply_model(draft);
                    },
                    "Apply"
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
            span { class: "micro", "Model" }
            select {
                class: "select model-select",
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
                span { class: "tiny", "Egress-approved providers only" }
                button {
                    class: "btn primary sm",
                    disabled: !allowed,
                    onclick: move |_| {
                        let draft = session.read().state.draft.clone();
                        session.write().apply_model(draft);
                    },
                    "Apply"
                }
            } else {
                span { class: "tiny", "Saved {saved}" }
            }
        }
    }
}

/// Contextual right pane: bot profile, conversation details, provider and
/// connector summaries.
#[component]
fn Inspector(session: Signal<Session>, overlay: Signal<Overlay>, open: Signal<bool>) -> Element {
    let mut open = open;
    let section = session.read().section;
    let conversation = session.read().conversation.clone();
    let kind = session.read().conversation_kind().map(str::to_owned);
    let accepts = session.read().conversation_accepts_turns();
    let title = session.read().conversation_title();
    let selected_bot = session.read().selected_bot.clone();
    let providers = session.read().state.providers.clone();
    let connectors = session.read().state.connectors.clone();
    let roster = {
        let state = session.read();
        state
            .state
            .listed_bots
            .iter()
            .map(|bot| {
                (
                    bot.id.clone(),
                    state.bot_display_name(&bot.id),
                    state.bot_avatar(&bot.id),
                    bot.role.clone(),
                    bot.enabled,
                    bot.live,
                    bot.model.clone(),
                )
            })
            .collect::<Vec<_>>()
    };
    let fleet_bot = roster
        .iter()
        .find(|(id, _, _, _, _, _, _)| Some(id.clone()) == selected_bot)
        .cloned();
    rsx! {
        aside { class: if open() { "inspector open" } else { "inspector" },
            div { class: "inspector-head",
                h2 { class: "inspector-title", "Inspector" }
                button {
                    class: "icon-btn",
                    title: "Hide inspector",
                    onclick: move |_| open.set(false),
                    Icon { name: "x", size: 16 }
                }
            }
            div { class: "inspector-body",
                if section == Section::Chat {
                    div { class: "card",
                        div { class: "card-head",
                            h3 { class: "card-title", "{title}" }
                            if let Some(kind) = kind.clone() {
                                span { class: "chip muted", "{kind}" }
                            }
                        }
                        p { class: "card-sub", "{conversation}" }
                        div { class: "chips",
                            span {
                                class: if accepts { "chip ok" } else { "chip warn" },
                                if accepts { "accepts turns" } else { "read only" }
                            }
                        }
                    }
                    div { class: "card",
                        div { class: "card-head",
                            h3 { class: "card-title", "Roster" }
                            button {
                                class: "btn ghost sm",
                                onclick: move |_| {
                                    session.write().refresh_team();
                                    overlay.set(Overlay::Team);
                                },
                                "Manage"
                            }
                        }
                        div { class: "roster",
                            for (id, name, avatar, role, enabled, live, model) in roster.clone() {
                                div { key: "{id}", class: "roster-row",
                                    span { class: "avatar sm bot", style: avatar_style(&id), "{avatar}" }
                                    span { class: "thread-title", "{name}" }
                                    span { class: "thread-meta", "{role}" }
                                    span {
                                        class: if enabled && live { "chip ok" } else { "chip warn" },
                                        if enabled && live { "live" } else { "offline" }
                                    }
                                    if let Some(model) = model {
                                        span { class: "tiny", "{human_model(&model)}" }
                                    }
                                }
                            }
                        }
                    }
                } else if section == Section::Fleet && selected_bot.is_some() {
                    div { class: "card",
                        if let Some((id, name, avatar, role, enabled, live, model)) = fleet_bot {
                            div { class: "row",
                                span { class: "avatar lg bot", style: avatar_style(&id), "{avatar}" }
                                div {
                                    h3 { class: "card-title", "{name}" }
                                    span { class: "card-sub", "{role}" }
                                }
                            }
                            div { class: "chips",
                                span {
                                    class: if enabled && live { "chip ok" } else { "chip warn" },
                                    if enabled && live { "live" } else { "not live" }
                                }
                                if let Some(model) = model {
                                    span { class: "chip", "Model: {human_model(&model)}" }
                                }
                            }
                        }
                        button {
                            class: "btn ghost sm",
                            onclick: move |_| {
                                if let Some(id) = selected_bot.clone() {
                                    session.write().open_bot(id);
                                }
                            },
                            "Open profile"
                        }
                    }
                } else if section == Section::Providers {
                    div { class: "card",
                        h3 { class: "card-title", "Providers" }
                        for provider in providers {
                            div { key: "{provider.id}", class: "stack-row",
                                span { "{provider.id}" }
                                span { class: "tiny", "{provider.kind}" }
                            }
                        }
                    }
                } else if section == Section::Connectors {
                    div { class: "card",
                        h3 { class: "card-title", "Connectors" }
                        for connector in connectors {
                            div { key: "{connector.id}", class: "stack-row",
                                span { "{connector.id}" }
                                span { class: "tiny", "{connector.kind}" }
                            }
                        }
                    }
                } else {
                    div { class: "empty",
                        div { class: "empty-icon", Icon { name: "dots", size: 20 } }
                        h3 { class: "empty-title", "Nothing to inspect" }
                        p { class: "empty-text", "Open a chat, a bot profile, or a provider to see details here." }
                    }
                }
            }
        }
    }
}

#[component]
fn NewConversationSheet(session: Signal<Session>, overlay: Signal<Overlay>) -> Element {
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
    let (members, candidates) = {
        let state = session.read();
        let roster = state
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
            .collect::<Vec<_>>();
        let selected = roster
            .iter()
            .filter(|(id, ..)| picked().contains(id))
            .cloned()
            .collect::<Vec<_>>();
        let available = teams::available_bots(&state.state.listed_bots, &draft_team)
            .into_iter()
            .filter_map(|id| roster.iter().find(|(bot, ..)| *bot == id).cloned())
            .collect::<Vec<_>>();
        (selected, available)
    };
    let picked_count = picked().len();
    let members_empty = members.is_empty();
    let candidates_empty = candidates.is_empty();
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
                        for candidate in ConversationKind::ALL {
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
                div { class: "field",
                    label { class: "field-label", "Id" }
                    input {
                        class: "input",
                        value: "{slug}",
                        placeholder: "id (lowercase slug)",
                        oninput: move |event| session.write().fleet.conv_slug = event.value(),
                    }
                    span { class: "hint", "The server assigns the id prefix (sec:, grp:, dir:, room:)." }
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
                        label { class: "field-label", "Initial team" }
                        span { class: "hint", "{picked_count} of 5 bots picked. The room is created first, then its team is written in the same step." }
                        div { class: "roster",
                            for (bot_id, bot_name, bot_avatar, bot_role) in members {
                                div { key: "{bot_id}", class: "roster-row",
                                    span { class: "avatar sm bot", style: avatar_style(&bot_id), "{bot_avatar}" }
                                    span { class: "thread-title", "{bot_name}" }
                                    span { class: "thread-meta", "{bot_role}" }
                                    button {
                                        class: "team-chip remove",
                                        title: "Remove from team",
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
                        div { class: "roster",
                            for (bot_id, bot_name, bot_avatar, _) in candidates {
                                button {
                                    key: "{bot_id}",
                                    class: "roster-row",
                                    onclick: {
                                        let id = bot_id.clone();
                                        let team = draft_team.clone();
                                        move |_| {
                                            let mut picked = picked;
                                            if let Some(next) = teams::with_member(&team, &id) {
                                                picked.set(next);
                                            }
                                        }
                                    },
                                    span { class: "avatar sm bot", style: avatar_style(&bot_id), "{bot_avatar}" }
                                    span { class: "thread-title", "{bot_name}" }
                                    span { class: "chip accent", "add" }
                                }
                            }
                            if candidates_empty {
                                p { class: "hint", "No other enabled bot can join this team." }
                            }
                        }
                    }
                } else if kind == ConversationKind::Group {
                    div { class: "field",
                        label { class: "field-label", "Team" }
                        span { class: "hint", "Only rooms carry a team. Create a room instead, then pick its members in the Team sheet." }
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
                        let slug = session.read().fleet.conv_slug.clone();
                        let title = session.read().fleet.conv_title.clone();
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
fn TeamSheet(session: Signal<Session>, overlay: Signal<Overlay>) -> Element {
    let mut overlay = overlay;
    let mut bot_query = use_signal(String::new);
    let conversation = session.read().conversation.clone();
    let is_room = teams::is_room(&conversation);
    let (members, available, mode, loading, error) = {
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
        let available = team
            .as_ref()
            .map(|team| {
                teams::available_bots(&state.state.listed_bots, team)
                    .into_iter()
                    .map(|id| {
                        (
                            id.clone(),
                            state.bot_display_name(&id),
                            state.bot_avatar(&id),
                        )
                    })
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        (
            members,
            available,
            team.as_ref().map(|team| team.mode),
            state.state.team_loading,
            state.state.team_error.clone(),
        )
    };
    let needle = bot_query().trim().to_lowercase();
    let mode_value = mode
        .map(|mode| mode.as_str())
        .unwrap_or("selected")
        .to_owned();
    let candidates = available
        .into_iter()
        .filter(|(_, name, _)| needle.is_empty() || name.to_lowercase().contains(&needle))
        .collect::<Vec<_>>();
    let candidates_empty = candidates.is_empty();
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
                h2 { class: "sheet-title", "Team" }
                div { class: "row",
                    if loading {
                        span { class: "spinner", title: "Loading the team" }
                    }
                    button {
                        class: "icon-btn",
                        title: "Refresh the team",
                        onclick: move |_| session.write().refresh_team(),
                        Icon { name: "refresh", size: 16 }
                    }
                    button {
                        class: "icon-btn escape-close",
                        title: "Close",
                        onclick: move |_| overlay.set(Overlay::None),
                        Icon { name: "x", size: 16 }
                    }
                }
            }
            div { class: "sheet-body",
                p { class: "hint", "Room: {conversation}" }
                if let Some(error) = error {
                    div { class: "banner err", span { "{error}" } }
                }
                if !is_room {
                    div { class: "banner warn",
                        span { "Teams are only available for rooms (room: conversations)." }
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
                                        title: "Remove from team",
                                        onclick: {
                                            let id = id.clone();
                                            move |_| session.write().remove_team_member(id.clone())
                                        },
                                        Icon { name: "x", size: 13 }
                                    }
                                }
                            }
                        }
                        span { class: "hint", "The team holds up to five enabled bots. The orchestrator is always implied." }
                    }
                    div { class: "field",
                        label { class: "field-label", "Add bot" }
                        input {
                            class: "input",
                            value: "{bot_query}",
                            placeholder: "Search enabled bots",
                            oninput: move |event| bot_query.set(event.value()),
                        }
                        div { class: "roster",
                            for (id, name, avatar) in candidates {
                                button {
                                    key: "{id}",
                                    class: "roster-row",
                                    onclick: {
                                        let id = id.clone();
                                        move |_| session.write().add_team_member(id.clone())
                                    },
                                    span { class: "avatar sm bot", style: avatar_style(&id), "{avatar}" }
                                    span { class: "thread-title", "{name}" }
                                    span { class: "chip accent", "add" }
                                }
                            }
                            if candidates_empty {
                                p { class: "hint", "No other enabled bot can join this team." }
                            }
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
        div { class: "pane",
            div { class: "pane-head",
                div {
                    h1 { class: "pane-title", "Fleet" }
                    p { class: "muted", "Specialists, tools, and approval rules." }
                }
                button {
                    class: "btn ghost",
                    onclick: move |_| session.write().refresh_bots(),
                    Icon { name: "refresh", size: 16 }
                    "Refresh"
                }
            }
            div { class: "pane-body",
                if !bot_binding {
                    div { class: "banner warn",
                        span { "This server does not route one-to-one bot chats yet. Update the server to 0.5.0 or later." }
                    }
                }
                div { class: "pane-section",
                details { class: "card",
                    summary { "New specialist" }
                    div { class: "form-grid",
                        div { class: "field",
                            label { class: "field-label", "Bot id" }
                            input {
                                class: "input",
                                value: "{bot_id}",
                                placeholder: "bot id (lowercase slug)",
                                oninput: move |event| bot_id.set(event.value()),
                            }
                        }
                        div { class: "field",
                            label { class: "field-label", "Skills" }
                            input {
                                class: "input",
                                value: "{bot_skills}",
                                placeholder: "skills: ask, notes",
                                oninput: move |event| bot_skills.set(event.value()),
                            }
                        }
                    }
                    div { class: "field",
                        label { class: "field-label", "Description" }
                        textarea {
                            class: "textarea",
                            value: "{bot_desc}",
                            placeholder: "What this specialist is for",
                            oninput: move |event| bot_desc.set(event.value()),
                        }
                    }
                    details {
                        summary { class: "tiny", "Tools" }
                        div { class: "form-grid",
                            for (index, name) in SPECIALIST_TOOLS.iter().enumerate() {
                                label { key: "{index}", class: "check",
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
                    p { class: "tiny", "The bot stays disabled until serabot.service restarts." }
                }
                details { class: "card",
                    summary { "New skill pack" }
                    div { class: "form-grid",
                        div { class: "field",
                            label { class: "field-label", "Skill id" }
                            input {
                                class: "input",
                                value: "{skill_id}",
                                placeholder: "skill id",
                                oninput: move |event| skill_id.set(event.value()),
                            }
                        }
                        div { class: "field",
                            label { class: "field-label", "Description" }
                            input {
                                class: "input",
                                value: "{skill_desc}",
                                placeholder: "One-line description",
                                oninput: move |event| skill_desc.set(event.value()),
                            }
                        }
                    }
                    div { class: "field",
                        label { class: "field-label", "Body" }
                        textarea {
                            class: "textarea",
                            value: "{skill_body}",
                            placeholder: "Optional markdown body",
                            oninput: move |event| skill_body.set(event.value()),
                        }
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
                }
                div { class: "card-grid",
                    if !bots_loaded {
                        div { class: "card",
                            div { class: "skeleton title" }
                            div { class: "skeleton" }
                            div { class: "skeleton block" }
                        }
                    }
                    for bot in bots {
                        div { class: "card", key: "{bot.id}",
                            div { class: "card-head",
                                div { class: "row",
                                    span { class: "avatar lg bot", style: avatar_style(&bot.id), "{bot.avatar}" }
                                    div {
                                        h3 { class: "card-title", "{bot.display_name}" }
                                        span { class: "card-sub", "{bot.id} · {bot.role}" }
                                    }
                                }
                                div { class: "chips",
                                    span { class: if bot.enabled { "chip ok" } else { "chip muted" }, if bot.enabled { "enabled" } else { "disabled" } }
                                    if !bot.live {
                                        span { class: "chip warn", "restart required" }
                                    }
                                }
                            }
                            if !bot.description.is_empty() {
                                p { class: "card-sub", "{bot.description}" }
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
                            div { class: "card-actions",
                                button {
                                    class: "btn primary sm",
                                    disabled: !bot.enabled || !bot_binding,
                                    onclick: {
                                        let id = bot.id.clone();
                                        move |_| session.write().open_bot_chat(id.clone())
                                    },
                                    "1:1 chat"
                                }
                                button {
                                    class: "btn ghost sm",
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
                        div { class: "empty",
                            div { class: "empty-icon", Icon { name: "robot", size: 20 } }
                            h3 { class: "empty-title", "No bots loaded yet" }
                            p { class: "empty-text", "Refresh, or create a specialist on the server." }
                        }
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
            div { class: "pane",
                div { class: "pane-head",
                    div {
                        h1 { class: "pane-title", "Bot profile" }
                        p { class: "muted", "Loading {id}…" }
                    }
                    button {
                        class: "btn ghost",
                        onclick: move |_| session.write().close_bot(),
                        "Back to fleet"
                    }
                }
                div { class: "pane-body",
                    div { class: "empty",
                        div { class: "empty-icon", Icon { name: "robot", size: 20 } }
                        h3 { class: "empty-title", "This bot is not in the roster yet." }
                    }
                }
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
        div { class: "pane",
            div { class: "pane-head",
                div {
                    h1 { class: "pane-title", "{display_name}" }
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
            div { class: "pane-body",
                div { class: "card-grid",
                    div { class: "card",
                        div { class: "row",
                            span { class: "avatar lg bot", style: avatar_style(&bot.id), "{avatar}" }
                            div {
                                h3 { class: "card-title", "{display_name}" }
                                span { class: "card-sub", "{bot.id} · {bot.role}" }
                            }
                        }
                        div { class: "chips",
                            span { class: if bot.enabled { "chip ok" } else { "chip muted" }, if bot.enabled { "enabled" } else { "disabled" } }
                            span { class: if bot.live { "chip ok" } else { "chip warn" }, if bot.live { "live" } else { "restart required" } }
                            span { class: "chip accent", "{bot.role}" }
                        }
                        if !bot_description.is_empty() {
                            p { class: "card-sub", "{bot_description}" }
                        } else {
                            p { class: "tiny", "No description yet. Add one below so the roster explains itself." }
                        }
                        if !bot.enabled {
                            p { class: "tiny", "This bot is disabled in its bot file. Enable it there and restart the server before chatting." }
                        }
                        if !bot_binding {
                            p { class: "tiny", "This server does not route one-to-one bot chats yet. Update the server to 0.5.0 or later." }
                        }
                        div { class: "chips",
                            span { class: "chip", if let Some(model) = bot_model.clone() { "Model: {human_model(&model)}" } else { "Model: conversation route" } }
                        }
                        p { class: "tiny", "Fallback models are not available yet; each turn uses the conversation route." }
                    }
                    div { class: "card",
                        div { class: "card-head",
                            h3 { class: "card-title", "Profile" }
                        }
                        p { class: "tiny", "Display name, avatar and description belong to the console. Behavior, tools and model stay in the bot file." }
                        p { class: "tiny", "Use an emoji or initials for the avatar. Image uploads are not supported yet." }
                        div { class: "form-grid",
                            div { class: "field",
                                label { class: "field-label", "Display name" }
                                input {
                                    class: "input",
                                    value: "{draft_name}",
                                    placeholder: "Display name",
                                    oninput: move |event| session.write().bot_profile.display_name = event.value(),
                                }
                            }
                            div { class: "field",
                                label { class: "field-label", "Avatar" }
                                input {
                                    class: "input",
                                    value: "{draft_avatar}",
                                    placeholder: "Avatar emoji or initials",
                                    oninput: move |event| session.write().bot_profile.avatar = event.value(),
                                }
                            }
                        }
                        div { class: "field",
                            label { class: "field-label", "Description" }
                            textarea {
                                class: "textarea",
                                value: "{draft_description}",
                                placeholder: "What this bot is for",
                                oninput: move |event| session.write().bot_profile.description = event.value(),
                            }
                        }
                        button {
                            class: "btn primary",
                            disabled: !can_save,
                            onclick: move |_| session.write().save_bot_profile(),
                            "Save profile"
                        }
                    }
                    div { class: "card",
                        div { class: "card-head",
                            h3 { class: "card-title", "Skills" }
                        }
                        if skills.is_empty() {
                            p { class: "tiny", "No skills attached to this bot." }
                        } else {
                            div { class: "stack",
                                for (name, description) in skills {
                                    div { class: "stack-row",
                                        strong { "{name}" }
                                        if !description.is_empty() {
                                            span { class: "tiny", "{description}" }
                                        }
                                    }
                                }
                            }
                        }
                    }
                    div { class: "card",
                        div { class: "card-head",
                            h3 { class: "card-title", "Can produce" }
                        }
                        if capabilities.is_empty() {
                            p { class: "tiny", "No output tools are attached to this bot." }
                        } else {
                            div { class: "chips",
                                for label in capabilities {
                                    span { class: "chip accent", "{label}" }
                                }
                            }
                            p { class: "tiny", "Generated decks and media are downloadable from their approvals and artifact links; per-bot generation history is not indexed yet." }
                        }
                    }
                    div { class: "card",
                        div { class: "card-head",
                            h3 { class: "card-title", "Routines" }
                        }
                        if is_watcher {
                            p { class: "tiny", "The watcher runs the morning brief and watched-source checks from the server schedule." }
                        } else {
                            p { class: "tiny", "No per-bot routine yet. Routines currently run on the Watcher schedule only." }
                        }
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
        div { class: "pane",
            div { class: "pane-head",
                div {
                    h1 { class: "pane-title", "Approvals" }
                    p { class: "muted", "Each decision binds the exact payload, destination, hash, policy and expiry." }
                }
                button {
                    class: "btn ghost",
                    onclick: move |_| session.write().refresh_approvals(),
                    "Refresh list"
                }
            }
            div { class: "pane-body",
                div { class: "card-grid",
                    if !approvals_loaded {
                        div { class: "card",
                            div { class: "skeleton title" }
                            div { class: "skeleton block" }
                        }
                    }
                    for (id, tool, summary, expires) in approvals {
                        div { class: "card", key: "{id}",
                            div { class: "card-head",
                                span { class: "chip accent", "{tool}" }
                                span { class: "tiny", "expires {expires}" }
                            }
                            p { class: "card-sub", "{summary}" }
                            div { class: "card-actions",
                                button {
                                    class: "btn primary sm",
                                    onclick: {
                                        let id = id.clone();
                                        move |_| session.write().decide(id.clone(), ApprovalDecision::Once)
                                    },
                                    "Approve once"
                                }
                                button {
                                    class: "btn ghost sm",
                                    onclick: {
                                        let id = id.clone();
                                        move |_| session.write().decide(id.clone(), ApprovalDecision::Always)
                                    },
                                    "Always"
                                }
                                button {
                                    class: "btn danger sm",
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
                        div { class: "empty",
                            div { class: "empty-icon", Icon { name: "shield", size: 20 } }
                            h3 { class: "empty-title", "Nothing pending" }
                            p { class: "empty-text", "Gateway requests appear here." }
                        }
                    }
                }
            }
        }
    }
}

/// The "Add provider" sheet.
///
/// A preset fills the id, base URL, kind and secret name, and offers its
/// model ids as a starting list. The owner pastes the API key; the write
/// goes through the normal admin path (`Session::save_provider`).
#[component]
fn AddProviderSheet(session: Signal<Session>, overlay: Signal<Overlay>) -> Element {
    let mut overlay = overlay;
    let mut selected = use_signal(String::new);
    let choices = provider_presets::presets()
        .iter()
        .map(|preset| (preset.id, preset.label))
        .collect::<Vec<_>>();
    let chosen = provider_presets::preset_for(&selected()).copied();
    let id = session.read().provider.id.clone();
    let kind = session.read().provider.kind.clone();
    let base_url = session.read().provider.base_url.clone();
    let models = session.read().provider.models.clone();
    let secret_name = session.read().provider.secret_name.clone();
    let api_key = session.read().provider.api_key.clone();
    let catalog_fetch = session.read().provider.catalog_fetch;
    let can_save = session.read().can_admin();
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
                h2 { class: "sheet-title", "Add provider" }
                button {
                    class: "icon-btn escape-close",
                    title: "Close",
                    onclick: move |_| overlay.set(Overlay::None),
                    Icon { name: "x", size: 16 }
                }
            }
            div { class: "sheet-body",
                div { class: "field",
                    label { class: "field-label", "Preset" }
                    select {
                        class: "select",
                        value: "{selected()}",
                        onchange: move |event| {
                            let id = event.value();
                            let Some(preset) = provider_presets::preset_for(&id).copied() else {
                                return;
                            };
                            let mut session = session;
                            let mut draft = session.write();
                            draft.provider.id = preset.id.to_owned();
                            draft.provider.kind = preset.kind.to_owned();
                            draft.provider.base_url = preset.base_url.to_owned();
                            draft.provider.secret_name = preset.secret_name.to_owned();
                            draft.provider.models = preset.models.join(", ");
                            // The key belongs to one provider; a new preset
                            // starts without one.
                            draft.provider.api_key.clear();
                            selected.set(preset.id.to_owned());
                        },
                        option { value: "", "Choose a preset" }
                        for (preset_id, preset_label) in choices {
                            option { value: "{preset_id}", "{preset_label}" }
                        }
                    }
                }
                if let Some(preset) = chosen {
                    div { class: "card",
                        div { class: "card-head",
                            h3 { class: "card-title", "{preset.label}" }
                            span { class: "chip accent", "{preset.kind}" }
                        }
                        p { class: "tiny", "{preset.note}" }
                        div { class: "chips",
                            for model in preset.models {
                                span { class: "chip", "{model}" }
                            }
                            if preset.models.is_empty() {
                                span { class: "chip warn", "no model list" }
                            }
                        }
                    }
                }
                div { class: "form-grid",
                    div { class: "field",
                        label { class: "field-label", "Provider id" }
                        input {
                            class: "input",
                            value: "{id}",
                            placeholder: "provider id (for example work)",
                            oninput: move |event| session.write().provider.id = event.value(),
                        }
                    }
                    div { class: "field",
                        label { class: "field-label", "Kind" }
                        select {
                            class: "select",
                            value: "{kind}",
                            onchange: move |event| session.write().provider.kind = event.value(),
                            for item in PROVIDER_KINDS {
                                option { value: "{item}", "{item}" }
                            }
                        }
                    }
                    div { class: "field",
                        label { class: "field-label", "Base URL" }
                        input {
                            class: "input",
                            value: "{base_url}",
                            placeholder: "https://api.example.com/v1",
                            oninput: move |event| session.write().provider.base_url = event.value(),
                        }
                    }
                    div { class: "field",
                        label { class: "field-label", "Models" }
                        input {
                            class: "input",
                            value: "{models}",
                            placeholder: "model ids, comma separated",
                            oninput: move |event| session.write().provider.models = event.value(),
                        }
                        span { class: "hint", "At least one model id is required." }
                    }
                    div { class: "field",
                        label { class: "field-label", "Secret name" }
                        input {
                            class: "input",
                            value: "{secret_name}",
                            placeholder: "secret name (for example WORK_API_KEY)",
                            oninput: move |event| session.write().provider.secret_name = event.value(),
                        }
                    }
                    div { class: "field",
                        label { class: "field-label", "API key" }
                        input {
                            class: "input",
                            "type": "password",
                            value: "{api_key}",
                            placeholder: "API key (leave empty if already stored)",
                            oninput: move |event| session.write().provider.api_key = event.value(),
                        }
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
            }
            div { class: "sheet-foot",
                button {
                    class: "btn ghost",
                    onclick: move |_| overlay.set(Overlay::None),
                    "Cancel"
                }
                button {
                    class: "btn primary",
                    disabled: !can_save,
                    onclick: move |_| {
                        session.write().save_provider();
                        overlay.set(Overlay::None);
                    },
                    "Save provider"
                }
            }
        }
    }
}

#[component]
fn ProvidersView(session: Signal<Session>, overlay: Signal<Overlay>) -> Element {
    let mut overlay = overlay;
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
    let provider_rows: Vec<ProviderRow> = providers
        .iter()
        .map(|provider| {
            (
                provider.id.clone(),
                provider.kind.clone(),
                provider.managed,
                provider.catalog_verified,
                provider.catalog_fetch,
                provider.data_egress_approved,
                provider.models.clone(),
                provider.routing_classes.join(", "),
                provider
                    .secrets
                    .iter()
                    .map(|(role, secret)| {
                        (
                            role.clone(),
                            secret.name.clone().unwrap_or_default(),
                            secret.configured,
                        )
                    })
                    .collect(),
            )
        })
        .collect();
    let mut confirm = use_signal(|| Option::<String>::None);
    rsx! {
        div { class: "pane",
            div { class: "pane-head",
                div {
                    h1 { class: "pane-title", "Providers" }
                    p { class: "muted", "Add a provider, pull its live models, and pick the route per conversation." }
                }
                div { class: "row",
                    button {
                        class: "btn primary",
                        onclick: move |_| overlay.set(Overlay::AddProvider),
                        Icon { name: "plus", size: 16 }
                        "Add provider"
                    }
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
            div { class: "pane-body",
                div { class: "card",
                    ModelBar { session }
                    p { class: "tiny", "Applies to the next turn; a running turn keeps its pinned route." }
                }
                if !loaded {
                    div { class: "card",
                        div { class: "skeleton title" }
                        div { class: "skeleton block" }
                    }
                }
                div { class: "card-grid",
                    for (provider_id, provider_kind, managed, catalog_verified, catalog_fetch, egress, models, routing, secrets) in provider_rows {
                        div { class: "card", key: "{provider_id}",
                            div { class: "card-head",
                                div {
                                    h3 { class: "card-title", "{provider_id}" }
                                    span { class: "card-sub", "{provider_kind}" }
                                }
                                if managed {
                                    span { class: "chip accent", "managed here" }
                                }
                            }
                            div { class: "chips",
                                if catalog_verified {
                                    span { class: "chip ok", "catalog verified" }
                                } else if catalog_fetch {
                                    span { class: "chip warn", "catalog fetch on" }
                                } else {
                                    span { class: "chip muted", "static models" }
                                }
                                if egress {
                                    span { class: "chip ok", "ready" }
                                } else {
                                    span { class: "chip warn", "egress blocked" }
                                }
                            }
                            div { class: "chips",
                                for model in models {
                                    span { class: "chip", "{model}" }
                                }
                            }
                            p { class: "tiny", "Routing: {routing}" }
                            div { class: "chips",
                                for (role, secret_name, configured) in secrets {
                                    span { class: if configured { "chip accent" } else { "chip warn" },
                                        "{role}: {secret_name}"
                                    }
                                }
                            }
                            if managed {
                                if confirm() == Some(provider_id.clone()) {
                                    div { class: "row",
                                        span { class: "tiny", "Remove {provider_id}? Conversations pinned to it must pick another route." }
                                        button {
                                            class: "btn danger sm",
                                            disabled: !can_save,
                                            onclick: {
                                                let id = provider_id.clone();
                                                move |_| {
                                                    session.write().remove_provider(id.clone());
                                                    confirm.set(None);
                                                }
                                            },
                                            "Remove"
                                        }
                                        button {
                                            class: "btn ghost sm",
                                            onclick: move |_| confirm.set(None),
                                            "Cancel"
                                        }
                                    }
                                } else {
                                    button {
                                        class: "btn ghost sm",
                                        onclick: {
                                            let id = provider_id.clone();
                                            move |_| confirm.set(Some(id.clone()))
                                        },
                                        "Remove provider"
                                    }
                                }
                            }
                        }
                    }
                }
                div { class: "card",
                    div { class: "card-head",
                        h3 { class: "card-title", "Add or update a provider" }
                    }
                    p { class: "tiny",
                        "Saved next to the database, never in serabot.toml. Keys go straight to the encrypted store and are never shown again. Any OpenAI-compatible /chat/completions endpoint works, including gateways that front Anthropic models."
                    }
                    div { class: "form-grid",
                        div { class: "field",
                            label { class: "field-label", "Provider id" }
                            input {
                                class: "input",
                                value: "{id}",
                                placeholder: "provider id (for example work)",
                                oninput: move |event| session.write().provider.id = event.value(),
                            }
                        }
                        div { class: "field",
                            label { class: "field-label", "Kind" }
                            select {
                                class: "select",
                                value: "{kind}",
                                onchange: move |event| session.write().provider.kind = event.value(),
                                for item in PROVIDER_KINDS {
                                    option { value: "{item}", "{item}" }
                                }
                            }
                        }
                        div { class: "field",
                            label { class: "field-label", "Base URL" }
                            input {
                                class: "input",
                                value: "{base_url}",
                                placeholder: "https://api.example.com/v1",
                                oninput: move |event| session.write().provider.base_url = event.value(),
                            }
                        }
                        div { class: "field",
                            label { class: "field-label", "Models" }
                            input {
                                class: "input",
                                value: "{models}",
                                placeholder: "model ids, comma separated",
                                oninput: move |event| session.write().provider.models = event.value(),
                            }
                        }
                        div { class: "field",
                            label { class: "field-label", "Secret name" }
                            input {
                                class: "input",
                                value: "{secret_name}",
                                placeholder: "secret name (for example WORK_API_KEY)",
                                oninput: move |event| session.write().provider.secret_name = event.value(),
                            }
                        }
                        div { class: "field",
                            label { class: "field-label", "API key" }
                            input {
                                class: "input",
                                "type": "password",
                                value: "{api_key}",
                                placeholder: "API key (leave empty if already stored)",
                                oninput: move |event| session.write().provider.api_key = event.value(),
                            }
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
}

#[component]
fn ConnectorsView(session: Signal<Session>) -> Element {
    let connectors = session.read().state.connectors.clone();
    let loaded = session.read().state.connectors_loaded;
    let can_save = session.read().can_admin();
    rsx! {
        div { class: "pane",
            div { class: "pane-head",
                div {
                    h1 { class: "pane-title", "Connectors" }
                    p { class: "muted", "Enable Google, Microsoft, Notion and AgentMail, and store their credentials." }
                }
                button {
                    class: "btn ghost",
                    onclick: move |_| session.write().refresh_connectors(),
                    "Refresh"
                }
            }
            div { class: "pane-body",
                if !loaded {
                    div { class: "card",
                        div { class: "skeleton title" }
                        div { class: "skeleton block" }
                    }
                }
                div { class: "card-grid",
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
                                    div { class: "card-head",
                                        div {
                                            h3 { class: "card-title", "{id}" }
                                            span { class: "card-sub", "{connector.kind}" }
                                        }
                                        span { class: if enabled { "chip ok" } else { "chip muted" },
                                            if enabled { "enabled" } else { "disabled" }
                                        }
                                    }
                                    if let Some(uri) = connector.redirect_uri.clone() {
                                        code { class: "tiny", "{uri}" }
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
                                                    div { class: "field",
                                                        label { class: "field-label", "{role}" }
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
                                    }
                                    p { class: "tiny",
                                        "Enabling makes the connector available; grant its tools to a bot in Fleet. Reads and sends stay approval-gated."
                                    }
                                    if connector_auth_url(&id, connector.redirect_uri.as_deref()).is_some() {
                                        p { class: "tiny",
                                            "Connect opens the provider's consent page in your browser. Register the redirect URI above with the client id the server stores, then paste the refresh token here."
                                        }
                                    }
                                    div { class: "card-actions",
                                        if let Some(url) = connector_auth_url(&id, connector.redirect_uri.as_deref()) {
                                            button {
                                                class: "btn ghost sm",
                                                title: "Open the provider's authorization page in your browser",
                                                onclick: {
                                                    let url = url.clone();
                                                    move |_| {
                                                        if let Err(error) = webbrowser::open(&url) {
                                                            session.write().state.error =
                                                                Some(format!("Could not open the browser: {error}"));
                                                        }
                                                    }
                                                },
                                                Icon { name: "external", size: 14 }
                                                "Connect"
                                            }
                                        }
                                        button {
                                            class: if enabled { "btn ghost sm" } else { "btn primary sm" },
                                            onclick: {
                                                let id = id.clone();
                                                move |_| session.write().toggle_connector(&id)
                                            },
                                            if enabled { "Disable" } else { "Enable" }
                                        }
                                        button {
                                            class: "btn ghost sm",
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
    let memories_empty = memories.is_empty();
    rsx! {
        div { class: "pane",
            div { class: "pane-head",
                div {
                    h1 { class: "pane-title", "Memory" }
                    p { class: "muted", "Owner-scoped facts. The orchestrator reads the 20 most recent." }
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
                        placeholder: "Filter content, kind or source",
                        oninput: move |event| session.write().memory.query = event.value(),
                    }
                    button {
                        class: "btn ghost",
                        onclick: move |_| session.write().load_memories(),
                        "Search"
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
                                span { class: "chip accent", "{memory.kind}" }
                                span { class: "tiny", "{memory.created_at} · {memory.source}" }
                            }
                            p { "{memory.content}" }
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
                                    "Delete"
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
                                class: "input",
                                value: "{slug}",
                                placeholder: "id (lowercase slug)",
                                oninput: move |event| session.write().fleet.conv_slug = event.value(),
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
    // The live sandbox posture. Every value the server does not report stays
    // "unknown"; nothing is guessed.
    let status = session.read().status.status.clone();
    let status_error = session.read().status.error.clone();
    let status_loading = session.read().status.loading;
    let restart_pending = session.read().restart_pending;
    let policy_version = status
        .as_ref()
        .and_then(|status| status.policy_version.clone())
        .unwrap_or_else(|| "unknown".into());
    let pending_tasks = match status.as_ref().and_then(|status| status.pending_tasks) {
        Some(count) if count >= 0 => count.to_string(),
        _ => "unknown".to_string(),
    };
    let pending_approvals = match status.as_ref().and_then(|status| status.pending_approvals) {
        Some(count) if count >= 0 => count.to_string(),
        _ => "unknown".to_string(),
    };
    let pending_tools = status
        .as_ref()
        .map(|status| {
            status
                .approvals
                .iter()
                .map(|approval| approval.tool.clone())
                .filter(|tool| !tool.is_empty())
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let pending_tools_empty = pending_tools.is_empty();
    let (bots_loaded, gated_tools) = {
        let state = session.read();
        let mut map: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for bot in &state.state.listed_bots {
            for tool in &bot.require_approval {
                map.entry(tool.clone()).or_default().push(bot.id.clone());
            }
        }
        let gated = map
            .into_iter()
            .map(|(tool, bots)| {
                let label = format!("{tool} · {}", bots.join(", "));
                (tool, label)
            })
            .collect::<Vec<_>>();
        (state.state.bots_loaded, gated)
    };
    let (providers_loaded, egress) = {
        let state = session.read();
        (
            state.state.providers_loaded,
            state
                .state
                .providers
                .iter()
                .map(|provider| {
                    (
                        provider.id.clone(),
                        if provider.data_egress_approved {
                            "approved".to_string()
                        } else {
                            "blocked".to_string()
                        },
                        provider.data_egress_approved,
                    )
                })
                .collect::<Vec<_>>(),
        )
    };
    let gated_empty = gated_tools.is_empty();
    let egress_empty = egress.is_empty();
    rsx! {
        div { class: "pane",
            div { class: "pane-head",
                div {
                    h1 { class: "pane-title", "Settings" }
                    p { class: "muted", "Connection, capabilities, appearance and updates." }
                }
            }
            div { class: "pane-body",
                div { class: "card",
                    div { class: "card-head",
                        h3 { class: "card-title", "Connection" }
                    }
                    p { class: "card-sub", "{server}" }
                    if !version.is_empty() {
                        p { class: "tiny", "Server {version}" }
                    }
                    div { class: "card-actions",
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
                    div { class: "card-head",
                        h3 { class: "card-title", "Sandbox" }
                        div { class: "row",
                            if status_loading {
                                span { class: "spinner", title: "Reading the server posture" }
                            }
                            button {
                                class: "btn ghost sm",
                                onclick: move |_| session.write().refresh_status(),
                                Icon { name: "refresh", size: 14 }
                                "Refresh posture"
                            }
                        }
                    }
                    p { class: "card-sub", "Read-only view of what the server enforces right now." }
                    div { class: "chips",
                        span { class: "chip", "policy: {policy_version}" }
                        span { class: "chip", "pending tasks: {pending_tasks}" }
                        span { class: "chip", "pending approvals: {pending_approvals}" }
                        span {
                            class: if restart_pending { "chip warn" } else { "chip ok" },
                            if restart_pending { "restart pending" } else { "no restart pending" }
                        }
                        span {
                            class: match audio_enabled { Some(true) => "chip ok", Some(false) => "chip warn", None => "chip muted" },
                            match audio_enabled { Some(true) => "Audio on", Some(false) => "Audio off", None => "Audio unknown" }
                        }
                        span {
                            class: match firecrawl_enabled { Some(true) => "chip ok", Some(false) => "chip muted", None => "chip muted" },
                            match firecrawl_enabled { Some(true) => "Firecrawl on", Some(false) => "Firecrawl off", None => "Firecrawl unknown" }
                        }
                        span {
                            class: match telegram_enabled { Some(true) => "chip ok", Some(false) => "chip muted", None => "chip muted" },
                            match telegram_enabled { Some(true) => "Telegram on", Some(false) => "Telegram off", None => "Telegram unknown" }
                        }
                    }
                    if let Some(error) = status_error {
                        div { class: "banner err", span { "{error}" } }
                    }
                    div { class: "field",
                        label { class: "field-label", "Owner approval required" }
                        if bots_loaded {
                            div { class: "chips",
                                for (_tool, label) in gated_tools {
                                    span { class: "chip warn", "{label}" }
                                }
                                if gated_empty {
                                    span { class: "chip muted", "no tool needs approval" }
                                }
                            }
                        } else {
                            span { class: "hint", "unknown" }
                        }
                    }
                    if !pending_tools_empty {
                        div { class: "field",
                            label { class: "field-label", "Waiting for your decision" }
                            div { class: "chips",
                                for tool in pending_tools {
                                    span { class: "chip accent", "{tool}" }
                                }
                            }
                        }
                    }
                    div { class: "field",
                        label { class: "field-label", "Data egress consent" }
                        if providers_loaded {
                            div { class: "chips",
                                for (provider_id, egress_state, approved) in egress {
                                    span { class: if approved { "chip ok" } else { "chip warn" },
                                        "{provider_id}: {egress_state}"
                                    }
                                }
                                if egress_empty {
                                    span { class: "chip muted", "no provider" }
                                }
                            }
                        } else {
                            span { class: "hint", "unknown" }
                        }
                    }
                }
                div { class: "card",
                    div { class: "card-head",
                        h3 { class: "card-title", "Capabilities" }
                    }
                    div { class: "chips",
                        span {
                            class: match audio_enabled { Some(true) => "chip ok", Some(false) => "chip warn", None => "chip muted" },
                            match audio_enabled { Some(true) => "Audio on", Some(false) => "Audio off", None => "Audio unknown" }
                        }
                        span {
                            class: match firecrawl_enabled { Some(true) => "chip ok", Some(false) => "chip muted", None => "chip muted" },
                            match firecrawl_enabled { Some(true) => "Firecrawl on", Some(false) => "Firecrawl off", None => "Firecrawl unknown" }
                        }
                        span {
                            class: match telegram_enabled { Some(true) => "chip ok", Some(false) => "chip muted", None => "chip muted" },
                            match telegram_enabled { Some(true) => "Telegram on", Some(false) => "Telegram off", None => "Telegram unknown" }
                        }
                        span {
                            class: match catalog_fetch { Some(true) => "chip ok", Some(false) => "chip warn", None => "chip muted" },
                            match catalog_fetch { Some(true) => "Model catalogs live", Some(false) => "Model catalogs static", None => "Model catalogs unknown" }
                        }
                    }
                    if audio_enabled == Some(true) {
                        p { class: "tiny",
                            "Transcription: {transcription}. Understanding: {understanding}."
                        }
                        p { class: "tiny",
                            "Voice notes sent to the bot's Telegram intake are transcribed with the configured Mistral audio model before the turn runs."
                        }
                    } else if audio_enabled == Some(false) {
                        p { class: "tiny", "Audio is disabled on the server (audio.enabled = false)." }
                    } else {
                        p { class: "tiny",
                            "This server build does not report capabilities. Update the server to see live status."
                        }
                    }
                    if catalog_fetch == Some(false) {
                        p { class: "tiny",
                            "Enable catalog_fetch in [llm] and use Fetch models on the Providers page to pull live model lists."
                        }
                    }
                    div { class: "form-grid",
                        div { class: "field",
                            label { class: "field-label", "Transcription model" }
                            input {
                                class: "input",
                                value: "{audio_transcription}",
                                placeholder: "transcription model",
                                oninput: move |event| session.write().audio.transcription_model = event.value(),
                            }
                        }
                        div { class: "field",
                            label { class: "field-label", "Understanding model" }
                            input {
                                class: "input",
                                value: "{audio_understanding}",
                                placeholder: "understanding model",
                                oninput: move |event| session.write().audio.understanding_model = event.value(),
                            }
                        }
                    }
                    p { class: "tiny",
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
                    div { class: "card-head",
                        h3 { class: "card-title", "Appearance" }
                    }
                    div { class: "card-actions",
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
                    div { class: "card-head",
                        h3 { class: "card-title", "Updates" }
                    }
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
                        p { class: "error-text", "{error}" }
                    }
                }
                div { class: "card",
                    div { class: "card-head",
                        h3 { class: "card-title", "Keyboard" }
                    }
                    p { class: "tiny", "Enter sends, Shift+Enter adds a line, Ctrl/Cmd+K focuses search, Esc closes the topmost sheet or menu." }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn authorization_urls_are_fixed_https_endpoints() {
        let url = connector_auth_url("gmail", None).expect("gmail has an authorization page");
        assert!(url.starts_with("https://accounts.google.com/"), "{url}");
        assert!(
            connector_auth_url("notion", None)
                .expect("notion has an authorization page")
                .starts_with("https://api.notion.com/"),
        );
        assert!(
            connector_auth_url("outlook", None)
                .expect("outlook has an authorization page")
                .starts_with("https://login.microsoftonline.com/"),
        );
        assert!(connector_auth_url("unknown", None).is_none());
    }

    #[test]
    fn a_redirect_uri_is_appended_only_when_it_is_a_plain_address() {
        let url = connector_auth_url("gmail", Some("http://127.0.0.1:8765/callback"))
            .expect("gmail has an authorization page");
        assert!(url.contains("redirect_uri="), "{url}");
        assert!(url.starts_with("https://"), "{url}");
        // Not an address, or carrying shell metacharacters: rejected.
        assert!(connector_auth_url("gmail", Some("javascript:alert(1)")).is_none());
        assert!(connector_auth_url("gmail", Some("https://ok.example/cb&calc")).is_none());
        assert!(connector_auth_url("gmail", Some("https://ok.example/cb?state=1")).is_none());
        assert!(connector_auth_url("gmail", Some("https://ok.example/\"")).is_none());
        assert!(connector_auth_url("gmail", Some("   ")).is_some());
    }
}
