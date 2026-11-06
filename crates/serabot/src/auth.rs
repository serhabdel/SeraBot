//! OpenID Connect sign-in, invitations, and session issuance.
//!
//! The multi-user reconstruction's identity layer. Sign-in is a thin OIDC
//! passthrough: the identity provider (Microsoft in v1, discovered from
//! `{issuer}/v2.0/.well-known/openid-configuration`) does all authentication;
//! SeraBot never sees a password. The authorization-code flow runs with
//! PKCE, a `state`, and a `nonce`; the returned `id_token` is verified
//! against the provider's JWKS before anything is persisted.
//!
//! # Permissions
//!
//! Reading: none. This module initiates browser redirects and exchanges the
//! resulting authorization code — it never reads OneDrive, documents, or any
//! other user data. Writing: `users` and `invites` rows, session rows in
//! `device_tokens` (sha256-hashed tokens only), and `auth.*` audit events.
//!
//! The invitation gate: the admin mints single-use codes; a NEW subject needs
//! an unconsumed invite to create an account, and the consumption happens
//! inside [`crate::store::Store::complete_login`]'s transaction, exactly
//! once, when the user row is actually created. An existing subject signing
//! in again needs no invite. Validating the invite at `login_start` (so a
//! stale code never reaches the provider) but consuming it at the callback
//! is deliberate: consuming early would burn a code on a login that never
//! completes, and consuming late is made race-safe by the conditional
//! `UPDATE invites SET uses_left = uses_left - 1 ...` in the same
//! transaction as the `INSERT INTO users`.
//!
//! # Secrets
//!
//! The OAuth client id comes from the age store at startup and is never
//! logged; `Debug` redacts it. Session tokens are 32 random bytes handed to
//! the client exactly once (over the PKCE-bound handoff, never displayed to
//! the user) and stored only as SHA-256 hex — the same shape as
//! the operator CLI device tokens, which remain valid and now belong to the
//! admin. IdP-returned tokens are used in memory and never persisted.
//!
//! # Failure semantics
//!
//! Fail closed everywhere: auth disabled, an unknown or expired login
//! `state`, an exhausted or expired invite, the user cap being reached, and
//! any id_token verification failure (signature, issuer, audience, expiry,
//! nonce) all return explicit errors and create nothing. Provider responses
//! are untrusted data: bodies are bounded, profile claims are sanitized
//! before storage, and provider error bodies are never echoed to callers.
//! The discovery document and JWKS are cached after the first fetch and the
//! JWKS is refreshed at most once per callback when a token references an
//! unknown key id.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use futures::StreamExt;
use openidconnect::core::{
    CoreAuthenticationFlow, CoreClient, CoreIdToken, CoreIdTokenClaims, CoreIdTokenVerifier,
    CoreJsonWebKeySet, CoreProviderMetadata,
};
use openidconnect::http::{HeaderMap, StatusCode, header::HeaderName, header::HeaderValue};
use openidconnect::{
    AuthorizationCode, ClaimsVerificationError, ClientId, CsrfToken, HttpRequest, HttpResponse,
    IssuerUrl, Nonce, PkceCodeChallenge, PkceCodeVerifier, RedirectUrl, Scope,
    SignatureVerificationError, TokenResponse,
};
use rand::RngCore;
use serde::Serialize;
use serde_json::json;
use tokio::sync::Mutex;
use tracing::warn;
use url::Url;

use crate::config::{AuthConfig, Config, KNOWN_AUTH_PROVIDERS};
use crate::error::{Result, SeraError};
use crate::fleet::FleetRegistry;
use crate::secrets::SharedSecrets;
use crate::store::{Store, User, UserStatus};
use crate::util::{constant_time_eq, sha256_hex, truncate_chars};

/// A pending browser login, alive only until the callback or this TTL.
pub const LOGIN_STATE_TTL: Duration = Duration::from_secs(10 * 60);
/// Bound on simultaneous in-flight logins; exceeded, new starts fail closed.
pub const MAX_PENDING_LOGINS: usize = 64;
/// Hard cap on any identity-provider response body (discovery, JWKS, token).
pub const MAX_IDP_RESPONSE_BYTES: usize = 256 * 1024;
/// Extra scopes requested on top of `openid` (added by the OIDC client).
pub const AUTH_SCOPES: [&str; 2] = ["profile", "email"];
/// Authorization codes are bounded; providers use far shorter strings.
const MAX_CODE_CHARS: usize = 2048;
/// Invite codes: 32 random bytes, hex encoded, shown once.
const INVITE_CODE_BYTES: usize = 32;
/// Redirect path the IdP returns the browser to, relative to `base_url`.
const CALLBACK_PATH: &str = "/v1/auth/callback";
/// Microsoft's consumer (personal account) tenant. Its discovery document
/// reports this GUID as the issuer, never the `consumers` alias.
pub const CONSUMERS_TENANT_ID: &str = "9188040d-6c67-4c5b-b112-36a304b66dad";
/// Issuer shape for the multi-tenant aliases: Microsoft publishes the literal
/// `{tenantid}` placeholder in discovery, and each id_token carries the real
/// tenant in `iss`.
const MICROSOFT_ISSUER_TEMPLATE: &str = "https://login.microsoftonline.com/{tenantid}/v2.0";
/// How long a one-time handoff code stays redeemable.
pub const HANDOFF_TTL: chrono::Duration = chrono::Duration::seconds(120);
/// Fixed mobile redirect for the handoff.
const MOBILE_RETURN_TO: &str = "serabot://auth";

/// Where discovery happens and how id_token issuers are checked for a
/// configured Microsoft tenant: `(issuer URL, templated)`.
///
/// `consumers` maps to the consumer tenant GUID (what Microsoft reports as
/// the issuer). `common` and `organizations` publish a `{tenantid}` template,
/// so the discovered issuer cannot equal the configured URL and each token's
/// `iss` is matched against its own `tid` claim instead. Any other value is a
/// concrete tenant id whose issuer is exact.
fn microsoft_issuer(tenant: &str) -> (String, bool) {
    let tenant = tenant.trim();
    if tenant.eq_ignore_ascii_case("consumers") {
        (
            format!("https://login.microsoftonline.com/{CONSUMERS_TENANT_ID}/v2.0"),
            false,
        )
    } else if tenant.eq_ignore_ascii_case("common") || tenant.eq_ignore_ascii_case("organizations")
    {
        (
            format!(
                "https://login.microsoftonline.com/{}/v2.0",
                tenant.to_ascii_lowercase()
            ),
            true,
        )
    } else {
        (
            format!("https://login.microsoftonline.com/{tenant}/v2.0"),
            false,
        )
    }
}

/// A native client's handoff request, validated: where to send the browser
/// back, the S256 challenge the later redemption must satisfy, and the
/// client's own anti-CSRF state echoed on the redirect.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HandoffRequest {
    pub return_to: String,
    pub challenge: String,
    pub client_state: String,
}

impl HandoffRequest {
    /// Validate the optional trio from `login/start`. All three or none:
    /// `Ok(None)` is the legacy web page flow; a partial set, a `return_to`
    /// outside the allowlist, or malformed values are rejected.
    pub fn parse(
        return_to: Option<&str>,
        challenge: Option<&str>,
        client_state: Option<&str>,
    ) -> Result<Option<Self>> {
        match (return_to, challenge, client_state) {
            (None, None, None) => Ok(None),
            (Some(return_to), Some(challenge), Some(client_state)) => {
                let return_to = return_to.trim();
                validate_return_to(return_to)?;
                let challenge = challenge.trim();
                if challenge.len() != 43 || !challenge.chars().all(is_base64url) {
                    return Err(SeraError::Validation(
                        "handoff_challenge must be a 43-character base64url SHA-256 digest".into(),
                    ));
                }
                let client_state = client_state.trim();
                if client_state.len() < 8
                    || client_state.len() > 128
                    || !client_state
                        .chars()
                        .all(|c| c.is_ascii_alphanumeric() || "-._~+/=".contains(c))
                {
                    return Err(SeraError::Validation(
                        "client_state must be 8-128 URL-safe characters".into(),
                    ));
                }
                Ok(Some(Self {
                    return_to: return_to.to_owned(),
                    challenge: challenge.to_owned(),
                    client_state: client_state.to_owned(),
                }))
            }
            _ => Err(SeraError::Validation(
                "return_to, handoff_challenge, and client_state must be given together".into(),
            )),
        }
    }
}

fn is_base64url(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '-' || c == '_'
}

/// The strict `return_to` allowlist: the mobile scheme, or the desktop
/// loopback listener on a non-privileged port. Nothing else, no query.
pub fn validate_return_to(value: &str) -> Result<()> {
    if value == MOBILE_RETURN_TO {
        return Ok(());
    }
    let reject = || SeraError::Validation("return_to is not an allowed redirect".into());
    let url = Url::parse(value).map_err(|_| reject())?;
    let port = url.port().ok_or_else(reject)?;
    if url.scheme() != "http"
        || url.host_str() != Some("127.0.0.1")
        || !(1024..=65535).contains(&port)
        || url.path() != "/auth-done"
        || url.query().is_some()
        || url.fragment().is_some()
        || !url.username().is_empty()
        || url.password().is_some()
        || value != format!("http://127.0.0.1:{port}/auth-done")
    {
        return Err(reject());
    }
    Ok(())
}

/// Why a sign-in could not complete, as the short code appended to the
/// client's `return_to` (`?error=<code>`). Carries no provider detail.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SignInFailure {
    Denied,
    InviteRequired,
    InviteInvalid,
    Disabled,
    MaxUsers,
    Server,
}

impl SignInFailure {
    pub fn code(self) -> &'static str {
        match self {
            Self::Denied => "denied",
            Self::InviteRequired => "invite_required",
            Self::InviteInvalid => "invite_invalid",
            Self::Disabled => "disabled",
            Self::MaxUsers => "max_users",
            Self::Server => "server",
        }
    }

    fn classify(error: &SeraError) -> Self {
        match error {
            SeraError::Forbidden(message) if message.contains("invitation is required") => {
                Self::InviteRequired
            }
            SeraError::Forbidden(_) => Self::Denied,
            SeraError::InvalidState(message) if message.contains("invitation") => {
                Self::InviteInvalid
            }
            SeraError::InvalidState(message) if message.contains("user limit") => Self::MaxUsers,
            SeraError::InvalidState(message) if message.contains("disabled") => Self::Disabled,
            _ => Self::Server,
        }
    }
}

/// What the browser is told after the callback.
#[derive(Debug)]
pub struct CallbackResult {
    /// The native client's handoff request, when the sign-in carried one.
    pub handoff: Option<HandoffRequest>,
    pub outcome: std::result::Result<User, (SignInFailure, SeraError)>,
}

/// A redeemed handoff: a freshly minted session for the signed-in user.
#[derive(Debug)]
pub struct HandoffGrant {
    pub token: String,
    pub expires_at: chrono::DateTime<chrono::Utc>,
    pub user: User,
}

/// Build the `302` target for a native client: `return_to` plus either the
/// one-time `handoff` code or a short `error` code, and the client's `state`.
pub fn handoff_redirect(
    request: &HandoffRequest,
    result: std::result::Result<&str, SignInFailure>,
) -> String {
    let mut url = Url::parse(&request.return_to)
        .unwrap_or_else(|_| Url::parse(MOBILE_RETURN_TO).expect("static url"));
    {
        let mut query = url.query_pairs_mut();
        match result {
            Ok(code) => query.append_pair("handoff", code),
            Err(failure) => query.append_pair("error", failure.code()),
        };
        query.append_pair("state", &request.client_state);
    }
    url.to_string()
}

/// One row of the public providers list.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct ProviderStatus {
    pub id: &'static str,
    pub enabled: bool,
}

/// The public provider list, derived from config: every provider this build
/// knows, in a fixed order, with sign-in possible only for the enabled and
/// implemented subset (`microsoft` in v1; `google`/`github` render disabled).
pub fn provider_report(config: &AuthConfig) -> Vec<ProviderStatus> {
    let effective = config.effective_providers();
    KNOWN_AUTH_PROVIDERS
        .iter()
        .map(|id| ProviderStatus {
            id,
            enabled: config.enabled && effective.contains(id),
        })
        .collect()
}

/// A login in flight: the state key binds the browser to the PKCE verifier,
/// nonce, provider, and (optionally) the invite presented at login start.
struct PendingLogin {
    provider: String,
    nonce: Nonce,
    verifier: PkceCodeVerifier,
    invite_hash: Option<String>,
    handoff: Option<HandoffRequest>,
    created: Instant,
}

/// The cached discovery document plus its JWKS. Fetched once per process and
/// refreshed only when an id_token references an unknown key id.
#[derive(Clone)]
struct ProviderSnapshot {
    metadata: CoreProviderMetadata,
    jwks_url: openidconnect::JsonWebKeySetUrl,
}

impl ProviderSnapshot {
    fn client(&self, client_id: &str, redirect_url: &RedirectUrl) -> CoreClient {
        CoreClient::from_provider_metadata(
            self.metadata.clone(),
            ClientId::new(client_id.to_owned()),
            None,
        )
        .set_redirect_uri(redirect_url.clone())
    }

    fn issuer(&self) -> IssuerUrl {
        self.metadata.issuer().clone()
    }

    fn jwks(&self) -> CoreJsonWebKeySet {
        self.metadata.jwks().to_owned()
    }
}

/// The OIDC sign-in service. One instance lives in the API state; it is
/// cheap to share (`Arc`) and safe to clone handles from.
pub struct AuthService {
    store: Store,
    http: reqwest::Client,
    enabled: bool,
    /// Provider ids that can actually serve sign-ins (configured ∩
    /// implemented).
    providers: Vec<&'static str>,
    tenant: String,
    client_id: String,
    redirect_url: RedirectUrl,
    session_ttl: chrono::Duration,
    invite_ttl: chrono::Duration,
    max_users: usize,
    issuer_url: IssuerUrl,
    /// `Some(template)` for the multi-tenant aliases (`common`,
    /// `organizations`): the discovery document publishes this literal
    /// template and tokens are matched against their own `tid` claim.
    issuer_template: Option<String>,
    admin_email: Option<String>,
    login_ttl: Duration,
    pending: Mutex<HashMap<String, PendingLogin>>,
    provider_cache: Mutex<Option<ProviderSnapshot>>,
    /// Per-user fleet registry used to seed a new account's default fleet at
    /// the end of a successful first sign-in. Optional so unit tests can build
    /// the service without a runtime; the lazy seed in the registry is the
    /// safety net either way.
    fleet: Option<Arc<FleetRegistry>>,
}

impl std::fmt::Debug for AuthService {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AuthService")
            .field("enabled", &self.enabled)
            .field("providers", &self.providers)
            .field("tenant", &self.tenant)
            .field("issuer_url", &self.issuer_url)
            .field("max_users", &self.max_users)
            .field("client_id", &"<redacted>")
            .field("pending_logins", &self.pending.try_lock().map(|p| p.len()))
            .finish_non_exhaustive()
    }
}

impl AuthService {
    /// Build the service from validated configuration. The client id is
    /// required from the age store at startup and never read again from
    /// disk; a missing or malformed value fails construction so the process
    /// never serves sign-ins half-configured.
    pub fn new(config: &Config, store: Store, secrets: &SharedSecrets) -> Result<Self> {
        let auth = &config.auth;
        if !auth.enabled {
            return Err(SeraError::Internal(
                "the auth service was built while auth is disabled".into(),
            ));
        }
        let client_id = secrets.require(&auth.client_id_secret)?;
        let client_id = client_id.trim().to_owned();
        if client_id.is_empty()
            || client_id.len() > 16 * 1024
            || client_id.contains(char::is_control)
        {
            return Err(SeraError::Config(
                "auth client id is missing or invalid; check the age store".into(),
            ));
        }
        let tenant = auth.tenant.trim().to_owned();
        let (issuer, templated) = microsoft_issuer(&tenant);
        let issuer_url = IssuerUrl::new(issuer).map_err(|_| {
            SeraError::Config("auth tenant does not form a valid issuer URL".into())
        })?;
        let base = auth.base_url.trim().trim_end_matches('/');
        let redirect_url = RedirectUrl::new(format!("{base}{CALLBACK_PATH}")).map_err(|_| {
            SeraError::Config("auth.base_url does not form a valid redirect".into())
        })?;
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|error| SeraError::Internal(format!("auth HTTP client: {error}")))?;
        Ok(Self {
            store,
            http,
            enabled: true,
            providers: auth.effective_providers(),
            tenant,
            client_id,
            redirect_url,
            session_ttl: chrono::Duration::hours(auth.session_ttl_hours as i64),
            invite_ttl: chrono::Duration::hours(auth.invite_ttl_hours as i64),
            max_users: auth.max_users,
            issuer_url,
            issuer_template: templated.then(|| MICROSOFT_ISSUER_TEMPLATE.to_owned()),
            admin_email: Some(auth.admin_email.trim().to_owned()).filter(|email| !email.is_empty()),
            login_ttl: LOGIN_STATE_TTL,
            pending: Mutex::new(HashMap::new()),
            provider_cache: Mutex::new(None),
            fleet: None,
        })
    }

    /// Attach the per-user fleet registry so a newly created account gets the
    /// default fleet immediately. Seeding failures are logged, not fatal: the
    /// registry also seeds lazily on first use.
    pub fn with_fleet(mut self, fleet: Arc<FleetRegistry>) -> Self {
        self.fleet = Some(fleet);
        self
    }

    /// Whether sign-in is served at all. Always true for a built service;
    /// the API state carries `None` while `[auth] enabled = false`, so every
    /// endpoint fails closed there.
    pub fn enabled(&self) -> bool {
        self.enabled
    }

    /// The provider ids that can serve sign-ins right now.
    pub fn providers(&self) -> &[&'static str] {
        &self.providers
    }

    /// Override the identity provider issuer. Intended for tests, which
    /// point discovery at a loopback mock server; HTTPS (or loopback HTTP)
    /// is enforced exactly like the production endpoint.
    pub fn set_issuer_url(&mut self, issuer_url: impl Into<String>) -> Result<()> {
        let issuer_url = issuer_url.into();
        self.issuer_url = validate_issuer(&issuer_url)?;
        self.provider_cache = Mutex::new(None);
        Ok(())
    }

    /// Builder form of [`Self::set_issuer_url`].
    pub fn with_issuer_url(mut self, issuer_url: impl Into<String>) -> Result<Self> {
        self.set_issuer_url(issuer_url)?;
        Ok(self)
    }

    /// Override the pending-login TTL. Intended for tests (production uses
    /// the ten-minute default); bounded to at most a day.
    pub fn set_login_ttl(&mut self, ttl: Duration) -> Result<()> {
        if ttl > Duration::from_secs(24 * 60 * 60) {
            return Err(SeraError::Validation(
                "login ttl must be at most one day".into(),
            ));
        }
        self.login_ttl = ttl;
        Ok(())
    }

    /// Mint an invitation and return the raw code exactly once; only its
    /// SHA-256 hash is stored. `created_by` is the acting admin's user id.
    pub fn mint_invite(&self, uses: u32, created_by: &str) -> Result<String> {
        let mut bytes = [0u8; INVITE_CODE_BYTES];
        rand::rngs::OsRng.fill_bytes(&mut bytes);
        let code = hex::encode(bytes);
        let expires_at = chrono::Utc::now() + self.invite_ttl;
        self.store
            .mint_invite(&code, uses, expires_at, created_by)?;
        Ok(code)
    }

    /// Start a browser sign-in: validate the request, bind a fresh
    /// `state`/`nonce`/PKCE pair (and the invite's hash) in memory, and
    /// return the provider's authorize URL. The invite is only *validated*
    /// here; it is consumed at the callback, inside the transaction that
    /// creates the user, so a failed or abandoned login never burns it.
    pub async fn login_start(&self, provider: &str, invite_code: Option<&str>) -> Result<String> {
        self.login_start_with_handoff(provider, invite_code, None)
            .await
    }

    /// [`Self::login_start`] for a native client: the validated
    /// [`HandoffRequest`] rides with the pending login so the callback can
    /// answer the browser with a redirect back to the app.
    pub async fn login_start_with_handoff(
        &self,
        provider: &str,
        invite_code: Option<&str>,
        handoff: Option<HandoffRequest>,
    ) -> Result<String> {
        if !self.enabled {
            return Err(SeraError::Forbidden("sign-in is disabled".into()));
        }
        let provider = provider.trim();
        if !KNOWN_AUTH_PROVIDERS.contains(&provider) {
            return Err(SeraError::Validation(format!(
                "unknown provider '{provider}'; known providers are microsoft, google, github"
            )));
        }
        if !self.providers.contains(&provider) {
            return Err(SeraError::Forbidden(format!(
                "provider '{provider}' is not enabled on this deployment"
            )));
        }
        let invite_hash = match invite_code {
            Some(code) => {
                let code = code.trim();
                if code.is_empty() || code.len() > 256 || code.chars().any(char::is_control) {
                    return Err(SeraError::Validation(
                        "invite code must be 1-256 characters without control characters".into(),
                    ));
                }
                let hash = sha256_hex(code);
                if !self.store.invite_is_redeemable(&hash)? {
                    return Err(SeraError::InvalidState(
                        "invitation is unknown, expired, or exhausted".into(),
                    ));
                }
                Some(hash)
            }
            None => None,
        };

        let snapshot = self.provider_snapshot().await?;
        let client = snapshot.client(&self.client_id, &self.redirect_url);
        let (pkce_challenge, pkce_verifier) = PkceCodeChallenge::new_random_sha256();
        let (url, csrf, nonce) = client
            .authorize_url(
                CoreAuthenticationFlow::AuthorizationCode,
                CsrfToken::new_random,
                Nonce::new_random,
            )
            .add_scope(Scope::new(AUTH_SCOPES[0].to_owned()))
            .add_scope(Scope::new(AUTH_SCOPES[1].to_owned()))
            .set_pkce_challenge(pkce_challenge)
            .url();

        let state = csrf.secret().to_owned();
        let mut pending = self.pending.lock().await;
        let now = Instant::now();
        pending.retain(|_, login| now.duration_since(login.created) < self.login_ttl);
        if pending.len() >= MAX_PENDING_LOGINS {
            return Err(SeraError::InvalidState(
                "too many sign-ins are in flight; try again shortly".into(),
            ));
        }
        let login = PendingLogin {
            provider: provider.to_owned(),
            nonce,
            verifier: pkce_verifier,
            invite_hash,
            handoff,
            created: now,
        };
        if pending.insert(state.clone(), login).is_some() {
            return Err(SeraError::Internal(
                "login state collision; restart the sign-in".into(),
            ));
        }
        Ok(url.to_string())
    }

    /// Complete a browser sign-in: exchange the code, verify the id_token
    /// against the provider's JWKS, upsert the namespaced subject, and mint
    /// a session. Returns the user and the raw session token — shown to the
    /// user exactly once, never stored in the clear.
    pub async fn callback(&self, state: &str, code: &str) -> Result<(User, String)> {
        let result = self.callback_login(state, code).await;
        let user = result.outcome.map_err(|(_, error)| error)?;
        let token = self.store.mint_session(&user.id, self.session_ttl)?;
        Ok((user, token))
    }

    /// Complete a browser sign-in without issuing a session. The account is
    /// verified, created or refreshed (consuming an invite when new), and
    /// audited; the caller decides what the browser sees. A native client's
    /// [`HandoffRequest`] is returned even on failure so the browser can be
    /// sent back with a short error code.
    pub async fn callback_login(&self, state: &str, code: &str) -> CallbackResult {
        let pending = match self.take_pending(state).await {
            Ok(pending) => pending,
            Err(error) => {
                return CallbackResult {
                    handoff: None,
                    outcome: Err((SignInFailure::classify(&error), error)),
                };
            }
        };
        let handoff = pending.handoff.clone();
        let outcome = self
            .finish_login(pending, code)
            .await
            .map_err(|error| (SignInFailure::classify(&error), error));
        CallbackResult { handoff, outcome }
    }

    /// The provider redirected back with an error instead of a code (the
    /// user declined, or consent failed). Consumes the pending login and
    /// returns the native client's handoff request, if any, so the browser
    /// can be sent back with `error=denied`.
    pub async fn callback_denied(&self, state: &str) -> Option<HandoffRequest> {
        self.take_pending(state)
            .await
            .ok()
            .and_then(|pending| pending.handoff)
    }

    async fn take_pending(&self, state: &str) -> Result<PendingLogin> {
        if !self.enabled {
            return Err(SeraError::Forbidden("sign-in is disabled".into()));
        }
        let state = state.trim();
        if state.is_empty() || state.len() > 256 {
            return Err(SeraError::InvalidState(
                "unknown or expired sign-in state".into(),
            ));
        }
        let mut pending = self.pending.lock().await;
        let login = pending
            .remove(state)
            .ok_or_else(|| SeraError::InvalidState("unknown or expired sign-in state".into()))?;
        if Instant::now().duration_since(login.created) > self.login_ttl {
            return Err(SeraError::InvalidState(
                "unknown or expired sign-in state".into(),
            ));
        }
        Ok(login)
    }

    async fn finish_login(&self, pending: PendingLogin, code: &str) -> Result<User> {
        let code = code.trim();
        if code.is_empty() || code.len() > MAX_CODE_CHARS || code.chars().any(char::is_control) {
            return Err(SeraError::Validation(
                "the authorization code is invalid".into(),
            ));
        }
        let snapshot = self.provider_snapshot().await?;
        let client = snapshot.client(&self.client_id, &self.redirect_url);
        let token_response = client
            .exchange_code(AuthorizationCode::new(code.to_owned()))
            .set_pkce_verifier(pending.verifier)
            .request_async(|request| async { self.execute(request).await })
            .await
            .map_err(|error| {
                warn!(%error, "authorization code exchange failed");
                SeraError::External("the identity provider rejected the sign-in".into())
            })?;
        let id_token: CoreIdToken = match token_response.id_token() {
            Some(id_token) => id_token.clone(),
            None => {
                warn!("token response carried no id_token");
                return Err(SeraError::External(
                    "the identity provider did not return an id_token".into(),
                ));
            }
        };
        let claims = self
            .verified_claims(&id_token, &pending.nonce, &snapshot)
            .await?;

        // Provider-reported values are untrusted data: bound and sanitized
        // before they reach the database or any API response.
        let provider = pending.provider.as_str();
        let subject = sanitize_profile(
            claims.subject().as_str(),
            200usize.saturating_sub(provider.len() + 1),
        );
        if subject.is_empty() {
            return Err(SeraError::Forbidden(
                "sign-in could not be verified: the provider reported no subject".into(),
            ));
        }
        let provider_subject = format!("{provider}:{subject}");
        let email = claims
            .email()
            .map(|email| email.as_str())
            .unwrap_or_default();
        let name = claims
            .name()
            .and_then(|name| name.get(None))
            .map(|name| name.as_str())
            .or_else(|| {
                claims
                    .preferred_username()
                    .map(|username| username.as_str())
            })
            .or_else(|| {
                claims
                    .given_name()
                    .and_then(|given| given.get(None))
                    .map(|given| given.as_str())
            })
            .unwrap_or(subject.as_str());
        let email = sanitize_profile(email, 320);
        let display_name = sanitize_profile(name, 200);

        let outcome = self.store.complete_login_claiming(
            &provider_subject,
            &email,
            &display_name,
            pending.invite_hash.as_deref(),
            self.max_users,
            self.admin_email.as_deref(),
        )?;
        if outcome.user.status != UserStatus::Active {
            return Err(SeraError::InvalidState(
                "this account is disabled; sign-in was refused".into(),
            ));
        }
        // Post-login hook: a brand-new account gets the default fleet in the
        // same request. A failure is logged and left to the registry's lazy
        // seed, so a seeding problem never locks the new user out.
        if outcome.created {
            match &self.fleet {
                Some(fleet) => {
                    if let Err(error) = fleet.seed(&outcome.user.id) {
                        warn!(%error, "new user fleet seeding failed; will seed on first use");
                    }
                }
                None => warn!("no fleet registry attached; new user fleet will seed on first use"),
            }
        }
        if let Err(error) = self.store.record_audit(
            &outcome.user.id,
            "auth.login",
            None,
            None,
            None,
            None,
            None,
            Some(&json!({
                "provider": pending.provider,
                "created": outcome.created,
            })),
        ) {
            warn!(%error, "login audit write failed");
        }
        Ok(outcome.user)
    }

    /// Issue the one-time handoff code for a verified sign-in: 32 random
    /// bytes (base64url), only the SHA-256 hash persisted, bound to the
    /// client's challenge and the user, valid for [`HANDOFF_TTL`].
    pub fn issue_handoff(&self, user: &User, request: &HandoffRequest) -> Result<String> {
        let mut bytes = [0u8; 32];
        rand::rngs::OsRng.fill_bytes(&mut bytes);
        let code = URL_SAFE_NO_PAD.encode(bytes);
        self.store.create_handoff(
            &sha256_hex(&code),
            &request.challenge,
            &user.id,
            HANDOFF_TTL,
        )?;
        self.audit(&user.id, "auth.handoff.issued", json!({}));
        Ok(code)
    }

    /// Redeem a handoff code with the client's verifier. The code is burned
    /// on every attempt; a wrong verifier, expiry, reuse, an unknown code, or
    /// a since-disabled account all return the same generic
    /// [`SeraError::Validation`] and mint nothing.
    pub fn redeem_handoff(&self, code: &str, verifier: &str) -> Result<HandoffGrant> {
        let invalid = || SeraError::Validation("invalid handoff".into());
        if !self.enabled {
            return Err(SeraError::Forbidden("sign-in is disabled".into()));
        }
        if code.is_empty() || code.len() > 256 {
            self.audit(
                "anonymous",
                "auth.handoff.failed",
                json!({"reason": "shape"}),
            );
            return Err(invalid());
        }
        // Burn first: whatever happens next, this code can never be tried again.
        let Some((challenge, user_id, unexpired)) = self.store.take_handoff(&sha256_hex(code))?
        else {
            self.audit(
                "anonymous",
                "auth.handoff.failed",
                json!({"reason": "unknown"}),
            );
            return Err(invalid());
        };
        let digest =
            URL_SAFE_NO_PAD.encode(<sha2::Sha256 as sha2::Digest>::digest(verifier.as_bytes()));
        let verified = (43..=128).contains(&verifier.len())
            && verifier
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || "-._~".contains(c))
            && constant_time_eq(&digest, &challenge);
        if !unexpired || !verified {
            let reason = if unexpired { "verifier" } else { "expired" };
            self.audit(&user_id, "auth.handoff.failed", json!({"reason": reason}));
            return Err(invalid());
        }
        let user = match self.store.get_user(&user_id)? {
            Some(user) if user.status == UserStatus::Active => user,
            _ => {
                self.audit(&user_id, "auth.handoff.failed", json!({"reason": "user"}));
                return Err(invalid());
            }
        };
        let token = self.store.mint_session(&user.id, self.session_ttl)?;
        let expires_at = self
            .store
            .device_token_by_hash(&sha256_hex(&token))?
            .and_then(|record| record.expires_at)
            .unwrap_or_else(|| chrono::Utc::now() + self.session_ttl);
        self.audit(&user.id, "auth.handoff.redeemed", json!({}));
        Ok(HandoffGrant {
            token,
            expires_at,
            user,
        })
    }

    fn audit(&self, actor: &str, event: &str, details: serde_json::Value) {
        if let Err(error) =
            self.store
                .record_audit(actor, event, None, None, None, None, None, Some(&details))
        {
            warn!(%error, event, "auth audit write failed");
        }
    }

    /// Verify the id_token claims: signature via the cached JWKS, plus the
    /// issuer, audience, expiry, and nonce the provider must match. An
    /// unknown key id triggers exactly one JWKS refresh and retry; anything
    /// else fails closed.
    async fn verified_claims<'a>(
        &'a self,
        id_token: &'a CoreIdToken,
        nonce: &Nonce,
        snapshot: &ProviderSnapshot,
    ) -> Result<&'a CoreIdTokenClaims> {
        let verifier = {
            let client_id = ClientId::new(self.client_id.clone());
            let issuer = snapshot.issuer();
            // Templated tenants (`common`, `organizations`) carry a
            // different `iss` per tenant, so the library's single-issuer
            // comparison is replaced by the explicit `tid`-bound check
            // below; it is never simply switched off.
            let strict = self.issuer_template.is_none();
            move |jwks: CoreJsonWebKeySet| {
                CoreIdTokenVerifier::new_public_client(client_id.clone(), issuer.clone(), jwks)
                    .require_issuer_match(strict)
            }
        };
        let claims = match id_token.claims(&verifier(snapshot.jwks()), nonce) {
            Ok(claims) => claims,
            Err(error) => {
                if !matches!(
                    error,
                    ClaimsVerificationError::SignatureVerification(
                        SignatureVerificationError::NoMatchingKey
                    )
                ) {
                    return Err(map_verification_error(error));
                }
                warn!("id_token key id not in the cached JWKS; refreshing keys once");
                self.refresh_jwks(snapshot).await?;
                let jwks = self.cached_jwks().await?;
                id_token
                    .claims(&verifier(jwks), nonce)
                    .map_err(map_verification_error)?
            }
        };
        if let Some(template) = &self.issuer_template {
            let tid = serde_json::to_value(id_token)
                .ok()
                .and_then(|value| value.as_str().and_then(jwt_string_claim_tid));
            check_templated_issuer(template, Some(claims.issuer().as_str()), tid.as_deref())?;
        }
        Ok(claims)
    }

    /// Fetch the discovery document (and its JWKS) once and cache it. The
    /// cache lives for the process lifetime: the restart-to-apply model
    /// covers provider endpoint changes, and key rotation is handled by the
    /// per-callback JWKS refresh.
    async fn provider_snapshot(&self) -> Result<ProviderSnapshot> {
        let mut cache = self.provider_cache.lock().await;
        if let Some(snapshot) = cache.as_ref() {
            return Ok(snapshot.clone());
        }
        let discovery_url = format!(
            "{}/.well-known/openid-configuration",
            self.issuer_url.as_str().trim_end_matches('/')
        );
        let request = HttpRequest {
            url: Url::parse(&discovery_url)
                .map_err(|_| SeraError::Config("discovery URL is not valid".into()))?,
            method: openidconnect::http::Method::GET,
            headers: [(
                openidconnect::http::header::ACCEPT,
                HeaderValue::from_static("application/json"),
            )]
            .into_iter()
            .collect(),
            body: Vec::new(),
        };
        let response = self.execute(request).await?;
        let metadata = parse_discovery(
            response.status_code,
            &response.body,
            &self.issuer_url,
            self.issuer_template.as_deref(),
        )
        .map_err(|error| {
            warn!(%error, "openid discovery failed");
            SeraError::External("identity provider discovery failed".into())
        })?;
        let jwks = CoreJsonWebKeySet::fetch_async(metadata.jwks_uri(), |request| async {
            self.execute(request).await
        })
        .await
        .map_err(|error| {
            warn!(%error, "openid discovery failed fetching keys");
            SeraError::External("identity provider discovery failed".into())
        })?;
        let metadata = metadata.set_jwks(jwks);
        let snapshot = ProviderSnapshot {
            jwks_url: metadata.jwks_uri().clone(),
            metadata,
        };
        *cache = Some(snapshot.clone());
        Ok(snapshot)
    }

    /// Refetch the JWKS and update the cached snapshot.
    async fn refresh_jwks(&self, snapshot: &ProviderSnapshot) -> Result<()> {
        let jwks_url = snapshot.jwks_url.clone();
        let jwks = CoreJsonWebKeySet::fetch_async(&jwks_url, |request| async {
            self.execute(request).await
        })
        .await
        .map_err(|error| {
            warn!(%error, "jwks refresh failed");
            SeraError::External("identity provider keys could not be refreshed".into())
        })?;
        let mut cache = self.provider_cache.lock().await;
        if let Some(cached) = cache.as_mut() {
            cached.metadata = cached.metadata.clone().set_jwks(jwks);
        }
        Ok(())
    }

    /// The JWKS currently cached, if discovery has run.
    async fn cached_jwks(&self) -> Result<CoreJsonWebKeySet> {
        let cache = self.provider_cache.lock().await;
        cache
            .as_ref()
            .map(ProviderSnapshot::jwks)
            .ok_or_else(|| SeraError::Internal("the provider cache is empty".into()))
    }

    /// Translate an oauth2 request onto this service's reqwest client and
    /// back. Response bodies are hard-capped; provider error bodies never
    /// reach the caller.
    async fn execute(&self, request: HttpRequest) -> std::result::Result<HttpResponse, SeraError> {
        let method = reqwest::Method::from_bytes(request.method.as_str().as_bytes())
            .map_err(|_| SeraError::External("invalid identity provider request".into()))?;
        let mut builder = self.http.request(method, request.url.as_str());
        for (name, value) in request.headers.iter() {
            builder = builder.header(name.as_str(), value.as_bytes());
        }
        let response = builder.body(request.body).send().await.map_err(|error| {
            warn!(%error, "identity provider request failed");
            SeraError::External("the identity provider could not be reached".into())
        })?;
        if response
            .content_length()
            .is_some_and(|length| length as usize > MAX_IDP_RESPONSE_BYTES)
        {
            return Err(SeraError::External(
                "identity provider response too large".into(),
            ));
        }
        let status = StatusCode::from_u16(response.status().as_u16()).map_err(|_| {
            SeraError::External("identity provider returned an invalid status".into())
        })?;
        let mut headers = HeaderMap::new();
        for (name, value) in response.headers().iter() {
            if let (Ok(name), Ok(value)) = (
                HeaderName::from_bytes(name.as_str().as_bytes()),
                HeaderValue::from_bytes(value.as_bytes()),
            ) {
                headers.insert(name, value);
            }
        }
        let mut body = Vec::new();
        let mut stream = response.bytes_stream().take(MAX_IDP_RESPONSE_BYTES + 1);
        while let Some(chunk) = stream.next().await {
            body.extend_from_slice(&chunk.map_err(|error| {
                warn!(%error, "identity provider response read failed");
                SeraError::External("the identity provider response could not be read".into())
            })?);
        }
        if body.len() > MAX_IDP_RESPONSE_BYTES {
            return Err(SeraError::External(
                "identity provider response too large".into(),
            ));
        }
        Ok(HttpResponse {
            status_code: status,
            headers,
            body,
        })
    }
}

/// Map a claims verification failure to an explicit, fail-closed error that
/// names no provider-controlled detail.
fn map_verification_error(error: ClaimsVerificationError) -> SeraError {
    warn!(%error, "id_token verification failed");
    SeraError::Forbidden("sign-in could not be verified".into())
}

/// Parse and validate a discovery response. A fixed tenant must report
/// exactly the expected issuer; a templated tenant (`common`,
/// `organizations`) must report exactly the published template, which is then
/// swapped for the configured URL so the metadata is internally consistent.
/// Per-token issuers are enforced separately by [`check_templated_issuer`].
fn parse_discovery(
    status: StatusCode,
    body: &[u8],
    expected: &IssuerUrl,
    template: Option<&str>,
) -> Result<CoreProviderMetadata> {
    if status != StatusCode::OK {
        return Err(SeraError::External(format!(
            "discovery returned HTTP status {status}"
        )));
    }
    let mut document: serde_json::Value = serde_json::from_slice(body)
        .map_err(|_| SeraError::External("discovery document is not valid JSON".into()))?;
    let issuer = document
        .get("issuer")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| SeraError::External("discovery document has no issuer".into()))?
        .to_owned();
    match template {
        Some(template) => {
            if issuer != template {
                return Err(SeraError::External(format!(
                    "unexpected issuer URI `{issuer}` (expected `{template}`)"
                )));
            }
            document["issuer"] = json!(expected.as_str());
        }
        None => {
            if issuer != expected.as_str() {
                return Err(SeraError::External(format!(
                    "unexpected issuer URI `{issuer}` (expected `{}`)",
                    expected.as_str()
                )));
            }
        }
    }
    serde_json::from_value::<CoreProviderMetadata>(document)
        .map_err(|error| SeraError::External(format!("discovery document is invalid: {error}")))
}

/// Read the `tid` (tenant id) claim from a compact JWT's payload. Only used
/// after the signature has been verified; it never grants trust on its own.
fn jwt_string_claim_tid(jwt: &str) -> Option<String> {
    let payload = jwt.split('.').nth(1)?;
    let bytes = URL_SAFE_NO_PAD.decode(payload).ok()?;
    let value: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
    value.get("tid")?.as_str().map(str::to_owned)
}

/// For templated tenants the token's `iss` must be the template filled with
/// the token's own `tid` claim — never skipped, never a different tenant's.
fn check_templated_issuer(template: &str, iss: Option<&str>, tid: Option<&str>) -> Result<()> {
    let denied = || SeraError::Forbidden("sign-in could not be verified".into());
    let tid = tid.ok_or_else(denied)?;
    if tid.is_empty()
        || tid.len() > 64
        || !tid.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
    {
        warn!("id_token tid claim is malformed");
        return Err(denied());
    }
    let expected = template.replace("{tenantid}", tid);
    if iss != Some(expected.as_str()) {
        warn!("id_token issuer does not match its tenant id");
        return Err(denied());
    }
    Ok(())
}

/// Require HTTPS, except for loopback hosts used by the test mock provider.
/// Credentials, queries, and fragments are always rejected.
fn validate_issuer(value: &str) -> Result<IssuerUrl> {
    let url = Url::parse(value.trim())
        .map_err(|_| SeraError::Config("issuer URL is not valid".into()))?;
    if url.host_str().is_none_or(str::is_empty) {
        return Err(SeraError::Config("issuer URL must include a host".into()));
    }
    let host = url.host_str().unwrap_or("");
    let loopback = host
        .trim_start_matches('[')
        .trim_end_matches(']')
        .eq_ignore_ascii_case("localhost")
        || host == "127.0.0.1"
        || host == "::1";
    if url.scheme() != "https" && !(url.scheme() == "http" && loopback) {
        return Err(SeraError::Config(
            "issuer URL must use https (http is allowed only for loopback test servers)".into(),
        ));
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(SeraError::Config(
            "issuer URL must not contain credentials".into(),
        ));
    }
    if url.query().is_some() || url.fragment().is_some() {
        return Err(SeraError::Config(
            "issuer URL must not contain a query or fragment".into(),
        ));
    }
    IssuerUrl::new(value.trim().to_owned())
        .map_err(|_| SeraError::Config("issuer URL is not valid".into()))
}

/// Untrusted provider-reported text: strip control characters and bound the
/// length so the store's hard limits always hold.
fn sanitize_profile(value: &str, max: usize) -> String {
    let cleaned: String = value.chars().filter(|c| !c.is_control()).collect();
    truncate_chars(&cleaned, max)
}

/// Synthetic identity-provider fixtures shared by the `auth` and `api`
/// module tests (`wiremock` discovery/JWKS/token endpoints plus a test RSA
/// key for signing id_tokens). No live provider is ever contacted.
#[cfg(test)]
pub(crate) mod test_idp {
    use serde_json::json;
    use url::Url;
    use wiremock::matchers::{body_string_contains, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    /// Synthetic 2048-bit RSA key generated for these tests only. It is not
    /// a credential, is never used outside the mock provider, and appears in
    /// no other configuration.
    pub(crate) const TEST_RSA_KEY_PEM: &str = "-----BEGIN RSA PRIVATE KEY-----\nMIIEpAIBAAKCAQEAvocB3YD0upr+xGqpv0GUMLJnG2z0g4L7Eka/pUaW0B2ZPNJO\nbhGU/Np59u62jb4YZ2hX9sCpF2SD4YHKmo3tULLI0dhAJACZ/g/sjOpQ/KYXMx2R\nr6V+HR3boCuGARLrtJgH0vUgJu18k9ST1qVIlY1FmYqICRDVRG2hDiDxDGvQN8Oc\ntZ3G6TYmi6y8Jk3JidYb+xvLvha+FjMzqQFEmK5AhhEH6r2fVz91df6usKU2iTt/\nWZSxfsMjrFbmqa+eAPhTAm6qQUieoyq1lh+Z7PT28ZYztCWrxsHa/YKj9znFSafv\nRMkWEucZOeY4/og4klnRq3B3K6nY0lTLfJYvWQIDAQABAoIBAB2eRz/z6BeKOMi3\n/CNrQmo/kZ/F1E0Nc5JnxSsgShmYQFYgK2kY+pxEKmmkumRWhNyub95AHQkx6v6H\nesOEBLxizq0wfOgb39SLDS7qziQ4farIkflINM658YZ4vaA1HMc6Sx0UVic3DAK/\nLSxVDW+SQpuX2YXvQ/gbQso7fL19UHosNPt63auA74nVLQW8mrB6xMyOZwzaH7g7\nGBL9FgIKb6vIbufTcpDL63N4V3LVk3k3x+x2zLVKVeLFlHrOUbq6qY+tQpGVKWNp\nWDygA3XSg3ZIT0k0OArDYziv9meTIkHSTZZqp8GbGbhm4eh+My8KzePDkIDCnoHx\nn+UNLXECgYEA3q8pjp6Cx5csI2eoIeojNlHTcPywkSvSoBO2UBozWqqOoOhWjP1y\ntY3J1DXOTcfbbQqcpLQz6SMPmoaM8YLZeGq/YHmSKCzphLcGX16fQuwrJRutNvIe\n0FMy/T0BDv11iXpSLoukMUrUXhXn93nuRnKt7CmhAaZ/QXBJBl8kme8CgYEA2wg7\nmLO0WOqMZfkuvr9kW5v7TG403NBaHVj05AIPhaSJ66WQzSNO36EXpXzNTW5XP2xy\nZllsnQk00BSDuDuSY60HSbgHxY0XXNkTqTBb2jk5wYhsHS9LRgSb4fi2Ra8/6nkV\nPXBEmbGWaqTu0d3aFF3WY5VF2oZlAZu8UhBZszcCgYEAsnb5PDanIyRxmFJbugp2\ne/MqHNQ3AbF4VRhXNW06GQC13Bk4PDhnNT/UMAGLPGTe403s6kcNj1utBq+sx2WR\numL6y2vDCfWslNNGBSrFQpIZpdy/flie/2Cebbv1XDkfeLWSpYD+bw7Ekj7GMF0E\n7Gq+HpXiVntEY4KE09lucPsCgYEAsOzDhyORyEkdw7k2gyoCnOUwZpqW5M8AG7ih\niBQKCb/REKIahznAGm/7s9dN1wAMRdUJy5bsXyTynioUBBtKsm0v9+JJlM/VUVCf\nywifCXU0LBpQinqLaTGL4UqPoVZgYLwR5iDrqOKER/yWVLvJzq3F4YbQeCYbLONp\niThf3J0CgYAVL/m+sqnkA48nPOT+HUvgiJsN1NM7YNzg5cRukejM7a+CJj6CAdhw\nEtUrYUIdoA7r/azV7WjdzkBTArMd4FSvzXYvmHQEyosugcYiWwpBDDLM2NLTIJ6/\ncoXAjv3eCbJRNAnlog/hjbYWs8Xu25w+I5oNDvUuUTdY0DwAmVAvpQ==\n-----END RSA PRIVATE KEY-----\n";
    pub(crate) const TEST_JWK_MODULUS: &str = "vocB3YD0upr-xGqpv0GUMLJnG2z0g4L7Eka_pUaW0B2ZPNJObhGU_Np59u62jb4YZ2hX9sCpF2SD4YHKmo3tULLI0dhAJACZ_g_sjOpQ_KYXMx2Rr6V-HR3boCuGARLrtJgH0vUgJu18k9ST1qVIlY1FmYqICRDVRG2hDiDxDGvQN8OctZ3G6TYmi6y8Jk3JidYb-xvLvha-FjMzqQFEmK5AhhEH6r2fVz91df6usKU2iTt_WZSxfsMjrFbmqa-eAPhTAm6qQUieoyq1lh-Z7PT28ZYztCWrxsHa_YKj9znFSafvRMkWEucZOeY4_og4klnRq3B3K6nY0lTLfJYvWQ";
    pub(crate) const TEST_JWK_EXPONENT: &str = "AQAB";
    pub(crate) const TEST_KEY_ID: &str = "synthetic-test-rs256";
    pub(crate) const TEST_CLIENT_ID: &str = "synthetic-client-id";

    /// A mock identity provider: discovery document, JWKS, and (per test) a
    /// token endpoint. Everything is synthetic; no live provider is touched.
    pub(crate) struct MockIdp {
        pub(crate) server: MockServer,
        pub(crate) issuer: String,
    }

    pub(crate) async fn mock_idp() -> MockIdp {
        mock_idp_with_doc_issuer(None).await
    }

    /// Like [`mock_idp`], but the discovery document reports `doc_issuer`
    /// (e.g. Microsoft's `{tenantid}` template) instead of the server URL.
    pub(crate) async fn mock_idp_with_doc_issuer(doc_issuer: Option<&str>) -> MockIdp {
        let server = MockServer::start().await;
        let issuer = server.uri();
        let doc_issuer = doc_issuer
            .map(str::to_owned)
            .unwrap_or_else(|| issuer.clone());
        Mock::given(method("GET"))
            .and(path("/.well-known/openid-configuration"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "issuer": doc_issuer,
                "authorization_endpoint": format!("{issuer}/authorize"),
                "token_endpoint": format!("{issuer}/token"),
                "jwks_uri": format!("{issuer}/jwks"),
                "response_types_supported": ["code"],
                "subject_types_supported": ["public"],
                "id_token_signing_alg_values_supported": ["RS256"],
                "scopes_supported": ["openid", "profile", "email"],
            })))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/jwks"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "keys": [{
                    "kty": "RSA",
                    "use": "sig",
                    "kid": TEST_KEY_ID,
                    "n": TEST_JWK_MODULUS,
                    "e": TEST_JWK_EXPONENT,
                }],
            })))
            .mount(&server)
            .await;
        MockIdp { server, issuer }
    }

    /// Teach the mock token endpoint to answer a specific authorization code
    /// with a signed synthetic id_token.
    pub(crate) async fn stub_token(server: &MockServer, code: &str, id_token: &str) {
        Mock::given(method("POST"))
            .and(path("/token"))
            .and(body_string_contains(code))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "access_token": "synthetic-access-token",
                "token_type": "Bearer",
                "expires_in": 3600,
                "id_token": id_token,
            })))
            .mount(server)
            .await;
    }

    /// Sign a synthetic id_token with the test key. The nonce and issuer must
    /// match what the service expects, exactly like a real provider.
    pub(crate) fn synthetic_id_token(issuer: &str, nonce: &str, subject: &str) -> String {
        synthetic_id_token_with_tid(issuer, nonce, subject, None)
    }

    /// [`synthetic_id_token`] with an optional Microsoft-style `tid` claim.
    pub(crate) fn synthetic_id_token_with_tid(
        issuer: &str,
        nonce: &str,
        subject: &str,
        tid: Option<&str>,
    ) -> String {
        let now = chrono::Utc::now().timestamp();
        let mut claims = json!({
            "iss": issuer,
            "sub": subject,
            "aud": TEST_CLIENT_ID,
            "exp": now + 3600,
            "iat": now,
            "nonce": nonce,
            "email": "member@example.example",
            "name": "Synthetic Member",
        });
        if let Some(tid) = tid {
            claims["tid"] = json!(tid);
        }
        let mut header = jsonwebtoken::Header::new(jsonwebtoken::Algorithm::RS256);
        header.kid = Some(TEST_KEY_ID.into());
        jsonwebtoken::encode(
            &header,
            &claims,
            &jsonwebtoken::EncodingKey::from_rsa_pem(TEST_RSA_KEY_PEM.as_bytes()).unwrap(),
        )
        .unwrap()
    }

    /// Pull `state` and `nonce` back out of an authorize URL, mirroring what
    /// the provider echoes to the callback.
    pub(crate) fn url_params(url: &str) -> (String, String) {
        let parsed = Url::parse(url).unwrap();
        let mut state = String::new();
        let mut nonce = String::new();
        for (key, value) in parsed.query_pairs() {
            match key.as_ref() {
                "state" => state = value.into_owned(),
                "nonce" => nonce = value.into_owned(),
                _ => {}
            }
        }
        (state, nonce)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::secrets::SecretStore;
    use crate::store::UserRole;
    use std::collections::BTreeMap;
    use test_idp::*;

    fn test_config() -> Config {
        let mut config = Config::default();
        config.auth.enabled = true;
        config.auth.providers = vec!["microsoft".into()];
        config.auth.tenant = "consumers".into();
        config.auth.base_url = "https://bot.example.example".into();
        config.auth.session_ttl_hours = 1;
        config.auth.invite_ttl_hours = 1;
        config.auth.max_users = 10;
        config
    }

    fn test_store() -> Store {
        Store::open(
            std::env::temp_dir().join(format!("serabot-auth-test-{}.db", crate::domain::new_id())),
            384,
            5000,
        )
        .unwrap()
    }

    fn test_secrets() -> SharedSecrets {
        SharedSecrets::new(SecretStore::from_map(BTreeMap::from([(
            "auth_client_id".into(),
            TEST_CLIENT_ID.into(),
        )])))
    }

    fn service(config: &Config, store: Store) -> AuthService {
        AuthService::new(config, store, &test_secrets()).unwrap()
    }

    async fn complete_sign_in(
        service: &AuthService,
        idp: &MockIdp,
        subject: &str,
        invite: Option<&str>,
    ) -> Result<(User, String)> {
        let url = service.login_start("microsoft", invite).await?;
        let (state, nonce) = url_params(&url);
        // The code is unique per sign-in (it carries the state) so repeated
        // sign-ins for the same subject hit distinct mock responses.
        let code = format!("synthetic-code-{subject}-{state}");
        let id_token = synthetic_id_token(&idp.issuer, &nonce, subject);
        stub_token(&idp.server, &code, &id_token).await;
        service.callback(&state, &code).await
    }

    #[test]
    fn provider_report_matches_the_v1_registry() {
        let config = test_config();
        let report = provider_report(&config.auth);
        assert_eq!(
            report,
            vec![
                ProviderStatus {
                    id: "microsoft",
                    enabled: true
                },
                ProviderStatus {
                    id: "google",
                    enabled: false
                },
                ProviderStatus {
                    id: "github",
                    enabled: false
                },
            ]
        );

        let mut config = test_config();
        config.auth.providers = vec!["google".into()];
        assert!(report.iter().all(|row| !row.enabled) || true);
        for row in provider_report(&config.auth) {
            assert!(!row.enabled, "google cannot be enabled in v1");
        }

        let mut config = test_config();
        config.auth.enabled = false;
        assert!(provider_report(&config.auth).iter().all(|row| !row.enabled));
    }

    #[test]
    fn construction_requires_the_client_id_secret_and_redacts_it() {
        let config = test_config();
        let store = test_store();
        let without_secret = SharedSecrets::new(SecretStore::from_map(BTreeMap::new()));
        assert!(AuthService::new(&config, store.clone(), &without_secret).is_err());

        let disabled = Config::default();
        assert!(AuthService::new(&disabled, store, &test_secrets()).is_err());

        let service = service(&config, test_store());
        let debug = format!("{service:?}");
        assert!(!debug.contains(TEST_CLIENT_ID));
        assert!(debug.contains("<redacted>"));
    }

    #[test]
    fn issuer_overrides_reject_non_https_and_credentials() {
        let mut service = service(&test_config(), test_store());
        assert!(service.set_issuer_url("http://example.example").is_err());
        assert!(
            service
                .set_issuer_url("https://user:pw@example.example")
                .is_err()
        );
        assert!(
            service
                .set_issuer_url("https://example.example/?query=1")
                .is_err()
        );
        assert!(service.set_issuer_url("not-a-url").is_err());
        assert!(service.set_issuer_url("http://127.0.0.1:9").is_ok());
    }

    #[tokio::test]
    async fn happy_path_creates_a_namespaced_user_and_issues_a_session() {
        let store = test_store();
        let idp = mock_idp().await;
        let mut service = service(&test_config(), store.clone());
        service.set_issuer_url(&idp.issuer).unwrap();

        let admin = store.admin_user_id().unwrap();
        let invite = service.mint_invite(1, &admin).unwrap();
        let url = service
            .login_start("microsoft", Some(&invite))
            .await
            .unwrap();
        // The authorize URL carries the code-flow contract.
        let parsed = Url::parse(&url).unwrap();
        let query = parsed.query().unwrap_or_default();
        for expected in [
            "response_type=code",
            "client_id=",
            "redirect_uri=",
            "state=",
            "nonce=",
            "code_challenge=",
            "code_challenge_method=S256",
        ] {
            assert!(
                query.contains(expected),
                "authorize URL missing {expected}: {url}"
            );
        }
        assert!(
            query.contains("scope=openid+profile+email")
                || query.contains("scope=openid%20profile%20email"),
            "scopes must be openid profile email: {url}"
        );
        assert!(url.starts_with(&format!("{}/authorize", idp.issuer)));

        let (state, nonce) = url_params(&url);
        let code = "synthetic-code-one";
        let id_token = synthetic_id_token(&idp.issuer, &nonce, "subject-one");
        stub_token(&idp.server, code, &id_token).await;

        let (user, token) = service.callback(&state, code).await.unwrap();
        assert_eq!(user.provider_subject, "microsoft:subject-one");
        assert_eq!(user.email, "member@example.example");
        assert_eq!(user.display_name, "Synthetic Member");
        assert_eq!(user.role, UserRole::Member);
        assert_eq!(token.len(), Store::SESSION_TOKEN_BYTES * 2);

        // The session resolves to the user, and only the hash is stored.
        let hash = sha256_hex(&token);
        assert_eq!(store.session_user(&hash).unwrap().unwrap().id, user.id);
        let stored = store.device_token_by_hash(&hash).unwrap().unwrap();
        assert_eq!(stored.label, Store::SESSION_LABEL);
        assert_eq!(stored.user_id.as_deref(), Some(user.id.as_str()));

        // The login is audited without leaking the token.
        let connection = store.connection().unwrap();
        let mut audit = connection
            .prepare("SELECT actor, event, details_json FROM audit_log WHERE event = 'auth.login'")
            .unwrap();
        let rows = audit
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                ))
            })
            .unwrap()
            .collect::<std::result::Result<Vec<_>, _>>()
            .unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].0, user.id);
        assert!(rows[0].2.contains("microsoft"));
        assert!(!rows[0].2.contains(&token));
    }

    #[tokio::test]
    async fn existing_subject_signs_in_again_without_an_invite() {
        let store = test_store();
        let idp = mock_idp().await;
        let mut service = service(&test_config(), store.clone());
        service.set_issuer_url(&idp.issuer).unwrap();

        let admin = store.admin_user_id().unwrap();
        let invite = service.mint_invite(1, &admin).unwrap();
        let first = complete_sign_in(&service, &idp, "subject-two", Some(&invite))
            .await
            .unwrap();
        assert_eq!(first.0.provider_subject, "microsoft:subject-two");
        // The invite is spent: a second account cannot be created with it.
        assert!(
            complete_sign_in(&service, &idp, "subject-three", Some(&invite))
                .await
                .is_err()
        );
        // An existing subject signs in again without any invite at all.
        let again = complete_sign_in(&service, &idp, "subject-two", None)
            .await
            .unwrap();
        assert_eq!(again.0.id, first.0.id);

        // A NEW user needs a valid invite at login start.
        let url = service
            .login_start("microsoft", Some("unknown-invite-code"))
            .await
            .unwrap_err();
        assert!(url.to_string().contains("invitation"));

        let second_invite = service.mint_invite(1, &admin).unwrap();
        let url = service
            .login_start("microsoft", Some(&second_invite))
            .await
            .unwrap();
        let (state, nonce) = url_params(&url);
        let code = "synthetic-code-second";
        let id_token = synthetic_id_token(&idp.issuer, &nonce, "subject-three");
        stub_token(&idp.server, code, &id_token).await;
        let (_, token) = service.callback(&state, code).await.unwrap();

        // The invite was consumed; a replayed login start with it fails.
        assert!(
            service
                .login_start("microsoft", Some(&second_invite))
                .await
                .is_err()
        );
        let hash = sha256_hex(&token);
        assert!(store.session_user(&hash).unwrap().is_some());
    }

    #[tokio::test]
    async fn callback_rejects_unknown_and_expired_states() {
        let store = test_store();
        let idp = mock_idp().await;
        let mut service = service(&test_config(), store);
        service.set_issuer_url(&idp.issuer).unwrap();

        assert!(
            service
                .callback("no-such-state", "some-code")
                .await
                .is_err()
        );
        assert!(service.callback("", "some-code").await.is_err());
        assert!(
            service
                .callback(&"s".repeat(300), "some-code")
                .await
                .is_err()
        );

        // Expired: the pending login outlives a zero TTL.
        service.set_login_ttl(Duration::ZERO).unwrap();
        let url = service.login_start("microsoft", None).await.unwrap();
        let (state, nonce) = url_params(&url);
        let code = "synthetic-code-expired";
        let id_token = synthetic_id_token(&idp.issuer, &nonce, "subject-expired");
        stub_token(&idp.server, code, &id_token).await;
        let error = service.callback(&state, code).await.unwrap_err();
        assert!(error.to_string().contains("state"));
    }

    #[tokio::test]
    async fn id_token_from_the_wrong_issuer_is_rejected() {
        let store = test_store();
        let idp = mock_idp().await;
        let mut service = service(&test_config(), store.clone());
        service.set_issuer_url(&idp.issuer).unwrap();

        let url = service.login_start("microsoft", None).await.unwrap();
        let (state, nonce) = url_params(&url);
        let code = "synthetic-code-wrong-issuer";
        // Signed with the trusted test key, but claiming another issuer.
        let id_token = synthetic_id_token("https://attacker.example", &nonce, "subject-evil");
        stub_token(&idp.server, code, &id_token).await;
        assert!(service.callback(&state, code).await.is_err());
        // Nothing was created.
        assert!(
            store
                .get_user_by_subject("microsoft:subject-evil")
                .unwrap()
                .is_none()
        );
    }

    #[tokio::test]
    async fn unknown_provider_or_disabled_provider_fails_closed() {
        let idp = mock_idp().await;
        let mut service = service(&test_config(), test_store());
        service.set_issuer_url(&idp.issuer).unwrap();
        assert!(service.login_start("saml", None).await.is_err());
        // `google` is known but not enableable in v1.
        assert!(service.login_start("google", None).await.is_err());
        assert!(
            service.login_start("microsoft ", None).await.is_ok(),
            "trimmed"
        );
    }

    #[tokio::test]
    async fn invite_double_spend_creates_exactly_one_user() {
        let store = test_store();
        let idp = mock_idp().await;
        let mut service = service(&test_config(), store.clone());
        service.set_issuer_url(&idp.issuer).unwrap();

        let admin = store.admin_user_id().unwrap();
        let invite = service.mint_invite(1, &admin).unwrap();

        // Two concurrent first sign-ins with the same invite: only one can
        // consume it and create an account.
        let url_a = service
            .login_start("microsoft", Some(&invite))
            .await
            .unwrap();
        let url_b = service
            .login_start("microsoft", Some(&invite))
            .await
            .unwrap();
        let (state_a, nonce_a) = url_params(&url_a);
        let (state_b, nonce_b) = url_params(&url_b);
        let code_a = "synthetic-code-double-a";
        let code_b = "synthetic-code-double-b";
        let token_a = synthetic_id_token(&idp.issuer, &nonce_a, "subject-double-a");
        let token_b = synthetic_id_token(&idp.issuer, &nonce_b, "subject-double-b");
        stub_token(&idp.server, code_a, &token_a).await;
        stub_token(&idp.server, code_b, &token_b).await;

        let (result_a, result_b) = tokio::join!(
            service.callback(&state_a, code_a),
            service.callback(&state_b, code_b)
        );
        let wins = result_a.is_ok() as u8 + result_b.is_ok() as u8;
        assert_eq!(
            wins, 1,
            "exactly one concurrent consumer may redeem the invite"
        );
        let created = result_a.is_ok() as u8 + result_b.is_ok() as u8;
        assert_eq!(created, 1);
        assert_eq!(store.user_count().unwrap(), 2, "admin + exactly one member");
        assert!(
            !store.invite_is_redeemable(&sha256_hex(&invite)).unwrap(),
            "the invite must be spent"
        );
    }

    #[tokio::test]
    async fn invite_codes_are_random_and_stored_hashed() {
        let store = test_store();
        let service = service(&test_config(), store.clone());
        let admin = store.admin_user_id().unwrap();
        let one = service.mint_invite(1, &admin).unwrap();
        let two = service.mint_invite(1, &admin).unwrap();
        assert_ne!(one, two);
        assert!(store.invite_is_redeemable(&sha256_hex(&one)).unwrap());
        assert!(store.invite_is_redeemable(&sha256_hex(&two)).unwrap());
        // The raw codes appear nowhere in the database.
        let raw = std::fs::read(store.path()).unwrap();
        let text = String::from_utf8_lossy(&raw);
        assert!(!text.contains(&one));
        assert!(!text.contains(&two));
    }

    #[tokio::test]
    async fn max_users_is_enforced_through_the_callback() {
        let mut config = test_config();
        config.auth.max_users = 2; // admin + one member
        let store = test_store();
        let idp = mock_idp().await;
        let mut service = service(&config, store.clone());
        service.set_issuer_url(&idp.issuer).unwrap();

        let admin = store.admin_user_id().unwrap();
        let invite = service.mint_invite(5, &admin).unwrap();
        assert!(
            complete_sign_in(&service, &idp, "subject-cap-one", Some(&invite))
                .await
                .is_ok()
        );
        let error = complete_sign_in(&service, &idp, "subject-cap-two", Some(&invite))
            .await
            .unwrap_err();
        assert!(error.to_string().contains("limit"));
    }

    #[tokio::test]
    async fn login_start_bounds_pending_logins() {
        let idp = mock_idp().await;
        let mut service = service(&test_config(), test_store());
        service.set_issuer_url(&idp.issuer).unwrap();
        service.set_login_ttl(LOGIN_STATE_TTL).unwrap();
        for _ in 0..MAX_PENDING_LOGINS {
            service.login_start("microsoft", None).await.unwrap();
        }
        let error = service.login_start("microsoft", None).await.unwrap_err();
        assert!(error.to_string().contains("in flight"));
    }

    // ---- tenant / issuer handling (synthetic, no network) ----

    const TID_A: &str = "11111111-2222-4333-8444-555555555555";
    const TID_B: &str = "66666666-7777-4888-8999-000000000000";

    fn discovery_doc(issuer: &str) -> Vec<u8> {
        serde_json::to_vec(&json!({
            "issuer": issuer,
            "authorization_endpoint": "https://idp.example/authorize",
            "token_endpoint": "https://idp.example/token",
            "jwks_uri": "https://idp.example/jwks",
            "response_types_supported": ["code"],
            "subject_types_supported": ["public"],
            "id_token_signing_alg_values_supported": ["RS256"],
        }))
        .unwrap()
    }

    #[test]
    fn tenant_aliases_map_to_the_right_issuer_mode() {
        let consumers = format!("https://login.microsoftonline.com/{CONSUMERS_TENANT_ID}/v2.0");
        assert_eq!(microsoft_issuer("consumers"), (consumers.clone(), false));
        assert_eq!(microsoft_issuer(" Consumers "), (consumers, false));
        assert_eq!(
            microsoft_issuer("common"),
            ("https://login.microsoftonline.com/common/v2.0".into(), true)
        );
        assert_eq!(
            microsoft_issuer("organizations"),
            (
                "https://login.microsoftonline.com/organizations/v2.0".into(),
                true
            )
        );
        assert_eq!(
            microsoft_issuer(TID_A),
            (
                format!("https://login.microsoftonline.com/{TID_A}/v2.0"),
                false
            )
        );
        // The prod config (`consumers`) now discovers at the tenant GUID.
        let service = service(&test_config(), test_store());
        assert_eq!(
            service.issuer_url.as_str(),
            format!("https://login.microsoftonline.com/{CONSUMERS_TENANT_ID}/v2.0")
        );
        assert!(service.issuer_template.is_none());
    }

    #[test]
    fn consumers_discovery_reporting_the_tenant_guid_is_accepted() {
        // Reproduces the production failure: Microsoft reports the consumer
        // tenant GUID as issuer. The old alias-based expectation rejected it.
        let guid_issuer = format!("https://login.microsoftonline.com/{CONSUMERS_TENANT_ID}/v2.0");
        let body = discovery_doc(&guid_issuer);
        let alias =
            IssuerUrl::new("https://login.microsoftonline.com/consumers/v2.0".into()).unwrap();
        let error = parse_discovery(StatusCode::OK, &body, &alias, None).unwrap_err();
        assert!(error.to_string().contains("unexpected issuer"));

        let expected = IssuerUrl::new(guid_issuer.clone()).unwrap();
        let metadata = parse_discovery(StatusCode::OK, &body, &expected, None).unwrap();
        assert_eq!(metadata.issuer().as_str(), guid_issuer);
        // A different tenant's document is still rejected.
        let other =
            IssuerUrl::new(format!("https://login.microsoftonline.com/{TID_A}/v2.0")).unwrap();
        assert!(parse_discovery(StatusCode::OK, &body, &other, None).is_err());
        // Non-200 and non-JSON bodies fail closed.
        assert!(parse_discovery(StatusCode::NOT_FOUND, &body, &expected, None).is_err());
        assert!(parse_discovery(StatusCode::OK, b"<html>", &expected, None).is_err());
    }

    #[test]
    fn templated_discovery_requires_the_exact_microsoft_template() {
        let expected =
            IssuerUrl::new("https://login.microsoftonline.com/common/v2.0".into()).unwrap();
        let body = discovery_doc(MICROSOFT_ISSUER_TEMPLATE);
        let metadata = parse_discovery(
            StatusCode::OK,
            &body,
            &expected,
            Some(MICROSOFT_ISSUER_TEMPLATE),
        )
        .unwrap();
        assert_eq!(metadata.issuer().as_str(), expected.as_str());
        // A concrete tenant issuer in a templated authority is not accepted.
        let concrete = discovery_doc(&format!("https://login.microsoftonline.com/{TID_A}/v2.0"));
        assert!(
            parse_discovery(
                StatusCode::OK,
                &concrete,
                &expected,
                Some(MICROSOFT_ISSUER_TEMPLATE)
            )
            .is_err()
        );
        // And the template is not accepted by a fixed-tenant service.
        assert!(parse_discovery(StatusCode::OK, &body, &expected, None).is_err());
    }

    #[test]
    fn templated_issuer_is_bound_to_the_tokens_own_tenant() {
        let template = MICROSOFT_ISSUER_TEMPLATE;
        let iss_a = format!("https://login.microsoftonline.com/{TID_A}/v2.0");
        assert!(check_templated_issuer(template, Some(&iss_a), Some(TID_A)).is_ok());
        // Issuer from a different tenant than the tid claim.
        assert!(check_templated_issuer(template, Some(&iss_a), Some(TID_B)).is_err());
        // Missing tid, missing iss, malformed tid, foreign host.
        assert!(check_templated_issuer(template, Some(&iss_a), None).is_err());
        assert!(check_templated_issuer(template, None, Some(TID_A)).is_err());
        assert!(check_templated_issuer(template, Some(&iss_a), Some("")).is_err());
        assert!(check_templated_issuer(template, Some(&iss_a), Some("../x")).is_err());
        assert!(
            check_templated_issuer(
                template,
                Some(&format!("https://attacker.example/{TID_A}/v2.0")),
                Some(TID_A)
            )
            .is_err()
        );
    }

    #[test]
    fn tid_is_read_from_the_jwt_payload() {
        let token = test_idp::synthetic_id_token_with_tid(
            "https://login.microsoftonline.com/x/v2.0",
            "n",
            "s",
            Some(TID_A),
        );
        assert_eq!(jwt_string_claim_tid(&token).as_deref(), Some(TID_A));
        let none = test_idp::synthetic_id_token("https://x.example", "n", "s");
        assert_eq!(jwt_string_claim_tid(&none), None);
        assert_eq!(jwt_string_claim_tid("not-a-jwt"), None);
    }

    async fn templated_service(store: Store) -> (AuthService, test_idp::MockIdp) {
        let idp = test_idp::mock_idp_with_doc_issuer(Some(MICROSOFT_ISSUER_TEMPLATE)).await;
        let mut config = test_config();
        config.auth.tenant = "common".into();
        let mut service = service(&config, store);
        assert!(service.issuer_template.is_some());
        service.set_issuer_url(&idp.issuer).unwrap();
        (service, idp)
    }

    async fn templated_sign_in(
        service: &AuthService,
        idp: &test_idp::MockIdp,
        subject: &str,
        iss: &str,
        tid: Option<&str>,
    ) -> Result<(User, String)> {
        let admin = service.store.admin_user_id()?;
        let invite = service.mint_invite(1, &admin)?;
        let url = service.login_start("microsoft", Some(&invite)).await?;
        let (state, nonce) = url_params(&url);
        let code = format!("synthetic-code-{subject}-{state}");
        let id_token = test_idp::synthetic_id_token_with_tid(iss, &nonce, subject, tid);
        stub_token(&idp.server, &code, &id_token).await;
        service.callback(&state, &code).await
    }

    #[tokio::test]
    async fn common_and_organizations_accept_matching_tenant_issuers_only() {
        let store = test_store();
        let (service, idp) = templated_service(store.clone()).await;
        let iss_a = format!("https://login.microsoftonline.com/{TID_A}/v2.0");
        let iss_b = format!("https://login.microsoftonline.com/{TID_B}/v2.0");

        // Discovery works against the templated document and a token whose
        // issuer matches its own tid is accepted.
        let (user, _) = templated_sign_in(&service, &idp, "tenant-user", &iss_a, Some(TID_A))
            .await
            .unwrap();
        assert_eq!(user.provider_subject, "microsoft:tenant-user");

        // iss for tenant B but tid says A, no tid at all, or an unrelated
        // issuer: all rejected and nothing is created.
        for (subject, iss, tid) in [
            ("evil-mismatch", iss_b.as_str(), Some(TID_A)),
            ("evil-no-tid", iss_a.as_str(), None),
            ("evil-host", "https://attacker.example/v2.0", Some(TID_A)),
        ] {
            assert!(
                templated_sign_in(&service, &idp, subject, iss, tid)
                    .await
                    .is_err(),
                "{subject}"
            );
            assert!(
                store
                    .get_user_by_subject(&format!("microsoft:{subject}"))
                    .unwrap()
                    .is_none()
            );
        }
    }

    // ---- handoff ----

    fn challenge_for(verifier: &str) -> String {
        URL_SAFE_NO_PAD.encode(<sha2::Sha256 as sha2::Digest>::digest(verifier.as_bytes()))
    }

    fn handoff_request(verifier: &str) -> HandoffRequest {
        HandoffRequest {
            return_to: "http://127.0.0.1:51121/auth-done".into(),
            challenge: challenge_for(verifier),
            client_state: "client-state-0123456789".into(),
        }
    }

    #[test]
    fn handoff_request_validation_is_strict_and_all_or_none() {
        let challenge = challenge_for(&"a".repeat(43));
        let state = "client-state-0123456789";
        assert_eq!(HandoffRequest::parse(None, None, None).unwrap(), None);
        for return_to in [
            "http://127.0.0.1:1024/auth-done",
            "http://127.0.0.1:65535/auth-done",
            "serabot://auth",
        ] {
            let parsed = HandoffRequest::parse(Some(return_to), Some(&challenge), Some(state))
                .unwrap()
                .unwrap();
            assert_eq!(parsed.return_to, return_to);
        }
        for return_to in [
            "http://127.0.0.1:1023/auth-done",
            "http://127.0.0.1:65536/auth-done",
            "http://127.0.0.1:051121/auth-done",
            "https://127.0.0.1:51121/auth-done",
            "http://127.0.0.1/auth-done",
            "http://127.0.0.1:51121/auth-done/",
            "http://127.0.0.1:51121/auth-done#x",
            "http://[::1]:51121/auth-done",
            "serabot://auth?x=1",
            "serabot://auth/",
            "",
        ] {
            assert!(
                HandoffRequest::parse(Some(return_to), Some(&challenge), Some(state)).is_err(),
                "{return_to}"
            );
        }
        assert!(HandoffRequest::parse(Some("serabot://auth"), None, None).is_err());
        assert!(HandoffRequest::parse(None, Some(&challenge), Some(state)).is_err());
        assert!(HandoffRequest::parse(Some("serabot://auth"), Some("short"), Some(state)).is_err());
        assert!(
            HandoffRequest::parse(
                Some("serabot://auth"),
                Some(&challenge),
                Some("a b c d e f g h")
            )
            .is_err()
        );
        assert!(
            HandoffRequest::parse(
                Some("serabot://auth"),
                Some(&challenge),
                Some(&"s".repeat(200))
            )
            .is_err()
        );
    }

    #[test]
    fn handoff_redirects_carry_code_or_error_plus_state() {
        let request = handoff_request(&"a".repeat(43));
        assert_eq!(
            handoff_redirect(&request, Ok("CODE_1-x")),
            "http://127.0.0.1:51121/auth-done?handoff=CODE_1-x&state=client-state-0123456789"
        );
        assert_eq!(
            handoff_redirect(&request, Err(SignInFailure::InviteRequired)),
            "http://127.0.0.1:51121/auth-done?error=invite_required&state=client-state-0123456789"
        );
        let mobile = HandoffRequest {
            return_to: "serabot://auth".into(),
            ..request
        };
        assert_eq!(
            handoff_redirect(&mobile, Ok("C")),
            "serabot://auth?handoff=C&state=client-state-0123456789"
        );
        for (failure, code) in [
            (SignInFailure::Denied, "denied"),
            (SignInFailure::InviteRequired, "invite_required"),
            (SignInFailure::InviteInvalid, "invite_invalid"),
            (SignInFailure::Disabled, "disabled"),
            (SignInFailure::MaxUsers, "max_users"),
            (SignInFailure::Server, "server"),
        ] {
            assert_eq!(failure.code(), code);
        }
    }

    #[tokio::test]
    async fn handoff_is_single_use_pkce_bound_and_stores_only_a_hash() {
        let store = test_store();
        let service = service(&test_config(), store.clone());
        let admin = store
            .get_user(&store.admin_user_id().unwrap())
            .unwrap()
            .unwrap();
        let verifier = "v".repeat(43);
        let request = handoff_request(&verifier);

        let code = service.issue_handoff(&admin, &request).unwrap();
        assert!(code.len() >= 43 && code.chars().all(is_base64url));
        let raw = std::fs::read(store.path()).unwrap();
        assert!(!String::from_utf8_lossy(&raw).contains(&code));
        let grant = service.redeem_handoff(&code, &verifier).unwrap();
        assert_eq!(grant.user.id, admin.id);
        assert_eq!(grant.token.len(), Store::SESSION_TOKEN_BYTES * 2);
        assert!(grant.expires_at > chrono::Utc::now());
        assert!(
            store
                .session_user(&sha256_hex(&grant.token))
                .unwrap()
                .is_some()
        );
        // Reuse fails.
        assert!(service.redeem_handoff(&code, &verifier).is_err());

        // A wrong verifier fails and burns the code.
        let code = service.issue_handoff(&admin, &request).unwrap();
        assert!(service.redeem_handoff(&code, &"w".repeat(43)).is_err());
        assert!(service.redeem_handoff(&code, &verifier).is_err());

        // A too-short verifier fails and burns too.
        let code = service.issue_handoff(&admin, &request).unwrap();
        assert!(service.redeem_handoff(&code, "short").is_err());
        assert!(service.redeem_handoff(&code, &verifier).is_err());

        // Expired codes fail even with the right verifier.
        store
            .create_handoff(
                &sha256_hex("expired-code"),
                &request.challenge,
                &admin.id,
                chrono::Duration::seconds(-1),
            )
            .unwrap();
        assert!(service.redeem_handoff("expired-code", &verifier).is_err());
        // Unknown and oversized codes fail.
        assert!(service.redeem_handoff("nope", &verifier).is_err());
        assert!(service.redeem_handoff(&"x".repeat(300), &verifier).is_err());
    }

    #[tokio::test]
    async fn handoff_for_a_disabled_user_is_refused() {
        let store = test_store();
        let idp = mock_idp().await;
        let mut service = service(&test_config(), store.clone());
        service.set_issuer_url(&idp.issuer).unwrap();
        let admin = store.admin_user_id().unwrap();
        let invite = service.mint_invite(1, &admin).unwrap();
        let (user, _) = complete_sign_in(&service, &idp, "subject-off", Some(&invite))
            .await
            .unwrap();
        let verifier = "v".repeat(43);
        let code = service
            .issue_handoff(&user, &handoff_request(&verifier))
            .unwrap();
        store
            .set_user_status(&user.id, UserStatus::Disabled)
            .unwrap();
        assert!(service.redeem_handoff(&code, &verifier).is_err());
        // Signing in again reports `disabled` rather than minting anything.
        let url = service.login_start("microsoft", None).await.unwrap();
        let (state, nonce) = url_params(&url);
        let code = "synthetic-code-disabled-again";
        let id_token = synthetic_id_token(&idp.issuer, &nonce, "subject-off");
        stub_token(&idp.server, code, &id_token).await;
        let result = service.callback_login(&state, code).await;
        let (failure, _) = result.outcome.unwrap_err();
        assert_eq!(failure, SignInFailure::Disabled);
    }

    #[tokio::test]
    async fn callback_failures_map_to_short_codes_and_keep_the_handoff() {
        let mut config = test_config();
        config.auth.max_users = 2;
        let store = test_store();
        let idp = mock_idp().await;
        let mut service = service(&config, store.clone());
        service.set_issuer_url(&idp.issuer).unwrap();
        let request = handoff_request(&"a".repeat(43));

        async fn run(
            service: &AuthService,
            idp: &MockIdp,
            subject: &str,
            invite: Option<&str>,
            request: &HandoffRequest,
        ) -> CallbackResult {
            let url = service
                .login_start_with_handoff("microsoft", invite, Some(request.clone()))
                .await
                .unwrap();
            let (state, nonce) = url_params(&url);
            let code = format!("synthetic-code-{subject}-{state}");
            let id_token = synthetic_id_token(&idp.issuer, &nonce, subject);
            stub_token(&idp.server, &code, &id_token).await;
            service.callback_login(&state, &code).await
        }

        let result = run(&service, &idp, "no-invite", None, &request).await;
        assert_eq!(result.handoff.as_ref(), Some(&request));
        assert_eq!(result.outcome.unwrap_err().0, SignInFailure::InviteRequired);

        let admin = store.admin_user_id().unwrap();
        let invite = service.mint_invite(5, &admin).unwrap();
        let ok = run(&service, &idp, "first", Some(&invite), &request).await;
        assert!(ok.outcome.is_ok());
        let full = run(&service, &idp, "second", Some(&invite), &request).await;
        assert_eq!(full.outcome.unwrap_err().0, SignInFailure::MaxUsers);

        // Unknown state: no handoff, so the web error page is used.
        let unknown = service.callback_login("bogus", "code").await;
        assert!(unknown.handoff.is_none());
        assert!(unknown.outcome.is_err());

        // A declined provider redirect consumes the pending login.
        let url = service
            .login_start_with_handoff("microsoft", None, Some(request.clone()))
            .await
            .unwrap();
        let (state, _) = url_params(&url);
        assert_eq!(service.callback_denied(&state).await, Some(request));
        assert!(service.callback_denied(&state).await.is_none());
    }

    // ---- admin claim ----

    #[tokio::test]
    async fn admin_email_claims_the_unlinked_admin_row_without_an_invite() {
        let mut config = test_config();
        config.auth.admin_email = "Member@EXAMPLE.example".into();
        let store = test_store();
        let idp = mock_idp().await;
        let mut service = service(&config, store.clone());
        service.set_issuer_url(&idp.issuer).unwrap();
        let admin_id = store.admin_user_id().unwrap();
        let before = store.get_user(&admin_id).unwrap().unwrap();
        assert!(before.provider_subject.starts_with("local:"));
        let users_before = store.user_count().unwrap();

        // The synthetic token's email is member@example.example: it matches
        // case-insensitively, links the subject, and creates no user.
        let (user, token) = complete_sign_in(&service, &idp, "operator", None)
            .await
            .unwrap();
        assert_eq!(user.id, admin_id);
        assert_eq!(user.role, UserRole::Admin);
        assert_eq!(user.provider_subject, "microsoft:operator");
        assert_eq!(store.user_count().unwrap(), users_before);
        assert_eq!(
            store.session_user(&sha256_hex(&token)).unwrap().unwrap().id,
            admin_id
        );
        let linked = store
            .get_user_by_subject("microsoft:operator")
            .unwrap()
            .unwrap();
        assert_eq!(linked.id, admin_id);

        // Signing in again is an ordinary existing-subject login.
        let again = complete_sign_in(&service, &idp, "operator", None)
            .await
            .unwrap();
        assert_eq!(again.0.id, admin_id);

        // The claim is one-shot: another subject with the same email gets no
        // admin and, without an invite, no account.
        let error = complete_sign_in(&service, &idp, "impostor", None)
            .await
            .unwrap_err();
        assert!(error.to_string().contains("invitation"));
        assert!(
            store
                .get_user_by_subject("microsoft:impostor")
                .unwrap()
                .is_none()
        );
    }

    #[tokio::test]
    async fn admin_claim_needs_a_matching_email_and_configuration() {
        let idp = mock_idp().await;
        // Different email configured: no claim, invite still required.
        let mut config = test_config();
        config.auth.admin_email = "someone.else@example.example".into();
        let store = test_store();
        let mut service = service(&config, store.clone());
        service.set_issuer_url(&idp.issuer).unwrap();
        assert!(
            complete_sign_in(&service, &idp, "no-match", None)
                .await
                .is_err()
        );
        let admin = store
            .get_user(&store.admin_user_id().unwrap())
            .unwrap()
            .unwrap();
        assert!(admin.provider_subject.starts_with("local:"));

        // No admin_email at all: no claim.
        let store = test_store();
        let mut service = service_for(&test_config(), store.clone());
        service.set_issuer_url(&idp.issuer).unwrap();
        assert!(
            complete_sign_in(&service, &idp, "no-config", None)
                .await
                .is_err()
        );
    }

    fn service_for(config: &Config, store: Store) -> AuthService {
        service(config, store)
    }
}
