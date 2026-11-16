//! PKCE (RFC 7636) for provider sign-in: the verifier and challenge, the
//! loopback redirect rules, the authorization URL and the code exchange.
//!
//! Token and authorization endpoints are pinned by host and path. Redirects
//! are never followed and response bodies are capped at 64 KiB. Codes,
//! verifiers and tokens never appear in an error message.

use base64::Engine;
use rand::RngCore;
use sha2::{Digest, Sha256};

use crate::error::{Result, SeraError};

use super::{PinnedEndpoint, TokenSet, read_capped};

/// A PKCE verifier and its S256 challenge.
pub fn pkce_s256() -> (String, String) {
    let mut bytes = [0u8; 32];
    rand::rngs::OsRng.fill_bytes(&mut bytes);
    let verifier = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes);
    let challenge = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .encode(Sha256::digest(verifier.as_bytes()));
    (verifier, challenge)
}

/// The shape of a verifier or challenge: 43 to 128 URL-safe characters.
pub fn pkce_token_ok(value: &str) -> bool {
    (43..=128).contains(&value.len())
        && value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'))
}

/// The shape of an OAuth `state` value: 16 to 128 URL-safe characters.
pub fn state_ok(value: &str) -> bool {
    (16..=128).contains(&value.len())
        && value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'))
}

/// A fresh random `state`.
pub fn random_state() -> String {
    let mut bytes = [0u8; 16];
    rand::rngs::OsRng.fill_bytes(&mut bytes);
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

/// Where a provider lets the browser return: an exact loopback address.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RedirectSpec {
    /// The one port the provider's app registration allows, or `None` when it
    /// accepts any unprivileged port.
    pub port: Option<u16>,
    /// The one callback path, or `None` when any path is accepted.
    pub path: Option<&'static str>,
}

/// Check `redirect_uri` against the provider's loopback rules: `http`, host
/// `127.0.0.1` or `localhost`, the provider's port and path, and no userinfo,
/// query or fragment.
pub fn validate_loopback_redirect(spec: &RedirectSpec, redirect_uri: &str) -> Result<()> {
    let refuse = || SeraError::Rejected {
        code: "bad_redirect",
        message: "The sign-in redirect must be this provider's loopback address".into(),
    };
    let url = url::Url::parse(redirect_uri).map_err(|_| refuse())?;
    let host_ok = matches!(url.host_str(), Some("127.0.0.1" | "localhost"));
    let port_ok = match (url.port(), spec.port) {
        (Some(actual), Some(expected)) => actual == expected,
        (Some(actual), None) => actual >= 1024,
        (None, _) => false,
    };
    let path_ok = spec.path.is_none_or(|expected| url.path() == expected);
    if url.scheme() != "http"
        || !host_ok
        || !port_ok
        || !path_ok
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || redirect_uri.len() > 256
    {
        return Err(refuse());
    }
    Ok(())
}

/// A standard OAuth 2.0 authorization-code client with PKCE.
#[derive(Debug, Clone)]
pub struct PkceSpec {
    pub authorize: PinnedEndpoint,
    pub token: PinnedEndpoint,
    pub client_id: String,
    /// Sent only to the token endpoint, for providers that require it.
    pub client_secret: Option<String>,
    /// Space-separated scopes; empty sends none.
    pub scope: String,
    pub redirect: RedirectSpec,
    /// Extra authorization parameters (for example `access_type=offline`).
    pub extra_params: Vec<(String, String)>,
}

impl PkceSpec {
    /// The authorization page the browser opens.
    pub fn authorize_url(
        &self,
        redirect_uri: &str,
        challenge: &str,
        state: &str,
    ) -> Result<String> {
        validate_loopback_redirect(&self.redirect, redirect_uri)?;
        if !pkce_token_ok(challenge) || !state_ok(state) {
            return Err(SeraError::Rejected {
                code: "bad_challenge",
                message: "The sign-in challenge or state is not valid".into(),
            });
        }
        let mut url = self.authorize.checked_url()?;
        {
            let mut query = url.query_pairs_mut();
            query
                .append_pair("client_id", &self.client_id)
                .append_pair("response_type", "code")
                .append_pair("redirect_uri", redirect_uri)
                .append_pair("code_challenge", challenge)
                .append_pair("code_challenge_method", "S256")
                .append_pair("state", state);
            if !self.scope.is_empty() {
                query.append_pair("scope", &self.scope);
            }
            for (key, value) in &self.extra_params {
                query.append_pair(key, value);
            }
        }
        Ok(url.to_string())
    }

    /// Exchange a one-time code for tokens.
    pub async fn exchange(
        &self,
        http: &reqwest::Client,
        code: &str,
        redirect_uri: &str,
        verifier: &str,
    ) -> Result<TokenSet> {
        validate_loopback_redirect(&self.redirect, redirect_uri)?;
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
        let mut form = vec![
            ("grant_type", "authorization_code"),
            ("code", code),
            ("redirect_uri", redirect_uri),
            ("client_id", self.client_id.as_str()),
            ("code_verifier", verifier),
        ];
        if let Some(secret) = self.client_secret.as_deref() {
            form.push(("client_secret", secret));
        }
        let url = self.token.checked_url()?;
        let response = http
            .post(url)
            .header("accept", "application/json")
            .form(&form)
            .send()
            .await
            .map_err(|_| SeraError::External("the sign-in provider could not be reached".into()))?;
        let status = response.status();
        let body = read_capped(response).await?;
        if !status.is_success() {
            return Err(SeraError::Rejected {
                code: "signin_refused",
                message: "The provider refused the sign-in code. Start the sign-in again.".into(),
            });
        }
        TokenSet::from_body(&body)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn challenge_is_s256_of_the_verifier() {
        let (verifier, challenge) = pkce_s256();
        assert!(pkce_token_ok(&verifier) && pkce_token_ok(&challenge));
        let expected = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(Sha256::digest(verifier.as_bytes()));
        assert_eq!(challenge, expected);
        assert_ne!(pkce_s256().0, verifier);
        // RFC 7636 appendix B.
        let rfc = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(Sha256::digest(
            b"dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk",
        ));
        assert_eq!(rfc, "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM");
    }

    #[test]
    fn loopback_redirects_are_exact() {
        let fixed = RedirectSpec {
            port: Some(51121),
            path: Some("/oauth-callback"),
        };
        assert!(
            validate_loopback_redirect(&fixed, "http://127.0.0.1:51121/oauth-callback").is_ok()
        );
        assert!(
            validate_loopback_redirect(&fixed, "http://localhost:51121/oauth-callback").is_ok()
        );
        for bad in [
            "http://127.0.0.1:51122/oauth-callback",
            "http://127.0.0.1:51121/other",
            "http://127.0.0.1/oauth-callback",
            "https://127.0.0.1:51121/oauth-callback",
            "http://evil.example:51121/oauth-callback",
            "http://127.0.0.1.evil.example:51121/oauth-callback",
            "http://user@127.0.0.1:51121/oauth-callback",
            "http://127.0.0.1:51121/oauth-callback?x=1",
            "http://127.0.0.1:51121/oauth-callback#f",
            "http://[::1]:51121/oauth-callback",
            "javascript:alert(1)",
            "",
        ] {
            assert!(validate_loopback_redirect(&fixed, bad).is_err(), "{bad}");
        }
        let any = RedirectSpec {
            port: None,
            path: None,
        };
        assert!(validate_loopback_redirect(&any, "http://localhost:51423/callback").is_ok());
        assert!(validate_loopback_redirect(&any, "http://localhost:80/callback").is_err());
    }
}
