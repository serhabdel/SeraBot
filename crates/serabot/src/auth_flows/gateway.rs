//! Browser sign-in for a `claude-agent-sdk` provider ("Sign in with
//! browser" on its card).
//!
//! The loopback gateway (`services/claude_gateway/`) runs the *first-party*
//! claude.ai OAuth PKCE flow — the same client id `claude auth login` uses.
//! That is deliberately not the third-party claude.ai OAuth Anthropic shut
//! down in 2026 (retired here in 0.74.25): SeraBot never builds claude.ai
//! URLs, never sees a code or verifier, and only relays two calls to the
//! gateway over loopback with the provider's gateway bearer token:
//!
//! - `POST {gateway}/v1/auth/start` → the authorize URL for the member's
//!   desktop loopback redirect (the gateway keeps the PKCE verifier);
//! - `POST {gateway}/v1/auth/complete` → swaps the code, stores the login
//!   where the SDK reads it, and reports the non-secret account identity.
//!
//! The stored account's credential is the gateway bearer token (that is what
//! authenticates turns to the gateway); the Claude login itself lives only
//! inside the gateway process.

use async_trait::async_trait;
use serde_json::{Value, json};
use zeroize::Zeroizing;

use super::pkce::RedirectSpec;
use super::{FlowKind, SignInProvider, http_client, read_capped, unsupported};
use crate::error::{Result, SeraError};

/// The `claude-agent-sdk` provider's browser sign-in. URL building and code
/// exchange happen gateway-side, so the trait's direct methods stay
/// unsupported; [`SignInService`](super::SignInService) relays via
/// [`gateway_auth_start`] / [`gateway_auth_complete`] instead.
pub struct ClaudeGatewaySignIn;

#[async_trait]
impl SignInProvider for ClaudeGatewaySignIn {
    fn flows(&self) -> &'static [FlowKind] {
        &[FlowKind::Pkce]
    }

    /// The redirect is the member's desktop loopback listener: any
    /// unprivileged port, any path.
    fn redirect_spec(&self) -> Option<RedirectSpec> {
        Some(RedirectSpec {
            port: None,
            path: None,
        })
    }

    fn authorize_url(&self, _redirect_uri: &str, _challenge: &str, _state: &str) -> Result<String> {
        Err(unsupported())
    }
}

/// A loopback gateway endpoint plus the bearer token that authenticates to
/// it (the provider's stored gateway token).
pub struct GatewayEndpoint {
    pub base_url: String,
    pub token: Zeroizing<String>,
}

/// What `POST /v1/auth/start` returns when starting a gateway-managed
/// `claude auth login` flow.
#[derive(Debug, Clone)]
pub struct GatewayAuthStarted {
    pub flow_id: String,
    /// The page to open: the CLI's automatic URL (its redirect is the CLI's
    /// own `localhost` listener) when the gateway caught it, else the manual
    /// URL whose last page shows a code to paste.
    pub url: String,
    /// The manual URL, whose last page shows a `code#state` to paste.
    pub manual_url: Option<String>,
    /// The CLI's loopback callback, when `url` is the automatic URL: the
    /// desktop listens on the same port so a browser on another machine than
    /// the gateway can still hand the redirect over.
    pub callback: Option<GatewayCallback>,
    /// The `state` the automatic URL carries.
    pub state: Option<String>,
    pub profile: String,
    pub expires_in: u64,
}

/// Where `claude auth login` waits for its browser redirect.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GatewayCallback {
    pub port: u16,
    pub path: String,
}

fn gateway_unreachable_error() -> SeraError {
    SeraError::Rejected {
        code: "gateway_unreachable",
        message: "The Claude Agent SDK gateway is not reachable. Start it on loopback first."
            .into(),
    }
}

fn map_gateway_error(status: reqwest::StatusCode, bytes: &[u8]) -> SeraError {
    let value: Value = serde_json::from_slice(bytes).unwrap_or(Value::Null);
    let code = value.get("code").and_then(Value::as_str);
    let gateway_message = || {
        value
            .get("error")
            .and_then(Value::as_str)
            .map(|m| m.chars().take(240).collect::<String>())
    };
    if status.as_u16() == 401 {
        return match code {
            Some("needs_reauth" | "not_signed_in") => SeraError::Rejected {
                code: "needs_reauth",
                message: gateway_message()
                    .unwrap_or_else(|| "Claude rejected this account. Sign in again.".into()),
            },
            Some("credentials_rejected") => SeraError::Rejected {
                code: "credentials_rejected",
                message: gateway_message()
                    .unwrap_or_else(|| "Anthropic rejected this API key.".into()),
            },
            _ => SeraError::Rejected {
                code: "gateway_unauthorized",
                message: "The Claude Agent SDK gateway rejected the bearer token.".into(),
            },
        };
    }
    match (status.as_u16(), code) {
        (404 | 410, Some("flow_not_found" | "flow_expired")) => {
            return SeraError::Rejected {
                code: "flow_expired",
                message: "This sign-in is no longer open. Start a new one.".into(),
            };
        }
        (409, Some("flow_used" | "flow_busy")) => {
            return SeraError::Rejected {
                code: "flow_used",
                message: gateway_message()
                    .unwrap_or_else(|| "This sign-in was already used.".into()),
            };
        }
        // Claude refused the code, or the CLI asked for it again (the
        // sign-in stays open then): show the CLI's own reason.
        (
            400,
            Some("auth_code_rejected" | "code_incomplete" | "invalid_code" | "state_mismatch"),
        ) => {
            return SeraError::Rejected {
                code: if code == Some("code_incomplete") {
                    "code_incomplete"
                } else {
                    "code_rejected"
                },
                message: gateway_message()
                    .unwrap_or_else(|| "Claude did not accept that code.".into()),
            };
        }
        _ => {}
    }
    if status.as_u16() == 503
        && value.get("code").and_then(Value::as_str) == Some("claude_cli_missing")
    {
        let message = value.get("error").and_then(Value::as_str).unwrap_or(
            "The Claude CLI (`claude`) is not installed or not on PATH on the gateway host.",
        );
        return SeraError::Rejected {
            code: "claude_cli_missing",
            message: message.to_owned(),
        };
    }
    let message = value
        .get("error")
        .and_then(Value::as_str)
        .unwrap_or("the local gateway refused the request");
    SeraError::External(message.to_owned())
}

impl GatewayEndpoint {
    /// Validate the endpoint: loopback HTTP only, per the gateway's own rules.
    pub fn validated(base_url: &str, token: Zeroizing<String>) -> Result<Self> {
        let base_url = crate::config::validate_loopback_gateway_url(base_url)?;
        Ok(Self { base_url, token })
    }

    fn url(&self, path: &str) -> String {
        format!("{}{}", self.base_url.trim_end_matches('/'), path)
    }

    async fn send_json(
        &self,
        method: reqwest::Method,
        path: &str,
        body: Option<Value>,
    ) -> Result<Value> {
        let client = http_client()?;
        let mut request = client
            .request(method, self.url(path))
            .bearer_auth(self.token.as_str());
        if let Some(body) = body {
            request = request.json(&body);
        }
        let response = request
            .send()
            .await
            .map_err(|_| gateway_unreachable_error())?;
        let status = response.status();
        let bytes = read_capped(response).await?;
        if !status.is_success() {
            return Err(map_gateway_error(status, &bytes));
        }
        serde_json::from_slice(&bytes)
            .map_err(|_| SeraError::External("the local gateway sent an unreadable reply".into()))
    }

    async fn get(&self, path: &str) -> Result<Value> {
        self.send_json(reqwest::Method::GET, path, None).await
    }

    async fn post(&self, path: &str, body: Value) -> Result<Value> {
        self.send_json(reqwest::Method::POST, path, Some(body))
            .await
    }
}

/// Unauthenticated `GET {base_url}/health`, validated as loopback.
pub async fn gateway_health(base_url: &str) -> Result<Value> {
    let base_url = crate::config::validate_loopback_gateway_url(base_url)?;
    let url = format!("{}/health", base_url.trim_end_matches('/'));
    let client = http_client()?;
    let response = client
        .get(url)
        .send()
        .await
        .map_err(|_| gateway_unreachable_error())?;
    let status = response.status();
    let bytes = read_capped(response).await?;
    if !status.is_success() {
        return Err(map_gateway_error(status, &bytes));
    }
    serde_json::from_slice(&bytes)
        .map_err(|_| SeraError::External("the local gateway sent an unreadable reply".into()))
}

/// Authenticated `GET /v1/auth/status`.
pub async fn gateway_auth_status(endpoint: &GatewayEndpoint) -> Result<Value> {
    endpoint.get("/v1/auth/status").await
}

/// Ask the gateway to start a sign-in flow (`POST /v1/auth/start`).
pub async fn gateway_auth_start(
    endpoint: &GatewayEndpoint,
    profile: Option<&str>,
) -> Result<GatewayAuthStarted> {
    let body = match profile {
        Some(p) => json!({ "profile": p }),
        None => json!({}),
    };
    let reply = endpoint.post("/v1/auth/start", body).await?;
    let flow_id = reply
        .get("flow_id")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| {
            SeraError::External("the local gateway did not return a sign-in flow id".into())
        })?
        .to_owned();
    let url = reply
        .get("url")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| {
            SeraError::External("the local gateway did not return a sign-in URL".into())
        })?
        .to_owned();
    let profile = reply
        .get("profile")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .unwrap_or("default")
        .to_owned();
    let expires_in = reply
        .get("expires_in")
        .and_then(Value::as_u64)
        .unwrap_or(600);
    let text = |key: &str| {
        reply
            .get(key)
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
    };
    let manual_url = text("manual_url").filter(|u| u.starts_with("https://"));
    let state = text("state");
    // Only a callback the desktop can safely listen for: an unprivileged
    // port and a plain path, together with the state that guards it.
    let callback = reply.get("callback").and_then(|cb| {
        let port = u16::try_from(cb.get("port")?.as_u64()?).ok()?;
        let path = cb.get("path")?.as_str()?;
        let plain = path.starts_with('/')
            && path.len() <= 64
            && path
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '/' | '_' | '-'));
        (port >= 1024 && plain && state.is_some()).then(|| GatewayCallback {
            port,
            path: path.to_owned(),
        })
    });
    Ok(GatewayAuthStarted {
        flow_id,
        url,
        manual_url,
        state: if callback.is_some() { state } else { None },
        callback,
        profile,
        expires_in,
    })
}

/// Poll the gateway's flow status (`GET /v1/auth/flow/{flow_id}`).
pub async fn gateway_auth_flow_status(
    endpoint: &GatewayEndpoint,
    gateway_flow_id: &str,
) -> Result<Value> {
    if gateway_flow_id.is_empty()
        || !gateway_flow_id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-'))
    {
        return Err(SeraError::Rejected {
            code: "bad_flow",
            message: "Invalid gateway flow id".into(),
        });
    }
    endpoint
        .get(&format!("/v1/auth/flow/{gateway_flow_id}"))
        .await
}

/// Finish a browser sign-in gateway-side (`POST /v1/auth/complete`): a
/// pasted `code#state`, or — with `state` — a browser redirect the desktop
/// caught, which the gateway hands to the CLI's own loopback listener.
/// Returns the full gateway reply (`{ok, account, profile}`).
pub async fn gateway_auth_complete(
    endpoint: &GatewayEndpoint,
    gateway_flow_id: &str,
    code: &str,
    state: Option<&str>,
) -> Result<Value> {
    let mut body = json!({ "flow_id": gateway_flow_id, "code": code });
    if let Some(state) = state.filter(|s| !s.is_empty()) {
        body["state"] = json!(state);
    }
    let reply = endpoint.post("/v1/auth/complete", body).await?;
    if reply.get("ok").and_then(Value::as_bool) != Some(true) {
        return Err(SeraError::External(
            "the local gateway did not finish the sign-in".into(),
        ));
    }
    Ok(reply)
}

/// Hand the gateway a pasted Claude credential (`POST /v1/auth/token`): a
/// `claude setup-token` token (`sk-ant-oat01-…`, billed to the Claude plan)
/// or a Console key (`sk-ant-api…`). The gateway keeps it in the account's
/// profile; SeraBot does not store it. `profile` is derived from the token's
/// hash so pasting the same token twice lands on the same account.
pub async fn gateway_auth_token(
    endpoint: &GatewayEndpoint,
    token: &str,
    profile: &str,
) -> Result<Value> {
    let reply = endpoint
        .post(
            "/v1/auth/token",
            json!({ "token": token, "profile": profile }),
        )
        .await?;
    if reply.get("ok").and_then(Value::as_bool) != Some(true) {
        return Err(SeraError::External(
            "the local gateway did not store the Claude token".into(),
        ));
    }
    Ok(reply)
}

/// The gateway profile for a pasted Claude token: `tok_` and the first 12
/// hex digits of its SHA-256 (not the token, and not reversible).
pub fn token_profile(token: &str) -> String {
    use sha2::Digest as _;
    let digest = sha2::Sha256::digest(token.trim().as_bytes());
    let hex: String = digest.iter().take(6).map(|b| format!("{b:02x}")).collect();
    format!("tok_{hex}")
}

/// Whether a pasted key is a Claude credential for the gateway rather than
/// the gateway's own bearer token.
pub fn is_claude_token(key: &str) -> bool {
    let key = key.trim();
    key.starts_with("sk-ant-oat") || key.starts_with("sk-ant-api")
}

/// Import the existing `claude` CLI login into the gateway's `"default"` profile
/// (`POST /v1/auth/import-cli`).
pub async fn gateway_auth_import_cli(endpoint: &GatewayEndpoint) -> Result<Value> {
    let reply = endpoint.post("/v1/auth/import-cli", json!({})).await?;
    if reply.get("ok").and_then(Value::as_bool) == Some(false) {
        return Err(SeraError::External(
            "the local gateway could not import the Claude CLI login".into(),
        ));
    }
    Ok(reply)
}

/// Verify a gateway profile's stored Claude credential live against Anthropic
/// (`POST /v1/auth/verify`).
pub async fn gateway_auth_verify(
    endpoint: &GatewayEndpoint,
    profile: Option<&str>,
) -> Result<Value> {
    let body = match profile {
        Some(p) if !p.trim().is_empty() => json!({ "profile": p.trim() }),
        _ => json!({}),
    };
    let reply = endpoint.post("/v1/auth/verify", body).await?;
    if reply.get("ok").and_then(Value::as_bool) != Some(true) {
        return Err(SeraError::Rejected {
            code: "needs_reauth",
            message: reply
                .get("error")
                .and_then(Value::as_str)
                .unwrap_or("Claude rejected this account.")
                .to_owned(),
        });
    }
    Ok(reply)
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn endpoint(server: &MockServer) -> GatewayEndpoint {
        GatewayEndpoint::validated(
            &format!("http://{}", server.address()),
            Zeroizing::new("gw-token".to_owned()),
        )
        .unwrap()
    }

    #[tokio::test]
    async fn non_loopback_gateway_urls_are_refused() {
        assert!(
            GatewayEndpoint::validated("https://127.0.0.1:8791", Zeroizing::new("t".into()))
                .is_err()
        );
        assert!(
            GatewayEndpoint::validated("http://192.168.1.5:8791", Zeroizing::new("t".into()))
                .is_err()
        );
        assert!(
            GatewayEndpoint::validated("http://127.0.0.1:8791", Zeroizing::new("t".into())).is_ok()
        );
    }

    #[tokio::test]
    async fn health_and_auth_status_work() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/health"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "status": "ok",
                "cli_installed": true,
                "sdk_installed": true,
            })))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/v1/auth/status"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "signed_in": true,
                "profiles": [{"profile": "default", "email": "owner@example.com"}],
            })))
            .mount(&server)
            .await;

        let health = gateway_health(&format!("http://{}", server.address()))
            .await
            .unwrap();
        assert_eq!(health["status"], "ok");
        assert_eq!(health["cli_installed"], true);

        let status = gateway_auth_status(&endpoint(&server)).await.unwrap();
        assert_eq!(status["signed_in"], true);
    }

    #[tokio::test]
    async fn start_relays_to_the_gateway_with_the_bearer_token() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/auth/start"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "flow_id": "gw-flow-1",
                "url": "https://claude.ai/oauth/authorize?state=abc",
                "profile": "default",
                "expires_in": 600,
            })))
            .expect(1)
            .mount(&server)
            .await;
        let started = gateway_auth_start(&endpoint(&server), None).await.unwrap();
        assert_eq!(started.flow_id, "gw-flow-1");
        assert_eq!(started.url, "https://claude.ai/oauth/authorize?state=abc");
        assert_eq!(started.profile, "default");
        assert_eq!(started.expires_in, 600);
    }

    #[tokio::test]
    async fn flow_status_complete_and_import_cli_work() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/auth/flow/gw-flow-1"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "status": "pending",
            })))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/v1/auth/complete"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "ok": true,
                "profile": "default",
                "account": {"emailAddress": "owner@example.com"},
            })))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/v1/auth/import-cli"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "ok": true,
                "profile": "default",
                "account": {"emailAddress": "cli@example.com"},
            })))
            .expect(1)
            .mount(&server)
            .await;

        let ep = endpoint(&server);
        let flow = gateway_auth_flow_status(&ep, "gw-flow-1").await.unwrap();
        assert_eq!(flow["status"], "pending");

        let reply = gateway_auth_complete(&ep, "gw-flow-1", "the-code", None)
            .await
            .unwrap();
        assert_eq!(reply["account"]["emailAddress"], "owner@example.com");
        assert_eq!(reply["profile"], "default");

        let imported = gateway_auth_import_cli(&ep).await.unwrap();
        assert_eq!(imported["account"]["emailAddress"], "cli@example.com");
    }

    #[tokio::test]
    async fn gateway_errors_map_to_actionable_rejected_codes() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/auth/status"))
            .respond_with(ResponseTemplate::new(401).set_body_json(json!({"error": "bad token"})))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/v1/auth/start"))
            .respond_with(ResponseTemplate::new(503).set_body_json(json!({
                "code": "claude_cli_missing",
                "error": "claude CLI not found",
            })))
            .mount(&server)
            .await;

        let ep = endpoint(&server);
        let err = gateway_auth_status(&ep).await.unwrap_err();
        assert!(
            matches!(
                err,
                SeraError::Rejected {
                    code: "gateway_unauthorized",
                    ..
                }
            ),
            "{err:?}"
        );

        let err = gateway_auth_start(&ep, None).await.unwrap_err();
        assert!(
            matches!(
                err,
                SeraError::Rejected {
                    code: "claude_cli_missing",
                    ..
                }
            ),
            "{err:?}"
        );

        // Unreachable port
        let dead =
            GatewayEndpoint::validated("http://127.0.0.1:1", Zeroizing::new("gw-token".to_owned()))
                .unwrap();
        let err = gateway_auth_status(&dead).await.unwrap_err();
        assert!(
            matches!(
                err,
                SeraError::Rejected {
                    code: "gateway_unreachable",
                    ..
                }
            ),
            "{err:?}"
        );
    }

    #[tokio::test]
    async fn start_keeps_only_a_safe_callback_and_complete_relays_the_state() {
        use wiremock::matchers::body_partial_json;
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/auth/start"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "flow_id": "gw-flow-2",
                "url": "https://claude.example/cai/oauth/authorize?x=1",
                "manual_url": "https://claude.example/oauth/authorize?z-1",
                "callback": {"port": 80, "path": "/callback"},
                "state": "s1",
            })))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/v1/auth/complete"))
            .and(body_partial_json(
                json!({"flow_id": "gw-flow-2", "code": "c", "state": "s1"}),
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"ok": true})))
            .expect(1)
            .mount(&server)
            .await;
        let ep = endpoint(&server);
        let started = gateway_auth_start(&ep, None).await.unwrap();
        // A privileged port is never handed to the desktop to listen on.
        assert_eq!(started.callback, None);
        assert_eq!(started.state, None);
        assert_eq!(
            started.manual_url.as_deref(),
            Some("https://claude.example/oauth/authorize?z-1")
        );
        gateway_auth_complete(&ep, "gw-flow-2", "c", Some("s1"))
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn gateway_flow_errors_map_to_their_own_codes() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/auth/flow/gone"))
            .respond_with(ResponseTemplate::new(404).set_body_json(json!({
                "error": "unknown sign-in flow", "code": "flow_not_found",
            })))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/v1/auth/complete"))
            .respond_with(ResponseTemplate::new(400).set_body_json(json!({
                "error": "Copy all of it, including the # in the middle.",
                "code": "code_incomplete",
            })))
            .mount(&server)
            .await;
        let ep = endpoint(&server);
        let err = gateway_auth_flow_status(&ep, "gone").await.unwrap_err();
        assert!(
            matches!(
                err,
                SeraError::Rejected {
                    code: "flow_expired",
                    ..
                }
            ),
            "{err:?}"
        );
        let err = gateway_auth_complete(&ep, "f", "half", None)
            .await
            .unwrap_err();
        match err {
            SeraError::Rejected {
                code: "code_incomplete",
                message,
            } => assert!(message.contains('#'), "{message}"),
            other => panic!("{other:?}"),
        }
    }

    #[tokio::test]
    async fn a_pasted_claude_token_goes_to_the_gateway_under_a_hashed_profile() {
        use wiremock::matchers::body_partial_json;
        let token = "sk-ant-oat01-synthetic-token-not-real-0000";
        assert!(is_claude_token(token));
        assert!(is_claude_token(" sk-ant-api03-synthetic "));
        assert!(!is_claude_token("gw-bearer-token"));
        let profile = token_profile(token);
        assert!(
            profile.starts_with("tok_") && profile.len() == 16,
            "{profile}"
        );
        assert!(!profile.contains("synthetic"));
        assert_eq!(profile, token_profile(&format!(" {token} ")));

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/auth/token"))
            .and(body_partial_json(
                json!({"token": token, "profile": profile}),
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "ok": true, "profile": profile, "billing": "plan",
                "account": {"authMethod": "oauth_token"},
            })))
            .expect(1)
            .mount(&server)
            .await;
        let reply = gateway_auth_token(&endpoint(&server), token, &profile)
            .await
            .unwrap();
        assert_eq!(reply["billing"], "plan");
    }

    #[tokio::test]
    async fn a_refused_exchange_surfaces_the_gateway_error() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/auth/complete"))
            .respond_with(ResponseTemplate::new(502).set_body_json(json!({
                "error": "token exchange failed",
            })))
            .mount(&server)
            .await;
        let error = gateway_auth_complete(&endpoint(&server), "gw-flow-1", "bad", None)
            .await
            .unwrap_err();
        assert!(
            error.to_string().contains("token exchange failed"),
            "{error}"
        );
    }

    #[tokio::test]
    async fn gateway_auth_verify_maps_ok_and_needs_reauth() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/auth/verify"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "ok": true,
                "profile": "tok_abc123",
                "billing": "plan",
                "model_count": 5,
            })))
            .up_to_n_times(1)
            .mount(&server)
            .await;
        let reply = gateway_auth_verify(&endpoint(&server), Some("tok_abc123"))
            .await
            .unwrap();
        assert_eq!(reply["model_count"], 5);

        Mock::given(method("POST"))
            .and(path("/v1/auth/verify"))
            .respond_with(ResponseTemplate::new(401).set_body_json(json!({
                "ok": false,
                "code": "needs_reauth",
                "error": "Claude rejected this setup-token.",
            })))
            .mount(&server)
            .await;
        let err = gateway_auth_verify(&endpoint(&server), Some("tok_abc123"))
            .await
            .unwrap_err();
        assert!(matches!(
            err,
            SeraError::Rejected {
                code: "needs_reauth",
                ..
            }
        ));
    }
}
