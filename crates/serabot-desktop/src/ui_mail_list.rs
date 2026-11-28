//! List-side pieces of the 0.74.14 mail window: message rows (avatar, snippet, time,
//! hover actions, thread count), loading skeletons, the bulk-action bar, the Snooze
//! dialog, the Undo toast and the shortcut sheet.
//!
//! Message text is rendered as text nodes only; nothing here builds HTML from mail.

use dioxus::prelude::*;
use serabot_desktop::MailMessageInfo;
use serabot_desktop::mail_view::{
    MAIL_SHORTCUTS, avatar_hue, full_date_label, initials, parse_mail_date, sender_email,
    sender_name, short_time_label, snooze_presets,
};

use crate::ui::Icon;
use crate::ui_mail::{
    MailCtx, is_starred, is_unread, normalize_custom_datetime, open_message, run_mail_job,
};
use crate::ui_mail_actions::{
    UndoKind, archive_messages, delete_messages, run_undo, set_flags, settle, snooze_messages,
};

/// Where a row sits inside a conversation.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct ThreadInfo {
    /// Messages in the conversation (1 when it stands alone).
    pub count: usize,
    pub expanded: bool,
    /// An older message shown under its conversation's newest one.
    pub is_child: bool,
}

/// One message in the list.
#[component]
pub(crate) fn MailRow(
    ctx: MailCtx,
    msg: MailMessageInfo,
    thread: ThreadInfo,
    order: Vec<u32>,
    now: i64,
    offset: i64,
    on_toggle_thread: EventHandler<()>,
) -> Element {
    let uid = msg.uid;
    let unread = is_unread(&msg);
    let starred = is_starred(&msg);
    let selected = ctx
        .selected
        .read()
        .as_ref()
        .is_some_and(|open| open.uid == uid);
    let checked = ctx.selection.read().contains(uid);
    let name = {
        let n = sender_name(&msg.from);
        if n.trim().is_empty() {
            "(Unknown sender)".to_owned()
        } else {
            n
        }
    };
    let subject = if msg.subject.trim().is_empty() {
        "(No subject)".to_owned()
    } else {
        msg.subject.clone()
    };
    let when = parse_mail_date(&msg.date)
        .map(|t| short_time_label(t, now, offset))
        .unwrap_or_else(|| msg.date.clone());
    let hue = avatar_hue(&sender_email(&msg.from));
    let letters = initials(&name);
    let snippet = msg.snippet.clone();
    let show_attach = msg.has_attachments || !msg.attachments.is_empty();
    let mut class = String::from("mail-row");
    if selected {
        class.push_str(" selected");
    }
    if unread {
        class.push_str(" unread");
    }
    if checked {
        class.push_str(" checked");
    }
    if thread.is_child {
        class.push_str(" child");
    }
    let mut selection = ctx.selection;

    rsx! {
        div {
            class: "{class}",
            role: "option",
            "aria-selected": if selected || checked { "true" } else { "false" },
            "data-uid": "{uid}",
            onclick: move |e: MouseEvent| {
                let mods = e.modifiers();
                if mods.contains(Modifiers::CONTROL) || mods.contains(Modifiers::META) {
                    selection.write().toggle(uid);
                } else if mods.contains(Modifiers::SHIFT) {
                    selection.write().extend_to(&order, uid);
                } else {
                    open_message(ctx, uid);
                }
            },
            button {
                class: if checked { "mail-check on" } else { "mail-check" },
                r#type: "button",
                title: if checked { "Deselect" } else { "Select" },
                "aria-label": if checked { "Deselect message" } else { "Select message" },
                "aria-pressed": if checked { "true" } else { "false" },
                onclick: move |e: MouseEvent| {
                    e.stop_propagation();
                    selection.write().toggle(uid);
                },
                if checked {
                    Icon { name: "check", size: 12 }
                }
            }
            span { class: "mail-avatar", style: "--hue: {hue}", "aria-hidden": "true", "{letters}" }
            div { class: "mail-row-main",
                div { class: "mail-row-top",
                    span { class: "mail-row-sender", "{name}" }
                    if thread.count > 1 && !thread.is_child {
                        button {
                            class: if thread.expanded { "mail-thread-count open" } else { "mail-thread-count" },
                            r#type: "button",
                            title: if thread.expanded { "Hide the conversation" } else { "Show the conversation" },
                            "aria-label": "Conversation of {thread.count} messages",
                            "aria-expanded": if thread.expanded { "true" } else { "false" },
                            onclick: move |e: MouseEvent| {
                                e.stop_propagation();
                                on_toggle_thread.call(());
                            },
                            "{thread.count}"
                        }
                    }
                    span { class: "mail-row-date", "{when}" }
                }
                div { class: "mail-row-subject", "{subject}" }
                if !snippet.is_empty() {
                    div { class: "mail-row-snippet", "{snippet}" }
                }
            }
            div { class: "mail-row-aside",
                if show_attach {
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
                        set_flags(ctx, vec![uid], None, Some(!starred), None);
                    },
                    Icon { name: "star", size: 14 }
                }
            }
            div { class: "mail-row-actions", role: "group", "aria-label": "Quick actions",
                button {
                    class: "icon-btn sm",
                    r#type: "button",
                    title: "Archive (e)",
                    "aria-label": "Archive message",
                    onclick: move |e: MouseEvent| {
                        e.stop_propagation();
                        archive_messages(ctx, vec![uid]);
                    },
                    Icon { name: "download", size: 14 }
                }
                button {
                    class: "icon-btn sm",
                    r#type: "button",
                    title: "Delete (#)",
                    "aria-label": "Delete message",
                    onclick: move |e: MouseEvent| {
                        e.stop_propagation();
                        delete_messages(ctx, vec![uid]);
                    },
                    Icon { name: "trash", size: 14 }
                }
                button {
                    class: "icon-btn sm",
                    r#type: "button",
                    title: "Snooze (z)",
                    "aria-label": "Snooze message",
                    onclick: move |e: MouseEvent| {
                        e.stop_propagation();
                        let mut snooze_for = ctx.snooze_for;
                        snooze_for.set(Some(vec![uid]));
                    },
                    Icon { name: "history", size: 14 }
                }
                button {
                    class: "icon-btn sm",
                    r#type: "button",
                    title: if unread { "Mark read (u)" } else { "Mark unread (u)" },
                    "aria-label": if unread { "Mark as read" } else { "Mark as unread" },
                    onclick: move |e: MouseEvent| {
                        e.stop_propagation();
                        set_flags(ctx, vec![uid], Some(unread), None, None);
                    },
                    Icon { name: "eye", size: 14 }
                }
            }
        }
    }
}

/// A day heading between groups of rows.
#[component]
pub(crate) fn DayHeader(label: String) -> Element {
    rsx! {
        div { class: "mail-day-header", role: "presentation", "{label}" }
    }
}

/// Placeholder rows while a folder loads.
#[component]
pub(crate) fn RowSkeletons() -> Element {
    rsx! {
        div { class: "mail-skeletons", "aria-busy": "true", "aria-label": "Loading messages",
            for i in 0..8 {
                div { key: "{i}", class: "mail-row skeleton-row",
                    span { class: "skeleton mail-skel-avatar" }
                    div { class: "mail-row-main",
                        span { class: "skeleton mail-skel-line wide" }
                        span { class: "skeleton mail-skel-line" }
                    }
                }
            }
        }
    }
}

/// Actions for the checked messages.
#[component]
pub(crate) fn BulkBar(ctx: MailCtx) -> Element {
    let selection = ctx.selection;
    let count = selection.read().len();
    if count == 0 {
        return rsx! {};
    }
    let mut clear = ctx.selection;
    let mut snooze_for = ctx.snooze_for;
    let noun = if count == 1 { "message" } else { "messages" };

    rsx! {
        div { class: "mail-bulk-bar", role: "toolbar", "aria-label": "Actions for selected messages",
            span { class: "mail-bulk-count", "{count} {noun} selected" }
            button {
                class: "btn ghost sm",
                r#type: "button",
                onclick: move |_| {
                    let uids = selection.read().uids();
                    archive_messages(ctx, uids);
                },
                Icon { name: "download", size: 14 }
                "Archive"
            }
            button {
                class: "btn ghost sm",
                r#type: "button",
                onclick: move |_| {
                    let uids = selection.read().uids();
                    delete_messages(ctx, uids);
                },
                Icon { name: "trash", size: 14 }
                "Delete"
            }
            button {
                class: "btn ghost sm",
                r#type: "button",
                onclick: move |_| {
                    let uids = selection.read().uids();
                    set_flags(ctx, uids, Some(true), None, Some("Marked as read".to_owned()));
                },
                "Mark read"
            }
            button {
                class: "btn ghost sm",
                r#type: "button",
                onclick: move |_| {
                    let uids = selection.read().uids();
                    set_flags(ctx, uids, Some(false), None, Some("Marked as unread".to_owned()));
                },
                "Mark unread"
            }
            button {
                class: "btn ghost sm",
                r#type: "button",
                onclick: move |_| {
                    let uids = selection.read().uids();
                    set_flags(ctx, uids, None, Some(true), Some("Starred".to_owned()));
                },
                Icon { name: "star", size: 14 }
                "Star"
            }
            button {
                class: "btn ghost sm",
                r#type: "button",
                onclick: move |_| {
                    let uids = selection.read().uids();
                    snooze_for.set(Some(uids));
                },
                Icon { name: "history", size: 14 }
                "Snooze"
            }
            button {
                class: "icon-btn sm mail-bulk-clear",
                r#type: "button",
                title: "Clear selection (Esc)",
                "aria-label": "Clear selection",
                onclick: move |_| clear.write().clear(),
                Icon { name: "x", size: 14 }
            }
        }
    }
}

/// "Snooze until…" choices for one or more messages.
#[component]
pub(crate) fn SnoozeDialog(ctx: MailCtx, now: i64, offset: i64) -> Element {
    let mut snooze_for = ctx.snooze_for;
    let Some(uids) = snooze_for.read().clone() else {
        return rsx! {};
    };
    let presets = snooze_presets(now, offset);
    let mut custom = use_signal(String::new);
    let custom_at = normalize_custom_datetime(&custom()).map(|t| t - offset * 60);

    rsx! {
        div {
            class: "mail-dialog-scrim",
            onclick: move |_| snooze_for.set(None),
        }
        div {
            class: "mail-dialog",
            role: "dialog",
            "aria-modal": "true",
            "aria-label": "Snooze until",
            onkeydown: move |e: KeyboardEvent| {
                if e.key() == Key::Escape {
                    snooze_for.set(None);
                }
            },
            h3 { class: "mail-dialog-title", "Snooze until\u{2026}" }
            div { class: "mail-dialog-list",
                for (label, at) in presets.iter().copied() {
                    {
                        let uids_pick = uids.clone();
                        let when = full_date_label(at, offset);
                        rsx! {
                            button {
                                key: "{label}",
                                class: "mail-dialog-item",
                                r#type: "button",
                                onclick: move |_| {
                                    snooze_for.set(None);
                                    snooze_messages(ctx, uids_pick.clone(), at, full_date_label(at, offset));
                                },
                                span { class: "mail-dialog-item-label", "{label}" }
                                span { class: "tiny muted", "{when}" }
                            }
                        }
                    }
                }
            }
            div { class: "mail-dialog-custom",
                label { class: "tiny muted", r#for: "mail-snooze-custom", "Pick a date and time" }
                input {
                    id: "mail-snooze-custom",
                    class: "input",
                    r#type: "datetime-local",
                    value: "{custom}",
                    oninput: move |e| custom.set(e.value()),
                }
                button {
                    class: "btn primary sm",
                    r#type: "button",
                    disabled: custom_at.is_none_or(|t| t <= now),
                    onclick: move |_| {
                        if let Some(at) = custom_at
                            && at > now
                        {
                            snooze_for.set(None);
                            snooze_messages(ctx, uids.clone(), at, full_date_label(at, offset));
                        }
                    },
                    "Snooze"
                }
            }
            div { class: "mail-dialog-foot",
                button {
                    class: "btn ghost sm",
                    r#type: "button",
                    onclick: move |_| snooze_for.set(None),
                    "Cancel"
                }
            }
        }
    }
}

/// The bar at the bottom of the mail window while something can still be undone.
#[component]
pub(crate) fn UndoBar(ctx: MailCtx) -> Element {
    let Some(entry) = ctx.undo.read().clone() else {
        return rsx! {};
    };
    let send_now_id = match &entry.kind {
        UndoKind::Send { scheduled_id } => Some(scheduled_id.clone()),
        _ => None,
    };
    rsx! {
        div { class: "mail-undo", role: "status", "aria-live": "polite",
            span { class: "mail-undo-label", "{entry.label}" }
            if let Some(sched_id) = send_now_id {
                button {
                    class: "btn primary sm mail-undo-btn",
                    r#type: "button",
                    onclick: move |_| {
                        let id = sched_id.clone();
                        settle(ctx);
                        spawn(async move {
                            let res = run_mail_job(ctx.session, move |c| c.mail_send_now(&id)).await;
                            match res {
                                Ok(()) => ctx.session.write().toast = Some("Sending email now\u{2026}".to_owned()),
                                Err(err) => ctx.session.write().toast = Some(err),
                            }
                        });
                    },
                    Icon { name: "send", size: 13 }
                    "Send now"
                }
            }
            button {
                class: "btn ghost sm mail-undo-btn",
                r#type: "button",
                onclick: move |_| run_undo(ctx),
                Icon { name: "undo", size: 14 }
                "Undo"
            }
            button {
                class: "icon-btn sm",
                r#type: "button",
                title: "Dismiss",
                "aria-label": "Dismiss",
                onclick: move |_| settle(ctx),
                Icon { name: "x", size: 14 }
            }
        }
    }
}

/// The keyboard shortcut cheat sheet.
#[component]
pub(crate) fn ShortcutsDialog(open: Signal<bool>) -> Element {
    let mut open = open;
    if !open() {
        return rsx! {};
    }
    rsx! {
        div { class: "mail-dialog-scrim", onclick: move |_| open.set(false) }
        div {
            class: "mail-dialog wide",
            role: "dialog",
            "aria-modal": "true",
            "aria-label": "Mail keyboard shortcuts",
            onkeydown: move |e: KeyboardEvent| {
                if e.key() == Key::Escape {
                    open.set(false);
                }
            },
            h3 { class: "mail-dialog-title", "Mail shortcuts" }
            dl { class: "mail-shortcut-list",
                for (keys, what) in MAIL_SHORTCUTS.iter().copied() {
                    div { key: "{keys}", class: "mail-shortcut-row",
                        dt { kbd { class: "palette-kbd", "{keys}" } }
                        dd { "{what}" }
                    }
                }
            }
            div { class: "mail-dialog-foot",
                button {
                    class: "btn ghost sm",
                    r#type: "button",
                    onclick: move |_| open.set(false),
                    "Close"
                }
            }
        }
    }
}
