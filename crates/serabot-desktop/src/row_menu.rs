//! What the "more actions" menu offers for a sidebar row or the open chat.
//!
//! The rows used to show four hover icons (switch off, delete, settings, pin)
//! that were easy to hit by mistake and hard to tell apart. They now live in one
//! menu, opened from a "more" button, a right click, or the chat header. The
//! choice of items is plain data so it can be tested without a window.

/// What kind of thing the menu is for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RowKind {
    /// A bot row (or a bot's one-to-one chat).
    Bot {
        enabled: bool,
        /// Whether the bot may be removed (the orchestrator may not).
        removable: bool,
        /// Whether setup is unfinished.
        needs_setup: bool,
    },
    /// A conversation (group, room or extra chat).
    Thread { archived: bool },
}

/// One menu entry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RowAction {
    FinishSetup,
    Settings,
    Pin,
    Unpin,
    SwitchOn,
    SwitchOff,
    DeleteBot,
    Rename,
    Archive,
    Unarchive,
}

impl RowAction {
    pub fn label(self) -> &'static str {
        match self {
            RowAction::FinishSetup => "Finish setting up",
            RowAction::Settings => "Settings",
            RowAction::Pin => "Pin",
            RowAction::Unpin => "Unpin",
            RowAction::SwitchOn => "Switch on",
            RowAction::SwitchOff => "Switch off",
            RowAction::DeleteBot => "Delete bot\u{2026}",
            RowAction::Rename => "Rename",
            RowAction::Archive => "Archive",
            RowAction::Unarchive => "Restore",
        }
    }

    /// The icon name from the console's icon set.
    pub fn icon(self) -> &'static str {
        match self {
            RowAction::FinishSetup => "sparkles",
            RowAction::Settings => "settings",
            RowAction::Pin | RowAction::Unpin => "star",
            RowAction::SwitchOn | RowAction::SwitchOff => "bolt",
            RowAction::DeleteBot | RowAction::Archive => "trash",
            RowAction::Rename => "edit",
            RowAction::Unarchive => "undo",
        }
    }

    /// Destructive entries are drawn apart and in the danger colour.
    pub fn is_danger(self) -> bool {
        matches!(self, RowAction::DeleteBot | RowAction::Archive)
    }

    /// Archiving keeps the chat and can be undone for a few seconds, so it
    /// does not ask. Deleting a bot opens its own confirm dialog.
    pub fn is_undoable(self) -> bool {
        matches!(self, RowAction::Archive)
    }
}

/// The entries for a row, safe ones first and destructive ones last.
pub fn actions(kind: &RowKind, pinned: bool) -> Vec<RowAction> {
    let pin = if pinned {
        RowAction::Unpin
    } else {
        RowAction::Pin
    };
    match kind {
        RowKind::Bot {
            enabled,
            removable,
            needs_setup,
        } => {
            let mut items = Vec::new();
            if *needs_setup {
                items.push(RowAction::FinishSetup);
            }
            items.push(RowAction::Settings);
            items.push(pin);
            if *removable {
                items.push(if *enabled {
                    RowAction::SwitchOff
                } else {
                    RowAction::SwitchOn
                });
                items.push(RowAction::DeleteBot);
            }
            items
        }
        RowKind::Thread { archived: true } => vec![RowAction::Unarchive, pin],
        RowKind::Thread { archived: false } => vec![RowAction::Rename, pin, RowAction::Archive],
    }
}

/// The header menu has no inline rename box, so it leaves Rename to the row.
pub fn header_actions(kind: &RowKind, pinned: bool) -> Vec<RowAction> {
    actions(kind, pinned)
        .into_iter()
        .filter(|action| *action != RowAction::Rename)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bot(enabled: bool, removable: bool, needs_setup: bool) -> RowKind {
        RowKind::Bot {
            enabled,
            removable,
            needs_setup,
        }
    }

    #[test]
    fn an_ordinary_bot_offers_settings_pin_switch_and_delete_last() {
        let items = actions(&bot(true, true, false), false);
        assert_eq!(
            items,
            vec![
                RowAction::Settings,
                RowAction::Pin,
                RowAction::SwitchOff,
                RowAction::DeleteBot
            ]
        );
        assert_eq!(items.last(), Some(&RowAction::DeleteBot));
    }

    #[test]
    fn the_orchestrator_cannot_be_switched_off_or_deleted() {
        let items = actions(&bot(true, false, false), true);
        assert_eq!(items, vec![RowAction::Settings, RowAction::Unpin]);
    }

    #[test]
    fn unfinished_setup_comes_first_and_off_bots_can_be_switched_on() {
        let items = actions(&bot(false, true, true), false);
        assert_eq!(items.first(), Some(&RowAction::FinishSetup));
        assert!(items.contains(&RowAction::SwitchOn));
        assert!(!items.contains(&RowAction::SwitchOff));
    }

    #[test]
    fn a_chat_can_be_renamed_pinned_and_archived() {
        let items = actions(&RowKind::Thread { archived: false }, false);
        assert_eq!(
            items,
            vec![RowAction::Rename, RowAction::Pin, RowAction::Archive]
        );
        assert_eq!(
            actions(&RowKind::Thread { archived: true }, false),
            vec![RowAction::Unarchive, RowAction::Pin]
        );
    }

    #[test]
    fn destructive_entries_are_marked_and_archive_is_undoable() {
        assert!(RowAction::Archive.is_danger());
        assert!(RowAction::DeleteBot.is_danger());
        assert!(!RowAction::Pin.is_danger());
        // Archive can be undone from a toast; deleting a bot is not undoable
        // and goes through its own confirm dialog.
        assert!(RowAction::Archive.is_undoable());
        assert!(!RowAction::DeleteBot.is_undoable());
    }

    #[test]
    fn the_header_menu_leaves_rename_to_the_row() {
        let items = header_actions(&RowKind::Thread { archived: false }, false);
        assert_eq!(items, vec![RowAction::Pin, RowAction::Archive]);
    }

    #[test]
    fn every_entry_has_a_label_and_an_icon() {
        for action in [
            RowAction::FinishSetup,
            RowAction::Settings,
            RowAction::Pin,
            RowAction::Unpin,
            RowAction::SwitchOn,
            RowAction::SwitchOff,
            RowAction::DeleteBot,
            RowAction::Rename,
            RowAction::Archive,
            RowAction::Unarchive,
        ] {
            assert!(!action.label().is_empty());
            assert!(!action.icon().is_empty());
        }
    }
}
