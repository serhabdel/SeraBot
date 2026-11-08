//! ElevenLabs OAuth sign-in plumbing shared by the hosted-MCP connector
//! (`elevenlabs_mcp`), the config defaults and the public web routes.
//!
//! The API-key connector (`elevenlabs_key_*` native tools) was removed in
//! 0.28.0; the account sign-in through the hosted MCP server is the only
//! ElevenLabs connector. This module carries only the CIMD public-client
//! metadata and the pinned endpoint constants — no API-key handling.

use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;
use secrecy::{ExposeSecret, SecretString};
use serde_json::{Value, json};

use crate::domain::Language;
use crate::error::{Result, SeraError};
use crate::maker::{ArtifactMeta, ArtifactStore};
use crate::policy::Policy;
use crate::secrets::CredentialWriter;

/// Largest OAuth token response body accepted (64 KiB).
const MAX_TOKEN_RESPONSE_BYTES: usize = 64 * 1024;
/// Largest media file the MCP connector downloads in one call (100 MiB).
pub(crate) const MAX_FLOW_DOWNLOAD_BYTES: usize = 100 * 1024 * 1024;

pub const ELEVENLABS_AUTHORIZE_URL: &str = "https://elevenlabs.io/app/oauth/authorize";
/// ElevenLabs hosted MCP server (Streamable HTTP; OAuth bearer, no API key).
/// The protected-resource document names the `api.us.elevenlabs.io` server as
/// the resource, so OAuth tokens are minted for — and spent against — this
/// endpoint first (the public host is the fallback).
pub const ELEVENLABS_MCP_URL: &str = "https://api.us.elevenlabs.io/v1/mcp";
/// Public fallback endpoint, tried when the regional one is unreachable.
pub const ELEVENLABS_MCP_URL_PUBLIC: &str = "https://api.elevenlabs.io/v1/mcp";
/// RFC 8707 `resource` sent on authorize, token and refresh requests: the
/// MCP server the token is for. Without it the token is not audience-scoped
/// for the MCP server and every MCP call is rejected.
pub const ELEVENLABS_MCP_RESOURCE: &str = ELEVENLABS_MCP_URL;
pub const ELEVENLABS_TOKEN_URL: &str = "https://api.us.elevenlabs.io/v1/oauth/token";
/// Public host that serves this server's client metadata document.
pub const PUBLIC_HOST: &str = "bot.seradyx.tech";
pub const CIMD_PATH: &str = "/.well-known/serabot-oauth-client.json";
/// Fixed loopback port so the CIMD document can declare exactly one redirect.
pub const OAUTH_CALLBACK_PORT: u16 = 51122;
pub const OAUTH_CALLBACK_PATH: &str = "/oauth-callback";
/// Product-scoped scopes covering the connector's tools: every scope the
/// authorization server advertises for this resource.
pub const DEFAULT_OAUTH_SCOPE: &str = "convai_read convai_write text_to_speech speech_history_read flows image_video_generation voice_generation";

/// The CIMD client_id: the https URL of the metadata document served publicly.
pub fn cimd_client_id() -> String {
    format!("https://{PUBLIC_HOST}{CIMD_PATH}")
}

pub fn oauth_callback_uri() -> String {
    format!("http://127.0.0.1:{OAUTH_CALLBACK_PORT}{OAUTH_CALLBACK_PATH}")
}

/// RFC 7591-style client metadata document fetched by the authorization
/// server when `client_id` is this document's URL (CIMD). Contains no secrets.
pub fn cimd_document() -> Value {
    json!({
        "client_id": cimd_client_id(),
        "client_name": "SeraBot",
        "client_uri": format!("https://{PUBLIC_HOST}/"),
        "redirect_uris": [oauth_callback_uri()],
        "grant_types": ["authorization_code", "refresh_token"],
        "response_types": ["code"],
        "token_endpoint_auth_method": "none",
        "subject_type": "public",
        "scope": DEFAULT_OAUTH_SCOPE,
    })
}

fn pin_oauth_endpoint(candidate: &str, official: &str) -> Result<reqwest::Url> {
    let value = candidate.trim().trim_end_matches('/');
    let official = official.trim_end_matches('/');
    if value == official {
        return reqwest::Url::parse(value)
            .map_err(|_| SeraError::Config("invalid OAuth endpoint".into()));
    }
    // Tests may point at a local mock authorization server.
    if cfg!(test) {
        let url = reqwest::Url::parse(value)
            .map_err(|_| SeraError::Config("invalid OAuth endpoint".into()))?;
        let host = url.host_str().unwrap_or("");
        let loopback = url.scheme() == "http"
            && (host == "127.0.0.1" || host.eq_ignore_ascii_case("localhost"))
            && url.port().is_some()
            && url.username().is_empty()
            && url.password().is_none()
            && url.query().is_none()
            && url.fragment().is_none();
        if loopback {
            return Ok(url);
        }
    }
    Err(SeraError::Config(
        "ElevenLabs OAuth endpoint must be the documented https URL".into(),
    ))
}

fn pkce_token_ok(value: &str) -> bool {
    (43..=128).contains(&value.len())
        && value.bytes().all(|byte| {
            byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_' || byte == b'~'
        })
}

fn state_ok(value: &str) -> bool {
    !value.is_empty() && value.len() <= 256 && value.bytes().all(|byte| byte.is_ascii_graphic())
}

/// The loopback redirect is fixed by the CIMD document, so only the exact
/// callback URI is accepted.
fn validate_callback_redirect(uri: &str) -> Result<()> {
    if uri == oauth_callback_uri() {
        return Ok(());
    }
    Err(SeraError::Validation(
        "ElevenLabs OAuth redirect_uri must be the fixed loopback callback".into(),
    ))
}

/// Build the ElevenLabs consent URL. `client_id` is the CIMD URL of our
/// public client metadata document.
pub fn authorize_url(
    client_id: &str,
    redirect_uri: &str,
    scope: &str,
    challenge: &str,
    state: &str,
) -> Result<String> {
    let id = reqwest::Url::parse(client_id.trim()).map_err(|_| {
        SeraError::Config("elevenlabs.oauth_client_id must be a valid https URL".into())
    })?;
    if id.scheme() != "https"
        || id.host_str().is_none()
        || !id.username().is_empty()
        || id.password().is_some()
    {
        return Err(SeraError::Config(
            "elevenlabs.oauth_client_id must be a credential-less https URL".into(),
        ));
    }
    validate_callback_redirect(redirect_uri)?;
    if !pkce_token_ok(challenge) || !state_ok(state) {
        return Err(SeraError::Validation(
            "invalid PKCE challenge or state".into(),
        ));
    }
    let mut url = pin_oauth_endpoint(ELEVENLABS_AUTHORIZE_URL, ELEVENLABS_AUTHORIZE_URL)?;
    {
        let mut query = url.query_pairs_mut();
        query.append_pair("client_id", id.as_str());
        query.append_pair("response_type", "code");
        query.append_pair("redirect_uri", redirect_uri);
        query.append_pair("scope", scope.trim());
        query.append_pair("code_challenge", challenge);
        query.append_pair("code_challenge_method", "S256");
        query.append_pair("state", state);
        // RFC 8707: bind the token to the MCP server resource.
        query.append_pair("resource", ELEVENLABS_MCP_RESOURCE);
    }
    // Defense in depth: the final URL must stay on the documented authorize
    // endpoint (host/path; the query is the PKCE material we just added).
    let host = url.host_str().unwrap_or("");
    let pinned =
        url.scheme() == "https" && host == "elevenlabs.io" && url.path() == "/app/oauth/authorize";
    #[cfg(test)]
    let loopback =
        url.scheme() == "http" && (host == "127.0.0.1" || host.eq_ignore_ascii_case("localhost"));
    #[cfg(not(test))]
    let loopback = false;
    if !pinned && !loopback {
        return Err(SeraError::Config(
            "ElevenLabs authorize URL left the pinned endpoint".into(),
        ));
    }
    Ok(url.to_string())
}

#[derive(Debug, Clone)]
pub struct TokenPair {
    pub access_token: String,
    pub refresh_token: Option<String>,
    pub expires_in: Option<u64>,
}

/// Exchange an authorization code at the pinned token endpoint
/// (public client, `token_endpoint_auth_method: none`).
pub async fn exchange_code(
    http: &reqwest::Client,
    client_id: &str,
    code: &str,
    redirect_uri: &str,
    verifier: &str,
) -> Result<TokenPair> {
    exchange_code_at(
        http,
        ELEVENLABS_TOKEN_URL,
        client_id,
        code,
        redirect_uri,
        verifier,
    )
    .await
}

/// `exchange_code` against an explicit token URL (loopback allowed in tests).
pub async fn exchange_code_at(
    http: &reqwest::Client,
    token_url: &str,
    client_id: &str,
    code: &str,
    redirect_uri: &str,
    verifier: &str,
) -> Result<TokenPair> {
    validate_callback_redirect(redirect_uri)?;
    if !state_ok(code) {
        return Err(SeraError::Validation("invalid authorization code".into()));
    }
    if !pkce_token_ok(verifier) {
        return Err(SeraError::Validation("invalid PKCE verifier".into()));
    }
    let url = pin_oauth_endpoint(token_url, ELEVENLABS_TOKEN_URL)?;
    token_request(
        http,
        url,
        &[
            ("grant_type", "authorization_code"),
            ("client_id", client_id.trim()),
            ("code", code),
            ("redirect_uri", redirect_uri),
            ("code_verifier", verifier),
            // RFC 8707: the token is for the MCP server, not the REST API.
            ("resource", ELEVENLABS_MCP_RESOURCE),
        ],
    )
    .await
}

/// Refresh grant; returns a new access token and, when the server rotates,
/// a new refresh token.
pub async fn refresh_grant(
    http: &reqwest::Client,
    client_id: &str,
    refresh_token: &str,
) -> Result<TokenPair> {
    refresh_grant_at(http, ELEVENLABS_TOKEN_URL, client_id, refresh_token).await
}

/// `refresh_grant` against an explicit token URL (loopback allowed in tests).
pub async fn refresh_grant_at(
    http: &reqwest::Client,
    token_url: &str,
    client_id: &str,
    refresh_token: &str,
) -> Result<TokenPair> {
    if refresh_token.is_empty() || refresh_token.len() > MAX_TOKEN_RESPONSE_BYTES {
        return Err(SeraError::Validation("invalid refresh token".into()));
    }
    let url = pin_oauth_endpoint(token_url, ELEVENLABS_TOKEN_URL)?;
    token_request(
        http,
        url,
        &[
            ("grant_type", "refresh_token"),
            ("client_id", client_id.trim()),
            ("refresh_token", refresh_token),
            // Keep the refreshed token bound to the same MCP resource.
            ("resource", ELEVENLABS_MCP_RESOURCE),
        ],
    )
    .await
}

async fn token_request(
    http: &reqwest::Client,
    url: reqwest::Url,
    form: &[(&str, &str)],
) -> Result<TokenPair> {
    let response = http.post(url).form(form).send().await.map_err(|error| {
        SeraError::External(format!("ElevenLabs OAuth token request failed: {error}"))
    })?;
    let status = response.status();
    let bytes = response
        .bytes()
        .await
        .map_err(|error| {
            SeraError::External(format!("ElevenLabs OAuth token read failed: {error}"))
        })?
        .to_vec();
    if bytes.len() > MAX_TOKEN_RESPONSE_BYTES {
        return Err(SeraError::External(
            "ElevenLabs OAuth token response exceeded size limit".into(),
        ));
    }
    // The request form carried the code/verifier/refresh token; only the
    // response's short `error` code may surface, never the form or tokens.
    let parsed: Option<Value> = serde_json::from_slice(&bytes).ok();
    if !status.is_success() {
        let code = parsed
            .as_ref()
            .and_then(|value| value.get("error").and_then(Value::as_str))
            .unwrap_or("request_failed");
        return Err(SeraError::External(format!(
            "ElevenLabs OAuth token request rejected (HTTP {}): {}",
            status.as_u16(),
            code.chars().take(200).collect::<String>()
        )));
    }
    let value = parsed.ok_or_else(|| {
        SeraError::External("ElevenLabs OAuth token response was not JSON".into())
    })?;
    if let Some(error) = value.get("error").and_then(Value::as_str) {
        return Err(SeraError::External(format!(
            "ElevenLabs OAuth token request rejected: {}",
            error.chars().take(200).collect::<String>()
        )));
    }
    let access = value
        .get("access_token")
        .and_then(Value::as_str)
        .unwrap_or("");
    if access.is_empty()
        || access.len() > MAX_TOKEN_RESPONSE_BYTES
        || !access.bytes().all(|b| b.is_ascii_graphic())
    {
        return Err(SeraError::External(
            "ElevenLabs OAuth token response was rejected".into(),
        ));
    }
    let refresh = value
        .get("refresh_token")
        .and_then(Value::as_str)
        .map(str::to_owned);
    if let Some(refresh) = &refresh
        && (refresh.is_empty()
            || refresh.len() > MAX_TOKEN_RESPONSE_BYTES
            || !refresh.bytes().all(|b| b.is_ascii_graphic()))
    {
        return Err(SeraError::External(
            "ElevenLabs OAuth token response was rejected".into(),
        ));
    }
    let expires_in = value.get("expires_in").and_then(|item| {
        item.as_u64()
            .or_else(|| item.as_str().and_then(|text| text.parse().ok()))
            .filter(|seconds| *seconds <= 86_400 * 7)
    });
    Ok(TokenPair {
        access_token: access.to_owned(),
        refresh_token: refresh,
        expires_in,
    })
}

/// One account's ElevenLabs sign-in: the access token is renewed from the
/// refresh token, and a rotated refresh token goes back to the account's own
/// sealed credential through `writer`.
pub struct OauthSession {
    tokens: tokio::sync::Mutex<OauthTokens>,
    client_id: String,
    token_url: String,
    writer: Option<Arc<dyn CredentialWriter>>,
    http: reqwest::Client,
}

struct OauthTokens {
    access_token: SecretString,
    /// Unix seconds; `None` means unknown, so the next use renews.
    expires_at: Option<i64>,
    refresh_token: SecretString,
}

impl OauthSession {
    /// A session for one stored sign-in. An empty access token or an unknown
    /// expiry is fine: the first use refreshes. `writer` receives refreshed
    /// tokens and their expiry.
    pub(crate) fn new(
        client_id: impl Into<String>,
        token_url: &str,
        access_token: &str,
        refresh_token: &str,
        expires_at: Option<i64>,
        writer: Option<Arc<dyn CredentialWriter>>,
    ) -> Result<Self> {
        if refresh_token.trim().is_empty() {
            return Err(SeraError::Config(
                "ElevenLabs OAuth refresh token is missing".into(),
            ));
        }
        let token_url = pin_oauth_endpoint(token_url, ELEVENLABS_TOKEN_URL)?.to_string();
        Ok(Self {
            tokens: tokio::sync::Mutex::new(OauthTokens {
                access_token: SecretString::new(access_token.to_owned()),
                expires_at,
                refresh_token: SecretString::new(refresh_token.to_owned()),
            }),
            client_id: client_id.into(),
            token_url,
            writer,
            http: reqwest::Client::builder()
                .timeout(Duration::from_secs(30))
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .map_err(|error| SeraError::External(error.to_string()))?,
        })
    }

    /// Current access token, refreshing first when missing or near expiry
    /// (30 s skew). The refresh holds the session lock, so concurrent callers
    /// share one refresh. A rotated refresh token is written back through the
    /// writer; a failed write fails the call so the rotation is never lost.
    pub(crate) async fn current_access_token(&self) -> Result<SecretString> {
        self.access(None).await
    }

    /// The server rejected `rejected`: renew it, unless another caller already
    /// replaced it while this one waited, in which case the new token is used.
    pub(crate) async fn renew_after_rejection(&self, rejected: &str) -> Result<SecretString> {
        self.access(Some(rejected)).await
    }

    async fn access(&self, rejected: Option<&str>) -> Result<SecretString> {
        let mut guard = self.tokens.lock().await;
        let stale = match (rejected, guard.expires_at) {
            (Some(rejected), _) => guard.access_token.expose_secret() == rejected,
            (None, Some(at)) => at <= Utc::now().timestamp() + 30,
            (None, None) => true,
        };
        if stale {
            let pair = refresh_grant_at(
                &self.http,
                &self.token_url,
                &self.client_id,
                guard.refresh_token.expose_secret(),
            )
            .await
            .map_err(|error| {
                SeraError::External(format!("ElevenLabs token refresh failed: {error}"))
            })?;
            let expires_at = pair
                .expires_in
                .map(|seconds| Utc::now().timestamp() + seconds.min(86_400 * 7) as i64);
            if let Some(writer) = &self.writer {
                writer
                    .persist_refresh(
                        pair.refresh_token.as_deref(),
                        pair.access_token.as_str(),
                        expires_at,
                    )
                    .map_err(|error| {
                        SeraError::External(format!(
                            "refreshed ElevenLabs token was not persisted: {error}"
                        ))
                    })?;
            }
            if let Some(refresh) = pair.refresh_token {
                guard.refresh_token = SecretString::new(refresh);
            }
            guard.expires_at = expires_at;
            guard.access_token = SecretString::new(pair.access_token);
        }
        Ok(guard.access_token.clone())
    }
}

/// A generation id as ElevenLabs issues it: letters, digits, `-` and `_`, up
/// to 128 characters. Anything else never reaches a URL path or a job row.
pub(crate) fn valid_generation_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 128
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
}

pub(crate) fn check_outbound_text(policy: &Policy, text: &str) -> Result<()> {
    if !policy.is_allowed(text) {
        return Err(SeraError::PolicyViolation);
    }
    Ok(())
}

pub(crate) fn extension_for_mime(mime: &str) -> &'static str {
    match mime {
        "audio/mpeg" | "audio/mp3" => "mp3",
        "audio/wav" | "audio/x-wav" => "wav",
        "audio/ogg" => "ogg",
        "audio/flac" => "flac",
        "audio/aac" => "aac",
        "audio/mp4" => "m4a",
        "audio/webm" => "weba",
        "image/gif" => "gif",
        "image/png" => "png",
        "image/jpeg" => "jpg",
        "image/webp" => "webp",
        "video/mp4" => "mp4",
        "video/webm" => "webm",
        _ => "bin",
    }
}

/// Shown when ElevenLabs refuses image or video because of the plan.
pub const PLAN_IMAGE_VIDEO_MESSAGE: &str = "Your ElevenLabs plan does not include image and video. Upgrade at elevenlabs.io/pricing, or ask for voice or sound instead.";
/// Shown when ElevenLabs refuses anything else because of plan or quota.
pub const PLAN_GENERIC_MESSAGE: &str = "Your ElevenLabs plan does not include this, or its credits are used up. Upgrade at elevenlabs.io/pricing, or ask for something your plan includes.";

/// A clear plan/quota message when ElevenLabs refused because of the
/// subscription (HTTP 402, or wording such as "requires a Pro plan",
/// "quota exceeded", "insufficient credits"); `None` for other failures.
pub(crate) fn plan_error_message(status: Option<u16>, text: &str) -> Option<&'static str> {
    let lower = text.to_ascii_lowercase();
    let has = |needle: &str| lower.contains(needle);
    let plan_words = has("plan")
        && (has("require") || has("upgrade") || has("not include") || has("available on"));
    let quota_words = (has("quota") || has("credit"))
        && (has("exceed")
            || has("insufficient")
            || has("not enough")
            || has("out of")
            || has("exhaust"));
    let hit = status == Some(402)
        || plan_words
        || quota_words
        || has("payment_required")
        || has("paid_plan_required");
    if !hit {
        return None;
    }
    if has("image") || has("video") || has("flows") {
        Some(PLAN_IMAGE_VIDEO_MESSAGE)
    } else {
        Some(PLAN_GENERIC_MESSAGE)
    }
}

pub(crate) fn sanitize_provider_error(body: &[u8]) -> String {
    let text = String::from_utf8_lossy(body);
    let parsed: Option<Value> = serde_json::from_str(&text).ok();
    parsed
        .as_ref()
        .and_then(|value| value.pointer("/detail/0/msg"))
        .and_then(Value::as_str)
        .or_else(|| {
            // ElevenLabs auth errors use an object: {"detail":{"message":"..."}}
            parsed
                .as_ref()
                .and_then(|value| value.pointer("/detail/message"))
                .and_then(Value::as_str)
        })
        .or_else(|| {
            parsed
                .as_ref()
                .and_then(|value| value.get("message"))
                .and_then(Value::as_str)
        })
        .unwrap_or(&text)
        .chars()
        .take(500)
        .collect()
}

pub(crate) fn store_media_artifact(
    artifacts: &ArtifactStore,
    bytes: &[u8],
    filename: &str,
    title: &str,
) -> Result<(String, String)> {
    let meta = ArtifactMeta {
        title: title.to_owned(),
        filename: filename.to_owned(),
        language: Language::En,
        source_artifact: None,
        created_at: Utc::now(),
    };
    let stored = artifacts.save(bytes, filename, meta)?;
    Ok((stored.hash, stored.filename))
}
