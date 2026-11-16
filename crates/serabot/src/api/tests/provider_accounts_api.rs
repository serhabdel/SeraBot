//! Provider accounts API (v0.49): several keys and sign-ins per provider, per
//! member. A wiremock server stands in for the provider and its identity
//! service; no real account is used.

use super::*;
use crate::accounts::Accounts;
use crate::auth_flows::device::DeviceSpec;
use crate::auth_flows::pkce::{PkceSpec, RedirectSpec};
use crate::auth_flows::{
    AccountIdentity, IdentityFn, OAuthSignIn, PinnedEndpoint, SignInRegistry, SignInService,
};
use crate::config::ProviderConfig;
use crate::model_router::ModelRouter;
use crate::providers::UserProviderCredentials;
use crate::user_secrets::UserSecrets;
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const REDIRECT: &str = "http://127.0.0.1:51121/oauth-callback";

struct Env {
    app: Router,
    store: Store,
    server: MockServer,
    alice: (String, String),
    bob: (String, String),
}

struct FakeQuota {
    fail: bool,
}

#[async_trait::async_trait]
impl QuotaReader for FakeQuota {
    async fn read(
        &self,
        _credential: &ProviderCredential,
        _identity: &Value,
    ) -> Result<Option<Value>> {
        if self.fail {
            return Err(SeraError::External("down".into()));
        }
        Ok(Some(
            json!({"remaining": 42, "resets_at": "2099-01-01T00:00:00Z"}),
        ))
    }
}

fn identity() -> IdentityFn {
    Arc::new(|tokens| {
        let email = tokens.id_token.clone().unwrap_or_default();
        Ok(AccountIdentity {
            label: "Signed-in account".into(),
            stable_id: email.to_lowercase(),
            identity: json!({"email": email, "plan": "synthetic-plan"}),
        })
    })
}

async fn env_with(quota: Option<bool>, egress_approved: bool) -> Env {
    let server = MockServer::start().await;
    let dir = std::env::temp_dir().join(format!("serabot-pacct-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let mut config = Config::default();
    config.api.enabled = true;
    config.api.token_hash = sha256_hex("test-token-that-is-long-enough-123");
    config.llm.model = "test".into();
    config.llm.base_url = "https://example.com/v1".into();
    config.llm.models = vec!["test-alternative".into()];
    config.memory.auto_extract = false;
    config.base_dir = dir.clone();
    config.llm.providers.insert(
        "acme".into(),
        ProviderConfig {
            base_url: format!("{}/v1", server.uri()),
            api_key_secret: "acme_api_key".into(),
            models: vec!["acme-1".into()],
            data_egress_approved: egress_approved,
            kind: "openai".into(),
            catalog_fetch: true,
            ..Default::default()
        },
    );
    let database = dir.join("serabot.db");
    config.database.path = database.to_string_lossy().into_owned();
    let config = Arc::new(config);
    let store = Store::open(&database, 384, 5000).unwrap();
    let users = Arc::new(UserSecrets::with_master_key(store.clone(), [9u8; 32]).unwrap());
    let secrets = SharedSecrets::new(crate::secrets::SecretStore::from_map(BTreeMap::from([(
        config.llm.api_key_secret.clone(),
        "synthetic-llm".into(),
    )])));
    let credentials = Arc::new(UserProviderCredentials::new(
        users.clone(),
        secrets.clone(),
        5,
        64,
    ));
    let refresher = credentials.refresher().clone();
    let model_router = ModelRouter::with_credentials(&config.llm, credentials).unwrap();
    let gateway = Gateway::new(
        store.clone(),
        Arc::new(Policy::empty()),
        ToolRegistry::default(),
        1,
        false,
    );
    let runtime = Runtime::new(
        config.clone(),
        store.clone(),
        gateway,
        model_router,
        vec![test_bot(
            "main",
            crate::orchestration::BotRole::Orchestrator,
        )],
        Arc::new(NullNotifier),
        tokio_util::sync::CancellationToken::new(),
    )
    .unwrap();
    let artifacts = Arc::new(ArtifactStore::new(dir.join("artifacts")).unwrap());
    let mut state = ApiState::new(
        runtime,
        store.clone(),
        config,
        artifacts,
        secrets,
        users,
        None,
    )
    .unwrap()
    .with_refresher(refresher)
    .unwrap();
    let mut registry = SignInRegistry::default();
    registry.register(
        "openai",
        Arc::new(OAuthSignIn {
            pkce: Some(PkceSpec {
                authorize: PinnedEndpoint::loopback(&format!("{}/authorize", server.uri())),
                token: PinnedEndpoint::loopback(&format!("{}/token", server.uri())),
                client_id: "synthetic-client".into(),
                client_secret: None,
                scope: "profile".into(),
                redirect: RedirectSpec {
                    port: Some(51121),
                    path: Some("/oauth-callback"),
                },
                extra_params: vec![],
            }),
            device: Some(DeviceSpec {
                device: PinnedEndpoint::loopback(&format!("{}/device", server.uri())),
                token: PinnedEndpoint::loopback(&format!("{}/device-token", server.uri())),
                client_id: "synthetic-client".into(),
                client_secret: None,
                scope: String::new(),
                verification_hosts: vec!["verify.example".into()],
                headers: vec![],
            }),
            auth_kind: crate::accounts::AuthKind::Oauth,
            fingerprint_scope: "openai".into(),
            identity: identity(),
        }),
    );
    let signin = Arc::new(
        SignInService::new(state.accounts.clone())
            .unwrap()
            .with_registry(registry),
    );
    state = state.with_signin(signin);
    if let Some(fail) = quota {
        state = state.with_quota_readers(HashMap::from([(
            "openai".to_owned(),
            Arc::new(FakeQuota { fail }) as Arc<dyn QuotaReader>,
        )]));
    }
    let alice = member(&store, "pacct-alice");
    let bob = member(&store, "pacct-bob");
    Env {
        app: router(state),
        store,
        server,
        alice,
        bob,
    }
}

async fn env() -> Env {
    env_with(None, true).await
}

fn member(store: &Store, subject: &str) -> (String, String) {
    let user = store
        .create_user(subject, &format!("{subject}@example.example"), subject)
        .unwrap();
    let token = mint_for(store, &user.id, subject);
    (user.id, token)
}

async fn call(
    env: &Env,
    token: &str,
    method: &str,
    uri: &str,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let response = env
        .app
        .clone()
        .oneshot(request_with_token(method, uri, token, body))
        .await
        .unwrap();
    let status = response.status();
    // A malformed body is refused by the extractor with plain text.
    let bytes = axum::body::to_bytes(response.into_body(), 1 << 20)
        .await
        .unwrap();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

/// A catalog endpoint that accepts exactly `key`.
async fn accept_key(server: &MockServer, key: &str) {
    Mock::given(method("GET"))
        .and(path("/v1/models"))
        .and(header("authorization", format!("Bearer {key}").as_str()))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "object": "list",
            "data": [{"id": "acme-1"}, {"id": "acme-2"}]
        })))
        .mount(server)
        .await;
}

async fn reject_key(server: &MockServer, key: &str) {
    Mock::given(method("GET"))
        .and(path("/v1/models"))
        .and(header("authorization", format!("Bearer {key}").as_str()))
        .respond_with(ResponseTemplate::new(401).set_body_string("bad key sk-never-echoed"))
        .mount(server)
        .await;
}

fn no_secrets(text: &str) {
    for secret in [
        "sk-alice",
        "sk-bob",
        "sk-good",
        "sk-bad",
        "sk-never-echoed",
        "access-synthetic",
        "refresh-synthetic",
        "code-synthetic",
        "dc-synthetic",
        "fingerprint",
    ] {
        assert!(!text.contains(secret), "{secret} leaked in {text}");
    }
}

fn audit_count(store: &Store, event: &str) -> i64 {
    store
        .connection()
        .unwrap()
        .query_row(
            "SELECT COUNT(*) FROM audit_log WHERE event = ?1",
            [event],
            |row| row.get(0),
        )
        .unwrap()
}

#[tokio::test]
async fn accounts_are_added_listed_changed_and_removed() {
    let env = env().await;
    let token = env.alice.1.clone();
    accept_key(&env.server, "sk-alice-1").await;
    accept_key(&env.server, "sk-alice-2").await;

    let (status, first) = call(
        &env,
        &token,
        "POST",
        "/v1/providers/acme/accounts",
        Some(json!({"label": "Work", "api_key": "sk-alice-1"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{first}");
    assert_eq!(first["created"], true);
    assert_eq!(first["verified"], true);
    no_secrets(&first.to_string());
    let first_id = first["id"].as_str().unwrap().to_owned();

    // The same key again is the same account.
    let (_, again) = call(
        &env,
        &token,
        "POST",
        "/v1/providers/acme/accounts",
        Some(json!({"label": "Other name", "api_key": "sk-alice-1"})),
    )
    .await;
    assert_eq!(again["id"], first["id"]);
    assert_eq!(again["created"], false);

    let (_, second) = call(
        &env,
        &token,
        "POST",
        "/v1/providers/acme/accounts",
        Some(json!({"label": "Home", "api_key": "sk-alice-2"})),
    )
    .await;
    let second_id = second["id"].as_str().unwrap().to_owned();

    // PATCH changes priority and label; the list is best first.
    let (status, patched) = call(
        &env,
        &token,
        "PATCH",
        &format!("/v1/providers/acme/accounts/{second_id}"),
        Some(json!({"label": "Home PC", "priority": 5, "weight": 3})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{patched}");
    assert_eq!(patched["account"]["label"], "Home PC");
    assert_eq!(patched["account"]["priority"], 5);
    assert_eq!(patched["account"]["weight"], 3);

    let (status, listed) = call(&env, &token, "GET", "/v1/providers/acme/accounts", None).await;
    assert_eq!(status, StatusCode::OK);
    let accounts = listed["accounts"].as_array().unwrap();
    assert_eq!(accounts.len(), 2, "{listed}");
    assert_eq!(accounts[0]["id"], second_id.as_str());
    assert_eq!(accounts[1]["label"], "Work");
    assert_eq!(accounts[0]["auth_kind"], "api_key");
    assert_eq!(accounts[0]["status"], "ok");
    no_secrets(&listed.to_string());

    // GET /v1/providers carries the accounts and counts the provider usable.
    let (_, providers) = call(&env, &token, "GET", "/v1/providers", None).await;
    let acme = providers["providers"]
        .as_array()
        .unwrap()
        .iter()
        .find(|provider| provider["id"] == "acme")
        .unwrap()
        .clone();
    assert_eq!(acme["accounts"].as_array().unwrap().len(), 2);
    assert_eq!(acme["credential"]["configured"], true);
    assert_eq!(acme["credential"]["accounts"], 2);
    assert_eq!(acme["signin"], json!(["pkce", "device"]));
    no_secrets(&providers.to_string());

    // Disabling every account keeps the provider on the page, so one can be
    // turned back on.
    for id in [&first_id, &second_id] {
        let (status, body) = call(
            &env,
            &token,
            "PATCH",
            &format!("/v1/providers/acme/accounts/{id}"),
            Some(json!({"disabled": true})),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["account"]["status"], "disabled");
    }
    let (_, providers) = call(&env, &token, "GET", "/v1/providers", None).await;
    let acme = providers["providers"]
        .as_array()
        .unwrap()
        .iter()
        .find(|provider| provider["id"] == "acme")
        .expect("a provider with only disabled accounts stays listed")
        .clone();
    assert_eq!(acme["credential"]["configured"], false);
    assert_eq!(acme["accounts"][0]["status"], "disabled");
    let (_, body) = call(
        &env,
        &token,
        "PATCH",
        &format!("/v1/providers/acme/accounts/{first_id}"),
        Some(json!({"disabled": false})),
    )
    .await;
    assert_eq!(body["account"]["status"], "ok");

    // DELETE removes one; a second DELETE is a 404.
    let (status, removed) = call(
        &env,
        &token,
        "DELETE",
        &format!("/v1/providers/acme/accounts/{first_id}"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{removed}");
    assert_eq!(removed["deleted"], true);
    let (status, _) = call(
        &env,
        &token,
        "DELETE",
        &format!("/v1/providers/acme/accounts/{first_id}"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (_, listed) = call(&env, &token, "GET", "/v1/providers/acme/accounts", None).await;
    assert_eq!(listed["accounts"].as_array().unwrap().len(), 1);

    // The `?force=true` spelling is accepted even when nothing pins the account.
    let (status, _) = call(
        &env,
        &token,
        "DELETE",
        &format!("/v1/providers/acme/accounts/{second_id}?force=true"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    // Every change was audited, with no key in the rows.
    assert_eq!(audit_count(&env.store, "provider.account_added"), 3);
    assert_eq!(audit_count(&env.store, "provider.account_disabled"), 2);
    assert_eq!(audit_count(&env.store, "provider.account_removed"), 2);
    let details: String = env
        .store
        .connection()
        .unwrap()
        .query_row(
            "SELECT group_concat(details_json) FROM audit_log WHERE event LIKE 'provider.account_%'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    no_secrets(&details);
}

#[tokio::test]
async fn the_legacy_key_route_is_the_default_account() {
    let env = env().await;
    let token = env.alice.1.clone();
    let (status, _) = call(
        &env,
        &token,
        "PUT",
        "/v1/providers/acme/key",
        Some(json!({"api_key": "sk-alice-legacy"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (_, listed) = call(&env, &token, "GET", "/v1/providers/acme/accounts", None).await;
    let accounts = listed["accounts"].as_array().unwrap();
    assert_eq!(accounts.len(), 1);
    assert_eq!(accounts[0]["id"], "default");
    assert_eq!(accounts[0]["label"], "Default");
    // A second key sits beside it.
    accept_key(&env.server, "sk-alice-extra").await;
    let (_, added) = call(
        &env,
        &token,
        "POST",
        "/v1/providers/acme/accounts",
        Some(json!({"api_key": "sk-alice-extra"})),
    )
    .await;
    assert_eq!(added["created"], true);
    let (_, listed) = call(&env, &token, "GET", "/v1/providers/acme/accounts", None).await;
    assert_eq!(listed["accounts"].as_array().unwrap().len(), 2);
    // The same key as the default is recognised as the default.
    let (_, same) = call(
        &env,
        &token,
        "POST",
        "/v1/providers/acme/accounts",
        Some(json!({"api_key": "sk-alice-legacy"})),
    )
    .await;
    assert_eq!(same["id"], "default");
    assert_eq!(same["created"], false);
    // The first edit moves the default into the account scheme.
    let (status, moved) = call(
        &env,
        &token,
        "PATCH",
        "/v1/providers/acme/accounts/default",
        Some(json!({"label": "Primary"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{moved}");
    assert_ne!(moved["account"]["id"], "default");
    assert_eq!(moved["account"]["label"], "Primary");
    no_secrets(&moved.to_string());
}

#[tokio::test]
async fn bad_requests_are_refused_with_a_reason() {
    let env = env().await;
    let token = env.alice.1.clone();
    for (body, code) in [
        (json!({"api_key": ""}), "bad_key"),
        (json!({"api_key": "   "}), "bad_key"),
        (json!({"api_key": "line\nbreak"}), "bad_key"),
        (
            json!({"label": "x".repeat(49), "api_key": "sk-alice-label"}),
            "bad_label",
        ),
    ] {
        let (status, response) = call(
            &env,
            &token,
            "POST",
            "/v1/providers/acme/accounts",
            Some(body.clone()),
        )
        .await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
        assert_eq!(response["code"], code, "{response}");
    }
    // Unknown fields, a missing key and an unknown provider.
    let (status, _) = call(
        &env,
        &token,
        "POST",
        "/v1/providers/acme/accounts",
        Some(json!({"api_key": "sk-alice-x", "extra": 1})),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    let (status, _) = call(
        &env,
        &token,
        "POST",
        "/v1/providers/acme/accounts",
        Some(json!({"label": "No key"})),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    for uri in [
        "/v1/providers/nope/accounts",
        "/v1/providers/nope/accounts/abcdef012345/test",
    ] {
        let method = if uri.ends_with("test") { "POST" } else { "GET" };
        let (status, _) = call(&env, &token, method, uri, None).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{uri}");
    }
    // Nothing to change; an empty label; a malformed id.
    accept_key(&env.server, "sk-alice-ok").await;
    let (_, added) = call(
        &env,
        &token,
        "POST",
        "/v1/providers/acme/accounts",
        Some(json!({"api_key": "sk-alice-ok"})),
    )
    .await;
    let id = added["id"].as_str().unwrap();
    let (status, response) = call(
        &env,
        &token,
        "PATCH",
        &format!("/v1/providers/acme/accounts/{id}"),
        Some(json!({})),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(response["code"], "nothing_to_change");
    let (status, response) = call(
        &env,
        &token,
        "PATCH",
        &format!("/v1/providers/acme/accounts/{id}"),
        Some(json!({"label": "  "})),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(response["code"], "bad_label");
    let (status, _) = call(
        &env,
        &token,
        "PATCH",
        "/v1/providers/acme/accounts/not-an-id",
        Some(json!({"label": "x"})),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn a_key_the_provider_refuses_is_not_stored_and_an_unreachable_one_is_marked_unverified() {
    let env = env().await;
    let token = env.alice.1.clone();
    reject_key(&env.server, "sk-bad-one").await;
    let (status, response) = call(
        &env,
        &token,
        "POST",
        "/v1/providers/acme/accounts",
        Some(json!({"label": "Bad", "api_key": "sk-bad-one"})),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{response}");
    assert_eq!(response["code"], "key_rejected");
    no_secrets(&response.to_string());
    let (_, listed) = call(&env, &token, "GET", "/v1/providers/acme/accounts", None).await;
    assert!(listed["accounts"].as_array().unwrap().is_empty());

    // A provider outage says nothing about the key: it is kept, unverified.
    Mock::given(method("GET"))
        .and(path("/v1/models"))
        .and(header("authorization", "Bearer sk-alice-outage"))
        .respond_with(ResponseTemplate::new(500))
        .mount(&env.server)
        .await;
    let (status, response) = call(
        &env,
        &token,
        "POST",
        "/v1/providers/acme/accounts",
        Some(json!({"api_key": "sk-alice-outage"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{response}");
    assert_eq!(response["verified"], false);
}

#[tokio::test]
async fn without_egress_approval_nothing_is_sent_to_the_provider() {
    let env = env_with(None, false).await;
    let token = env.alice.1.clone();
    let (status, response) = call(
        &env,
        &token,
        "POST",
        "/v1/providers/acme/accounts",
        Some(json!({"api_key": "sk-alice-quiet"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{response}");
    assert_eq!(response["verified"], false);
    let id = response["id"].as_str().unwrap();
    let (_, tested) = call(
        &env,
        &token,
        "POST",
        &format!("/v1/providers/acme/accounts/{id}/test"),
        None,
    )
    .await;
    assert_eq!(tested["status"], "egress_not_approved");
    assert_eq!(tested["ok"], false);
    assert!(env.server.received_requests().await.unwrap().is_empty());
}

#[tokio::test]
async fn the_account_limit_is_reported() {
    let env = env_with(None, false).await;
    let token = env.alice.1.clone();
    for index in 0..10 {
        let (status, _) = call(
            &env,
            &token,
            "POST",
            "/v1/providers/acme/accounts",
            Some(json!({"api_key": format!("sk-alice-{index}")})),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
    }
    let (status, response) = call(
        &env,
        &token,
        "POST",
        "/v1/providers/acme/accounts",
        Some(json!({"api_key": "sk-alice-eleven"})),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(response["code"], "account_limit");
}

#[tokio::test]
async fn another_member_finds_nothing_of_my_accounts() {
    let env = env().await;
    let (alice, bob) = (env.alice.1.clone(), env.bob.1.clone());
    accept_key(&env.server, "sk-alice-1").await;
    let (_, added) = call(
        &env,
        &alice,
        "POST",
        "/v1/providers/acme/accounts",
        Some(json!({"label": "Mine", "api_key": "sk-alice-1"})),
    )
    .await;
    let id = added["id"].as_str().unwrap().to_owned();
    let base = format!("/v1/providers/acme/accounts/{id}");

    let (status, listed) = call(&env, &bob, "GET", "/v1/providers/acme/accounts", None).await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        listed["accounts"].as_array().unwrap().is_empty(),
        "{listed}"
    );
    for (verb, uri, body) in [
        ("PATCH", base.clone(), Some(json!({"label": "Stolen"}))),
        ("DELETE", base.clone(), None),
        ("POST", format!("{base}/test"), None),
        ("GET", format!("{base}/quota"), None),
    ] {
        let (status, response) = call(&env, &bob, verb, &uri, body).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{verb} {uri}: {response}");
        no_secrets(&response.to_string());
    }
    // Bob's own providers listing never mentions the account either.
    let (_, providers) = call(&env, &bob, "GET", "/v1/providers", None).await;
    assert!(!providers.to_string().contains(&id));
    // Alice's account is untouched.
    let (_, listed) = call(&env, &alice, "GET", "/v1/providers/acme/accounts", None).await;
    assert_eq!(listed["accounts"][0]["label"], "Mine");
    // The same key under bob is a different account (the fingerprint is per
    // member), so bob's add does not reveal that alice holds it.
    accept_key(&env.server, "sk-alice-1").await;
    let (_, bobs) = call(
        &env,
        &bob,
        "POST",
        "/v1/providers/acme/accounts",
        Some(json!({"api_key": "sk-alice-1"})),
    )
    .await;
    assert_eq!(bobs["created"], true);
    assert_ne!(bobs["id"], added["id"]);
}

#[tokio::test]
async fn testing_an_account_reports_a_stable_status() {
    let env = env().await;
    let token = env.alice.1.clone();
    accept_key(&env.server, "sk-good-key").await;
    reject_key(&env.server, "sk-bad-key").await;
    // A key the provider later revoked cannot be added through the API, so
    // store it the way an earlier good sign-in left it.
    let accounts = Accounts::new(env.store.clone(), {
        Arc::new(UserSecrets::with_master_key(env.store.clone(), [9u8; 32]).unwrap())
    });
    let good = accounts
        .add_api_key(&env.alice.0, "acme", "Good", "sk-good-key")
        .unwrap();
    let bad = accounts
        .add_api_key(&env.alice.0, "acme", "Revoked", "sk-bad-key")
        .unwrap();

    let (status, ok) = call(
        &env,
        &token,
        "POST",
        &format!("/v1/providers/acme/accounts/{}/test", good.account_id),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(ok["ok"], true);
    assert_eq!(ok["status"], "ok");
    assert_eq!(ok["model_count"], 2);
    no_secrets(&ok.to_string());

    let (_, rejected) = call(
        &env,
        &token,
        "POST",
        &format!("/v1/providers/acme/accounts/{}/test", bad.account_id),
        None,
    )
    .await;
    assert_eq!(rejected["ok"], false);
    assert_eq!(rejected["status"], "key_rejected");
    no_secrets(&rejected.to_string());
    let (_, listed) = call(&env, &token, "GET", "/v1/providers/acme/accounts", None).await;
    let revoked = listed["accounts"]
        .as_array()
        .unwrap()
        .iter()
        .find(|account| account["label"] == "Revoked")
        .unwrap();
    assert_eq!(revoked["status"], "needs_reauth");

    // A provider outage is "unreachable" and changes nothing.
    Mock::given(method("GET"))
        .and(path("/v1/models"))
        .and(header("authorization", "Bearer sk-down-key"))
        .respond_with(ResponseTemplate::new(503))
        .mount(&env.server)
        .await;
    let down = accounts
        .add_api_key(&env.alice.0, "acme", "Down", "sk-down-key")
        .unwrap();
    let (_, unreachable) = call(
        &env,
        &token,
        "POST",
        &format!("/v1/providers/acme/accounts/{}/test", down.account_id),
        None,
    )
    .await;
    assert_eq!(unreachable["status"], "unreachable");
    assert_eq!(audit_count(&env.store, "provider.account_tested"), 3);
}

#[tokio::test]
async fn quota_is_cached_and_refreshing_is_throttled() {
    let env = env_with(Some(false), true).await;
    let token = env.alice.1.clone();
    accept_key(&env.server, "sk-alice-quota").await;
    let (_, added) = call(
        &env,
        &token,
        "POST",
        "/v1/providers/acme/accounts",
        Some(json!({"api_key": "sk-alice-quota"})),
    )
    .await;
    let id = added["id"].as_str().unwrap();
    let uri = format!("/v1/providers/acme/accounts/{id}/quota");

    let (status, cached) = call(&env, &token, "GET", &uri, None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(cached["refresh"], "cached");
    assert!(cached["quota"].is_null());

    let (_, fresh) = call(&env, &token, "GET", &format!("{uri}?refresh=1"), None).await;
    assert_eq!(fresh["refresh"], "refreshed");
    assert_eq!(fresh["quota"]["remaining"], 42);
    // At most once per four minutes; the cached reading is still served.
    let (_, throttled) = call(&env, &token, "GET", &format!("{uri}?refresh=1"), None).await;
    assert_eq!(throttled["refresh"], "throttled");
    assert_eq!(throttled["quota"]["remaining"], 42);
    // The snapshot is on the listing too.
    let (_, listed) = call(&env, &token, "GET", "/v1/providers/acme/accounts", None).await;
    assert_eq!(listed["accounts"][0]["quota"]["remaining"], 42);
    no_secrets(&listed.to_string());

    // A provider with no reader reports that, and a failing reader is just
    // "no reading".
    let plain = env_with(None, true).await;
    accept_key(&plain.server, "sk-alice-q2").await;
    let (_, added) = call(
        &plain,
        &plain.alice.1.clone(),
        "POST",
        "/v1/providers/acme/accounts",
        Some(json!({"api_key": "sk-alice-q2"})),
    )
    .await;
    let (_, unsupported) = call(
        &plain,
        &plain.alice.1.clone(),
        "GET",
        &format!(
            "/v1/providers/acme/accounts/{}/quota?refresh=1",
            added["id"].as_str().unwrap()
        ),
        None,
    )
    .await;
    assert_eq!(unsupported["refresh"], "unsupported");
    let failing = env_with(Some(true), true).await;
    accept_key(&failing.server, "sk-alice-q3").await;
    let (_, added) = call(
        &failing,
        &failing.alice.1.clone(),
        "POST",
        "/v1/providers/acme/accounts",
        Some(json!({"api_key": "sk-alice-q3"})),
    )
    .await;
    let (status, none) = call(
        &failing,
        &failing.alice.1.clone(),
        "GET",
        &format!(
            "/v1/providers/acme/accounts/{}/quota?refresh=true",
            added["id"].as_str().unwrap()
        ),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(none["refresh"], "no_reading");
    assert!(none["quota"].is_null());
}

fn token_reply(email: &str) -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_json(json!({
        "access_token": "access-synthetic",
        "refresh_token": "refresh-synthetic",
        "expires_in": 3600,
        "id_token": email,
    }))
}

#[tokio::test]
async fn a_browser_sign_in_creates_an_account_and_masks_the_email() {
    let env = env().await;
    let (alice, bob) = (env.alice.1.clone(), env.bob.1.clone());
    Mock::given(method("POST"))
        .and(path("/token"))
        .respond_with(token_reply("Person@Example.Example"))
        .mount(&env.server)
        .await;
    let (status, started) = call(
        &env,
        &alice,
        "POST",
        "/v1/providers/acme/signin/start",
        Some(json!({"flow": "pkce", "redirect_uri": REDIRECT})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{started}");
    let flow_id = started["flow_id"].as_str().unwrap().to_owned();
    assert!(started["url"].as_str().unwrap().contains("code_challenge="));
    assert!(started["expires_in"].as_u64().unwrap() > 0);
    no_secrets(&started.to_string());
    let status_uri = format!("/v1/providers/acme/signin/{flow_id}");
    let (status, pending) = call(&env, &alice, "GET", &status_uri, None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(pending["status"], "pending");

    // Bob can neither watch nor finish it.
    let (status, _) = call(&env, &bob, "GET", &status_uri, None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let body = json!({"flow_id": flow_id, "code": "code-synthetic", "redirect_uri": REDIRECT});
    let (status, _) = call(
        &env,
        &bob,
        "POST",
        "/v1/providers/acme/signin/complete",
        Some(body.clone()),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    let (status, done) = call(
        &env,
        &alice,
        "POST",
        "/v1/providers/acme/signin/complete",
        Some(body.clone()),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{done}");
    assert_eq!(done["created"], true);
    no_secrets(&done.to_string());
    let (_, finished) = call(&env, &alice, "GET", &status_uri, None).await;
    assert_eq!(finished["status"], "ok");
    assert_eq!(finished["account_id"], done["account_id"]);

    // The listing shows the identity with the email masked.
    let (_, listed) = call(&env, &alice, "GET", "/v1/providers/acme/accounts", None).await;
    let account = &listed["accounts"][0];
    assert_eq!(account["auth_kind"], "oauth");
    assert_eq!(account["identity"]["email"], "P***@Example.Example");
    assert_eq!(account["identity"]["plan"], "synthetic-plan");
    assert!(!listed.to_string().contains("Person@"));
    no_secrets(&listed.to_string());
    // The code is single-use.
    let (status, replay) = call(
        &env,
        &alice,
        "POST",
        "/v1/providers/acme/signin/complete",
        Some(body),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(replay["code"], "flow_used");
    assert_eq!(audit_count(&env.store, "provider.account_signed_in"), 1);
}

#[tokio::test]
async fn sign_in_requests_are_validated() {
    let env = env().await;
    let token = env.alice.1.clone();
    for (body, code) in [
        (json!({"flow": "carrier-pigeon"}), "bad_flow"),
        (json!({"flow": "pkce"}), "bad_redirect"),
        (
            json!({"flow": "pkce", "redirect_uri": "http://127.0.0.1:9999/oauth-callback"}),
            "bad_redirect",
        ),
        (
            json!({"flow": "pkce", "redirect_uri": REDIRECT, "challenge": "short"}),
            "bad_challenge",
        ),
    ] {
        let (status, response) = call(
            &env,
            &token,
            "POST",
            "/v1/providers/acme/signin/start",
            Some(body.clone()),
        )
        .await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
        assert_eq!(response["code"], code, "{response}");
    }
    // Unknown fields, an unknown flow id, and a provider with no sign-in.
    let (status, _) = call(
        &env,
        &token,
        "POST",
        "/v1/providers/acme/signin/start",
        Some(json!({"flow": "pkce", "redirect_uri": REDIRECT, "extra": 1})),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    let (status, _) = call(
        &env,
        &token,
        "POST",
        "/v1/providers/acme/signin/complete",
        Some(json!({"flow_id": "nope", "code": "c", "redirect_uri": REDIRECT})),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _) = call(&env, &token, "GET", "/v1/providers/acme/signin/nope", None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, response) = call(
        &env,
        &token,
        "POST",
        "/v1/providers/default/signin/start",
        Some(json!({"flow": "pkce", "redirect_uri": REDIRECT})),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(response["code"], "signin_unsupported");
    let (status, _) = call(
        &env,
        &token,
        "POST",
        "/v1/providers/nope/signin/start",
        Some(json!({"flow": "pkce", "redirect_uri": REDIRECT})),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn the_old_oauth_routes_are_aliases_of_sign_in() {
    let env = env().await;
    let token = env.alice.1.clone();
    Mock::given(method("POST"))
        .and(path("/token"))
        .respond_with(token_reply("legacy@example.example"))
        .mount(&env.server)
        .await;
    let (verifier, challenge) = crate::auth_flows::pkce::pkce_s256();
    let (status, started) = call(
        &env,
        &token,
        "POST",
        "/v1/providers/acme/oauth/start",
        Some(json!({"challenge": challenge, "state": "state-0123456789abcdef", "redirect_uri": REDIRECT})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{started}");
    assert_eq!(started["flow"], "pkce");
    assert!(
        started["url"]
            .as_str()
            .unwrap()
            .contains(&format!("code_challenge={challenge}"))
    );
    // No flow id: the newest open flow is finished, with the desktop's verifier.
    let (status, done) = call(
        &env,
        &token,
        "POST",
        "/v1/providers/acme/oauth/complete",
        Some(json!({"code": "code-synthetic", "verifier": verifier, "redirect_uri": REDIRECT})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{done}");
    assert_eq!(done["configured"], true);
    assert_eq!(done["created"], true);
    no_secrets(&done.to_string());
    let (_, listed) = call(&env, &token, "GET", "/v1/providers/acme/accounts", None).await;
    assert_eq!(listed["accounts"].as_array().unwrap().len(), 1);
    // Nothing open any more.
    let (status, _) = call(
        &env,
        &token,
        "POST",
        "/v1/providers/acme/oauth/complete",
        Some(json!({"code": "code-synthetic", "verifier": verifier})),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn a_device_sign_in_finishes_in_the_background() {
    let env = env().await;
    let (alice, bob) = (env.alice.1.clone(), env.bob.1.clone());
    Mock::given(method("POST"))
        .and(path("/device"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "device_code": "dc-synthetic",
            "user_code": "WXYZ-9876",
            "verification_uri": "https://verify.example/device",
            "interval": 1,
            "expires_in": 120,
        })))
        .mount(&env.server)
        .await;
    Mock::given(method("POST"))
        .and(path("/device-token"))
        .respond_with(token_reply("device@example.example"))
        .mount(&env.server)
        .await;
    let (status, started) = call(
        &env,
        &alice,
        "POST",
        "/v1/providers/acme/signin/start",
        Some(json!({"flow": "device"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{started}");
    assert_eq!(started["user_code"], "WXYZ-9876");
    assert_eq!(started["url"], "https://verify.example/device");
    assert_eq!(started["interval"], 1);
    no_secrets(&started.to_string());
    let uri = format!(
        "/v1/providers/acme/signin/{}",
        started["flow_id"].as_str().unwrap()
    );
    let mut last = Value::Null;
    for _ in 0..50 {
        let (_, status) = call(&env, &alice, "GET", &uri, None).await;
        last = status;
        if last["status"] != "pending" {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert_eq!(last["status"], "ok", "{last}");
    assert_eq!(last["created"], true);
    let (_, listed) = call(&env, &alice, "GET", "/v1/providers/acme/accounts", None).await;
    assert_eq!(listed["accounts"][0]["auth_kind"], "oauth");
    let (_, bobs) = call(&env, &bob, "GET", "/v1/providers/acme/accounts", None).await;
    assert!(bobs["accounts"].as_array().unwrap().is_empty());
    let (status, _) = call(&env, &bob, "GET", &uri, None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[test]
fn emails_are_masked() {
    assert_eq!(mask_email("person@example.com"), "p***@example.com");
    assert_eq!(mask_email("@example.com"), "***");
    assert_eq!(mask_email("no-at-sign"), "***");
}
