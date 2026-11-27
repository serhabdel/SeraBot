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

impl GatewayEndpoint {
    /// Validate the endpoint: loopback HTTP only, per the gateway's own rules.
    pub fn validated(base_url: &str, token: Zeroizing<String>) -> Result<Self> {
        let base_url = crate::config::validate_loopback_gateway_url(base_url)?;
        Ok(Self { base_url, token })
    }

    fn url(&self, path: &str) -> String {
        format!("{}{}", self.base_url.trim_end_matches('/'), path)
    }

    async fn post(&self, path: &str, body: Value) -> Result<Value> {
        let client = http_client()?;
        let response = client
            .post(self.url(path))
            .bearer_auth(self.token.as_str())
            .json(&body)
            .send()
            .await
            .map_err(|_| SeraError::External("the local gateway did not answer".into()))?;
        let status = response.status();
        let bytes = read_capped(response).await?;
        let value: Value = serde_json::from_slice(&bytes).map_err(|_| {
            SeraError::External("the local gateway sent an unreadable reply".into())
        })?;
        if !status.is_success() {
            let message = value
                .get("error")
                .and_then(Value::as_str)
                .unwrap_or("the local gateway refused the sign-in");
            return Err(SeraError::External(message.to_owned()));
        }
        Ok(value)
    }
}

/// Ask the gateway for the authorize URL of a browser sign-in. Returns
/// `(url, state)`.
pub async fn gateway_auth_start(
    endpoint: &GatewayEndpoint,
    redirect_uri: &str,
    state: &str,
) -> Result<(String, String)> {
    let reply = endpoint
        .post(
            "/v1/auth/start",
            json!({"redirect_uri": redirect_uri, "state": state}),
        )
        .await?;
    let url = reply.get("url").and_then(Value::as_str).ok_or_else(|| {
        SeraError::External("the local gateway did not return a sign-in URL".into())
    })?;
    let state = reply.get("state").and_then(Value::as_str).ok_or_else(|| {
        SeraError::External("the local gateway did not return a sign-in state".into())
    })?;
    Ok((url.to_owned(), state.to_owned()))
}

/// Finish the browser sign-in gateway-side. Returns the non-secret account
/// identity the gateway stored.
pub async fn gateway_auth_complete(
    endpoint: &GatewayEndpoint,
    state: &str,
    code: &str,
    redirect_uri: &str,
) -> Result<Value> {
    let reply = endpoint
        .post(
            "/v1/auth/complete",
            json!({"state": state, "code": code, "redirect_uri": redirect_uri}),
        )
        .await?;
    if reply.get("ok").and_then(Value::as_bool) != Some(true) {
        return Err(SeraError::External(
            "the local gateway did not finish the sign-in".into(),
        ));
    }
    Ok(reply.get("account").cloned().unwrap_or(Value::Null))
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
    async fn start_relays_to_the_gateway_with_the_bearer_token() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/auth/start"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "url": "https://claude.ai/oauth/authorize?state=abc",
                "state": "abc",
                "expires_in": 600,
            })))
            .expect(1)
            .mount(&server)
            .await;
        let (url, state) =
            gateway_auth_start(&endpoint(&server), "http://127.0.0.1:54545/callback", "abc")
                .await
                .unwrap();
        assert_eq!(url, "https://claude.ai/oauth/authorize?state=abc");
        assert_eq!(state, "abc");
    }

    #[tokio::test]
    async fn complete_relays_the_code_and_returns_the_account() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/auth/complete"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "ok": true,
                "account": {"emailAddress": "owner@example.com"},
            })))
            .expect(1)
            .mount(&server)
            .await;
        let account = gateway_auth_complete(
            &endpoint(&server),
            "abc",
            "the-code",
            "http://127.0.0.1:54545/callback",
        )
        .await
        .unwrap();
        assert_eq!(account["emailAddress"], "owner@example.com");
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
        let error = gateway_auth_complete(
            &endpoint(&server),
            "abc",
            "bad",
            "http://127.0.0.1:54545/callback",
        )
        .await
        .unwrap_err();
        assert!(
            error.to_string().contains("token exchange failed"),
            "{error}"
        );
    }
}
