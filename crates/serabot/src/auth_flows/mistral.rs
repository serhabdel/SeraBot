//! Mistral Vibe browser sign-in: PKCE-protected session creation on
//! `console.mistral.ai`, polling until completed, then exchanging for an API key.
//!
//! Wire protocol:
//! - `POST https://console.mistral.ai/api/vibe/sign-in` with
//!   `{"code_challenge": ..., "code_challenge_method": "S256"}` ->
//!   `{"process_id", "sign_in_url", "poll_url", "expires_at"}`.
//! - `GET <poll_url>` -> `410` expired, `5xx` transient error, or 2xx
//!   `{"status": "pending" | "expired" | "denied" | "error" | "completed", ...}`.
//! - On `"completed"`, `POST https://console.mistral.ai/api/vibe/sign-in/{process_id}/exchange`
//!   with `{"exchange_token", "code_verifier"}` -> `{"api_key"}`.
//!
//! `sign_in_url` and `poll_url` are pinned to `https://console.mistral.ai`
//! (and `/api/` for `poll_url`). The `code_verifier` is packed inside
//! `DeviceStart.device_code` and never logged.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::accounts::AuthKind;
use crate::error::{Result, SeraError};
use crate::user_secrets::ProviderCredential;

use super::device::{self, DevicePoll, DeviceStart};
use super::pkce::pkce_s256;
use super::{
    FlowKind, PinnedEndpoint, SignInProvider, SignedIn, TokenSet, credential_ok, read_capped,
};

const MISTRAL_HOST: &str = "console.mistral.ai";
const SIGN_IN_PATH: &str = "/api/vibe/sign-in";
const API_BASE: &str = "https://console.mistral.ai/api";
const DEFAULT_EXPIRES_IN_SECS: u64 = 600;
const MIN_EXPIRES_IN_SECS: u64 = 30;
const POLL_INTERVAL_SECS: u64 = 3;

const INELIGIBLE_WORKSPACE_MSG: &str = "Your Mistral workspace is not eligible for Vibe API key creation; paste an API key from console.mistral.ai instead.";

#[derive(Serialize, Deserialize)]
struct PackedState {
    process_id: String,
    poll_url: String,
    code_verifier: String,
}

/// Mistral Vibe browser sign-in, registered under the `mistral` kind.
pub struct MistralSignIn {
    start: PinnedEndpoint,
    api_base: String,
    #[cfg(test)]
    test_base: Option<String>,
}

impl MistralSignIn {
    /// The sign-in against Mistral's console host.
    pub fn new() -> Self {
        Self {
            start: PinnedEndpoint::https(MISTRAL_HOST, SIGN_IN_PATH),
            api_base: API_BASE.to_owned(),
            #[cfg(test)]
            test_base: None,
        }
    }

    /// Point all endpoints at a mock server on the exact loopback, for tests.
    #[cfg(test)]
    pub(crate) fn for_test(base: &str) -> Self {
        let base = base.trim_end_matches('/');
        Self {
            start: PinnedEndpoint::loopback(&format!("{base}{SIGN_IN_PATH}")),
            api_base: format!("{base}/api"),
            test_base: Some(base.to_owned()),
        }
    }

    fn validate_console_url(&self, raw: &str, require_api_prefix: bool) -> Result<String> {
        let refuse = || SeraError::External("the sign-in page was refused".into());
        let url = url::Url::parse(raw).map_err(|_| refuse())?;
        if !url.username().is_empty() || url.password().is_some() || url.fragment().is_some() {
            return Err(refuse());
        }
        #[cfg(test)]
        if let Some(base) = &self.test_base {
            let base_url = url::Url::parse(base).map_err(|_| refuse())?;
            if url.scheme() != base_url.scheme()
                || url.host_str() != base_url.host_str()
                || url.port() != base_url.port()
            {
                return Err(refuse());
            }
            if require_api_prefix && !url.path().starts_with("/api/") {
                return Err(refuse());
            }
            return Ok(url.to_string());
        }
        let host = url.host_str().unwrap_or("");
        if url.scheme() != "https"
            || !host.eq_ignore_ascii_case(MISTRAL_HOST)
            || url.port().is_some()
        {
            return Err(refuse());
        }
        if require_api_prefix && !url.path().starts_with("/api/") {
            return Err(refuse());
        }
        Ok(url.to_string())
    }
}

impl Default for MistralSignIn {
    fn default() -> Self {
        Self::new()
    }
}

fn process_id_ok(value: &str) -> bool {
    !value.is_empty()
        && value != "."
        && value != ".."
        && value.len() <= 256
        && !value.contains(['/', '?', '#', '\\'])
        && !value.chars().any(|c| c.is_whitespace() || c.is_control())
}

fn expires_in_from(expires_at: Option<&str>) -> u64 {
    let Some(raw) = expires_at else {
        return DEFAULT_EXPIRES_IN_SECS;
    };
    let Ok(dt) = chrono::DateTime::parse_from_rfc3339(raw) else {
        return DEFAULT_EXPIRES_IN_SECS;
    };
    let secs = (dt.with_timezone(&chrono::Utc) - chrono::Utc::now()).num_seconds();
    if secs <= 0 {
        return MIN_EXPIRES_IN_SECS;
    }
    (secs as u64).clamp(MIN_EXPIRES_IN_SECS, device::MAX_WAIT_SECS)
}

#[async_trait]
impl SignInProvider for MistralSignIn {
    fn flows(&self) -> &'static [FlowKind] {
        &[FlowKind::Device]
    }

    async fn device_start(&self, http: &reqwest::Client) -> Result<DeviceStart> {
        let (code_verifier, code_challenge) = pkce_s256();
        let response = http
            .post(self.start.checked_url()?)
            .header("accept", "application/json")
            .json(&json!({
                "code_challenge": code_challenge,
                "code_challenge_method": "S256",
            }))
            .send()
            .await
            .map_err(|_| SeraError::External("the sign-in provider could not be reached".into()))?;
        let status = response.status();
        let body = read_capped(response).await?;
        if !status.is_success() {
            return Err(SeraError::Rejected {
                code: "signin_unavailable",
                message: "The provider could not start a sign-in. Try again later.".into(),
            });
        }
        let value: Value = serde_json::from_slice(&body).map_err(|_| {
            SeraError::External("the sign-in provider sent an invalid reply".into())
        })?;
        let incomplete =
            || SeraError::External("the sign-in provider sent an incomplete reply".into());

        let process_id = value
            .get("process_id")
            .and_then(Value::as_str)
            .filter(|id| process_id_ok(id))
            .ok_or_else(incomplete)?
            .to_owned();
        let raw_sign_in_url = value
            .get("sign_in_url")
            .and_then(Value::as_str)
            .ok_or_else(incomplete)?;
        let raw_poll_url = value
            .get("poll_url")
            .and_then(Value::as_str)
            .ok_or_else(incomplete)?;

        let sign_in_url = self.validate_console_url(raw_sign_in_url, false)?;
        let poll_url = self.validate_console_url(raw_poll_url, true)?;
        let expires_in = expires_in_from(value.get("expires_at").and_then(Value::as_str));

        let packed = serde_json::to_string(&PackedState {
            process_id,
            poll_url,
            code_verifier,
        })
        .map_err(|_| SeraError::Internal("could not encode sign-in state".into()))?;

        Ok(DeviceStart {
            device_code: packed,
            user_code: String::new(),
            verification_uri: sign_in_url,
            interval: POLL_INTERVAL_SECS,
            expires_in,
        })
    }

    async fn device_poll(&self, http: &reqwest::Client, device_code: &str) -> Result<DevicePoll> {
        let Ok(state) = serde_json::from_str::<PackedState>(device_code) else {
            return Ok(DevicePoll::Failed("Mistral sign-in state is invalid"));
        };
        if !process_id_ok(&state.process_id)
            || self.validate_console_url(&state.poll_url, true).is_err()
        {
            return Ok(DevicePoll::Failed("Mistral sign-in state is invalid"));
        }

        let response = http
            .get(&state.poll_url)
            .header("accept", "application/json")
            .send()
            .await
            .map_err(|_| SeraError::External("the sign-in provider could not be reached".into()))?;
        let status = response.status();
        if status == reqwest::StatusCode::GONE {
            return Ok(DevicePoll::Expired);
        }
        if status.is_server_error() {
            return Err(SeraError::External(
                "the sign-in provider is unavailable".into(),
            ));
        }
        let body = read_capped(response).await?;
        if !status.is_success() {
            return Ok(DevicePoll::Failed("Mistral sign-in failed"));
        }

        let Ok(value) = serde_json::from_slice::<Value>(&body) else {
            return Ok(DevicePoll::Failed("Mistral sign-in sent an invalid reply"));
        };
        match value.get("status").and_then(Value::as_str) {
            Some("pending") => Ok(DevicePoll::Pending),
            Some("expired") => Ok(DevicePoll::Expired),
            Some("denied") => Ok(DevicePoll::Denied),
            Some("error") => Ok(DevicePoll::Failed("Mistral sign-in reported an error")),
            Some("completed") => {
                let Some(exchange_token) = value
                    .get("exchange_token")
                    .and_then(Value::as_str)
                    .filter(|token| !token.is_empty() && credential_ok(token))
                else {
                    return Ok(DevicePoll::Failed(
                        "Mistral sign-in did not return an exchange token",
                    ));
                };

                let exchange_url = format!(
                    "{}/vibe/sign-in/{}/exchange",
                    self.api_base, state.process_id
                );
                let exchange_resp = http
                    .post(&exchange_url)
                    .header("accept", "application/json")
                    .json(&json!({
                        "exchange_token": exchange_token,
                        "code_verifier": state.code_verifier,
                    }))
                    .send()
                    .await
                    .map_err(|_| {
                        SeraError::External("the sign-in provider could not be reached".into())
                    })?;
                let ex_status = exchange_resp.status();
                let ex_body = read_capped(exchange_resp).await?;
                if ex_status == reqwest::StatusCode::FORBIDDEN {
                    if let Ok(err_val) = serde_json::from_slice::<Value>(&ex_body) {
                        let code = err_val.get("error_code").and_then(|v| {
                            v.as_u64()
                                .or_else(|| v.as_str().and_then(|s| s.parse().ok()))
                        });
                        if code == Some(9004) {
                            return Ok(DevicePoll::Failed(INELIGIBLE_WORKSPACE_MSG));
                        }
                    }
                    return Ok(DevicePoll::Failed(
                        "Mistral sign-in could not exchange the token",
                    ));
                }
                if !ex_status.is_success() {
                    return Ok(DevicePoll::Failed(
                        "Mistral sign-in could not exchange the token",
                    ));
                }
                let Ok(ex_val) = serde_json::from_slice::<Value>(&ex_body) else {
                    return Ok(DevicePoll::Failed(
                        "Mistral sign-in could not exchange the token",
                    ));
                };
                let Some(api_key) = ex_val
                    .get("api_key")
                    .and_then(Value::as_str)
                    .filter(|k| credential_ok(k))
                    .map(str::to_owned)
                else {
                    return Ok(DevicePoll::Failed(
                        "Mistral sign-in did not return a valid API key",
                    ));
                };
                Ok(DevicePoll::Done(TokenSet {
                    access_token: api_key,
                    refresh_token: None,
                    expires_in: None,
                    id_token: None,
                    refresh_expires_in: None,
                }))
            }
            _ => Ok(DevicePoll::Failed(
                "Mistral sign-in returned an unknown status",
            )),
        }
    }

    async fn finish_device(&self, _http: &reqwest::Client, tokens: TokenSet) -> Result<SignedIn> {
        Ok(SignedIn {
            label: "Mistral AI".into(),
            auth_kind: AuthKind::Minted,
            fingerprint: None,
            identity: json!({}),
            credential: ProviderCredential {
                api_key: tokens.access_token,
                ..Default::default()
            },
        })
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use serde_json::{Value, json};
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use super::*;
    use crate::accounts::Accounts;
    use crate::auth_flows::{
        DevicePlan, FlowKind, FlowRecord, FlowRef, FlowStatus, SignInRegistry, SignInService,
        StartRequest,
    };
    use crate::store::Store;
    use crate::user_secrets::UserSecrets;

    fn test_service(mistral: MistralSignIn) -> (Arc<SignInService>, String) {
        let store = Store::open_in_memory(384).unwrap();
        let user_id = store
            .create_user("owner-1", "owner@example.example", "Owner")
            .unwrap()
            .id;
        let secrets = Arc::new(
            UserSecrets::with_master_key(store.clone(), [7u8; crate::user_secrets::KEY_BYTES])
                .unwrap(),
        );
        let accounts = Arc::new(Accounts::new(store, secrets));
        let mut registry = SignInRegistry::standard();
        registry.register("mistral", Arc::new(mistral));
        let service = Arc::new(
            SignInService::new(accounts)
                .unwrap()
                .with_registry(registry),
        );
        (service, user_id)
    }

    #[tokio::test]
    async fn happy_path_start_pending_completed_exchange() {
        let server = MockServer::start().await;
        let base = server.uri();
        let expires_at = (chrono::Utc::now() + chrono::Duration::seconds(300)).to_rfc3339();

        Mock::given(method("POST"))
            .and(path("/api/vibe/sign-in"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "process_id": "proc-123",
                "sign_in_url": format!("{base}/vibe/sign-in?p=proc-123"),
                "poll_url": format!("{base}/api/vibe/sign-in/proc-123/poll"),
                "expires_at": expires_at,
            })))
            .mount(&server)
            .await;

        // First poll: pending
        Mock::given(method("GET"))
            .and(path("/api/vibe/sign-in/proc-123/poll"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "status": "pending",
            })))
            .up_to_n_times(1)
            .mount(&server)
            .await;

        // Second poll: completed
        Mock::given(method("GET"))
            .and(path("/api/vibe/sign-in/proc-123/poll"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "status": "completed",
                "exchange_token": "exch-tok-999",
            })))
            .mount(&server)
            .await;

        Mock::given(method("POST"))
            .and(path("/api/vibe/sign-in/proc-123/exchange"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "api_key": "mistral-api-key-secret",
            })))
            .mount(&server)
            .await;

        let provider = Arc::new(MistralSignIn::for_test(&base));
        let http = crate::auth_flows::http_client().unwrap();

        let started = provider.device_start(&http).await.unwrap();
        assert!(started.user_code.is_empty());
        assert_eq!(started.interval, 3);
        assert!(started.expires_in >= 30 && started.expires_in <= 300);
        assert_eq!(
            started.verification_uri,
            format!("{base}/vibe/sign-in?p=proc-123")
        );

        let first = provider
            .device_poll(&http, &started.device_code)
            .await
            .unwrap();
        assert!(matches!(first, DevicePoll::Pending));

        let second = provider
            .device_poll(&http, &started.device_code)
            .await
            .unwrap();
        let tokens = match second {
            DevicePoll::Done(tokens) => tokens,
            other => panic!("expected Done, got {other:?}"),
        };
        assert_eq!(tokens.access_token, "mistral-api-key-secret");

        let signed_in = provider.finish_device(&http, tokens).await.unwrap();
        assert_eq!(signed_in.label, "Mistral AI");
        assert_eq!(signed_in.auth_kind, AuthKind::Minted);
        assert!(signed_in.fingerprint.is_none());
        assert_eq!(signed_in.identity, json!({}));
        assert_eq!(signed_in.credential.api_key, "mistral-api-key-secret");
    }

    #[tokio::test]
    async fn start_via_service_omits_empty_user_code() {
        let server = MockServer::start().await;
        let base = server.uri();
        Mock::given(method("POST"))
            .and(path("/api/vibe/sign-in"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "process_id": "proc-svc",
                "sign_in_url": format!("{base}/vibe/sign-in?p=proc-svc"),
                "poll_url": format!("{base}/api/vibe/sign-in/proc-svc/poll"),
            })))
            .mount(&server)
            .await;

        let (service, user_id) = test_service(MistralSignIn::for_test(&base));
        let started = service
            .start(&user_id, "mistral", "mistral", StartRequest::default())
            .await
            .unwrap();
        assert_eq!(started.flow, FlowKind::Device);
        assert!(started.user_code.is_none());
        assert_eq!(started.interval, Some(3));
        let serialized: Value = serde_json::to_value(&started).unwrap();
        assert!(serialized.get("user_code").is_none());
    }

    #[tokio::test]
    async fn poll_410_returns_expired() {
        let server = MockServer::start().await;
        let base = server.uri();
        Mock::given(method("POST"))
            .and(path("/api/vibe/sign-in"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "process_id": "proc-exp",
                "sign_in_url": format!("{base}/vibe/sign-in?p=proc-exp"),
                "poll_url": format!("{base}/api/vibe/sign-in/proc-exp/poll"),
            })))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/vibe/sign-in/proc-exp/poll"))
            .respond_with(ResponseTemplate::new(410))
            .mount(&server)
            .await;

        let provider = MistralSignIn::for_test(&base);
        let http = crate::auth_flows::http_client().unwrap();
        let started = provider.device_start(&http).await.unwrap();
        let poll = provider
            .device_poll(&http, &started.device_code)
            .await
            .unwrap();
        assert!(matches!(poll, DevicePoll::Expired));
    }

    #[tokio::test]
    async fn poll_denied_returns_denied() {
        let server = MockServer::start().await;
        let base = server.uri();
        Mock::given(method("POST"))
            .and(path("/api/vibe/sign-in"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "process_id": "proc-den",
                "sign_in_url": format!("{base}/vibe/sign-in?p=proc-den"),
                "poll_url": format!("{base}/api/vibe/sign-in/proc-den/poll"),
            })))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/vibe/sign-in/proc-den/poll"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "status": "denied",
            })))
            .mount(&server)
            .await;

        let provider = MistralSignIn::for_test(&base);
        let http = crate::auth_flows::http_client().unwrap();
        let started = provider.device_start(&http).await.unwrap();
        let poll = provider
            .device_poll(&http, &started.device_code)
            .await
            .unwrap();
        assert!(matches!(poll, DevicePoll::Denied));
    }

    #[tokio::test]
    async fn exchange_403_error_9004_returns_ineligible_message() {
        let server = MockServer::start().await;
        let base = server.uri();
        Mock::given(method("POST"))
            .and(path("/api/vibe/sign-in"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "process_id": "proc-9004",
                "sign_in_url": format!("{base}/vibe/sign-in?p=proc-9004"),
                "poll_url": format!("{base}/api/vibe/sign-in/proc-9004/poll"),
            })))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/vibe/sign-in/proc-9004/poll"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "status": "completed",
                "exchange_token": "exch-9004",
            })))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/api/vibe/sign-in/proc-9004/exchange"))
            .respond_with(ResponseTemplate::new(403).set_body_json(json!({
                "error_code": 9004,
                "message": "workspace ineligible",
            })))
            .mount(&server)
            .await;

        let provider = Arc::new(MistralSignIn::for_test(&base));
        let http = crate::auth_flows::http_client().unwrap();
        let started = provider.device_start(&http).await.unwrap();
        let poll = provider
            .device_poll(&http, &started.device_code)
            .await
            .unwrap();
        assert!(matches!(poll, DevicePoll::Failed(INELIGIBLE_WORKSPACE_MSG)));

        // Also verify it propagates through SignInService::run_device_flow.
        let (service, user_id) = test_service(MistralSignIn::for_test(&base));
        let flow_id = service.flows.insert(
            &user_id,
            FlowRecord {
                provider: "mistral".into(),
                kind: FlowKind::Device,
                redirect_uri: None,
                state: None,
                verifier: None,
                client_holds_verifier: false,
                expires: std::time::Instant::now() + std::time::Duration::from_secs(60),
                created: std::time::Instant::now(),
                status: FlowStatus::Pending,
                consumed: false,
            },
        );
        service
            .run_device_flow(
                &FlowRef {
                    user_id: &user_id,
                    provider_id: "mistral",
                    flow_id: &flow_id,
                },
                provider,
                DevicePlan {
                    device_code: zeroize::Zeroizing::new(started.device_code),
                    interval: 3,
                    expires_in: 60,
                },
                |_| async {},
            )
            .await;
        let status = service.status(&user_id, "mistral", &flow_id).unwrap();
        assert_eq!(
            status,
            FlowStatus::Denied {
                reason: INELIGIBLE_WORKSPACE_MSG.into(),
            }
        );
    }

    #[tokio::test]
    async fn unpinned_urls_are_rejected() {
        let server = MockServer::start().await;
        let base = server.uri();
        let http = crate::auth_flows::http_client().unwrap();

        // Unpinned sign_in_url
        Mock::given(method("POST"))
            .and(path("/api/vibe/sign-in"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "process_id": "proc-bad-1",
                "sign_in_url": "https://evil.example/vibe",
                "poll_url": format!("{base}/api/vibe/sign-in/proc-bad-1/poll"),
            })))
            .up_to_n_times(1)
            .mount(&server)
            .await;

        let provider = MistralSignIn::for_test(&base);
        let err = provider.device_start(&http).await.unwrap_err();
        assert!(matches!(err, SeraError::External(_)));

        // Unpinned poll_url (wrong host)
        Mock::given(method("POST"))
            .and(path("/api/vibe/sign-in"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "process_id": "proc-bad-2",
                "sign_in_url": format!("{base}/vibe/sign-in"),
                "poll_url": "https://evil.example/api/poll",
            })))
            .up_to_n_times(1)
            .mount(&server)
            .await;

        let err = provider.device_start(&http).await.unwrap_err();
        assert!(matches!(err, SeraError::External(_)));

        // poll_url missing /api/ prefix
        Mock::given(method("POST"))
            .and(path("/api/vibe/sign-in"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "process_id": "proc-bad-3",
                "sign_in_url": format!("{base}/vibe/sign-in"),
                "poll_url": format!("{base}/not-api/poll"),
            })))
            .mount(&server)
            .await;

        let err = provider.device_start(&http).await.unwrap_err();
        assert!(matches!(err, SeraError::External(_)));

        // Production validator rejects non-https and non-console.mistral.ai
        let prod = MistralSignIn::new();
        assert!(
            prod.validate_console_url("https://console.mistral.ai/vibe/sign-in", false)
                .is_ok()
        );
        assert!(
            prod.validate_console_url("https://console.mistral.ai/api/vibe/sign-in/p/poll", true)
                .is_ok()
        );
        assert!(
            prod.validate_console_url("http://console.mistral.ai/vibe/sign-in", false)
                .is_err()
        );
        assert!(
            prod.validate_console_url("https://evil.example/vibe/sign-in", false)
                .is_err()
        );
        assert!(
            prod.validate_console_url("https://console.mistral.ai/vibe/poll", true)
                .is_err()
        );
    }
}
