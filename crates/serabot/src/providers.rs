//! Native provider registry.
//!
//! OpenAI-compatible providers chat on `/chat/completions`. `google-antigravity`
//! chats on the Cloud Code envelope instead. Provider *endpoints and model
//! catalogs* are admin-defined configuration; *credentials* are per user
//! (phase 4): the operator's existing age-store values are imported into the
//! admin's envelope-encrypted rows once, and every client is built from the
//! acting user's own credential (or an active admin grant). The age store
//! keeps only server-level values such as the OAuth app client id/secret.
//! Live catalogs are fetched only after egress consent and are intersected
//! with the owner allowlist. Mistral, DeepSeek, MiniMax (including M Plan on
//! the same OpenAI-compatible host), MiniMax China, Muse, and Antigravity pin
//! catalog fetches to their documented hosts.

use std::collections::BTreeSet;
use std::fmt;
use std::future::Future;
use std::sync::Arc;
use std::time::{Duration, Instant};

use secrecy::SecretString;
use serde_json::Value;

use crate::accounts::{AccountRefresher, AccountStatus, Accounts, DEFAULT_ACCOUNT, RefreshError};
use crate::config::{Config, ProviderConfig};
use crate::error::{Result, SeraError};
use crate::llm::{AccountRefreshTarget, LlmClient, validate_base_url};
use crate::model_router::ProviderCredentials;
use crate::secrets::SharedSecrets;
use crate::store::Store;
use crate::user_secrets::{ProviderCredential, UserCredentialWriter, UserSecrets};
use crate::wire::Wire;

pub const MUSE_CLIENT_ID: &str = "1031625952748946";
pub const MUSE_API_VERSION: &str = "1.0.0";
pub use crate::cloud_code::ANTIGRAVITY_CLIENT_ID;

const MUSE_AUTH_HOST: &str = "auth.meta.com";
const MUSE_API_HOST: &str = "api.meta.ai";
const MUSE_MINT_PATH: &str = "/muse-code/key";
const GOOGLE_TOKEN_HOST: &str = "oauth2.googleapis.com";
const GOOGLE_TOKEN_PATH: &str = "/token";
pub(crate) const USER_AGENT: &str = "SeraBot/0.1";

/// Documented model-list endpoints. Chat may use a `/v1` base; DeepSeek's list
/// is on the host root (`GET /models`), not `GET /v1/models`.
const OFFICIAL_CATALOGS: &[OfficialCatalog] = &[
    OfficialCatalog {
        kind: "mistral",
        host: "api.mistral.ai",
        path: "/v1/models",
        strict: true,
    },
    OfficialCatalog {
        kind: "deepseek",
        host: "api.deepseek.com",
        path: "/models",
        strict: true,
    },
    OfficialCatalog {
        kind: "minimax",
        host: "api.minimax.io",
        path: "/v1/models",
        strict: true,
    },
    OfficialCatalog {
        kind: "minimax-cn",
        host: "api.minimaxi.com",
        path: "/v1/models",
        strict: true,
    },
    OfficialCatalog {
        kind: "muse-code",
        host: "api.meta.ai",
        path: "/v1/models",
        strict: true,
    },
    // Anthropic's list is `{data, has_more, last_id}` with no `object` field,
    // and is read page by page with `x-api-key`.
    OfficialCatalog {
        kind: "anthropic",
        host: "api.anthropic.com",
        path: "/v1/models",
        strict: false,
    },
    OfficialCatalog {
        kind: "opencode-go",
        host: "opencode.ai",
        path: "/zen/go/v1/models",
        strict: false,
    },
    OfficialCatalog {
        kind: "opencode-zen",
        host: "opencode.ai",
        path: "/zen/v1/models",
        strict: false,
    },
    // Perplexity chats on the host root but lists models under /v1; its list
    // shape is not pinned, so the looser parser applies.
    OfficialCatalog {
        kind: "perplexity",
        host: "api.perplexity.ai",
        path: "/v1/models",
        strict: false,
    },
];

struct OfficialCatalog {
    kind: &'static str,
    host: &'static str,
    path: &'static str,
    /// Whether the list shape is pinned (`{object:"list", data:[...]}`).
    strict: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedCatalog {
    pub url: String,
    pub strict: bool,
}

#[derive(Debug, Clone, Copy)]
pub struct BuiltinProvider {
    pub kind: &'static str,
    pub base_url: Option<&'static str>,
    pub auth: &'static str,
    pub summary: &'static str,
}

const BUILTINS: &[BuiltinProvider] = &[
    BuiltinProvider {
        kind: "mistral",
        base_url: Some("https://api.mistral.ai/v1"),
        auth: "api_key",
        summary: "Mistral chat; catalog GET https://api.mistral.ai/v1/models",
    },
    BuiltinProvider {
        kind: "deepseek",
        base_url: Some("https://api.deepseek.com/v1"),
        auth: "api_key",
        summary: "DeepSeek chat; catalog GET https://api.deepseek.com/models",
    },
    BuiltinProvider {
        kind: "minimax",
        base_url: Some("https://api.minimax.io/v1"),
        auth: "api_key",
        summary: "MiniMax global and M Plan; catalog GET https://api.minimax.io/v1/models",
    },
    BuiltinProvider {
        kind: "minimax-cn",
        base_url: Some("https://api.minimaxi.com/v1"),
        auth: "api_key",
        summary: "MiniMax China chat; catalog GET https://api.minimaxi.com/v1/models",
    },
    BuiltinProvider {
        kind: "openai",
        base_url: Some("https://api.openai.com/v1"),
        auth: "api_key",
        summary: "OpenAI chat; GET /models catalog",
    },
    BuiltinProvider {
        kind: "groq",
        base_url: Some("https://api.groq.com/openai/v1"),
        auth: "api_key",
        summary: "Groq OpenAI-compatible chat",
    },
    BuiltinProvider {
        kind: "together",
        base_url: Some("https://api.together.xyz/v1"),
        auth: "api_key",
        summary: "Together OpenAI-compatible chat",
    },
    BuiltinProvider {
        kind: "fireworks",
        base_url: Some("https://api.fireworks.ai/inference/v1"),
        auth: "api_key",
        summary: "Fireworks OpenAI-compatible chat",
    },
    BuiltinProvider {
        kind: "openrouter",
        base_url: Some("https://openrouter.ai/api/v1"),
        auth: "api_key",
        summary: "OpenRouter OpenAI-compatible chat",
    },
    BuiltinProvider {
        kind: "xai",
        base_url: Some("https://api.x.ai/v1"),
        auth: "api_key",
        summary: "xAI OpenAI-compatible chat",
    },
    BuiltinProvider {
        kind: "cerebras",
        base_url: Some("https://api.cerebras.ai/v1"),
        auth: "api_key",
        summary: "Cerebras OpenAI-compatible chat",
    },
    BuiltinProvider {
        kind: "moonshot",
        base_url: Some("https://api.moonshot.ai/v1"),
        auth: "api_key",
        summary: "Moonshot OpenAI-compatible chat",
    },
    BuiltinProvider {
        kind: "gemini",
        base_url: Some("https://generativelanguage.googleapis.com/v1beta/openai"),
        auth: "api_key",
        summary: "Google Gemini API, OpenAI-compatible endpoint; catalog GET {base}/models",
    },
    BuiltinProvider {
        kind: "perplexity",
        base_url: Some("https://api.perplexity.ai"),
        auth: "api_key",
        summary: "Perplexity Sonar chat; catalog GET https://api.perplexity.ai/v1/models",
    },
    BuiltinProvider {
        kind: "huggingface",
        base_url: Some("https://router.huggingface.co/v1"),
        auth: "api_key",
        summary: "Hugging Face Inference Providers router; catalog GET {base}/models",
    },
    BuiltinProvider {
        kind: "nebius",
        base_url: Some("https://api.tokenfactory.nebius.com/v1"),
        auth: "api_key",
        summary: "Nebius Token Factory; catalog GET {base}/models",
    },
    BuiltinProvider {
        kind: "sambanova",
        base_url: Some("https://api.sambanova.ai/v1"),
        auth: "api_key",
        summary: "SambaNova Cloud; catalog GET {base}/models",
    },
    BuiltinProvider {
        kind: "anthropic",
        base_url: Some("https://api.anthropic.com/v1"),
        auth: "api_key",
        summary: "Anthropic Messages API with x-api-key (several keys per member); catalog GET https://api.anthropic.com/v1/models",
    },
    BuiltinProvider {
        kind: "opencode-go",
        base_url: Some("https://opencode.ai/zen/go/v1"),
        auth: "api_key",
        summary: "OpenCode Go subscription, OpenAI-compatible with an x-opencode-session header",
    },
    BuiltinProvider {
        kind: "opencode-zen",
        base_url: Some("https://opencode.ai/zen/v1"),
        auth: "api_key",
        summary: "OpenCode Zen pay-as-you-go, OpenAI-compatible",
    },
    BuiltinProvider {
        kind: "muse-code",
        base_url: Some("https://api.meta.ai/v1"),
        auth: "oauth",
        summary: "Muse Code device login, minted key, Responses chat, catalog GET https://api.meta.ai/v1/models",
    },
    BuiltinProvider {
        kind: "openai-codex",
        base_url: Some(crate::auth_flows::codex::BASE_URL),
        auth: "oauth",
        summary: "ChatGPT plan (Codex) sign-in; Responses chat at chatgpt.com/backend-api/codex, static model list",
    },
    BuiltinProvider {
        kind: "github-copilot",
        base_url: Some(crate::auth_flows::copilot::BASE_URL),
        auth: "oauth",
        summary: "GitHub Copilot device sign-in; chat completions at api.githubcopilot.com (or the plan's own host), catalog GET {base}/models",
    },
    BuiltinProvider {
        kind: "google-antigravity",
        base_url: Some("https://daily-cloudcode-pa.googleapis.com"),
        auth: "oauth",
        summary: "Antigravity PKCE; Cloud Code chat and POST /v1internal:fetchAvailableModels",
    },
];

pub fn builtins() -> &'static [BuiltinProvider] {
    BUILTINS
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OAuthStyle {
    MuseMint,
    RefreshToken,
    /// A Claude plan sign-in: a JSON refresh grant that rotates the token.
    ClaudeRefresh,
    /// A Codex (ChatGPT plan) sign-in: a form refresh grant that rotates the token.
    CodexRefresh,
    /// A GitHub Copilot sign-in: the long-lived GitHub token is exchanged for a
    /// short-lived Copilot token. The refresh token never rotates.
    CopilotExchange,
}

pub enum AuthPlan {
    ApiKey,
    OAuth {
        style: OAuthStyle,
        token_url: &'static str,
        client_id_fallback: Option<&'static str>,
    },
}

pub struct OAuthMaterial {
    pub style: OAuthStyle,
    pub refresh_token: String,
    pub token_url: String,
    pub client_id: String,
    pub client_secret: String,
}

pub struct MuseCredentials {
    pub api_key: SecretString,
    pub identity_token: SecretString,
}

impl fmt::Debug for MuseCredentials {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("MuseCredentials")
            .field("api_key", &"<redacted>")
            .field("identity_token", &"<redacted>")
            .finish()
    }
}

pub fn canonical_kind(kind: &str) -> Result<String> {
    let kind = kind.trim();
    if kind.is_empty() || kind == "openai-compatible" {
        return Ok("openai-compatible".into());
    }
    if BUILTINS.iter().any(|item| item.kind == kind) {
        return Ok(kind.to_owned());
    }
    Err(SeraError::Config(format!("unknown provider kind '{kind}'")))
}

pub fn resolve_base_url(kind: &str, base_url: &str) -> Result<String> {
    let kind = canonical_kind(kind)?;
    if kind == "google-antigravity" {
        let chosen = if base_url.trim().is_empty() {
            "https://daily-cloudcode-pa.googleapis.com"
        } else {
            base_url
        };
        let endpoint = validate_base_url(chosen)?;
        let _ = crate::cloud_code::cloud_code_origins(&endpoint)?;
        return Ok(endpoint);
    }
    let chosen = if base_url.trim().is_empty() {
        BUILTINS
            .iter()
            .find(|item| item.kind == kind)
            .and_then(|item| item.base_url)
            .ok_or_else(|| {
                SeraError::Config(format!(
                    "provider kind {kind} requires an explicit HTTPS base_url"
                ))
            })?
    } else {
        base_url
    };
    let endpoint = validate_base_url(chosen)?;
    let host = url::Url::parse(&endpoint)
        .ok()
        .and_then(|url| url.host_str().map(|host| host.to_ascii_lowercase()))
        .unwrap_or_default();
    if is_cloud_code_host(&host) {
        return Err(SeraError::Config(
            "Cloud Code Assist endpoints are not OpenAI-compatible chat bases".into(),
        ));
    }
    Ok(endpoint)
}

pub fn auth_plan(kind: &str, auth: &str) -> Result<AuthPlan> {
    let kind = canonical_kind(kind)?;
    let requested = auth.trim();
    let mode = if requested.is_empty() {
        if matches!(
            kind.as_str(),
            "muse-code" | "google-antigravity" | "openai-codex" | "github-copilot"
        ) {
            "oauth"
        } else {
            "api_key"
        }
    } else {
        requested
    };
    match mode {
        "api_key" => Ok(AuthPlan::ApiKey),
        "oauth" => match kind.as_str() {
            "muse-code" => Ok(AuthPlan::OAuth {
                style: OAuthStyle::MuseMint,
                token_url: "https://api.meta.ai/muse-code/key",
                client_id_fallback: None,
            }),
            "google-antigravity" => Ok(AuthPlan::OAuth {
                style: OAuthStyle::RefreshToken,
                token_url: "https://oauth2.googleapis.com/token",
                client_id_fallback: Some(ANTIGRAVITY_CLIENT_ID),
            }),
            "openai-codex" => Ok(AuthPlan::OAuth {
                style: OAuthStyle::CodexRefresh,
                token_url: crate::auth_flows::codex::TOKEN_URL,
                client_id_fallback: Some(crate::auth_flows::codex::CLIENT_ID),
            }),
            "github-copilot" => Ok(AuthPlan::OAuth {
                style: OAuthStyle::CopilotExchange,
                token_url: crate::auth_flows::copilot::TOKEN_URL,
                client_id_fallback: None,
            }),
            _ => Err(SeraError::Config(
                "oauth auth is only supported for muse-code, google-antigravity, openai-codex and github-copilot"
                    .into(),
            )),
        },
        _ => Err(SeraError::Config("auth must be api_key or oauth".into())),
    }
}

pub fn check_profile(
    kind: &str,
    base_url: &str,
    auth: &str,
    refresh_token_secret: &str,
    _oauth_client_secret_secret: &str,
    catalog_fetch: bool,
    catalog_ttl_secs: u64,
) -> Result<()> {
    let endpoint = resolve_base_url(kind, base_url)?;
    match auth_plan(kind, auth)? {
        AuthPlan::ApiKey => {
            if !refresh_token_secret.trim().is_empty() {
                return Err(SeraError::Config(
                    "refresh_token_secret requires auth = \"oauth\"".into(),
                ));
            }
        }
        // The Google client secret has a compiled-in default, so
        // `oauth_client_secret_secret` is only an optional override.
        AuthPlan::OAuth { .. } => {}
    }
    if catalog_fetch && !(1..=86_400).contains(&catalog_ttl_secs) {
        return Err(SeraError::Config(
            "catalog_ttl_secs must be between 1 and 86400 when catalog_fetch is true".into(),
        ));
    }
    if catalog_fetch {
        resolve_catalog(kind, &endpoint)?;
    }
    Ok(())
}

pub fn resolve_catalog(kind: &str, base_url: &str) -> Result<ResolvedCatalog> {
    let kind = canonical_kind(kind)?;
    if kind == "google-antigravity" {
        return Ok(ResolvedCatalog {
            url: crate::cloud_code::primary_catalog_url(base_url)?,
            strict: true,
        });
    }
    let base = validate_base_url(base_url)?;
    let parsed =
        url::Url::parse(&base).map_err(|_| SeraError::Config("invalid catalog base url".into()))?;
    if let Some(spec) = OFFICIAL_CATALOGS.iter().find(|item| item.kind == kind) {
        if is_exact_loopback(&parsed) {
            return Ok(ResolvedCatalog {
                url: validate_base_url(&format!("{base}/models"))?,
                strict: spec.strict,
            });
        }
        let host = parsed.host_str().unwrap_or("");
        if !host.eq_ignore_ascii_case(spec.host) {
            return Err(SeraError::Config(
                "catalog fetch for this provider only contacts its documented host".into(),
            ));
        }
        return Ok(ResolvedCatalog {
            url: validate_base_url(&format!("https://{}{}", spec.host, spec.path))?,
            strict: spec.strict,
        });
    }
    Ok(ResolvedCatalog {
        url: validate_base_url(&format!("{base}/models"))?,
        strict: false,
    })
}

fn is_exact_loopback(url: &url::Url) -> bool {
    match url.host() {
        Some(url::Host::Ipv4(ip)) => ip == std::net::Ipv4Addr::LOCALHOST,
        Some(url::Host::Ipv6(ip)) => ip == std::net::Ipv6Addr::LOCALHOST,
        _ => false,
    }
}

pub fn enforce_oauth_target(style: OAuthStyle, token_url: &str) -> Result<String> {
    let endpoint = validate_base_url(token_url)?;
    let parsed = url::Url::parse(&endpoint)
        .map_err(|_| SeraError::Config("invalid oauth token url".into()))?;
    let host = parsed.host_str().unwrap_or("");
    let ok = match style {
        OAuthStyle::MuseMint => {
            host.eq_ignore_ascii_case(MUSE_API_HOST) && parsed.path() == MUSE_MINT_PATH
        }
        OAuthStyle::RefreshToken => {
            host.eq_ignore_ascii_case(GOOGLE_TOKEN_HOST) && parsed.path() == GOOGLE_TOKEN_PATH
        }
        OAuthStyle::ClaudeRefresh => {
            host.eq_ignore_ascii_case(crate::auth_flows::claude::TOKEN_HOST)
                && parsed.path() == crate::auth_flows::claude::TOKEN_PATH
        }
        OAuthStyle::CodexRefresh => {
            host.eq_ignore_ascii_case(crate::auth_flows::codex::AUTH_HOST)
                && parsed.path() == crate::auth_flows::codex::TOKEN_PATH
        }
        OAuthStyle::CopilotExchange => {
            host.eq_ignore_ascii_case(crate::auth_flows::copilot::API_HOST)
                && parsed.path() == crate::auth_flows::copilot::TOKEN_PATH
        }
    };
    if !ok {
        return Err(SeraError::Config(
            "oauth token url is not the documented endpoint for this provider".into(),
        ));
    }
    Ok(endpoint)
}

pub fn valid_model_id(model: &str) -> bool {
    !model.is_empty()
        && model.len() <= 256
        && model.trim() == model
        && !model.chars().any(char::is_control)
}

/// What a provider's own model list says about one model. Everything is
/// optional: only fields the API actually returned are filled in.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize)]
pub struct ModelMeta {
    /// Friendly name when the API gives one that differs from the id.
    pub name: Option<String>,
    /// Context window in tokens.
    pub context_length: Option<u64>,
    /// Largest reply in tokens, when the API says.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_output_tokens: Option<u64>,
    /// Capability tags from the API: `vision`, `tools`, `reasoning`, `free`,
    /// `cheap`.
    pub tags: Vec<String>,
}

/// A provider's live chat-capable model list, keyed by model id.
pub type ModelCatalog = std::collections::BTreeMap<String, ModelMeta>;

/// Catalog shape check shared by the loose and strict parsers.
const CATALOG_MAX_BYTES: usize = 1_048_576;
const CATALOG_MAX_ENTRIES: usize = 4_000;

/// Parse a provider's model list into ids plus metadata, keeping only models
/// that can chat. Accepts `{data:[...]}`, `{models:[...]}` or a bare array and
/// strips Gemini's `models/` prefix. `strict` is for providers whose list shape
/// is pinned (`{object:"list", data:[{id, object:"model"}]}`): any other shape
/// fails closed. Ids and bodies are never copied into errors.
pub fn parse_model_catalog(bytes: &[u8], strict: bool) -> Result<ModelCatalog> {
    if bytes.len() > CATALOG_MAX_BYTES {
        return Err(SeraError::External("model catalog is too large".into()));
    }
    let value: Value = serde_json::from_slice(bytes)
        .map_err(|_| SeraError::External("malformed model catalog".into()))?;
    if strict && value.get("object").and_then(Value::as_str) != Some("list") {
        return Err(SeraError::External("model catalog was not a list".into()));
    }
    let entries = if strict {
        value.get("data").and_then(Value::as_array)
    } else {
        value
            .get("data")
            .and_then(Value::as_array)
            .or_else(|| value.get("models").and_then(Value::as_array))
            .or_else(|| value.as_array())
    }
    .ok_or_else(|| SeraError::External("model catalog has no model list".into()))?;
    if entries.len() > CATALOG_MAX_ENTRIES {
        return Err(SeraError::External("model catalog is too large".into()));
    }
    let mut catalog = ModelCatalog::new();
    for entry in entries {
        if strict
            && entry
                .get("object")
                .and_then(Value::as_str)
                .is_some_and(|kind| kind != "model")
        {
            continue;
        }
        let Some(raw) = entry.get("id").and_then(Value::as_str) else {
            continue;
        };
        let id = raw.strip_prefix("models/").unwrap_or(raw);
        let id_ok = if strict {
            official_model_id(id)
        } else {
            valid_model_id(id)
        };
        if !id_ok || !chat_capable(id, entry) {
            continue;
        }
        catalog.insert(id.to_owned(), model_meta(id, entry));
    }
    if catalog.is_empty() {
        return Err(SeraError::External(
            "model catalog contained no usable model ids".into(),
        ));
    }
    Ok(catalog)
}

/// Ids only, for callers that do not need metadata.
pub fn parse_model_ids(bytes: &[u8]) -> Result<BTreeSet<String>> {
    parse_model_catalog(bytes, false).map(|catalog| catalog.into_keys().collect())
}

/// Official Mistral, DeepSeek, and MiniMax lists are `{object:list, data:[{id, object:model}]}`.
/// Unexpected shapes fail closed. Ids and response bodies are never copied into errors.
pub fn parse_official_catalog(bytes: &[u8]) -> Result<BTreeSet<String>> {
    parse_model_catalog(bytes, true).map(|catalog| catalog.into_keys().collect())
}

/// Ids that are never chat models, used only when the API says nothing about
/// capabilities.
const NON_CHAT_ID_HINTS: [&str; 10] = [
    "embed",
    "moderation",
    "whisper",
    "tts",
    "dall-e",
    "gpt-image",
    "transcribe",
    "rerank",
    "sora",
    "realtime",
];

fn chat_capable(id: &str, entry: &Value) -> bool {
    // GitHub Copilot's list carries embeddings too, models that only take the
    // Responses API (this client speaks chat completions) and models its
    // picker hides.
    if entry
        .pointer("/capabilities/type")
        .and_then(Value::as_str)
        .is_some_and(|kind| !kind.eq_ignore_ascii_case("chat"))
    {
        return false;
    }
    if entry
        .get("supported_endpoints")
        .and_then(Value::as_array)
        .is_some_and(|endpoints| {
            !endpoints.is_empty()
                && !endpoints
                    .iter()
                    .any(|item| item.as_str() == Some("/chat/completions"))
        })
    {
        return false;
    }
    if entry.get("model_picker_enabled").and_then(Value::as_bool) == Some(false) {
        return false;
    }
    if let Some(chat) = entry
        .pointer("/capabilities/completion_chat")
        .and_then(Value::as_bool)
    {
        return chat;
    }
    if entry.get("supports_chat").and_then(Value::as_bool) == Some(false) {
        return false;
    }
    if let Some(kind) = entry
        .get("type")
        .and_then(Value::as_str)
        .map(str::to_ascii_lowercase)
    {
        return !matches!(
            kind.as_str(),
            "image" | "embedding" | "embeddings" | "moderation" | "rerank" | "audio" | "video"
        ) && !kind.contains("transcri");
    }
    if let Some(outputs) = entry
        .pointer("/architecture/output_modalities")
        .and_then(Value::as_array)
    {
        return outputs.iter().any(|item| item.as_str() == Some("text"));
    }
    let lowered = id.to_ascii_lowercase();
    !NON_CHAT_ID_HINTS.iter().any(|hint| lowered.contains(hint))
}

fn model_meta(id: &str, entry: &Value) -> ModelMeta {
    let name = ["name", "display_name", "displayName"]
        .iter()
        .find_map(|key| entry.get(*key).and_then(Value::as_str))
        .map(str::trim)
        .filter(|name| !name.is_empty() && name.len() <= 120 && *name != id)
        .filter(|name| !name.chars().any(char::is_control))
        .map(str::to_owned);
    let context_length = [
        // Copilot's prompt limit is what a conversation must fit, and is often
        // well under the model's native window.
        "/capabilities/limits/max_prompt_tokens",
        "/capabilities/limits/max_context_window_tokens",
        "/max_context_length",
        "/context_length",
        "/context_window",
        "/max_model_len",
        "/inputTokenLimit",
        "/top_provider/context_length",
    ]
    .iter()
    .find_map(|pointer| entry.pointer(pointer).and_then(Value::as_u64))
    .filter(|tokens| *tokens > 0);
    let has_item = |pointer: &str, wanted: &str| {
        entry
            .pointer(pointer)
            .and_then(Value::as_array)
            .is_some_and(|items| items.iter().any(|item| item.as_str() == Some(wanted)))
    };
    let flag = |pointer: &str| entry.pointer(pointer).and_then(Value::as_bool) == Some(true);
    let mut tags = Vec::new();
    if flag("/capabilities/vision")
        || flag("/capabilities/supports/vision")
        || flag("/supports_image_input")
        || has_item("/architecture/input_modalities", "image")
    {
        tags.push("vision".to_owned());
    }
    if flag("/capabilities/function_calling")
        || flag("/capabilities/supports/tool_calls")
        || flag("/supports_tools")
        || has_item("/supported_parameters", "tools")
    {
        tags.push("tools".to_owned());
    }
    if flag("/capabilities/reasoning")
        || flag("/capabilities/supports/thinking")
        || has_item("/supported_parameters", "reasoning")
        || has_item("/supported_parameters", "include_reasoning")
    {
        tags.push("reasoning".to_owned());
    }
    // Prices are USD per token, as decimal strings (OpenRouter).
    if let Some(price) = entry
        .pointer("/pricing/prompt")
        .and_then(Value::as_str)
        .and_then(|text| text.parse::<f64>().ok())
        .filter(|price| price.is_finite() && *price >= 0.0)
    {
        if price == 0.0 {
            tags.push("free".to_owned());
        } else if price <= 0.000_000_3 {
            tags.push("cheap".to_owned());
        }
    }
    ModelMeta {
        name,
        context_length,
        max_output_tokens: entry
            .pointer("/capabilities/limits/max_output_tokens")
            .and_then(Value::as_u64)
            .filter(|tokens| *tokens > 0),
        tags,
    }
}
pub(crate) fn official_model_id(model: &str) -> bool {
    valid_model_id(model)
        && model
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-' | ':' | '+'))
}

/// How long the sign-in mint waits before each retry on a 429 or 5xx. The
/// device code is one-shot, so a transient mint failure must not fail the login.
pub const MUSE_MINT_RETRY_WAITS: [Duration; 2] = [Duration::from_secs(5), Duration::from_secs(10)];

/// What a successful Muse key mint says. The key is a secret; `Debug` hides it.
pub struct MuseMint {
    pub api_key: String,
    /// Lower-cased account email, when Meta said.
    pub email: Option<String>,
    /// The subscription tier name, when Meta said.
    pub tier: Option<String>,
    /// Meta's stable user id, when it said.
    pub user_id: Option<String>,
}

impl fmt::Debug for MuseMint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("MuseMint")
            .field("email", &self.email)
            .field("tier", &self.tier)
            .finish_non_exhaustive()
    }
}

/// Re-mint a Muse API key from the Meta identity token. Never sends `onboard`;
/// only the interactive sign-in does (see [`muse_mint`]).
pub async fn muse_mint_api_key(
    http: &reqwest::Client,
    token_url: &str,
    identity_token: &str,
) -> Result<String> {
    muse_mint(http, token_url, identity_token, false, &[])
        .await
        .map(|mint| mint.api_key)
}

/// Mint a Muse Code API key. `onboard: true` is for the interactive sign-in
/// only: it creates the subscription record. On a 429 or a 5xx the mint waits
/// for each of `retry_waits` and tries again.
pub async fn muse_mint(
    http: &reqwest::Client,
    token_url: &str,
    identity_token: &str,
    onboard: bool,
    retry_waits: &[Duration],
) -> Result<MuseMint> {
    let body = if onboard {
        serde_json::json!({"onboard": true})
    } else {
        serde_json::json!({})
    };
    let mut attempt = 0;
    let (status, bytes) = loop {
        let response = http
            .post(token_url)
            .header("user-agent", USER_AGENT)
            .header("accept", "application/json")
            .header("x-api-version", MUSE_API_VERSION)
            .bearer_auth(identity_token)
            .json(&body)
            .send()
            .await
            .map_err(|_| SeraError::External("Muse API-key mint failed".into()))?;
        let status = response.status();
        let bytes = response
            .bytes()
            .await
            .map_err(|_| SeraError::External("Muse API-key mint failed".into()))?;
        let transient = status.as_u16() == 429 || status.is_server_error();
        match retry_waits.get(attempt) {
            Some(wait) if transient => {
                attempt += 1;
                tokio::time::sleep(*wait).await;
            }
            _ => break (status, bytes),
        }
    };
    if !status.is_success() {
        // Typed, so a re-mint that is refused (401/403) can park the account.
        return Err(SeraError::upstream(
            status.as_u16(),
            "mint_failed",
            format!("Muse API-key mint failed with status {}", status.as_u16()),
        ));
    }
    if bytes.len() > 65_536 {
        return Err(SeraError::External("Muse API-key mint failed".into()));
    }
    let value: Value = serde_json::from_slice(&bytes)
        .map_err(|_| SeraError::External("Muse API-key mint failed".into()))?;
    if value.get("is_subs_active").and_then(Value::as_bool) == Some(false) {
        return Err(SeraError::Rejected {
            code: "muse_inactive",
            message: "Your Muse Code subscription is inactive. Activate it on muse.ai first."
                .into(),
        });
    }
    let key = value
        .get("api_key")
        .and_then(Value::as_str)
        .filter(|key| credential_ok(key));
    let Some(key) = key else {
        let payment = value.get("require_payment").and_then(Value::as_bool) == Some(true)
            || value.get("action_url").is_some_and(|url| !url.is_null())
            || value
                .get("require_payment_action_url")
                .is_some_and(|url| !url.is_null());
        return Err(if payment {
            SeraError::Rejected {
                code: "muse_subscription_required",
                message: "A Muse Code subscription is required. Subscribe on muse.ai first.".into(),
            }
        } else {
            SeraError::External("Muse API-key mint failed: the reply had no API key".into())
        });
    };
    let text = |field: &str, max: usize| {
        value
            .get(field)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|text| !text.is_empty() && text.len() <= max)
            .filter(|text| !text.chars().any(char::is_control))
            .map(str::to_owned)
    };
    Ok(MuseMint {
        api_key: key.to_owned(),
        email: text("user_email", 254).map(|email| email.to_lowercase()),
        tier: text("subs_tier_name", 64),
        user_id: text("user_id", 128),
    })
}

pub struct AccessGrant {
    pub access_token: String,
    pub refresh_token: Option<String>,
    /// Seconds the access token lives, when the provider said.
    pub expires_in: Option<u64>,
}

impl fmt::Debug for AccessGrant {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AccessGrant")
            .field("access_token", &"<redacted>")
            .field(
                "refresh_token",
                &self.refresh_token.as_ref().map(|_| "<redacted>"),
            )
            .finish()
    }
}

pub async fn refresh_token_grant(
    http: &reqwest::Client,
    token_url: &str,
    refresh_token: &str,
    client_id: &str,
    client_secret: &str,
) -> Result<AccessGrant> {
    refresh_token_exchange(http, token_url, refresh_token, client_id, client_secret)
        .await
        .map_err(RefreshError::into_error)
}

/// Like [`refresh_token_grant`], but tells a refused refresh token
/// (`invalid_grant`, or a 401) apart from a transient failure, so the caller
/// can park the account instead of retrying.
pub async fn refresh_token_exchange(
    http: &reqwest::Client,
    token_url: &str,
    refresh_token: &str,
    client_id: &str,
    client_secret: &str,
) -> std::result::Result<AccessGrant, RefreshError> {
    let failed = |message: &str| RefreshError::Failed(SeraError::External(message.into()));
    let response = http
        .post(token_url)
        .header("user-agent", USER_AGENT)
        .header("accept", "application/json")
        .form(&[
            ("grant_type", "refresh_token"),
            ("refresh_token", refresh_token),
            ("client_id", client_id),
            ("client_secret", client_secret),
        ])
        .send()
        .await
        .map_err(|_| failed("oauth token refresh failed"))?;
    let status = response.status();
    let bytes = response
        .bytes()
        .await
        .map_err(|_| failed("oauth token refresh failed"))?;
    let value: Option<Value> = if bytes.len() > 65_536 {
        None
    } else {
        serde_json::from_slice(&bytes).ok()
    };
    if !status.is_success() {
        let refused = matches!(status.as_u16(), 400 | 401)
            && (status.as_u16() == 401
                || value
                    .as_ref()
                    .and_then(|body| body.get("error"))
                    .and_then(Value::as_str)
                    == Some("invalid_grant"));
        if refused {
            return Err(RefreshError::InvalidGrant);
        }
        return Err(RefreshError::Failed(SeraError::External(format!(
            "oauth token refresh failed with status {}",
            status.as_u16()
        ))));
    }
    let value = value.ok_or_else(|| failed("oauth token refresh failed"))?;
    let token = value
        .get("access_token")
        .and_then(Value::as_str)
        .filter(|token| credential_ok(token))
        .ok_or_else(|| failed("oauth token refresh failed"))?;
    let rotated = value
        .get("refresh_token")
        .and_then(Value::as_str)
        .filter(|token| credential_ok(token))
        .map(str::to_owned);
    let expires_in = value
        .get("expires_in")
        .and_then(Value::as_u64)
        .filter(|seconds| *seconds > 0);
    Ok(AccessGrant {
        access_token: token.to_owned(),
        refresh_token: rotated,
        expires_in,
    })
}

pub async fn login_muse_code<F>(http: &reqwest::Client, on_code: F) -> Result<MuseCredentials>
where
    F: FnOnce(&str, &str),
{
    login_muse_inner(http, &documented_endpoints()?, on_code, tokio::time::sleep).await
}

struct MuseEndpoints {
    authorization: String,
    token: String,
    mint: String,
}

fn documented_endpoints() -> Result<MuseEndpoints> {
    Ok(MuseEndpoints {
        authorization: validate_base_url("https://auth.meta.com/oidc/device/authorization")?,
        token: validate_base_url("https://auth.meta.com/oidc/device/token")?,
        mint: validate_base_url("https://api.meta.ai/muse-code/key")?,
    })
}

async fn login_muse_inner<F, S, Fut>(
    http: &reqwest::Client,
    endpoints: &MuseEndpoints,
    on_code: F,
    sleep: S,
) -> Result<MuseCredentials>
where
    F: FnOnce(&str, &str),
    S: Fn(Duration) -> Fut,
    Fut: Future<Output = ()>,
{
    let (status, body) = post_form(
        http,
        &endpoints.authorization,
        &[("client_id", MUSE_CLIENT_ID)],
    )
    .await?;
    if !status.is_success() {
        return Err(SeraError::External(format!(
            "Muse login could not be started (HTTP {})",
            status.as_u16()
        )));
    }
    let device_code = required_code(body.get("device_code"))?;
    let user_code = required_code(body.get("user_code"))?;
    let verification = body
        .get("verification_uri_complete")
        .and_then(Value::as_str)
        .or_else(|| body.get("verification_uri").and_then(Value::as_str))
        .ok_or_else(|| SeraError::External("Muse verification URL was rejected".into()))?;
    let verification = pin_muse_verification_uri(verification)?;
    let mut interval = bounded_secs(body.get("interval"), 5, 60);
    let expires = bounded_secs(body.get("expires_in"), 900, 900);
    on_code(&verification, &user_code);
    let deadline = Instant::now() + Duration::from_secs(expires);
    let mut identity = None;
    for _ in 0..200 {
        if Instant::now() >= deadline {
            break;
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        sleep(Duration::from_secs(interval).min(remaining)).await;
        if Instant::now() >= deadline {
            break;
        }
        let (status, body) = post_form(
            http,
            &endpoints.token,
            &[
                ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
                ("device_code", device_code.as_str()),
                ("client_id", MUSE_CLIENT_ID),
            ],
        )
        .await?;
        if status.is_success() {
            if let Some(token) = body
                .get("access_token")
                .and_then(Value::as_str)
                .filter(|token| credential_ok(token))
            {
                identity = Some(token.to_owned());
                break;
            }
            return Err(SeraError::External("Muse login failed".into()));
        }
        match oauth_error_code(&body) {
            "authorization_pending" => {}
            "slow_down" => interval = interval.saturating_add(5).clamp(1, 60),
            "access_denied" => return Err(SeraError::External("Muse login was denied".into())),
            "expired_token" => {
                return Err(SeraError::External("Muse login request expired".into()));
            }
            _ => {
                return Err(SeraError::External(format!(
                    "Muse login failed (HTTP {})",
                    status.as_u16()
                )));
            }
        }
    }
    let Some(identity) = identity else {
        return Err(SeraError::External("Muse login request expired".into()));
    };
    let api_key = muse_mint(
        http,
        &endpoints.mint,
        &identity,
        true,
        &MUSE_MINT_RETRY_WAITS,
    )
    .await?
    .api_key;
    Ok(MuseCredentials {
        api_key: SecretString::new(api_key),
        identity_token: SecretString::new(identity),
    })
}

fn pin_muse_verification_uri(value: &str) -> Result<String> {
    let url = url::Url::parse(value)
        .map_err(|_| SeraError::External("Muse verification URL was rejected".into()))?;
    let host = url.host_str().unwrap_or("");
    if url.scheme() != "https"
        || !host.eq_ignore_ascii_case(MUSE_AUTH_HOST)
        || !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
    {
        return Err(SeraError::External(
            "Muse verification URL was rejected".into(),
        ));
    }
    Ok(url.to_string())
}

async fn post_form(
    http: &reqwest::Client,
    url: &str,
    form: &[(&str, &str)],
) -> Result<(reqwest::StatusCode, Value)> {
    let response = http
        .post(url)
        .header("user-agent", USER_AGENT)
        .header("accept", "application/json")
        .form(form)
        .send()
        .await
        .map_err(|_| SeraError::External("Muse login request failed".into()))?;
    let status = response.status();
    let bytes = response
        .bytes()
        .await
        .map_err(|_| SeraError::External("Muse login request failed".into()))?;
    if bytes.len() > 65_536 {
        return Err(SeraError::External(
            "Muse login response is too large".into(),
        ));
    }
    let value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    Ok((status, value))
}

fn required_code(value: Option<&Value>) -> Result<String> {
    let code = value
        .and_then(Value::as_str)
        .filter(|code| credential_ok(code) && code.len() <= 128)
        .ok_or_else(|| SeraError::External("Muse login returned an incomplete response".into()))?;
    Ok(code.to_owned())
}

fn credential_ok(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 8_192
        && value.trim() == value
        && !value.chars().any(char::is_control)
}

fn bounded_secs(value: Option<&Value>, fallback: u64, cap: u64) -> u64 {
    let secs = value
        .and_then(|value| {
            value.as_u64().or_else(|| {
                value
                    .as_f64()
                    .filter(|number| number.is_finite() && *number > 0.0)
                    .map(|number| number as u64)
            })
        })
        .unwrap_or(fallback);
    secs.clamp(1, cap)
}

fn oauth_error_code(value: &Value) -> &'static str {
    match value.get("error").and_then(Value::as_str) {
        Some("authorization_pending") => "authorization_pending",
        Some("slow_down") => "slow_down",
        Some("access_denied") => "access_denied",
        Some("expired_token") => "expired_token",
        _ => "unrecognized",
    }
}

fn is_cloud_code_host(host: &str) -> bool {
    const ROOTS: [&str; 2] = [
        "cloudcode-pa.googleapis.com",
        "cloudcode-pa.sandbox.googleapis.com",
    ];
    ROOTS.iter().any(|root| {
        host == *root || host.ends_with(&format!(".{root}")) || host.ends_with(&format!("-{root}"))
    })
}

// ---- Per-user provider credentials (multi-user phase 4) ----

/// Per-user provider client factory.
///
/// Provider *endpoints and model catalogs* stay admin-defined in config; the
/// *credential* comes from the acting user's envelope-encrypted row (or an
/// active admin grant). Building a client for a user with no credential is an
/// explicit error â€” never a fallback to another account's key.
pub struct UserProviderCredentials {
    users: Arc<UserSecrets>,
    server_secrets: SharedSecrets,
    timeout_secs: u64,
    max_output_tokens: u32,
    /// Account reads, and the single-flight refresh every OAuth client uses.
    refresher: Arc<AccountRefresher>,
}

impl std::fmt::Debug for UserProviderCredentials {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("UserProviderCredentials")
            .field("timeout_secs", &self.timeout_secs)
            .field("max_output_tokens", &self.max_output_tokens)
            .finish_non_exhaustive()
    }
}

impl UserProviderCredentials {
    pub fn new(
        users: Arc<UserSecrets>,
        server_secrets: SharedSecrets,
        timeout_secs: u64,
        max_output_tokens: u32,
    ) -> Self {
        let accounts = Arc::new(Accounts::new(users.store().clone(), users.clone()));
        Self {
            users,
            server_secrets,
            timeout_secs,
            max_output_tokens,
            refresher: Arc::new(AccountRefresher::new(accounts)),
        }
    }

    /// Use a refresher shared with the API, so a token refreshed there and a
    /// token refreshed by a turn take the same lock.
    pub fn with_refresher(mut self, refresher: Arc<AccountRefresher>) -> Self {
        self.refresher = refresher;
        self
    }

    pub fn refresher(&self) -> &Arc<AccountRefresher> {
        &self.refresher
    }

    /// The account a client built without a pin uses: the acting user's own
    /// best account. Healthy accounts come first (priority, then oldest); the
    /// legacy `default` credential sits where priority 0 puts it. `None` when
    /// the user has no account of their own (an admin grant may still apply).
    fn pick_account(&self, user_id: &str, provider: &str) -> Result<Option<String>> {
        let accounts = self.refresher.accounts();
        let mut candidates: Vec<(i64, AccountStatus, String)> = accounts
            .store()
            .provider_accounts(user_id, Some(provider))?
            .into_iter()
            .filter(|account| account.status != AccountStatus::Disabled)
            .map(|account| (account.priority, account.status, account.account_id))
            .collect();
        if accounts
            .store()
            .user_credential(user_id, provider)?
            .is_some()
        {
            let at = candidates
                .iter()
                .position(|(priority, _, _)| *priority < 0)
                .unwrap_or(candidates.len());
            candidates.insert(at, (0, AccountStatus::Ok, DEFAULT_ACCOUNT.to_owned()));
        }
        let rank = |status: AccountStatus| match status {
            AccountStatus::Ok => 0,
            AccountStatus::RateLimited | AccountStatus::QuotaExhausted => 1,
            AccountStatus::NeedsReauth | AccountStatus::Disabled => 2,
        };
        // A stable sort keeps the priority order inside each rank.
        candidates.sort_by_key(|(_, status, _)| rank(*status));
        Ok(candidates.into_iter().next().map(|(_, _, id)| id))
    }
}

impl ProviderCredentials for UserProviderCredentials {
    fn available(&self, user_id: &str, provider: &str) -> bool {
        let granted_or_default = self
            .users
            .credential_status(user_id, provider)
            .map(|status| status.usable())
            .unwrap_or(false);
        granted_or_default
            || self
                .users
                .store()
                .usable_account_ids(user_id, provider)
                .map(|ids| !ids.is_empty())
                .unwrap_or(false)
    }

    fn client(&self, user_id: &str, provider: &str, profile: &ProviderConfig) -> Result<LlmClient> {
        if let Some(account_id) = self.pick_account(user_id, provider)? {
            return self.client_for_account(user_id, provider, profile, &account_id);
        }
        // No account of the user's own: an admin grant, or nothing.
        let (owner, secret) = self
            .users
            .resolve_with_owner(user_id, provider)?
            .ok_or_else(|| {
                SeraError::Forbidden(format!(
                    "no credential is configured for provider '{provider}'; add a key or ask the admin for a grant"
                ))
            })?;
        let credential = ProviderCredential::parse(&secret)?;
        open_provider_client(
            profile,
            &credential,
            &self.server_secrets,
            self.timeout_secs,
            self.max_output_tokens,
            CredentialOwner {
                users: &self.users,
                user_id: &owner,
                provider,
                account_id: DEFAULT_ACCOUNT,
                refresher: Some(&self.refresher),
            },
        )
    }

    fn client_for_account(
        &self,
        user_id: &str,
        provider: &str,
        profile: &ProviderConfig,
        account_id: &str,
    ) -> Result<LlmClient> {
        let credential = self
            .refresher
            .accounts()
            .credential(user_id, provider, account_id)?
            .ok_or_else(|| {
                SeraError::Forbidden(format!(
                    "no credential is configured for provider '{provider}'; add a key or ask the admin for a grant"
                ))
            })?;
        open_provider_client(
            profile,
            &credential,
            &self.server_secrets,
            self.timeout_secs,
            self.max_output_tokens,
            CredentialOwner {
                users: &self.users,
                user_id,
                provider,
                account_id,
                refresher: Some(&self.refresher),
            },
        )
    }
}

/// Where a per-user provider client's rotated credential must be written.
#[derive(Clone, Copy)]
pub struct CredentialOwner<'a> {
    pub users: &'a Arc<UserSecrets>,
    /// The credential's owner: the acting user, or the grantor for a grant.
    pub user_id: &'a str,
    pub provider: &'a str,
    /// The owner's account the credential came from ([`DEFAULT_ACCOUNT`] for a
    /// pre-accounts credential or a grant).
    pub account_id: &'a str,
    /// When set, an OAuth client refreshes through this single-flight
    /// refresher and the rotated tokens are written to the account row.
    pub refresher: Option<&'a Arc<AccountRefresher>>,
}

/// Build one provider client from a resolved credential.
///
/// `credential.api_key` is the bearer (an API key, or the access token for an
/// OAuth provider); `credential.refresh_token` enables the refresh flow. The
/// OAuth *app* client id/secret stay server-level in the age store - they are
/// the registration, not the user's secret.
pub fn open_provider_client(
    profile: &ProviderConfig,
    credential: &ProviderCredential,
    server_secrets: &SharedSecrets,
    timeout_secs: u64,
    max_output_tokens: u32,
    owner: CredentialOwner<'_>,
) -> Result<LlmClient> {
    if credential.api_key.trim().is_empty() {
        return Err(SeraError::Config(
            "the stored provider credential has no access token or API key".into(),
        ));
    }
    let endpoint = resolve_base_url(&profile.kind, &profile.base_url)?;
    let kind = canonical_kind(&profile.kind)?;
    // A Claude plan sign-in is an `anthropic` account that carries a refresh
    // token. It exists only for the admin's own account, only while the
    // operator has not turned `plan_oauth` off, and is never reachable through
    // a grant.
    let claude_plan = kind == "anthropic" && !credential.refresh_token.trim().is_empty();
    if claude_plan && !(profile.plan_oauth_enabled() && owner.account_id != DEFAULT_ACCOUNT) {
        return Err(SeraError::Forbidden(
            "Claude plan sign-in is turned off on this server".into(),
        ));
    }
    // A Codex (ChatGPT plan) account exists only as a member's own sign-in with
    // a refresh token, only while the operator has not turned `plan_oauth`
    // off, and is never reachable through a grant (a grant carries only the
    // grantor's default credential).
    if kind == "openai-codex"
        && !(profile.plan_oauth_enabled()
            && owner.account_id != DEFAULT_ACCOUNT
            && !credential.refresh_token.trim().is_empty())
    {
        return Err(SeraError::Forbidden(
            "Codex plan sign-in is turned off on this server".into(),
        ));
    }
    // A GitHub Copilot account is likewise a member's own sign-in only.
    if kind == "github-copilot"
        && !(profile.plan_oauth_enabled()
            && owner.account_id != DEFAULT_ACCOUNT
            && !credential.refresh_token.trim().is_empty())
    {
        return Err(SeraError::Forbidden(
            "GitHub Copilot sign-in is turned off on this server".into(),
        ));
    }
    // The plan decides the Copilot host (individual, business, enterprise); the
    // sign-in remembered the one GitHub named.
    let endpoint = if kind == "github-copilot"
        && profile.base_url.trim().is_empty()
        && let Some(refresher) = owner.refresher
        && let Some(base) = refresher
            .accounts()
            .store()
            .provider_account(owner.user_id, owner.provider, owner.account_id)?
            .and_then(|row| {
                row.identity
                    .get("api_base")
                    .and_then(Value::as_str)
                    .and_then(crate::auth_flows::copilot::api_base_ok)
            }) {
        base
    } else {
        endpoint
    };
    let mut client = LlmClient::new(
        &endpoint,
        credential.api_key.clone(),
        timeout_secs,
        max_output_tokens,
    )?;
    let plan = auth_plan(&profile.kind, &profile.auth)?;
    let mut oauth_installed = false;
    // A member's own sign-in carries its refresh token in the account row; the
    // config marker only matters for the operator's imported credential.
    let refreshable =
        !profile.refresh_token_secret.trim().is_empty() || owner.account_id != DEFAULT_ACCOUNT;
    let oauth_target = match plan {
        AuthPlan::OAuth {
            style,
            token_url,
            client_id_fallback,
        } => Some((style, token_url, client_id_fallback)),
        AuthPlan::ApiKey if claude_plan => Some((
            OAuthStyle::ClaudeRefresh,
            crate::auth_flows::claude::TOKEN_URL,
            None,
        )),
        AuthPlan::ApiKey => None,
    };
    if let Some((style, token_url, client_id_fallback)) = oauth_target
        && refreshable
        && !credential.refresh_token.trim().is_empty()
    {
        let client_id = if style == OAuthStyle::ClaudeRefresh {
            // Claude Code's public client id, unless an age-store value overrides it.
            crate::auth_flows::claude::client_id_for(
                server_secrets,
                &profile.oauth_client_id_secret,
            )
        } else if profile.oauth_client_id_secret.trim().is_empty() {
            client_id_fallback.unwrap_or("").to_owned()
        } else {
            server_secrets.require(&profile.oauth_client_id_secret)?
        };
        // Antigravity's client secret is a public installed-app credential:
        // an age-store override, then AGY_CLIENT_SECRET, then the default.
        let client_secret = if style == OAuthStyle::RefreshToken {
            crate::cloud_code::antigravity_client_secret(
                server_secrets,
                &profile.oauth_client_secret_secret,
            )
        } else if profile.oauth_client_secret_secret.trim().is_empty() {
            String::new()
        } else {
            server_secrets.require(&profile.oauth_client_secret_secret)?
        };
        client = client.with_oauth(OAuthMaterial {
            style,
            refresh_token: credential.refresh_token.clone(),
            token_url: token_url.to_owned(),
            client_id,
            client_secret,
        })?;
        oauth_installed = true;
    }
    if oauth_installed {
        match owner.refresher {
            // Every refresh goes through the account's single-flight lock and
            // is written back to the owner's account row.
            Some(refresher) => {
                client = client.with_account_refresh(
                    AccountRefreshTarget {
                        refresher: refresher.clone(),
                        user_id: owner.user_id.to_owned(),
                        provider: owner.provider.to_owned(),
                        account_id: owner.account_id.to_owned(),
                    },
                    credential.generation.clone(),
                    credential.expires_at,
                )?;
            }
            // Rotation writes back into the credential's owner row (the acting
            // user, or the grantor for a granted provider).
            None => {
                client =
                    client.with_credential_writer(Arc::new(UserCredentialWriter::provider(
                        owner.users.clone(),
                        owner.user_id,
                        owner.provider,
                    )))?;
            }
        }
    }
    match kind.as_str() {
        "muse-code" => {
            client = client
                .with_header("x-api-version", MUSE_API_VERSION)
                .with_wire(Wire::Responses);
        }
        "anthropic" => {
            client = client.with_wire(if claude_plan {
                Wire::AnthropicPlan
            } else {
                Wire::AnthropicKey
            });
        }
        "openai-codex" => client = client.with_wire(Wire::Responses),
        "github-copilot" => client = client.with_copilot(),
        "opencode-go" => client = client.with_opencode_session(),
        _ => {}
    }
    // The operator's own credential uses the provider's project; a member's
    // account never does, it has its own.
    let member_account = owner.account_id != DEFAULT_ACCOUNT;
    if kind == "google-antigravity" {
        let project = if member_account {
            ""
        } else {
            profile.project_id.as_str()
        };
        client = client.with_cloud_code(project)?;
        if !profile.client_version.trim().is_empty() {
            client = client.with_cloud_version(&profile.client_version);
        }
    }
    if member_account && let Some(refresher) = owner.refresher {
        let accounts = refresher.accounts();
        let row =
            accounts
                .store()
                .provider_account(owner.user_id, owner.provider, owner.account_id)?;
        let generation = row.as_ref().map_or(0, |row| row.session_generation);
        client = client.with_account(
            AccountRefreshTarget {
                refresher: refresher.clone(),
                user_id: owner.user_id.to_owned(),
                provider: owner.provider.to_owned(),
                account_id: owner.account_id.to_owned(),
            },
            generation,
            matches!(
                kind.as_str(),
                "muse-code" | "anthropic" | "openai-codex" | "github-copilot"
            ),
        );
        if kind == "openai-codex" {
            // The ChatGPT workspace the sign-in named goes on every request.
            let chatgpt_account = row
                .as_ref()
                .and_then(|row| row.identity.get("chatgpt_account_id"))
                .and_then(Value::as_str)
                .filter(|id| {
                    (1..=64).contains(&id.len())
                        && id
                            .chars()
                            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'))
                })
                .map(str::to_owned);
            client = client.with_codex(chatgpt_account);
        }
        if kind == "google-antigravity"
            && let Some(row) = row
        {
            let known = row
                .identity
                .get("project_id")
                .and_then(Value::as_str)
                .filter(|project| {
                    crate::cloud_code::validate_project_id("google-antigravity", project).is_ok()
                })
                .map(str::to_owned);
            let checked = row
                .identity
                .get("project_checked_at")
                .and_then(Value::as_i64);
            client = client.with_project_discovery(
                known,
                checked,
                Arc::new(AccountProjectMemo {
                    accounts: accounts.clone(),
                    user_id: owner.user_id.to_owned(),
                    provider: owner.provider.to_owned(),
                    account_id: owner.account_id.to_owned(),
                }),
            );
        }
    }
    Ok(client)
}

/// Writes a discovered Cloud Code project (and the fact that discovery ran)
/// into the member account's own identity.
struct AccountProjectMemo {
    accounts: Arc<Accounts>,
    user_id: String,
    provider: String,
    account_id: String,
}

impl crate::llm::ProjectMemo for AccountProjectMemo {
    fn record(&self, discovery: &crate::cloud_code::Discovery) {
        let mut patch = serde_json::json!({
            "project_checked_at": chrono::Utc::now().timestamp(),
        });
        if let Some(project) = &discovery.project {
            patch["project_id"] = serde_json::json!(project);
        }
        if let Some(tier) = &discovery.tier {
            patch["tier"] = serde_json::json!(tier);
        }
        if let Err(error) = self.accounts.store().merge_provider_account_identity(
            &self.user_id,
            &self.provider,
            &self.account_id,
            &patch,
        ) {
            tracing::warn!(%error, "could not remember a discovered Cloud Code project");
        }
    }
}

/// Copy the operator's existing age-store provider credentials into the
/// admin's per-user rows, exactly once.
///
/// Lossless: both the API key/access token and the refresh token are carried
/// over verbatim. Idempotent: a provider the admin already has a row for is
/// never overwritten, and a `kv` marker per provider makes the import
/// once-only so a later delete is not undone by a restart. Providers with no
/// age-store value are left unmarked, so a value set later with
/// `serabot secrets set` is still imported on the next start. Nothing secret
/// is logged; the return value is the number of rows imported.
pub fn import_age_provider_credentials(
    config: &Config,
    secrets: &SharedSecrets,
    users: &UserSecrets,
    store: &Store,
    admin_id: &str,
) -> Result<usize> {
    let default = config.llm.default_provider();
    let mut imported = 0;
    for (id, profile) in std::iter::once(("default", &default)).chain(
        config
            .llm
            .providers
            .iter()
            .map(|(id, profile)| (id.as_str(), profile)),
    ) {
        let marker = format!("provider_imported:{admin_id}:{id}");
        if store.kv_get(&marker)?.is_some() {
            continue;
        }
        let api_key = secrets
            .get(profile.api_key_secret.trim())
            .unwrap_or_default();
        let refresh_token = if profile.refresh_token_secret.trim().is_empty() {
            None
        } else {
            secrets.get(profile.refresh_token_secret.trim())
        };
        if api_key.is_empty() && refresh_token.is_none() {
            // Nothing to import yet; leave the marker off.
            continue;
        }
        if users.credential_status(admin_id, id)?.own {
            // The admin already has a row: never overwrite, just mark it.
            store.kv_set(&marker, "1")?;
            continue;
        }
        let credential = ProviderCredential {
            api_key,
            refresh_token: refresh_token.unwrap_or_default(),
            ..Default::default()
        };
        if credential.api_key.is_empty() {
            // The refresh flow can mint an access token, but the client needs
            // one to start: report it without any secret and let routing fail
            // with an explicit error until the operator re-runs the login.
            tracing::warn!(
                provider = id,
                "imported provider credential has no access token; sign in again before routing to it"
            );
        }
        users.set(admin_id, id, &credential.encode()?)?;
        store.kv_set(&marker, "1")?;
        imported += 1;
    }
    Ok(imported)
}

/// Bring an operator's *new* age-store sign-in (after `serabot providers login`
/// or `serabot secrets set`) into the admin's accounts.
///
/// [`import_age_provider_credentials`] runs once per provider, so a later
/// sign-in used to be ignored. Here the age store's refresh token is
/// fingerprinted (a keyed hash, never the token) and remembered per provider:
/// the first run only records it, because the once-only import already copied
/// that sign-in; when it later differs, the new sign-in is added as another
/// admin account beside the old ones, with the provider's configured project in
/// its identity. Nothing is overwritten or removed. Returns the accounts added.
pub fn import_age_oauth_sign_ins(
    config: &Config,
    secrets: &SharedSecrets,
    accounts: &Accounts,
    store: &Store,
    admin_id: &str,
) -> Result<usize> {
    let default = config.llm.default_provider();
    let mut added = 0;
    for (id, profile) in std::iter::once(("default", &default)).chain(
        config
            .llm
            .providers
            .iter()
            .map(|(id, profile)| (id.as_str(), profile)),
    ) {
        if profile.refresh_token_secret.trim().is_empty()
            || canonical_kind(&profile.kind).is_ok_and(|kind| kind != "google-antigravity")
        {
            continue;
        }
        let (Some(refresh), Some(access)) = (
            secrets.get(profile.refresh_token_secret.trim()),
            secrets.get(profile.api_key_secret.trim()),
        ) else {
            continue;
        };
        if refresh.is_empty() || access.is_empty() {
            continue;
        }
        let fingerprint = accounts.key_fingerprint(admin_id, id, &refresh);
        let marker = format!("provider_signin_seen:{admin_id}:{id}");
        match store.kv_get(&marker)? {
            Some(seen) if seen == fingerprint => continue,
            // First sight: the once-only import already holds this sign-in.
            None => {
                store.kv_set(&marker, &fingerprint)?;
                continue;
            }
            Some(_) => {}
        }
        let identity = if profile.project_id.trim().is_empty() {
            serde_json::json!({})
        } else {
            serde_json::json!({"project_id": profile.project_id.trim()})
        };
        accounts.add(
            admin_id,
            crate::accounts::AddAccount {
                provider: id.to_owned(),
                label: "Imported sign-in".into(),
                auth_kind: crate::accounts::AuthKind::Oauth,
                fingerprint: fingerprint.clone(),
                identity,
                credential: ProviderCredential {
                    api_key: access,
                    refresh_token: refresh,
                    ..Default::default()
                },
                priority: 0,
            },
        )?;
        store.kv_set(&marker, &fingerprint)?;
        added += 1;
    }
    Ok(added)
}

#[cfg(test)]
mod account_clients_tests;

#[cfg(test)]
mod wire_clients_tests;

#[cfg(test)]
mod tests {
    use super::*;
    use secrecy::ExposeSecret;
    use serde_json::json;
    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[test]
    fn builtins_cover_requested_providers_and_reject_cloud_code_chat() {
        let kinds: Vec<_> = builtins().iter().map(|item| item.kind).collect();
        for kind in [
            "mistral",
            "deepseek",
            "minimax",
            "muse-code",
            "openai",
            "groq",
            "openrouter",
            "google-antigravity",
            "gemini",
            "perplexity",
            "huggingface",
            "nebius",
            "sambanova",
        ] {
            assert!(kinds.contains(&kind), "{kind}");
        }
        assert_eq!(
            resolve_base_url("deepseek", "").unwrap(),
            "https://api.deepseek.com/v1"
        );
        assert_eq!(
            resolve_base_url("minimax", "").unwrap(),
            "https://api.minimax.io/v1"
        );
        assert_eq!(
            resolve_base_url("google-antigravity", "").unwrap(),
            "https://daily-cloudcode-pa.googleapis.com"
        );
        assert!(
            resolve_base_url("google-antigravity", "https://cloudcode-pa.googleapis.com").is_ok()
        );
        assert!(resolve_base_url("google-antigravity", "https://evil.example").is_err());
        assert!(
            resolve_base_url(
                "openai-compatible",
                "https://daily-cloudcode-pa.sandbox.googleapis.com"
            )
            .is_err()
        );
        assert!(canonical_kind("nope").is_err());
        assert!(
            enforce_oauth_target(OAuthStyle::MuseMint, "https://evil.example/muse-code/key")
                .is_err()
        );
    }

    #[test]
    fn new_provider_kinds_resolve_documented_bases_and_catalog_urls() {
        for (kind, base, catalog, strict) in [
            (
                "gemini",
                "https://generativelanguage.googleapis.com/v1beta/openai",
                "https://generativelanguage.googleapis.com/v1beta/openai/models",
                false,
            ),
            (
                "huggingface",
                "https://router.huggingface.co/v1",
                "https://router.huggingface.co/v1/models",
                false,
            ),
            (
                "nebius",
                "https://api.tokenfactory.nebius.com/v1",
                "https://api.tokenfactory.nebius.com/v1/models",
                false,
            ),
            (
                "sambanova",
                "https://api.sambanova.ai/v1",
                "https://api.sambanova.ai/v1/models",
                false,
            ),
            (
                "perplexity",
                "https://api.perplexity.ai",
                "https://api.perplexity.ai/v1/models",
                false,
            ),
        ] {
            assert_eq!(resolve_base_url(kind, "").unwrap(), base, "{kind}");
            let resolved = resolve_catalog(kind, base).unwrap();
            assert_eq!(resolved.url, catalog, "{kind}");
            assert_eq!(resolved.strict, strict, "{kind}");
            assert!(matches!(auth_plan(kind, "").unwrap(), AuthPlan::ApiKey));
        }
        // Perplexity only contacts its own host for the list.
        assert!(resolve_catalog("perplexity", "https://evil.example").is_err());
        // An Ollama-style server is a custom OpenAI-compatible endpoint on
        // exact loopback; any other plain-http host is refused.
        assert!(resolve_base_url("openai-compatible", "http://127.0.0.1:11434/v1").is_ok());
        assert!(resolve_base_url("openai-compatible", "http://192.168.1.5:11434/v1").is_err());
    }

    #[test]
    fn catalog_parser_keeps_ids_and_drops_junk() {
        let ids = parse_model_ids(
            serde_json::to_vec(&json!({
                "data": [
                    {"id": "kept"},
                    {"id": " also-kept"},
                    {"name": "not-an-id"},
                    {"id": "second"}
                ]
            }))
            .unwrap()
            .as_slice(),
        )
        .unwrap();
        assert_eq!(
            ids.into_iter().collect::<Vec<_>>(),
            vec!["kept".to_string(), "second".to_string()]
        );
        assert!(parse_model_ids(b"{}").is_err());
    }

    #[tokio::test]
    async fn muse_login_pins_verification_host_and_mints_without_echoing_secrets() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/device"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "device_code": "device-secret",
                "user_code": "ABCD-EFGH",
                "verification_uri": "https://auth.meta.com/activate?code=ABCD-EFGH",
                "interval": 1,
                "expires_in": 30
            })))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "access_token": "identity-token"
            })))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/muse-code/key"))
            .and(header("authorization", "Bearer identity-token"))
            .and(header("x-api-version", "1.0.0"))
            .and(header("user-agent", "SeraBot/0.1"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "api_key": "minted-key"
            })))
            .expect(1)
            .mount(&server)
            .await;
        let http = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .unwrap();
        let endpoints = MuseEndpoints {
            authorization: validate_base_url(&format!("{}/device", server.uri())).unwrap(),
            token: validate_base_url(&format!("{}/token", server.uri())).unwrap(),
            mint: validate_base_url(&format!("{}/muse-code/key", server.uri())).unwrap(),
        };
        let mut seen = String::new();
        let creds = login_muse_inner(
            &http,
            &endpoints,
            |uri, code| {
                seen = format!("{uri} {code}");
            },
            |_| std::future::ready(()),
        )
        .await
        .unwrap();
        assert!(seen.starts_with("https://auth.meta.com/"));
        assert!(seen.contains("ABCD-EFGH"));
        assert!(!seen.contains("device-secret"));
        assert_eq!(creds.api_key.expose_secret(), "minted-key");
        assert_eq!(creds.identity_token.expose_secret(), "identity-token");
    }

    #[tokio::test]
    async fn muse_login_rejects_unpinned_verification_urls() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/device"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "device_code": "device-secret",
                "user_code": "ABCD",
                "verification_uri": "https://evil.example/phish",
                "interval": 1,
                "expires_in": 30
            })))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/token"))
            .respond_with(ResponseTemplate::new(500))
            .expect(0)
            .mount(&server)
            .await;
        let http = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .unwrap();
        let endpoints = MuseEndpoints {
            authorization: validate_base_url(&format!("{}/device", server.uri())).unwrap(),
            token: validate_base_url(&format!("{}/token", server.uri())).unwrap(),
            mint: validate_base_url(&format!("{}/muse-code/key", server.uri())).unwrap(),
        };
        let error = login_muse_inner(&http, &endpoints, |_, _| {}, |_| std::future::ready(()))
            .await
            .unwrap_err();
        let message = error.to_string();
        assert!(message.contains("rejected"));
        assert!(!message.contains("device-secret"));
        assert!(!message.contains("evil.example"));
    }

    #[test]
    fn official_catalogs_pin_documented_hosts_and_deepseek_root_path() {
        assert_eq!(
            resolve_catalog("mistral", "https://api.mistral.ai/v1")
                .unwrap()
                .url,
            "https://api.mistral.ai/v1/models"
        );
        assert_eq!(
            resolve_catalog("deepseek", "https://api.deepseek.com/v1")
                .unwrap()
                .url,
            "https://api.deepseek.com/models"
        );
        assert_eq!(
            resolve_catalog("minimax", "https://api.minimax.io/v1")
                .unwrap()
                .url,
            "https://api.minimax.io/v1/models"
        );
        assert_eq!(
            resolve_catalog("minimax-cn", "https://api.minimaxi.com/v1")
                .unwrap()
                .url,
            "https://api.minimaxi.com/v1/models"
        );
        assert_eq!(
            resolve_catalog("muse-code", "https://api.meta.ai/v1")
                .unwrap()
                .url,
            "https://api.meta.ai/v1/models"
        );
        assert_eq!(
            resolve_catalog("google-antigravity", "https://cloudcode-pa.googleapis.com")
                .unwrap()
                .url,
            "https://cloudcode-pa.googleapis.com/v1internal:fetchAvailableModels"
        );
        assert!(resolve_catalog("google-antigravity", "https://evil.example").is_err());
        assert!(check_profile("google-antigravity", "", "", "", "", true, 1800).is_ok());
        let loopback = resolve_catalog("mistral", "http://127.0.0.1:9/v1").unwrap();
        assert!(loopback.strict);
        assert_eq!(loopback.url, "http://127.0.0.1:9/v1/models");
        let generic = resolve_catalog("openai-compatible", "https://api.example.com/v1").unwrap();
        assert!(!generic.strict);
        assert_eq!(generic.url, "https://api.example.com/v1/models");
        let mismatch = resolve_catalog("minimax", "https://evil.example/v1").unwrap_err();
        let message = mismatch.to_string();
        assert!(message.contains("documented host"));
        assert!(!message.contains("evil.example"));
        assert!(
            check_profile(
                "deepseek",
                "https://proxy.example/v1",
                "",
                "",
                "",
                false,
                1800
            )
            .is_ok()
        );
        assert!(
            check_profile(
                "deepseek",
                "https://proxy.example/v1",
                "",
                "",
                "",
                true,
                1800
            )
            .is_err()
        );
        assert!(check_profile("deepseek", "", "", "", "", true, 1800).is_ok());
    }

    #[test]
    fn official_catalog_parser_keeps_chat_ids_and_drops_secrets() {
        let secret = "synthetic-catalog-secret";
        let ids = parse_official_catalog(
            serde_json::to_vec(&json!({
                "object": "list",
                "data": [
                    {"id": "mistral-large-latest", "object": "model", "capabilities": {"completion_chat": true}},
                    {"id": "mistral-embed", "object": "model", "capabilities": {"completion_chat": false}, "description": secret},
                    {"id": "bad/id", "object": "model", "owned_by": secret},
                    {"id": "ft:open-mistral-7b:abc", "object": "model"},
                    {"id": "MiniMax-M3.1-Flash", "object": "model"},
                    {"id": "deepseek-flash", "object": "model", "name": "DeepSeek-V4.1-Flash"}
                ]
            }))
            .unwrap()
            .as_slice(),
        )
        .unwrap();
        assert_eq!(
            ids.into_iter().collect::<Vec<_>>(),
            vec![
                "MiniMax-M3.1-Flash".to_string(),
                "deepseek-flash".to_string(),
                "ft:open-mistral-7b:abc".to_string(),
                "mistral-large-latest".to_string(),
            ]
        );
        let rejected = parse_official_catalog(
            serde_json::to_vec(&json!({
                "object": "model",
                "id": secret,
                "data": [{"id": "kept"}]
            }))
            .unwrap()
            .as_slice(),
        )
        .unwrap_err();
        assert!(!rejected.to_string().contains(secret));
        assert!(parse_official_catalog(br#"{"models":[{"id":"kept"}]}"#).is_err());
    }

    // ---- Per-user credential import (multi-user phase 4) ----

    fn import_fixture() -> (Config, SharedSecrets, Store, UserSecrets, String) {
        let mut config = Config::default();
        config.llm.api_key_secret = "llm_api_key".into();
        config.llm.providers.insert(
            "antigravity".into(),
            ProviderConfig {
                base_url: String::new(),
                api_key_secret: "antigravity_access_token".into(),
                models: vec!["gemini-3-pro".into()],
                data_egress_approved: true,
                kind: "google-antigravity".into(),
                auth: "oauth".into(),
                refresh_token_secret: "antigravity_refresh_token".into(),
                oauth_client_secret_secret: "antigravity_oauth_client_secret".into(),
                ..ProviderConfig::default()
            },
        );
        let secrets = SharedSecrets::new(crate::secrets::SecretStore::from_map(
            std::collections::BTreeMap::from([
                ("llm_api_key".to_owned(), "sk-operator".to_owned()),
                (
                    "antigravity_access_token".to_owned(),
                    "access-operator".to_owned(),
                ),
                (
                    "antigravity_refresh_token".to_owned(),
                    "refresh-operator".to_owned(),
                ),
            ]),
        ));
        let store = Store::open_in_memory(384).unwrap();
        let admin = store.admin_user_id().unwrap();
        let users = UserSecrets::with_master_key(store.clone(), [3u8; 32]).unwrap();
        (config, secrets, store, users, admin)
    }

    #[test]
    fn the_age_store_credentials_import_losslessly_and_idempotently() {
        let (config, secrets, store, users, admin) = import_fixture();
        let imported =
            import_age_provider_credentials(&config, &secrets, &users, &store, &admin).unwrap();
        assert_eq!(imported, 2);

        // Both the API key/access token and the refresh token are preserved.
        let default =
            ProviderCredential::parse(&users.get(&admin, "default").unwrap().unwrap()).unwrap();
        assert_eq!(default.api_key, "sk-operator");
        assert!(default.refresh_token.is_empty());
        let antigravity =
            ProviderCredential::parse(&users.get(&admin, "antigravity").unwrap().unwrap()).unwrap();
        assert_eq!(antigravity.api_key, "access-operator");
        assert_eq!(antigravity.refresh_token, "refresh-operator");

        // A second startup imports nothing and changes nothing.
        assert_eq!(
            import_age_provider_credentials(&config, &secrets, &users, &store, &admin).unwrap(),
            0
        );
        assert_eq!(
            ProviderCredential::parse(&users.get(&admin, "antigravity").unwrap().unwrap())
                .unwrap()
                .refresh_token,
            "refresh-operator"
        );
    }

    #[test]
    fn an_existing_admin_credential_is_never_overwritten_and_a_delete_sticks() {
        let (config, secrets, store, users, admin) = import_fixture();
        let existing = ProviderCredential {
            api_key: "sk-console".into(),
            refresh_token: String::new(),
            ..Default::default()
        };
        users
            .set(&admin, "default", &existing.encode().unwrap())
            .unwrap();
        let imported =
            import_age_provider_credentials(&config, &secrets, &users, &store, &admin).unwrap();
        assert_eq!(imported, 1, "only the provider without a row is imported");
        assert_eq!(
            ProviderCredential::parse(&users.get(&admin, "default").unwrap().unwrap())
                .unwrap()
                .api_key,
            "sk-console"
        );

        // Deleting an imported credential is not undone by a restart.
        users.delete(&admin, "antigravity").unwrap();
        assert_eq!(
            import_age_provider_credentials(&config, &secrets, &users, &store, &admin).unwrap(),
            0
        );
        assert!(users.get(&admin, "antigravity").unwrap().is_none());
    }

    #[test]
    fn a_provider_with_no_age_value_stays_unmarked_until_one_appears() {
        let mut config = Config::default();
        config.llm.api_key_secret = "llm_api_key".into();
        let store = Store::open_in_memory(384).unwrap();
        let admin = store.admin_user_id().unwrap();
        let users = UserSecrets::with_master_key(store.clone(), [3u8; 32]).unwrap();
        let empty = SharedSecrets::new(crate::secrets::SecretStore::from_map(
            std::collections::BTreeMap::new(),
        ));
        assert_eq!(
            import_age_provider_credentials(&config, &empty, &users, &store, &admin).unwrap(),
            0
        );
        // A value set later with `serabot secrets set` is picked up.
        let later = SharedSecrets::new(crate::secrets::SecretStore::from_map(
            std::collections::BTreeMap::from([("llm_api_key".to_owned(), "sk-later".to_owned())]),
        ));
        assert_eq!(
            import_age_provider_credentials(&config, &later, &users, &store, &admin).unwrap(),
            1
        );
        assert_eq!(
            ProviderCredential::parse(&users.get(&admin, "default").unwrap().unwrap())
                .unwrap()
                .api_key,
            "sk-later"
        );
    }

    #[test]
    fn a_credential_is_built_from_the_users_own_row_and_never_falls_back() {
        let (config, secrets, store, users, admin) = import_fixture();
        import_age_provider_credentials(&config, &secrets, &users, &store, &admin).unwrap();
        let member = store
            .create_user("microsoft:member", "member@example.example", "Member")
            .unwrap()
            .id;
        let credentials = UserProviderCredentials::new(
            Arc::new(UserSecrets::with_master_key(store.clone(), [3u8; 32]).unwrap()),
            secrets.clone(),
            5,
            100,
        );
        let profile = config.llm.default_provider();
        // The admin resolves their own imported key.
        assert!(credentials.available(&admin, "default"));
        let client = credentials.client(&admin, "default", &profile).unwrap();
        assert_eq!(client.base_url(), "https://api.mistral.ai/v1");
        // The member has no credential and no grant: no client, explicit error.
        assert!(!credentials.available(&member, "default"));
        let error = credentials
            .client(&member, "default", &profile)
            .unwrap_err();
        assert!(error.to_string().contains("no credential"));

        // A grant makes the admin's credential usable by the member without
        // copying it.
        users.grant(&admin, &member, "default").unwrap();
        assert!(credentials.available(&member, "default"));
        assert!(credentials.client(&member, "default", &profile).is_ok());
    }

    /// A chat endpoint that answers `text` to exactly one bearer key.
    async fn answer_for_key(server: &wiremock::MockServer, key: &str, text: &str) {
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .and(header("authorization", format!("Bearer {key}").as_str()))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "choices": [{"message": {"role": "assistant", "content": text}, "finish_reason": "stop"}],
                "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2}
            })))
            .mount(server)
            .await;
    }

    async fn who_answers(client: &LlmClient) -> String {
        client
            .clone()
            .with_model("synthetic")
            .complete(&[crate::domain::LlmMessage::user("hi")], &[], None, None)
            .await
            .unwrap()
            .message
            .content
            .unwrap()
    }

    #[tokio::test]
    async fn accounts_make_a_provider_usable_and_pick_the_best_healthy_one() {
        let store = Store::open_in_memory(384).unwrap();
        let users = Arc::new(UserSecrets::with_master_key(store.clone(), [3u8; 32]).unwrap());
        let credentials = UserProviderCredentials::new(
            users.clone(),
            SharedSecrets::new(crate::secrets::SecretStore::from_map(Default::default())),
            5,
            64,
        );
        let accounts = credentials.refresher().accounts().clone();
        let alice = store
            .create_user("microsoft:pick-a", "a@example.example", "Alice")
            .unwrap()
            .id;
        let bob = store
            .create_user("microsoft:pick-b", "b@example.example", "Bob")
            .unwrap()
            .id;
        let server = wiremock::MockServer::start().await;
        for (key, text) in [
            ("key-low", "low"),
            ("key-high", "high"),
            ("key-broken", "broken"),
            ("key-default", "default"),
        ] {
            answer_for_key(&server, key, text).await;
        }
        let profile = ProviderConfig {
            base_url: server.uri(),
            api_key_secret: "acme".into(),
            models: vec!["synthetic".into()],
            ..Default::default()
        };

        // Nothing yet: not usable, and no client.
        assert!(!credentials.available(&alice, "acme"));
        assert!(credentials.client(&alice, "acme", &profile).is_err());

        let low = accounts
            .add_api_key(&alice, "acme", "Low", "key-low")
            .unwrap();
        let high = accounts
            .add_api_key(&alice, "acme", "High", "key-high")
            .unwrap();
        let broken = accounts
            .add_api_key(&alice, "acme", "Broken", "key-broken")
            .unwrap();
        use crate::accounts::AccountPatch;
        accounts
            .update(
                &alice,
                "acme",
                &high.account_id,
                &AccountPatch {
                    priority: Some(5),
                    ..Default::default()
                },
            )
            .unwrap();
        accounts
            .update(
                &alice,
                "acme",
                &broken.account_id,
                &AccountPatch {
                    priority: Some(99),
                    ..Default::default()
                },
            )
            .unwrap();
        accounts
            .set_status(
                &alice,
                "acme",
                &broken.account_id,
                crate::accounts::AccountStatus::NeedsReauth,
                Some("rejected"),
                None,
            )
            .unwrap();
        // Accounts alone make the provider available, for this user only.
        assert!(credentials.available(&alice, "acme"));
        assert!(!credentials.available(&bob, "acme"));
        // The best healthy account answers, whatever its row order.
        let client = credentials.client(&alice, "acme", &profile).unwrap();
        assert_eq!(who_answers(&client).await, "high");
        // A named account is used even when it is not the best.
        let client = credentials
            .client_for_account(&alice, "acme", &profile, &low.account_id)
            .unwrap();
        assert_eq!(who_answers(&client).await, "low");
        // Another member cannot name Alice's account.
        assert!(
            credentials
                .client_for_account(&bob, "acme", &profile, &low.account_id)
                .is_err()
        );
        // When the best one fails over to needs_reauth the next one is used.
        accounts
            .set_status(
                &alice,
                "acme",
                &high.account_id,
                crate::accounts::AccountStatus::NeedsReauth,
                None,
                None,
            )
            .unwrap();
        let client = credentials.client(&alice, "acme", &profile).unwrap();
        assert_eq!(who_answers(&client).await, "low");
        // A disabled account is never picked, even when it is all that is left.
        accounts
            .update(
                &alice,
                "acme",
                &low.account_id,
                &AccountPatch {
                    disabled: Some(true),
                    ..Default::default()
                },
            )
            .unwrap();
        let client = credentials.client(&alice, "acme", &profile).unwrap();
        assert_ne!(who_answers(&client).await, "low");

        // The pre-accounts credential competes at priority 0.
        users
            .set(
                &bob,
                "acme",
                &ProviderCredential {
                    api_key: "key-default".into(),
                    ..Default::default()
                }
                .encode()
                .unwrap(),
            )
            .unwrap();
        assert!(credentials.available(&bob, "acme"));
        let client = credentials.client(&bob, "acme", &profile).unwrap();
        assert_eq!(who_answers(&client).await, "default");
    }

    fn catalog_of(value: serde_json::Value, strict: bool) -> ModelCatalog {
        parse_model_catalog(&serde_json::to_vec(&value).unwrap(), strict).unwrap()
    }

    #[test]
    fn mistral_catalog_keeps_chat_models_with_context_and_capability_tags() {
        let catalog = catalog_of(
            json!({"object": "list", "data": [
                {"id": "mistral-large-latest", "object": "model", "name": "mistral-large-latest",
                 "max_context_length": 131072,
                 "capabilities": {"completion_chat": true, "function_calling": true, "vision": true}},
                {"id": "mistral-embed", "object": "model",
                 "capabilities": {"completion_chat": false}},
                {"id": "magistral-medium-latest", "object": "model", "name": "Magistral Medium",
                 "max_context_length": 40000,
                 "capabilities": {"completion_chat": true, "function_calling": false, "vision": false}}
            ]}),
            true,
        );
        assert_eq!(
            catalog.keys().map(String::as_str).collect::<Vec<_>>(),
            vec!["magistral-medium-latest", "mistral-large-latest"]
        );
        let large = &catalog["mistral-large-latest"];
        assert_eq!(large.name, None, "a name equal to the id is not a name");
        assert_eq!(large.context_length, Some(131_072));
        assert_eq!(large.tags, vec!["vision", "tools"]);
        let magistral = &catalog["magistral-medium-latest"];
        assert_eq!(magistral.name.as_deref(), Some("Magistral Medium"));
        assert!(magistral.tags.is_empty());
    }

    #[test]
    fn openrouter_catalog_reads_modalities_parameters_and_pricing() {
        let catalog = catalog_of(
            json!({"data": [
                {"id": "vendor/cheap-vision", "name": "Vendor: Cheap Vision", "context_length": 200000,
                 "architecture": {"input_modalities": ["text", "image"], "output_modalities": ["text"]},
                 "supported_parameters": ["tools", "reasoning", "temperature"],
                 "pricing": {"prompt": "0.0000001", "completion": "0.0000004"}},
                {"id": "vendor/free:free", "name": "Free One", "context_length": 8192,
                 "architecture": {"input_modalities": ["text"], "output_modalities": ["text"]},
                 "pricing": {"prompt": "0", "completion": "0"}},
                {"id": "vendor/image-only", "name": "Painter",
                 "architecture": {"input_modalities": ["text"], "output_modalities": ["image"]}}
            ]}),
            false,
        );
        assert_eq!(catalog.len(), 2);
        let cheap = &catalog["vendor/cheap-vision"];
        assert_eq!(cheap.context_length, Some(200_000));
        assert_eq!(cheap.tags, vec!["vision", "tools", "reasoning", "cheap"]);
        assert_eq!(catalog["vendor/free:free"].tags, vec!["free"]);
    }

    #[test]
    fn openai_style_lists_drop_non_chat_ids_and_gemini_prefixes_are_stripped() {
        let openai = catalog_of(
            json!({"object": "list", "data": [
                {"id": "gpt-5", "object": "model"}, {"id": "text-embedding-3-small", "object": "model"},
                {"id": "whisper-1", "object": "model"}, {"id": "omni-moderation-latest", "object": "model"},
                {"id": "gpt-image-1", "object": "model"}, {"id": "gpt-4o-mini", "object": "model"}
            ]}),
            false,
        );
        assert_eq!(
            openai.keys().map(String::as_str).collect::<Vec<_>>(),
            vec!["gpt-4o-mini", "gpt-5"]
        );
        let gemini = catalog_of(
            json!({"data": [
                {"id": "models/gemini-2.5-flash", "object": "model"},
                {"id": "models/text-embedding-004", "object": "model"}
            ]}),
            false,
        );
        assert_eq!(
            gemini.keys().map(String::as_str).collect::<Vec<_>>(),
            vec!["gemini-2.5-flash"]
        );
    }

    #[test]
    fn together_and_fireworks_shapes_use_their_own_type_and_support_flags() {
        let together = catalog_of(
            json!([
                {"id": "meta/chat-70b", "type": "chat", "display_name": "Chat 70B", "context_length": 131072},
                {"id": "meta/embedder", "type": "embedding"},
                {"id": "meta/painter", "type": "image"}
            ]),
            false,
        );
        assert_eq!(together.len(), 1);
        assert_eq!(together["meta/chat-70b"].name.as_deref(), Some("Chat 70B"));
        let fireworks = catalog_of(
            json!({"data": [
                {"id": "accounts/fireworks/models/a", "supports_chat": true, "supports_tools": true,
                 "supports_image_input": true, "context_length": 65536},
                {"id": "accounts/fireworks/models/b", "supports_chat": false}
            ]}),
            false,
        );
        assert_eq!(fireworks.len(), 1);
        assert_eq!(
            fireworks["accounts/fireworks/models/a"].tags,
            vec!["vision", "tools"]
        );
    }

    #[test]
    fn strict_catalogs_fail_closed_and_never_echo_the_body() {
        for body in [
            json!({"models": [{"id": "ok-id"}]}),
            json!({"object": "list", "data": [{"object": "model"}], "note": "synthetic-body-secret"}),
            json!({"object": "list", "data": [{"id": "not/an id"}]}),
        ] {
            let error = parse_model_catalog(&serde_json::to_vec(&body).unwrap(), true)
                .unwrap_err()
                .to_string();
            assert!(!error.contains("synthetic-body-secret"), "{error}");
            assert!(!error.contains("not/an id"), "{error}");
        }
        assert!(parse_model_catalog(b"not json", false).is_err());
        assert!(parse_model_catalog(&vec![b' '; 1_048_577], false).is_err());
    }
}
