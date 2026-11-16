//! Meta Muse Code member sign-in: the OAuth device flow, then a Muse API key.
//!
//! This follows the owner's HielCowork client:
//!
//! - `POST https://auth.meta.com/oidc/device/authorization/` with the public
//!   client id and no scope; the verification page is pinned to `auth.meta.com`.
//! - The token endpoint is polled generically (RFC 8628). Meta publishes no
//!   refresh token: the identity token it returns is kept only to mint again
//!   and to read quota.
//! - The key is minted with `POST https://api.meta.ai/muse-code/key`. Only this
//!   interactive mint sends `onboard: true`. A 429 or 5xx is retried after
//!   5 s and then 10 s, because the device code is one-shot. A failed mint
//!   stores nothing.
//! - `x-api-version: 1.0.0` goes on every Muse call.
//!
//! The stored credential carries the minted key as `api_key` and the Meta
//! identity token as `refresh_token`. The tier and email are non-secret and live
//! in the account's identity. A login with no stable identity is refused.

use std::time::Duration;

use async_trait::async_trait;
use serde_json::json;

use crate::accounts::{AuthKind, identity_fingerprint};
use crate::error::{Result, SeraError};
use crate::providers::{MUSE_API_VERSION, MUSE_CLIENT_ID, MUSE_MINT_RETRY_WAITS, muse_mint};
use crate::user_secrets::ProviderCredential;

use super::device::{DevicePoll, DeviceSpec, DeviceStart};
use super::{FlowKind, PinnedEndpoint, SignInProvider, SignedIn, TokenSet};

/// The host Meta's verification page must be on.
const VERIFICATION_HOST: &str = "auth.meta.com";

/// The Muse Code sign-in, registered under the `muse-code` kind.
pub struct MuseSignIn {
    device: DeviceSpec,
    mint: PinnedEndpoint,
    retry_waits: Vec<Duration>,
}

impl MuseSignIn {
    /// The sign-in against Meta's hosts.
    pub fn new() -> Self {
        Self {
            device: DeviceSpec {
                device: PinnedEndpoint::https("auth.meta.com", "/oidc/device/authorization/"),
                token: PinnedEndpoint::https("auth.meta.com", "/oidc/device/token/"),
                client_id: MUSE_CLIENT_ID.to_owned(),
                client_secret: None,
                scope: String::new(),
                verification_hosts: vec![VERIFICATION_HOST.to_owned()],
                headers: vec![("x-api-version", MUSE_API_VERSION)],
            },
            mint: PinnedEndpoint::https("api.meta.ai", "/muse-code/key"),
            retry_waits: MUSE_MINT_RETRY_WAITS.to_vec(),
        }
    }

    /// Point every endpoint at a mock server on the exact loopback, and make the
    /// mint retries instant.
    #[cfg(test)]
    pub(crate) fn for_test(base: &str, verification_host: &str) -> Self {
        Self {
            device: DeviceSpec {
                device: PinnedEndpoint::loopback(&format!("{base}/device")),
                token: PinnedEndpoint::loopback(&format!("{base}/token")),
                client_id: MUSE_CLIENT_ID.to_owned(),
                client_secret: None,
                scope: String::new(),
                verification_hosts: vec![verification_host.to_owned()],
                headers: vec![("x-api-version", MUSE_API_VERSION)],
            },
            mint: PinnedEndpoint::loopback(&format!("{base}/key")),
            retry_waits: vec![Duration::from_millis(1), Duration::from_millis(1)],
        }
    }
}

impl Default for MuseSignIn {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl SignInProvider for MuseSignIn {
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
        // Meta's access token is the identity token. It is kept to mint again
        // and to read quota; any refresh token or expiry it carries is dropped.
        let identity_token = tokens.access_token;
        let url = self.mint.checked_url()?;
        let minted = muse_mint(http, url.as_str(), &identity_token, true, &self.retry_waits)
            .await
            .map_err(|error| match error {
                // The rejected-subscription words reach the member as they are;
                // anything else is a plain failure with no provider text.
                SeraError::Rejected { .. } => error,
                _ => SeraError::External("Muse Code key mint failed".into()),
            })?;
        let stable_id = minted
            .user_id
            .clone()
            .or_else(|| minted.email.clone())
            .ok_or_else(|| SeraError::Rejected {
                code: "no_stable_identity",
                message: "Meta did not say who signed in, so the account was not saved".into(),
            })?;
        let mut identity = json!({});
        if let Some(email) = &minted.email {
            identity["email"] = json!(email);
        }
        if let Some(tier) = &minted.tier {
            identity["tier"] = json!(tier);
        }
        Ok(SignedIn {
            label: minted.email.clone().unwrap_or_else(|| "Meta Muse".into()),
            auth_kind: AuthKind::Device,
            fingerprint: Some(identity_fingerprint("muse-code", &stable_id)),
            identity,
            credential: ProviderCredential {
                api_key: minted.api_key,
                refresh_token: identity_token,
                expires_at: None,
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
    use wiremock::matchers::{body_json, body_string_contains, header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};
    use zeroize::Zeroizing;

    use super::*;
    use crate::accounts::Accounts;
    use crate::auth_flows::{
        DevicePlan, FlowKind, FlowRecord, FlowRef, FlowStatus, SignInRegistry, SignInService,
        StartRequest,
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
            UserSecrets::with_master_key(store.clone(), [5u8; crate::user_secrets::KEY_BYTES])
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
            "muse-code",
            Arc::new(MuseSignIn::for_test(&server.uri(), "verify.example")),
        );
        Arc::new(
            SignInService::new(harness.accounts.clone())
                .unwrap()
                .with_registry(registry),
        )
    }

    fn device_flow(provider: &str) -> FlowRecord {
        let now = std::time::Instant::now();
        FlowRecord {
            provider: provider.into(),
            kind: FlowKind::Device,
            redirect_uri: None,
            verifier: None,
            client_holds_verifier: false,
            expires: now + Duration::from_secs(900),
            created: now,
            status: FlowStatus::Pending,
            consumed: false,
        }
    }

    async fn poll_answers(server: &MockServer, answers: Vec<(u16, Value)>) {
        for (status, body) in answers {
            Mock::given(method("POST"))
                .and(path("/token"))
                .and(body_string_contains("device_code=dc-synthetic"))
                .respond_with(ResponseTemplate::new(status).set_body_json(body))
                .up_to_n_times(1)
                .mount(server)
                .await;
        }
    }

    fn approved() -> (u16, Value) {
        (
            200,
            json!({"access_token": "meta-identity-token", "refresh_token": "meta-refresh-dropped", "expires_in": 60}),
        )
    }

    fn minted_reply() -> Value {
        json!({
            "api_key": "LLM|minted-synthetic-key",
            "user_email": "  Muse.Person@Example.Example ",
            "subs_tier_name": "Plus",
            "user_id": "meta-user-7",
            "is_subs_active": true,
            "require_payment": false
        })
    }

    /// Run the poller to its end with instant waits.
    async fn run(
        harness: &Harness,
        server: &MockServer,
        user: &str,
    ) -> (FlowStatus, Arc<SignInService>) {
        let service = service(harness, server);
        let flow_id = service.flows.insert(user, device_flow("muse-code"));
        let provider = service.registry.get("muse-code").cloned().unwrap();
        service
            .run_device_flow(
                &FlowRef {
                    user_id: user,
                    provider_id: "muse-code",
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
        let status = service.status(user, "muse-code", &flow_id).unwrap();
        (status, service)
    }

    fn mint_requests(requests: &[wiremock::Request]) -> Vec<&wiremock::Request> {
        requests
            .iter()
            .filter(|request| request.url.path() == "/key")
            .collect()
    }

    #[tokio::test]
    async fn the_device_flow_mints_a_key_and_stores_both_secrets_sealed() {
        let harness = harness();
        let alice = member(&harness, "microsoft:muse-ok");
        let server = MockServer::start().await;
        poll_answers(
            &server,
            vec![
                (400, json!({"error": "authorization_pending"})),
                (400, json!({})),
                (400, json!({"error": "slow_down"})),
                approved(),
            ],
        )
        .await;
        Mock::given(method("POST"))
            .and(path("/key"))
            .and(header("authorization", "Bearer meta-identity-token"))
            .and(header("x-api-version", "1.0.0"))
            // Only the interactive sign-in mint onboards.
            .and(body_json(json!({"onboard": true})))
            .respond_with(ResponseTemplate::new(200).set_body_json(minted_reply()))
            .expect(1)
            .mount(&server)
            .await;
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
            .get(&alice, "muse-code", &account_id)
            .unwrap()
            .unwrap();
        assert_eq!(account.auth_kind, AuthKind::Device);
        assert_eq!(account.label, "muse.person@example.example");
        assert_eq!(account.identity["email"], "muse.person@example.example");
        assert_eq!(account.identity["tier"], "Plus");
        // The identity is not a secret and holds none.
        assert!(
            !account
                .identity
                .to_string()
                .contains("minted-synthetic-key")
        );
        // The minted key is the credential's key; Meta's token is kept to mint
        // again; the refresh token and expiry Meta sent are dropped.
        let credential = harness
            .accounts
            .credential(&alice, "muse-code", &account_id)
            .unwrap()
            .unwrap();
        assert_eq!(credential.api_key, "LLM|minted-synthetic-key");
        assert_eq!(credential.refresh_token, "meta-identity-token");
        assert_eq!(credential.expires_at, None);
        // Every call to Meta carried the API version.
        for request in server.received_requests().await.unwrap() {
            assert_eq!(
                request
                    .headers
                    .get("x-api-version")
                    .map(|v| v.to_str().unwrap()),
                Some("1.0.0"),
                "{}",
                request.url.path()
            );
        }
        // Another member has no such account.
        let bob = member(&harness, "microsoft:muse-ok-bob");
        assert!(harness.accounts.list(&bob, "muse-code").unwrap().is_empty());
    }

    #[tokio::test]
    async fn the_mint_retries_a_429_and_a_5xx_then_succeeds() {
        let harness = harness();
        let alice = member(&harness, "microsoft:muse-retry");
        let server = MockServer::start().await;
        poll_answers(&server, vec![approved()]).await;
        Mock::given(method("POST"))
            .and(path("/key"))
            .respond_with(ResponseTemplate::new(429))
            .up_to_n_times(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/key"))
            .respond_with(ResponseTemplate::new(503))
            .up_to_n_times(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/key"))
            .respond_with(ResponseTemplate::new(200).set_body_json(minted_reply()))
            .mount(&server)
            .await;
        let (status, _service) = run(&harness, &server, &alice).await;
        assert!(matches!(status, FlowStatus::Ok { .. }), "{status:?}");
        let received = server.received_requests().await.unwrap();
        assert_eq!(mint_requests(&received).len(), 3);
    }

    #[tokio::test]
    async fn a_failed_mint_stores_nothing() {
        for (status, body, reason) in [
            (503, json!({}), "provider_error"),
            (401, json!({}), "provider_error"),
            (
                200,
                json!({"is_subs_active": false, "api_key": "LLM|never-kept"}),
                "muse_inactive",
            ),
            (
                200,
                json!({"is_subs_active": true, "require_payment": true}),
                "muse_subscription_required",
            ),
            (
                200,
                json!({"action_url": "https://muse.example/pay"}),
                "muse_subscription_required",
            ),
            (200, json!({"is_subs_active": true}), "provider_error"),
            // A key, but nobody it belongs to: not saved.
            (
                200,
                json!({"api_key": "LLM|anonymous"}),
                "no_stable_identity",
            ),
        ] {
            let harness = harness();
            let alice = member(&harness, &format!("microsoft:muse-fail-{reason}-{status}"));
            let server = MockServer::start().await;
            poll_answers(&server, vec![approved()]).await;
            Mock::given(method("POST"))
                .and(path("/key"))
                .respond_with(ResponseTemplate::new(status).set_body_json(body.clone()))
                .mount(&server)
                .await;
            let (flow, _service) = run(&harness, &server, &alice).await;
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
                    .list(&alice, "muse-code")
                    .unwrap()
                    .is_empty(),
                "{status} {body}"
            );
            // A persistent 5xx is tried three times in all (then 5 s and 10 s,
            // instant here); a refusal once.
            let received = server.received_requests().await.unwrap();
            let expected = if status >= 500 { 3 } else { 1 };
            assert_eq!(mint_requests(&received).len(), expected, "{status} {body}");
        }
    }

    #[tokio::test]
    async fn starting_the_flow_returns_the_code_and_pins_the_verification_page() {
        let harness = harness();
        let alice = member(&harness, "microsoft:muse-start");
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/device"))
            .and(header("x-api-version", "1.0.0"))
            .and(body_string_contains("client_id=1031625952748946"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "device_code": "dc-synthetic",
                "user_code": "ABCD-1234",
                "verification_uri": "https://verify.example/device",
                "verification_uri_complete": "https://verify.example/device?user_code=ABCD-1234",
                "interval": 5,
                "expires_in": 300
            })))
            .mount(&server)
            .await;
        // The poller that starts in the background never gets approval here.
        poll_answers(&server, vec![(400, json!({"error": "access_denied"}))]).await;
        let service = service(&harness, &server);
        let started = service
            .start(
                &alice,
                "muse-code",
                "muse-code",
                StartRequest {
                    flow: Some(FlowKind::Device),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        assert_eq!(started.user_code.as_deref(), Some("ABCD-1234"));
        assert!(started.url.starts_with("https://verify.example/device"));
        assert_eq!(started.interval, Some(5));
        assert_eq!(started.expires_in, 300);
        // A page on another host is refused before any code is shown.
        let hostile = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/device"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "device_code": "dc-synthetic",
                "user_code": "ABCD-1234",
                "verification_uri": "https://evil.example/device"
            })))
            .mount(&hostile)
            .await;
        assert!(
            self::service(&harness, &hostile)
                .start(
                    &alice,
                    "muse-code",
                    "muse-code",
                    StartRequest {
                        flow: Some(FlowKind::Device),
                        ..Default::default()
                    },
                )
                .await
                .is_err()
        );
        // Muse has no browser flow.
        assert!(
            service
                .start(
                    &alice,
                    "muse-code",
                    "muse-code",
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

    #[test]
    fn the_standard_registry_offers_the_muse_device_flow() {
        assert_eq!(
            SignInRegistry::standard().flows_for("muse-code"),
            ["device"]
        );
        let pins = MuseSignIn::new();
        assert_eq!(
            pins.mint.checked_url().unwrap().as_str(),
            "https://api.meta.ai/muse-code/key"
        );
        assert_eq!(
            pins.device.device.checked_url().unwrap().as_str(),
            "https://auth.meta.com/oidc/device/authorization/"
        );
        assert_eq!(pins.device.verification_hosts, ["auth.meta.com"]);
        assert_eq!(pins.retry_waits, MUSE_MINT_RETRY_WAITS);
    }
}
