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

fn focus_chat_history_search() {
    document::eval(
        "requestAnimationFrame(() => { const el = document.getElementById('chat-history-search'); if (el) { el.focus(); el.select(); } })",
    );
}

fn scroll_selected_history_row() {
    document::eval(
        "requestAnimationFrame(() => { const el = document.querySelector('.chat-switcher-row.kb-selected'); if (el && el.scrollIntoView) el.scrollIntoView({ block: 'nearest' }); })",
    );
}

fn trim_snippet_parts<'a>(snippet: &'a str, query: &str) -> (&'a str, &'a str, &'a str) {
    let trimmed = snippet.trim();
    let q = query.trim();
    if q.is_empty() {
        return (trimmed, "", "");
    }
    let lower_s = trimmed.to_ascii_lowercase();
    let lower_q = q.to_ascii_lowercase();
    if let Some(pos) = lower_s.find(&lower_q) {
        let end = pos + lower_q.len();
        if trimmed.is_char_boundary(pos) && trimmed.is_char_boundary(end) {
            return (&trimmed[..pos], &trimmed[pos..end], &trimmed[end..]);
        }
    }
    (trimmed, "", "")
}

async fn run_conv_search(
    session: Signal<Session>,
    query: String,
    bot: String,
) -> Vec<serabot_desktop::ConversationSearchHit> {
    let Some(creds) = session.read().credentials.clone() else {
        return Vec::new();
    };
    let (tx, rx) = tokio::sync::oneshot::channel();
    if std::thread::Builder::new()
        .name("serabot-conv-search".into())
        .spawn(move || {
            let hits = serabot_desktop::ApiClient::new(creds.endpoint.clone(), creds.token.clone())
                .and_then(|c| c.search_conversations(&query, Some(&bot), 15))
                .unwrap_or_default();
            let _ = tx.send(hits);
        })
        .is_err()
    {
        return Vec::new();
    }
    rx.await.unwrap_or_default()
}

/// The chat switcher in the chat header: the open chat's name with a chevron
/// plus a clear History button. Opens a solid, searchable, date-grouped history
/// popover for switching chats, renaming/archiving, or starting a new chat.
#[component]
pub(crate) fn ChatSwitcher(session: Signal<Session>, bot: String, title: String) -> Element {
    let mut session = session;
    let mut open = use_context::<ChatHistoryOpen>().0;
    let mut query = use_signal(String::new);
    let mut selected_idx = use_signal(|| 0usize);
    let mut renaming = use_signal(|| Option::<(String, String)>::None);
    let mut search_hits = use_signal(Vec::<serabot_desktop::ConversationSearchHit>::new);
    let mut search_busy = use_signal(|| false);
    let mut search_seq = use_signal(|| 0u64);
    let mut prev_bot = use_signal(|| bot.clone());
    if *prev_bot.peek() != bot {
        prev_bot.set(bot.clone());
        query.set(String::new());
        selected_idx.set(0);
        renaming.set(None);
        search_hits.set(Vec::new());
        search_busy.set(false);
        let next_seq = search_seq().wrapping_add(1);
        search_seq.set(next_seq);
    }

    let trigger_search = {
        let bot = bot.clone();
        move |raw_q: String| {
            let next_seq = search_seq().wrapping_add(1);
            search_seq.set(next_seq);
            let q = raw_q.trim().to_owned();
            if q.chars().count() < 2 {
                search_hits.set(Vec::new());
                search_busy.set(false);
                return;
            }
            let bot = bot.clone();
            search_busy.set(true);
            spawn(async move {
                let hits = run_conv_search(session, q, bot).await;
                if *search_seq.peek() == next_seq {
                    search_hits.set(hits);
                    search_busy.set(false);
                }
            });
        }
    };

    let (entries, bot_name, enabled, active_conv, active_title, unread) = {
        let state = session.read();
        let entries = serabot_desktop::bot_chats::switcher_entries(
            state.state.bot_chats.get(&bot),
            &bot,
            &state.conversation,
        );
        let bot_name = state.bot_display_name(&bot);
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
            bot_name,
            enabled,
            state.conversation.clone(),
            state.conversation_title(),
            unread,
        )
    };

    let total_count = entries.len();
    let today = today_utc_date();
    let now_secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);

    // Ensure the active conversation carries its resolved title before filtering.
    let resolved_entries: Vec<serabot_desktop::bot_chats::SwitcherEntry> = entries
        .into_iter()
        .map(|mut entry| {
            if entry.id == active_conv
                && !active_title.is_empty()
                && entry.title.starts_with("New conversation")
            {
                entry.title = active_title.clone();
            }
            entry
        })
        .collect();

    let groups = serabot_desktop::bot_chats::filter_and_group_switcher_entries(
        resolved_entries,
        &query(),
        &today,
        now_secs,
    );

    let flat_ids: Vec<String> = groups
        .iter()
        .flat_map(|g| g.entries.iter().map(|e| e.id.clone()))
        .collect();
    let filtered_count = flat_ids.len();
    let current_sel = if filtered_count == 0 {
        0
    } else {
        selected_idx().min(filtered_count - 1)
    };
    let mut flat_cursor: usize = 0;

    let toggle_open = {
        let mut open = open;
        let mut query = query;
        let mut selected_idx = selected_idx;
        let mut renaming = renaming;
        move || {
            renaming.set(None);
            let next = !open();
            open.set(next);
            if next {
                query.set(String::new());
                selected_idx.set(0);
                search_hits.set(Vec::new());
                search_busy.set(false);
                focus_chat_history_search();
            }
        }
    };

    let query_str = query();
    let has_message_search = query_str.trim().chars().count() >= 2;
    let hits = search_hits();

    rsx! {
        div { class: "chat-switcher",
            div { class: "chat-switcher-triggers",
                button {
                    class: "chat-switcher-btn",
                    r#type: "button",
                    title: "{title} — Switch chat or browse history",
                    "aria-haspopup": "dialog",
                    "aria-expanded": if open() { "true" } else { "false" },
                    onclick: {
                        let mut toggle = toggle_open;
                        move |_| toggle()
                    },
                    strong { title: "{title}", "{title}" }
                    if total_count > 1 {
                        span { class: "chat-switcher-count", "{total_count}" }
                    }
                    Icon { name: if open() { "chevron-up" } else { "chevron-down" }, size: 14 }
                }
                button {
                    class: if open() { "btn ghost sm chat-history-btn active" } else { "btn ghost sm chat-history-btn" },
                    r#type: "button",
                    title: "Browse conversation history with {bot_name}",
                    "aria-label": "Conversation history",
                    "aria-haspopup": "dialog",
                    "aria-expanded": if open() { "true" } else { "false" },
                    onclick: {
                        let mut toggle = toggle_open;
                        move |_| toggle()
                    },
                    Icon { name: "history", size: 14 }
                    span { class: "chat-history-btn-label", "History" }
                    if total_count > 1 {
                        span { class: "chat-history-pill", "{total_count}" }
                    }
                }
            }
            if open() {
                div {
                    class: "row-menu-scrim chat-switcher-scrim",
                    onclick: move |_| {
                        renaming.set(None);
                        open.set(false);
                    },
                }
                div {
                    class: "row-menu chat-switcher-menu",
                    role: "dialog",
                    "aria-modal": "false",
                    tabindex: "-1",
                    "aria-label": "{bot_name} conversation history",
                    onmounted: move |_| {
                        focus_chat_history_search();
                    },
                    onkeydown: {
                        let flat_ids = flat_ids.clone();
                        move |event: KeyboardEvent| {
                            if renaming().is_some() {
                                return;
                            }
                            match event.key() {
                                Key::Escape => {
                                    event.prevent_default();
                                    event.stop_propagation();
                                    open.set(false);
                                }
                                Key::ArrowDown if !flat_ids.is_empty() => {
                                    event.prevent_default();
                                    event.stop_propagation();
                                    let next = (current_sel + 1) % flat_ids.len();
                                    selected_idx.set(next);
                                    scroll_selected_history_row();
                                }
                                Key::ArrowUp if !flat_ids.is_empty() => {
                                    event.prevent_default();
                                    event.stop_propagation();
                                    let next = if current_sel == 0 {
                                        flat_ids.len() - 1
                                    } else {
                                        current_sel - 1
                                    };
                                    selected_idx.set(next);
                                    scroll_selected_history_row();
                                }
                                Key::Enter => {
                                    if let Some(target_id) = flat_ids.get(current_sel).cloned() {
                                        event.prevent_default();
                                        event.stop_propagation();
                                        open.set(false);
                                        session.write().open_conversation(target_id);
                                    }
                                }
                                _ => {}
                            }
                        }
                    },
                    div { class: "chat-switcher-head",
                        div { class: "chat-switcher-head-title",
                            Icon { name: "history", size: 15 }
                            span { class: "chat-switcher-heading", "{bot_name} history" }
                            span { class: "chat-switcher-count", "{total_count}" }
                        }
                        button {
                            class: "icon-btn sm",
                            r#type: "button",
                            title: "Close (Esc)",
                            "aria-label": "Close conversation history",
                            onclick: move |_| {
                                renaming.set(None);
                                open.set(false);
                            },
                            Icon { name: "x", size: 14 }
                        }
                    }
                    div { class: "chat-switcher-search",
                        span { class: "chat-switcher-search-icon",
                            Icon { name: "search", size: 14 }
                        }
                        input {
                            id: "chat-history-search",
                            class: "chat-switcher-search-input",
                            r#type: "search",
                            placeholder: "Filter by title or search messages\u{2026}",
                            "aria-label": "Filter chats by title or search messages",
                            value: "{query}",
                            oninput: {
                                let mut trigger = trigger_search.clone();
                                move |event| {
                                    let val = event.value();
                                    query.set(val.clone());
                                    selected_idx.set(0);
                                    trigger(val);
                                }
                            },
                        }
                        if !query().trim().is_empty() {
                            button {
                                class: "icon-btn sm",
                                r#type: "button",
                                title: "Clear filter",
                                "aria-label": "Clear filter",
                                onclick: move |_| {
                                    query.set(String::new());
                                    selected_idx.set(0);
                                    search_hits.set(Vec::new());
                                    search_busy.set(false);
                                    focus_chat_history_search();
                                },
                                Icon { name: "x", size: 12 }
                            }
                        }
                    }
                    div { class: "chat-switcher-list", role: "listbox", "aria-label": "Conversations",
                        if groups.is_empty() && (!has_message_search || (!search_busy() && hits.is_empty())) {
                            div { class: "chat-switcher-empty",
                                p { class: "chat-switcher-empty-title", "No matching chats" }
                                p { class: "chat-switcher-empty-sub", "Try another keyword or clear the filter." }
                                button {
                                    class: "btn ghost sm",
                                    r#type: "button",
                                    onclick: move |_| {
                                        query.set(String::new());
                                        selected_idx.set(0);
                                        search_hits.set(Vec::new());
                                        search_busy.set(false);
                                        focus_chat_history_search();
                                    },
                                    "Clear filter"
                                }
                            }
                        }
                        for group in groups {
                            div {
                                key: "{group.label}",
                                class: "chat-switcher-group",
                                div { class: "chat-switcher-group-label", "{group.label}" }
                                for entry in group.entries {
                                    {
                                        let row_idx = flat_cursor;
                                        flat_cursor += 1;
                                        let is_kb_selected = row_idx == current_sel;
                                        let entry_id = entry.id.clone();
                                        let display_title = entry.title.clone();
                                        let editing = renaming()
                                            .as_ref()
                                            .is_some_and(|(edit_id, _)| edit_id == &entry_id);
                                        let turns_label = serabot_desktop::bot_chats::format_turn_count(entry.turns);
                                        let mut meta_parts = Vec::new();
                                        if entry.turns > 0 {
                                            meta_parts.push(turns_label);
                                        }
                                        if !entry.relative_time.is_empty() {
                                            meta_parts.push(entry.relative_time.clone());
                                        }
                                        let meta = meta_parts.join(" \u{00b7} ");
                                        let exact_stamp = entry
                                            .last_activity
                                            .as_deref()
                                            .map(stamp)
                                            .unwrap_or_default();
                                        let row_tooltip = if exact_stamp.is_empty() {
                                            display_title.clone()
                                        } else {
                                            format!("{display_title} \u{2014} {exact_stamp}")
                                        };
                                        let unread_n = unread.get(&entry_id).copied().unwrap_or(0);
                                        let badge = chat_feed::unread_badge(unread_n);
                                        let badge_label = chat_feed::unread_label(unread_n);
                                        let row_class = match (entry.active, is_kb_selected) {
                                            (true, true) => "chat-switcher-row active kb-selected",
                                            (true, false) => "chat-switcher-row active",
                                            (false, true) => "chat-switcher-row kb-selected",
                                            (false, false) => "chat-switcher-row",
                                        };
                                        rsx! {
                                            div {
                                                key: "{entry_id}",
                                                class: "{row_class}",
                                                role: "option",
                                                "aria-selected": if entry.active { "true" } else { "false" },
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
                                                        title: "{row_tooltip}",
                                                        onclick: {
                                                            let id = entry_id.clone();
                                                            move |_| {
                                                                open.set(false);
                                                                session.write().open_conversation(id.clone());
                                                            }
                                                        },
                                                        span { class: "chat-switcher-item-icon",
                                                            Icon { name: if entry.active { "check" } else { "chat" }, size: 15 }
                                                        }
                                                        div { class: "chat-switcher-copy",
                                                            div { class: "chat-switcher-title-line",
                                                                span { class: "chat-switcher-title", title: "{display_title}", "{display_title}" }
                                                                if entry.primary {
                                                                    span { class: "chat-switcher-badge", "Main" }
                                                                }
                                                            }
                                                            if !meta.is_empty() {
                                                                span { class: "chat-switcher-meta", title: "{exact_stamp}", "{meta}" }
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
                            }
                        }
                        if has_message_search && (search_busy() || !hits.is_empty()) {
                            div { class: "chat-switcher-group chat-switcher-msg-hits",
                                div { class: "chat-switcher-group-label",
                                    if search_busy() && hits.is_empty() {
                                        "Searching messages\u{2026}"
                                    } else {
                                        "Message matches"
                                    }
                                }
                                for (idx, hit) in hits.iter().enumerate() {
                                    {
                                        let conv_id = hit.conversation_id.clone();
                                        let hit_title = if hit.title.trim().is_empty() {
                                            "Chat".to_owned()
                                        } else {
                                            hit.title.clone()
                                        };
                                        let (pre, matched, post) = trim_snippet_parts(&hit.snippet, &query_str);
                                        let when = if hit.created_at.is_empty() {
                                            String::new()
                                        } else {
                                            stamp(&hit.created_at)
                                        };
                                        let role_label = if hit.role == "user" { "You" } else { &bot_name };
                                        rsx! {
                                            div {
                                                key: "hit-{idx}-{conv_id}",
                                                class: "chat-switcher-row",
                                                role: "option",
                                                "aria-selected": "false",
                                                button {
                                                    class: "row-menu-item chat-switcher-item chat-switcher-hit-item",
                                                    r#type: "button",
                                                    title: "{hit_title}",
                                                    onclick: move |_| {
                                                        open.set(false);
                                                        session.write().open_conversation(conv_id.clone());
                                                    },
                                                    span { class: "chat-switcher-item-icon",
                                                        Icon { name: "search", size: 14 }
                                                    }
                                                    div { class: "chat-switcher-copy",
                                                        div { class: "chat-switcher-title-line",
                                                            span { class: "chat-switcher-title", "{hit_title}" }
                                                            if !when.is_empty() {
                                                                span { class: "chat-switcher-badge", "{role_label} · {when}" }
                                                            }
                                                        }
                                                        if !hit.snippet.trim().is_empty() {
                                                            span { class: "chat-switcher-snippet",
                                                                "{pre}"
                                                                if !matched.is_empty() {
                                                                    mark { class: "chat-switcher-mark", "{matched}" }
                                                                }
                                                                "{post}"
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
                    div { class: "chat-switcher-foot",
                        button {
                            class: "btn primary sm chat-switcher-new",
                            r#type: "button",
                            disabled: !enabled,
                            onclick: {
                                let bot = bot.clone();
                                move |_| {
                                    open.set(false);
                                    session.write().new_bot_chat(bot.clone());
                                }
                            },
                            Icon { name: "plus", size: 14 }
                            span { "New chat" }
                        }
                        span { class: "chat-switcher-hint", "\u{2191}\u{2193} Navigate \u{00b7} Enter Open \u{00b7} Esc" }
                    }
                }
            }
        }
    }
}
