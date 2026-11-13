//! Composer rules for the chat: which dropped, pasted or picked files a draft
//! admits, the per-conversation text drafts, send readiness, the Enter key
//! policy and the length counter. Pure functions, so the rules are tested
//! without a window.

use std::collections::HashMap;

use crate::{MAX_ATTACHMENT_BYTES, MAX_ATTACHMENTS, valid_attachment_name};

/// Total bytes one message may carry across its files (the 25 MB ceiling).
pub const MAX_MESSAGE_BYTES: u64 = MAX_ATTACHMENT_BYTES as u64;

/// The server's limit on message text, in characters.
pub const MAX_TEXT_CHARS: usize = 8000;

/// Shown under an empty composer. Plain words, no key glyphs.
pub const COMPOSER_HINT: &str = "Enter sends, Shift+Enter adds a line";

/// Images at most this big get an inline thumbnail. Larger pictures show the
/// file icon, so the bytes never double in memory for a preview.
pub const THUMBNAIL_MAX_BYTES: usize = 2 * 1024 * 1024;

/// Files a draft already holds: uploaded or still queued.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Held {
    pub count: usize,
    pub bytes: u64,
}

/// Which incoming files (by index) were admitted, and one line per refusal.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Admission {
    pub accepted: Vec<usize>,
    pub refused: Vec<String>,
}

/// Admit files against the draft's limits, in order: name, size, count and
/// the per-message total. A refused file never blocks the ones after it.
pub fn admit_files(held: Held, incoming: &[(&str, u64)]) -> Admission {
    let mut admission = Admission::default();
    let mut count = held.count;
    let mut bytes = held.bytes;
    for (index, (name, size)) in incoming.iter().enumerate() {
        if !valid_attachment_name(name) {
            admission
                .refused
                .push(format!("{name}: this file type cannot be attached."));
        } else if *size == 0 || *size > MAX_ATTACHMENT_BYTES as u64 {
            admission
                .refused
                .push(format!("{name}: files must be between 1 byte and 25 MB."));
        } else if count >= MAX_ATTACHMENTS {
            admission.refused.push(format!(
                "{name}: a message holds at most {MAX_ATTACHMENTS} files. Remove one to add it."
            ));
        } else if bytes + size > MAX_MESSAGE_BYTES {
            admission.refused.push(format!(
                "{name}: a message can carry 25 MB in total. Remove a file to add this one."
            ));
        } else {
            count += 1;
            bytes += size;
            admission.accepted.push(index);
        }
    }
    admission
}

/// Text drafts keyed by conversation id. Memory only: a draft lasts for the
/// running app and follows the conversation it was typed in. Files are never
/// kept here; they belong to the upload that produced them.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Drafts {
    texts: HashMap<String, String>,
}

impl Drafts {
    /// Keep the text typed in a conversation. Blank text removes the entry.
    pub fn store(&mut self, conversation: &str, text: &str) {
        if text.trim().is_empty() {
            self.texts.remove(conversation);
        } else {
            self.texts.insert(conversation.to_owned(), text.to_owned());
        }
    }

    /// The draft for a conversation, or an empty string.
    pub fn restore(&self, conversation: &str) -> String {
        self.texts.get(conversation).cloned().unwrap_or_default()
    }

    /// Forget a conversation's draft (called when its message is sent).
    pub fn clear(&mut self, conversation: &str) {
        self.texts.remove(conversation);
    }
}

/// The picture type of an image file name, or `None` for other files.
pub fn image_mime(name: &str) -> Option<&'static str> {
    let lower = name.to_ascii_lowercase();
    if lower.ends_with(".png") {
        Some("image/png")
    } else if lower.ends_with(".jpg") || lower.ends_with(".jpeg") {
        Some("image/jpeg")
    } else if lower.ends_with(".gif") {
        Some("image/gif")
    } else if lower.ends_with(".webp") {
        Some("image/webp")
    } else {
        None
    }
}

/// A `data:` URL preview for a small picture, for the chip thumbnail. Large
/// pictures and non-images get no preview and show the file icon instead.
pub fn thumbnail_data_url(name: &str, bytes: &[u8]) -> Option<String> {
    let mime = image_mime(name)?;
    if bytes.is_empty() || bytes.len() > THUMBNAIL_MAX_BYTES {
        return None;
    }
    use base64::Engine;
    Some(format!(
        "data:{mime};base64,{}",
        base64::engine::general_purpose::STANDARD.encode(bytes)
    ))
}

/// Whether a session composer value should replace the on-screen draft during
/// streaming repaints. While the textarea is focused and both sides have text,
/// the local draft wins. An empty local draft still takes a restored message,
/// so a failed send puts the text back even if the box stayed focused.
pub fn sync_composer_from_session(
    composer_focused: bool,
    session_composer_empty: bool,
    local_empty: bool,
) -> bool {
    !composer_focused || session_composer_empty || local_empty
}

/// True when the draft can go out: text or files, under the length limit,
/// and no upload still in flight.
pub fn can_submit(text: &str, files: usize, busy: bool) -> bool {
    let chars = text.chars().count();
    if busy || chars > MAX_TEXT_CHARS {
        return false;
    }
    files > 0 || !text.trim().is_empty()
}

/// How close the draft is to the server's limit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LengthState {
    /// Under 90 percent of the limit: no counter.
    Quiet,
    /// Between 90 percent and the limit: show the counter.
    Near,
    /// Past the limit: the counter explains what to do.
    Over,
}

pub fn length_state(chars: usize) -> LengthState {
    if chars > MAX_TEXT_CHARS {
        LengthState::Over
    } else if chars * 10 >= MAX_TEXT_CHARS * 9 {
        LengthState::Near
    } else {
        LengthState::Quiet
    }
}

/// `7812` -> `7,812`.
pub fn group_thousands(value: usize) -> String {
    let digits = value.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (index, ch) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index).is_multiple_of(3) {
            out.push(',');
        }
        out.push(ch);
    }
    out
}

/// The counter line, or the fix when the draft is over the limit.
pub fn length_message(chars: usize) -> Option<String> {
    match length_state(chars) {
        LengthState::Quiet => None,
        LengthState::Near => Some(format!(
            "{} of {} characters",
            group_thousands(chars),
            group_thousands(MAX_TEXT_CHARS)
        )),
        LengthState::Over => Some(format!(
            "{} characters over the limit. Shorten the message or attach it as a file.",
            group_thousands(chars - MAX_TEXT_CHARS)
        )),
    }
}

/// Whether a key press sends. Ctrl or Cmd with Enter always sends. Plain
/// Enter sends unless the owner turned that off; then Shift+Enter sends and
/// plain Enter adds a line.
pub fn enter_sends(enter_sends_pref: bool, shift: bool, command: bool) -> bool {
    command || (enter_sends_pref && !shift)
}

/// Whether an Enter key press should send. An Enter that confirms an input
/// method (IME) candidate must never send: it belongs to the composition.
pub fn submit_on_enter(
    enter_sends_pref: bool,
    shift: bool,
    command: bool,
    composing: bool,
) -> bool {
    !composing && enter_sends(enter_sends_pref, shift, command)
}

/// Whether Esc in the composer should stop the running reply: only while a
/// reply is running and no approval is pending (that has its own Decline).
pub fn escape_stops_run(run_active: bool, awaiting_approval: bool) -> bool {
    run_active && !awaiting_approval
}

/// A slash command that does something instead of inserting text.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SlashAction {
    NewChat,
    Memory,
    Attach,
    Stop,
    Expand,
}

/// `(command, what it does, action)`, in the order the picker lists them.
pub const SLASH_ACTIONS: [(&str, &str, SlashAction); 5] = [
    (
        "new",
        "Start a new chat with this bot",
        SlashAction::NewChat,
    ),
    (
        "memory",
        "Open the bots\u{2019} memory",
        SlashAction::Memory,
    ),
    ("attach", "Attach files", SlashAction::Attach),
    ("stop", "Stop the reply", SlashAction::Stop),
    (
        "expand",
        "Make the message box bigger or smaller",
        SlashAction::Expand,
    ),
];

/// The actions whose command contains `query` (case-insensitive).
pub fn slash_actions(query: &str) -> Vec<(&'static str, &'static str, SlashAction)> {
    let needle = query.trim().to_lowercase();
    SLASH_ACTIONS
        .iter()
        .copied()
        .filter(|(command, description, _)| {
            needle.is_empty()
                || command.contains(&needle)
                || description.to_lowercase().contains(&needle)
        })
        .collect()
}

/// The action a draft stands for: exactly `/command` and nothing else, so a
/// message that merely starts with a slash is still sent as text.
pub fn parse_slash_action(draft: &str) -> Option<SlashAction> {
    let command = draft.trim().strip_prefix('/')?;
    SLASH_ACTIONS
        .iter()
        .find(|(name, _, _)| *name == command.to_lowercase())
        .map(|(_, _, action)| *action)
}

/// The text Up recalls into an empty composer: the owner's latest message
/// that is not blank. `texts` runs oldest to newest.
pub fn recall_text(texts: &[&str]) -> Option<String> {
    texts
        .iter()
        .rev()
        .find(|text| !text.trim().is_empty())
        .map(|text| (*text).to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn admits_within_limits_and_refuses_the_rest_with_a_reason() {
        let files = [
            ("notes.txt", 10),
            ("run.exe", 10),
            ("empty.pdf", 0),
            ("huge.pdf", MAX_ATTACHMENT_BYTES as u64 + 1),
            ("ok.png", 20),
        ];
        let admission = admit_files(
            Held::default(),
            &files.iter().map(|(n, s)| (*n, *s)).collect::<Vec<_>>(),
        );
        assert_eq!(admission.accepted, vec![0, 4]);
        assert_eq!(admission.refused.len(), 3);
        assert!(admission.refused[0].starts_with("run.exe:"));
        assert!(admission.refused[1].contains("between 1 byte and 25 MB"));
    }

    #[test]
    fn count_limit_counts_files_already_held() {
        let incoming = [("a.txt", 1), ("b.txt", 1), ("c.txt", 1)];
        let admission = admit_files(Held { count: 5, bytes: 5 }, &incoming);
        assert_eq!(admission.accepted, vec![0]);
        assert_eq!(admission.refused.len(), 2);
        assert!(admission.refused[0].contains("at most 6 files"));
    }

    #[test]
    fn message_total_is_capped_at_25_mb() {
        let half = (MAX_MESSAGE_BYTES / 2) + 1;
        let incoming = [("one.pdf", half), ("two.pdf", half)];
        let admission = admit_files(Held::default(), &incoming);
        assert_eq!(admission.accepted, vec![0]);
        assert!(admission.refused[0].contains("25 MB in total"));
        // Held bytes count toward the same total.
        let held = admit_files(
            Held {
                count: 1,
                bytes: MAX_MESSAGE_BYTES - 10,
            },
            &[("late.txt", 11)],
        );
        assert!(held.accepted.is_empty());
    }

    #[test]
    fn drafts_are_per_conversation_and_blank_text_is_dropped() {
        let mut drafts = Drafts::default();
        drafts.store("dir:main", "draft one");
        drafts.store("room:team", "draft two");
        assert_eq!(drafts.restore("dir:main"), "draft one");
        assert_eq!(drafts.restore("room:team"), "draft two");
        assert_eq!(drafts.restore("dir:other"), "");
        drafts.store("dir:main", "   ");
        assert_eq!(drafts.restore("dir:main"), "");
        drafts.store("room:team", "next");
        drafts.clear("room:team");
        assert_eq!(drafts.restore("room:team"), "");
    }

    #[test]
    fn submit_needs_text_or_files_and_no_upload_in_flight() {
        assert!(!can_submit("   ", 0, false));
        assert!(can_submit("   ", 1, false));
        assert!(can_submit("hi", 0, false));
        assert!(!can_submit("hi", 0, true));
        let long = "a".repeat(MAX_TEXT_CHARS + 1);
        assert!(!can_submit(&long, 1, false));
    }

    #[test]
    fn length_counter_appears_near_the_limit_and_explains_overflow() {
        assert_eq!(length_message(100), None);
        assert_eq!(length_message(7199), None);
        assert_eq!(
            length_message(7200).as_deref(),
            Some("7,200 of 8,000 characters")
        );
        assert!(
            length_message(8005)
                .unwrap()
                .starts_with("5 characters over the limit")
        );
        assert_eq!(length_state(8000), LengthState::Near);
    }

    #[test]
    fn thousands_grouping() {
        assert_eq!(group_thousands(0), "0");
        assert_eq!(group_thousands(999), "999");
        assert_eq!(group_thousands(1000), "1,000");
        assert_eq!(group_thousands(1234567), "1,234,567");
    }

    #[test]
    fn enter_policy_always_sends_on_command_enter() {
        assert!(enter_sends(true, false, false));
        assert!(!enter_sends(true, true, false));
        assert!(enter_sends(true, true, true));
        assert!(!enter_sends(false, false, false));
        assert!(enter_sends(false, false, true));
    }

    #[test]
    fn chat_view_layout_rules_stay_in_place() {
        // Guards PR-N: the composer sits in the flow (so it can never cover the
        // last messages or the jump button), the jump button hangs above it,
        // replies are plain text, and chat switching has its own styles.
        let css = include_str!("../assets/foundation.css");
        for needle in [
            ".chat-column > .composer-wrap {\n  position: relative;",
            ".composer-wrap > .scroll-bottom {\n  position: absolute;",
            "bottom: 100%;",
            ".msg.last-reply .msg-actions {",
            ".msg.bot .msg-body {\n  max-width: 100%;",
            ".chat-switcher-btn {",
            ".thread-group.new-chat-row {",
            ".integration-tabs {",
        ] {
            assert!(css.contains(needle), "missing chat rule: {needle}");
        }
    }

    #[test]
    fn composer_layout_rules_stay_in_place() {
        // Guards the PR-I fix: a column container, a full-width borderless text
        // area (no focus outline of its own), a toolbar row, and the hint kept
        // out of the container.
        let css = include_str!("../assets/foundation.css");
        for needle in [
            ".composer-wrap .composer {\n  display: flex;\n  flex-direction: column;",
            ".composer-wrap .composer .composer-input:focus-visible",
            "outline: none;",
            ".composer-wrap .composer .composer-row {",
            "margin-left: auto;",
            ".composer-wrap .composer-hint {",
            "max-height: 40vh;",
            "max-height: 70vh;",
            "min-height: 72px;",
        ] {
            assert!(css.contains(needle), "missing composer rule: {needle}");
        }
    }

    #[test]
    fn slash_actions_match_by_command_or_description() {
        assert_eq!(slash_actions("").len(), SLASH_ACTIONS.len());
        assert_eq!(slash_actions("att")[0].2, SlashAction::Attach);
        assert_eq!(slash_actions("FILES")[0].2, SlashAction::Attach);
        assert!(slash_actions("zzz").is_empty());
    }

    #[test]
    fn only_an_exact_command_is_an_action() {
        assert_eq!(parse_slash_action("/attach"), Some(SlashAction::Attach));
        assert_eq!(parse_slash_action("/new"), Some(SlashAction::NewChat));
        assert_eq!(parse_slash_action("/Memory"), Some(SlashAction::Memory));
        assert_eq!(parse_slash_action("/new chat please"), None);
        assert_eq!(parse_slash_action("  /Stop "), Some(SlashAction::Stop));
        assert_eq!(parse_slash_action("/stop now please"), None);
        assert_eq!(parse_slash_action("new"), None);
        assert_eq!(parse_slash_action("/unknown"), None);
        assert_eq!(parse_slash_action(""), None);
    }

    #[test]
    fn enter_during_ime_composition_never_sends() {
        assert!(submit_on_enter(true, false, false, false));
        assert!(!submit_on_enter(true, false, false, true));
        assert!(!submit_on_enter(false, false, true, true));
        assert!(submit_on_enter(false, false, true, false));
    }

    #[test]
    fn escape_stops_only_a_running_reply() {
        assert!(escape_stops_run(true, false));
        assert!(!escape_stops_run(false, false));
        assert!(!escape_stops_run(true, true));
    }

    #[test]
    fn thumbnails_only_for_small_pictures() {
        let url = thumbnail_data_url("shot.PNG", &[0x89, 0x50, 0x4E, 0x47]).unwrap();
        assert_eq!(url, "data:image/png;base64,iVBORw==");
        assert_eq!(thumbnail_data_url("notes.txt", b"hi"), None);
        assert_eq!(thumbnail_data_url("empty.jpg", b""), None);
        let big = vec![0u8; THUMBNAIL_MAX_BYTES + 1];
        assert_eq!(thumbnail_data_url("big.jpg", &big), None);
        assert_eq!(image_mime("a.webp"), Some("image/webp"));
    }

    #[test]
    fn composer_draft_keeps_local_text_while_focused() {
        assert!(!sync_composer_from_session(true, false, false));
        assert!(sync_composer_from_session(true, false, true));
        assert!(sync_composer_from_session(true, true, false));
        assert!(sync_composer_from_session(false, false, false));
        assert!(sync_composer_from_session(false, true, false));
    }

    #[test]
    fn up_recalls_the_latest_non_blank_user_text() {
        assert_eq!(
            recall_text(&["first", "second", " "]),
            Some("second".into())
        );
        assert_eq!(recall_text(&["   "]), None);
        assert_eq!(recall_text(&[]), None);
    }
}
