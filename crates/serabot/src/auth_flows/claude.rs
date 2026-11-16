//! Claude plan sign-in (admin only, on by default, `plan_oauth = false` turns it off).
//!
//! This is the claude.ai OAuth flow that Claude Code uses: PKCE through a fixed
//! loopback port on the signer's own desktop (or a pasted code), a JSON token
//! exchange, and a rotating refresh token. It mirrors the owner's aio-proxy
//! `claude-code` plugin (the Claude client HielCowork's tooling uses): client
//! id, scopes, endpoints and headers are the same. Anthropic's terms do not let
//! a third-party product offer plan sign-in, and both the owner's HielCowork
//! audit and openbot's notes rate a multi-user server as the risky shape. So,
//! by the owner's decision:
//!
//! - it is on unless a provider sets `plan_oauth = false`, the API lets only
//!   the admin start it, and the page shows a risk banner beside it;
//! - the OAuth client id is Claude Code's public installed-app client id,
//!   compiled in as a default; an age-store value
//!   (`oauth_client_id_secret`) overrides it;
//! - the account lives in the admin's own account list and is never shared
//!   through a provider grant;
//! - a login that does not say a stable account id is refused, so signing in
//!   twice never makes two accounts (the fingerprint is
//!   `sha256("claude:account:<uuid>")`);
//! - no `claude-code` user agent is imitated (the refresh and chat requests
//!   carry the SDK user agent aio-proxy uses) and no CLI credential file is
//!   read.
//!
//! The refresh token rotates on every refresh, so every refresh goes through the
//! single-flight account refresher.
//!
//! Tokens, codes and verifiers never appear in an error or an event.

use async_trait::async_trait;
use serde_json::{Value, json};

use crate::accounts::{AuthKind, RefreshError, RefreshGrant, identity_fingerprint};
use crate::error::{Result, SeraError};
use crate::secrets::SharedSecrets;
use crate::user_secrets::ProviderCredential;

use super::pkce::{RedirectSpec, pkce_token_ok, state_ok, validate_loopback_redirect};
use super::{FlowKind, PinnedEndpoint, SignInProvider, SignedIn, TokenSet, read_capped};

/// The token host and path (pinned).
pub const TOKEN_HOST: &str = "api.anthropic.com";
pub const TOKEN_PATH: &str = "/v1/oauth/token";
pub const TOKEN_URL: &str = "https://api.anthropic.com/v1/oauth/token";
const AUTHORIZE_HOST: &str = "claude.ai";
const AUTHORIZE_PATH: &str = "/oauth/authorize";
/// Scopes Claude Code asks for (from the owner's aio-proxy client).
pub const SCOPE: &str = "org:create_api_key user:profile user:inference user:sessions:claude_code user:mcp_servers user:file_upload";
/// The beta header a plan token needs, here only on refresh.
const OAUTH_BETA: &str = crate::wire::anthropic::OAUTH_BETA;
/// Claude Code's public installed-app OAuth client id (an identifier, not a
/// secret: it appears in every authorize URL). The owner's aio-proxy
/// `claude-code` plugin ships the same value. An age-store value overrides it.
pub const DEFAULT_CLIENT_ID: &str = "9d1c250a-e61b-44d9-88ed-5944d1962f5e";
/// The user agent aio-proxy sends on refresh and on plan chat requests.
pub const PLAN_USER_AGENT: &str = "anthropic-sdk-typescript/0.112.1 userOAuthProvider";
/// The fixed loopback port and path Claude's client registration allows.
pub const CALLBACK_PORT: u16 = 54545;
pub const CALLBACK_PATH: &str = "/callback";
/// Longest account label kept.
const MAX_LABEL_BYTES: usize = 120;

fn redirect_spec() -> RedirectSpec {
    RedirectSpec {
        port: Some(CALLBACK_PORT),
        path: Some(CALLBACK_PATH),
    }
}

/// The OAuth client id: the age-store override when one is stored, else the
/// compiled-in public client id.
pub fn client_id_for(secrets: &SharedSecrets, override_name: &str) -> String {
    Some(override_name.trim())
        .filter(|name| !name.is_empty())
        .and_then(|name| secrets.get(name))
        .map(|id| id.trim().to_owned())
        .filter(|id| !id.is_empty() && id.len() <= 128 && !id.chars().any(char::is_control))
        .unwrap_or_else(|| DEFAULT_CLIENT_ID.to_owned())
}

/// The code a member pasted or the redirect carried: `code`, `code#state`, or
/// the whole callback URL. Returns the code and the state, when it came with
/// one.
pub fn split_pasted_code(input: &str) -> (String, Option<String>) {
    let input = input.trim();
    if (input.starts_with("http://") || input.starts_with("https://"))
        && let Ok(url) = url::Url::parse(input)
    {
        let mut code = None;
        let mut state = None;
        for (key, value) in url.query_pairs() {
            match key.as_ref() {
                "code" => code = Some(value.into_owned()),
                "state" => state = Some(value.into_owned()),
                _ => {}
            }
        }
        if let Some(code) = code {
            return (code, state);
        }
    }
    match input.split_once('#') {
        Some((code, state)) => (code.trim().to_owned(), Some(state.trim().to_owned())),
        None => (input.to_owned(), None),
    }
}

/// The Claude plan sign-in, registered under the `anthropic` kind.
pub struct ClaudeSignIn {
    secrets: SharedSecrets,
    client_id_name: String,
    authorize: PinnedEndpoint,
    token: PinnedEndpoint,
}

impl ClaudeSignIn {
    pub fn new(secrets: SharedSecrets, client_id_name: &str) -> Self {
        Self {
            secrets,
            client_id_name: client_id_name.trim().to_owned(),
            authorize: PinnedEndpoint::https(AUTHORIZE_HOST, AUTHORIZE_PATH),
            token: PinnedEndpoint::https(TOKEN_HOST, TOKEN_PATH),
        }
    }

    /// The sign-in for this server's configuration. It is always registered so
    /// the Providers page can offer it before an `anthropic` provider is saved;
    /// each provider's `plan_oauth` flag (checked per request) is what turns it
    /// off. The client-id override comes from the first provider that has the
    /// flag on.
    pub fn from_config(config: &crate::config::Config, secrets: SharedSecrets) -> Self {
        let name = config
            .llm
            .providers
            .values()
            .find(|profile| profile.plan_oauth_enabled() && profile.is_anthropic())
            .map(|profile| profile.oauth_client_id_secret.as_str())
            .unwrap_or("");
        Self::new(secrets, name)
    }

    /// Point every endpoint at a mock server on the exact loopback.
    #[cfg(test)]
    pub(crate) fn for_test(secrets: SharedSecrets, client_id_name: &str, base: &str) -> Self {
        Self {
            secrets,
            client_id_name: client_id_name.to_owned(),
            authorize: PinnedEndpoint::loopback(&format!("{base}/authorize")),
            token: PinnedEndpoint::loopback(&format!("{base}/token")),
        }
    }

    fn client_id(&self) -> Result<String> {
        Ok(client_id_for(&self.secrets, &self.client_id_name))
    }
}

fn email_ok(email: &str) -> bool {
    email.len() <= 254
        && email.contains('@')
        && !email.chars().any(|c| c.is_control() || c.is_whitespace())
}

/// Who a token reply says signed in. The account uuid is the stable id.
struct Who {
    uuid: String,
    email: Option<String>,
    organization: Option<String>,
}

fn who(body: &Value) -> Option<Who> {
    let uuid = body
        .pointer("/account/uuid")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|id| {
            (1..=64).contains(&id.len())
                && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
        })?
        .to_owned();
    let email = body
        .pointer("/account/email_address")
        .and_then(Value::as_str)
        .map(|email| email.trim().to_lowercase())
        .filter(|email| email_ok(email));
    let organization = body
        .pointer("/organization/name")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|name| !name.is_empty() && name.len() <= MAX_LABEL_BYTES)
        .filter(|name| !name.chars().any(char::is_control))
        .map(str::to_owned);
    Some(Who {
        uuid,
        email,
        organization,
    })
}

#[async_trait]
impl SignInProvider for ClaudeSignIn {
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
        let client_id = self.client_id()?;
        let mut url = self.authorize.checked_url()?;
        url.query_pairs_mut()
            .append_pair("client_id", &client_id)
            .append_pair("code", "true")
            .append_pair("code_challenge", challenge)
            .append_pair("code_challenge_method", "S256")
            .append_pair("redirect_uri", redirect_uri)
            .append_pair("response_type", "code")
            .append_pair("scope", SCOPE)
            .append_pair("state", state);
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
        // A pasted state must be the one this flow started with.
        if pasted_state
            .as_deref()
            .is_some_and(|pasted| !state.is_empty() && pasted != state)
        {
            return Err(SeraError::Rejected {
                code: "bad_code",
                message: "The sign-in code does not belong to this sign-in".into(),
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
        let client_id = self.client_id()?;
        let mut body = json!({
            "grant_type": "authorization_code",
            "code": code,
            "redirect_uri": redirect_uri,
            "client_id": client_id,
            "code_verifier": verifier,
        });
        if !state.is_empty() {
            body["state"] = json!(state);
        }
        let response = http
            .post(self.token.checked_url()?)
            .header("accept", "application/json")
            .json(&body)
            .send()
            .await
            .map_err(|_| SeraError::External("the sign-in provider could not be reached".into()))?;
        let status = response.status();
        let bytes = read_capped(response).await?;
        if !status.is_success() {
            return Err(SeraError::Rejected {
                code: "signin_refused",
                message: "The provider refused the sign-in code. Start the sign-in again.".into(),
            });
        }
        let tokens = TokenSet::from_body(&bytes)?;
        // The refresh token rotates; without one the account would stop
        // working within hours.
        if tokens.refresh_token.is_none() {
            return Err(SeraError::Rejected {
                code: "no_refresh_token",
                message:
                    "The provider did not return a long-lived sign-in. Start the sign-in again."
                        .into(),
            });
        }
        let value: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
        // No stable account id, no account: signing in twice must not make two.
        let who = who(&value).ok_or_else(|| SeraError::Rejected {
            code: "no_stable_identity",
            message: "Claude did not say who signed in, so the account was not saved".into(),
        })?;
        let label = who
            .email
            .clone()
            .or_else(|| who.organization.clone())
            .unwrap_or_else(|| "Claude plan".into());
        let mut identity = json!({"plan": "Claude plan"});
        if let Some(email) = &who.email {
            identity["email"] = json!(email);
        }
        if let Some(org) = &who.organization {
            identity["org"] = json!(org);
        }
        Ok(SignedIn {
            label,
            auth_kind: AuthKind::Oauth,
            fingerprint: Some(identity_fingerprint(
                "claude",
                &format!("account:{}", who.uuid),
            )),
            identity,
            credential: ProviderCredential {
                expires_at: tokens.expires_at(),
                api_key: tokens.access_token,
                refresh_token: tokens.refresh_token.unwrap_or_default(),
                generation: None,
            },
        })
    }
}

/// One refresh of a Claude plan token. The refresh token rotates: the reply
/// carries the next one. A refused token (`invalid_grant`, 401, 403 or an
/// unknown client) is terminal; anything else leaves the credential untouched.
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
        .header("anthropic-beta", OAUTH_BETA)
        .header("user-agent", PLAN_USER_AGENT)
        .json(&json!({
            "grant_type": "refresh_token",
            "client_id": client_id,
            "refresh_token": refresh_token,
        }))
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
            .and_then(|body| body.get("error").and_then(Value::as_str).map(str::to_owned))
            .unwrap_or_default();
        let refused = matches!(status.as_u16(), 401 | 403)
            || matches!(code.as_str(), "invalid_grant" | "invalid_client");
        if refused {
            return Err(RefreshError::InvalidGrant);
        }
        return Err(RefreshError::Failed(SeraError::External(format!(
            "oauth token refresh failed with status {}",
            status.as_u16()
        ))));
    }
    let tokens = TokenSet::from_body(&bytes).map_err(|_| failed("oauth token refresh failed"))?;
    Ok(RefreshGrant {
        expires_at: tokens.expires_at(),
        access_token: tokens.access_token,
        refresh_token: tokens.refresh_token,
    })
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::sync::Arc;

    use sha2::{Digest, Sha256};
    use wiremock::matchers::{body_partial_json, header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use super::*;
    use crate::accounts::Accounts;
    use crate::auth_flows::{CompleteRequest, SignInRegistry, SignInService, StartRequest};
    use crate::secrets::SecretStore;
    use crate::store::Store;
    use crate::user_secrets::UserSecrets;

    const REDIRECT: &str = "http://localhost:54545/callback";
    const CLIENT_NAME: &str = "claude_oauth_client_id";
    const CLIENT_ID: &str = "synthetic-claude-client-id";

    struct Harness {
        store: Store,
        accounts: Arc<Accounts>,
    }

    fn harness() -> Harness {
        let store = Store::open_in_memory(384).unwrap();
        let secrets = Arc::new(
            UserSecrets::with_master_key(store.clone(), [4u8; crate::user_secrets::KEY_BYTES])
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

    fn age_store(with_id: bool) -> SharedSecrets {
        let mut values = BTreeMap::new();
        if with_id {
            values.insert(CLIENT_NAME.to_owned(), CLIENT_ID.to_owned());
        }
        SharedSecrets::new(SecretStore::from_map(values))
    }

    fn service(harness: &Harness, server: &MockServer, with_id: bool) -> Arc<SignInService> {
        let mut registry = SignInRegistry::default();
        registry.register(
            "anthropic",
            Arc::new(ClaudeSignIn::for_test(
                age_store(with_id),
                CLIENT_NAME,
                &server.uri(),
            )),
        );
        Arc::new(
            SignInService::new(harness.accounts.clone())
                .unwrap()
                .with_registry(registry),
        )
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

    /// A token reply naming an account.
    fn token_reply(access: &str, refresh: Option<&str>, uuid: Option<&str>) -> ResponseTemplate {
        let mut body = json!({
            "access_token": access,
            "token_type": "Bearer",
            "expires_in": 28_800,
            "organization": {"uuid": "org-1", "name": "Synthetic Org"}
        });
        if let Some(refresh) = refresh {
            body["refresh_token"] = json!(refresh);
        }
        if let Some(uuid) = uuid {
            body["account"] = json!({"uuid": uuid, "email_address": "Plan@Example.Example"});
        }
        ResponseTemplate::new(200).set_body_json(body)
    }

    async fn mount_token(server: &MockServer, code: &str, reply: ResponseTemplate) {
        Mock::given(method("POST"))
            .and(path("/token"))
            .and(body_partial_json(json!({"code": code})))
            .respond_with(reply)
            .mount(server)
            .await;
    }

    #[test]
    fn the_account_fingerprint_is_sha256_of_claude_account_uuid() {
        assert_eq!(
            identity_fingerprint("claude", "account:uuid-1"),
            hex::encode(Sha256::digest(b"claude:account:uuid-1"))
        );
    }

    #[tokio::test]
    async fn the_authorization_url_follows_the_claude_client() {
        let harness = harness();
        let alice = member(&harness, "microsoft:claude-url");
        let server = MockServer::start().await;
        let service = service(&harness, &server, true);
        let started = service
            .start(&alice, "claude", "anthropic", start_request())
            .await
            .unwrap();
        let url = url::Url::parse(&started.url).unwrap();
        let query: BTreeMap<String, String> = url
            .query_pairs()
            .map(|(key, value)| (key.into_owned(), value.into_owned()))
            .collect();
        assert_eq!(query["client_id"], CLIENT_ID);
        assert_eq!(query["response_type"], "code");
        assert_eq!(query["code"], "true");
        assert_eq!(query["code_challenge_method"], "S256");
        assert_eq!(query["redirect_uri"], REDIRECT);
        assert_eq!(query["scope"], SCOPE);
        assert_eq!(query["state"], started.state.clone().unwrap());
        // The production endpoint is claude.ai; only fixtures use loopback.
        let real = ClaudeSignIn::new(age_store(true), CLIENT_NAME);
        let real_url = real
            .authorize_url(REDIRECT, &"c".repeat(43), &"s".repeat(16))
            .unwrap();
        assert!(real_url.starts_with("https://claude.ai/oauth/authorize?"));
    }

    #[tokio::test]
    async fn without_a_stored_client_id_the_public_default_is_used() {
        let harness = harness();
        let alice = member(&harness, "microsoft:claude-noid");
        let server = MockServer::start().await;
        let service = service(&harness, &server, false);
        let started = service
            .start(&alice, "claude", "anthropic", start_request())
            .await
            .unwrap();
        let url = url::Url::parse(&started.url).unwrap();
        let query: BTreeMap<String, String> = url
            .query_pairs()
            .map(|(key, value)| (key.into_owned(), value.into_owned()))
            .collect();
        assert_eq!(query["client_id"], DEFAULT_CLIENT_ID);
        // No name configured at all is the same answer, and a stored value wins.
        let unnamed = ClaudeSignIn::new(age_store(true), "");
        let url = unnamed
            .authorize_url(REDIRECT, &"c".repeat(43), &"s".repeat(16))
            .unwrap();
        assert!(url.contains(DEFAULT_CLIENT_ID));
        assert_eq!(client_id_for(&age_store(true), CLIENT_NAME), CLIENT_ID);
        assert_eq!(DEFAULT_CLIENT_ID, "9d1c250a-e61b-44d9-88ed-5944d1962f5e");
    }

    #[tokio::test]
    async fn the_code_is_exchanged_as_json_and_stored_sealed_with_a_stable_identity() {
        let harness = harness();
        let alice = member(&harness, "microsoft:claude-exchange");
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/token"))
            .and(body_partial_json(json!({
                "grant_type": "authorization_code",
                "code": "code-synthetic",
                "redirect_uri": REDIRECT,
                "client_id": CLIENT_ID,
            })))
            .respond_with(token_reply("access-1", Some("refresh-1"), Some("uuid-1")))
            .expect(1)
            .mount(&server)
            .await;
        let service = service(&harness, &server, true);
        let started = service
            .start(&alice, "claude", "anthropic", start_request())
            .await
            .unwrap();
        let outcome = service
            .complete(
                &alice,
                "claude",
                "anthropic",
                complete_request(&started.flow_id, "code-synthetic"),
            )
            .await
            .unwrap();
        assert!(outcome.created);
        let body: Value =
            serde_json::from_slice(&server.received_requests().await.unwrap()[0].body).unwrap();
        // The verifier and the flow's own state go to the token endpoint.
        assert!(body["code_verifier"].as_str().unwrap().len() >= 43);
        assert_eq!(body["state"], started.state.clone().unwrap());
        let account = harness
            .accounts
            .get(&alice, "claude", &outcome.account_id)
            .unwrap()
            .unwrap();
        assert_eq!(account.auth_kind, AuthKind::Oauth);
        assert_eq!(account.label, "plan@example.example");
        assert_eq!(account.identity["email"], "plan@example.example");
        assert_eq!(account.identity["org"], "Synthetic Org");
        let stored = harness
            .accounts
            .credential(&alice, "claude", &outcome.account_id)
            .unwrap()
            .unwrap();
        assert_eq!(stored.api_key, "access-1");
        assert_eq!(stored.refresh_token, "refresh-1");
        assert!(stored.expires_at.is_some());
        // No token, code or verifier in anything the account shows.
        let shown = serde_json::to_string(&account).unwrap();
        for secret in ["access-1", "refresh-1", "code-synthetic"] {
            assert!(!shown.contains(secret), "{secret}");
        }
    }

    #[tokio::test]
    async fn signing_in_twice_updates_the_same_account_and_two_accounts_stay_apart() {
        let harness = harness();
        let alice = member(&harness, "microsoft:claude-dupe");
        let server = MockServer::start().await;
        mount_token(
            &server,
            "c1",
            token_reply("access-a", Some("refresh-a"), Some("uuid-1")),
        )
        .await;
        mount_token(
            &server,
            "c2",
            token_reply("access-b", Some("refresh-b"), Some("uuid-1")),
        )
        .await;
        mount_token(
            &server,
            "c3",
            token_reply("access-c", Some("refresh-c"), Some("uuid-2")),
        )
        .await;
        let service = service(&harness, &server, true);
        let mut ids = Vec::new();
        for code in ["c1", "c2", "c3"] {
            let started = service
                .start(&alice, "claude", "anthropic", start_request())
                .await
                .unwrap();
            ids.push(
                service
                    .complete(
                        &alice,
                        "claude",
                        "anthropic",
                        complete_request(&started.flow_id, code),
                    )
                    .await
                    .unwrap(),
            );
        }
        assert!(ids[0].created);
        // The same Claude account again: one row, new tokens.
        assert!(!ids[1].created);
        assert_eq!(ids[0].account_id, ids[1].account_id);
        // A different Claude account is another row.
        assert!(ids[2].created);
        assert_ne!(ids[0].account_id, ids[2].account_id);
        assert_eq!(harness.accounts.list(&alice, "claude").unwrap().len(), 2);
        let first = harness
            .accounts
            .credential(&alice, "claude", &ids[0].account_id)
            .unwrap()
            .unwrap();
        assert_eq!(first.api_key, "access-b");
        assert_eq!(first.refresh_token, "refresh-b");
    }

    #[tokio::test]
    async fn a_login_with_no_stable_id_or_no_refresh_token_stores_nothing() {
        let harness = harness();
        let alice = member(&harness, "microsoft:claude-refuse");
        let server = MockServer::start().await;
        mount_token(&server, "no-id", token_reply("a", Some("r"), None)).await;
        mount_token(
            &server,
            "no-refresh",
            token_reply("a", None, Some("uuid-9")),
        )
        .await;
        let service = service(&harness, &server, true);
        for (code, expected) in [
            ("no-id", "no_stable_identity"),
            ("no-refresh", "no_refresh_token"),
        ] {
            let started = service
                .start(&alice, "claude", "anthropic", start_request())
                .await
                .unwrap();
            let error = service
                .complete(
                    &alice,
                    "claude",
                    "anthropic",
                    complete_request(&started.flow_id, code),
                )
                .await
                .unwrap_err();
            match error {
                SeraError::Rejected { code, .. } => assert_eq!(code, expected),
                other => panic!("{other:?}"),
            }
        }
        assert!(harness.accounts.list(&alice, "claude").unwrap().is_empty());
    }

    #[tokio::test]
    async fn a_pasted_code_must_carry_this_flows_state() {
        let harness = harness();
        let alice = member(&harness, "microsoft:claude-paste");
        let server = MockServer::start().await;
        mount_token(
            &server,
            "pasted",
            token_reply("access-p", Some("refresh-p"), Some("uuid-p")),
        )
        .await;
        let service = service(&harness, &server, true);
        let started = service
            .start(&alice, "claude", "anthropic", start_request())
            .await
            .unwrap();
        let state = started.state.clone().unwrap();
        // Another sign-in's state is refused before anything is sent.
        let wrong = service
            .complete(
                &alice,
                "claude",
                "anthropic",
                complete_request(&started.flow_id, "pasted#some-other-state-value"),
            )
            .await
            .unwrap_err();
        assert!(matches!(
            wrong,
            SeraError::Rejected {
                code: "bad_code",
                ..
            }
        ));
        assert!(server.received_requests().await.unwrap().is_empty());
        // That did not burn the flow? It did: a refused code is a used code.
        let again = service
            .complete(
                &alice,
                "claude",
                "anthropic",
                complete_request(&started.flow_id, &format!("pasted#{state}")),
            )
            .await;
        assert!(again.is_err());
        // A fresh flow takes `code#state` as the page shows it.
        let started = service
            .start(&alice, "claude", "anthropic", start_request())
            .await
            .unwrap();
        let state = started.state.clone().unwrap();
        let ok = service
            .complete(
                &alice,
                "claude",
                "anthropic",
                complete_request(&started.flow_id, &format!("pasted#{state}")),
            )
            .await
            .unwrap();
        assert!(ok.created);
    }

    #[tokio::test]
    async fn a_flow_belongs_to_its_member_and_is_single_use() {
        let harness = harness();
        let alice = member(&harness, "microsoft:claude-owner");
        let bob = member(&harness, "microsoft:claude-thief");
        let server = MockServer::start().await;
        mount_token(
            &server,
            "mine",
            token_reply("access-m", Some("refresh-m"), Some("uuid-m")),
        )
        .await;
        let service = service(&harness, &server, true);
        let started = service
            .start(&alice, "claude", "anthropic", start_request())
            .await
            .unwrap();
        // Bob finds no such flow, and nothing is sent upstream.
        let stolen = service
            .complete(
                &bob,
                "claude",
                "anthropic",
                complete_request(&started.flow_id, "mine"),
            )
            .await
            .unwrap_err();
        assert!(matches!(stolen, SeraError::NotFound(_)));
        assert!(server.received_requests().await.unwrap().is_empty());
        assert!(harness.accounts.list(&bob, "claude").unwrap().is_empty());
        service
            .complete(
                &alice,
                "claude",
                "anthropic",
                complete_request(&started.flow_id, "mine"),
            )
            .await
            .unwrap();
        let reused = service
            .complete(
                &alice,
                "claude",
                "anthropic",
                complete_request(&started.flow_id, "mine"),
            )
            .await
            .unwrap_err();
        assert!(matches!(
            reused,
            SeraError::Rejected {
                code: "flow_used",
                ..
            }
        ));
    }

    #[tokio::test]
    async fn only_the_fixed_loopback_redirect_is_accepted() {
        let harness = harness();
        let alice = member(&harness, "microsoft:claude-redirect");
        let server = MockServer::start().await;
        let service = service(&harness, &server, true);
        for bad in [
            "http://localhost:54546/callback",
            "http://localhost:54545/oauth-callback",
            "https://claude.ai/callback",
            "http://evil.example:54545/callback",
        ] {
            let error = service
                .start(
                    &alice,
                    "claude",
                    "anthropic",
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
        // No device flow exists for Claude.
        let device = service
            .start(
                &alice,
                "claude",
                "anthropic",
                StartRequest {
                    flow: Some(FlowKind::Device),
                    ..Default::default()
                },
            )
            .await
            .unwrap_err();
        assert!(matches!(
            device,
            SeraError::Rejected {
                code: "flow_unsupported",
                ..
            }
        ));
    }

    #[tokio::test]
    async fn a_refused_code_leaves_no_secret_in_the_error() {
        let harness = harness();
        let alice = member(&harness, "microsoft:claude-refused");
        let server = MockServer::start().await;
        mount_token(
            &server,
            "bad-code",
            ResponseTemplate::new(400).set_body_json(
                json!({"error": "invalid_grant", "error_description": "code-secret"}),
            ),
        )
        .await;
        let service = service(&harness, &server, true);
        let started = service
            .start(&alice, "claude", "anthropic", start_request())
            .await
            .unwrap();
        let error = service
            .complete(
                &alice,
                "claude",
                "anthropic",
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
        assert!(harness.accounts.list(&alice, "claude").unwrap().is_empty());
    }

    #[tokio::test]
    async fn refresh_rotates_and_a_refusal_is_terminal() {
        let server = MockServer::start().await;
        let http = crate::auth_flows::http_client().unwrap();
        Mock::given(method("POST"))
            .and(path("/ok"))
            .and(header("anthropic-beta", "oauth-2025-04-20"))
            .and(body_partial_json(json!({
                "grant_type": "refresh_token",
                "client_id": CLIENT_ID,
                "refresh_token": "old-refresh",
            })))
            .respond_with(token_reply("new-access", Some("new-refresh"), None))
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
        assert_eq!(grant.access_token, "new-access");
        assert_eq!(grant.refresh_token.as_deref(), Some("new-refresh"));
        assert!(grant.expires_at.is_some());
        for (status, body) in [
            (400, json!({"error": "invalid_grant"})),
            (401, json!({})),
            (403, json!({})),
            (400, json!({"error": "invalid_client"})),
        ] {
            Mock::given(method("POST"))
                .and(path(format!("/refused-{status}")))
                .respond_with(ResponseTemplate::new(status).set_body_json(body))
                .mount(&server)
                .await;
            let refused = refresh_exchange(
                &http,
                &format!("{}/refused-{status}", server.uri()),
                "old-refresh",
                CLIENT_ID,
            )
            .await;
            assert!(
                matches!(refused, Err(RefreshError::InvalidGrant)),
                "{status}"
            );
        }
        // A 5xx or a network failure leaves the credential alone.
        Mock::given(method("POST"))
            .and(path("/down"))
            .respond_with(ResponseTemplate::new(503))
            .mount(&server)
            .await;
        let down = refresh_exchange(&http, &format!("{}/down", server.uri()), "r", CLIENT_ID).await;
        assert!(matches!(down, Err(RefreshError::Failed(_))));
    }

    #[test]
    fn pasted_codes_come_in_three_shapes() {
        assert_eq!(split_pasted_code("abc"), ("abc".into(), None));
        assert_eq!(
            split_pasted_code("  abc#state-1 "),
            ("abc".into(), Some("state-1".into()))
        );
        assert_eq!(
            split_pasted_code("http://localhost:54545/callback?code=abc&state=s1"),
            ("abc".into(), Some("s1".into()))
        );
        // A URL with no code is treated as the text it is, and fails later.
        assert_eq!(
            split_pasted_code("http://localhost:54545/callback?error=access_denied").0,
            "http://localhost:54545/callback?error=access_denied"
        );
    }

    #[test]
    fn the_redirect_is_the_fixed_loopback() {
        let spec = redirect_spec();
        assert!(validate_loopback_redirect(&spec, "http://localhost:54545/callback").is_ok());
        assert!(validate_loopback_redirect(&spec, "http://127.0.0.1:54545/callback").is_ok());
        assert!(validate_loopback_redirect(&spec, "http://localhost:54546/callback").is_err());
        assert!(
            validate_loopback_redirect(&spec, "http://localhost:54545/oauth-callback").is_err()
        );
        assert!(validate_loopback_redirect(&spec, "https://claude.ai/callback").is_err());
    }
}
