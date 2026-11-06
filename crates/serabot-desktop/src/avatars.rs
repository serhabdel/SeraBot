//! Avatar images in the console.
//!
//! The server hands back one 256x256 PNG per bot or group. The worker thread
//! turns it into a `data:` URL; this module keeps the URLs and publishes them
//! to the page as CSS custom properties, once, in a single `<style>` element
//! (`:root { --av-bot-recruiter: url("data:...") }`). Each avatar tile then
//! only carries a tiny `--avatar-img: var(--av-bot-recruiter)` in its inline
//! style, so a long chat does not repeat an 80 KB string per message. A tile
//! with no image keeps the gradient letter tile.

use std::collections::BTreeMap;
use std::sync::RwLock;

/// What an avatar belongs to. The id is the bot id, or the room conversation
/// id for a group.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum AvatarTarget {
    Bot(String),
    Group(String),
}

impl AvatarTarget {
    /// Stable map key, also the CSS custom-property suffix.
    pub fn key(&self) -> String {
        match self {
            Self::Bot(id) => css_key("bot", id),
            Self::Group(id) => css_key("grp", id),
        }
    }
}

/// `bot` + `marketing-lead` -> `bot-marketing-lead`; `grp` + `room:hiring` ->
/// `grp-room-hiring`. Ids are lowercase slugs, so the result is a valid
/// custom-property name.
pub fn css_key(prefix: &str, id: &str) -> String {
    let tail: String = id
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '-'
            }
        })
        .collect();
    format!("{prefix}-{tail}")
}

/// One fetched avatar (or the knowledge that there is none).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AvatarImage {
    /// The server's version stamp the image was fetched at.
    pub version: Option<String>,
    /// `data:image/png;base64,...`, or `None` when the server has no image.
    pub data_url: Option<String>,
}

static PUBLISHED: RwLock<Vec<String>> = RwLock::new(Vec::new());

/// Publish which keys currently have an image, so tiles know whether to
/// reference one. Called by the session whenever the cache changes.
pub fn publish(images: &BTreeMap<String, AvatarImage>) {
    let keys: Vec<String> = images
        .iter()
        .filter(|(_, image)| image.data_url.is_some())
        .map(|(key, _)| key.clone())
        .collect();
    if let Ok(mut published) = PUBLISHED.write() {
        *published = keys;
    }
}

/// Whether the tile for `key` should reference an image.
pub fn has_image(key: &str) -> bool {
    PUBLISHED
        .read()
        .map(|published| published.iter().any(|candidate| candidate == key))
        .unwrap_or(false)
}

/// Inline style for the tile of `key`: the hue (letter-tile fallback) plus the
/// image reference when one exists.
pub fn tile_style(key: &str, hue: u16) -> String {
    if has_image(key) {
        format!("--avatar-hue: {hue}; --avatar-img: var(--av-{key});")
    } else {
        format!("--avatar-hue: {hue};")
    }
}

/// The single stylesheet that defines every `--av-*` property.
pub fn stylesheet(images: &BTreeMap<String, AvatarImage>) -> String {
    let mut css = String::from(":root {");
    for (key, image) in images {
        if let Some(url) = &image.data_url {
            css.push_str(&format!(" --av-{key}: url(\"{url}\");"));
        }
    }
    css.push_str(" }");
    css
}

/// `data:image/png;base64,...` for the bytes of a PNG.
pub fn png_data_url(bytes: &[u8]) -> String {
    use base64::Engine;
    format!(
        "data:image/png;base64,{}",
        base64::engine::general_purpose::STANDARD.encode(bytes)
    )
}

/// Preview URL for a picked image whose format has been sniffed.
pub fn preview_data_url(mime: &str, bytes: &[u8]) -> String {
    use base64::Engine;
    format!(
        "data:{mime};base64,{}",
        base64::engine::general_purpose::STANDARD.encode(bytes)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_are_valid_css_names() {
        assert_eq!(
            AvatarTarget::Bot("marketing-lead".into()).key(),
            "bot-marketing-lead"
        );
        assert_eq!(
            AvatarTarget::Group("room:hiring".into()).key(),
            "grp-room-hiring"
        );
        assert_eq!(css_key("bot", "a b;c"), "bot-a-b-c");
    }

    #[test]
    fn the_stylesheet_defines_only_fetched_images() {
        let mut images = BTreeMap::new();
        images.insert(
            "bot-a".to_owned(),
            AvatarImage {
                version: Some("v1".into()),
                data_url: Some(png_data_url(&[1, 2, 3])),
            },
        );
        images.insert(
            "bot-b".to_owned(),
            AvatarImage {
                version: None,
                data_url: None,
            },
        );
        let css = stylesheet(&images);
        assert!(css.contains("--av-bot-a: url(\"data:image/png;base64,AQID\")"));
        assert!(!css.contains("bot-b"));
    }

    #[test]
    fn tiles_reference_an_image_only_after_it_is_published() {
        let key = "bot-test-tile-unique";
        assert_eq!(tile_style(key, 12), "--avatar-hue: 12;");
        let mut images = BTreeMap::new();
        images.insert(
            key.to_owned(),
            AvatarImage {
                version: None,
                data_url: Some(png_data_url(&[0])),
            },
        );
        publish(&images);
        assert!(tile_style(key, 12).contains("--avatar-img: var(--av-bot-test-tile-unique)"));
        publish(&BTreeMap::new());
        assert_eq!(tile_style(key, 12), "--avatar-hue: 12;");
    }
}
