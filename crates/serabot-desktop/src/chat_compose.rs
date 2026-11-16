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

/// Shown under an empty composer when Enter sends. Plain words, no key glyphs.
pub const COMPOSER_HINT: &str = "Enter sends, Shift+Enter adds a line";

/// Shown under an empty composer when Enter inserts a newline.
pub const COMPOSER_HINT_MOD_ENTER: &str = "Ctrl+Enter sends, Enter adds a line";

/// Hint text matching the owner's Enter-to-send setting.
pub fn composer_hint(enter_sends_pref: bool) -> &'static str {
    if enter_sends_pref {
        COMPOSER_HINT
    } else {
        COMPOSER_HINT_MOD_ENTER
    }
}

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
    Vault,
    Attach,
    Stop,
    Expand,
}

/// `(command, what it does, action)`, in the order the picker lists them.
pub const SLASH_ACTIONS: [(&str, &str, SlashAction); 6] = [
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
    (
        "vault",
        "Open Vault or use a safe vault:// reference",
        SlashAction::Vault,
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

/// Which group of slash-picker rows a flattened zero-based index points to:
/// built-in actions first, then bot skills, then attached tools.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SlashPick {
    Action(usize),
    Skill(usize),
    Tool(usize),
}

/// Map a flattened active index across `(actions, skills, tools)` to the exact
/// section and local index so highlighting, Arrow keys, Tab and Enter never drift.
pub fn slash_pick_at(
    index: usize,
    actions: usize,
    skills: usize,
    tools: usize,
) -> Option<SlashPick> {
    if index < actions {
        Some(SlashPick::Action(index))
    } else if index < actions + skills {
        Some(SlashPick::Skill(index - actions))
    } else if index < actions + skills + tools {
        Some(SlashPick::Tool(index - actions - skills))
    } else {
        None
    }
}

/// Format `body` as a markdown blockquote and append it cleanly to `existing`.
pub fn quote_reply_text(existing: &str, body: &str) -> String {
    let trimmed = body.trim();
    if trimmed.is_empty() {
        return existing.to_owned();
    }
    let quoted = trimmed
        .lines()
        .map(|line| {
            let line = line.trim_end();
            if line.is_empty() {
                ">".to_owned()
            } else {
                format!("> {line}")
            }
        })
        .collect::<Vec<_>>()
        .join("\n");
    let head = existing.trim_end();
    if head.is_empty() {
        format!("{quoted}\n\n")
    } else {
        format!("{head}\n\n{quoted}\n\n")
    }
}

/// Placeholder inserted in place of raw secrets when the user chooses to redact.
pub const REDACTED_PLACEHOLDER: &str = "[REDACTED \u{2014} store in Vault]";

/// Kind of sensitive credential detected in a composer draft.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SensitiveKind {
    OnePasswordToken,
    ApiKey,
    GitHubToken,
    SlackToken,
    GoogleApiKey,
    AwsAccessKey,
    PrivateKeyPem,
    BearerToken,
}

impl SensitiveKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::OnePasswordToken => "1Password service account token",
            Self::ApiKey => "API key",
            Self::GitHubToken => "GitHub token",
            Self::SlackToken => "Slack token",
            Self::GoogleApiKey => "Google API key",
            Self::AwsAccessKey => "AWS access key",
            Self::PrivateKeyPem => "PEM private key",
            Self::BearerToken => "Bearer token",
        }
    }
}

/// Byte span of a detected secret in the draft.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SensitiveSpan {
    pub kind: SensitiveKind,
    pub start: usize,
    pub end: usize,
}

/// All non-overlapping sensitive credential spans found in a composer draft.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SensitiveDraftMatch {
    pub spans: Vec<SensitiveSpan>,
}

impl SensitiveDraftMatch {
    /// Short human-readable summary of the detected credential kinds.
    pub fn summary(&self) -> String {
        let mut labels: Vec<&'static str> = Vec::new();
        for span in &self.spans {
            let label = span.kind.label();
            if !labels.contains(&label) {
                labels.push(label);
            }
        }
        labels.join(", ")
    }
}

fn is_token_char(b: u8) -> bool {
    b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.' | b'+' | b'=' | b'/' | b'~')
}

fn is_key_char(b: u8) -> bool {
    b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-')
}

fn is_boundary_before(bytes: &[u8], idx: usize) -> bool {
    if idx == 0 {
        return true;
    }
    let prev = bytes[idx - 1];
    !prev.is_ascii_alphanumeric() && prev != b'_' && prev != b'/' && prev != b':'
}

fn scan_while(bytes: &[u8], start: usize, pred: fn(u8) -> bool) -> usize {
    let mut i = start;
    while i < bytes.len() && pred(bytes[i]) {
        i += 1;
    }
    i
}

/// Safe vault/password-manager reference prefixes that must never trigger the guard.
fn safe_ref_end(text: &str, idx: usize) -> Option<usize> {
    let rest = &text[idx..];
    for prefix in ["op://", "pass://", "vault://"] {
        if rest.starts_with(prefix) {
            let end = text[idx..]
                .find(char::is_whitespace)
                .map_or(text.len(), |rel| idx + rel);
            return Some(end);
        }
    }
    None
}

/// Detect pasted raw secrets in a composer draft.
///
/// Safe references (`op://...`, `pass://...`, `vault://...`) and normal prose
/// or URLs do not trigger.
pub fn detect_sensitive_draft(text: &str) -> Option<SensitiveDraftMatch> {
    let bytes = text.as_bytes();
    let mut spans: Vec<SensitiveSpan> = Vec::new();
    let mut i = 0;

    while i < bytes.len() {
        // Never trigger inside safe vault/password-manager references.
        if is_boundary_before(bytes, i)
            && let Some(end) = safe_ref_end(text, i)
        {
            i = end;
            continue;
        }

        // PEM private keys: -----BEGIN ... PRIVATE KEY----- ... -----END ... PRIVATE KEY-----
        if text[i..].starts_with("-----BEGIN ")
            && let Some(nl_or_dash) = text[i..].find("PRIVATE KEY-----")
        {
            let header_end = i + nl_or_dash + "PRIVATE KEY-----".len();
            let end = if let Some(rel_end) = text[header_end..].find("-----END ")
                && let Some(rel_close) = text[header_end + rel_end..].find("PRIVATE KEY-----")
            {
                header_end + rel_end + rel_close + "PRIVATE KEY-----".len()
            } else {
                text.len()
            };
            spans.push(SensitiveSpan {
                kind: SensitiveKind::PrivateKeyPem,
                start: i,
                end,
            });
            i = end;
            continue;
        }

        if is_boundary_before(bytes, i) {
            let rest = &text[i..];

            // 1Password service account token: ops_eyJ...
            if rest.starts_with("ops_eyJ") {
                let end = scan_while(bytes, i + "ops_eyJ".len(), is_token_char);
                if end - i >= 16 {
                    spans.push(SensitiveSpan {
                        kind: SensitiveKind::OnePasswordToken,
                        start: i,
                        end,
                    });
                    i = end;
                    continue;
                }
            }

            // OpenAI / Anthropic keys: sk-ant-..., sk-proj-..., sk-...
            if rest.starts_with("sk-ant-")
                || rest.starts_with("sk-proj-")
                || rest.starts_with("sk-")
            {
                let end = scan_while(bytes, i, is_key_char);
                if end - i >= 20 {
                    spans.push(SensitiveSpan {
                        kind: SensitiveKind::ApiKey,
                        start: i,
                        end,
                    });
                    i = end;
                    continue;
                }
            }

            // GitHub tokens: ghp_..., github_pat_...
            if rest.starts_with("ghp_") || rest.starts_with("github_pat_") {
                let end = scan_while(bytes, i, is_key_char);
                if end - i >= 20 {
                    spans.push(SensitiveSpan {
                        kind: SensitiveKind::GitHubToken,
                        start: i,
                        end,
                    });
                    i = end;
                    continue;
                }
            }

            // Slack tokens: xoxb-..., xoxp-...
            if rest.starts_with("xoxb-") || rest.starts_with("xoxp-") {
                let end = scan_while(bytes, i, is_key_char);
                if end - i >= 16 {
                    spans.push(SensitiveSpan {
                        kind: SensitiveKind::SlackToken,
                        start: i,
                        end,
                    });
                    i = end;
                    continue;
                }
            }

            // Google API keys: AIza...
            if rest.starts_with("AIza") {
                let end = scan_while(bytes, i, is_key_char);
                if end - i >= 24 {
                    spans.push(SensitiveSpan {
                        kind: SensitiveKind::GoogleApiKey,
                        start: i,
                        end,
                    });
                    i = end;
                    continue;
                }
            }

            // AWS access key IDs: AKIA + 16 uppercase alphanumeric
            if rest.starts_with("AKIA") && bytes.len() >= i + 20 {
                let candidate = &bytes[i + 4..i + 20];
                let all_upper_alnum = candidate
                    .iter()
                    .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit());
                let next_ok = bytes
                    .get(i + 20)
                    .is_none_or(|b| !b.is_ascii_alphanumeric() && *b != b'_');
                if all_upper_alnum && next_ok {
                    spans.push(SensitiveSpan {
                        kind: SensitiveKind::AwsAccessKey,
                        start: i,
                        end: i + 20,
                    });
                    i += 20;
                    continue;
                }
            }

            // Long Bearer <token>
            if rest.len() > 7 && rest[..7].eq_ignore_ascii_case("bearer ") {
                let mut tok_start = i + 7;
                while tok_start < bytes.len() && bytes[tok_start] == b' ' {
                    tok_start += 1;
                }
                let tok_end = scan_while(bytes, tok_start, is_token_char);
                if tok_end - tok_start >= 20 {
                    spans.push(SensitiveSpan {
                        kind: SensitiveKind::BearerToken,
                        start: tok_start,
                        end: tok_end,
                    });
                    i = tok_end;
                    continue;
                }
            }
        }

        // Advance to next UTF-8 character boundary.
        let ch_len = text[i..].chars().next().map_or(1, char::len_utf8);
        i += ch_len;
    }

    if spans.is_empty() {
        None
    } else {
        Some(SensitiveDraftMatch { spans })
    }
}

/// Replace detected raw secrets with `[REDACTED — store in Vault]` while
/// keeping surrounding message text intact.
pub fn redact_sensitive_draft(text: &str) -> String {
    let Some(matched) = detect_sensitive_draft(text) else {
        return text.to_owned();
    };
    let mut out = String::with_capacity(text.len());
    let mut cursor = 0;
    for span in matched.spans {
        if span.start > cursor {
            out.push_str(&text[cursor..span.start]);
        }
        out.push_str(REDACTED_PLACEHOLDER);
        cursor = span.end;
    }
    if cursor < text.len() {
        out.push_str(&text[cursor..]);
    }
    out
}

/// Derive a concise task title from a chat message's first meaningful line,
/// stripping leading markdown markers and truncating cleanly to ~100 chars.
pub fn task_title_from_message(content: &str) -> String {
    let first_line = content
        .lines()
        .map(str::trim)
        .map(|line| {
            line.trim_start_matches('#')
                .trim_start_matches('>')
                .trim_start_matches("- ")
                .trim_start_matches("* ")
                .trim()
        })
        .find(|line| !line.is_empty())
        .unwrap_or("Follow up on chat message");

    const MAX_LEN: usize = 100;
    if first_line.chars().count() <= MAX_LEN {
        first_line.to_owned()
    } else {
        let mut truncated: String = first_line.chars().take(MAX_LEN - 1).collect();
        if let Some(space) = truncated.rfind(' ')
            && space >= MAX_LEN / 2
        {
            truncated.truncate(space);
        }
        truncated.push('\u{2026}');
        truncated
    }
}

// ---- Heavy work: the send queue, delegate mentions and the key help (v0.50) ----

/// Most messages one conversation's queue holds.
pub const MAX_QUEUED: usize = 8;

/// A message waiting for the running reply to end.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct QueuedMessage {
    pub id: u64,
    pub text: String,
    /// The bot an `@bot` mention routes this message to.
    pub target: Option<String>,
    /// "Steer": the running reply is stopped so this goes next.
    pub steer: bool,
}

/// Why a message did not enter the queue.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum QueueRefusal {
    Empty,
    TooLong,
    Full,
}

impl QueueRefusal {
    pub fn message(self) -> &'static str {
        match self {
            Self::Empty => "Type a message first.",
            Self::TooLong => "That message is too long to queue. Shorten it first.",
            Self::Full => "The queue is full. Wait for a reply or delete one.",
        }
    }
}

/// Messages typed while a reply runs. They go out one at a time, in order,
/// when the conversation is idle. While one is being edited the queue holds:
/// the front message is never sent from under the owner's cursor.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SendQueue {
    items: Vec<QueuedMessage>,
    next_id: u64,
    editing: Option<u64>,
}

impl SendQueue {
    pub fn items(&self) -> &[QueuedMessage] {
        &self.items
    }

    pub fn len(&self) -> usize {
        self.items.len()
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// The message being edited, if any (the edit hold).
    pub fn editing(&self) -> Option<u64> {
        self.editing
    }

    fn check(text: &str) -> Result<String, QueueRefusal> {
        let trimmed = text.trim();
        if trimmed.is_empty() {
            Err(QueueRefusal::Empty)
        } else if trimmed.chars().count() > MAX_TEXT_CHARS {
            Err(QueueRefusal::TooLong)
        } else {
            Ok(trimmed.to_owned())
        }
    }

    fn position(&self, id: u64) -> Option<usize> {
        self.items.iter().position(|item| item.id == id)
    }

    pub fn push(&mut self, text: &str, target: Option<String>) -> Result<u64, QueueRefusal> {
        let text = Self::check(text)?;
        if self.items.len() >= MAX_QUEUED {
            return Err(QueueRefusal::Full);
        }
        self.next_id = self.next_id.wrapping_add(1);
        let id = self.next_id;
        self.items.push(QueuedMessage {
            id,
            text,
            target,
            steer: false,
        });
        Ok(id)
    }

    pub fn remove(&mut self, id: u64) -> bool {
        let Some(index) = self.position(id) else {
            return false;
        };
        self.items.remove(index);
        if self.editing == Some(id) {
            self.editing = None;
        }
        true
    }

    pub fn move_up(&mut self, id: u64) -> bool {
        match self.position(id) {
            Some(index) if index > 0 => {
                self.items.swap(index, index - 1);
                true
            }
            _ => false,
        }
    }

    pub fn move_down(&mut self, id: u64) -> bool {
        match self.position(id) {
            Some(index) if index + 1 < self.items.len() => {
                self.items.swap(index, index + 1);
                true
            }
            _ => false,
        }
    }

    /// Start editing: the text to put in the field. The queue holds until
    /// the edit is saved or cancelled.
    pub fn begin_edit(&mut self, id: u64) -> Option<String> {
        let index = self.position(id)?;
        self.editing = Some(id);
        Some(self.items[index].text.clone())
    }

    pub fn commit_edit(&mut self, id: u64, text: &str) -> Result<(), QueueRefusal> {
        let text = Self::check(text)?;
        if let Some(index) = self.position(id) {
            self.items[index].text = text;
        }
        if self.editing == Some(id) {
            self.editing = None;
        }
        Ok(())
    }

    pub fn cancel_edit(&mut self) {
        self.editing = None;
    }

    /// Steer: send this one next and stop the running reply to make room.
    /// A message being edited cannot be steered.
    pub fn steer(&mut self, id: u64) -> bool {
        if self.editing == Some(id) {
            return false;
        }
        let Some(index) = self.position(id) else {
            return false;
        };
        let mut item = self.items.remove(index);
        item.steer = true;
        self.items.insert(0, item);
        true
    }

    /// Whether the running reply should be stopped for a steered message.
    pub fn wants_interrupt(&self) -> bool {
        self.items
            .first()
            .is_some_and(|item| item.steer && self.editing != Some(item.id))
    }

    /// The next message to send now: none while a reply runs, and none while
    /// the front message is held by an edit.
    pub fn take_next(&mut self, run_active: bool) -> Option<QueuedMessage> {
        let front = self.items.first()?;
        if run_active || self.editing == Some(front.id) {
            return None;
        }
        Some(self.items.remove(0))
    }
}

/// Queues by conversation id, kept while the app runs like the drafts.
#[derive(Debug, Default)]
pub struct Queues {
    by_conversation: HashMap<String, SendQueue>,
}

impl Queues {
    /// Put a conversation's queue away (empty queues are not kept).
    pub fn stash(&mut self, conversation: &str, queue: SendQueue) {
        if queue.is_empty() {
            self.by_conversation.remove(conversation);
        } else {
            self.by_conversation.insert(conversation.to_owned(), queue);
        }
    }

    /// Take a conversation's queue back.
    pub fn restore(&mut self, conversation: &str) -> SendQueue {
        self.by_conversation
            .remove(conversation)
            .unwrap_or_default()
    }

    /// Conversations with something waiting (not the open one).
    pub fn waiting(&self) -> Vec<&str> {
        self.by_conversation.keys().map(String::as_str).collect()
    }
}

/// A message after delegate routing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Routed {
    /// The bot that answers, when a leading mention picked one.
    pub target: Option<String>,
    pub text: String,
}

/// `@analyst summarize this` goes to `analyst` as `summarize this`. Only a
/// leading mention of a known bot other than the current one routes; in a
/// room the server reads mentions itself, so the text goes through as typed.
pub fn route_mention(text: &str, bots: &[String], current: Option<&str>, room: bool) -> Routed {
    let plain = Routed {
        target: None,
        text: text.to_owned(),
    };
    if room {
        return plain;
    }
    let trimmed = text.trim_start();
    let Some(rest) = trimmed.strip_prefix('@') else {
        return plain;
    };
    let token_end = rest.find(char::is_whitespace).unwrap_or(rest.len());
    let (token, body) = rest.split_at(token_end);
    let body = body.trim();
    if body.is_empty()
        || !crate::valid_speaker(token)
        || Some(token) == current
        || !bots.iter().any(|bot| bot == token)
    {
        return plain;
    }
    Routed {
        target: Some(token.to_owned()),
        text: body.to_owned(),
    }
}

/// One row of the shortcuts sheet: group, keys, what it does.
pub type ShortcutRow = (&'static str, &'static str, &'static str);

/// The keys the chat answers to, matching the owner's Enter setting.
pub fn shortcut_rows(enter_sends_pref: bool) -> Vec<ShortcutRow> {
    let (send, newline) = if enter_sends_pref {
        ("Enter", "Shift+Enter")
    } else {
        ("Ctrl+Enter", "Enter")
    };
    vec![
        ("Writing", send, "Send the message"),
        ("Writing", newline, "Add a line"),
        (
            "Writing",
            send,
            "While a reply is running: add the message to the queue",
        ),
        ("Writing", "Up", "Bring back your last message (empty box)"),
        ("Writing", "/", "Commands, skills and tools"),
        ("Writing", "@", "Mention a bot: it answers that message"),
        ("Runs", "Esc", "Stop the reply that is running"),
        ("Runs", "Ctrl+/", "Back to the message box"),
        ("Chat", "Ctrl+F", "Search this conversation"),
        ("Chat", "Ctrl+End", "Jump to the latest message"),
        ("Chat", "F1", "Show this list"),
    ]
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
            ".msg-action-time {",
            ".msg.bot .msg-body {\n  max-width: 100%;",
            ".chat-switcher-btn {",
            ".thread-group.new-chat-row {",
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

    #[test]
    fn composer_hint_reflects_enter_preference() {
        assert_eq!(composer_hint(true), COMPOSER_HINT);
        assert_eq!(composer_hint(false), COMPOSER_HINT_MOD_ENTER);
    }

    #[test]
    fn slash_pick_at_maps_actions_skills_and_tools_in_order() {
        assert_eq!(slash_pick_at(0, 2, 3, 1), Some(SlashPick::Action(0)));
        assert_eq!(slash_pick_at(1, 2, 3, 1), Some(SlashPick::Action(1)));
        assert_eq!(slash_pick_at(2, 2, 3, 1), Some(SlashPick::Skill(0)));
        assert_eq!(slash_pick_at(4, 2, 3, 1), Some(SlashPick::Skill(2)));
        assert_eq!(slash_pick_at(5, 2, 3, 1), Some(SlashPick::Tool(0)));
        assert_eq!(slash_pick_at(6, 2, 3, 1), None);
    }

    #[test]
    fn quote_reply_formats_multiline_quotes_and_preserves_existing_draft() {
        assert_eq!(
            quote_reply_text("", "line 1\n\nline 2"),
            "> line 1\n>\n> line 2\n\n"
        );
        assert_eq!(quote_reply_text("Draft", "quoted"), "Draft\n\n> quoted\n\n");
        assert_eq!(quote_reply_text("Draft", "   "), "Draft");
    }

    #[test]
    fn vault_slash_command_is_registered_and_parsed() {
        assert_eq!(parse_slash_action("/vault"), Some(SlashAction::Vault));
        assert_eq!(parse_slash_action(" /VAULT "), Some(SlashAction::Vault));
        assert!(
            slash_actions("vault")
                .iter()
                .any(|(cmd, _, act)| *cmd == "vault" && *act == SlashAction::Vault)
        );
    }

    #[test]
    fn sensitive_draft_guard_detects_raw_tokens() {
        let cases = [
            (
                "Use ops_eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9.abc123",
                SensitiveKind::OnePasswordToken,
            ),
            ("key: sk-1234567890abcdef123456", SensitiveKind::ApiKey),
            (
                "anthropic sk-ant-api03-abcdef1234567890xyz",
                SensitiveKind::ApiKey,
            ),
            ("openai sk-proj-abcdef1234567890xyz", SensitiveKind::ApiKey),
            (
                "token ghp_1234567890abcdef1234567890abcdef",
                SensitiveKind::GitHubToken,
            ),
            (
                "pat github_pat_11AAAAAA0123456789abcdef",
                SensitiveKind::GitHubToken,
            ),
            (
                "slack xoxb-123456789012-123456789012-abcdef",
                SensitiveKind::SlackToken,
            ),
            (
                "slack user xoxp-123456789012-123456789012-abcdef",
                SensitiveKind::SlackToken,
            ),
            (
                "google AIzaSyDaGmWKa4JsXZ-HjGw7ISLn_3namBGt",
                SensitiveKind::GoogleApiKey,
            ),
            ("aws AKIAIOSFODNN7EXAMPLE", SensitiveKind::AwsAccessKey),
            (
                "-----BEGIN RSA PRIVATE KEY-----\nMIIEowIBAAKCAQEA\n-----END RSA PRIVATE KEY-----",
                SensitiveKind::PrivateKeyPem,
            ),
            (
                "Authorization: Bearer eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9.payload",
                SensitiveKind::BearerToken,
            ),
        ];
        for (sample, expected_kind) in cases {
            let matched = detect_sensitive_draft(sample)
                .unwrap_or_else(|| panic!("expected match for: {sample}"));
            assert_eq!(matched.spans[0].kind, expected_kind, "for {sample}");
        }
    }

    #[test]
    fn sensitive_draft_guard_ignores_safe_references_and_prose() {
        for safe in [
            "Use op://Private/GitHub/token for this",
            "Read pass://Personal/API/key please",
            "Fetch vault://openai_api_key from my vault",
            "Check https://example.com/docs/api-keys and https://github.com/org/repo",
            "Normal prose with sk-short or Bearer short or AKIA123",
            "Let's discuss the API key rotation schedule tomorrow.",
        ] {
            assert_eq!(
                detect_sensitive_draft(safe),
                None,
                "false positive on: {safe}"
            );
            assert_eq!(redact_sensitive_draft(safe), safe);
        }
    }

    #[test]
    fn redact_sensitive_draft_replaces_tokens_and_preserves_surrounding_text() {
        let draft = "Please use sk-proj-abcdef1234567890xyz and op://Work/Key/val for testing.";
        let redacted = redact_sensitive_draft(draft);
        assert_eq!(
            redacted,
            "Please use [REDACTED \u{2014} store in Vault] and op://Work/Key/val for testing."
        );
    }

    #[test]
    fn task_title_from_message_cleans_markdown_and_truncates() {
        assert_eq!(
            task_title_from_message("\n## Review Q3 invoice\nDetails follow."),
            "Review Q3 invoice"
        );
        assert_eq!(
            task_title_from_message("- Send contract to legal"),
            "Send contract to legal"
        );
        let long = "Please review the attached quarterly financial report and prepare an executive summary for Friday's board meeting";
        let title = task_title_from_message(long);
        assert!(title.chars().count() <= 100);
        assert!(title.ends_with('\u{2026}'));
    }

    fn queue_of(texts: &[&str]) -> (SendQueue, Vec<u64>) {
        let mut queue = SendQueue::default();
        let ids = texts
            .iter()
            .map(|text| queue.push(text, None).unwrap())
            .collect();
        (queue, ids)
    }

    fn texts(queue: &SendQueue) -> Vec<&str> {
        queue.items().iter().map(|m| m.text.as_str()).collect()
    }

    #[test]
    fn the_queue_refuses_blank_long_and_too_many() {
        let mut queue = SendQueue::default();
        assert_eq!(queue.push("  ", None), Err(QueueRefusal::Empty));
        assert_eq!(
            queue.push(&"x".repeat(MAX_TEXT_CHARS + 1), None),
            Err(QueueRefusal::TooLong)
        );
        for i in 0..MAX_QUEUED {
            queue.push(&format!("m{i}"), None).unwrap();
        }
        assert_eq!(queue.push("one more", None), Err(QueueRefusal::Full));
        assert_eq!(queue.len(), MAX_QUEUED);
    }

    #[test]
    fn queued_messages_go_out_in_order_and_only_when_idle() {
        let (mut queue, _) = queue_of(&["first", "second"]);
        assert!(queue.take_next(true).is_none(), "a reply is running");
        assert_eq!(queue.take_next(false).unwrap().text, "first");
        assert_eq!(queue.take_next(false).unwrap().text, "second");
        assert!(queue.take_next(false).is_none());
    }

    #[test]
    fn reorder_moves_one_step_and_stops_at_the_ends() {
        let (mut queue, ids) = queue_of(&["a", "b", "c"]);
        assert!(!queue.move_up(ids[0]));
        assert!(queue.move_up(ids[2]));
        assert_eq!(texts(&queue), ["a", "c", "b"]);
        assert!(queue.move_down(ids[0]));
        assert_eq!(texts(&queue), ["c", "a", "b"]);
        assert!(!queue.move_down(ids[1]));
        assert!(!queue.move_up(999));
    }

    #[test]
    fn an_edit_holds_the_front_message_until_it_is_saved() {
        let (mut queue, ids) = queue_of(&["draft one", "draft two"]);
        assert_eq!(queue.begin_edit(ids[0]).as_deref(), Some("draft one"));
        assert_eq!(queue.editing(), Some(ids[0]));
        assert!(queue.take_next(false).is_none(), "held while editing");
        // A blank save is refused and the hold stays.
        assert_eq!(queue.commit_edit(ids[0], " "), Err(QueueRefusal::Empty));
        assert_eq!(queue.editing(), Some(ids[0]));
        queue.commit_edit(ids[0], "  better  ").unwrap();
        assert_eq!(queue.editing(), None);
        assert_eq!(queue.take_next(false).unwrap().text, "better");
    }

    #[test]
    fn editing_a_later_message_does_not_hold_the_front() {
        let (mut queue, ids) = queue_of(&["one", "two"]);
        queue.begin_edit(ids[1]);
        assert_eq!(queue.take_next(false).unwrap().text, "one");
        // Cancelling lets the rest go; deleting the edited message does too.
        queue.cancel_edit();
        assert_eq!(queue.editing(), None);
        queue.begin_edit(ids[1]);
        assert!(queue.remove(ids[1]));
        assert_eq!(queue.editing(), None);
        assert!(queue.is_empty());
    }

    #[test]
    fn steer_moves_a_message_to_the_front_and_asks_to_interrupt() {
        let (mut queue, ids) = queue_of(&["a", "b", "c"]);
        assert!(!queue.wants_interrupt());
        assert!(queue.steer(ids[2]));
        assert_eq!(texts(&queue), ["c", "a", "b"]);
        assert!(queue.wants_interrupt());
        // Still held while a reply runs; sent once it is stopped.
        assert!(queue.take_next(true).is_none());
        let sent = queue.take_next(false).unwrap();
        assert!(sent.steer);
        assert!(!queue.wants_interrupt());
        // A message under edit cannot be steered.
        queue.begin_edit(ids[1]);
        assert!(!queue.steer(ids[1]));
    }

    #[test]
    fn queues_follow_their_conversation() {
        let mut queues = Queues::default();
        let (queue, _) = queue_of(&["later"]);
        queues.stash("dir:main", queue);
        queues.stash("dir:other", SendQueue::default());
        assert_eq!(queues.waiting(), vec!["dir:main"]);
        assert!(queues.restore("dir:other").is_empty());
        assert_eq!(queues.restore("dir:main").len(), 1);
        assert!(queues.restore("dir:main").is_empty(), "taken once");
    }

    #[test]
    fn a_leading_mention_routes_to_a_known_delegate() {
        let bots = vec!["analyst".to_owned(), "maker".to_owned(), "main".to_owned()];
        let routed = route_mention("@analyst  summarize the notes", &bots, Some("main"), false);
        assert_eq!(routed.target.as_deref(), Some("analyst"));
        assert_eq!(routed.text, "summarize the notes");
        // Unknown bot, the current bot, a bare mention, a mid-text mention and
        // a look-alike address all stay as typed.
        for text in [
            "@ghost hello",
            "@main hello",
            "@analyst",
            "hello @analyst",
            "@Analyst hello",
            "@analyst.evil hello",
        ] {
            let routed = route_mention(text, &bots, Some("main"), false);
            assert_eq!(routed.target, None, "{text}");
            assert_eq!(routed.text, text);
        }
        // In a room the server parses mentions itself.
        let room = route_mention("@analyst hello", &bots, Some("main"), true);
        assert_eq!(room.target, None);
        assert_eq!(room.text, "@analyst hello");
    }

    #[test]
    fn the_shortcut_sheet_follows_the_enter_setting() {
        let enter = shortcut_rows(true);
        assert!(
            enter
                .iter()
                .any(|r| r.1 == "Shift+Enter" && r.2 == "Add a line")
        );
        let ctrl = shortcut_rows(false);
        assert!(
            ctrl.iter()
                .any(|r| r.1 == "Ctrl+Enter" && r.2 == "Send the message")
        );
        assert!(ctrl.iter().any(|r| r.1 == "Esc"));
        assert!(ctrl.iter().any(|r| r.1 == "F1"));
    }
}
