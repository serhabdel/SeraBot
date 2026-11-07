//! Bundled default avatars for the shipped bots.
//!
//! A user who has not uploaded a picture for a bot sees the default for that
//! bot's name. The images are compiled in, so the fallback needs no file
//! access at runtime and no database row. An upload always wins, and deleting
//! it restores the default.
//!
//! `main` uses the brand app icon, not a mascot: it is the one bot that has
//! no persona picture of its own.

/// Content type of every bundled default.
pub const DEFAULT_AVATAR_MIME: &str = "image/png";

/// The `avatar_version` the bot list reports for a bundled default. The
/// images only change with a release, so one fixed stamp is enough.
pub const DEFAULT_AVATAR_VERSION: &str = "bundled";

const MAIN: &[u8] = include_bytes!("../assets/brand/apple-touch-icon-180.png");
const ANALYST: &[u8] = include_bytes!("../assets/bots/analyst.png");
const LEGAL: &[u8] = include_bytes!("../assets/bots/legal.png");
const MAKER: &[u8] = include_bytes!("../assets/bots/maker.png");
const WATCHER: &[u8] = include_bytes!("../assets/bots/watcher.png");
const MARKETER: &[u8] = include_bytes!("../assets/bots/marketer.png");
const COMPTABLE: &[u8] = include_bytes!("../assets/bots/comptable.png");
const AUDITOR: &[u8] = include_bytes!("../assets/bots/auditor.png");
const MARKETS: &[u8] = include_bytes!("../assets/bots/markets.png");
const COURIER: &[u8] = include_bytes!("../assets/bots/courier.png");
const WHATSAPP: &[u8] = include_bytes!("../assets/bots/whatsapp.png");
const INBOX: &[u8] = include_bytes!("../assets/bots/inbox.png");

/// The bundled default for a bot, keyed by its name (the bot id). `None` for
/// a bot without one, so the caller keeps answering 404.
pub fn default_avatar(bot: &str) -> Option<&'static [u8]> {
    Some(match bot {
        "main" => MAIN,
        "analyst" => ANALYST,
        "legal" => LEGAL,
        "maker" => MAKER,
        "watcher" => WATCHER,
        "marketer" => MARKETER,
        "comptable" => COMPTABLE,
        "auditor" => AUDITOR,
        "markets" => MARKETS,
        "courier" => COURIER,
        "whatsapp" => WHATSAPP,
        "inbox" => INBOX,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Width and height from the IHDR chunk of a PNG.
    fn png_dimensions(bytes: &[u8]) -> (u32, u32) {
        assert_eq!(&bytes[..8], b"\x89PNG\r\n\x1a\n", "not a png");
        assert_eq!(&bytes[12..16], b"IHDR", "no IHDR chunk");
        let width = u32::from_be_bytes(bytes[16..20].try_into().unwrap());
        let height = u32::from_be_bytes(bytes[20..24].try_into().unwrap());
        (width, height)
    }

    #[test]
    fn persona_pictures_are_256_pixel_pngs() {
        for name in [
            "analyst",
            "legal",
            "maker",
            "watcher",
            "marketer",
            "comptable",
            "auditor",
            "markets",
            "courier",
            "whatsapp",
            "inbox",
        ] {
            let bytes = default_avatar(name).unwrap();
            assert_eq!(png_dimensions(bytes), (256, 256), "{name}");
        }
    }

    #[test]
    fn main_has_the_brand_icon_and_unknown_bots_have_none() {
        let main = default_avatar("main").unwrap();
        assert_eq!(png_dimensions(main), (180, 180));
        assert!(default_avatar("recruiter").is_none());
        assert!(default_avatar("").is_none());
        // Ids are case-sensitive slugs; a near miss gets no picture.
        assert!(default_avatar("Analyst").is_none());
    }

    /// Every bot file the repository ships has a picture, so a new persona
    /// cannot ship with the letter tile by accident.
    #[test]
    fn every_shipped_bot_has_a_default_picture() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../bots");
        let mut checked = 0usize;
        for entry in std::fs::read_dir(&root).expect("bots directory") {
            let path = entry.expect("bots entry").path();
            if path.extension().and_then(|value| value.to_str()) != Some("toml") {
                continue;
            }
            let stem = path.file_stem().and_then(|value| value.to_str()).unwrap();
            assert!(
                default_avatar(stem).is_some(),
                "{stem} has no default avatar"
            );
            checked += 1;
        }
        assert!(
            checked >= 12,
            "expected the twelve shipped bots, found {checked}"
        );
    }
}
