//! Inline brand SVG logos for connectors and providers.
//!
//! `brand_svg` maps a connector id or provider kind (lowercased, `_` -> `-`)
//! to a small inline SVG. Unknown keys return `None` so callers can fall back
//! to an initial-letter avatar. SVGs use literal brand fills; the container
//! (`.brand-logo`) provides the themed background and border.
//!
//! Every mark is a hand-simplified official brand glyph on a `0 0 24 24`
//! viewBox with `width`/`height` at `100%` so it scales to the container.
//! No external images are hotlinked; each SVG stays well under 1.5 KB.

/// Normalizes a caller-supplied key: lowercase, `_` -> `-`.
fn normalize(key: &str) -> String {
    key.to_ascii_lowercase().replace('_', "-")
}

const GOOGLE_DRIVE: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" width="100%" height="100%"><path fill="#FFBA00" d="M8.5 3h7L23 16h-7z"/><path fill="#00AC47" d="M8.5 3 1 16l3.5 6L12 9z"/><path fill="#2684FC" d="M4.5 22h15L23 16H8z"/></svg>"##;

const GOOGLE_SHEETS: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" width="100%" height="100%"><path fill="#0F9D58" d="M5.5 2.5h8.2L19 7.8v12.7a1.5 1.5 0 0 1-1.5 1.5h-12A1.5 1.5 0 0 1 4 20.5v-16A1.5 1.5 0 0 1 5.5 2.5z"/><path fill="#087F46" d="M13.7 2.5 19 7.8h-4.2a1.1 1.1 0 0 1-1.1-1.1z"/><rect x="8" y="11.5" width="8" height="7" rx="0.8" fill="#fff"/><path stroke="#0F9D58" stroke-width="1.2" d="M8 15.2h8M12 11.5v7"/></svg>"##;

const GOOGLE_CALENDAR: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" width="100%" height="100%"><rect x="3" y="3" width="18" height="18" rx="3.5" fill="#4285F4"/><text x="12" y="16.8" text-anchor="middle" font-family="Arial, Helvetica, sans-serif" font-size="10" font-weight="700" fill="#fff">31</text></svg>"##;

const GMAIL: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" width="100%" height="100%"><rect x="2" y="4.5" width="20" height="15" rx="2" fill="#fff"/><path fill="#4285F4" d="M4.8 4.5H4a2 2 0 0 0-2 2v11a2 2 0 0 0 2 2h.8z"/><path fill="#34A853" d="M19.2 4.5H20a2 2 0 0 1 2 2v11a2 2 0 0 1-2 2h-.8z"/><path fill="#EA4335" d="M4.8 4.5 12 10.6l7.2-6.1v3.4L12 14l-7.2-6.1z"/><path fill="#FBBC04" d="M2 4.5h2.8L2 7.3z"/></svg>"##;

const OUTLOOK: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" width="100%" height="100%"><rect x="3" y="3" width="18" height="18" rx="3.5" fill="#0078D4"/><circle cx="12" cy="12" r="5.2" fill="none" stroke="#fff" stroke-width="2.6"/></svg>"##;

const NOTION: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" width="100%" height="100%"><rect x="3" y="3" width="18" height="18" rx="2.5" fill="#000"/><path fill="#fff" d="M7.5 17V7h2.6l4.4 6.4V7h2.5v10h-2.6l-4.4-6.4V17z"/></svg>"##;

const AGENTMAIL: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" width="100%" height="100%"><rect x="2.5" y="2.5" width="19" height="19" rx="5" fill="#6366F1"/><rect x="6" y="8.5" width="12" height="8" rx="1.5" fill="none" stroke="#fff" stroke-width="1.6"/><path d="m6.8 9.5 5.2 4 5.2-4" fill="none" stroke="#fff" stroke-width="1.6" stroke-linecap="round" stroke-linejoin="round"/></svg>"##;

const ELEVENLABS: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" width="100%" height="100%"><rect x="2.5" y="2.5" width="19" height="19" rx="5" fill="#fff"/><rect x="8.1" y="6.5" width="2.8" height="11" rx="1.4" fill="#000"/><rect x="13.1" y="6.5" width="2.8" height="11" rx="1.4" fill="#000"/></svg>"##;

const TAVILY: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" width="100%" height="100%"><circle cx="12" cy="12" r="8.5" fill="#0D9488"/><path d="M12 6.2a5.8 5.8 0 1 0 5.8 5.8" fill="none" stroke="#fff" stroke-width="2" stroke-linecap="round"/><circle cx="12" cy="12" r="2.4" fill="#fff"/></svg>"##;

const FIRECRAWL: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" width="100%" height="100%"><path fill="#F97316" d="M12 2.5s5.5 4.8 5.5 9.7a5.5 5.5 0 0 1-11 0c0-2.3 1.1-4.4 2.3-6.1.2 1.5.9 2.7 2 3.4-.5-3.1.2-5.4 1.2-7z"/><path fill="#FDBA74" d="M12 11.5c1.5 1.4 2.3 2.6 2.3 4a2.3 2.3 0 0 1-4.6 0c0-1.4.8-2.6 2.3-4z"/></svg>"##;

const MISTRAL: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" width="100%" height="100%"><path fill="#FFD800" d="M3 4.5h3v3H3zM18 4.5h3v3h-3z"/><path fill="#FFAF00" d="M3 7.5h3v3H3zM6 7.5h3v3H6zM15 7.5h3v3h-3zM18 7.5h3v3h-3z"/><path fill="#FF8205" d="M3 10.5h3v3H3zM9 10.5h3v3H9zM12 10.5h3v3h-3zM18 10.5h3v3h-3z"/><path fill="#FA500F" d="M3 13.5h3v3H3zM18 13.5h3v3h-3z"/><path fill="#E10500" d="M3 16.5h3v3H3zM18 16.5h3v3h-3z"/></svg>"##;

const OPENAI: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" width="100%" height="100%"><rect x="2.5" y="2.5" width="19" height="19" rx="5" fill="#fff"/><g fill="#000"><path d="M12 4.4l1.7 2.9L12 10.2l-1.7-2.9z"/><path d="M12 4.4l1.7 2.9L12 10.2l-1.7-2.9z" transform="rotate(60 12 12)"/><path d="M12 4.4l1.7 2.9L12 10.2l-1.7-2.9z" transform="rotate(120 12 12)"/><path d="M12 4.4l1.7 2.9L12 10.2l-1.7-2.9z" transform="rotate(180 12 12)"/><path d="M12 4.4l1.7 2.9L12 10.2l-1.7-2.9z" transform="rotate(240 12 12)"/><path d="M12 4.4l1.7 2.9L12 10.2l-1.7-2.9z" transform="rotate(300 12 12)"/></g></svg>"##;

const XAI: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" width="100%" height="100%"><rect x="2.5" y="2.5" width="19" height="19" rx="5" fill="#000"/><path fill="#fff" d="M6.5 6h3.1l2.4 3.4L14.4 6h3.1l-4.1 6 4.1 6h-3.1l-2.4-3.4L9.6 18H6.5l4.1-6z"/></svg>"##;

const DEEPSEEK: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" width="100%" height="100%"><rect x="2.5" y="2.5" width="19" height="19" rx="5" fill="#4D6BFE"/><path fill="#fff" d="M5.5 14.5C8 9.5 12.5 7 18.5 7c-1.5 5-6.5 8-13 7.5z"/><circle cx="15.3" cy="10" r="1.1" fill="#4D6BFE"/></svg>"##;

const GROQ: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" width="100%" height="100%"><rect x="2.5" y="2.5" width="19" height="19" rx="5" fill="#F55036"/><path d="M17 8.6A6 6 0 1 0 18 12h-6" fill="none" stroke="#fff" stroke-width="2.4" stroke-linecap="round"/></svg>"##;

const TOGETHER: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" width="100%" height="100%"><rect x="2.5" y="2.5" width="19" height="19" rx="5" fill="#111827"/><circle cx="8" cy="9" r="2.6" fill="#22D3EE"/><circle cx="16" cy="9" r="2.6" fill="#A78BFA"/><circle cx="12" cy="15.5" r="2.6" fill="#F472B6"/></svg>"##;

const FIREWORKS: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" width="100%" height="100%"><rect x="2.5" y="2.5" width="19" height="19" rx="5" fill="#312E81"/><g stroke="#FBBF24" stroke-width="1.7" stroke-linecap="round"><path d="M12 4.5v3M12 16.5v3M4.5 12h3M16.5 12h3M6.7 6.7l2.1 2.1M15.2 15.2l2.1 2.1M17.3 6.7l-2.1 2.1M8.8 15.2l-2.1 2.1"/></g><circle cx="12" cy="12" r="2.3" fill="#FDE68A"/></svg>"##;

const OPENROUTER: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" width="100%" height="100%"><rect x="2.5" y="2.5" width="19" height="19" rx="5" fill="#0F172A"/><path d="M5.5 7.5 10 12l-4.5 4.5M12 7.5l4.5 4.5-4.5 4.5" fill="none" stroke="#fff" stroke-width="2.2" stroke-linecap="round" stroke-linejoin="round"/></svg>"##;

const CEREBRAS: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" width="100%" height="100%"><rect x="2.5" y="2.5" width="19" height="19" rx="5" fill="#18181B"/><rect x="6" y="6" width="12" height="12" rx="2" fill="none" stroke="#fff" stroke-width="1.8"/><rect x="9.6" y="9.6" width="4.8" height="4.8" rx="1" fill="#fff"/><path d="M9 6V3.8M15 6V3.8M9 20.2V18M15 20.2V18M6 9H3.8M6 15H3.8M20.2 9H18M20.2 15H18" stroke="#fff" stroke-width="1.6" stroke-linecap="round"/></svg>"##;

const MOONSHOT: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" width="100%" height="100%"><rect x="2.5" y="2.5" width="19" height="19" rx="5" fill="#0B132B"/><circle cx="10.5" cy="12.5" r="6.5" fill="#F1F5F9"/><circle cx="13.8" cy="10.5" r="5.6" fill="#0B132B"/><circle cx="17.5" cy="7" r="1.2" fill="#FDE047"/></svg>"##;

const MINIMAX: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" width="100%" height="100%"><rect x="2.5" y="2.5" width="19" height="19" rx="5" fill="#fff"/><path d="M5.5 17V7.5l3.4 4.6L12 8.5l3.1 3.6 3.4-4.6V17" fill="none" stroke="#E11D48" stroke-width="2.3" stroke-linecap="round" stroke-linejoin="round"/></svg>"##;

const ANTIGRAVITY: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" width="100%" height="100%"><rect x="2.5" y="2.5" width="19" height="19" rx="5" fill="#1E1B4B"/><g fill="none" stroke-width="1.5"><ellipse cx="12" cy="12" rx="8" ry="3.1" stroke="#818CF8"/><ellipse cx="12" cy="12" rx="8" ry="3.1" stroke="#38BDF8" transform="rotate(60 12 12)"/><ellipse cx="12" cy="12" rx="8" ry="3.1" stroke="#F472B6" transform="rotate(-60 12 12)"/></g><circle cx="12" cy="12" r="2.1" fill="#FDE047"/></svg>"##;

const MUSE_CODE: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" width="100%" height="100%"><rect x="2.5" y="2.5" width="19" height="19" rx="5" fill="#0891B2"/><path d="m8.5 8-3.5 4 3.5 4M15.5 8l3.5 4-3.5 4M13.2 6.5l-2.4 11" fill="none" stroke="#fff" stroke-width="1.9" stroke-linecap="round" stroke-linejoin="round"/></svg>"##;

const OLLAMA: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" width="100%" height="100%"><rect x="2.5" y="2.5" width="19" height="19" rx="5" fill="#fff"/><path fill="#111" d="M8 5.6c-.9-.9-2.5-.4-2.5.8v4.4c0 3.9 2.6 6.7 6.5 6.7s6.5-2.8 6.5-6.7V6.4c0-1.2-1.6-1.7-2.5-.8L14.5 8h-5z"/><circle cx="9.9" cy="11.6" r="1" fill="#fff"/><circle cx="14.1" cy="11.6" r="1" fill="#fff"/></svg>"##;

const OPENAI_COMPATIBLE: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" width="100%" height="100%"><rect x="2.5" y="2.5" width="19" height="19" rx="5" fill="#475569"/><path d="M9 8V4.8M15 8V4.8M7 8h10v3.5a5 5 0 0 1-10 0zM12 16.5v3.7" fill="none" stroke="#fff" stroke-width="1.9" stroke-linecap="round" stroke-linejoin="round"/></svg>"##;

const CUSTOM: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" width="100%" height="100%"><rect x="2.5" y="2.5" width="19" height="19" rx="5" fill="#334155"/><path d="M5.5 8.5h13M5.5 15.5h13" stroke="#fff" stroke-width="1.8" stroke-linecap="round"/><circle cx="9.5" cy="8.5" r="2.1" fill="#334155" stroke="#fff" stroke-width="1.8"/><circle cx="14.5" cy="15.5" r="2.1" fill="#334155" stroke="#fff" stroke-width="1.8"/></svg>"##;

/// Returns the inline SVG markup for a brand key, if we have one.
///
/// The key is normalized (lowercased, `_` -> `-`) before lookup; unknown
/// keys return `None` so callers can fall back to an initial-letter avatar.
pub fn brand_svg(key: &str) -> Option<&'static str> {
    let key = normalize(key);
    Some(match key.as_str() {
        "google-drive" => GOOGLE_DRIVE,
        "google-sheets" => GOOGLE_SHEETS,
        "google-calendar" => GOOGLE_CALENDAR,
        "gmail" => GMAIL,
        "outlook" | "outlook-calendar" => OUTLOOK,
        "notion" => NOTION,
        "agentmail" => AGENTMAIL,
        "elevenlabs" => ELEVENLABS,
        "tavily" => TAVILY,
        "firecrawl" => FIRECRAWL,
        "mistral" => MISTRAL,
        "openai" => OPENAI,
        "xai" => XAI,
        "deepseek" => DEEPSEEK,
        "groq" => GROQ,
        "together" => TOGETHER,
        "fireworks" => FIREWORKS,
        "openrouter" => OPENROUTER,
        "cerebras" => CEREBRAS,
        "moonshot" => MOONSHOT,
        "minimax" | "minimax-cn" => MINIMAX,
        "google-antigravity" | "antigravity" => ANTIGRAVITY,
        "muse-code" => MUSE_CODE,
        "ollama" => OLLAMA,
        "openai-compatible" => OPENAI_COMPATIBLE,
        "custom" => CUSTOM,
        _ => return None,
    })
}

/// Returns a human-readable label for a known brand key (same normalization
/// as [`brand_svg`]), for use in alt/title text. Unknown keys return `None`.
pub fn brand_label(key: &str) -> Option<&'static str> {
    let key = normalize(key);
    Some(match key.as_str() {
        "google-drive" => "Google Drive",
        "google-sheets" => "Google Sheets",
        "google-calendar" => "Google Calendar",
        "gmail" => "Gmail",
        "outlook" => "Outlook",
        "outlook-calendar" => "Outlook Calendar",
        "notion" => "Notion",
        "agentmail" => "AgentMail",
        "elevenlabs" => "ElevenLabs",
        "tavily" => "Tavily",
        "firecrawl" => "Firecrawl",
        "mistral" => "Mistral AI",
        "openai" => "OpenAI",
        "xai" => "xAI (Grok)",
        "deepseek" => "DeepSeek",
        "groq" => "Groq",
        "together" => "Together AI",
        "fireworks" => "Fireworks AI",
        "openrouter" => "OpenRouter",
        "cerebras" => "Cerebras",
        "moonshot" => "Moonshot (Kimi)",
        "minimax" => "MiniMax",
        "minimax-cn" => "MiniMax (China)",
        "google-antigravity" | "antigravity" => "Google Antigravity",
        "muse-code" => "Muse Code",
        "ollama" => "Ollama",
        "openai-compatible" => "OpenAI-Compatible",
        "custom" => "Custom",
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every key the UI may pass to `brand_svg` / `brand_label`.
    const ALL_KEYS: &[&str] = &[
        // Connectors
        "google-drive",
        "google-sheets",
        "google-calendar",
        "gmail",
        "outlook",
        "outlook-calendar",
        "notion",
        "agentmail",
        "elevenlabs",
        "tavily",
        "firecrawl",
        // Providers / kinds
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
    fn every_known_key_returns_a_well_formed_small_svg() {
        for key in ALL_KEYS {
            let svg = brand_svg(key).unwrap_or_else(|| panic!("missing brand svg for {key}"));
            assert!(svg.starts_with("<svg"), "{key} must start with <svg");
            assert!(svg.contains("</svg>"), "{key} must contain </svg>");
            assert!(
                svg.contains("viewBox=\"0 0 24 24\""),
                "{key} must use the 0 0 24 24 viewBox"
            );
            assert!(
                svg.contains("width=\"100%\"") && svg.contains("height=\"100%\""),
                "{key} must fill its container"
            );
            assert!(
                svg.len() < 1536,
                "{key} svg is too large: {} bytes",
                svg.len()
            );
            assert!(
                brand_label(key).is_some_and(|label| !label.is_empty()),
                "{key} must have a label"
            );
        }
    }

    #[test]
    fn keys_are_normalized_before_lookup() {
        assert_eq!(brand_svg("Google_Drive"), brand_svg("google-drive"));
        assert_eq!(brand_svg("GOOGLE-DRIVE"), brand_svg("google-drive"));
        assert_eq!(brand_svg("MiniMax_CN"), brand_svg("minimax-cn"));
        assert_eq!(brand_svg("Google_Antigravity"), brand_svg("antigravity"));
        assert_eq!(
            brand_label("Google_Antigravity"),
            brand_label("google-antigravity")
        );
    }

    #[test]
    fn unknown_key_returns_none() {
        assert!(brand_svg("definitely-not-a-brand").is_none());
        assert!(brand_label("definitely-not-a-brand").is_none());
        assert!(brand_svg("").is_none());
        assert!(brand_label("").is_none());
    }

    #[test]
    fn labels_are_human_readable() {
        assert_eq!(
            brand_label("google-antigravity"),
            Some("Google Antigravity")
        );
        assert_eq!(brand_label("antigravity"), Some("Google Antigravity"));
        assert_eq!(brand_label("gmail"), Some("Gmail"));
        assert_eq!(brand_label("google-drive"), Some("Google Drive"));
        assert_eq!(brand_label("openai-compatible"), Some("OpenAI-Compatible"));
        assert_eq!(brand_label("minimax-cn"), Some("MiniMax (China)"));
    }
}
