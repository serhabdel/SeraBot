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
use serabot_desktop::session::{Section, Session};
use serabot_desktop::{
    MailAccountStatus, MailAttachmentInfo, MailMessageInfo, MailStatus, ScheduledMailItem,
};

use crate::ui::Icon;

const DEFAULT_MAIL_LIMIT: u32 = 50;
const SCHEDULED_VIRTUAL_FOLDER: &str = "__scheduled__";

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
enum ComposeMode {
    New,
    Reply,
    Forward,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ComposeState {
    mode: ComposeMode,
    to: String,
    cc: String,
    bcc: String,
    subject: String,
    body: String,
    selected_hashes: BTreeSet<String>,
}

impl ComposeState {
    fn new_empty() -> Self {
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

    fn reply_to(msg: &MailMessageInfo) -> Self {
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

    fn forward_of(msg: &MailMessageInfo) -> Self {
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
async fn run_mail_job<T, F>(session: Signal<Session>, job: F) -> Result<T, String>
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

fn toggle_flag_local(msg: &mut MailMessageInfo, flag: &str, on: bool) {
    msg.flags.retain(|f| !f.eq_ignore_ascii_case(flag));
    if on {
        msg.flags.push(flag.to_owned());
    }
}

fn update_message_flags_in_list(messages: &mut [MailMessageInfo], uid: u32, flag: &str, on: bool) {
    if let Some(item) = messages.iter_mut().find(|m| m.uid == uid) {
        toggle_flag_local(item, flag, on);
    }
}

#[cfg(windows)]
fn save_bytes_with_picker(default_name: &str, bytes: &[u8]) -> Result<Option<String>, String> {
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
fn save_bytes_with_picker(_default_name: &str, _bytes: &[u8]) -> Result<Option<String>, String> {
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

/// Dedicated 3-pane Mail workspace (`Section::Mail`).
#[component]
pub fn MailView(session: Signal<Session>) -> Element {
    let mut mail_status = use_signal(|| Option::<MailStatus>::None);
    let mut loading_status = use_signal(|| true);
    let mut account = use_signal(|| Option::<String>::None);
    let folders = use_signal(|| vec!["INBOX".to_owned()]);
    let mut folder = use_signal(|| "INBOX".to_owned());
    let mut messages = use_signal(Vec::<MailMessageInfo>::new);
    let mut selected_msg = use_signal(|| Option::<MailMessageInfo>::None);
    let mut reader_html_mode = use_signal(|| true);
    let remote_images_loaded = use_signal(|| false);
    let mut scheduled_items = use_signal(Vec::<ScheduledMailItem>::new);
    let mut selected_scheduled = use_signal(|| Option::<ScheduledMailItem>::None);
    let mut reschedule_open = use_signal(|| false);
    let mut reschedule_input = use_signal(String::new);
    let mut filter = use_signal(|| MailFilter::All);
    let mut search_input = use_signal(String::new);
    let mut active_search = use_signal(String::new);
    let loading_list = use_signal(|| false);
    let loading_reader = use_signal(|| false);
    let mut error = use_signal(|| Option::<String>::None);
    let mut confirm_delete = use_signal(|| false);
    let mut move_menu_open = use_signal(|| false);
    let mut move_target = use_signal(String::new);
    let mut compose = use_signal(|| Option::<ComposeState>::None);
    let mut signature_modal_open = use_signal(|| false);

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
                        account.set(initial_account.clone());
                        load_folder_and_messages(
                            session,
                            initial_account,
                            "INBOX".to_owned(),
                            String::new(),
                            folders,
                            messages,
                            selected_msg,
                            loading_list,
                            error,
                        );
                    }
                }
                Err(err) => error.set(Some(err)),
            }
        });
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
        .find(|a| {
            let cur = account().unwrap_or_default();
            a.account == cur
        })
        .cloned();
    let visible_messages = filter_messages(&messages(), filter());
    let current_uid = selected_msg().map(|m| m.uid);
    let is_scheduled_view = folder() == SCHEDULED_VIRTUAL_FOLDER;
    let current_sched_id = selected_scheduled().map(|s| s.id);

    rsx! {
        div { class: "mail-shell",
            // Left pane: account switcher, Compose, folders, Scheduled, Signature, Manage accounts
            aside { class: "mail-sidebar", "aria-label": "Mail folders",
                button {
                    class: "btn primary mail-compose-btn",
                    r#type: "button",
                    onclick: move |_| compose.set(Some(ComposeState::new_empty())),
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
                                let next = if v.is_empty() { None } else { Some(v) };
                                account.set(next.clone());
                                folder.set("INBOX".to_owned());
                                search_input.set(String::new());
                                active_search.set(String::new());
                                confirm_delete.set(false);
                                move_menu_open.set(false);
                                move_target.set(String::new());
                                selected_scheduled.set(None);
                                reschedule_open.set(false);
                                load_folder_and_messages(
                                    session,
                                    next,
                                    "INBOX".to_owned(),
                                    String::new(),
                                    folders,
                                    messages,
                                    selected_msg,
                                    loading_list,
                                    error,
                                );
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

                nav { class: "mail-folder-list",
                    for f in folders().iter() {
                        {
                            let f_name = f.clone();
                            let f_click = f.clone();
                            let (icon_name, label) = folder_icon_and_label(&f_name);
                            let active = !is_scheduled_view && folder().eq_ignore_ascii_case(&f_name);
                            rsx! {
                                button {
                                    key: "{f_name}",
                                    class: if active { "mail-folder-item active" } else { "mail-folder-item" },
                                    r#type: "button",
                                    onclick: move |_| {
                                        folder.set(f_click.clone());
                                        confirm_delete.set(false);
                                        move_menu_open.set(false);
                                        move_target.set(String::new());
                                        selected_scheduled.set(None);
                                        reschedule_open.set(false);
                                        load_folder_and_messages(
                                            session,
                                            account(),
                                            f_click.clone(),
                                            active_search(),
                                            folders,
                                            messages,
                                            selected_msg,
                                            loading_list,
                                            error,
                                        );
                                    },
                                    Icon { name: icon_name.to_owned(), size: 15 }
                                    span { class: "mail-folder-label", "{label}" }
                                }
                            }
                        }
                    }
                    div { class: "mail-folder-divider" }
                    button {
                        class: if is_scheduled_view { "mail-folder-item active" } else { "mail-folder-item" },
                        r#type: "button",
                        onclick: move |_| {
                            folder.set(SCHEDULED_VIRTUAL_FOLDER.to_owned());
                            confirm_delete.set(false);
                            move_menu_open.set(false);
                            selected_msg.set(None);
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

                div { class: "mail-sidebar-foot",
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
                        onclick: move |_| session.write().open_section(Section::Integrations),
                        Icon { name: "settings", size: 14 }
                        "Manage accounts"
                    }
                }
            }

            // Middle pane: scheduled list OR search + filter + message list
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
                            div { class: "mail-list-empty muted", "Loading scheduled emails\u{2026}" }
                        } else if scheduled_items().is_empty() {
                            div { class: "mail-list-empty muted", "No scheduled emails for this mailbox." }
                        } else {
                            for item in scheduled_items().iter() {
                                {
                                    let item_click = item.clone();
                                    let selected = current_sched_id.as_deref() == Some(item.id.as_str());
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
                                    rsx! {
                                        div {
                                            key: "{item.id}",
                                            class: if selected { "mail-row selected" } else { "mail-row" },
                                            onclick: move |_| {
                                                reschedule_open.set(false);
                                                reschedule_input.set(String::new());
                                                selected_scheduled.set(Some(item_click.clone()));
                                            },
                                            div { class: "mail-row-top",
                                                span { class: "{status_cls}", "{item.status}" }
                                                span { class: "mail-row-sender", "{rcpts}" }
                                            }
                                            div { class: "mail-row-bottom",
                                                span { class: "mail-row-subject", "{subject}" }
                                                span { class: "mail-row-date", "{item.send_at}" }
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
                                class: "input mail-search-input",
                                r#type: "text",
                                placeholder: "Search folder (Enter)",
                                "aria-label": "Search messages",
                                value: "{search_input}",
                                oninput: move |e| search_input.set(e.value()),
                                onkeydown: move |e: KeyboardEvent| {
                                    if e.key() == Key::Enter && !e.is_composing() {
                                        let q = search_input().trim().to_owned();
                                        active_search.set(q.clone());
                                        load_folder_and_messages(
                                            session,
                                            account(),
                                            folder(),
                                            q,
                                            folders,
                                            messages,
                                            selected_msg,
                                            loading_list,
                                            error,
                                        );
                                    }
                                },
                            }
                            if !search_input().is_empty() || !active_search().is_empty() {
                                button {
                                    class: "icon-btn sm",
                                    r#type: "button",
                                    title: "Clear search",
                                    "aria-label": "Clear search",
                                    onclick: move |_| {
                                        search_input.set(String::new());
                                        active_search.set(String::new());
                                        load_folder_and_messages(
                                            session,
                                            account(),
                                            folder(),
                                            String::new(),
                                            folders,
                                            messages,
                                            selected_msg,
                                            loading_list,
                                            error,
                                        );
                                    },
                                    Icon { name: "x", size: 13 }
                                }
                            }
                        }
                        button {
                            class: "icon-btn",
                            r#type: "button",
                            title: "Refresh",
                            "aria-label": "Refresh messages",
                            disabled: loading_list(),
                            onclick: move |_| {
                                load_folder_and_messages(
                                    session,
                                    account(),
                                    folder(),
                                    active_search(),
                                    folders,
                                    messages,
                                    selected_msg,
                                    loading_list,
                                    error,
                                );
                            },
                            Icon { name: "refresh", size: 15 }
                        }
                    }

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
                                onclick: move |_| filter.set(tab),
                                "{label}"
                            }
                        }
                    }

                    if let Some(err) = error() {
                        div { class: "banner err mail-banner", role: "alert",
                            span { class: "grow", "{err}" }
                        }
                    }

                    div { class: "mail-list-scroll",
                        if loading_list() {
                            div { class: "mail-list-empty muted", "Loading messages\u{2026}" }
                        } else if visible_messages.is_empty() {
                            div { class: "mail-list-empty muted",
                                if !active_search().is_empty() {
                                    "No messages match your search."
                                } else {
                                    "No messages in this view."
                                }
                            }
                        } else {
                            for msg in visible_messages.iter() {
                                {
                                    let uid = msg.uid;
                                    let unread = is_unread(msg);
                                    let starred = is_starred(msg);
                                    let has_attach = !msg.attachments.is_empty();
                                    let selected = current_uid == Some(uid);
                                    let sender = if msg.from.trim().is_empty() {
                                        "(Unknown sender)".to_owned()
                                    } else {
                                        msg.from.clone()
                                    };
                                    let subject = if msg.subject.trim().is_empty() {
                                        "(No subject)".to_owned()
                                    } else {
                                        msg.subject.clone()
                                    };
                                    let date = msg.date.clone();
                                    rsx! {
                                        div {
                                            key: "{uid}",
                                            class: if selected {
                                                "mail-row selected"
                                            } else if unread {
                                                "mail-row unread"
                                            } else {
                                                "mail-row"
                                            },
                                            onclick: move |_| {
                                                confirm_delete.set(false);
                                                move_menu_open.set(false);
                                                reader_html_mode.set(true);
                                                load_full_message(
                                                    session,
                                                    account(),
                                                    folder(),
                                                    uid,
                                                    false,
                                                    messages,
                                                    selected_msg,
                                                    remote_images_loaded,
                                                    loading_reader,
                                                    error,
                                                );
                                            },
                                            div { class: "mail-row-top",
                                                span { class: if unread { "mail-unread-dot active" } else { "mail-unread-dot" } }
                                                span { class: "mail-row-sender", "{sender}" }
                                                span { class: "mail-row-date", "{date}" }
                                            }
                                            div { class: "mail-row-bottom",
                                                span { class: "mail-row-subject", "{subject}" }
                                                div { class: "mail-row-badges",
                                                    if has_attach {
                                                        span { class: "mail-row-attach", title: "Has attachments",
                                                            Icon { name: "paperclip", size: 13 }
                                                        }
                                                    }
                                                    button {
                                                        class: if starred { "mail-star-btn starred" } else { "mail-star-btn" },
                                                        r#type: "button",
                                                        title: if starred { "Unstar" } else { "Star" },
                                                        "aria-label": if starred { "Unstar message" } else { "Star message" },
                                                        onclick: move |e: MouseEvent| {
                                                            e.stop_propagation();
                                                            let next_star = !starred;
                                                            update_message_flags_in_list(&mut messages.write(), uid, "\\Flagged", next_star);
                                                            if let Some(ref mut cur) = *selected_msg.write()
                                                                && cur.uid == uid
                                                            {
                                                                toggle_flag_local(cur, "\\Flagged", next_star);
                                                            }
                                                            let acct = account();
                                                            let fold = folder();
                                                            spawn(async move {
                                                                if let Err(err) = run_mail_job(session, move |c| {
                                                                    c.mail_flag(acct.as_deref(), &fold, uid, None, Some(next_star))
                                                                }).await {
                                                                    session.write().toast = Some(format!("Could not update star: {err}"));
                                                                }
                                                            });
                                                        },
                                                        Icon { name: "star", size: 14 }
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

            // Right pane: scheduled item detail OR message reader
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
                                            span { class: "mail-meta-val", "{format_unix_rfc3339(item.send_at.max(0) as u64)} ({item.status})" }
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
                                                                let Some(send_at) = normalize_custom_datetime(&reschedule_input()) else {
                                                                    return;
                                                                };
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
                } else if loading_reader() {
                    div { class: "mail-reader-empty muted", "Loading message\u{2026}" }
                } else if let Some(msg) = selected_msg() {
                    {
                        let uid = msg.uid;
                        let unread = is_unread(&msg);
                        let starred = is_starred(&msg);
                        let has_html = msg
                            .body_html
                            .as_deref()
                            .is_some_and(|h| !h.trim().is_empty());
                        let msg_reply = msg.clone();
                        let msg_fwd = msg.clone();
                        let msg_bot = msg.clone();
                        let other_folders: Vec<String> = folders()
                            .into_iter()
                            .filter(|f| !f.eq_ignore_ascii_case(&folder()))
                            .collect();
                        rsx! {
                            div { class: "mail-reader-head",
                                h2 { class: "mail-reader-subject",
                                    if msg.subject.trim().is_empty() {
                                        "(No subject)"
                                    } else {
                                        "{msg.subject}"
                                    }
                                }
                                div { class: "mail-reader-meta",
                                    div { class: "mail-meta-row",
                                        span { class: "mail-meta-key", "From" }
                                        span { class: "mail-meta-val", "{msg.from}" }
                                    }
                                    if !msg.to.trim().is_empty() {
                                        div { class: "mail-meta-row",
                                            span { class: "mail-meta-key", "To" }
                                            span { class: "mail-meta-val", "{msg.to}" }
                                        }
                                    }
                                    div { class: "mail-meta-row",
                                        span { class: "mail-meta-key", "Date" }
                                        span { class: "mail-meta-val", "{msg.date}" }
                                    }
                                }

                                div { class: "mail-reader-actions",
                                    button {
                                        class: "btn ghost sm",
                                        r#type: "button",
                                        onclick: move |_| compose.set(Some(ComposeState::reply_to(&msg_reply))),
                                        Icon { name: "undo", size: 14 }
                                        "Reply"
                                    }
                                    button {
                                        class: "btn ghost sm",
                                        r#type: "button",
                                        onclick: move |_| compose.set(Some(ComposeState::forward_of(&msg_fwd))),
                                        Icon { name: "external", size: 14 }
                                        "Forward"
                                    }
                                    button {
                                        class: if starred { "btn ghost sm mail-starred-active" } else { "btn ghost sm" },
                                        r#type: "button",
                                        onclick: move |_| {
                                            let next_star = !starred;
                                            update_message_flags_in_list(&mut messages.write(), uid, "\\Flagged", next_star);
                                            if let Some(ref mut cur) = *selected_msg.write() {
                                                toggle_flag_local(cur, "\\Flagged", next_star);
                                            }
                                            let acct = account();
                                            let fold = folder();
                                            spawn(async move {
                                                if let Err(err) = run_mail_job(session, move |c| {
                                                    c.mail_flag(acct.as_deref(), &fold, uid, None, Some(next_star))
                                                }).await {
                                                    session.write().toast = Some(format!("Could not update star: {err}"));
                                                }
                                            });
                                        },
                                        Icon { name: "star", size: 14 }
                                        if starred { "Starred" } else { "Star" }
                                    }
                                    button {
                                        class: "btn ghost sm",
                                        r#type: "button",
                                        onclick: move |_| {
                                            let next_seen = unread;
                                            update_message_flags_in_list(&mut messages.write(), uid, "\\Seen", next_seen);
                                            if let Some(ref mut cur) = *selected_msg.write() {
                                                toggle_flag_local(cur, "\\Seen", next_seen);
                                            }
                                            let acct = account();
                                            let fold = folder();
                                            spawn(async move {
                                                if let Err(err) = run_mail_job(session, move |c| {
                                                    c.mail_flag(acct.as_deref(), &fold, uid, Some(next_seen), None)
                                                }).await {
                                                    session.write().toast = Some(format!("Could not update read state: {err}"));
                                                }
                                            });
                                        },
                                        if unread { "Mark read" } else { "Mark unread" }
                                    }

                                    div { class: "mail-move-wrap",
                                        button {
                                            class: "btn ghost sm",
                                            r#type: "button",
                                            onclick: move |_| {
                                                let open = !move_menu_open();
                                                if open && move_target().is_empty()
                                                    && let Some(first) = other_folders.first()
                                                {
                                                    move_target.set(first.clone());
                                                }
                                                move_menu_open.set(open);
                                            },
                                            Icon { name: "folder", size: 14 }
                                            "Move to"
                                        }
                                        if move_menu_open() {
                                            div { class: "mail-move-popover",
                                                select {
                                                    class: "input",
                                                    "aria-label": "Destination folder",
                                                    value: "{move_target}",
                                                    onchange: move |e| move_target.set(e.value()),
                                                    for f in other_folders.iter() {
                                                        option { key: "{f}", value: "{f}", "{f}" }
                                                    }
                                                }
                                                button {
                                                    class: "btn primary sm",
                                                    r#type: "button",
                                                    disabled: move_target().trim().is_empty(),
                                                    onclick: move |_| {
                                                        let dest = move_target().trim().to_owned();
                                                        move_menu_open.set(false);
                                                        let acct = account();
                                                        let fold = folder();
                                                        spawn(async move {
                                                            match run_mail_job(session, move |c| {
                                                                c.mail_move(acct.as_deref(), &fold, uid, &dest)
                                                            }).await {
                                                                Ok(()) => {
                                                                    messages.write().retain(|m| m.uid != uid);
                                                                    selected_msg.set(None);
                                                                    session.write().toast = Some("Message moved.".to_owned());
                                                                }
                                                                Err(err) => {
                                                                    session.write().toast = Some(format!("Move failed: {err}"));
                                                                }
                                                            }
                                                        });
                                                    },
                                                    "Move"
                                                }
                                            }
                                        }
                                    }

                                    if confirm_delete() {
                                        div { class: "mail-delete-confirm",
                                            button {
                                                class: "btn danger sm",
                                                r#type: "button",
                                                onclick: move |_| {
                                                    confirm_delete.set(false);
                                                    let acct = account();
                                                    let fold = folder();
                                                    spawn(async move {
                                                        match run_mail_job(session, move |c| {
                                                            c.mail_delete(acct.as_deref(), &fold, uid)
                                                        }).await {
                                                            Ok(()) => {
                                                                messages.write().retain(|m| m.uid != uid);
                                                                selected_msg.set(None);
                                                                session.write().toast = Some("Message deleted.".to_owned());
                                                            }
                                                            Err(err) => {
                                                                session.write().toast = Some(format!("Delete failed: {err}"));
                                                            }
                                                        }
                                                    });
                                                },
                                                "Confirm delete"
                                            }
                                            button {
                                                class: "btn ghost sm",
                                                r#type: "button",
                                                onclick: move |_| confirm_delete.set(false),
                                                "Cancel"
                                            }
                                        }
                                    } else {
                                        button {
                                            class: "btn ghost sm",
                                            r#type: "button",
                                            onclick: move |_| confirm_delete.set(true),
                                            Icon { name: "trash", size: 14 }
                                            "Delete"
                                        }
                                    }

                                    if has_html {
                                        div { class: "mail-format-pill", role: "group", "aria-label": "Message format",
                                            button {
                                                class: if reader_html_mode() { "seg-btn active" } else { "seg-btn" },
                                                r#type: "button",
                                                onclick: move |_| reader_html_mode.set(true),
                                                "HTML"
                                            }
                                            button {
                                                class: if !reader_html_mode() { "seg-btn active" } else { "seg-btn" },
                                                r#type: "button",
                                                onclick: move |_| reader_html_mode.set(false),
                                                "Plain"
                                            }
                                        }
                                    }

                                    button {
                                        class: "btn ghost sm mail-ask-bot-btn",
                                        r#type: "button",
                                        onclick: move |_| {
                                            let prompt = format_ask_bot_prompt(
                                                account().as_deref(),
                                                &folder(),
                                                &msg_bot,
                                            );
                                            let mut sess = session.write();
                                            sess.composer = prompt;
                                            sess.open_section(Section::Chat);
                                        },
                                        Icon { name: "robot", size: 14 }
                                        "Ask bot"
                                    }
                                }
                            }

                            if msg.has_remote_images && !remote_images_loaded() {
                                div { class: "mail-remote-img-banner",
                                    span { class: "mail-remote-img-text", "Remote images blocked" }
                                    span { class: "muted", "·" }
                                    button {
                                        class: "btn ghost sm",
                                        r#type: "button",
                                        onclick: move |_| {
                                            load_full_message(
                                                session,
                                                account(),
                                                folder(),
                                                uid,
                                                true,
                                                messages,
                                                selected_msg,
                                                remote_images_loaded,
                                                loading_reader,
                                                error,
                                            );
                                        },
                                        "Load remote images"
                                    }
                                }
                            }

                            div { class: "mail-reader-body",
                                if has_html && reader_html_mode() {
                                    if let Some(ref html) = msg.body_html {
                                        iframe {
                                            class: "mail-html-frame",
                                            "sandbox": "",
                                            srcdoc: "{wrap_email_html_srcdoc(html)}",
                                            title: "Email HTML content",
                                        }
                                    }
                                } else {
                                    pre { class: "mail-plain-body", "{msg.body}" }
                                }
                            }

                            if !msg.attachments.is_empty() {
                                div { class: "mail-attachments",
                                    h3 { class: "mail-attachments-title",
                                        Icon { name: "paperclip", size: 14 }
                                        "Attachments ({msg.attachments.len()})"
                                    }
                                    div { class: "mail-attachments-list",
                                        for (idx, att) in msg.attachments.iter().enumerate() {
                                            {
                                                let att: MailAttachmentInfo = att.clone();
                                                let name = if att.name.trim().is_empty() {
                                                    format!("attachment-{idx}")
                                                } else {
                                                    att.name.clone()
                                                };
                                                let name_save = name.clone();
                                                let size_label = format_bytes(att.size);
                                                rsx! {
                                                    div { key: "{idx}-{name}", class: "mail-attachment-row",
                                                        div { class: "mail-attachment-info",
                                                            span { class: "mail-attachment-name", "{name}" }
                                                            span { class: "tiny muted", "{att.content_type} · {size_label}" }
                                                        }
                                                        div { class: "mail-attachment-actions",
                                                            button {
                                                                class: "btn ghost sm",
                                                                r#type: "button",
                                                                onclick: move |_| {
                                                                    let acct = account();
                                                                    let fold = folder();
                                                                    let label = name_save.clone();
                                                                    spawn(async move {
                                                                        match run_mail_job(session, move |c| {
                                                                            c.mail_save_attachment_to_library(acct.as_deref(), &fold, uid, idx)
                                                                        }).await {
                                                                            Ok(_) => {
                                                                                session.write().toast = Some(format!("Saved {label} to Library."));
                                                                            }
                                                                            Err(err) => {
                                                                                session.write().toast = Some(format!("Could not save to Library: {err}"));
                                                                            }
                                                                        }
                                                                    });
                                                                },
                                                                Icon { name: "library", size: 13 }
                                                                "Save to Library"
                                                            }
                                                            button {
                                                                class: "btn ghost sm",
                                                                r#type: "button",
                                                                onclick: move |_| {
                                                                    let acct = account();
                                                                    let fold = folder();
                                                                    let label = name.clone();
                                                                    spawn(async move {
                                                                        match run_mail_job(session, move |c| {
                                                                            c.mail_attachment_bytes(acct.as_deref(), &fold, uid, idx)
                                                                        }).await {
                                                                            Ok(bytes) => {
                                                                                match save_bytes_with_picker(&label, &bytes) {
                                                                                    Ok(Some(path)) => {
                                                                                        session.write().toast = Some(format!("Saved to {path}"));
                                                                                    }
                                                                                    Ok(None) => {}
                                                                                    Err(err) => {
                                                                                        session.write().toast = Some(err);
                                                                                    }
                                                                                }
                                                                            }
                                                                            Err(err) => {
                                                                                session.write().toast = Some(format!("Download failed: {err}"));
                                                                            }
                                                                        }
                                                                    });
                                                                },
                                                                Icon { name: "download", size: 13 }
                                                                "Save to disk"
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
                } else {
                    div { class: "mail-reader-empty muted",
                        Icon { name: "send", size: 24 }
                        p { "Select a message to read." }
                    }
                }
            }
        }

        if let Some(draft) = compose() {
            MailComposeModal {
                session,
                account: account(),
                initial: draft,
                on_close: move |_| compose.set(None),
                on_sent: move |_| {
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
                    } else {
                        load_folder_and_messages(
                            session,
                            account(),
                            folder(),
                            active_search(),
                            folders,
                            messages,
                            selected_msg,
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
                account_status: active_account_status,
                on_close: move |_| signature_modal_open.set(false),
                on_saved: move |(sig, sig_html, logo_hash): (Option<String>, Option<String>, Option<String>)| {
                    signature_modal_open.set(false);
                    if let Some(ref mut st) = *mail_status.write() {
                        let cur = account().unwrap_or_default();
                        if let Some(acct_row) = st.accounts.iter_mut().find(|a| a.account == cur) {
                            acct_row.signature = sig;
                            acct_row.signature_html = sig_html;
                            acct_row.signature_logo_hash = logo_hash;
                        }
                    }
                },
            }
        }
    }
}

#[component]
fn MailComposeModal(
    session: Signal<Session>,
    account: Option<String>,
    initial: ComposeState,
    on_close: EventHandler<()>,
    on_sent: EventHandler<()>,
) -> Element {
    let mut to = use_signal(|| initial.to.clone());
    let mut cc = use_signal(|| initial.cc.clone());
    let mut bcc = use_signal(|| initial.bcc.clone());
    let mut show_cc_bcc = use_signal(|| !initial.cc.is_empty() || !initial.bcc.is_empty());
    let mut subject = use_signal(|| initial.subject.clone());
    let mut body = use_signal(|| initial.body.clone());
    let mut html_mode = use_signal(|| true);
    let mut preview_open = use_signal(|| false);
    let mut link_open = use_signal(|| false);
    let mut link_url = use_signal(|| "https://".to_owned());
    let mut link_label = use_signal(String::new);
    let mut selected_hashes = use_signal(|| initial.selected_hashes.clone());
    let mut library_items = use_signal(Vec::<LibraryItem>::new);
    let mut picker_open = use_signal(|| false);
    let mut loading_library = use_signal(|| false);
    let mut send_later_open = use_signal(|| false);
    let mut custom_send_at = use_signal(String::new);
    let mut sending = use_signal(|| false);
    let mut send_error = use_signal(|| Option::<String>::None);

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
    let submit_send = move |_| {
        let recipients = parse_recipients(&to());
        if recipients.is_empty() {
            send_error.set(Some("Enter at least one recipient.".to_owned()));
            return;
        }
        sending.set(true);
        send_error.set(None);
        send_later_open.set(false);
        let acct = account_for_send.clone();
        let cc_list = parse_recipients(&cc());
        let bcc_list = parse_recipients(&bcc());
        let subj = subject();
        let (plain_body, body_html) = compose_to_body_and_html(&body(), html_mode());
        let hashes: Vec<String> = selected_hashes().into_iter().collect();
        spawn(async move {
            let res = run_mail_job(session, move |c| {
                c.mail_send(
                    acct.as_deref(),
                    &recipients,
                    &cc_list,
                    &bcc_list,
                    &subj,
                    &plain_body,
                    body_html,
                    &hashes,
                )
            })
            .await;
            sending.set(false);
            match res {
                Ok(()) => {
                    session.write().toast = Some("Email sent.".to_owned());
                    on_sent.call(());
                }
                Err(err) => send_error.set(Some(err)),
            }
        });
    };

    let account_for_sched = account.clone();
    let trigger_schedule = Callback::new(move |send_at: i64| {
        let recipients = parse_recipients(&to());
        if recipients.is_empty() {
            send_error.set(Some("Enter at least one recipient.".to_owned()));
            return;
        }
        sending.set(true);
        send_error.set(None);
        send_later_open.set(false);
        let acct = account_for_sched.clone();
        let cc_list = parse_recipients(&cc());
        let bcc_list = parse_recipients(&bcc());
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

    let (_, preview_html_opt) = compose_to_body_and_html(&body(), true);
    let preview_srcdoc = wrap_email_html_srcdoc(preview_html_opt.as_deref().unwrap_or(""));

    rsx! {
        div { class: "mp-sheet-scrim", onclick: move |_| on_close.call(()) }
        aside {
            class: "mp-sheet mail-compose-modal",
            role: "dialog",
            "aria-modal": "true",
            "aria-label": "{modal_title}",
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
                div { class: "field",
                    div { class: "mail-field-head-row",
                        label { class: "field-label", r#for: "mail-compose-to", "To" }
                        button {
                            class: "btn ghost sm",
                            r#type: "button",
                            onclick: move |_| show_cc_bcc.set(!show_cc_bcc()),
                            if show_cc_bcc() { "Hide Cc/Bcc" } else { "Cc / Bcc" }
                        }
                    }
                    input {
                        id: "mail-compose-to",
                        class: "input",
                        r#type: "text",
                        placeholder: "alice@example.com, bob@example.com",
                        disabled: sending(),
                        value: "{to}",
                        oninput: move |e| to.set(e.value()),
                    }
                }
                if show_cc_bcc() {
                    div { class: "mail-cc-bcc-grid",
                        div { class: "field",
                            label { class: "field-label", r#for: "mail-compose-cc", "Cc" }
                            input {
                                id: "mail-compose-cc",
                                class: "input",
                                r#type: "text",
                                placeholder: "cc@example.com",
                                disabled: sending(),
                                value: "{cc}",
                                oninput: move |e| cc.set(e.value()),
                            }
                        }
                        div { class: "field",
                            label { class: "field-label", r#for: "mail-compose-bcc", "Bcc" }
                            input {
                                id: "mail-compose-bcc",
                                class: "input",
                                r#type: "text",
                                placeholder: "bcc@example.com",
                                disabled: sending(),
                                value: "{bcc}",
                                oninput: move |e| bcc.set(e.value()),
                            }
                        }
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
                        onclick: load_library,
                        Icon { name: "paperclip", size: 14 }
                        "Attach from Library ({selected_hashes().len()})"
                    }
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
                                    let checked = selected_hashes().contains(&id);
                                    let label = item.label();
                                    rsx! {
                                        label { key: "{id}", class: "mail-library-item",
                                            input {
                                                r#type: "checkbox",
                                                checked,
                                                onchange: move |_| {
                                                    let mut set = selected_hashes.write();
                                                    if set.contains(&id_toggle) {
                                                        set.remove(&id_toggle);
                                                    } else {
                                                        set.insert(id_toggle.clone());
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
                            onclick: submit_send,
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
}
