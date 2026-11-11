use super::*;
use serabot_desktop::row_menu::{self, RowAction, RowKind};

/// Carries out a menu entry on the session.
fn run_row_action(
    action: RowAction,
    session: Signal<Session>,
    toast: Signal<Option<(String, bool)>>,
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
            session.write().archive_conversation(id.to_owned());
            toast.set(Some(("Archived.".into(), false)));
        }
        RowAction::Unarchive => {
            session.write().unarchive_conversation(id.to_owned());
            toast.set(Some(("Restored.".into(), false)));
        }
    }
}

/// The "more actions" menu for one row or for the open chat. Opened from the
/// row's more button, a right click, or the chat header; closes on a click
/// outside, on Escape, or after an action. Archive asks first inside the menu.
#[component]
pub(crate) fn RowMenu(
    session: Signal<Session>,
    toast: Signal<Option<(String, bool)>>,
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
    let mut confirming = use_signal(|| false);
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
            if confirming() {
                div { class: "row-menu-confirm", role: "group", "aria-label": "Confirm archive",
                    p { class: "caption", "Archive \u{201c}{label}\u{201d}? You can restore it later." }
                    div { class: "row-menu-confirm-actions",
                        button {
                            class: "btn ghost sm",
                            autofocus: true,
                            onclick: move |event| {
                                event.stop_propagation();
                                confirming.set(false);
                                menu.set(None);
                            },
                            "Cancel"
                        }
                        button {
                            class: "btn danger sm",
                            onclick: {
                                let id = id.clone();
                                let pin_id = pin_id.clone();
                                let label = label.clone();
                                move |event| {
                                    event.stop_propagation();
                                    run_row_action(RowAction::Archive, session, toast, renaming, &id, &pin_id, &label);
                                    confirming.set(false);
                                    menu.set(None);
                                }
                            },
                            "Archive"
                        }
                    }
                }
            } else {
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
                                if action.confirms_in_menu() {
                                    confirming.set(true);
                                    return;
                                }
                                run_row_action(action, session, toast, renaming, &id, &pin_id, &label);
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
}
