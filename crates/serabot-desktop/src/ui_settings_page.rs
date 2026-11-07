//! Pieces of the Settings page (0.17): the left section list and the cards
//! for the device-local preferences. The older cards (account, Telegram,
//! voice, safety checks, updates) stay in `ui.rs` and are shown per section.
//!
//! Preferences saved here live on this device only (`creds::UiPrefs`).

use dioxus::prelude::*;
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
