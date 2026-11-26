//! Brand logos for connectors and providers.
//!
//! Each vendor's own icon is a small PNG under `assets/brands/`, embedded with
//! `include_bytes!` and shown as a `data:` URI, so nothing is fetched at run
//! time. `assets/brands/SOURCES.md` records where every file came from. The
//! logos are trademarks of their owners and only identify the integration.
//!
//! Adding a brand is one line in [`BRANDS`] (plus its PNG). Generic marks that
//! belong to no vendor (custom, OpenAI-compatible) use a neutral inline SVG.
//! Unknown keys return `None` so callers draw a letter avatar.

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

const SVG_GITHUB: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24"><rect x="2" y="2" width="20" height="20" rx="5" fill="#181717"/><path d="M12 5.4a6.7 6.7 0 0 0-2.12 13.06c.34.06.46-.14.46-.32v-1.24c-1.86.4-2.25-.79-2.25-.79-.3-.77-.74-.98-.74-.98-.61-.42.05-.41.05-.41.67.05 1.03.69 1.03.69.6 1.03 1.57.73 1.95.56.06-.43.24-.73.43-.9-1.49-.17-3.05-.74-3.05-3.31 0-.73.26-1.33.69-1.8-.07-.17-.3-.85.07-1.78 0 0 .56-.18 1.84.69a6.4 6.4 0 0 1 3.36 0c1.28-.87 1.84-.69 1.84-.69.37.93.14 1.61.07 1.78.43.47.69 1.07.69 1.8 0 2.58-1.57 3.14-3.06 3.3.24.21.46.62.46 1.25v1.85c0 .18.12.39.46.32A6.7 6.7 0 0 0 12 5.4Z" fill="#fff"/></svg>"##;

const SVG_TODOIST: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24"><rect x="2" y="2" width="20" height="20" rx="5" fill="#E44332"/><path d="M6.5 9.2 10.6 11.6 17.5 7.6M6.5 13 10.6 15.4 17.5 11.4M6.5 16.8 10.6 19.2 17.5 15.2" fill="none" stroke="#fff" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round"/></svg>"##;

const SVG_SLACK: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24"><rect x="2" y="2" width="20" height="20" rx="5" fill="#4A154B"/><path d="M9.5 14a1.2 1.2 0 1 1-1.2-1.2h1.2V14Zm.6 0a1.2 1.2 0 1 1 2.4 0v3a1.2 1.2 0 1 1-2.4 0v-3Z" fill="#E01E5A"/><path d="M10 9.5a1.2 1.2 0 1 1 1.2-1.2v1.2H10Zm0 .6a1.2 1.2 0 1 1 0 2.4H7a1.2 1.2 0 1 1 0-2.4h3Z" fill="#36C5F0"/><path d="M14.5 10a1.2 1.2 0 1 1 1.2 1.2h-1.2V10Zm-.6 0a1.2 1.2 0 1 1-2.4 0V7a1.2 1.2 0 1 1 2.4 0v3Z" fill="#2EB67D"/><path d="M14 14.5a1.2 1.2 0 1 1-1.2 1.2v-1.2H14Zm0-.6a1.2 1.2 0 1 1 0-2.4h3a1.2 1.2 0 1 1 0 2.4h-3Z" fill="#ECB22E"/></svg>"##;

const SVG_EXA: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24"><rect x="2" y="2" width="20" height="20" rx="5" fill="#1F40ED"/><path d="M7.5 7.5h9M7.5 12h6.5M7.5 16.5h9" fill="none" stroke="#fff" stroke-width="2" stroke-linecap="round"/><circle cx="16.5" cy="12" r="1.4" fill="#fff"/></svg>"##;

const SVG_BRAVE: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24"><rect x="2" y="2" width="20" height="20" rx="5" fill="#FB542B"/><path d="M12 5.8 16.8 8l-1.2 6.8L12 18.2l-3.6-3.4L7.2 8 12 5.8Z" fill="none" stroke="#fff" stroke-width="1.8" stroke-linejoin="round"/><path d="M10 11.2h4l-.6 2.6-1.4 1-1.4-1-.6-2.6Z" fill="#fff"/></svg>"##;

const SVG_LINEAR: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24"><rect x="2" y="2" width="20" height="20" rx="5" fill="#5E6AD2"/><path d="M6.4 13.8a6 6 0 0 0 3.8 3.8L6.4 13.8Zm-.3-2.5 6.6 6.6a6 6 0 0 0 1.8-.7L6.8 9.5a6 6 0 0 0-.7 1.8Zm2-3.4 8 8a6 6 0 1 0-8-8Z" fill="#fff"/></svg>"##;

const SVG_RESEND: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24"><rect x="2" y="2" width="20" height="20" rx="5" fill="#111827"/><path d="M7.5 16.5V7.5h4.8a2.8 2.8 0 0 1 0 5.6H9.8l5.2 3.4" fill="none" stroke="#fff" stroke-width="1.9" stroke-linecap="round" stroke-linejoin="round"/></svg>"##;

const SVG_WORKSPACE_FILES: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24"><rect x="2" y="2" width="20" height="20" rx="5" fill="#0F766E"/><path d="M6.5 8.5a1.5 1.5 0 0 1 1.5-1.5h2.6l1.4 1.5h4a1.5 1.5 0 0 1 1.5 1.5v5.5a1.5 1.5 0 0 1-1.5 1.5H8a1.5 1.5 0 0 1-1.5-1.5v-7Z" fill="none" stroke="#fff" stroke-width="1.7" stroke-linejoin="round"/></svg>"##;

const SVG_CLOUDFLARE: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24"><rect x="2" y="2" width="20" height="20" rx="5" fill="#F38020"/><path d="M16.2 15.8H8.1a2.6 2.6 0 0 1-.4-5.1 4 4 0 0 1 7.7-.8 2.9 2.9 0 0 1 .8 5.9Z" fill="#fff"/></svg>"##;

const SVG_POSTGRES: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24"><rect x="2" y="2" width="20" height="20" rx="5" fill="#336791"/><path d="M12 6.5c-2.6 0-4.5 1.7-4.5 4.2 0 2 1 3.7 2.2 4.5v2.3l2.3-1 2.3 1v-2.3c1.2-.8 2.2-2.5 2.2-4.5 0-2.5-1.9-4.2-4.5-4.2Z" fill="none" stroke="#fff" stroke-width="1.7" stroke-linejoin="round"/></svg>"##;

const SVG_SENTRY: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24"><rect x="2" y="2" width="20" height="20" rx="5" fill="#362D59"/><path d="M12.9 7.2a1 1 0 0 0-1.8 0l-4.6 8a1 1 0 0 0 .9 1.5h2.2v-1.6H8.5l3.5-6.1 3.5 6.1h-2.1v1.6h3.2a1 1 0 0 0 .9-1.5l-4.6-8Z" fill="#fff"/></svg>"##;

const SVG_STRIPE: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24"><rect x="2" y="2" width="20" height="20" rx="5" fill="#635BFF"/><path d="M14.7 10.3c0-.9-.7-1.3-1.9-1.3-1.4 0-2.6.5-3.4 1.1v-2.2c.9-.5 2.1-.9 3.4-.9 2.5 0 4.1 1.2 4.1 3.3 0 3.3-4.6 2.8-4.6 4.4 0 .6.6 1 1.6 1 1.3 0 2.6-.5 3.5-1.1v2.2c-1 .6-2.2.9-3.5.9-2.5 0-4-1.2-4-3.2 0-3.5 4.8-3 4.8-4.2Z" fill="#fff"/></svg>"##;

const SVG_ANTHROPIC: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24"><rect x="2" y="2" width="20" height="20" rx="5" fill="#191919"/><path d="M13.9 7h2.15l3.7 10h-2.1l-3.75-10Zm-4.55 0h2.25l3.75 10h-2.1l-.78-2.25H8.42L7.64 17H5.55l3.8-10Zm-.32 6.02h2.83l-1.41-4.1-1.42 4.1Z" fill="#FAF9F6"/></svg>"##;

const SVG_CLAUDE: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24"><rect x="2" y="2" width="20" height="20" rx="5" fill="#D97757"/><g stroke="#fff" stroke-linecap="round" stroke-width="1.45"><line x1="12" y1="6.3" x2="12" y2="17.7"/><line x1="6.3" y1="12" x2="17.7" y2="12"/><line x1="7.8" y1="7.8" x2="16.2" y2="16.2"/><line x1="16.2" y1="7.8" x2="7.8" y2="16.2"/><line x1="9.2" y1="7.1" x2="14.8" y2="16.9"/><line x1="7.1" y1="14.8" x2="16.9" y2="9.2"/></g><circle cx="12" cy="12" r="1.8" fill="#fff"/></svg>"##;

const SVG_WHATSAPP: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24"><rect x="2" y="2" width="20" height="20" rx="5" fill="#25D366"/><path d="M12 6.2a5.8 5.8 0 0 0-5 8.7l-.7 2.8 2.9-.7A5.8 5.8 0 1 0 12 6.2Z" fill="none" stroke="#fff" stroke-width="1.65" stroke-linejoin="round"/><path d="M9.9 9.7c-.2-.4-.3-.4-.5-.4h-.5c-.2 0-.4.1-.6.4-.2.2-.8.8-.8 1.9s.8 2.2.9 2.3c.1.2 1.6 2.5 3.9 3.4 1.9.7 2.3.6 2.7.5.4-.1 1.3-.5 1.5-1 .2-.5.2-.9.1-1-.1-.1-.2-.2-.6-.4l-1.3-.6c-.2-.1-.3-.1-.5.1l-.5.6c-.2.2-.3.2-.5.1-.3-.1-1-.4-1.9-1.2-.7-.6-1.2-1.4-1.3-1.6-.1-.2 0-.4.1-.5l.4-.5.2-.4c.1-.2 0-.3 0-.4l-.6-1.3Z" fill="#fff"/></svg>"##;

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
        keys: &["notion", "notion-mcp"],
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
    Brand {
        keys: &["github", "github-mcp", "github-copilot"],
        label: "GitHub",
        mark: Mark::Svg(SVG_GITHUB),
    },
    Brand {
        keys: &["todoist"],
        label: "Todoist",
        mark: Mark::Svg(SVG_TODOIST),
    },
    Brand {
        keys: &["slack", "slack-mcp"],
        label: "Slack",
        mark: Mark::Svg(SVG_SLACK),
    },
    Brand {
        keys: &["whatsapp"],
        label: "WhatsApp",
        mark: Mark::Svg(SVG_WHATSAPP),
    },
    Brand {
        keys: &["exa"],
        label: "Exa Search",
        mark: Mark::Svg(SVG_EXA),
    },
    // Marketplace plugins
    Brand {
        keys: &["brave-search"],
        label: "Brave Search",
        mark: Mark::Svg(SVG_BRAVE),
    },
    Brand {
        keys: &["linear"],
        label: "Linear",
        mark: Mark::Svg(SVG_LINEAR),
    },
    Brand {
        keys: &["resend"],
        label: "Resend",
        mark: Mark::Svg(SVG_RESEND),
    },
    Brand {
        keys: &["workspace-files"],
        label: "Workspace Files",
        mark: Mark::Svg(SVG_WORKSPACE_FILES),
    },
    Brand {
        keys: &["cloudflare"],
        label: "Cloudflare",
        mark: Mark::Svg(SVG_CLOUDFLARE),
    },
    Brand {
        keys: &["postgres-read"],
        label: "PostgreSQL",
        mark: Mark::Svg(SVG_POSTGRES),
    },
    Brand {
        keys: &["sentry"],
        label: "Sentry",
        mark: Mark::Svg(SVG_SENTRY),
    },
    Brand {
        keys: &["stripe-read"],
        label: "Stripe",
        mark: Mark::Svg(SVG_STRIPE),
    },
    // Providers
    Brand {
        keys: &["mistral"],
        label: "Mistral AI",
        mark: png!("mistral"),
    },
    Brand {
        keys: &["openai", "openai-codex", "chatgpt-codex"],
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
    Brand {
        keys: &["gemini"],
        label: "Google Gemini",
        mark: png!("gemini"),
    },
    Brand {
        keys: &["perplexity"],
        label: "Perplexity",
        mark: png!("perplexity"),
    },
    Brand {
        keys: &["huggingface"],
        label: "Hugging Face",
        mark: png!("huggingface"),
    },
    Brand {
        keys: &["nebius"],
        label: "Nebius",
        mark: png!("nebius"),
    },
    Brand {
        keys: &["sambanova"],
        label: "SambaNova",
        mark: png!("sambanova"),
    },
    Brand {
        keys: &["inception", "inceptionlabs"],
        label: "Inception Labs",
        mark: png!("inception"),
    },
    Brand {
        keys: &["anthropic"],
        label: "Anthropic",
        mark: Mark::Svg(SVG_ANTHROPIC),
    },
    Brand {
        keys: &["claude", "claude-code", "claude-agent", "claude-agent-sdk"],
        label: "Claude",
        mark: Mark::Svg(SVG_CLAUDE),
    },
    Brand {
        keys: &["muse-code"],
        label: "Muse Code",
        mark: png!("muse"),
    },
    Brand {
        keys: &["opencode-go", "opencode-zen"],
        label: "OpenCode",
        mark: png!("opencode"),
    },
    Brand {
        keys: &["nous"],
        label: "Nous Portal",
        mark: png!("nous"),
    },
    Brand {
        keys: &["kiro"],
        label: "AWS Kiro",
        mark: png!("kiro"),
    },
    Brand {
        keys: &["devin", "devin-cli"],
        label: "Devin",
        mark: png!("devin"),
    },
    // Generic marks that belong to no vendor
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

/// Returns whether a brand key has a dedicated PNG or SVG mark.
pub fn has_mark(key: &str) -> bool {
    find(key).is_some()
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
        "notion-mcp",
        "agentmail",
        "elevenlabs",
        "tavily",
        "firecrawl",
        "composio",
        "github",
        "github-mcp",
        "github-copilot",
        "todoist",
        "slack",
        "slack-mcp",
        "whatsapp",
        "exa",
        "brave-search",
        "linear",
        "resend",
        "workspace-files",
        "cloudflare",
        "postgres-read",
        "sentry",
        "stripe-read",
        "mistral",
        "openai",
        "openai-codex",
        "chatgpt-codex",
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
        "anthropic",
        "claude",
        "claude-code",
        "claude-agent",
        "claude-agent-sdk",
        "opencode-go",
        "opencode-zen",
        "nous",
        "kiro",
        "devin",
        "devin-cli",
        "ollama",
        "gemini",
        "perplexity",
        "huggingface",
        "nebius",
        "sambanova",
        "inception",
        "inceptionlabs",
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
