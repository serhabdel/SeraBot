//! Pieces of the Settings page (0.17): the left section list and the cards
//! for the device-local preferences. The older cards (account, Telegram,
//! voice, safety checks, updates) stay in `ui.rs` and are shown per section.
//!
//! Preferences saved here live on this device only (`creds::UiPrefs`).

use crate::ui::Icon;
use dioxus::prelude::*;
use serabot_desktop::local_folders::BotScope;
use serabot_desktop::session::{Session, SettingsGroup, SettingsSection, Theme};
use serabot_desktop::theme::Tone;

use crate::ui_widgets::{SecretField, StatusChip};

/// The list of sections on the left, grouped under Preferences, Connections &
/// Data, and System. Admin only shows for admins.
#[component]
pub fn SettingsNav(session: Signal<Session>) -> Element {
    let mut session = session;
    let (current, is_admin, mem0_available) = {
        let s = session.read();
        let mem0_available = s
            .state
            .memory_settings
            .as_ref()
            .is_some_and(|view| view.mem0.available);
        (s.settings_section, s.can_admin(), mem0_available)
    };
    let mut query = use_signal(String::new);
    let needle = query().trim().to_owned();
    // While searching, one ranked list replaces the groups.
    let mut found: Vec<(i32, SettingsSection)> = SettingsSection::ALL
        .into_iter()
        .filter(|s| s.visible_with_memory(is_admin, mem0_available))
        .filter_map(|s| {
            serabot_desktop::picker_search::best_score(
                &needle,
                &[(s.label(), 4), (s.description(), 2), (s.keywords(), 1)],
            )
            .map(|score| (score, s))
        })
        .collect();
    found.sort_by_key(|a| std::cmp::Reverse(a.0));
    let first_hit = found.first().map(|(_, section)| *section);
    rsx! {
        nav { class: "settings-nav", "aria-label": "Settings sections",
            div { class: "settings-search",
                input {
                    id: "settings-search",
                    class: "input",
                    r#type: "search",
                    placeholder: "Search settings\u{2026}",
                    "aria-label": "Search settings",
                    value: "{query}",
                    oninput: move |e| query.set(e.value()),
                    onkeydown: move |e: KeyboardEvent| {
                        if e.key() == Key::Escape {
                            query.set(String::new());
                        } else if e.key() == Key::Enter
                            && let Some(first) = first_hit
                        {
                            query.set(String::new());
                            session.write().open_settings_section(first);
                        }
                    },
                }
            }
            if !needle.is_empty() {
                div { class: "settings-nav-group", role: "list",
                    if found.is_empty() {
                        div { class: "settings-nav-empty muted", "No setting matches \"{needle}\"." }
                    }
                    for (_, section) in found.iter().copied() {
                        button {
                            key: "{section.label()}",
                            class: if current == section { "settings-nav-item on" } else { "settings-nav-item" },
                            "aria-current": if current == section { "page" } else { "false" },
                            onclick: move |_| {
                                query.set(String::new());
                                session.write().open_settings_section(section);
                            },
                            span { class: "settings-nav-label", "{section.label()}" }
                            span { class: "settings-nav-desc", "{section.description()}" }
                        }
                    }
                }
            } else {
                for group in SettingsGroup::ALL {
                    {
                        let items: Vec<SettingsSection> = SettingsSection::ALL
                            .into_iter()
                            .filter(|s| s.group() == group && s.visible_with_memory(is_admin, mem0_available))
                            .collect();
                        rsx! {
                            if !items.is_empty() {
                                div { key: "{group.label()}", class: "settings-nav-group",
                                    div { class: "settings-nav-group-label", "{group.label()}" }
                                    for section in items {
                                        button {
                                            key: "{section.label()}",
                                            class: if current == section { "settings-nav-item on" } else { "settings-nav-item" },
                                            "aria-current": if current == section { "page" } else { "false" },
                                            onclick: move |_| session.write().open_settings_section(section),
                                            span { class: "settings-nav-label", "{section.label()}" }
                                            span { class: "settings-nav-desc", "{section.description()}" }
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
pub fn AppearanceCard(session: Signal<Session>) -> Element {
    let mut session = session;
    let (theme, prefs) = {
        let s = session.read();
        (s.theme, s.prefs.clone())
    };
    let themes = [
        (Theme::Light, "Light"),
        (Theme::Dark, "Dark"),
        (Theme::System, "Same as my computer"),
    ];
    let sizes = [("small", "Small"), ("normal", "Normal"), ("large", "Large")];
    rsx! {
        div { class: "card",
            div { class: "card-head",
                h3 { class: "card-title", "Appearance" }
            }
            div { class: "field",
                label { class: "field-label", "Theme" }
                div { class: "seg", role: "group", "aria-label": "Theme",
                    for (mode, label) in themes {
                        button {
                            key: "{label}",
                            class: if theme == mode { "seg-btn active" } else { "seg-btn" },
                            "aria-pressed": if theme == mode { "true" } else { "false" },
                            onclick: move |_| session.write().set_theme(mode),
                            "{label}"
                        }
                    }
                }
            }
            div { class: "field",
                label { class: "field-label", "Text size" }
                div { class: "seg", role: "group", "aria-label": "Text size",
                    for (value, label) in sizes {
                        button {
                            key: "{value}",
                            class: if prefs.text_size == value { "seg-btn active" } else { "seg-btn" },
                            "aria-pressed": if prefs.text_size == value { "true" } else { "false" },
                            onclick: {
                                let prefs = prefs.clone();
                                move |_| {
                                    let mut next = prefs.clone();
                                    next.text_size = value.to_owned();
                                    session.write().set_prefs(next);
                                }
                            },
                            "{label}"
                        }
                    }
                }
            }
            label { class: "check",
                input {
                    r#type: "checkbox",
                    checked: prefs.reduce_motion,
                    onchange: {
                        let prefs = prefs.clone();
                        move |event| {
                            let mut next = prefs.clone();
                            next.reduce_motion = event.checked();
                            session.write().set_prefs(next);
                        }
                    },
                }
                span { "Reduce motion" }
            }
            label { class: "check",
                input {
                    r#type: "checkbox",
                    checked: prefs.handwritten_notes,
                    onchange: {
                        let prefs = prefs.clone();
                        move |event| {
                            let mut next = prefs.clone();
                            next.handwritten_notes = event.checked();
                            session.write().set_prefs(next);
                        }
                    },
                }
                span { "Handwritten notes" }
            }
            p { class: "tiny",
                if prefs.handwritten_notes {
                    "Short notes and sign-offs from your bots render in a handwriting font."
                } else {
                    "Short notes and sign-offs from your bots render as plain text."
                }
            }
            p { class: "tiny", "These choices apply on this computer only." }
        }
    }
}

#[component]
pub fn NotificationsCard(session: Signal<Session>) -> Element {
    let mut session = session;
    let prefs = session.read().prefs.clone();
    rsx! {
        div { class: "card",
            div { class: "card-head",
                h3 { class: "card-title", "Notifications" }
            }
            label { class: "check",
                input {
                    r#type: "checkbox",
                    checked: prefs.notify_approvals,
                    onchange: {
                        let prefs = prefs.clone();
                        move |event| {
                            let mut next = prefs.clone();
                            next.notify_approvals = event.checked();
                            session.write().set_prefs(next);
                        }
                    },
                }
                span { "Tell me when a bot is waiting for my OK" }
            }
            p { class: "tiny", "You see a short message in this window. Requests also wait on the Approvals page." }
        }
    }
}

#[component]
pub fn ChatPrefsCard(session: Signal<Session>) -> Element {
    let mut session = session;
    let prefs = session.read().prefs.clone();
    rsx! {
        div { class: "card",
            div { class: "card-head",
                h3 { class: "card-title", "Chat" }
            }
            label { class: "check",
                input {
                    r#type: "checkbox",
                    checked: prefs.enter_sends,
                    onchange: {
                        let prefs = prefs.clone();
                        move |event| {
                            let mut next = prefs.clone();
                            next.enter_sends = event.checked();
                            session.write().set_prefs(next);
                        }
                    },
                }
                span { "Press Enter to send" }
            }
            p { class: "tiny",
                if prefs.enter_sends {
                    "Enter sends your message. Shift+Enter starts a new line."
                } else {
                    "Ctrl+Enter sends your message. Enter starts a new line."
                }
            }
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Mem0CardState {
    Loading,
    Error,
    Unavailable,
    Available,
}

fn classify_mem0_card(
    settings: Option<&serabot_desktop::MemorySettingsView>,
    error: Option<&str>,
) -> Mem0CardState {
    match settings {
        Some(view) if view.mem0.available => Mem0CardState::Available,
        Some(_) => Mem0CardState::Unavailable,
        None if error.is_some() => Mem0CardState::Error,
        None => Mem0CardState::Loading,
    }
}

fn validate_turn_limits_draft(
    calls_input: &str,
    rounds_input: &str,
    view: Option<serabot_desktop::TurnLimitsView>,
) -> Result<(String, String), String> {
    let Some(current) = view else {
        return Err("Wait for the limits to load first.".into());
    };
    let calls_text = if calls_input.trim().is_empty() {
        current.max_tool_calls_per_turn.to_string()
    } else {
        calls_input.trim().to_owned()
    };
    let rounds_text = if rounds_input.trim().is_empty() {
        current.max_tool_rounds.to_string()
    } else {
        rounds_input.trim().to_owned()
    };
    serabot_desktop::media_view::parse_turn_limits(&calls_text, &rounds_text, current.ceiling)?;
    Ok((calls_text, rounds_text))
}

/// The owner's per-turn work limits (0.24): how many tool calls and model
/// rounds one bot turn may use. The server ceiling and the config default are
/// shown; an empty field keeps the current value.
#[component]
pub fn TurnLimitsCard(session: Signal<Session>) -> Element {
    let mut session = session;
    let mut calls = use_signal(String::new);
    let mut rounds = use_signal(String::new);
    let mut local_error = use_signal(|| Option::<String>::None);
    use_effect(move || session.write().load_turn_limits());
    let (view, server_error) = {
        let s = session.read();
        (s.state.turn_limits, s.state.turn_limits_error.clone())
    };
    let summary = match view {
        Some(view) => serabot_desktop::media_view::limits_summary(&view),
        None if server_error.is_some() => "Could not load your work limits.".to_owned(),
        None => "Loading your work limits\u{2026}".to_owned(),
    };
    let ceiling = view.map(|view| view.ceiling);
    let has_draft = !calls.read().trim().is_empty() || !rounds.read().trim().is_empty();
    let error = local_error().or(server_error);
    let invalid_draft =
        has_draft && validate_turn_limits_draft(&calls.read(), &rounds.read(), view).is_err();
    rsx! {
        div { class: "card",
            div { class: "card-head",
                h3 { class: "card-title", "Bot work limits" }
                if view.is_none() {
                    button {
                        class: "btn ghost sm",
                        r#type: "button",
                        onclick: move |_| session.write().load_turn_limits(),
                        "Retry"
                    }
                }
            }
            p { class: "tiny", "{summary}" }
            p { class: "tiny",
                "When a turn reaches a limit, the bot answers with what it has so far and offers Continue."
            }
            label { class: "field",
                span { class: "field-label", "Tool calls per turn" }
                input {
                    class: if invalid_draft && !calls.read().trim().is_empty() { "input invalid" } else { "input" },
                    r#type: "number",
                    min: "1",
                    max: "{ceiling.map(|c| c.max_tool_calls_per_turn).unwrap_or(0)}",
                    placeholder: "{view.map(|v| v.max_tool_calls_per_turn).unwrap_or(0)}",
                    disabled: view.is_none(),
                    value: "{calls}",
                    oninput: move |event| {
                        local_error.set(None);
                        calls.set(event.value());
                    },
                }
            }
            label { class: "field",
                span { class: "field-label", "Tool rounds per turn" }
                input {
                    class: if invalid_draft && !rounds.read().trim().is_empty() { "input invalid" } else { "input" },
                    r#type: "number",
                    min: "1",
                    max: "{ceiling.map(|c| c.max_tool_rounds).unwrap_or(0)}",
                    placeholder: "{view.map(|v| v.max_tool_rounds).unwrap_or(0)}",
                    disabled: view.is_none(),
                    value: "{rounds}",
                    oninput: move |event| {
                        local_error.set(None);
                        rounds.set(event.value());
                    },
                }
            }
            if let Some(error) = error {
                p { class: "field-error tiny", role: "alert", "{error}" }
            }
            div { class: "row-actions",
                button {
                    class: "btn primary sm",
                    r#type: "button",
                    disabled: view.is_none() || !has_draft,
                    onclick: move |_| {
                        let current = session.read().state.turn_limits;
                        let validated = validate_turn_limits_draft(&calls.read(), &rounds.read(), current);
                        match validated {
                            Ok((calls_text, rounds_text)) => {
                                local_error.set(None);
                                session.write().save_turn_limits(&calls_text, &rounds_text);
                                calls.set(String::new());
                                rounds.set(String::new());
                            }
                            Err(msg) => {
                                local_error.set(Some(msg));
                            }
                        }
                    },
                    "Save"
                }
                button {
                    class: "btn sm",
                    r#type: "button",
                    disabled: view.is_none(),
                    onclick: move |_| {
                        local_error.set(None);
                        session.write().reset_turn_limits();
                        calls.set(String::new());
                        rounds.set(String::new());
                    },
                    "Reset to default"
                }
            }
        }
    }
}

/// Settings > Web search (0.28.0): the Tavily and Firecrawl switches with
struct WebSearchRow {
    id: String,
    title: String,
    enabled: bool,
    key_configured: bool,
    dirty: bool,
}

/// their API key status, read from `GET /v1/connectors` and written through
/// the existing admin connector route. The key value itself never appears.
#[component]
pub fn WebSearchCard(session: Signal<Session>) -> Element {
    let mut session = session;
    let can_save = session.read().can_admin();
    let (connectors_loaded, rows) = {
        let s = session.read();
        let rows: Vec<WebSearchRow> = ["tavily", "firecrawl"]
            .iter()
            .filter_map(|id| {
                let connector = s.state.connectors.iter().find(|c| c.id == *id)?;
                let draft_enabled = s
                    .connector_drafts
                    .get(*id)
                    .map(|draft| draft.enabled)
                    .unwrap_or(connector.enabled);
                let dirty = draft_enabled != connector.enabled;
                let key_configured = connector
                    .secrets
                    .get("api_key")
                    .is_some_and(|secret| secret.configured);
                let title = match *id {
                    "tavily" => "Tavily Search",
                    _ => "Firecrawl",
                };
                Some(WebSearchRow {
                    id: (*id).to_owned(),
                    title: title.to_owned(),
                    enabled: draft_enabled,
                    key_configured,
                    dirty,
                })
            })
            .collect();
        (s.state.connectors_loaded, rows)
    };
    rsx! {
        div { class: "card",
            div { class: "card-head",
                h3 { class: "card-title", "Web search & crawl" }
            }
            p { class: "tiny",
                "Let bots search the web and read pages. Each switch needs its API key saved on the Connectors page; the key value is never shown here."
            }
            if !can_save {
                p { class: "tiny", role: "note",
                    "Only an admin can turn web search connectors on or off."
                }
            }
            if rows.is_empty() {
                if connectors_loaded {
                    p { class: "tiny", "No web search connectors are configured on this server." }
                } else {
                    p { class: "tiny", role: "status", "Loading connectors\u{2026}" }
                }
            }
            for row in rows {
                div { key: "{row.id}", class: "kv-row websearch-row",
                    label { class: "check",
                        input {
                            r#type: "checkbox",
                            checked: row.enabled,
                            disabled: !can_save,
                            "aria-label": "Enable {row.title}",
                            onchange: {
                                let id = row.id.clone();
                                move |event| {
                                    session.write().set_connector_enabled(&id, event.checked());
                                }
                            },
                        }
                        span { "{row.title}" }
                    }
                    span {
                        class: if row.key_configured { "chip mono accent" } else { "chip mono warn" },
                        if row.key_configured { "API key saved" } else { "No API key" }
                    }
                    span {
                        class: if row.enabled { "chip ok" } else { "chip muted" },
                        if row.enabled { "on" } else { "off" }
                    }
                    button {
                        class: if row.dirty { "btn primary sm" } else { "btn ghost sm" },
                        r#type: "button",
                        disabled: !can_save || !row.dirty,
                        onclick: {
                            let id = row.id.clone();
                            move |_| session.write().save_connector(id.clone())
                        },
                        if row.dirty { "Save" } else { "Saved" }
                    }
                }
            }
            p { class: "tiny",
                "Turning one on makes its tools available to bots (give them in Bots). Connectors also carry a Test button for a read-only key check."
            }
        }
    }
}

/// Settings > Memory: optional Mem0 recall when the server enables it (Big Wave E2).
#[component]
pub fn Mem0RecallCard(session: Signal<Session>) -> Element {
    let mut session = session;
    let key_draft = use_signal(String::new);
    let mut confirming_remove = use_signal(|| false);
    use_effect(move || session.write().load_memory_settings());
    let (settings, error) = {
        let s = session.read();
        (
            s.state.memory_settings.clone(),
            s.state.memory_settings_error.clone(),
        )
    };
    match classify_mem0_card(settings.as_ref(), error.as_deref()) {
        Mem0CardState::Loading => {
            return rsx! {
                div { class: "card",
                    div { class: "card-head",
                        h3 { class: "card-title", "Mem0 recall" }
                    }
                    p { class: "tiny", role: "status", "Loading memory settings\u{2026}" }
                }
            };
        }
        Mem0CardState::Error => {
            let err_msg = error.unwrap_or_else(|| "Could not load memory settings.".into());
            return rsx! {
                div { class: "card",
                    div { class: "card-head",
                        h3 { class: "card-title", "Mem0 recall" }
                        button {
                            class: "btn ghost sm",
                            r#type: "button",
                            onclick: move |_| session.write().load_memory_settings(),
                            "Retry"
                        }
                    }
                    p { class: "field-error tiny", role: "alert", "{err_msg}" }
                }
            };
        }
        Mem0CardState::Unavailable => {
            return rsx! {
                div { class: "card",
                    div { class: "card-head",
                        h3 { class: "card-title", "Mem0 recall" }
                    }
                    p { class: "tiny", "Mem0 extra recall is not enabled on this server." }
                }
            };
        }
        Mem0CardState::Available => {}
    }
    let Some(settings) = settings else {
        return rsx! {};
    };
    let configured = settings.mem0.configured;
    rsx! {
        div { class: "card",
            div { class: "card-head",
                h3 { class: "card-title", "Mem0 recall" }
            }
            p { class: "tiny",
                "When on, your turns also recall matching facts from Mem0 Platform and merge them with local memory. Your API key stays encrypted on the server and is never shown again after you save it. See docs/memory.md."
            }
            label { class: "check",
                input {
                    r#type: "checkbox",
                    checked: configured,
                    disabled: true,
                    "aria-readonly": "true",
                }
                span { "Use Mem0 for extra recall (save or remove your key below)" }
            }
            div { class: "health-status",
                if configured {
                    StatusChip { tone: Tone::Ok, text: "Key saved".to_owned() }
                } else {
                    StatusChip { tone: Tone::Muted, text: "Off, no key".to_owned() }
                }
            }
            div { class: "field",
                span { class: "field-label", "Mem0 API key" }
                SecretField {
                    value: key_draft,
                    label: "Mem0 API key".to_owned(),
                    placeholder: if configured {
                        "Paste a new key to replace yours".to_owned()
                    } else {
                        "Paste your Mem0 API key".to_owned()
                    },
                }
            }
            div { class: "row-actions",
                button {
                    class: "btn primary sm",
                    r#type: "button",
                    disabled: key_draft.read().trim().is_empty(),
                    onclick: move |_| {
                        let mut key_draft = key_draft;
                        session.write().save_mem0_key(&key_draft.read());
                        key_draft.set(String::new());
                    },
                    if configured { "Replace key" } else { "Save key" }
                }
                if configured && !confirming_remove() {
                    button {
                        class: "btn sm",
                        r#type: "button",
                        onclick: move |_| confirming_remove.set(true),
                        "Remove key"
                    }
                }
            }
            if configured && confirming_remove() {
                div { class: "vault-confirm", role: "alertdialog", "aria-label": "Confirm removing the Mem0 key",
                    span { class: "caption", "Remove your Mem0 key? Extra recall stops until you save a new one." }
                    button {
                        class: "btn danger sm",
                        r#type: "button",
                        onclick: move |_| {
                            confirming_remove.set(false);
                            session.write().delete_mem0_key();
                        },
                        "Remove key"
                    }
                    button {
                        class: "btn ghost sm",
                        r#type: "button",
                        onclick: move |_| confirming_remove.set(false),
                        "Keep key"
                    }
                }
            }
            if let Some(error) = error {
                p { class: "field-error tiny", role: "alert", "{error}" }
            }
        }
    }
}

#[component]
pub fn AboutCard(session: Signal<Session>) -> Element {
    let (server, version) = {
        let s = session.read();
        (s.server.clone(), s.state.version.clone())
    };
    rsx! {
        div { class: "card",
            div { class: "card-head",
                h3 { class: "card-title", "About SeraBot" }
            }
            div { class: "kv-row",
                span { class: "kv-key", "This app" }
                span { class: "kv-val", "{env!(\"CARGO_PKG_VERSION\")}" }
            }
            div { class: "kv-row",
                span { class: "kv-key", "Server" }
                span { class: "kv-val", "{server}" }
            }
            if !version.is_empty() {
                div { class: "kv-row",
                    span { class: "kv-key", "Server version" }
                    span { class: "kv-val", "{version}" }
                }
            }
        }
    }
}

/// Settings > Local folders: the allowlist of folders on this PC the bots may
/// use. Empty by default, which means off. Device-local; the server only ever
/// hears the folder id, a label and the mode.
#[component]
pub fn LocalFoldersCard(session: Signal<Session>) -> Element {
    let mut session = session;
    let mut filter = use_signal(String::new);
    let mut confirm_remove = use_signal(|| Option::<String>::None);
    // Counting files and size is lazy: it starts when this card is shown.
    use_effect(move || {
        session.write().want_folder_stats();
    });
    let (folders, notice, running, stats, bots) = {
        let s = session.read();
        // The owner's enabled bots, by name and title, for the per-folder grant.
        let bots: Vec<(String, String)> = s
            .state
            .listed_bots
            .iter()
            .filter(|bot| bot.enabled)
            .map(|bot| (bot.id.clone(), s.bot_display_name(&bot.id)))
            .collect();
        (
            s.local_folders.clone(),
            s.local_notice.clone(),
            s.local_agent_running(),
            s.folder_stats.clone(),
            bots,
        )
    };
    let needle = filter.read().trim().to_ascii_lowercase();
    let visible_folders: Vec<_> = if needle.is_empty() {
        folders.clone()
    } else {
        folders
            .iter()
            .filter(|f| {
                f.id.to_ascii_lowercase().contains(&needle)
                    || f.path.to_ascii_lowercase().contains(&needle)
            })
            .cloned()
            .collect()
    };
    rsx! {
        div { class: "card",
            div { class: "card-head",
                h3 { class: "card-title", "Local folders" }
                if !folders.is_empty() {
                    span { class: "chip muted", "{folders.len()}" }
                }
            }
            p { class: "tiny",
                "Let your bots browse, search and read big folders on this PC, bring Word, Excel, PowerPoint, PDF and image files in to work on, and with your approval save results back. "
                "It is off until you add a folder. Only this app can see the folder paths; the bots get a short name."
            }
            p { class: "tiny muted",
                strong { "Local PC folders" }
                " are directories on this machine \u{2014} "
                strong { "OneDrive cloud storage" }
                " is managed separately in Integrations and does not require a local PC folder."
            }
            if folders.len() > 2 {
                div { class: "search-wrap",
                    input {
                        class: "input",
                        r#type: "search",
                        placeholder: "Filter allowed folders\u{2026}",
                        "aria-label": "Filter allowed folders",
                        value: "{filter}",
                        oninput: move |event| filter.set(event.value()),
                    }
                    if !needle.is_empty() {
                        button {
                            class: "btn ghost sm",
                            r#type: "button",
                            onclick: move |_| filter.set(String::new()),
                            "Clear"
                        }
                    }
                }
            }
            if folders.is_empty() {
                p { class: "hint", "No folders allowed. The bots cannot see anything on this PC." }
            } else if visible_folders.is_empty() {
                p { class: "hint", "No allowed folders match \u{201c}{filter}\u{201d}." }
            }
            div { class: "roster",
                for folder in visible_folders {
                    div { key: "{folder.id}", class: "roster-row",
                        div { class: "thread-copy",
                            span { class: "thread-title", "{folder.id}" }
                            span { class: "thread-meta", title: "{folder.path}", "{folder.path}" }
                            span { class: "thread-meta",
                                {
                                    stats
                                        .get(&folder.id)
                                        .map(|status| status.label())
                                        .unwrap_or_else(|| "Counting\u{2026}".to_string())
                                }
                                " \u{b7} "
                                {if folder.mode.as_str() == "read_write" { "bots can read and, with approval, write" } else { "bots can only read" }}
                            }
                            BotScopePicker {
                                session,
                                folder_id: folder.id.clone(),
                                scope: folder.bots.clone(),
                                bots: bots.clone(),
                            }
                        }
                        select {
                            class: "select",
                            "aria-label": "Access for {folder.id}",
                            value: "{folder.mode.as_str()}",
                            onchange: {
                                let id = folder.id.clone();
                                move |event| {
                                    let mode = if event.value() == "read_write" {
                                        serabot_desktop::local_folders::FolderMode::ReadWrite
                                    } else {
                                        serabot_desktop::local_folders::FolderMode::Read
                                    };
                                    session.write().set_local_folder_mode(&id, mode);
                                }
                            },
                            option { value: "read", selected: folder.mode.as_str() == "read", "Read only" }
                            option { value: "read_write", selected: folder.mode.as_str() == "read_write", "Read and write" }
                        }
                        if confirm_remove.read().as_deref() == Some(folder.id.as_str()) {
                            div { class: "row-actions", role: "alertdialog", "aria-label": "Remove {folder.id}?",
                                button {
                                    class: "btn danger sm",
                                    r#type: "button",
                                    onclick: {
                                        let id = folder.id.clone();
                                        move |_| {
                                            confirm_remove.set(None);
                                            session.write().remove_local_folder(&id);
                                        }
                                    },
                                    "Remove"
                                }
                                button {
                                    class: "btn ghost sm",
                                    r#type: "button",
                                    onclick: move |_| confirm_remove.set(None),
                                    "Keep"
                                }
                            }
                        } else {
                            button {
                                class: "btn icon",
                                r#type: "button",
                                title: "Remove {folder.id}",
                                "aria-label": "Remove {folder.id}",
                                onclick: {
                                    let id = folder.id.clone();
                                    move |_| confirm_remove.set(Some(id.clone()))
                                },
                                Icon { name: "trash", size: 15 }
                            }
                        }
                    }
                }
            }
            div { class: "row",
                button {
                    class: "btn",
                    r#type: "button",
                    onclick: move |_| session.write().pick_local_folder(),
                    "Add folder\u{2026}"
                }
                if !folders.is_empty() {
                    button {
                        class: "btn",
                        r#type: "button",
                        onclick: move |_| session.write().recount_folders(),
                        "Recount"
                    }
                }
                if running {
                    span { class: "chip ok", "Connected to your bots" }
                }
            }
            if let Some(notice) = notice {
                p {
                    class: if serabot_desktop::local_folders::notice_is_error(&notice) {
                        "settings-error-inline"
                    } else {
                        "settings-notice-ok"
                    },
                    role: if serabot_desktop::local_folders::notice_is_error(&notice) { "alert" } else { "status" },
                    "{notice}"
                }
            }
            p { class: "tiny",
                "Writes always ask for your approval. Links, junctions, and files like .env or private keys are never shared. "
                "If this app is closed, the bots get a clear error instead."
            }
        }
    }
}

/// One folder's bot grant: every bot, or only the bots ticked here. Switching
/// back to "Only these bots" remembers prior picks made in this view.
#[component]
pub fn BotScopePicker(
    session: Signal<Session>,
    folder_id: String,
    scope: BotScope,
    bots: Vec<(String, String)>,
) -> Element {
    let mut session = session;
    let scoped = matches!(scope, BotScope::Only { .. });
    let chosen: Vec<String> = match &scope {
        BotScope::Only { bots } => bots.clone(),
        BotScope::All => Vec::new(),
    };
    let mut remembered_only = use_signal(|| chosen.clone());
    if scoped && !chosen.is_empty() && *remembered_only.read() != chosen {
        remembered_only.set(chosen.clone());
    }
    rsx! {
        div { class: "bot-scope",
            div { class: "seg", role: "group", "aria-label": "Bots that can use {folder_id}",
                button {
                    class: if scoped { "seg-btn" } else { "seg-btn active" },
                    r#type: "button",
                    "aria-pressed": if scoped { "false" } else { "true" },
                    onclick: {
                        let id = folder_id.clone();
                        move |_| session.write().set_local_folder_bots(&id, BotScope::All)
                    },
                    "All bots"
                }
                button {
                    class: if scoped { "seg-btn active" } else { "seg-btn" },
                    r#type: "button",
                    "aria-pressed": if scoped { "true" } else { "false" },
                    onclick: {
                        let id = folder_id.clone();
                        move |_| {
                            let saved = remembered_only.read().clone();
                            session.write().set_local_folder_bots(&id, BotScope::Only { bots: saved });
                        }
                    },
                    if scoped {
                        "Only these bots ({chosen.len()})"
                    } else {
                        "Only these bots"
                    }
                }
            }
            if scoped {
                if bots.is_empty() {
                    p { class: "tiny", "No enabled bots yet." }
                } else if bots.len() > 1 {
                    div { class: "row-actions",
                        button {
                            class: "btn ghost sm",
                            r#type: "button",
                            onclick: {
                                let id = folder_id.clone();
                                let all_names: Vec<String> = bots.iter().map(|(n, _)| n.clone()).collect();
                                move |_| {
                                    remembered_only.set(all_names.clone());
                                    session.write().set_local_folder_bots(&id, BotScope::Only { bots: all_names.clone() });
                                }
                            },
                            "Select all"
                        }
                        button {
                            class: "btn ghost sm",
                            r#type: "button",
                            disabled: chosen.is_empty(),
                            onclick: {
                                let id = folder_id.clone();
                                move |_| {
                                    remembered_only.set(Vec::new());
                                    session.write().set_local_folder_bots(&id, BotScope::Only { bots: Vec::new() });
                                }
                            },
                            "Clear"
                        }
                    }
                }
                for (name, title) in bots.clone() {
                    label { key: "{name}", class: "check",
                        input {
                            r#type: "checkbox",
                            checked: chosen.contains(&name),
                            onchange: {
                                let (id, name, chosen) = (folder_id.clone(), name.clone(), chosen.clone());
                                move |event| {
                                    let mut next = chosen.clone();
                                    next.retain(|item| item != &name);
                                    if event.checked() {
                                        next.push(name.clone());
                                    }
                                    remembered_only.set(next.clone());
                                    session.write().set_local_folder_bots(&id, BotScope::Only { bots: next });
                                }
                            },
                        }
                        span { "{title}" }
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serabot_desktop::{Mem0SettingsView, MemorySettingsView, TurnLimitPair, TurnLimitsView};

    #[test]
    fn mem0_card_classifies_error_separate_from_loading() {
        assert_eq!(classify_mem0_card(None, None), Mem0CardState::Loading);
        assert_eq!(
            classify_mem0_card(None, Some("network error")),
            Mem0CardState::Error
        );
        let unavail = MemorySettingsView {
            provider: String::new(),
            mem0: Mem0SettingsView {
                available: false,
                configured: false,
            },
        };
        assert_eq!(
            classify_mem0_card(Some(&unavail), None),
            Mem0CardState::Unavailable
        );
        let avail = MemorySettingsView {
            provider: "mem0".into(),
            mem0: Mem0SettingsView {
                available: true,
                configured: true,
            },
        };
        assert_eq!(
            classify_mem0_card(Some(&avail), None),
            Mem0CardState::Available
        );
    }

    #[test]
    fn turn_limits_validation_rejects_out_of_bounds_without_clearing_caller_draft() {
        let view = TurnLimitsView {
            max_tool_calls_per_turn: 24,
            max_tool_rounds: 8,
            default: TurnLimitPair {
                max_tool_calls_per_turn: 24,
                max_tool_rounds: 8,
            },
            ceiling: TurnLimitPair {
                max_tool_calls_per_turn: 64,
                max_tool_rounds: 16,
            },
            overridden: false,
        };
        assert!(validate_turn_limits_draft("999", "", Some(view)).is_err());
        assert_eq!(
            validate_turn_limits_draft("30", "", Some(view)).unwrap(),
            ("30".into(), "8".into())
        );
        assert!(validate_turn_limits_draft("10", "4", None).is_err());
    }
}
