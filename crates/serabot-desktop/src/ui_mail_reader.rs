//! The reading pane of the 0.74.14 mail window: sender header with avatar, one
//! action toolbar, unsubscribe and remote-image banners, the Ask-bot strip, the
//! body (sandboxed iframe or plain text) and attachments.
//!
//! Security invariant carried over from `ui_mail`: mail headers and plain bodies are
//! rendered as text nodes only, and HTML only inside `<iframe sandbox="" srcdoc=…>`.

use dioxus::prelude::*;
use serabot_desktop::mail_view::{
    avatar_hue, full_date_label, initials, parse_mail_date, sender_email, sender_name,
};
use serabot_desktop::{MailAttachmentInfo, MailMessageInfo};

use crate::ui::Icon;
use crate::ui_mail::{
    ComposeState, MailCtx, format_ask_bot_prompt, format_bytes, is_starred, is_unread,
    open_message_with_remote_images, run_mail_job, save_bytes_with_picker,
    wrap_email_html_srcdoc_with_remote,
};
use crate::ui_mail_actions::{archive_messages, defer_remove, delete_messages, set_flags};
use serabot_desktop::session::Section;

/// What the Ask-bot strip can do with the open message: (button, instruction).
const ASK_TASKS: &[(&str, &str)] = &[
    (
        "Summarize",
        "Summarize this email in a few bullet points and tell me whether I need to act.",
    ),
    (
        "Draft a reply",
        "Read this email and draft a short, polite reply for me to review. Do not send it.",
    ),
    (
        "Draft HTML reply",
        "Read this email and draft a polished HTML reply with my signature using mail_draft for me to review.",
    ),
    (
        "Extract tasks",
        "Read this email and list any tasks or deadlines in it. Offer to add them to my tasks and wait for my answer.",
    ),
    (
        "Suggest replies",
        "Read this email and give me three short reply options I can pick from: agree, decline, and ask for more time.",
    ),
];

fn open_chat_with(ctx: MailCtx, instruction: &str, msg: &MailMessageInfo) {
    let prompt = format!(
        "{instruction}\n\n{}",
        format_ask_bot_prompt((*ctx.account.peek()).as_deref(), &ctx.folder.peek(), msg)
    );
    let mut session = ctx.session;
    let mut guard = session.write();
    guard.composer = prompt;
    guard.open_section(Section::Chat);
}

#[component]
pub(crate) fn MailReader(
    ctx: MailCtx,
    msg: MailMessageInfo,
    own_address: String,
    offset: i64,
) -> Element {
    let session = ctx.session;
    let uid = msg.uid;
    let unread = is_unread(&msg);
    let starred = is_starred(&msg);
    let has_html = msg
        .body_html
        .as_deref()
        .is_some_and(|h| !h.trim().is_empty());
    let name = sender_name(&msg.from);
    let email = sender_email(&msg.from);
    let hue = avatar_hue(&email);
    let letters = initials(&name);
    let when = parse_mail_date(&msg.date)
        .map(|t| full_date_label(t, offset))
        .unwrap_or_else(|| msg.date.clone());
    let other_folders: Vec<String> = ctx
        .folders
        .read()
        .iter()
        .filter(|f| !f.eq_ignore_ascii_case(&ctx.folder.read()))
        .cloned()
        .collect();

    let move_choices = other_folders.clone();
    let mut move_open = use_signal(|| false);
    let mut move_target = use_signal(String::new);
    let mut ask_open = ctx.ask_open;
    let mut unsub_confirm = use_signal(|| false);
    let mut reader_html = ctx.reader_html;
    let mut compose = ctx.compose;
    let mut snooze_for = ctx.snooze_for;

    let msg_reply = msg.clone();
    let msg_reply_all = msg.clone();
    let msg_fwd = msg.clone();
    let own_for_reply_all = own_address;

    rsx! {
        div { class: "mail-reader-head",
            h2 { class: "mail-reader-subject",
                if msg.subject.trim().is_empty() {
                    "(No subject)"
                } else {
                    "{msg.subject}"
                }
            }
            div { class: "mail-sender-row",
                span { class: "mail-avatar lg", style: "--hue: {hue}", "aria-hidden": "true", "{letters}" }
                div { class: "mail-sender-text",
                    div { class: "mail-sender-name", "{name}" }
                    div { class: "mail-sender-sub", "{email} \u{b7} {when}" }
                }
            }
            if !msg.to.trim().is_empty() || !msg.cc.trim().is_empty() {
                div { class: "mail-recipients",
                    if !msg.to.trim().is_empty() {
                        span { class: "mail-recipients-line",
                            span { class: "mail-meta-key", "To" }
                            span { class: "mail-meta-val", "{msg.to}" }
                        }
                    }
                    if !msg.cc.trim().is_empty() {
                        span { class: "mail-recipients-line",
                            span { class: "mail-meta-key", "Cc" }
                            span { class: "mail-meta-val", "{msg.cc}" }
                        }
                    }
                }
            }

            div { class: "mail-toolbar", role: "toolbar", "aria-label": "Message actions",
                button {
                    class: "btn ghost sm",
                    r#type: "button",
                    title: "Reply (r)",
                    onclick: move |_| compose.set(Some(ComposeState::reply_to(&msg_reply))),
                    Icon { name: "undo", size: 14 }
                    "Reply"
                }
                button {
                    class: "btn ghost sm",
                    r#type: "button",
                    title: "Reply all (a)",
                    onclick: move |_| {
                        compose.set(Some(ComposeState::reply_all(&msg_reply_all, &own_for_reply_all)));
                    },
                    Icon { name: "users", size: 14 }
                    "Reply all"
                }
                button {
                    class: "btn ghost sm",
                    r#type: "button",
                    title: "Forward (f)",
                    onclick: move |_| compose.set(Some(ComposeState::forward_of(&msg_fwd))),
                    Icon { name: "external", size: 14 }
                    "Forward"
                }
                span { class: "mail-toolbar-sep", "aria-hidden": "true" }
                button {
                    class: "icon-btn",
                    r#type: "button",
                    title: "Archive (e)",
                    "aria-label": "Archive",
                    onclick: move |_| archive_messages(ctx, vec![uid]),
                    Icon { name: "download", size: 15 }
                }
                button {
                    class: "icon-btn",
                    r#type: "button",
                    title: "Delete (#)",
                    "aria-label": "Delete",
                    onclick: move |_| delete_messages(ctx, vec![uid]),
                    Icon { name: "trash", size: 15 }
                }
                button {
                    class: "icon-btn",
                    r#type: "button",
                    title: "Snooze (z)",
                    "aria-label": "Snooze",
                    onclick: move |_| snooze_for.set(Some(vec![uid])),
                    Icon { name: "history", size: 15 }
                }
                button {
                    class: if starred { "icon-btn mail-starred-active" } else { "icon-btn" },
                    r#type: "button",
                    title: if starred { "Unstar (s)" } else { "Star (s)" },
                    "aria-label": if starred { "Unstar" } else { "Star" },
                    "aria-pressed": if starred { "true" } else { "false" },
                    onclick: move |_| set_flags(ctx, vec![uid], None, Some(!starred), None),
                    Icon { name: "star", size: 15 }
                }
                button {
                    class: "icon-btn",
                    r#type: "button",
                    title: if unread { "Mark read (u)" } else { "Mark unread (u)" },
                    "aria-label": if unread { "Mark as read" } else { "Mark as unread" },
                    onclick: move |_| set_flags(ctx, vec![uid], Some(unread), None, None),
                    Icon { name: "eye", size: 15 }
                }
                div { class: "mail-move-wrap",
                    button {
                        class: "icon-btn",
                        r#type: "button",
                        title: "Move to a folder",
                        "aria-label": "Move to a folder",
                        "aria-expanded": if move_open() { "true" } else { "false" },
                        onclick: move |_| {
                            let open = !move_open();
                            if open && move_target().is_empty()
                                && let Some(first) = other_folders.first()
                            {
                                move_target.set(first.clone());
                            }
                            move_open.set(open);
                        },
                        Icon { name: "folder", size: 15 }
                    }
                    if move_open() {
                        div { class: "mail-move-popover",
                            select {
                                class: "input",
                                "aria-label": "Destination folder",
                                value: "{move_target}",
                                onchange: move |e| move_target.set(e.value()),
                                for f in move_choices.iter() {
                                    option { key: "{f}", value: "{f}", "{f}" }
                                }
                            }
                            button {
                                class: "btn primary sm",
                                r#type: "button",
                                disabled: move_target().trim().is_empty(),
                                onclick: move |_| {
                                    let dest = move_target().trim().to_owned();
                                    move_open.set(false);
                                    defer_remove(
                                        ctx,
                                        vec![uid],
                                        Some(dest.clone()),
                                        format!("Moved to {dest}"),
                                    );
                                },
                                "Move"
                            }
                        }
                    }
                }
                span { class: "mail-toolbar-grow" }
                if has_html {
                    div { class: "mail-format-pill", role: "group", "aria-label": "Message format",
                        button {
                            class: if reader_html() { "seg-btn active" } else { "seg-btn" },
                            r#type: "button",
                            onclick: move |_| reader_html.set(true),
                            "HTML"
                        }
                        button {
                            class: if !reader_html() { "seg-btn active" } else { "seg-btn" },
                            r#type: "button",
                            onclick: move |_| reader_html.set(false),
                            "Plain"
                        }
                    }
                }
                button {
                    class: if ask_open() { "btn ghost sm mail-ask-bot-btn active" } else { "btn ghost sm mail-ask-bot-btn" },
                    r#type: "button",
                    "aria-expanded": if ask_open() { "true" } else { "false" },
                    onclick: move |_| ask_open.set(!ask_open()),
                    Icon { name: "robot", size: 14 }
                    "Ask bot"
                }
            }

            if ask_open() {
                div { class: "mail-ask-strip", role: "group", "aria-label": "Ask a bot about this message",
                    for (label, instruction) in ASK_TASKS.iter().copied() {
                        {
                            let msg_ask = msg.clone();
                            rsx! {
                                button {
                                    key: "{label}",
                                    class: "chip mail-ask-chip",
                                    r#type: "button",
                                    onclick: move |_| open_chat_with(ctx, instruction, &msg_ask),
                                    "{label}"
                                }
                            }
                        }
                    }
                    span { class: "tiny muted", "Opens a chat with the message filled in. Nothing is sent until you press Send." }
                }
            }
        }

        if let Some(unsub) = msg.unsubscribe.clone() {
            div { class: "mail-unsub-banner",
                span { class: "mail-unsub-text", "This looks like a mailing list." }
                if let Some(target) = unsub.mailto.clone() {
                    if unsub_confirm() {
                        span { class: "tiny muted", "Send an unsubscribe request?" }
                        button {
                            class: "btn primary sm",
                            r#type: "button",
                            onclick: move |_| {
                                unsub_confirm.set(false);
                                let acct = (*ctx.account.peek()).clone();
                                let target = target.clone();
                                spawn(async move {
                                    let res = run_mail_job(session, move |c| {
                                        c.mail_unsubscribe(acct.as_deref(), &target)
                                    })
                                    .await;
                                    let mut session = session;
                                    session.write().toast = Some(match res {
                                        Ok(()) => "Unsubscribe request sent.".to_owned(),
                                        Err(err) => format!("Could not unsubscribe: {err}"),
                                    });
                                });
                            },
                            "Send request"
                        }
                        button {
                            class: "btn ghost sm",
                            r#type: "button",
                            onclick: move |_| unsub_confirm.set(false),
                            "Cancel"
                        }
                    } else {
                        button {
                            class: "btn ghost sm",
                            r#type: "button",
                            onclick: move |_| unsub_confirm.set(true),
                            "Unsubscribe"
                        }
                    }
                } else if let Some(url) = unsub.url.clone() {
                    button {
                        class: "btn ghost sm",
                        r#type: "button",
                        title: "{url}",
                        onclick: move |_| {
                            let _ = webbrowser::open(&url);
                        },
                        Icon { name: "external", size: 13 }
                        "Open unsubscribe page"
                    }
                }
            }
        }

        if msg.has_remote_images && !*ctx.remote_loaded.read() {
            div { class: "mail-remote-img-banner",
                span { class: "mail-remote-img-text", "Remote images blocked" }
                span { class: "muted", "\u{b7}" }
                button {
                    class: "btn ghost sm",
                    r#type: "button",
                    onclick: move |_| open_message_with_remote_images(ctx, uid),
                    "Load remote images"
                }
            }
        }

        div { class: "mail-reader-body",
            if has_html && reader_html() {
                if let Some(ref html) = msg.body_html {
                    div { class: "mail-paper-card",
                        iframe {
                            class: "mail-html-frame",
                            "sandbox": "",
                            srcdoc: "{wrap_email_html_srcdoc_with_remote(html, *ctx.remote_loaded.read())}",
                            title: "Email HTML content",
                        }
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
                                        span { class: "tiny muted", "{att.content_type} \u{b7} {size_label}" }
                                    }
                                    div { class: "mail-attachment-actions",
                                        button {
                                            class: "btn ghost sm",
                                            r#type: "button",
                                            onclick: move |_| {
                                                let acct = (*ctx.account.peek()).clone();
                                                let fold = (*ctx.folder.peek()).clone();
                                                let label = name_save.clone();
                                                spawn(async move {
                                                    let res = run_mail_job(session, move |c| {
                                                        c.mail_save_attachment_to_library(acct.as_deref(), &fold, uid, idx)
                                                    })
                                                    .await;
                                                    let mut session = session;
                                                    session.write().toast = Some(match res {
                                                        Ok(_) => format!("Saved {label} to Library."),
                                                        Err(err) => format!("Could not save to Library: {err}"),
                                                    });
                                                });
                                            },
                                            Icon { name: "library", size: 13 }
                                            "Save to Library"
                                        }
                                        button {
                                            class: "btn ghost sm",
                                            r#type: "button",
                                            onclick: move |_| {
                                                let acct = (*ctx.account.peek()).clone();
                                                let fold = (*ctx.folder.peek()).clone();
                                                let label = name.clone();
                                                spawn(async move {
                                                    let res = run_mail_job(session, move |c| {
                                                        c.mail_attachment_bytes(acct.as_deref(), &fold, uid, idx)
                                                    })
                                                    .await;
                                                    let mut session = session;
                                                    session.write().toast = Some(match res {
                                                        Ok(bytes) => match save_bytes_with_picker(&label, &bytes) {
                                                            Ok(Some(path)) => format!("Saved to {path}"),
                                                            Ok(None) => return,
                                                            Err(err) => err,
                                                        },
                                                        Err(err) => format!("Download failed: {err}"),
                                                    });
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
