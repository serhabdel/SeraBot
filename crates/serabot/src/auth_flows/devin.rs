//! Devin CLI OAuth sign-in (PKCE + pasted `devin-session-token$...`).
//!
//! Wire protocol:
//! - Authorize: `GET https://app.devin.ai/auth/cli/continue` with
//!   `code_challenge`, `code_challenge_method=S256`, `state`,
//!   `prompt=select_account`, and `redirect_uri` (`http://127.0.0.1:59653/callback`).
//! - Exchange: `POST https://api.devin.ai/auth/cli/token` with JSON
//!   `{"code": code, "code_verifier": verifier}` and `Accept: application/json`,
//!   returning `{"token": "<jwt-or-session-token>", ...}`.
//! - Paste flow: a member may also paste a `devin-session-token$...` directly.
//!
//! Tokens, codes, and verifiers never appear in an error or an event.

use async_trait::async_trait;
use base64::Engine;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::accounts::{AuthKind, identity_fingerprint};
use crate::error::{Result, SeraError};
use crate::user_secrets::ProviderCredential;

use super::pkce::{RedirectSpec, pkce_token_ok, state_ok, validate_loopback_redirect};
use super::{FlowKind, PinnedEndpoint, SignInProvider, SignedIn, credential_ok, read_capped};

/// Devin CLI authorization host and path (pinned).
pub const AUTHORIZE_HOST: &str = "app.devin.ai";
pub const AUTHORIZE_PATH: &str = "/auth/cli/continue";
/// Devin CLI token exchange host and path (pinned).
pub const TOKEN_HOST: &str = "api.devin.ai";
pub const TOKEN_PATH: &str = "/auth/cli/token";
pub const TOKEN_URL: &str = "https://api.devin.ai/auth/cli/token";

/// Fixed loopback port and path Devin CLI's OAuth client allows.
pub const CALLBACK_PORT: u16 = 59653;
pub const CALLBACK_PATH: &str = "/callback";

/// Prefix for pasted Devin CLI session tokens.
pub const SESSION_TOKEN_PREFIX: &str = "devin-session-token$";

/// Fallback token lifetime when a returned token has no `exp` claim (1 year).
const FALLBACK_TTL_SECS: i64 = 365 * 24 * 60 * 60;

/// Account label stored for Devin CLI sign-ins.
const ACCOUNT_LABEL: &str = "Devin CLI";

fn redirect_spec() -> RedirectSpec {
    RedirectSpec {
        port: Some(CALLBACK_PORT),
        path: Some(CALLBACK_PATH),
    }
}

/// Devin CLI sign-in, registered under the `devin` provider kind.
pub struct DevinSignIn {
    authorize: PinnedEndpoint,
    token: PinnedEndpoint,
}

impl DevinSignIn {
    /// Create a Devin CLI sign-in against the pinned production endpoints.
    pub fn new() -> Self {
        Self {
            authorize: PinnedEndpoint::https(AUTHORIZE_HOST, AUTHORIZE_PATH),
            token: PinnedEndpoint::https(TOKEN_HOST, TOKEN_PATH),
        }
    }

    /// Point the token endpoint at a mock server on exact loopback for tests.
    #[cfg(test)]
    pub(crate) fn for_test(token_url: &str) -> Self {
        Self {
            authorize: PinnedEndpoint::https(AUTHORIZE_HOST, AUTHORIZE_PATH),
            token: PinnedEndpoint::loopback(token_url),
        }
    }
}

impl Default for DevinSignIn {
    fn default() -> Self {
        Self::new()
    }
}

/// Decode a JWT payload without verifying its signature (the token just arrived
/// over TLS from the pinned token endpoint).
fn jwt_payload(jwt: &str) -> Option<Value> {
    let payload = jwt.split('.').nth(1)?;
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(payload.trim_end_matches('='))
        .ok()?;
    serde_json::from_slice(&bytes).ok()
}

/// Parse the `exp` claim (Unix seconds) from a JWT, if present and positive.
pub fn parse_jwt_exp(token: &str) -> Option<i64> {
    jwt_payload(token)?
        .get("exp")
        .and_then(Value::as_i64)
        .filter(|exp| *exp > 0)
}

fn email_ok(email: &str) -> bool {
    email.len() <= 254
        && email.contains('@')
        && !email.chars().any(|c| c.is_control() || c.is_whitespace())
}

fn id_ok(value: &str) -> bool {
    (1..=128).contains(&value.len()) && !value.chars().any(|c| c.is_control() || c.is_whitespace())
}

fn extract_identity(body: &Value, token: &str) -> (String, Option<String>, Option<String>) {
    let payload = jwt_payload(token);
    let user_id = body
        .get("user_id")
        .or_else(|| body.get("sub"))
        .and_then(Value::as_str)
        .or_else(|| {
            payload
                .as_ref()
                .and_then(|p| p.get("sub").or_else(|| p.get("user_id")))
                .and_then(Value::as_str)
        })
        .map(str::trim)
        .filter(|id| id_ok(id))
        .map(str::to_owned);

    let email = body
        .get("email")
        .and_then(Value::as_str)
        .or_else(|| {
            payload
                .as_ref()
                .and_then(|p| p.get("email"))
                .and_then(Value::as_str)
        })
        .map(|e| e.trim().to_lowercase())
        .filter(|e| email_ok(e));

    let stable = match (&user_id, &email) {
        (Some(id), _) => format!("account:{id}"),
        (None, Some(email)) => format!("email:{email}"),
        (None, None) => {
            let digest = hex::encode(Sha256::digest(token.as_bytes()));
            format!("token:{digest}")
        }
    };
    (stable, user_id, email)
}

/// Split a pasted callback URL (`http://127.0.0.1:59653/callback?code=...&state=...`)
/// or raw code/token.
fn split_pasted_code(input: &str) -> (String, Option<String>) {
    let input = input.trim();
    if (input.starts_with("http://") || input.starts_with("https://"))
        && let Ok(url) = url::Url::parse(input)
    {
        let mut code = None;
        let mut state = None;
        for (key, value) in url.query_pairs() {
            match key.as_ref() {
                "code" => {
                    let trimmed = value.trim();
                    if !trimmed.is_empty() {
                        code = Some(trimmed.to_owned());
                    }
                }
                "state" => state = Some(value.trim().to_owned()),
                _ => {}
            }
        }
        if let Some(code) = code {
            return (code, state);
        }
    }
    (input.to_owned(), None)
}

#[async_trait]
impl SignInProvider for DevinSignIn {
    fn flows(&self) -> &'static [FlowKind] {
        &[FlowKind::Pkce]
    }

    fn redirect_spec(&self) -> Option<RedirectSpec> {
        Some(redirect_spec())
    }

    fn authorize_url(&self, redirect_uri: &str, challenge: &str, state: &str) -> Result<String> {
        validate_loopback_redirect(&redirect_spec(), redirect_uri)?;
        if !pkce_token_ok(challenge) || !state_ok(state) {
            return Err(SeraError::Rejected {
                code: "bad_challenge",
                message: "The sign-in challenge or state is not valid".into(),
            });
        }
        let mut url = self.authorize.checked_url()?;
        url.query_pairs_mut()
            .append_pair("code_challenge", challenge)
            .append_pair("code_challenge_method", "S256")
            .append_pair("state", state)
            .append_pair("prompt", "select_account")
            .append_pair("redirect_uri", redirect_uri);
        Ok(url.to_string())
    }

    async fn exchange_pkce(
        &self,
        http: &reqwest::Client,
        code: &str,
        redirect_uri: &str,
        verifier: &str,
    ) -> Result<SignedIn> {
        self.exchange_pkce_with_state(http, code, redirect_uri, verifier, "")
            .await
    }

    async fn exchange_pkce_with_state(
        &self,
        http: &reqwest::Client,
        code: &str,
        redirect_uri: &str,
        verifier: &str,
        state: &str,
    ) -> Result<SignedIn> {
        validate_loopback_redirect(&redirect_spec(), redirect_uri)?;
        let (code, pasted_state) = split_pasted_code(code);
        if pasted_state
            .as_deref()
            .is_some_and(|pasted| !state.is_empty() && pasted != state)
        {
            return Err(SeraError::Rejected {
                code: "bad_code",
                message: "The sign-in code does not belong to this sign-in".into(),
            });
        }

        if let Some(rest) = code.strip_prefix(SESSION_TOKEN_PREFIX) {
            if rest.is_empty()
                || !credential_ok(&code)
                || code.chars().any(|c| c.is_whitespace() || c.is_control())
            {
                return Err(SeraError::Rejected {
                    code: "bad_code",
                    message: "The Devin session token is not valid".into(),
                });
            }
            let digest = hex::encode(Sha256::digest(code.as_bytes()));
            return Ok(SignedIn {
                label: ACCOUNT_LABEL.into(),
                auth_kind: AuthKind::Oauth,
                fingerprint: Some(identity_fingerprint(
                    "devin",
                    &format!("session_token:{digest}"),
                )),
                identity: json!({"plan": ACCOUNT_LABEL}),
                credential: ProviderCredential {
                    expires_at: None,
                    api_key: code,
                    refresh_token: String::new(),
                    generation: None,
                },
            });
        }

        if code.is_empty()
            || code.len() > 2_048
            || code.chars().any(char::is_control)
            || !pkce_token_ok(verifier)
        {
            return Err(SeraError::Rejected {
                code: "bad_code",
                message: "The sign-in code or verifier is not valid".into(),
            });
        }

        let response = http
            .post(self.token.checked_url()?)
            .header("accept", "application/json")
            .json(&json!({
                "code": code,
                "code_verifier": verifier,
            }))
            .send()
            .await
            .map_err(|_| SeraError::External("the sign-in provider could not be reached".into()))?;

        let status = response.status();
        let bytes = read_capped(response).await?;
        if !status.is_success() {
            let error_code = serde_json::from_slice::<Value>(&bytes)
                .ok()
                .and_then(|body| body.get("error").and_then(Value::as_str).map(str::to_owned))
                .unwrap_or_default();
            tracing::warn!(
                status = status.as_u16(),
                error_code = error_code.as_str(),
                "Devin CLI OAuth token exchange failed"
            );
            if status.as_u16() == 429 || status.is_server_error() {
                return Err(SeraError::External(
                    "Devin could not complete sign-in right now. Try again shortly.".into(),
                ));
            }
            return Err(SeraError::Rejected {
                code: "signin_refused",
                message: "The provider refused the sign-in code. Start the sign-in again.".into(),
            });
        }

        let body: Value = serde_json::from_slice(&bytes).map_err(|_| {
            SeraError::External("the sign-in provider sent an invalid token reply".into())
        })?;
        let token = body
            .get("token")
            .and_then(Value::as_str)
            .filter(|t| credential_ok(t))
            .ok_or_else(|| {
                SeraError::External("the sign-in provider sent an invalid token reply".into())
            })?
            .to_owned();

        let refresh_token = body
            .get("refresh_token")
            .and_then(Value::as_str)
            .filter(|t| credential_ok(t))
            .map(str::to_owned)
            .unwrap_or_default();

        let expires_at = parse_jwt_exp(&token)
            .unwrap_or_else(|| chrono::Utc::now().timestamp() + FALLBACK_TTL_SECS);

        let (stable, user_id, email) = extract_identity(&body, &token);
        let mut identity = json!({"plan": ACCOUNT_LABEL});
        if let Some(uid) = user_id {
            identity["user_id"] = json!(uid);
        }
        if let Some(email) = email {
            identity["email"] = json!(email);
        }

        Ok(SignedIn {
            label: ACCOUNT_LABEL.into(),
            auth_kind: AuthKind::Oauth,
            fingerprint: Some(identity_fingerprint("devin", &stable)),
            identity,
            credential: ProviderCredential {
                expires_at: Some(expires_at),
                api_key: token,
                refresh_token,
                generation: None,
            },
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{body_json, header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    const REDIRECT_URI: &str = "http://127.0.0.1:59653/callback";
    const VERIFIER: &str = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
    const CHALLENGE: &str = "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM";
    const STATE: &str = "state_token_1234567890";

    fn fake_jwt(payload: &Value) -> String {
        let header = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(br#"{"alg":"HS256","typ":"JWT"}"#);
        let body = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(serde_json::to_vec(payload).unwrap());
        format!("{header}.{body}.signature")
    }

    #[test]
    fn authorize_url_has_expected_shape() {
        let provider = DevinSignIn::new();
        assert_eq!(provider.flows(), &[FlowKind::Pkce]);
        assert_eq!(
            provider.redirect_spec(),
            Some(RedirectSpec {
                port: Some(59653),
                path: Some("/callback"),
            })
        );

        let raw = provider
            .authorize_url(REDIRECT_URI, CHALLENGE, STATE)
            .unwrap();
        let url = url::Url::parse(&raw).unwrap();
        assert_eq!(url.scheme(), "https");
        assert_eq!(url.host_str(), Some("app.devin.ai"));
        assert_eq!(url.path(), "/auth/cli/continue");

        let params: std::collections::BTreeMap<String, String> = url
            .query_pairs()
            .map(|(k, v)| (k.into_owned(), v.into_owned()))
            .collect();
        assert_eq!(
            params.get("code_challenge").map(String::as_str),
            Some(CHALLENGE)
        );
        assert_eq!(
            params.get("code_challenge_method").map(String::as_str),
            Some("S256")
        );
        assert_eq!(params.get("state").map(String::as_str), Some(STATE));
        assert_eq!(
            params.get("prompt").map(String::as_str),
            Some("select_account")
        );
        assert_eq!(
            params.get("redirect_uri").map(String::as_str),
            Some(REDIRECT_URI)
        );
    }

    #[test]
    fn authorize_url_rejects_wrong_redirect_or_challenge() {
        let provider = DevinSignIn::new();
        assert!(
            provider
                .authorize_url("http://127.0.0.1:9999/callback", CHALLENGE, STATE)
                .is_err()
        );
        assert!(
            provider
                .authorize_url("http://127.0.0.1:59653/other", CHALLENGE, STATE)
                .is_err()
        );
        assert!(
            provider
                .authorize_url(REDIRECT_URI, "short", STATE)
                .is_err()
        );
        assert!(
            provider
                .authorize_url(REDIRECT_URI, CHALLENGE, "bad")
                .is_err()
        );
    }

    #[tokio::test]
    async fn pkce_exchange_posts_json_and_extracts_jwt_exp() {
        let server = MockServer::start().await;
        let expected_exp = chrono::Utc::now().timestamp() + 7200;
        let jwt = fake_jwt(&json!({
            "sub": "user-devin-42",
            "email": "dev@example.com",
            "exp": expected_exp,
        }));

        Mock::given(method("POST"))
            .and(path("/auth/cli/token"))
            .and(header("accept", "application/json"))
            .and(body_json(json!({
                "code": "auth-code-123",
                "code_verifier": VERIFIER,
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "token": jwt,
            })))
            .mount(&server)
            .await;

        let provider = DevinSignIn::for_test(&format!("{}/auth/cli/token", server.uri()));
        let http = super::super::http_client().unwrap();
        let signed_in = provider
            .exchange_pkce(&http, "auth-code-123", REDIRECT_URI, VERIFIER)
            .await
            .unwrap();

        assert_eq!(signed_in.label, "Devin CLI");
        assert_eq!(signed_in.auth_kind, AuthKind::Oauth);
        assert_eq!(signed_in.credential.api_key, jwt);
        assert_eq!(signed_in.credential.expires_at, Some(expected_exp));
        assert_eq!(
            signed_in.fingerprint,
            Some(identity_fingerprint("devin", "account:user-devin-42"))
        );
        assert_eq!(signed_in.identity["email"], json!("dev@example.com"));
    }

    #[tokio::test]
    async fn pkce_exchange_falls_back_to_one_year_when_token_has_no_exp() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/auth/cli/token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "token": "opaque-devin-token-abc",
            })))
            .mount(&server)
            .await;

        let provider = DevinSignIn::for_test(&format!("{}/auth/cli/token", server.uri()));
        let http = super::super::http_client().unwrap();
        let before = chrono::Utc::now().timestamp() + FALLBACK_TTL_SECS - 5;
        let signed_in = provider
            .exchange_pkce(&http, "auth-code-xyz", REDIRECT_URI, VERIFIER)
            .await
            .unwrap();
        let after = chrono::Utc::now().timestamp() + FALLBACK_TTL_SECS + 5;

        let exp = signed_in.credential.expires_at.unwrap();
        assert!(exp >= before && exp <= after);
        assert_eq!(signed_in.label, "Devin CLI");
        assert_eq!(signed_in.auth_kind, AuthKind::Oauth);
    }

    #[tokio::test]
    async fn pasted_session_token_succeeds_without_network() {
        let provider = DevinSignIn::new();
        let http = super::super::http_client().unwrap();
        let token = "devin-session-token$abc123def456";

        let first = provider
            .exchange_pkce(&http, token, REDIRECT_URI, VERIFIER)
            .await
            .unwrap();
        let second = provider
            .exchange_pkce(&http, token, REDIRECT_URI, VERIFIER)
            .await
            .unwrap();

        let expected_digest = hex::encode(Sha256::digest(token.as_bytes()));
        assert_eq!(first.label, "Devin CLI");
        assert_eq!(first.auth_kind, AuthKind::Oauth);
        assert_eq!(first.credential.api_key, token);
        assert_eq!(
            first.fingerprint,
            Some(identity_fingerprint(
                "devin",
                &format!("session_token:{expected_digest}")
            ))
        );
        assert_eq!(first.fingerprint, second.fingerprint);
    }

    #[tokio::test]
    async fn pasted_session_token_rejects_empty_whitespace_and_control_chars() {
        let provider = DevinSignIn::new();
        let http = super::super::http_client().unwrap();

        for bad in [
            "devin-session-token$",
            "devin-session-token$has space",
            "devin-session-token$has\ttab",
            "devin-session-token$has\x07bell",
        ] {
            assert!(
                provider
                    .exchange_pkce(&http, bad, REDIRECT_URI, VERIFIER)
                    .await
                    .is_err(),
                "expected rejection for {bad:?}"
            );
        }
    }

    #[test]
    fn jwt_exp_parsing_handles_valid_and_invalid_tokens() {
        let valid = fake_jwt(&json!({"exp": 1_800_000_000i64}));
        assert_eq!(parse_jwt_exp(&valid), Some(1_800_000_000));

        let no_exp = fake_jwt(&json!({"sub": "u1"}));
        assert_eq!(parse_jwt_exp(&no_exp), None);

        let zero_exp = fake_jwt(&json!({"exp": 0}));
        assert_eq!(parse_jwt_exp(&zero_exp), None);

        assert_eq!(parse_jwt_exp("not-a-jwt"), None);
        assert_eq!(parse_jwt_exp("a.!!!invalid-b64!!!.c"), None);
    }
}
