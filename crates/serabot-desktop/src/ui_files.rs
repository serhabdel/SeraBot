//! Chat files (0.18, extended in 0.19): attachment chips on the composer and
//! on sent messages, picture previews with a full-size view, and media cards
//! for files the bots produce (decks, PDFs, screenshots, browser downloads)
//! with Open and Save as... buttons.
//!
//! Downloads go through the authenticated, user-scoped API; nothing here
//! touches a server path. Pasted clipboard files arrive through a small JS
//! bridge as base64 and are decoded here before they join the upload queue.

use crate::ui::Icon;
use base64::Engine;
use dioxus::prelude::*;
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
  document.addEventListener('paste', async (event) => {
    const target = event.target;
    if (!target || target.id !== 'composer-input') return;
    const files = Array.from((event.clipboardData && event.clipboardData.files) || []);
    if (files.length === 0) return;
    event.preventDefault();
    for (const file of files) {
      // Oversized files are not read; an empty payload is refused in Rust.
      if (file.size > 25 * 1024 * 1024) {
        window.__serabotDx.send({ name: file.name || 'pasted', data: '' });
        continue;
      }
      const bytes = new Uint8Array(await file.arrayBuffer());
      let binary = '';
      for (let i = 0; i < bytes.length; i += 0x8000) {
        binary += String.fromCharCode.apply(null, bytes.subarray(i, i + 0x8000));
      }
      window.__serabotDx.send({ name: file.name || 'pasted-image.png', data: btoa(binary) });
    }
  });
}
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
    [".png", ".jpg", ".jpeg", ".gif", ".webp"]
        .iter()
        .any(|ext| lower.ends_with(ext))
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
            onclick: move |_| onclose.call(()),
            onkeydown: move |event: KeyboardEvent| {
                if event.key() == Key::Escape {
                    onclose.call(());
                }
            },
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

/// Cards for the newest files produced in this conversation.
#[component]
pub fn MediaCards(session: Signal<Session>) -> Element {
    let mut open_previews = use_signal(std::collections::BTreeSet::<String>::new);
    let (entries, pending, downloading) = {
        let state = session.read();
        let open = open_previews();
        let entries: Vec<(
            serabot_desktop::LibraryEntry,
            Option<serabot_desktop::preview::PreviewKind>,
            bool,
        )> = state
            .state
            .library
            .iter()
            .take(MAX_CARDS)
            .cloned()
            .map(|entry| {
                let kind = serabot_desktop::preview::preview_kind_for_name(
                    entry.filename.as_deref().unwrap_or_default(),
                );
                let is_open = open.contains(&entry.hash);
                (entry, kind, is_open)
            })
            .collect();
        (
            entries,
            serabot_desktop::media_view::pending_jobs(&state.state.media_jobs)
                .into_iter()
                .cloned()
                .collect::<Vec<_>>(),
            state.downloading,
        )
    };
    if entries.is_empty() && pending.is_empty() {
        return rsx! {};
    }
    rsx! {
        div { class: "media-cards", role: "list", "aria-label": "Files from this chat",
            // A background render still running (0.24): a placeholder until its file lands.
            for job in pending {
                div { key: "job-{job.id}", class: "media-card pending", role: "listitem",
                    span { class: "media-card-icon", Icon { name: "refresh", size: 18 } }
                    div { class: "media-card-copy",
                        span { class: "media-card-title", "{serabot_desktop::media_view::placeholder_label(&job)}" }
                        span { class: "media-card-meta", role: "status", "{job.bot}" }
                    }
                }
            }
            for (entry, preview_kind, preview_open) in entries {
                // 0.28.0: documents the server can render (pdf/pptx/xlsx/docx/md)
                // get a Preview toggle next to Open.
                div { key: "{entry.hash}", class: "media-card", role: "listitem",
                    span { class: "media-card-icon",
                        Icon {
                            name: if is_image_name(entry.filename.as_deref().unwrap_or_default()) { "image" } else { "paperclip" },
                            size: 18,
                        }
                    }
                    div { class: "media-card-copy",
                        span { class: "media-card-title",
                            "{entry.title.clone().or(entry.filename.clone()).unwrap_or_else(|| entry.hash.clone())}"
                        }
                        span { class: "media-card-meta", "{humanize_label(&entry.tool)}" }
                        InlineMedia {
                            session,
                            hash: entry.hash.clone(),
                            mime: entry.mime.clone().unwrap_or_default(),
                        }
                        if let Some(kind) = preview_kind {
                            button {
                                class: "btn small",
                                onclick: {
                                    let hash = entry.hash.clone();
                                    move |_| {
                                        let mut open = open_previews.write();
                                        if !open.remove(&hash) {
                                            open.insert(hash.clone());
                                        }
                                        drop(open);
                                        session.write().request_doc_preview(hash.clone());
                                    }
                                },
                                if preview_open { "Hide preview" } else { "Preview" }
                            }
                            if preview_open {
                                DocPreviewPane {
                                    session,
                                    hash: entry.hash.clone(),
                                    label: kind.label().to_owned(),
                                }
                            }
                        }
                    }
                    button {
                        class: "btn small",
                        disabled: downloading,
                        onclick: {
                            let hash = entry.hash.clone();
                            move |_| session.write().open_artifact(hash.clone())
                        },
                        "Open"
                    }
                    button {
                        class: "btn small",
                        disabled: downloading,
                        onclick: {
                            let hash = entry.hash.clone();
                            move |_| session.write().save_artifact_as(hash.clone())
                        },
                        "Save as\u{2026}"
                    }
                }
            }
        }
    }
}

/// The server-rendered preview pane for one document artifact (0.28.0):
/// `GET /v1/artifacts/{hash}/preview`. While the read runs it shows a
/// spinner; a missing preview (older server, 404) or a malformed one says
/// so and the card's Open/Save stay the fallback.
#[component]
fn DocPreviewPane(session: Signal<Session>, hash: String, label: String) -> Element {
    let cached = session.read().state.doc_previews.get(&hash).cloned();
    let pending = session.read().doc_previews_pending.contains(&hash);
    match cached {
        Some(Some(preview)) => match preview.render() {
            Some(serabot_desktop::preview::PreviewContent::Image(url)) => rsx! {
                div { class: "doc-preview",
                    div { class: "doc-preview-head",
                        span { class: "grow", "{label}" }
                    }
                    div { class: "doc-preview-body",
                        img { src: "{url}", alt: "Preview of the file" }
                    }
                }
            },
            Some(serabot_desktop::preview::PreviewContent::Text(text)) => rsx! {
                div { class: "doc-preview",
                    div { class: "doc-preview-head",
                        span { class: "grow", "{label}" }
                    }
                    div { class: "doc-preview-body",
                        pre { "{text}" }
                    }
                }
            },
            None => rsx! {
                div { class: "doc-preview",
                    div { class: "doc-preview-head",
                        span { class: "grow", "No preview available for this file. Use Open." }
                    }
                }
            },
        },
        Some(None) => rsx! {
            div { class: "doc-preview",
                div { class: "doc-preview-head",
                    span { class: "grow", "The server has no preview for this file. Use Open." }
                }
            }
        },
        None if pending => rsx! {
            div { class: "doc-preview",
                div { class: "doc-preview-head", role: "status",
                    span { class: "spinner", "aria-hidden": "true" }
                    span { class: "grow", "Loading preview\u{2026}" }
                }
            }
        },
        None => rsx! {},
    }
}

/// An inline player for a video or audio file (0.24). Until its bytes are read
/// back, a Play button asks the session for them. Other files draw nothing.
#[component]
fn InlineMedia(session: Signal<Session>, hash: String, mime: String) -> Element {
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
        assert!(!is_image_name("deck.pdf"));
    }

    #[test]
    fn copy_script_embeds_the_text_as_a_js_literal() {
        let script = copy_script("say \"hi\"\n</script>");
        assert!(script.contains(r#""say \"hi\"\n</script>""#));
        assert!(script.contains("navigator.clipboard.writeText"));
        assert!(script.contains("execCommand('copy')"));
    }

    #[test]
    fn pasted_payloads_decode_and_bad_base64_is_empty() {
        let (name, bytes) = decode_pasted(PastedFile {
            name: "shot.png".into(),
            data: "aGk=".into(),
        });
        assert_eq!(name, "shot.png");
        assert_eq!(bytes, b"hi");
        let (_, bad) = decode_pasted(PastedFile {
            name: "x.png".into(),
            data: "***".into(),
        });
        assert!(bad.is_empty());
        let (_, empty) = decode_pasted(PastedFile {
            name: "big.png".into(),
            data: String::new(),
        });
        assert!(empty.is_empty());
    }
}
