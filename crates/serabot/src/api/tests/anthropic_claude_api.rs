//! Anthropic accounts through the API (v0.49): several API keys per member,
//! and the admin-only, flag-gated Claude plan sign-in. A wiremock server
//! stands in for Anthropic and claude.ai.

use super::*;
use crate::auth_flows::claude::ClaudeSignIn;
use crate::auth_flows::{SignInRegistry, SignInService};
use crate::config::ProviderConfig;
use crate::model_router::ModelRouter;
use crate::providers::UserProviderCredentials;
use crate::user_secrets::UserSecrets;
use wiremock::matchers::{body_partial_json, header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const REDIRECT: &str = "http://localhost:54545/callback";
const OWNER: &str = "test-token-that-is-long-enough-123";
const CLIENT_NAME: &str = "claude_client_id";

struct Env {
    app: Router,
    server: MockServer,
    alice: String,
    alice_id: String,
    bob: String,
}

async fn env() -> Env {
    let server = MockServer::start().await;
    let dir = std::env::temp_dir().join(format!("serabot-claude-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let mut config = Config::default();
    config.api.enabled = true;
    config.api.token_hash = sha256_hex(OWNER);
    config.llm.model = "test".into();
    config.llm.base_url = "https://example.com/v1".into();
    config.memory.auto_extract = false;
    config.base_dir = dir.clone();
    let base = format!("{}/v1", server.uri());
    // `claude` has the plan sign-in turned on; `anthropic` does not.
    config.llm.providers.insert(
        "claude".into(),
        ProviderConfig {
            kind: "anthropic".into(),
            api_key_secret: "claude_api_key".into(),
            base_url: base.clone(),
            models: vec!["claude-sonnet-5-5".into()],
            data_egress_approved: true,
            catalog_fetch: true,
            oauth_client_id_secret: CLIENT_NAME.into(),
            plan_oauth: true,
            ..Default::default()
        },
    );
    config.llm.providers.insert(
        "anthropic".into(),
        ProviderConfig {
            kind: "anthropic".into(),
            api_key_secret: "anthropic_api_key".into(),
            base_url: base,
            models: vec!["claude-sonnet-5-5".into()],
            data_egress_approved: true,
            catalog_fetch: true,
            ..Default::default()
        },
    );
    let database = dir.join("serabot.db");
    config.database.path = database.to_string_lossy().into_owned();
    let config = Arc::new(config);
    let store = Store::open(&database, 384, 5000).unwrap();
    let users = Arc::new(UserSecrets::with_master_key(store.clone(), [6u8; 32]).unwrap());
    let age = BTreeMap::from([
        (config.llm.api_key_secret.clone(), "synthetic-llm".into()),
        (CLIENT_NAME.to_owned(), "synthetic-claude-client-id".into()),
    ]);
    let secrets = SharedSecrets::new(crate::secrets::SecretStore::from_map(age));
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
        secrets.clone(),
        users,
        None,
    )
    .unwrap()
    .with_refresher(refresher)
    .unwrap();
    let mut registry = SignInRegistry::default();
    registry.register(
        "anthropic",
        Arc::new(ClaudeSignIn::for_test(secrets, CLIENT_NAME, &server.uri())),
    );
    let signin = Arc::new(
        SignInService::new(state.accounts.clone())
            .unwrap()
            .with_registry(registry),
    );
    state = state.with_signin(signin);
    let alice_user = store
        .create_user(
            "claude-alice",
            "claude-alice@example.example",
            "claude-alice",
        )
        .unwrap();
    let alice = mint_for(&store, &alice_user.id, "claude-alice");
    let bob_user = store
        .create_user("claude-bob", "claude-bob@example.example", "claude-bob")
        .unwrap();
    let bob = mint_for(&store, &bob_user.id, "claude-bob");
    Env {
        app: router(state),
        server,
        alice,
        alice_id: alice_user.id,
        bob,
    }
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
    let bytes = axum::body::to_bytes(response.into_body(), 1 << 20)
        .await
        .unwrap();
    (
        status,
        serde_json::from_slice(&bytes)
            .unwrap_or_else(|_| json!({"raw": String::from_utf8_lossy(&bytes)})),
    )
}

async fn catalog_accepting(server: &MockServer, key: &str) {
    Mock::given(method("GET"))
        .and(path("/v1/models"))
        .and(header("x-api-key", key))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": [{"type": "model", "id": "claude-sonnet-5-5"}], "has_more": false
        })))
        .mount(server)
        .await;
}

async fn plan_token(server: &MockServer, code: &str, uuid: &str) {
    Mock::given(method("POST"))
        .and(path("/token"))
        .and(body_partial_json(json!({"code": code})))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "access_token": format!("plan-access-{code}"),
            "refresh_token": format!("plan-refresh-{code}"),
            "expires_in": 28800,
            "account": {"uuid": uuid, "email_address": "Plan@Example.Example"}
        })))
        .mount(server)
        .await;
}

fn provider<'a>(listed: &'a Value, id: &str) -> &'a Value {
    listed["providers"]
        .as_array()
        .unwrap()
        .iter()
        .find(|provider| provider["id"] == id)
        .unwrap_or_else(|| panic!("{id} not listed in {listed}"))
}

#[tokio::test]
async fn a_member_keeps_several_anthropic_api_keys() {
    let env = env().await;
    catalog_accepting(&env.server, "sk-ant-first").await;
    catalog_accepting(&env.server, "sk-ant-second").await;
    let mut ids = Vec::new();
    for (label, key) in [("Work", "sk-ant-first"), ("Spare", "sk-ant-second")] {
        let (status, body) = call(
            &env,
            &env.alice,
            "POST",
            "/v1/providers/anthropic/accounts",
            Some(json!({"label": label, "api_key": key})),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["created"], true);
        assert_eq!(body["verified"], true);
        assert!(!body.to_string().contains("sk-ant"));
        ids.push(body["id"].as_str().unwrap().to_owned());
    }
    assert_ne!(ids[0], ids[1]);
    // The same key again is recognised, not stored twice.
    let (_, again) = call(
        &env,
        &env.alice,
        "POST",
        "/v1/providers/anthropic/accounts",
        Some(json!({"label": "Work again", "api_key": "sk-ant-first"})),
    )
    .await;
    assert_eq!(again["created"], false);
    assert_eq!(again["id"], ids[0].as_str());
    let (_, listed) = call(
        &env,
        &env.alice,
        "GET",
        "/v1/providers/anthropic/accounts",
        None,
    )
    .await;
    assert_eq!(listed["accounts"].as_array().unwrap().len(), 2);
    assert!(
        listed["accounts"]
            .as_array()
            .unwrap()
            .iter()
            .all(|account| account["auth_kind"] == "api_key")
    );
    assert!(!listed.to_string().contains("sk-ant"));
    // A plain API-key provider offers no sign-in, and Bob sees no accounts.
    assert_eq!(listed["signin"], json!([]));
    let (_, bobs) = call(
        &env,
        &env.bob,
        "GET",
        "/v1/providers/anthropic/accounts",
        None,
    )
    .await;
    assert!(bobs["accounts"].as_array().unwrap().is_empty());
    let uri = format!("/v1/providers/anthropic/accounts/{}/test", ids[0]);
    let (status, _) = call(&env, &env.bob, "POST", &uri, None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    // Alice's test of her own account reaches Anthropic with her key.
    let (status, tested) = call(&env, &env.alice, "POST", &uri, None).await;
    assert_eq!(status, StatusCode::OK, "{tested}");
    assert_eq!(tested["ok"], true);
    assert_eq!(tested["model_count"], 1);
}

#[tokio::test]
async fn a_key_anthropic_refuses_is_not_stored() {
    let env = env().await;
    Mock::given(method("GET"))
        .and(path("/v1/models"))
        .respond_with(ResponseTemplate::new(401).set_body_json(json!({
            "type": "error", "error": {"type": "authentication_error", "message": "invalid x-api-key"}
        })))
        .mount(&env.server)
        .await;
    let (status, body) = call(
        &env,
        &env.alice,
        "POST",
        "/v1/providers/anthropic/accounts",
        Some(json!({"label": "Bad", "api_key": "sk-ant-bad"})),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert_eq!(body["code"], "key_rejected");
    let (_, listed) = call(
        &env,
        &env.alice,
        "GET",
        "/v1/providers/anthropic/accounts",
        None,
    )
    .await;
    assert!(listed["accounts"].as_array().unwrap().is_empty());
}

#[tokio::test]
async fn members_cannot_start_the_plan_sign_in_but_the_admin_can() {
    let env = env().await;
    plan_token(&env.server, "admin-code", "uuid-admin").await;
    let start = json!({"flow": "pkce", "redirect_uri": REDIRECT});
    let (status, body) = call(
        &env,
        &env.alice,
        "POST",
        "/v1/providers/claude/signin/start",
        Some(start.clone()),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    // The old spelling is closed to members too.
    let (status, _) = call(
        &env,
        &env.alice,
        "POST",
        "/v1/providers/claude/oauth/start",
        Some(json!({
            "challenge": "c".repeat(43), "state": "s".repeat(20), "redirect_uri": REDIRECT
        })),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, done) = call(
        &env,
        &env.alice,
        "POST",
        "/v1/providers/claude/signin/complete",
        Some(json!({"flow_id": "x", "code": "admin-code", "redirect_uri": REDIRECT})),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{done}");

    let (status, started) = call(
        &env,
        OWNER,
        "POST",
        "/v1/providers/claude/signin/start",
        Some(start),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{started}");
    let url = started["url"].as_str().unwrap();
    assert!(url.contains("code=true") && url.contains("code_challenge_method=S256"));
    let (status, done) = call(
        &env,
        OWNER,
        "POST",
        "/v1/providers/claude/signin/complete",
        Some(json!({
            "flow_id": started["flow_id"], "code": "admin-code", "redirect_uri": REDIRECT
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{done}");
    assert_eq!(done["created"], true);
    let text = done.to_string();
    assert!(!text.contains("plan-access") && !text.contains("plan-refresh"));
    // It is the admin's account, masked, and nobody else sees it.
    let (_, mine) = call(&env, OWNER, "GET", "/v1/providers/claude/accounts", None).await;
    let accounts = mine["accounts"].as_array().unwrap();
    assert_eq!(accounts.len(), 1);
    assert_eq!(accounts[0]["auth_kind"], "oauth");
    assert_eq!(accounts[0]["identity"]["email"], "p***@example.example");
    assert!(!mine.to_string().contains("plan-access"));
    for token in [&env.alice, &env.bob] {
        let (_, theirs) = call(&env, token, "GET", "/v1/providers/claude/accounts", None).await;
        assert!(theirs["accounts"].as_array().unwrap().is_empty());
    }
    // Members never get a token through the plan account: the admin cannot
    // share it with a grant either (a grant shares only the default key).
    let (status, granted) = call(
        &env,
        OWNER,
        "POST",
        "/v1/admin/grants",
        Some(json!({"grantee": env.alice_id, "provider": "claude"})),
    )
    .await;
    assert_ne!(status, StatusCode::OK, "{granted}");
    let (_, grants) = call(&env, OWNER, "GET", "/v1/admin/grants", None).await;
    assert!(grants["grants"].as_array().unwrap().is_empty());
}

#[tokio::test]
async fn the_plan_sign_in_is_off_unless_the_operator_turned_it_on() {
    let env = env().await;
    // `anthropic` has no `plan_oauth`: not even the admin can start one.
    let (status, body) = call(
        &env,
        OWNER,
        "POST",
        "/v1/providers/anthropic/signin/start",
        Some(json!({"flow": "pkce", "redirect_uri": REDIRECT})),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert_eq!(body["code"], "signin_unsupported");
    let (status, _) = call(
        &env,
        OWNER,
        "POST",
        "/v1/admin/providers/anthropic/oauth/start",
        Some(json!({
            "challenge": "c".repeat(43), "state": "s".repeat(20), "redirect_uri": REDIRECT
        })),
    )
    .await;
    assert_ne!(status, StatusCode::OK);
    let (_, accounts) = call(&env, OWNER, "GET", "/v1/providers/anthropic/accounts", None).await;
    assert!(accounts["accounts"].as_array().unwrap().is_empty());
}

#[tokio::test]
async fn listings_show_the_plan_sign_in_and_its_risk_only_to_the_admin() {
    let env = env().await;
    // A provider is listed once the caller holds an account on it.
    catalog_accepting(&env.server, "sk-ant-listed").await;
    for token in [OWNER, env.alice.as_str()] {
        for provider in ["claude", "anthropic"] {
            let (status, body) = call(
                &env,
                token,
                "POST",
                &format!("/v1/providers/{provider}/accounts"),
                Some(json!({"label": "Key", "api_key": "sk-ant-listed"})),
            )
            .await;
            assert_eq!(status, StatusCode::OK, "{body}");
        }
    }
    let (_, admin) = call(&env, OWNER, "GET", "/v1/providers", None).await;
    let claude = provider(&admin, "claude");
    assert_eq!(claude["signin"], json!(["pkce"]));
    let risk = claude["signin_risk"].as_str().unwrap();
    assert!(risk.contains("terms") && risk.contains("never shared"));
    assert_eq!(provider(&admin, "anthropic")["signin"], json!([]));
    assert!(provider(&admin, "anthropic")["signin_risk"].is_null());
    let (_, member) = call(&env, &env.alice, "GET", "/v1/providers", None).await;
    assert_eq!(provider(&member, "claude")["signin"], json!([]));
    assert!(provider(&member, "claude")["signin_risk"].is_null());
    // The admin's own listing of the provider's accounts agrees.
    let (_, flows) = call(&env, OWNER, "GET", "/v1/providers/claude/accounts", None).await;
    assert_eq!(flows["signin"], json!(["pkce"]));
    let (_, flows) = call(
        &env,
        &env.alice,
        "GET",
        "/v1/providers/claude/accounts",
        None,
    )
    .await;
    assert_eq!(flows["signin"], json!([]));
    assert!(flows["signin_risk"].is_null());
}
