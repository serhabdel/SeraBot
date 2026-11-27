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
    pub url: String,
    pub profile: String,
    pub expires_in: u64,
}

fn gateway_unreachable_error() -> SeraError {
    SeraError::Rejected {
        code: "gateway_unreachable",
        message: "The Claude Agent SDK gateway is not reachable. Start it on loopback first."
            .into(),
    }
}

fn map_gateway_error(status: reqwest::StatusCode, bytes: &[u8]) -> SeraError {
    if status.as_u16() == 401 {
        return SeraError::Rejected {
            code: "gateway_unauthorized",
            message: "The Claude Agent SDK gateway rejected the bearer token.".into(),
        };
    }
    let value: Value = serde_json::from_slice(bytes).unwrap_or(Value::Null);
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
    Ok(GatewayAuthStarted {
        flow_id,
        url,
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

/// Finish a browser sign-in gateway-side by submitting the pasted code
/// (`POST /v1/auth/complete`). Returns the full gateway reply (`{ok, account, profile}`).
pub async fn gateway_auth_complete(
    endpoint: &GatewayEndpoint,
    gateway_flow_id: &str,
    code: &str,
) -> Result<Value> {
    let reply = endpoint
        .post(
            "/v1/auth/complete",
            json!({ "flow_id": gateway_flow_id, "code": code }),
        )
        .await?;
    if reply.get("ok").and_then(Value::as_bool) != Some(true) {
        return Err(SeraError::External(
            "the local gateway did not finish the sign-in".into(),
        ));
    }
    Ok(reply)
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

        let reply = gateway_auth_complete(&ep, "gw-flow-1", "the-code")
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
    async fn a_refused_exchange_surfaces_the_gateway_error() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/auth/complete"))
            .respond_with(ResponseTemplate::new(502).set_body_json(json!({
                "error": "token exchange failed",
            })))
            .mount(&server)
            .await;
        let error = gateway_auth_complete(&endpoint(&server), "gw-flow-1", "bad")
            .await
            .unwrap_err();
        assert!(
            error.to_string().contains("token exchange failed"),
            "{error}"
        );
    }
}
