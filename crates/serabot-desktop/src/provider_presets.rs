//! Provider presets for the "Add provider" sheet and Provider Directory.
//!
//! Data only: no I/O, no UI. The `kind` values are the server's own provider
//! kinds (`crates/serabot/src/providers.rs`), so a preset can be posted to
//! `POST /v1/admin/providers` unchanged.

/// One ready-made provider profile.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ProviderPreset {
    /// Stable id of the preset. Used to look the preset up again.
    pub id: &'static str,
    /// Name shown in the picker.
    pub label: &'static str,
    /// Subtitle or short summary of the provider.
    pub subtitle: &'static str,
    /// Base URL the server talks to. Empty for the custom preset.
    pub base_url: &'static str,
    /// Server provider kind (`openai`, `mistral`, `groq`, `openrouter`,
    /// `openai-compatible`, `google-antigravity`, etc.).
    pub kind: &'static str,
    /// Default id prefilled in the provider form.
    pub default_id: &'static str,
    /// Model ids offered as a starting list. The owner can change them.
    pub models: &'static [&'static str],
    /// Name of the secret in the encrypted store that holds the API key.
    pub secret_name: &'static str,
    /// Whether to fetch the live model list (/models).
    pub catalog_fetch: bool,
    /// Auth type: "api_key" or "oauth".
    pub auth: &'static str,
    /// Badge label: "OAuth", "API Key", "Local", "Custom".
    pub badge: &'static str,
    /// One short line for the owner.
    pub note: &'static str,
}

const PRESETS: &[ProviderPreset] = &[
    ProviderPreset {
        id: "antigravity",
        label: "Google Antigravity",
        subtitle: "Google Cloud Code OAuth (PKCE) — Gemini & Claude models",
        base_url: "https://daily-cloudcode-pa.googleapis.com",
        kind: "google-antigravity",
        default_id: "antigravity",
        models: &[
            "gemini-2.5-pro",
            "gemini-2.5-flash",
            "claude-sonnet-4-5",
            "claude-opus-4",
        ],
        secret_name: "antigravity_access_token",
        catalog_fetch: true,
        auth: "oauth",
        badge: "OAuth",
        note: "Google Cloud Code OAuth (PKCE) — Gemini & Claude models",
    },
    ProviderPreset {
        id: "deepseek",
        label: "DeepSeek",
        subtitle: "DeepSeek V3, R1 & Reasoner API",
        base_url: "https://api.deepseek.com/v1",
        kind: "deepseek",
        default_id: "deepseek",
        models: &["deepseek-chat", "deepseek-reasoner"],
        secret_name: "deepseek_api_key",
        catalog_fetch: true,
        auth: "api_key",
        badge: "API Key",
        note: "DeepSeek V3, R1 & Reasoner API",
    },
    ProviderPreset {
        id: "minimax",
        label: "MiniMax",
        subtitle: "MiniMax M3.1 Flash, M3 & M-Plan API",
        base_url: "https://api.minimax.io/v1",
        kind: "minimax",
        default_id: "minimax",
        models: &["MiniMax-M3.1-Flash", "MiniMax-M3", "MiniMax-Text-01"],
        secret_name: "minimax_m_plan_key",
        catalog_fetch: true,
        auth: "api_key",
        badge: "API Key",
        note: "MiniMax M3.1 Flash, M3 & M-Plan API",
    },
    ProviderPreset {
        id: "minimax-cn",
        label: "MiniMax (China)",
        subtitle: "MiniMax China domestic endpoint",
        base_url: "https://api.minimaxi.com/v1",
        kind: "minimax-cn",
        default_id: "minimax-cn",
        models: &["MiniMax-M3.1-Flash", "MiniMax-M3"],
        secret_name: "minimax_cn_api_key",
        catalog_fetch: true,
        auth: "api_key",
        badge: "API Key",
        note: "MiniMax China domestic endpoint",
    },
    ProviderPreset {
        id: "mistral",
        label: "Mistral AI",
        subtitle: "Mistral Large, Magistral, Codestral & Ministral",
        base_url: "https://api.mistral.ai/v1",
        kind: "mistral",
        default_id: "mistral",
        models: &[
            "mistral-large-latest",
            "mistral-medium-latest",
            "mistral-small-latest",
            "codestral-latest",
            "ministral-8b-latest",
            "magistral-medium-latest",
            "devstral-small-latest",
        ],
        secret_name: "mistral_api_key",
        catalog_fetch: true,
        auth: "api_key",
        badge: "API Key",
        note: "Mistral Large, Magistral, Codestral & Ministral",
    },
    ProviderPreset {
        id: "openai",
        label: "OpenAI",
        subtitle: "GPT-4.1, GPT-4o & o-series reasoning",
        base_url: "https://api.openai.com/v1",
        kind: "openai",
        default_id: "openai",
        models: &["gpt-4.1", "gpt-4.1-mini", "gpt-4o", "o3", "o4-mini"],
        secret_name: "openai_api_key",
        catalog_fetch: true,
        auth: "api_key",
        badge: "API Key",
        note: "GPT-4.1, GPT-4o & o-series reasoning",
    },
    ProviderPreset {
        id: "openrouter",
        label: "OpenRouter",
        subtitle: "Unified router for Claude, Gemini, Llama & DeepSeek",
        base_url: "https://openrouter.ai/api/v1",
        kind: "openrouter",
        default_id: "openrouter",
        models: &[
            "anthropic/claude-sonnet-4",
            "google/gemini-2.5-pro",
            "deepseek/deepseek-r1",
        ],
        secret_name: "openrouter_api_key",
        catalog_fetch: true,
        auth: "api_key",
        badge: "API Key",
        note: "Unified router for Claude, Gemini, Llama & DeepSeek",
    },
    ProviderPreset {
        id: "groq",
        label: "Groq",
        subtitle: "Ultra-fast LPU inference for Llama & Qwen",
        base_url: "https://api.groq.com/openai/v1",
        kind: "groq",
        default_id: "groq",
        models: &[
            "llama-3.3-70b-versatile",
            "llama-3.1-8b-instant",
            "qwen-qwq-32b",
        ],
        secret_name: "groq_api_key",
        catalog_fetch: true,
        auth: "api_key",
        badge: "API Key",
        note: "Ultra-fast LPU inference for Llama & Qwen",
    },
    ProviderPreset {
        id: "xai",
        label: "xAI (Grok)",
        subtitle: "Grok 3 & Grok 3 Mini API",
        base_url: "https://api.x.ai/v1",
        kind: "xai",
        default_id: "xai",
        models: &["grok-3-beta", "grok-3-mini-beta"],
        secret_name: "xai_api_key",
        catalog_fetch: true,
        auth: "api_key",
        badge: "API Key",
        note: "Grok 3 & Grok 3 Mini API",
    },
    ProviderPreset {
        id: "moonshot",
        label: "Moonshot (Kimi)",
        subtitle: "Kimi K2 & Moonshot v1 long-context API",
        base_url: "https://api.moonshot.ai/v1",
        kind: "moonshot",
        default_id: "moonshot",
        models: &["kimi-k2-0711-preview", "moonshot-v1-128k"],
        secret_name: "moonshot_api_key",
        catalog_fetch: true,
        auth: "api_key",
        badge: "API Key",
        note: "Kimi K2 & Moonshot v1 long-context API",
    },
    ProviderPreset {
        id: "cerebras",
        label: "Cerebras",
        subtitle: "Wafer-scale high-speed inference",
        base_url: "https://api.cerebras.ai/v1",
        kind: "cerebras",
        default_id: "cerebras",
        models: &["llama-3.3-70b", "llama3.1-8b"],
        secret_name: "cerebras_api_key",
        catalog_fetch: true,
        auth: "api_key",
        badge: "API Key",
        note: "Wafer-scale high-speed inference",
    },
    ProviderPreset {
        id: "together",
        label: "Together AI",
        subtitle: "Open-weights cloud inference",
        base_url: "https://api.together.xyz/v1",
        kind: "together",
        default_id: "together",
        models: &[
            "meta-llama/Llama-3.3-70B-Instruct-Turbo",
            "deepseek-ai/DeepSeek-R1",
        ],
        secret_name: "together_api_key",
        catalog_fetch: true,
        auth: "api_key",
        badge: "API Key",
        note: "Open-weights cloud inference",
    },
    ProviderPreset {
        id: "fireworks",
        label: "Fireworks AI",
        subtitle: "Fast production open-model inference",
        base_url: "https://api.fireworks.ai/inference/v1",
        kind: "fireworks",
        default_id: "fireworks",
        models: &[
            "accounts/fireworks/models/deepseek-v3",
            "accounts/fireworks/models/llama-v3p3-70b-instruct",
        ],
        secret_name: "fireworks_api_key",
        catalog_fetch: true,
        auth: "api_key",
        badge: "API Key",
        note: "Fast production open-model inference",
    },
    ProviderPreset {
        id: "muse-code",
        label: "Muse Code",
        subtitle: "Muse Spark OAuth device sign-in",
        base_url: "https://api.musecode.ai/v1",
        kind: "muse-code",
        default_id: "muse",
        models: &["muse-spark-1.3"],
        secret_name: "muse_code_api_key",
        catalog_fetch: true,
        auth: "oauth",
        badge: "OAuth",
        note: "Muse Spark OAuth device sign-in",
    },
    ProviderPreset {
        id: "ollama",
        label: "Ollama (Local)",
        subtitle: "Local model server on 127.0.0.1:11434",
        base_url: "http://127.0.0.1:11434/v1",
        kind: "openai-compatible",
        default_id: "ollama",
        models: &["llama3.3", "qwen2.5-coder:32b"],
        secret_name: "ollama_api_key",
        catalog_fetch: false,
        auth: "api_key",
        badge: "Local",
        note: "Local model server on 127.0.0.1:11434",
    },
    ProviderPreset {
        id: "custom",
        label: "Custom OpenAI-Compatible",
        subtitle: "Any /v1/chat/completions gateway or proxy",
        base_url: "",
        kind: "openai-compatible",
        default_id: "",
        models: &[],
        secret_name: "",
        catalog_fetch: false,
        auth: "api_key",
        badge: "Custom",
        note: "Any /v1/chat/completions gateway or proxy",
    },
];

/// The preset catalog, in picker order.
pub fn presets() -> &'static [ProviderPreset] {
    PRESETS
}

/// One preset by id.
pub fn preset_for(id: &str) -> Option<&'static ProviderPreset> {
    PRESETS.iter().find(|preset| preset.id == id)
}

/// The preset that leaves the base URL to the owner.
pub fn custom_preset() -> &'static ProviderPreset {
    preset_for("custom").expect("the custom preset is always present")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn every_preset_is_complete_and_unique() {
        let mut ids = HashSet::new();
        for preset in presets() {
            assert!(!preset.id.is_empty(), "{:?}", preset.id);
            assert!(!preset.label.is_empty(), "{:?}", preset.id);
            assert!(!preset.subtitle.is_empty(), "{:?}", preset.id);
            assert!(!preset.note.is_empty(), "{:?}", preset.id);
            assert!(!preset.badge.is_empty(), "{:?}", preset.id);
            assert!(
                matches!(preset.auth, "api_key" | "oauth"),
                "{:?}",
                preset.id
            );
            if preset.id != "custom" {
                assert!(!preset.secret_name.is_empty(), "{:?}", preset.id);
                assert!(!preset.default_id.is_empty(), "{:?}", preset.id);
            }
            assert!(
                ids.insert(preset.id),
                "duplicate preset id: {:?}",
                preset.id
            );
            assert!(!preset.kind.is_empty(), "{:?}", preset.id);
            assert!(
                preset
                    .kind
                    .bytes()
                    .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-'),
                "{:?}",
                preset.id
            );
        }
    }

    #[test]
    fn the_custom_preset_has_no_base_url() {
        let custom = custom_preset();
        assert_eq!(custom.id, "custom");
        assert!(custom.base_url.is_empty());
        for preset in presets() {
            if preset.id != "custom" {
                assert!(
                    preset.base_url.starts_with("https://")
                        || preset.base_url.starts_with("http://127.0.0.1"),
                    "{:?}",
                    preset.id
                );
            }
        }
    }

    #[test]
    fn presets_match_the_server_provider_kinds() {
        for preset in presets() {
            assert!(
                matches!(
                    preset.kind,
                    "openai"
                        | "mistral"
                        | "groq"
                        | "openrouter"
                        | "openai-compatible"
                        | "google-antigravity"
                        | "deepseek"
                        | "minimax"
                        | "minimax-cn"
                        | "xai"
                        | "moonshot"
                        | "cerebras"
                        | "together"
                        | "fireworks"
                        | "muse-code"
                ),
                "{:?}",
                preset.id
            );
        }
        assert_eq!(
            preset_for("openai").unwrap().base_url,
            "https://api.openai.com/v1"
        );
        assert_eq!(
            preset_for("antigravity").unwrap().kind,
            "google-antigravity"
        );
        assert_eq!(
            preset_for("deepseek").unwrap().base_url,
            "https://api.deepseek.com/v1"
        );
        assert_eq!(
            preset_for("minimax").unwrap().base_url,
            "https://api.minimax.io/v1"
        );
        assert_eq!(
            preset_for("mistral").unwrap().base_url,
            "https://api.mistral.ai/v1"
        );
        assert_eq!(
            preset_for("groq").unwrap().base_url,
            "https://api.groq.com/openai/v1"
        );
        assert_eq!(
            preset_for("openrouter").unwrap().base_url,
            "https://openrouter.ai/api/v1"
        );
        assert_eq!(
            preset_for("ollama").unwrap().base_url,
            "http://127.0.0.1:11434/v1"
        );
        assert!(preset_for("nope").is_none());
    }
}
