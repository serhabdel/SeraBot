//! Native provider registry.
//!
//! Chat stays on OpenAI-compatible `/chat/completions`. OAuth credentials stay in
//! the age secret store. Live catalogs are fetched only after egress consent and
//! are intersected with the owner allowlist.

use std::collections::BTreeSet;
use std::fmt;
use std::future::Future;
use std::time::{Duration, Instant};

use secrecy::SecretString;
use serde_json::Value;

use crate::error::{Result, SeraError};
use crate::llm::validate_base_url;

pub const MUSE_CLIENT_ID: &str = "1031625952748946";
pub const MUSE_API_VERSION: &str = "1.0.0";
pub const ANTIGRAVITY_CLIENT_ID: &str =
    "1071006060591-tmhssin2h21lcre235vtolojh4g403ep.apps.googleusercontent.com";

const MUSE_AUTH_HOST: &str = "auth.meta.com";
const MUSE_API_HOST: &str = "api.meta.ai";
const MUSE_MINT_PATH: &str = "/muse-code/key";
const GOOGLE_TOKEN_HOST: &str = "oauth2.googleapis.com";
const GOOGLE_TOKEN_PATH: &str = "/token";
const USER_AGENT: &str = "SeraBot/0.1";

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
        summary: "Mistral chat; GET /models catalog",
    },
    BuiltinProvider {
        kind: "deepseek",
        base_url: Some("https://api.deepseek.com/v1"),
        auth: "api_key",
        summary: "DeepSeek chat; GET /models catalog",
    },
    BuiltinProvider {
        kind: "minimax",
        base_url: Some("https://api.minimax.io/v1"),
        auth: "api_key",
        summary: "MiniMax global OpenAI-compatible chat",
    },
    BuiltinProvider {
        kind: "minimax-cn",
        base_url: Some("https://api.minimaxi.com/v1"),
        auth: "api_key",
        summary: "MiniMax China OpenAI-compatible chat",
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
        kind: "muse-code",
        base_url: Some("https://api.meta.ai/v1"),
        auth: "oauth",
        summary: "Muse Code device login, minted key, GET /models",
    },
    BuiltinProvider {
        kind: "google-antigravity",
        base_url: None,
        auth: "oauth",
        summary: "Google OAuth refresh; chat base must be OpenAI-compatible",
    },
];

pub fn builtins() -> &'static [BuiltinProvider] {
    BUILTINS
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OAuthStyle {
    MuseMint,
    RefreshToken,
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
        if matches!(kind.as_str(), "muse-code" | "google-antigravity") {
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
            _ => Err(SeraError::Config(
                "oauth auth is only supported for muse-code and google-antigravity".into(),
            )),
        },
        _ => Err(SeraError::Config(
            "auth must be api_key or oauth".into(),
        )),
    }
}

pub fn check_profile(
    kind: &str,
    base_url: &str,
    auth: &str,
    refresh_token_secret: &str,
    oauth_client_secret_secret: &str,
    catalog_fetch: bool,
    catalog_ttl_secs: u64,
) -> Result<()> {
    let _ = resolve_base_url(kind, base_url)?;
    match auth_plan(kind, auth)? {
        AuthPlan::ApiKey => {
            if !refresh_token_secret.trim().is_empty() {
                return Err(SeraError::Config(
                    "refresh_token_secret requires auth = \"oauth\"".into(),
                ));
            }
        }
        AuthPlan::OAuth { style, .. } => {
            if !refresh_token_secret.trim().is_empty()
                && style == OAuthStyle::RefreshToken
                && oauth_client_secret_secret.trim().is_empty()
            {
                return Err(SeraError::Config(
                    "oauth_client_secret_secret is required when refreshing Google tokens".into(),
                ));
            }
        }
    }
    if catalog_fetch && !(1..=86_400).contains(&catalog_ttl_secs) {
        return Err(SeraError::Config(
            "catalog_ttl_secs must be between 1 and 86400 when catalog_fetch is true".into(),
        ));
    }
    Ok(())
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

pub fn parse_model_ids(bytes: &[u8]) -> Result<BTreeSet<String>> {
    if bytes.len() > 1_048_576 {
        return Err(SeraError::External("model catalog is too large".into()));
    }
    let value: Value = serde_json::from_slice(bytes)
        .map_err(|_| SeraError::External("malformed model catalog".into()))?;
    let entries = value
        .get("data")
        .and_then(Value::as_array)
        .or_else(|| value.get("models").and_then(Value::as_array))
        .or_else(|| value.as_array())
        .ok_or_else(|| SeraError::External("model catalog has no model list".into()))?;
    if entries.len() > 4_000 {
        return Err(SeraError::External("model catalog is too large".into()));
    }
    let mut ids = BTreeSet::new();
    for entry in entries {
        let Some(id) = entry.get("id").and_then(Value::as_str) else {
            continue;
        };
        if valid_model_id(id) {
            ids.insert(id.to_owned());
        }
    }
    if ids.is_empty() {
        return Err(SeraError::External(
            "model catalog contained no usable model ids".into(),
        ));
    }
    Ok(ids)
}

pub async fn muse_mint_api_key(
    http: &reqwest::Client,
    token_url: &str,
    identity_token: &str,
) -> Result<String> {
    let response = http
        .post(token_url)
        .header("user-agent", USER_AGENT)
        .header("accept", "application/json")
        .header("x-api-version", MUSE_API_VERSION)
        .bearer_auth(identity_token)
        .json(&serde_json::json!({}))
        .send()
        .await
        .map_err(|_| SeraError::External("Muse API-key mint failed".into()))?;
    let status = response.status();
    let bytes = response
        .bytes()
        .await
        .map_err(|_| SeraError::External("Muse API-key mint failed".into()))?;
    if bytes.len() > 65_536 || !status.is_success() {
        return Err(SeraError::External(format!(
            "Muse API-key mint failed with status {}",
            status.as_u16()
        )));
    }
    let value: Value = serde_json::from_slice(&bytes)
        .map_err(|_| SeraError::External("Muse API-key mint failed".into()))?;
    let key = value
        .get("api_key")
        .and_then(Value::as_str)
        .filter(|key| credential_ok(key))
        .ok_or_else(|| SeraError::External("Muse API-key mint failed".into()))?;
    Ok(key.to_owned())
}

pub async fn refresh_token_grant(
    http: &reqwest::Client,
    token_url: &str,
    refresh_token: &str,
    client_id: &str,
    client_secret: &str,
) -> Result<String> {
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
        .map_err(|_| SeraError::External("oauth token refresh failed".into()))?;
    let status = response.status();
    let bytes = response
        .bytes()
        .await
        .map_err(|_| SeraError::External("oauth token refresh failed".into()))?;
    if bytes.len() > 65_536 || !status.is_success() {
        return Err(SeraError::External(format!(
            "oauth token refresh failed with status {}",
            status.as_u16()
        )));
    }
    let value: Value = serde_json::from_slice(&bytes)
        .map_err(|_| SeraError::External("oauth token refresh failed".into()))?;
    let token = value
        .get("access_token")
        .and_then(Value::as_str)
        .filter(|token| credential_ok(token))
        .ok_or_else(|| SeraError::External("oauth token refresh failed".into()))?;
    Ok(token.to_owned())
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
                (
                    "grant_type",
                    "urn:ietf:params:oauth:grant-type:device_code",
                ),
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
    let api_key = muse_mint_api_key(http, &endpoints.mint, &identity).await?;
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
        return Err(SeraError::External("Muse login response is too large".into()));
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
        assert!(
            resolve_base_url(
                "google-antigravity",
                "https://cloudcode-pa.googleapis.com"
            )
            .is_err()
        );
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
}
