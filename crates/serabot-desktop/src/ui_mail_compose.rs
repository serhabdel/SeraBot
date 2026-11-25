//! Pieces of the 0.74.15 mail composer: the chip-based recipient field and the
//! attachment tray (files from this PC, drag and drop, and the Library).
//!
//! Recipients are kept in the caller's `Signal<String>` as a `", "` joined list, so
//! the rest of the composer does not change; this component turns what is typed
//! into chips and back.

use std::collections::BTreeSet;

use dioxus::prelude::*;
use serabot_desktop::MailContact;
use serabot_desktop::mail_view::{
    add_recipients, check_attachment, drain_plausible, is_complete_address, is_plausible_recipient,
    join_recipients, recipient_label, sender_email, split_recipient_input, suggestion_query,
};
use serabot_desktop::session::Session;
use serde::Deserialize;

use crate::ui::Icon;
use crate::ui_mail::{format_bytes, run_mail_job};

/// A file attached to the message being written.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct AttachedFile {
    pub hash: String,
    pub name: String,
    pub size: u64,
}

const NONE: usize = usize::MAX;

/// "To", "Cc" or "Bcc": chips for finished addresses and one box for the next.
///
/// Enter, comma, semicolon and Tab turn the typed text into a chip; so does a space
/// after a finished address, a paste of several addresses, and leaving the field.
/// Backspace in the empty box removes the last chip. Arrow keys move through
/// suggestions and Enter picks the highlighted one.
#[component]
pub(crate) fn RecipientField(
    session: Signal<Session>,
    id: String,
    label: String,
    value: Signal<String>,
    placeholder: String,
    disabled: bool,
) -> Element {
    let mut value = value;
    let mut draft = use_signal(String::new);
    let mut suggestions = use_signal(Vec::<MailContact>::new);
    let mut active = use_signal(|| NONE);
    let mut seen = use_signal(String::new);
    let mut focused = use_signal(|| false);
    let chips = split_recipient_input(&value());

    let commit = Callback::new(move |text: String| {
        let mut list = split_recipient_input(&value());
        add_recipients(&mut list, &text);
        value.set(join_recipients(&list));
        draft.set(String::new());
        suggestions.set(Vec::new());
        active.set(NONE);
    });

    let fetch = Callback::new(move |text: String| {
        let Some(q) = suggestion_query(&text) else {
            suggestions.set(Vec::new());
            return;
        };
        seen.set(q.clone());
        spawn(async move {
            let q_job = q.clone();
            let res = run_mail_job(session, move |c| c.mail_contacts(&q_job, 6)).await;
            if seen() == q {
                let have: Vec<String> = split_recipient_input(&value())
                    .iter()
                    .map(|e| sender_email(e))
                    .collect();
                let list: Vec<MailContact> = res
                    .unwrap_or_default()
                    .into_iter()
                    .filter(|c| !have.contains(&c.email.to_ascii_lowercase()))
                    .collect();
                suggestions.set(list);
                active.set(NONE);
            }
        });
    });

    let focus_id = id.clone();
    let shown_placeholder = if chips.is_empty() {
        placeholder
    } else {
        String::new()
    };

    rsx! {
        div { class: if focused() { "rcpt-field focused" } else { "rcpt-field" },
            label { class: "field-label", r#for: "{id}", "{label}" }
            div {
                class: "rcpt-box",
                onclick: move |_| {
                    let _ = dioxus::document::eval(&format!(
                        "document.getElementById('{focus_id}')?.focus();"
                    ));
                },
                for (i, entry) in chips.iter().enumerate() {
                    {
                        let entry_rm = entry.clone();
                        let bad = !is_plausible_recipient(entry);
                        let text = recipient_label(entry);
                        rsx! {
                            span {
                                key: "{i}-{entry}",
                                class: if bad { "rcpt-chip invalid" } else { "rcpt-chip" },
                                title: if bad { "This does not look like an email address" } else { "{entry}" },
                                span { class: "rcpt-text", "{text}" }
                                button {
                                    class: "rcpt-x",
                                    r#type: "button",
                                    disabled,
                                    "aria-label": "Remove {text}",
                                    onclick: move |e: MouseEvent| {
                                        e.stop_propagation();
                                        let mut list = split_recipient_input(&value());
                                        list.retain(|x| *x != entry_rm);
                                        value.set(join_recipients(&list));
                                    },
                                    Icon { name: "x", size: 11 }
                                }
                            }
                        }
                    }
                }
                input {
                    id: "{id}",
                    class: "rcpt-input",
                    r#type: "text",
                    autocomplete: "off",
                    placeholder: "{shown_placeholder}",
                    disabled,
                    value: "{draft}",
                    onfocus: move |_| focused.set(true),
                    onblur: move |_| {
                        focused.set(false);
                        let text = draft();
                        if !text.trim().is_empty() {
                            commit.call(text);
                        }
                        suggestions.set(Vec::new());
                    },
                    oninput: move |e| {
                        let v = e.value();
                        if v.contains([',', ';', '\n']) {
                            let (done, rest) = drain_plausible(&v);
                            let mut list = split_recipient_input(&value());
                            for entry in done {
                                add_recipients(&mut list, &entry);
                            }
                            value.set(join_recipients(&list));
                            draft.set(rest.clone());
                            fetch.call(rest);
                        } else {
                            draft.set(v.clone());
                            fetch.call(v);
                        }
                    },
                    onkeydown: move |e: KeyboardEvent| {
                        let mods = e.modifiers();
                        let ctrl = mods.contains(Modifiers::CONTROL) || mods.contains(Modifiers::META);
                        match e.key() {
                            Key::Enter => {
                                if ctrl {
                                    // Ctrl+Enter sends: make sure the typed address counts.
                                    let text = draft();
                                    if !text.trim().is_empty() {
                                        commit.call(text);
                                    }
                                    return;
                                }
                                e.prevent_default();
                                let list = suggestions();
                                if let Some(pick) = list.get(active()) {
                                    commit.call(pick.email.clone());
                                } else {
                                    let text = draft();
                                    if !text.trim().is_empty() {
                                        commit.call(text);
                                    }
                                }
                            }
                            Key::Tab => {
                                let text = draft();
                                if !text.trim().is_empty() {
                                    commit.call(text);
                                }
                            }
                            Key::Backspace => {
                                if draft().is_empty() {
                                    let mut list = split_recipient_input(&value());
                                    if list.pop().is_some() {
                                        value.set(join_recipients(&list));
                                    }
                                }
                            }
                            Key::ArrowDown => {
                                let count = suggestions().len();
                                if count > 0 {
                                    e.prevent_default();
                                    let now = active();
                                    active.set(if now == NONE { 0 } else { (now + 1).min(count - 1) });
                                }
                            }
                            Key::ArrowUp => {
                                if !suggestions().is_empty() {
                                    e.prevent_default();
                                    let now = active();
                                    active.set(if now == NONE || now == 0 { NONE } else { now - 1 });
                                }
                            }
                            Key::Escape => {
                                if !suggestions().is_empty() {
                                    e.stop_propagation();
                                    suggestions.set(Vec::new());
                                    active.set(NONE);
                                }
                            }
                            Key::Character(c) => {
                                let text = draft();
                                // A comma or semicolon ends an address; so does a space after one
                                // that already looks finished.
                                let ends_entry = !text.trim().is_empty()
                                    && (c == ","
                                        || c == ";"
                                        || (c == " "
                                            && !text.contains('<')
                                            && is_complete_address(&text)));
                                if ends_entry {
                                    e.prevent_default();
                                    commit.call(text);
                                }
                            }
                            _ => {}
                        }
                    },
                }
            }
            if focused() && !suggestions().is_empty() {
                div { class: "mail-suggest", role: "listbox", "aria-label": "Suggested recipients",
                    for (i, c) in suggestions().iter().enumerate() {
                        {
                            let email = c.email.clone();
                            let text = if c.name.trim().is_empty() {
                                c.email.clone()
                            } else {
                                format!("{} <{}>", c.name, c.email)
                            };
                            rsx! {
                                button {
                                    key: "{email}",
                                    class: if active() == i { "mail-suggest-item active" } else { "mail-suggest-item" },
                                    r#type: "button",
                                    role: "option",
                                    "aria-selected": if active() == i { "true" } else { "false" },
                                    onmousedown: move |e: MouseEvent| {
                                        e.prevent_default();
                                        commit.call(email.clone());
                                    },
                                    "{text}"
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

/// The attached files as removable chips, an upload counter and the last problem.
#[component]
pub(crate) fn AttachmentTray(
    items: Vec<AttachedFile>,
    busy: usize,
    notice: Option<String>,
    on_remove: EventHandler<String>,
) -> Element {
    if items.is_empty() && busy == 0 && notice.is_none() {
        return rsx! {};
    }
    let total: u64 = items.iter().map(|a| a.size).sum();
    rsx! {
        div { class: "mail-attach-tray", role: "group", "aria-label": "Attachments",
            for item in items.iter() {
                {
                    let hash = item.hash.clone();
                    let size = if item.size > 0 {
                        format_bytes(item.size as usize)
                    } else {
                        String::new()
                    };
                    rsx! {
                        span { key: "{item.hash}", class: "attach-chip", title: "{item.name}",
                            Icon { name: "paperclip", size: 12 }
                            span { class: "attach-name", "{item.name}" }
                            if !size.is_empty() {
                                span { class: "attach-size", "{size}" }
                            }
                            button {
                                class: "rcpt-x",
                                r#type: "button",
                                "aria-label": "Remove attachment {item.name}",
                                onclick: move |_| on_remove.call(hash.clone()),
                                Icon { name: "x", size: 11 }
                            }
                        }
                    }
                }
            }
            if busy > 0 {
                span { class: "attach-chip pending", role: "status",
                    span { class: "spinner" }
                    "Uploading {busy}\u{2026}"
                }
            }
            if total > 0 {
                span { class: "tiny muted attach-total", "{format_bytes(total as usize)} attached" }
            }
            if let Some(text) = notice {
                span { class: "attach-notice", role: "alert", "{text}" }
            }
        }
    }
}

/// Upload files to the member's Library and attach them. Each file is checked
/// against the message limits first; problems show in the tray instead of failing
/// the whole batch.
pub(crate) fn upload_attachments(
    session: Signal<Session>,
    files: Vec<(String, Vec<u8>)>,
    attached: Signal<Vec<AttachedFile>>,
    selected: Signal<BTreeSet<String>>,
    busy: Signal<usize>,
    notice: Signal<Option<String>>,
) {
    for (name, bytes) in files {
        let size = bytes.len() as u64;
        let (count, total) = {
            let list = attached.peek();
            (
                list.len() + *busy.peek(),
                list.iter().map(|a| a.size).sum::<u64>(),
            )
        };
        let mut notice_set = notice;
        if let Err(msg) = check_attachment(count, total, size) {
            notice_set.set(Some(format!("{name}: {msg}")));
            continue;
        }
        let mut busy_set = busy;
        let now = *busy.peek();
        busy_set.set(now + 1);
        notice_set.set(None);
        spawn(async move {
            let job_name = name.clone();
            let res = run_mail_job(session, move |c| c.library_upload(&job_name, &bytes)).await;
            let mut busy_set = busy;
            let now = *busy.peek();
            busy_set.set(now.saturating_sub(1));
            let mut notice_set = notice;
            match res {
                Ok(reply) => match reply.item {
                    Some(item) => attach(attached, selected, item.id, name, size),
                    None => notice_set.set(Some(format!("{name}: the upload did not finish."))),
                },
                Err(err) => notice_set.set(Some(format!("{name}: {err}"))),
            }
        });
    }
}

/// Record an attachment (no duplicates).
pub(crate) fn attach(
    attached: Signal<Vec<AttachedFile>>,
    selected: Signal<BTreeSet<String>>,
    hash: String,
    name: String,
    size: u64,
) {
    let mut attached = attached;
    let mut selected = selected;
    if !attached.peek().iter().any(|a| a.hash == hash) {
        attached.write().push(AttachedFile {
            hash: hash.clone(),
            name,
            size,
        });
    }
    selected.write().insert(hash);
}

/// Drop an attachment from the message (the Library copy stays).
pub(crate) fn detach(
    attached: Signal<Vec<AttachedFile>>,
    selected: Signal<BTreeSet<String>>,
    hash: &str,
) {
    let mut attached = attached;
    let mut selected = selected;
    attached.write().retain(|a| a.hash != hash);
    selected.write().remove(hash);
}

/// Files chosen on this PC, and the names of any too big to attach.
pub(crate) struct PickedFiles {
    pub files: Vec<(String, Vec<u8>)>,
    pub skipped: Vec<String>,
}

/// Choose files from this PC. Windows only; elsewhere it says so.
#[cfg(windows)]
pub(crate) fn pick_local_files() -> Result<PickedFiles, String> {
    let mut picked = PickedFiles {
        files: Vec::new(),
        skipped: Vec::new(),
    };
    let Some(paths) = rfd::FileDialog::new()
        .set_title("Attach files")
        .pick_files()
    else {
        return Ok(picked);
    };
    for path in paths {
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("attachment")
            .to_owned();
        // Check the size first so a huge file is never read into memory.
        let len = std::fs::metadata(&path)
            .map_err(|e| format!("Could not read {name}: {e}"))?
            .len();
        if len > serabot_desktop::mail_view::MAX_ATTACH_TOTAL_BYTES {
            picked.skipped.push(name);
            continue;
        }
        let bytes = std::fs::read(&path).map_err(|e| format!("Could not read {name}: {e}"))?;
        picked.files.push((name, bytes));
    }
    Ok(picked)
}

#[cfg(not(windows))]
pub(crate) fn pick_local_files() -> Result<PickedFiles, String> {
    Err("Choosing files from disk needs the Windows app.".to_owned())
}

/// Bare addresses (`bob@host`) from a recipients field, which may hold
/// `Name <bob@host>` entries.
pub(crate) fn recipient_addresses(field: &str) -> Vec<String> {
    split_recipient_input(field)
        .iter()
        .map(|entry| sender_email(entry))
        .filter(|addr| !addr.is_empty())
        .collect()
}

/// Entries of the three recipient fields that are not addresses.
pub(crate) fn invalid_recipients(fields: &[&str]) -> Vec<String> {
    fields
        .iter()
        .flat_map(|field| split_recipient_input(field))
        .filter(|entry| !is_plausible_recipient(entry))
        .collect()
}

/// One dropped file as the page sends it. `too_big` files are not read.
#[derive(Clone, Debug, Deserialize)]
pub(crate) struct DroppedFile {
    pub name: String,
    #[serde(default)]
    pub data: String,
    #[serde(default)]
    pub too_big: bool,
}

/// Lets files be dropped anywhere on the composer. It marks the composer while a
/// file is dragged over it and forwards dropped files to Rust as base64.
pub(crate) const MAIL_DROP_JS: &str = r#"
(function () {
  if (window.__serabotMailDropCleanup) { window.__serabotMailDropCleanup(); }
  const root = () => document.querySelector('.mail-compose-modal');
  const LIMIT = 18 * 1024 * 1024;
  let depth = 0;
  const hasFiles = (e) => e.dataTransfer && Array.from(e.dataTransfer.types || []).indexOf('Files') >= 0;
  const inside = (e) => { const r = root(); return !!r && r.contains(e.target); };
  const mark = (on) => { const r = root(); if (r) r.classList.toggle('drop-active', on); };
  const onOver = (e) => { if (hasFiles(e) && inside(e)) e.preventDefault(); };
  const onEnter = (e) => { if (hasFiles(e) && inside(e)) { depth += 1; mark(true); e.preventDefault(); } };
  const onLeave = (e) => { if (!hasFiles(e)) return; depth = Math.max(0, depth - 1); if (depth === 0) mark(false); };
  const onDrop = async (e) => {
    if (!hasFiles(e) || !inside(e)) return;
    e.preventDefault(); depth = 0; mark(false);
    for (const file of Array.from(e.dataTransfer.files)) {
      if (file.size > LIMIT) { dioxus.send({ name: file.name, data: '', too_big: true }); continue; }
      const bytes = new Uint8Array(await file.arrayBuffer());
      let bin = '';
      for (let i = 0; i < bytes.length; i += 0x8000) {
        bin += String.fromCharCode.apply(null, bytes.subarray(i, i + 0x8000));
      }
      dioxus.send({ name: file.name, data: btoa(bin) });
    }
  };
  document.addEventListener('dragover', onOver);
  document.addEventListener('dragenter', onEnter);
  document.addEventListener('dragleave', onLeave);
  document.addEventListener('drop', onDrop);
  window.__serabotMailDropCleanup = () => {
    document.removeEventListener('dragover', onOver);
    document.removeEventListener('dragenter', onEnter);
    document.removeEventListener('dragleave', onLeave);
    document.removeEventListener('drop', onDrop);
    mark(false);
    window.__serabotMailDropCleanup = null;
  };
})();
"#;
