//! Antigravity member sign-in: Google OAuth with PKCE on the member's own
//! loopback, then the account's email and its Cloud Code companion project.
//!
//! This follows the owner's HielCowork client exactly:
//!
//! - The scopes never include `openid` (Google sends a desktop PKCE flow with
//!   `openid` to a consent screen that hangs). `access_type=offline` and
//!   `prompt=consent` are what make Google return a refresh token.
//! - The redirect is `http://127.0.0.1:<port>/oauth-callback`; Google's desktop
//!   client accepts any loopback port, so the desktop may fall back from 51121.
//! - The OAuth client secret is the registration's, not the member's. It is
//!   Google's public installed-app credential, compiled in as a default (as in
//!   HielCowork); an age-store value or `AGY_CLIENT_SECRET` overrides it.
//! - The email comes from the userinfo endpoint (display and identity only).
//! - Project discovery runs `loadCodeAssist` and, when the account has no
//!   project, `onboardUser` up to three times three seconds apart, asking the
//!   daily host and then prod. The project and tier are stored in the account's
//!   identity. An account without a project is still saved; its envelope then
//!   omits `project` and the project is looked up again on first use.
//!
//! Tokens, codes and the client secret never appear in an error or an event.

use std::time::Duration;

use async_trait::async_trait;
use serde_json::{Value, json};

use crate::accounts::{AuthKind, identity_fingerprint};
use crate::cloud_code::{
    ANTIGRAVITY_SCOPES, Discovery, antigravity_client_secret, discover_project_with,
    discovery_origins,
};
use crate::error::{Result, SeraError};
use crate::secrets::SharedSecrets;
use crate::user_secrets::ProviderCredential;

use super::pkce::{PkceSpec, RedirectSpec};
use super::{FlowKind, PinnedEndpoint, SignInProvider, SignedIn, read_capped};

/// The age-store name of the OAuth client secret when the provider names none.
pub const DEFAULT_CLIENT_SECRET_NAME: &str = "antigravity_oauth_client_secret";
/// The callback path the Antigravity OAuth client registration allows.
pub const CALLBACK_PATH: &str = "/oauth-callback";
/// The fixed loopback port the desktop tries first.
pub const PREFERRED_PORT: u16 = 51121;
/// What a member sees when the server has no Google client secret to sign in
/// with. Names where the admin fixes it; carries no secret.
pub const MISSING_CLIENT_SECRET: &str = "Antigravity sign-in is not set up on this server yet: the Google client secret is missing. The admin can paste it on the AI providers page (Google Antigravity, Set up sign-in) or set AGY_CLIENT_SECRET on the server.";
/// How far apart `onboardUser` attempts are.
const ONBOARD_PAUSE: Duration = Duration::from_secs(3);
/// Longest email accepted from userinfo.
const MAX_EMAIL_BYTES: usize = 254;

/// The Antigravity sign-in, registered under the `google-antigravity` kind.
pub struct GoogleSignIn {
    secrets: SharedSecrets,
    client_secret_name: String,
    client_id_name: Option<String>,
    client_version: Option<String>,
    authorize: PinnedEndpoint,
    token: PinnedEndpoint,
    userinfo: PinnedEndpoint,
    /// Where project discovery asks, in order.
    origins: Vec<String>,
    pause: Duration,
}

impl GoogleSignIn {
    /// The sign-in against Google's hosts. `client_secret_name` and
    /// `client_id_name` are the age-store names the provider configures (the
    /// id falls back to the published Antigravity client id).
    pub fn new(
        secrets: SharedSecrets,
        client_secret_name: &str,
        client_id_name: Option<&str>,
        client_version: Option<&str>,
    ) -> Result<Self> {
        let origins = discovery_origins("https://daily-cloudcode-pa.googleapis.com")?;
        Ok(Self {
            secrets,
            client_secret_name: if client_secret_name.trim().is_empty() {
                DEFAULT_CLIENT_SECRET_NAME.to_owned()
            } else {
                client_secret_name.trim().to_owned()
            },
            client_id_name: client_id_name
                .map(str::trim)
                .filter(|name| !name.is_empty())
                .map(str::to_owned),
            client_version: client_version
                .map(str::trim)
                .filter(|version| !version.is_empty())
                .map(str::to_owned),
            authorize: PinnedEndpoint::https("accounts.google.com", "/o/oauth2/v2/auth"),
            token: PinnedEndpoint::https("oauth2.googleapis.com", "/token"),
            userinfo: PinnedEndpoint::https("www.googleapis.com", "/oauth2/v1/userinfo"),
            origins,
            pause: ONBOARD_PAUSE,
        })
    }

    /// The sign-in for a provider configuration.
    pub fn from_config(config: &crate::config::Config, secrets: SharedSecrets) -> Result<Self> {
        let default = config.llm.default_provider();
        let profile = std::iter::once(&default)
            .chain(config.llm.providers.values())
            .find(|profile| {
                crate::providers::canonical_kind(&profile.kind)
                    .is_ok_and(|kind| kind == "google-antigravity")
            });
        match profile {
            Some(profile) => Self::new(
                secrets,
                &profile.oauth_client_secret_secret,
                Some(&profile.oauth_client_id_secret),
                Some(&profile.client_version),
            ),
            None => Self::new(secrets, "", None, None),
        }
    }

    /// Point every endpoint at a mock server on the exact loopback.
    #[cfg(test)]
    pub(crate) fn for_test(secrets: SharedSecrets, base: &str) -> Self {
        Self {
            secrets,
            client_secret_name: DEFAULT_CLIENT_SECRET_NAME.to_owned(),
            client_id_name: None,
            client_version: None,
            authorize: PinnedEndpoint::loopback(&format!("{base}/authorize")),
            token: PinnedEndpoint::loopback(&format!("{base}/token")),
            userinfo: PinnedEndpoint::loopback(&format!("{base}/userinfo")),
            origins: vec![base.to_owned()],
            pause: Duration::from_millis(1),
        }
    }

    /// The OAuth client secret this sign-in would use (may be empty when the
    /// build has no default and the operator set none).
    fn client_secret(&self) -> String {
        antigravity_client_secret(&self.secrets, &self.client_secret_name)
    }

    /// The OAuth client id: the age-store value the provider names, then
    /// `AGY_CLIENT_ID`, then the published Antigravity id (refresh reads the
    /// same order, so an account keeps one client).
    fn client_id(&self) -> String {
        crate::cloud_code::antigravity_client_id(
            &self.secrets,
            self.client_id_name.as_deref().unwrap_or(""),
        )
    }

    /// The OAuth client. The secret is the registration's public installed-app
    /// credential: a console-saved value wins, then `AGY_CLIENT_SECRET`, then
    /// the compiled-in default, so sign-in never needs a setup step.
    fn spec(&self) -> Result<PkceSpec> {
        let secret = self.client_secret();
        if secret.trim().is_empty() {
            // Google refuses the code exchange without the registration's
            // secret. Say so before the member is sent to Google, not after.
            return Err(SeraError::Rejected {
                code: "signin_not_set_up",
                message: MISSING_CLIENT_SECRET.into(),
            });
        }
        Ok(PkceSpec {
            authorize: self.authorize.clone(),
            token: self.token.clone(),
            client_id: self.client_id(),
            client_secret: Some(secret),
            // No `openid`: see the module docs.
            scope: ANTIGRAVITY_SCOPES.to_owned(),
            redirect: RedirectSpec {
                port: None,
                path: Some(CALLBACK_PATH),
            },
            extra_params: vec![
                ("access_type".into(), "offline".into()),
                ("prompt".into(), "consent".into()),
            ],
        })
    }

    /// The signed-in account's email (and Google id), for display and identity.
    async fn userinfo(&self, http: &reqwest::Client, access_token: &str) -> Result<Userinfo> {
        let mut url = self.userinfo.checked_url()?;
        url.query_pairs_mut().append_pair("alt", "json");
        let refused = || SeraError::Rejected {
            code: "userinfo_failed",
            message: "Google did not say who signed in. Start the sign-in again.".into(),
        };
        let response = http
            .get(url)
            .header("accept", "application/json")
            .bearer_auth(access_token)
            .send()
            .await
            .map_err(|_| SeraError::External("the sign-in provider could not be reached".into()))?;
        let status = response.status();
        let body = read_capped(response).await?;
        if !status.is_success() {
            return Err(refused());
        }
        let value: Value = serde_json::from_slice(&body).map_err(|_| refused())?;
        let email = value
            .get("email")
            .and_then(Value::as_str)
            .map(|email| email.trim().to_lowercase())
            .filter(|email| {
                email.len() <= MAX_EMAIL_BYTES
                    && email.contains('@')
                    && !email.chars().any(|c| c.is_control() || c.is_whitespace())
            });
        let id = value
            .get("id")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|id| (1..=64).contains(&id.len()) && id.chars().all(|c| c.is_ascii_digit()))
            .map(str::to_owned);
        Ok(Userinfo { email, id })
    }
}

struct Userinfo {
    email: Option<String>,
    id: Option<String>,
}

/// The non-secret identity stored with a Google account.
pub fn google_identity(email: Option<&str>, discovery: &Discovery) -> Value {
    let mut identity = json!({});
    if let Some(email) = email {
        identity["email"] = json!(email);
    }
    if let Some(project) = &discovery.project {
        identity["project_id"] = json!(project);
    }
    if let Some(tier) = &discovery.tier {
        identity["tier"] = json!(tier);
    }
    identity
}

#[async_trait]
impl SignInProvider for GoogleSignIn {
    fn flows(&self) -> &'static [FlowKind] {
        &[FlowKind::Pkce]
    }

    fn redirect_spec(&self) -> Option<RedirectSpec> {
        Some(RedirectSpec {
            port: None,
            path: Some(CALLBACK_PATH),
        })
    }

    fn setup_missing(&self) -> Option<&'static str> {
        self.client_secret()
            .trim()
            .is_empty()
            .then_some("client_secret")
    }

    fn authorize_url(&self, redirect_uri: &str, challenge: &str, state: &str) -> Result<String> {
        self.spec()?.authorize_url(redirect_uri, challenge, state)
    }

    async fn exchange_pkce(
        &self,
        http: &reqwest::Client,
        code: &str,
        redirect_uri: &str,
        verifier: &str,
    ) -> Result<SignedIn> {
        let tokens = self
            .spec()?
            .exchange(http, code, redirect_uri, verifier)
            .await?;
        // `prompt=consent` is what returns a refresh token; without one the
        // account would stop working within the hour.
        let refresh_token = tokens
            .refresh_token
            .clone()
            .ok_or_else(|| SeraError::Rejected {
                code: "no_refresh_token",
                message: "Google did not return a long-lived sign-in. Start the sign-in again."
                    .into(),
            })?;
        let who = self.userinfo(http, &tokens.access_token).await?;
        let stable_id = who
            .email
            .clone()
            .or_else(|| who.id.clone())
            .ok_or_else(|| SeraError::Rejected {
                code: "no_stable_identity",
                message: "Google did not say who signed in, so the account was not saved".into(),
            })?;
        // The live Antigravity Hub version, as HielCowork presents it (the
        // provider's own `client_version` and `AGY_VERSION` win).
        let version = crate::cloud_code::antigravity_version(self.client_version.as_deref()).await;
        let discovery = discover_project_with(
            http,
            &tokens.access_token,
            &self.origins,
            self.pause,
            Some(&version),
        )
        .await;
        let label = who.email.clone().unwrap_or_else(|| "Google account".into());
        Ok(SignedIn {
            label,
            auth_kind: AuthKind::Oauth,
            fingerprint: Some(identity_fingerprint("google-antigravity", &stable_id)),
            identity: google_identity(who.email.as_deref(), &discovery),
            credential: ProviderCredential {
                expires_at: tokens.expires_at(),
                api_key: tokens.access_token,
                refresh_token,
                generation: None,
            },
        })
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::sync::Arc;

    use serde_json::json;
    use wiremock::matchers::{body_string_contains, header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use super::*;
    use crate::accounts::Accounts;
    use crate::auth_flows::{CompleteRequest, SignInRegistry, SignInService, StartRequest};
    use crate::cloud_code::{ANTIGRAVITY_CLIENT_ID, ANTIGRAVITY_DEFAULT_CLIENT_SECRET};
    use crate::secrets::SecretStore;
    use crate::store::Store;
    use crate::user_secrets::UserSecrets;

    const REDIRECT: &str = "http://127.0.0.1:51121/oauth-callback";
    const CLIENT_SECRET: &str = "synthetic-antigravity-client-secret";

    struct Harness {
        store: Store,
        accounts: Arc<Accounts>,
    }

    fn harness() -> Harness {
        let store = Store::open_in_memory(384).unwrap();
        let secrets = Arc::new(
            UserSecrets::with_master_key(store.clone(), [3u8; crate::user_secrets::KEY_BYTES])
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

    fn age_store(with_secret: bool) -> SharedSecrets {
        let mut values = BTreeMap::new();
        if with_secret {
            values.insert(
                DEFAULT_CLIENT_SECRET_NAME.to_owned(),
                CLIENT_SECRET.to_owned(),
            );
        }
        SharedSecrets::new(SecretStore::from_map(values))
    }

    fn service(harness: &Harness, server: &MockServer, with_secret: bool) -> Arc<SignInService> {
        let mut registry = SignInRegistry::default();
        registry.register(
            "google-antigravity",
            Arc::new(GoogleSignIn::for_test(
                age_store(with_secret),
                &server.uri(),
            )),
        );
        Arc::new(
            SignInService::new(harness.accounts.clone())
                .unwrap()
                .with_registry(registry),
        )
    }

    fn pkce_request(redirect: &str) -> StartRequest {
        StartRequest {
            flow: Some(FlowKind::Pkce),
            redirect_uri: Some(redirect.into()),
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

    /// One Google account on the mock: its code, tokens, email and project.
    async fn google_account(
        server: &MockServer,
        code: &str,
        email: &str,
        project: Option<&str>,
        refresh: bool,
    ) {
        let access = format!("access-for-{code}");
        let mut tokens = json!({"access_token": access, "expires_in": 3600});
        if refresh {
            tokens["refresh_token"] = json!(format!("refresh-for-{code}"));
        }
        Mock::given(method("POST"))
            .and(path("/token"))
            .and(body_string_contains(format!("code={code}")))
            .respond_with(ResponseTemplate::new(200).set_body_json(tokens))
            .mount(server)
            .await;
        Mock::given(method("GET"))
            .and(path("/userinfo"))
            .and(header("authorization", format!("Bearer {access}").as_str()))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(
                    json!({"email": email, "id": "1234567890", "name": "Synthetic"}),
                ),
            )
            .mount(server)
            .await;
        let bearer = format!("Bearer {access}");
        match project {
            Some(project) => {
                Mock::given(method("POST"))
                    .and(path("/v1internal:loadCodeAssist"))
                    .and(header("authorization", bearer.as_str()))
                    .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                        "cloudaicompanionProject": project,
                        "currentTier": {"id": "standard-tier"}
                    })))
                    .mount(server)
                    .await;
            }
            None => {
                Mock::given(method("POST"))
                    .and(path("/v1internal:loadCodeAssist"))
                    .and(header("authorization", bearer.as_str()))
                    .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                        "allowedTiers": [{"id": "free-tier", "isDefault": true}]
                    })))
                    .mount(server)
                    .await;
                Mock::given(method("POST"))
                    .and(path("/v1internal:onboardUser"))
                    .and(header("authorization", bearer.as_str()))
                    .respond_with(ResponseTemplate::new(200).set_body_json(json!({"done": false})))
                    .mount(server)
                    .await;
            }
        }
    }

    #[tokio::test]
    async fn the_authorization_url_follows_hielcowork() {
        let harness = harness();
        let alice = member(&harness, "microsoft:google-url");
        let server = MockServer::start().await;
        let service = service(&harness, &server, true);
        let started = service
            .start(
                &alice,
                "antigravity",
                "google-antigravity",
                pkce_request(REDIRECT),
            )
            .await
            .unwrap();
        let url = url::Url::parse(&started.url).unwrap();
        let query: BTreeMap<String, String> = url
            .query_pairs()
            .map(|(key, value)| (key.into_owned(), value.into_owned()))
            .collect();
        assert_eq!(query["client_id"], ANTIGRAVITY_CLIENT_ID);
        assert_eq!(query["response_type"], "code");
        assert_eq!(query["redirect_uri"], REDIRECT);
        assert_eq!(query["access_type"], "offline");
        assert_eq!(query["prompt"], "consent");
        assert_eq!(query["code_challenge_method"], "S256");
        // Never `openid`: Google hangs a desktop PKCE flow that asks for it.
        let scopes: Vec<&str> = query["scope"].split(' ').collect();
        assert!(!scopes.contains(&"openid"));
        assert!(scopes.contains(&"https://www.googleapis.com/auth/cloud-platform"));
        assert!(scopes.contains(&"https://www.googleapis.com/auth/userinfo.email"));
        // The client secret never reaches the browser.
        assert!(!started.url.contains(CLIENT_SECRET));
        assert!(!started.url.contains("client_secret"));
    }

    #[tokio::test]
    async fn the_redirect_is_the_loopback_callback_on_any_port() {
        let harness = harness();
        let alice = member(&harness, "microsoft:google-redirect");
        let server = MockServer::start().await;
        let service = service(&harness, &server, true);
        // The desktop may fall back from 51121 when it is busy.
        for ok in [
            "http://127.0.0.1:51121/oauth-callback",
            "http://127.0.0.1:53811/oauth-callback",
            "http://localhost:51121/oauth-callback",
        ] {
            assert!(
                service
                    .start(
                        &alice,
                        "antigravity",
                        "google-antigravity",
                        pkce_request(ok)
                    )
                    .await
                    .is_ok(),
                "{ok}"
            );
        }
        for bad in [
            "http://127.0.0.1:51121/other",
            "http://127.0.0.1/oauth-callback",
            "http://127.0.0.1:80/oauth-callback",
            "https://127.0.0.1:51121/oauth-callback",
            "http://evil.example:51121/oauth-callback",
        ] {
            let error = service
                .start(
                    &alice,
                    "antigravity",
                    "google-antigravity",
                    pkce_request(bad),
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
    }

    /// Whether this build or environment supplies a client secret without the
    /// age store (the compiled-in default or `AGY_CLIENT_SECRET`).
    fn built_in_secret() -> bool {
        let env = std::env::var(crate::cloud_code::ANTIGRAVITY_CLIENT_SECRET_ENV).ok();
        !crate::cloud_code::pick_antigravity_client_secret(None, env.as_deref())
            .trim()
            .is_empty()
    }

    #[tokio::test]
    async fn without_a_stored_client_secret_sign_in_starts_only_with_a_built_in_one() {
        let harness = harness();
        let alice = member(&harness, "microsoft:google-nosecret");
        let server = MockServer::start().await;
        let service = service(&harness, &server, false);
        let result = service
            .start(
                &alice,
                "antigravity",
                "google-antigravity",
                pkce_request(REDIRECT),
            )
            .await;
        if built_in_secret() {
            // The public default client secret is used server-side and never shown.
            let started = result.unwrap();
            assert!(
                ANTIGRAVITY_DEFAULT_CLIENT_SECRET.is_empty()
                    || !started.url.contains(ANTIGRAVITY_DEFAULT_CLIENT_SECRET)
            );
            assert!(!started.url.contains("client_secret"));
        } else {
            // No secret anywhere: refused before the member is sent to Google,
            // with words that say where the admin fixes it.
            match result {
                Err(SeraError::Rejected { code, message }) => {
                    assert_eq!(code, "signin_not_set_up");
                    assert!(message.contains("AI providers"), "{message}");
                }
                Err(other) => panic!("unexpected error: {other}"),
                Ok(_) => panic!("sign-in started without a client secret"),
            }
        }
    }

    #[test]
    fn a_missing_client_secret_is_reported_as_setup_and_a_stored_one_is_ready() {
        let stored = GoogleSignIn::for_test(age_store(true), "http://127.0.0.1:9");
        assert_eq!(stored.setup_missing(), None);
        let bare = GoogleSignIn::for_test(age_store(false), "http://127.0.0.1:9");
        let expected = (!built_in_secret()).then_some("client_secret");
        assert_eq!(bare.setup_missing(), expected);
    }

    #[test]
    fn the_client_secret_prefers_the_store_then_the_environment_then_the_default() {
        use crate::cloud_code::pick_antigravity_client_secret as pick;
        assert_eq!(pick(Some(" stored "), Some("env")), "stored");
        assert_eq!(pick(None, Some(" env ")), "env");
        assert_eq!(
            pick(Some("  "), Some("")),
            ANTIGRAVITY_DEFAULT_CLIENT_SECRET
        );
        assert_eq!(pick(None, None), ANTIGRAVITY_DEFAULT_CLIENT_SECRET);
    }

    #[tokio::test]
    async fn two_google_accounts_get_two_projects_and_a_repeat_updates_in_place() {
        let harness = harness();
        let alice = member(&harness, "microsoft:google-two");
        let server = MockServer::start().await;
        google_account(
            &server,
            "code-a",
            "Alpha@Example.Example",
            Some("proj-alpha"),
            true,
        )
        .await;
        google_account(
            &server,
            "code-b",
            "beta@example.example",
            Some("proj-beta"),
            true,
        )
        .await;
        let service = service(&harness, &server, true);

        let mut ids = Vec::new();
        for code in ["code-a", "code-b"] {
            let started = service
                .start(
                    &alice,
                    "antigravity",
                    "google-antigravity",
                    pkce_request(REDIRECT),
                )
                .await
                .unwrap();
            let outcome = service
                .complete(
                    &alice,
                    "antigravity",
                    "google-antigravity",
                    complete_request(&started.flow_id, code),
                )
                .await
                .unwrap();
            assert!(outcome.created);
            ids.push(outcome.account_id);
        }
        assert_ne!(ids[0], ids[1]);
        let listed = harness.accounts.list(&alice, "antigravity").unwrap();
        assert_eq!(listed.len(), 2);
        let project = |id: &str| {
            harness
                .accounts
                .get(&alice, "antigravity", id)
                .unwrap()
                .unwrap()
                .identity
        };
        let (a, b) = (project(&ids[0]), project(&ids[1]));
        assert_eq!(a["project_id"], "proj-alpha");
        assert_eq!(b["project_id"], "proj-beta");
        assert_eq!(a["tier"], "standard-tier");
        // The email is lower-cased and is the account's label.
        assert_eq!(a["email"], "alpha@example.example");
        assert_eq!(
            harness
                .accounts
                .get(&alice, "antigravity", &ids[0])
                .unwrap()
                .unwrap()
                .label,
            "alpha@example.example"
        );
        // The sealed credential carries the access and refresh tokens.
        let credential = harness
            .accounts
            .credential(&alice, "antigravity", &ids[0])
            .unwrap()
            .unwrap();
        assert_eq!(credential.api_key, "access-for-code-a");
        assert_eq!(credential.refresh_token, "refresh-for-code-a");
        assert!(credential.expires_at.is_some());

        // The same email signing in again refreshes that account in place.
        let started = service
            .start(
                &alice,
                "antigravity",
                "google-antigravity",
                pkce_request(REDIRECT),
            )
            .await
            .unwrap();
        let again = service
            .complete(
                &alice,
                "antigravity",
                "google-antigravity",
                complete_request(&started.flow_id, "code-a"),
            )
            .await
            .unwrap();
        assert!(!again.created);
        assert_eq!(again.account_id, ids[0]);
        assert_eq!(
            harness.accounts.list(&alice, "antigravity").unwrap().len(),
            2
        );

        // Another member sees none of it.
        let bob = member(&harness, "microsoft:google-two-bob");
        assert!(
            harness
                .accounts
                .list(&bob, "antigravity")
                .unwrap()
                .is_empty()
        );
        let started = service
            .start(
                &alice,
                "antigravity",
                "google-antigravity",
                pkce_request(REDIRECT),
            )
            .await
            .unwrap();
        assert!(matches!(
            service
                .complete(
                    &bob,
                    "antigravity",
                    "google-antigravity",
                    complete_request(&started.flow_id, "code-b"),
                )
                .await,
            Err(SeraError::NotFound(_))
        ));
    }

    #[tokio::test]
    async fn an_account_without_a_project_is_saved_and_omits_project() {
        let harness = harness();
        let alice = member(&harness, "microsoft:google-noproject");
        let server = MockServer::start().await;
        google_account(&server, "code-n", "none@example.example", None, true).await;
        let service = service(&harness, &server, true);
        let started = service
            .start(
                &alice,
                "antigravity",
                "google-antigravity",
                pkce_request(REDIRECT),
            )
            .await
            .unwrap();
        let outcome = service
            .complete(
                &alice,
                "antigravity",
                "google-antigravity",
                complete_request(&started.flow_id, "code-n"),
            )
            .await
            .unwrap();
        let account = harness
            .accounts
            .get(&alice, "antigravity", &outcome.account_id)
            .unwrap()
            .unwrap();
        assert!(account.identity.get("project_id").is_none());
        // Discovery tried onboarding up to three times.
        let onboards = server
            .received_requests()
            .await
            .unwrap()
            .iter()
            .filter(|request| request.url.path() == "/v1internal:onboardUser")
            .count();
        assert_eq!(onboards, 3);
        // With no project the envelope has no `project` (an empty one is a 404).
        let project = account
            .identity
            .get("project_id")
            .and_then(Value::as_str)
            .unwrap_or("");
        let envelope = crate::cloud_code::build_cloud_code_envelope(
            "gemini-3.8-flash",
            project,
            &[crate::domain::LlmMessage::user("hi")],
            &[],
            100,
            None,
        )
        .unwrap();
        assert!(envelope.get("project").is_none());
    }

    #[tokio::test]
    async fn a_login_without_a_refresh_token_or_an_identity_stores_nothing() {
        let harness = harness();
        let alice = member(&harness, "microsoft:google-refused");
        let server = MockServer::start().await;
        // No refresh token.
        google_account(
            &server,
            "code-r",
            "r@example.example",
            Some("proj-r"),
            false,
        )
        .await;
        // A token Google cannot name an owner for.
        Mock::given(method("POST"))
            .and(path("/token"))
            .and(body_string_contains("code=code-u"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(
                    json!({"access_token": "access-u", "refresh_token": "refresh-u"}),
                ),
            )
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/userinfo"))
            .and(header("authorization", "Bearer access-u"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"name": "Nobody"})))
            .mount(&server)
            .await;
        let service = service(&harness, &server, true);
        for (code, expected) in [
            ("code-r", "no_refresh_token"),
            ("code-u", "no_stable_identity"),
        ] {
            let started = service
                .start(
                    &alice,
                    "antigravity",
                    "google-antigravity",
                    pkce_request(REDIRECT),
                )
                .await
                .unwrap();
            let error = service
                .complete(
                    &alice,
                    "antigravity",
                    "google-antigravity",
                    complete_request(&started.flow_id, code),
                )
                .await
                .unwrap_err();
            assert!(
                matches!(&error, SeraError::Rejected { code, .. } if *code == expected),
                "{expected}: {error}"
            );
        }
        assert!(
            harness
                .accounts
                .list(&alice, "antigravity")
                .unwrap()
                .is_empty()
        );
    }

    #[tokio::test]
    async fn the_token_request_carries_the_client_secret_and_the_verifier_only_to_google() {
        let harness = harness();
        let alice = member(&harness, "microsoft:google-secret");
        let server = MockServer::start().await;
        google_account(&server, "code-s", "s@example.example", Some("proj-s"), true).await;
        let service = service(&harness, &server, true);
        let started = service
            .start(
                &alice,
                "antigravity",
                "google-antigravity",
                pkce_request(REDIRECT),
            )
            .await
            .unwrap();
        service
            .complete(
                &alice,
                "antigravity",
                "google-antigravity",
                complete_request(&started.flow_id, "code-s"),
            )
            .await
            .unwrap();
        let received = server.received_requests().await.unwrap();
        let token = received
            .iter()
            .find(|request| request.url.path() == "/token")
            .unwrap();
        let form: BTreeMap<String, String> = url::form_urlencoded::parse(&token.body)
            .map(|(key, value)| (key.into_owned(), value.into_owned()))
            .collect();
        assert_eq!(form["client_secret"], CLIENT_SECRET);
        assert_eq!(form["client_id"], ANTIGRAVITY_CLIENT_ID);
        assert_eq!(form["grant_type"], "authorization_code");
        assert!(form.contains_key("code_verifier"));
        // Userinfo is asked for JSON, as HielCowork does.
        let userinfo = received
            .iter()
            .find(|request| request.url.path() == "/userinfo")
            .unwrap();
        assert_eq!(userinfo.url.query(), Some("alt=json"));
        // And the discovery call identifies as Antigravity.
        let discovery = received
            .iter()
            .find(|request| request.url.path() == "/v1internal:loadCodeAssist")
            .unwrap();
        assert_eq!(
            discovery
                .headers
                .get("user-agent")
                .unwrap()
                .to_str()
                .unwrap(),
            crate::cloud_code::CLOUD_CODE_USER_AGENT
        );
        // No event or audit row names a token, a code or the secret.
        let connection = harness.store.connection().unwrap();
        let events: String = connection
            .query_row(
                "SELECT COALESCE(group_concat(payload_json), '') FROM events",
                [],
                |row| row.get(0),
            )
            .unwrap();
        let audit: String = connection
            .query_row(
                "SELECT COALESCE(group_concat(COALESCE(details_json, '')), '') FROM audit_log",
                [],
                |row| row.get(0),
            )
            .unwrap_or_default();
        for text in [events, audit] {
            for secret in [
                CLIENT_SECRET,
                "access-for-code-s",
                "refresh-for-code-s",
                "code-s",
            ] {
                assert!(!text.contains(secret), "{secret}");
            }
        }
    }

    #[test]
    fn the_standard_registry_offers_google_through_the_api_only() {
        // `standard()` has no age store, so Google is registered by the API.
        assert!(
            SignInRegistry::standard()
                .get("google-antigravity")
                .is_none()
        );
        let google = GoogleSignIn::new(age_store(true), "", None, None).unwrap();
        assert_eq!(google.flows(), &[FlowKind::Pkce]);
        let mut registry = SignInRegistry::standard();
        registry.register("google-antigravity", Arc::new(google));
        assert_eq!(registry.flows_for("google-antigravity"), ["pkce"]);
    }
}
