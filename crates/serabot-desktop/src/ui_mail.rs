//! Dedicated 3-pane Desktop Mail workspace (0.74.12).
//!
//! Security invariant: all message headers and bodies are untrusted data and
//! are rendered exclusively as Dioxus text nodes — never as HTML.

use std::collections::BTreeSet;

use dioxus::prelude::*;
use serabot_desktop::bot_library::LibraryItem;
use serabot_desktop::session::{Section, Session};
use serabot_desktop::{MailAccountStatus, MailAttachmentInfo, MailMessageInfo, MailStatus};

use crate::ui::Icon;

const DEFAULT_MAIL_LIMIT: u32 = 50;

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
    subject: String,
    body: String,
    selected_hashes: BTreeSet<String>,
}

impl ComposeState {
    fn new_empty() -> Self {
        Self {
            mode: ComposeMode::New,
            to: String::new(),
            subject: String::new(),
            body: String::new(),
            selected_hashes: BTreeSet::new(),
        }
    }

    fn reply_to(msg: &MailMessageInfo) -> Self {
        Self {
            mode: ComposeMode::Reply,
            to: extract_email_addr(&msg.from),
            subject: format_reply_subject(&msg.subject),
            body: format_reply_quote(msg),
            selected_hashes: BTreeSet::new(),
        }
    }

    fn forward_of(msg: &MailMessageInfo) -> Self {
        Self {
            mode: ComposeMode::Forward,
            to: String::new(),
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
fn load_full_message(
    session: Signal<Session>,
    account: Option<String>,
    folder: String,
    uid: u32,
    mut messages: Signal<Vec<MailMessageInfo>>,
    mut selected_msg: Signal<Option<MailMessageInfo>>,
    mut loading_reader: Signal<bool>,
    mut error: Signal<Option<String>>,
) {
    loading_reader.set(true);
    error.set(None);
    spawn(async move {
        let acct_read = account.clone();
        let fold_read = folder.clone();
        let res = run_mail_job(session, move |c| {
            c.mail_message(acct_read.as_deref(), &fold_read, uid)
        })
        .await;
        loading_reader.set(false);
        match res {
            Ok(mut msg) => {
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
    let visible_messages = filter_messages(&messages(), filter());
    let current_uid = selected_msg().map(|m| m.uid);

    rsx! {
        div { class: "mail-shell",
            // Left pane: account switcher, Compose, folders, Manage accounts
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
                            let active = folder().eq_ignore_ascii_case(&f_name);
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
                }

                div { class: "mail-sidebar-foot",
                    button {
                        class: "btn ghost sm mail-manage-link",
                        r#type: "button",
                        onclick: move |_| session.write().open_section(Section::Integrations),
                        Icon { name: "settings", size: 14 }
                        "Manage accounts"
                    }
                }
            }

            // Middle pane: search, All/Unread/Starred filter, Refresh, message list
            section { class: "mail-list-pane", "aria-label": "Messages",
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
                                            load_full_message(
                                                session,
                                                account(),
                                                folder(),
                                                uid,
                                                messages,
                                                selected_msg,
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

            // Right pane: reader
            section { class: "mail-reader-pane", "aria-label": "Message reader",
                if loading_reader() {
                    div { class: "mail-reader-empty muted", "Loading message\u{2026}" }
                } else if let Some(msg) = selected_msg() {
                    {
                        let uid = msg.uid;
                        let unread = is_unread(&msg);
                        let starred = is_starred(&msg);
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

                            div { class: "mail-reader-body",
                                pre { class: "mail-plain-body", "{msg.body}" }
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
    let mut subject = use_signal(|| initial.subject.clone());
    let mut body = use_signal(|| initial.body.clone());
    let mut selected_hashes = use_signal(|| initial.selected_hashes.clone());
    let mut library_items = use_signal(Vec::<LibraryItem>::new);
    let mut picker_open = use_signal(|| false);
    let mut loading_library = use_signal(|| false);
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

    let submit_send = move |_| {
        let recipients = parse_recipients(&to());
        if recipients.is_empty() {
            send_error.set(Some("Enter at least one recipient.".to_owned()));
            return;
        }
        sending.set(true);
        send_error.set(None);
        let acct = account.clone();
        let subj = subject();
        let text = body();
        let hashes: Vec<String> = selected_hashes().into_iter().collect();
        spawn(async move {
            let res = run_mail_job(session, move |c| {
                c.mail_send(acct.as_deref(), &recipients, &subj, &text, &hashes)
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
                    label { class: "field-label", r#for: "mail-compose-to", "To" }
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
                    label { class: "field-label", r#for: "mail-compose-text", "Message" }
                    textarea {
                        id: "mail-compose-text",
                        class: "input mail-compose-textarea",
                        rows: 10,
                        disabled: sending(),
                        value: "{body}",
                        oninput: move |e| body.set(e.value()),
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

                div { class: "card-actions",
                    button {
                        class: "btn ghost",
                        r#type: "button",
                        disabled: sending(),
                        onclick: move |_| on_close.call(()),
                        "Cancel"
                    }
                    button {
                        class: "btn primary",
                        r#type: "button",
                        disabled: sending(),
                        onclick: submit_send,
                        Icon { name: "send", size: 14 }
                        if sending() { "Sending\u{2026}" } else { "Send" }
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
}
