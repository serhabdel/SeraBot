//! Provider sign-in flows for members.
//!
//! One framework serves every provider that signs in with a browser or a
//! device code: a member starts a flow, the server remembers it for ten
//! minutes, and finishing it stores an account under the member's own id. Two
//! shapes exist:
//!
//! - **PKCE** (`pkce`): the server builds the authorization URL; the desktop
//!   opens it and catches the redirect on an exact loopback address; the
//!   member's desktop sends back the code and the server exchanges it.
//! - **Device** (`device`): the server asks the provider for a code, the member
//!   approves on the provider's page, and a background poller (RFC 8628) stores
//!   the account when the provider says yes.
//!
//! A flow belongs to `(user, flow id)` and to one provider. Another member, or
//! another provider id, finds nothing. Token endpoints are pinned by host and
//! path, redirects are never followed and bodies are capped at 64 KiB. Codes,
//! verifiers, device codes and tokens never appear in an error or an event.
//!
//! Providers plug in through [`SignInProvider`]; [`SignInRegistry`] maps a
//! provider kind to its implementation. This module ships OpenRouter's PKCE
//! sign-in and Muse's device sign-in (`muse`); Antigravity's (`google`) needs
//! the server's age store for its OAuth client secret and is registered by the
//! API; the Claude sign-in registers the same way. Codex's (`codex`) and GitHub
//! Copilot's (`copilot`) are standard: their client ids are public.

pub mod claude;
pub mod codex;
pub mod copilot;
pub mod device;
pub mod devin;
pub mod gateway;
pub mod google;
pub mod kiro;
pub mod mistral;
pub mod muse;
pub mod pkce;
#[cfg(test)]
mod tests;

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use serde::Serialize;
use serde_json::{Value, json};
use zeroize::Zeroizing;

use crate::accounts::{Accounts, AddAccount, AddOutcome, AuthKind};
use crate::error::{Result, SeraError};
use crate::store::Store;
use crate::user_secrets::ProviderCredential;

use device::{DevicePoll, DeviceStart, PollEnd};
use pkce::RedirectSpec;

/// One async lock per key (a user and account), so a slow token endpoint
/// stalls only that account's refresh and a rotating refresh token is still
/// never spent twice.
#[derive(Clone, Default)]
pub struct KeyedLocks {
    locks: Arc<Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>>,
}

impl KeyedLocks {
    /// The lock for `key`; callers `.lock().await` it. Idle entries are
    /// dropped when the map grows, which cannot break a lock in use because
    /// holders keep their own `Arc`.
    pub fn get(&self, key: &str) -> Arc<tokio::sync::Mutex<()>> {
        let mut locks = self.locks.lock().unwrap_or_else(|e| e.into_inner());
        if locks.len() > 256 {
            locks.retain(|_, lock| Arc::strong_count(lock) > 1);
        }
        locks.entry(key.to_owned()).or_default().clone()
    }
}

/// How long a started flow is remembered.
pub const FLOW_TTL: Duration = Duration::from_secs(600);
/// Most flows one member can have open at once; the oldest is dropped.
pub const MAX_FLOWS_PER_USER: usize = 8;
/// Largest response body any flow reads.
pub const MAX_BODY_BYTES: usize = 64 * 1024;
/// Longest access, refresh or id token accepted from a provider.
const MAX_TOKEN_BYTES: usize = 16 * 1024;

/// A client every flow uses: no redirects, bounded time.
pub fn http_client() -> Result<reqwest::Client> {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .redirect(reqwest::redirect::Policy::none())
        .user_agent(crate::providers::USER_AGENT)
        .build()
        .map_err(|_| SeraError::External("sign-in could not start".into()))
}

/// Read a response body, refusing more than [`MAX_BODY_BYTES`].
pub async fn read_capped(mut response: reqwest::Response) -> Result<Vec<u8>> {
    let too_large = || SeraError::External("the sign-in provider sent too much data".into());
    if response
        .content_length()
        .is_some_and(|length| length > MAX_BODY_BYTES as u64)
    {
        return Err(too_large());
    }
    let mut body = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| SeraError::External("the sign-in provider could not be read".into()))?
    {
        if body.len() + chunk.len() > MAX_BODY_BYTES {
            return Err(too_large());
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

pub(crate) fn credential_ok(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_TOKEN_BYTES
        && value.trim() == value
        && !value.chars().any(char::is_control)
}

/// An endpoint pinned by host and path: `https://{host}{path}` and nothing
/// else. A pin never follows a redirect or a configured override.
#[derive(Debug, Clone)]
pub struct PinnedEndpoint {
    host: String,
    path: String,
    #[cfg(test)]
    loopback: Option<String>,
}

impl PinnedEndpoint {
    pub fn https(host: &str, path: &str) -> Self {
        Self {
            host: host.to_owned(),
            path: path.to_owned(),
            #[cfg(test)]
            loopback: None,
        }
    }

    /// A mock server on the exact loopback address, for tests.
    #[cfg(test)]
    pub(crate) fn loopback(url: &str) -> Self {
        Self {
            host: "127.0.0.1".into(),
            path: String::new(),
            loopback: Some(url.to_owned()),
        }
    }

    /// The pinned URL, rebuilt and checked on every use.
    pub fn checked_url(&self) -> Result<url::Url> {
        let refuse = || SeraError::Config("a sign-in endpoint is not pinned correctly".into());
        #[cfg(test)]
        if let Some(raw) = &self.loopback {
            let url = url::Url::parse(raw).map_err(|_| refuse())?;
            if url.scheme() != "http" || url.host_str() != Some("127.0.0.1") {
                return Err(refuse());
            }
            return Ok(url);
        }
        let url = url::Url::parse(&format!("https://{}{}", self.host, self.path))
            .map_err(|_| refuse())?;
        if url.scheme() != "https"
            || url.host_str() != Some(self.host.as_str())
            || url.path() != self.path
            || url.query().is_some()
            || !url.username().is_empty()
            || url.password().is_some()
        {
            return Err(refuse());
        }
        Ok(url)
    }
}

/// What a token endpoint returned. Holds secrets; `Debug` redacts them.
pub struct TokenSet {
    pub access_token: String,
    pub refresh_token: Option<String>,
    /// Seconds the access token lives.
    pub expires_in: Option<u64>,
    /// An OpenID Connect identity token, when the provider sent one.
    pub id_token: Option<String>,
    /// Seconds the refresh token lives, when the provider says (GitHub does).
    pub refresh_expires_in: Option<u64>,
}

impl std::fmt::Debug for TokenSet {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("TokenSet")
            .field("expires_in", &self.expires_in)
            .finish_non_exhaustive()
    }
}

impl TokenSet {
    /// Parse a token-endpoint body.
    pub fn from_body(body: &[u8]) -> Result<Self> {
        let bad = || SeraError::External("the sign-in provider sent an invalid token reply".into());
        let value: Value = serde_json::from_slice(body).map_err(|_| bad())?;
        let text = |field: &str| {
            value
                .get(field)
                .and_then(Value::as_str)
                .filter(|token| credential_ok(token))
                .map(str::to_owned)
        };
        Ok(Self {
            access_token: text("access_token").ok_or_else(bad)?,
            refresh_token: text("refresh_token"),
            expires_in: value
                .get("expires_in")
                .and_then(Value::as_u64)
                .filter(|seconds| *seconds > 0),
            id_token: text("id_token"),
            refresh_expires_in: value
                .get("refresh_token_expires_in")
                .and_then(Value::as_u64)
                .filter(|seconds| *seconds > 0),
        })
    }

    /// Unix seconds when the access token stops working, if known.
    pub fn expires_at(&self) -> Option<i64> {
        self.expires_in
            .map(|seconds| chrono::Utc::now().timestamp() + seconds as i64)
    }
}

/// The two shapes of flow.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FlowKind {
    Pkce,
    Device,
}

impl FlowKind {
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "pkce" => Some(Self::Pkce),
            "device" => Some(Self::Device),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pkce => "pkce",
            Self::Device => "device",
        }
    }
}

/// Where a flow stands, as the status route reports it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FlowStatus {
    Pending,
    Ok { account_id: String, created: bool },
    Denied { reason: String },
    Expired,
}

impl FlowStatus {
    pub fn word(&self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Ok { .. } => "ok",
            Self::Denied { .. } => "denied",
            Self::Expired => "expired",
        }
    }
}

/// An account a sign-in produced, ready to store.
pub struct SignedIn {
    pub label: String,
    pub auth_kind: AuthKind,
    /// A stable identity fingerprint (`accounts::identity_fingerprint`). `None`
    /// is only allowed for a plain API key, which gets a keyed fingerprint.
    pub fingerprint: Option<String>,
    /// Non-secret facts: email, plan, project id.
    pub identity: Value,
    pub credential: ProviderCredential,
}

/// A provider's sign-in. Every method defaults to "not supported".
#[async_trait]
pub trait SignInProvider: Send + Sync {
    /// The flow shapes this provider offers.
    fn flows(&self) -> &'static [FlowKind];

    /// The loopback redirect this provider's app registration allows.
    fn redirect_spec(&self) -> Option<RedirectSpec> {
        None
    }

    /// What this server still lacks before the sign-in can finish (for one, a
    /// client secret the operator has to supply). `None` when it is ready.
    fn setup_missing(&self) -> Option<&'static str> {
        None
    }

    /// The authorization page for a PKCE flow.
    fn authorize_url(&self, _redirect_uri: &str, _challenge: &str, _state: &str) -> Result<String> {
        Err(unsupported())
    }

    /// Turn a one-time code into an account.
    async fn exchange_pkce(
        &self,
        _http: &reqwest::Client,
        _code: &str,
        _redirect_uri: &str,
        _verifier: &str,
    ) -> Result<SignedIn> {
        Err(unsupported())
    }

    /// Like [`Self::exchange_pkce`], also given the `state` the flow started
    /// with, for a provider whose token endpoint wants it back.
    async fn exchange_pkce_with_state(
        &self,
        http: &reqwest::Client,
        code: &str,
        redirect_uri: &str,
        verifier: &str,
        _state: &str,
    ) -> Result<SignedIn> {
        self.exchange_pkce(http, code, redirect_uri, verifier).await
    }

    /// Ask the provider for a device code.
    async fn device_start(&self, _http: &reqwest::Client) -> Result<DeviceStart> {
        Err(unsupported())
    }

    /// Poll the provider once.
    async fn device_poll(&self, _http: &reqwest::Client, _device_code: &str) -> Result<DevicePoll> {
        Err(unsupported())
    }

    /// Turn the tokens a device flow produced into an account.
    async fn finish_device(&self, _http: &reqwest::Client, _tokens: TokenSet) -> Result<SignedIn> {
        Err(unsupported())
    }
}

fn unsupported() -> SeraError {
    SeraError::Rejected {
        code: "signin_unsupported",
        message: "this provider has no sign-in; add its API key".into(),
    }
}

/// Provider kind to sign-in implementation.
#[derive(Clone, Default)]
pub struct SignInRegistry {
    providers: HashMap<String, Arc<dyn SignInProvider>>,
}

impl SignInRegistry {
    /// The sign-ins this build ships.
    pub fn standard() -> Self {
        let mut registry = Self::default();
        registry.register("openrouter", Arc::new(OpenRouterSignIn));
        registry.register("muse-code", Arc::new(muse::MuseSignIn::new()));
        // Codex's client id is public and compiled in, so it needs no secret.
        // Each provider's `plan_oauth` flag (checked per request) turns it off.
        registry.register("openai-codex", Arc::new(codex::CodexSignIn::new()));
        registry.register("github-copilot", Arc::new(copilot::CopilotSignIn::new()));
        registry.register("mistral", Arc::new(mistral::MistralSignIn::new()));
        registry.register("devin", Arc::new(devin::DevinSignIn::new()));
        registry.register("kiro", Arc::new(kiro::KiroSignIn::new()));
        registry
    }

    pub fn register(&mut self, kind: &str, provider: Arc<dyn SignInProvider>) {
        self.providers.insert(kind.to_owned(), provider);
    }

    pub fn get(&self, kind: &str) -> Option<&Arc<dyn SignInProvider>> {
        self.providers.get(kind)
    }

    /// The flow shapes offered for a provider kind (empty when none).
    pub fn flows_for(&self, kind: &str) -> Vec<&'static str> {
        self.get(kind)
            .map(|provider| provider.flows().iter().map(|flow| flow.as_str()).collect())
            .unwrap_or_default()
    }
}

/// OpenRouter's "Sign in with OpenRouter": PKCE that ends in an API key.
struct OpenRouterSignIn;

#[async_trait]
impl SignInProvider for OpenRouterSignIn {
    fn flows(&self) -> &'static [FlowKind] {
        &[FlowKind::Pkce]
    }

    fn redirect_spec(&self) -> Option<RedirectSpec> {
        // OpenRouter takes any unprivileged loopback port and path.
        Some(RedirectSpec {
            port: None,
            path: None,
        })
    }

    fn authorize_url(&self, redirect_uri: &str, challenge: &str, state: &str) -> Result<String> {
        crate::openrouter::authorize_url(redirect_uri, challenge, state)
    }

    async fn exchange_pkce(
        &self,
        http: &reqwest::Client,
        code: &str,
        _redirect_uri: &str,
        verifier: &str,
    ) -> Result<SignedIn> {
        let key =
            crate::openrouter::exchange_code(http, crate::openrouter::KEYS_URL, code, verifier)
                .await?;
        Ok(SignedIn {
            label: "OpenRouter".into(),
            auth_kind: AuthKind::Minted,
            fingerprint: None,
            identity: json!({}),
            credential: ProviderCredential {
                api_key: key,
                ..Default::default()
            },
        })
    }
}

struct FlowRecord {
    provider: String,
    kind: FlowKind,
    redirect_uri: Option<String>,
    /// The `state` a browser flow started with.
    state: Option<String>,
    /// The verifier the server generated. `None` when the desktop holds it,
    /// or when a gateway holds it (`gateway` is then set).
    verifier: Option<Zeroizing<String>>,
    client_holds_verifier: bool,
    /// A `claude-agent-sdk` flow: the loopback gateway base URL the browser
    /// sign-in is relayed to.
    gateway: Option<String>,
    expires: Instant,
    created: Instant,
    status: FlowStatus,
    /// Set once a code has been taken for exchange: a code is single-use.
    consumed: bool,
}

type FlowKey = (String, String);

/// What a PKCE flow hands over when its code is taken for exchange.
struct TakenFlow {
    /// The verifier the server generated, when the desktop did not keep it.
    verifier: Option<Zeroizing<String>>,
    client_holds_verifier: bool,
    /// The redirect the flow started with.
    redirect_uri: String,
    /// The `state` the flow started with.
    state: String,
    /// The gateway a `claude-agent-sdk` flow is relayed to, if any.
    gateway: Option<String>,
}

/// The open flows, keyed by `(user, flow id)`.
pub struct FlowStore {
    ttl: Duration,
    flows: Mutex<HashMap<FlowKey, FlowRecord>>,
}

impl FlowStore {
    pub fn new(ttl: Duration) -> Self {
        Self {
            ttl,
            flows: Mutex::new(HashMap::new()),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<FlowKey, FlowRecord>> {
        self.flows
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn insert(&self, user_id: &str, record: FlowRecord) -> String {
        let flow_id = new_flow_id();
        let mut flows = self.lock();
        let now = Instant::now();
        // Forget what has been dead for a full TTL, then make room.
        flows.retain(|_, flow| now < flow.expires + self.ttl);
        let oldest = flows
            .iter()
            .filter(|((owner, _), _)| owner == user_id)
            .map(|((_, id), flow)| (id.clone(), flow.created))
            .collect::<Vec<_>>();
        if oldest.len() >= MAX_FLOWS_PER_USER
            && let Some((id, _)) = oldest.into_iter().min_by_key(|(_, created)| *created)
        {
            flows.remove(&(user_id.to_owned(), id));
        }
        flows.insert((user_id.to_owned(), flow_id.clone()), record);
        flow_id
    }

    /// Take a PKCE flow for exchange. The lookup is by `(user, flow id)` and
    /// provider, so another member's flow, or another provider's, is simply
    /// not found. A flow that is expired, finished or already taken is refused.
    fn take_pkce(
        &self,
        user_id: &str,
        provider: &str,
        flow_id: &str,
        redirect_uri: Option<&str>,
        has_verifier: bool,
    ) -> Result<TakenFlow> {
        let mut flows = self.lock();
        let record = flows
            .get_mut(&(user_id.to_owned(), flow_id.to_owned()))
            .filter(|record| record.provider == provider && record.kind == FlowKind::Pkce)
            .ok_or_else(|| SeraError::NotFound("sign-in flow".into()))?;
        if record.consumed || record.status != FlowStatus::Pending {
            return Err(SeraError::Rejected {
                code: "flow_used",
                message: "This sign-in was already used. Start a new one.".into(),
            });
        }
        if Instant::now() >= record.expires {
            record.status = FlowStatus::Expired;
            return Err(SeraError::Rejected {
                code: "flow_expired",
                message: "This sign-in expired. Start a new one.".into(),
            });
        }
        if redirect_uri.is_some_and(|given| record.redirect_uri.as_deref() != Some(given)) {
            return Err(SeraError::Rejected {
                code: "bad_redirect",
                message: "The sign-in redirect does not match the one it started with".into(),
            });
        }
        // A request that cannot be finished must not burn the member's flow.
        // A gateway flow keeps its verifier inside the gateway, so the
        // desktop sends none.
        if record.client_holds_verifier && !has_verifier {
            return Err(SeraError::Rejected {
                code: "bad_code",
                message: "The sign-in verifier is required".into(),
            });
        }
        record.consumed = true;
        Ok(TakenFlow {
            verifier: record.verifier.clone(),
            client_holds_verifier: record.client_holds_verifier,
            redirect_uri: record.redirect_uri.clone().unwrap_or_default(),
            state: record.state.clone().unwrap_or_default(),
            gateway: record.gateway.clone(),
        })
    }

    fn set_status(&self, user_id: &str, flow_id: &str, status: FlowStatus) {
        if let Some(record) = self
            .lock()
            .get_mut(&(user_id.to_owned(), flow_id.to_owned()))
        {
            record.status = status;
        }
    }

    /// Whether a device flow is still waiting (the poller stops when not).
    fn is_pending(&self, user_id: &str, flow_id: &str) -> bool {
        self.lock()
            .get(&(user_id.to_owned(), flow_id.to_owned()))
            .is_some_and(|record| record.status == FlowStatus::Pending)
    }

    /// The newest open PKCE flow of a provider, for the pre-0.49 sign-in route
    /// that does not name a flow.
    fn latest_pending_pkce(&self, user_id: &str, provider: &str) -> Option<String> {
        let flows = self.lock();
        flows
            .iter()
            .filter(|((owner, _), record)| {
                owner == user_id
                    && record.provider == provider
                    && record.kind == FlowKind::Pkce
                    && !record.consumed
                    && record.status == FlowStatus::Pending
            })
            .max_by_key(|(_, record)| record.created)
            .map(|((_, id), _)| id.clone())
    }

    fn status(&self, user_id: &str, provider: &str, flow_id: &str) -> Result<FlowStatus> {
        let mut flows = self.lock();
        let record = flows
            .get_mut(&(user_id.to_owned(), flow_id.to_owned()))
            .filter(|record| record.provider == provider)
            .ok_or_else(|| SeraError::NotFound("sign-in flow".into()))?;
        if record.status == FlowStatus::Pending && Instant::now() >= record.expires {
            record.status = FlowStatus::Expired;
        }
        Ok(record.status.clone())
    }
}

fn new_flow_id() -> String {
    use rand::RngCore;
    let mut bytes = [0u8; 16];
    rand::rngs::OsRng.fill_bytes(&mut bytes);
    hex::encode(bytes)
}

/// A member's request to start a flow.
#[derive(Debug, Default)]
pub struct StartRequest {
    pub flow: Option<FlowKind>,
    pub redirect_uri: Option<String>,
    /// A challenge the desktop made and whose verifier it keeps.
    pub challenge: Option<String>,
    pub state: Option<String>,
}

/// What a started flow tells the desktop.
#[derive(Debug, Serialize)]
pub struct StartedFlow {
    pub flow_id: String,
    pub flow: FlowKind,
    pub url: String,
    pub expires_in: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user_code: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub interval: Option<u64>,
    /// The `state` the authorization URL carries (PKCE), so the desktop can
    /// check the redirect.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub state: Option<String>,
}

/// A member's request to finish a PKCE flow.
#[derive(Debug)]
pub struct CompleteRequest {
    /// `None` finishes the member's newest open flow for this provider (the
    /// route that predates flow ids).
    pub flow_id: Option<String>,
    pub code: String,
    /// When given, must be the redirect the flow started with. The route that
    /// predates flow ids may leave it out.
    pub redirect_uri: Option<String>,
    /// Required only when the desktop made the challenge.
    pub verifier: Option<String>,
}

/// The sign-in service: flow map, provider registry, account store.
pub struct SignInService {
    registry: SignInRegistry,
    flows: FlowStore,
    accounts: Arc<Accounts>,
    store: Store,
    http: reqwest::Client,
}

impl SignInService {
    pub fn new(accounts: Arc<Accounts>) -> Result<Self> {
        Ok(Self {
            registry: SignInRegistry::standard(),
            flows: FlowStore::new(FLOW_TTL),
            store: accounts.store().clone(),
            accounts,
            http: http_client()?,
        })
    }

    pub fn with_registry(mut self, registry: SignInRegistry) -> Self {
        self.registry = registry;
        self
    }

    pub fn with_ttl(mut self, ttl: Duration) -> Self {
        self.flows = FlowStore::new(ttl);
        self
    }

    pub fn registry(&self) -> &SignInRegistry {
        &self.registry
    }

    fn provider_for(&self, kind: &str) -> Result<Arc<dyn SignInProvider>> {
        self.registry.get(kind).cloned().ok_or_else(unsupported)
    }

    /// Start a flow for `provider_id` (a configured provider whose kind is
    /// `kind`).
    pub async fn start(
        self: &Arc<Self>,
        user_id: &str,
        provider_id: &str,
        kind: &str,
        request: StartRequest,
    ) -> Result<StartedFlow> {
        self.start_inner(user_id, provider_id, kind, request, None)
            .await
    }

    /// Start a browser sign-in relayed to a `claude-agent-sdk` loopback
    /// gateway (the gateway holds the PKCE verifier and builds the URL).
    pub async fn start_with_gateway(
        self: &Arc<Self>,
        user_id: &str,
        provider_id: &str,
        kind: &str,
        request: StartRequest,
        gateway: gateway::GatewayEndpoint,
    ) -> Result<StartedFlow> {
        self.start_inner(user_id, provider_id, kind, request, Some(gateway))
            .await
    }

    async fn start_inner(
        self: &Arc<Self>,
        user_id: &str,
        provider_id: &str,
        kind: &str,
        request: StartRequest,
        gateway: Option<gateway::GatewayEndpoint>,
    ) -> Result<StartedFlow> {
        let provider = self.provider_for(kind)?;
        let flows = provider.flows();
        let flow = match request.flow {
            Some(flow) => flow,
            None if flows.len() == 1 => flows[0],
            None => FlowKind::Pkce,
        };
        if !flows.contains(&flow) {
            return Err(SeraError::Rejected {
                code: "flow_unsupported",
                message: "this provider does not offer that kind of sign-in".into(),
            });
        }
        let started = match flow {
            FlowKind::Pkce if gateway.is_some() => {
                self.start_gateway(user_id, provider_id, &*provider, request, gateway)
                    .await?
            }
            FlowKind::Pkce => self.start_pkce(user_id, provider_id, &*provider, request)?,
            FlowKind::Device => self.start_device(user_id, provider_id, provider).await?,
        };
        self.publish(user_id, provider_id, &started.flow_id, &FlowStatus::Pending);
        Ok(started)
    }

    fn start_pkce(
        &self,
        user_id: &str,
        provider_id: &str,
        provider: &dyn SignInProvider,
        request: StartRequest,
    ) -> Result<StartedFlow> {
        let redirect_uri = request.redirect_uri.ok_or_else(|| SeraError::Rejected {
            code: "bad_redirect",
            message: "A sign-in redirect is required".into(),
        })?;
        if let Some(spec) = provider.redirect_spec() {
            pkce::validate_loopback_redirect(&spec, &redirect_uri)?;
        }
        let (verifier, challenge, client_holds) = match request.challenge {
            Some(challenge) => {
                if !pkce::pkce_token_ok(&challenge) {
                    return Err(SeraError::Rejected {
                        code: "bad_challenge",
                        message: "The sign-in challenge is not valid".into(),
                    });
                }
                (None, challenge, true)
            }
            None => {
                let (verifier, challenge) = pkce::pkce_s256();
                (Some(Zeroizing::new(verifier)), challenge, false)
            }
        };
        let state = match request.state {
            Some(state) if pkce::state_ok(&state) => state,
            Some(_) => {
                return Err(SeraError::Rejected {
                    code: "bad_challenge",
                    message: "The sign-in state is not valid".into(),
                });
            }
            None => pkce::random_state(),
        };
        let url = provider.authorize_url(&redirect_uri, &challenge, &state)?;
        let now = Instant::now();
        let flow_id = self.flows.insert(
            user_id,
            FlowRecord {
                provider: provider_id.to_owned(),
                kind: FlowKind::Pkce,
                redirect_uri: Some(redirect_uri),
                state: Some(state.clone()),
                verifier,
                client_holds_verifier: client_holds,
                gateway: None,
                expires: now + self.flows.ttl,
                created: now,
                status: FlowStatus::Pending,
                consumed: false,
            },
        );
        Ok(StartedFlow {
            flow_id,
            flow: FlowKind::Pkce,
            url,
            expires_in: self.flows.ttl.as_secs(),
            user_code: None,
            interval: None,
            state: Some(state),
        })
    }

    /// Start a browser sign-in relayed to a `claude-agent-sdk` loopback
    /// gateway. The gateway builds the authorize URL and keeps the PKCE
    /// verifier; the desktop only sees the URL and the `state`.
    async fn start_gateway(
        &self,
        user_id: &str,
        provider_id: &str,
        provider: &dyn SignInProvider,
        request: StartRequest,
        gateway: Option<gateway::GatewayEndpoint>,
    ) -> Result<StartedFlow> {
        let endpoint = gateway
            .ok_or_else(|| SeraError::Internal("gateway flow without an endpoint".into()))?;
        let redirect_uri = request.redirect_uri.ok_or_else(|| SeraError::Rejected {
            code: "bad_redirect",
            message: "A sign-in redirect is required".into(),
        })?;
        if let Some(spec) = provider.redirect_spec() {
            pkce::validate_loopback_redirect(&spec, &redirect_uri)?;
        }
        // The desktop may pass its state so it can check the redirect; the
        // challenge/verifier are the gateway's alone.
        let state = match request.state {
            Some(state) if pkce::state_ok(&state) => state,
            Some(_) => {
                return Err(SeraError::Rejected {
                    code: "bad_challenge",
                    message: "The sign-in state is not valid".into(),
                });
            }
            None => pkce::random_state(),
        };
        let (url, state) = gateway::gateway_auth_start(&endpoint, &redirect_uri, &state).await?;
        let now = Instant::now();
        let flow_id = self.flows.insert(
            user_id,
            FlowRecord {
                provider: provider_id.to_owned(),
                kind: FlowKind::Pkce,
                redirect_uri: Some(redirect_uri),
                state: Some(state.clone()),
                verifier: None,
                client_holds_verifier: false,
                gateway: Some(endpoint.base_url.clone()),
                expires: now + self.flows.ttl,
                created: now,
                status: FlowStatus::Pending,
                consumed: false,
            },
        );
        Ok(StartedFlow {
            flow_id,
            flow: FlowKind::Pkce,
            url,
            expires_in: self.flows.ttl.as_secs(),
            user_code: None,
            interval: None,
            state: Some(state),
        })
    }

    async fn start_device(
        self: &Arc<Self>,
        user_id: &str,
        provider_id: &str,
        provider: Arc<dyn SignInProvider>,
    ) -> Result<StartedFlow> {
        let started = provider.device_start(&self.http).await?;
        let lifetime = Duration::from_secs(started.expires_in.min(device::MAX_WAIT_SECS));
        let now = Instant::now();
        let flow_id = self.flows.insert(
            user_id,
            FlowRecord {
                provider: provider_id.to_owned(),
                kind: FlowKind::Device,
                redirect_uri: None,
                state: None,
                verifier: None,
                client_holds_verifier: false,
                gateway: None,
                expires: now + lifetime,
                created: now,
                status: FlowStatus::Pending,
                consumed: false,
            },
        );
        let service = self.clone();
        let (user, provider_name, id) =
            (user_id.to_owned(), provider_id.to_owned(), flow_id.clone());
        let (device_code, interval, expires_in) = (
            Zeroizing::new(started.device_code),
            started.interval,
            started.expires_in,
        );
        tokio::spawn(async move {
            let flow = FlowRef {
                user_id: &user,
                provider_id: &provider_name,
                flow_id: &id,
            };
            let plan = DevicePlan {
                device_code,
                interval,
                expires_in,
            };
            service
                .run_device_flow(&flow, provider, plan, tokio::time::sleep)
                .await;
        });
        Ok(StartedFlow {
            flow_id,
            flow: FlowKind::Device,
            url: started.verification_uri,
            expires_in: lifetime.as_secs(),
            user_code: (!started.user_code.is_empty()).then_some(started.user_code),
            interval: Some(started.interval),
            state: None,
        })
    }

    /// Poll a device flow to its end and record the result. `sleep` is
    /// injectable so tests do not wait.
    pub(crate) async fn run_device_flow<S, SFut>(
        &self,
        flow: &FlowRef<'_>,
        provider: Arc<dyn SignInProvider>,
        plan: DevicePlan,
        sleep: S,
    ) where
        S: Fn(Duration) -> SFut,
        SFut: std::future::Future<Output = ()>,
    {
        let FlowRef {
            user_id,
            provider_id,
            flow_id,
        } = *flow;
        let DevicePlan {
            device_code,
            interval,
            expires_in,
        } = plan;
        let end = device::run_poller(
            interval,
            expires_in,
            || {
                let provider = provider.clone();
                let code = device_code.clone();
                async move {
                    // The flow was dropped (evicted or expired): stop quietly.
                    if !self.flows.is_pending(user_id, flow_id) {
                        return Ok(DevicePoll::Expired);
                    }
                    provider.device_poll(&self.http, &code).await
                }
            },
            sleep,
        )
        .await;
        let status = match end {
            PollEnd::Done(tokens) => match provider.finish_device(&self.http, tokens).await {
                Ok(signed_in) => match self.store_account(user_id, provider_id, signed_in) {
                    Ok(outcome) => FlowStatus::Ok {
                        account_id: outcome.account_id,
                        created: outcome.created,
                    },
                    Err(error) => FlowStatus::Denied {
                        reason: failure_reason(&error),
                    },
                },
                Err(error) => FlowStatus::Denied {
                    reason: failure_reason(&error),
                },
            },
            PollEnd::Denied => FlowStatus::Denied {
                reason: "access_denied".into(),
            },
            PollEnd::Expired => FlowStatus::Expired,
            PollEnd::Failed(reason) => FlowStatus::Denied {
                reason: reason.into(),
            },
        };
        self.flows.set_status(user_id, flow_id, status.clone());
        self.publish(user_id, provider_id, flow_id, &status);
    }

    /// Finish a PKCE flow with the code the browser returned.
    pub async fn complete(
        &self,
        user_id: &str,
        provider_id: &str,
        kind: &str,
        request: CompleteRequest,
    ) -> Result<AddOutcome> {
        self.complete_inner(user_id, provider_id, kind, request, None)
            .await
    }

    /// Finish a sign-in whose flow runs on a `claude-agent-sdk` loopback
    /// gateway.
    pub async fn complete_with_gateway(
        &self,
        user_id: &str,
        provider_id: &str,
        kind: &str,
        request: CompleteRequest,
        gateway: gateway::GatewayEndpoint,
    ) -> Result<AddOutcome> {
        self.complete_inner(user_id, provider_id, kind, request, Some(gateway))
            .await
    }

    async fn complete_inner(
        &self,
        user_id: &str,
        provider_id: &str,
        kind: &str,
        request: CompleteRequest,
        gateway: Option<gateway::GatewayEndpoint>,
    ) -> Result<AddOutcome> {
        let provider = self.provider_for(kind)?;
        let flow_id = match request.flow_id {
            Some(id) => id,
            None => self
                .flows
                .latest_pending_pkce(user_id, provider_id)
                .ok_or_else(|| SeraError::NotFound("sign-in flow".into()))?,
        };
        let taken = self.flows.take_pkce(
            user_id,
            provider_id,
            &flow_id,
            request.redirect_uri.as_deref(),
            request.verifier.is_some(),
        )?;
        // A gateway flow: the gateway holds the verifier and swaps the code;
        // the account credential is the gateway bearer token.
        let result = if let (Some(base), Some(endpoint)) = (&taken.gateway, gateway) {
            debug_assert_eq!(base, &endpoint.base_url);
            gateway::gateway_auth_complete(
                &endpoint,
                &taken.state,
                &request.code,
                &taken.redirect_uri,
            )
            .await
            .and_then(|account| {
                let identity = json!({
                    "source": "claude-agent-sdk-gateway",
                    "account": account,
                });
                let fingerprint = crate::accounts::identity_fingerprint(
                    provider_id,
                    &serde_json::to_string(&identity).unwrap_or_default(),
                );
                let signed_in = SignedIn {
                    label: "Claude (browser sign-in)".into(),
                    auth_kind: AuthKind::Oauth,
                    fingerprint: Some(fingerprint),
                    identity,
                    credential: ProviderCredential {
                        api_key: endpoint.token.as_str().to_owned(),
                        ..Default::default()
                    },
                };
                self.store_account(user_id, provider_id, signed_in)
            })
        } else {
            let verifier = if taken.client_holds_verifier {
                Zeroizing::new(request.verifier.ok_or_else(|| SeraError::Rejected {
                    code: "bad_code",
                    message: "The sign-in verifier is required".into(),
                })?)
            } else {
                taken
                    .verifier
                    .ok_or_else(|| SeraError::Internal("a sign-in flow lost its verifier".into()))?
            };
            provider
                .exchange_pkce_with_state(
                    &self.http,
                    &request.code,
                    &taken.redirect_uri,
                    &verifier,
                    &taken.state,
                )
                .await
                .and_then(|signed_in| self.store_account(user_id, provider_id, signed_in))
        };
        let status = match &result {
            Ok(outcome) => FlowStatus::Ok {
                account_id: outcome.account_id.clone(),
                created: outcome.created,
            },
            Err(error) => FlowStatus::Denied {
                reason: failure_reason(error),
            },
        };
        self.flows.set_status(user_id, &flow_id, status.clone());
        self.publish(user_id, provider_id, &flow_id, &status);
        result
    }

    /// Store a finished sign-in as the member's account.
    fn store_account(
        &self,
        user_id: &str,
        provider_id: &str,
        signed_in: SignedIn,
    ) -> Result<AddOutcome> {
        let SignedIn {
            label,
            auth_kind,
            fingerprint,
            identity,
            credential,
        } = signed_in;
        let fingerprint = match fingerprint {
            Some(fingerprint) => fingerprint,
            None if matches!(auth_kind, AuthKind::ApiKey | AuthKind::Minted)
                && credential.refresh_token.is_empty() =>
            {
                self.accounts
                    .key_fingerprint(user_id, provider_id, &credential.api_key)
            }
            None => {
                return Err(SeraError::Rejected {
                    code: "no_stable_identity",
                    message: "The provider did not say who signed in, so the account was not saved"
                        .into(),
                });
            }
        };
        let outcome = self.accounts.add(
            user_id,
            AddAccount {
                provider: provider_id.to_owned(),
                label,
                auth_kind,
                fingerprint,
                identity,
                credential,
                priority: 0,
            },
        )?;
        let event = if outcome.created {
            "provider.account_signed_in"
        } else {
            "provider.account_reauthed"
        };
        if let Err(error) = self.store.record_audit(
            user_id,
            event,
            None,
            None,
            None,
            None,
            None,
            Some(&json!({
                "provider": provider_id,
                "account": outcome.account_id,
                "auth_kind": auth_kind.as_str(),
            })),
        ) {
            tracing::warn!(%error, "could not audit a provider sign-in");
        }
        Ok(outcome)
    }

    /// The state of one flow, for the member that started it.
    pub fn status(&self, user_id: &str, provider_id: &str, flow_id: &str) -> Result<FlowStatus> {
        self.flows.status(user_id, provider_id, flow_id)
    }

    fn publish(&self, user_id: &str, provider_id: &str, flow_id: &str, status: &FlowStatus) {
        let mut payload = json!({
            "provider": provider_id,
            "flow_id": flow_id,
            "status": status.word(),
        });
        match status {
            FlowStatus::Ok {
                account_id,
                created,
            } => {
                payload["account_id"] = json!(account_id);
                payload["created"] = json!(created);
            }
            FlowStatus::Denied { reason } => payload["reason"] = json!(reason),
            _ => {}
        }
        if let Err(error) = self
            .store
            .append_event(user_id, "provider.signin", &payload)
        {
            tracing::warn!(%error, "could not publish a sign-in event");
        }
    }
}

/// A short, secret-free word for why a flow failed.
fn failure_reason(error: &SeraError) -> String {
    match error {
        SeraError::Rejected { code, .. } => (*code).to_owned(),
        SeraError::External(_) | SeraError::Upstream { .. } => "provider_error".into(),
        _ => "failed".into(),
    }
}

/// Who a set of tokens belongs to.
pub struct AccountIdentity {
    pub label: String,
    /// The stable id the fingerprint is made from (an account id, a lower-cased
    /// email); never a token.
    pub stable_id: String,
    pub identity: Value,
}

/// Which flow a background poller is working for.
#[derive(Clone, Copy)]
pub(crate) struct FlowRef<'a> {
    pub user_id: &'a str,
    pub provider_id: &'a str,
    pub flow_id: &'a str,
}

/// What the poller needs to run one device flow.
pub(crate) struct DevicePlan {
    pub device_code: Zeroizing<String>,
    /// Seconds between polls.
    pub interval: u64,
    /// Seconds the codes stay valid.
    pub expires_in: u64,
}

/// The step that turns a provider's tokens into who signed in.
pub type IdentityFn = Arc<dyn Fn(&TokenSet) -> Result<AccountIdentity> + Send + Sync>;

/// A generic OAuth sign-in built from a [`pkce::PkceSpec`] and/or a
/// [`device::DeviceSpec`]. Provider-specific sign-ins wrap this and supply the
/// step that turns tokens into an account (who signed in, which plan).
pub struct OAuthSignIn {
    pub pkce: Option<pkce::PkceSpec>,
    pub device: Option<device::DeviceSpec>,
    pub auth_kind: AuthKind,
    /// The provider name the identity fingerprint is scoped to.
    pub fingerprint_scope: String,
    pub identity: IdentityFn,
}

impl OAuthSignIn {
    fn signed_in(&self, tokens: TokenSet) -> Result<SignedIn> {
        let who = (self.identity)(&tokens)?;
        Ok(SignedIn {
            label: who.label,
            auth_kind: self.auth_kind,
            fingerprint: Some(crate::accounts::identity_fingerprint(
                &self.fingerprint_scope,
                &who.stable_id,
            )),
            identity: who.identity,
            credential: ProviderCredential {
                expires_at: tokens.expires_at(),
                api_key: tokens.access_token,
                refresh_token: tokens.refresh_token.unwrap_or_default(),
                generation: None,
            },
        })
    }
}

#[async_trait]
impl SignInProvider for OAuthSignIn {
    fn flows(&self) -> &'static [FlowKind] {
        match (self.pkce.is_some(), self.device.is_some()) {
            (true, true) => &[FlowKind::Pkce, FlowKind::Device],
            (true, false) => &[FlowKind::Pkce],
            (false, true) => &[FlowKind::Device],
            (false, false) => &[],
        }
    }

    fn redirect_spec(&self) -> Option<RedirectSpec> {
        self.pkce.as_ref().map(|spec| spec.redirect)
    }

    fn authorize_url(&self, redirect_uri: &str, challenge: &str, state: &str) -> Result<String> {
        self.pkce
            .as_ref()
            .ok_or_else(unsupported)?
            .authorize_url(redirect_uri, challenge, state)
    }

    async fn exchange_pkce(
        &self,
        http: &reqwest::Client,
        code: &str,
        redirect_uri: &str,
        verifier: &str,
    ) -> Result<SignedIn> {
        let spec = self.pkce.as_ref().ok_or_else(unsupported)?;
        let tokens = spec.exchange(http, code, redirect_uri, verifier).await?;
        self.signed_in(tokens)
    }

    async fn device_start(&self, http: &reqwest::Client) -> Result<DeviceStart> {
        self.device
            .as_ref()
            .ok_or_else(unsupported)?
            .start(http)
            .await
    }

    async fn device_poll(&self, http: &reqwest::Client, device_code: &str) -> Result<DevicePoll> {
        self.device
            .as_ref()
            .ok_or_else(unsupported)?
            .poll_once(http, device_code)
            .await
    }

    async fn finish_device(&self, _http: &reqwest::Client, tokens: TokenSet) -> Result<SignedIn> {
        self.signed_in(tokens)
    }
}
