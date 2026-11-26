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
    /// Auth type: "api_key", "oauth" (sign-in only), "oauth_or_key" or "none" (local server).
    pub auth: &'static str,
    /// Badge label: "OAuth", "OAuth / API Key", "API Key", "Local", "Custom".
    pub badge: &'static str,
    /// One short line for the owner.
    pub note: &'static str,
    /// The provider's own page for creating an API key. Empty when the
    /// provider has no key page (sign-in only, or a local server).
    pub key_url: &'static str,
    /// A real sign-in flow the provider documents for apps: pkce (browser
    /// authorization code), device (device code), browser (browser sign-in
    /// polled via device flow) or pkce_google (Google Cloud Code). Empty for
    /// providers that only take an API key.
    pub sign_in: &'static str,
    /// A plain-words risk shown beside a sign-in that rides on a vendor's
    /// consumer plan rather than an API contract. Empty when there is none.
    pub risk: &'static str,
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
            DirectoryFilter::ApiKey => matches!(preset.auth, "api_key" | "oauth_or_key"),
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

/// Shown beside the Antigravity sign-in: it uses Google's own desktop client,
/// so the account can be suspended.
pub const PLAN_RISK_GOOGLE: &str = "This signs in through Google's Antigravity client, not an API contract. Google may suspend an account used this way. It is your own account and is never shared.";

/// Shown beside a Claude plan sign-in when the server does not send its own
/// text.
pub const PLAN_RISK_CLAUDE: &str = "This signs in through Claude's own client, not an API contract. Anthropic may restrict or suspend an account used this way. It is your own account and is never shared. API keys are the supported path.";

/// Shown beside a Codex (ChatGPT plan) sign-in when the server does not send
/// its own text.
pub const PLAN_RISK_CODEX: &str = "This signs in through ChatGPT's own Codex client, not an API contract. OpenAI may restrict or suspend an account used this way. It is your own account and is never shared. API keys on the OpenAI provider are the supported path.";

/// Shown beside a GitHub Copilot sign-in when the server does not send its own
/// text.
pub const PLAN_RISK_COPILOT: &str = "This signs in through GitHub Copilot's own editor client, not an API contract. GitHub may restrict or suspend an account used this way. It is your own account and is never shared. A GitHub account without a Copilot plan cannot use it.";

const PRESETS: &[ProviderPreset] = &[
    ProviderPreset {
        id: "antigravity",
        label: "Google Antigravity",
        subtitle: "Sign in with Google (browser) — Gemini & Claude models",
        base_url: "https://daily-cloudcode-pa.googleapis.com",
        kind: "google-antigravity",
        default_id: "antigravity",
        models: &[
            "gemini-4-argon-high",
            "gemini-4-argon-medium",
            "gemini-4-argon-low",
            "gemini-3.8-flash-tiered",
            "gemini-3.1-pro-low",
            "claude-opus-5-5-thinking",
            "claude-opus-5-5",
            "claude-sonnet-5-5-thinking",
            "claude-sonnet-5-5",
        ],
        secret_name: "antigravity_access_token",
        catalog_fetch: true,
        auth: "oauth",
        badge: "OAuth",
        note: "Sign in with Google (browser) — Gemini & Claude models",
        key_url: "",
        sign_in: "pkce_google",
        risk: PLAN_RISK_GOOGLE,
    },
    ProviderPreset {
        id: "deepseek",
        label: "DeepSeek",
        subtitle: "DeepSeek V4 Flash & V4 Pro API",
        base_url: "https://api.deepseek.com/v1",
        kind: "deepseek",
        default_id: "deepseek",
        models: &["deepseek-flash", "deepseek-v4-pro"],
        secret_name: "deepseek_api_key",
        catalog_fetch: true,
        auth: "api_key",
        badge: "API Key",
        note: "DeepSeek V4 Flash & V4 Pro API",
        key_url: "https://platform.deepseek.com/api_keys",
        sign_in: "",
        risk: "",
    },
    ProviderPreset {
        id: "minimax",
        label: "MiniMax",
        subtitle: "MiniMax M3.1 Flash, M3 & M-Plan API",
        base_url: "https://api.minimax.io/v1",
        kind: "minimax",
        default_id: "minimax",
        models: &["MiniMax-M3", "MiniMax-M3.1-Flash"],
        secret_name: "minimax_m_plan_key",
        catalog_fetch: true,
        auth: "api_key",
        badge: "API Key",
        note: "MiniMax M3.1 Flash, M3 & M-Plan API",
        key_url: "https://platform.minimax.io/user-center/basic-information/interface-key",
        sign_in: "",
        risk: "",
    },
    ProviderPreset {
        id: "minimax-cn",
        label: "MiniMax (China)",
        subtitle: "MiniMax China domestic endpoint",
        base_url: "https://api.minimaxi.com/v1",
        kind: "minimax-cn",
        default_id: "minimax-cn",
        models: &["MiniMax-M3", "MiniMax-M3.1-Flash"],
        secret_name: "minimax_cn_api_key",
        catalog_fetch: true,
        auth: "api_key",
        badge: "API Key",
        note: "MiniMax China domestic endpoint",
        key_url: "https://platform.minimaxi.com/user-center/basic-information/interface-key",
        sign_in: "",
        risk: "",
    },
    ProviderPreset {
        id: "mistral",
        label: "Mistral AI",
        subtitle: "Mistral Large 4 and Mistral Small",
        base_url: "https://api.mistral.ai/v1",
        kind: "mistral",
        default_id: "mistral",
        // Static fallback list of ids confirmed on Mistral's model pages
        // (2026-11). Ministral 3 ids are not confirmed, so they are left out.
        // The live /models fetch (catalog_fetch) is authoritative; this list
        // only prefills the form when the catalog is not fetched.
        models: &[
            "mistral-large-4",
            "mistral-small-2603",
            "mistral-small-latest",
        ],
        secret_name: "mistral_api_key",
        catalog_fetch: true,
        auth: "oauth_or_key",
        badge: "OAuth / API Key",
        note: "Mistral Large 4 and Mistral Small — sign in with Mistral or paste an API key",
        key_url: "https://console.mistral.ai/api-keys",
        sign_in: "browser",
        risk: "",
    },
    ProviderPreset {
        id: "openai",
        label: "OpenAI",
        subtitle: "GPT-6, GPT-5.6, GPT-5 and gpt-oss",
        base_url: "https://api.openai.com/v1",
        kind: "openai",
        default_id: "openai",
        // Static fallback list (OpenAI's current models page); the default,
        // gpt-6.1-sol, comes first. The live /models fetch is authoritative.
        models: &[
            "gpt-6.1-sol",
            "gpt-6-astra",
            "gpt-6.1-terra",
            "gpt-6-luna",
            "gpt-6-sol",
            "gpt-5.6-sol",
            "gpt-5.6-terra",
            "gpt-5.6-luna",
            "gpt-5.5",
            "gpt-5.4",
            "gpt-5.4-mini",
            "gpt-5.2",
            "gpt-oss-120b",
            "gpt-oss-20b",
        ],
        secret_name: "openai_api_key",
        catalog_fetch: true,
        auth: "api_key",
        badge: "API Key",
        note: "GPT-6, GPT-5.6, GPT-5 and gpt-oss",
        key_url: "https://platform.openai.com/api-keys",
        sign_in: "",
        risk: "",
    },
    ProviderPreset {
        id: "chatgpt-codex",
        label: "ChatGPT (Codex)",
        subtitle: "Use your ChatGPT plan through Codex. Sign in with the browser or a code; add several accounts. For an API key, use the OpenAI provider.",
        base_url: "https://chatgpt.com/backend-api/codex",
        kind: "openai-codex",
        default_id: "codex",
        // The ChatGPT Codex lineup, as the owner's HielCowork client lists it
        // (the endpoint has no public model list); the default comes first.
        models: &[
            "gpt-6.1-sol",
            "gpt-6.1-terra",
            "gpt-6-astra",
            "gpt-6-sol",
            "gpt-6-luna",
            "gpt-5.6-sol",
            "gpt-5.6-terra",
            "gpt-5.6-luna",
        ],
        secret_name: "codex_access_token",
        catalog_fetch: false,
        auth: "oauth",
        badge: "OAuth",
        note: "Sign in with ChatGPT (browser or code, several accounts)",
        key_url: "",
        sign_in: "pkce",
        risk: PLAN_RISK_CODEX,
    },
    ProviderPreset {
        id: "github-copilot",
        label: "GitHub Copilot",
        subtitle: "The models your Copilot plan includes, with no API key. Sign in with GitHub using a code; add several accounts.",
        base_url: "https://api.githubcopilot.com",
        kind: "github-copilot",
        default_id: "copilot",
        // HielCowork's static list is only "auto" and the real list comes from
        // the plan's own catalog. The server does not use the auto router, but
        // a provider needs an allowlist, so this seeds it with ids HielCowork
        // names; the live catalog (`catalog_fetch`) then limits it to what the
        // plan actually serves on chat completions.
        models: &[
            "claude-sonnet-5.5",
            "claude-opus-5.5",
            "claude-haiku-5.5",
            "gpt-6.1-sol",
            "gpt-6.1-terra",
        ],
        secret_name: "copilot_access_token",
        catalog_fetch: true,
        auth: "oauth",
        badge: "OAuth",
        note: "Sign in with GitHub (device code, several accounts)",
        key_url: "",
        sign_in: "device",
        risk: PLAN_RISK_COPILOT,
    },
    ProviderPreset {
        id: "openrouter",
        label: "OpenRouter",
        subtitle: "Unified router for Claude, Gemini, Llama & DeepSeek",
        base_url: "https://openrouter.ai/api/v1",
        kind: "openrouter",
        default_id: "openrouter",
        // Only the auto router is confirmed; the live /models fetch lists
        // the rest.
        models: &["openrouter/auto"],
        secret_name: "openrouter_api_key",
        catalog_fetch: true,
        auth: "api_key",
        badge: "Sign in or API key",
        note: "Unified router for Claude, Gemini, Llama & DeepSeek",
        key_url: "https://openrouter.ai/settings/keys",
        sign_in: "pkce",
        risk: "",
    },
    ProviderPreset {
        id: "groq",
        label: "Groq",
        subtitle: "Ultra-fast LPU inference for gpt-oss & Llama",
        base_url: "https://api.groq.com/openai/v1",
        kind: "groq",
        default_id: "groq",
        // Production models only; Groq's preview models come from the live
        // /models fetch.
        models: &[
            "openai/gpt-oss-120b",
            "openai/gpt-oss-20b",
            "llama-3.3-70b-versatile",
            "llama-3.1-8b-instant",
        ],
        secret_name: "groq_api_key",
        catalog_fetch: true,
        auth: "api_key",
        badge: "API Key",
        note: "Ultra-fast LPU inference for gpt-oss & Llama",
        key_url: "https://console.groq.com/keys",
        sign_in: "",
        risk: "",
    },
    ProviderPreset {
        id: "xai",
        label: "xAI (Grok)",
        subtitle: "Grok 4.x API",
        base_url: "https://api.x.ai/v1",
        kind: "xai",
        default_id: "xai",
        models: &[
            "grok-4.7",
            "grok-4.6",
            "grok-4.5",
            "grok-4.3",
            "grok-4.20-0309-reasoning",
            "grok-4.20-0309-non-reasoning",
            "grok-4.20-multi-agent-0309",
            "grok-build-0.1",
        ],
        secret_name: "xai_api_key",
        catalog_fetch: true,
        auth: "api_key",
        badge: "API Key",
        note: "Grok 4.x API",
        key_url: "https://console.x.ai",
        sign_in: "",
        risk: "",
    },
    ProviderPreset {
        id: "moonshot",
        label: "Moonshot (Kimi)",
        subtitle: "Kimi K3 and K3.1 Pro API",
        base_url: "https://api.moonshot.ai/v1",
        kind: "moonshot",
        default_id: "moonshot",
        models: &["kimi-k3.1-pro", "kimi-k3"],
        secret_name: "moonshot_api_key",
        catalog_fetch: true,
        auth: "api_key",
        badge: "API Key",
        note: "Kimi K3 and K3.1 Pro API",
        key_url: "https://platform.moonshot.ai/console/api-keys",
        sign_in: "",
        risk: "",
    },
    ProviderPreset {
        id: "cerebras",
        label: "Cerebras",
        subtitle: "Wafer-scale high-speed inference",
        base_url: "https://api.cerebras.ai/v1",
        kind: "cerebras",
        default_id: "cerebras",
        models: &["gpt-oss-120b", "qwen-3.8-27b"],
        secret_name: "cerebras_api_key",
        catalog_fetch: true,
        auth: "api_key",
        badge: "API Key",
        note: "Wafer-scale high-speed inference",
        key_url: "https://cloud.cerebras.ai/platform",
        sign_in: "",
        risk: "",
    },
    ProviderPreset {
        id: "together",
        label: "Together AI",
        subtitle: "Open-weights cloud inference",
        base_url: "https://api.together.xyz/v1",
        kind: "together",
        default_id: "together",
        models: &[
            "openai/gpt-oss-120b",
            "moonshotai/Kimi-K3.1-Pro",
            "moonshotai/Kimi-K3",
            "zai-org/GLM-5.3",
            "zai-org/GLM-5.3-Flash",
            "zai-org/GLM-5.2",
            "MiniMaxAI/MiniMax-M3.1-Flash",
            "Qwen/Qwen3.7-Max",
            "Qwen/Qwen3.7-Plus",
            "Qwen/Qwen3.8-Flash",
            "Qwen/Qwen3.6-Plus",
        ],
        secret_name: "together_api_key",
        catalog_fetch: true,
        auth: "api_key",
        badge: "API Key",
        note: "Open-weights cloud inference",
        key_url: "https://api.together.ai/settings/api-keys",
        sign_in: "",
        risk: "",
    },
    ProviderPreset {
        id: "fireworks",
        label: "Fireworks AI",
        subtitle: "Fast production open-model inference",
        base_url: "https://api.fireworks.ai/inference/v1",
        kind: "fireworks",
        default_id: "fireworks",
        // No current serverless list is confirmed; "Fetch models" fills it.
        models: &[],
        secret_name: "fireworks_api_key",
        catalog_fetch: true,
        auth: "api_key",
        badge: "API Key",
        note: "Fast production open-model inference",
        key_url: "https://app.fireworks.ai/settings/users/api-keys",
        sign_in: "",
        risk: "",
    },
    ProviderPreset {
        id: "gemini",
        label: "Google Gemini",
        subtitle: "Gemini 3.x Flash and newer, through Google's OpenAI-compatible endpoint",
        base_url: "https://generativelanguage.googleapis.com/v1beta/openai",
        kind: "gemini",
        default_id: "gemini",
        models: &[
            "gemini-3.8-flash",
            "gemini-4-argon",
            "gemini-4-carbon",
            "gemini-4-hydrogen",
            "gemini-3.6-flash",
            "gemini-3.5-flash-lite",
            "gemini-3.1-flash-lite",
            "gemini-3.1-pro-preview",
            "gemini-3-flash-preview",
        ],
        secret_name: "gemini_api_key",
        catalog_fetch: true,
        auth: "api_key",
        badge: "API Key",
        note: "Gemini API key from Google AI Studio",
        key_url: "https://aistudio.google.com/apikey",
        sign_in: "",
        risk: "",
    },
    ProviderPreset {
        id: "perplexity",
        label: "Perplexity",
        subtitle: "Sonar models with live web search and citations",
        base_url: "https://api.perplexity.ai",
        kind: "perplexity",
        default_id: "perplexity",
        // Only sonar is confirmed; other Sonar ids come from the live fetch.
        models: &["sonar"],
        secret_name: "perplexity_api_key",
        catalog_fetch: true,
        auth: "api_key",
        badge: "API Key",
        note: "Sonar models with web search",
        key_url: "https://www.perplexity.ai/account/api/keys",
        sign_in: "",
        risk: "",
    },
    ProviderPreset {
        id: "huggingface",
        label: "Hugging Face",
        subtitle: "Inference Providers router: open models from many hosts, one token",
        base_url: "https://router.huggingface.co/v1",
        kind: "huggingface",
        default_id: "huggingface",
        // No current list is confirmed; "Fetch models" fills it.
        models: &[],
        secret_name: "huggingface_api_key",
        catalog_fetch: true,
        auth: "api_key",
        badge: "API Key",
        note: "Hugging Face access token with inference permission",
        key_url: "https://huggingface.co/settings/tokens",
        sign_in: "",
        risk: "",
    },
    ProviderPreset {
        id: "nebius",
        label: "Nebius Token Factory",
        subtitle: "Open models on Nebius infrastructure",
        base_url: "https://api.tokenfactory.nebius.com/v1",
        kind: "nebius",
        default_id: "nebius",
        // No current list is confirmed; "Fetch models" fills it.
        models: &[],
        secret_name: "nebius_api_key",
        catalog_fetch: true,
        auth: "api_key",
        badge: "API Key",
        note: "Open models on Nebius infrastructure",
        key_url: "https://tokenfactory.nebius.com",
        sign_in: "",
        risk: "",
    },
    ProviderPreset {
        id: "sambanova",
        label: "SambaNova Cloud",
        subtitle: "Fast open-model inference on SambaNova chips",
        base_url: "https://api.sambanova.ai/v1",
        kind: "sambanova",
        default_id: "sambanova",
        // No current list is confirmed; "Fetch models" fills it.
        models: &[],
        secret_name: "sambanova_api_key",
        catalog_fetch: true,
        auth: "api_key",
        badge: "API Key",
        note: "Fast open-model inference on SambaNova chips",
        key_url: "https://cloud.sambanova.ai/apis",
        sign_in: "",
        risk: "",
    },
    ProviderPreset {
        id: "inception",
        label: "Inception Labs",
        subtitle: "Mercury 3 diffusion LLMs — ultra-fast OpenAI-compatible API (API winds down Dec 28, 2026)",
        base_url: "https://api.inceptionlabs.ai/v1",
        kind: "inception",
        default_id: "inception",
        models: &[
            "mercury-3",
            "mercury-2.5",
            "mercury-2",
            "mercury-edit-2",
            "mercury-voice",
        ],
        secret_name: "inception_api_key",
        catalog_fetch: true,
        auth: "api_key",
        badge: "API Key",
        note: "Mercury 3 (1M context) · API available through Dec 28, 2026",
        key_url: "https://platform.inceptionlabs.ai/",
        sign_in: "",
        risk: "",
    },
    ProviderPreset {
        id: "anthropic",
        label: "Anthropic (Claude)",
        subtitle: "Claude models on the native Messages API. Sign in with Claude or add several API keys; each account is yours alone.",
        base_url: "https://api.anthropic.com/v1",
        kind: "anthropic",
        default_id: "anthropic",
        // Valid Anthropic model IDs first so the fallback list works before
        // a live /models fetch; newer aliases follow.
        models: &[
            "claude-sonnet-4-5",
            "claude-opus-4-5",
            "claude-haiku-4-5",
            "claude-sonnet-4-6",
            "claude-opus-4-6",
            "claude-opus-4-7",
            "claude-opus-4-8",
            "claude-opus-5-5",
            "claude-fable-5-2",
            "claude-sonnet-5-5",
            "claude-haiku-5-5",
            "claude-fable-5-1",
            "claude-fable-5",
            "claude-opus-5",
            "claude-sonnet-5",
        ],
        secret_name: "anthropic_api_key",
        catalog_fetch: true,
        auth: "api_key",
        badge: "OAuth / API Key",
        note: "Sign in with Claude (browser, several accounts) or add API keys",
        key_url: "https://console.anthropic.com/settings/keys",
        sign_in: "pkce",
        risk: PLAN_RISK_CLAUDE,
    },
    ProviderPreset {
        id: "opencode-go",
        label: "OpenCode Go",
        subtitle: "OpenCode Go subscription: open coding models behind one key",
        base_url: "https://opencode.ai/zen/go/v1",
        kind: "opencode-go",
        default_id: "opencode-go",
        models: &[
            "glm-5.3-flash",
            "glm-5.3",
            "kimi-k2.7-code",
            "kimi-k3",
            "deepseek-v4.1-flash",
            "deepseek-v4-pro",
            "qwen3.8-max",
            "qwen3.8-flash",
            "mimo-v2.6-flash",
            "mimo-v2.6-pro",
            "minimax-m3",
            "grok-4.7",
            "muse-spark-1.3-contributor",
            "space-bunny-free",
        ],
        secret_name: "opencode_go_api_key",
        catalog_fetch: true,
        auth: "api_key",
        badge: "API Key",
        note: "OpenCode Go key; every request carries a session id",
        key_url: "https://opencode.ai/auth",
        sign_in: "",
        risk: "",
    },
    ProviderPreset {
        id: "opencode-zen",
        label: "OpenCode Zen",
        subtitle: "OpenCode Zen pay-as-you-go models behind one key",
        base_url: "https://opencode.ai/zen/v1",
        kind: "opencode-zen",
        default_id: "opencode-zen",
        models: &[
            "claude-fable-5-1",
            "claude-opus-5",
            "claude-sonnet-5",
            "claude-haiku-4-5",
            "gemini-3.8-flash",
            "gpt-6-astra",
            "gpt-5.6-luna",
            "gpt-5.5",
            "grok-4.6",
            "muse-spark-1.3",
            "kimi-k3",
            "glm-5.3",
            "deepseek-v4-flash-free",
            "mimo-v2.6-flash-free",
            "mimo-v2.5-free",
            "ling-3.0-flash-fin-free",
            "nemotron-3-ultra-free",
            "nemotron-3.5-lightning-free",
            "muse-spark-1.3-contributor-free",
            "union-alpha",
            "jev-1.13-free",
        ],
        secret_name: "opencode_zen_api_key",
        catalog_fetch: true,
        auth: "api_key",
        badge: "API Key",
        note: "OpenCode Zen key (pay as you go)",
        key_url: "https://opencode.ai/auth",
        sign_in: "",
        risk: "",
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
        risk: "",
    },
    ProviderPreset {
        id: "muse-code",
        label: "Muse Code",
        subtitle: "Muse Spark OAuth device sign-in",
        base_url: "https://api.meta.ai/v1",
        kind: "muse-code",
        default_id: "muse",
        models: &[
            "muse-spark-1.3",
            "muse-spark-1.3-contributor",
            "muse-spark-1.2",
            "muse-spark-1.2-contributor",
            "muse-spark-1.1",
        ],
        secret_name: "muse_code_api_key",
        catalog_fetch: true,
        auth: "oauth",
        badge: "OAuth",
        note: "Muse Spark OAuth device sign-in",
        key_url: "",
        sign_in: "device",
        risk: "",
    },
    ProviderPreset {
        id: "claude-agent-sdk",
        label: "Claude Agent SDK (Gateway)",
        subtitle: "Local loopback Claude Agent SDK gateway (127.0.0.1:8791)",
        base_url: "http://127.0.0.1:8791",
        kind: "claude-agent-sdk",
        default_id: "claude-agent-sdk",
        models: &["claude-sonnet-4-5", "claude-opus-4-5", "claude-haiku-4-5"],
        secret_name: "claude_gateway_token",
        catalog_fetch: true,
        auth: "api_key",
        badge: "Gateway",
        note: "Local loopback Claude Agent SDK gateway — requires a local gateway token",
        key_url: "",
        sign_in: "",
        risk: "",
    },
    ProviderPreset {
        id: "ollama",
        label: "Ollama (Local)",
        subtitle: "Your own Ollama server. No key needed. Change the address for another machine.",
        base_url: "http://127.0.0.1:11434/v1",
        kind: "openai-compatible",
        default_id: "ollama",
        models: &[
            "qwen3.8",
            "qwen3.5",
            "qwen3-coder",
            "gemma4",
            "llama3.2",
            "deepseek-r1",
            "gpt-oss",
            "phi4",
        ],
        secret_name: "ollama_api_key",
        catalog_fetch: true,
        auth: "none",
        badge: "Local",
        note: "Local model server on 127.0.0.1:11434",
        key_url: "",
        sign_in: "",
        risk: "",
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
        risk: "",
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
    fn together_pins_no_dated_snapshot_model_id() {
        // Dated snapshot ids go stale and cannot be confirmed offline; the
        // live "Fetch models" catalog supplies those instead.
        for preset in presets().iter().filter(|p| p.id == "together") {
            for model in preset.models {
                let tail = model.rsplit('-').next().unwrap_or("");
                // MMDD stamps only; vendor YYMM ids such as mistral-small-2603 are real.
                let dated = tail.len() == 4
                    && tail.chars().all(|c| c.is_ascii_digit())
                    && matches!(tail[..2].parse::<u8>(), Ok(1..=12))
                    && matches!(tail[2..].parse::<u8>(), Ok(1..=31));
                assert!(!dated, "{}: {model}", preset.id);
            }
        }
    }

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
                matches!(preset.auth, "api_key" | "oauth" | "oauth_or_key" | "none"),
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
                        | "openai-codex"
                        | "github-copilot"
                        | "opencode-go"
                        | "opencode-zen"
                        | "gemini"
                        | "perplexity"
                        | "huggingface"
                        | "nebius"
                        | "sambanova"
                        | "inception"
                        | "claude-agent-sdk"
                ),
                "{:?}",
                preset.id
            );
        }
        assert_eq!(
            preset_for("inception").unwrap().base_url,
            "https://api.inceptionlabs.ai/v1"
        );
        assert_eq!(preset_for("inception").unwrap().kind, "inception");
        assert_eq!(
            preset_for("inception").unwrap().models,
            [
                "mercury-3",
                "mercury-2.5",
                "mercury-2",
                "mercury-edit-2",
                "mercury-voice",
            ]
        );
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
        assert_eq!(anthropic.badge, "OAuth / API Key");
        // A key or a Claude browser sign-in (PKCE); the card says both.
        assert_eq!((anthropic.auth, anthropic.sign_in), ("api_key", "pkce"));
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

    /// The Codex card follows the owner's HielCowork client: the same
    /// endpoint, the same models in the same order, sign-in only (an API key
    /// is not valid on the ChatGPT backend; that is the OpenAI card).
    #[test]
    fn the_codex_preset_matches_hielcowork() {
        let codex = preset_for("chatgpt-codex").unwrap();
        assert_eq!(codex.kind, "openai-codex");
        assert_eq!(codex.base_url, "https://chatgpt.com/backend-api/codex");
        assert_eq!((codex.auth, codex.sign_in), ("oauth", "pkce"));
        assert!(codex.key_url.is_empty() && !codex.catalog_fetch);
        assert_eq!(
            codex.models,
            [
                "gpt-6.1-sol",
                "gpt-6.1-terra",
                "gpt-6-astra",
                "gpt-6-sol",
                "gpt-6-luna",
                "gpt-5.6-sol",
                "gpt-5.6-terra",
                "gpt-5.6-luna",
            ]
        );
        assert!(codex.subtitle.contains("OpenAI provider"));
        assert!(!is_unverified("chatgpt-codex"));
        assert!(provider_matches_preset("mine", "openai-codex", codex));
        // The card is found by its name, its model or its kind.
        for query in ["chatgpt", "gpt-6.1-terra", "codex"] {
            let found = filter_presets(query, DirectoryFilter::OAuth, |_| false);
            assert!(found.iter().any(|p| p.id == "chatgpt-codex"), "{query}");
        }
        // The API-key OpenAI card stays a plain key provider.
        assert_eq!(preset_for("openai").unwrap().risk, "");
    }

    /// The Copilot card follows the owner's HielCowork client: the same
    /// `api.githubcopilot.com` base, GitHub's device sign-in, no key, and a
    /// model list that is read from the plan (HielCowork's static list is only
    /// "auto").
    #[test]
    fn the_copilot_preset_matches_hielcowork() {
        let copilot = preset_for("github-copilot").unwrap();
        assert_eq!(copilot.kind, "github-copilot");
        assert_eq!(copilot.base_url, "https://api.githubcopilot.com");
        assert_eq!((copilot.auth, copilot.sign_in), ("oauth", "device"));
        assert!(copilot.key_url.is_empty() && copilot.catalog_fetch);
        assert_eq!(copilot.models.first(), Some(&"claude-sonnet-5.5"));
        assert!(!copilot.models.contains(&"auto"));
        assert!(!is_unverified("github-copilot"));
        assert!(provider_matches_preset("mine", "github-copilot", copilot));
        for query in ["copilot", "github"] {
            let found = filter_presets(query, DirectoryFilter::OAuth, |_| false);
            assert!(found.iter().any(|p| p.id == "github-copilot"), "{query}");
        }
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
        for expected in ["mistral-large-4", "mistral-small-2603"] {
            assert!(mistral.models.contains(&expected), "{expected}");
        }
        let openai = preset_for("openai").unwrap();
        assert_eq!(openai.models.first(), Some(&"gpt-6.1-sol"));
        for expected in ["gpt-6-astra", "gpt-5.6-luna", "gpt-5.5", "gpt-oss-120b"] {
            assert!(openai.models.contains(&expected), "{expected}");
        }
        let anthropic = preset_for("anthropic").unwrap();
        for expected in ["claude-fable-5-2", "claude-opus-5-5", "claude-haiku-4-5"] {
            assert!(anthropic.models.contains(&expected), "{expected}");
        }
        let gemini = preset_for("gemini").unwrap();
        assert_eq!(gemini.models.first(), Some(&"gemini-3.8-flash"));
        assert!(gemini.models.contains(&"gemini-4-argon"));
        assert_eq!(preset_for("moonshot").unwrap().models[0], "kimi-k3.1-pro");
        assert_eq!(preset_for("cerebras").unwrap().models.len(), 2);
    }

    /// The lists follow the owner's HielCowork reference (dated 2026-11-02).
    /// These are the ids that reference no longer offers; none may come back.
    #[test]
    fn no_preset_offers_a_retired_model() {
        const RETIRED: &[&str] = &[
            "gemini-2.5-pro",
            "gemini-2.5-flash",
            "claude-opus-4",
            "gpt-5",
            "gpt-5-mini",
            "gpt-5-nano",
            "gpt-4o",
            "gpt-4o-mini",
            "o3",
            "o4-mini",
            "deepseek-chat",
            "deepseek-reasoner",
            "MiniMax-Text-01",
            "qwen-qwq-32b",
            "mistral-large-2411",
        ];
        for preset in presets() {
            for model in preset.models {
                assert!(
                    !RETIRED.contains(model),
                    "{:?} offers retired model {model:?}",
                    preset.id
                );
            }
        }
        let antigravity = preset_for("antigravity").unwrap();
        for expected in [
            "gemini-4-argon-high",
            "gemini-3.8-flash-tiered",
            "claude-opus-5-5-thinking",
            "claude-sonnet-5-5",
        ] {
            assert!(antigravity.models.contains(&expected), "{expected}");
        }
        let muse = preset_for("muse-code").unwrap();
        assert_eq!(muse.models.first(), Some(&"muse-spark-1.3"));
        assert!(muse.models.contains(&"muse-spark-1.1"));
    }

    #[test]
    fn auth_is_honest_for_every_preset() {
        for preset in presets() {
            // Only these providers document a sign-in flow for apps.
            let expected_sign_in = match preset.id {
                "openrouter" => "pkce",
                "antigravity" => "pkce_google",
                "muse-code" => "device",
                // Claude: a key or a browser sign-in (PKCE) on the same card.
                "anthropic" => "pkce",
                // Codex: a ChatGPT browser sign-in (the server adds the device code).
                "chatgpt-codex" => "pkce",
                // Copilot: GitHub's device code (it has no browser flow).
                "github-copilot" => "device",
                // Mistral: browser sign-in or an API key.
                "mistral" => "browser",
                _ => "",
            };
            assert_eq!(preset.sign_in, expected_sign_in, "{:?}", preset.id);
            match preset.auth {
                // A key provider names the page where the key is created.
                "api_key" | "oauth_or_key"
                    if !matches!(preset.id, "custom" | "claude-agent-sdk") =>
                {
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
        for id in ["openai", "gemini", "perplexity", "huggingface"] {
            let preset = preset_for(id).unwrap();
            assert_eq!((preset.auth, preset.sign_in), ("api_key", ""), "{id}");
        }
        let mistral = preset_for("mistral").unwrap();
        assert_eq!((mistral.auth, mistral.sign_in), ("oauth_or_key", "browser"));
        assert_eq!(preset_for("ollama").unwrap().auth, "none");
    }

    #[test]
    fn only_plan_sign_ins_carry_a_risk_line() {
        let risky: Vec<&str> = presets()
            .iter()
            .filter(|preset| !preset.risk.is_empty())
            .map(|preset| preset.id)
            .collect();
        assert_eq!(
            risky,
            [
                "antigravity",
                "chatgpt-codex",
                "github-copilot",
                "anthropic"
            ]
        );
        assert!(PLAN_RISK_COPILOT.contains("suspend"));
        assert!(PLAN_RISK_GOOGLE.contains("suspend"));
        assert!(PLAN_RISK_CODEX.contains("API keys on the OpenAI provider"));
        assert!(PLAN_RISK_CLAUDE.contains("API keys are the supported path"));
        // A risk never hides on a plain key provider.
        for id in ["openai", "mistral", "opencode-go", "nous", "muse-code"] {
            assert!(preset_for(id).unwrap().risk.is_empty(), "{id}");
        }
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

        let by_model = filter_presets("deepseek-v4-pro", DirectoryFilter::All, |_| false);
        assert!(by_model.iter().any(|p| p.id == "deepseek"));
        assert!(provider_matches_preset(
            "openai",
            "openai",
            preset_for("openai").unwrap()
        ));
    }
}
