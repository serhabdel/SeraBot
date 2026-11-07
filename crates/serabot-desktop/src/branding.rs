//! Brand logos for connectors and providers.
//!
//! Each vendor's own icon is a small PNG under `assets/brands/`, embedded with
//! `include_bytes!` and shown as a `data:` URI, so nothing is fetched at run
//! time. `assets/brands/SOURCES.md` records where every file came from. The
//! logos are trademarks of their owners and only identify the integration.
//!
//! Adding a brand is one line in [`BRANDS`] (plus its PNG). Generic marks that
//! belong to no vendor (custom, OpenAI-compatible, Muse Code) use a neutral
//! inline SVG. Unknown keys return `None` so callers draw a letter avatar.

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;

/// What a brand is drawn with.
#[derive(Clone, Copy)]
enum Mark {
    /// The vendor's own icon, 64x64 PNG.
    Png(&'static [u8]),
    /// A neutral generic mark (not a vendor logo).
    Svg(&'static str),
}

/// One brand: the lookup keys (lowercase, `-` separated), a label, a mark.
struct Brand {
    keys: &'static [&'static str],
    label: &'static str,
    mark: Mark,
}

macro_rules! png {
    ($name:literal) => {
        Mark::Png(include_bytes!(concat!("../assets/brands/", $name, ".png")))
    };
}

const GENERIC_CUSTOM: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24"><rect x="2.5" y="2.5" width="19" height="19" rx="5" fill="#334155"/><path d="M5.5 8.5h13M5.5 15.5h13" stroke="#fff" stroke-width="1.8" stroke-linecap="round"/><circle cx="9.5" cy="8.5" r="2.1" fill="#334155" stroke="#fff" stroke-width="1.8"/><circle cx="14.5" cy="15.5" r="2.1" fill="#334155" stroke="#fff" stroke-width="1.8"/></svg>"##;

const GENERIC_PLUG: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24"><rect x="2.5" y="2.5" width="19" height="19" rx="5" fill="#475569"/><path d="M9 8V4.8M15 8V4.8M7 8h10v3.5a5 5 0 0 1-10 0zM12 16.5v3.7" fill="none" stroke="#fff" stroke-width="1.9" stroke-linecap="round" stroke-linejoin="round"/></svg>"##;

const GENERIC_CODE: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24"><rect x="2.5" y="2.5" width="19" height="19" rx="5" fill="#0891B2"/><path d="m8.5 8-3.5 4 3.5 4M15.5 8l3.5 4-3.5 4M13.2 6.5l-2.4 11" fill="none" stroke="#fff" stroke-width="1.9" stroke-linecap="round" stroke-linejoin="round"/></svg>"##;

/// Every known brand. To add one: drop `<key>.png` in `assets/brands/`, list
/// it in `SOURCES.md`, and add a line here.
static BRANDS: &[Brand] = &[
    // Connectors
    Brand {
        keys: &["google-drive"],
        label: "Google Drive",
        mark: png!("google-drive"),
    },
    Brand {
        keys: &["google-sheets"],
        label: "Google Sheets",
        mark: png!("google-sheets"),
    },
    Brand {
        keys: &["google-calendar"],
        label: "Google Calendar",
        mark: png!("google-calendar"),
    },
    Brand {
        keys: &["gmail"],
        label: "Gmail",
        mark: png!("gmail"),
    },
    Brand {
        keys: &["outlook"],
        label: "Outlook",
        mark: png!("outlook"),
    },
    Brand {
        keys: &["outlook-calendar"],
        label: "Outlook Calendar",
        mark: png!("outlook"),
    },
    Brand {
        keys: &["onedrive"],
        label: "OneDrive",
        mark: png!("onedrive"),
    },
    Brand {
        keys: &["notion"],
        label: "Notion",
        mark: png!("notion"),
    },
    Brand {
        keys: &["agentmail"],
        label: "AgentMail",
        mark: png!("agentmail"),
    },
    Brand {
        keys: &["elevenlabs"],
        label: "ElevenLabs",
        mark: png!("elevenlabs"),
    },
    Brand {
        keys: &["tavily"],
        label: "Tavily",
        mark: png!("tavily"),
    },
    Brand {
        keys: &["firecrawl"],
        label: "Firecrawl",
        mark: png!("firecrawl"),
    },
    Brand {
        keys: &["composio"],
        label: "Composio",
        mark: png!("composio"),
    },
    // Providers
    Brand {
        keys: &["mistral"],
        label: "Mistral AI",
        mark: png!("mistral"),
    },
    Brand {
        keys: &["openai"],
        label: "OpenAI",
        mark: png!("openai"),
    },
    Brand {
        keys: &["xai"],
        label: "xAI (Grok)",
        mark: png!("xai"),
    },
    Brand {
        keys: &["deepseek"],
        label: "DeepSeek",
        mark: png!("deepseek"),
    },
    Brand {
        keys: &["groq"],
        label: "Groq",
        mark: png!("groq"),
    },
    Brand {
        keys: &["together"],
        label: "Together AI",
        mark: png!("together"),
    },
    Brand {
        keys: &["fireworks"],
        label: "Fireworks AI",
        mark: png!("fireworks"),
    },
    Brand {
        keys: &["openrouter"],
        label: "OpenRouter",
        mark: png!("openrouter"),
    },
    Brand {
        keys: &["cerebras"],
        label: "Cerebras",
        mark: png!("cerebras"),
    },
    Brand {
        keys: &["moonshot"],
        label: "Moonshot (Kimi)",
        mark: png!("moonshot"),
    },
    Brand {
        keys: &["minimax"],
        label: "MiniMax",
        mark: png!("minimax"),
    },
    Brand {
        keys: &["minimax-cn"],
        label: "MiniMax (China)",
        mark: png!("minimax"),
    },
    Brand {
        keys: &["google-antigravity", "antigravity"],
        label: "Google Antigravity",
        mark: png!("antigravity"),
    },
    Brand {
        keys: &["ollama"],
        label: "Ollama",
        mark: png!("ollama"),
    },
    // Generic marks that belong to no vendor
    Brand {
        keys: &["muse-code"],
        label: "Muse Code",
        mark: Mark::Svg(GENERIC_CODE),
    },
    Brand {
        keys: &["openai-compatible"],
        label: "OpenAI-Compatible",
        mark: Mark::Svg(GENERIC_PLUG),
    },
    Brand {
        keys: &["custom"],
        label: "Custom",
        mark: Mark::Svg(GENERIC_CUSTOM),
    },
];

/// Normalizes a caller-supplied key: lowercase, `_` -> `-`.
fn normalize(key: &str) -> String {
    key.to_ascii_lowercase().replace('_', "-")
}

fn find(key: &str) -> Option<&'static Brand> {
    let key = normalize(key);
    BRANDS
        .iter()
        .find(|brand| brand.keys.contains(&key.as_str()))
}

/// A `data:` URI for the brand's image, if we have one. Unknown keys return
/// `None` so callers can fall back to an initial-letter avatar.
pub fn brand_image_src(key: &str) -> Option<String> {
    let brand = find(key)?;
    Some(match brand.mark {
        Mark::Png(bytes) => format!("data:image/png;base64,{}", STANDARD.encode(bytes)),
        Mark::Svg(svg) => format!("data:image/svg+xml;base64,{}", STANDARD.encode(svg)),
    })
}

/// Returns a human-readable label for a known brand key (same normalization
/// as [`brand_image_src`]), for use in alt/title text. Unknown keys return
/// `None`.
pub fn brand_label(key: &str) -> Option<&'static str> {
    find(key).map(|brand| brand.label)
}

#[cfg(test)]
mod tests {
    use super::*;

    const PNG_MAGIC: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];

    /// Every key the UI may pass in.
    const ALL_KEYS: &[&str] = &[
        "google-drive",
        "google-sheets",
        "google-calendar",
        "gmail",
        "outlook",
        "outlook-calendar",
        "onedrive",
        "notion",
        "agentmail",
        "elevenlabs",
        "tavily",
        "firecrawl",
        "composio",
        "mistral",
        "openai",
        "xai",
        "deepseek",
        "groq",
        "together",
        "fireworks",
        "openrouter",
        "cerebras",
        "moonshot",
        "minimax",
        "minimax-cn",
        "google-antigravity",
        "antigravity",
        "muse-code",
        "ollama",
        "openai-compatible",
        "custom",
    ];

    #[test]
    fn every_known_key_has_an_image_and_a_label() {
        for key in ALL_KEYS {
            let src = brand_image_src(key).unwrap_or_else(|| panic!("missing image for {key}"));
            assert!(
                src.starts_with("data:image/png;base64,")
                    || src.starts_with("data:image/svg+xml;base64,"),
                "{key} must be a base64 data URI"
            );
            assert!(
                brand_label(key).is_some_and(|label| !label.is_empty()),
                "{key} must have a label"
            );
        }
    }

    #[test]
    fn vendor_logos_are_real_small_png_files() {
        let mut total = 0;
        for brand in BRANDS {
            if let Mark::Png(bytes) = brand.mark {
                assert!(
                    bytes.starts_with(&PNG_MAGIC),
                    "{} is not a PNG",
                    brand.label
                );
                // IHDR width and height are big-endian u32 at bytes 16..24.
                let width = u32::from_be_bytes(bytes[16..20].try_into().unwrap());
                let height = u32::from_be_bytes(bytes[20..24].try_into().unwrap());
                assert_eq!((width, height), (64, 64), "{} must be 64x64", brand.label);
                assert!(bytes.len() < 16 * 1024, "{} is too large", brand.label);
                total += bytes.len();
            }
        }
        assert!(total < 300 * 1024, "logos add {total} bytes");
    }

    #[test]
    fn the_image_payload_decodes_back_to_the_png() {
        let src = brand_image_src("notion").unwrap();
        let payload = src.strip_prefix("data:image/png;base64,").unwrap();
        let bytes = STANDARD.decode(payload).unwrap();
        assert!(bytes.starts_with(&PNG_MAGIC));
    }

    #[test]
    fn keys_are_unique_and_normalized_before_lookup() {
        let mut seen = std::collections::BTreeSet::new();
        for brand in BRANDS {
            for key in brand.keys {
                assert_eq!(*key, normalize(key), "{key} must be stored normalized");
                assert!(seen.insert(*key), "duplicate brand key {key}");
            }
        }
        assert_eq!(
            brand_image_src("Google_Drive"),
            brand_image_src("google-drive")
        );
        assert_eq!(
            brand_image_src("GOOGLE-DRIVE"),
            brand_image_src("google-drive")
        );
        assert_eq!(brand_image_src("MiniMax_CN"), brand_image_src("minimax-cn"));
        assert_eq!(
            brand_image_src("Google_Antigravity"),
            brand_image_src("antigravity")
        );
    }

    #[test]
    fn unknown_key_returns_none() {
        assert!(brand_image_src("definitely-not-a-brand").is_none());
        assert!(brand_label("definitely-not-a-brand").is_none());
        assert!(brand_image_src("").is_none());
        assert!(brand_label("").is_none());
    }

    #[test]
    fn labels_are_human_readable() {
        assert_eq!(
            brand_label("google-antigravity"),
            Some("Google Antigravity")
        );
        assert_eq!(brand_label("gmail"), Some("Gmail"));
        assert_eq!(brand_label("onedrive"), Some("OneDrive"));
        assert_eq!(brand_label("openai-compatible"), Some("OpenAI-Compatible"));
        assert_eq!(brand_label("minimax-cn"), Some("MiniMax (China)"));
    }
}
