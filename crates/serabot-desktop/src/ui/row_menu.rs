use super::*;
use serabot_desktop::row_menu::{self, RowAction, RowKind};

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
            "aria-label": "Actions for {label}",
            onkeydown: move |event: KeyboardEvent| {
                if event.key() == Key::Escape {
                    event.stop_propagation();
                    menu.set(None);
                }
            },
            for action in items {
                button {
                    key: "{action.label()}",
                    class: if action.is_danger() { "row-menu-item danger" } else { "row-menu-item" },
                    role: "menuitem",
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
