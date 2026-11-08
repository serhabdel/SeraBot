//! Pieces of the Settings page (0.17): the left section list and the cards
//! for the device-local preferences. The older cards (account, Telegram,
//! voice, safety checks, updates) stay in `ui.rs` and are shown per section.
//!
//! Preferences saved here live on this device only (`creds::UiPrefs`).

use crate::ui::Icon;
use dioxus::prelude::*;
use serabot_desktop::local_folders::BotScope;
use serabot_desktop::session::{Session, SettingsSection, Theme};

/// The list of sections on the left. Admin only shows for admins.
#[component]
pub fn SettingsNav(session: Signal<Session>) -> Element {
    let mut session = session;
    let (current, is_admin) = {
        let s = session.read();
        (s.settings_section, s.can_admin())
    };
    rsx! {
        nav { class: "settings-nav", "aria-label": "Settings sections",
            for section in SettingsSection::ALL.into_iter().filter(|s| s.visible(is_admin)) {
                button {
                    key: "{section.label()}",
                    class: if current == section { "settings-nav-item on" } else { "settings-nav-item" },
                    "aria-current": if current == section { "page" } else { "false" },
                    onclick: move |_| session.write().settings_section = section,
                    "{section.label()}"
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
    rsx! {
        div { class: "card",
            div { class: "card-head",
                h3 { class: "card-title", "Local folders" }
            }
            p { class: "tiny",
                "Let your bots browse, search and read big folders on this PC, bring Word, Excel, PowerPoint, PDF and image files in to work on, and with your approval save results back. "
                "It is off until you add a folder. Only this app can see the folder paths; the bots get a short name."
            }
            if folders.is_empty() {
                p { class: "hint", "No folders allowed. The bots cannot see anything on this PC." }
            }
            div { class: "roster",
                for folder in folders.clone() {
                    div { key: "{folder.id}", class: "roster-row",
                        div { class: "thread-copy",
                            span { class: "thread-title", "{folder.id}" }
                            span { class: "thread-meta", "{folder.path}" }
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
                        button {
                            class: "btn icon",
                            title: "Remove {folder.id}",
                            "aria-label": "Remove {folder.id}",
                            onclick: {
                                let id = folder.id.clone();
                                move |_| session.write().remove_local_folder(&id)
                            },
                            Icon { name: "trash", size: 15 }
                        }
                    }
                }
            }
            div { class: "row",
                button {
                    class: "btn",
                    onclick: move |_| session.write().pick_local_folder(),
                    "Add folder\u{2026}"
                }
                if !folders.is_empty() {
                    button {
                        class: "btn",
                        onclick: move |_| session.write().recount_folders(),
                        "Recount"
                    }
                }
                if running {
                    span { class: "chip ok", "Connected to your bots" }
                }
            }
            if let Some(notice) = notice {
                p { class: "hint", "{notice}" }
            }
            p { class: "tiny",
                "Writes always ask for your approval. Links, junctions, and files like .env or private keys are never shared. "
                "If this app is closed, the bots get a clear error instead."
            }
        }
    }
}

/// One folder's bot grant: every bot, or only the bots ticked here. A new
/// folder that is switched to "Only these bots" starts with none ticked.
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
    rsx! {
        div { class: "bot-scope",
            div { class: "seg", role: "group", "aria-label": "Bots that can use {folder_id}",
                button {
                    class: if scoped { "seg-btn" } else { "seg-btn active" },
                    "aria-pressed": if scoped { "false" } else { "true" },
                    onclick: {
                        let id = folder_id.clone();
                        move |_| session.write().set_local_folder_bots(&id, BotScope::All)
                    },
                    "All bots"
                }
                button {
                    class: if scoped { "seg-btn active" } else { "seg-btn" },
                    "aria-pressed": if scoped { "true" } else { "false" },
                    onclick: {
                        let id = folder_id.clone();
                        move |_| session.write().set_local_folder_bots(&id, BotScope::Only { bots: Vec::new() })
                    },
                    "Only these bots"
                }
            }
            if scoped {
                if bots.is_empty() {
                    p { class: "tiny", "No enabled bots yet." }
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
