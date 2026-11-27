//! Codex / ChatGPT plan sign-in (on by default, `plan_oauth = false` turns it off).
//!
//! This is the sign-in the Codex CLI uses, as the owner's HielCowork client
//! drives it: browser PKCE through the fixed loopback `localhost:1455`, or a
//! device code, against `auth.openai.com` with Codex's public installed-app
//! client id. By the owner's decision it is on unless a provider sets
//! `plan_oauth = false`, and the page shows a risk banner beside it. The
//! account is the member's own, lives in their account list, is sealed under
//! their key and is never shared through a provider grant. A ChatGPT plan is
//! consumer access, not an API contract; API keys on the OpenAI provider are
//! the supported path.
//!
//! - **PKCE**: `GET /oauth/authorize` with `id_token_add_organizations=true`,
//!   then a form POST to `/oauth/token`.
//! - **Device** (not RFC 8628): `POST /api/accounts/deviceauth/usercode` with
//!   `{client_id}` returns a `device_auth_id` and a user code; the member enters
//!   the code at `auth.openai.com/codex/device`; `POST .../deviceauth/token`
//!   with `{device_auth_id, user_code}` answers 202/204/400/404/428 while
//!   waiting and then returns an `authorization_code` and the `code_verifier`,
//!   which the token endpoint trades for tokens (redirect
//!   `auth.openai.com/deviceauth/callback`).
//! - The account id, email and plan come from the JWT claims of the id token
//!   (then the access token). They are read, not verified: the tokens arrived
//!   directly from the pinned token endpoint. The account id is sent on chat
//!   requests as `ChatGPT-Account-Id`.
//! - The refresh token rotates, so every refresh goes through the single-flight
//!   account refresher.
//!
//! Tokens, codes, verifiers and device codes never appear in an error or an
//! event.

use async_trait::async_trait;
use base64::Engine;
use serde_json::{Value, json};

use crate::accounts::{AuthKind, RefreshError, RefreshGrant, identity_fingerprint};
use crate::error::{Result, SeraError};
use crate::user_secrets::ProviderCredential;

use super::device::{DevicePoll, DeviceStart, MAX_INTERVAL_SECS, MAX_WAIT_SECS};
use super::pkce::{PkceSpec, RedirectSpec};
use super::{
    FlowKind, PinnedEndpoint, SignInProvider, SignedIn, TokenSet, credential_ok, read_capped,
};

/// Codex's public installed-app OAuth client id (an identifier, not a secret:
/// it is in every authorize URL).
pub const CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";
pub const AUTH_HOST: &str = "auth.openai.com";
pub const TOKEN_PATH: &str = "/oauth/token";
pub const TOKEN_URL: &str = "https://auth.openai.com/oauth/token";
const AUTHORIZE_PATH: &str = "/oauth/authorize";
const DEVICE_USERCODE_PATH: &str = "/api/accounts/deviceauth/usercode";
const DEVICE_TOKEN_PATH: &str = "/api/accounts/deviceauth/token";
/// Where the member types the device code.
pub const DEVICE_VERIFY_URL: &str = "https://auth.openai.com/codex/device";
const DEVICE_REDIRECT_URI: &str = "https://auth.openai.com/deviceauth/callback";
pub const SCOPE: &str = "openid profile email offline_access";
/// The fixed loopback port and path Codex's client registration allows.
pub const CALLBACK_PORT: u16 = 1455;
pub const CALLBACK_PATH: &str = "/auth/callback";
/// The chat endpoint the signed-in account talks to.
pub const BASE_URL: &str = "https://chatgpt.com/backend-api/codex";
/// The `originator` header Codex requests carry.
pub const ORIGINATOR: &str = "serabot";
/// The beta header the Codex Responses endpoint is sent.
pub const OPENAI_BETA: &str = "responses=v1";
/// The models a ChatGPT plan offers in Codex, as the owner's HielCowork client
/// lists them (the endpoint has no public model list).
pub const MODELS: &[&str] = &[
    "gpt-6.1-sol",
    "gpt-6.1-terra",
    "gpt-6-astra",
    "gpt-6-sol",
    "gpt-6-luna",
    "gpt-5.6-sol",
    "gpt-5.6-terra",
    "gpt-5.6-luna",
];
const AUTH_CLAIM: &str = "https://api.openai.com/auth";
const PROFILE_CLAIM: &str = "https://api.openai.com/profile";
/// The device-code wait, as HielCowork bounds it (15 minutes).
const DEVICE_WAIT_SECS: u64 = 900;

fn redirect_spec() -> RedirectSpec {
    RedirectSpec {
        port: Some(CALLBACK_PORT),
        path: Some(CALLBACK_PATH),
    }
}

/// What the JWT claims of a sign-in say about who signed in. Everything is
/// non-secret and bounded.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Claims {
    pub account_id: Option<String>,
    pub email: Option<String>,
    pub plan: Option<String>,
    /// The access token's `exp`, Unix seconds.
    pub expires_at: Option<i64>,
}

fn jwt_payload(jwt: &str) -> Option<Value> {
    let payload = jwt.split('.').nth(1)?;
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(payload.trim_end_matches('='))
        .ok()?;
    serde_json::from_slice(&bytes).ok()
}

fn id_ok(value: &str) -> bool {
    (1..=64).contains(&value.len())
        && value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'))
}

fn email_ok(email: &str) -> bool {
    email.len() <= 254
        && email.contains('@')
        && !email.chars().any(|c| c.is_control() || c.is_whitespace())
}

fn text<'a>(value: &'a Value, path: &[&str]) -> Option<&'a str> {
    let mut node = value;
    for key in path {
        node = node.get(*key)?;
    }
    node.as_str()
}

/// Read the account id, email and plan from the id token, then the access token.
pub fn claims(id_token: Option<&str>, access_token: &str) -> Claims {
    let mut found = Claims::default();
    let access = jwt_payload(access_token);
    found.expires_at = access
        .as_ref()
        .and_then(|payload| payload.get("exp"))
        .and_then(Value::as_i64)
        .filter(|at| *at > 0);
    for payload in [id_token.and_then(jwt_payload), access]
        .into_iter()
        .flatten()
    {
        if found.account_id.is_none() {
            found.account_id = text(&payload, &["chatgpt_account_id"])
                .or_else(|| text(&payload, &[AUTH_CLAIM, "chatgpt_account_id"]))
                .map(str::trim)
                .filter(|id| id_ok(id))
                .map(str::to_owned);
        }
        if found.email.is_none() {
            found.email = text(&payload, &["email"])
                .or_else(|| text(&payload, &["preferred_username"]))
                .or_else(|| text(&payload, &[PROFILE_CLAIM, "email"]))
                .map(|email| email.trim().to_lowercase())
                .filter(|email| email_ok(email));
        }
        if found.plan.is_none() {
            found.plan = text(&payload, &[AUTH_CLAIM, "chatgpt_plan_type"])
                .map(str::trim)
                .filter(|plan| {
                    (1..=32).contains(&plan.len())
                        && plan
                            .chars()
                            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'))
                })
                .map(str::to_owned);
        }
    }
    found
}

/// The Codex plan sign-in, registered under the `openai-codex` kind.
pub struct CodexSignIn {
    pkce: PkceSpec,
    device_start: PinnedEndpoint,
    device_token: PinnedEndpoint,
}

impl CodexSignIn {
    /// The sign-in against OpenAI's hosts.
    pub fn new() -> Self {
        Self::with_endpoints(
            PinnedEndpoint::https(AUTH_HOST, AUTHORIZE_PATH),
            PinnedEndpoint::https(AUTH_HOST, TOKEN_PATH),
            PinnedEndpoint::https(AUTH_HOST, DEVICE_USERCODE_PATH),
            PinnedEndpoint::https(AUTH_HOST, DEVICE_TOKEN_PATH),
        )
    }

    fn with_endpoints(
        authorize: PinnedEndpoint,
        token: PinnedEndpoint,
        device_start: PinnedEndpoint,
        device_token: PinnedEndpoint,
    ) -> Self {
        Self {
            pkce: PkceSpec {
                authorize,
                token,
                client_id: CLIENT_ID.to_owned(),
                client_secret: None,
                scope: SCOPE.to_owned(),
                redirect: redirect_spec(),
                extra_params: vec![("id_token_add_organizations".into(), "true".into())],
            },
            device_start,
            device_token,
        }
    }

    /// Point every endpoint at a mock server on the exact loopback.
    #[cfg(test)]
    pub(crate) fn for_test(base: &str) -> Self {
        Self::with_endpoints(
            PinnedEndpoint::loopback(&format!("{base}/authorize")),
            PinnedEndpoint::loopback(&format!("{base}/token")),
            PinnedEndpoint::loopback(&format!("{base}/usercode")),
            PinnedEndpoint::loopback(&format!("{base}/devicetoken")),
        )
    }

    /// Turn tokens into the account they belong to.
    fn signed_in(tokens: TokenSet) -> Result<SignedIn> {
        // The refresh token rotates; without one the account would stop working
        // within hours.
        let Some(refresh_token) = tokens.refresh_token.clone() else {
            return Err(SeraError::Rejected {
                code: "no_refresh_token",
                message:
                    "The provider did not return a long-lived sign-in. Start the sign-in again."
                        .into(),
            });
        };
        let who = claims(tokens.id_token.as_deref(), &tokens.access_token);
        // No stable identity, no account: signing in twice must not make two.
        let stable = match (&who.account_id, &who.email) {
            (Some(id), _) => format!("account:{id}"),
            (None, Some(email)) => format!("email:{email}"),
            (None, None) => {
                return Err(SeraError::Rejected {
                    code: "no_stable_identity",
                    message: "ChatGPT did not say who signed in, so the account was not saved"
                        .into(),
                });
            }
        };
        let mut identity = json!({"plan": "ChatGPT plan"});
        if let Some(plan) = &who.plan {
            identity["plan"] = json!(format!("ChatGPT {plan}"));
        }
        if let Some(email) = &who.email {
            identity["email"] = json!(email);
        }
        if let Some(id) = &who.account_id {
            identity["chatgpt_account_id"] = json!(id);
        }
        Ok(SignedIn {
            label: who.email.clone().unwrap_or_else(|| "ChatGPT plan".into()),
            auth_kind: AuthKind::Oauth,
            fingerprint: Some(identity_fingerprint("openai-codex", &stable)),
            identity,
            credential: ProviderCredential {
                expires_at: who.expires_at.or_else(|| tokens.expires_at()),
                api_key: tokens.access_token,
                refresh_token,
                generation: None,
            },
        })
    }

    fn json_post(
        &self,
        http: &reqwest::Client,
        endpoint: &PinnedEndpoint,
    ) -> Result<reqwest::RequestBuilder> {
        Ok(http
            .post(endpoint.checked_url()?)
            .header("accept", "application/json"))
    }
}

impl Default for CodexSignIn {
    fn default() -> Self {
        Self::new()
    }
}

fn unreachable_error() -> SeraError {
    SeraError::External("the sign-in provider could not be reached".into())
}

/// A poll interval the provider sent as a number or a numeric string.
fn interval_secs(value: Option<&Value>) -> u64 {
    value
        .and_then(|value| {
            value
                .as_u64()
                .or_else(|| value.as_str().and_then(|text| text.trim().parse().ok()))
        })
        .unwrap_or(5)
        .clamp(1, MAX_INTERVAL_SECS)
}

#[async_trait]
impl SignInProvider for CodexSignIn {
    fn flows(&self) -> &'static [FlowKind] {
        &[FlowKind::Pkce, FlowKind::Device]
    }

    fn redirect_spec(&self) -> Option<RedirectSpec> {
        Some(redirect_spec())
    }

    fn authorize_url(&self, redirect_uri: &str, challenge: &str, state: &str) -> Result<String> {
        self.pkce.authorize_url(redirect_uri, challenge, state)
    }

    async fn exchange_pkce(
        &self,
        http: &reqwest::Client,
        code: &str,
        redirect_uri: &str,
        verifier: &str,
    ) -> Result<SignedIn> {
        let tokens = self
            .pkce
            .exchange(http, code, redirect_uri, verifier)
            .await?;
        Self::signed_in(tokens)
    }

    async fn device_start(&self, http: &reqwest::Client) -> Result<DeviceStart> {
        let response = self
            .json_post(http, &self.device_start)?
            .json(&json!({"client_id": CLIENT_ID}))
            .send()
            .await
            .map_err(|_| unreachable_error())?;
        let status = response.status();
        let body = read_capped(response).await?;
        if !status.is_success() {
            return Err(SeraError::Rejected {
                code: "signin_unavailable",
                message: "ChatGPT could not start a device sign-in. Turn on device code authorization in ChatGPT settings, or use the browser sign-in.".into(),
            });
        }
        let value: Value = serde_json::from_slice(&body).map_err(|_| {
            SeraError::External("the sign-in provider sent an invalid reply".into())
        })?;
        let field = |names: &[&str]| {
            names.iter().find_map(|name| {
                value
                    .get(*name)
                    .and_then(Value::as_str)
                    .filter(|text| credential_ok(text) && text.len() <= 128)
                    .map(str::to_owned)
            })
        };
        let incomplete =
            || SeraError::External("the sign-in provider sent an incomplete reply".into());
        let user_code = field(&["user_code", "usercode"]).ok_or_else(incomplete)?;
        let device_auth_id = field(&["device_auth_id"]).ok_or_else(incomplete)?;
        Ok(DeviceStart {
            // The poll needs both; the flow keeps one opaque code.
            device_code: format!("{device_auth_id}\n{user_code}"),
            user_code,
            verification_uri: DEVICE_VERIFY_URL.to_owned(),
            interval: interval_secs(value.get("interval")),
            expires_in: DEVICE_WAIT_SECS.min(MAX_WAIT_SECS),
        })
    }

    async fn device_poll(&self, http: &reqwest::Client, device_code: &str) -> Result<DevicePoll> {
        let Some((device_auth_id, user_code)) = device_code.split_once('\n') else {
            return Ok(DevicePoll::Failed("provider_refused"));
        };
        let response = self
            .json_post(http, &self.device_token)?
            .json(&json!({"device_auth_id": device_auth_id, "user_code": user_code}))
            .send()
            .await
            .map_err(|_| unreachable_error())?;
        let status = response.status().as_u16();
        if (500..600).contains(&status) {
            return Err(SeraError::External(
                "the sign-in provider is unavailable".into(),
            ));
        }
        let body = read_capped(response).await?;
        // Still waiting: no content, accepted, not found, or "not yet".
        if matches!(status, 202 | 204 | 400 | 404 | 428) {
            return Ok(DevicePoll::Pending);
        }
        if status == 429 {
            return Ok(DevicePoll::SlowDown);
        }
        if !(200..300).contains(&status) {
            return Ok(DevicePoll::Failed("provider_refused"));
        }
        let value: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
        let grant = |name: &str| {
            value
                .get(name)
                .and_then(Value::as_str)
                .filter(|text| credential_ok(text))
        };
        let (Some(code), Some(verifier)) = (grant("authorization_code"), grant("code_verifier"))
        else {
            return Ok(DevicePoll::Pending);
        };
        // The approval is one-shot: whatever goes wrong now ends the flow.
        let url = self.pkce.token.checked_url()?;
        let response = http
            .post(url)
            .header("accept", "application/json")
            .form(&[
                ("grant_type", "authorization_code"),
                ("code", code),
                ("redirect_uri", DEVICE_REDIRECT_URI),
                ("client_id", CLIENT_ID),
                ("code_verifier", verifier),
            ])
            .send()
            .await;
        let Ok(response) = response else {
            return Ok(DevicePoll::Failed("provider_error"));
        };
        let ok = response.status().is_success();
        let Ok(body) = read_capped(response).await else {
            return Ok(DevicePoll::Failed("provider_error"));
        };
        if !ok {
            return Ok(DevicePoll::Failed("provider_refused"));
        }
        Ok(match TokenSet::from_body(&body) {
            Ok(tokens) => DevicePoll::Done(tokens),
            Err(_) => DevicePoll::Failed("bad_token_reply"),
        })
    }

    async fn finish_device(&self, _http: &reqwest::Client, tokens: TokenSet) -> Result<SignedIn> {
        Self::signed_in(tokens)
    }
}

/// One refresh of a Codex plan token. The refresh token rotates: the reply
/// carries the next one (the old one is kept when it does not). A refused token
/// (`invalid_grant`, 400 or 401) is terminal; a 403 (no Codex access) and
/// anything else leaves the credential untouched.
pub async fn refresh_exchange(
    http: &reqwest::Client,
    token_url: &str,
    refresh_token: &str,
    client_id: &str,
) -> std::result::Result<RefreshGrant, RefreshError> {
    let failed = |message: &str| RefreshError::Failed(SeraError::External(message.into()));
    let response = http
        .post(token_url)
        .header("accept", "application/json")
        .form(&[
            ("grant_type", "refresh_token"),
            ("refresh_token", refresh_token),
            ("client_id", client_id),
        ])
        .send()
        .await
        .map_err(|_| failed("oauth token refresh failed"))?;
    let status = response.status();
    let bytes = read_capped(response)
        .await
        .map_err(|_| failed("oauth token refresh failed"))?;
    if !status.is_success() {
        let code = serde_json::from_slice::<Value>(&bytes)
            .ok()
            .and_then(|body| {
                body.get("error")
                    .and_then(|error| error.as_str().or_else(|| error.get("code")?.as_str()))
                    .map(str::to_owned)
            })
            .unwrap_or_default();
        if matches!(status.as_u16(), 400 | 401)
            || matches!(code.as_str(), "invalid_grant" | "invalid_client")
        {
            return Err(RefreshError::InvalidGrant);
        }
        if status.as_u16() == 403 {
            return Err(failed(
                "ChatGPT refused this Codex session; confirm the account has Codex access",
            ));
        }
        return Err(RefreshError::Failed(SeraError::External(format!(
            "oauth token refresh failed with status {}",
            status.as_u16()
        ))));
    }
    let tokens = TokenSet::from_body(&bytes).map_err(|_| failed("oauth token refresh failed"))?;
    let who = claims(tokens.id_token.as_deref(), &tokens.access_token);
    Ok(RefreshGrant {
        expires_at: who.expires_at.or_else(|| tokens.expires_at()),
        access_token: tokens.access_token,
        refresh_token: tokens.refresh_token,
    })
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    use sha2::{Digest, Sha256};
    use wiremock::matchers::{body_json, body_string_contains, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};
    use zeroize::Zeroizing;

    use super::*;
    use crate::accounts::Accounts;
    use crate::auth_flows::{
        CompleteRequest, DevicePlan, FlowRecord, FlowRef, FlowStatus, SignInRegistry,
        SignInService, StartRequest,
    };
    use crate::store::Store;
    use crate::user_secrets::UserSecrets;

    const REDIRECT: &str = "http://localhost:1455/auth/callback";

    struct Harness {
        store: Store,
        accounts: Arc<Accounts>,
    }

    fn harness() -> Harness {
        let store = Store::open_in_memory(384).unwrap();
        let secrets = Arc::new(
            UserSecrets::with_master_key(store.clone(), [8u8; crate::user_secrets::KEY_BYTES])
                .unwrap(),
        );
        Harness {
            accounts: Arc::new(Accounts::new(store.clone(), secrets)),
            store,
        }
    }

    fn member(harness: &Harness, subject: &str) -> String {
        harness
            .store
            .create_user(subject, "member@example.example", "Synthetic Member")
            .unwrap()
            .id
    }

    fn service(harness: &Harness, server: &MockServer) -> Arc<SignInService> {
        let mut registry = SignInRegistry::default();
        registry.register(
            "openai-codex",
            Arc::new(CodexSignIn::for_test(&server.uri())),
        );
        Arc::new(
            SignInService::new(harness.accounts.clone())
                .unwrap()
                .with_registry(registry),
        )
    }

    fn jwt(claims: Value) -> String {
        let part = |value: &Value| {
            base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(value.to_string())
        };
        format!("{}.{}.sig", part(&json!({"alg": "none"})), part(&claims))
    }

    /// An id token and access token naming one ChatGPT account.
    fn plan_tokens(account: Option<&str>, email: Option<&str>, exp: i64) -> Value {
        let mut id = json!({"sub": "user-1"});
        if let Some(email) = email {
            id["email"] = json!(email);
        }
        if let Some(account) = account {
            id[AUTH_CLAIM] = json!({"chatgpt_account_id": account, "chatgpt_plan_type": "plus"});
        }
        json!({
            "access_token": jwt(json!({"exp": exp, AUTH_CLAIM: {"chatgpt_account_id": account}})),
            "id_token": jwt(id),
            "refresh_token": "refresh-synthetic-1",
            "expires_in": 3600
        })
    }

    async fn mount_token(server: &MockServer, code: &str, body: Value) {
        Mock::given(method("POST"))
            .and(path("/token"))
            .and(body_string_contains(format!("code={code}")))
            .respond_with(ResponseTemplate::new(200).set_body_json(body))
            .mount(server)
            .await;
    }

    fn start_request() -> StartRequest {
        StartRequest {
            flow: Some(FlowKind::Pkce),
            redirect_uri: Some(REDIRECT.into()),
            ..Default::default()
        }
    }

    fn complete_request(flow_id: &str, code: &str) -> CompleteRequest {
        CompleteRequest {
            flow_id: Some(flow_id.into()),
            code: code.into(),
            redirect_uri: Some(REDIRECT.into()),
            verifier: None,
        }
    }

    #[test]
    fn claims_read_the_account_email_and_plan_from_the_tokens() {
        let exp = chrono::Utc::now().timestamp() + 7_200;
        let tokens = plan_tokens(Some("acct-1"), Some("Person@Example.Example"), exp);
        let who = claims(
            tokens["id_token"].as_str(),
            tokens["access_token"].as_str().unwrap(),
        );
        assert_eq!(who.account_id.as_deref(), Some("acct-1"));
        assert_eq!(who.email.as_deref(), Some("person@example.example"));
        assert_eq!(who.plan.as_deref(), Some("plus"));
        assert_eq!(who.expires_at, Some(exp));
        // A top-level claim, the access token alone, and garbage all work or fail soft.
        let top =
            jwt(json!({"chatgpt_account_id": "top-1", "preferred_username": "x@example.example"}));
        let who = claims(None, &top);
        assert_eq!(who.account_id.as_deref(), Some("top-1"));
        assert_eq!(who.email.as_deref(), Some("x@example.example"));
        assert_eq!(claims(Some("nonsense"), "also.nonsense"), Claims::default());
        // A hostile account id is not kept.
        let hostile = jwt(json!({"chatgpt_account_id": "a b\nc"}));
        assert!(claims(None, &hostile).account_id.is_none());
    }

    #[test]
    fn the_models_match_the_owners_hielcowork_codex_list() {
        assert_eq!(
            MODELS,
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
    }

    #[test]
    fn the_fingerprint_is_sha256_of_the_account_identity() {
        assert_eq!(
            identity_fingerprint("openai-codex", "account:acct-1"),
            hex::encode(Sha256::digest(b"openai-codex:account:acct-1"))
        );
    }

    #[tokio::test]
    async fn the_authorization_url_follows_the_codex_client() {
        let harness = harness();
        let alice = member(&harness, "microsoft:codex-url");
        let server = MockServer::start().await;
        let service = service(&harness, &server);
        let started = service
            .start(&alice, "codex", "openai-codex", start_request())
            .await
            .unwrap();
        let url = url::Url::parse(&started.url).unwrap();
        let query: BTreeMap<String, String> = url
            .query_pairs()
            .map(|(key, value)| (key.into_owned(), value.into_owned()))
            .collect();
        assert_eq!(query["client_id"], CLIENT_ID);
        assert_eq!(query["response_type"], "code");
        assert_eq!(query["code_challenge_method"], "S256");
        assert_eq!(query["redirect_uri"], REDIRECT);
        assert_eq!(query["scope"], SCOPE);
        assert_eq!(query["id_token_add_organizations"], "true");
        assert_eq!(query["state"], started.state.clone().unwrap());
        // The production endpoint is auth.openai.com; only fixtures use loopback.
        let real = CodexSignIn::new()
            .authorize_url(REDIRECT, &"c".repeat(43), &"s".repeat(16))
            .unwrap();
        assert!(real.starts_with("https://auth.openai.com/oauth/authorize?"));
    }

    #[tokio::test]
    async fn the_code_is_exchanged_as_a_form_and_stored_sealed_with_a_stable_identity() {
        let harness = harness();
        let alice = member(&harness, "microsoft:codex-exchange");
        let server = MockServer::start().await;
        let exp = chrono::Utc::now().timestamp() + 7_200;
        Mock::given(method("POST"))
            .and(path("/token"))
            .and(body_string_contains("grant_type=authorization_code"))
            .and(body_string_contains("code=code-synthetic"))
            .and(body_string_contains(format!("client_id={CLIENT_ID}")))
            .and(body_string_contains(
                "redirect_uri=http%3A%2F%2Flocalhost%3A1455%2Fauth%2Fcallback",
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(plan_tokens(
                Some("acct-1"),
                Some("Person@Example.Example"),
                exp,
            )))
            .expect(1)
            .mount(&server)
            .await;
        let service = service(&harness, &server);
        let started = service
            .start(&alice, "codex", "openai-codex", start_request())
            .await
            .unwrap();
        let outcome = service
            .complete(
                &alice,
                "codex",
                "openai-codex",
                complete_request(&started.flow_id, "code-synthetic"),
            )
            .await
            .unwrap();
        assert!(outcome.created);
        let sent =
            String::from_utf8(server.received_requests().await.unwrap()[0].body.clone()).unwrap();
        assert!(sent.contains("code_verifier="));
        let account = harness
            .accounts
            .get(&alice, "codex", &outcome.account_id)
            .unwrap()
            .unwrap();
        assert_eq!(account.auth_kind, AuthKind::Oauth);
        assert_eq!(account.label, "person@example.example");
        assert_eq!(account.identity["email"], "person@example.example");
        assert_eq!(account.identity["chatgpt_account_id"], "acct-1");
        assert_eq!(account.identity["plan"], "ChatGPT plus");
        let stored = harness
            .accounts
            .credential(&alice, "codex", &outcome.account_id)
            .unwrap()
            .unwrap();
        assert_eq!(stored.refresh_token, "refresh-synthetic-1");
        // The expiry is the access token's own `exp`.
        assert_eq!(stored.expires_at, Some(exp));
        let shown = serde_json::to_string(&account).unwrap();
        for secret in ["refresh-synthetic-1", "code-synthetic", &stored.api_key] {
            assert!(!shown.contains(secret), "{secret}");
        }
    }

    #[tokio::test]
    async fn signing_in_twice_updates_one_account_and_two_workspaces_stay_apart() {
        let harness = harness();
        let alice = member(&harness, "microsoft:codex-dupe");
        let server = MockServer::start().await;
        let exp = chrono::Utc::now().timestamp() + 7_200;
        for (code, account) in [("c1", "acct-1"), ("c2", "acct-1"), ("c3", "acct-2")] {
            mount_token(
                &server,
                code,
                plan_tokens(Some(account), Some("p@example.example"), exp),
            )
            .await;
        }
        let service = service(&harness, &server);
        let mut outcomes = Vec::new();
        for code in ["c1", "c2", "c3"] {
            let started = service
                .start(&alice, "codex", "openai-codex", start_request())
                .await
                .unwrap();
            outcomes.push(
                service
                    .complete(
                        &alice,
                        "codex",
                        "openai-codex",
                        complete_request(&started.flow_id, code),
                    )
                    .await
                    .unwrap(),
            );
        }
        assert!(outcomes[0].created && !outcomes[1].created);
        assert_eq!(outcomes[0].account_id, outcomes[1].account_id);
        assert!(outcomes[2].created);
        assert_ne!(outcomes[0].account_id, outcomes[2].account_id);
        assert_eq!(harness.accounts.list(&alice, "codex").unwrap().len(), 2);
    }

    #[tokio::test]
    async fn a_login_with_no_identity_or_no_refresh_token_stores_nothing() {
        let harness = harness();
        let alice = member(&harness, "microsoft:codex-refuse");
        let server = MockServer::start().await;
        let exp = chrono::Utc::now().timestamp() + 7_200;
        let mut nobody = plan_tokens(None, None, exp);
        nobody["access_token"] = json!(jwt(json!({"exp": exp})));
        mount_token(&server, "no-id", nobody).await;
        let mut no_refresh = plan_tokens(Some("acct-9"), Some("n@example.example"), exp);
        no_refresh.as_object_mut().unwrap().remove("refresh_token");
        mount_token(&server, "no-refresh", no_refresh).await;
        let service = service(&harness, &server);
        for (code, expected) in [
            ("no-id", "no_stable_identity"),
            ("no-refresh", "no_refresh_token"),
        ] {
            let started = service
                .start(&alice, "codex", "openai-codex", start_request())
                .await
                .unwrap();
            let error = service
                .complete(
                    &alice,
                    "codex",
                    "openai-codex",
                    complete_request(&started.flow_id, code),
                )
                .await
                .unwrap_err();
            match error {
                SeraError::Rejected { code, .. } => assert_eq!(code, expected),
                other => panic!("{other:?}"),
            }
        }
        assert!(harness.accounts.list(&alice, "codex").unwrap().is_empty());
    }

    #[tokio::test]
    async fn a_flow_belongs_to_its_member_and_only_the_fixed_loopback_is_accepted() {
        let harness = harness();
        let alice = member(&harness, "microsoft:codex-owner");
        let bob = member(&harness, "microsoft:codex-thief");
        let server = MockServer::start().await;
        let service = service(&harness, &server);
        let started = service
            .start(&alice, "codex", "openai-codex", start_request())
            .await
            .unwrap();
        let stolen = service
            .complete(
                &bob,
                "codex",
                "openai-codex",
                complete_request(&started.flow_id, "mine"),
            )
            .await
            .unwrap_err();
        assert!(matches!(stolen, SeraError::NotFound(_)));
        assert!(server.received_requests().await.unwrap().is_empty());
        for bad in [
            "http://localhost:1456/auth/callback",
            "http://localhost:1455/callback",
            "https://auth.openai.com/auth/callback",
            "http://evil.example:1455/auth/callback",
        ] {
            let error = service
                .start(
                    &alice,
                    "codex",
                    "openai-codex",
                    StartRequest {
                        flow: Some(FlowKind::Pkce),
                        redirect_uri: Some(bad.into()),
                        ..Default::default()
                    },
                )
                .await
                .unwrap_err();
            assert!(
                matches!(
                    error,
                    SeraError::Rejected {
                        code: "bad_redirect",
                        ..
                    }
                ),
                "{bad}"
            );
        }
    }

    #[tokio::test]
    async fn a_refused_code_leaves_no_secret_in_the_error() {
        let harness = harness();
        let alice = member(&harness, "microsoft:codex-refused");
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/token"))
            .respond_with(ResponseTemplate::new(400).set_body_json(
                json!({"error": "invalid_grant", "error_description": "code-secret"}),
            ))
            .mount(&server)
            .await;
        let service = service(&harness, &server);
        let started = service
            .start(&alice, "codex", "openai-codex", start_request())
            .await
            .unwrap();
        let error = service
            .complete(
                &alice,
                "codex",
                "openai-codex",
                complete_request(&started.flow_id, "bad-code"),
            )
            .await
            .unwrap_err();
        let text = format!("{error:?} {error}");
        assert!(matches!(
            error,
            SeraError::Rejected {
                code: "signin_refused",
                ..
            }
        ));
        assert!(!text.contains("bad-code") && !text.contains("code-secret"));
        assert!(harness.accounts.list(&alice, "codex").unwrap().is_empty());
    }

    fn device_flow() -> FlowRecord {
        let now = Instant::now();
        FlowRecord {
            provider: "codex".into(),
            kind: FlowKind::Device,
            redirect_uri: None,
            state: None,
            verifier: None,
            client_holds_verifier: false,
            expires: now + Duration::from_secs(900),
            gateway: None,

            created: now,
            status: FlowStatus::Pending,
            consumed: false,
        }
    }

    async fn mount_usercode(server: &MockServer) {
        Mock::given(method("POST"))
            .and(path("/usercode"))
            .and(body_json(json!({"client_id": CLIENT_ID})))
            .respond_with(ResponseTemplate::new(200).set_body_json(
                json!({"device_auth_id": "dev-auth-1", "user_code": "ABCD-1234", "interval": "7"}),
            ))
            .mount(server)
            .await;
    }

    #[tokio::test]
    async fn the_device_flow_starts_with_the_code_and_the_fixed_page() {
        let harness = harness();
        let alice = member(&harness, "microsoft:codex-device-start");
        let server = MockServer::start().await;
        mount_usercode(&server).await;
        let service = service(&harness, &server);
        let started = service
            .start(
                &alice,
                "codex",
                "openai-codex",
                StartRequest {
                    flow: Some(FlowKind::Device),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        assert_eq!(started.url, DEVICE_VERIFY_URL);
        assert_eq!(started.user_code.as_deref(), Some("ABCD-1234"));
        assert_eq!(started.interval, Some(7));
        assert!(
            !serde_json::to_string(&started)
                .unwrap()
                .contains("dev-auth-1")
        );
        // A server with device authorization off says so in plain words.
        let off = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/usercode"))
            .respond_with(ResponseTemplate::new(404))
            .mount(&off)
            .await;
        let error = self::service(&harness, &off)
            .start(
                &alice,
                "codex",
                "openai-codex",
                StartRequest {
                    flow: Some(FlowKind::Device),
                    ..Default::default()
                },
            )
            .await
            .unwrap_err();
        assert!(matches!(
            error,
            SeraError::Rejected {
                code: "signin_unavailable",
                ..
            }
        ));
    }

    /// Poll answers in order: each is served once.
    async fn poll_answers(server: &MockServer, answers: Vec<ResponseTemplate>) {
        for answer in answers {
            Mock::given(method("POST"))
                .and(path("/devicetoken"))
                .and(body_json(
                    json!({"device_auth_id": "dev-auth-1", "user_code": "ABCD-1234"}),
                ))
                .respond_with(answer)
                .up_to_n_times(1)
                .mount(server)
                .await;
        }
    }

    async fn run_device(
        harness: &Harness,
        server: &MockServer,
        user: &str,
    ) -> (FlowStatus, Arc<SignInService>) {
        let service = service(harness, server);
        let flow_id = service.flows.insert(user, device_flow());
        let provider = service.registry.get("openai-codex").cloned().unwrap();
        service
            .run_device_flow(
                &FlowRef {
                    user_id: user,
                    provider_id: "codex",
                    flow_id: &flow_id,
                },
                provider,
                DevicePlan {
                    device_code: Zeroizing::new("dev-auth-1\nABCD-1234".into()),
                    interval: 1,
                    expires_in: 900,
                },
                |_| async {},
            )
            .await;
        let status = service.status(user, "codex", &flow_id).unwrap();
        (status, service)
    }

    #[tokio::test]
    async fn the_device_poll_waits_through_pending_answers_then_exchanges_the_grant() {
        let harness = harness();
        let alice = member(&harness, "microsoft:codex-device-ok");
        let server = MockServer::start().await;
        let exp = chrono::Utc::now().timestamp() + 7_200;
        poll_answers(
            &server,
            vec![
                ResponseTemplate::new(404),
                ResponseTemplate::new(204),
                ResponseTemplate::new(400),
                ResponseTemplate::new(428),
                ResponseTemplate::new(200).set_body_json(
                    json!({"authorization_code": "device-code-1", "code_verifier": "verifier-1"}),
                ),
            ],
        )
        .await;
        Mock::given(method("POST"))
            .and(path("/token"))
            .and(body_string_contains("code=device-code-1"))
            .and(body_string_contains("code_verifier=verifier-1"))
            .and(body_string_contains(
                "redirect_uri=https%3A%2F%2Fauth.openai.com%2Fdeviceauth%2Fcallback",
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(plan_tokens(
                Some("acct-d"),
                Some("d@example.example"),
                exp,
            )))
            .expect(1)
            .mount(&server)
            .await;
        let (status, _) = run_device(&harness, &server, &alice).await;
        let FlowStatus::Ok {
            account_id,
            created,
        } = status
        else {
            panic!("{status:?}");
        };
        assert!(created);
        let stored = harness
            .accounts
            .credential(&alice, "codex", &account_id)
            .unwrap()
            .unwrap();
        assert_eq!(stored.refresh_token, "refresh-synthetic-1");
        assert_eq!(
            harness
                .accounts
                .get(&alice, "codex", &account_id)
                .unwrap()
                .unwrap()
                .identity["chatgpt_account_id"],
            "acct-d"
        );
    }

    #[tokio::test]
    async fn a_refused_device_poll_is_terminal_and_a_5xx_is_tolerated() {
        let harness = harness();
        let alice = member(&harness, "microsoft:codex-device-no");
        let server = MockServer::start().await;
        poll_answers(
            &server,
            vec![ResponseTemplate::new(503), ResponseTemplate::new(403)],
        )
        .await;
        let (status, _) = run_device(&harness, &server, &alice).await;
        assert_eq!(
            status,
            FlowStatus::Denied {
                reason: "provider_refused".into()
            }
        );
        assert!(harness.accounts.list(&alice, "codex").unwrap().is_empty());
    }

    #[tokio::test]
    async fn refresh_rotates_and_the_failure_classes_differ() {
        let server = MockServer::start().await;
        let http = crate::auth_flows::http_client().unwrap();
        let exp = chrono::Utc::now().timestamp() + 7_200;
        Mock::given(method("POST"))
            .and(path("/ok"))
            .and(body_string_contains("grant_type=refresh_token"))
            .and(body_string_contains("refresh_token=old-refresh"))
            .and(body_string_contains(format!("client_id={CLIENT_ID}")))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "access_token": jwt(json!({"exp": exp})),
                "refresh_token": "new-refresh",
                "expires_in": 60
            })))
            .mount(&server)
            .await;
        let grant = refresh_exchange(
            &http,
            &format!("{}/ok", server.uri()),
            "old-refresh",
            CLIENT_ID,
        )
        .await
        .unwrap();
        assert_eq!(grant.refresh_token.as_deref(), Some("new-refresh"));
        // The access token's own `exp` wins over `expires_in`.
        assert_eq!(grant.expires_at, Some(exp));
        for (status, body) in [
            (400, json!({"error": "invalid_grant"})),
            (401, json!({})),
            (200, json!({"nope": 1})),
        ] {
            Mock::given(method("POST"))
                .and(path(format!("/r-{status}")))
                .respond_with(ResponseTemplate::new(status).set_body_json(body))
                .mount(&server)
                .await;
        }
        let url = |name: &str| format!("{}/{name}", server.uri());
        for name in ["r-400", "r-401"] {
            let refused = refresh_exchange(&http, &url(name), "r", CLIENT_ID).await;
            assert!(matches!(refused, Err(RefreshError::InvalidGrant)), "{name}");
        }
        // A reply with no token, a 403 (no Codex access) and a 5xx keep the credential.
        Mock::given(method("POST"))
            .and(path("/r-403"))
            .respond_with(ResponseTemplate::new(403))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/r-503"))
            .respond_with(ResponseTemplate::new(503))
            .mount(&server)
            .await;
        for name in ["r-200", "r-403", "r-503"] {
            let kept = refresh_exchange(&http, &url(name), "r", CLIENT_ID).await;
            assert!(matches!(kept, Err(RefreshError::Failed(_))), "{name}");
        }
    }
}
