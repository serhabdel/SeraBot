//! Chat files (0.18): attachment chips on the composer and on sent messages,
//! and media cards for files the bots produce (decks, PDFs, screenshots,
//! browser downloads) with Open and Save as... buttons.
//!
//! Downloads go through the authenticated, user-scoped API; nothing here
//! touches a server path.

use crate::ui::Icon;
use dioxus::prelude::*;
use serabot_desktop::session::{Session, humanize_label};

/// How many of the newest produced files get a card under the transcript.
const MAX_CARDS: usize = 6;

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

/// Chips under a sent user message.
#[component]
pub fn MessageFiles(session: Signal<Session>, id: String) -> Element {
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
    rsx! {
        div { class: "file-chips",
            for file in files {
                span { key: "{file.id}", class: "file-chip", title: "{file.filename}",
                    Icon { name: if file.is_image { "image" } else { "paperclip" }, size: 13 }
                    span { class: "file-chip-name", "{file.filename}" }
                    span { class: "file-chip-size", "{size_label(file.size_bytes)}" }
                }
            }
        }
    }
}

/// Files attached to the draft, with a remove button each.
#[component]
pub fn ComposerFiles(session: Signal<Session>) -> Element {
    let (files, waiting, uploading) = {
        let state = session.read();
        (
            state.composer_files.clone(),
            state.upload_pending(),
            state.uploading,
        )
    };
    if files.is_empty() && waiting == 0 && !uploading {
        return rsx! {};
    }
    rsx! {
        div { class: "file-chips composer-files",
            for file in files {
                span { key: "{file.id}", class: "file-chip", title: "{file.filename}",
                    Icon { name: if file.is_image { "image" } else { "paperclip" }, size: 13 }
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

/// Cards for the newest files produced in this conversation.
#[component]
pub fn MediaCards(session: Signal<Session>) -> Element {
    let (entries, downloading) = {
        let state = session.read();
        (
            state
                .state
                .library
                .iter()
                .take(MAX_CARDS)
                .cloned()
                .collect::<Vec<_>>(),
            state.downloading,
        )
    };
    if entries.is_empty() {
        return rsx! {};
    }
    rsx! {
        div { class: "media-cards", role: "list", "aria-label": "Files from this chat",
            for entry in entries {
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
}
