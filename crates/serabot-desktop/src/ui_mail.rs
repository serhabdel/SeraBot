//! Dedicated 3-pane Desktop Mail workspace (0.74.12, upgraded 0.74.13).
//!
//! Security invariant: all message headers and plain bodies are untrusted data
//! rendered exclusively as Dioxus text nodes. HTML bodies and previews are
//! rendered exclusively inside `<iframe sandbox="" srcdoc="...">` (no scripts,
//! no forms, no same-origin access).

use std::collections::BTreeSet;
use std::time::{SystemTime, UNIX_EPOCH};

use base64::Engine as _;
use dioxus::prelude::*;
use serabot_desktop::bot_library::LibraryItem;
use serabot_desktop::mail_view::{
    DayBucket, MailKey, Selection, day_bucket, full_date_label, group_threads, map_key,
    parse_mail_date, sender_name,
};
use serabot_desktop::session::{Section, Session};
use serabot_desktop::{
    MailAccountStatus, MailDraftRequest, MailMessageInfo, MailSnoozeItem, MailStatus,
    SavedMailSearch, ScheduledMailItem,
};
use serde::Deserialize;

use crate::ui::Icon;
use crate::ui_files::{PastedFile, decode_pasted};
use crate::ui_mail_actions::{
    FAILED, SEND_HOLD_SECS, UndoEntry, UndoKind, announce, archive_messages, delete_messages,
    queue_draft_delete, restore_failed, run_undo, set_flags, settle, sleep_ms,
};
use crate::ui_mail_compose::{
    AttachedFile, AttachmentTray, DroppedFile, MAIL_DROP_JS, RecipientField, attach, detach,
    invalid_recipients, pick_local_files, recipient_addresses, upload_attachments,
};
use crate::ui_mail_list::{
    BulkBar, DayHeader, MailRow, RowSkeletons, ShortcutsDialog, SnoozeDialog, ThreadInfo, UndoBar,
};
use crate::ui_mail_reader::MailReader;

const DEFAULT_MAIL_LIMIT: u32 = 50;
const SCHEDULED_VIRTUAL_FOLDER: &str = "__scheduled__";
const SNOOZED_VIRTUAL_FOLDER: &str = "__snoozed__";

/// Filter tabs for the middle message list.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum MailFilter {
    #[default]
    All,
    Unread,
    Starred,
}

/// Mode for the Compose / Reply / Forward modal.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ComposeMode {
    New,
    Reply,
    Forward,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ComposeState {
    mode: ComposeMode,
    to: String,
    cc: String,
    bcc: String,
    subject: String,
    body: String,
    selected_hashes: BTreeSet<String>,
}

impl ComposeState {
    pub(crate) fn new_empty() -> Self {
        Self {
            mode: ComposeMode::New,
            to: String::new(),
            cc: String::new(),
            bcc: String::new(),
            subject: String::new(),
            body: String::new(),
            selected_hashes: BTreeSet::new(),
        }
    }

    pub(crate) fn reply_to(msg: &MailMessageInfo) -> Self {
        Self {
            mode: ComposeMode::Reply,
            to: extract_email_addr(&msg.from),
            cc: String::new(),
            bcc: String::new(),
            subject: format_reply_subject(&msg.subject),
            body: format_reply_quote(msg),
            selected_hashes: BTreeSet::new(),
        }
    }

    /// Reply to the sender and keep everyone else on Cc, leaving out the sender, the
    /// mailbox's own address and duplicates.
    pub(crate) fn reply_all(msg: &MailMessageInfo, own: &str) -> Self {
        let mut base = Self::reply_to(msg);
        let own = own.trim().to_ascii_lowercase();
        let sender = extract_email_addr(&msg.from).to_ascii_lowercase();
        let mut cc: Vec<String> = Vec::new();
        for raw in [&msg.to, &msg.cc] {
            for entry in parse_recipients(raw) {
                let addr = extract_email_addr(&entry);
                let lower = addr.to_ascii_lowercase();
                if lower.is_empty() || lower == own || lower == sender {
                    continue;
                }
                if !cc.iter().any(|c| c.eq_ignore_ascii_case(&addr)) {
                    cc.push(addr);
                }
            }
        }
        base.cc = cc.join(", ");
        base
    }

    pub(crate) fn forward_of(msg: &MailMessageInfo) -> Self {
        Self {
            mode: ComposeMode::Forward,
            to: String::new(),
            cc: String::new(),
            bcc: String::new(),
            subject: format_forward_subject(&msg.subject),
            body: format_forward_quote(msg),
            selected_hashes: BTreeSet::new(),
        }
    }
}

fn extract_email_addr(raw: &str) -> String {
    let trimmed = raw.trim();
    if let (Some(start), Some(end)) = (trimmed.rfind('<'), trimmed.rfind('>'))
        && start < end
    {
        return trimmed[start + 1..end].trim().to_owned();
    }
    trimmed.to_owned()
}

/// Wrap untrusted email HTML in a clean light-mode document for `<iframe sandbox="" srcdoc="...">`.
pub fn wrap_email_html_srcdoc(body_html: &str) -> String {
    wrap_email_html_srcdoc_with_remote(body_html, false)
}

pub fn wrap_email_html_srcdoc_with_remote(body_html: &str, allow_remote_images: bool) -> String {
    let img_src = if allow_remote_images {
        "data: https: http:"
    } else {
        "data:"
    };
    format!(
        "<!doctype html><html><head><meta charset=\"utf-8\">\
         <meta http-equiv=\"Content-Security-Policy\" content=\"default-src 'none'; style-src 'unsafe-inline'; img-src {img_src};\">\
         <style>\
         :root{{color-scheme:light;}}\
         html,body{{margin:0;padding:16px;background:#ffffff;color:#1a1d21;\
         font-family:-apple-system,BlinkMacSystemFont,'Segoe UI',Roboto,sans-serif;\
         font-size:14px;line-height:1.55;word-break:break-word;}}\
         img{{max-width:100%;height:auto;}}\
         pre,code{{white-space:pre-wrap;}}\
         a{{color:#1d4ed8;}}\
         blockquote{{margin:8px 0;padding-left:12px;border-left:3px solid #d1d5db;color:#4b5563;}}\
         </style></head><body>{body_html}</body></html>"
    )
}

fn escape_html_text(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    for ch in raw.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            _ => out.push(ch),
        }
    }
    out
}

/// Strip simple HTML tags to produce a clean plain-text fallback.
pub fn strip_html_tags(html: &str) -> String {
    let normalized = html
        .replace("<br>", "\n")
        .replace("<br/>", "\n")
        .replace("<br />", "\n")
        .replace("</p>", "\n")
        .replace("</div>", "\n")
        .replace("</li>", "\n")
        .replace("</blockquote>", "\n");
    let mut out = String::with_capacity(normalized.len());
    let mut in_tag = false;
    for ch in normalized.chars() {
        match ch {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => out.push(ch),
            _ => {}
        }
    }
    out.replace("&nbsp;", " ")
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .trim()
        .to_owned()
}

fn has_explicit_html_tags(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    const TAG_PREFIXES: &[&str] = &[
        "<b>",
        "<i>",
        "<u>",
        "<p>",
        "<p ",
        "<br>",
        "<br/>",
        "<br />",
        "<ul>",
        "<ol>",
        "<li>",
        "<a ",
        "<blockquote>",
        "<div>",
        "<div ",
        "<span>",
        "<span ",
        "<h1>",
        "<h2>",
        "<h3>",
    ];
    TAG_PREFIXES.iter().any(|pat| lower.contains(pat))
}

/// Convert composer text into `(plain_body, optional_body_html)` based on `html_mode`.
pub fn compose_to_body_and_html(raw: &str, html_mode: bool) -> (String, Option<String>) {
    if !html_mode {
        return (raw.to_owned(), None);
    }
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return (String::new(), None);
    }
    let has_tags = has_explicit_html_tags(trimmed);
    let inner_html = if has_tags {
        trimmed.replace('\n', "<br>\n")
    } else {
        escape_html_text(trimmed).replace('\n', "<br>\n")
    };
    let html = format!(
        "<div style=\"font-family:-apple-system,BlinkMacSystemFont,'Segoe UI',Roboto,sans-serif;font-size:14px;line-height:1.55;\">{inner_html}</div>"
    );
    let plain = if has_tags {
        strip_html_tags(trimmed)
    } else {
        raw.to_owned()
    };
    (plain, Some(html))
}

fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = (yoe as i64) + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = y + if m <= 2 { 1 } else { 0 };
    (year, m as u32, d as u32)
}

fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = y - if m <= 2 { 1 } else { 0 };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = (y - era * 400) as u64;
    let m_adj = if m > 2 { m - 3 } else { m + 9 } as u64;
    let doy = (153 * m_adj + 2) / 5 + (d as u64) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + (doe as i64) - 719_468
}

/// Format a Unix timestamp (seconds) as `YYYY-MM-DD HH:MM UTC`.
pub fn format_unix_rfc3339(secs: u64) -> String {
    let days = (secs / 86_400) as i64;
    let rem = secs % 86_400;
    let hour = rem / 3600;
    let min = (rem % 3600) / 60;
    let sec = rem % 60;
    let (year, month, day) = civil_from_days(days);
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{min:02}:{sec:02}Z")
}

fn now_unix_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

pub fn schedule_preset_in_one_hour(now_secs: u64) -> i64 {
    now_secs.saturating_add(3600) as i64
}

pub fn schedule_preset_tomorrow_9am(now_secs: u64) -> i64 {
    let tomorrow_days = (now_secs / 86_400).saturating_add(1);
    (tomorrow_days * 86_400 + 9 * 3600) as i64
}

pub fn schedule_preset_next_monday_9am(now_secs: u64) -> i64 {
    let days = (now_secs / 86_400) as i64;
    // 1970-01-01 was Thursday (4); 0=Sun, 1=Mon, ..., 6=Sat.
    let dow = (days + 4).rem_euclid(7);
    let delta = match (8 - dow).rem_euclid(7) {
        0 => 7,
        d => d,
    };
    let target_days = days + delta;
    target_days * 86_400 + 9 * 3600
}

/// Parse a `datetime-local` input (`YYYY-MM-DDTHH:MM` or `YYYY-MM-DDTHH:MM:SS`) into a Unix timestamp (UTC).
pub fn normalize_custom_datetime(raw: &str) -> Option<i64> {
    let trimmed = raw.trim().trim_end_matches('Z');
    let (date_part, time_part) = trimmed.split_once('T')?;
    let mut d_parts = date_part.split('-');
    let year: i64 = d_parts.next()?.parse().ok()?;
    let month: u32 = d_parts.next()?.parse().ok()?;
    let day: u32 = d_parts.next()?.parse().ok()?;
    if d_parts.next().is_some() || !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }
    let mut t_parts = time_part.split(':');
    let hour: i64 = t_parts.next()?.parse().ok()?;
    let min: i64 = t_parts.next()?.parse().ok()?;
    let sec: i64 = t_parts
        .next()
        .map(|s| s.parse::<i64>())
        .transpose()
        .ok()?
        .unwrap_or(0);
    if !(0..=23).contains(&hour) || !(0..=59).contains(&min) || !(0..=59).contains(&sec) {
        return None;
    }
    let days = days_from_civil(year, month, day);
    Some(days * 86_400 + hour * 3600 + min * 60 + sec)
}

pub fn is_image_library_item(item: &LibraryItem) -> bool {
    if item
        .mime
        .as_deref()
        .is_some_and(|m| m.to_ascii_lowercase().starts_with("image/"))
    {
        return true;
    }
    let name = item
        .filename
        .as_deref()
        .or(item.name.as_deref())
        .unwrap_or_default()
        .to_ascii_lowercase();
    matches!(
        name.rsplit('.').next(),
        Some("png" | "jpg" | "jpeg" | "gif" | "webp" | "svg")
    )
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct SignatureFields {
    name: String,
    title: String,
    company: String,
    phone: String,
    website: String,
}

fn build_signature_html_and_plain(
    fields: &SignatureFields,
    logo_src: Option<&str>,
) -> (String, String) {
    let mut plain_lines = Vec::new();
    if !fields.name.trim().is_empty() {
        plain_lines.push(fields.name.trim().to_owned());
    }
    let role_line = [fields.title.trim(), fields.company.trim()]
        .into_iter()
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join(" · ");
    if !role_line.is_empty() {
        plain_lines.push(role_line.clone());
    }
    if !fields.phone.trim().is_empty() {
        plain_lines.push(fields.phone.trim().to_owned());
    }
    if !fields.website.trim().is_empty() {
        plain_lines.push(fields.website.trim().to_owned());
    }
    let plain = plain_lines.join("\n");

    let mut html = String::from(
        "<table cellpadding=\"0\" cellspacing=\"0\" style=\"font-family:-apple-system,BlinkMacSystemFont,'Segoe UI',Roboto,sans-serif;font-size:13px;line-height:1.45;color:#1a1d21;border-top:1px solid #e5e7eb;padding-top:10px;margin-top:12px;\"><tr>",
    );
    if let Some(src) = logo_src.filter(|s| !s.trim().is_empty()) {
        let safe_src = escape_html_text(src.trim());
        html.push_str(&format!(
            "<td style=\"padding-right:12px;vertical-align:top;\"><img src=\"{safe_src}\" alt=\"Logo\" style=\"max-height:56px;max-width:140px;border-radius:4px;display:block;\"></td>"
        ));
    }
    html.push_str("<td style=\"vertical-align:top;\">");
    if !fields.name.trim().is_empty() {
        html.push_str(&format!(
            "<div style=\"font-weight:600;font-size:14px;\">{}</div>",
            escape_html_text(fields.name.trim())
        ));
    }
    if !role_line.is_empty() {
        html.push_str(&format!(
            "<div style=\"color:#4b5563;\">{}</div>",
            escape_html_text(&role_line)
        ));
    }
    if !fields.phone.trim().is_empty() {
        html.push_str(&format!(
            "<div style=\"color:#4b5563;\">{}</div>",
            escape_html_text(fields.phone.trim())
        ));
    }
    if !fields.website.trim().is_empty() {
        let site = fields.website.trim();
        let safe_site = escape_html_text(site);
        let href = if site.starts_with("http://") || site.starts_with("https://") {
            safe_site.clone()
        } else {
            format!("https://{safe_site}")
        };
        html.push_str(&format!(
            "<div><a href=\"{href}\" style=\"color:#1d4ed8;text-decoration:none;\">{safe_site}</a></div>"
        ));
    }
    html.push_str("</td></tr></table>");
    (plain, html)
}

/// Run one blocking mail API call on a dedicated background thread.
pub(crate) async fn run_mail_job<T, F>(session: Signal<Session>, job: F) -> Result<T, String>
where
    T: Send + 'static,
    F: FnOnce(&serabot_desktop::ApiClient) -> Result<T, serabot_desktop::ApiError> + Send + 'static,
{
    let Some(creds) = session.read().credentials.clone() else {
        return Err("Not signed in.".to_owned());
    };
    let (tx, rx) = tokio::sync::oneshot::channel();
    if let Err(err) = std::thread::Builder::new()
        .name("serabot-mail-ws".into())
        .spawn(move || {
            let res = serabot_desktop::ApiClient::new(creds.endpoint.clone(), creds.token.clone())
                .and_then(|client| job(&client))
                .map_err(|e| e.to_string());
            let _ = tx.send(res);
        })
    {
        return Err(format!("Could not start request thread: {err}"));
    }
    rx.await
        .map_err(|_| "Request thread exited unexpectedly.".to_owned())?
}

/// Map a folder name to an icon name (from `crate::ui::Icon`) and display label.
pub fn folder_icon_and_label(folder: &str) -> (&'static str, String) {
    let trimmed = folder.trim();
    let tail = trimmed.rsplit(['/', '.']).next().unwrap_or(trimmed).trim();
    let lower = tail.to_ascii_lowercase();
    match lower.as_str() {
        "inbox" => ("folder", "Inbox".to_owned()),
        "sent" | "sent items" | "sent messages" => ("send", "Sent".to_owned()),
        "drafts" | "draft" => ("edit", "Drafts".to_owned()),
        "archive" | "all mail" => ("library", "Archive".to_owned()),
        "trash" | "deleted" | "deleted items" | "bin" => ("trash", "Trash".to_owned()),
        "spam" | "junk" | "junk e-mail" | "junk email" => ("alert", "Spam".to_owned()),
        _ => (
            "folder",
            if trimmed.is_empty() {
                "Folder".to_owned()
            } else {
                trimmed.to_owned()
            },
        ),
    }
}

/// Whether a message lacks the `\Seen` IMAP flag.
pub fn is_unread(msg: &MailMessageInfo) -> bool {
    !msg.flags
        .iter()
        .any(|f| f.eq_ignore_ascii_case("\\Seen") || f.eq_ignore_ascii_case("seen"))
}

/// Whether a message carries the `\Flagged` IMAP flag.
pub fn is_starred(msg: &MailMessageInfo) -> bool {
    msg.flags
        .iter()
        .any(|f| f.eq_ignore_ascii_case("\\Flagged") || f.eq_ignore_ascii_case("flagged"))
}

/// Filter a slice of messages by `All`, `Unread`, or `Starred`.
pub fn filter_messages(messages: &[MailMessageInfo], filter: MailFilter) -> Vec<MailMessageInfo> {
    messages
        .iter()
        .filter(|msg| match filter {
            MailFilter::All => true,
            MailFilter::Unread => is_unread(msg),
            MailFilter::Starred => is_starred(msg),
        })
        .cloned()
        .collect()
}

/// Format a reply subject without duplicating `Re:`.
pub fn format_reply_subject(subject: &str) -> String {
    let trimmed = subject.trim();
    if trimmed.len() >= 3 && trimmed[..3].eq_ignore_ascii_case("re:") {
        trimmed.to_owned()
    } else if trimmed.is_empty() {
        "Re:".to_owned()
    } else {
        format!("Re: {trimmed}")
    }
}

/// Format a forward subject without duplicating `Fwd:`.
pub fn format_forward_subject(subject: &str) -> String {
    let trimmed = subject.trim();
    let lower = trimmed.to_ascii_lowercase();
    if lower.starts_with("fwd:") || lower.starts_with("fw:") {
        trimmed.to_owned()
    } else if trimmed.is_empty() {
        "Fwd:".to_owned()
    } else {
        format!("Fwd: {trimmed}")
    }
}

/// Format a quoted reply body.
pub fn format_reply_quote(msg: &MailMessageInfo) -> String {
    let mut out = String::from("\n\n");
    out.push_str(&format!("On {}, {} wrote:\n", msg.date, msg.from));
    for line in msg.body.lines() {
        out.push_str("> ");
        out.push_str(line);
        out.push('\n');
    }
    out
}

/// Format a forwarded message body.
pub fn format_forward_quote(msg: &MailMessageInfo) -> String {
    let mut out = String::from("\n\n---------- Forwarded message ----------\n");
    out.push_str(&format!("From: {}\n", msg.from));
    if !msg.to.trim().is_empty() {
        out.push_str(&format!("To: {}\n", msg.to));
    }
    out.push_str(&format!("Date: {}\n", msg.date));
    out.push_str(&format!("Subject: {}\n\n", msg.subject));
    out.push_str(&msg.body);
    out
}

/// Build the pre-filled chat prompt for **"Ask bot"**.
pub fn format_ask_bot_prompt(account: Option<&str>, folder: &str, msg: &MailMessageInfo) -> String {
    let acct = account
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("default");
    format!(
        "Help me with this email (account: {acct}, folder: {folder}, UID: {}):\nFrom: {}\nSubject: {}\n\n",
        msg.uid, msg.from, msg.subject
    )
}

/// Split comma- or semicolon-separated email addresses into a trimmed list.
pub fn parse_recipients(raw: &str) -> Vec<String> {
    raw.split([',', ';'])
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .collect()
}

/// Human-readable byte size for attachments.
pub fn format_bytes(size: usize) -> String {
    if size < 1024 {
        format!("{size} B")
    } else if size < 1024 * 1024 {
        format!("{:.1} KB", size as f64 / 1024.0)
    } else {
        format!("{:.1} MB", size as f64 / (1024.0 * 1024.0))
    }
}

/// Set or clear one flag on a message. The server reports flags without the
/// leading backslash (`Seen`), the UI used to write them with it (`\Seen`); both
/// spellings count as the same flag.
pub(crate) fn toggle_flag_local(msg: &mut MailMessageInfo, flag: &str, on: bool) {
    let bare = flag.trim_start_matches('\\');
    msg.flags
        .retain(|f| !f.trim_start_matches('\\').eq_ignore_ascii_case(bare));
    if on {
        msg.flags.push(bare.to_owned());
    }
}

pub(crate) fn update_message_flags_in_list(
    messages: &mut [MailMessageInfo],
    uid: u32,
    flag: &str,
    on: bool,
) {
    if let Some(item) = messages.iter_mut().find(|m| m.uid == uid) {
        toggle_flag_local(item, flag, on);
    }
}

#[cfg(windows)]
pub(crate) fn save_bytes_with_picker(
    default_name: &str,
    bytes: &[u8],
) -> Result<Option<String>, String> {
    let Some(path) = rfd::FileDialog::new()
        .set_title("Save attachment")
        .set_file_name(default_name)
        .save_file()
    else {
        return Ok(None);
    };
    std::fs::write(&path, bytes).map_err(|e| format!("Could not write file: {e}"))?;
    Ok(Some(path.display().to_string()))
}

#[cfg(not(windows))]
pub(crate) fn save_bytes_with_picker(
    _default_name: &str,
    _bytes: &[u8],
) -> Result<Option<String>, String> {
    Err("Save to disk needs the Windows app.".to_owned())
}

#[allow(clippy::too_many_arguments)]
fn load_folder_and_messages(
    session: Signal<Session>,
    account: Option<String>,
    folder: String,
    search_query: String,
    mut folders: Signal<Vec<String>>,
    mut messages: Signal<Vec<MailMessageInfo>>,
    mut selected_msg: Signal<Option<MailMessageInfo>>,
    mut loading_list: Signal<bool>,
    mut error: Signal<Option<String>>,
) {
    loading_list.set(true);
    error.set(None);
    selected_msg.set(None);
    spawn(async move {
        let acct_for_folders = account.clone();
        if let Ok(list) = run_mail_job(session, move |c| {
            c.mail_folders(acct_for_folders.as_deref())
        })
        .await
        {
            let mut normalized = list;
            if !normalized.iter().any(|f| f.eq_ignore_ascii_case("INBOX")) {
                normalized.insert(0, "INBOX".to_owned());
            }
            folders.set(normalized);
        }
        let q = search_query.trim().to_owned();
        let res = run_mail_job(session, move |c| {
            if q.is_empty() {
                c.mail_messages(account.as_deref(), &folder, DEFAULT_MAIL_LIMIT)
            } else {
                c.mail_search(account.as_deref(), &folder, &q, DEFAULT_MAIL_LIMIT)
            }
        })
        .await;
        loading_list.set(false);
        match res {
            Ok(list) => messages.set(list),
            Err(msg) => error.set(Some(msg)),
        }
    });
}

#[allow(clippy::too_many_arguments)]
fn load_scheduled_messages(
    session: Signal<Session>,
    account: Option<String>,
    mut scheduled_items: Signal<Vec<ScheduledMailItem>>,
    mut selected_scheduled: Signal<Option<ScheduledMailItem>>,
    mut loading_list: Signal<bool>,
    mut error: Signal<Option<String>>,
) {
    loading_list.set(true);
    error.set(None);
    selected_scheduled.set(None);
    spawn(async move {
        let res = run_mail_job(session, move |c| c.mail_list_scheduled(account.as_deref())).await;
        loading_list.set(false);
        match res {
            Ok(list) => scheduled_items.set(list),
            Err(msg) => error.set(Some(msg)),
        }
    });
}

#[allow(clippy::too_many_arguments)]
fn load_full_message(
    session: Signal<Session>,
    account: Option<String>,
    folder: String,
    uid: u32,
    allow_remote_images: bool,
    mut messages: Signal<Vec<MailMessageInfo>>,
    mut selected_msg: Signal<Option<MailMessageInfo>>,
    mut remote_images_loaded: Signal<bool>,
    mut loading_reader: Signal<bool>,
    mut error: Signal<Option<String>>,
) {
    loading_reader.set(true);
    error.set(None);
    spawn(async move {
        let acct_read = account.clone();
        let fold_read = folder.clone();
        let res = run_mail_job(session, move |c| {
            c.mail_message_with_options(acct_read.as_deref(), &fold_read, uid, allow_remote_images)
        })
        .await;
        loading_reader.set(false);
        match res {
            Ok(mut msg) => {
                remote_images_loaded.set(allow_remote_images);
                let was_unread = is_unread(&msg);
                if was_unread {
                    toggle_flag_local(&mut msg, "\\Seen", true);
                    update_message_flags_in_list(&mut messages.write(), uid, "\\Seen", true);
                    let acct_flag = account;
                    let fold_flag = folder;
                    spawn(async move {
                        let _ = run_mail_job(session, move |c| {
                            c.mail_flag(acct_flag.as_deref(), &fold_flag, uid, Some(true), None)
                        })
                        .await;
                    });
                }
                selected_msg.set(Some(msg));
            }
            Err(err) => error.set(Some(err)),
        }
    });
}

/// Everything the list, the reader and the mail actions share. Signals are `Copy`,
/// so the whole bundle is `Copy` and can be handed to any component or closure.
#[derive(Clone, Copy, PartialEq)]
pub(crate) struct MailCtx {
    pub session: Signal<Session>,
    pub account: Signal<Option<String>>,
    pub folder: Signal<String>,
    pub folders: Signal<Vec<String>>,
    pub messages: Signal<Vec<MailMessageInfo>>,
    pub selected: Signal<Option<MailMessageInfo>>,
    pub selection: Signal<Selection>,
    pub undo: Signal<Option<UndoEntry>>,
    pub seq: Signal<u64>,
    pub remote_loaded: Signal<bool>,
    pub loading_reader: Signal<bool>,
    pub loading_list: Signal<bool>,
    pub error: Signal<Option<String>>,
    pub reader_html: Signal<bool>,
    pub snooze_for: Signal<Option<Vec<u32>>>,
    pub ask_open: Signal<bool>,
    pub compose: Signal<Option<ComposeState>>,
    pub filter: Signal<MailFilter>,
    pub active_search: Signal<String>,
    pub shortcuts_open: Signal<bool>,
}

/// Open a message in the reader (marks it read).
pub(crate) fn open_message(ctx: MailCtx, uid: u32) {
    let mut reader_html = ctx.reader_html;
    let mut ask_open = ctx.ask_open;
    reader_html.set(true);
    ask_open.set(false);
    load_full_message(
        ctx.session,
        (*ctx.account.peek()).clone(),
        (*ctx.folder.peek()).clone(),
        uid,
        false,
        ctx.messages,
        ctx.selected,
        ctx.remote_loaded,
        ctx.loading_reader,
        ctx.error,
    );
}

/// Reload the open message with remote images allowed.
pub(crate) fn open_message_with_remote_images(ctx: MailCtx, uid: u32) {
    load_full_message(
        ctx.session,
        (*ctx.account.peek()).clone(),
        (*ctx.folder.peek()).clone(),
        uid,
        true,
        ctx.messages,
        ctx.selected,
        ctx.remote_loaded,
        ctx.loading_reader,
        ctx.error,
    );
}

/// Reload the current folder or search. Settles any pending Undo first so a
/// deferred move cannot reappear in the new list.
pub(crate) fn reload(ctx: MailCtx) {
    settle(ctx);
    let mut selection = ctx.selection;
    selection.write().clear();
    load_folder_and_messages(
        ctx.session,
        (*ctx.account.peek()).clone(),
        (*ctx.folder.peek()).clone(),
        (*ctx.active_search.peek()).clone(),
        ctx.folders,
        ctx.messages,
        ctx.selected,
        ctx.loading_list,
        ctx.error,
    );
}

/// One entry of the rendered list: a day heading or a message row.
enum ListItem {
    Day(&'static str),
    Row(Box<MailMessageInfo>, ThreadInfo),
}

/// Group, order and label the visible messages. With `threaded` set, each
/// conversation shows its newest message with a count; expanded conversations list
/// the older ones under it.
fn build_list_items(
    rows: &[MailMessageInfo],
    threaded: bool,
    expanded: &BTreeSet<String>,
    now: i64,
    offset: i64,
) -> Vec<ListItem> {
    let mut items: Vec<ListItem> = Vec::new();
    let mut last_bucket: Option<DayBucket> = None;
    let mut push_day = |items: &mut Vec<ListItem>, msg: &MailMessageInfo| {
        if let Some(ts) = parse_mail_date(&msg.date) {
            let bucket = day_bucket(ts, now, offset);
            if last_bucket != Some(bucket) {
                last_bucket = Some(bucket);
                items.push(ListItem::Day(bucket.label()));
            }
        }
    };
    if !threaded {
        for msg in rows {
            push_day(&mut items, msg);
            items.push(ListItem::Row(Box::new(msg.clone()), ThreadInfo::default()));
        }
        return items;
    }
    let keyed: Vec<(u32, String)> = rows.iter().map(|m| (m.uid, m.thread_key.clone())).collect();
    for (key, uids) in group_threads(&keyed) {
        let Some(head) = rows.iter().find(|m| m.uid == uids[0]) else {
            continue;
        };
        let open = expanded.contains(&key);
        push_day(&mut items, head);
        items.push(ListItem::Row(
            Box::new(head.clone()),
            ThreadInfo {
                count: uids.len(),
                expanded: open,
                is_child: false,
            },
        ));
        if open {
            for uid in &uids[1..] {
                if let Some(child) = rows.iter().find(|m| m.uid == *uid) {
                    items.push(ListItem::Row(
                        Box::new(child.clone()),
                        ThreadInfo {
                            count: 1,
                            expanded: false,
                            is_child: true,
                        },
                    ));
                }
            }
        }
    }
    items
}

/// Messages from the page: key presses (with whether focus is in a text field) and
/// the viewer's UTC offset.
#[derive(Clone, Debug, Deserialize)]
struct BridgeEvent {
    kind: String,
    #[serde(default)]
    key: String,
    #[serde(default)]
    shift: bool,
    #[serde(default)]
    ctrl: bool,
    #[serde(default)]
    alt: bool,
    #[serde(default)]
    typing: bool,
    #[serde(default)]
    minutes: i64,
}

/// Page-side helper for the mail window. It owns the parts that need no Rust state:
/// resizable panes and the density choice (both remembered in `localStorage`),
/// reports key presses so the shortcuts work, and reports the UTC offset once.
/// Re-running it replaces the previous listeners.
const MAIL_BRIDGE_JS: &str = r#"
(function () {
  if (window.__serabotMailCleanup) { window.__serabotMailCleanup(); }
  const KEY = 'serabot.mail.layout.v1';
  const shell = () => document.querySelector('.mail-shell');
  const read = () => { try { return JSON.parse(localStorage.getItem(KEY)) || {}; } catch (e) { return {}; } };
  const write = (v) => { try { localStorage.setItem(KEY, JSON.stringify(v)); } catch (e) {} };
  const clamp = (v, lo, hi) => Math.min(hi, Math.max(lo, v));
  const apply = () => {
    const s = shell(); if (!s) return;
    const l = read();
    const total = s.clientWidth || 1200;
    const sidebar = clamp(l.sidebar || 220, 180, 320);
    const room = Math.max(total - sidebar - 360, 260);
    const list = clamp(l.list || 340, 260, Math.min(560, room));
    s.style.setProperty('--mail-sidebar-w', sidebar + 'px');
    s.style.setProperty('--mail-list-w', list + 'px');
    s.dataset.density = l.density === 'compact' ? 'compact' : 'comfortable';
  };
  let drag = null;
  const onDown = (e) => {
    const bar = e.target.closest && e.target.closest('.mail-resizer');
    const s = shell();
    if (!bar || !s) return;
    drag = { which: bar.dataset.resize, left: s.getBoundingClientRect().left, id: e.pointerId, bar };
    try { bar.setPointerCapture(e.pointerId); } catch (err) {}
    bar.classList.add('dragging');
    e.preventDefault();
  };
  const onMove = (e) => {
    if (!drag) return;
    const s = shell(); if (!s) return;
    const l = read();
    const x = e.clientX - drag.left;
    if (drag.which === 'sidebar') { l.sidebar = clamp(Math.round(x), 180, 320); }
    else { l.list = clamp(Math.round(x - (l.sidebar || 220) - 6), 260, 560); }
    write(l); apply();
  };
  const onUp = () => { if (drag) { drag.bar.classList.remove('dragging'); drag = null; } };
  const onKey = (e) => {
    if (!shell()) { cleanup(); return; }
    if (['Shift', 'Control', 'Alt', 'Meta', 'CapsLock'].indexOf(e.key) >= 0) return;
    const el = document.activeElement;
    const tag = el && el.tagName;
    const typing = !!(el && (tag === 'INPUT' || tag === 'TEXTAREA' || tag === 'SELECT' || el.isContentEditable))
      || !!document.querySelector('.mp-sheet, .mail-dialog');
    const ctrl = e.ctrlKey || e.metaKey;
    if (!typing && ctrl && (e.key === 'a' || e.key === 'z')) e.preventDefault();
    dioxus.send({ kind: 'key', key: e.key, shift: e.shiftKey, ctrl: ctrl, alt: e.altKey, typing: typing });
  };
  const onResize = () => apply();
  const cleanup = () => {
    document.removeEventListener('pointerdown', onDown);
    document.removeEventListener('pointermove', onMove);
    document.removeEventListener('pointerup', onUp);
    document.removeEventListener('keydown', onKey);
    window.removeEventListener('resize', onResize);
    window.__serabotMailCleanup = null;
  };
  window.__serabotMailCleanup = cleanup;
  document.addEventListener('pointerdown', onDown);
  document.addEventListener('pointermove', onMove);
  document.addEventListener('pointerup', onUp);
  document.addEventListener('keydown', onKey);
  window.addEventListener('resize', onResize);
  window.__serabotMail = {
    toggleDensity: () => { const l = read(); l.density = l.density === 'compact' ? 'comfortable' : 'compact'; write(l); apply(); },
    focusSearch: () => { const i = document.getElementById('mail-search'); if (i) { i.focus(); i.select(); } },
    apply: apply,
  };
  apply();
  dioxus.send({ kind: 'offset', minutes: -new Date().getTimezoneOffset() });
})();
"#;

/// The address mail is sent from for the active mailbox (used to leave yourself out
/// of Reply all).
fn own_address_of(status: &Option<MailStatus>, account: &Option<String>) -> String {
    let Some(status) = status else {
        return String::new();
    };
    let wanted = account.clone().unwrap_or_default();
    status
        .accounts
        .iter()
        .find(|a| a.account == wanted)
        .and_then(|a| a.username.clone())
        .or_else(|| status.username.clone())
        .unwrap_or_default()
}

/// Run a shortcut. `typing` was already ruled out by the page.
fn handle_key(ctx: MailCtx, key: MailKey, status: Signal<Option<MailStatus>>) {
    let visible: Vec<u32> = filter_messages(&ctx.messages.peek(), *ctx.filter.peek())
        .iter()
        .map(|m| m.uid)
        .collect();
    let open = (*ctx.selected.peek()).clone();
    let open_uid = open.as_ref().map(|m| m.uid);
    let checked = ctx.selection.peek().uids();
    let targets: Vec<u32> = if checked.is_empty() {
        open_uid.into_iter().collect()
    } else {
        checked
    };
    let mut compose = ctx.compose;
    let mut snooze_for = ctx.snooze_for;
    let mut shortcuts_open = ctx.shortcuts_open;
    let mut selection = ctx.selection;
    let mut ask_open = ctx.ask_open;
    match key {
        MailKey::NextMessage | MailKey::PrevMessage => {
            let idx = open_uid.and_then(|u| visible.iter().position(|v| *v == u));
            let next = match (key, idx) {
                (MailKey::NextMessage, Some(i)) => visible.get(i + 1),
                (MailKey::PrevMessage, Some(i)) => i.checked_sub(1).and_then(|p| visible.get(p)),
                _ => visible.first(),
            };
            if let Some(uid) = next.copied() {
                open_message(ctx, uid);
            }
        }
        MailKey::OpenMessage => {
            if open_uid.is_none()
                && let Some(uid) = visible.first().copied()
            {
                open_message(ctx, uid);
            }
        }
        MailKey::Compose => compose.set(Some(ComposeState::new_empty())),
        MailKey::Reply => {
            if let Some(msg) = &open {
                compose.set(Some(ComposeState::reply_to(msg)));
            }
        }
        MailKey::ReplyAll => {
            if let Some(msg) = &open {
                let own = own_address_of(&status.peek(), &ctx.account.peek());
                compose.set(Some(ComposeState::reply_all(msg, &own)));
            }
        }
        MailKey::Forward => {
            if let Some(msg) = &open {
                compose.set(Some(ComposeState::forward_of(msg)));
            }
        }
        MailKey::Archive => archive_messages(ctx, targets),
        MailKey::Delete => delete_messages(ctx, targets),
        MailKey::ToggleStar => {
            if let Some(msg) = &open {
                set_flags(ctx, targets, None, Some(!is_starred(msg)), None);
            }
        }
        MailKey::ToggleRead => {
            if let Some(msg) = &open {
                set_flags(ctx, targets, Some(is_unread(msg)), None, None);
            }
        }
        MailKey::Snooze => {
            if !targets.is_empty() {
                snooze_for.set(Some(targets));
            }
        }
        MailKey::SelectToggle => {
            if let Some(uid) = open_uid {
                selection.write().toggle(uid);
            }
        }
        MailKey::SelectAll => selection.write().select_all(&visible),
        MailKey::FocusSearch => {
            let _ = dioxus::document::eval(
                "window.__serabotMail && window.__serabotMail.focusSearch();",
            );
        }
        MailKey::Undo => {
            if ctx.undo.peek().is_some() {
                run_undo(ctx);
            }
        }
        MailKey::Escape => {
            if snooze_for.peek().is_some() {
                snooze_for.set(None);
            } else if *shortcuts_open.peek() {
                shortcuts_open.set(false);
            } else if !selection.peek().is_empty() {
                selection.write().clear();
            } else if *ask_open.peek() {
                ask_open.set(false);
            }
        }
        MailKey::Help => shortcuts_open.set(true),
        MailKey::GoInbox => {
            let mut folder = ctx.folder;
            folder.set("INBOX".to_owned());
            reload(ctx);
        }
    }
}

fn load_snoozed_messages(ctx: MailCtx, mut items: Signal<Vec<MailSnoozeItem>>) {
    let session = ctx.session;
    let account = (*ctx.account.peek()).clone();
    let mut loading = ctx.loading_list;
    let mut error = ctx.error;
    loading.set(true);
    error.set(None);
    spawn(async move {
        let res = run_mail_job(session, move |c| c.mail_list_snoozed(account.as_deref())).await;
        loading.set(false);
        match res {
            Ok(list) => items.set(list),
            Err(msg) => error.set(Some(msg)),
        }
    });
}

/// The Mail workspace (`Section::Mail`): folders and saved searches, a message
/// list with conversations, day headings, bulk actions and quick actions, and a
/// reading pane. Panes can be resized and the list density changed.
#[component]
pub fn MailView(session: Signal<Session>) -> Element {
    let mut mail_status = use_signal(|| Option::<MailStatus>::None);
    let mut loading_status = use_signal(|| true);
    let mut account = use_signal(|| Option::<String>::None);
    let folders = use_signal(|| vec!["INBOX".to_owned()]);
    let mut folder = use_signal(|| "INBOX".to_owned());
    let messages = use_signal(Vec::<MailMessageInfo>::new);
    let selected = use_signal(|| Option::<MailMessageInfo>::None);
    let reader_html = use_signal(|| true);
    let remote_loaded = use_signal(|| false);
    let selection = use_signal(Selection::default);
    let undo = use_signal(|| Option::<UndoEntry>::None);
    let seq = use_signal(|| 0u64);
    let snooze_for = use_signal(|| Option::<Vec<u32>>::None);
    let ask_open = use_signal(|| false);
    let compose = use_signal(|| Option::<ComposeState>::None);
    let filter = use_signal(|| MailFilter::All);
    let active_search = use_signal(String::new);
    let shortcuts_open = use_signal(|| false);
    let loading_list = use_signal(|| false);
    let loading_reader = use_signal(|| false);
    let error = use_signal(|| Option::<String>::None);

    let mut scheduled_items = use_signal(Vec::<ScheduledMailItem>::new);
    let mut selected_scheduled = use_signal(|| Option::<ScheduledMailItem>::None);
    let mut snoozed_items = use_signal(Vec::<MailSnoozeItem>::new);
    let mut reschedule_open = use_signal(|| false);
    let mut reschedule_input = use_signal(String::new);
    let mut search_input = use_signal(String::new);
    let mut search_help_open = use_signal(|| false);
    let mut saved_searches = use_signal(Vec::<SavedMailSearch>::new);
    let mut threads_on = use_signal(|| true);
    let mut expanded = use_signal(BTreeSet::<String>::new);
    let mut tz_offset = use_signal(|| 0i64);
    let mut signature_modal_open = use_signal(|| false);

    let ctx = MailCtx {
        session,
        account,
        folder,
        folders,
        messages,
        selected,
        selection,
        undo,
        seq,
        remote_loaded,
        loading_reader,
        loading_list,
        error,
        reader_html,
        snooze_for,
        ask_open,
        compose,
        filter,
        active_search,
        shortcuts_open,
    };

    // "Compose email" from the command palette.
    use_effect(move || {
        if session.read().mail_compose_request {
            session.write().mail_compose_request = false;
            let mut compose = compose;
            compose.set(Some(ComposeState::new_empty()));
        }
    });

    // Rows whose move or delete failed after the window was closed come back here.
    use_effect(move || {
        if !FAILED.read().is_empty() {
            restore_failed(ctx);
        }
    });

    use_hook(move || {
        spawn(async move {
            let res = run_mail_job(session, |c| c.mail_status()).await;
            loading_status.set(false);
            match res {
                Ok(st) => {
                    let has_any = st.connected || !st.accounts.is_empty();
                    let initial_account = if !st.connected && !st.accounts.is_empty() {
                        Some(st.accounts[0].account.clone())
                    } else {
                        None
                    };
                    mail_status.set(Some(st));
                    if has_any {
                        account.set(initial_account);
                        reload(ctx);
                        if let Ok(list) = run_mail_job(session, |c| c.mail_saved_searches()).await {
                            saved_searches.set(list);
                        }
                    }
                }
                Err(err) => {
                    let mut error = error;
                    error.set(Some(err));
                }
            }
        });
    });

    // Keyboard shortcuts, pane resizing and the viewer's UTC offset come from the page.
    use_future(move || async move {
        let mut bridge = dioxus::document::eval(MAIL_BRIDGE_JS);
        while let Ok(event) = bridge.recv::<BridgeEvent>().await {
            match event.kind.as_str() {
                "offset" => tz_offset.set(event.minutes),
                "key" => {
                    if let Some(key) =
                        map_key(&event.key, event.shift, event.ctrl, event.alt, event.typing)
                    {
                        handle_key(ctx, key, mail_status);
                    }
                }
                _ => {}
            }
        }
    });

    let status_val = mail_status();
    let has_mailbox = status_val
        .as_ref()
        .is_some_and(|s| s.connected || !s.accounts.is_empty());

    if loading_status() {
        return rsx! {
            div { class: "mail-empty-wrap",
                div { class: "card mail-empty-card",
                    p { class: "muted", "Checking mailbox connection\u{2026}" }
                }
            }
        };
    }

    if !has_mailbox {
        return rsx! {
            div { class: "mail-empty-wrap",
                div { class: "card mail-empty-card",
                    div { class: "mail-empty-icon",
                        Icon { name: "send", size: 28 }
                    }
                    h2 { "Connect your mailbox" }
                    p { class: "muted",
                        "Read, search, organize, and send emails directly from SeraBot using your own SMTP and IMAP credentials. Your password stays sealed per user on the server."
                    }
                    ul { class: "mail-empty-steps",
                        li { "Open Settings, then Integrations, then Mail." }
                        li { "Enter your SMTP and IMAP details and press Connect." }
                        li { "Come back here. Your inbox loads on its own." }
                    }
                    if let Some(err) = error() {
                        div { class: "banner err", role: "alert", span { class: "grow", "{err}" } }
                    }
                    div { class: "card-actions",
                        button {
                            class: "btn primary",
                            r#type: "button",
                            onclick: move |_| session.write().open_section(Section::Integrations),
                            "Connect mailbox"
                        }
                    }
                }
            }
        };
    }

    let accounts: Vec<MailAccountStatus> = status_val
        .as_ref()
        .map(|s| s.accounts.clone())
        .unwrap_or_default();
    let active_account_status = accounts
        .iter()
        .find(|a| a.account == account().unwrap_or_default())
        .cloned();
    let own_address = own_address_of(&status_val, &account());
    let now = now_unix_secs() as i64;
    let offset = tz_offset();
    let visible_messages = filter_messages(&messages(), filter());
    let is_scheduled_view = folder() == SCHEDULED_VIRTUAL_FOLDER;
    let is_snoozed_view = folder() == SNOOZED_VIRTUAL_FOLDER;
    let is_virtual_view = is_scheduled_view || is_snoozed_view;
    let current_sched_id = selected_scheduled().map(|s| s.id);
    let items = build_list_items(
        &visible_messages,
        threads_on() && active_search().is_empty(),
        &expanded(),
        now,
        offset,
    );
    let order: Vec<u32> = items
        .iter()
        .filter_map(|item| match item {
            ListItem::Row(msg, _) => Some(msg.uid),
            ListItem::Day(_) => None,
        })
        .collect();
    let unread_count = messages().iter().filter(|m| is_unread(m)).count();
    let search_active = !active_search().is_empty();
    let already_saved = saved_searches()
        .iter()
        .any(|s| s.query.eq_ignore_ascii_case(active_search().trim()));
    let folder_list = folders();

    rsx! {
        div { class: "mail-shell", tabindex: "-1",
            // Left pane: Compose, mailbox switcher, folders, saved searches
            aside { class: "mail-sidebar", "aria-label": "Mail folders",
                button {
                    class: "btn primary mail-compose-btn",
                    r#type: "button",
                    title: "Compose (c)",
                    onclick: move |_| {
                        let mut compose = compose;
                        compose.set(Some(ComposeState::new_empty()));
                    },
                    Icon { name: "edit", size: 15 }
                    "Compose"
                }

                if accounts.len() > 1 {
                    div { class: "mail-account-switcher",
                        label { class: "tiny muted", r#for: "mail-ws-account", "Mailbox" }
                        select {
                            id: "mail-ws-account",
                            class: "input",
                            value: "{account().clone().unwrap_or_default()}",
                            onchange: move |e| {
                                let v = e.value();
                                account.set(if v.is_empty() { None } else { Some(v) });
                                folder.set("INBOX".to_owned());
                                search_input.set(String::new());
                                let mut active_search = active_search;
                                active_search.set(String::new());
                                selected_scheduled.set(None);
                                reschedule_open.set(false);
                                reload(ctx);
                            },
                            if status_val.as_ref().is_some_and(|s| s.connected) {
                                option { value: "", "Default" }
                            }
                            for acct in accounts.iter().filter(|a| !a.account.trim().is_empty()) {
                                option {
                                    key: "{acct.account}",
                                    value: "{acct.account}",
                                    "{acct.account}"
                                }
                            }
                        }
                    }
                }

                nav { class: "mail-folder-list", "aria-label": "Folders",
                    for f in folder_list.iter() {
                        {
                            let f_name = f.clone();
                            let f_click = f.clone();
                            let (icon_name, label) = folder_icon_and_label(&f_name);
                            let active = !is_virtual_view && folder().eq_ignore_ascii_case(&f_name);
                            let badge = if active && unread_count > 0 && label == "Inbox" {
                                Some(unread_count)
                            } else {
                                None
                            };
                            rsx! {
                                button {
                                    key: "{f_name}",
                                    class: if active { "mail-folder-item active" } else { "mail-folder-item" },
                                    r#type: "button",
                                    "aria-current": if active { "page" } else { "false" },
                                    onclick: move |_| {
                                        folder.set(f_click.clone());
                                        selected_scheduled.set(None);
                                        reschedule_open.set(false);
                                        reload(ctx);
                                    },
                                    Icon { name: icon_name.to_owned(), size: 15 }
                                    span { class: "mail-folder-label", "{label}" }
                                    if let Some(n) = badge {
                                        span { class: "mail-folder-badge", "{n}" }
                                    }
                                }
                            }
                        }
                    }
                    div { class: "mail-folder-divider" }
                    button {
                        class: if is_snoozed_view { "mail-folder-item active" } else { "mail-folder-item" },
                        r#type: "button",
                        onclick: move |_| {
                            folder.set(SNOOZED_VIRTUAL_FOLDER.to_owned());
                            let mut selected = selected;
                            selected.set(None);
                            load_snoozed_messages(ctx, snoozed_items);
                        },
                        Icon { name: "history", size: 15 }
                        span { class: "mail-folder-label", "Snoozed" }
                    }
                    button {
                        class: if is_scheduled_view { "mail-folder-item active" } else { "mail-folder-item" },
                        r#type: "button",
                        onclick: move |_| {
                            folder.set(SCHEDULED_VIRTUAL_FOLDER.to_owned());
                            let mut selected = selected;
                            selected.set(None);
                            reschedule_open.set(false);
                            load_scheduled_messages(
                                session,
                                account(),
                                scheduled_items,
                                selected_scheduled,
                                loading_list,
                                error,
                            );
                        },
                        Icon { name: "workflows", size: 15 }
                        span { class: "mail-folder-label", "Scheduled" }
                    }
                }

                if !saved_searches().is_empty() {
                    div { class: "mail-saved", role: "group", "aria-label": "Saved searches",
                        div { class: "mail-saved-title", "Saved searches" }
                        for s in saved_searches().iter() {
                            {
                                let s_apply = s.clone();
                                let s_delete = s.clone();
                                rsx! {
                                    div { key: "{s.id}", class: "mail-saved-row",
                                        button {
                                            class: "mail-saved-item",
                                            r#type: "button",
                                            title: "{s.query}",
                                            onclick: move |_| {
                                                if is_virtual_view {
                                                    folder.set("INBOX".to_owned());
                                                }
                                                search_input.set(s_apply.query.clone());
                                                let mut active_search = active_search;
                                                active_search.set(s_apply.query.clone());
                                                reload(ctx);
                                            },
                                            Icon { name: "search", size: 13 }
                                            span { class: "mail-folder-label", "{s.name}" }
                                        }
                                        button {
                                            class: "icon-btn sm mail-saved-x",
                                            r#type: "button",
                                            title: "Remove this saved search",
                                            "aria-label": "Remove saved search {s.name}",
                                            onclick: move |_| {
                                                let id = s_delete.id.clone();
                                                let id_rm = s_delete.id.clone();
                                                spawn(async move {
                                                    let res = run_mail_job(session, move |c| {
                                                        c.mail_delete_saved_search(&id)
                                                    })
                                                    .await;
                                                    match res {
                                                        Ok(()) => saved_searches.write().retain(|x| x.id != id_rm),
                                                        Err(err) => {
                                                            session.write().toast = Some(format!("Could not remove the saved search: {err}"));
                                                        }
                                                    }
                                                });
                                            },
                                            Icon { name: "x", size: 12 }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }

                div { class: "mail-sidebar-foot",
                    button {
                        class: "btn ghost sm mail-manage-link",
                        r#type: "button",
                        title: "Toggle compact rows",
                        onclick: move |_| {
                            let _ = dioxus::document::eval(
                                "window.__serabotMail && window.__serabotMail.toggleDensity();",
                            );
                        },
                        Icon { name: "list", size: 14 }
                        "Density"
                    }
                    button {
                        class: "btn ghost sm mail-manage-link",
                        r#type: "button",
                        onclick: move |_| signature_modal_open.set(true),
                        Icon { name: "edit", size: 14 }
                        "Signature"
                    }
                    button {
                        class: "btn ghost sm mail-manage-link",
                        r#type: "button",
                        onclick: move |_| {
                            let mut shortcuts_open = shortcuts_open;
                            shortcuts_open.set(true);
                        },
                        Icon { name: "code", size: 14 }
                        "Shortcuts"
                    }
                    button {
                        class: "btn ghost sm mail-manage-link",
                        r#type: "button",
                        onclick: move |_| session.write().open_section(Section::Integrations),
                        Icon { name: "settings", size: 14 }
                        "Manage accounts"
                    }
                }
            }

            div { class: "mail-resizer", "data-resize": "sidebar", role: "separator", "aria-orientation": "vertical", "aria-label": "Resize folders" }

            // Middle pane
            section { class: "mail-list-pane", "aria-label": "Messages",
                if is_scheduled_view {
                    div { class: "mail-list-toolbar",
                        span { class: "mail-scheduled-heading", "Scheduled emails" }
                        button {
                            class: "icon-btn",
                            r#type: "button",
                            title: "Refresh scheduled",
                            "aria-label": "Refresh scheduled emails",
                            disabled: loading_list(),
                            onclick: move |_| {
                                load_scheduled_messages(
                                    session,
                                    account(),
                                    scheduled_items,
                                    selected_scheduled,
                                    loading_list,
                                    error,
                                );
                            },
                            Icon { name: "refresh", size: 15 }
                        }
                    }
                    if let Some(err) = error() {
                        div { class: "banner err mail-banner", role: "alert",
                            span { class: "grow", "{err}" }
                        }
                    }
                    div { class: "mail-list-scroll",
                        if loading_list() {
                            RowSkeletons {}
                        } else if scheduled_items().is_empty() {
                            div { class: "mail-list-empty",
                                Icon { name: "workflows", size: 22 }
                                p { class: "muted", "Nothing is scheduled." }
                                p { class: "tiny muted", "Use Send later in a new message to queue one." }
                            }
                        } else {
                            for item in scheduled_items().iter() {
                                {
                                    let item_click = item.clone();
                                    let selected_row = current_sched_id.as_deref() == Some(item.id.as_str());
                                    let rcpts = if item.to.is_empty() {
                                        "(No recipients)".to_owned()
                                    } else {
                                        item.to.join(", ")
                                    };
                                    let subject = if item.subject.trim().is_empty() {
                                        "(No subject)".to_owned()
                                    } else {
                                        item.subject.clone()
                                    };
                                    let status_cls = match item.status.to_ascii_lowercase().as_str() {
                                        "sent" => "mail-sched-badge sent",
                                        "failed" => "mail-sched-badge failed",
                                        _ => "mail-sched-badge pending",
                                    };
                                    let when = full_date_label(item.send_at, offset);
                                    rsx! {
                                        div {
                                            key: "{item.id}",
                                            class: if selected_row { "mail-row selected" } else { "mail-row" },
                                            role: "option",
                                            onclick: move |_| {
                                                reschedule_open.set(false);
                                                reschedule_input.set(String::new());
                                                selected_scheduled.set(Some(item_click.clone()));
                                            },
                                            div { class: "mail-row-main",
                                                div { class: "mail-row-top",
                                                    span { class: "{status_cls}", "{item.status}" }
                                                    span { class: "mail-row-sender", "{rcpts}" }
                                                    span { class: "mail-row-date", "{when}" }
                                                }
                                                div { class: "mail-row-subject", "{subject}" }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                } else if is_snoozed_view {
                    div { class: "mail-list-toolbar",
                        span { class: "mail-scheduled-heading", "Snoozed" }
                        button {
                            class: "icon-btn",
                            r#type: "button",
                            title: "Refresh snoozed",
                            "aria-label": "Refresh snoozed messages",
                            disabled: loading_list(),
                            onclick: move |_| load_snoozed_messages(ctx, snoozed_items),
                            Icon { name: "refresh", size: 15 }
                        }
                    }
                    if let Some(err) = error() {
                        div { class: "banner err mail-banner", role: "alert",
                            span { class: "grow", "{err}" }
                        }
                    }
                    div { class: "mail-list-scroll",
                        if loading_list() {
                            RowSkeletons {}
                        } else if snoozed_items().is_empty() {
                            div { class: "mail-list-empty",
                                Icon { name: "history", size: 22 }
                                p { class: "muted", "No snoozed messages." }
                                p { class: "tiny muted", "Press z on a message to bring it back later." }
                            }
                        } else {
                            for item in snoozed_items().iter() {
                                {
                                    let wake = full_date_label(item.wake_at, offset);
                                    let subject = if item.subject.trim().is_empty() {
                                        "(No subject)".to_owned()
                                    } else {
                                        item.subject.clone()
                                    };
                                    let name = sender_name(&item.from_addr);
                                    let item_back = item.clone();
                                    rsx! {
                                        div { key: "{item.id}", class: "mail-row snoozed-row", role: "option",
                                            div { class: "mail-row-main",
                                                div { class: "mail-row-top",
                                                    span { class: "mail-row-sender", "{name}" }
                                                    span { class: "mail-row-date", "{wake}" }
                                                }
                                                div { class: "mail-row-subject", "{subject}" }
                                                div { class: "mail-row-snippet", "Returns to {item_back.folder} at that time" }
                                            }
                                            div { class: "mail-row-actions always",
                                                button {
                                                    class: "btn ghost sm",
                                                    r#type: "button",
                                                    onclick: move |_| {
                                                        let it = item_back.clone();
                                                        let acct = account();
                                                        spawn(async move {
                                                            let (fold, uid, id) = (it.folder.clone(), it.uid, it.id.clone());
                                                            let res = run_mail_job(session, move |c| {
                                                                c.mail_unsnooze(acct.as_deref(), &fold, uid)
                                                            })
                                                            .await;
                                                            match res {
                                                                Ok(()) => {
                                                                    snoozed_items.write().retain(|x| x.id != id);
                                                                    session.write().toast = Some("Back in your folder.".to_owned());
                                                                }
                                                                Err(err) => {
                                                                    session.write().toast = Some(format!("Could not unsnooze: {err}"));
                                                                }
                                                            }
                                                        });
                                                    },
                                                    "Unsnooze"
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                } else {
                    div { class: "mail-list-toolbar",
                        div { class: "mail-search-box",
                            Icon { name: "search", size: 14 }
                            input {
                                id: "mail-search",
                                class: "input mail-search-input",
                                r#type: "text",
                                placeholder: "Search (from: to: subject: is:unread has:attachment)",
                                "aria-label": "Search messages",
                                value: "{search_input}",
                                oninput: move |e| search_input.set(e.value()),
                                onkeydown: move |e: KeyboardEvent| {
                                    if e.key() == Key::Enter && !e.is_composing() {
                                        let q = search_input().trim().to_owned();
                                        let mut active_search = active_search;
                                        active_search.set(q);
                                        reload(ctx);
                                    } else if e.key() == Key::Escape {
                                        search_help_open.set(false);
                                    }
                                },
                            }
                            if !search_input().is_empty() || search_active {
                                button {
                                    class: "icon-btn sm",
                                    r#type: "button",
                                    title: "Clear search",
                                    "aria-label": "Clear search",
                                    onclick: move |_| {
                                        search_input.set(String::new());
                                        let mut active_search = active_search;
                                        active_search.set(String::new());
                                        reload(ctx);
                                    },
                                    Icon { name: "x", size: 13 }
                                }
                            }
                            if search_active && !already_saved {
                                button {
                                    class: "icon-btn sm",
                                    r#type: "button",
                                    title: "Save this search",
                                    "aria-label": "Save this search",
                                    onclick: move |_| {
                                        let q = active_search().trim().to_owned();
                                        let name: String = q.chars().take(40).collect();
                                        spawn(async move {
                                            let res = run_mail_job(session, move |c| {
                                                c.mail_create_saved_search(&name, &q)
                                            })
                                            .await;
                                            match res {
                                                Ok(saved) => {
                                                    saved_searches.write().push(saved);
                                                    session.write().toast = Some("Search saved.".to_owned());
                                                }
                                                Err(err) => {
                                                    session.write().toast = Some(format!("Could not save the search: {err}"));
                                                }
                                            }
                                        });
                                    },
                                    Icon { name: "plus", size: 13 }
                                }
                            }
                            button {
                                class: if search_help_open() { "icon-btn sm active" } else { "icon-btn sm" },
                                r#type: "button",
                                title: "Search tips",
                                "aria-label": "Search tips",
                                "aria-expanded": if search_help_open() { "true" } else { "false" },
                                onclick: move |_| search_help_open.set(!search_help_open()),
                                Icon { name: "alert-circle", size: 13 }
                            }
                        }
                        button {
                            class: "icon-btn",
                            r#type: "button",
                            title: if threads_on() { "Showing conversations" } else { "Showing single messages" },
                            "aria-label": "Group messages into conversations",
                            "aria-pressed": if threads_on() { "true" } else { "false" },
                            onclick: move |_| threads_on.set(!threads_on()),
                            Icon { name: "threads", size: 15 }
                        }
                        button {
                            class: "icon-btn",
                            r#type: "button",
                            title: "Refresh",
                            "aria-label": "Refresh messages",
                            disabled: loading_list(),
                            onclick: move |_| reload(ctx),
                            Icon { name: "refresh", size: 15 }
                        }
                    }

                    if search_help_open() {
                        div { class: "mail-search-help", role: "note",
                            p { class: "tiny muted", "Combine words with these filters:" }
                            ul {
                                li { code { "from:anna" } " sender" }
                                li { code { "to:bob" } " recipient" }
                                li { code { "subject:invoice" } " subject" }
                                li { code { "is:unread" } " or " code { "is:starred" } }
                                li { code { "has:attachment" } " with a file" }
                                li { code { "after:2026-01-01" } " and " code { "before:2026-06-30" } }
                            }
                        }
                    }

                    BulkBar { ctx }

                    div { class: "mail-filter-bar", role: "tablist", "aria-label": "Filter messages",
                        for (tab, label) in [
                            (MailFilter::All, "All"),
                            (MailFilter::Unread, "Unread"),
                            (MailFilter::Starred, "Starred"),
                        ] {
                            button {
                                key: "{label}",
                                class: if filter() == tab { "seg-btn active" } else { "seg-btn" },
                                r#type: "button",
                                role: "tab",
                                "aria-selected": if filter() == tab { "true" } else { "false" },
                                onclick: move |_| {
                                    let mut filter = filter;
                                    filter.set(tab);
                                },
                                "{label}"
                            }
                        }
                    }

                    if let Some(err) = error() {
                        div { class: "banner err mail-banner", role: "alert",
                            span { class: "grow", "{err}" }
                            button {
                                class: "btn ghost sm",
                                r#type: "button",
                                onclick: move |_| reload(ctx),
                                "Try again"
                            }
                        }
                    }

                    div { class: "mail-list-scroll", role: "listbox", "aria-label": "Messages", "aria-multiselectable": "true",
                        if loading_list() && messages().is_empty() {
                            RowSkeletons {}
                        } else if items.is_empty() {
                            div { class: "mail-list-empty",
                                if search_active {
                                    Icon { name: "search", size: 22 }
                                    p { class: "muted", "No messages match your search." }
                                    p { class: "tiny muted", "Try fewer words, or search the whole mailbox from another folder." }
                                } else if filter() != MailFilter::All {
                                    Icon { name: "check", size: 22 }
                                    p { class: "muted", "Nothing here with this filter." }
                                } else {
                                    Icon { name: "check", size: 22 }
                                    p { class: "muted", "You are all caught up." }
                                    p { class: "tiny muted", "No messages in this folder." }
                                }
                            }
                        } else {
                            for item in items.iter() {
                                {
                                    match item {
                                        ListItem::Day(label) => rsx! {
                                            DayHeader { key: "day-{label}", label: label.to_string() }
                                        },
                                        ListItem::Row(msg, info) => {
                                            let info = *info;
                                            let thread_key = msg.thread_key.clone();
                                            rsx! {
                                                MailRow {
                                                    key: "{msg.uid}",
                                                    ctx,
                                                    msg: (**msg).clone(),
                                                    thread: info,
                                                    order: order.clone(),
                                                    now,
                                                    offset,
                                                    on_toggle_thread: move |_| {
                                                        let mut set = expanded.write();
                                                        if !set.remove(&thread_key) {
                                                            set.insert(thread_key.clone());
                                                        }
                                                    },
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

            div { class: "mail-resizer", "data-resize": "list", role: "separator", "aria-orientation": "vertical", "aria-label": "Resize message list" }

            // Right pane
            section { class: "mail-reader-pane", "aria-label": "Message reader",
                if is_scheduled_view {
                    if let Some(item) = selected_scheduled() {
                        {
                            let item_id = item.id.clone();
                            let item_id_cancel = item.id.clone();
                            let is_pending = item.status.eq_ignore_ascii_case("pending");
                            rsx! {
                                div { class: "mail-reader-head",
                                    h2 { class: "mail-reader-subject",
                                        if item.subject.trim().is_empty() {
                                            "(No subject)"
                                        } else {
                                            "{item.subject}"
                                        }
                                    }
                                    div { class: "mail-reader-meta",
                                        div { class: "mail-meta-row",
                                            span { class: "mail-meta-key", "To" }
                                            span { class: "mail-meta-val", "{item.to.join(\", \")}" }
                                        }
                                        if !item.cc.is_empty() {
                                            div { class: "mail-meta-row",
                                                span { class: "mail-meta-key", "Cc" }
                                                span { class: "mail-meta-val", "{item.cc.join(\", \")}" }
                                            }
                                        }
                                        if !item.bcc.is_empty() {
                                            div { class: "mail-meta-row",
                                                span { class: "mail-meta-key", "Bcc" }
                                                span { class: "mail-meta-val", "{item.bcc.join(\", \")}" }
                                            }
                                        }
                                        div { class: "mail-meta-row",
                                            span { class: "mail-meta-key", "Send" }
                                            span { class: "mail-meta-val", "{full_date_label(item.send_at, offset)} ({item.status})" }
                                        }
                                    }

                                    if let Some(ref err_msg) = item.last_error {
                                        div { class: "banner err", role: "alert", span { class: "grow", "{err_msg}" } }
                                    }

                                    div { class: "mail-reader-actions",
                                        if is_pending {
                                            div { class: "mail-move-wrap",
                                                button {
                                                    class: "btn ghost sm",
                                                    r#type: "button",
                                                    onclick: move |_| reschedule_open.set(!reschedule_open()),
                                                    Icon { name: "workflows", size: 14 }
                                                    "Reschedule"
                                                }
                                                if reschedule_open() {
                                                    div { class: "mail-reschedule-popover",
                                                        input {
                                                            class: "input",
                                                            r#type: "datetime-local",
                                                            "aria-label": "New send time",
                                                            value: "{reschedule_input}",
                                                            oninput: move |e| reschedule_input.set(e.value()),
                                                        }
                                                        button {
                                                            class: "btn primary sm",
                                                            r#type: "button",
                                                            disabled: normalize_custom_datetime(&reschedule_input()).is_none(),
                                                            onclick: move |_| {
                                                                let Some(local_at) = normalize_custom_datetime(&reschedule_input()) else {
                                                                    return;
                                                                };
                                                                let send_at = local_at - offset * 60;
                                                                let id = item_id.clone();
                                                                let row_id = item_id.clone();
                                                                reschedule_open.set(false);
                                                                spawn(async move {
                                                                    match run_mail_job(session, move |c| {
                                                                        c.mail_reschedule(&id, send_at)
                                                                    }).await {
                                                                        Ok(()) => {
                                                                            if let Some(ref mut cur) = *selected_scheduled.write() {
                                                                                cur.send_at = send_at;
                                                                            }
                                                                            for row in scheduled_items.write().iter_mut() {
                                                                                if row.id == row_id {
                                                                                    row.send_at = send_at;
                                                                                }
                                                                            }
                                                                            session.write().toast = Some("Email rescheduled.".to_owned());
                                                                        }
                                                                        Err(err) => {
                                                                            session.write().toast = Some(format!("Reschedule failed: {err}"));
                                                                        }
                                                                    }
                                                                });
                                                            },
                                                            "Save"
                                                        }
                                                    }
                                                }
                                            }
                                        }
                                        if is_pending {
                                            button {
                                                class: "btn danger sm",
                                                r#type: "button",
                                                onclick: move |_| {
                                                    let id = item_id_cancel.clone();
                                                    let id_rm = item_id_cancel.clone();
                                                    spawn(async move {
                                                        match run_mail_job(session, move |c| {
                                                            c.mail_cancel_scheduled(&id)
                                                        }).await {
                                                            Ok(()) => {
                                                                scheduled_items.write().retain(|s| s.id != id_rm);
                                                                selected_scheduled.set(None);
                                                                session.write().toast = Some("Scheduled email canceled.".to_owned());
                                                            }
                                                            Err(err) => {
                                                                session.write().toast = Some(format!("Cancel failed: {err}"));
                                                            }
                                                        }
                                                    });
                                                },
                                                Icon { name: "trash", size: 14 }
                                                "Cancel scheduled"
                                            }
                                        }
                                    }
                                }
                                div { class: "mail-reader-body",
                                    if let Some(ref html) = item.body_html
                                        && !html.trim().is_empty()
                                    {
                                        iframe {
                                            class: "mail-html-frame",
                                            "sandbox": "",
                                            srcdoc: "{wrap_email_html_srcdoc(html)}",
                                            title: "Scheduled email HTML preview",
                                        }
                                    } else {
                                        pre { class: "mail-plain-body", "{item.body}" }
                                    }
                                }
                            }
                        }
                    } else {
                        div { class: "mail-reader-empty muted",
                            Icon { name: "workflows", size: 24 }
                            p { "Select a scheduled email to inspect or reschedule." }
                        }
                    }
                } else if is_snoozed_view {
                    div { class: "mail-reader-empty muted",
                        Icon { name: "history", size: 24 }
                        p { "Snoozed messages leave your list and come back, unread, at the time you chose." }
                    }
                } else if loading_reader() {
                    div { class: "mail-reader-empty muted",
                        span { class: "skeleton mail-skel-line wide" }
                        span { class: "skeleton mail-skel-line" }
                        span { class: "skeleton mail-skel-line wide" }
                    }
                } else if let Some(msg) = selected() {
                    MailReader {
                        key: "{msg.uid}",
                        ctx,
                        msg: msg.clone(),
                        own_address: own_address.clone(),
                        offset,
                    }
                } else {
                    div { class: "mail-reader-empty muted",
                        Icon { name: "send", size: 24 }
                        p { "Select a message to read it." }
                        p { class: "tiny", "Press ? for keyboard shortcuts." }
                    }
                }
            }

            UndoBar { ctx }
            SnoozeDialog { ctx, now, offset }
            ShortcutsDialog { open: shortcuts_open }
        }

        if let Some(draft) = compose() {
            MailComposeModal {
                session,
                ctx,
                account: account(),
                initial: draft,
                on_close: move |_| {
                    let mut compose = compose;
                    compose.set(None);
                },
                on_sent: move |_| {
                    let mut compose = compose;
                    compose.set(None);
                    if folder() == SCHEDULED_VIRTUAL_FOLDER {
                        load_scheduled_messages(
                            session,
                            account(),
                            scheduled_items,
                            selected_scheduled,
                            loading_list,
                            error,
                        );
                    }
                },
            }
        }

        if signature_modal_open() {
            MailSignatureModal {
                session,
                account: account(),
                account_status: active_account_status.clone(),
                on_close: move |_| signature_modal_open.set(false),
                on_saved: move |(sig, sig_html, sig_logo): (Option<String>, Option<String>, Option<String>)| {
                    signature_modal_open.set(false);
                    let acct_key = account().unwrap_or_default();
                    if let Some(ref mut st) = *mail_status.write() {
                        for a in st.accounts.iter_mut() {
                            if a.account == acct_key {
                                a.signature = sig.clone();
                                a.signature_html = sig_html.clone();
                                a.signature_logo_hash = sig_logo.clone();
                            }
                        }
                    }
                },
            }
        }
    }
}

fn compose_fingerprint(to: &str, cc: &str, subject: &str, body: &str) -> String {
    format!("{to}\u{1}{cc}\u{1}{subject}\u{1}{body}")
}

#[component]
fn MailComposeModal(
    session: Signal<Session>,
    ctx: MailCtx,
    account: Option<String>,
    initial: ComposeState,
    on_close: EventHandler<()>,
    on_sent: EventHandler<()>,
) -> Element {
    let to = use_signal(|| initial.to.clone());
    let cc = use_signal(|| initial.cc.clone());
    let bcc = use_signal(|| initial.bcc.clone());
    let mut show_cc_bcc = use_signal(|| !initial.cc.is_empty() || !initial.bcc.is_empty());
    let mut subject = use_signal(|| initial.subject.clone());
    let mut body = use_signal(|| initial.body.clone());
    let mut html_mode = use_signal(|| true);
    let mut preview_open = use_signal(|| false);
    let mut link_open = use_signal(|| false);
    let mut link_url = use_signal(|| "https://".to_owned());
    let mut link_label = use_signal(String::new);
    let selected_hashes = use_signal(|| initial.selected_hashes.clone());
    let mut library_items = use_signal(Vec::<LibraryItem>::new);
    let mut picker_open = use_signal(|| false);
    let mut loading_library = use_signal(|| false);
    let mut send_later_open = use_signal(|| false);
    let mut custom_send_at = use_signal(String::new);
    let mut sending = use_signal(|| false);
    let mut send_error = use_signal(|| Option::<String>::None);
    let attached = use_signal(Vec::<AttachedFile>::new);
    let attach_busy = use_signal(|| 0usize);
    let mut attach_notice = use_signal(|| Option::<String>::None);
    let mut draft_uid = use_signal(|| Option::<u32>::None);
    let mut draft_folder = use_signal(String::new);
    let mut draft_state = use_signal(|| "");
    let mut finished = use_signal(|| false);

    let modal_title = match initial.mode {
        ComposeMode::New => "New message",
        ComposeMode::Reply => "Reply",
        ComposeMode::Forward => "Forward",
    };

    let load_library = move |_| {
        let next = !picker_open();
        picker_open.set(next);
        if next && library_items().is_empty() && !loading_library() {
            loading_library.set(true);
            spawn(async move {
                let res = run_mail_job(session, |c| c.global_library()).await;
                loading_library.set(false);
                if let Ok(status) = res {
                    library_items.set(status.items);
                }
            });
        }
    };

    let mut append_snippet = move |snippet: &str| {
        let mut cur = body();
        if !cur.is_empty() && !cur.ends_with('\n') && !cur.ends_with(' ') {
            cur.push(' ');
        }
        cur.push_str(snippet);
        body.set(cur);
    };

    let account_for_send = account.clone();
    let submit_send = Callback::new(move |()| {
        let bad = invalid_recipients(&[&to(), &cc(), &bcc()]);
        if !bad.is_empty() {
            send_error.set(Some(format!(
                "These are not email addresses: {}",
                bad.join(", ")
            )));
            return;
        }
        let recipients = recipient_addresses(&to());
        if recipients.is_empty() {
            send_error.set(Some("Enter at least one recipient.".to_owned()));
            return;
        }
        sending.set(true);
        send_error.set(None);
        send_later_open.set(false);
        let acct = account_for_send.clone();
        let cc_list = recipient_addresses(&cc());
        let bcc_list = recipient_addresses(&bcc());
        let subj = subject();
        let (plain_body, body_html) = compose_to_body_and_html(&body(), html_mode());
        let hashes: Vec<String> = selected_hashes().into_iter().collect();
        // Attachments cannot ride through the server-side hold, so those send at once.
        let can_hold = hashes.is_empty();
        let hold_at = now_unix_secs() as i64 + SEND_HOLD_SECS;
        let acct_del = acct.clone();
        let old_draft = draft_uid().map(|uid| (draft_folder(), uid));
        spawn(async move {
            let res = run_mail_job(session, move |c| {
                if can_hold {
                    match c.mail_send_with_undo(
                        acct.as_deref(),
                        &recipients,
                        &cc_list,
                        &bcc_list,
                        &subj,
                        &plain_body,
                        body_html.clone(),
                        hold_at,
                    ) {
                        Ok(item) => return Ok(Some(item.id)),
                        // A clock that disagrees with the server: send straight away.
                        Err(e) if e.to_string().contains("hold") => {}
                        Err(e) => return Err(e),
                    }
                }
                c.mail_send(
                    acct.as_deref(),
                    &recipients,
                    &cc_list,
                    &bcc_list,
                    &subj,
                    &plain_body,
                    body_html,
                    &hashes,
                )?;
                Ok(None)
            })
            .await;
            sending.set(false);
            match res {
                Ok(held) => {
                    finished.set(true);
                    match held {
                        Some(id) => announce(
                            ctx,
                            "Message sent".to_owned(),
                            UndoKind::Send { scheduled_id: id },
                            ((SEND_HOLD_SECS - 1) * 1000) as u32,
                        ),
                        None => session.write().toast = Some("Email sent.".to_owned()),
                    }
                    if let Some((folder, uid)) = old_draft {
                        queue_draft_delete(acct_del, folder, uid);
                    }
                    on_sent.call(());
                }
                Err(err) => send_error.set(Some(err)),
            }
        });
    });

    let account_for_sched = account.clone();
    let trigger_schedule = Callback::new(move |send_at: i64| {
        let bad = invalid_recipients(&[&to(), &cc(), &bcc()]);
        if !bad.is_empty() {
            send_error.set(Some(format!(
                "These are not email addresses: {}",
                bad.join(", ")
            )));
            return;
        }
        let recipients = recipient_addresses(&to());
        if recipients.is_empty() {
            send_error.set(Some("Enter at least one recipient.".to_owned()));
            return;
        }
        sending.set(true);
        send_error.set(None);
        send_later_open.set(false);
        let acct = account_for_sched.clone();
        let cc_list = recipient_addresses(&cc());
        let bcc_list = recipient_addresses(&bcc());
        let subj = subject();
        let (plain_body, body_html) = compose_to_body_and_html(&body(), html_mode());
        spawn(async move {
            let res = run_mail_job(session, move |c| {
                c.mail_schedule(
                    acct.as_deref(),
                    &recipients,
                    &cc_list,
                    &bcc_list,
                    &subj,
                    &plain_body,
                    body_html,
                    send_at,
                )
            })
            .await;
            sending.set(false);
            match res {
                Ok(item) => {
                    finished.set(true);
                    session.write().toast = Some(format!(
                        "Email scheduled for {}.",
                        format_unix_rfc3339(item.send_at.max(0) as u64)
                    ));
                    on_sent.call(());
                }
                Err(err) => send_error.set(Some(err)),
            }
        });
    });

    // Files dropped anywhere on the composer are uploaded and attached.
    use_future(move || async move {
        let mut bridge = dioxus::document::eval(MAIL_DROP_JS);
        while let Ok(file) = bridge.recv::<DroppedFile>().await {
            if file.too_big {
                attach_notice.set(Some(format!("{}: too large to attach.", file.name)));
                continue;
            }
            let (name, bytes) = decode_pasted(PastedFile {
                name: file.name,
                data: file.data,
            });
            upload_attachments(
                session,
                vec![(name, bytes)],
                attached,
                selected_hashes,
                attach_busy,
                attach_notice,
            );
        }
    });

    // Save a draft to the mailbox's Drafts folder once typing has paused for a few
    // seconds, replacing the previous autosave.
    let account_for_draft = account.clone();
    let fp_start = compose_fingerprint(&initial.to, &initial.cc, &initial.subject, &initial.body);
    use_future(move || {
        let acct = account_for_draft.clone();
        let start = fp_start.clone();
        async move {
            let mut saved_fp = start.clone();
            let mut last_seen = start;
            loop {
                sleep_ms(3000).await;
                if *finished.peek() {
                    break;
                }
                let fp = compose_fingerprint(&to.peek(), &cc.peek(), &subject.peek(), &body.peek());
                if fp != last_seen {
                    last_seen = fp;
                    continue;
                }
                if fp == saved_fp || *sending.peek() {
                    continue;
                }
                let (plain, html) = compose_to_body_and_html(&body.peek(), *html_mode.peek());
                let req = MailDraftRequest {
                    account: acct.clone(),
                    folder: None,
                    replace_uid: *draft_uid.peek(),
                    to: recipient_addresses(&to.peek()),
                    cc: recipient_addresses(&cc.peek()),
                    subject: (*subject.peek()).clone(),
                    body: plain,
                    body_html: html,
                };
                draft_state.set("Saving draft\u{2026}");
                let res = run_mail_job(session, move |c| c.mail_save_draft(&req)).await;
                // One attempt per edit: a failing server is not retried until the next change.
                saved_fp = fp;
                match res {
                    Ok(saved) => {
                        if *finished.peek() {
                            // The message went out while this save was in flight.
                            if let Some(uid) = saved.uid {
                                queue_draft_delete(acct.clone(), saved.folder.clone(), uid);
                            }
                            break;
                        }
                        draft_uid.set(saved.uid);
                        draft_folder.set(saved.folder);
                        draft_state.set("Draft saved");
                    }
                    Err(_) => draft_state.set("Draft not saved"),
                }
            }
        }
    });

    let (_, preview_html_opt) = compose_to_body_and_html(&body(), true);
    let preview_srcdoc = wrap_email_html_srcdoc(preview_html_opt.as_deref().unwrap_or(""));

    rsx! {
        div { class: "mp-sheet-scrim", onclick: move |_| on_close.call(()) }
        aside {
            class: "mp-sheet mail-compose-modal",
            role: "dialog",
            "aria-modal": "true",
            "aria-label": "{modal_title}",
            onkeydown: move |e: KeyboardEvent| {
                let mods = e.modifiers();
                let ctrl = mods.contains(Modifiers::CONTROL) || mods.contains(Modifiers::META);
                if e.key() == Key::Escape {
                    on_close.call(());
                } else if e.key() == Key::Enter && ctrl && !sending() {
                    e.prevent_default();
                    submit_send.call(());
                }
            },
            div { class: "mail-drop-hint", "aria-hidden": "true",
                Icon { name: "upload", size: 22 }
                span { "Drop files to attach them" }
            }
            div { class: "mp-sheet-head",
                h2 { class: "mp-sheet-title", "{modal_title}" }
                button {
                    class: "icon-btn",
                    r#type: "button",
                    title: "Close",
                    "aria-label": "Close",
                    onclick: move |_| on_close.call(()),
                    Icon { name: "x", size: 16 }
                }
            }
            div { class: "mp-sheet-body mail-compose-body",
                div { class: "rcpt-head",
                    RecipientField {
                        session,
                        id: "mail-compose-to".to_owned(),
                        label: "To".to_owned(),
                        value: to,
                        placeholder: "Type an address and press Enter".to_owned(),
                        disabled: sending(),
                    }
                    button {
                        class: "btn ghost sm rcpt-ccbcc",
                        r#type: "button",
                        onclick: move |_| show_cc_bcc.set(!show_cc_bcc()),
                        if show_cc_bcc() { "Hide Cc/Bcc" } else { "Cc / Bcc" }
                    }
                }
                if show_cc_bcc() {
                    RecipientField {
                        session,
                        id: "mail-compose-cc".to_owned(),
                        label: "Cc".to_owned(),
                        value: cc,
                        placeholder: "Copy".to_owned(),
                        disabled: sending(),
                    }
                    RecipientField {
                        session,
                        id: "mail-compose-bcc".to_owned(),
                        label: "Bcc".to_owned(),
                        value: bcc,
                        placeholder: "Hidden copy".to_owned(),
                        disabled: sending(),
                    }
                }
                div { class: "field",
                    label { class: "field-label", r#for: "mail-compose-subject", "Subject" }
                    input {
                        id: "mail-compose-subject",
                        class: "input",
                        r#type: "text",
                        placeholder: "Subject",
                        disabled: sending(),
                        value: "{subject}",
                        oninput: move |e| subject.set(e.value()),
                    }
                }
                div { class: "field",
                    div { class: "mail-field-head-row",
                        label { class: "field-label", r#for: "mail-compose-text", "Message" }
                        div { class: "mail-format-pill", role: "group", "aria-label": "Compose mode",
                            button {
                                class: if html_mode() { "seg-btn active" } else { "seg-btn" },
                                r#type: "button",
                                onclick: move |_| html_mode.set(true),
                                "HTML"
                            }
                            button {
                                class: if !html_mode() { "seg-btn active" } else { "seg-btn" },
                                r#type: "button",
                                onclick: move |_| {
                                    html_mode.set(false);
                                    preview_open.set(false);
                                    link_open.set(false);
                                },
                                "Plain"
                            }
                        }
                    }

                    if html_mode() {
                        div { class: "mail-compose-toolbar", role: "toolbar", "aria-label": "Formatting",
                            button {
                                class: "btn ghost sm mail-fmt-btn",
                                r#type: "button",
                                title: "Bold",
                                onclick: move |_| append_snippet("<b>bold</b>"),
                                strong { "B" }
                            }
                            button {
                                class: "btn ghost sm mail-fmt-btn",
                                r#type: "button",
                                title: "Italic",
                                onclick: move |_| append_snippet("<i>italic</i>"),
                                em { "I" }
                            }
                            button {
                                class: "btn ghost sm mail-fmt-btn",
                                r#type: "button",
                                title: "Underline",
                                onclick: move |_| append_snippet("<u>underline</u>"),
                                span { style: "text-decoration: underline;", "U" }
                            }
                            button {
                                class: "btn ghost sm mail-fmt-btn",
                                r#type: "button",
                                title: "Bullet list",
                                onclick: move |_| append_snippet("\n<ul>\n  <li>Item</li>\n</ul>"),
                                Icon { name: "list", size: 13 }
                                "List"
                            }
                            button {
                                class: "btn ghost sm mail-fmt-btn",
                                r#type: "button",
                                title: "Insert link",
                                onclick: move |_| link_open.set(!link_open()),
                                Icon { name: "external", size: 13 }
                                "Link"
                            }
                            button {
                                class: "btn ghost sm mail-fmt-btn",
                                r#type: "button",
                                title: "Block quote",
                                onclick: move |_| append_snippet("\n<blockquote>Quote</blockquote>"),
                                "Quote"
                            }
                            button {
                                class: if preview_open() { "btn ghost sm mail-fmt-btn active" } else { "btn ghost sm mail-fmt-btn" },
                                r#type: "button",
                                onclick: move |_| preview_open.set(!preview_open()),
                                Icon { name: "eye", size: 13 }
                                if preview_open() { "Edit" } else { "Preview" }
                            }
                        }
                    }

                    if html_mode() && link_open() {
                        div { class: "mail-link-bar",
                            input {
                                class: "input",
                                r#type: "url",
                                placeholder: "https://example.com",
                                value: "{link_url}",
                                oninput: move |e| link_url.set(e.value()),
                            }
                            input {
                                class: "input",
                                r#type: "text",
                                placeholder: "Link text",
                                value: "{link_label}",
                                oninput: move |e| link_label.set(e.value()),
                            }
                            button {
                                class: "btn primary sm",
                                r#type: "button",
                                onclick: move |_| {
                                    let url = link_url().trim().to_owned();
                                    if !url.is_empty() {
                                        let label = if link_label().trim().is_empty() {
                                            url.clone()
                                        } else {
                                            link_label().trim().to_owned()
                                        };
                                        let snippet = format!(
                                            "<a href=\"{}\">{}</a>",
                                            escape_html_text(&url),
                                            escape_html_text(&label)
                                        );
                                        append_snippet(&snippet);
                                    }
                                    link_open.set(false);
                                    link_label.set(String::new());
                                },
                                "Insert"
                            }
                        }
                    }

                    if html_mode() && preview_open() {
                        iframe {
                            class: "mail-html-frame mail-compose-preview",
                            "sandbox": "",
                            srcdoc: "{preview_srcdoc}",
                            title: "Compose HTML preview",
                        }
                    } else {
                        textarea {
                            id: "mail-compose-text",
                            class: "input mail-compose-textarea",
                            rows: 10,
                            disabled: sending(),
                            value: "{body}",
                            oninput: move |e| body.set(e.value()),
                        }
                    }
                }

                div { class: "mail-compose-attach-bar",
                    button {
                        class: "btn ghost sm",
                        r#type: "button",
                        disabled: sending(),
                        title: "Choose files from this computer. You can also drop them anywhere here.",
                        onclick: move |_| match pick_local_files() {
                            Ok(picked) => {
                                upload_attachments(
                                    session,
                                    picked.files,
                                    attached,
                                    selected_hashes,
                                    attach_busy,
                                    attach_notice,
                                );
                                if !picked.skipped.is_empty() {
                                    attach_notice.set(Some(format!(
                                        "Too large to attach: {}",
                                        picked.skipped.join(", ")
                                    )));
                                }
                            }
                            Err(err) => attach_notice.set(Some(err)),
                        },
                        Icon { name: "paperclip", size: 14 }
                        "Attach files"
                    }
                    button {
                        class: "btn ghost sm",
                        r#type: "button",
                        disabled: sending(),
                        onclick: load_library,
                        Icon { name: "library", size: 14 }
                        "From Library"
                    }
                    span { class: "tiny muted", "or drop files here" }
                }
                AttachmentTray {
                    items: attached(),
                    busy: attach_busy(),
                    notice: attach_notice(),
                    on_remove: move |hash: String| detach(attached, selected_hashes, &hash),
                }

                if picker_open() {
                    div { class: "mail-library-picker",
                        if loading_library() {
                            p { class: "tiny muted", "Loading Library\u{2026}" }
                        } else if library_items().is_empty() {
                            p { class: "tiny muted", "No files in your Library yet." }
                        } else {
                            for item in library_items().iter().take(40) {
                                {
                                    let id = item.id.clone();
                                    let id_toggle = item.id.clone();
                                    let name_toggle = item.label();
                                    let size_toggle = item.size_bytes.unwrap_or(0);
                                    let checked = selected_hashes().contains(&id);
                                    let label = item.label();
                                    rsx! {
                                        label { key: "{id}", class: "mail-library-item",
                                            input {
                                                r#type: "checkbox",
                                                checked,
                                                onchange: move |_| {
                                                    if checked {
                                                        detach(attached, selected_hashes, &id_toggle);
                                                    } else {
                                                        attach(
                                                            attached,
                                                            selected_hashes,
                                                            id_toggle.clone(),
                                                            name_toggle.clone(),
                                                            size_toggle,
                                                        );
                                                    }
                                                },
                                            }
                                            span { class: "mail-library-item-name", "{label}" }
                                            span { class: "tiny muted", "{item.day()}" }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }

                if let Some(err) = send_error() {
                    div { class: "banner err", role: "alert", span { class: "grow", "{err}" } }
                }

                div { class: "card-actions mail-compose-actions",
                    span { class: "tiny muted mail-draft-state", role: "status", "{draft_state}" }
                    button {
                        class: "btn ghost",
                        r#type: "button",
                        disabled: sending(),
                        onclick: move |_| on_close.call(()),
                        "Cancel"
                    }
                    div { class: "mail-send-split",
                        button {
                            class: "btn primary",
                            r#type: "button",
                            disabled: sending(),
                            onclick: move |_| submit_send.call(()),
                            Icon { name: "send", size: 14 }
                            if sending() { "Sending\u{2026}" } else { "Send" }
                        }
                        button {
                            class: "btn ghost mail-send-later-btn",
                            r#type: "button",
                            disabled: sending(),
                            onclick: move |_| send_later_open.set(!send_later_open()),
                            Icon { name: "workflows", size: 14 }
                            "Send later"
                            Icon { name: "chevron-down", size: 13 }
                        }
                        if send_later_open() {
                            div { class: "mail-send-later-menu",
                                button {
                                    class: "mail-send-later-opt",
                                    r#type: "button",
                                    onclick: move |_| {
                                        let ts = schedule_preset_in_one_hour(now_unix_secs());
                                        trigger_schedule.call(ts);
                                    },
                                    "In 1 hour"
                                }
                                button {
                                    class: "mail-send-later-opt",
                                    r#type: "button",
                                    onclick: move |_| {
                                        let ts = schedule_preset_tomorrow_9am(now_unix_secs());
                                        trigger_schedule.call(ts);
                                    },
                                    "Tomorrow 09:00"
                                }
                                button {
                                    class: "mail-send-later-opt",
                                    r#type: "button",
                                    onclick: move |_| {
                                        let ts = schedule_preset_next_monday_9am(now_unix_secs());
                                        trigger_schedule.call(ts);
                                    },
                                    "Next Monday 09:00"
                                }
                                div { class: "mail-send-later-custom",
                                    label { class: "tiny muted", "Custom date & time" }
                                    input {
                                        class: "input",
                                        r#type: "datetime-local",
                                        value: "{custom_send_at}",
                                        oninput: move |e| custom_send_at.set(e.value()),
                                    }
                                    button {
                                        class: "btn primary sm",
                                        r#type: "button",
                                        disabled: normalize_custom_datetime(&custom_send_at()).is_none(),
                                        onclick: move |_| {
                                            if let Some(ts) = normalize_custom_datetime(&custom_send_at()) {
                                                trigger_schedule.call(ts);
                                            }
                                        },
                                        "Schedule"
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

#[cfg(windows)]
fn pick_image_file_for_upload() -> Result<Option<(String, Vec<u8>)>, String> {
    let Some(path) = rfd::FileDialog::new()
        .set_title("Choose signature logo")
        .add_filter("Images", &["png", "jpg", "jpeg", "gif", "webp", "svg"])
        .pick_file()
    else {
        return Ok(None);
    };
    let filename = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("logo.png")
        .to_owned();
    let bytes = std::fs::read(&path).map_err(|e| format!("Could not read file: {e}"))?;
    Ok(Some((filename, bytes)))
}

#[cfg(not(windows))]
fn pick_image_file_for_upload() -> Result<Option<(String, Vec<u8>)>, String> {
    Err("Uploading from disk needs the Windows app.".to_owned())
}

fn load_logo_preview_data_url(
    session: Signal<Session>,
    hash: String,
    mut logo_data_url: Signal<Option<String>>,
) {
    spawn(async move {
        let res = run_mail_job(session, move |c| {
            if let Some((ct, bytes)) = c.library_thumbnail(&hash, 160)? {
                return Ok((ct, bytes));
            }
            let (name, bytes) = c.artifact(&hash)?;
            let ct = if name.to_ascii_lowercase().ends_with(".svg") {
                "image/svg+xml".to_owned()
            } else if name.to_ascii_lowercase().ends_with(".jpg")
                || name.to_ascii_lowercase().ends_with(".jpeg")
            {
                "image/jpeg".to_owned()
            } else {
                "image/png".to_owned()
            };
            Ok((ct, bytes))
        })
        .await;
        if let Ok((ct, bytes)) = res {
            let b64 = base64::engine::general_purpose::STANDARD.encode(&bytes);
            logo_data_url.set(Some(format!("data:{ct};base64,{b64}")));
        }
    });
}

#[component]
fn MailSignatureModal(
    session: Signal<Session>,
    account: Option<String>,
    account_status: Option<MailAccountStatus>,
    on_close: EventHandler<()>,
    on_saved: EventHandler<(Option<String>, Option<String>, Option<String>)>,
) -> Element {
    let initial_plain = account_status
        .as_ref()
        .and_then(|a| a.signature.clone())
        .unwrap_or_default();
    let initial_html = account_status
        .as_ref()
        .and_then(|a| a.signature_html.clone())
        .unwrap_or_default();
    let initial_logo = account_status
        .as_ref()
        .and_then(|a| a.signature_logo_hash.clone());

    let mut sig_name = use_signal(String::new);
    let mut sig_title = use_signal(String::new);
    let mut sig_company = use_signal(String::new);
    let mut sig_phone = use_signal(String::new);
    let mut sig_website = use_signal(String::new);
    let mut custom_html_mode = use_signal(|| !initial_html.trim().is_empty());
    let mut custom_html = use_signal(|| initial_html.clone());
    let mut custom_plain = use_signal(|| initial_plain.clone());
    let mut logo_hash = use_signal(|| initial_logo.clone());
    let mut logo_data_url = use_signal(|| Option::<String>::None);
    let mut library_picker_open = use_signal(|| false);
    let mut library_images = use_signal(Vec::<LibraryItem>::new);
    let mut loading_library = use_signal(|| false);
    let mut uploading = use_signal(|| false);
    let mut saving = use_signal(|| false);
    let mut sig_error = use_signal(|| Option::<String>::None);

    use_hook(move || {
        if let Some(hash) = initial_logo {
            load_logo_preview_data_url(session, hash, logo_data_url);
        }
    });

    let fields = SignatureFields {
        name: sig_name(),
        title: sig_title(),
        company: sig_company(),
        phone: sig_phone(),
        website: sig_website(),
    };
    let (built_plain, built_html) =
        build_signature_html_and_plain(&fields, logo_data_url().as_deref());
    let preview_html = if custom_html_mode() {
        custom_html()
    } else {
        built_html.clone()
    };
    let preview_srcdoc = wrap_email_html_srcdoc(&preview_html);

    let open_library_picker = move |_| {
        let next = !library_picker_open();
        library_picker_open.set(next);
        if next && library_images().is_empty() && !loading_library() {
            loading_library.set(true);
            spawn(async move {
                let res = run_mail_job(session, |c| c.global_library()).await;
                loading_library.set(false);
                if let Ok(status) = res {
                    let imgs: Vec<LibraryItem> = status
                        .items
                        .into_iter()
                        .filter(is_image_library_item)
                        .collect();
                    library_images.set(imgs);
                }
            });
        }
    };

    let upload_logo = move |_| {
        sig_error.set(None);
        match pick_image_file_for_upload() {
            Ok(Some((filename, bytes))) => {
                uploading.set(true);
                spawn(async move {
                    let res =
                        run_mail_job(session, move |c| c.library_upload(&filename, &bytes)).await;
                    uploading.set(false);
                    match res {
                        Ok(reply) => {
                            if let Some(item) = reply.item {
                                let hash = item.id.clone();
                                logo_hash.set(Some(hash.clone()));
                                load_logo_preview_data_url(session, hash, logo_data_url);
                                session.write().toast =
                                    Some("Logo uploaded to Library.".to_owned());
                            }
                        }
                        Err(err) => sig_error.set(Some(err)),
                    }
                });
            }
            Ok(None) => {}
            Err(err) => sig_error.set(Some(err)),
        }
    };

    let account_for_save = account.clone();
    let save_signature = move |_| {
        saving.set(true);
        sig_error.set(None);
        let acct = account_for_save.clone();
        let hash = logo_hash();
        let (final_plain, final_html) = if custom_html_mode() {
            let p = if custom_plain().trim().is_empty() {
                strip_html_tags(&custom_html())
            } else {
                custom_plain()
            };
            (Some(p), Some(custom_html()))
        } else {
            let f = SignatureFields {
                name: sig_name(),
                title: sig_title(),
                company: sig_company(),
                phone: sig_phone(),
                website: sig_website(),
            };
            let (p, h) = build_signature_html_and_plain(&f, None);
            (Some(p), Some(h))
        };
        let plain_cb = final_plain.clone();
        let html_cb = final_html.clone();
        let hash_cb = hash.clone();
        spawn(async move {
            let res = run_mail_job(session, move |c| {
                c.mail_update_signature(acct.as_deref(), final_plain, final_html, hash)
            })
            .await;
            saving.set(false);
            match res {
                Ok(()) => {
                    session.write().toast = Some("Signature saved.".to_owned());
                    on_saved.call((plain_cb, html_cb, hash_cb));
                }
                Err(err) => sig_error.set(Some(err)),
            }
        });
    };

    rsx! {
        div { class: "mp-sheet-scrim", onclick: move |_| on_close.call(()) }
        aside {
            class: "mp-sheet mail-signature-modal",
            role: "dialog",
            "aria-modal": "true",
            "aria-label": "Email signature builder",
            div { class: "mp-sheet-head",
                h2 { class: "mp-sheet-title", "Signature Builder" }
                button {
                    class: "icon-btn",
                    r#type: "button",
                    title: "Close",
                    "aria-label": "Close",
                    onclick: move |_| on_close.call(()),
                    Icon { name: "x", size: 16 }
                }
            }
            div { class: "mp-sheet-body mail-compose-body",
                div { class: "mail-field-head-row",
                    span { class: "tiny muted",
                        "Account: "
                        strong { "{account.as_deref().unwrap_or(\"Default\")}" }
                    }
                    div { class: "mail-format-pill", role: "group", "aria-label": "Signature editor mode",
                        button {
                            class: if !custom_html_mode() { "seg-btn active" } else { "seg-btn" },
                            r#type: "button",
                            onclick: move |_| custom_html_mode.set(false),
                            "Fields"
                        }
                        button {
                            class: if custom_html_mode() { "seg-btn active" } else { "seg-btn" },
                            r#type: "button",
                            onclick: move |_| {
                                if custom_html().trim().is_empty() {
                                    custom_html.set(built_html.clone());
                                }
                                if custom_plain().trim().is_empty() {
                                    custom_plain.set(built_plain.clone());
                                }
                                custom_html_mode.set(true);
                            },
                            "Custom HTML"
                        }
                    }
                }

                if custom_html_mode() {
                    div { class: "field",
                        label { class: "field-label", r#for: "mail-sig-html", "Signature HTML" }
                        textarea {
                            id: "mail-sig-html",
                            class: "input mail-sig-textarea",
                            rows: 6,
                            disabled: saving(),
                            value: "{custom_html}",
                            oninput: move |e| custom_html.set(e.value()),
                        }
                    }
                    div { class: "field",
                        label { class: "field-label", r#for: "mail-sig-plain", "Plain-text fallback" }
                        textarea {
                            id: "mail-sig-plain",
                            class: "input",
                            rows: 3,
                            disabled: saving(),
                            value: "{custom_plain}",
                            oninput: move |e| custom_plain.set(e.value()),
                        }
                    }
                } else {
                    div { class: "mail-sig-grid",
                        div { class: "field",
                            label { class: "field-label", r#for: "mail-sig-name", "Name" }
                            input {
                                id: "mail-sig-name",
                                class: "input",
                                r#type: "text",
                                placeholder: "Jane Doe",
                                value: "{sig_name}",
                                oninput: move |e| sig_name.set(e.value()),
                            }
                        }
                        div { class: "field",
                            label { class: "field-label", r#for: "mail-sig-title", "Title" }
                            input {
                                id: "mail-sig-title",
                                class: "input",
                                r#type: "text",
                                placeholder: "Head of Operations",
                                value: "{sig_title}",
                                oninput: move |e| sig_title.set(e.value()),
                            }
                        }
                        div { class: "field",
                            label { class: "field-label", r#for: "mail-sig-company", "Company" }
                            input {
                                id: "mail-sig-company",
                                class: "input",
                                r#type: "text",
                                placeholder: "Acme Corp",
                                value: "{sig_company}",
                                oninput: move |e| sig_company.set(e.value()),
                            }
                        }
                        div { class: "field",
                            label { class: "field-label", r#for: "mail-sig-phone", "Phone" }
                            input {
                                id: "mail-sig-phone",
                                class: "input",
                                r#type: "text",
                                placeholder: "+1 555 010 9999",
                                value: "{sig_phone}",
                                oninput: move |e| sig_phone.set(e.value()),
                            }
                        }
                    }
                    div { class: "field",
                        label { class: "field-label", r#for: "mail-sig-website", "Website" }
                        input {
                            id: "mail-sig-website",
                            class: "input",
                            r#type: "url",
                            placeholder: "https://acme.example",
                            value: "{sig_website}",
                            oninput: move |e| sig_website.set(e.value()),
                        }
                    }
                }

                div { class: "mail-sig-logo-row",
                    button {
                        class: "btn ghost sm",
                        r#type: "button",
                        disabled: saving() || uploading(),
                        onclick: open_library_picker,
                        Icon { name: "image", size: 14 }
                        "Logo from Library"
                    }
                    button {
                        class: "btn ghost sm",
                        r#type: "button",
                        disabled: saving() || uploading(),
                        onclick: upload_logo,
                        Icon { name: "upload", size: 14 }
                        if uploading() { "Uploading\u{2026}" } else { "Upload logo" }
                    }
                    if logo_hash().is_some() {
                        button {
                            class: "btn ghost sm",
                            r#type: "button",
                            onclick: move |_| {
                                logo_hash.set(None);
                                logo_data_url.set(None);
                            },
                            Icon { name: "x", size: 13 }
                            "Remove logo"
                        }
                    }
                }

                if library_picker_open() {
                    div { class: "mail-library-picker",
                        if loading_library() {
                            p { class: "tiny muted", "Loading Library images\u{2026}" }
                        } else if library_images().is_empty() {
                            p { class: "tiny muted", "No images in your Library yet." }
                        } else {
                            for item in library_images().iter().take(30) {
                                {
                                    let hash = item.id.clone();
                                    let hash_pick = item.id.clone();
                                    let label = item.label();
                                    let active = logo_hash().as_deref() == Some(hash.as_str());
                                    rsx! {
                                        button {
                                            key: "{hash}",
                                            class: if active { "mail-library-item active" } else { "mail-library-item" },
                                            r#type: "button",
                                            onclick: move |_| {
                                                logo_hash.set(Some(hash_pick.clone()));
                                                library_picker_open.set(false);
                                                load_logo_preview_data_url(session, hash_pick.clone(), logo_data_url);
                                            },
                                            Icon { name: "image", size: 13 }
                                            span { class: "mail-library-item-name", "{label}" }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }

                div { class: "field",
                    span { class: "field-label", "Live preview" }
                    iframe {
                        class: "mail-html-frame mail-sig-preview",
                        "sandbox": "",
                        srcdoc: "{preview_srcdoc}",
                        title: "Signature live preview",
                    }
                }

                if let Some(err) = sig_error() {
                    div { class: "banner err", role: "alert", span { class: "grow", "{err}" } }
                }

                div { class: "card-actions",
                    button {
                        class: "btn ghost",
                        r#type: "button",
                        disabled: saving(),
                        onclick: move |_| on_close.call(()),
                        "Cancel"
                    }
                    button {
                        class: "btn primary",
                        r#type: "button",
                        disabled: saving(),
                        onclick: save_signature,
                        Icon { name: "check", size: 14 }
                        if saving() { "Saving\u{2026}" } else { "Save signature" }
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_msg(uid: u32, flags: &[&str]) -> MailMessageInfo {
        MailMessageInfo {
            uid,
            subject: "Quarterly Report".to_owned(),
            from: "alice@example.test".to_owned(),
            to: "bob@example.test".to_owned(),
            date: "2026-11-24".to_owned(),
            flags: flags.iter().map(|s| (*s).to_owned()).collect(),
            body: "Line one\nLine two".to_owned(),
            body_html: None,
            has_remote_images: false,
            attachments: vec![],
            ..Default::default()
        }
    }

    #[test]
    fn folder_icon_and_label_maps_common_and_custom_folders() {
        assert_eq!(
            folder_icon_and_label("INBOX"),
            ("folder", "Inbox".to_owned())
        );
        assert_eq!(
            folder_icon_and_label("INBOX.Sent"),
            ("send", "Sent".to_owned())
        );
        assert_eq!(
            folder_icon_and_label("Drafts"),
            ("edit", "Drafts".to_owned())
        );
        assert_eq!(
            folder_icon_and_label("Archive"),
            ("library", "Archive".to_owned())
        );
        assert_eq!(
            folder_icon_and_label("Trash"),
            ("trash", "Trash".to_owned())
        );
        assert_eq!(folder_icon_and_label("Junk"), ("alert", "Spam".to_owned()));
        assert_eq!(
            folder_icon_and_label("Clients/Acme"),
            ("folder", "Clients/Acme".to_owned())
        );
    }

    #[test]
    fn unread_and_starred_filters_work() {
        let msgs = vec![
            sample_msg(1, &[]),
            sample_msg(2, &["\\Seen"]),
            sample_msg(3, &["\\Seen", "\\Flagged"]),
            sample_msg(4, &["\\Flagged"]),
        ];
        assert_eq!(filter_messages(&msgs, MailFilter::All).len(), 4);
        assert_eq!(
            filter_messages(&msgs, MailFilter::Unread)
                .into_iter()
                .map(|m| m.uid)
                .collect::<Vec<_>>(),
            vec![1, 4]
        );
        assert_eq!(
            filter_messages(&msgs, MailFilter::Starred)
                .into_iter()
                .map(|m| m.uid)
                .collect::<Vec<_>>(),
            vec![3, 4]
        );
    }

    #[test]
    fn reply_and_forward_formatting_avoids_duplicate_prefixes() {
        assert_eq!(format_reply_subject("Hello"), "Re: Hello");
        assert_eq!(format_reply_subject("Re: Hello"), "Re: Hello");
        assert_eq!(format_reply_subject("re: Hello"), "re: Hello");
        assert_eq!(format_forward_subject("Hello"), "Fwd: Hello");
        assert_eq!(format_forward_subject("Fwd: Hello"), "Fwd: Hello");
        assert_eq!(format_forward_subject("Fw: Hello"), "Fw: Hello");

        let msg = sample_msg(9, &[]);
        let reply = format_reply_quote(&msg);
        assert!(reply.contains("On 2026-11-24, alice@example.test wrote:"));
        assert!(reply.contains("> Line one\n> Line two\n"));

        let fwd = format_forward_quote(&msg);
        assert!(fwd.contains("---------- Forwarded message ----------"));
        assert!(fwd.contains("From: alice@example.test"));
        assert!(fwd.contains("Subject: Quarterly Report"));
    }

    #[test]
    fn ask_bot_prompt_and_recipient_parsing() {
        let msg = sample_msg(42, &[]);
        let prompt = format_ask_bot_prompt(Some("work"), "INBOX", &msg);
        assert!(prompt.contains("account: work, folder: INBOX, UID: 42"));
        assert!(prompt.contains("Subject: Quarterly Report"));

        assert_eq!(
            parse_recipients(" a@b.test , c@d.test; ; e@f.test "),
            vec!["a@b.test", "c@d.test", "e@f.test"]
        );
        assert_eq!(format_bytes(512), "512 B");
        assert_eq!(format_bytes(2048), "2.0 KB");
    }

    #[test]
    fn html_reader_wrapper_and_composer_conversion_work() {
        let wrapped = wrap_email_html_srcdoc("<p>Hello</p>");
        assert!(wrapped.contains("color-scheme:light"));
        assert!(wrapped.contains("Content-Security-Policy"));
        assert!(wrapped.contains("img-src data:;"));
        assert!(wrapped.contains("<p>Hello</p>"));

        let (plain_only, html_none) = compose_to_body_and_html("Hello\nWorld", false);
        assert_eq!(plain_only, "Hello\nWorld");
        assert!(html_none.is_none());

        // Plain-text email addresses or inequalities must not be treated as HTML tags.
        let reply_quote = "From: Alice <alice@example.com>\na < b && c > d";
        let (plain_quote, html_quote) = compose_to_body_and_html(reply_quote, true);
        assert_eq!(plain_quote, reply_quote);
        let html_quote_str = html_quote.unwrap();
        assert!(html_quote_str.contains("&lt;alice@example.com&gt;"));

        let (plain_fb, html_some) = compose_to_body_and_html("<b>Hi</b>\nThere", true);
        assert_eq!(plain_fb, "Hi\nThere");
        let html_str = html_some.unwrap();
        assert!(html_str.contains("<b>Hi</b><br>\nThere"));
    }

    #[test]
    fn schedule_presets_and_signature_builder_work() {
        // 2026-11-25T08:00:00Z = 20782 days * 86400 + 8 * 3600 = 1795593600
        let now = 1_795_593_600u64;
        assert_eq!(format_unix_rfc3339(now), "2026-11-25T08:00:00Z");
        assert_eq!(schedule_preset_in_one_hour(now), (now + 3600) as i64);
        assert_eq!(
            format_unix_rfc3339(schedule_preset_tomorrow_9am(now) as u64),
            "2026-11-26T09:00:00Z"
        );
        // 2026-11-25 is Wednesday -> next Monday is 2026-11-30
        assert_eq!(
            format_unix_rfc3339(schedule_preset_next_monday_9am(now) as u64),
            "2026-11-30T09:00:00Z"
        );
        assert_eq!(
            normalize_custom_datetime("2026-12-01T14:30").map(|t| format_unix_rfc3339(t as u64)),
            Some("2026-12-01T14:30:00Z".to_owned())
        );

        let fields = SignatureFields {
            name: "Alice".into(),
            title: "CEO".into(),
            company: "Acme".into(),
            phone: "+1 555 0100".into(),
            website: "acme.example".into(),
        };
        let (plain, html) =
            build_signature_html_and_plain(&fields, Some("data:image/png;base64,AA"));
        assert!(plain.contains("Alice"));
        assert!(plain.contains("CEO · Acme"));
        assert!(html.contains("https://acme.example"));
        assert!(html.contains("data:image/png;base64,AA"));
    }

    #[test]
    fn flags_match_with_or_without_the_backslash() {
        // The server reports `Seen`; older UI code wrote `\\Seen`.
        let mut msg = sample_msg(1, &["Seen", "Flagged"]);
        toggle_flag_local(&mut msg, "\\Seen", false);
        assert!(is_unread(&msg));
        toggle_flag_local(&mut msg, "\\Seen", true);
        assert!(!is_unread(&msg));
        toggle_flag_local(&mut msg, "\\Flagged", false);
        assert!(!is_starred(&msg));
        assert_eq!(msg.flags, vec!["Seen".to_owned()]);
    }

    #[test]
    fn reply_all_keeps_others_and_drops_self_sender_and_duplicates() {
        let mut msg = sample_msg(1, &[]);
        msg.from = "Alice <alice@example.test>".into();
        msg.to = "me@example.test, Bob <bob@example.test>".into();
        msg.cc = "carol@example.test, ME@example.test, alice@example.test".into();
        let draft = ComposeState::reply_all(&msg, "me@example.test");
        assert_eq!(draft.to, "alice@example.test");
        assert_eq!(draft.cc, "bob@example.test, carol@example.test");
        assert!(draft.subject.starts_with("Re: "));
    }

    #[test]
    fn list_items_group_conversations_and_add_day_headings() {
        let mk = |uid: u32, key: &str, date: &str| MailMessageInfo {
            thread_key: key.into(),
            date: date.into(),
            ..sample_msg(uid, &[])
        };
        let rows = vec![
            mk(5, "t1", "Wed, 25 Nov 2026 09:00:00 +0000"),
            mk(4, "t2", "Wed, 25 Nov 2026 08:00:00 +0000"),
            mk(3, "t1", "Mon, 23 Nov 2026 08:00:00 +0000"),
        ];
        let now = 1_795_600_800; // Wed 25 Nov 2026 10:00 UTC
        let summary = |items: &[ListItem]| -> Vec<String> {
            items
                .iter()
                .map(|item| match item {
                    ListItem::Day(label) => format!("day:{label}"),
                    ListItem::Row(msg, info) => {
                        format!("{}:{}:{}", msg.uid, info.count, info.is_child)
                    }
                })
                .collect()
        };
        let folded = build_list_items(&rows, true, &BTreeSet::new(), now, 0);
        assert_eq!(
            summary(&folded),
            vec!["day:Today", "5:2:false", "4:1:false"]
        );
        let open: BTreeSet<String> = ["t1".to_owned()].into();
        let expanded = build_list_items(&rows, true, &open, now, 0);
        assert_eq!(
            summary(&expanded),
            vec!["day:Today", "5:2:false", "3:1:true", "4:1:false"]
        );
        let flat = build_list_items(&rows, false, &BTreeSet::new(), now, 0);
        assert_eq!(
            summary(&flat),
            vec![
                "day:Today",
                "5:0:false",
                "4:0:false",
                "day:This week",
                "3:0:false"
            ]
        );
    }
}
