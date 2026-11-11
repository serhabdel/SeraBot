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
    branding,
    chat_cards::{self, CardState, CardView},
    chat_compose, chat_feed, chat_scroll, file_icons, provider_presets, provider_view,
    session::{Section, Session, SettingsSection, SignInPhase, Theme, humanize_label},
    teams, ui_highlight,
    working::BotActivity,
};
// The UI pacing constants are only read by the Windows message pump below.
#[cfg(windows)]
use serabot_desktop::ui_tick;
use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::rc::Rc;
use std::time::Instant;

use crate::ui_bots::{MeetTeamSheet, NewBotSheet, NewGroupSheet, OnboardingView};
use crate::ui_browser::BrowserView;
use crate::ui_cards::ChatCardRow;
use crate::ui_connectors::ConnectorsView;
use crate::ui_dock::BrowserDock;
use crate::ui_files::{
    COMPOSER_CARET_BRIDGE_JS, COMPOSER_CARET_SETUP_JS, CONFIRM_RESET_JS, ComposerFiles, MediaCards,
    MessageFiles, PASTE_BRIDGE_JS, PastedFile, copy_script, decode_pasted,
};
use crate::ui_settings::SettingsPanel;
use crate::ui_settings_page::{
    AboutCard, AppearanceCard, ChatPrefsCard, LocalFoldersCard, Mem0RecallCard, NotificationsCard,
    SettingsNav, TurnLimitsCard, WebSearchCard,
};
use crate::ui_usage::UsagePage;
use crate::ui_vault::VaultPage;

mod approvals;
mod chat;
mod connect;
mod conversations;
mod markdown;
mod memory;
mod providers;
mod row_menu;
mod settings_view;
mod sheets;
mod shell;
pub(crate) use approvals::*;
pub(crate) use chat::*;
pub(crate) use connect::*;
pub(crate) use conversations::*;
pub(crate) use markdown::*;
pub(crate) use memory::*;
pub(crate) use providers::*;
pub(crate) use row_menu::*;
pub(crate) use settings_view::*;
pub(crate) use sheets::*;
pub(crate) use shell::*;

const CSS: &str = include_str!("../assets/app.css");
/// Reports whether the system prefers a dark theme, now and on every change.
const SYSTEM_THEME_JS: &str = "const m = window.matchMedia('(prefers-color-scheme: dark)'); dioxus.send(m.matches); m.addEventListener('change', (e) => dioxus.send(e.matches));";
/// Styles of the 0.17 usage and settings pages, kept apart from app.css.
const CSS_PAGES: &str = concat!(
    include_str!("../assets/usage.css"),
    include_str!("../assets/settings.css"),
    include_str!("../assets/files.css"),
    include_str!("../assets/jobs.css"),
    include_str!("../assets/foundation.css")
);

/// The `data-activity` value the stylesheet keys the avatar ring and the
/// "Working..." label on: `working` (animated ring), `waiting` (static dot)
/// or `idle`.
fn activity_attr(activity: Option<&BotActivity>) -> &'static str {
    match activity {
        Some(BotActivity::Working(_)) => "working",
        Some(BotActivity::WaitingApproval) => "waiting",
        None => "idle",
    }
}

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
    /// The newest reply a tool limit ended, while idle: offers Continue (0.24).
    continue_offer: bool,
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
    /// "Meet your team": the shipped bots the owner can add.
    MeetTeam,
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
        "elevenlabs_key_list_voices"
        | "elevenlabs_key_text_to_speech"
        | "elevenlabs_key_sound_effect"
        | "elevenlabs_key_transcribe"
        | "elevenlabs_key_generate_image"
        | "elevenlabs_key_generate_video" => Some("Voice and media"),
        "elevenlabs_mcp_search_tools" | "elevenlabs_mcp_describe_tool" | "elevenlabs_mcp_call" => {
            Some("ElevenLabs tools")
        }
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
    // `mut` is needed only by the Windows message pump below.
    #[cfg_attr(not(windows), allow(unused_mut))]
    let mut session = use_signal(Session::new);
    #[cfg_attr(not(windows), allow(unused_mut))]
    let mut revision = use_signal(|| 0u64);
    use_coroutine(move |_: UnboundedReceiver<()>| async move {
        loop {
            #[cfg(windows)]
            {
                let wake = session.read().ui_wake.clone();
                let mut saw_wake = wake.swap(false, std::sync::atomic::Ordering::AcqRel);
                if !saw_wake {
                    for _ in 0..ui_tick::UI_IDLE_POLLS {
                        tokio::time::sleep(std::time::Duration::from_millis(ui_tick::UI_POLL_MS))
                            .await;
                        if wake.swap(false, std::sync::atomic::Ordering::AcqRel) {
                            saw_wake = true;
                            break;
                        }
                    }
                }
                let before = session.read().ui_fingerprint();
                session.with_mut(|session| session.tick());
                let after = session.read().ui_fingerprint();
                if before != after {
                    revision.set(revision() + 1);
                }
                // SSE wakes skip the idle wait; pace at ~60fps so we do not spin.
                if saw_wake {
                    tokio::time::sleep(std::time::Duration::from_millis(ui_tick::UI_SSE_FRAME_MS))
                        .await;
                }
            }
            #[cfg(not(windows))]
            std::future::pending::<()>().await;
        }
    });
    use_effect(move || {
        let _ = revision();
        let _ = dioxus::document::eval(COMPOSER_CARET_BRIDGE_JS);
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
        "logs" => "M6 4h9l3 3v13H6zM9 9h6M9 13h6M9 17h4",
        "workflows" => "M12 8v4l3 2M12 3a9 9 0 1 0 0 18 9 9 0 0 0 0-18z",
        "globe" => {
            "M12 3a9 9 0 1 0 0 18 9 9 0 0 0 0-18zM3 12h18M12 3c3 3 3 15 0 18M12 3c-3 3-3 15 0 18"
        }
        "key" => "M14.5 9.5a4 4 0 1 1-1.2-2.8L21 14.4V18h-3.6v-2h-2v-2h-2z",
        "alert" => "M12 4l9 16H3zM12 10v4M12 17v.5",
        "alert-circle" => "M12 3a9 9 0 1 0 0 18 9 9 0 0 0 0-18zM12 8v5M12 16v.5",
        "eye" => "M2 12s4-7 10-7 10 7 10 7-4 7-10 7S2 12 2 12zM12 9a3 3 0 1 0 0 6 3 3 0 0 0 0-6z",
        "eye-off" => {
            "M3 3l18 18M10.6 6.1A9.7 9.7 0 0 1 12 5c6 0 10 7 10 7a17 17 0 0 1-3.2 4M6.6 7.7A16.6 16.6 0 0 0 2 12s4 7 10 7a9.7 9.7 0 0 0 4.1-.9M9.9 9.9a3 3 0 0 0 4.2 4.2"
        }
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
