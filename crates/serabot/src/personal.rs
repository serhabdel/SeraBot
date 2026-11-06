//! Per-user connectors: Outlook mail, Outlook Calendar, Google Drive, Sheets,
//! Calendar, Gmail, and Notion.
//!
//! Every account connects **its own** mailbox, calendar, drive, or workspace;
//! there is no server-wide token for these products. The sealed credential
//! lives in the per-user envelope store (`user_secrets`), exactly like OneDrive.
//!
//! - **Microsoft** (Outlook, Outlook Calendar): authorization code + PKCE
//!   against the same public-client Entra app that signs users in (no client
//!   secret). Each connector requests only its own delegated scopes, so the
//!   user consents incrementally, one connector at a time. The redirect is the
//!   one already registered for OneDrive.
//! - **Google** (Drive, Sheets, Calendar, Gmail): authorization code + PKCE
//!   against an owner-created Google OAuth client (client id and secret in the
//!   age store, written through the admin connector route). One refresh token
//!   per user; connecting another Google product re-consents with the union of
//!   the scopes the user already granted plus the new one.
//! - **Notion**: the user pastes an internal integration token. Public OAuth is
//!   offered only when `notion_oauth_client_id` / `notion_oauth_client_secret`
//!   are configured.
//!
//! No consent flow ever starts without a client id: the authorize URL builder
//! refuses an empty one, and the status view reports "not set up" instead of a
//! sign-in action. Tokens are never logged, returned, or shared across users.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use base64::Engine;
use rand::RngCore;
use secrecy::ExposeSecret;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::sync::Mutex;
use url::Url;

use crate::config::{Config, OAuthConnectorSettings};
use crate::connectors::{
    ConnectorKind, OAuthProvider, encode_component, map_transport, notion_id_ok, page_size,
    pkce_s256, plausible_secret, redact, redact_json, required_str, sanitize_code, segment_ok,
    sheet_range_ok,
};
use crate::error::{Result, SeraError};
use crate::gateway::{ApprovalDetails, Tool, ToolContext, ToolOutput, ToolRegistry, ToolSpec};
use crate::onedrive::CONSENT_CALLBACK_PATH;
use crate::secrets::SharedSecrets;
use crate::user_secrets::UserSecrets;
use crate::util::truncate_chars;

/// Microsoft consent lands on the redirect already registered for OneDrive, so
/// the owner adds no new redirect URI to the Entra app.
pub const MICROSOFT_CALLBACK_PATH: &str = CONSENT_CALLBACK_PATH;
pub const GOOGLE_CALLBACK_PATH: &str = "/v1/connectors/google/callback";
pub const NOTION_CALLBACK_PATH: &str = "/v1/connectors/notion/callback";
/// Age-store names the admin Google setup writes (shared by the four Google
/// connectors in the default configuration).
pub const GOOGLE_CLIENT_ID_SECRET: &str = "google_oauth_client_id";
pub const GOOGLE_CLIENT_SECRET_SECRET: &str = "google_oauth_client_secret";
pub const NOTION_HELP_URL: &str = "https://www.notion.so/profile/integrations";
pub const NOTION_INSTRUCTIONS: &str =
    "Open the page in Notion, choose the ... menu, then Connections, and add your integration.";

const CONSENT_TTL: Duration = Duration::from_secs(10 * 60);
const MAX_PENDING_CONSENTS: usize = 64;
const MAX_CODE_CHARS: usize = 2048;
const MAX_TOKEN_BYTES: usize = 64 * 1024;
const MAX_API_BYTES: usize = 512 * 1024;
const NOTION_VERSION: &str = "2022-06-28";
const MICROSOFT_IDENTITY_SCOPE: &str = "offline_access openid email";
const GOOGLE_IDENTITY_SCOPE: &str = "openid email";
/// Access tokens are renewed this long before they expire.
const REFRESH_SKEW: Duration = Duration::from_secs(60);

/// The stored, sealed record for one provider. Never printed or returned.
#[derive(Clone, Default, Serialize, Deserialize)]
struct StoredCredential {
    /// OAuth refresh token, or the Notion integration token.
    refresh_token: String,
    #[serde(default)]
    account: Option<String>,
    /// Google only: the connector ids covered by the granted scopes.
    #[serde(default)]
    connectors: Vec<String>,
    /// `oauth` or `token`.
    #[serde(default)]
    method: String,
}

impl std::fmt::Debug for StoredCredential {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("StoredCredential")
            .field("refresh_token", &"<redacted>")
            .field("account", &self.account)
            .finish_non_exhaustive()
    }
}

/// What the console needs to draw one personal connector card.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PersonalView {
    /// `not_set_up`, `ready`, or `connected`.
    pub state: &'static str,
    pub account: Option<String>,
    /// Short note for the admin when the connector is not set up.
    pub hint: Option<String>,
    /// How the user can connect: `oauth`, `token`.
    pub methods: Vec<&'static str>,
    pub help_url: Option<&'static str>,
    pub instructions: Option<&'static str>,
    /// The exact redirect URI the admin registers with the provider.
    pub redirect_uri: Option<String>,
    /// `google` when an admin can set the provider up from the console.
    pub admin_setup: Option<&'static str>,
}

impl PersonalView {
    pub fn to_json(&self) -> Value {
        json!({
            "state": self.state,
            "account": self.account,
            "hint": self.hint,
            "methods": self.methods,
            "help_url": self.help_url,
            "instructions": self.instructions,
            "redirect_uri": self.redirect_uri,
            "admin_setup": self.admin_setup,
        })
    }
}

/// Provider endpoints. Production values are fixed; tests point them at a
/// loopback mock.
#[derive(Debug, Clone)]
struct Endpoints {
    /// Test-only overrides; production derives these from the tenant.
    microsoft_authorize: Option<String>,
    microsoft_token: Option<String>,
    google_authorize: String,
    google_token: String,
    notion_authorize: String,
    notion_token: String,
    api_override: Option<String>,
}

impl Endpoints {
    fn production() -> Self {
        Self {
            microsoft_authorize: None,
            microsoft_token: None,
            google_authorize: "https://accounts.google.com/o/oauth2/v2/auth".into(),
            google_token: "https://oauth2.googleapis.com/token".into(),
            notion_authorize: "https://api.notion.com/v1/oauth/authorize".into(),
            notion_token: "https://api.notion.com/v1/oauth/token".into(),
            api_override: None,
        }
    }

    fn api_base(&self, kind: ConnectorKind) -> String {
        self.api_override
            .clone()
            .unwrap_or_else(|| kind.api_base().to_owned())
    }
}

struct Pending {
    user_id: String,
    kind: ConnectorKind,
    verifier: String,
    /// Google: every connector the resulting grant covers.
    grant: Vec<String>,
    created: Instant,
}

struct CachedAccess {
    token: String,
    expires: Instant,
}

/// Resolved, non-empty OAuth app parameters for one provider.
struct AppParams {
    client_id: String,
    client_secret: Option<String>,
    redirect_url: String,
}

/// Per-user connector service shared by the HTTP API and the tools.
pub struct PersonalConnectors {
    users: Arc<UserSecrets>,
    secrets: SharedSecrets,
    http: reqwest::Client,
    /// `(client-id secret name, tenant)` candidates, in preference order.
    microsoft_apps: Vec<(String, String)>,
    base: String,
    legacy_outlook: [(String, String); 2],
    notion_secrets: (String, String),
    google_secrets: (String, String),
    endpoints: Endpoints,
    pending: Mutex<HashMap<String, Pending>>,
    access: Mutex<HashMap<(String, &'static str), CachedAccess>>,
    refresh_gate: Mutex<()>,
    ttl: Duration,
}

impl std::fmt::Debug for PersonalConnectors {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PersonalConnectors")
            .field("base", &self.base)
            .finish_non_exhaustive()
    }
}

impl PersonalConnectors {
    pub fn new(config: &Config, secrets: &SharedSecrets, users: Arc<UserSecrets>) -> Result<Self> {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|error| SeraError::Internal(format!("connector HTTP client: {error}")))?;
        let base = config
            .onedrive
            .effective_redirect_base(&config.auth)
            .trim()
            .trim_end_matches('/')
            .to_owned();
        let microsoft_apps = vec![
            (
                config.auth.client_id_secret.clone(),
                config.auth.tenant.trim().to_owned(),
            ),
            (
                config.onedrive.client_id_secret.clone(),
                config.onedrive.tenant.trim().to_owned(),
            ),
        ];
        Ok(Self {
            users,
            secrets: secrets.clone(),
            http,
            microsoft_apps,
            base,
            legacy_outlook: [
                (
                    config.connectors.outlook.client_id_secret.clone(),
                    config.connectors.outlook.client_secret_secret.clone(),
                ),
                (
                    config.connectors.outlook_calendar.client_id_secret.clone(),
                    config
                        .connectors
                        .outlook_calendar
                        .client_secret_secret
                        .clone(),
                ),
            ],
            notion_secrets: (
                config.connectors.notion.client_id_secret.clone(),
                config.connectors.notion.client_secret_secret.clone(),
            ),
            google_secrets: (
                config.connectors.google_drive.client_id_secret.clone(),
                config.connectors.google_drive.client_secret_secret.clone(),
            ),
            endpoints: Endpoints::production(),
            pending: Mutex::new(HashMap::new()),
            access: Mutex::new(HashMap::new()),
            refresh_gate: Mutex::new(()),
            ttl: CONSENT_TTL,
        })
    }

    /// Point every provider endpoint (token, authorize, API) at a mock server.
    #[cfg(test)]
    pub(crate) fn with_test_endpoints(mut self, base: &str) -> Self {
        let base = base.trim_end_matches('/');
        self.endpoints = Endpoints {
            microsoft_authorize: Some(format!("{base}/ms/authorize")),
            microsoft_token: Some(format!("{base}/ms/token")),
            google_authorize: format!("{base}/google/authorize"),
            google_token: format!("{base}/google/token"),
            notion_authorize: format!("{base}/notion/authorize"),
            notion_token: format!("{base}/notion/token"),
            api_override: Some(base.to_owned()),
        };
        self
    }

    // ---- Set-up resolution ----

    fn secret(&self, name: &str) -> Option<String> {
        self.secrets
            .get(name)
            .map(|value| value.trim().to_owned())
            .filter(|value| !value.is_empty() && plausible_secret(value))
    }

    fn base_ok(&self) -> bool {
        self.base.starts_with("https://")
            || self.base.starts_with("http://127.0.0.1")
            || self.base.starts_with("http://localhost")
    }

    fn callback_path(provider: OAuthProvider) -> &'static str {
        match provider {
            OAuthProvider::Microsoft => MICROSOFT_CALLBACK_PATH,
            OAuthProvider::Google => GOOGLE_CALLBACK_PATH,
            OAuthProvider::Notion => NOTION_CALLBACK_PATH,
        }
    }

    /// The redirect URI an admin registers with the provider, when the server
    /// knows its public address.
    pub fn redirect_uri(&self, provider: OAuthProvider) -> Option<String> {
        self.base_ok()
            .then(|| format!("{}{}", self.base, Self::callback_path(provider)))
    }

    /// `true` while the old global Outlook client (`outlook_client_id` plus
    /// secret) is configured: that deployment keeps the old admin path.
    fn legacy_outlook_active(&self, kind: ConnectorKind) -> bool {
        let index = match kind {
            ConnectorKind::OutlookMail => 0,
            ConnectorKind::OutlookCalendar => 1,
            _ => return false,
        };
        let (id, secret) = &self.legacy_outlook[index];
        self.secret(id).is_some() && self.secret(secret).is_some()
    }

    /// The OAuth app for a provider, or the admin hint explaining what is
    /// missing. This is the one place a client id is resolved: an empty or
    /// absent id never produces parameters.
    fn app_params(&self, provider: OAuthProvider) -> std::result::Result<AppParams, String> {
        let redirect_url = self.redirect_uri(provider).ok_or_else(|| {
            "Admin: set the server's public https address (auth.base_url) first.".to_owned()
        })?;
        match provider {
            OAuthProvider::Microsoft => {
                let client_id = self
                    .microsoft_apps
                    .iter()
                    .find_map(|(name, _)| self.secret(name))
                    .ok_or_else(|| {
                        "Admin: Microsoft sign-in is not configured on the server (secret auth_client_id)."
                            .to_owned()
                    })?;
                Ok(AppParams {
                    client_id,
                    client_secret: None,
                    redirect_url,
                })
            }
            OAuthProvider::Google => {
                let client_id = self.secret(&self.google_secrets.0);
                let client_secret = self.secret(&self.google_secrets.1);
                match (client_id, client_secret) {
                    (Some(client_id), Some(client_secret)) => Ok(AppParams {
                        client_id,
                        client_secret: Some(client_secret),
                        redirect_url,
                    }),
                    _ => Err(
                        "Admin: Google is not set up yet. Use Set up Google on this screen."
                            .to_owned(),
                    ),
                }
            }
            OAuthProvider::Notion => {
                let client_id = self.secret(&self.notion_secrets.0);
                let client_secret = self.secret(&self.notion_secrets.1);
                match (client_id, client_secret) {
                    (Some(client_id), Some(client_secret)) => Ok(AppParams {
                        client_id,
                        client_secret: Some(client_secret),
                        redirect_url,
                    }),
                    _ => Err("Notion sign-in is not configured.".to_owned()),
                }
            }
        }
    }

    /// Microsoft endpoint for the tenant that belongs to the client id in use.
    fn microsoft_endpoint(&self, authorize: bool) -> String {
        let overridden = if authorize {
            &self.endpoints.microsoft_authorize
        } else {
            &self.endpoints.microsoft_token
        };
        if let Some(url) = overridden {
            return url.clone();
        }
        let tenant = self
            .microsoft_apps
            .iter()
            .find(|(name, _)| self.secret(name).is_some())
            .map_or("consumers", |(_, tenant)| tenant.as_str());
        let leaf = if authorize { "authorize" } else { "token" };
        format!("https://login.microsoftonline.com/{tenant}/oauth2/v2.0/{leaf}")
    }

    /// Whether a consent can start for this connector right now. `Err` carries
    /// the admin hint.
    pub fn consent_ready(&self, kind: ConnectorKind) -> std::result::Result<(), String> {
        self.app_params(kind.provider()).map(|_| ())
    }

    // ---- Credential access ----

    fn credential_id(kind: ConnectorKind) -> &'static str {
        match kind {
            ConnectorKind::OutlookMail => "conn.outlook",
            ConnectorKind::OutlookCalendar => "conn.outlook_calendar",
            ConnectorKind::Notion => "conn.notion",
            ConnectorKind::GoogleDrive
            | ConnectorKind::GoogleSheets
            | ConnectorKind::GoogleCalendar
            | ConnectorKind::Gmail => "conn.google",
        }
    }

    fn title(kind: ConnectorKind) -> &'static str {
        match kind {
            ConnectorKind::OutlookMail => "Outlook",
            ConnectorKind::OutlookCalendar => "Outlook Calendar",
            ConnectorKind::GoogleDrive => "Google Drive",
            ConnectorKind::GoogleSheets => "Google Sheets",
            ConnectorKind::GoogleCalendar => "Google Calendar",
            ConnectorKind::Gmail => "Gmail",
            ConnectorKind::Notion => "Notion",
        }
    }

    fn read_credential(
        &self,
        user_id: &str,
        kind: ConnectorKind,
    ) -> Result<Option<StoredCredential>> {
        let Some(secret) = self.users.get(user_id, Self::credential_id(kind))? else {
            return Ok(None);
        };
        let credential: StoredCredential = serde_json::from_str(secret.expose_secret())
            .map_err(|_| SeraError::Secrets("stored connector credential is unreadable".into()))?;
        if kind.provider() == OAuthProvider::Google
            && !credential
                .connectors
                .iter()
                .any(|id| id.as_str() == kind.section())
        {
            return Ok(None);
        }
        Ok(Some(credential))
    }

    fn write_credential(
        &self,
        user_id: &str,
        kind: ConnectorKind,
        credential: &StoredCredential,
    ) -> Result<()> {
        let encoded = serde_json::to_string(credential)?;
        self.users.set(user_id, Self::credential_id(kind), &encoded)
    }

    /// Whether the user has connected this connector. Never decrypts for
    /// another user; the credential row is the acting user's own.
    pub fn connected(&self, user_id: &str, kind: ConnectorKind) -> Result<bool> {
        Ok(self.read_credential(user_id, kind)?.is_some())
    }

    /// The card state for one connector, or `None` when the old global
    /// Outlook client is configured and keeps the legacy admin card.
    pub fn view(&self, user_id: &str, kind: ConnectorKind) -> Result<Option<PersonalView>> {
        if self.legacy_outlook_active(kind) {
            return Ok(None);
        }
        let provider = kind.provider();
        let redirect_uri = self.redirect_uri(provider);
        let admin_setup = (provider == OAuthProvider::Google).then_some("google");
        let (help_url, instructions) = if provider == OAuthProvider::Notion {
            (Some(NOTION_HELP_URL), Some(NOTION_INSTRUCTIONS))
        } else {
            (None, None)
        };
        let setup = self.app_params(provider);
        let mut methods = Vec::new();
        if setup.is_ok() {
            methods.push("oauth");
        }
        if provider == OAuthProvider::Notion {
            methods.insert(0, "token");
        }
        if let Some(credential) = self.read_credential(user_id, kind)? {
            return Ok(Some(PersonalView {
                state: "connected",
                account: credential.account,
                hint: None,
                methods,
                help_url,
                instructions,
                redirect_uri,
                admin_setup,
            }));
        }
        let (state, hint) = if methods.is_empty() {
            (
                "not_set_up",
                Some(setup.err().unwrap_or_else(|| "Not set up yet.".to_owned())),
            )
        } else {
            ("ready", None)
        };
        Ok(Some(PersonalView {
            state,
            account: None,
            hint,
            methods,
            help_url,
            instructions,
            redirect_uri,
            admin_setup,
        }))
    }

    /// Forget the acting user's connection to one connector. Returns whether
    /// anything was connected.
    pub async fn disconnect(&self, user_id: &str, kind: ConnectorKind) -> Result<bool> {
        let id = Self::credential_id(kind);
        let was_connected = if kind.provider() == OAuthProvider::Google {
            match self.read_credential(user_id, kind)? {
                Some(mut credential) => {
                    credential
                        .connectors
                        .retain(|c| c.as_str() != kind.section());
                    if credential.connectors.is_empty() {
                        self.users.delete(user_id, id)?;
                    } else {
                        self.write_credential(user_id, kind, &credential)?;
                    }
                    true
                }
                None => false,
            }
        } else {
            self.users.delete(user_id, id)?
        };
        self.access
            .lock()
            .await
            .remove(&(user_id.to_owned(), kind.section()));
        Ok(was_connected)
    }

    // ---- Consent ----

    fn scope_for(&self, kind: ConnectorKind, grant: &[String]) -> String {
        match kind.provider() {
            OAuthProvider::Microsoft => {
                let data = match kind {
                    ConnectorKind::OutlookMail => "Mail.ReadWrite Mail.Send",
                    _ => "Calendars.ReadWrite",
                };
                format!("{MICROSOFT_IDENTITY_SCOPE} {data}")
            }
            OAuthProvider::Google => {
                let mut scopes = vec![GOOGLE_IDENTITY_SCOPE.to_owned()];
                for id in grant {
                    if let Some(granted) = ConnectorKind::from_section(id) {
                        scopes.push(granted.scope().to_owned());
                    }
                }
                scopes.join(" ")
            }
            OAuthProvider::Notion => String::new(),
        }
    }

    /// Whether a callback `state` belongs to a pending personal consent.
    pub async fn owns_state(&self, state: &str) -> bool {
        self.pending.lock().await.contains_key(state.trim())
    }

    /// Forget a pending consent (the user declined at the provider).
    pub async fn take_state(&self, state: &str) -> bool {
        self.pending.lock().await.remove(state.trim()).is_some()
    }

    /// Start a consent for the acting user and return the provider's authorize
    /// URL. Fails closed (never builds a URL) while the provider is not set up.
    pub async fn start(&self, user_id: &str, kind: ConnectorKind) -> Result<String> {
        if self.legacy_outlook_active(kind) {
            return Err(SeraError::InvalidState(
                "this connector is managed by the admin".into(),
            ));
        }
        let provider = kind.provider();
        let params = self.app_params(provider).map_err(SeraError::InvalidState)?;
        let mut grant = Vec::new();
        if provider == OAuthProvider::Google {
            if let Some(existing) = self
                .users
                .get(user_id, Self::credential_id(kind))?
                .and_then(|secret| {
                    serde_json::from_str::<StoredCredential>(secret.expose_secret()).ok()
                })
            {
                grant = existing.connectors;
            }
            if !grant.iter().any(|id| id.as_str() == kind.section()) {
                grant.push(kind.section().to_owned());
            }
        }
        let (verifier, challenge) = pkce_s256();
        let mut state_bytes = [0u8; 32];
        rand::rngs::OsRng.fill_bytes(&mut state_bytes);
        let state = hex::encode(state_bytes);
        let scope = self.scope_for(kind, &grant);
        let authorize = match provider {
            OAuthProvider::Microsoft => self.microsoft_endpoint(true),
            OAuthProvider::Google => self.endpoints.google_authorize.clone(),
            OAuthProvider::Notion => self.endpoints.notion_authorize.clone(),
        };
        let url = build_authorize_url(
            &authorize,
            provider,
            &params.client_id,
            &params.redirect_url,
            &scope,
            &state,
            &challenge,
        )?;
        let mut pending = self.pending.lock().await;
        let now = Instant::now();
        pending.retain(|_, consent| now.duration_since(consent.created) < self.ttl);
        if pending.len() >= MAX_PENDING_CONSENTS {
            return Err(SeraError::InvalidState(
                "too many connections are in flight; try again shortly".into(),
            ));
        }
        pending.insert(
            state,
            Pending {
                user_id: user_id.to_owned(),
                kind,
                verifier,
                grant,
                created: now,
            },
        );
        Ok(url)
    }

    /// Finish a consent: exchange the code and seal the credential for the
    /// user who started it. Returns that user and the connector.
    pub async fn complete(&self, state: &str, code: &str) -> Result<(String, ConnectorKind)> {
        let state = state.trim();
        let code = code.trim();
        if state.is_empty() || state.len() > 256 {
            return Err(SeraError::InvalidState("unknown or expired state".into()));
        }
        if code.is_empty() || code.len() > MAX_CODE_CHARS || code.chars().any(char::is_control) {
            return Err(SeraError::Validation(
                "the authorization code is invalid".into(),
            ));
        }
        let pending = {
            let mut pending = self.pending.lock().await;
            let consent = pending
                .remove(state)
                .ok_or_else(|| SeraError::InvalidState("unknown or expired state".into()))?;
            if Instant::now().duration_since(consent.created) > self.ttl {
                return Err(SeraError::InvalidState("unknown or expired state".into()));
            }
            consent
        };
        let kind = pending.kind;
        let provider = kind.provider();
        let params = self.app_params(provider).map_err(SeraError::InvalidState)?;
        let reply = match provider {
            OAuthProvider::Microsoft => {
                let scope = self.scope_for(kind, &pending.grant);
                let request = self.http.post(self.microsoft_endpoint(false)).form(&[
                    ("client_id", params.client_id.as_str()),
                    ("grant_type", "authorization_code"),
                    ("code", code),
                    ("redirect_uri", params.redirect_url.as_str()),
                    ("code_verifier", pending.verifier.as_str()),
                    ("scope", scope.as_str()),
                ]);
                self.token_call(request, "Microsoft").await?
            }
            OAuthProvider::Google => {
                let secret = params.client_secret.clone().unwrap_or_default();
                let request = self.http.post(&self.endpoints.google_token).form(&[
                    ("client_id", params.client_id.as_str()),
                    ("client_secret", secret.as_str()),
                    ("grant_type", "authorization_code"),
                    ("code", code),
                    ("redirect_uri", params.redirect_url.as_str()),
                    ("code_verifier", pending.verifier.as_str()),
                ]);
                self.token_call(request, "Google").await?
            }
            OAuthProvider::Notion => {
                let raw = format!(
                    "{}:{}",
                    params.client_id,
                    params.client_secret.clone().unwrap_or_default()
                );
                let encoded = base64::engine::general_purpose::STANDARD.encode(raw);
                let request = self
                    .http
                    .post(&self.endpoints.notion_token)
                    .header("authorization", format!("Basic {encoded}"))
                    .json(&json!({
                        "grant_type": "authorization_code",
                        "code": code,
                        "redirect_uri": params.redirect_url,
                    }));
                self.token_call(request, "Notion").await?
            }
        };
        let account = match provider {
            OAuthProvider::Notion => reply.notion_account(),
            _ => reply.id_token.as_deref().and_then(id_token_account),
        };
        let token = match provider {
            OAuthProvider::Notion => reply.access_token.clone(),
            _ => reply.refresh_token.clone(),
        }
        .filter(|token| !token.is_empty() && plausible_secret(token))
        .ok_or_else(|| {
            SeraError::External(format!(
                "{} did not return a usable token; consent may have been declined",
                Self::title(kind)
            ))
        })?;
        let credential = StoredCredential {
            refresh_token: token,
            account,
            connectors: pending.grant.clone(),
            method: "oauth".into(),
        };
        self.write_credential(&pending.user_id, kind, &credential)?;
        self.access
            .lock()
            .await
            .retain(|(user, _), _| user != &pending.user_id);
        Ok((pending.user_id, kind))
    }

    /// Store a Notion internal integration token for the acting user after
    /// Notion accepts it. Returns the workspace or bot name for display.
    pub async fn connect_notion_token(&self, user_id: &str, token: &str) -> Result<Option<String>> {
        let token = token.trim();
        if token.is_empty() || token.len() > 512 || !plausible_secret(token) {
            return Err(SeraError::Validation(
                "that does not look like a Notion integration token".into(),
            ));
        }
        let url = format!(
            "{}/v1/users/me",
            self.endpoints.api_base(ConnectorKind::Notion)
        );
        let response = self
            .http
            .get(url)
            .bearer_auth(token)
            .header("Notion-Version", NOTION_VERSION)
            .send()
            .await
            .map_err(|error| map_transport("notion", error))?;
        let status = response.status();
        let bytes = response
            .bytes()
            .await
            .map_err(|error| map_transport("notion", error))?;
        if status == reqwest::StatusCode::UNAUTHORIZED || status == reqwest::StatusCode::FORBIDDEN {
            return Err(SeraError::Validation("Notion rejected that token".into()));
        }
        if !status.is_success() || bytes.len() > MAX_API_BYTES {
            return Err(SeraError::External(format!(
                "Notion could not verify the token (HTTP {})",
                status.as_u16()
            )));
        }
        let value: Value = serde_json::from_slice(&bytes)
            .map_err(|_| SeraError::External("Notion returned an unreadable response".into()))?;
        let account = value
            .pointer("/bot/workspace_name")
            .and_then(Value::as_str)
            .or_else(|| value.get("name").and_then(Value::as_str))
            .map(display_text)
            .filter(|text| !text.is_empty());
        self.write_credential(
            user_id,
            ConnectorKind::Notion,
            &StoredCredential {
                refresh_token: token.to_owned(),
                account: account.clone(),
                connectors: Vec::new(),
                method: "token".into(),
            },
        )?;
        self.access
            .lock()
            .await
            .remove(&(user_id.to_owned(), ConnectorKind::Notion.section()));
        Ok(account)
    }

    async fn token_call(
        &self,
        request: reqwest::RequestBuilder,
        provider: &str,
    ) -> Result<TokenReply> {
        let response = request
            .send()
            .await
            .map_err(|error| map_transport("oauth token", error))?;
        let status = response.status();
        let bytes = response
            .bytes()
            .await
            .map_err(|error| map_transport("oauth token", error))?;
        if bytes.len() > MAX_TOKEN_BYTES {
            return Err(SeraError::External(
                "oauth token response exceeded size limit".into(),
            ));
        }
        let reply: TokenReply = serde_json::from_slice(&bytes).unwrap_or_default();
        if !status.is_success() || reply.error.as_deref().is_some_and(|e| !e.is_empty()) {
            let code = reply
                .error
                .as_deref()
                .map(|code| sanitize_code(code, &[]))
                .unwrap_or_else(|| "request_failed".into());
            if code == "invalid_grant" {
                return Err(SeraError::Forbidden(
                    "the connection has expired or was revoked; connect it again from the console"
                        .into(),
                ));
            }
            return Err(SeraError::External(format!(
                "{provider} rejected the request (HTTP {}): {code}",
                status.as_u16()
            )));
        }
        Ok(reply)
    }

    // ---- Use ----

    /// A current access token for the acting user's own connection. Fails
    /// closed when the user has not connected this product; never falls back
    /// to another account.
    pub async fn access_token(&self, user_id: &str, kind: ConnectorKind) -> Result<String> {
        let key = (user_id.to_owned(), kind.section());
        if let Some(token) = self.cached(&key).await {
            return Ok(token);
        }
        let _gate = self.refresh_gate.lock().await;
        if let Some(token) = self.cached(&key).await {
            return Ok(token);
        }
        let mut credential = self.read_credential(user_id, kind)?.ok_or_else(|| {
            SeraError::Forbidden(format!(
                "{} is not connected for this account; connect it from the console first",
                Self::title(kind)
            ))
        })?;
        let provider = kind.provider();
        if provider == OAuthProvider::Notion {
            // Integration tokens do not expire.
            return Ok(credential.refresh_token);
        }
        let params = self.app_params(provider).map_err(SeraError::InvalidState)?;
        let reply = match provider {
            OAuthProvider::Microsoft => {
                let scope = self.scope_for(kind, &[]);
                let request = self.http.post(self.microsoft_endpoint(false)).form(&[
                    ("client_id", params.client_id.as_str()),
                    ("grant_type", "refresh_token"),
                    ("refresh_token", credential.refresh_token.as_str()),
                    ("scope", scope.as_str()),
                ]);
                self.token_call(request, "Microsoft").await?
            }
            _ => {
                let secret = params.client_secret.clone().unwrap_or_default();
                let request = self.http.post(&self.endpoints.google_token).form(&[
                    ("client_id", params.client_id.as_str()),
                    ("client_secret", secret.as_str()),
                    ("grant_type", "refresh_token"),
                    ("refresh_token", credential.refresh_token.as_str()),
                ]);
                self.token_call(request, "Google").await?
            }
        };
        let access = reply
            .access_token
            .clone()
            .filter(|token| plausible_secret(token))
            .ok_or_else(|| SeraError::External("the provider returned no access token".into()))?;
        if let Some(rotated) = reply
            .refresh_token
            .as_deref()
            .filter(|token| plausible_secret(token) && *token != credential.refresh_token)
        {
            rotated.clone_into(&mut credential.refresh_token);
            self.write_credential(user_id, kind, &credential)?;
        }
        let lifetime = Duration::from_secs(reply.expires_in.unwrap_or(0).min(86_400));
        if lifetime > REFRESH_SKEW {
            self.access.lock().await.insert(
                key,
                CachedAccess {
                    token: access.clone(),
                    expires: Instant::now() + lifetime - REFRESH_SKEW,
                },
            );
        }
        Ok(access)
    }

    async fn cached(&self, key: &(String, &'static str)) -> Option<String> {
        let access = self.access.lock().await;
        access
            .get(key)
            .filter(|cached| cached.expires > Instant::now())
            .map(|cached| cached.token.clone())
    }

    /// Run one read-only call as the acting user.
    pub async fn read(&self, user_id: &str, kind: ConnectorKind, args: &Value) -> Result<Value> {
        let token = self.access_token(user_id, kind).await?;
        let base = Url::parse(&self.endpoints.api_base(kind))
            .map_err(|_| SeraError::Config("connector API base was rejected".into()))?;
        let join = |path: &str| -> Result<Url> {
            base.join(path.trim_start_matches('/'))
                .map_err(|_| SeraError::Config("connector URL was rejected".into()))
        };
        let request = match kind {
            ConnectorKind::GoogleDrive => {
                let mut url = join("/drive/v3/files")?;
                url.query_pairs_mut()
                    .append_pair("pageSize", &page_size(args).to_string())
                    .append_pair("fields", "files(id,name,mimeType),nextPageToken");
                self.http.get(url)
            }
            ConnectorKind::GoogleSheets => {
                let spreadsheet_id = required_str(args, "spreadsheet_id")?;
                let range = required_str(args, "range")?;
                if !segment_ok(spreadsheet_id, 10, 128) {
                    return Err(SeraError::Validation(
                        "spreadsheet_id must be 10..=128 letters, digits, '_' or '-'".into(),
                    ));
                }
                if !sheet_range_ok(range) {
                    return Err(SeraError::Validation(
                        "range must be 1..=128 characters without URL separators".into(),
                    ));
                }
                self.http.get(join(&format!(
                    "/v4/spreadsheets/{}/values/{}",
                    encode_component(spreadsheet_id),
                    encode_component(range)
                ))?)
            }
            ConnectorKind::GoogleCalendar => {
                let mut url = join("/calendar/v3/calendars/primary/events")?;
                let time_min = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string();
                url.query_pairs_mut()
                    .append_pair("maxResults", &page_size(args).to_string())
                    .append_pair("singleEvents", "true")
                    .append_pair("orderBy", "startTime")
                    .append_pair("timeMin", &time_min);
                self.http.get(url)
            }
            ConnectorKind::Gmail => {
                let mut url = join("/gmail/v1/users/me/messages")?;
                url.query_pairs_mut()
                    .append_pair("maxResults", &page_size(args).to_string());
                self.http.get(url)
            }
            ConnectorKind::OutlookMail => {
                let mut url = join("/v1.0/me/messages")?;
                url.query_pairs_mut()
                    .append_pair("$top", &page_size(args).to_string())
                    .append_pair("$select", "id,subject,receivedDateTime");
                self.http.get(url)
            }
            ConnectorKind::OutlookCalendar => {
                let mut url = join("/v1.0/me/events")?;
                url.query_pairs_mut()
                    .append_pair("$top", &page_size(args).to_string())
                    .append_pair("$select", "id,subject,start,end");
                self.http.get(url)
            }
            ConnectorKind::Notion => {
                let database_id = required_str(args, "database_id")?;
                if !notion_id_ok(database_id) {
                    return Err(SeraError::Validation(
                        "database_id must be a Notion UUID".into(),
                    ));
                }
                self.http
                    .post(join(&format!(
                        "/v1/databases/{}/query",
                        encode_component(database_id)
                    ))?)
                    .header("Notion-Version", NOTION_VERSION)
                    .json(&json!({ "page_size": page_size(args) }))
            }
        };
        let response = request
            .bearer_auth(&token)
            .send()
            .await
            .map_err(|error| map_transport(kind.section(), error))?;
        let status = response.status();
        let bytes = response
            .bytes()
            .await
            .map_err(|error| map_transport(kind.section(), error))?;
        if bytes.len() > MAX_API_BYTES {
            return Err(SeraError::External(format!(
                "{} response exceeded size limit",
                kind.section()
            )));
        }
        let secrets = vec![token];
        if !status.is_success() {
            return Err(SeraError::External(redact(
                &format!("{} HTTP {}", kind.section(), status.as_u16()),
                &secrets,
            )));
        }
        let mut value: Value = serde_json::from_slice(&bytes).map_err(|_| {
            SeraError::External(format!("{} returned malformed JSON", kind.section()))
        })?;
        redact_json(&mut value, &secrets);
        Ok(value)
    }
}

/// The authorize URL. Refuses an empty client id so a half-configured server
/// can never send a user to a provider page that will only show an error.
fn build_authorize_url(
    authorize_url: &str,
    provider: OAuthProvider,
    client_id: &str,
    redirect_url: &str,
    scope: &str,
    state: &str,
    challenge: &str,
) -> Result<String> {
    if client_id.trim().is_empty() {
        return Err(SeraError::InvalidState(
            "this connector is not set up yet".into(),
        ));
    }
    let mut url = Url::parse(authorize_url)
        .map_err(|_| SeraError::Config("authorize URL is invalid".into()))?;
    {
        let mut query = url.query_pairs_mut();
        query.append_pair("client_id", client_id);
        query.append_pair("response_type", "code");
        query.append_pair("redirect_uri", redirect_url);
        query.append_pair("state", state);
        match provider {
            OAuthProvider::Microsoft => {
                query.append_pair("response_mode", "query");
                query.append_pair("scope", scope);
                query.append_pair("code_challenge", challenge);
                query.append_pair("code_challenge_method", "S256");
            }
            OAuthProvider::Google => {
                query.append_pair("scope", scope);
                query.append_pair("code_challenge", challenge);
                query.append_pair("code_challenge_method", "S256");
                query.append_pair("access_type", "offline");
                query.append_pair("prompt", "consent");
                query.append_pair("include_granted_scopes", "true");
            }
            OAuthProvider::Notion => {
                query.append_pair("owner", "user");
            }
        }
    }
    Ok(url.to_string())
}

#[derive(Deserialize, Default)]
struct TokenReply {
    #[serde(default)]
    access_token: Option<String>,
    #[serde(default)]
    refresh_token: Option<String>,
    #[serde(default)]
    id_token: Option<String>,
    #[serde(default)]
    expires_in: Option<u64>,
    #[serde(default)]
    error: Option<String>,
    #[serde(default)]
    workspace_name: Option<String>,
    #[serde(default)]
    owner: Option<Value>,
}

impl TokenReply {
    fn notion_account(&self) -> Option<String> {
        self.owner
            .as_ref()
            .and_then(|owner| owner.pointer("/user/person/email"))
            .and_then(Value::as_str)
            .or(self.workspace_name.as_deref())
            .map(display_text)
            .filter(|text| !text.is_empty())
    }
}

/// Printable, bounded text for display next to "Connected as".
fn display_text(value: &str) -> String {
    truncate_chars(
        &value
            .chars()
            .filter(|character| !character.is_control())
            .collect::<String>(),
        120,
    )
}

/// The account email or username from an `id_token`. The token arrives over
/// TLS directly from the provider's token endpoint and is used for display
/// only; it grants nothing.
fn id_token_account(id_token: &str) -> Option<String> {
    let payload = id_token.split('.').nth(1)?;
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(payload.trim_end_matches('='))
        .ok()?;
    let claims: Value = serde_json::from_slice(&bytes).ok()?;
    ["email", "preferred_username"]
        .into_iter()
        .find_map(|claim| claims.get(claim).and_then(Value::as_str))
        .map(display_text)
        .filter(|text| !text.is_empty())
}

/// Register the per-user read tools. A product whose legacy global connector
/// is enabled keeps that tool instead (the names are identical).
pub fn register_personal_tools(
    registry: &mut ToolRegistry,
    config: &Config,
    service: &Arc<PersonalConnectors>,
) -> Result<()> {
    let connectors = &config.connectors;
    for kind in ConnectorKind::ALL {
        let legacy: &dyn OAuthConnectorSettings = match kind {
            ConnectorKind::GoogleDrive => &connectors.google_drive,
            ConnectorKind::GoogleSheets => &connectors.google_sheets,
            ConnectorKind::GoogleCalendar => &connectors.google_calendar,
            ConnectorKind::Gmail => &connectors.gmail,
            ConnectorKind::OutlookMail => &connectors.outlook,
            ConnectorKind::OutlookCalendar => &connectors.outlook_calendar,
            ConnectorKind::Notion => &connectors.notion,
        };
        if legacy.oauth_enabled() {
            continue;
        }
        registry.register(PersonalReadTool {
            service: service.clone(),
            kind,
        })?;
    }
    Ok(())
}

struct PersonalReadTool {
    service: Arc<PersonalConnectors>,
    kind: ConnectorKind,
}

#[async_trait]
impl Tool for PersonalReadTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: self.kind.tool_name().into(),
            description: self.kind.description().into(),
            parameters: self.kind.parameters(),
            external_effect: true,
        }
    }

    fn approval_details(&self, args: &Value) -> Result<ApprovalDetails> {
        let summary = match self.kind {
            ConnectorKind::GoogleSheets => {
                let id = args
                    .get("spreadsheet_id")
                    .and_then(Value::as_str)
                    .unwrap_or("unset");
                format!("Read Google Sheets {}", truncate_chars(id, 80))
            }
            ConnectorKind::Notion => {
                let id = args
                    .get("database_id")
                    .and_then(Value::as_str)
                    .unwrap_or("unset");
                format!("Query Notion database {}", truncate_chars(id, 80))
            }
            _ => self.kind.summary().to_owned(),
        };
        Ok(ApprovalDetails {
            summary,
            destination: Some(self.service.endpoints.api_base(self.kind)),
            artifact_hash: None,
        })
    }

    async fn execute(&self, args: Value, context: &ToolContext) -> Result<ToolOutput> {
        let value = self
            .service
            .read(&context.user_id, self.kind, &args)
            .await?;
        Ok(ToolOutput {
            content: value.to_string(),
            data: Some(value),
            destination: Some(self.service.endpoints.api_base(self.kind)),
            ..Default::default()
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::secrets::SecretStore;
    use crate::store::Store;
    use std::collections::BTreeMap;
    use wiremock::matchers::{body_string_contains, header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn config() -> Config {
        let mut config = Config::default();
        config.auth.base_url = "https://bot.example.example".into();
        config
    }

    fn service(
        secrets: &[(&str, &str)],
        server: Option<&MockServer>,
    ) -> (Arc<PersonalConnectors>, String, String) {
        let store = Store::open_in_memory(384).unwrap();
        let users = Arc::new(UserSecrets::with_master_key(store.clone(), [7u8; 32]).unwrap());
        let alice = store
            .create_user("microsoft:alice", "alice@example.example", "Alice")
            .unwrap()
            .id;
        let bob = store
            .create_user("microsoft:bob", "bob@example.example", "Bob")
            .unwrap()
            .id;
        let map: BTreeMap<String, String> = secrets
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect();
        let secrets = SharedSecrets::new(SecretStore::from_map(map));
        let mut service = PersonalConnectors::new(&config(), &secrets, users).unwrap();
        if let Some(server) = server {
            service = service.with_test_endpoints(&server.uri());
        }
        (Arc::new(service), alice, bob)
    }

    fn id_token(email: &str) -> String {
        let payload = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(json!({"email": email}).to_string());
        format!("e30.{payload}.sig")
    }

    fn query_value(url: &str, key: &str) -> Option<String> {
        Url::parse(url)
            .unwrap()
            .query_pairs()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.into_owned())
    }

    #[tokio::test]
    async fn microsoft_consent_uses_the_public_client_and_only_the_needed_scopes() {
        let (service, alice, _bob) = service(&[("auth_client_id", "entra-client")], None);
        let mail = service
            .start(&alice, ConnectorKind::OutlookMail)
            .await
            .unwrap();
        assert_eq!(query_value(&mail, "client_id").unwrap(), "entra-client");
        assert_eq!(
            query_value(&mail, "scope").unwrap(),
            "offline_access openid email Mail.ReadWrite Mail.Send"
        );
        assert_eq!(query_value(&mail, "code_challenge_method").unwrap(), "S256");
        assert_eq!(
            query_value(&mail, "redirect_uri").unwrap(),
            "https://bot.example.example/v1/connectors/onedrive/callback"
        );
        assert!(!mail.contains("client_secret"));
        let calendar = service
            .start(&alice, ConnectorKind::OutlookCalendar)
            .await
            .unwrap();
        let scope = query_value(&calendar, "scope").unwrap();
        assert!(scope.contains("Calendars.ReadWrite") && !scope.contains("Mail."));
    }

    #[tokio::test]
    async fn no_authorize_url_is_ever_built_without_a_client_id() {
        // Nothing configured at all.
        let (service, alice, _bob) = service(&[], None);
        for kind in ConnectorKind::ALL {
            if kind == ConnectorKind::Notion {
                continue; // Notion connects by pasted token, never by an empty-id OAuth URL.
            }
            let error = service.start(&alice, kind).await.unwrap_err();
            assert!(matches!(error, SeraError::InvalidState(_)), "{kind:?}");
            let view = service.view(&alice, kind).unwrap().unwrap();
            assert_eq!(view.state, "not_set_up", "{kind:?}");
            assert!(view.methods.is_empty() && view.hint.is_some(), "{kind:?}");
        }
        assert!(matches!(
            service.start(&alice, ConnectorKind::Notion).await,
            Err(SeraError::InvalidState(_))
        ));
        // A blank secret value counts as missing, not as an empty client id.
        let (blank, alice, _) = self::service(
            &[
                ("auth_client_id", "   "),
                ("google_oauth_client_id", ""),
                ("google_oauth_client_secret", "s3cretvalue"),
            ],
            None,
        );
        assert!(
            blank
                .start(&alice, ConnectorKind::OutlookMail)
                .await
                .is_err()
        );
        assert!(blank.start(&alice, ConnectorKind::Gmail).await.is_err());
        // The URL builder itself refuses an empty id as the last line of defence.
        assert!(
            build_authorize_url(
                "https://login.example.example/authorize",
                OAuthProvider::Microsoft,
                "  ",
                "https://bot.example.example/cb",
                "scope",
                "state",
                "challenge",
            )
            .is_err()
        );
    }

    #[tokio::test]
    async fn a_missing_public_address_is_reported_as_not_set_up() {
        let store = Store::open_in_memory(384).unwrap();
        let users = Arc::new(UserSecrets::with_master_key(store.clone(), [7u8; 32]).unwrap());
        let alice = store
            .create_user("microsoft:alice", "alice@example.example", "Alice")
            .unwrap()
            .id;
        let secrets = SharedSecrets::new(SecretStore::from_map(BTreeMap::from([(
            "auth_client_id".to_owned(),
            "entra-client".to_owned(),
        )])));
        let service = PersonalConnectors::new(&Config::default(), &secrets, users).unwrap();
        let view = service
            .view(&alice, ConnectorKind::OutlookMail)
            .unwrap()
            .unwrap();
        assert_eq!(view.state, "not_set_up");
        assert!(view.hint.unwrap().contains("auth.base_url"));
        assert!(
            service
                .start(&alice, ConnectorKind::OutlookMail)
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn the_legacy_outlook_client_is_used_only_when_its_secret_is_configured() {
        let (legacy, alice, _) = service(
            &[
                ("outlook_client_id", "legacy-client"),
                ("outlook_client_secret", "legacy-secret"),
            ],
            None,
        );
        assert!(
            legacy
                .view(&alice, ConnectorKind::OutlookMail)
                .unwrap()
                .is_none()
        );
        let (fresh, alice, _) = service(&[("auth_client_id", "entra-client")], None);
        assert_eq!(
            fresh
                .view(&alice, ConnectorKind::OutlookMail)
                .unwrap()
                .unwrap()
                .state,
            "ready"
        );
    }

    #[tokio::test]
    async fn consent_completes_per_user_and_tokens_stay_isolated() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/ms/token"))
            .and(body_string_contains("grant_type=authorization_code"))
            .and(body_string_contains("code_verifier="))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "access_token": "access-alice",
                "refresh_token": "refresh-alice",
                "expires_in": 3600,
                "id_token": id_token("alice@outlook.example"),
            })))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/ms/token"))
            .and(body_string_contains("grant_type=refresh_token"))
            .and(body_string_contains("refresh_token=refresh-alice"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "access_token": "access-alice-2",
                "expires_in": 3600,
            })))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/v1.0/me/messages"))
            .and(header("authorization", "Bearer access-alice-2"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "value": [{"id": "m1", "subject": "Hello"}]
            })))
            .expect(1)
            .mount(&server)
            .await;
        let (service, alice, bob) = service(&[("auth_client_id", "entra-client")], Some(&server));
        let url = service
            .start(&alice, ConnectorKind::OutlookMail)
            .await
            .unwrap();
        let state = query_value(&url, "state").unwrap();
        assert!(service.owns_state(&state).await);
        let (user, kind) = service.complete(&state, "synthetic-code").await.unwrap();
        assert_eq!(
            (user.as_str(), kind),
            (alice.as_str(), ConnectorKind::OutlookMail)
        );
        // The state is single use.
        assert!(service.complete(&state, "synthetic-code").await.is_err());
        assert!(!service.owns_state(&state).await);

        let alice_view = service
            .view(&alice, ConnectorKind::OutlookMail)
            .unwrap()
            .unwrap();
        assert_eq!(alice_view.state, "connected");
        assert_eq!(alice_view.account.as_deref(), Some("alice@outlook.example"));
        // Bob sees his own, unconnected state and cannot use Alice's token.
        let bob_view = service
            .view(&bob, ConnectorKind::OutlookMail)
            .unwrap()
            .unwrap();
        assert_eq!(bob_view.state, "ready");
        assert!(bob_view.account.is_none());
        let error = service
            .read(&bob, ConnectorKind::OutlookMail, &json!({}))
            .await
            .unwrap_err();
        assert!(error.to_string().contains("not connected for this account"));
        // Connecting one Microsoft product does not connect the other.
        assert!(
            !service
                .connected(&alice, ConnectorKind::OutlookCalendar)
                .unwrap()
        );

        let messages = service
            .read(&alice, ConnectorKind::OutlookMail, &json!({"page_size": 5}))
            .await
            .unwrap();
        assert_eq!(messages["value"][0]["id"], "m1");

        // Disconnect is scoped to the acting user.
        assert!(
            !service
                .disconnect(&bob, ConnectorKind::OutlookMail)
                .await
                .unwrap()
        );
        assert!(
            service
                .connected(&alice, ConnectorKind::OutlookMail)
                .unwrap()
        );
        assert!(
            service
                .disconnect(&alice, ConnectorKind::OutlookMail)
                .await
                .unwrap()
        );
        assert!(
            !service
                .connected(&alice, ConnectorKind::OutlookMail)
                .unwrap()
        );
        server.verify().await;
    }

    #[tokio::test]
    async fn google_consent_is_incremental_and_per_connector() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/google/token"))
            .and(body_string_contains("grant_type=authorization_code"))
            .and(body_string_contains("client_secret=google-secret"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "access_token": "g-access",
                "refresh_token": "g-refresh",
                "expires_in": 3600,
                "id_token": id_token("alice@gmail.example"),
            })))
            .mount(&server)
            .await;
        let (service, alice, bob) = service(
            &[
                ("google_oauth_client_id", "google-client"),
                ("google_oauth_client_secret", "google-secret"),
            ],
            Some(&server),
        );
        let url = service.start(&alice, ConnectorKind::Gmail).await.unwrap();
        assert_eq!(query_value(&url, "client_id").unwrap(), "google-client");
        assert_eq!(query_value(&url, "access_type").unwrap(), "offline");
        assert_eq!(
            query_value(&url, "redirect_uri").unwrap(),
            "https://bot.example.example/v1/connectors/google/callback"
        );
        let scope = query_value(&url, "scope").unwrap();
        assert!(scope.contains("gmail.readonly") && !scope.contains("drive"));
        assert!(!url.contains("google-secret"));
        let state = query_value(&url, "state").unwrap();
        service.complete(&state, "synthetic-code").await.unwrap();
        assert!(service.connected(&alice, ConnectorKind::Gmail).unwrap());
        assert!(
            !service
                .connected(&alice, ConnectorKind::GoogleDrive)
                .unwrap()
        );
        assert!(!service.connected(&bob, ConnectorKind::Gmail).unwrap());

        // Enabling Drive later re-consents with the union of both scopes.
        let url = service
            .start(&alice, ConnectorKind::GoogleDrive)
            .await
            .unwrap();
        let scope = query_value(&url, "scope").unwrap();
        assert!(scope.contains("gmail.readonly") && scope.contains("drive.readonly"));
        let state = query_value(&url, "state").unwrap();
        service.complete(&state, "synthetic-code").await.unwrap();
        assert!(service.connected(&alice, ConnectorKind::Gmail).unwrap());
        assert!(
            service
                .connected(&alice, ConnectorKind::GoogleDrive)
                .unwrap()
        );

        // Disconnecting Drive leaves Gmail connected.
        service
            .disconnect(&alice, ConnectorKind::GoogleDrive)
            .await
            .unwrap();
        assert!(service.connected(&alice, ConnectorKind::Gmail).unwrap());
        assert!(
            !service
                .connected(&alice, ConnectorKind::GoogleDrive)
                .unwrap()
        );
    }

    #[tokio::test]
    async fn notion_connects_with_a_pasted_token_and_offers_oauth_only_when_configured() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/users/me"))
            .and(header("authorization", "Bearer ntn_goodtoken123"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "object": "user",
                "bot": {"workspace_name": "Acme Notes"},
            })))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/v1/users/me"))
            .respond_with(ResponseTemplate::new(401))
            .mount(&server)
            .await;
        let (service, alice, bob) = service(&[], Some(&server));
        let view = service
            .view(&alice, ConnectorKind::Notion)
            .unwrap()
            .unwrap();
        assert_eq!(view.state, "ready");
        assert_eq!(view.methods, vec!["token"]);
        assert_eq!(view.help_url, Some(NOTION_HELP_URL));
        assert!(
            service
                .connect_notion_token(&alice, "ntn_badtoken")
                .await
                .is_err()
        );
        let account = service
            .connect_notion_token(&alice, "ntn_goodtoken123")
            .await
            .unwrap();
        assert_eq!(account.as_deref(), Some("Acme Notes"));
        let connected = service
            .view(&alice, ConnectorKind::Notion)
            .unwrap()
            .unwrap();
        assert_eq!(connected.state, "connected");
        assert_eq!(connected.account.as_deref(), Some("Acme Notes"));
        assert_eq!(
            service
                .access_token(&alice, ConnectorKind::Notion)
                .await
                .unwrap(),
            "ntn_goodtoken123"
        );
        assert!(
            service
                .access_token(&bob, ConnectorKind::Notion)
                .await
                .is_err()
        );

        let (with_oauth, alice, _) = self::service(
            &[
                ("notion_oauth_client_id", "notion-client"),
                ("notion_oauth_client_secret", "notion-secret"),
            ],
            None,
        );
        let view = with_oauth
            .view(&alice, ConnectorKind::Notion)
            .unwrap()
            .unwrap();
        assert_eq!(view.methods, vec!["token", "oauth"]);
    }

    #[tokio::test]
    async fn an_expired_grant_asks_the_user_to_reconnect() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/ms/token"))
            .respond_with(
                ResponseTemplate::new(400).set_body_json(json!({"error": "invalid_grant"})),
            )
            .mount(&server)
            .await;
        let (service, alice, _) = service(&[("auth_client_id", "entra-client")], Some(&server));
        service
            .write_credential(
                &alice,
                ConnectorKind::OutlookMail,
                &StoredCredential {
                    refresh_token: "stale-refresh".into(),
                    ..Default::default()
                },
            )
            .unwrap();
        let error = service
            .access_token(&alice, ConnectorKind::OutlookMail)
            .await
            .unwrap_err();
        assert!(error.to_string().contains("connect it again"));
        assert!(!error.to_string().contains("stale-refresh"));
    }

    #[test]
    fn id_token_display_name_is_bounded_and_clean() {
        assert_eq!(
            id_token_account(&id_token("a@b.example")).as_deref(),
            Some("a@b.example")
        );
        assert!(id_token_account("not-a-jwt").is_none());
        assert!(id_token_account("a.b.c").is_none());
    }
}
