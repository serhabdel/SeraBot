//! OpenRouter "Sign in with OpenRouter" (OAuth PKCE).
//!
//! Documented flow (<https://openrouter.ai/docs/use-cases/oauth-pkce>): the
//! user is sent to `https://openrouter.ai/auth` with a `callback_url` and an
//! S256 `code_challenge`; OpenRouter redirects back with a one-time `code`;
//! the app exchanges it with `POST /api/v1/auth/keys` for a user-controlled
//! API key. There is no client registration, no client secret and no refresh
//! token: the result is a normal OpenRouter API key, billed to the user.
//!
//! The server builds the authorization URL and performs the exchange, so the
//! key goes straight into the acting user's envelope-encrypted store and is
//! never returned to the desktop. Keys, codes and verifiers never appear in
//! errors or logs.

use serde_json::{Value, json};

use crate::error::{Result, SeraError};
use crate::llm::validate_base_url;

/// Where the user approves the app.
pub const AUTH_URL: &str = "https://openrouter.ai/auth";
/// Where the one-time code becomes an API key.
pub const KEYS_URL: &str = "https://openrouter.ai/api/v1/auth/keys";

/// The authorization page for a loopback sign-in.
///
/// `callback_url` must be a loopback address (`localhost` or `127.0.0.1`,
/// any port): OpenRouter supports localhost callbacks, and the desktop only
/// ever listens there. `challenge` is the unpadded base64url S256 digest of
/// the verifier the caller keeps; `state` is echoed back on the callback.
pub fn authorize_url(callback_url: &str, challenge: &str, state: &str) -> Result<String> {
    let callback = url::Url::parse(callback_url)
        .map_err(|_| SeraError::Validation("invalid sign-in request".into()))?;
    let loopback = callback.scheme() == "http"
        && matches!(callback.host_str(), Some("localhost" | "127.0.0.1"))
        && callback.port().is_some()
        && callback.username().is_empty()
        && callback.password().is_none()
        && callback.fragment().is_none();
    let challenge_ok = (43..=128).contains(&challenge.len())
        && challenge
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'));
    let state_ok = !state.is_empty()
        && state.len() <= 128
        && state
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'));
    if !loopback || !challenge_ok || !state_ok {
        return Err(SeraError::Validation("invalid sign-in request".into()));
    }
    let mut url = url::Url::parse(AUTH_URL).expect("constant URL parses");
    url.query_pairs_mut()
        .append_pair("callback_url", callback.as_str())
        .append_pair("code_challenge", challenge)
        .append_pair("code_challenge_method", "S256");
    // `state` is returned unchanged on the callback; the desktop checks it.
    url.query_pairs_mut().append_pair("state", state);
    Ok(url.to_string())
}

/// Exchange the one-time code for the user's API key.
///
/// `keys_url` is [`KEYS_URL`] in production; tests point it at a synthetic
/// server. Any failure is reported by status only: the response body and the
/// request secrets are never copied into the error.
pub async fn exchange_code(
    http: &reqwest::Client,
    keys_url: &str,
    code: &str,
    verifier: &str,
) -> Result<String> {
    let secretish = |value: &str| {
        !value.is_empty()
            && value.len() <= 512
            && value
                .bytes()
                .all(|byte| byte.is_ascii_graphic() && byte != b'"' && byte != b'\\')
    };
    if !secretish(code) || !secretish(verifier) {
        return Err(SeraError::Validation("invalid sign-in request".into()));
    }
    let endpoint = validate_base_url(keys_url)?;
    let response = http
        .post(&endpoint)
        .header("user-agent", crate::providers::USER_AGENT)
        .header("accept", "application/json")
        .json(&json!({
            "code": code,
            "code_verifier": verifier,
            "code_challenge_method": "S256",
        }))
        .send()
        .await
        .map_err(|_| SeraError::External("OpenRouter sign-in failed".into()))?;
    let status = response.status();
    let bytes = response
        .bytes()
        .await
        .map_err(|_| SeraError::External("OpenRouter sign-in failed".into()))?;
    if !status.is_success() {
        // 403 is an invalid or expired code per the docs.
        return Err(SeraError::External(format!(
            "OpenRouter sign-in failed with status {}",
            status.as_u16()
        )));
    }
    if bytes.len() > 65_536 {
        return Err(SeraError::External("OpenRouter sign-in failed".into()));
    }
    let value: Value = serde_json::from_slice(&bytes)
        .map_err(|_| SeraError::External("OpenRouter sign-in failed".into()))?;
    let key = value
        .get("key")
        .and_then(Value::as_str)
        .filter(|key| {
            !key.is_empty()
                && key.len() <= 8_192
                && key.trim() == *key
                && !key.chars().any(char::is_control)
        })
        .ok_or_else(|| SeraError::External("OpenRouter sign-in failed".into()))?;
    Ok(key.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{body_json, header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    const CHALLENGE: &str = "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM";

    fn http() -> reqwest::Client {
        reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .unwrap()
    }

    #[test]
    fn authorize_url_carries_the_documented_parameters() {
        let url = authorize_url("http://localhost:51423/callback", CHALLENGE, "state_1").unwrap();
        let parsed = url::Url::parse(&url).unwrap();
        assert_eq!(parsed.host_str(), Some("openrouter.ai"));
        assert_eq!(parsed.path(), "/auth");
        let pairs: std::collections::BTreeMap<_, _> = parsed.query_pairs().into_owned().collect();
        assert_eq!(pairs["callback_url"], "http://localhost:51423/callback");
        assert_eq!(pairs["code_challenge"], CHALLENGE);
        assert_eq!(pairs["code_challenge_method"], "S256");
        assert_eq!(pairs["state"], "state_1");
        assert!(authorize_url("http://127.0.0.1:9/cb", CHALLENGE, "s").is_ok());
    }

    #[test]
    fn authorize_url_refuses_non_loopback_callbacks_and_bad_inputs() {
        for callback in [
            "https://evil.example/cb",
            "http://evil.example:80/cb",
            "http://localhost/cb",
            "http://user:pw@localhost:1/cb",
            "not a url",
        ] {
            assert!(
                authorize_url(callback, CHALLENGE, "s").is_err(),
                "{callback}"
            );
        }
        assert!(authorize_url("http://localhost:1/cb", "short", "s").is_err());
        assert!(authorize_url("http://localhost:1/cb", CHALLENGE, "").is_err());
        assert!(authorize_url("http://localhost:1/cb", CHALLENGE, "a b").is_err());
    }

    #[tokio::test]
    async fn exchange_posts_the_documented_body_and_returns_the_key() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v1/auth/keys"))
            .and(header("content-type", "application/json"))
            .and(body_json(json!({
                "code": "synthetic-code",
                "code_verifier": "synthetic-verifier",
                "code_challenge_method": "S256"
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "key": "sk-or-synthetic-key",
                "user_id": "user_synthetic"
            })))
            .expect(1)
            .mount(&server)
            .await;
        let key = exchange_code(
            &http(),
            &format!("{}/api/v1/auth/keys", server.uri()),
            "synthetic-code",
            "synthetic-verifier",
        )
        .await
        .unwrap();
        assert_eq!(key, "sk-or-synthetic-key");
    }

    #[tokio::test]
    async fn exchange_failures_report_status_only_and_never_echo_secrets() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(
                ResponseTemplate::new(403)
                    .set_body_string("bad code synthetic-code verifier synthetic-verifier"),
            )
            .mount(&server)
            .await;
        let url = format!("{}/api/v1/auth/keys", server.uri());
        let error = exchange_code(&http(), &url, "synthetic-code", "synthetic-verifier")
            .await
            .unwrap_err()
            .to_string();
        assert!(error.contains("403"), "{error}");
        assert!(!error.contains("synthetic-code"), "{error}");
        assert!(!error.contains("synthetic-verifier"), "{error}");

        let odd = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"key": ""})))
            .mount(&odd)
            .await;
        assert!(
            exchange_code(
                &http(),
                &format!("{}/api/v1/auth/keys", odd.uri()),
                "synthetic-code",
                "synthetic-verifier"
            )
            .await
            .is_err()
        );
        // Control characters in the code never reach the network.
        assert!(
            exchange_code(&http(), &url, "bad\ncode", "synthetic-verifier")
                .await
                .is_err()
        );
    }
}
