//! Chat files (0.18, extended in 0.19 and 0.44): attachment chips on the
//! composer and on sent messages, picture previews with a full-size view,
//! media cards for files the bots produce, and the Right Sidebar Visual
//! Preview Dock (`PreviewDock`) with rich renderers for images, PDFs,
//! PowerPoint decks, Excel workbooks, Word documents, and Markdown.
//!
//! Downloads go through the authenticated, user-scoped API; nothing here
//! touches a server path. Pasted clipboard files arrive through a small JS
//! bridge as base64 and are decoded here before they join the upload queue.

use crate::ui::Icon;
use base64::Engine;
use dioxus::prelude::*;
use serabot_desktop::preview::{DocBlockVisual, PreviewKind, PreviewVisual, SlideShapeKind};
use serabot_desktop::session::{Session, humanize_label};
use serde::Deserialize;

/// How many of the newest produced files get a card under the transcript.
const MAX_CARDS: usize = 6;

/// Listens for pastes into the composer and forwards any files on the
/// clipboard to Rust as base64. Text pastes are left alone. The listener is
/// installed once per page; each run points it at the newest channel.
pub const PASTE_BRIDGE_JS: &str = r#"
window.__serabotDx = dioxus;
if (!window.__serabotPasteBridge) {
  window.__serabotPasteBridge = true;
  async function forwardFiles(files) {
    for (const file of files) {
      // Oversized files are not read; an empty payload is refused in Rust.
      if (file.size > 25 * 1024 * 1024) {
        window.__serabotDx.send({ name: file.name || 'file', data: '' });
        continue;
      }
      const bytes = new Uint8Array(await file.arrayBuffer());
      let binary = '';
      for (let i = 0; i < bytes.length; i += 0x8000) {
        binary += String.fromCharCode.apply(null, bytes.subarray(i, i + 0x8000));
      }
      window.__serabotDx.send({ name: file.name || 'pasted-image.png', data: btoa(binary) });
    }
  }
  document.addEventListener('paste', async (event) => {
    const target = event.target;
    if (!target || target.id !== 'composer-input') return;
    const files = Array.from((event.clipboardData && event.clipboardData.files) || []);
    if (files.length === 0) return;
    event.preventDefault();
    await forwardFiles(files);
  });
  let dragDepth = 0;
  function setDropActive(on) {
    const chat = document.querySelector('.chat');
    if (chat) chat.classList.toggle('drop-active', on);
  }
  document.addEventListener('dragenter', (event) => {
    if (!event.dataTransfer || !Array.from(event.dataTransfer.types || []).includes('Files')) return;
    dragDepth += 1;
    setDropActive(true);
  });
  document.addEventListener('dragover', (event) => {
    if (!event.dataTransfer || !Array.from(event.dataTransfer.types || []).includes('Files')) return;
    event.preventDefault();
    event.dataTransfer.dropEffect = 'copy';
  });
  document.addEventListener('dragleave', () => {
    dragDepth = Math.max(0, dragDepth - 1);
    if (dragDepth === 0) setDropActive(false);
  });
  document.addEventListener('drop', async (event) => {
    dragDepth = 0;
    setDropActive(false);
    const files = Array.from((event.dataTransfer && event.dataTransfer.files) || []);
    if (files.length === 0) return;
    if (!document.getElementById('composer-input')) return;
    event.preventDefault();
    await forwardFiles(files);
  });
}
"#;

/// Remembers the composer caret across streaming repaints.
pub const COMPOSER_CARET_SETUP_JS: &str = r#"
(function () {
  if (window.__serabotComposerCaret) return;
  window.__serabotComposerCaret = true;
  window.__serabotComposerGen = 0;
  const remember = () => {
    const el = document.getElementById('composer-input');
    if (!el || el !== document.activeElement) return;
    const start = el.selectionStart;
    const end = el.selectionEnd;
    if (start == null || end == null) return;
    window.__serabotComposerSel = [start, end];
    window.__serabotComposerGen = (window.__serabotComposerGen || 0) + 1;
  };
  document.addEventListener('selectionchange', remember);
  document.addEventListener('input', remember, true);
  document.addEventListener('keydown', (e) => {
    if (e.target && e.target.id === 'composer-input') {
      window.__serabotComposerGen = (window.__serabotComposerGen || 0) + 1;
    }
  }, true);
})();
"#;

/// After a streaming repaint, put the caret back when the composer still has focus.
pub const COMPOSER_CARET_BRIDGE_JS: &str = r#"
(function () {
  const sel = window.__serabotComposerSel;
  if (!Array.isArray(sel) || sel.length !== 2) return;
  const el = document.getElementById('composer-input');
  if (!el || el !== document.activeElement) return;
  const gen = window.__serabotComposerGen || 0;
  requestAnimationFrame(() => {
    if (document.activeElement !== el) return;
    if ((window.__serabotComposerGen || 0) !== gen) return;
    if (el.selectionStart === sel[0] && el.selectionEnd === sel[1]) return;
    try {
      el.setSelectionRange(sel[0], sel[1]);
    } catch (_) {}
  });
})();
"#;

/// Waits 1.5 seconds, then tells Rust, so a "Copied" label can reset.
pub const CONFIRM_RESET_JS: &str =
    "await new Promise((resolve) => setTimeout(resolve, 1500)); dioxus.send(true);";

/// One pasted file as the bridge sends it.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct PastedFile {
    pub name: String,
    pub data: String,
}

/// Decode a pasted file. Bad base64 yields no bytes, which the composer
/// refuses with its usual size message.
pub fn decode_pasted(file: PastedFile) -> (String, Vec<u8>) {
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(file.data.as_bytes())
        .unwrap_or_default();
    (file.name, bytes)
}

/// Script that copies `text` to the clipboard. It falls back to the classic
/// selection copy when the async clipboard API is unavailable in the webview.
pub fn copy_script(text: &str) -> String {
    let literal = serde_json::to_string(text).unwrap_or_else(|_| "\"\"".into());
    format!(
        "(async (t) => {{ try {{ await navigator.clipboard.writeText(t); }} catch (e) {{ \
         const a = document.createElement('textarea'); a.value = t; a.setAttribute('readonly', ''); \
         a.style.position = 'fixed'; a.style.opacity = '0'; document.body.appendChild(a); a.select(); \
         document.execCommand('copy'); a.remove(); }} }})({literal});"
    )
}

/// `1536` -> `1.5 KB`.
pub fn size_label(bytes: u64) -> String {
    const KB: f64 = 1024.0;
    let value = bytes as f64;
    if value < KB {
        format!("{bytes} B")
    } else if value < KB * KB {
        format!("{:.1} KB", value / KB)
    } else {
        format!("{:.1} MB", value / (KB * KB))
    }
}

/// True for names that are pictures (screenshots, generated images).
pub fn is_image_name(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    [".png", ".jpg", ".jpeg", ".gif", ".webp", ".bmp", ".svg"]
        .iter()
        .any(|ext| lower.ends_with(ext))
}

/// Real vendored file-type icon `data:` URI for `filename`, when its extension
/// is recognized.
pub fn file_icon_src(filename: &str) -> Option<String> {
    let (_, ext) = filename.rsplit_once('.')?;
    if ext.is_empty() {
        return None;
    }
    serabot_desktop::file_icons::file_ext_image_src(ext)
}

/// Determine the preview kind from filename or MIME type.
fn entry_preview_kind(entry: &serabot_desktop::LibraryEntry) -> Option<PreviewKind> {
    serabot_desktop::preview::preview_kind_for_entry(
        entry.filename.as_deref(),
        entry.mime.as_deref(),
    )
}

/// Convert a 0-based column index into an Excel column label (`0 -> A`, `25 -> Z`, `26 -> AA`).
pub fn excel_col_label(mut idx: usize) -> String {
    let mut out = Vec::new();
    loop {
        let rem = idx % 26;
        out.push((b'A' + rem as u8) as char);
        if idx < 26 {
            break;
        }
        idx = (idx / 26) - 1;
    }
    out.into_iter().rev().collect()
}

/// Heuristic for right-aligning numeric/currency/percentage spreadsheet cells.
fn is_numeric_cell(cell: &str) -> bool {
    let s = cell.trim();
    if s.is_empty() {
        return false;
    }
    let stripped = s
        .trim_start_matches(['$', '€', '£', '¥', '+', '-'])
        .trim_end_matches('%')
        .replace([',', ' '], "");
    !stripped.is_empty() && stripped.parse::<f64>().is_ok()
}

/// A picture shown at full size over the transcript. Clicking anywhere, or
/// pressing Escape, closes it.
#[component]
pub fn Lightbox(src: String, alt: String, onclose: EventHandler<()>) -> Element {
    rsx! {
        div {
            class: "lightbox",
            role: "dialog",
            "aria-modal": "true",
            "aria-label": "{alt}",
            tabindex: -1,
            onmounted: move |event| async move {
                let _ = event.data().set_focus(true).await;
            },
            onclick: move |_| onclose.call(()),
            onkeydown: move |event: KeyboardEvent| {
                if event.key() == Key::Escape {
                    event.stop_propagation();
                    onclose.call(());
                }
            },
            button {
                class: "btn icon ghost lightbox-close",
                r#type: "button",
                "aria-label": "Close image",
                onclick: move |event| {
                    event.stop_propagation();
                    onclose.call(());
                },
                Icon { name: "x", size: 16 }
            }
            img { class: "lightbox-image", src: "{src}", alt: "{alt}" }
        }
    }
}

/// Chips under a sent user message. Pictures with a preview show the
/// thumbnail; a click opens it full size.
#[component]
pub fn MessageFiles(session: Signal<Session>, id: String) -> Element {
    let mut open = use_signal(|| Option::<(String, String)>::None);
    let files = session
        .read()
        .state
        .messages
        .iter()
        .find(|message| message.id == id)
        .map(|message| message.attachments.clone())
        .unwrap_or_default();
    if files.is_empty() {
        return rsx! {};
    }
    let thumbs: Vec<Option<String>> = {
        let state = session.read();
        files
            .iter()
            .map(|file| state.attachment_thumbs.get(&file.id).cloned())
            .collect()
    };
    rsx! {
        div { class: "file-chips",
            for (file, thumb) in files.into_iter().zip(thumbs) {
                span { key: "{file.id}", class: "file-chip", title: "{file.filename}",
                    if let Some(url) = thumb.clone() {
                        button {
                            class: "file-thumb",
                            "aria-label": "Open {file.filename} full size",
                            onclick: {
                                let url = url.clone();
                                let name = file.filename.clone();
                                move |_| open.set(Some((url.clone(), name.clone())))
                            },
                            img { src: "{url}", alt: "" }
                        }
                    } else if let Some(icon_src) = file_icon_src(&file.filename) {
                        img { class: "file-ext-icon", src: "{icon_src}", alt: "" }
                    } else {
                        Icon { name: if file.is_image { "image" } else { "paperclip" }, size: 13 }
                    }
                    span { class: "file-chip-name", "{file.filename}" }
                    span { class: "file-chip-size", "{size_label(file.size_bytes)}" }
                }
            }
            if let Some((url, name)) = open() {
                Lightbox {
                    src: url,
                    alt: name,
                    onclose: move |_| open.set(None),
                }
            }
        }
    }
}

/// Files attached to the draft, with a remove button each.
#[component]
pub fn ComposerFiles(session: Signal<Session>) -> Element {
    let (files, thumbs, waiting, uploading, notice) = {
        let state = session.read();
        let files = state.composer_files.clone();
        let thumbs: Vec<Option<String>> = files
            .iter()
            .map(|file| state.attachment_thumbs.get(&file.id).cloned())
            .collect();
        (
            files,
            thumbs,
            state.upload_pending(),
            state.uploading,
            state.attach_notice.clone(),
        )
    };
    if files.is_empty() && waiting == 0 && !uploading && notice.is_none() {
        return rsx! {};
    }
    rsx! {
        if let Some(message) = notice {
            div { class: "composer-notice", role: "alert",
                span { "{message}" }
                button {
                    class: "file-chip-x",
                    title: "Dismiss",
                    "aria-label": "Dismiss the file notice",
                    onclick: move |_| session.write().attach_notice = None,
                    Icon { name: "x", size: 12 }
                }
            }
        }
        if !files.is_empty() || waiting > 0 || uploading {
            div { class: "file-chips composer-files",
                for (file, thumb) in files.into_iter().zip(thumbs) {
                    span { key: "{file.id}", class: "file-chip", title: "{file.filename}",
                        if let Some(url) = thumb {
                            img { class: "file-thumb-img", src: "{url}", alt: "" }
                        } else {
                            Icon { name: if file.is_image { "image" } else { "paperclip" }, size: 13 }
                        }
                        span { class: "file-chip-name", "{file.filename}" }
                        span { class: "file-chip-size", "{size_label(file.size_bytes)}" }
                        button {
                            class: "file-chip-x",
                            title: "Remove",
                            "aria-label": "Remove {file.filename}",
                            onclick: {
                                let id = file.id.clone();
                                move |_| session.write().remove_attachment(&id)
                            },
                            Icon { name: "x", size: 12 }
                        }
                    }
                }
                if uploading || waiting > 0 {
                    span { class: "file-chip pending", role: "status", "Uploading\u{2026}" }
                }
            }
        }
    }
}

/// Media cards attached to a specific assistant message turn.
#[component]
pub fn TurnMediaCards(
    session: Signal<Session>,
    entries: Vec<serabot_desktop::LibraryEntry>,
) -> Element {
    let mut active_hash = use_signal(|| Option::<String>::None);
    let downloading = session.read().downloading;
    if !downloading && active_hash.read().is_some() {
        active_hash.set(None);
    }
    if entries.is_empty() {
        return rsx! {};
    }
    rsx! {
        div { class: "media-cards turn-media-cards", role: "list", "aria-label": "Message files",
            for entry in entries {
                MediaCardRow {
                    key: "{entry.hash}",
                    session,
                    entry,
                    active_hash,
                    downloading,
                }
            }
        }
    }
}

/// Pending media jobs and unattached trailing library entries at the bottom of chat.
#[component]
pub fn TrailingMediaCards(
    session: Signal<Session>,
    entries: Vec<serabot_desktop::LibraryEntry>,
) -> Element {
    let mut show_all = use_signal(|| false);
    let mut active_hash = use_signal(|| Option::<String>::None);
    let total_entries = entries.len();
    let limit = if show_all() { total_entries } else { MAX_CARDS };
    let visible_entries: Vec<_> = entries.into_iter().take(limit).collect();
    let (pending, downloading) = {
        let state = session.read();
        (
            serabot_desktop::media_view::pending_jobs(&state.state.media_jobs)
                .into_iter()
                .cloned()
                .collect::<Vec<_>>(),
            state.downloading,
        )
    };
    if !downloading && active_hash.read().is_some() {
        active_hash.set(None);
    }
    if visible_entries.is_empty() && pending.is_empty() {
        return rsx! {};
    }
    rsx! {
        div { class: "media-cards", role: "list", "aria-label": "Files from this chat",
            for job in pending {
                div { key: "job-{job.id}", class: "media-card pending", role: "listitem",
                    span { class: "media-card-icon", Icon { name: "refresh", size: 18 } }
                    div { class: "media-card-copy",
                        span { class: "media-card-title", "{serabot_desktop::media_view::placeholder_label(&job)}" }
                        span { class: "media-card-meta", role: "status", "{job.bot}" }
                    }
                }
            }
            for entry in visible_entries {
                MediaCardRow {
                    key: "{entry.hash}",
                    session,
                    entry,
                    active_hash,
                    downloading,
                }
            }
            if total_entries > MAX_CARDS {
                button {
                    class: "btn ghost sm media-cards-more",
                    r#type: "button",
                    onclick: move |_| show_all.toggle(),
                    if show_all() {
                        "Show fewer"
                    } else {
                        "Show all ({total_entries})"
                    }
                }
            }
        }
    }
}

/// Legacy wrapper for conversation-level media cards.
#[component]
pub fn MediaCards(session: Signal<Session>) -> Element {
    let entries = session.read().state.library.clone();
    rsx! {
        TrailingMediaCards { session, entries }
    }
}

#[component]
fn MediaCardRow(
    session: Signal<Session>,
    entry: serabot_desktop::LibraryEntry,
    mut active_hash: Signal<Option<String>>,
    downloading: bool,
) -> Element {
    let preview_kind = entry_preview_kind(&entry);
    let is_image = preview_kind == Some(PreviewKind::Image);
    let hash_for_effect = entry.hash.clone();
    use_effect(move || {
        if is_image {
            let needs_fetch = {
                let s = session.peek();
                !s.state.doc_previews.contains_key(&hash_for_effect)
                    && !s.doc_previews_pending.contains(&hash_for_effect)
            };
            if needs_fetch {
                session.write().request_doc_preview(hash_for_effect.clone());
            }
        }
    });

    let fname = entry.filename.as_deref().unwrap_or_default();
    let ext_icon = file_icon_src(fname);
    let title = entry
        .title
        .clone()
        .or_else(|| entry.filename.clone())
        .unwrap_or_else(|| entry.hash.clone());
    let is_this_busy = downloading && active_hash.read().as_deref() == Some(entry.hash.as_str());

    let (is_dock_open, image_thumb_url) = {
        let s = session.read();
        let dock_open = s.state.preview_dock_hash.as_deref() == Some(entry.hash.as_str());
        let thumb = if is_image {
            s.state
                .doc_previews
                .get(&entry.hash)
                .and_then(|opt| opt.as_ref())
                .and_then(|p| match &p.visual {
                    PreviewVisual::Image { data_url, .. } => Some(data_url.clone()),
                    _ => None,
                })
        } else {
            None
        };
        (dock_open, thumb)
    };

    rsx! {
        div { class: if is_dock_open { "media-card active" } else { "media-card" }, role: "listitem",
            if let Some(url) = image_thumb_url {
                button {
                    class: "media-card-thumb",
                    r#type: "button",
                    title: "Preview {title}",
                    "aria-label": "Preview {title}",
                    onclick: {
                        let hash = entry.hash.clone();
                        move |_| session.write().open_preview_dock(&hash)
                    },
                    img { src: "{url}", alt: "{title}" }
                }
            } else {
                span { class: "media-card-icon",
                    if let Some(icon_src) = ext_icon {
                        img { class: "file-ext-icon", src: "{icon_src}", alt: "" }
                    } else {
                        Icon {
                            name: if is_image || is_image_name(fname) { "image" } else { "paperclip" },
                            size: 18,
                        }
                    }
                }
            }
            div { class: "media-card-copy",
                span { class: "media-card-title", "{title}" }
                span { class: "media-card-meta", "{humanize_label(&entry.tool)}" }
                InlineMedia {
                    session,
                    hash: entry.hash.clone(),
                    mime: entry.mime.clone().unwrap_or_default(),
                }
            }
            if preview_kind.is_some() {
                button {
                    class: if is_dock_open { "btn small primary" } else { "btn small" },
                    r#type: "button",
                    "aria-expanded": if is_dock_open { "true" } else { "false" },
                    onclick: {
                        let hash = entry.hash.clone();
                        move |_| session.write().toggle_doc_preview(&hash)
                    },
                    if is_dock_open { "Hide preview" } else { "Preview" }
                }
            }
            button {
                class: "btn small",
                r#type: "button",
                disabled: downloading,
                onclick: {
                    let hash = entry.hash.clone();
                    move |_| {
                        active_hash.set(Some(hash.clone()));
                        session.write().open_artifact(hash.clone());
                    }
                },
                if is_this_busy {
                    span { class: "spinner", role: "status", "aria-label": "Downloading" }
                } else {
                    "Open"
                }
            }
            button {
                class: "btn small",
                r#type: "button",
                disabled: downloading,
                onclick: {
                    let hash = entry.hash.clone();
                    move |_| {
                        active_hash.set(Some(hash.clone()));
                        session.write().save_artifact_as(hash.clone());
                    }
                },
                "Save as\u{2026}"
            }
        }
    }
}

/// Right Sidebar Visual Preview Dock: renders images, multi-page PDFs (vector SVG
/// or formatted page sheets), PowerPoint slides, Excel multi-sheet grids, Word
/// rich blocks, and Markdown in a dedicated studio canvas.
#[component]
pub fn PreviewDock(session: Signal<Session>) -> Element {
    let mut all_pdf_pages = use_signal(|| false);

    let (hash_opt, wide, active_page, zoom, text_mode, downloading, cached, pending, lib_entry) = {
        let s = session.read();
        let h = s.state.preview_dock_hash.clone();
        let cached = h
            .as_ref()
            .and_then(|hash| s.state.doc_previews.get(hash).cloned());
        let pending = h
            .as_ref()
            .is_some_and(|hash| s.doc_previews_pending.contains(hash));
        let lib_entry = h
            .as_ref()
            .and_then(|hash| s.state.library.iter().find(|e| &e.hash == hash).cloned());
        (
            h,
            s.state.preview_dock_wide,
            s.state.preview_dock_page,
            s.state.preview_dock_zoom,
            s.state.preview_dock_text_mode,
            s.downloading,
            cached,
            pending,
            lib_entry,
        )
    };

    let Some(hash) = hash_opt else {
        return rsx! {
            aside { class: "inspector preview-dock", "aria-hidden": "true" }
        };
    };

    let dock_class = if wide {
        "inspector preview-dock open wide"
    } else {
        "inspector preview-dock open"
    };

    let preview_opt = cached.as_ref().and_then(|o| o.as_ref());
    let filename = preview_opt
        .and_then(|p| p.filename.clone())
        .or_else(|| lib_entry.as_ref().and_then(|e| e.filename.clone()))
        .unwrap_or_default();
    let title = preview_opt
        .and_then(|p| p.title.clone())
        .or_else(|| lib_entry.as_ref().and_then(|e| e.title.clone()))
        .or_else(|| (!filename.is_empty()).then(|| filename.clone()))
        .unwrap_or_else(|| hash.clone());
    let kind = preview_opt
        .and_then(|p| p.kind)
        .or_else(|| lib_entry.as_ref().and_then(entry_preview_kind));
    let kind_label = kind.map(PreviewKind::label).unwrap_or("File");
    let ext_icon = file_icon_src(&filename);

    let meta_pill = preview_opt.and_then(|p| match &p.visual {
        PreviewVisual::Image {
            width,
            height,
            size_bytes,
            ..
        } => {
            let mut parts = Vec::new();
            if let (Some(w), Some(h)) = (width, height) {
                parts.push(format!("{w}\u{00d7}{h} px"));
            }
            if let Some(sz) = size_bytes {
                parts.push(size_label(*sz));
            }
            (!parts.is_empty()).then(|| parts.join(" \u{00b7} "))
        }
        PreviewVisual::Pdf { page_count, .. } => Some(if *page_count == 1 {
            "1 page".into()
        } else {
            format!("{page_count} pages")
        }),
        PreviewVisual::Pptx { slide_count, .. } => Some(if *slide_count == 1 {
            "1 slide".into()
        } else {
            format!("{slide_count} slides")
        }),
        PreviewVisual::Xlsx { sheets } => Some(if sheets.len() == 1 {
            "1 sheet".into()
        } else {
            format!("{} sheets", sheets.len())
        }),
        PreviewVisual::Docx { blocks } => Some(format!("{} blocks", blocks.len())),
        PreviewVisual::Markdown { .. } => Some("Markdown".into()),
        PreviewVisual::PlainText => None,
    });

    let can_toggle_text = preview_opt.is_some_and(|p| {
        p.kind != Some(PreviewKind::Image)
            && p.text.as_deref().is_some_and(|t| !t.trim().is_empty())
    });

    rsx! {
        aside {
            class: "{dock_class}",
            role: "complementary",
            "aria-label": "File preview: {title}",
            div { class: "preview-dock-head",
                div { class: "preview-dock-title-row",
                    span { class: "preview-dock-icon",
                        if let Some(src) = ext_icon {
                            img { class: "file-ext-icon", src: "{src}", alt: "" }
                        } else {
                            Icon {
                                name: if kind == Some(PreviewKind::Image) { "image" } else { "logs" },
                                size: 17,
                            }
                        }
                    }
                    div { class: "preview-dock-title-copy",
                        h2 { class: "preview-dock-title", title: "{title}", "{title}" }
                        div { class: "preview-dock-badges",
                            span { class: "preview-kind-badge", "{kind_label}" }
                            if let Some(meta) = meta_pill {
                                span { class: "preview-meta-pill", "{meta}" }
                            }
                        }
                    }
                }
                div { class: "preview-dock-actions",
                    if can_toggle_text {
                        button {
                            class: if text_mode { "btn small primary" } else { "btn small" },
                            r#type: "button",
                            title: if text_mode { "Switch to visual preview" } else { "View raw extracted text" },
                            onclick: move |_| session.write().toggle_preview_dock_text_mode(),
                            if text_mode { "Visual" } else { "Text" }
                        }
                    }
                    button {
                        class: "btn icon ghost",
                        r#type: "button",
                        title: if wide { "Standard width" } else { "Expand width" },
                        "aria-label": if wide { "Standard width" } else { "Expand width" },
                        onclick: move |_| session.write().toggle_preview_dock_wide(),
                        Icon { name: if wide { "minimize" } else { "maximize" }, size: 15 }
                    }
                    button {
                        class: "btn small",
                        r#type: "button",
                        disabled: downloading,
                        onclick: {
                            let h = hash.clone();
                            move |_| session.write().open_artifact(h.clone())
                        },
                        "Open"
                    }
                    button {
                        class: "btn small",
                        r#type: "button",
                        disabled: downloading,
                        onclick: {
                            let h = hash.clone();
                            move |_| session.write().save_artifact_as(h.clone())
                        },
                        "Save as\u{2026}"
                    }
                    button {
                        class: "btn icon ghost",
                        r#type: "button",
                        title: "Close preview",
                        "aria-label": "Close preview",
                        onclick: move |_| session.write().close_preview_dock(),
                        Icon { name: "x", size: 16 }
                    }
                }
            }

            {
                match cached {
                    None if pending => rsx! {
                        div { class: "preview-dock-empty", role: "status",
                            span { class: "spinner", "aria-hidden": "true" }
                            span { "Loading visual preview\u{2026}" }
                        }
                    },
                    None => rsx! {
                        div { class: "preview-dock-empty",
                            p { "Preview not loaded yet." }
                            button {
                                class: "btn small",
                                r#type: "button",
                                onclick: {
                                    let h = hash.clone();
                                    move |_| session.write().request_doc_preview(h.clone())
                                },
                                "Retry"
                            }
                        }
                    },
                    Some(None) => rsx! {
                        div { class: "preview-dock-empty",
                            p { "The server has no preview for this file. Use Open to view it locally." }
                        }
                    },
                    Some(Some(preview)) if text_mode => {
                        let raw = preview.text.clone().unwrap_or_else(|| "No extracted text.".into());
                        rsx! {
                            div { class: "preview-dock-stage",
                                div { class: "preview-doc-sheet",
                                    pre { class: "preview-raw-text", "{raw}" }
                                }
                            }
                        }
                    }
                    Some(Some(preview)) => match &preview.visual {
                        PreviewVisual::Image { data_url, width, height, .. } => {
                            let dims = match (width, height) {
                                (Some(w), Some(h)) => format!("{w} \u{00d7} {h} px"),
                                _ => "Image".into(),
                            };
                            rsx! {
                                div { class: "preview-dock-toolbar",
                                    span { class: "preview-toolbar-info", "{dims}" }
                                    div { class: "preview-zoom-controls",
                                        button {
                                            class: "btn small ghost",
                                            r#type: "button",
                                            onclick: move |_| session.write().set_preview_dock_zoom(zoom.saturating_sub(25)),
                                            "\u{2212}"
                                        }
                                        button {
                                            class: "btn small ghost",
                                            r#type: "button",
                                            title: "Reset zoom to Fit",
                                            onclick: move |_| session.write().set_preview_dock_zoom(100),
                                            if zoom == 100 { "Fit" } else { "{zoom}%" }
                                        }
                                        button {
                                            class: "btn small ghost",
                                            r#type: "button",
                                            onclick: move |_| session.write().set_preview_dock_zoom(zoom.saturating_add(25)),
                                            "+"
                                        }
                                    }
                                }
                                div { class: "preview-dock-stage preview-image-stage",
                                    img {
                                        class: if zoom > 100 { "preview-image-img zoomed" } else { "preview-image-img" },
                                        src: "{data_url}",
                                        alt: "{title}",
                                        style: "width: {zoom}%;",
                                    }
                                }
                            }
                        }
                        PreviewVisual::Pdf { page_count, pages } => {
                            let total = pages.len().max(1);
                            let cur = active_page.min(total - 1);
                            let total_pages = (*page_count).max(total as u64);
                            let show_all = all_pdf_pages();
                            let visible_pages: Vec<_> = if show_all {
                                pages.iter().collect()
                            } else {
                                pages.get(cur).into_iter().collect()
                            };
                            rsx! {
                                div { class: "preview-dock-toolbar",
                                    div { class: "preview-nav-controls",
                                        button {
                                            class: "btn small ghost",
                                            r#type: "button",
                                            disabled: show_all || cur == 0,
                                            onclick: move |_| session.write().set_preview_dock_page(cur.saturating_sub(1)),
                                            Icon { name: "chevron-left", size: 14 }
                                        }
                                        span { class: "preview-page-counter",
                                            "Page {cur + 1} / {total_pages}"
                                        }
                                        button {
                                            class: "btn small ghost",
                                            r#type: "button",
                                            disabled: show_all || cur + 1 >= total,
                                            onclick: move |_| session.write().set_preview_dock_page((cur + 1).min(total - 1)),
                                            Icon { name: "chevron-right", size: 14 }
                                        }
                                        if total > 1 {
                                            button {
                                                class: if show_all { "btn small primary" } else { "btn small ghost" },
                                                r#type: "button",
                                                onclick: move |_| all_pdf_pages.toggle(),
                                                "All pages"
                                            }
                                        }
                                    }
                                    div { class: "preview-zoom-controls",
                                        button {
                                            class: "btn small ghost",
                                            r#type: "button",
                                            onclick: move |_| session.write().set_preview_dock_zoom(zoom.saturating_sub(15)),
                                            "\u{2212}"
                                        }
                                        button {
                                            class: "btn small ghost",
                                            r#type: "button",
                                            onclick: move |_| session.write().set_preview_dock_zoom(100),
                                            "{zoom}%"
                                        }
                                        button {
                                            class: "btn small ghost",
                                            r#type: "button",
                                            onclick: move |_| session.write().set_preview_dock_zoom(zoom.saturating_add(15)),
                                            "+"
                                        }
                                    }
                                }
                                if total > 1 && !show_all {
                                    div { class: "preview-pill-strip", role: "tablist", "aria-label": "Pages",
                                        for (idx, p) in pages.iter().enumerate() {
                                            button {
                                                key: "pdf-p-{idx}",
                                                class: if idx == cur { "preview-pill active" } else { "preview-pill" },
                                                r#type: "button",
                                                onclick: move |_| session.write().set_preview_dock_page(idx),
                                                "{p.page}"
                                            }
                                        }
                                    }
                                }
                                div { class: "preview-dock-stage",
                                    for p in visible_pages {
                                        {
                                            let has_vectors = !p.rects.is_empty() || !p.text_blocks.is_empty();
                                            let bg = p.background_hex.as_deref().unwrap_or("#ffffff");
                                            let ratio = format!("{:.3} / {:.3}", p.width_pt, p.height_pt);
                                            rsx! {
                                                div {
                                                    key: "pdf-page-{p.page}",
                                                    class: "preview-pdf-page",
                                                    style: "aspect-ratio: {ratio}; width: {zoom}%; background: {bg};",
                                                    if has_vectors {
                                                        svg {
                                                            class: "preview-pdf-svg",
                                                            view_box: "0 0 {p.width_pt} {p.height_pt}",
                                                            rect {
                                                                x: "0",
                                                                y: "0",
                                                                width: "{p.width_pt}",
                                                                height: "{p.height_pt}",
                                                                fill: "{bg}",
                                                            }
                                                            for (ri, r) in p.rects.iter().enumerate() {
                                                                rect {
                                                                    key: "r-{ri}",
                                                                    x: "{r.x_pt}",
                                                                    y: "{r.y_pt}",
                                                                    width: "{r.w_pt}",
                                                                    height: "{r.h_pt}",
                                                                    fill: r.fill_hex.as_deref().unwrap_or("none"),
                                                                    stroke: r.stroke_hex.as_deref().unwrap_or("none"),
                                                                }
                                                            }
                                                            for (ti, tb) in p.text_blocks.iter().enumerate() {
                                                                text {
                                                                    key: "tb-{ti}",
                                                                    x: "{tb.x_pt}",
                                                                    y: "{tb.y_pt}",
                                                                    font_size: "{tb.font_size}",
                                                                    fill: tb.color_hex.as_deref().unwrap_or("#111827"),
                                                                    font_weight: if tb.bold { "700" } else { "400" },
                                                                    "{tb.text}"
                                                                }
                                                            }
                                                        }
                                                    } else {
                                                        div { class: "preview-pdf-text-page",
                                                            div { class: "preview-pdf-page-badge", "Page {p.page}" }
                                                            for (pi, para) in p.text.split("\n\n").filter(|s| !s.trim().is_empty()).enumerate() {
                                                                p { key: "para-{pi}", class: "preview-doc-p", "{para.trim()}" }
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
                        PreviewVisual::Pptx { slide_count, slides } => {
                            let total = slides.len().max(1);
                            let cur = active_page.min(total - 1);
                            let total_slides = (*slide_count).max(total);
                            let active_slide = slides.get(cur);
                            rsx! {
                                div { class: "preview-dock-toolbar",
                                    div { class: "preview-nav-controls",
                                        button {
                                            class: "btn small ghost",
                                            r#type: "button",
                                            disabled: cur == 0,
                                            onclick: move |_| session.write().set_preview_dock_page(cur.saturating_sub(1)),
                                            Icon { name: "chevron-left", size: 14 }
                                        }
                                        span { class: "preview-page-counter",
                                            "Slide {cur + 1} / {total_slides}"
                                        }
                                        button {
                                            class: "btn small ghost",
                                            r#type: "button",
                                            disabled: cur + 1 >= total,
                                            onclick: move |_| session.write().set_preview_dock_page((cur + 1).min(total - 1)),
                                            Icon { name: "chevron-right", size: 14 }
                                        }
                                    }
                                    div { class: "preview-zoom-controls",
                                        button {
                                            class: "btn small ghost",
                                            r#type: "button",
                                            onclick: move |_| session.write().set_preview_dock_zoom(zoom.saturating_sub(15)),
                                            "\u{2212}"
                                        }
                                        button {
                                            class: "btn small ghost",
                                            r#type: "button",
                                            onclick: move |_| session.write().set_preview_dock_zoom(100),
                                            "{zoom}%"
                                        }
                                        button {
                                            class: "btn small ghost",
                                            r#type: "button",
                                            onclick: move |_| session.write().set_preview_dock_zoom(zoom.saturating_add(15)),
                                            "+"
                                        }
                                    }
                                }
                                if total > 1 {
                                    div { class: "preview-pill-strip", role: "tablist", "aria-label": "Slides",
                                        for (idx, s) in slides.iter().enumerate() {
                                            button {
                                                key: "slide-tab-{idx}",
                                                class: if idx == cur { "preview-pill active" } else { "preview-pill" },
                                                r#type: "button",
                                                onclick: move |_| session.write().set_preview_dock_page(idx),
                                                "Slide {s.slide}"
                                            }
                                        }
                                    }
                                }
                                div { class: "preview-dock-stage",
                                    if let Some(s) = active_slide {
                                        {
                                            let bg = s.background_hex.as_deref().unwrap_or("#ffffff");
                                            let ratio = format!("{:.3} / {:.3}", s.width_pt, s.height_pt);
                                            rsx! {
                                                div {
                                                    class: "preview-slide-stage",
                                                    style: "aspect-ratio: {ratio}; width: {zoom}%; background: {bg};",
                                                    if !s.shapes.is_empty() {
                                                        for (si, sh) in s.shapes.iter().enumerate() {
                                                            {
                                                                let left = (sh.x_pt / s.width_pt) * 100.0;
                                                                let top = (sh.y_pt / s.height_pt) * 100.0;
                                                                let width = (sh.w_pt / s.width_pt) * 100.0;
                                                                let height = (sh.h_pt / s.height_pt) * 100.0;
                                                                let pos = format!(
                                                                    "left: {left:.2}%; top: {top:.2}%; width: {width:.2}%; height: {height:.2}%;"
                                                                );
                                                                match sh.kind {
                                                                    SlideShapeKind::Rect => {
                                                                        let fill = sh.fill_hex.as_deref().unwrap_or("transparent");
                                                                        let stroke = sh.stroke_hex.as_deref().unwrap_or("transparent");
                                                                        rsx! {
                                                                            div {
                                                                                key: "sh-{si}",
                                                                                class: "preview-slide-shape preview-slide-rect",
                                                                                style: "{pos} background: {fill}; border-color: {stroke};",
                                                                            }
                                                                        }
                                                                    }
                                                                    SlideShapeKind::Text => {
                                                                        let color = sh.color_hex.as_deref().unwrap_or("#111827");
                                                                        let weight = if sh.bold { "700" } else { "400" };
                                                                        let align = sh.align.as_deref().unwrap_or("left");
                                                                        let fsize = sh.font_size.unwrap_or(14.0);
                                                                        let bg_fill = sh.fill_hex.as_deref().unwrap_or("transparent");
                                                                        let txt = sh.text.as_deref().unwrap_or("");
                                                                        rsx! {
                                                                            div {
                                                                                key: "sh-{si}",
                                                                                class: "preview-slide-shape preview-slide-text",
                                                                                style: "{pos} color: {color}; font-weight: {weight}; text-align: {align}; font-size: clamp(9px, {fsize * 0.72:.1}px, 28px); background: {bg_fill};",
                                                                                "{txt}"
                                                                            }
                                                                        }
                                                                    }
                                                                    SlideShapeKind::Table => rsx! {
                                                                        div {
                                                                            key: "sh-{si}",
                                                                            class: "preview-slide-shape preview-slide-table-wrap",
                                                                            style: "{pos}",
                                                                            table { class: "preview-slide-table",
                                                                                for (ri, row) in sh.rows.iter().enumerate() {
                                                                                    tr { key: "tr-{ri}",
                                                                                        for (ci, cell) in row.iter().enumerate() {
                                                                                            if ri == 0 {
                                                                                                th { key: "th-{ci}", "{cell}" }
                                                                                            } else {
                                                                                                td { key: "td-{ci}", "{cell}" }
                                                                                            }
                                                                                        }
                                                                                    }
                                                                                }
                                                                            }
                                                                        }
                                                                    },
                                                                    SlideShapeKind::Image => rsx! {
                                                                        if let Some(url) = &sh.image_data_url {
                                                                            img {
                                                                                key: "sh-{si}",
                                                                                class: "preview-slide-shape preview-slide-img",
                                                                                style: "{pos}",
                                                                                src: "{url}",
                                                                                alt: "",
                                                                            }
                                                                        }
                                                                    },
                                                                }
                                                            }
                                                        }
                                                    } else {
                                                        div { class: "preview-pdf-text-page",
                                                            if let Some(stitle) = &s.title {
                                                                h3 { class: "preview-doc-h2", "{stitle}" }
                                                            }
                                                            for (pi, para) in s.text.split("\n\n").filter(|p| !p.trim().is_empty()).enumerate() {
                                                                p { key: "sp-{pi}", class: "preview-doc-p", "{para.trim()}" }
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
                        PreviewVisual::Xlsx { sheets } => {
                            let cur = active_page.min(sheets.len().saturating_sub(1));
                            let active_sheet = sheets.get(cur);
                            let col_count = active_sheet
                                .map(|s| s.rows.iter().map(Vec::len).max().unwrap_or(0))
                                .unwrap_or(0);
                            rsx! {
                                if !sheets.is_empty() {
                                    div { class: "preview-dock-toolbar",
                                        div { class: "preview-pill-strip", role: "tablist", "aria-label": "Sheets",
                                            for (idx, sh) in sheets.iter().enumerate() {
                                                button {
                                                    key: "sheet-tab-{idx}",
                                                    class: if idx == cur { "preview-pill active" } else { "preview-pill" },
                                                    r#type: "button",
                                                    onclick: move |_| session.write().set_preview_dock_page(idx),
                                                    "{sh.name}"
                                                }
                                            }
                                        }
                                        if let Some(sh) = active_sheet.filter(|s| !s.range.is_empty()) {
                                            span { class: "preview-toolbar-info", "{sh.range}" }
                                        }
                                    }
                                }
                                div { class: "preview-dock-stage preview-xlsx-stage",
                                    if let Some(sh) = active_sheet {
                                        div { class: "xlsx-grid-wrap",
                                            table { class: "xlsx-grid",
                                                thead {
                                                    tr {
                                                        th { class: "xlsx-corner", "" }
                                                        for ci in 0..col_count {
                                                            th { key: "col-{ci}", class: "xlsx-col-head",
                                                                "{excel_col_label(ci)}"
                                                            }
                                                        }
                                                    }
                                                }
                                                tbody {
                                                    for (ri, row) in sh.rows.iter().enumerate() {
                                                        tr {
                                                            key: "row-{ri}",
                                                            class: if ri == 0 { "xlsx-header-row" } else { "" },
                                                            th { class: "xlsx-row-head", "{ri + 1}" }
                                                            for ci in 0..col_count {
                                                                {
                                                                    let val = row.get(ci).map(String::as_str).unwrap_or("");
                                                                    let num_cls = if ri > 0 && is_numeric_cell(val) {
                                                                        "xlsx-cell xlsx-num"
                                                                    } else {
                                                                        "xlsx-cell"
                                                                    };
                                                                    rsx! {
                                                                        td { key: "c-{ci}", class: "{num_cls}", "{val}" }
                                                                    }
                                                                }
                                                            }
                                                        }
                                                    }
                                                }
                                            }
                                        }
                                        if sh.truncated {
                                            p { class: "preview-truncated-note",
                                                "Showing first rows/columns. Click Open for the full workbook."
                                            }
                                        }
                                    }
                                }
                            }
                        }
                        PreviewVisual::Docx { blocks } => rsx! {
                            div { class: "preview-dock-stage",
                                div { class: "preview-doc-sheet",
                                    for (bi, block) in blocks.iter().enumerate() {
                                        match block {
                                            DocBlockVisual::Heading { level, text } => match level {
                                                1 => rsx! { h1 { key: "b-{bi}", class: "preview-doc-h1", "{text}" } },
                                                2 => rsx! { h2 { key: "b-{bi}", class: "preview-doc-h2", "{text}" } },
                                                _ => rsx! { h3 { key: "b-{bi}", class: "preview-doc-h3", "{text}" } },
                                            },
                                            DocBlockVisual::Paragraph { text } => rsx! {
                                                p { key: "b-{bi}", class: "preview-doc-p", "{text}" }
                                            },
                                            DocBlockVisual::ListItem { text, ordered } => rsx! {
                                                div { key: "b-{bi}", class: "preview-doc-li",
                                                    span { class: "preview-doc-bullet",
                                                        if *ordered { "\u{2022}" } else { "\u{2022}" }
                                                    }
                                                    span { "{text}" }
                                                }
                                            },
                                            DocBlockVisual::Table { rows } => rsx! {
                                                table { key: "b-{bi}", class: "preview-doc-table",
                                                    for (ri, row) in rows.iter().enumerate() {
                                                        tr { key: "dr-{ri}",
                                                            for (ci, cell) in row.iter().enumerate() {
                                                                if ri == 0 {
                                                                    th { key: "dc-{ci}", "{cell}" }
                                                                } else {
                                                                    td { key: "dc-{ci}", "{cell}" }
                                                                }
                                                            }
                                                        }
                                                    }
                                                }
                                            },
                                            DocBlockVisual::Image { data_url, alt } => rsx! {
                                                img {
                                                    key: "b-{bi}",
                                                    class: "preview-doc-img",
                                                    src: "{data_url}",
                                                    alt: alt.as_deref().unwrap_or(""),
                                                }
                                            },
                                        }
                                    }
                                    if preview.truncated {
                                        p { class: "preview-truncated-note",
                                            "Preview truncated. Click Open for the full document."
                                        }
                                    }
                                }
                            }
                        },
                        PreviewVisual::Markdown { markdown } => rsx! {
                            div { class: "preview-dock-stage",
                                div { class: "preview-doc-sheet preview-md-sheet",
                                    crate::ui::MarkdownBody {
                                        message_id: format!("preview-{hash}"),
                                        content: markdown.clone(),
                                    }
                                }
                            }
                        },
                        PreviewVisual::PlainText => {
                            let body = preview.text.clone().unwrap_or_default();
                            rsx! {
                                div { class: "preview-dock-stage",
                                    div { class: "preview-doc-sheet",
                                        for (pi, para) in body.split("\n\n").filter(|s| !s.trim().is_empty()).enumerate() {
                                            p { key: "pt-{pi}", class: "preview-doc-p", "{para.trim()}" }
                                        }
                                    }
                                }
                            }
                        }
                    },
                }
            }
        }
    }
}

/// An inline player for a video or audio file (0.24). Until its bytes are read
/// back, a Play button asks the session for them. Other files draw nothing.
#[component]
pub(crate) fn InlineMedia(session: Signal<Session>, hash: String, mime: String) -> Element {
    let Some(player) = serabot_desktop::media_view::inline_player(&mime) else {
        return rsx! {};
    };
    let src = session.read().state.media_previews.get(&hash).cloned();
    match (src, player) {
        (Some(src), serabot_desktop::media_view::InlinePlayer::Video) => rsx! {
            video {
                class: "media-player",
                controls: true,
                preload: "metadata",
                src: "{src}",
                "aria-label": "Video",
            }
        },
        (Some(src), serabot_desktop::media_view::InlinePlayer::Audio) => rsx! {
            audio {
                class: "media-player",
                controls: true,
                preload: "metadata",
                src: "{src}",
                "aria-label": "Audio",
            }
        },
        (None, _) => rsx! {
            button {
                class: "btn small",
                onclick: move |_| session.write().request_preview(hash.clone(), mime.clone()),
                "Play"
            }
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn size_labels() {
        assert_eq!(size_label(12), "12 B");
        assert_eq!(size_label(1536), "1.5 KB");
        assert_eq!(size_label(5 * 1024 * 1024), "5.0 MB");
    }

    #[test]
    fn image_names() {
        assert!(is_image_name("Shot.PNG"));
        assert!(is_image_name("logo.svg"));
        assert!(!is_image_name("deck.pdf"));
    }

    #[test]
    fn excel_columns_and_numbers() {
        assert_eq!(excel_col_label(0), "A");
        assert_eq!(excel_col_label(25), "Z");
        assert_eq!(excel_col_label(26), "AA");
        assert!(is_numeric_cell("$1,250.50"));
        assert!(is_numeric_cell("-42.5%"));
        assert!(!is_numeric_cell("Revenue"));
    }

    #[test]
    fn copy_script_escapes_quotes_and_newlines() {
        let script = copy_script("a\"b\nc");
        assert!(script.contains("\"a\\\"b\\nc\""));
    }

    #[test]
    fn decode_pasted_handles_valid_and_invalid_base64() {
        let valid = decode_pasted(PastedFile {
            name: "shot.png".into(),
            data: "aGk=".into(),
        });
        assert_eq!(valid.0, "shot.png");
        assert_eq!(valid.1, b"hi");

        let bad = decode_pasted(PastedFile {
            name: "bad.png".into(),
            data: "!!!".into(),
        });
        assert!(bad.1.is_empty());
    }
}
