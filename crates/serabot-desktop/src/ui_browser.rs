//! The Browser page (0.16): every browser your bots have open, side by side.
//!
//! The grid shows one card per open browser with a live picture (about one
//! refresh a second, only while this page is on screen and the window has
//! focus), the profile name, the address, which bot is driving and who holds
//! control. A click opens the focus view: the browser large on the left, the
//! conversation of the bot driving it on the right. Below the grid, the
//! profiles section lists, adds, renames and deletes profiles.
//!
//! The pictures, polling and 429 back-off live in `panel.rs`; this file draws
//! and forwards clicks. Pointer and key capture on the large picture is done
//! by the script in `ui_dock.rs`, which looks for `#dock-stage`/`#dock-shot`.

use crate::ui::{ChatView, Icon, Overlay, avatar_style};
use dioxus::prelude::*;
use serabot_desktop::panel::{address_to_url, host_label, profile_users};
use serabot_desktop::session::Session;
use serabot_desktop::{
    BrowserController, BrowserInput, BrowserPointer, BrowserSessionInfo, BrowserTab,
};

/// A grid card: the session, its latest picture, and the bot driving it.
type Card = (BrowserSessionInfo, Option<String>, Option<(String, String)>);

#[component]
pub fn BrowserView(session: Signal<Session>, overlay: Signal<Overlay>) -> Element {
    let focus = session.read().panel.focus.clone();
    match focus {
        Some(id) => rsx! { FocusView { session, overlay, id } },
        None => rsx! { BrowserGrid { session } },
    }
}

/// Where the bot last moved or clicked, drawn over the live picture by the
/// script in `ui_dock.rs` (which maps viewport pixels onto the image). Hidden
/// from assistive tech; only rendered while the bot drives.
#[component]
pub fn PointerOverlay(pointer: Option<BrowserPointer>) -> Element {
    let Some(p) = pointer else {
        return rsx! {};
    };
    rsx! {
        div {
            id: "dock-pointer",
            class: "dock-pointer",
            "aria-hidden": "true",
            "data-x": "{p.x}",
            "data-y": "{p.y}",
            "data-vw": "{p.vw}",
            "data-vh": "{p.vh}",
            "data-seq": "{p.seq}",
            "data-action": "{p.action}",
            span { class: "dock-pointer-ripple" }
            span { class: "dock-pointer-dot" }
        }
    }
}

/// A short label for a tab: its title, else its host, else "New tab".
fn tab_label(tab: &BrowserTab) -> String {
    let title = tab.title.trim();
    if !title.is_empty() {
        return title.chars().take(28).collect();
    }
    match host_label(Some(tab.url.as_str())) {
        host if !host.is_empty() => host.chars().take(28).collect(),
        _ => "New tab".to_owned(),
    }
}

/// The tab strip above the page. Switching and closing need control, like
/// typing into the page does.
#[component]
fn TabStrip(session: Signal<Session>, tabs: Vec<BrowserTab>, in_control: bool) -> Element {
    let mut session = session;
    let only = tabs.len() <= 1;
    rsx! {
        div { class: "tab-strip", role: "tablist", "aria-label": "Browser tabs",
            for tab in tabs {
                {
                    let label = tab_label(&tab);
                    let switch_id = tab.id.clone();
                    let close_id = tab.id.clone();
                    rsx! {
                        div { key: "{tab.id}", class: if tab.active { "tab active" } else { "tab" },
                            button {
                                class: "tab-main",
                                role: "tab",
                                "aria-selected": if tab.active { "true" } else { "false" },
                                title: if in_control { "{tab.url}" } else { "Take control to switch tabs" },
                                disabled: !in_control && !tab.active,
                                onclick: move |_| session.write().panel.switch_tab(&switch_id),
                                "{label}"
                            }
                            if in_control && !only {
                                button {
                                    class: "tab-close",
                                    "aria-label": "Close tab {label}",
                                    title: "Close tab",
                                    onclick: move |_| session.write().panel.close_tab(&close_id),
                                    "\u{d7}"
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

fn controller_pill(controller: BrowserController) -> (&'static str, &'static str) {
    match controller {
        BrowserController::Bot => ("pill ok", "Bot is driving"),
        BrowserController::User => ("pill warn", "You're driving"),
    }
}

#[component]
fn BrowserGrid(session: Signal<Session>) -> Element {
    let mut session = session;
    let (cards, loaded) = {
        let s = session.read();
        let cards: Vec<Card> = s
            .panel
            .active_sessions()
            .into_iter()
            .map(|info| {
                let shot = s.panel.shots.get(&info.id).cloned();
                let bot = info
                    .bot
                    .as_ref()
                    .map(|bot| (bot.clone(), s.bot_display_name(bot)));
                (info.clone(), shot, bot)
            })
            .collect();
        (cards, s.panel.sessions_loaded)
    };
    rsx! {
        div { class: "pane browser-page",
            div { class: "pane-head",
                div {
                    h1 { class: "pane-title", "Browser" }
                    p { class: "muted", "Websites your bots have open. Watch them, step in, or close them." }
                }
            }
            div { class: "pane-body",
                if !loaded {
                    div { class: "browser-grid", "aria-busy": "true",
                        div { class: "browser-card skeleton-card", div { class: "skeleton block", " " } }
                        div { class: "browser-card skeleton-card", div { class: "skeleton block", " " } }
                    }
                } else if cards.is_empty() {
                    div { class: "empty browser-empty",
                        div { class: "empty-icon", Icon { name: "globe", size: 20 } }
                        h2 { class: "empty-title", "No browser open." }
                        p { class: "empty-text", "Ask a bot to open a website." }
                    }
                } else {
                    div { class: "browser-grid",
                        for (info, shot, bot) in cards {
                            {
                                let (pill, label) = controller_pill(info.controller);
                                let host = host_label(info.url.as_deref());
                                let id = info.id.clone();
                                let title = info.title.clone().unwrap_or_default();
                                rsx! {
                                    div { key: "{info.id}", class: "browser-card",
                                        button {
                                            class: "browser-card-main",
                                            "aria-label": "Open {info.profile_name}: {host}",
                                            onclick: move |_| session.write().focus_browser(&id),
                                            div { class: "browser-thumb",
                                                if let Some(src) = shot {
                                                    img { class: "browser-shot", alt: "Live view of {host}", draggable: false, src: "{src}" }
                                                } else {
                                                    div { class: "skeleton block", " " }
                                                }
                                            }
                                            div { class: "browser-card-copy",
                                                div { class: "browser-card-top",
                                                    strong { class: "browser-profile", "{info.profile_name}" }
                                                    span { class: "{pill}", "{label}" }
                                                }
                                                span { class: "browser-host", title: "{title}", "{host}" }
                                                if let Some((bot_id, name)) = bot {
                                                    span { class: "browser-bot",
                                                        span { class: "avatar sm bot", style: avatar_style(&bot_id),
                                                            "{name.chars().next().map(|c| c.to_uppercase().to_string()).unwrap_or_default()}"
                                                        }
                                                        "{name}"
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
                ProfilesSection { session }
            }
        }
    }
}

/// Profiles: a named set of logins. Rename, delete (with a confirm step) and
/// add; each row says which bots use it.
#[component]
fn ProfilesSection(session: Signal<Session>) -> Element {
    let mut session = session;
    let mut new_name = use_signal(String::new);
    let mut renaming = use_signal(|| Option::<(String, String)>::None);
    let mut confirming = use_signal(|| Option::<String>::None);
    let (profiles, loaded, busy, error, notice) = {
        let s = session.read();
        let p = &s.panel;
        (
            p.profiles.clone(),
            p.profiles_loaded,
            p.profiles_busy,
            p.profiles_error.clone(),
            p.profiles_notice.clone(),
        )
    };
    let rows: Vec<(serabot_desktop::BrowserProfile, Vec<String>)> = {
        let s = session.read();
        profiles
            .iter()
            .map(|profile| {
                let users = profile_users(profile)
                    .iter()
                    .map(|bot| s.bot_display_name(bot))
                    .collect();
                (profile.clone(), users)
            })
            .collect()
    };
    let full = profiles.len() >= serabot_desktop::MAX_BROWSER_PROFILES;
    rsx! {
        section { class: "profiles-section", "aria-labelledby": "profiles-title",
            div { class: "pane-section-title", id: "profiles-title", "Profiles" }
            p { class: "muted profiles-sub",
                "A profile keeps its own logins and cookies. Give a bot its own profile in its settings."
            }
            if let Some(message) = error {
                div { class: "error-text", role: "alert", "{message}" }
            }
            if let Some(message) = notice {
                div { class: "vault-notice", role: "status",
                    Icon { name: "check", size: 14 }
                    span { "{message}" }
                }
            }
            div { class: "profile-list",
                if !loaded {
                    div { class: "skeleton row", " " }
                }
                for (profile, users) in rows {
                    {
                        let id = profile.id.clone();
                        let editing = renaming().as_ref().is_some_and(|(edit, _)| edit == &id);
                        let confirm = confirming().as_deref() == Some(id.as_str());
                        let used = if users.is_empty() {
                            "No bots use it yet.".to_owned()
                        } else {
                            format!("Used by {}", users.join(", "))
                        };
                        rsx! {
                            div { key: "{profile.id}", class: "profile-row",
                                div { class: "profile-copy",
                                    if editing {
                                        input {
                                            class: "input",
                                            autofocus: true,
                                            "aria-label": "Profile name",
                                            value: "{renaming().map(|(_, text)| text).unwrap_or_default()}",
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
                                                            session.write().panel.rename_profile(&id, &text);
                                                        }
                                                        renaming.set(None);
                                                    } else if event.key() == Key::Escape {
                                                        renaming.set(None);
                                                    }
                                                }
                                            },
                                        }
                                    } else {
                                        div { class: "profile-name",
                                            strong { "{profile.name}" }
                                            if profile.is_default {
                                                span { class: "chip muted", "Default" }
                                            }
                                        }
                                        span { class: "caption", "{used}" }
                                    }
                                }
                                if confirm {
                                    div { class: "vault-confirm", role: "group", "aria-label": "Confirm delete",
                                        span { class: "caption", "Delete {profile.name}? Its logins go too." }
                                        button {
                                            class: "btn danger sm",
                                            disabled: busy,
                                            onclick: {
                                                let id = id.clone();
                                                move |_| {
                                                    session.write().panel.delete_profile(&id);
                                                    confirming.set(None);
                                                }
                                            },
                                            "Delete"
                                        }
                                        button { class: "btn ghost sm", onclick: move |_| confirming.set(None), "Keep" }
                                    }
                                } else if editing {
                                    div { class: "profile-actions",
                                        button {
                                            class: "btn primary sm",
                                            disabled: busy,
                                            onclick: {
                                                let id = id.clone();
                                                move |_| {
                                                    if let Some((_, text)) = renaming() {
                                                        session.write().panel.rename_profile(&id, &text);
                                                    }
                                                    renaming.set(None);
                                                }
                                            },
                                            "Save"
                                        }
                                        button { class: "btn ghost sm", onclick: move |_| renaming.set(None), "Cancel" }
                                    }
                                } else {
                                    div { class: "profile-actions",
                                        button {
                                            class: "icon-btn",
                                            title: "Rename",
                                            "aria-label": "Rename {profile.name}",
                                            onclick: {
                                                let id = id.clone();
                                                let name = profile.name.clone();
                                                move |_| renaming.set(Some((id.clone(), name.clone())))
                                            },
                                            Icon { name: "edit", size: 15 }
                                        }
                                        if !profile.is_default {
                                            button {
                                                class: "icon-btn",
                                                title: "Delete",
                                                "aria-label": "Delete {profile.name}",
                                                onclick: {
                                                    let id = id.clone();
                                                    move |_| confirming.set(Some(id.clone()))
                                                },
                                                Icon { name: "trash", size: 15 }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
            form {
                class: "profile-add",
                onsubmit: move |event| {
                    event.prevent_default();
                    session.write().panel.create_profile(&new_name());
                    new_name.set(String::new());
                },
                input {
                    class: "input",
                    value: "{new_name}",
                    placeholder: "New profile name, like Work",
                    "aria-label": "New profile name",
                    disabled: full,
                    oninput: move |event| new_name.set(event.value()),
                }
                button {
                    class: "btn",
                    r#type: "submit",
                    disabled: busy || full || new_name().trim().is_empty(),
                    Icon { name: "plus", size: 15 }
                    "Add profile"
                }
            }
            if full {
                span { class: "hint", "That's the most profiles you can have (8). Delete one to add another." }
            }
        }
    }
}

/// One browser large, with the conversation of the bot driving it.
#[component]
fn FocusView(session: Signal<Session>, overlay: Signal<Overlay>, id: String) -> Element {
    let mut session = session;
    let mut full = use_signal(|| false);
    let mut editing = use_signal(|| false);
    let mut draft = use_signal(String::new);
    let mut confirm_end = use_signal(|| false);
    let (info, shot, busy, error, bot, in_control) = {
        let s = session.read();
        let info = s.panel.session(&id).cloned();
        let bot = info.as_ref().and_then(|i| i.bot.clone());
        (
            info,
            s.panel.shots.get(&id).cloned(),
            s.panel.control_busy,
            s.panel.browser_error.clone(),
            bot,
            s.panel.user_in_control(),
        )
    };
    let active = info.as_ref().is_some_and(|i| i.active);
    let profile_name = info
        .as_ref()
        .map(|i| i.profile_name.clone())
        .unwrap_or_else(|| id.clone());
    let tabs = info.as_ref().map(|i| i.tabs.clone()).unwrap_or_default();
    let pointer = info.as_ref().and_then(|i| i.pointer.clone());
    let url = info
        .as_ref()
        .and_then(|i| i.url.clone())
        .unwrap_or_default();
    let url_text = if url.is_empty() {
        "No page open".to_owned()
    } else {
        url.clone()
    };
    let (pill, label) = controller_pill(
        info.as_ref()
            .map_or(BrowserController::Bot, |i| i.controller),
    );
    let bot_name = bot.as_ref().map(|b| session.read().bot_display_name(b));
    let bot_letter = bot.as_ref().map(|b| session.read().bot_avatar(b));
    let has_chat = info.as_ref().is_some_and(|i| i.conversation_id.is_some()) || bot.is_some();
    let url_shown = if editing() { draft() } else { url.clone() };
    rsx! {
        div { class: if full() { "browser-focus full" } else { "browser-focus" },
            onkeydown: move |event: KeyboardEvent| {
                if event.key() == Key::Escape && full() {
                    full.set(false);
                }
            },
            section { class: "focus-stage", "aria-label": "{profile_name} browser",
                div { class: "focus-bar",
                    button {
                        class: "btn ghost sm",
                        title: "Back to all browsers",
                        onclick: move |_| session.write().panel.clear_focus(),
                        Icon { name: "chevron-left", size: 14 }
                        "All browsers"
                    }
                    strong { class: "focus-name", "{profile_name}" }
                    span { class: "{pill}", role: "status", if active { "{label}" } else { "Closed" } }
                    div { class: "focus-spacer" }
                    button {
                        class: "icon-btn",
                        title: if full() { "Leave full window" } else { "Full window" },
                        "aria-label": if full() { "Leave full window" } else { "Full window" },
                        "aria-pressed": if full() { "true" } else { "false" },
                        onclick: move |_| full.set(!full()),
                        Icon { name: if full() { "minimize" } else { "maximize" }, size: 16 }
                    }
                }
                if in_control {
                    div { class: "dock-url editable",
                        Icon { name: "globe", size: 14 }
                        input {
                            class: "dock-address",
                            "aria-label": "Address",
                            placeholder: "Type an address and press Enter",
                            value: "{url_shown}",
                            onfocus: {
                                let url = url.clone();
                                move |_| {
                                    draft.set(url.clone());
                                    editing.set(true);
                                }
                            },
                            onblur: move |_| editing.set(false),
                            oninput: move |event| draft.set(event.value()),
                            onkeydown: move |event: KeyboardEvent| {
                                event.stop_propagation();
                                if event.key() == Key::Enter {
                                    match address_to_url(&draft()) {
                                        Some(url) => {
                                            session.write().panel.send_input(BrowserInput::Navigate { url });
                                            editing.set(false);
                                        }
                                        None => {
                                            session.write().panel.browser_error = Some("Enter a web address.".into());
                                        }
                                    }
                                }
                            },
                        }
                    }
                } else {
                    div { class: "dock-url", title: "{url_text}",
                        Icon { name: "globe", size: 14 }
                        span { class: "dock-url-text", "{url_text}" }
                    }
                }
                if active && !tabs.is_empty() {
                    TabStrip { session, tabs: tabs.clone(), in_control }
                }
                div { class: if active && shot.is_some() { "focus-view has-shot" } else { "focus-view" },
                    if active && shot.is_some() {
                        div {
                            id: "dock-stage",
                            class: if in_control { "dock-stage control" } else { "dock-stage" },
                            tabindex: if in_control { 0 } else { -1 },
                            "data-control": if in_control { "user" } else { "bot" },
                            "aria-label": if in_control { "Live page. You are in control: click, scroll and type here." } else { "Live page" },
                            img {
                                id: "dock-shot",
                                class: "dock-shot",
                                alt: "Live view of the page",
                                draggable: false,
                                src: shot.clone().unwrap_or_default(),
                            }
                            if !in_control {
                                PointerOverlay { pointer }
                            }
                        }
                    } else if active {
                        div { class: "skeleton block", " " }
                    } else {
                        div { class: "empty",
                            div { class: "empty-icon", Icon { name: "globe", size: 20 } }
                            h2 { class: "empty-title", "This browser is closed" }
                            p { class: "empty-text", "Go back to see the ones that are still open." }
                        }
                    }
                }
                if let Some(message) = error {
                    div { class: "error-text dock-error", role: "alert", "{message}" }
                }
                div { class: "focus-actions",
                    if in_control {
                        button {
                            class: "btn primary",
                            disabled: busy || !active,
                            onclick: move |_| session.write().panel.hand_back(),
                            "Give control back"
                        }
                    } else {
                        button {
                            class: "btn primary",
                            disabled: busy || !active,
                            onclick: move |_| session.write().panel.take_over(),
                            "Take control"
                        }
                    }
                    button {
                        class: "btn ghost",
                        title: "Close this view. The bot keeps working.",
                        onclick: move |_| session.write().panel.clear_focus(),
                        "Hide"
                    }
                    div { class: "focus-spacer" }
                    if confirm_end() {
                        span { class: "caption", "End this browser?" }
                        button {
                            class: "btn danger",
                            onclick: move |_| {
                                session.write().panel.close_browser();
                                confirm_end.set(false);
                            },
                            "End session"
                        }
                        button { class: "btn ghost", onclick: move |_| confirm_end.set(false), "Keep it" }
                    } else {
                        button {
                            class: "btn ghost danger-text",
                            disabled: !active,
                            onclick: move |_| confirm_end.set(true),
                            "End session"
                        }
                    }
                }
            }
            section { class: "focus-chat", "aria-label": "Conversation",
                div { class: "focus-chat-head",
                    if let (Some(b), Some(name), Some(letter)) = (bot.clone(), bot_name, bot_letter) {
                        span { class: "avatar bot", style: avatar_style(&b), "{letter}" }
                        div { class: "chat-header-copy",
                            strong { "{name}" }
                            span { "is driving this browser" }
                        }
                    } else {
                        div { class: "chat-header-copy",
                            strong { "Conversation" }
                            span { "No bot has used this browser yet" }
                        }
                    }
                }
                if has_chat {
                    ChatView { session, overlay }
                } else {
                    div { class: "empty",
                        div { class: "empty-icon", Icon { name: "chat", size: 20 } }
                        p { class: "empty-text", "When a bot opens a page here, its conversation shows up next to it." }
                    }
                }
            }
        }
    }
}

/// Plain label for tests of the pill wording.
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_pill_names_who_is_driving() {
        assert_eq!(controller_pill(BrowserController::Bot).1, "Bot is driving");
        assert_eq!(controller_pill(BrowserController::User).1, "You're driving");
    }
}
