//! GitHub Copilot member sign-in: GitHub's device flow, then the Copilot token
//! exchange (on by default, `plan_oauth = false` turns it off).
//!
//! This follows the owner's HielCowork client (`OAuthService.cs`,
//! `CopilotService.cs`):
//!
//! - `POST https://github.com/login/device/code` with the Copilot extension's
//!   public client id and scope `read:user`; the member approves at
//!   `github.com/login/device` and the token endpoint
//!   `https://github.com/login/oauth/access_token` is polled generically
//!   (RFC 8628; GitHub answers a pending poll with HTTP 200 and an `error`).
//! - The GitHub token is long-lived and does not rotate. It is kept as the
//!   credential's `refresh_token`.
//! - `GET https://api.github.com/copilot_internal/v2/token` (bearer = the GitHub
//!   token, plus the editor identity headers) returns a Copilot token that lasts
//!   about half an hour, its `expires_at`, and `endpoints.api`, the host the
//!   plan is served from (individual, business and enterprise differ). The
//!   Copilot token is the credential's `api_key`; the host is kept in the
//!   account's identity when it is an `https` `*.githubcopilot.com` host.
//!   Every refresh repeats this exchange, through the single-flight account
//!   refresher. A 401 means GitHub no longer accepts the saved sign-in.
//! - `GET https://api.github.com/user` names the account (login and numeric id).
//!   The id is the stable identity; a sign-in that cannot say who signed in is
//!   not saved.
//!
//! None of this is a published GitHub API for third-party apps: the client id
//! and the editor headers are the VS Code Copilot extension's, so the page shows
//! a risk banner and GitHub may restrict an account used this way. The account
//! is the member's own, sealed under their key and never shared through a
//! provider grant. Tokens and device codes never appear in an error or an
//! event.

use async_trait::async_trait;
use serde_json::{Value, json};

use crate::accounts::{AuthKind, RefreshError, RefreshGrant, identity_fingerprint};
use crate::error::{Result, SeraError};
use crate::user_secrets::ProviderCredential;

use super::device::{DevicePoll, DeviceSpec, DeviceStart};
use super::{
    FlowKind, PinnedEndpoint, SignInProvider, SignedIn, TokenSet, credential_ok, read_capped,
};

/// The VS Code Copilot extension's public GitHub OAuth app id (an identifier,
/// not a secret: it is in every device-code request).
pub const CLIENT_ID: &str = "Iv1.b507a08c87ecfe98";
const DEVICE_HOST: &str = "github.com";
const DEVICE_PATH: &str = "/login/device/code";
const POLL_PATH: &str = "/login/oauth/access_token";
/// The host the member's verification page must be on.
const VERIFICATION_HOST: &str = "github.com";
pub const API_HOST: &str = "api.github.com";
pub const TOKEN_PATH: &str = "/copilot_internal/v2/token";
pub const TOKEN_URL: &str = "https://api.github.com/copilot_internal/v2/token";
const USER_PATH: &str = "/user";
const SCOPE: &str = "read:user";
/// The chat endpoint of an individual plan; business and enterprise accounts
/// name their own host in the token reply.
pub const BASE_URL: &str = "https://api.githubcopilot.com";
/// Hosts a plan may be served from.
const API_HOST_SUFFIX: &str = ".githubcopilot.com";

/// The editor identity the Copilot API wants on every call (HielCowork's
/// `CopilotService.ApplyEditorHeaders`).
pub const EDITOR_HEADERS: [(&str, &str); 5] = [
    ("user-agent", "GitHubCopilotChat/0.32.4"),
    ("editor-version", "vscode/1.105.1"),
    ("editor-plugin-version", "copilot-chat/0.32.4"),
    ("copilot-integration-id", "vscode-chat"),
    ("x-github-api-version", "2026-08-01"),
];
/// Sent on the model-list call only.
pub const CATALOG_INTENT: (&str, &str) = ("openai-intent", "model-access");

/// What the token exchange returned. Holds a secret; `Debug` hides it.
pub struct CopilotToken {
    pub token: String,
    /// Unix seconds when the token stops working.
    pub expires_at: i64,
    /// The `https` host the plan is served from, when the reply named a valid one.
    pub api_base: Option<String>,
}

impl std::fmt::Debug for CopilotToken {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("CopilotToken")
            .field("expires_at", &self.expires_at)
            .field("api_base", &self.api_base)
            .finish_non_exhaustive()
    }
}

/// Why the token exchange failed.
#[derive(Debug)]
pub enum ExchangeError {
    /// GitHub no longer accepts the GitHub token.
    Unauthorized,
    /// No active Copilot subscription (or an organisation disabled it).
    NoSubscription,
    Other(SeraError),
}

/// The `https://host` of a plan's API when it is a GitHub Copilot host.
pub fn api_base_ok(value: &str) -> Option<String> {
    let url = url::Url::parse(value).ok()?;
    let host = url.host_str()?.to_ascii_lowercase();
    let valid = url.scheme() == "https"
        && url.port().is_none()
        && url.username().is_empty()
        && url.password().is_none()
        && (host == "githubcopilot.com" || host.ends_with(API_HOST_SUFFIX));
    valid.then(|| format!("https://{host}"))
}

/// Add the editor identity headers to a request.
fn with_editor(mut request: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
    for (name, value) in EDITOR_HEADERS {
        request = request.header(name, value);
    }
    request
}

/// Trade the long-lived GitHub token for a Copilot token.
pub async fn exchange_token(
    http: &reqwest::Client,
    url: url::Url,
    github_token: &str,
) -> std::result::Result<CopilotToken, ExchangeError> {
    let other = |message: &str| ExchangeError::Other(SeraError::External(message.into()));
    let request = with_editor(
        http.get(url)
            .header("accept", "application/json")
            .bearer_auth(github_token),
    );
    let response = request
        .send()
        .await
        .map_err(|_| other("the Copilot token exchange could not be reached"))?;
    let status = response.status();
    let body = read_capped(response).await.map_err(ExchangeError::Other)?;
    match status.as_u16() {
        401 => return Err(ExchangeError::Unauthorized),
        403 | 404 => return Err(ExchangeError::NoSubscription),
        _ if !status.is_success() => {
            return Err(other("the Copilot token exchange was refused"));
        }
        _ => {}
    }
    let value: Value =
        serde_json::from_slice(&body).map_err(|_| other("the Copilot token reply was invalid"))?;
    let token = value
        .get("token")
        .and_then(Value::as_str)
        .filter(|token| credential_ok(token))
        .ok_or_else(|| other("GitHub did not return a Copilot token"))?
        .to_owned();
    // HielCowork's fallback when the reply names no expiry: 25 minutes.
    let expires_at = value
        .get("expires_at")
        .and_then(Value::as_i64)
        .filter(|at| *at > 0)
        .unwrap_or_else(|| chrono::Utc::now().timestamp() + 25 * 60);
    let api_base = value
        .pointer("/endpoints/api")
        .and_then(Value::as_str)
        .and_then(api_base_ok);
    Ok(CopilotToken {
        token,
        expires_at,
        api_base,
    })
}

/// One refresh of a Copilot token: the exchange again, from the GitHub token.
/// The GitHub token does not rotate, so no refresh token is returned. A 401 is
/// terminal (sign in again); a lost subscription or anything else leaves the
/// credential untouched.
pub async fn refresh_exchange(
    http: &reqwest::Client,
    token_url: &str,
    github_token: &str,
    _client_id: &str,
) -> std::result::Result<RefreshGrant, RefreshError> {
    let url = url::Url::parse(token_url)
        .map_err(|_| RefreshError::Failed(SeraError::Config("invalid oauth token url".into())))?;
    match exchange_token(http, url, github_token).await {
        Ok(copilot) => Ok(RefreshGrant {
            access_token: copilot.token,
            refresh_token: None,
            expires_at: Some(copilot.expires_at),
        }),
        Err(ExchangeError::Unauthorized) => Err(RefreshError::InvalidGrant),
        Err(ExchangeError::NoSubscription) => Err(RefreshError::Failed(SeraError::External(
            "this GitHub account has no active Copilot subscription".into(),
        ))),
        Err(ExchangeError::Other(error)) => Err(RefreshError::Failed(error)),
    }
}

/// Who a GitHub token belongs to: the numeric id (stable) and the login.
struct GitHubUser {
    id: Option<u64>,
    login: Option<String>,
}

fn login_ok(login: &str) -> bool {
    (1..=39).contains(&login.len()) && login.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
}

/// The GitHub Copilot sign-in, registered under the `github-copilot` kind.
pub struct CopilotSignIn {
    device: DeviceSpec,
    token: PinnedEndpoint,
    user: PinnedEndpoint,
}

impl CopilotSignIn {
    /// The sign-in against GitHub's hosts.
    pub fn new() -> Self {
        Self {
            device: DeviceSpec {
                device: PinnedEndpoint::https(DEVICE_HOST, DEVICE_PATH),
                token: PinnedEndpoint::https(DEVICE_HOST, POLL_PATH),
                client_id: CLIENT_ID.to_owned(),
                client_secret: None,
                scope: SCOPE.to_owned(),
                verification_hosts: vec![VERIFICATION_HOST.to_owned()],
                headers: Vec::new(),
            },
            token: PinnedEndpoint::https(API_HOST, TOKEN_PATH),
            user: PinnedEndpoint::https(API_HOST, USER_PATH),
        }
    }

    /// Point every endpoint at a mock server on the exact loopback.
    #[cfg(test)]
    pub(crate) fn for_test(base: &str) -> Self {
        Self {
            device: DeviceSpec {
                device: PinnedEndpoint::loopback(&format!("{base}/device")),
                token: PinnedEndpoint::loopback(&format!("{base}/poll")),
                client_id: CLIENT_ID.to_owned(),
                client_secret: None,
                scope: SCOPE.to_owned(),
                verification_hosts: vec![VERIFICATION_HOST.to_owned()],
                headers: Vec::new(),
            },
            token: PinnedEndpoint::loopback(&format!("{base}/copilot_internal/v2/token")),
            user: PinnedEndpoint::loopback(&format!("{base}/user")),
        }
    }

    async fn read_user(&self, http: &reqwest::Client, github_token: &str) -> Result<GitHubUser> {
        let response = http
            .get(self.user.checked_url()?)
            .header("accept", "application/json")
            .bearer_auth(github_token)
            .send()
            .await
            .map_err(|_| SeraError::External("GitHub could not be reached".into()))?;
        let status = response.status();
        let body = read_capped(response).await?;
        if !status.is_success() {
            return Err(SeraError::External(
                "GitHub did not name the account".into(),
            ));
        }
        let value: Value = serde_json::from_slice(&body)
            .map_err(|_| SeraError::External("GitHub sent an invalid account reply".into()))?;
        Ok(GitHubUser {
            id: value.get("id").and_then(Value::as_u64).filter(|id| *id > 0),
            login: value
                .get("login")
                .and_then(Value::as_str)
                .filter(|login| login_ok(login))
                .map(str::to_owned),
        })
    }
}

impl Default for CopilotSignIn {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl SignInProvider for CopilotSignIn {
    fn flows(&self) -> &'static [FlowKind] {
        &[FlowKind::Device]
    }

    async fn device_start(&self, http: &reqwest::Client) -> Result<DeviceStart> {
        self.device.start(http).await
    }

    async fn device_poll(&self, http: &reqwest::Client, device_code: &str) -> Result<DevicePoll> {
        self.device.poll_once(http, device_code).await
    }

    async fn finish_device(&self, http: &reqwest::Client, tokens: TokenSet) -> Result<SignedIn> {
        let github_token = tokens.access_token;
        let copilot = exchange_token(http, self.token.checked_url()?, &github_token)
            .await
            .map_err(|error| match error {
                ExchangeError::NoSubscription => SeraError::Rejected {
                    code: "copilot_not_subscribed",
                    message: "This GitHub account has no active Copilot subscription (or an organisation has disabled it). Check github.com/settings/copilot.".into(),
                },
                ExchangeError::Unauthorized => SeraError::Rejected {
                    code: "copilot_refused",
                    message: "GitHub did not accept the sign-in. Try again.".into(),
                },
                ExchangeError::Other(error) => error,
            })?;
        let user = self.read_user(http, &github_token).await?;
        let stable_id = match (user.id, &user.login) {
            (Some(id), _) => format!("id:{id}"),
            (None, Some(login)) => format!("login:{}", login.to_ascii_lowercase()),
            (None, None) => {
                return Err(SeraError::Rejected {
                    code: "no_stable_identity",
                    message: "GitHub did not say who signed in, so the account was not saved"
                        .into(),
                });
            }
        };
        let mut identity = json!({});
        if let Some(login) = &user.login {
            identity["login"] = json!(login);
        }
        if let Some(base) = &copilot.api_base {
            identity["api_base"] = json!(base);
        }
        Ok(SignedIn {
            label: user.login.unwrap_or_else(|| "GitHub Copilot".into()),
            auth_kind: AuthKind::Device,
            fingerprint: Some(identity_fingerprint("github-copilot", &stable_id)),
            identity,
            credential: ProviderCredential {
                api_key: copilot.token,
                refresh_token: github_token,
                expires_at: Some(copilot.expires_at),
                generation: None,
            },
        })
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::Duration;

    use serde_json::{Value, json};
    use wiremock::matchers::{body_string_contains, header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};
    use zeroize::Zeroizing;

    use super::*;
    use crate::accounts::Accounts;
    use crate::auth_flows::{
        DevicePlan, FlowRecord, FlowRef, FlowStatus, SignInRegistry, SignInService, StartRequest,
    };
    use crate::store::Store;
    use crate::user_secrets::UserSecrets;

    struct Harness {
        store: Store,
        accounts: Arc<Accounts>,
    }

    fn harness() -> Harness {
        let store = Store::open_in_memory(384).unwrap();
        let secrets = Arc::new(
            UserSecrets::with_master_key(store.clone(), [6u8; crate::user_secrets::KEY_BYTES])
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
            "github-copilot",
            Arc::new(CopilotSignIn::for_test(&server.uri())),
        );
        Arc::new(
            SignInService::new(harness.accounts.clone())
                .unwrap()
                .with_registry(registry),
        )
    }

    fn device_flow() -> FlowRecord {
        let now = std::time::Instant::now();
        FlowRecord {
            provider: "github-copilot".into(),
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

    async fn poll_answers(server: &MockServer, answers: Vec<Value>) {
        for body in answers {
            Mock::given(method("POST"))
                .and(path("/poll"))
                .and(body_string_contains("device_code=dc-synthetic"))
                // GitHub answers every poll with 200.
                .respond_with(ResponseTemplate::new(200).set_body_json(body))
                .up_to_n_times(1)
                .mount(server)
                .await;
        }
    }

    fn exchange_reply(expires_at: i64) -> Value {
        json!({
            "token": "tid=synthetic;exp=1;sku=free",
            "expires_at": expires_at,
            "endpoints": {"api": "https://api.business.githubcopilot.com"}
        })
    }

    async fn mount_exchange(server: &MockServer, status: u16, body: Value) {
        Mock::given(method("GET"))
            .and(path("/copilot_internal/v2/token"))
            .and(header("authorization", "Bearer gho_synthetic_github"))
            .and(header("editor-version", "vscode/1.105.1"))
            .and(header("editor-plugin-version", "copilot-chat/0.32.4"))
            .and(header("copilot-integration-id", "vscode-chat"))
            .and(header("x-github-api-version", "2026-08-01"))
            .and(header("user-agent", "GitHubCopilotChat/0.32.4"))
            .respond_with(ResponseTemplate::new(status).set_body_json(body))
            .mount(server)
            .await;
    }

    async fn mount_user(server: &MockServer, body: Value) {
        Mock::given(method("GET"))
            .and(path("/user"))
            .and(header("authorization", "Bearer gho_synthetic_github"))
            .respond_with(ResponseTemplate::new(200).set_body_json(body))
            .mount(server)
            .await;
    }

    async fn run(
        harness: &Harness,
        server: &MockServer,
        user: &str,
    ) -> (FlowStatus, Arc<SignInService>) {
        let service = service(harness, server);
        let flow_id = service.flows.insert(user, device_flow());
        let provider = service.registry.get("github-copilot").cloned().unwrap();
        service
            .run_device_flow(
                &FlowRef {
                    user_id: user,
                    provider_id: "github-copilot",
                    flow_id: &flow_id,
                },
                provider,
                DevicePlan {
                    device_code: Zeroizing::new("dc-synthetic".into()),
                    interval: 1,
                    expires_in: 900,
                },
                |_| async {},
            )
            .await;
        let status = service.status(user, "github-copilot", &flow_id).unwrap();
        (status, service)
    }

    fn approved() -> Value {
        json!({"access_token": "gho_synthetic_github", "token_type": "bearer", "scope": "read:user"})
    }

    #[tokio::test]
    async fn the_device_flow_exchanges_the_token_and_stores_both_secrets_sealed() {
        let harness = harness();
        let alice = member(&harness, "microsoft:copilot-ok");
        let server = MockServer::start().await;
        poll_answers(
            &server,
            vec![
                json!({"error": "authorization_pending"}),
                json!({"error": "slow_down"}),
                approved(),
            ],
        )
        .await;
        let expires = chrono::Utc::now().timestamp() + 1800;
        mount_exchange(&server, 200, exchange_reply(expires)).await;
        mount_user(&server, json!({"id": 4242, "login": "Synthetic-User"})).await;
        let (status, _service) = run(&harness, &server, &alice).await;
        let FlowStatus::Ok {
            account_id,
            created,
        } = status
        else {
            panic!("expected a signed-in flow, got {status:?}");
        };
        assert!(created);
        let account = harness
            .accounts
            .get(&alice, "github-copilot", &account_id)
            .unwrap()
            .unwrap();
        assert_eq!(account.auth_kind, AuthKind::Device);
        assert_eq!(account.identity["login"], "Synthetic-User");
        assert_eq!(
            account.identity["api_base"],
            "https://api.business.githubcopilot.com"
        );
        // Nothing secret is in the non-secret identity.
        let shown = account.identity.to_string();
        assert!(!shown.contains("gho_synthetic") && !shown.contains("tid="));
        // The Copilot token is the key; the GitHub token is kept to exchange again.
        let credential = harness
            .accounts
            .credential(&alice, "github-copilot", &account_id)
            .unwrap()
            .unwrap();
        assert_eq!(credential.api_key, "tid=synthetic;exp=1;sku=free");
        assert_eq!(credential.refresh_token, "gho_synthetic_github");
        assert_eq!(credential.expires_at, Some(expires));
        // Another member has no such account.
        let bob = member(&harness, "microsoft:copilot-ok-bob");
        assert!(
            harness
                .accounts
                .list(&bob, "github-copilot")
                .unwrap()
                .is_empty()
        );
    }

    #[tokio::test]
    async fn the_same_github_user_is_one_account_and_another_is_a_second() {
        let harness = harness();
        let alice = member(&harness, "microsoft:copilot-multi");
        let mut ids = Vec::new();
        for (github_id, login) in [(7, "first"), (7, "renamed"), (8, "second")] {
            let server = MockServer::start().await;
            poll_answers(&server, vec![approved()]).await;
            mount_exchange(
                &server,
                200,
                exchange_reply(chrono::Utc::now().timestamp() + 1800),
            )
            .await;
            mount_user(&server, json!({"id": github_id, "login": login})).await;
            let (status, _) = run(&harness, &server, &alice).await;
            let FlowStatus::Ok { account_id, .. } = status else {
                panic!("{status:?}");
            };
            ids.push(account_id);
        }
        // A rename is the same account; a different id is another.
        assert_eq!(ids[0], ids[1]);
        assert_ne!(ids[0], ids[2]);
        assert_eq!(
            harness
                .accounts
                .list(&alice, "github-copilot")
                .unwrap()
                .len(),
            2
        );
    }

    #[tokio::test]
    async fn a_failed_exchange_or_unnamed_user_stores_nothing() {
        for (status, body, user, reason) in [
            (
                403,
                json!({}),
                json!({"id": 1, "login": "x"}),
                "copilot_not_subscribed",
            ),
            (
                404,
                json!({}),
                json!({"id": 1, "login": "x"}),
                "copilot_not_subscribed",
            ),
            (
                401,
                json!({}),
                json!({"id": 1, "login": "x"}),
                "copilot_refused",
            ),
            (
                500,
                json!({}),
                json!({"id": 1, "login": "x"}),
                "provider_error",
            ),
            (
                200,
                json!({"nope": 1}),
                json!({"id": 1, "login": "x"}),
                "provider_error",
            ),
            (
                200,
                exchange_reply(chrono::Utc::now().timestamp() + 1800),
                json!({"name": "nobody"}),
                "no_stable_identity",
            ),
        ] {
            let harness = harness();
            let alice = member(
                &harness,
                &format!("microsoft:copilot-fail-{reason}-{status}"),
            );
            let server = MockServer::start().await;
            poll_answers(&server, vec![approved()]).await;
            mount_exchange(&server, status, body.clone()).await;
            mount_user(&server, user.clone()).await;
            let (flow, _) = run(&harness, &server, &alice).await;
            assert_eq!(
                flow,
                FlowStatus::Denied {
                    reason: reason.into()
                },
                "{status} {body}"
            );
            assert!(
                harness
                    .accounts
                    .list(&alice, "github-copilot")
                    .unwrap()
                    .is_empty()
            );
        }
    }

    #[tokio::test]
    async fn denial_and_expiry_end_the_flow() {
        for (answer, expected) in [
            (
                json!({"error": "access_denied"}),
                FlowStatus::Denied {
                    reason: "access_denied".into(),
                },
            ),
            (json!({"error": "expired_token"}), FlowStatus::Expired),
        ] {
            let harness = harness();
            let alice = member(&harness, "microsoft:copilot-end");
            let server = MockServer::start().await;
            poll_answers(&server, vec![answer]).await;
            let (status, _) = run(&harness, &server, &alice).await;
            assert_eq!(status, expected);
        }
    }

    #[tokio::test]
    async fn starting_the_flow_sends_the_scope_and_pins_the_verification_page() {
        let harness = harness();
        let alice = member(&harness, "microsoft:copilot-start");
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/device"))
            .and(header("accept", "application/json"))
            .and(body_string_contains("client_id=Iv1.b507a08c87ecfe98"))
            .and(body_string_contains("scope=read%3Auser"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "device_code": "dc-synthetic",
                "user_code": "ABCD-1234",
                "verification_uri": "https://github.com/login/device",
                "interval": 5,
                "expires_in": 900
            })))
            .mount(&server)
            .await;
        poll_answers(&server, vec![json!({"error": "access_denied"})]).await;
        let started = service(&harness, &server)
            .start(
                &alice,
                "github-copilot",
                "github-copilot",
                StartRequest {
                    flow: Some(FlowKind::Device),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        assert_eq!(started.user_code.as_deref(), Some("ABCD-1234"));
        assert_eq!(started.url, "https://github.com/login/device");
        assert_eq!(started.interval, Some(5));
        // A page on another host is refused before any code is shown.
        let hostile = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/device"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "device_code": "dc-synthetic",
                "user_code": "ABCD-1234",
                "verification_uri": "https://evil.example/login/device"
            })))
            .mount(&hostile)
            .await;
        assert!(
            service(&harness, &hostile)
                .start(
                    &alice,
                    "github-copilot",
                    "github-copilot",
                    StartRequest {
                        flow: Some(FlowKind::Device),
                        ..Default::default()
                    },
                )
                .await
                .is_err()
        );
        // There is no browser flow.
        assert!(
            service(&harness, &server)
                .start(
                    &alice,
                    "github-copilot",
                    "github-copilot",
                    StartRequest {
                        flow: Some(FlowKind::Pkce),
                        redirect_uri: Some("http://127.0.0.1:51121/oauth-callback".into()),
                        ..Default::default()
                    },
                )
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn the_refresh_exchanges_again_and_a_401_needs_a_new_sign_in() {
        let http = crate::auth_flows::http_client().unwrap();
        let server = MockServer::start().await;
        let expires = chrono::Utc::now().timestamp() + 1700;
        mount_exchange(&server, 200, exchange_reply(expires)).await;
        let url = format!("{}/copilot_internal/v2/token", server.uri());
        let grant = refresh_exchange(&http, &url, "gho_synthetic_github", "")
            .await
            .unwrap();
        assert_eq!(grant.access_token, "tid=synthetic;exp=1;sku=free");
        // The GitHub token does not rotate.
        assert!(grant.refresh_token.is_none());
        assert_eq!(grant.expires_at, Some(expires));
        // A token GitHub refuses is terminal; a lost subscription or a 5xx is not.
        assert!(
            refresh_exchange(&http, &url, "gho_other", "")
                .await
                .is_err()
        );
        for (status, terminal) in [(401, true), (403, false), (404, false), (503, false)] {
            let server = MockServer::start().await;
            Mock::given(method("GET"))
                .and(path("/copilot_internal/v2/token"))
                .respond_with(ResponseTemplate::new(status))
                .mount(&server)
                .await;
            let url = format!("{}/copilot_internal/v2/token", server.uri());
            let result = refresh_exchange(&http, &url, "gho_synthetic_github", "").await;
            assert_eq!(
                matches!(result, Err(RefreshError::InvalidGrant)),
                terminal,
                "{status}"
            );
            assert!(result.is_err());
        }
    }

    #[test]
    fn only_a_github_copilot_host_is_taken_from_the_token_reply() {
        for good in [
            "https://api.githubcopilot.com",
            "https://api.business.githubcopilot.com/",
            "https://API.Enterprise.GitHubCopilot.com",
        ] {
            assert!(api_base_ok(good).is_some(), "{good}");
        }
        assert_eq!(
            api_base_ok("https://API.Enterprise.GitHubCopilot.com/x").as_deref(),
            Some("https://api.enterprise.githubcopilot.com")
        );
        for bad in [
            "http://api.githubcopilot.com",
            "https://evil.example",
            "https://githubcopilot.com.evil.example",
            "https://user@api.githubcopilot.com",
            "https://api.githubcopilot.com:8443",
            "https://127.0.0.1",
            "not a url",
        ] {
            assert!(api_base_ok(bad).is_none(), "{bad}");
        }
    }

    #[test]
    fn the_standard_registry_offers_the_copilot_device_flow_with_pinned_hosts() {
        assert_eq!(
            SignInRegistry::standard().flows_for("github-copilot"),
            ["device"]
        );
        let pins = CopilotSignIn::new();
        assert_eq!(
            pins.device.device.checked_url().unwrap().as_str(),
            "https://github.com/login/device/code"
        );
        assert_eq!(
            pins.device.token.checked_url().unwrap().as_str(),
            "https://github.com/login/oauth/access_token"
        );
        assert_eq!(pins.token.checked_url().unwrap().as_str(), TOKEN_URL);
        assert_eq!(
            pins.user.checked_url().unwrap().as_str(),
            "https://api.github.com/user"
        );
        assert_eq!(pins.device.scope, "read:user");
        assert_eq!(pins.device.verification_hosts, ["github.com"]);
    }
}
