//! Dioxus Desktop shell. Windows-only; the Rust session owns all network work.
//!
//! Layout: a three-pane macOS-style shell (glass sidebar, sticky glass topbar,
//! centered chat column, optional contextual inspector). The design system lives
//! in `assets/app.css`; this file owns the markup and the chat ergonomics.

#![cfg_attr(not(windows), allow(dead_code))]

use dioxus::prelude::*;
use serabot_desktop::{
    ApprovalDecision, ConversationKind, MAX_TEAM_MEMBERS, MemoryEntry, MemoryStatsView, Role,
    SPECIALIST_TOOLS, Selection, TaskStatus, Team, TeamMember, TeamMode,
    activity::{Step, StepPhase},
    audio_capture::MicRecorder,
    branding, chat_compose, chat_feed, provider_presets, provider_view,
    session::{Section, Session, SettingsSection, SignInPhase, Theme, humanize_label},
    teams, ui_highlight,
};
use std::collections::{BTreeMap, HashMap, HashSet};

use crate::ui_bots::{NewBotSheet, NewGroupSheet, OnboardingView};
use crate::ui_browser::BrowserView;
use crate::ui_connectors::ConnectorsView;
use crate::ui_dock::BrowserDock;
use crate::ui_files::{
    CONFIRM_RESET_JS, ComposerFiles, MediaCards, MessageFiles, PASTE_BRIDGE_JS, PastedFile,
    copy_script, decode_pasted,
};
use crate::ui_settings::SettingsPanel;
use crate::ui_settings_page::{
    AboutCard, AppearanceCard, ChatPrefsCard, LocalFoldersCard, NotificationsCard, SettingsNav,
};
use crate::ui_usage::UsagePage;
use crate::ui_vault::VaultPage;

const CSS: &str = include_str!("../assets/app.css");
/// Reports whether the system prefers a dark theme, now and on every change.
const SYSTEM_THEME_JS: &str = "const m = window.matchMedia('(prefers-color-scheme: dark)'); dioxus.send(m.matches); m.addEventListener('change', (e) => dioxus.send(e.matches));";
/// Styles of the 0.17 usage and settings pages, kept apart from app.css.
const CSS_PAGES: &str = concat!(
    include_str!("../assets/usage.css"),
    include_str!("../assets/settings.css"),
    include_str!("../assets/files.css")
);

/// A chat row: id, owner flag, speaker, avatar, bot, time, body, peer flag.
/// One prepared transcript row: what the message draws, the day heading
/// where a new day starts, and the actions it offers.
#[derive(Clone, PartialEq)]
struct MsgView {
    id: String,
    user: bool,
    speaker: String,
    avatar: String,
    bot: Option<String>,
    stamp: String,
    content: String,
    peer: bool,
    class: String,
    day: Option<String>,
    /// The last user message while idle: offers Edit.
    edit: bool,
    /// The last bot reply while idle: Regenerate sends this text again.
    regen: Option<String>,
}

/// A roster row: bot id, display name, avatar, enabled, live, role.
type RosterRow = (String, String, String, bool, bool, String);

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

/// One visible picker row: index, value, label, optional provider header
/// (brand, name) and the row's metadata.
type PickerItem = (
    usize,
    String,
    String,
    Option<(String, String)>,
    Option<provider_view::PickerRow>,
);

/// The provider kinds the server understands.
const PROVIDER_KINDS: [&str; 20] = [
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
    "gemini",
    "perplexity",
    "huggingface",
    "nebius",
    "sambanova",
    "muse-code",
];

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

pub(crate) fn stamp(raw: &str) -> String {
    raw.chars().take(16).collect::<String>().replace('T', " ")
}

/// Short timestamp for the chat list: "10:33" when the message is from today
/// (server stamps are UTC), "Nov 1 10:33" otherwise. A full date on every row
/// is noise; day separators carry the date instead.
fn msg_stamp(raw: &str) -> String {
    let Some(time) = raw.get(11..16) else {
        return stamp(raw);
    };
    let date = raw.get(..10).unwrap_or_default();
    if date == today_utc_date() {
        return time.to_owned();
    }
    let Some((m, d)) = parse_md(date) else {
        return stamp(raw);
    };
    const MONTHS: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    let month = MONTHS
        .get(m.wrapping_sub(1) as usize)
        .copied()
        .unwrap_or("?");
    format!("{month} {d} {time}")
}

/// Today's date as `YYYY-MM-DD` in UTC, without pulling in a date crate.
fn today_utc_date() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    civil_from_days((secs / 86_400) as i64)
}

/// Parse the month and day of a `YYYY-MM-DD` date.
fn parse_md(date: &str) -> Option<(u32, u32)> {
    let m = date.get(5..7)?.parse().ok()?;
    let d = date.get(8..10)?.parse().ok()?;
    Some((m, d))
}

/// Howard Hinnant's civil-from-days algorithm: days since 1970-01-01 to
/// `YYYY-MM-DD`.
fn civil_from_days(days: i64) -> String {
    let z = days + 719_468;
    let era = (if z >= 0 { z } else { z - 146_096 }) / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!("{y:04}-{m:02}-{d:02}")
}

/// Overlay surfaces that sit above the shell.
#[derive(Clone, Copy, PartialEq)]
pub(crate) enum Overlay {
    None,
    /// The create-bot sheet: name, picture, model, group.
    NewBot,
    /// The create-group sheet: name, picture, members, rules, model.
    NewGroup,
    /// The new-conversation sheet. The kind lives in the session draft, so
    /// the menu, the sheet and the Threads form always agree.
    NewConversation,
    /// The guided "Add provider" sheet.
    AddProvider,
    Team,
}

fn avatar_hue(id: &str) -> u16 {
    let mut hash: u32 = 0;
    for byte in id.bytes() {
        hash = hash.wrapping_mul(31).wrapping_add(u32::from(byte));
    }
    (hash % 360) as u16
}

/// Only the hue travels to the DOM; the colors themselves are CSS tokens.
pub(crate) fn avatar_style(id: &str) -> String {
    serabot_desktop::avatars::tile_style(
        &serabot_desktop::avatars::css_key("bot", id),
        avatar_hue(id),
    )
}

/// Tile style for a conversation row: a direct chat wears its bot's picture,
/// a group its own, anything else the hue only.
fn conversation_avatar_style(id: &str) -> String {
    if id.starts_with("room:") {
        group_avatar_style(id)
    } else {
        avatar_style(id.strip_prefix("dir:").unwrap_or(id))
    }
}

/// Same for a group (room) tile; `id` is the room conversation id.
pub(crate) fn group_avatar_style(id: &str) -> String {
    serabot_desktop::avatars::tile_style(
        &serabot_desktop::avatars::css_key("grp", id),
        avatar_hue(id),
    )
}

pub(crate) fn human_model(model: &str) -> String {
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

pub(crate) fn human_provider(provider: &str) -> String {
    if provider == "default" {
        "Default provider".into()
    } else {
        human_model(provider)
    }
}

fn capability_label(tool: &str) -> Option<&'static str> {
    match tool {
        "deck_build" => Some("Slide decks"),
        "docx_build" | "docx_import" | "docx_edit" | "docx_inspect" | "docx_read" => {
            Some("Word documents")
        }
        "xlsx_build" | "xlsx_import" | "xlsx_edit" | "xlsx_inspect" | "xlsx_read" => {
            Some("Excel workbooks")
        }
        "pptx_import" | "pptx_inspect" | "pptx_edit" => Some("PowerPoint decks"),
        "pdf_inspect" | "pdf_edit" | "pdf_merge" => Some("PDF pages"),
        "image_inspect" | "image_resize" | "image_convert" => Some("Images"),
        "local_list" | "local_search" | "local_stat" | "local_read" | "local_write"
        | "local_import" | "local_export" => Some("Folders on this PC"),
        "xlsx_analyze" | "timesheet_analyze" => Some("Spreadsheet analysis"),
        "rag_search" | "document_index" => Some("Document search"),
        "fetch_url" | "watch_now" | "firecrawl_scrape" | "firecrawl_map" | "firecrawl_search"
        | "firecrawl_crawl" | "exa_search" | "exa_get_contents" => Some("Web research"),
        "audio_transcribe" | "audio_understand" => Some("Audio notes"),
        "elevenlabs_text_to_speech"
        | "elevenlabs_sound_effect"
        | "elevenlabs_transcribe"
        | "elevenlabs_generate_image"
        | "elevenlabs_generate_video" => Some("Voice and media"),
        "elevenlabs_mcp_list_tools" | "elevenlabs_mcp_call" => Some("ElevenLabs tools"),
        "onedrive_upload" | "onedrive_list" => Some("OneDrive files"),
        "github_search_repos"
        | "github_list_issues"
        | "github_get_issue"
        | "github_create_issue"
        | "github_comment_issue"
        | "github_get_file" => Some("GitHub"),
        "todoist_list_tasks" | "todoist_add_task" | "todoist_complete_task" => {
            Some("Todoist tasks")
        }
        "slack_list_channels" | "slack_read_channel" | "slack_post_message" => Some("Slack"),
        "skill_list" | "skill_search" | "skill_load" => Some("Skills"),
        "delegate" => Some("Team handoffs"),
        _ => None,
    }
}

/// The host part of the first `http(s)` URL in `text`, if any.
#[allow(dead_code)]
fn host_in(text: &str) -> Option<String> {
    let start = text.find("://")? + 3;
    let rest = &text[start..];
    let end = rest
        .find(|c: char| c == '/' || c == '?' || c == '#' || c.is_whitespace())
        .unwrap_or(rest.len());
    let host = rest[..end].trim_start_matches("www.");
    (!host.is_empty()).then(|| host.to_owned())
}

/// The vault entry name quoted as `secret 'name'` in a tool summary.
#[allow(dead_code)]
fn secret_in(text: &str) -> Option<String> {
    let start = text.find("secret '")? + "secret '".len();
    let end = text[start..].find('\'')?;
    let name = &text[start..start + end];
    (!name.is_empty()).then(|| name.to_owned())
}

/// A plain sentence for a tool the owner is likely to see, or `None` for a
/// tool without one (the caller falls back to the tool's own label).
/// `done` picks the past tense for finished work and the "wants to" form for a
/// request that waits on the owner.
#[allow(dead_code)]
fn tool_sentence(tool: &str, summary: &str, done: bool) -> Option<String> {
    let sentence = match tool {
        "browser_type" if secret_in(summary).is_some() => {
            let name = secret_in(summary)?;
            if done {
                format!("Signed in with your saved login \"{name}\". The bot never saw it.")
            } else {
                format!("Wants to sign in with your saved login \"{name}\". The bot never sees it.")
            }
        }
        "browser_navigate" => {
            let site = host_in(summary).unwrap_or_else(|| "a web page".to_owned());
            if done {
                format!("Opened {site}")
            } else {
                format!("Wants to open {site}")
            }
        }
        "browser_click" => if done {
            "Clicked on the page"
        } else {
            "Wants to click on the page"
        }
        .to_owned(),
        "browser_type" => if done {
            "Typed into the page"
        } else {
            "Wants to type into the page"
        }
        .to_owned(),
        "browser_press" => if done {
            "Pressed a key"
        } else {
            "Wants to press a key"
        }
        .to_owned(),
        "browser_snapshot" | "browser_screenshot" => "Looked at the page".to_owned(),
        "browser_tabs" => "Checked the browser tabs".to_owned(),
        "browser_new_tab" => if done {
            "Opened a new tab"
        } else {
            "Wants to open a new tab"
        }
        .to_owned(),
        "browser_close" => if done {
            "Closed the browser"
        } else {
            "Wants to close the browser"
        }
        .to_owned(),
        "vault_list" => "Checked which saved logins exist".to_owned(),
        _ => return None,
    };
    Some(sentence)
}

/// The `@mention` query being typed at the end of `text`, if any: the last
/// `@` whose tail contains no whitespace, is at most 32 chars and matches
/// `[a-z0-9-_]*`. A bare `@` yields `Some("")`.
fn mention_query(text: &str) -> Option<String> {
    let at = text.rfind('@')?;
    let tail = text.get(at + 1..)?;
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
        Some(at) => format!("{}@{} ", text.get(..at).unwrap_or_default(), id),
        None => format!("{}@{} ", text, id),
    }
}

/// Replace the `/query` at the start of the last line of `text` with `id `.
fn insert_slash_text(text: &str, id: &str) -> String {
    let line_start = text.rfind('\n').map(|i| i + 1).unwrap_or(0);
    format!("{}{} ", text.get(..line_start).unwrap_or_default(), id)
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

/// Taskbar / title-bar icon: a 64x64 RGBA bitmap generated from the brand
/// mascot (see `assets/icon-64.rgba`). `None` falls back to the default icon.
#[cfg(windows)]
fn window_icon() -> Option<dioxus::desktop::tao::window::Icon> {
    const RGBA: &[u8] = include_bytes!("../assets/icon-64.rgba");
    dioxus::desktop::tao::window::Icon::from_rgba(RGBA.to_vec(), 64, 64).ok()
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
                    .with_window_icon(window_icon())
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
    let mut system_dark = use_signal(|| true);
    use_future(move || async move {
        let mut feed = dioxus::document::eval(SYSTEM_THEME_JS);
        while let Ok(dark) = feed.recv::<bool>().await {
            system_dark.set(dark);
        }
    });
    let theme = match session.read().theme {
        Theme::System if system_dark() => "dark",
        Theme::System => "light",
        other => other.as_str(),
    };
    let (text_size, motion) = {
        let s = session.read();
        (
            s.prefs.text_size.clone(),
            if s.prefs.reduce_motion {
                "reduce"
            } else {
                "full"
            },
        )
    };
    let _revision = revision();
    rsx! {
        style { {CSS} }
        style { {CSS_PAGES} }
        div { class: "root", "data-theme": theme, "data-text": "{text_size}", "data-motion": motion,
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
pub(crate) fn Icon(name: String, #[props(default = 20)] size: u32) -> Element {
    let path = match name.as_str() {
        "chat" => "M4 5h16v11H8l-4 4V5z",
        "search" => "M11 4a7 7 0 1 0 0 14 7 7 0 0 0 0-14zM20 20l-4-4",
        "plus" => "M12 5v14M5 12h14",
        "send" => "M4 12l16-8-6 16-2-6-8-2z",
        "copy" => "M9 9h10v10H9zM5 15V5h10",
        "arrow-down" => "M12 5v14M6 13l6 6 6-6",
        "stop" => "M7 7h10v10H7z",
        "mic" => "M12 3a3 3 0 0 1 3 3v6a3 3 0 0 1-6 0V6a3 3 0 0 1 3-3zM5 11a7 7 0 0 0 14 0M12 18v3",
        "paperclip" => {
            "M20 11l-8.5 8.5a5 5 0 0 1-7-7L13 4a3.5 3.5 0 0 1 5 5l-8.5 8.5a2 2 0 0 1-3-3L15 6"
        }
        "image" => "M4 5h16v14H4zM4 16l5-5 4 4 3-3 4 4M9 9.5a1 1 0 1 1-2 0 1 1 0 0 1 2 0z",
        "settings" => {
            "M12 15a3 3 0 1 0 0-6 3 3 0 0 0 0 6zM12.2 2h-.4a2 2 0 0 0-2 2v.2a2 2 0 0 1-1 1.7l-.4.3a2 2 0 0 1-2 0l-.2-.1a2 2 0 0 0-2.7.7l-.2.4a2 2 0 0 0 .7 2.7l.2.1a2 2 0 0 1 1 1.7v.5a2 2 0 0 1-1 1.7l-.2.1a2 2 0 0 0-.7 2.7l.2.4a2 2 0 0 0 2.7.7l.2-.1a2 2 0 0 1 2 0l.4.3a2 2 0 0 1 1 1.7v.2a2 2 0 0 0 2 2h.4a2 2 0 0 0 2-2v-.2a2 2 0 0 1 1-1.7l.4-.3a2 2 0 0 1 2 0l.2.1a2 2 0 0 0 2.7-.7l.2-.4a2 2 0 0 0-.7-2.7l-.2-.1a2 2 0 0 1-1-1.7v-.5a2 2 0 0 1 1-1.7l.2-.1a2 2 0 0 0 .7-2.7l-.2-.4a2 2 0 0 0-2.7-.7l-.2.1a2 2 0 0 1-2 0l-.4-.3a2 2 0 0 1-1-1.7V4a2 2 0 0 0-2-2z"
        }
        "users" => {
            "M9 11a3 3 0 1 0 0-6 3 3 0 0 0 0 6zM3 19a6 6 0 0 1 12 0M17 11a3 3 0 0 0 0-6M16 14a5 5 0 0 1 5 5"
        }
        "robot" => {
            "M7 8h10a2 2 0 0 1 2 2v6a2 2 0 0 1-2 2H7a2 2 0 0 1-2-2v-6a2 2 0 0 1 2-2zM12 4v4M9 13h.01M15 13h.01"
        }
        "check" => "M5 13l4 4 10-10",
        "star" => "M12 3l2.7 5.6 6.1.9-4.4 4.3 1 6.1L12 17l-5.4 2.9 1-6.1L3.2 9.5l6.1-.9L12 3z",
        "x" => "M6 6l12 12M18 6L6 18",
        "chevron-down" => "M6 9l6 6 6-6",
        "chevron-up" => "M6 15l6-6 6 6",
        "chevron-right" => "M9 6l6 6-6 6",
        "chevron-left" => "M15 6l-6 6 6 6",
        "maximize" => "M4 9V4h5M20 9V4h-5M4 15v5h5M20 15v5h-5",
        "minimize" => "M9 4v5H4M15 4v5h5M9 20v-5H4M15 20v-5h5",
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
        "connectors" => "M9 2v6M15 2v6M6 8h12v4a6 6 0 0 1-12 0V8zM12 18v4",
        "approvals" => "M12 3a9 9 0 1 0 0 18 9 9 0 0 0 0-18zM8 12l3 3 5-6",
        "fleet" => "M4 4h6v6H4zM14 4h6v6h-6zM4 14h6v6H4zM14 14h6v6h-6z",
        "lock" => "M6 11h12v9H6zM8 11V8a4 4 0 0 1 8 0v3",
        "usage" => "M5 20V10M12 20V4M19 20v-7",
        "globe" => {
            "M12 3a9 9 0 1 0 0 18 9 9 0 0 0 0-18zM3 12h18M12 3c3 3 3 15 0 18M12 3c-3 3-3 15 0 18"
        }
        "key" => "M14.5 9.5a4 4 0 1 1-1.2-2.8L21 14.4V18h-3.6v-2h-2v-2h-2z",
        "panel" => "M4 5h16v14H4zM15 5v14",
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

/// Brand logos for all supported connectors and providers. The embedded
/// images live in [`branding`]; a brand without a known mark falls back to
/// an initial-letter avatar.
#[component]
pub fn BrandLogo(brand: String, #[props(default = 24)] size: u32) -> Element {
    let title = branding::brand_label(&brand).map(str::to_owned);
    let image = branding::brand_image_src(&brand);
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
            if let Some(src) = image {
                img {
                    src: "{src}",
                    alt: "",
                    draggable: "false",
                    style: "width: 100%; height: 100%; object-fit: contain; border-radius: inherit;",
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
    OrderedList {
        start: usize,
        items: Vec<String>,
    },
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
            let after_hashes = trimmed.get(hash_count..).unwrap_or_default();
            if hash_count <= 6 && after_hashes.starts_with(' ') {
                let rest = after_hashes.trim();
                let title = rest
                    .strip_prefix("**")
                    .and_then(|inner| inner.strip_suffix("**"))
                    .unwrap_or(rest);
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
                    items.push(
                        l_trim
                            .get(marker_len..)
                            .unwrap_or_default()
                            .trim()
                            .to_owned(),
                    );
                    i += 1;
                } else if !l_trim.is_empty()
                    && (lines[i].starts_with("  ") || lines[i].starts_with('\t'))
                    && !items.is_empty()
                {
                    if let Some(last) = items.last_mut() {
                        last.push(' ');
                        last.push_str(l_trim);
                    }
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
            // The first number is the list's start: a list that continues
            // after other blocks (or a blank line) must not restart at 1.
            let start = first_word
                .trim_end_matches('.')
                .parse::<usize>()
                .unwrap_or(1);
            let mut items = Vec::new();
            while i < lines.len() {
                let l_trim = lines[i].trim();
                let fw = l_trim.split_whitespace().next().unwrap_or("");
                if l_trim.is_empty() && !items.is_empty() {
                    // A blank line inside a loose list keeps it one list.
                    let next = lines[i + 1..]
                        .iter()
                        .map(|l| l.trim())
                        .find(|l| !l.is_empty());
                    let continues = next.is_some_and(|l| {
                        let w = l.split_whitespace().next().unwrap_or("");
                        w.ends_with('.')
                            && !w.trim_end_matches('.').is_empty()
                            && w.trim_end_matches('.').chars().all(|c| c.is_ascii_digit())
                    });
                    if continues {
                        i += 1;
                        continue;
                    }
                    break;
                }
                if fw.ends_with('.') && fw.trim_end_matches('.').chars().all(|c| c.is_ascii_digit())
                {
                    let item_text = l_trim.get(fw.len()..).unwrap_or_default().trim().to_owned();
                    items.push(item_text);
                    i += 1;
                } else if !l_trim.is_empty()
                    && (lines[i].starts_with("  ") || lines[i].starts_with('\t'))
                    && !items.is_empty()
                {
                    if let Some(last) = items.last_mut() {
                        last.push(' ');
                        last.push_str(l_trim);
                    }
                    i += 1;
                } else {
                    break;
                }
            }
            blocks.push(MdBlock::OrderedList { start, items });
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

/// A fenced code block: the language label, a Copy button that confirms on
/// the button itself, and token colours from `ui_highlight`.
#[component]
fn CodeBlock(lang: String, code: String) -> Element {
    let mut copied = use_signal(|| false);
    let label = match lang.trim() {
        "" => "text".to_owned(),
        name => name.to_owned(),
    };
    let tokens = ui_highlight::tokenize(&lang, &code);
    rsx! {
        div { class: "md-code-block",
            div { class: "md-code-head",
                span { class: "md-code-lang", "{label}" }
                button {
                    class: "md-code-copy",
                    "aria-label": "Copy code",
                    onclick: {
                        let code = code.clone();
                        move |_| {
                            let _ = dioxus::document::eval(&copy_script(&code));
                            copied.set(true);
                            spawn(async move {
                                let mut done = dioxus::document::eval(CONFIRM_RESET_JS);
                                let _ = done.recv::<bool>().await;
                                copied.set(false);
                            });
                        }
                    },
                    if copied() { "Copied" } else { "Copy" }
                }
            }
            pre {
                code {
                    for (index, token) in tokens.into_iter().enumerate() {
                        span { key: "{index}", class: ui_highlight::css_class(token.kind), "{token.text}" }
                    }
                }
            }
        }
    }
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
                        CodeBlock { key: "{idx}", lang, code }
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
                    MdBlock::OrderedList { start, items } => rsx! {
                        ol { key: "{idx}", class: "md-list md-ol", start: "{start}",
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
    let error = session.read().connect_error.clone();
    let connecting = session.read().connecting;
    let can_retry = session.read().has_saved_session();
    let theme = session.read().theme;
    let version = env!("CARGO_PKG_VERSION");
    let phase = session.read().sign_in.phase;
    let busy = session.read().sign_in.busy() || connecting;
    let sign_in_error = session.read().sign_in.error.clone();
    let sign_in_notice = session.read().sign_in.notice.clone();
    let invite_code = session.read().sign_in.invite_code.clone();
    let show_invite = session.read().sign_in.show_invite;
    let server_empty = server.trim().is_empty();
    // Same check the session runs on connect, shown as the user types.
    let server_invalid =
        !server_empty && serabot_desktop::validate_endpoint(server.trim()).is_err();
    rsx! {
        div { class: "connect",
            div { class: "connect-card",
                div { class: "brand brand-lg", "S" }
                h1 { "SeraBot" }
                p { class: "muted", "Sign in to continue" }
                div { class: "field",
                    label { class: "field-label", r#for: "server-input", "Server" }
                    input {
                        class: if server_invalid { "input invalid" } else { "input" },
                        id: "server-input",
                        value: "{server}",
                        placeholder: "https://bot.seradyx.tech",
                        disabled: busy,
                        "aria-invalid": if server_invalid { "true" } else { "false" },
                        oninput: move |event| session.write().server = event.value(),
                    }
                    if server_invalid {
                        p { class: "field-error", role: "alert",
                            "Use an https:// address. Plain http works only for localhost."
                        }
                    }
                }
                if connecting && phase == SignInPhase::Idle {
                    div { class: "row",
                        span { class: "spinner", title: "Checking your saved session" }
                        p { class: "tiny", "Checking your saved session…" }
                    }
                }
                if phase == SignInPhase::Waiting {
                    div { class: "banner info",
                        span { class: "spinner", title: "Waiting for the browser" }
                        span { "Finish signing in in your browser, then come back here." }
                    }
                }
                if let Some(message) = sign_in_notice {
                    div { class: "banner info", span { "{message}" } }
                }
                if let Some(message) = sign_in_error {
                    div { class: "banner err", span { "{message}" } }
                }
                if let Some(message) = error {
                    div { class: "banner err", span { "{message}" } }
                }
                if show_invite {
                    div { class: "field",
                        label { class: "field-label", "Invite code" }
                        input {
                            class: "input",
                            value: "{invite_code}",
                            placeholder: "only needed the first time",
                            disabled: busy,
                            oninput: move |event| session.write().sign_in.invite_code = event.value(),
                        }
                    }
                }
                div { class: "row",
                    button {
                        class: "btn primary",
                        disabled: busy || server_empty,
                        title: "Open the Microsoft sign-in page in your browser",
                        onclick: move |_| session.write().start_sign_in(),
                        match phase {
                            SignInPhase::Starting => "Opening browser…",
                            SignInPhase::Waiting => "Waiting for browser…",
                            SignInPhase::Idle => "Sign in with Microsoft",
                        }
                    }
                    if phase != SignInPhase::Idle {
                        button {
                            class: "btn ghost",
                            onclick: move |_| session.write().cancel_sign_in(),
                            "Cancel"
                        }
                    }
                    if can_retry && !connecting && phase == SignInPhase::Idle {
                        button {
                            class: "btn ghost",
                            onclick: move |_| session.write().connect_now(),
                            "Try again"
                        }
                    }
                }
                if server_empty && !busy {
                    p { class: "tiny", "Enter the server address to sign in." }
                }
                if !show_invite && !busy {
                    button {
                        class: "btn ghost sm",
                        onclick: move |_| session.write().sign_in.show_invite = true,
                        "I have an invite code"
                    }
                }
                div { class: "row",
                    button {
                        class: "btn ghost sm",
                        onclick: move |_| {
                            let next = session.read().theme.toggled();
                            session.write().set_theme(next);
                        },
                        if theme == Theme::Dark { "Light theme" } else { "Dark theme" }
                    }
                }
                p { class: "tiny",
                    "Your session is kept in Windows Credential Manager. Only the server address and theme are written to settings.json."
                }
            }
            p { class: "tiny", "v{version}" }
        }
    }
}

#[component]
fn Shell(session: Signal<Session>) -> Element {
    let section = session.read().section;
    let mut session = session;
    let overlay = use_signal(|| Overlay::None);
    let chat = section == Section::Chat;
    let mut drawer_open = use_signal(|| false);
    let mut toast = use_signal(|| Option::<(String, bool)>::None);
    let onboarding_open = chat && session.read().onboarding_ui.open;
    let settings_open = session.read().bot_settings.open;
    // One stylesheet defines every avatar image; tiles only reference it.
    let avatar_css = serabot_desktop::avatars::stylesheet(&session.read().state.avatars);

    rsx! {
        style { "{avatar_css}" }
        div { class: "app-shell",
            IconRail { session, toast }
            if chat {
                Sidebar { session, overlay, drawer_open, toast }
                div {
                    class: if drawer_open() { "scrim drawer-scrim open" } else { "scrim drawer-scrim" },
                    onclick: move |_| drawer_open.set(false),
                }
            }
            main { class: "main",
                if chat && !onboarding_open {
                    Topbar { session, overlay, drawer_open }
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
                        Section::Vault => rsx! { VaultPage { session } },
                        Section::Usage => rsx! { UsagePage { session } },
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
                Overlay::None => rsx! {},
            }}
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
fn IconRail(session: Signal<Session>, toast: Signal<Option<(String, bool)>>) -> Element {
    let mut session = session;
    let section = session.read().section;
    let pending = session.read().state.approvals.len();
    let destinations: [(Section, &str, &str); 8] = [
        (Section::Chat, "chat", "Chats"),
        (Section::Browser, "globe", "Browser"),
        (Section::Approvals, "approvals", "Approvals"),
        (Section::Memory, "memory", "Memory"),
        (Section::Connectors, "connectors", "Connectors"),
        (Section::Vault, "lock", "Vault"),
        (Section::Usage, "usage", "Usage"),
        (Section::Providers, "providers", "AI keys"),
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
fn NewMenu(
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
fn Sidebar(
    session: Signal<Session>,
    overlay: Signal<Overlay>,
    drawer_open: Signal<bool>,
    toast: Signal<Option<(String, bool)>>,
) -> Element {
    let mut overlay = overlay;
    let mut toast = toast;
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
                        RailItem::Bot { id, label, role, avatar, active, enabled, live, unread, pinned } => {
                            let pin_id = id.strip_prefix("dir:").unwrap_or(&id).to_owned();
                            let removable = serabot_desktop::can_remove_bot(&pin_id, &role);
                            rsx! {
                                div { key: "{id}", class: if active { "thread active" } else { "thread" },
                                    button {
                                        class: "thread-main thread-bot",
                                        onclick: {
                                            let id = id.clone();
                                            move |_| {
                                                let bot = id.strip_prefix("dir:").unwrap_or(&id).to_owned();
                                                session.write().open_bot_chat(bot);
                                            }
                                        },
                                        span { class: "avatar bot", style: conversation_avatar_style(&id), "{avatar}" }
                                        div { class: "thread-copy",
                                            span { class: "thread-title", "{label}" }
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
                                    div { class: "thread-actions",
                                        if !enabled && role != "orchestrator" {
                                            button {
                                                class: "btn icon",
                                                title: "Finish setting up",
                                                "aria-label": "Finish setting up {label}",
                                                onclick: {
                                                    let bot = pin_id.clone();
                                                    move |_| session.write().open_onboarding(bot.clone())
                                                },
                                                Icon { name: "sparkles", size: 15 }
                                            }
                                        }
                                        if removable {
                                            button {
                                                class: "btn icon",
                                                title: if enabled { "Switch off" } else { "Switch on" },
                                                "aria-label": if enabled { "Switch off {label}" } else { "Switch on {label}" },
                                                onclick: {
                                                    let bot = pin_id.clone();
                                                    move |_| session.write().set_bot_enabled(bot.clone(), !enabled)
                                                },
                                                Icon { name: "bolt", size: 15 }
                                            }
                                            button {
                                                class: "btn icon",
                                                title: "Delete bot",
                                                "aria-label": "Delete {label}",
                                                onclick: {
                                                    let bot = pin_id.clone();
                                                    move |_| session.write().ask_delete_bot(bot.clone())
                                                },
                                                Icon { name: "trash", size: 15 }
                                            }
                                        }
                                        button {
                                            class: "btn icon",
                                            title: "Bot settings",
                                            "aria-label": "Settings for {label}",
                                            onclick: {
                                                let bot = pin_id.clone();
                                                move |_| session.write().open_bot_settings(bot.clone())
                                            },
                                            Icon { name: "settings", size: 15 }
                                        }
                                        PinToggle { session, id: pin_id.clone(), pinned }
                                    }
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
                                            span { class: "avatar bot", style: conversation_avatar_style(&id), "{initial_of(&label)}" }
                                            div { class: "thread-copy",
                                                span { class: "thread-title", "{label}" }
                                                span { class: "thread-meta", "{subtitle}" }
                                            }
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
        }
    }
}

/// Server step icon names to the console's icon set.
fn step_icon(name: &str) -> &'static str {
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
fn ActivityRow(mut session: Signal<Session>, step: Step) -> Element {
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
            span { class: "step-icon", Icon { name: step_icon(&step.icon), size: 14 } }
            span { class: "step-label", "{step.label}" }
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
fn PlanPanel(plan: serabot_desktop::chat_plan::ChatPlan) -> Element {
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
fn initial_of(label: &str) -> String {
    label
        .chars()
        .find(|c| c.is_alphanumeric())
        .map(|c| c.to_uppercase().collect())
        .unwrap_or_else(|| "C".to_owned())
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
fn Topbar(
    session: Signal<Session>,
    overlay: Signal<Overlay>,
    drawer_open: Signal<bool>,
) -> Element {
    let mut overlay = overlay;
    let mut drawer_open = drawer_open;
    let title = session.read().conversation_title();
    let active_bot = session
        .read()
        .conversation
        .strip_prefix("dir:")
        .map(str::to_owned);
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
    rsx! {
        header { class: "topbar chat-header",
            button {
                class: "icon-btn drawer-toggle",
                title: "Show chats",
                "aria-label": "Show chats",
                onclick: move |_| drawer_open.set(true),
                Icon { name: "list", size: 16 }
            }
            span {
                class: "avatar bot",
                style: active_bot.as_deref().map(avatar_style).unwrap_or_else(|| avatar_style(&title)),
                "{avatar}"
            }
            div { class: "chat-header-copy",
                strong { "{title}" }
                span { "{subtitle}" }
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
                        span { key: "{id}", class: "avatar sm bot", style: avatar_style(&id), "{avatar}" }
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
fn focus_composer() {
    let _ = dioxus::document::eval("document.getElementById('composer-input')?.focus();");
}

#[component]
pub(crate) fn ChatView(session: Signal<Session>, overlay: Signal<Overlay>) -> Element {
    let conversation_kind = session.read().conversation_kind().map(str::to_owned);
    let group_like = matches!(conversation_kind.as_deref(), Some("group") | Some("room"));
    let orchestrator = session
        .read()
        .state
        .listed_bots
        .iter()
        .find(|bot| bot.role == "orchestrator")
        .map(|bot| bot.id.clone());
    let today = today_utc_date();
    let idle = session
        .read()
        .state
        .task
        .as_ref()
        .is_none_or(|task| task.status.terminal());
    let awaiting_approval = session
        .read()
        .state
        .task
        .as_ref()
        .is_some_and(|task| task.status == TaskStatus::AwaitingApproval);
    // One prepared row per message: what the transcript draws, the day
    // heading where a new day starts, and which actions it offers.
    let messages: Vec<MsgView> = {
        let state = session.read();
        let last_user = state
            .state
            .messages
            .iter()
            .rposition(|m| m.role == Role::User);
        let last_bot = state
            .state
            .messages
            .iter()
            .rposition(|m| m.role != Role::User);
        let regen_text = last_user.map(|index| state.state.messages[index].content.clone());
        let mut rows: Vec<MsgView> = Vec::with_capacity(state.state.messages.len());
        let mut prev_user: Option<bool> = None;
        let mut prev_day: Option<String> = None;
        for (index, message) in state.state.messages.iter().enumerate() {
            let user = message.role == Role::User;
            let (speaker, avatar, bot) = state.message_speaker(message);
            let peer = !user
                && group_like
                && bot.as_deref().is_some()
                && bot.as_deref() != orchestrator.as_deref();
            let day_now = chat_feed::day_key(&message.created_at, &today);
            let day = match &day_now {
                Some(now) if prev_day.as_deref() != Some(now.as_str()) => {
                    Some(chat_feed::day_heading(now, &today))
                }
                _ => None,
            };
            if day_now.is_some() {
                prev_day = day_now;
            }
            let speaker_change = prev_user != Some(user);
            prev_user = Some(user);
            let mut class = String::from(if user {
                "msg user"
            } else if peer {
                "msg bot peer"
            } else {
                "msg bot"
            });
            if speaker_change {
                class.push_str(" speaker-change");
            }
            rows.push(MsgView {
                id: message.id.clone(),
                user,
                speaker,
                avatar,
                bot,
                stamp: msg_stamp(&message.created_at),
                content: message.content.clone(),
                peer,
                class,
                day,
                edit: idle && Some(index) == last_user,
                regen: if idle && Some(index) == last_bot && !user {
                    regen_text.clone()
                } else {
                    None
                },
            });
        }
        rows
    };
    let task = session
        .read()
        .state
        .task
        .as_ref()
        .map(|task| (task.status, task.status.label().to_owned()));
    let live_task: Option<TaskStatus> = session
        .read()
        .state
        .task
        .as_ref()
        .and_then(|task| (!task.status.terminal()).then_some(task.status));
    let (steps, steps_summary, sending) = {
        let state = session.read();
        let sending =
            state.state.messages.last().is_some_and(|message| {
                message.role == Role::User && message.id.starts_with("opt-")
            });
        (
            state.state.activity.steps.clone(),
            state.state.activity.summary(),
            sending,
        )
    };
    let chat_plan = session.read().state.plan.clone();
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
    // "Main is working", plus the step that is running right now.
    let running_step = steps
        .iter()
        .find(|step| step.phase == StepPhase::Running)
        .map(|step| step.label.clone());
    let working = chat_feed::working_line(&active_name, running_step.as_deref());
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
    // The team row is collapsed by default: routing state changes rarely, so it
    // sits behind one caption chip instead of a permanent row of pills.
    let mut team_collapsed = use_signal(|| true);
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
    // Message list and composer state (0.19).
    let mut at_bottom = use_signal(|| true);
    let mut unseen = use_signal(|| 0usize);
    let mut last_count = use_signal(|| 0usize);
    let mut dragging = use_signal(|| false);
    let mut composer_focused = use_signal(|| false);
    let mut copied = use_signal(|| Option::<String>::None);
    // Files pasted into the composer arrive through the JS bridge.
    use_future(move || async move {
        let mut bridge = dioxus::document::eval(PASTE_BRIDGE_JS);
        while let Ok(pasted) = bridge.recv::<PastedFile>().await {
            session.write().queue_files(vec![decode_pasted(pasted)]);
        }
    });
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
        let count = session.read().state.messages.len();
        let own_send =
            session.read().state.messages.last().is_some_and(|message| {
                message.role == Role::User && message.id.starts_with("opt-")
            });
        let previous = *last_count.peek();
        // A fresh mount (first render or a switch) starts at the newest line.
        let jump = own_send || previous == 0;
        if count != previous {
            if count > previous && !*at_bottom.peek() && !own_send {
                let added = *unseen.peek() + (count - previous);
                unseen.set(added);
            }
            last_count.set(count);
        }
        // Your own message always brings you to the newest line; a reply
        // only follows when you were already near the bottom.
        if own_send {
            at_bottom.set(true);
            unseen.set(0);
        }
        let _ = dioxus::document::eval(&format!(
            "const el=document.getElementById('messages'); if(el && ({jump} || el.scrollHeight - el.scrollTop - el.clientHeight < 80)){{el.scrollTop=el.scrollHeight;}}"
        ));
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
    let composer_chars = composer.chars().count();
    let length_note = chat_compose::length_message(composer_chars);
    let over_limit = composer_chars > chat_compose::MAX_TEXT_CHARS;
    let composer_file_count = session.read().composer_files.len();
    let busy = session.read().uploading || session.read().upload_pending() > 0;
    let send_ready = chat_compose::can_submit(&composer, composer_file_count, busy);
    rsx! {
        div { class: "pane-body chat-body",
            ondragover: move |event: DragEvent| {
                event.prevent_default();
                dragging.set(true);
            },
            ondragleave: move |_| dragging.set(false),
            ondrop: move |event: DragEvent| async move {
                event.prevent_default();
                dragging.set(false);
                // Admit by name and size first: a refused file is never read.
                let dropped = event.data_transfer().files();
                let meta: Vec<(String, u64)> = dropped.iter().map(|file| (file.name(), file.size())).collect();
                let sizes: Vec<(&str, u64)> = meta.iter().map(|(name, size)| (name.as_str(), *size)).collect();
                let held = session.read().attachments_held();
                let admission = chat_compose::admit_files(held, &sizes);
                let mut picked = Vec::new();
                for (index, file) in dropped.iter().enumerate() {
                    if admission.accepted.contains(&index) {
                        let bytes = file.read_bytes().await.map(|data| data.to_vec()).unwrap_or_default();
                        picked.push((file.name(), bytes));
                    }
                }
                session.write().queue_files(picked);
                session.write().refuse_files(admission.refused);
            },
            if dragging() {
                div { class: "drop-zone", "aria-hidden": "true",
                    div { class: "drop-zone-card",
                        span { class: "drop-zone-title", "Drop files to attach them" }
                        span { class: "drop-zone-meta", "Up to 6 files, 25 MB in total" }
                    }
                }
            }
            div { class: "chat-column",
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
                if let Some(plan) = chat_plan.clone() {
                    PlanPanel { plan }
                }
                div {
                    class: if group_like { "messages group" } else { "messages" },
                    id: "messages",
                    onscroll: move |event: ScrollEvent| {
                        let distance = event.scroll_height() as f64
                            - event.scroll_top()
                            - event.client_height() as f64;
                        let bottom = distance < 80.0;
                        at_bottom.set(bottom);
                        if bottom {
                            unseen.set(0);
                        }
                    },
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
                            if accepts {
                                div { class: "suggestions", role: "group", "aria-label": "Starter prompts",
                                    for text in chat_feed::suggestions(active_bot.as_deref()).iter().copied() {
                                        button {
                                            class: "suggestion",
                                            onclick: move |_| {
                                                session.write().composer = text.to_owned();
                                                focus_composer();
                                            },
                                            "{text}"
                                        }
                                    }
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
                    for row in messages.into_iter() {
                        Fragment { key: "{row.id}",
                            if let Some(heading) = row.day.clone() {
                                div { class: "day-sep", role: "separator",
                                    span { "{heading}" }
                                }
                            }
                            div { class: "{row.class}",
                                span {
                                    class: if row.user { "msg-avatar avatar user" } else { "msg-avatar avatar bot" },
                                    style: row.bot.as_deref().map(avatar_style).unwrap_or_default(),
                                    "{row.avatar}"
                                }
                                div { class: "msg-main",
                                    div { class: "msg-head",
                                        // The own-message label restates what the
                                        // right-aligned bubble already says.
                                        if !row.user {
                                            span { class: "msg-name", "{row.speaker}" }
                                        }
                                        span { class: "msg-time", "{row.stamp}" }
                                        if let Some(id) = row.bot.as_ref() {
                                            if let Some((role, live)) = bot_meta.get(id) {
                                                span { class: "team-chip", "{role}" }
                                                span {
                                                    class: if *live { "chip ok" } else { "chip warn" },
                                                    if *live { "live" } else { "not live" }
                                                }
                                            }
                                        }
                                        if row.peer {
                                            span { class: "team-chip", "peer bot" }
                                        }
                                    }
                                    div { class: "msg-body", title: "{row.stamp}",
                                        MarkdownBody { content: row.content.clone() }
                                        if row.user {
                                            MessageFiles { session, id: row.id.clone() }
                                        }
                                    }
                                    div { class: "msg-actions", role: "toolbar", "aria-label": "Message actions",
                                        button {
                                            class: "msg-action",
                                            "aria-label": "Copy message",
                                            title: "Copy",
                                            onclick: {
                                                let text = row.content.clone();
                                                let id = row.id.clone();
                                                move |_| {
                                                    let _ = dioxus::document::eval(&copy_script(&text));
                                                    copied.set(Some(id.clone()));
                                                    let id = id.clone();
                                                    spawn(async move {
                                                        let mut done = dioxus::document::eval(CONFIRM_RESET_JS);
                                                        let _ = done.recv::<bool>().await;
                                                        if copied.peek().as_deref() == Some(id.as_str()) {
                                                            copied.set(None);
                                                        }
                                                    });
                                                }
                                            },
                                            Icon { name: "copy", size: 13 }
                                            span { class: "msg-action-label",
                                                if copied().as_deref() == Some(row.id.as_str()) { "Copied" } else { "Copy" }
                                            }
                                        }
                                        if row.edit {
                                            button {
                                                class: "msg-action",
                                                "aria-label": "Edit message",
                                                title: "Put this message back in the composer",
                                                onclick: {
                                                    let text = row.content.clone();
                                                    move |_| session.write().edit_text(text.clone())
                                                },
                                                Icon { name: "edit", size: 13 }
                                                span { class: "msg-action-label", "Edit" }
                                            }
                                        }
                                        if let Some(text) = row.regen.clone() {
                                            button {
                                                class: "msg-action",
                                                "aria-label": "Regenerate reply",
                                                title: "Send your last message again",
                                                onclick: move |_| session.write().regenerate(text.clone()),
                                                Icon { name: "refresh", size: 13 }
                                                span { class: "msg-action-label", "Regenerate" }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                    // Files the bots produced in this chat (Open / Save as).
                    MediaCards { session }
                    // What the bot is doing right now, in plain words: a
                    // shimmer while it thinks, one row per step, and a small
                    // "N steps" summary once the reply has landed.
                    if sending && live_task.is_none() {
                        div { class: "activity", role: "status",
                            span { class: "shimmer", "Sending…" }
                        }
                    }
                    if live_task.is_some() {
                        div { class: "activity", role: "status", "aria-live": "polite",
                            span { class: "working",
                                span { class: "working-text", "{working}" }
                                span { class: "working-dots", "aria-hidden": "true",
                                    span {}
                                    span {}
                                    span {}
                                }
                            }
                            for step in steps.iter().cloned() {
                                ActivityRow { session, step, key: "{step.call_id}" }
                            }
                        }
                    } else if !steps.is_empty() {
                        details { class: "activity done",
                            summary { class: "activity-summary",
                                Icon { name: "chevron-right", size: 12 }
                                "{steps_summary}"
                            }
                            for step in steps.iter().cloned() {
                                ActivityRow { session, step, key: "{step.call_id}" }
                            }
                        }
                    }
                }
                if !at_bottom() {
                    button {
                        class: "scroll-bottom",
                        "aria-label": "Scroll to the newest message",
                        title: "Newest messages",
                        onclick: move |_| {
                            let _ = dioxus::document::eval("const el=document.getElementById('messages'); if(el){el.scrollTo({top: el.scrollHeight, behavior: 'smooth'});}");
                            unseen.set(0);
                        },
                        Icon { name: "arrow-down", size: 16 }
                        if let Some(badge) = chat_feed::unseen_badge(unseen()) {
                            span { class: "scroll-badge", "aria-label": "{badge} unseen messages", "{badge}" }
                        }
                    }
                }
                div { class: "composer-wrap",
                    if let Some(message) = transcript_error {
                        div { class: "banner err", span { "{message}" } }
                    }
                    div { class: "composer",
                        ComposerFiles { session }
                        textarea {
                            class: "composer-input",
                            id: "composer-input",
                            rows: 1,
                            "aria-label": "Message",
                            value: "{composer}",
                            placeholder: if accepts { format!("Message {active_name}…") } else { "This node does not accept turns".into() },
                            disabled: !accepts,
                            onfocus: move |_| composer_focused.set(true),
                            onblur: move |_| composer_focused.set(false),
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
                                // Up in an empty composer recalls the last message.
                                if event.key() == Key::ArrowUp && session.read().composer.is_empty() {
                                    let texts: Vec<String> = session
                                        .read()
                                        .state
                                        .messages
                                        .iter()
                                        .filter(|message| message.role == Role::User)
                                        .map(|message| message.content.clone())
                                        .collect();
                                    let refs: Vec<&str> = texts.iter().map(String::as_str).collect();
                                    if let Some(text) = chat_compose::recall_text(&refs) {
                                        event.prevent_default();
                                        session.write().composer = text;
                                    }
                                }
                                if event.key() == Key::Enter {
                                    let modifiers = event.modifiers();
                                    let command = modifiers.contains(Modifiers::CONTROL)
                                        || modifiers.contains(Modifiers::META);
                                    let enter_sends = session.read().prefs.enter_sends;
                                    if chat_compose::enter_sends(
                                        enter_sends,
                                        modifiers.contains(Modifiers::SHIFT),
                                        command,
                                    ) {
                                        event.prevent_default();
                                        session.write().submit();
                                    }
                                }
                            },
                        }
                        if composer_focused() && composer.is_empty() {
                            div { class: "composer-hint", "{chat_compose::COMPOSER_HINT}" }
                        }
                        if let Some(note) = length_note.clone() {
                            div {
                                class: if over_limit { "composer-count over" } else { "composer-count" },
                                role: "status",
                                "{note}"
                            }
                        }
                        div { class: "composer-row",
                            NewMenu { session, overlay }
                            // The model chip only applies to one-to-one bot
                            // chats; rooms and the owner chat stay
                            // auto-routed (the Providers page keeps the full
                            // ModelBar).
                            if is_one_to_one {
                                ModelBar { session, compact: true }
                            }
                            div { class: "team-row",
                                if group_like {
                                    // Collapsed: one caption chip with a count.
                                    // The member pills only render expanded,
                                    // so the resting composer has no pill row.
                                    button {
                                        class: "chip muted team-chip",
                                        title: if team_collapsed() { "Show members" } else { "Hide members" },
                                        onclick: move |_| team_collapsed.set(!team_collapsed()),
                                        "Talking to {talking_to.len()}"
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
                                    "aria-label": if recording() { "Stop recording" } else { "Record a voice note" },
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
                                button {
                                    class: "composer-mic",
                                    title: "Attach files (images, PDF, Office, text; up to 25 MB)",
                                    "aria-label": "Attach files",
                                    disabled: !accepts,
                                    onclick: move |_| session.write().pick_attachments(),
                                    Icon { name: "paperclip", size: 16 }
                                }
                                if awaiting_approval {
                                    span { class: "composer-waiting", role: "status",
                                        "Waiting for your approval. Decline it to stop."
                                    }
                                } else if live_task.is_some() {
                                    button {
                                        class: "btn stop-btn",
                                        title: "Stop the reply",
                                        "aria-label": "Stop the reply",
                                        onclick: move |_| session.write().stop_turn(),
                                        Icon { name: "stop", size: 14 }
                                    }
                                } else {
                                    button {
                                        class: "btn primary send-btn",
                                        title: "Send",
                                        "aria-label": "Send message",
                                        disabled: !can_send || !send_ready,
                                        onclick: move |_| session.write().submit(),
                                        Icon { name: "send", size: 16 }
                                    }
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

/// Shown under the picker when the server lists no concrete model.
const NO_MODELS_HINT: &str = "Ask the admin to allow more models.";

#[component]
fn ModelBar(session: Signal<Session>, #[props(default = false)] compact: bool) -> Element {
    // Picker items: (value, label). "auto" first, then the catalog entries
    // (or the static provider model lists when the catalog is empty).
    let (items, current, allowed, saved, rows, kinds) = {
        let session = session.read();
        let catalog = session.state.catalog.as_ref();
        let mut items: Vec<(String, String)> =
            vec![("auto".to_owned(), "Auto · approved route".to_owned())];
        let mut current = "auto".to_owned();
        let mut allowed = false;
        let mut saved = "auto".to_owned();
        let mut rows: Vec<provider_view::PickerRow> = Vec::new();
        let kinds: BTreeMap<String, String> = session
            .state
            .providers
            .iter()
            .map(|provider| (provider.id.clone(), provider.kind.clone()))
            .collect();
        if let Some(catalog) = catalog {
            if !catalog.selection.is_auto() {
                current = format!("{}|{}", catalog.selection.provider, catalog.selection.model);
            }
            // The server's own list, with friendly names, sizes and tags;
            // the plain provider model lists only when it sent no entries.
            if catalog.entries.is_empty() {
                let plain: Vec<serabot_desktop::CatalogEntry> = catalog
                    .providers
                    .iter()
                    .flat_map(|provider| {
                        provider
                            .models
                            .iter()
                            .map(|model| serabot_desktop::CatalogEntry {
                                provider: provider.id.clone(),
                                model: model.clone(),
                                data_egress_approved: provider.data_egress_approved,
                                name: None,
                                context_length: None,
                                tags: Vec::new(),
                            })
                    })
                    .collect();
                rows = provider_view::picker_rows(&plain, human_model);
            } else {
                rows = provider_view::picker_rows(&catalog.entries, human_model);
            }
            for row in &rows {
                items.push((
                    row.value.clone(),
                    format!("{} \u{b7} {}", row.title, human_provider(&row.provider)),
                ));
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
        (items, current, allowed, saved, rows, kinds)
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
    // Only "auto" is left: the server offered no concrete model.
    let no_models = session.read().state.catalog.is_some() && items.len() <= 1;
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
    // Each visible row with its provider header (drawn when the provider
    // changes) and the metadata the provider's own list gave us.
    let render_rows: Vec<PickerItem> = {
        let mut previous: Option<String> = None;
        filtered
            .iter()
            .map(|(index, value, label)| {
                let row = rows.iter().find(|row| &row.value == value).cloned();
                let header = row.as_ref().and_then(|row| {
                    if previous.as_deref() == Some(row.provider.as_str()) {
                        return None;
                    }
                    previous = Some(row.provider.clone());
                    let kind = kinds
                        .get(&row.provider)
                        .cloned()
                        .unwrap_or_else(|| row.provider.clone());
                    let brand = if branding::brand_label(&row.provider).is_some() {
                        row.provider.clone()
                    } else {
                        kind.clone()
                    };
                    let name = provider_view::display_name(
                        &row.provider,
                        &kind,
                        branding::brand_label(&kind),
                    );
                    Some((brand, name))
                });
                (*index, value.clone(), label.clone(), header, row)
            })
            .collect()
    };
    rsx! {
        div { class: if compact { "model-bar compact" } else { "model-bar" },
            if !compact {
                span { class: "micro", "Model" }
            }
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
                        if no_models {
                            div { class: "pop-empty",
                                "No models are available for your providers."
                            }
                            div { class: "pop-empty tiny", "{NO_MODELS_HINT}" }
                        } else if filtered.is_empty() {
                            div { class: "pop-empty", "No models match." }
                        }
                        for (index, item_value, label, header, row) in render_rows {
                            div { key: "{item_value}",
                            if let Some((brand, name)) = header {
                                div { class: "pop-group model-group",
                                    BrandLogo { brand, size: 16 }
                                    span { "{name}" }
                                }
                            }
                            button {
                                class: if index == active { "pop-item active" } else { "pop-item" },
                                onclick: {
                                    let item_value = item_value.clone();
                                    move |_| {
                                        let mut session = session.write();
                                        set_model_draft(&mut session, &item_value);
                                        open.set(false);
                                        query.set(String::new());
                                    }
                                },
                                if let Some(row) = row {
                                    div { class: "model-row",
                                        span { class: "model-title", "{row.title}" }
                                        div { class: "model-meta",
                                            if row.title != row.model {
                                                span { class: "model-id", "{row.model}" }
                                            }
                                            if let Some(context) = row.context.clone() {
                                                span { class: "chip muted", "{context} context" }
                                            }
                                            for tag in row.tags.clone() {
                                                span { class: "chip", "{tag}" }
                                            }
                                        }
                                    }
                                } else {
                                    span { "{label}" }
                                }
                                if item_value == current {
                                    Icon { name: "check", size: 14 }
                                }
                            }
                            }
                        }
                    }
                }
            }
            if changed && !compact {
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
            } else if changed {
                button {
                    class: "btn primary sm",
                    disabled: !allowed,
                    onclick: move |_| {
                        let draft = session.read().state.draft.clone();
                        session.write().apply_model(draft);
                    },
                    "Apply"
                }
            } else if !compact {
                span { class: "tiny", "Saved {saved}" }
            }
        }
    }
}

/// Contextual right pane: bot profile, conversation details, provider and
/// connector summaries.
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
                            p { class: "empty-text", "Requests that need your OK appear here." }
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
                    "aria-label": "Close",
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
                            draft.provider.auth = preset.auth.to_owned();
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
                                strong { "Sign-in only" }
                                p { class: "tiny", "Save the provider first. Then press Sign in on its card on the Providers page: your browser opens, you approve, and the tokens go straight to the server's encrypted store. There is no API key to paste." }
                            }
                        } else if preset.sign_in == "pkce" {
                            div { class: "banner info", style: "margin: 8px 0; display: flex; flex-direction: column; gap: 6px;",
                                strong { "Two ways to connect" }
                                p { class: "tiny", "Paste an API key below, or save the provider and use Sign in with {preset.label} on its card. Sign-in creates a key for you and saves it to your account." }
                            }
                        } else if preset.auth == "none" {
                            div { class: "banner info", style: "margin: 8px 0;",
                                p { class: "tiny", "This is a local server. It needs no key and no sign-in. Change the address below if it runs on another machine." }
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
                                strong { "This provider signs in, it has no API key." }
                                p { class: "tiny", "After you save, use the Sign in button on this provider's card on the Providers page. Your browser opens, you approve, and the tokens land in the server's encrypted store ({secret_name})." }
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
                        if chosen.is_none_or(|preset| preset.auth != "none") {
                            div { class: "field",
                                label { class: "field-label", "API key" }
                                input {
                                    class: "input",
                                    "type": "password",
                                    value: "{api_key}",
                                    placeholder: "API key (leave empty if already stored)",
                                    oninput: move |event| session.write().provider.api_key = event.value(),
                                }
                                if let Some(url) = chosen.map(|preset| preset.key_url).filter(|url| !url.is_empty()) {
                                    button {
                                        class: "btn ghost sm",
                                        style: "align-self: flex-start;",
                                        title: "Open the provider's site to create a key",
                                        onclick: move |_| session.write().open_in_browser(url.to_owned()),
                                        Icon { name: "external", size: 14 }
                                        "Get a key"
                                    }
                                }
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
    let provider_cards: Vec<serabot_desktop::ProviderStatus> = providers.clone();
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
                if loaded && provider_cards.is_empty() {
                    div { class: "empty",
                        div { class: "empty-icon", Icon { name: "providers", size: 20 } }
                        h2 { class: "empty-title", "No AI providers yet" }
                        p { class: "empty-text", "Add a provider and an API key so your bots can answer." }
                        button {
                            class: "btn primary",
                            onclick: move |_| overlay.set(Overlay::AddProvider),
                            Icon { name: "plus", size: 16 }
                            "Add provider"
                        }
                    }
                }
                div { class: "card-grid provider-grid",
                    for provider in provider_cards {
                        {
                            let provider_id = provider.id.clone();
                            let signed_in = provider_signed_in(&provider.kind, &provider.secrets);
                            let conn = provider_view::connection(&provider, signed_in);
                            let is_oauth = provider.auth == "oauth";
                            let preset = provider_presets::presets()
                                .iter()
                                .find(|preset| preset.id == provider.id)
                                .or_else(|| {
                                    provider_presets::presets().iter().find(|preset| {
                                        preset.kind == provider.kind && preset.kind != "openai-compatible"
                                    })
                                })
                                .copied();
                            let brand = if branding::brand_label(&provider.id).is_some() {
                                provider.id.clone()
                            } else {
                                provider.kind.clone()
                            };
                            let title = provider_view::display_name(
                                &provider.id,
                                &provider.kind,
                                branding::brand_label(&provider.kind),
                            );
                            let key_url = preset.map(|p| p.key_url).filter(|url| !url.is_empty());
                            let takes_key = !is_oauth && preset.is_none_or(|p| p.auth != "none");
                            let pkce_sign_in = provider.kind == "openrouter";
                            let signing_in = session.read().oauth_signing_in.as_deref() == Some(provider.id.as_str());
                            let testing = session.read().provider_testing.as_deref() == Some(provider.id.as_str());
                            let draft_key = session
                                .read()
                                .provider_key_drafts
                                .get(&provider.id)
                                .cloned()
                                .unwrap_or_default();
                            let has_key_text = !draft_key.trim().is_empty();
                            let model_line = provider_view::model_line(&provider);
                            let shown: Vec<String> = provider.models.iter().take(6).cloned().collect();
                            let extra = provider.models.len().saturating_sub(shown.len());
                            let star_model = provider
                                .default_model
                                .clone()
                                .or_else(|| provider.models.first().cloned());
                            let is_default = provider.is_default;
                            let managed = provider.managed;
                            let routing = provider.routing_classes.join(", ");
                            let egress = provider.data_egress_approved;
                            let sub = if provider.id == provider.kind || provider.id == "default" {
                                provider.kind.clone()
                            } else {
                                format!("{} ({})", provider.id, provider.kind)
                            };
                            rsx! {
                                div { class: if is_default { "card provider-card is-default" } else { "card provider-card" }, key: "{provider_id}",
                                    div { class: "card-head",
                                        div { style: "display: flex; align-items: center; gap: 12px; min-width: 0;",
                                            BrandLogo { brand: brand.clone(), size: 36 }
                                            div { style: "min-width: 0;",
                                                h3 { class: "card-title", "{title}" }
                                                span { class: "card-sub", "{sub}" }
                                            }
                                        }
                                        if is_default {
                                            span { class: "chip accent default-badge",
                                                Icon { name: "star", size: 12 }
                                                "Default"
                                            }
                                        }
                                    }
                                    div { class: "chips",
                                        span { class: "{conn.class()}", "{conn.label()}" }
                                        if managed {
                                            span { class: "chip accent", "managed" }
                                        }
                                        if !egress {
                                            span { class: "chip warn", "sending blocked" }
                                        }
                                    }
                                    p { class: "tiny", "{model_line}" }
                                    div { class: "chips",
                                        for model in shown {
                                            span { class: "chip", "{model}" }
                                        }
                                        if extra > 0 {
                                            span { class: "chip muted", "+{extra} more" }
                                        }
                                    }
                                    if !routing.is_empty() {
                                        p { class: "tiny", "Used for: {routing}" }
                                    }
                                    if takes_key {
                                        div { class: "provider-key-row",
                                            input {
                                                class: "input",
                                                "type": "password",
                                                value: "{draft_key}",
                                                placeholder: if provider.credential.own { "Paste a new key to replace yours" } else { "Paste your API key" },
                                                oninput: {
                                                    let id = provider.id.clone();
                                                    move |event| {
                                                        session.write().provider_key_drafts.insert(id.clone(), event.value());
                                                    }
                                                },
                                            }
                                            button {
                                                class: "btn primary sm",
                                                disabled: !can_save || !has_key_text,
                                                title: "Saved to your account only, encrypted. It is never shown again.",
                                                onclick: {
                                                    let id = provider.id.clone();
                                                    move |_| session.write().save_provider_key(id.clone())
                                                },
                                                if provider.credential.own { "Replace key" } else { "Save key" }
                                            }
                                        }
                                    }
                                    div { class: "card-actions",
                                        if pkce_sign_in {
                                            button {
                                                class: "btn ghost sm",
                                                title: "Sign in on openrouter.ai. Your key is created for you and saved to your account.",
                                                disabled: !can_save || signing_in,
                                                onclick: {
                                                    let id = provider.id.clone();
                                                    let kind = provider.kind.clone();
                                                    move |_| session.write().sign_in_provider(id.clone(), kind.clone())
                                                },
                                                BrandLogo { brand: "openrouter".to_owned(), size: 16 }
                                                if signing_in { "Waiting for browser..." } else { "Sign in with OpenRouter" }
                                            }
                                        }
                                        if is_oauth {
                                            button {
                                                class: if conn.is_connected() { "btn ghost sm" } else { "btn primary sm" },
                                                title: "Sign in through your browser. Tokens are stored on the server and never shown here.",
                                                disabled: !can_save || signing_in,
                                                onclick: {
                                                    let id = provider.id.clone();
                                                    let kind = provider.kind.clone();
                                                    move |_| session.write().sign_in_provider(id.clone(), kind.clone())
                                                },
                                                BrandLogo { brand: brand.clone(), size: 16 }
                                                if signing_in {
                                                    "Waiting for browser..."
                                                } else if conn.is_connected() {
                                                    "Sign in again"
                                                } else {
                                                    "Sign in with {title}"
                                                }
                                            }
                                        }
                                        if let Some(url) = key_url {
                                            button {
                                                class: "btn ghost sm",
                                                title: "Open the provider's site to create a key",
                                                onclick: move |_| session.write().open_in_browser(url.to_owned()),
                                                Icon { name: "external", size: 14 }
                                                "Get a key"
                                            }
                                        }
                                        button {
                                            class: "btn ghost sm",
                                            title: "Ask the provider for its model list with your key",
                                            disabled: !can_save || testing || !conn.is_connected(),
                                            onclick: {
                                                let id = provider.id.clone();
                                                move |_| session.write().test_provider(id.clone())
                                            },
                                            Icon { name: "refresh", size: 14 }
                                            if testing { "Testing..." } else { "Test" }
                                        }
                                        if !is_default && conn.is_connected() && star_model.is_some() {
                                            button {
                                                class: "btn ghost sm",
                                                title: "Use this provider for new chats",
                                                disabled: !can_save,
                                                onclick: {
                                                    let id = provider.id.clone();
                                                    let model = star_model.clone().unwrap_or_default();
                                                    move |_| session.write().set_default_provider(id.clone(), model.clone())
                                                },
                                                Icon { name: "star", size: 14 }
                                                "Set as default"
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
                                        || (p.kind == preset.kind && preset.kind != "openai-compatible"))
                                        && (p.credential.configured || p.auth == "oauth" && p.secrets.values().any(|s| s.configured))
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
                                                        draft.provider.auth = preset.auth.to_owned();
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
                        }
        }
    }
}

/// Filter chips on the Memory page: `(filter key, label)`.
const MEMORY_FILTERS: &[(&str, &str)] = &[
    ("all", "All"),
    ("saved", "Saved"),
    ("learned", "Learned"),
    ("superseded", "Superseded"),
    ("review", "To review"),
];

/// One memory card, prepared before rendering so the markup stays plain.
#[derive(Clone, PartialEq)]
struct MemoryRow {
    id: String,
    content: String,
    kind: String,
    origin: &'static str,
    superseded: bool,
    pending: bool,
    delete_label: &'static str,
    bot: Option<String>,
    importance: String,
    created: String,
    recalled: String,
}

fn memory_rows(memories: &[MemoryEntry]) -> Vec<MemoryRow> {
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
            created: memory.created_at.clone(),
            recalled: match &memory.last_recalled_at {
                Some(at) => format!("last recalled {at} ({}×)", memory.recall_count),
                None => "not recalled yet".into(),
            },
        })
        .collect()
}

/// The header line: counts by origin and status, and the last extraction.
fn memory_header(stats: Option<&MemoryStatsView>) -> String {
    match stats {
        None => "Owner-scoped memory. Loading counts…".into(),
        Some(stats) => {
            let count = |map: &std::collections::BTreeMap<String, i64>, key: &str| {
                map.get(key).copied().unwrap_or(0)
            };
            let last = stats
                .last_extraction_at
                .as_deref()
                .map_or("never".to_owned(), |at| at.to_owned());
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
fn MemoryView(session: Signal<Session>) -> Element {
    let memories = memory_rows(&session.read().state.memories);
    let memories_loaded = session.read().state.memories_loaded;
    let header = memory_header(session.read().state.memory_stats.as_ref());
    let kind = session.read().memory.kind.clone();
    let content = session.read().memory.content.clone();
    let source = session.read().memory.source.clone();
    let query = session.read().memory.query.clone();
    let filter = session.read().memory.filter.clone();
    let confirm = session.read().memory.delete_confirm.clone();
    let editing = session.read().memory.editing.clone();
    let edit_content = session.read().memory.edit_content.clone();
    let memories_empty = memories.is_empty();
    rsx! {
        div { class: "pane",
            div { class: "pane-head",
                div {
                    h1 { class: "pane-title", "Memory" }
                    p { class: "muted", "{header}" }
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
                        placeholder: "Search all of your memory, including old facts",
                        oninput: move |event| session.write().memory.query = event.value(),
                    }
                    button {
                        class: "btn ghost",
                        onclick: move |_| session.write().load_memories(),
                        "Search"
                    }
                }
                div { class: "field-row",
                    for (key, label) in MEMORY_FILTERS.iter().copied() {
                        button {
                            key: "{key}",
                            class: if filter == key { "chip accent" } else { "chip" },
                            onclick: move |_| session.write().set_memory_filter(key),
                            "{label}"
                        }
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
                                span { class: if memory.origin == "Learned" { "chip" } else { "chip accent" }, "{memory.origin}" }
                                if let Some(bot) = memory.bot.clone() {
                                    span { class: "chip", "{bot}" }
                                }
                                span { class: "chip", "{memory.kind}" }
                                if memory.superseded {
                                    span { class: "chip", "Superseded" }
                                }
                                if memory.pending {
                                    span { class: "chip accent", "To review" }
                                }
                                span { class: "tiny", "{memory.importance}" }
                            }
                            if editing.as_deref() == Some(memory.id.as_str()) {
                                textarea {
                                    class: "textarea",
                                    value: "{edit_content}",
                                    oninput: move |event| session.write().memory.edit_content = event.value(),
                                }
                                div { class: "field-row",
                                    button {
                                        class: "btn primary sm",
                                        onclick: move |_| session.write().save_memory_edit(),
                                        "Save"
                                    }
                                    button {
                                        class: "btn ghost sm",
                                        onclick: move |_| session.write().cancel_memory_edit(),
                                        "Cancel"
                                    }
                                }
                            } else {
                                p { "{memory.content}" }
                                p { class: "tiny", "{memory.created} · {memory.recalled}" }
                                if memory.pending {
                                    button {
                                        class: "btn primary sm",
                                        onclick: {
                                            let id = memory.id.clone();
                                            move |_| session.write().approve_memory(id.clone())
                                        },
                                        "Approve"
                                    }
                                }
                                button {
                                    class: "btn ghost sm",
                                    onclick: {
                                        let id = memory.id.clone();
                                        let text = memory.content.clone();
                                        move |_| session.write().begin_memory_edit(id.clone(), &text)
                                    },
                                    "Edit"
                                }
                            }
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
                                    "{memory.delete_label}"
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

#[component]
fn SettingsView(session: Signal<Session>) -> Element {
    let signed_in_user = session.read().state.user.clone();
    let signing_out = session.read().sign_out.in_flight;
    let server = session.read().server.clone();
    let version = session.read().state.version.clone();
    let update = session.read().update.info.clone();
    let update_error = session.read().update.error.clone();
    let update_notice = session.read().update.notice.clone();
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
    // Voice/OCR surface. Read-only status comes from the server; the editable
    // fields are the draft the Save button writes.
    let voice_loaded = session.read().voice.loaded;
    let voice_loading = session.read().voice.loading;
    let voice_stt = session.read().voice.stt_model.clone();
    let voice_stt_ready = session.read().voice.stt_ready;
    let voice_stt_detail = session.read().voice.stt_detail.clone();
    let voice_tts = session.read().voice.tts_model.clone();
    let voice_tts_ready = session.read().voice.tts_ready;
    let voice_tts_detail = session.read().voice.tts_detail.clone();
    let voice_id = session.read().voice.voice_id.clone();
    let voice_ocr = session.read().voice.ocr_model.clone();
    let voice_ocr_ready = session.read().voice.ocr_ready;
    let voice_ocr_detail = session.read().voice.ocr_detail.clone();
    let voice_ocr_enabled = session.read().voice.ocr_enabled;
    let voice_reply_mode = session.read().voice.reply_mode.clone();
    let voice_max_chars = session.read().voice.max_speech_chars;
    // The live sandbox posture. Every value the server does not report stays
    // "unknown"; nothing is guessed.
    let status = session.read().status.status.clone();
    let status_error = session.read().status.error.clone();
    let status_loading = session.read().status.loading;
    let restart_pending = session.read().restart_pending;
    let telegram_status = session.read().telegram.status.clone();
    let telegram_loaded = session.read().telegram.loaded;
    let telegram_loading = session.read().telegram.loading;
    let telegram_error = session.read().telegram.error.clone();
    let telegram_link = session.read().telegram.link.clone();
    let telegram_linking = session.read().telegram.linking;
    let telegram_unlinking = session.read().telegram.unlinking;
    let telegram_confirm = session.read().telegram.confirm_unlink;
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
                let label = format!("{} · {}", humanize_label(&tool), bots.join(", "));
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
                            "allowed".to_string()
                        } else {
                            "not allowed".to_string()
                        },
                        provider.data_egress_approved,
                    )
                })
                .collect::<Vec<_>>(),
        )
    };
    let gated_empty = gated_tools.is_empty();
    let egress_empty = egress.is_empty();
    let sel = session.read().settings_section;
    let settings_new_cards = rsx! {
        if sel == SettingsSection::Appearance {
            AppearanceCard { session }
        }
        if sel == SettingsSection::Notifications {
            NotificationsCard { session }
        }
        if sel == SettingsSection::Chat {
            ChatPrefsCard { session }
        }
        if sel == SettingsSection::LocalFolders {
            LocalFoldersCard { session }
        }
        if sel == SettingsSection::About {
            AboutCard { session }
        }
    };
    rsx! {
        div { class: "pane",
            div { class: "pane-head",
                div {
                    h1 { class: "pane-title", "Settings" }
                    p { class: "muted", "Your account, how SeraBot looks and behaves, and what is connected." }
                }
            }
            div { class: "pane-body settings-layout",
                SettingsNav { session }
                div { class: "settings-content",
                    {settings_new_cards}
                    if sel == SettingsSection::Account {
                    div { class: "card",
                        div { class: "card-head",
                            h3 { class: "card-title", "Your account" }
                        }
                        p { class: "card-sub", "{server}" }
                        if !version.is_empty() {
                            p { class: "tiny", "Server {version}" }
                        }
                        if let Some(user) = &signed_in_user {
                            div { class: "kv-row",
                                span { class: "kv-key", "Signed in as" }
                                span { class: "kv-val", "{user.label()}" }
                            }
                            if !user.email.is_empty() && user.email != user.label() {
                                div { class: "kv-row",
                                    span { class: "kv-key", "Email" }
                                    span { class: "kv-val", "{user.email}" }
                                }
                            }
                            if !user.role.is_empty() {
                                div { class: "kv-row",
                                    span { class: "kv-key", "Role" }
                                    span { class: "kv-val", "{user.role}" }
                                }
                            }
                        } else {
                            p { class: "tiny", "Signed in with a saved session." }
                        }
                        div { class: "card-actions",
                            button {
                                class: "btn danger",
                                disabled: signing_out,
                                title: "End this session on the server and remove it from this device",
                                onclick: move |_| session.write().sign_out(),
                                if signing_out { "Signing out…" } else { "Sign out" }
                            }
                            if signing_out {
                                span { class: "spinner", title: "Signing out" }
                            }
                        }
                    }
                    }
                    if sel == SettingsSection::Telegram {
                    div { class: "card",
                        div { class: "card-head",
                            h3 { class: "card-title", "Telegram" }
                            div { class: "row",
                                if telegram_loading {
                                    span { class: "spinner", title: "Reading Telegram status" }
                                }
                                button {
                                    class: "btn ghost sm",
                                    disabled: telegram_loading || telegram_linking || telegram_unlinking,
                                    onclick: move |_| session.write().refresh_telegram(),
                                    Icon { name: "refresh", size: 14 }
                                    "Refresh status"
                                }
                            }
                        }
                        p { class: "card-sub",
                            "Link Telegram to chat with your bots from your phone. They use the same memory and ask you first, just like here."
                        }
                        if let Some(status) = telegram_status {
                            div { class: "chips",
                                span {
                                    class: if status.enabled { "chip ok" } else { "chip muted" },
                                    if status.enabled { "Telegram on" } else { "Telegram off" }
                                }
                                span {
                                    class: if status.linked { "chip ok" } else { "chip muted" },
                                    if status.linked { "linked" } else { "not linked" }
                                }
                                if let Some(chat) = status.chat_label() {
                                    span { class: "chip mono", "{chat}" }
                                }
                                if let Some(since) = status.linked_at.as_deref() {
                                    span { class: "chip", "since {stamp(since)}" }
                                }
                            }
                            if !status.enabled {
                                p { class: "tiny",
                                    "Telegram is turned off on the server. Ask your admin to turn it on."
                                }
                            }
                            if status.linked {
                                if telegram_confirm {
                                    div { class: "row",
                                        span { class: "tiny",
                                            "Unlink this chat? Messages from it will be ignored until you link again."
                                        }
                                        button {
                                            class: "btn danger sm",
                                            disabled: telegram_unlinking || telegram_loading,
                                            onclick: move |_| session.write().confirm_unlink_telegram(),
                                            if telegram_unlinking { "Unlinking…" } else { "Unlink" }
                                        }
                                        button {
                                            class: "btn ghost sm",
                                            disabled: telegram_unlinking,
                                            onclick: move |_| session.write().cancel_unlink_telegram(),
                                            "Cancel"
                                        }
                                    }
                                } else {
                                    div { class: "card-actions",
                                        button {
                                            class: "btn danger sm",
                                            onclick: move |_| session.write().request_unlink_telegram(),
                                            "Unlink Telegram"
                                        }
                                    }
                                }
                            } else if status.enabled {
                                div { class: "card-actions",
                                    button {
                                        class: "btn primary",
                                        disabled: telegram_linking || telegram_loading,
                                        onclick: move |_| session.write().link_telegram(),
                                        if telegram_linking { "Requesting code…" } else { "Link Telegram" }
                                    }
                                }
                            }
                        } else if telegram_loading {
                            div { class: "skeleton block" }
                        } else if telegram_loaded && telegram_error.is_none() {
                            p { class: "tiny",
                                "Telegram status is unavailable. Press Refresh status to retry."
                            }
                        } else if !telegram_loaded && telegram_error.is_none() {
                            p { class: "tiny", "Reading Telegram status…" }
                        }
                        if let Some(link) = telegram_link {
                            div { class: "field",
                                label { class: "field-label", "Send this command to the SeraBot bot" }
                                code { class: "link-code", "{link.command()}" }
                                p { class: "tiny",
                                    "The code works once and expires 10 minutes after it was requested. After sending it, press Refresh status."
                                }
                                if let Some(expires) = link.expires_at.as_deref() {
                                    p { class: "tiny", "Expires {expires}." }
                                }
                            }
                        }
                        if let Some(message) = telegram_error {
                            div { class: "banner err", span { "{message}" } }
                        }
                    }
                    }
                    if sel == SettingsSection::Admin && can_admin {
                    div { class: "card",
                        div { class: "card-head",
                            h3 { class: "card-title", "Safety checks" }
                            div { class: "row",
                                if status_loading {
                                    span { class: "spinner", title: "Reading the server posture" }
                                }
                                button {
                                    class: "btn ghost sm",
                                    onclick: move |_| session.write().refresh_status(),
                                    Icon { name: "refresh", size: 14 }
                                    "Refresh"
                                }
                            }
                        }
                        p { class: "card-sub", "What the server checks before your bots act." }
                        div { class: "chips",
                            span { class: "chip", "Safety rules: {policy_version}" }
                            span { class: "chip", "Tasks waiting: {pending_tasks}" }
                            span { class: "chip", "Waiting for your OK: {pending_approvals}" }
                            span {
                                class: if restart_pending { "chip warn" } else { "chip ok" },
                                if restart_pending { "Restart needed" } else { "Up to date" }
                            }
                        }
                        if let Some(error) = status_error {
                            div { class: "banner err", span { "{error}" } }
                        }
                        div { class: "field",
                            label { class: "field-label", "Bots ask you first before using" }
                            if bots_loaded {
                                div { class: "chips",
                                    for (_tool, label) in gated_tools {
                                        span { class: "chip warn", "{label}" }
                                    }
                                    if gated_empty {
                                        span { class: "chip muted", "Nothing needs your OK" }
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
                            label { class: "field-label", "Sending data to AI providers" }
                            if providers_loaded {
                                div { class: "chips",
                                    for (provider_id, egress_state, approved) in egress {
                                        span { class: if approved { "chip ok" } else { "chip warn" },
                                            "{provider_id}: {egress_state}"
                                        }
                                    }
                                    if egress_empty {
                                        span { class: "chip muted", "No AI provider yet" }
                                    }
                                }
                            } else {
                                span { class: "hint", "unknown" }
                            }
                        }
                    }
                    }
                    if sel == SettingsSection::Admin && can_admin {
                    div { class: "card",
                        div { class: "card-head",
                            h3 { class: "card-title", "What's turned on" }
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
                                match catalog_fetch { Some(true) => "Model lists update themselves", Some(false) => "Model lists are fixed", None => "Model lists unknown" }
                            }
                        }
                        if audio_enabled == Some(true) {
                            p { class: "tiny",
                                "Speech-to-text: {transcription}. Audio understanding: {understanding}."
                            }
                            p { class: "tiny",
                                "Voice notes you send on Telegram are turned into text before the bot answers."
                            }
                        } else if audio_enabled == Some(false) {
                            p { class: "tiny", "Audio is turned off on the server." }
                        } else {
                            p { class: "tiny",
                                "This server is too old to report what is turned on. Update it to see live status."
                            }
                        }
                        if catalog_fetch == Some(false) {
                            p { class: "tiny",
                                "Ask your admin to turn on live model lists, then use Fetch models on the AI keys page."
                            }
                        }
                        div { class: "form-grid",
                            div { class: "field",
                                label { class: "field-label", "Speech-to-text model" }
                                input {
                                    class: "input",
                                    value: "{audio_transcription}",
                                    placeholder: "speech-to-text model",
                                    oninput: move |event| session.write().audio.transcription_model = event.value(),
                                }
                            }
                            div { class: "field",
                                label { class: "field-label", "Audio understanding model" }
                                input {
                                    class: "input",
                                    value: "{audio_understanding}",
                                    placeholder: "audio understanding model",
                                    oninput: move |event| session.write().audio.understanding_model = event.value(),
                                }
                            }
                        }
                        p { class: "tiny",
                            "Audio uses the Mistral key you already saved under AI keys."
                        }
                        label { class: "check",
                            input {
                                "type": "checkbox",
                                checked: audio_on,
                                onchange: move |event| session.write().audio.enabled = event.checked(),
                            }
                            span { "Turn on audio transcription and understanding" }
                        }
                        button {
                            class: "btn primary",
                            disabled: !can_admin,
                            onclick: move |_| session.write().save_audio(),
                            "Save audio settings"
                        }
                    }
                    }
                    if sel == SettingsSection::Voice {
                    div { class: "card",
                        div { class: "card-head",
                            h3 { class: "card-title", "Voice and scanned pages" }
                            if voice_loading {
                                span { class: "chip muted", "Loading" }
                            }
                        }
                        div { class: "card-actions",
                            span {
                                class: if voice_stt_ready { "chip ok" } else { "chip warn" },
                                if voice_stt_ready { "Speech-to-text ready" } else { "Speech-to-text off" }
                            }
                            span {
                                class: if voice_tts_ready { "chip ok" } else { "chip warn" },
                                if voice_tts_ready { "Voice replies ready" } else { "Voice replies off" }
                            }
                            span {
                                class: if voice_ocr_ready { "chip ok" } else { "chip warn" },
                                if voice_ocr_ready { "Scanned pages ready" } else { "Scanned pages off" }
                            }
                        }
                        if !voice_loaded {
                            p { class: "tiny",
                                "Load the settings to see whether speech-to-text, voice replies and scanned-page reading can run on this server."
                            }
                            button {
                                class: "btn ghost",
                                disabled: voice_loading,
                                onclick: move |_| session.write().load_voice(),
                                "Load voice settings"
                            }
                        } else {
                            p { class: "tiny",
                                "Scanned PDFs and images are read with OCR when they have no text. Voice replies send a Telegram voice note instead of text."
                            }
                            if let Some(detail) = voice_stt_detail.clone() {
                                p { class: "tiny", "Speech to text: {detail}" }
                            }
                            if let Some(detail) = voice_tts_detail.clone() {
                                p { class: "tiny", "Voice replies: {detail}" }
                            }
                            if let Some(detail) = voice_ocr_detail.clone() {
                                p { class: "tiny", "Scanned pages: {detail}" }
                            }
                            div { class: "form-grid",
                                div { class: "field",
                                    label { class: "field-label", "Speech-to-text model" }
                                    input {
                                        class: "input",
                                        value: "{voice_stt}",
                                        disabled: true,
                                        placeholder: "speech-to-text model",
                                    }
                                }
                                div { class: "field",
                                    label { class: "field-label", "Voice reply model" }
                                    input {
                                        class: "input",
                                        value: "{voice_tts}",
                                        placeholder: "voxtral-mini-tts-2603",
                                        oninput: move |event| session.write().voice.tts_model = event.value(),
                                    }
                                }
                                div { class: "field",
                                    label { class: "field-label", "Voice" }
                                    input {
                                        class: "input",
                                        value: "{voice_id}",
                                        placeholder: "voice name from your provider",
                                        oninput: move |event| session.write().voice.voice_id = event.value(),
                                    }
                                }
                                div { class: "field",
                                    label { class: "field-label", "Scanned-page model" }
                                    input {
                                        class: "input",
                                        value: "{voice_ocr}",
                                        placeholder: "mistral-ocr-latest",
                                        oninput: move |event| session.write().voice.ocr_model = event.value(),
                                    }
                                }
                            }
                            label { class: "check",
                                input {
                                    "type": "checkbox",
                                    checked: voice_ocr_enabled,
                                    onchange: move |event| session.write().voice.ocr_enabled = event.checked(),
                                }
                                span { "Read scanned PDFs and images when they have no text" }
                            }
                            div { class: "card-actions",
                                for mode in ["text", "voice"] {
                                    button {
                                        class: if voice_reply_mode == mode { "btn primary" } else { "btn ghost" },
                                        onclick: {
                                            let mode = mode.to_string();
                                            move |_| session.write().voice.reply_mode = mode.clone()
                                        },
                                        if mode == "voice" { "Reply with voice" } else { "Reply with text" }
                                    }
                                }
                            }
                            p { class: "tiny",
                                "Voice replies stop at {voice_max_chars} characters. In Telegram, /voice and /text switch the style for that chat."
                            }
                            button {
                                class: "btn primary",
                                disabled: !can_admin,
                                onclick: move |_| session.write().save_voice(),
                                "Save voice settings"
                            }
                        }
                    }
                    }
                    if sel == SettingsSection::About {
                    div { class: "card",
                        div { class: "card-head",
                            h3 { class: "card-title", "Updates" }
                        }
                        button {
                            class: "btn ghost",
                            disabled: checking,
                            onclick: move |_| session.write().check_updates_manual(),
                            if checking { "Checking…" } else { "Check for updates" }
                        }
                        if let Some(message) = update_notice {
                            p { class: "tiny", "{message}" }
                        }
                        if let Some(info) = update {
                            p { class: "accent", "v{info.version} available" }
                            button {
                                class: "btn primary",
                                onclick: {
                                    let info = info.clone();
                                    move |_| {
                                        let result = session.write().apply_update(&info);
                                        match result {
                                            Ok(()) => std::process::exit(0),
                                            Err(error) => session.write().update.error = Some(error),
                                        }
                                    }
                                },
                                "Install and restart"
                            }
                        }
                        if let Some(error) = update_error {
                            p { class: "error-text", "{error}" }
                        }
                    }
                    }
                    if sel == SettingsSection::Chat {
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
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn civil_from_days_matches_known_dates() {
        // 1970-01-01 is day 0; 2026-11-01 is day 20758.
        assert_eq!(civil_from_days(0), "1970-01-01");
        assert_eq!(civil_from_days(20_758), "2026-11-01");
        assert_eq!(civil_from_days(20_757), "2026-10-31");
        // Leap-year boundary.
        assert_eq!(civil_from_days(18_321), "2020-02-29");
        assert_eq!(civil_from_days(18_322), "2020-03-01");
    }

    #[test]
    fn msg_stamp_shows_time_today_and_short_date_otherwise() {
        let today = today_utc_date();
        let stamped_today = format!("{today}T10:33:45Z");
        assert_eq!(msg_stamp(&stamped_today), "10:33");

        let other = msg_stamp("2026-10-30T14:05:00Z");
        assert_eq!(other, "Oct 30 14:05");

        // Garbage in, the long form out — never a panic.
        assert_eq!(msg_stamp(""), "");
        assert_eq!(msg_stamp("nope"), "nope");
    }

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
    fn ordered_lists_keep_their_start_number_and_loose_items() {
        let blocks = parse_markdown_blocks(
            "3. three
4. four",
        );
        assert_eq!(
            blocks,
            vec![MdBlock::OrderedList {
                start: 3,
                items: vec!["three".into(), "four".into()],
            }]
        );
        // Blank lines between items keep one list rather than N lists of "1.".
        let loose = parse_markdown_blocks(
            "1. a

2. b

3. c",
        );
        assert_eq!(
            loose,
            vec![MdBlock::OrderedList {
                start: 1,
                items: vec!["a".into(), "b".into(), "c".into()],
            }]
        );
        // A list resumed after another block starts at its own number.
        let resumed = parse_markdown_blocks(
            "1. a

break

2. b",
        );
        assert!(matches!(
            resumed.last(),
            Some(MdBlock::OrderedList { start: 2, .. })
        ));
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

    #[test]
    fn known_tools_get_plain_sentences() {
        assert_eq!(
            tool_sentence(
                "browser_type",
                "Type vault secret 'venue-portal' (password) into e1 on the current page",
                true
            )
            .as_deref(),
            Some("Signed in with your saved login \"venue-portal\". The bot never saw it.")
        );
        assert_eq!(
            tool_sentence(
                "browser_navigate",
                "Open https://www.portal.example.com/calendar?m=12 in the server browser",
                true
            )
            .as_deref(),
            Some("Opened portal.example.com")
        );
        assert_eq!(
            tool_sentence(
                "browser_navigate",
                "Open https://a.example/ in the server browser",
                false
            )
            .as_deref(),
            Some("Wants to open a.example")
        );
        assert_eq!(
            tool_sentence("browser_click", "", true).as_deref(),
            Some("Clicked on the page")
        );
        assert_eq!(tool_sentence("deck_build", "x", false), None);
    }
}
