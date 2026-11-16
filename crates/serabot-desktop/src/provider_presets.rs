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
    /// Auth type: "api_key", "oauth" (sign-in only) or "none" (local server).
    pub auth: &'static str,
    /// Badge label: "OAuth", "API Key", "Local", "Custom".
    pub badge: &'static str,
    /// One short line for the owner.
    pub note: &'static str,
    /// The provider's own page for creating an API key. Empty when the
    /// provider has no key page (sign-in only, or a local server).
    pub key_url: &'static str,
    /// A real sign-in flow the provider documents for apps: pkce (browser
    /// authorization code), device (device code) or pkce_google (Google
    /// Cloud Code). Empty for providers that only take an API key.
    pub sign_in: &'static str,
}

/// Filter tabs for the Provider Directory.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum DirectoryFilter {
    #[default]
    All,
    ApiKey,
    OAuth,
    Local,
    Connected,
}

impl DirectoryFilter {
    pub const ALL: [Self; 5] = [
        Self::All,
        Self::ApiKey,
        Self::OAuth,
        Self::Local,
        Self::Connected,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::All => "All",
            Self::ApiKey => "API Key",
            Self::OAuth => "OAuth",
            Self::Local => "Local",
            Self::Connected => "Connected",
        }
    }
}

/// Whether a configured provider id/kind matches `preset`.
pub fn provider_matches_preset(
    provider_id: &str,
    provider_kind: &str,
    preset: &ProviderPreset,
) -> bool {
    provider_id == preset.id || (provider_kind == preset.kind && preset.kind != "openai-compatible")
}

/// Filter `presets()` by search query and directory filter.
pub fn filter_presets(
    query: &str,
    filter: DirectoryFilter,
    is_connected: impl Fn(&ProviderPreset) -> bool,
) -> Vec<&'static ProviderPreset> {
    let needle = query.trim().to_lowercase();
    PRESETS
        .iter()
        .filter(|preset| match filter {
            DirectoryFilter::All => true,
            DirectoryFilter::ApiKey => preset.auth == "api_key",
            DirectoryFilter::OAuth => preset.auth == "oauth" || !preset.sign_in.is_empty(),
            DirectoryFilter::Local => preset.auth == "none",
            DirectoryFilter::Connected => is_connected(preset),
        })
        .filter(|preset| {
            needle.is_empty()
                || preset.label.to_lowercase().contains(&needle)
                || preset.subtitle.to_lowercase().contains(&needle)
                || preset.kind.to_lowercase().contains(&needle)
                || preset.id.to_lowercase().contains(&needle)
                || preset
                    .models
                    .iter()
                    .any(|m| m.to_lowercase().contains(&needle))
        })
        .collect()
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
        key_url: "",
        sign_in: "pkce_google",
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
        key_url: "https://platform.deepseek.com/api_keys",
        sign_in: "",
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
        key_url: "https://platform.minimax.io/user-center/basic-information/interface-key",
        sign_in: "",
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
        key_url: "https://platform.minimaxi.com/user-center/basic-information/interface-key",
        sign_in: "",
    },
    ProviderPreset {
        id: "mistral",
        label: "Mistral AI",
        subtitle: "Mistral Large, Magistral, Codestral, Ministral, Pixtral & open models",
        base_url: "https://api.mistral.ai/v1",
        kind: "mistral",
        default_id: "mistral",
        // Static fallback list of well-known stable model ids. The live
        // /models fetch (catalog_fetch) is authoritative; this list only
        // prefills the form when the catalog is not fetched.
        models: &[
            "mistral-large-latest",
            "mistral-large-2411",
            "magistral-medium-latest",
            "magistral-small-latest",
            "codestral-latest",
            "codestral-2501",
            "ministral-8b-latest",
            "ministral-3b-latest",
            "ministral-14b-latest",
            "devstral-small-latest",
            "devstral-medium-latest",
            "mistral-medium-latest",
            "mistral-small-latest",
            "pixtral-large-latest",
            "pixtral-12b-latest",
            "open-mistral-nemo",
            "open-mistral-7b",
            "open-mixtral-8x7b",
            "open-mixtral-8x22b",
            "mistral-embed",
            "mistral-moderation-latest",
        ],
        secret_name: "mistral_api_key",
        catalog_fetch: true,
        auth: "api_key",
        badge: "API Key",
        note: "Mistral Large, Magistral, Codestral, Ministral, Pixtral & open models",
        key_url: "https://console.mistral.ai/api-keys",
        sign_in: "",
    },
    ProviderPreset {
        id: "openai",
        label: "OpenAI",
        subtitle: "GPT-5, GPT-4.1, GPT-4o & o-series reasoning",
        base_url: "https://api.openai.com/v1",
        kind: "openai",
        default_id: "openai",
        // Static fallback list; the live /models fetch is authoritative.
        models: &[
            "gpt-5",
            "gpt-5-mini",
            "gpt-5-nano",
            "gpt-4o-mini",
            "gpt-4.1",
            "gpt-4.1-mini",
            "gpt-4o",
            "o3",
            "o4-mini",
        ],
        secret_name: "openai_api_key",
        catalog_fetch: true,
        auth: "api_key",
        badge: "API Key",
        note: "GPT-5, GPT-4.1, GPT-4o & o-series reasoning",
        key_url: "https://platform.openai.com/api-keys",
        sign_in: "",
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
        badge: "Sign in or API key",
        note: "Unified router for Claude, Gemini, Llama & DeepSeek",
        key_url: "https://openrouter.ai/settings/keys",
        sign_in: "pkce",
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
        key_url: "https://console.groq.com/keys",
        sign_in: "",
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
        key_url: "https://console.x.ai",
        sign_in: "",
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
        key_url: "https://platform.moonshot.ai/console/api-keys",
        sign_in: "",
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
        key_url: "https://cloud.cerebras.ai/platform",
        sign_in: "",
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
        key_url: "https://api.together.ai/settings/api-keys",
        sign_in: "",
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
        key_url: "https://app.fireworks.ai/settings/users/api-keys",
        sign_in: "",
    },
    ProviderPreset {
        id: "gemini",
        label: "Google Gemini",
        subtitle: "Gemini 2.5 Pro, Flash and newer, through Google's OpenAI-compatible endpoint",
        base_url: "https://generativelanguage.googleapis.com/v1beta/openai",
        kind: "gemini",
        default_id: "gemini",
        models: &["gemini-2.5-pro", "gemini-2.5-flash"],
        secret_name: "gemini_api_key",
        catalog_fetch: true,
        auth: "api_key",
        badge: "API Key",
        note: "Gemini API key from Google AI Studio",
        key_url: "https://aistudio.google.com/apikey",
        sign_in: "",
    },
    ProviderPreset {
        id: "perplexity",
        label: "Perplexity",
        subtitle: "Sonar models with live web search and citations",
        base_url: "https://api.perplexity.ai",
        kind: "perplexity",
        default_id: "perplexity",
        models: &["sonar", "sonar-pro", "sonar-reasoning-pro"],
        secret_name: "perplexity_api_key",
        catalog_fetch: true,
        auth: "api_key",
        badge: "API Key",
        note: "Sonar models with web search",
        key_url: "https://www.perplexity.ai/account/api/keys",
        sign_in: "",
    },
    ProviderPreset {
        id: "huggingface",
        label: "Hugging Face",
        subtitle: "Inference Providers router: open models from many hosts, one token",
        base_url: "https://router.huggingface.co/v1",
        kind: "huggingface",
        default_id: "huggingface",
        models: &[
            "meta-llama/Llama-3.3-70B-Instruct",
            "Qwen/Qwen2.5-72B-Instruct",
        ],
        secret_name: "huggingface_api_key",
        catalog_fetch: true,
        auth: "api_key",
        badge: "API Key",
        note: "Hugging Face access token with inference permission",
        key_url: "https://huggingface.co/settings/tokens",
        sign_in: "",
    },
    ProviderPreset {
        id: "nebius",
        label: "Nebius Token Factory",
        subtitle: "Open models on Nebius infrastructure",
        base_url: "https://api.tokenfactory.nebius.com/v1",
        kind: "nebius",
        default_id: "nebius",
        models: &["meta-llama/Llama-3.3-70B-Instruct"],
        secret_name: "nebius_api_key",
        catalog_fetch: true,
        auth: "api_key",
        badge: "API Key",
        note: "Open models on Nebius infrastructure",
        key_url: "https://tokenfactory.nebius.com",
        sign_in: "",
    },
    ProviderPreset {
        id: "sambanova",
        label: "SambaNova Cloud",
        subtitle: "Fast open-model inference on SambaNova chips",
        base_url: "https://api.sambanova.ai/v1",
        kind: "sambanova",
        default_id: "sambanova",
        models: &["Meta-Llama-3.3-70B-Instruct"],
        secret_name: "sambanova_api_key",
        catalog_fetch: true,
        auth: "api_key",
        badge: "API Key",
        note: "Fast open-model inference on SambaNova chips",
        key_url: "https://cloud.sambanova.ai/apis",
        sign_in: "",
    },
    ProviderPreset {
        id: "anthropic",
        label: "Anthropic (Claude)",
        subtitle: "Claude models on the native Messages API. Add several API keys; each member's keys are their own.",
        base_url: "https://api.anthropic.com/v1",
        kind: "anthropic",
        default_id: "anthropic",
        models: &["claude-sonnet-5-5", "claude-opus-5-5"],
        secret_name: "anthropic_api_key",
        catalog_fetch: true,
        auth: "api_key",
        badge: "API Key",
        note: "Anthropic API key. Claude plan sign-in is off by default and admin-only",
        key_url: "https://console.anthropic.com/settings/keys",
        sign_in: "",
    },
    ProviderPreset {
        id: "opencode-go",
        label: "OpenCode Go",
        subtitle: "OpenCode Go subscription: open coding models behind one key",
        base_url: "https://opencode.ai/zen/go/v1",
        kind: "opencode-go",
        default_id: "opencode-go",
        models: &["glm-5.3-flash", "glm-5.3", "kimi-k2.7-code"],
        secret_name: "opencode_go_api_key",
        catalog_fetch: true,
        auth: "api_key",
        badge: "API Key",
        note: "OpenCode Go key; every request carries a session id",
        key_url: "https://opencode.ai/auth",
        sign_in: "",
    },
    ProviderPreset {
        id: "opencode-zen",
        label: "OpenCode Zen",
        subtitle: "OpenCode Zen pay-as-you-go models behind one key",
        base_url: "https://opencode.ai/zen/v1",
        kind: "opencode-zen",
        default_id: "opencode-zen",
        models: &["claude-fable-5-1"],
        secret_name: "opencode_zen_api_key",
        catalog_fetch: true,
        auth: "api_key",
        badge: "API Key",
        note: "OpenCode Zen key (pay as you go)",
        key_url: "https://opencode.ai/auth",
        sign_in: "",
    },
    ProviderPreset {
        id: "nous",
        label: "Nous Portal (Hermes)",
        subtitle: "Nous Research's Hermes models, OpenAI-compatible, with an API key",
        base_url: "https://inference-api.nousresearch.com/v1",
        kind: "openai-compatible",
        default_id: "nous",
        models: &[],
        secret_name: "nous_api_key",
        catalog_fetch: true,
        auth: "api_key",
        badge: "API Key",
        note: "Base URL not verified against Nous docs: confirm it before approving egress",
        key_url: "https://portal.nousresearch.com",
        sign_in: "",
    },
    ProviderPreset {
        id: "muse-code",
        label: "Muse Code",
        subtitle: "Muse Spark OAuth device sign-in",
        base_url: "https://api.meta.ai/v1",
        kind: "muse-code",
        default_id: "muse",
        models: &["muse-spark-1.3"],
        secret_name: "muse_code_api_key",
        catalog_fetch: true,
        auth: "oauth",
        badge: "OAuth",
        note: "Muse Spark OAuth device sign-in",
        key_url: "",
        sign_in: "device",
    },
    ProviderPreset {
        id: "ollama",
        label: "Ollama (Local)",
        subtitle: "Your own Ollama server. No key needed. Change the address for another machine.",
        base_url: "http://127.0.0.1:11434/v1",
        kind: "openai-compatible",
        default_id: "ollama",
        models: &["llama3.3", "qwen2.5-coder:32b"],
        secret_name: "ollama_api_key",
        catalog_fetch: true,
        auth: "none",
        badge: "Local",
        note: "Local model server on 127.0.0.1:11434",
        key_url: "",
        sign_in: "",
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
        key_url: "",
        sign_in: "",
    },
];

/// Presets whose base URL or key page no research note confirms. The sheet
/// shows their note, and the owner checks the vendor's docs before approving
/// egress.
pub const UNVERIFIED_PRESETS: &[&str] = &["nous"];

/// Whether a preset's endpoint is unconfirmed (see [`UNVERIFIED_PRESETS`]).
pub fn is_unverified(id: &str) -> bool {
    UNVERIFIED_PRESETS.contains(&id)
}

/// The preset catalog, in picker order.
pub fn presets() -> &'static [ProviderPreset] {
    PRESETS
}

/// One preset by id.
pub fn preset_for(id: &str) -> Option<&'static ProviderPreset> {
    PRESETS.iter().find(|preset| preset.id == id)
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
                matches!(preset.auth, "api_key" | "oauth" | "none"),
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
        let custom = preset_for("custom").expect("custom preset");
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
                        | "anthropic"
                        | "opencode-go"
                        | "opencode-zen"
                        | "gemini"
                        | "perplexity"
                        | "huggingface"
                        | "nebius"
                        | "sambanova"
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

    #[test]
    fn the_v049_provider_presets_match_the_server() {
        let anthropic = preset_for("anthropic").unwrap();
        assert_eq!(anthropic.kind, "anthropic");
        assert_eq!(anthropic.base_url, "https://api.anthropic.com/v1");
        // API keys only: the plan sign-in is an admin flag, not a preset.
        assert_eq!((anthropic.auth, anthropic.sign_in), ("api_key", ""));
        let go = preset_for("opencode-go").unwrap();
        assert_eq!(go.kind, "opencode-go");
        assert_eq!(go.base_url, "https://opencode.ai/zen/go/v1");
        let zen = preset_for("opencode-zen").unwrap();
        assert_eq!(zen.kind, "opencode-zen");
        assert_eq!(zen.base_url, "https://opencode.ai/zen/v1");
        // Nous is a plain OpenAI-compatible provider; its URL is flagged.
        let nous = preset_for("nous").unwrap();
        assert_eq!(nous.kind, "openai-compatible");
        assert!(is_unverified("nous"));
        assert!(nous.note.contains("not verified"));
        for verified in ["anthropic", "opencode-go", "opencode-zen", "muse-code"] {
            assert!(!is_unverified(verified), "{verified}");
        }
        // Muse's catalog is pinned to the documented host.
        assert_eq!(
            preset_for("muse-code").unwrap().base_url,
            "https://api.meta.ai/v1"
        );
        // Matching a configured provider to a preset works by kind for the
        // native kinds and by id for the shared OpenAI-compatible kind.
        assert!(provider_matches_preset("mine", "anthropic", anthropic));
        assert!(provider_matches_preset("nous", "openai-compatible", nous));
        assert!(!provider_matches_preset("other", "openai-compatible", nous));
    }

    #[test]
    fn fallback_model_lists_are_unique_well_formed_and_cover_the_flagships() {
        for preset in presets() {
            let mut seen = HashSet::new();
            for model in preset.models {
                assert!(
                    crate::valid_model_id(model),
                    "bad model id {model:?} in {:?}",
                    preset.id
                );
                assert!(seen.insert(model), "duplicate {model:?} in {:?}", preset.id);
            }
        }
        let mistral = preset_for("mistral").unwrap();
        assert!(mistral.models.len() >= 20, "the fallback list grew");
        for expected in [
            "mistral-large-latest",
            "ministral-14b-latest",
            "mistral-embed",
            "mistral-moderation-latest",
        ] {
            assert!(mistral.models.contains(&expected), "{expected}");
        }
        let openai = preset_for("openai").unwrap();
        for expected in ["gpt-5", "gpt-5-mini", "gpt-5-nano", "gpt-4o-mini"] {
            assert!(openai.models.contains(&expected), "{expected}");
        }
    }

    #[test]
    fn auth_is_honest_for_every_preset() {
        for preset in presets() {
            // Only these providers document a sign-in flow for apps.
            let expected_sign_in = match preset.id {
                "openrouter" => "pkce",
                "antigravity" => "pkce_google",
                "muse-code" => "device",
                _ => "",
            };
            assert_eq!(preset.sign_in, expected_sign_in, "{:?}", preset.id);
            match preset.auth {
                // A key provider names the page where the key is created.
                "api_key" if preset.id != "custom" => {
                    assert!(preset.key_url.starts_with("https://"), "{:?}", preset.id)
                }
                // A sign-in-only provider has no key to get.
                "oauth" => {
                    assert!(!preset.sign_in.is_empty(), "{:?}", preset.id);
                    assert!(preset.key_url.is_empty(), "{:?}", preset.id);
                }
                "none" => assert!(preset.key_url.is_empty(), "{:?}", preset.id),
                _ => {}
            }
        }
        // Key-only providers never claim a sign-in.
        for id in ["openai", "mistral", "gemini", "perplexity", "huggingface"] {
            let preset = preset_for(id).unwrap();
            assert_eq!((preset.auth, preset.sign_in), ("api_key", ""), "{id}");
        }
        assert_eq!(preset_for("ollama").unwrap().auth, "none");
    }

    #[test]
    fn directory_filters_and_search_cover_presets() {
        let all = filter_presets("", DirectoryFilter::All, |_| false);
        assert_eq!(all.len(), presets().len());

        let local = filter_presets("", DirectoryFilter::Local, |_| false);
        assert!(local.iter().all(|p| p.auth == "none"));
        assert!(local.iter().any(|p| p.id == "ollama"));

        let oauth = filter_presets("", DirectoryFilter::OAuth, |_| false);
        assert!(oauth.iter().any(|p| p.id == "antigravity"));
        assert!(oauth.iter().any(|p| p.id == "openrouter"));

        let connected = filter_presets("", DirectoryFilter::Connected, |p| p.id == "openai");
        assert_eq!(connected.len(), 1);
        assert_eq!(connected[0].id, "openai");

        let by_model = filter_presets("deepseek-reasoner", DirectoryFilter::All, |_| false);
        assert!(by_model.iter().any(|p| p.id == "deepseek"));
        assert!(provider_matches_preset(
            "openai",
            "openai",
            preset_for("openai").unwrap()
        ));
    }
}
