//! Dioxus Desktop shell. Windows-only; the Rust session owns all network work.
//!
//! Layout: a three-pane macOS-style shell (glass sidebar, sticky glass topbar,
//! centered chat column, optional contextual inspector). The design system lives
//! in `assets/app.css`; this file owns the markup and the chat ergonomics.

#![cfg_attr(not(windows), allow(dead_code))]

use dioxus::prelude::*;
use serabot_desktop::{
    ApprovalDecision, BotConfigUpdate, ConversationKind, MAX_TEAM_MEMBERS, Role, SPECIALIST_TOOLS,
    Selection, TaskStatus, Team, TeamMember, TeamMode,
    audio_capture::MicRecorder,
    branding, parse_skill_list, provider_presets,
    session::{Section, Session, Theme, humanize_label},
    teams, valid_skill_id,
};
use std::collections::{BTreeMap, HashMap, HashSet};

/// Cap for the skills array of one bot config. Mirrors
/// `MAX_SCAFFOLD_SKILLS` in `api.rs`, which is crate-private.
const MAX_BOT_SKILLS: usize = 16;

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
    bool,
    bool,
);

/// Whether an OAuth provider holds its sign-in material. PKCE providers keep a
/// refresh token; device-flow providers keep the minted key.
fn provider_signed_in(
    kind: &str,
    secrets: &BTreeMap<String, serabot_desktop::SecretStatus>,
) -> bool {
    let configured = |role: &str| secrets.get(role).is_some_and(|secret| secret.configured);
    if kind == "muse-code" {
        configured("api_key")
    } else {
        configured("refresh_token")
    }
}

/// The provider kinds the server understands.
const PROVIDER_KINDS: [&str; 15] = [
    "openai-compatible",
    "google-antigravity",
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
    "muse-code",
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
/// Cmd/Ctrl+K focuses the sidebar search; Esc closes the topmost sheet, menu
/// or pop. A pointerdown outside a `.menu-wrap` closes the menu/pop it owns
/// through its trigger (or hidden `.escape-close`), so the Rust-side open
/// signal stays in sync instead of only the CSS class changing.
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
      var open = document.querySelectorAll('.sheet.open, .menu.open, .pop.open');
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
  document.addEventListener('pointerdown', function (event) {
    var open = document.querySelectorAll('.pop.open, .menu.open');
    for (var i = 0; i < open.length; i += 1) {
      var overlay = open[i];
      if (overlay.contains(event.target)) { continue; }
      var parent = overlay.parentElement;
      var wrap = parent ? parent.closest('.menu-wrap') : null;
      if (wrap && wrap.contains(event.target)) { continue; }
      var closer = overlay.querySelector('.escape-close');
      if (closer) { closer.click(); continue; }
      var trigger = wrap ? wrap.querySelector('.menu-trigger') : null;
      if (trigger) { trigger.click(); } else { overlay.classList.remove('open'); }
    }
  });
})();"#;

fn stamp(raw: &str) -> String {
    raw.chars().take(16).collect::<String>().replace('T', " ")
}

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

/// The chat inspector panes: details, the artifact library, and group members.
#[derive(Clone, Copy, PartialEq)]
enum InspectorTab {
    Details,
    Library,
    Members,
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

/// The `@mention` query being typed at the end of `text`, if any: the last
/// `@` whose tail contains no whitespace, is at most 32 chars and matches
/// `[a-z0-9-_]*`. A bare `@` yields `Some("")`.
fn mention_query(text: &str) -> Option<String> {
    let at = text.rfind('@')?;
    let tail = &text[at + 1..];
    if tail.len() > 32 || tail.chars().any(char::is_whitespace) {
        return None;
    }
    if !tail
        .bytes()
        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'_')
    {
        return None;
    }
    Some(tail.to_owned())
}

/// The `/command` query at the start of the last line of `text`, if the rest
/// of that line has no whitespace. A bare `/` yields `Some("")`.
fn slash_query(text: &str) -> Option<String> {
    let line = text.rsplit('\n').next().unwrap_or(text);
    let rest = line.strip_prefix('/')?;
    if rest.chars().any(char::is_whitespace) {
        return None;
    }
    Some(rest.to_owned())
}

/// Replace the trailing `@query` in `text` with `@id `.
fn insert_mention_text(text: &str, id: &str) -> String {
    match text.rfind('@') {
        Some(at) => format!("{}@{} ", &text[..at], id),
        None => format!("{}@{} ", text, id),
    }
}

/// Replace the `/query` at the start of the last line of `text` with `id `.
fn insert_slash_text(text: &str, id: &str) -> String {
    let line_start = text.rfind('\n').map(|i| i + 1).unwrap_or(0);
    format!("{}{} ", &text[..line_start], id)
}

/// Bots matching a mention query: `(id, display name, avatar)`.
fn mention_bot_matches(session: &Session, query: &str) -> Vec<(String, String, String)> {
    let needle = query.to_lowercase();
    session
        .state
        .listed_bots
        .iter()
        .filter(|bot| {
            needle.is_empty()
                || bot.id.to_lowercase().contains(&needle)
                || session
                    .bot_display_name(&bot.id)
                    .to_lowercase()
                    .contains(&needle)
        })
        .map(|bot| {
            (
                bot.id.clone(),
                session.bot_display_name(&bot.id),
                session.bot_avatar(&bot.id),
            )
        })
        .collect()
}

/// Connector ids matching a mention query.
fn mention_connector_matches(session: &Session, query: &str) -> Vec<String> {
    let needle = query.to_lowercase();
    session
        .state
        .connectors
        .iter()
        .map(|connector| connector.id.clone())
        .filter(|id| needle.is_empty() || id.to_lowercase().contains(&needle))
        .collect()
}

/// Skills matching a slash query: `(id, name, description)`.
fn slash_skill_matches(session: &Session, query: &str) -> Vec<(String, String, String)> {
    let needle = query.to_lowercase();
    session
        .state
        .listed_skills
        .iter()
        .filter(|skill| {
            needle.is_empty()
                || skill.id.to_lowercase().contains(&needle)
                || skill.name.to_lowercase().contains(&needle)
        })
        .map(|skill| {
            (
                skill.id.clone(),
                skill.name.clone(),
                skill.description.clone(),
            )
        })
        .collect()
}

/// Specialist tools matching a slash query (by id or capability label).
fn slash_tool_matches(query: &str) -> Vec<&'static str> {
    let needle = query.to_lowercase();
    SPECIALIST_TOOLS
        .iter()
        .copied()
        .filter(|tool| {
            needle.is_empty()
                || tool.contains(&needle)
                || capability_label(tool)
                    .is_some_and(|label| label.to_lowercase().contains(&needle))
        })
        .collect()
}

/// Parse a picker value (`"auto"` or `"provider|model"`) into a selection —
/// exactly what the old `<select>` did with its option values.
fn parse_model_value(raw: &str) -> Selection {
    if raw == "auto" {
        Selection::auto()
    } else {
        let (provider, model) = raw.split_once('|').unwrap_or(("auto", ""));
        if provider == "auto" {
            Selection::auto()
        } else {
            Selection {
                provider: provider.to_owned(),
                model: model.to_owned(),
            }
        }
    }
}

/// Set the draft model selection from a picker value.
fn set_model_draft(session: &mut Session, raw: &str) {
    session.state.draft = parse_model_value(raw);
}

pub fn run() {
    #[cfg(windows)]
    {
        let config = dioxus::desktop::Config::new()
            .with_menu(Option::<dioxus::desktop::muda::Menu>::None)
            // WebView2 flags: on Windows the dioxus:// origin maps to
            // http://dioxus.index.html, which is not a secure context, so
            // navigator.mediaDevices is undefined and getUserMedia fails with
            // "Microphone capture is not available in this window"; without a
            // host handler WebView2 also denies the capture permission request.
            // Treat our own app origin as secure and auto-accept mic capture for
            // this trusted page. wry REPLACES its default args when additional
            // args are given, so re-add the defaults here.
            .with_windows_browser_args(
                "--disable-features=msWebOOUI,msPdfOOUI,msSmartScreenProtection \
                 --autoplay-policy=no-user-gesture-required \
                 --unsafely-treat-insecure-origin-as-secure=http://dioxus.index.html \
                 --auto-accept-camera-and-microphone-capture",
            )
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
        "chevron-up" => "M6 15l6-6 6 6",
        "chevron-right" => "M9 6l6 6-6 6",
        "undo" => "M9 14l-5-5 5-5M4 9h11a5 5 0 0 1 0 10h-2",
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

/// Inline brand SVG logos for all supported connectors and providers.
/// The SVG markup itself lives in [`branding`]; a brand without a known
/// mark falls back to an initial-letter avatar.
#[component]
pub fn BrandLogo(brand: String, #[props(default = 24)] size: u32) -> Element {
    let title = branding::brand_label(&brand).map(str::to_owned);
    let svg = branding::brand_svg(&brand).map(str::to_owned);
    let initial: String = brand
        .chars()
        .next()
        .map(|c| c.to_uppercase().collect::<String>())
        .unwrap_or_else(|| "?".to_owned());
    rsx! {
        div {
            class: "brand-logo",
            style: "width: {size}px; height: {size}px;",
            title: title.as_deref().unwrap_or(""),
            if let Some(svg) = svg {
                div {
                    style: "width: 100%; height: 100%; display: flex;",
                    dangerous_inner_html: "{svg}",
                }
            } else {
                span { style: "font-size: {size * 3 / 5}px; font-weight: 600;", "{initial}" }
            }
        }
    }
}

// ------------------------------------------------ Pure-Rust Markdown Renderer ---

#[derive(Clone, Debug, PartialEq)]
pub enum MdBlock {
    Heading {
        level: usize,
        text: String,
    },
    CodeBlock {
        lang: String,
        code: String,
    },
    Rule,
    Quote(Vec<String>),
    UnorderedList(Vec<String>),
    OrderedList(Vec<String>),
    Table {
        headers: Vec<String>,
        rows: Vec<Vec<String>>,
    },
    Paragraph(String),
}

#[derive(Clone, Debug, PartialEq)]
pub enum InlineSpan {
    Text(String),
    Code(String),
    Bold(Vec<InlineSpan>),
    Italic(String),
    Link { label: String, url: String },
}

pub fn parse_markdown_blocks(content: &str) -> Vec<MdBlock> {
    let mut blocks = Vec::new();
    let lines: Vec<&str> = content.lines().collect();
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i];
        let trimmed = line.trim();
        if trimmed.is_empty() {
            i += 1;
            continue;
        }

        // Fenced code block
        if trimmed.starts_with("```") {
            let lang = trimmed.trim_start_matches('`').trim().to_owned();
            let mut code_lines = Vec::new();
            i += 1;
            while i < lines.len() {
                if lines[i].trim().starts_with("```") {
                    i += 1;
                    break;
                }
                code_lines.push(lines[i]);
                i += 1;
            }
            blocks.push(MdBlock::CodeBlock {
                lang,
                code: code_lines.join("\n"),
            });
            continue;
        }

        // Horizontal rule (---, ***, ___)
        if (trimmed == "---" || trimmed == "***" || trimmed == "___")
            || (trimmed.len() >= 3 && trimmed.chars().all(|c| c == '-' || c == '*' || c == '_'))
        {
            blocks.push(MdBlock::Rule);
            i += 1;
            continue;
        }

        // Heading
        if trimmed.starts_with('#') {
            let hash_count = trimmed.chars().take_while(|c| *c == '#').count();
            if hash_count <= 6 && trimmed[hash_count..].starts_with(' ') {
                let rest = trimmed[hash_count..].trim();
                let title = if rest.starts_with("**") && rest.ends_with("**") && rest.len() >= 4 {
                    &rest[2..rest.len() - 2]
                } else {
                    rest
                };
                blocks.push(MdBlock::Heading {
                    level: hash_count,
                    text: title.trim().to_owned(),
                });
                i += 1;
                continue;
            }
        }

        // Blockquote
        if trimmed.starts_with("> ") || trimmed == ">" {
            let mut quote_lines = Vec::new();
            while i < lines.len() {
                let q_trim = lines[i].trim();
                if let Some(stripped) = q_trim.strip_prefix("> ") {
                    quote_lines.push(stripped.trim().to_owned());
                    i += 1;
                } else if q_trim == ">" {
                    quote_lines.push(String::new());
                    i += 1;
                } else if !q_trim.is_empty()
                    && !quote_lines.is_empty()
                    && !q_trim.starts_with('#')
                    && !q_trim.starts_with('-')
                    && !q_trim.starts_with("```")
                {
                    quote_lines.push(q_trim.to_owned());
                    i += 1;
                } else {
                    break;
                }
            }
            blocks.push(MdBlock::Quote(quote_lines));
            continue;
        }

        // Markdown Table
        if trimmed.starts_with('|')
            && i + 1 < lines.len()
            && lines[i + 1].trim().starts_with('|')
            && lines[i + 1].contains("---")
        {
            let parse_table_row = |row: &str| -> Vec<String> {
                let trimmed_row = row.trim().trim_start_matches('|').trim_end_matches('|');
                trimmed_row
                    .split('|')
                    .map(|c| c.trim().to_owned())
                    .collect()
            };
            let headers = parse_table_row(trimmed);
            i += 2;
            let mut rows = Vec::new();
            while i < lines.len() && lines[i].trim().starts_with('|') {
                rows.push(parse_table_row(lines[i]));
                i += 1;
            }
            blocks.push(MdBlock::Table { headers, rows });
            continue;
        }

        // Unordered list
        if trimmed.starts_with("- ") || trimmed.starts_with("* ") || trimmed.starts_with("• ") {
            let mut items = Vec::new();
            while i < lines.len() {
                let l_trim = lines[i].trim();
                if l_trim.starts_with("- ") || l_trim.starts_with("* ") || l_trim.starts_with("• ")
                {
                    let marker_len = if l_trim.starts_with("• ") { 3 } else { 2 };
                    items.push(l_trim[marker_len..].trim().to_owned());
                    i += 1;
                } else if !l_trim.is_empty()
                    && (lines[i].starts_with("  ") || lines[i].starts_with('\t'))
                    && !items.is_empty()
                {
                    let last = items.last_mut().unwrap();
                    last.push(' ');
                    last.push_str(l_trim);
                    i += 1;
                } else {
                    break;
                }
            }
            blocks.push(MdBlock::UnorderedList(items));
            continue;
        }

        // Ordered list
        let first_word = trimmed.split_whitespace().next().unwrap_or("");
        if first_word.ends_with('.')
            && first_word
                .trim_end_matches('.')
                .chars()
                .all(|c| c.is_ascii_digit())
        {
            let mut items = Vec::new();
            while i < lines.len() {
                let l_trim = lines[i].trim();
                let fw = l_trim.split_whitespace().next().unwrap_or("");
                if fw.ends_with('.') && fw.trim_end_matches('.').chars().all(|c| c.is_ascii_digit())
                {
                    let item_text = l_trim[fw.len()..].trim().to_owned();
                    items.push(item_text);
                    i += 1;
                } else if !l_trim.is_empty()
                    && (lines[i].starts_with("  ") || lines[i].starts_with('\t'))
                    && !items.is_empty()
                {
                    let last = items.last_mut().unwrap();
                    last.push(' ');
                    last.push_str(l_trim);
                    i += 1;
                } else {
                    break;
                }
            }
            blocks.push(MdBlock::OrderedList(items));
            continue;
        }

        // Paragraph
        let mut para_lines = Vec::new();
        while i < lines.len() {
            let p_trim = lines[i].trim();
            if p_trim.is_empty()
                || p_trim.starts_with("```")
                || p_trim.starts_with('#')
                || p_trim.starts_with("> ")
                || p_trim == ">"
                || p_trim.starts_with("- ")
                || p_trim.starts_with("* ")
                || p_trim.starts_with("• ")
                || (p_trim.starts_with('|')
                    && i + 1 < lines.len()
                    && lines[i + 1].trim().starts_with('|'))
                || (p_trim == "---" || p_trim == "***" || p_trim == "___")
            {
                break;
            }
            para_lines.push(p_trim);
            i += 1;
        }
        if !para_lines.is_empty() {
            blocks.push(MdBlock::Paragraph(para_lines.join(" ")));
        }
    }
    blocks
}

pub fn parse_inline(text: &str) -> Vec<InlineSpan> {
    let mut spans = Vec::new();
    let mut rest = text;

    while !rest.is_empty() {
        // 1. Bold with inline code: **`code`**
        if rest.starts_with("**`")
            && let Some(end_code) = rest[3..].find('`')
        {
            let code = &rest[3..3 + end_code];
            let after_code = &rest[3 + end_code + 1..];
            if let Some(stripped) = after_code.strip_prefix("**") {
                spans.push(InlineSpan::Bold(vec![InlineSpan::Code(code.to_owned())]));
                rest = stripped;
                continue;
            }
        }

        // 2. Bold: **...**
        if rest.starts_with("**")
            && let Some(end) = rest[2..].find("**")
        {
            let inner = &rest[2..2 + end];
            let sub_spans = parse_inline(inner);
            spans.push(InlineSpan::Bold(sub_spans));
            rest = &rest[2 + end + 2..];
            continue;
        }

        // 3. Inline Code: `...`
        if rest.starts_with('`')
            && let Some(end) = rest[1..].find('`')
        {
            let code = &rest[1..1 + end];
            spans.push(InlineSpan::Code(code.to_owned()));
            rest = &rest[1 + end + 1..];
            continue;
        }

        // 4. Link: [label](url)
        if rest.starts_with('[')
            && let Some(close_bracket) = rest.find(']')
            && rest[close_bracket..].starts_with("](")
        {
            let after_bracket = &rest[close_bracket + 2..];
            if let Some(close_paren) = after_bracket.find(')') {
                let label = &rest[1..close_bracket];
                let url = &after_bracket[..close_paren];
                spans.push(InlineSpan::Link {
                    label: label.to_owned(),
                    url: url.to_owned(),
                });
                rest = &after_bracket[close_paren + 1..];
                continue;
            }
        }

        // 5. Italic: *...* (where not **)
        if rest.starts_with('*')
            && !rest.starts_with("**")
            && let Some(end) = rest[1..].find('*')
        {
            let inner = &rest[1..1 + end];
            if !inner.is_empty() && !inner.starts_with('*') && !inner.ends_with('*') {
                spans.push(InlineSpan::Italic(inner.to_owned()));
                rest = &rest[1 + end + 1..];
                continue;
            }
        }

        // Consume normal text up to the next formatting token. Skip the first
        // *char* (not byte): slicing at byte 1 panics when `rest` starts with a
        // multibyte character (emoji, accented letters).
        let next_pos = rest
            .char_indices()
            .skip(1)
            .find(|(_, c)| matches!(c, '*' | '`' | '['))
            .map(|(idx, _)| idx)
            .unwrap_or(rest.len());

        let chunk = &rest[..next_pos];
        if let Some(InlineSpan::Text(last_text)) = spans.last_mut() {
            last_text.push_str(chunk);
        } else {
            spans.push(InlineSpan::Text(chunk.to_owned()));
        }
        rest = &rest[next_pos..];
    }

    spans
}

#[component]
pub fn MarkdownBody(content: String) -> Element {
    let blocks = parse_markdown_blocks(&content);
    rsx! {
        div { class: "md-body",
            for (idx, block) in blocks.into_iter().enumerate() {
                {match block {
                    MdBlock::Heading { level, text } => match level {
                        1 => rsx! { h1 { key: "{idx}", class: "md-h1", InlineMarkdown { text } } },
                        2 => rsx! { h2 { key: "{idx}", class: "md-h2", InlineMarkdown { text } } },
                        3 => rsx! { h3 { key: "{idx}", class: "md-h3", InlineMarkdown { text } } },
                        _ => rsx! { h4 { key: "{idx}", class: "md-h4", InlineMarkdown { text } } },
                    },
                    MdBlock::CodeBlock { lang, code } => rsx! {
                        div { key: "{idx}", class: "md-code-block",
                            if !lang.is_empty() {
                                div { class: "md-code-head", span { "{lang}" } }
                            }
                            pre { code { "{code}" } }
                        }
                    },
                    MdBlock::Rule => rsx! {
                        hr { key: "{idx}", class: "md-hr" }
                    },
                    MdBlock::Quote(lines) => rsx! {
                        blockquote { key: "{idx}", class: "md-quote",
                            for (l_idx, line) in lines.into_iter().enumerate() {
                                p { key: "{l_idx}", InlineMarkdown { text: line } }
                            }
                        }
                    },
                    MdBlock::UnorderedList(items) => rsx! {
                        ul { key: "{idx}", class: "md-list",
                            for (i_idx, item) in items.into_iter().enumerate() {
                                li { key: "{i_idx}", InlineMarkdown { text: item } }
                            }
                        }
                    },
                    MdBlock::OrderedList(items) => rsx! {
                        ol { key: "{idx}", class: "md-list md-ol",
                            for (i_idx, item) in items.into_iter().enumerate() {
                                li { key: "{i_idx}", InlineMarkdown { text: item } }
                            }
                        }
                    },
                    MdBlock::Table { headers, rows } => rsx! {
                        div { key: "{idx}", class: "md-table-wrap",
                            table { class: "md-table",
                                thead {
                                    tr {
                                        for (h_idx, header) in headers.into_iter().enumerate() {
                                            th { key: "{h_idx}", InlineMarkdown { text: header } }
                                        }
                                    }
                                }
                                tbody {
                                    for (r_idx, row) in rows.into_iter().enumerate() {
                                        tr { key: "{r_idx}",
                                            for (c_idx, cell) in row.into_iter().enumerate() {
                                                td { key: "{c_idx}", InlineMarkdown { text: cell } }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    },
                    MdBlock::Paragraph(text) => rsx! {
                        p { key: "{idx}", class: "md-p", InlineMarkdown { text } }
                    },
                }}
            }
        }
    }
}

#[component]
pub fn InlineMarkdown(text: String) -> Element {
    let spans = parse_inline(&text);
    rsx! {
        for (idx, span) in spans.into_iter().enumerate() {
            RenderSpan { key: "{idx}", span }
        }
    }
}

#[component]
fn RenderSpan(span: InlineSpan) -> Element {
    match span {
        InlineSpan::Text(s) => rsx! { "{s}" },
        InlineSpan::Code(code) => rsx! { code { class: "md-inline-code", "{code}" } },
        InlineSpan::Bold(sub_spans) => rsx! {
            strong {
                for (idx, sub) in sub_spans.into_iter().enumerate() {
                    RenderSpan { key: "{idx}", span: sub }
                }
            }
        },
        InlineSpan::Italic(s) => rsx! { em { "{s}" } },
        InlineSpan::Link { label, url } => rsx! {
            a { class: "md-link", href: "{url}", target: "_blank", rel: "noopener noreferrer", "{label}" }
        },
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
            Sidebar { session, overlay, drawer_open, toast, inspector_open }
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
                    "New chat"
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
                    "New group chat"
                }
            }
        }
    }
}

/// Inline SVG pin button: toggles a bot id or conversation id in the pin
/// list. Stops propagation so it never triggers the surrounding row.
#[component]
fn PinToggle(session: Signal<Session>, id: String, pinned: bool) -> Element {
    rsx! {
        button {
            class: if pinned { "pin-toggle pinned" } else { "pin-toggle" },
            title: if pinned { "Unpin" } else { "Pin" },
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
fn Sidebar(
    session: Signal<Session>,
    overlay: Signal<Overlay>,
    drawer_open: Signal<bool>,
    toast: Signal<Option<(String, bool)>>,
    inspector_open: Signal<bool>,
) -> Element {
    let mut overlay = overlay;
    let mut toast = toast;
    let mut inspector_open = inspector_open;
    let connected = session.read().state.connected;
    let mut search = use_signal(String::new);
    let mut collapsed = use_signal(HashSet::<String>::new);
    let mut renaming = use_signal(|| Option::<(String, String)>::None);
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
                        let subtitle = state.conversation_subtitle(grand);
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

        if needle.is_empty() {
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
                        subtitle: state.conversation_subtitle(conversation),
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
                        RailItem::Bot { id, label, role, avatar, active, enabled, live, unread, pinned } => {
                            let pin_id = id.strip_prefix("dir:").unwrap_or(&id).to_owned();
                            rsx! {
                                div { key: "{id}", class: if active { "thread active" } else { "thread" },
                                    button {
                                        class: "thread-main thread-bot",
                                        onclick: {
                                            let id = id.clone();
                                            move |_| {
                                                let bot = id.strip_prefix("dir:").unwrap_or(&id).to_owned();
                                                session.write().open_bot_chat(bot);
                                                // A bot click also reveals its profile in the inspector.
                                                inspector_open.set(true);
                                            }
                                        },
                                        span { class: "avatar sm bot", style: avatar_style(&id), "{avatar}" }
                                        div { class: "thread-copy",
                                            span { class: "thread-title", "{label}" }
                                            span { class: "thread-meta", if enabled { "{role}" } else { "{role} · disabled" } }
                                        }
                                        if unread > 0 {
                                            span { class: "unread-badge", "{unread}" }
                                        }
                                    }
                                    span {
                                        class: if live { "thread-dot live" } else { "thread-dot off" },
                                        title: if live { "live" } else { "restart required" }
                                    }
                                    PinToggle { session, id: pin_id, pinned }
                                }
                            }
                        },
                        RailItem::Thread { id, label, subtitle, depth, active, unread, archived, pinned } => {
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
                                            if !archived && unread > 0 {
                                                span { class: "unread-badge", "{unread}" }
                                            }
                                        }
                                        div { class: "thread-actions",
                                            if archived {
                                                button {
                                                    class: "btn icon",
                                                    title: "Unarchive",
                                                    onclick: {
                                                        let id = id.clone();
                                                        move |_| {
                                                            session.write().unarchive_conversation(id.clone());
                                                            toast.set(Some(("Restored.".into(), false)));
                                                        }
                                                    },
                                                    Icon { name: "undo", size: 15 }
                                                }
                                                PinToggle { session, id: id.clone(), pinned }
                                            } else {
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
                                                PinToggle { session, id: id.clone(), pinned }
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
                UserMenu { session, toast }
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
        unread: i64,
        pinned: bool,
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
fn UserMenu(session: Signal<Session>, toast: Signal<Option<(String, bool)>>) -> Element {
    let mut session = session;
    let mut toast = toast;
    let mut open = use_signal(|| false);
    let theme = session.read().theme;
    rsx! {
        div { class: "menu-wrap grow",
            button {
                class: "user-chip menu-trigger",
                onclick: move |_| open.set(!open()),
                span { class: "avatar sm user", "Y" }
                span { class: "user-chip-name", "Owner" }
                Icon { name: if open() { "chevron-down" } else { "chevron-up" }, size: 14 }
            }
            div {
                class: if open() { "menu open top right" } else { "menu top right" },
                onkeydown: move |event: KeyboardEvent| {
                    if event.key() == Key::Escape {
                        open.set(false);
                    }
                },
                div { class: "menu-label", "Owner" }
                button {
                    class: "menu-item",
                    onclick: move |_| {
                        open.set(false);
                        session.write().open_section(Section::Settings);
                    },
                    Icon { name: "settings", size: 16 }
                    "Profile and settings"
                }
                button {
                    class: "menu-item",
                    onclick: move |_| {
                        open.set(false);
                        session.write().open_section(Section::Fleet);
                    },
                    Icon { name: "robot", size: 16 }
                    "Bots"
                }
                button {
                    class: "menu-item",
                    onclick: move |_| {
                        open.set(false);
                        session.write().open_section(Section::Memory);
                    },
                    Icon { name: "bolt", size: 16 }
                    "Memory"
                }
                button {
                    class: "menu-item",
                    onclick: move |_| {
                        open.set(false);
                        session.write().open_section(Section::Approvals);
                    },
                    Icon { name: "shield", size: 16 }
                    "Approvals"
                }
                button {
                    class: "menu-item",
                    onclick: move |_| {
                        open.set(false);
                        session.write().open_section(Section::Providers);
                    },
                    Icon { name: "grid", size: 16 }
                    "Providers"
                }
                button {
                    class: "menu-item",
                    onclick: move |_| {
                        open.set(false);
                        session.write().open_section(Section::Connectors);
                    },
                    Icon { name: "external", size: 16 }
                    "Connectors"
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
                        session.write().disconnect();
                        toast.set(Some(("Disconnected.".into(), false)));
                    },
                    Icon { name: "x", size: 16 }
                    "Disconnect"
                }
            }
        }
    }
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
                    title: "Group members",
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
    // Composer pickers: `Some(query)` while a bare `@query` (mention) or a
    // line-leading `/query` (skill/tool) is being typed.
    let mut mention = use_signal(|| Option::<String>::None);
    let mut mention_idx = use_signal(|| 0usize);
    let mut slash = use_signal(|| Option::<String>::None);
    let mut slash_idx = use_signal(|| 0usize);
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
            "const el=document.getElementById('messages'); if(el && el.scrollHeight - el.scrollTop - el.clientHeight < 80){el.scrollTop=el.scrollHeight;}",
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
    // Picker contents for the open pop (empty when no pop is open). The slash
    // pop wins over the mention pop when both triggers are present.
    let show_mention = mention().is_some() && slash().is_none();
    let show_slash = slash().is_some();
    let mention_q = mention().unwrap_or_default();
    let slash_q = slash().unwrap_or_default();
    let mention_bots = if show_mention {
        mention_bot_matches(&session.read(), &mention_q)
    } else {
        Vec::new()
    };
    let mention_conns = if show_mention {
        mention_connector_matches(&session.read(), &mention_q)
    } else {
        Vec::new()
    };
    let mention_total = mention_bots.len() + mention_conns.len();
    let mention_active = mention_idx().min(mention_total.saturating_sub(1));
    let slash_skills = if show_slash {
        slash_skill_matches(&session.read(), &slash_q)
    } else {
        Vec::new()
    };
    let slash_tools = if show_slash {
        slash_tool_matches(&slash_q)
    } else {
        Vec::new()
    };
    let slash_total = slash_skills.len() + slash_tools.len();
    let slash_active = slash_idx().min(slash_total.saturating_sub(1));
    // Copies for the key handler closures (they move their captures).
    let key_mention_bots = mention_bots.clone();
    let key_mention_conns = mention_conns.clone();
    let key_slash_skills = slash_skills.clone();
    let key_slash_tools = slash_tool_matches(&slash_q);
    rsx! {
        div { class: "pane-body chat-body",
            div { class: "chat-column",
                div { class: "chat-head",
                    div { class: "row",
                        span { class: "micro", "Conversation" }
                        // The model selector only applies to one-to-one bot
                        // chats; rooms and the owner chat stay auto-routed
                        // (the Providers page keeps the full ModelBar).
                        if is_one_to_one {
                            ModelBar { session }
                        }
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
                                    "Sections and folders organize the rail. Open a chat or a group to talk."
                                } else if is_one_to_one {
                                    "A one-to-one conversation. Replies come from {active_name}, not the orchestrator."
                                } else {
                                    "Ask the orchestrator, or open a group with your specialists."
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
                                                title: if in_team.contains(&bot_id) { "Already in the group" } else { "Add to the group" },
                                                onclick: {
                                                    let bot_id = bot_id.clone();
                                                    move |_| session.write().add_team_member(bot_id.clone())
                                                },
                                                "add to group"
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
                                div { class: "msg-body",
                                    MarkdownBody { content: content.clone() }
                                }
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
                            oninput: move |event| {
                                let value = event.value();
                                session.write().composer = value.clone();
                                mention.set(mention_query(&value));
                                mention_idx.set(0);
                                slash.set(slash_query(&value));
                                slash_idx.set(0);
                            },
                            onkeydown: move |event: KeyboardEvent| {
                                // Same priority as the render below: slash wins.
                                let mention_open = mention().is_some() && slash().is_none();
                                let slash_open = slash().is_some();
                                if mention_open || slash_open {
                                    match event.key() {
                                        Key::ArrowDown => {
                                            event.prevent_default();
                                            if mention_open {
                                                let count = key_mention_bots.len() + key_mention_conns.len();
                                                if count > 0 {
                                                    mention_idx.set((mention_idx() + 1).min(count - 1));
                                                }
                                            } else {
                                                let count = key_slash_skills.len() + key_slash_tools.len();
                                                if count > 0 {
                                                    slash_idx.set((slash_idx() + 1).min(count - 1));
                                                }
                                            }
                                        }
                                        Key::ArrowUp => {
                                            event.prevent_default();
                                            if mention_open {
                                                mention_idx.set(mention_idx().saturating_sub(1));
                                            } else {
                                                slash_idx.set(slash_idx().saturating_sub(1));
                                            }
                                        }
                                        Key::Enter => {
                                            event.prevent_default();
                                            event.stop_propagation();
                                            if mention_open {
                                                let count = key_mention_bots.len() + key_mention_conns.len();
                                                let idx = mention_idx().min(count.saturating_sub(1));
                                                let id = if idx < key_mention_bots.len() {
                                                    key_mention_bots.get(idx).map(|b| b.0.clone())
                                                } else {
                                                    key_mention_conns.get(idx - key_mention_bots.len()).cloned()
                                                };
                                                if let Some(id) = id {
                                                    let text = session.read().composer.clone();
                                                    session.write().composer = insert_mention_text(&text, &id);
                                                    mention.set(None);
                                                }
                                            } else {
                                                let count = key_slash_skills.len() + key_slash_tools.len();
                                                let idx = slash_idx().min(count.saturating_sub(1));
                                                let id = if idx < key_slash_skills.len() {
                                                    key_slash_skills.get(idx).map(|s| s.0.clone())
                                                } else {
                                                    key_slash_tools.get(idx - key_slash_skills.len()).map(|t| t.to_string())
                                                };
                                                if let Some(id) = id {
                                                    let text = session.read().composer.clone();
                                                    session.write().composer = insert_slash_text(&text, &id);
                                                    slash.set(None);
                                                }
                                            }
                                        }
                                        Key::Escape => {
                                            event.prevent_default();
                                            event.stop_propagation();
                                            mention.set(None);
                                            slash.set(None);
                                        }
                                        _ => {}
                                    }
                                    return;
                                }
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
                                        title: if team_collapsed() { "Show members" } else { "Hide members" },
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
                    if show_mention {
                        div { class: "mention-pop pop above open",
                            // Hidden closer: GLOBAL_KEYS_JS clicks it on Esc /
                            // outside pointerdown so the signal stays in sync.
                            button {
                                class: "escape-close",
                                style: "display: none;",
                                tabindex: -1,
                                onclick: move |_| {
                                    mention.set(None);
                                    mention_idx.set(0);
                                },
                            }
                            div { class: "pop-list",
                                if mention_total == 0 {
                                    div { class: "pop-empty", "No bots or connectors match." }
                                }
                                if !mention_bots.is_empty() {
                                    div { class: "pop-group", "Bots" }
                                    for (index, (id, name, avatar)) in mention_bots.iter().enumerate() {
                                        button {
                                            key: "{id}",
                                            class: if index == mention_active { "pop-item active" } else { "pop-item" },
                                            onclick: {
                                                let id = id.clone();
                                                move |_| {
                                                    let text = session.read().composer.clone();
                                                    session.write().composer = insert_mention_text(&text, &id);
                                                    mention.set(None);
                                                    mention_idx.set(0);
                                                }
                                            },
                                            span { class: "avatar sm bot", style: avatar_style(id), "{avatar}" }
                                            span { "{name}" }
                                            span { class: "micro", "@{id}" }
                                        }
                                    }
                                }
                                if !mention_conns.is_empty() {
                                    div { class: "pop-group", "Connectors" }
                                    for (offset, cid) in mention_conns.iter().enumerate() {
                                        button {
                                            key: "{cid}",
                                            class: if mention_bots.len() + offset == mention_active { "pop-item active" } else { "pop-item" },
                                            onclick: {
                                                let cid = cid.clone();
                                                move |_| {
                                                    let text = session.read().composer.clone();
                                                    session.write().composer = insert_mention_text(&text, &cid);
                                                    mention.set(None);
                                                    mention_idx.set(0);
                                                }
                                            },
                                            BrandLogo { brand: cid.clone(), size: 18 }
                                            span { "{cid}" }
                                        }
                                    }
                                }
                            }
                        }
                    }
                    if show_slash {
                        div { class: "mention-pop pop above open",
                            button {
                                class: "escape-close",
                                style: "display: none;",
                                tabindex: -1,
                                onclick: move |_| {
                                    slash.set(None);
                                    slash_idx.set(0);
                                },
                            }
                            div { class: "pop-list",
                                if slash_total == 0 {
                                    div { class: "pop-empty", "No skills or tools match." }
                                }
                                if !slash_skills.is_empty() {
                                    div { class: "pop-group", "Skills" }
                                    for (index, (id, name, description)) in slash_skills.iter().enumerate() {
                                        button {
                                            key: "{id}",
                                            class: if index == slash_active { "pop-item active" } else { "pop-item" },
                                            onclick: {
                                                let id = id.clone();
                                                move |_| {
                                                    let text = session.read().composer.clone();
                                                    session.write().composer = insert_slash_text(&text, &id);
                                                    slash.set(None);
                                                    slash_idx.set(0);
                                                }
                                            },
                                            div { style: "display: flex; flex-direction: column; align-items: flex-start; min-width: 0;",
                                                span { "{name}" }
                                                if !description.trim().is_empty() {
                                                    span { class: "caption", "{description}" }
                                                }
                                                span { class: "micro", "{id}" }
                                            }
                                        }
                                    }
                                }
                                if !slash_tools.is_empty() {
                                    div { class: "pop-group", "Tools" }
                                    for (offset, tool) in slash_tools.iter().enumerate() {
                                        button {
                                            key: "{tool}",
                                            class: if slash_skills.len() + offset == slash_active { "pop-item active" } else { "pop-item" },
                                            onclick: {
                                                let tool = tool.to_string();
                                                move |_| {
                                                    let text = session.read().composer.clone();
                                                    session.write().composer = insert_slash_text(&text, &tool);
                                                    slash.set(None);
                                                    slash_idx.set(0);
                                                }
                                            },
                                            div { style: "display: flex; flex-direction: column; align-items: flex-start; min-width: 0;",
                                                span { "{tool}" }
                                                if let Some(label) = capability_label(tool) {
                                                    span { class: "caption", "{label}" }
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

#[component]
fn ModelBar(session: Signal<Session>) -> Element {
    // Picker items: (value, label). "auto" first, then the catalog entries
    // (or the static provider model lists when the catalog is empty).
    let (items, current, allowed, saved) = {
        let session = session.read();
        let catalog = session.state.catalog.as_ref();
        let mut items: Vec<(String, String)> =
            vec![("auto".to_owned(), "Auto · approved route".to_owned())];
        let mut current = "auto".to_owned();
        let mut allowed = false;
        let mut saved = "auto".to_owned();
        if let Some(catalog) = catalog {
            if !catalog.selection.is_auto() {
                current = format!("{}|{}", catalog.selection.provider, catalog.selection.model);
            }
            for entry in &catalog.entries {
                items.push((
                    format!("{}|{}", entry.provider, entry.model),
                    format!(
                        "{} · {}",
                        human_model(&entry.model),
                        human_provider(&entry.provider)
                    ),
                ));
            }
            if catalog.entries.is_empty() {
                for provider in &catalog.providers {
                    for model in &provider.models {
                        items.push((
                            format!("{}|{}", provider.id, model),
                            format!("{} · {}", human_model(model), human_provider(&provider.id)),
                        ));
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
        (items, current, allowed, saved)
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
    let current_label = items
        .iter()
        .find(|(item_value, _)| *item_value == value)
        .map(|(_, label)| label.clone())
        .unwrap_or_else(|| "Auto · approved route".to_owned());
    let mut open = use_signal(|| false);
    let mut query = use_signal(String::new);
    let mut active_idx = use_signal(|| 0usize);
    // Case-insensitive substring match on the human label and on the raw
    // provider/model ids carried by the item value.
    let needle = query().trim().to_lowercase();
    let filtered: Vec<(usize, String, String)> = items
        .iter()
        .enumerate()
        .filter(|(_, item)| {
            needle.is_empty()
                || item.1.to_lowercase().contains(&needle)
                || item.0.to_lowercase().contains(&needle)
        })
        .map(|(index, item)| (index, item.0.clone(), item.1.clone()))
        .collect();
    let active = if filtered.is_empty() {
        0
    } else {
        active_idx().min(filtered.len() - 1)
    };
    // The key handler runs from its own closure: give it its own copy so the
    // pop-list below can still use `filtered`.
    let keyed = filtered.clone();
    rsx! {
        div { class: "model-bar",
            span { class: "micro", "Model" }
            div {
                class: "menu-wrap",
                onkeydown: move |event: KeyboardEvent| {
                    if !open() {
                        return;
                    }
                    let count = keyed.len();
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
                            event.stop_propagation();
                            if let Some((_, item_value, _)) =
                                keyed.get(active_idx().min(count.saturating_sub(1)))
                            {
                                let mut session = session.write();
                                set_model_draft(&mut session, item_value);
                                open.set(false);
                                query.set(String::new());
                            }
                        }
                        Key::Escape => {
                            event.prevent_default();
                            event.stop_propagation();
                            open.set(false);
                        }
                        _ => {}
                    }
                },
                button {
                    class: "btn ghost sm menu-trigger",
                    title: "Pick the model route for the next turn",
                    onclick: move |_| {
                        if !open() {
                            query.set(String::new());
                            let position = items.iter().position(|(v, _)| *v == value).unwrap_or(0);
                            active_idx.set(position);
                        }
                        open.set(!open());
                    },
                    span { "{current_label}" }
                    Icon { name: if open() { "chevron-up" } else { "chevron-down" }, size: 14 }
                }
                div {
                    class: if open() { "pop open" } else { "pop" },
                    style: "min-width: 280px;",
                    div { class: "pop-search",
                        input {
                            class: "input",
                            placeholder: "Search models…",
                            value: "{query}",
                            oninput: move |event| {
                                query.set(event.value());
                                active_idx.set(0);
                            },
                        }
                    }
                    div { class: "pop-list",
                        if filtered.is_empty() {
                            div { class: "pop-empty", "No models match." }
                        }
                        for (index, item_value, label) in filtered.clone() {
                            button {
                                key: "{item_value}",
                                class: if index == active { "pop-item active" } else { "pop-item" },
                                onclick: move |_| {
                                    let mut session = session.write();
                                    set_model_draft(&mut session, &item_value);
                                    open.set(false);
                                    query.set(String::new());
                                },
                                span { "{label}" }
                                if item_value == current {
                                    Icon { name: "check", size: 14 }
                                }
                            }
                        }
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
    let is_room = teams::is_room(&conversation);
    let mut tab = use_signal(|| InspectorTab::Details);
    let library = session.read().state.library.clone();
    let library_loaded = session.read().state.library_loaded;
    let library_loading = session.read().state.library_loading;
    let library_error = session.read().state.library_error.clone();
    let downloading = session.read().downloading;
    let rules_draft = session.read().team_rules_draft.clone();
    let (team_members, team_loaded, team_loading) = {
        let state = session.read();
        let members = state
            .state
            .team
            .as_ref()
            .map(teams::member_ids)
            .unwrap_or_default();
        (members, state.state.team_loaded, state.state.team_loading)
    };
    // A dir: conversation is a one-to-one bot chat: the Details tab shows
    // that bot's profile (from the cached GET /v1/bots/{id}) next to the
    // conversation card.
    let chat_bot_id = conversation.strip_prefix("dir:").map(str::to_owned);
    let bot_detail = chat_bot_id
        .as_deref()
        .and_then(|id| session.read().state.bot_details.get(id).cloned());
    let bot_live = chat_bot_id.as_deref().and_then(|id| {
        session
            .read()
            .state
            .listed_bots
            .iter()
            .find(|bot| bot.id == id)
            .map(|bot| bot.live)
    });
    let pins = session.read().pins.clone();
    let listed_skills = session.read().state.listed_skills.clone();
    let mut skill_query = use_signal(String::new);
    // Skills card: "add a skill" filter + the tiny saved caption after a write.
    let mut add_query = use_signal(String::new);
    let mut skills_saved = use_signal(|| false);
    let pins_view = {
        let rows: Vec<(String, String, String, bool)> = pins
            .iter()
            .map(|pin| {
                let state = session.read();
                (
                    pin.clone(),
                    state.pin_label(pin),
                    state.pin_avatar(pin),
                    state.pin_is_bot(pin),
                )
            })
            .collect();
        rsx! {
            if !rows.is_empty() {
                div { class: "pins",
                    for (pin, label, avatar, is_bot) in rows {
                        div {
                            key: "{pin}",
                            class: "pin-row",
                            title: "Open {label}",
                            onclick: {
                                let pin = pin.clone();
                                move |_| session.write().open_pin(pin.clone())
                            },
                            if is_bot {
                                span { class: "avatar sm bot", style: avatar_style(&pin), "{avatar}" }
                            } else {
                                span { class: "avatar sm user", "{avatar}" }
                            }
                            span { class: "thread-title", "{label}" }
                            PinToggle { session, id: pin.clone(), pinned: true }
                        }
                    }
                }
            }
        }
    };
    let bot_profile_view = if let Some(bot_id) = chat_bot_id.clone() {
        if let Some(detail) = bot_detail.clone() {
            let avatar = if detail.avatar.trim().is_empty() {
                session.read().bot_avatar(&detail.id)
            } else {
                detail.avatar.clone()
            };
            let fallback_provider = detail
                .fallback_provider
                .as_deref()
                .map(human_provider)
                .unwrap_or_else(|| "—".to_owned());
            let fallback_model = detail
                .fallback_model
                .as_deref()
                .map(human_model)
                .unwrap_or_else(|| "—".to_owned());
            let pinned_route = session
                .read()
                .state
                .catalog
                .as_ref()
                .map(|catalog| catalog.selection.clone());
            let skill_needle = skill_query().trim().to_lowercase();
            let skill_chips: Vec<String> = detail
                .skills
                .iter()
                .filter(|skill| {
                    skill_needle.is_empty() || skill.to_lowercase().contains(&skill_needle)
                })
                .cloned()
                .collect();
            // Skills the owner can still attach: valid ids, not already on the
            // bot, matching the add-filter.
            let attached_skills = detail.skills.clone();
            let skills_bot_id = detail.id.clone();
            let add_needle = add_query().trim().to_lowercase();
            let addable_skills: Vec<(String, String, String)> = listed_skills
                .iter()
                .filter(|skill| valid_skill_id(&skill.id))
                .filter(|skill| !attached_skills.iter().any(|s| s == &skill.id))
                .filter(|skill| {
                    add_needle.is_empty()
                        || skill.id.to_lowercase().contains(&add_needle)
                        || skill.name.to_lowercase().contains(&add_needle)
                })
                .map(|skill| {
                    (
                        skill.id.clone(),
                        skill.name.clone(),
                        skill.description.clone(),
                    )
                })
                .collect();
            let skills_full = attached_skills.len() >= MAX_BOT_SKILLS;
            let tool_rows: Vec<(String, bool, Option<&'static str>)> = detail
                .tools
                .iter()
                .map(|tool| {
                    (
                        tool.clone(),
                        detail.require_approval.iter().any(|item| item == tool),
                        capability_label(tool),
                    )
                })
                .collect();
            let instructions = detail.instructions.clone();
            let instruction_preview = instructions.as_ref().map(|text| {
                let truncated: String = text.chars().take(8000).collect();
                let cut = text.chars().count() > 8000;
                (truncated, if cut { "…" } else { "" })
            });
            rsx! {
                div { class: "card",
                    div { class: "card-head",
                        h3 { class: "card-title", "{detail.display_name}" }
                        span { class: "chip", "{detail.role}" }
                    }
                    div { class: "row",
                        span { class: "avatar lg bot", style: avatar_style(&detail.id), "{avatar}" }
                        div { class: "chips",
                            span {
                                class: if detail.enabled { "chip ok" } else { "chip warn" },
                                if detail.enabled { "enabled" } else { "disabled" }
                            }
                            if let Some(live) = bot_live {
                                span {
                                    class: if live { "chip ok" } else { "chip warn" },
                                    if live { "live" } else { "restart required" }
                                }
                            }
                        }
                    }
                    if !detail.description.trim().is_empty() {
                        p { class: "card-sub", "{detail.description}" }
                    }
                }
                div { class: "card",
                    div { class: "card-head",
                        h3 { class: "card-title", "Routing" }
                    }
                    div { class: "kv-row",
                        span { class: "kv-key", "Provider" }
                        span { class: "kv-val", "{human_provider(&detail.provider)}" }
                    }
                    div { class: "kv-row",
                        span { class: "kv-key", "Model" }
                        span { class: "kv-val", "{human_model(&detail.model)}" }
                    }
                    div { class: "kv-row",
                        span { class: "kv-key", "Fallback provider" }
                        span { class: "kv-val", "{fallback_provider}" }
                    }
                    div { class: "kv-row",
                        span { class: "kv-key", "Fallback model" }
                        span { class: "kv-val", "{fallback_model}" }
                    }
                    if !detail.routing_classes.is_empty() {
                        div { class: "chips",
                            for class in detail.routing_classes.clone() {
                                span { class: "chip muted", "{class}" }
                            }
                        }
                        p { class: "hint", "Routing classes this bot accepts." }
                    }
                    if let Some(selection) = pinned_route {
                        if !selection.is_auto()
                            && (selection.provider != detail.provider || selection.model != detail.model)
                        {
                            p { class: "hint",
                                "This conversation is pinned to {human_model(&selection.model)} via {human_provider(&selection.provider)} in the chat head."
                            }
                        }
                    }
                }
                div { class: "card",
                    div { class: "card-head",
                        h3 { class: "card-title", "Skills" }
                        span { class: "chip muted", "{detail.skills.len()} / {MAX_BOT_SKILLS}" }
                        if skills_saved() {
                            span { class: "caption", "saved" }
                        }
                    }
                    div { class: "skill-row",
                        input {
                            class: "input grow",
                            placeholder: "Filter skills…",
                            value: "{skill_query}",
                            oninput: move |event| skill_query.set(event.value()),
                        }
                    }
                    if detail.skills.is_empty() {
                        p { class: "hint", "No skills attached." }
                    } else if skill_chips.is_empty() {
                        p { class: "hint", "No skill matches the filter." }
                    } else {
                        div { class: "chips",
                            for skill in skill_chips {
                                {
                                    let bot_id = skills_bot_id.clone();
                                    let attached = attached_skills.clone();
                                    let skill_id = skill.clone();
                                    rsx! {
                                        span { class: "chip accent",
                                            "{skill}"
                                            button {
                                                class: "team-chip remove",
                                                title: "Remove {skill}",
                                                onclick: move |_| {
                                                    let mut next: Vec<String> = attached
                                                        .iter()
                                                        .filter(|s| **s != skill_id)
                                                        .cloned()
                                                        .collect();
                                                    next.retain(|s| valid_skill_id(s));
                                                    session.write().save_bot_config(
                                                        bot_id.clone(),
                                                        BotConfigUpdate {
                                                            skills: Some(next),
                                                            ..Default::default()
                                                        },
                                                    );
                                                    skills_saved.set(true);
                                                },
                                                Icon { name: "x", size: 12 }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                    div { class: "skill-row",
                        input {
                            class: "input grow",
                            placeholder: "Add a skill…",
                            value: "{add_query}",
                            oninput: move |event| {
                                add_query.set(event.value());
                                skills_saved.set(false);
                            },
                        }
                    }
                    if !addable_skills.is_empty() {
                        div {
                            class: "pop-list",
                            style: "max-height: 180px; margin-top: 6px; border: 1px solid var(--hairline); border-radius: var(--r-xs);",
                            for (id, name, description) in addable_skills {
                                {
                                    let bot_id = skills_bot_id.clone();
                                    let attached = attached_skills.clone();
                                    let pick_id = id.clone();
                                    rsx! {
                                        button {
                                            key: "{id}",
                                            class: "pop-item",
                                            disabled: skills_full,
                                            title: if skills_full { "Skill cap reached ({MAX_BOT_SKILLS})" } else { "Attach {id}" },
                                            onclick: move |_| {
                                                if !valid_skill_id(&pick_id) || attached.len() >= MAX_BOT_SKILLS {
                                                    return;
                                                }
                                                let mut next = attached.clone();
                                                if next.contains(&pick_id) {
                                                    return;
                                                }
                                                next.push(pick_id.clone());
                                                session.write().save_bot_config(
                                                    bot_id.clone(),
                                                    BotConfigUpdate {
                                                        skills: Some(next),
                                                        ..Default::default()
                                                    },
                                                );
                                                skills_saved.set(true);
                                                add_query.set(String::new());
                                            },
                                            div { style: "display: flex; flex-direction: column; align-items: flex-start; gap: 1px; min-width: 0;",
                                                span { "{name}" }
                                                if !description.trim().is_empty() {
                                                    span { class: "caption", "{description}" }
                                                }
                                                span { class: "micro", "{id}" }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    } else if !add_query().trim().is_empty() {
                        p { class: "hint", "No unattached skill matches." }
                    }
                    if skills_full {
                        p { class: "hint", "A bot carries at most {MAX_BOT_SKILLS} skills." }
                    }
                }
                div { class: "card",
                    div { class: "card-head",
                        h3 { class: "card-title", "Tools" }
                        span { class: "chip muted", "{detail.tools.len()}" }
                    }
                    if tool_rows.is_empty() {
                        p { class: "hint", "No tools enabled." }
                    } else {
                        div { class: "chips",
                            for (tool, needs_approval, _cap) in tool_rows.clone() {
                                span {
                                    class: if needs_approval { "chip warn" } else { "chip" },
                                    title: if needs_approval { "Requires approval" } else { "Enabled tool" },
                                    "{tool}"
                                }
                            }
                        }
                        div { class: "row",
                            for (tool, _needs_approval, cap) in tool_rows {
                                if let Some(label) = cap {
                                    span { class: "tiny", "{tool}: {label}" }
                                }
                            }
                        }
                    }
                }
                div { class: "card",
                    div { class: "card-head",
                        h3 { class: "card-title", "Instructions" }
                        span { class: "chip muted", "{detail.instructions_bytes} bytes" }
                    }
                    if let Some((preview, ellipsis)) = instruction_preview {
                        details {
                            summary { "Preview prompt" }
                            pre {
                                style: "max-height: 240px; overflow: auto; margin: 8px 0 0; white-space: pre-wrap;",
                                "{preview}{ellipsis}"
                            }
                        }
                        p { class: "hint", "{detail.instructions_bytes} bytes total; shown truncated to 8,000 characters." }
                    } else {
                        p { class: "hint", "No custom instructions; the role prompt applies." }
                    }
                }
                div { class: "card",
                    div { class: "card-head",
                        h3 { class: "card-title", "Memory & Library" }
                    }
                    div { class: "kv-row",
                        span { class: "kv-key", "Memories" }
                        span { class: "kv-val", "{detail.memory_count}" }
                    }
                    div { class: "kv-row",
                        span { class: "kv-key", "Artifacts" }
                        span { class: "kv-val", "{detail.artifact_count}" }
                    }
                    div { class: "row",
                        button {
                            class: "btn ghost sm",
                            onclick: {
                                let id = detail.id.clone();
                                move |_| session.write().open_bot(id.clone())
                            },
                            "Open in Fleet"
                        }
                        button {
                            class: "btn ghost sm",
                            onclick: move |_| session.write().open_section(Section::Memory),
                            "Open Memory"
                        }
                    }
                }
            }
        } else {
            rsx! {
                div { class: "card",
                    div { class: "card-head",
                        h3 { class: "card-title", "Bot profile" }
                        span { class: "spinner", title: "Loading bot record" }
                    }
                    div { class: "skeleton", " " }
                    p { class: "hint", "Loading the bot record from the server…" }
                    div { class: "row",
                        button {
                            class: "btn ghost sm",
                            onclick: {
                                let bot_id = bot_id.clone();
                                move |_| session.write().load_bot_detail(bot_id.clone())
                            },
                            Icon { name: "refresh", size: 14 }
                            "Refresh"
                        }
                    }
                }
            }
        }
    } else {
        rsx! {}
    };
    let details_view = rsx! {
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
    };
    let library_view = rsx! {
        div { class: "card",
            div { class: "card-head",
                h3 { class: "card-title", "Library" }
                if library_loading || downloading {
                    span { class: "spinner", title: "Working" }
                }
            }
            if let Some(error) = library_error.clone() {
                p { class: "hint", "{error}" }
            }
            if !library_loaded {
                p { class: "hint", "The library loads with the transcript." }
            } else if library.is_empty() {
                p { class: "hint", "No files yet. Approved decks and documents appear here." }
            } else {
                div { class: "roster",
                    for entry in library.clone() {
                        div { key: "{entry.action_id}", class: "roster-row",
                            div { class: "thread-copy",
                                span { class: "thread-title",
                                    "{entry.title.clone().or(entry.filename.clone()).unwrap_or_else(|| entry.hash.clone())}"
                                }
                                span { class: "thread-meta",
                                    "{humanize_label(&entry.tool)} · {stamp(&entry.created_at)}"
                                }
                            }
                            button {
                                class: "btn icon",
                                title: "Download to Downloads",
                                disabled: downloading,
                                onclick: {
                                    let hash = entry.hash.clone();
                                    move |_| session.write().download_artifact(hash.clone())
                                },
                                Icon { name: "download", size: 15 }
                            }
                        }
                    }
                }
            }
        }
    };
    let members_view = rsx! {
        div { class: "card",
            div { class: "card-head",
                h3 { class: "card-title", "Members" }
                button {
                    class: "btn ghost sm",
                    onclick: move |_| {
                        session.write().refresh_team();
                        overlay.set(Overlay::Team);
                    },
                    "Manage"
                }
            }
            if is_room && !team_loaded && !team_loading {
                button {
                    class: "btn ghost sm",
                    onclick: move |_| session.write().refresh_team(),
                    "Load members"
                }
            } else if team_members.is_empty() {
                p { class: "hint", "No members yet. Every enabled bot answers when no members are set." }
            } else {
                div { class: "roster",
                    for id in team_members.clone() {
                        {
                            let found = roster.iter().find(|(bot, ..)| bot == &id).cloned();
                            match found {
                                Some((_, name, avatar, role, enabled, live, model)) => rsx! {
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
                                },
                                None => rsx! {},
                            }
                        }
                    }
                }
            }
        }
        if is_room {
            div { class: "card",
                div { class: "card-head",
                    h3 { class: "card-title", "Group instructions" }
                }
                p { class: "hint", "Standing rules every member reads before answering in this group." }
                textarea {
                    class: "textarea",
                    value: "{rules_draft}",
                    placeholder: "For example: answer in English, cite sources, keep updates short…",
                    oninput: move |event| session.write().team_rules_draft = event.value(),
                }
                div { class: "row",
                    button {
                        class: "btn primary sm",
                        onclick: move |_| session.write().save_team_rules(),
                        "Save instructions"
                    }
                }
            }
        }
    };
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
                    {pins_view}
                    div { class: "inspector-tabs",
                        button {
                            class: if tab() == InspectorTab::Details { "tab active" } else { "tab" },
                            onclick: move |_| tab.set(InspectorTab::Details),
                            "Details"
                        }
                        button {
                            class: if tab() == InspectorTab::Library { "tab active" } else { "tab" },
                            onclick: move |_| tab.set(InspectorTab::Library),
                            "Library"
                        }
                        if is_room {
                            button {
                                class: if tab() == InspectorTab::Members { "tab active" } else { "tab" },
                                onclick: move |_| tab.set(InspectorTab::Members),
                                "Members"
                            }
                        }
                    }
                    {match tab() {
                        InspectorTab::Details => rsx! {
                            {details_view}
                            {bot_profile_view}
                        },
                        InspectorTab::Library => library_view,
                        InspectorTab::Members => members_view,
                    }}
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
    let chosen_label = chosen.map(|preset| preset.label).unwrap_or("the provider");
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
                            draft.provider.id = if preset.default_id.is_empty() {
                                String::new()
                            } else {
                                preset.default_id.to_owned()
                            };
                            draft.provider.kind = preset.kind.to_owned();
                            draft.provider.base_url = preset.base_url.to_owned();
                            draft.provider.secret_name = preset.secret_name.to_owned();
                            draft.provider.models = preset.models.join(", ");
                            draft.provider.catalog_fetch = preset.catalog_fetch;
                            // The key belongs to one provider; a new preset starts without one.
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
                            div { style: "display: flex; align-items: center; gap: 10px;",
                                BrandLogo { brand: preset.id.to_owned(), size: 28 }
                                div {
                                    h3 { class: "card-title", "{preset.label}" }
                                    span { class: "card-sub", "{preset.subtitle}" }
                                }
                            }
                            span { class: "chip accent", "{preset.badge}" }
                        }
                        if preset.auth == "oauth" {
                            div { class: "banner info", style: "margin: 8px 0; display: flex; flex-direction: column; gap: 6px;",
                                strong { "OAuth PKCE Authentication" }
                                p { class: "tiny", "Save the provider first. Then open the Providers page and press the primary “Sign in with {preset.label}” button on its card: your browser opens, you approve, and the tokens go straight to the server's encrypted store. Nothing is typed or pasted here." }
                            }
                        }
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
                    if chosen.is_some_and(|preset| preset.auth == "oauth") {
                        div { class: "field", style: "grid-column: 1 / -1;",
                            label { class: "field-label", "Sign-in" }
                            div { class: "banner info", style: "display: flex; flex-direction: column; gap: 6px;",
                                strong { "This provider uses OAuth, not an API key." }
                                p { class: "tiny", "After you save, use the “Sign in with {chosen_label}” button on this provider's card on the Providers page — not this form. Your browser opens, you approve, and the tokens land in the server's encrypted store ({secret_name}). Nothing is typed or pasted here." }
                            }
                        }
                    } else {
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
    let usage = session.read().state.usage.clone();
    // Per-day rows preformatted: (day label, "N tokens · M calls").
    let usage_days: Vec<(String, String)> = usage
        .as_ref()
        .map(|report| {
            report
                .per_day
                .iter()
                .take(14)
                .map(|bucket| {
                    (
                        bucket.day.clone().unwrap_or_else(|| "—".to_owned()),
                        format!("{} tokens · {} calls", bucket.total_tokens, bucket.calls),
                    )
                })
                .collect()
        })
        .unwrap_or_default();
    let usage_card = rsx! {
        div { class: "card",
            div { class: "card-head",
                h3 { class: "card-title", "Token usage" }
                button {
                    class: "btn ghost sm",
                    onclick: move |_| session.write().load_usage(7),
                    "Load last 7 days"
                }
            }
            if let Some(report) = usage.clone() {
                div { class: "kv-row",
                    span { class: "kv-key", "Calls" }
                    span { class: "kv-val", "{report.totals.calls}" }
                }
                div { class: "kv-row",
                    span { class: "kv-key", "Prompt tokens" }
                    span { class: "kv-val", "{report.totals.prompt_tokens}" }
                }
                div { class: "kv-row",
                    span { class: "kv-key", "Completion tokens" }
                    span { class: "kv-val", "{report.totals.completion_tokens}" }
                }
                div { class: "kv-row",
                    span { class: "kv-key", "Total tokens" }
                    span { class: "kv-val", "{report.totals.total_tokens}" }
                }
                if report.per_day.is_empty() {
                    p { class: "caption", "No calls in this window." }
                }
                for (day, summary) in usage_days.clone() {
                    div { class: "kv-row",
                        span { class: "kv-key", "{day}" }
                        span { class: "kv-val", "{summary}" }
                    }
                }
            } else {
                p { class: "caption", "No usage report loaded yet. Load the last 7 days to see calls and tokens." }
            }
        }
    };
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
                provider.auth == "oauth",
                provider_signed_in(&provider.kind, &provider.secrets),
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
                {usage_card}
                if !loaded {
                    div { class: "card",
                        div { class: "skeleton title" }
                        div { class: "skeleton block" }
                    }
                }
                div { class: "card-grid",
                    for (provider_id, provider_kind, managed, catalog_verified, catalog_fetch, egress, models, routing, secrets, is_oauth, signed_in) in provider_rows {
                        {
                            let is_connected = if is_oauth {
                                signed_in
                            } else {
                                secrets.iter().any(|(_, _, configured)| *configured) || provider_id == "ollama"
                            };
                            let signing_in = session.read().oauth_signing_in.as_deref() == Some(provider_id.as_str());
                            // Preset label for the kind, else the brand label.
                            let kind_label = provider_presets::presets()
                                .iter()
                                .find(|preset| preset.kind == provider_kind)
                                .map(|preset| preset.label)
                                .or_else(|| branding::brand_label(&provider_kind))
                                .unwrap_or(provider_kind.as_str())
                                .to_owned();
                            rsx! {
                                div { class: "card", key: "{provider_id}",
                                    div { class: "card-head",
                                        div { style: "display: flex; align-items: center; gap: 10px;",
                                            BrandLogo { brand: provider_id.clone(), size: 28 }
                                            div {
                                                h3 { class: "card-title", "{provider_id}" }
                                                span { class: "card-sub", "{provider_kind}" }
                                            }
                                        }
                                        div { style: "display: flex; align-items: center; gap: 6px;",
                                            span {
                                                class: if is_connected { "status-pill connected" } else { "status-pill disconnected" },
                                                if is_oauth {
                                                    if is_connected { "Signed in" } else { "Not signed in" }
                                                } else if is_connected { "Connected" } else { "Not connected" }
                                            }
                                            if managed {
                                                span { class: "chip accent", "managed" }
                                            }
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
                                            if configured || !secret_name.is_empty() {
                                                span { class: if configured { "chip accent" } else { "chip warn" },
                                                    "{role}: {secret_name}"
                                                }
                                            }
                                        }
                                    }
                                    if is_oauth {
                                        div { class: "card-actions",
                                            button {
                                                class: if is_connected { "btn ghost sm" } else { "btn primary sm" },
                                                title: "Sign in through your browser. Tokens are stored on the server and never shown here.",
                                                disabled: !can_save || signing_in,
                                                onclick: {
                                                    let id = provider_id.clone();
                                                    let kind = provider_kind.clone();
                                                    move |_| session.write().sign_in_provider(id.clone(), kind.clone())
                                                },
                                                BrandLogo { brand: provider_kind.clone(), size: 16 }
                                                if signing_in {
                                                    "Waiting for browser…"
                                                } else if is_connected {
                                                    "Sign in again with {kind_label}"
                                                } else {
                                                    "Sign in with {kind_label}"
                                                }
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
                    }
                }
                div { class: "card",
                    div { class: "card-head",
                        div {
                            h3 { class: "card-title", "Provider Directory — Available Integrations" }
                            p { class: "muted", "All supported model providers and gateways. Connect in one click or configure credentials." }
                        }
                    }
                    div { class: "integration-grid",
                        for preset in provider_presets::presets() {
                            {
                                let is_connected = providers.iter().any(|p| {
                                    (p.id == preset.id
                                        || p.kind == preset.kind
                                        || (preset.id == "antigravity" && p.kind == "google-antigravity"))
                                        && (p.secrets.values().any(|s| s.configured) || preset.id == "ollama")
                                });
                                rsx! {
                                    div { class: "integration-card", key: "{preset.id}",
                                        div {
                                            div { class: "card-head", style: "align-items: flex-start; margin-bottom: 8px;",
                                                div { style: "display: flex; align-items: center; gap: 10px;",
                                                    BrandLogo { brand: preset.id.to_owned(), size: 30 }
                                                    div {
                                                        h4 { style: "margin: 0; font-size: 0.98rem; font-weight: 600;", "{preset.label}" }
                                                        span { class: "card-sub", style: "font-size: 0.78rem;", "{preset.kind}" }
                                                    }
                                                }
                                                span { class: "chip accent", style: "font-size: 0.72rem; padding: 2px 7px;", "{preset.badge}" }
                                            }
                                            p { class: "tiny", style: "margin-bottom: 10px; line-height: 1.45;", "{preset.subtitle}" }
                                            div { class: "chips", style: "margin-bottom: 12px;",
                                                for model in preset.models.iter().take(3) {
                                                    span { class: "chip", style: "font-size: 0.7rem;", "{model}" }
                                                }
                                                if preset.models.len() > 3 {
                                                    span { class: "chip muted", style: "font-size: 0.7rem;", "+{preset.models.len() - 3} more" }
                                                }
                                            }
                                        }
                                        div { style: "display: flex; align-items: center; justify-content: space-between; border-top: 1px solid var(--hairline); padding-top: 10px; margin-top: 6px;",
                                            span {
                                                class: if is_connected { "status-pill connected" } else { "status-pill disconnected" },
                                                if is_connected { "Connected" } else { "Not connected" }
                                            }
                                            button {
                                                class: if is_connected { "btn ghost sm" } else { "btn primary sm" },
                                                onclick: {
                                                    let preset = *preset;
                                                    move |_| {
                                                        let mut draft = session.write();
                                                        draft.provider.id = if preset.default_id.is_empty() {
                                                            String::new()
                                                        } else {
                                                            preset.default_id.to_owned()
                                                        };
                                                        draft.provider.kind = preset.kind.to_owned();
                                                        draft.provider.base_url = preset.base_url.to_owned();
                                                        draft.provider.secret_name = preset.secret_name.to_owned();
                                                        draft.provider.models = preset.models.join(", ");
                                                        draft.provider.catalog_fetch = preset.catalog_fetch;
                                                        draft.provider.api_key.clear();
                                                        overlay.set(Overlay::AddProvider);
                                                    }
                                                },
                                                if is_connected { "Configure" } else { "Connect" }
                                            }
                                        }
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
                    p { class: "muted", "Enable Google, Microsoft, Notion, AgentMail and web research, and store their credentials." }
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
                for (suite_title, suite_ids) in [
                    ("Google Workspace", &["google_drive", "google_sheets", "google_calendar", "gmail"][..]),
                    ("Microsoft 365", &["outlook", "outlook_calendar"][..]),
                    ("Knowledge & Email", &["notion", "agentmail"][..]),
                    ("Web Research", &["tavily", "firecrawl"][..]),
                    ("Voice & Media", &["elevenlabs"][..]),
                ] {
                    {
                        let suite_connectors: Vec<_> = connectors
                            .iter()
                            .filter(|c| suite_ids.contains(&c.id.as_str()))
                            .cloned()
                            .collect();
                        if !suite_connectors.is_empty() {
                            rsx! {
                                h2 { class: "connector-suite-title", "{suite_title}" }
                                div { class: "card-grid",
                                    for connector in suite_connectors {
                                        {
                                            let id = connector.id.clone();
                                            let is_connected = connector.secrets.values().any(|s| s.configured);
                                            let display_title = match id.as_str() {
                                                "google_drive" => "Google Drive",
                                                "google_sheets" => "Google Sheets",
                                                "google_calendar" => "Google Calendar",
                                                "gmail" => "Gmail",
                                                "outlook" => "Microsoft Outlook",
                                                "outlook_calendar" => "Outlook Calendar",
                                                "notion" => "Notion",
                                                "agentmail" => "AgentMail",
                                                "tavily" => "Tavily Search",
                                                "firecrawl" => "Firecrawl",
                                                "elevenlabs" => "ElevenLabs",
                                                _ => &id,
                                            };
                                            let enabled = session
                                                .read()
                                                .connector_drafts
                                                .get(&id)
                                                .map(|draft| draft.enabled)
                                                .unwrap_or(connector.enabled);
                                            let testing = session.read().connector_testing.as_deref() == Some(id.as_str());
                                            let signing_in = session.read().oauth_signing_in.as_deref() == Some(id.as_str());
                                            let auth_mode = session
                                                .read()
                                                .connector_drafts
                                                .get(&id)
                                                .and_then(|draft| draft.auth.clone())
                                                .or_else(|| connector.auth.clone())
                                                .unwrap_or_default();
                                            let test_result = session.read().state.connector_tests.get(&id).cloned();
                                            let values = session
                                                .read()
                                                .connector_drafts
                                                .get(&id)
                                                .map(|draft| draft.values.clone())
                                                .unwrap_or_default();
                                            rsx! {
                                                div { class: "card", key: "{id}",
                                                    div { class: "card-head",
                                                        div { style: "display: flex; align-items: center; gap: 10px;",
                                                            BrandLogo { brand: id.clone(), size: 28 }
                                                            div {
                                                                h3 { class: "card-title", "{display_title}" }
                                                                span { class: "card-sub", "{connector.kind}" }
                                                            }
                                                        }
                                                        div { style: "display: flex; align-items: center; gap: 6px;",
                                                            span {
                                                                class: if is_connected { "status-pill connected" } else { "status-pill disconnected" },
                                                                if is_connected { "Connected" } else { "Not connected" }
                                                            }
                                                            span { class: if enabled { "chip ok" } else { "chip muted" },
                                                                if enabled { "enabled" } else { "disabled" }
                                                            }
                                                        }
                                                    }
                                                    if let Some(uri) = connector.redirect_uri.clone() {
                                                        code { class: "tiny", "{uri}" }
                                                    }
                                                    div { class: "chips",
                                                        for (role, secret) in &connector.secrets {
                                                            if secret.configured || secret.name.as_deref().is_some_and(|n| !n.is_empty()) {
                                                                span { class: if secret.configured { "chip mono accent" } else { "chip mono warn" },
                                                                    "{role}: {secret.name.clone().unwrap_or_default()}"
                                                                }
                                                            }
                                                        }
                                                    }
                                                    if connector.auth_modes.len() > 1 {
                                                        div { class: "field",
                                                            label { class: "field-label", "auth mode" }
                                                            select {
                                                                class: "input",
                                                                value: "{auth_mode}",
                                                                title: "How this connector authenticates",
                                                                onchange: {
                                                                    let id = id.clone();
                                                                    move |event| session.write().set_connector_auth(&id, event.value())
                                                                },
                                                                for mode in &connector.auth_modes {
                                                                    option { value: "{mode}", "{mode}" }
                                                                }
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
                                                    if let Some(result) = test_result {
                                                        p { class: if result.ok { "banner ok" } else { "banner err" },
                                                            "{result.message}"
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
                                                        if connector.oauth.is_some()
                                                            && connector.auth_modes.iter().any(|m| m == "oauth")
                                                        {
                                                            button {
                                                                class: "btn ghost sm",
                                                                title: "Sign in with your ElevenLabs account in your browser (PKCE)",
                                                                disabled: signing_in || !can_save,
                                                                onclick: {
                                                                    let id = id.clone();
                                                                    let callback_uri = connector
                                                                        .oauth
                                                                        .as_ref()
                                                                        .and_then(|o| o.callback_uri.clone())
                                                                        .unwrap_or_default();
                                                                    move |_| session.write().sign_in_connector(id.clone(), callback_uri.clone())
                                                                },
                                                                if signing_in { "Signing in…" } else { "Sign in" }
                                                            }
                                                        }
                                                        button {
                                                            class: "btn ghost sm",
                                                            title: "Run a read-only check of the saved credentials",
                                                            disabled: !can_save || testing,
                                                            onclick: {
                                                                let id = id.clone();
                                                                move |_| session.write().test_connector(id.clone())
                                                            },
                                                            if testing { "Testing…" } else { "Test" }
                                                        }
                                                    }
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        } else {
                            rsx! {}
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
    fn mention_query_tracks_a_trailing_at_token() {
        assert_eq!(mention_query(""), None);
        assert_eq!(mention_query("hello"), None);
        assert_eq!(mention_query("hey @"), Some(String::new()));
        assert_eq!(mention_query("hey @ana"), Some("ana".into()));
        assert_eq!(
            mention_query("hey @an-a_2 "),
            None,
            "trailing space closes the token"
        );
        assert_eq!(
            mention_query("mail a@b.example"),
            None,
            "non-slug tail rejected"
        );
        assert_eq!(mention_query("ping @Analyst"), None, "uppercase rejected");
        assert_eq!(
            mention_query(&format!("@{}", "a".repeat(33))),
            None,
            "longer than 32 chars rejected"
        );
        assert_eq!(
            mention_query(&format!("@{}", "a".repeat(32))),
            Some("a".repeat(32))
        );
        assert_eq!(mention_query("@one then @two"), Some("two".into()));
        assert_eq!(mention_query("héllo @ana wörld"), None);
        assert_eq!(mention_query("héllo @ana"), Some("ana".into()));
    }

    #[test]
    fn slash_query_tracks_a_line_leading_command() {
        assert_eq!(slash_query(""), None);
        assert_eq!(slash_query("hello"), None);
        assert_eq!(slash_query("/"), Some(String::new()));
        assert_eq!(slash_query("/sk"), Some("sk".into()));
        assert_eq!(slash_query("line one\n/de"), Some("de".into()));
        assert_eq!(slash_query("/deck build"), None, "space ends the command");
        assert_eq!(slash_query("https://x"), None, "not at line start");
        assert_eq!(slash_query("a /b"), None, "not at line start");
    }

    #[test]
    fn insertions_replace_only_the_trailing_trigger() {
        assert_eq!(insert_mention_text("hey @an", "analyst"), "hey @analyst ");
        assert_eq!(insert_mention_text("@", "gmail"), "@gmail ");
        assert_eq!(insert_mention_text("no trigger", "x"), "no trigger@x ");
        assert_eq!(
            insert_mention_text("one\ntwo @mk", "maker"),
            "one\ntwo @maker "
        );
        assert_eq!(insert_slash_text("/sk", "skill_load"), "skill_load ");
        assert_eq!(
            insert_slash_text("line\n/de", "deck_build"),
            "line\ndeck_build "
        );
        assert_eq!(insert_slash_text("/", "rag_search"), "rag_search ");
    }

    #[test]
    fn mention_and_markdown_helpers_never_panic_on_multibyte_input() {
        // Fuzz-ish sweep: none of these may panic, whatever the bytes.
        let inputs = [
            "@analyst hi",
            "ciao @ana",
            "@😀",
            "é@x",
            "😀 @analyst",
            "héllo wörld @maker 😀",
            "@",
            "@@",
            &format!("@{}", "a".repeat(40)),
            "line\n@mk",
            "/",
            "/dé",
            "é/de",
        ];
        for input in inputs {
            let q = mention_query(input);
            let s = slash_query(input);
            if let Some(q) = q {
                let _ = insert_mention_text(input, &q);
            }
            if let Some(s) = s {
                let _ = insert_slash_text(input, &s);
            }
            // A user message is rendered through the markdown parser on submit.
            let _ = parse_markdown_blocks(input);
            let _ = parse_inline(input);
        }
        // Markdown edge cases, incl. multibyte right after formatting tokens.
        let md = [
            "**bold**😀",
            "😀",
            "é",
            "`code`é",
            "[lbl](https://x)é",
            "*it*😀",
            "# é",
            "- é😀",
            "| a | b |\n|---|---|\n| é | 😀 |",
            "```rust\nlet é = 1;\n```",
            "> é 😀",
            "**`c`**😀",
        ];
        for input in md {
            let _ = parse_markdown_blocks(input);
            let _ = parse_inline(input);
        }
    }

    #[test]
    fn model_picker_values_parse_like_the_old_select() {
        assert!(parse_model_value("auto").is_auto());
        assert!(parse_model_value("auto|").is_auto());
        assert!(parse_model_value("garbage").is_auto());
        let parsed = parse_model_value("groq|llama-3.3-70b");
        assert_eq!(parsed.provider, "groq");
        assert_eq!(parsed.model, "llama-3.3-70b");
        assert!(!parsed.is_auto());
    }

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

    #[test]
    fn markdown_parser_handles_screenshot_patterns() {
        let text = r#"### **3. Cosa NON ho in questa sessione**
- **`firecrawl_search`** o **`firecrawl_scrape`**: non sono attivi
---
*Nota*: i connettori sono configurabili via UI."#;

        let blocks = parse_markdown_blocks(text);
        assert_eq!(blocks.len(), 4);

        match &blocks[0] {
            MdBlock::Heading { level, text } => {
                assert_eq!(*level, 3);
                assert_eq!(text, "3. Cosa NON ho in questa sessione");
            }
            other => panic!("expected Heading, got {:?}", other),
        }

        match &blocks[1] {
            MdBlock::UnorderedList(items) => {
                assert_eq!(items.len(), 1);
                let spans = parse_inline(&items[0]);
                assert_eq!(
                    spans,
                    vec![
                        InlineSpan::Bold(vec![InlineSpan::Code("firecrawl_search".into())]),
                        InlineSpan::Text(" o ".into()),
                        InlineSpan::Bold(vec![InlineSpan::Code("firecrawl_scrape".into())]),
                        InlineSpan::Text(": non sono attivi".into()),
                    ]
                );
            }
            other => panic!("expected UnorderedList, got {:?}", other),
        }

        assert_eq!(blocks[2], MdBlock::Rule);

        match &blocks[3] {
            MdBlock::Paragraph(para) => {
                let spans = parse_inline(para);
                assert_eq!(
                    spans,
                    vec![
                        InlineSpan::Italic("Nota".into()),
                        InlineSpan::Text(": i connettori sono configurabili via UI.".into()),
                    ]
                );
            }
            other => panic!("expected Paragraph, got {:?}", other),
        }
    }

    #[test]
    fn markdown_parser_handles_tables_and_codeblocks() {
        let text = "```rust\nfn main() {\n    println!(\"Hello\");\n}\n```\n\n| Name | Role | Status |\n|---|---|---|\n| Analyst | Finance | Live |\n| Maker | Slides | Off |\n";
        let blocks = parse_markdown_blocks(text);
        assert_eq!(blocks.len(), 2);

        match &blocks[0] {
            MdBlock::CodeBlock { lang, code } => {
                assert_eq!(lang, "rust");
                assert!(code.contains("println!"));
            }
            other => panic!("expected CodeBlock, got {:?}", other),
        }

        match &blocks[1] {
            MdBlock::Table { headers, rows } => {
                assert_eq!(headers, &["Name", "Role", "Status"]);
                assert_eq!(rows.len(), 2);
                assert_eq!(rows[0], &["Analyst", "Finance", "Live"]);
                assert_eq!(rows[1], &["Maker", "Slides", "Off"]);
            }
            other => panic!("expected Table, got {:?}", other),
        }
    }
}
