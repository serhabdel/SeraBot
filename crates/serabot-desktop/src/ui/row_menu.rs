use super::*;
use serabot_desktop::row_menu::{self, RowAction, RowKind};

/// Move keyboard focus within the currently focused `[role="menu"]` across
/// enabled `[role^="menuitem"]` children (`step`: 1, -1, or extreme Home/End).
fn handle_menu_key(key: &Key) -> bool {
    let script = match key {
        Key::ArrowDown => Some(
            "(() => { const m = document.activeElement && document.activeElement.closest('[role=\"menu\"]'); if (!m) return; const items = Array.from(m.querySelectorAll('[role^=\"menuitem\"]:not([disabled])')); if (!items.length) return; const i = items.indexOf(document.activeElement); items[(i + 1 + items.length) % items.length].focus(); })()",
        ),
        Key::ArrowUp => Some(
            "(() => { const m = document.activeElement && document.activeElement.closest('[role=\"menu\"]'); if (!m) return; const items = Array.from(m.querySelectorAll('[role^=\"menuitem\"]:not([disabled])')); if (!items.length) return; const i = items.indexOf(document.activeElement); items[(i <= 0 ? items.length - 1 : i - 1)].focus(); })()",
        ),
        Key::Home => Some(
            "(() => { const m = document.activeElement && document.activeElement.closest('[role=\"menu\"]'); const first = m && m.querySelector('[role^=\"menuitem\"]:not([disabled])'); if (first) first.focus(); })()",
        ),
        Key::End => Some(
            "(() => { const m = document.activeElement && document.activeElement.closest('[role=\"menu\"]'); if (!m) return; const items = Array.from(m.querySelectorAll('[role^=\"menuitem\"]:not([disabled])')); if (items.length) items[items.length - 1].focus(); })()",
        ),
        _ => None,
    };
    if let Some(js) = script {
        document::eval(js);
        true
    } else {
        false
    }
}

/// Keep a floating `.row-menu` inside the viewport if it overflows the bottom edge.
const FLIP_OVERFLOW_MENU_JS: &str = "requestAnimationFrame(() => { for (const m of document.querySelectorAll('.row-menu[role=\"menu\"]')) { m.style.top = ''; m.style.bottom = ''; const r = m.getBoundingClientRect(); if (r.bottom > window.innerHeight - 8 && r.top > r.height + 16) { m.style.top = 'auto'; m.style.bottom = 'calc(100% - 4px)'; } } })";

/// Fires after five seconds; the undo bar listens for it.
const UNDO_TIMER_JS: &str = "setTimeout(() => dioxus.send(true), 5000);";

/// Show the "Chat archived. Undo" bar for five seconds.
fn start_undo(undo: Signal<Option<String>>, id: String) {
    let mut undo = undo;
    undo.set(Some(id.clone()));
    spawn(async move {
        let mut done = dioxus::document::eval(UNDO_TIMER_JS);
        let _ = done.recv::<bool>().await;
        if undo.peek().as_deref() == Some(id.as_str()) {
            undo.set(None);
        }
    });
}

/// Carries out a menu entry on the session.
#[allow(clippy::too_many_arguments)]
fn run_row_action(
    action: RowAction,
    session: Signal<Session>,
    toast: Signal<Option<(String, bool)>>,
    undo: Signal<Option<String>>,
    renaming: Signal<Option<(String, String)>>,
    id: &str,
    pin_id: &str,
    label: &str,
) {
    let mut session = session;
    let mut toast = toast;
    let mut renaming = renaming;
    match action {
        RowAction::FinishSetup => session.write().open_onboarding(pin_id.to_owned()),
        RowAction::Settings => session.write().open_bot_settings(pin_id.to_owned()),
        RowAction::Pin | RowAction::Unpin => session.write().toggle_pin(pin_id.to_owned()),
        RowAction::SwitchOn => session.write().set_bot_enabled(pin_id.to_owned(), true),
        RowAction::SwitchOff => session.write().set_bot_enabled(pin_id.to_owned(), false),
        RowAction::DeleteBot => session.write().ask_delete_bot(pin_id.to_owned()),
        RowAction::Rename => renaming.set(Some((id.to_owned(), label.to_owned()))),
        RowAction::SetImage => session.write().pick_group_image(id.to_owned()),
        RowAction::Archive => {
            // Leaving an archived chat open would strand the reader in it.
            let home = {
                let state = session.read();
                (state.conversation == id)
                    .then(|| state.conversation_bot(id))
                    .flatten()
            };
            session.write().archive_conversation(id.to_owned());
            if let Some(bot) = home {
                session.write().open_bot_chat(bot);
            }
            start_undo(undo, id.to_owned());
        }
        RowAction::Unarchive => {
            session.write().unarchive_conversation(id.to_owned());
            toast.set(Some(("Restored.".into(), false)));
        }
    }
}

/// The "more actions" menu for one row or for the open chat. Opened from the
/// row's more button, a right click, or the chat header; closes on a click
/// outside, on Escape, or after an action. Archive does not ask: it shows an
/// Undo bar instead.
#[component]
pub(crate) fn RowMenu(
    session: Signal<Session>,
    toast: Signal<Option<(String, bool)>>,
    undo: Signal<Option<String>>,
    renaming: Signal<Option<(String, String)>>,
    menu: Signal<Option<String>>,
    id: String,
    pin_id: String,
    label: String,
    kind: RowKind,
    pinned: bool,
    #[props(default = false)] header: bool,
) -> Element {
    let mut menu = menu;
    let items = if header {
        row_menu::header_actions(&kind, pinned)
    } else {
        row_menu::actions(&kind, pinned)
    };
    rsx! {
        div {
            class: "row-menu-scrim",
            onclick: move |event| {
                event.stop_propagation();
                menu.set(None);
            },
            oncontextmenu: move |event| {
                event.prevent_default();
                menu.set(None);
            },
        }
        div {
            class: if header { "row-menu header" } else { "row-menu" },
            role: "menu",
            tabindex: "-1",
            "aria-label": "Actions for {label}",
            onmounted: move |_| {
                document::eval(FLIP_OVERFLOW_MENU_JS);
            },
            onkeydown: move |event: KeyboardEvent| {
                let key = event.key();
                if key == Key::Escape {
                    event.stop_propagation();
                    menu.set(None);
                } else if handle_menu_key(&key) {
                    event.prevent_default();
                    event.stop_propagation();
                }
            },
            for (idx, action) in items.into_iter().enumerate() {
                button {
                    key: "{action.label()}",
                    class: if action.is_danger() { "row-menu-item danger" } else { "row-menu-item" },
                    r#type: "button",
                    role: "menuitem",
                    autofocus: idx == 0,
                    onclick: {
                        let id = id.clone();
                        let pin_id = pin_id.clone();
                        let label = label.clone();
                        move |event| {
                            event.stop_propagation();
                            run_row_action(action, session, toast, undo, renaming, &id, &pin_id, &label);
                            menu.set(None);
                        }
                    },
                    Icon { name: action.icon().to_owned(), size: 15 }
                    span { "{action.label()}" }
                }
            }
        }
    }
}

/// The bar that follows an archive: what happened and a way back.
#[component]
pub(crate) fn UndoBar(session: Signal<Session>, undo: Signal<Option<String>>) -> Element {
    let mut undo = undo;
    let Some(id) = undo() else {
        return rsx! {};
    };
    rsx! {
        div { class: "toast undo-bar", role: "status",
            span { "Chat archived" }
            button {
                class: "btn ghost sm",
                onclick: move |_| {
                    session.write().unarchive_conversation(id.clone());
                    undo.set(None);
                },
                "Undo"
            }
        }
    }
}

/// Inline rename for a chat row: Enter saves, Escape cancels.
#[component]
pub(crate) fn RenameField(
    session: Signal<Session>,
    toast: Signal<Option<(String, bool)>>,
    renaming: Signal<Option<(String, String)>>,
    id: String,
) -> Element {
    let mut renaming = renaming;
    let mut toast = toast;
    let mut save = {
        let id = id.clone();
        move || {
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
    };
    let mut save_key = save.clone();
    rsx! {
        div { class: "thread-rename",
            input {
                class: "input",
                autofocus: true,
                "aria-label": "Chat name",
                maxlength: 120,
                value: "{renaming().as_ref().map(|(_, text)| text.clone()).unwrap_or_default()}",
                oninput: move |event| {
                    if let Some((_, text)) = renaming.write().as_mut() {
                        *text = event.value();
                    }
                },
                onkeydown: move |event: KeyboardEvent| {
                    if event.key() == Key::Enter {
                        event.prevent_default();
                        save_key();
                    }
                    if event.key() == Key::Escape {
                        renaming.set(None);
                    }
                },
            }
            button {
                class: "icon-btn",
                title: "Save name",
                "aria-label": "Save name",
                onclick: move |_| save(),
                Icon { name: "check", size: 16 }
            }
        }
    }
}

/// The chat switcher in the chat header: the open chat's name with a chevron.
/// It lists the bot's chats, marks the open one, and starts a new chat. It is
/// always there for a bot chat, so switching never depends on finding a
/// hidden control in the sidebar.
#[component]
pub(crate) fn ChatSwitcher(session: Signal<Session>, bot: String, title: String) -> Element {
    let mut session = session;
    let mut open = use_signal(|| false);
    let mut renaming = use_signal(|| Option::<(String, String)>::None);
    let (entries, enabled, active_conv, active_title, unread) = {
        let state = session.read();
        let entries = serabot_desktop::bot_chats::switcher_entries(
            state.state.bot_chats.get(&bot),
            &bot,
            &state.conversation,
        );
        let enabled = state
            .state
            .listed_bots
            .iter()
            .find(|item| item.id == bot)
            .is_none_or(|item| item.enabled);
        let unread: std::collections::HashMap<String, i64> = state
            .state
            .conversations
            .iter()
            .map(|c| (c.id.clone(), c.unread_count))
            .collect();
        (
            entries,
            enabled,
            state.conversation.clone(),
            state.conversation_title(),
            unread,
        )
    };
    let count = entries.len();
    let active_pos = entries.iter().position(|entry| entry.active);
    rsx! {
        div { class: "chat-switcher",
            button {
                class: "chat-switcher-btn",
                r#type: "button",
                title: "Switch chat or start a new one",
                "aria-haspopup": "menu",
                "aria-expanded": if open() { "true" } else { "false" },
                onclick: move |_| {
                    renaming.set(None);
                    open.set(!open());
                },
                strong { "{title}" }
                if count > 1 {
                    span { class: "chat-switcher-count", "{count}" }
                }
                Icon { name: if open() { "chevron-up" } else { "chevron-down" }, size: 14 }
            }
            if open() {
                div {
                    class: "row-menu-scrim",
                    onclick: move |_| {
                        renaming.set(None);
                        open.set(false);
                    },
                }
                div {
                    class: "row-menu chat-switcher-menu",
                    role: "menu",
                    tabindex: "-1",
                    "aria-label": "Chats with this bot",
                    onmounted: move |_| {
                        document::eval(FLIP_OVERFLOW_MENU_JS);
                    },
                    onkeydown: move |event: KeyboardEvent| {
                        if renaming().is_some() {
                            return;
                        }
                        let key = event.key();
                        if key == Key::Escape {
                            event.stop_propagation();
                            open.set(false);
                        } else if handle_menu_key(&key) {
                            event.prevent_default();
                            event.stop_propagation();
                        }
                    },
                    for (idx, entry) in entries.into_iter().enumerate() {
                        {
                            let entry_id = entry.id.clone();
                            let display_title = if entry.id == active_conv
                                && !active_title.is_empty()
                                && entry.title.starts_with("New conversation")
                            {
                                active_title.clone()
                            } else {
                                entry.title.clone()
                            };
                            let editing = renaming()
                                .as_ref()
                                .is_some_and(|(edit_id, _)| edit_id == &entry_id);
                            let mut meta_parts = Vec::new();
                            if entry.turns == 1 {
                                meta_parts.push("1 turn".to_owned());
                            } else if entry.turns > 1 {
                                meta_parts.push(format!("{} turns", entry.turns));
                            }
                            if let Some(ts) = entry.last_activity.as_deref() {
                                let formatted = stamp(ts);
                                if !formatted.is_empty() {
                                    meta_parts.push(formatted);
                                }
                            }
                            let meta = meta_parts.join(" \u{00b7} ");
                            let unread_n = unread.get(&entry_id).copied().unwrap_or(0);
                            let badge = chat_feed::unread_badge(unread_n);
                            let badge_label = chat_feed::unread_label(unread_n);
                            rsx! {
                                div {
                                    key: "{entry_id}",
                                    class: if entry.active { "chat-switcher-row active" } else { "chat-switcher-row" },
                                    if editing {
                                        div { class: "chat-switcher-rename",
                                            input {
                                                class: "input",
                                                autofocus: true,
                                                "aria-label": "Chat name",
                                                maxlength: 120,
                                                "data-rename-init": "{renaming().as_ref().map(|(_, t)| t.clone()).unwrap_or_default()}",
                                                onmounted: move |_| {
                                                    document::eval(
                                                        "(() => { const el = document.querySelector('.chat-switcher-rename input[data-rename-init]'); if (!el) return; el.value = el.getAttribute('data-rename-init') || ''; el.removeAttribute('data-rename-init'); el.setSelectionRange(el.value.length, el.value.length); })()",
                                                    );
                                                },
                                                oninput: move |event| {
                                                    if let Some((_, text)) = renaming.write().as_mut() {
                                                        *text = event.value();
                                                    }
                                                },
                                                onkeydown: {
                                                    let id = entry_id.clone();
                                                    move |event: KeyboardEvent| {
                                                        if event.key() == Key::Enter {
                                                            event.prevent_default();
                                                            event.stop_propagation();
                                                            if let Some((_, text)) = renaming()
                                                                && !text.trim().is_empty()
                                                            {
                                                                session.write().rename_conversation(id.clone(), text);
                                                            }
                                                            renaming.set(None);
                                                        } else if event.key() == Key::Escape {
                                                            event.stop_propagation();
                                                            renaming.set(None);
                                                        }
                                                    }
                                                },
                                            }
                                            button {
                                                class: "icon-btn",
                                                r#type: "button",
                                                title: "Save name",
                                                "aria-label": "Save name",
                                                onclick: {
                                                    let id = entry_id.clone();
                                                    move |event| {
                                                        event.stop_propagation();
                                                        if let Some((_, text)) = renaming()
                                                            && !text.trim().is_empty()
                                                        {
                                                            session.write().rename_conversation(id.clone(), text);
                                                        }
                                                        renaming.set(None);
                                                    }
                                                },
                                                Icon { name: "check", size: 14 }
                                            }
                                        }
                                    } else {
                                        button {
                                            class: if entry.active { "row-menu-item active chat-switcher-item" } else { "row-menu-item chat-switcher-item" },
                                            r#type: "button",
                                            role: "menuitemradio",
                                            autofocus: idx == active_pos.unwrap_or(0),
                                            "aria-checked": if entry.active { "true" } else { "false" },
                                            onclick: {
                                                let id = entry_id.clone();
                                                move |_| {
                                                    open.set(false);
                                                    session.write().open_conversation(id.clone());
                                                }
                                            },
                                            Icon { name: if entry.active { "check" } else { "chat" }, size: 15 }
                                            div { class: "chat-switcher-copy",
                                                span { class: "chat-switcher-title", "{display_title}" }
                                                if !meta.is_empty() {
                                                    span { class: "chat-switcher-meta", "{meta}" }
                                                }
                                            }
                                            if let Some(b) = badge {
                                                span { class: "unread-badge", role: "img", "aria-label": "{badge_label}", "{b}" }
                                            }
                                        }
                                        if !entry.primary {
                                            div { class: "chat-switcher-actions",
                                                button {
                                                    class: "icon-btn sm",
                                                    r#type: "button",
                                                    title: "Rename chat",
                                                    "aria-label": "Rename {display_title}",
                                                    onclick: {
                                                        let id = entry_id.clone();
                                                        let current_title = display_title.clone();
                                                        move |event| {
                                                            event.stop_propagation();
                                                            renaming.set(Some((id.clone(), current_title.clone())));
                                                        }
                                                    },
                                                    Icon { name: "edit", size: 13 }
                                                }
                                                button {
                                                    class: "icon-btn sm danger",
                                                    r#type: "button",
                                                    title: "Archive chat",
                                                    "aria-label": "Archive {display_title}",
                                                    onclick: {
                                                        let id = entry_id.clone();
                                                        let bot = bot.clone();
                                                        let is_active = entry.active;
                                                        move |event| {
                                                            event.stop_propagation();
                                                            session.write().archive_conversation(id.clone());
                                                            if is_active {
                                                                session.write().open_bot_chat(bot.clone());
                                                            }
                                                        }
                                                    },
                                                    Icon { name: "trash", size: 13 }
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                    button {
                        class: "row-menu-item new",
                        r#type: "button",
                        role: "menuitem",
                        disabled: !enabled,
                        onclick: {
                            let bot = bot.clone();
                            move |_| {
                                open.set(false);
                                session.write().new_bot_chat(bot.clone());
                            }
                        },
                        Icon { name: "plus", size: 15 }
                        span { "New chat" }
                    }
                }
            }
        }
    }
}
