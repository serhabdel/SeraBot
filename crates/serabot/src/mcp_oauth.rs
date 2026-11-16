//! OAuth for hosted MCP servers (v0.51, M2): the generic sign-in a marketplace
//! plugin with `auth.kind: oauth` uses. It generalises `elevenlabs_oauth`
//! (CIMD client id, fixed loopback redirect) to servers found by discovery.
//!
//! Flow, each step bounded to 64 KiB and 20 s:
//!
//! 1. Protected-resource metadata (RFC 9728) names the authorization server.
//! 2. Authorization-server metadata (RFC 8414, then OIDC discovery) names the
//!    endpoints.
//! 3. The client identity is, in order: SeraBot's client metadata document
//!    (CIMD) when the server supports it and the manifest asks for it, dynamic
//!    registration (RFC 7591), then the manifest's configured client id.
//! 4. Authorization code with PKCE S256 and an RFC 8707 `resource`.
//!
//! Every endpoint, the metadata documents included, must be `https` and resolve
//! only to public addresses ([`crate::plugin_runtime::resolve_checked`]); the
//! request is then pinned to the addresses that were checked. Redirects are
//! never followed. Tokens are sealed per user under
//! `marketplace.<slug>.<account>.oauth` and never appear in an error, a log or
//! the `Debug` output.
//!
//! Failure outcomes (see [`OauthAccounts::refresh_account`]):
//!
//! | server says             | tokens | registration | account state   |
//! |-------------------------|--------|--------------|-----------------|
//! | `invalid_grant`         | drop   | keep         | `needs_sign_in` |
//! | `invalid_client`        | drop   | drop         | `needs_sign_in` |
//! | network, 5xx, anything  | keep   | keep         | `unreachable`   |

use std::future::Future;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;
use secrecy::ExposeSecret;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use url::Url;

use crate::auth_flows::pkce::{pkce_s256, pkce_token_ok, random_state, state_ok};
use crate::auth_flows::read_capped;
use crate::elevenlabs_oauth::{cimd_client_id, oauth_callback_uri};
use crate::error::{Result, SeraError};
use crate::marketplace::OauthSpec;
use crate::plugin_runtime::resolve_checked;
use crate::store::Store;
use crate::user_secrets::UserSecrets;

/// Longest any one request may take.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(20);
/// Longest access or refresh token accepted. Two of them plus the rest of the
/// record stay inside the sealed-value limit.
const MAX_TOKEN_CHARS: usize = 4 * 1024;
const MAX_ENDPOINT_CHARS: usize = 2 * 1024;
const MAX_TOKEN_LIFETIME_SECS: u64 = 86_400 * 7;
/// How close to expiry an access token counts as stale.
const EXPIRY_SKEW_SECS: i64 = 30;

type LookupFuture = Pin<Box<dyn Future<Output = std::io::Result<Vec<SocketAddr>>> + Send>>;
type Lookup = Arc<dyn Fn(String, u16) -> LookupFuture + Send + Sync>;

/// The network side of the flow: name resolution and the public-address rule.
#[derive(Clone)]
pub struct OauthNet {
    lookup: Lookup,
    /// Tests only: allow plain `http` to a loopback mock server.
    allow_loopback_http: bool,
}

impl OauthNet {
    /// The system resolver. Production uses only this.
    pub fn system() -> Self {
        Self {
            lookup: Arc::new(|host, port| {
                Box::pin(async move {
                    Ok(tokio::net::lookup_host((host.as_str(), port))
                        .await?
                        .collect())
                })
            }),
            allow_loopback_http: false,
        }
    }

    #[cfg(test)]
    pub(crate) fn loopback_for_tests() -> Self {
        Self {
            allow_loopback_http: true,
            ..Self::system()
        }
    }

    #[cfg(test)]
    pub(crate) fn with_lookup(lookup: Lookup) -> Self {
        Self {
            lookup,
            allow_loopback_http: false,
        }
    }

    fn is_test_loopback(&self, url: &Url) -> bool {
        self.allow_loopback_http
            && url.scheme() == "http"
            && matches!(url.host_str(), Some("127.0.0.1" | "localhost"))
    }

    /// The egress rule for an MCP server URL: [`crate::plugin_runtime::check_egress`],
    /// except that tests may name a loopback mock server.
    pub fn check_mcp_egress(&self, url: &str, hosts: &[String]) -> Result<()> {
        if self.is_loopback_test_url(url) {
            return Ok(());
        }
        crate::plugin_runtime::check_egress(url, hosts)
    }

    /// True only for a test net and a plain-http loopback URL.
    pub fn is_loopback_test_url(&self, url: &str) -> bool {
        Url::parse(url).is_ok_and(|parsed| self.is_test_loopback(&parsed))
    }

    /// The URL must be `https`, carry no credentials and no fragment, and name
    /// a host whose every address is public. Returns the pinned addresses.
    async fn check(
        &self,
        raw: &str,
    ) -> Result<(Url, Option<crate::plugin_runtime::PinnedAddresses>)> {
        if raw.len() > MAX_ENDPOINT_CHARS {
            return Err(SeraError::Validation("OAuth URL is too long".into()));
        }
        let url =
            Url::parse(raw).map_err(|_| SeraError::Validation("OAuth URL is invalid".into()))?;
        if self.is_test_loopback(&url) {
            return Ok((url, None));
        }
        if url.scheme() != "https" {
            return Err(SeraError::Forbidden(
                "OAuth endpoints must use https".into(),
            ));
        }
        if !url.username().is_empty() || url.password().is_some() || url.fragment().is_some() {
            return Err(SeraError::Forbidden(
                "OAuth URLs must not carry credentials or a fragment".into(),
            ));
        }
        let lookup = self.lookup.clone();
        let pinned = resolve_checked(url.as_str(), move |host, port| (lookup)(host, port)).await?;
        Ok((url, Some(pinned)))
    }

    async fn client(&self, raw: &str) -> Result<(Url, reqwest::Client)> {
        let (url, pinned) = self.check(raw).await?;
        let mut builder = reqwest::Client::builder()
            .timeout(REQUEST_TIMEOUT)
            .redirect(reqwest::redirect::Policy::none())
            .user_agent(crate::providers::USER_AGENT);
        if let Some(pinned) = &pinned {
            builder = builder.resolve_to_addrs(&pinned.host, &pinned.addrs);
        }
        let client = builder
            .build()
            .map_err(|_| SeraError::External("OAuth client could not start".into()))?;
        Ok((url, client))
    }

    /// Read-only check for an endpoint the server never calls itself (the
    /// browser does): same rules, no request.
    async fn check_only(&self, raw: &str) -> Result<()> {
        self.check(raw).await.map(|_| ())
    }

    async fn get(&self, raw: &str) -> Result<(u16, Vec<u8>)> {
        let (url, client) = self.client(raw).await?;
        let response = client
            .get(url)
            .header("accept", "application/json")
            .send()
            .await
            .map_err(|_| SeraError::External("OAuth server could not be reached".into()))?;
        let status = response.status().as_u16();
        Ok((status, read_capped(response).await?))
    }

    async fn post_form(&self, raw: &str, form: &[(&str, &str)]) -> Result<(u16, Vec<u8>)> {
        let (url, client) = self.client(raw).await?;
        let response = client
            .post(url)
            .header("accept", "application/json")
            .form(form)
            .send()
            .await
            .map_err(|_| SeraError::External("OAuth server could not be reached".into()))?;
        let status = response.status().as_u16();
        Ok((status, read_capped(response).await?))
    }

    async fn post_json(&self, raw: &str, body: &Value) -> Result<(u16, Vec<u8>)> {
        let (url, client) = self.client(raw).await?;
        let response = client
            .post(url)
            .header("accept", "application/json")
            .json(body)
            .send()
            .await
            .map_err(|_| SeraError::External("OAuth server could not be reached".into()))?;
        let status = response.status().as_u16();
        Ok((status, read_capped(response).await?))
    }
}

impl std::fmt::Debug for OauthNet {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.debug_struct("OauthNet").finish_non_exhaustive()
    }
}

/// What discovery learned about one MCP server's authorization.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Discovery {
    /// The RFC 8707 `resource`: the MCP server the token is for.
    pub resource: String,
    pub issuer: String,
    pub authorization_endpoint: String,
    pub token_endpoint: String,
    pub registration_endpoint: Option<String>,
    pub revocation_endpoint: Option<String>,
    /// The server accepts a client metadata document URL as `client_id`.
    pub cimd_supported: bool,
}

fn json_body(bytes: &[u8]) -> Option<Value> {
    serde_json::from_slice(bytes).ok()
}

fn same_url(a: &str, b: &str) -> bool {
    a.trim_end_matches('/') == b.trim_end_matches('/')
}

/// `https://host/.well-known/<suffix><path>`: the RFC 9728 and RFC 8414 rule
/// for inserting the well-known segment between host and path.
fn well_known(base: &Url, suffix: &str) -> Result<String> {
    let mut url = base.clone();
    let path = base.path().trim_end_matches('/').to_owned();
    url.set_path(&format!("/.well-known/{suffix}{path}"));
    url.set_query(None);
    url.set_fragment(None);
    Ok(url.to_string())
}

fn string_of<'a>(value: &'a Value, key: &str) -> Option<&'a str> {
    value.get(key).and_then(Value::as_str)
}

/// Find the endpoints for `mcp_url`. `metadata_url` overrides where the
/// protected-resource document is looked for.
pub async fn discover(
    net: &OauthNet,
    mcp_url: &str,
    metadata_url: Option<&str>,
) -> Result<Discovery> {
    let mcp =
        Url::parse(mcp_url).map_err(|_| SeraError::Validation("MCP URL is invalid".into()))?;
    let prm_url = match metadata_url {
        Some(url) => url.to_owned(),
        None => well_known(&mcp, "oauth-protected-resource")?,
    };
    let (status, body) = net.get(&prm_url).await?;
    let prm = (status == 200)
        .then(|| json_body(&body))
        .flatten()
        .ok_or_else(|| SeraError::External("the server publishes no OAuth metadata".into()))?;
    let resource = string_of(&prm, "resource")
        .ok_or_else(|| SeraError::External("OAuth resource metadata has no resource".into()))?;
    // RFC 9728 section 3.3: the resource must be the one that was asked about,
    // or a hostile metadata document could redirect the token's audience.
    if !same_url(resource, mcp_url) {
        return Err(SeraError::External(
            "OAuth resource metadata is for a different resource".into(),
        ));
    }
    let issuer = prm
        .get("authorization_servers")
        .and_then(Value::as_array)
        .and_then(|list| list.first())
        .and_then(Value::as_str)
        .ok_or_else(|| SeraError::External("the server names no authorization server".into()))?
        .to_owned();
    let issuer_url = Url::parse(&issuer)
        .map_err(|_| SeraError::External("authorization server is invalid".into()))?;

    let candidates = [
        well_known(&issuer_url, "oauth-authorization-server")?,
        well_known(&issuer_url, "openid-configuration")?,
        format!(
            "{}/.well-known/openid-configuration",
            issuer.trim_end_matches('/')
        ),
    ];
    let mut meta = None;
    let mut last_error = None;
    for candidate in candidates {
        match net.get(&candidate).await {
            Ok((200, body)) => {
                if let Some(value) = json_body(&body)
                    && string_of(&value, "issuer").is_some_and(|found| same_url(found, &issuer))
                {
                    meta = Some(value);
                    break;
                }
            }
            Ok(_) => {}
            Err(error) => last_error = Some(error),
        }
    }
    let Some(meta) = meta else {
        return Err(last_error.unwrap_or_else(|| {
            SeraError::External("the authorization server publishes no usable metadata".into())
        }));
    };
    if let Some(methods) = meta
        .get("code_challenge_methods_supported")
        .and_then(Value::as_array)
        && !methods.iter().any(|m| m.as_str() == Some("S256"))
    {
        return Err(SeraError::External(
            "the authorization server does not offer PKCE S256".into(),
        ));
    }
    let required = |key: &str| {
        string_of(&meta, key)
            .map(str::to_owned)
            .ok_or_else(|| SeraError::External(format!("authorization metadata lacks {key}")))
    };
    let authorization_endpoint = required("authorization_endpoint")?;
    let token_endpoint = required("token_endpoint")?;
    let registration_endpoint = string_of(&meta, "registration_endpoint").map(str::to_owned);
    let revocation_endpoint = string_of(&meta, "revocation_endpoint").map(str::to_owned);
    // Every endpoint the flow will use must be public and https.
    net.check_only(&authorization_endpoint).await?;
    net.check_only(&token_endpoint).await?;
    for endpoint in registration_endpoint
        .iter()
        .chain(revocation_endpoint.iter())
    {
        net.check_only(endpoint).await?;
    }
    let cimd_supported = meta
        .get("client_id_metadata_document_supported")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    Ok(Discovery {
        resource: resource.to_owned(),
        issuer,
        authorization_endpoint,
        token_endpoint,
        registration_endpoint,
        revocation_endpoint,
        cimd_supported,
    })
}

/// Where a client identity came from.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ClientSource {
    Cimd,
    Dcr,
    Configured,
}

/// The identity SeraBot presents to one authorization server.
#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ClientRegistration {
    pub client_id: String,
    /// Set only when dynamic registration issued one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_secret: Option<String>,
    pub source: ClientSource,
}

impl std::fmt::Debug for ClientRegistration {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ClientRegistration")
            .field("client_id", &self.client_id)
            .field(
                "client_secret",
                &self.client_secret.as_ref().map(|_| "<redacted>"),
            )
            .field("source", &self.source)
            .finish()
    }
}

fn client_id_ok(value: &str) -> bool {
    !value.is_empty() && value.len() <= 512 && value.bytes().all(|b| b.is_ascii_graphic())
}

/// Pick the client identity: CIMD, then dynamic registration, then the
/// manifest's configured client id.
pub async fn register(
    net: &OauthNet,
    discovery: &Discovery,
    spec: &OauthSpec,
) -> Result<ClientRegistration> {
    if spec.uses_cimd && discovery.cimd_supported {
        return Ok(ClientRegistration {
            client_id: cimd_client_id(),
            client_secret: None,
            source: ClientSource::Cimd,
        });
    }
    if let Some(endpoint) = &discovery.registration_endpoint {
        let body = json!({
            "client_name": "SeraBot",
            "redirect_uris": [oauth_callback_uri()],
            "grant_types": ["authorization_code", "refresh_token"],
            "response_types": ["code"],
            "token_endpoint_auth_method": "none",
            "scope": spec.scopes.join(" "),
        });
        // A registration that fails is not the end: a configured id may exist.
        if let Ok((status, bytes)) = net.post_json(endpoint, &body).await
            && (status == 200 || status == 201)
            && let Some(value) = json_body(&bytes)
            && let Some(id) = string_of(&value, "client_id")
            && client_id_ok(id)
        {
            let secret = string_of(&value, "client_secret")
                .filter(|secret| !secret.is_empty() && secret.len() <= MAX_TOKEN_CHARS)
                .map(str::to_owned);
            return Ok(ClientRegistration {
                client_id: id.to_owned(),
                client_secret: secret,
                source: ClientSource::Dcr,
            });
        }
    }
    match &spec.client_id {
        Some(id) if client_id_ok(id) => Ok(ClientRegistration {
            client_id: id.clone(),
            client_secret: None,
            source: ClientSource::Configured,
        }),
        _ => Err(SeraError::Config(
            "no OAuth client identity is available for this server".into(),
        )),
    }
}

/// A started sign-in: the URL to open and the values to keep until the code
/// comes back. The verifier is a secret; keep it server-side only.
pub struct AuthRequest {
    pub url: String,
    pub state: String,
    pub verifier: String,
}

impl std::fmt::Debug for AuthRequest {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AuthRequest")
            .finish_non_exhaustive()
    }
}

/// Build the consent URL: PKCE S256 plus the RFC 8707 `resource`.
pub fn authorize_request(
    discovery: &Discovery,
    client: &ClientRegistration,
    scopes: &[String],
) -> Result<AuthRequest> {
    let (verifier, challenge) = pkce_s256();
    let state = random_state();
    let mut url = Url::parse(&discovery.authorization_endpoint)
        .map_err(|_| SeraError::External("authorization endpoint is invalid".into()))?;
    {
        let mut query = url.query_pairs_mut();
        query.append_pair("response_type", "code");
        query.append_pair("client_id", &client.client_id);
        query.append_pair("redirect_uri", &oauth_callback_uri());
        if !scopes.is_empty() {
            query.append_pair("scope", &scopes.join(" "));
        }
        query.append_pair("code_challenge", &challenge);
        query.append_pair("code_challenge_method", "S256");
        query.append_pair("state", &state);
        query.append_pair("resource", &discovery.resource);
    }
    Ok(AuthRequest {
        url: url.to_string(),
        state,
        verifier,
    })
}

/// The sealed record for one account: tokens, expiry, scope and the client
/// registration, plus the endpoints refresh and revoke need.
#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct OauthRecord {
    /// Empty after the tokens were dropped.
    pub access_token: String,
    #[serde(default)]
    pub refresh_token: Option<String>,
    /// Unix seconds; `None` means unknown, so the next use refreshes.
    #[serde(default)]
    pub expires_at: Option<i64>,
    #[serde(default)]
    pub scope: Option<String>,
    pub client: ClientRegistration,
    pub token_endpoint: String,
    #[serde(default)]
    pub revocation_endpoint: Option<String>,
    pub resource: String,
}

impl std::fmt::Debug for OauthRecord {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("OauthRecord")
            .field("has_access_token", &!self.access_token.is_empty())
            .field("has_refresh_token", &self.refresh_token.is_some())
            .field("expires_at", &self.expires_at)
            .field("client", &self.client)
            .field("resource", &self.resource)
            .finish_non_exhaustive()
    }
}

impl OauthRecord {
    /// Whether the access token is missing or about to expire.
    pub fn needs_refresh(&self, now: i64) -> bool {
        self.access_token.is_empty()
            || self
                .expires_at
                .is_none_or(|at| at <= now + EXPIRY_SKEW_SECS)
    }

    fn without_tokens(&self) -> Self {
        Self {
            access_token: String::new(),
            refresh_token: None,
            expires_at: None,
            ..self.clone()
        }
    }
}

/// Why a token request failed, as the outcome table needs it.
#[derive(Debug, PartialEq, Eq)]
pub enum TokenFailure {
    /// The grant (code or refresh token) is no longer valid.
    InvalidGrant,
    /// The server no longer knows this client.
    InvalidClient,
    /// Network failure, a 5xx, or anything that does not prove the tokens bad.
    Transient(String),
}

fn token_ok(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_TOKEN_CHARS
        && value.bytes().all(|b| b.is_ascii_graphic())
}

fn classify(status: u16, body: &Value) -> TokenFailure {
    let code = string_of(body, "error").unwrap_or("");
    let rejected = status == 400 || status == 401;
    match code {
        "invalid_grant" if rejected => TokenFailure::InvalidGrant,
        "invalid_client" | "unauthorized_client" if rejected => TokenFailure::InvalidClient,
        _ => {
            let shown: String = code
                .chars()
                .filter(|c| c.is_ascii_alphanumeric() || *c == '_')
                .take(60)
                .collect();
            TokenFailure::Transient(
                format!("token request answered {status} {shown}")
                    .trim()
                    .to_owned(),
            )
        }
    }
}

/// Read a token endpoint answer into a record, keeping what the old record
/// had when the server omits it (a refresh token that did not rotate).
fn parse_token_answer(
    status: u16,
    bytes: &[u8],
    base: &OauthRecord,
) -> std::result::Result<OauthRecord, TokenFailure> {
    let parsed = json_body(bytes);
    if !(200..300).contains(&status) {
        return Err(classify(status, parsed.as_ref().unwrap_or(&Value::Null)));
    }
    let transient = |why: &str| TokenFailure::Transient(why.to_owned());
    let value = parsed.ok_or_else(|| transient("token answer was not JSON"))?;
    let access = string_of(&value, "access_token").unwrap_or("");
    if !token_ok(access) {
        return Err(transient("token answer had no usable access token"));
    }
    if let Some(kind) = string_of(&value, "token_type")
        && !kind.eq_ignore_ascii_case("bearer")
    {
        return Err(transient("token answer was not a bearer token"));
    }
    let refresh = match string_of(&value, "refresh_token") {
        Some(token) if token_ok(token) => Some(token.to_owned()),
        Some(_) => return Err(transient("token answer had an unusable refresh token")),
        None => base.refresh_token.clone(),
    };
    let expires_at = value
        .get("expires_in")
        .and_then(|item| {
            item.as_u64()
                .or_else(|| item.as_str().and_then(|text| text.parse().ok()))
        })
        .map(|seconds| Utc::now().timestamp() + seconds.min(MAX_TOKEN_LIFETIME_SECS) as i64);
    let scope = string_of(&value, "scope")
        .filter(|scope| scope.len() <= 1024 && !scope.chars().any(char::is_control))
        .map(str::to_owned)
        .or_else(|| base.scope.clone());
    Ok(OauthRecord {
        access_token: access.to_owned(),
        refresh_token: refresh,
        expires_at,
        scope,
        ..base.clone()
    })
}

fn with_client<'a>(
    client: &'a ClientRegistration,
    mut form: Vec<(&'a str, &'a str)>,
) -> Vec<(&'a str, &'a str)> {
    form.push(("client_id", client.client_id.as_str()));
    if let Some(secret) = &client.client_secret {
        form.push(("client_secret", secret.as_str()));
    }
    form
}

/// Exchange the authorization code for the first record.
pub async fn exchange_code(
    net: &OauthNet,
    discovery: &Discovery,
    client: &ClientRegistration,
    code: &str,
    verifier: &str,
    requested_scopes: &[String],
) -> Result<OauthRecord> {
    if !state_ok_code(code) {
        return Err(SeraError::Validation("invalid authorization code".into()));
    }
    if !pkce_token_ok(verifier) {
        return Err(SeraError::Validation("invalid PKCE verifier".into()));
    }
    let redirect = oauth_callback_uri();
    let form = with_client(
        client,
        vec![
            ("grant_type", "authorization_code"),
            ("code", code),
            ("redirect_uri", redirect.as_str()),
            ("code_verifier", verifier),
            ("resource", discovery.resource.as_str()),
        ],
    );
    let (status, bytes) = net.post_form(&discovery.token_endpoint, &form).await?;
    let base = OauthRecord {
        access_token: String::new(),
        refresh_token: None,
        expires_at: None,
        scope: (!requested_scopes.is_empty()).then(|| requested_scopes.join(" ")),
        client: client.clone(),
        token_endpoint: discovery.token_endpoint.clone(),
        revocation_endpoint: discovery.revocation_endpoint.clone(),
        resource: discovery.resource.clone(),
    };
    parse_token_answer(status, &bytes, &base).map_err(|failure| match failure {
        TokenFailure::InvalidGrant | TokenFailure::InvalidClient => {
            SeraError::External("the server rejected the sign-in code".into())
        }
        TokenFailure::Transient(why) => SeraError::External(why),
    })
}

/// Codes follow the `state` charset loosely; bound and printable is enough.
fn state_ok_code(code: &str) -> bool {
    !code.is_empty() && code.len() <= 2048 && code.bytes().all(|b| b.is_ascii_graphic())
}

/// A state value shape check, for the caller matching the redirect.
pub fn callback_state_ok(state: &str) -> bool {
    state_ok(state)
}

/// Renew an access token from the refresh token. The caller applies the
/// outcome table; this only reports what the server said.
pub async fn refresh(
    net: &OauthNet,
    record: &OauthRecord,
) -> std::result::Result<OauthRecord, TokenFailure> {
    let Some(refresh_token) = record.refresh_token.as_deref() else {
        return Err(TokenFailure::InvalidGrant);
    };
    let form = with_client(
        &record.client,
        vec![
            ("grant_type", "refresh_token"),
            ("refresh_token", refresh_token),
            ("resource", record.resource.as_str()),
        ],
    );
    let (status, bytes) = net
        .post_form(&record.token_endpoint, &form)
        .await
        .map_err(|error| TokenFailure::Transient(short(&error)))?;
    parse_token_answer(status, &bytes, record)
}

/// RFC 7009 revocation, best effort: any failure is ignored and reported only
/// as `false`. The refresh token is revoked when there is one (servers revoke
/// the access token with it), else the access token.
pub async fn revoke(net: &OauthNet, record: &OauthRecord) -> bool {
    let Some(endpoint) = &record.revocation_endpoint else {
        return false;
    };
    let (token, hint) = match &record.refresh_token {
        Some(token) => (token.as_str(), "refresh_token"),
        None if !record.access_token.is_empty() => (record.access_token.as_str(), "access_token"),
        None => return false,
    };
    let form = with_client(
        &record.client,
        vec![("token", token), ("token_type_hint", hint)],
    );
    matches!(net.post_form(endpoint, &form).await, Ok((status, _)) if (200..300).contains(&status))
}

fn short(error: &SeraError) -> String {
    error.to_string().chars().take(160).collect()
}

/// What [`OauthAccounts::refresh_account`] did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RefreshOutcome {
    /// A current access token is stored.
    Ready,
    /// The refresh token was refused: tokens dropped, registration kept.
    NeedsSignIn,
    /// The client was refused: tokens and registration dropped.
    ClientDropped,
    /// The server could not be reached or failed: everything kept.
    Unreachable,
}

/// One user's marketplace OAuth accounts: sealed records plus their state rows.
/// Every method takes the acting user's id.
#[derive(Clone)]
pub struct OauthAccounts {
    store: Store,
    secrets: Arc<UserSecrets>,
    net: OauthNet,
}

impl OauthAccounts {
    pub fn new(store: Store, secrets: Arc<UserSecrets>, net: OauthNet) -> Self {
        Self {
            store,
            secrets,
            net,
        }
    }

    /// Seal a freshly signed-in record and mark the account ready.
    pub fn save_signed_in(
        &self,
        user_id: &str,
        slug: &str,
        account_id: &str,
        record: &OauthRecord,
    ) -> Result<()> {
        self.seal(user_id, slug, account_id, record)?;
        self.state(user_id, slug, account_id, "ready", None)
    }

    pub fn load(&self, user_id: &str, slug: &str, account_id: &str) -> Result<Option<OauthRecord>> {
        let Some(secret) = self
            .secrets
            .get_marketplace_oauth(user_id, slug, account_id)?
        else {
            return Ok(None);
        };
        serde_json::from_str(secret.expose_secret())
            .map(Some)
            .map_err(|_| SeraError::Secrets("stored OAuth record is unreadable".into()))
    }

    fn seal(
        &self,
        user_id: &str,
        slug: &str,
        account_id: &str,
        record: &OauthRecord,
    ) -> Result<()> {
        let text = serde_json::to_string(record)
            .map_err(|_| SeraError::Internal("OAuth record could not be encoded".into()))?;
        self.secrets
            .set_marketplace_oauth(user_id, slug, account_id, &text)
    }

    fn state(
        &self,
        user_id: &str,
        slug: &str,
        account_id: &str,
        state: &str,
        error: Option<&str>,
    ) -> Result<()> {
        self.store
            .marketplace_account_set_state(user_id, slug, account_id, state, error)?;
        Ok(())
    }

    /// Refresh the account's access token and apply the outcome table.
    pub async fn refresh_account(
        &self,
        user_id: &str,
        slug: &str,
        account_id: &str,
    ) -> Result<RefreshOutcome> {
        let record = self
            .load(user_id, slug, account_id)?
            .ok_or_else(|| SeraError::NotFound("marketplace account".into()))?;
        match refresh(&self.net, &record).await {
            Ok(renewed) => {
                // A rotated refresh token must not be lost: fail if it cannot
                // be stored.
                self.seal(user_id, slug, account_id, &renewed)?;
                self.state(user_id, slug, account_id, "ready", None)?;
                Ok(RefreshOutcome::Ready)
            }
            Err(TokenFailure::InvalidGrant) => {
                self.seal(user_id, slug, account_id, &record.without_tokens())?;
                self.state(
                    user_id,
                    slug,
                    account_id,
                    "needs_sign_in",
                    Some("invalid_grant"),
                )?;
                Ok(RefreshOutcome::NeedsSignIn)
            }
            Err(TokenFailure::InvalidClient) => {
                self.secrets
                    .delete_marketplace_oauth(user_id, slug, account_id)?;
                self.state(
                    user_id,
                    slug,
                    account_id,
                    "needs_sign_in",
                    Some("invalid_client"),
                )?;
                Ok(RefreshOutcome::ClientDropped)
            }
            Err(TokenFailure::Transient(why)) => {
                self.state(user_id, slug, account_id, "unreachable", Some(&why))?;
                Ok(RefreshOutcome::Unreachable)
            }
        }
    }

    /// Uninstall: revoke at the server on a best-effort basis, then delete the
    /// sealed record and the account row. Returns whether the server confirmed
    /// the revocation; local deletion happens either way.
    pub async fn forget_account(
        &self,
        user_id: &str,
        slug: &str,
        account_id: &str,
    ) -> Result<bool> {
        let revoked = match self.load(user_id, slug, account_id) {
            Ok(Some(record)) => revoke(&self.net, &record).await,
            _ => false,
        };
        self.secrets
            .delete_marketplace_oauth(user_id, slug, account_id)?;
        self.store
            .marketplace_account_delete(user_id, slug, account_id)?;
        Ok(revoked)
    }
}

#[cfg(test)]
mod tests;
