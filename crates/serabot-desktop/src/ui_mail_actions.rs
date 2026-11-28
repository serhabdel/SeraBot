//! Mail actions for the 0.74.14 workspace: optimistic flag changes that roll back
//! on failure, archive/delete/move that wait a few seconds so they can be undone,
//! snooze, and the shared Undo toast state.
//!
//! The local list changes first. Remote calls run through `run_mail_job`; when one
//! fails, the rows come back and a toast says why.
//!
//! A deferred move or delete lives in a global signal, not in the window's state, so
//! it still reaches the server if the user leaves Mail during the Undo window. The
//! always-mounted shell runs the timer and the remote calls ([`use_mail_committer`]).
//! Rows whose move failed are parked in [`FAILED`] until a mail window puts them
//! back.

use dioxus::prelude::*;
use serabot_desktop::MailMessageInfo;
use serabot_desktop::mail_view::special_folder;
use serabot_desktop::session::Session;

use crate::ui_mail::{MailCtx, run_mail_job, toggle_flag_local, update_message_flags_in_list};

/// How long archive/delete/move wait before they reach the server.
pub(crate) const DEFER_MS: u32 = 6000;
/// How long a finished action keeps its Undo button.
pub(crate) const TOAST_MS: u32 = 6000;

/// A move or delete that has left the list but not yet the server.
#[derive(Clone, PartialEq)]
pub(crate) struct DeferredMove {
    pub id: u64,
    pub rows: Vec<MailMessageInfo>,
    /// `None` deletes.
    pub dest: Option<String>,
    pub account: Option<String>,
    pub folder: String,
}

static PENDING: GlobalSignal<Option<DeferredMove>> = Signal::global(|| None);
/// Moves whose Undo window is over (or was dismissed), waiting for the shell to send them.
static OUTBOX: GlobalSignal<Vec<DeferredMove>> = Signal::global(Vec::new);
/// Autosaved drafts to delete once their message went out: `(account, folder, uid)`.
static DRAFT_TRASH: GlobalSignal<Vec<(Option<String>, String, u32)>> = Signal::global(Vec::new);
/// Moves the server refused, waiting to be put back into a visible list.
pub(crate) static FAILED: GlobalSignal<Vec<DeferredMove>> = Signal::global(Vec::new);
static DEFER_SEQ: GlobalSignal<u64> = Signal::global(|| 0);

#[derive(Clone, PartialEq)]
pub(crate) enum UndoKind {
    /// Rows already gone from the list; `id` names the [`DeferredMove`].
    Deferred { id: u64 },
    /// A message held by the server for a few seconds before sending.
    Send { scheduled_id: String },
    /// Messages hidden until later; undo wakes them now.
    Snooze {
        rows: Vec<MailMessageInfo>,
        account: Option<String>,
        folder: String,
    },
    /// Flags changed; undo puts back each message's previous flags.
    Flags {
        prev: Vec<(u32, Vec<String>)>,
        account: Option<String>,
        folder: String,
    },
}

#[derive(Clone, PartialEq)]
pub(crate) struct UndoEntry {
    pub id: u64,
    pub label: String,
    pub kind: UndoKind,
}

/// Wait without blocking the UI thread (a timer in the page).
pub(crate) async fn sleep_ms(ms: u32) {
    let mut timer = dioxus::document::eval(&format!("setTimeout(() => dioxus.send(true), {ms});"));
    let _ = timer.recv::<bool>().await;
}

fn flags_contain(flags: &[String], name: &str) -> bool {
    flags
        .iter()
        .any(|f| f.trim_start_matches('\\').eq_ignore_ascii_case(name))
}

fn toast(ctx: MailCtx, text: String) {
    let mut session = ctx.session;
    session.write().toast = Some(text);
}

/// Remove rows from the open list and clear anything that pointed at them.
pub(crate) fn remove_rows(ctx: MailCtx, uids: &[u32]) -> Vec<MailMessageInfo> {
    let MailCtx {
        mut messages,
        mut selected,
        mut selection,
        ..
    } = ctx;
    let removed: Vec<MailMessageInfo> = messages
        .read()
        .iter()
        .filter(|m| uids.contains(&m.uid))
        .cloned()
        .collect();
    messages.write().retain(|m| !uids.contains(&m.uid));
    let open_was_removed = selected
        .read()
        .as_ref()
        .is_some_and(|m| uids.contains(&m.uid));
    if open_was_removed {
        selected.set(None);
    }
    let order: Vec<u32> = messages.read().iter().map(|m| m.uid).collect();
    selection.write().retain_in(&order);
    removed
}

/// Put rows back (newest first) when the user is still looking at the same list.
pub(crate) fn restore_rows(
    ctx: MailCtx,
    rows: Vec<MailMessageInfo>,
    account: &Option<String>,
    folder: &str,
) {
    let MailCtx {
        mut messages,
        account: cur_account,
        folder: cur_folder,
        ..
    } = ctx;
    if *cur_account.peek() != *account || *cur_folder.peek() != folder {
        return;
    }
    let mut list = messages.write();
    for row in rows {
        if !list.iter().any(|m| m.uid == row.uid) {
            list.push(row);
        }
    }
    list.sort_by_key(|m| std::cmp::Reverse(m.uid));
}

/// Take the pending move out of the global slot, if it is the one asked for.
fn take_pending(id: Option<u64>) -> Option<DeferredMove> {
    let current: Option<DeferredMove> = (*PENDING.peek()).clone();
    match current {
        Some(pending) if id.is_none_or(|wanted| wanted == pending.id) => {
            *PENDING.write() = None;
            Some(pending)
        }
        _ => None,
    }
}

/// Send a deferred move or delete to the server. Called from the shell's scope, so it
/// finishes even when the mail window is gone. A failure is reported and parked.
fn commit_remote(session: Signal<Session>, pending: DeferredMove) {
    spawn(async move {
        let uids: Vec<u32> = pending.rows.iter().map(|m| m.uid).collect();
        let (acct, fold, dest) = (
            pending.account.clone(),
            pending.folder.clone(),
            pending.dest.clone(),
        );
        let res = run_mail_job(session, move |c| {
            for uid in &uids {
                match &dest {
                    Some(dest) => c.mail_move(acct.as_deref(), &fold, *uid, dest)?,
                    None => c.mail_delete(acct.as_deref(), &fold, *uid)?,
                }
            }
            Ok(())
        })
        .await;
        if let Err(err) = res {
            let verb = if pending.dest.is_some() {
                "Move"
            } else {
                "Delete"
            };
            let mut session = session;
            session.write().toast = Some(format!("{verb} failed: {err}"));
            FAILED.write().push(pending);
        }
    });
}

/// Put back rows whose move failed, if they belong to the list now on screen.
/// Called by the mail window whenever [`FAILED`] changes.
pub(crate) fn restore_failed(ctx: MailCtx) {
    let failed: Vec<DeferredMove> = std::mem::take(&mut *FAILED.write());
    for item in failed {
        restore_rows(ctx, item.rows, &item.account, &item.folder);
    }
}

/// Settle the current Undo entry now: a deferred move/delete reaches the server.
pub(crate) fn settle(ctx: MailCtx) {
    let mut undo = ctx.undo;
    if undo.peek().is_some() {
        undo.set(None);
    }
    if let Some(pending) = take_pending(None) {
        OUTBOX.write().push(pending);
    }
}

/// Queue the deletion of an autosaved draft that is no longer needed.
pub(crate) fn queue_draft_delete(account: Option<String>, folder: String, uid: u32) {
    DRAFT_TRASH.write().push((account, folder, uid));
}

/// Unread messages across the member's mailboxes, refreshed by [`use_mail_unread_poll`].
pub(crate) static MAIL_UNREAD: GlobalSignal<u32> = Signal::global(|| 0);

/// Checks every connected mailbox for unread inbox mail a few seconds after start and
/// then every 90 seconds, and keeps [`MAIL_UNREAD`] current for the rail badge. A
/// check costs one IMAP search per mailbox and never fetches a message. Failures are
/// ignored: the badge just keeps its last value. Call it from the always-mounted shell.
pub(crate) fn use_mail_unread_poll(session: Signal<Session>) {
    use_future(move || async move {
        sleep_ms(5_000).await;
        loop {
            let res = run_mail_job(session, |c| {
                let status = c.mail_status()?;
                let labels: Vec<Option<String>> = if status.accounts.is_empty() {
                    if status.connected {
                        vec![None]
                    } else {
                        Vec::new()
                    }
                } else {
                    status
                        .accounts
                        .iter()
                        .map(|a| (!a.account.is_empty()).then(|| a.account.clone()))
                        .collect()
                };
                let mut total = 0u32;
                for label in labels {
                    total =
                        total.saturating_add(c.mail_unread(label.as_deref(), "INBOX").unwrap_or(0));
                }
                Ok(total)
            })
            .await;
            if let Ok(total) = res
                && *MAIL_UNREAD.peek() != total
            {
                *MAIL_UNREAD.write() = total;
            }
            sleep_ms(90_000).await;
        }
    });
}

/// Runs the Undo timer and the remote calls for mail actions. Call it once from a
/// component that stays mounted for the whole session (the shell).
pub(crate) fn use_mail_committer(session: Signal<Session>) {
    // A move that just left the list gets its Undo window, then goes out.
    use_effect(move || {
        let pending: Option<DeferredMove> = (*PENDING.read()).clone();
        if let Some(pending) = pending {
            spawn(async move {
                sleep_ms(DEFER_MS).await;
                if let Some(done) = take_pending(Some(pending.id)) {
                    OUTBOX.write().push(done);
                }
            });
        }
    });
    // Settled moves go to the server.
    use_effect(move || {
        if OUTBOX.read().is_empty() {
            return;
        }
        let ready: Vec<DeferredMove> = std::mem::take(&mut *OUTBOX.write());
        for pending in ready {
            commit_remote(session, pending);
        }
    });
    // Leftover autosaves are removed in the background.
    use_effect(move || {
        if DRAFT_TRASH.read().is_empty() {
            return;
        }
        let ready: Vec<(Option<String>, String, u32)> = std::mem::take(&mut *DRAFT_TRASH.write());
        for (account, folder, uid) in ready {
            spawn(async move {
                let _ = run_mail_job(session, move |c| {
                    c.mail_delete(account.as_deref(), &folder, uid)
                })
                .await;
            });
        }
    });
}

/// Show an Undo toast that disappears after `ttl_ms`. An earlier entry is settled
/// first.
pub(crate) fn push_undo(ctx: MailCtx, label: String, kind: UndoKind, ttl_ms: u32) {
    let mut undo = ctx.undo;
    let mut seq = ctx.seq;
    let id = *seq.peek() + 1;
    seq.set(id);
    undo.set(Some(UndoEntry { id, label, kind }));
    spawn(async move {
        sleep_ms(ttl_ms).await;
        let still_ours = undo.peek().as_ref().is_some_and(|e| e.id == id);
        if still_ours {
            undo.set(None);
        }
    });
}

/// Show an Undo toast for something that already happened (flags, snooze, send).
/// A pending move or delete is settled first.
pub(crate) fn announce(ctx: MailCtx, label: String, kind: UndoKind, ttl_ms: u32) {
    settle(ctx);
    push_undo(ctx, label, kind, ttl_ms);
}

/// The Undo button: cancel whatever the toast describes.
pub(crate) fn run_undo(ctx: MailCtx) {
    let mut undo = ctx.undo;
    let current: Option<UndoEntry> = (*undo.peek()).clone();
    let Some(entry) = current else {
        return;
    };
    undo.set(None);
    let session = ctx.session;
    match entry.kind {
        UndoKind::Deferred { id } => {
            if let Some(pending) = take_pending(Some(id)) {
                restore_rows(ctx, pending.rows, &pending.account, &pending.folder);
            }
        }
        UndoKind::Send { scheduled_id } => {
            spawn(async move {
                let res =
                    run_mail_job(session, move |c| c.mail_cancel_scheduled(&scheduled_id)).await;
                let mut session = session;
                session.write().toast = Some(match res {
                    Ok(()) => "Sending cancelled. Nothing was sent.".to_owned(),
                    Err(_) => "Too late to cancel: the message was already sent.".to_owned(),
                });
            });
        }
        UndoKind::Snooze {
            rows,
            account,
            folder,
        } => {
            let (acct, fold) = (account.clone(), folder.clone());
            let uids: Vec<u32> = rows.iter().map(|m| m.uid).collect();
            restore_rows(ctx, rows, &account, &folder);
            spawn(async move {
                let res = run_mail_job(session, move |c| {
                    for uid in &uids {
                        c.mail_unsnooze(acct.as_deref(), &fold, *uid)?;
                    }
                    Ok(())
                })
                .await;
                if let Err(err) = res {
                    let mut session = session;
                    session.write().toast = Some(format!("Could not undo the snooze: {err}"));
                }
            });
        }
        UndoKind::Flags {
            prev,
            account,
            folder,
        } => revert_flags(ctx, prev, account, folder),
    }
}

fn revert_flags(
    ctx: MailCtx,
    prev: Vec<(u32, Vec<String>)>,
    account: Option<String>,
    folder: String,
) {
    let MailCtx {
        session,
        mut messages,
        mut selected,
        ..
    } = ctx;
    for (uid, flags) in &prev {
        if let Some(row) = messages.write().iter_mut().find(|m| m.uid == *uid) {
            row.flags = flags.clone();
        }
        if let Some(cur) = selected.write().as_mut()
            && cur.uid == *uid
        {
            cur.flags = flags.clone();
        }
    }
    spawn(async move {
        let res = run_mail_job(session, move |c| {
            for (uid, flags) in &prev {
                c.mail_flag(
                    account.as_deref(),
                    &folder,
                    *uid,
                    Some(flags_contain(flags, "Seen")),
                    Some(flags_contain(flags, "Flagged")),
                )?;
            }
            Ok(())
        })
        .await;
        if let Err(err) = res {
            let mut session = session;
            session.write().toast = Some(format!("Could not undo: {err}"));
        }
    });
}

/// Change `\Seen` and/or `\Flagged` on several messages. The list updates at once;
/// a failure puts the old flags back. `undo_label` adds an Undo toast.
pub(crate) fn set_flags(
    ctx: MailCtx,
    uids: Vec<u32>,
    seen: Option<bool>,
    flagged: Option<bool>,
    undo_label: Option<String>,
) {
    if uids.is_empty() {
        return;
    }
    let MailCtx {
        session,
        account,
        folder,
        mut messages,
        mut selected,
        ..
    } = ctx;
    let prev: Vec<(u32, Vec<String>)> = messages
        .read()
        .iter()
        .filter(|m| uids.contains(&m.uid))
        .map(|m| (m.uid, m.flags.clone()))
        .collect();
    for uid in &uids {
        if let Some(on) = seen {
            update_message_flags_in_list(&mut messages.write(), *uid, "\\Seen", on);
        }
        if let Some(on) = flagged {
            update_message_flags_in_list(&mut messages.write(), *uid, "\\Flagged", on);
        }
        if let Some(cur) = selected.write().as_mut()
            && cur.uid == *uid
        {
            if let Some(on) = seen {
                toggle_flag_local(cur, "\\Seen", on);
            }
            if let Some(on) = flagged {
                toggle_flag_local(cur, "\\Flagged", on);
            }
        }
    }
    let acct = (*account.peek()).clone();
    let fold = (*folder.peek()).clone();
    if let Some(label) = undo_label {
        announce(
            ctx,
            label,
            UndoKind::Flags {
                prev: prev.clone(),
                account: acct.clone(),
                folder: fold.clone(),
            },
            TOAST_MS,
        );
    }
    let (acct_job, fold_job) = (acct, fold);
    spawn(async move {
        let res = run_mail_job(session, move |c| {
            for uid in &uids {
                c.mail_flag(acct_job.as_deref(), &fold_job, *uid, seen, flagged)?;
            }
            Ok(())
        })
        .await;
        if let Err(err) = res {
            for (uid, flags) in &prev {
                if let Some(row) = messages.write().iter_mut().find(|m| m.uid == *uid) {
                    row.flags = flags.clone();
                }
                if let Some(cur) = selected.write().as_mut()
                    && cur.uid == *uid
                {
                    cur.flags = flags.clone();
                }
            }
            let mut session = session;
            session.write().toast = Some(format!("Could not update messages: {err}"));
        }
    });
}

/// Archive, move or delete. The rows leave the list now and reach the server after
/// [`DEFER_MS`] unless the user presses Undo. `dest` `None` deletes.
pub(crate) fn defer_remove(ctx: MailCtx, uids: Vec<u32>, dest: Option<String>, label: String) {
    if uids.is_empty() {
        return;
    }
    let account = (*ctx.account.peek()).clone();
    let folder = (*ctx.folder.peek()).clone();
    // An older pending action goes first so the two cannot be re-ordered.
    settle(ctx);
    let rows = remove_rows(ctx, &uids);
    if rows.is_empty() {
        return;
    }
    let id = *DEFER_SEQ.peek() + 1;
    *DEFER_SEQ.write() = id;
    *PENDING.write() = Some(DeferredMove {
        id,
        rows,
        dest,
        account,
        folder,
    });
    push_undo(ctx, label, UndoKind::Deferred { id }, DEFER_MS);
}

/// Move messages to the mailbox's Archive folder.
pub(crate) fn archive_messages(ctx: MailCtx, uids: Vec<u32>) {
    if uids.is_empty() {
        return;
    }
    let folders: Vec<String> = (*ctx.folders.peek()).clone();
    let current = (*ctx.folder.peek()).clone();
    let Some(dest) =
        special_folder(&folders, &["archive", "archives", "all mail"]).map(str::to_owned)
    else {
        toast(
            ctx,
            "This mailbox has no Archive folder. Use Move to pick one.".to_owned(),
        );
        return;
    };
    if dest.eq_ignore_ascii_case(&current) {
        toast(ctx, "Those messages are already in Archive.".to_owned());
        return;
    }
    let label = if uids.len() == 1 {
        "Archived".to_owned()
    } else {
        format!("Archived {} messages", uids.len())
    };
    defer_remove(ctx, uids, Some(dest), label);
}

/// Move messages to Trash, or delete them for good when they are already in Trash
/// or the mailbox has none. Either way the Undo window applies.
pub(crate) fn delete_messages(ctx: MailCtx, uids: Vec<u32>) {
    if uids.is_empty() {
        return;
    }
    let folders: Vec<String> = (*ctx.folders.peek()).clone();
    let current = (*ctx.folder.peek()).clone();
    let trash = special_folder(
        &folders,
        &[
            "trash",
            "deleted",
            "deleted items",
            "deleted messages",
            "bin",
        ],
    )
    .map(str::to_owned);
    let count = uids.len();
    match trash {
        Some(dest) if !dest.eq_ignore_ascii_case(&current) => {
            let label = if count == 1 {
                "Moved to Trash".to_owned()
            } else {
                format!("Moved {count} messages to Trash")
            };
            defer_remove(ctx, uids, Some(dest), label);
        }
        _ => {
            let label = if count == 1 {
                "Deleted for good".to_owned()
            } else {
                format!("Deleted {count} messages for good")
            };
            defer_remove(ctx, uids, None, label);
        }
    }
}

/// Hide messages until `wake_at`. The server keeps the snooze; the rows leave the
/// list now and come back on failure.
pub(crate) fn snooze_messages(ctx: MailCtx, uids: Vec<u32>, wake_at: i64, until_label: String) {
    if uids.is_empty() {
        return;
    }
    let session = ctx.session;
    let account = (*ctx.account.peek()).clone();
    let folder = (*ctx.folder.peek()).clone();
    settle(ctx);
    let rows = remove_rows(ctx, &uids);
    if rows.is_empty() {
        return;
    }
    let jobs: Vec<(u32, String, String)> = rows
        .iter()
        .map(|m| (m.uid, m.subject.clone(), m.from.clone()))
        .collect();
    let (acct_job, fold_job) = (account.clone(), folder.clone());
    spawn(async move {
        let res = run_mail_job(session, move |c| {
            for (uid, subject, from) in &jobs {
                c.mail_snooze(acct_job.as_deref(), &fold_job, *uid, wake_at, subject, from)?;
            }
            Ok(())
        })
        .await;
        match res {
            Ok(()) => {
                let count = rows.len();
                let label = if count == 1 {
                    format!("Snoozed until {until_label}")
                } else {
                    format!("Snoozed {count} messages until {until_label}")
                };
                announce(
                    ctx,
                    label,
                    UndoKind::Snooze {
                        rows,
                        account,
                        folder,
                    },
                    TOAST_MS,
                );
            }
            Err(err) => {
                restore_rows(ctx, rows, &account, &folder);
                let mut session = session;
                session.write().toast = Some(format!("Snooze failed: {err}"));
            }
        }
    });
}
