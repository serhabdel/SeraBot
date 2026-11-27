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
    #[allow(dead_code)]
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
    // `claude` has the plan sign-in on (the default); `anthropic` turns it off.
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
            plan_oauth: false,
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

#[allow(dead_code)]
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
async fn members_and_admin_are_refused_third_party_claude_oauth() {
    let env = env().await;
    let start = json!({"flow": "pkce", "redirect_uri": REDIRECT});
    for token in [OWNER, env.alice.as_str()] {
        let (status, body) = call(
            &env,
            token,
            "POST",
            "/v1/providers/claude/signin/start",
            Some(start.clone()),
        )
        .await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
        assert_eq!(body["code"], "claude_oauth_retired", "{body}");

        let (status, body) = call(
            &env,
            token,
            "POST",
            "/v1/providers/claude/oauth/start",
            Some(json!({
                "challenge": "c".repeat(43), "state": "s".repeat(20), "redirect_uri": REDIRECT
            })),
        )
        .await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
        assert_eq!(body["code"], "claude_oauth_retired", "{body}");

        let (status, done) = call(
            &env,
            token,
            "POST",
            "/v1/providers/claude/signin/complete",
            Some(json!({"flow_id": "x", "code": "admin-code", "redirect_uri": REDIRECT})),
        )
        .await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{done}");
        assert_eq!(done["code"], "claude_oauth_retired", "{done}");
    }
}

#[tokio::test]
async fn the_plan_sign_in_is_off_when_the_operator_turned_it_off() {
    let env = env().await;
    let (status, body) = call(
        &env,
        OWNER,
        "POST",
        "/v1/providers/anthropic/signin/start",
        Some(json!({"flow": "pkce", "redirect_uri": REDIRECT})),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert_eq!(body["code"], "claude_oauth_retired", "{body}");
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
async fn listings_hide_retired_claude_plan_sign_in() {
    let env = env().await;
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
    for token in [OWNER, env.alice.as_str()] {
        let (_, listed) = call(&env, token, "GET", "/v1/providers", None).await;
        assert_eq!(provider(&listed, "claude")["signin"], json!([]));
        assert!(provider(&listed, "claude")["signin_risk"].is_null());
        assert_eq!(provider(&listed, "anthropic")["signin"], json!([]));
        assert!(provider(&listed, "anthropic")["signin_risk"].is_null());
        let (_, flows) = call(&env, token, "GET", "/v1/providers/claude/accounts", None).await;
        assert_eq!(flows["signin"], json!([]));
        assert!(flows["signin_risk"].is_null());
    }
}

#[tokio::test]
async fn a_console_saved_claude_provider_accepts_keys_and_refuses_oauth() {
    let env = env().await;
    let (status, saved) = call(
        &env,
        OWNER,
        "POST",
        "/v1/admin/providers",
        Some(json!({
            "id": "claude-console", "kind": "anthropic",
            "base_url": "https://api.anthropic.com/v1",
            "models": ["claude-sonnet-5-5"],
            "api_key_secret": "claude_console_api_key", "catalog_fetch": true,
        })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{saved}");
    assert_eq!(saved["restart_required"], false, "{saved}");
    let (status, started) = call(
        &env,
        OWNER,
        "POST",
        "/v1/providers/claude-console/signin/start",
        Some(json!({"flow": "pkce", "redirect_uri": REDIRECT})),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{started}");
    assert_eq!(started["code"], "claude_oauth_retired", "{started}");
    let (_, admin) = call(&env, OWNER, "GET", "/v1/providers", None).await;
    let listed = provider(&admin, "claude-console");
    assert_eq!(listed["signin"], json!([]), "{listed}");
    assert_eq!(listed["plan_signin"], "off", "{listed}");
    assert_eq!(listed["pending_restart"], false, "{listed}");
}

#[tokio::test]
async fn the_accounts_listing_says_why_a_plan_sign_in_is_missing() {
    let env = env().await;
    let (_, off) = call(&env, OWNER, "GET", "/v1/providers/anthropic/accounts", None).await;
    assert_eq!(off["plan_signin"], "off", "{off}");
    assert_eq!(off["signin"], json!([]));
    let (_, claude) = call(&env, OWNER, "GET", "/v1/providers/claude/accounts", None).await;
    assert_eq!(claude["plan_signin"], "off", "{claude}");
    assert!(claude["signin_setup"].is_null(), "{claude}");
    assert_eq!(claude["pending_restart"], false, "{claude}");
}

/// Saving and removing a console provider reach the bots without a restart:
/// the provider is served as soon as it is saved and gone once removed.
#[tokio::test]
async fn console_provider_changes_apply_without_a_restart() {
    let env = env().await;
    let (status, saved) = call(
        &env,
        OWNER,
        "POST",
        "/v1/admin/providers",
        Some(json!({
            "id": "claude-live", "kind": "anthropic",
            "base_url": "https://api.anthropic.com/v1",
            "models": ["claude-haiku-5-5"],
            "api_key_secret": "claude_live_api_key", "catalog_fetch": false,
        })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{saved}");
    assert_eq!(saved["restart_required"], false, "{saved}");
    let (_, listed) = call(&env, OWNER, "GET", "/v1/providers", None).await;
    let live = provider(&listed, "claude-live");
    assert_eq!(live["managed"], true, "{live}");
    assert_eq!(live["pending_restart"], false, "{live}");
    // Saved again with another model: the router serves the new list at once.
    let (status, saved) = call(
        &env,
        OWNER,
        "POST",
        "/v1/admin/providers",
        Some(json!({
            "id": "claude-live", "kind": "anthropic",
            "base_url": "https://api.anthropic.com/v1",
            "models": ["claude-haiku-5-5", "claude-sonnet-5-5"],
            "api_key_secret": "claude_live_api_key", "catalog_fetch": false,
        })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{saved}");
    let (_, listed) = call(&env, OWNER, "GET", "/v1/providers", None).await;
    let live = provider(&listed, "claude-live");
    assert_eq!(
        live["models"],
        json!(["claude-haiku-5-5", "claude-sonnet-5-5"]),
        "{live}"
    );
    assert_eq!(live["pending_restart"], false, "{live}");
    // Removed: no longer served or listed, and no restart is asked for.
    let (status, removed) = call(
        &env,
        OWNER,
        "DELETE",
        "/v1/admin/providers/claude-live",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{removed}");
    assert_eq!(removed["restart_required"], false, "{removed}");
    let (_, listed) = call(&env, OWNER, "GET", "/v1/providers", None).await;
    assert!(
        listed["providers"]
            .as_array()
            .unwrap()
            .iter()
            .all(|provider| provider["id"] != "claude-live"),
        "{listed}"
    );
}

#[tokio::test]
async fn claude_agent_sdk_gateway_status_token_import_and_signin_flow() {
    let gw_server = MockServer::start().await;
    let dir = std::env::temp_dir().join(format!("serabot-gw-api-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let mut config = Config::default();
    config.api.enabled = true;
    config.api.token_hash = sha256_hex(OWNER);
    config.llm.model = "test".into();
    config.llm.base_url = "https://example.com/v1".into();
    config.memory.auto_extract = false;
    config.base_dir = dir.clone();
    config.llm.providers.insert(
        "claude-sdk".into(),
        ProviderConfig {
            kind: "claude-agent-sdk".into(),
            api_key_secret: "claude_gateway_token".into(),
            base_url: gw_server.uri(),
            models: vec!["claude-sonnet-4-5".into()],
            data_egress_approved: true,
            catalog_fetch: false,
            ..Default::default()
        },
    );
    let database = dir.join("serabot.db");
    config.database.path = database.to_string_lossy().into_owned();
    let config = Arc::new(config);
    let store = Store::open(&database, 384, 5000).unwrap();
    let users = Arc::new(UserSecrets::with_master_key(store.clone(), [8u8; 32]).unwrap());
    let mut secret_store =
        crate::secrets::SecretStore::open_paths(dir.join("secrets.age"), dir.join("identity.txt"))
            .unwrap();
    secret_store
        .set(&config.llm.api_key_secret, "synthetic-llm")
        .unwrap();
    secret_store.save().unwrap();
    let secrets = SharedSecrets::new(secret_store);
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
    let state = ApiState::new(
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
    let alice_user = store
        .create_user("gw-alice", "gw-alice@example.example", "gw-alice")
        .unwrap();
    let alice = mint_for(&store, &alice_user.id, "gw-alice");
    let app = router(state);

    let send = |token: String, method: &'static str, uri: String, body: Option<Value>| {
        let app = app.clone();
        async move {
            let response = app
                .oneshot(request_with_token(method, &uri, &token, body))
                .await
                .unwrap();
            let status = response.status();
            let bytes = axum::body::to_bytes(response.into_body(), 1 << 20)
                .await
                .unwrap();
            (
                status,
                serde_json::from_slice::<Value>(&bytes)
                    .unwrap_or_else(|_| json!({"raw": String::from_utf8_lossy(&bytes)})),
            )
        }
    };

    Mock::given(method("GET"))
        .and(path("/health"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "status": "ok",
            "cli_installed": true,
            "cli_version": "2.1.0",
            "sdk_installed": true,
        })))
        .mount(&gw_server)
        .await;

    // 1. Before token is set: gateway-status reports token_configured = false, signin/start returns gateway_token_missing.
    let (status, gw_status) = send(
        OWNER.into(),
        "GET",
        "/v1/providers/claude-sdk/gateway-status".into(),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{gw_status}");
    assert_eq!(gw_status["reachable"], true);
    assert_eq!(gw_status["token_configured"], false);
    assert_eq!(gw_status["auth_ok"], false);
    assert_eq!(gw_status["cli_installed"], true);

    let (status, missing) = send(
        OWNER.into(),
        "POST",
        "/v1/providers/claude-sdk/signin/start".into(),
        Some(json!({})),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{missing}");
    assert_eq!(missing["code"], "gateway_token_missing");

    // 2. Non-admin cannot generate token; admin can.
    let (status, _) = send(
        alice.clone(),
        "POST",
        "/v1/providers/claude-sdk/gateway-token".into(),
        Some(json!({"generate": true})),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    let (status, tok_resp) = send(
        OWNER.into(),
        "POST",
        "/v1/providers/claude-sdk/gateway-token".into(),
        Some(json!({"generate": true})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{tok_resp}");
    assert_eq!(tok_resp["ok"], true);
    let generated_token = tok_resp["token"].as_str().unwrap().to_owned();
    assert!(generated_token.starts_with("gw_"));
    assert!(
        tok_resp["startup_command"]
            .as_str()
            .unwrap()
            .contains(&generated_token)
    );

    // 3. With token set, gateway-status probes /v1/auth/status with bearer auth.
    Mock::given(method("GET"))
        .and(path("/v1/auth/status"))
        .and(header(
            "authorization",
            format!("Bearer {generated_token}").as_str(),
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "signed_in": true,
            "profiles": [{"profile": "default", "email": "owner@example.com"}],
            "default_cli_login": {"emailAddress": "cli@example.com"},
        })))
        .mount(&gw_server)
        .await;

    let (status, gw_status) = send(
        OWNER.into(),
        "GET",
        "/v1/providers/claude-sdk/gateway-status".into(),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{gw_status}");
    assert_eq!(gw_status["token_configured"], true);
    assert_eq!(gw_status["auth_ok"], true);
    assert_eq!(gw_status["profiles"].as_array().unwrap().len(), 1);

    // 4. Import existing CLI login.
    Mock::given(method("POST"))
        .and(path("/v1/auth/import-cli"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "ok": true,
            "profile": "default",
            "account": {"emailAddress": "cli@example.com", "subscriptionType": "max"},
        })))
        .mount(&gw_server)
        .await;

    let (status, imported) = send(
        OWNER.into(),
        "POST",
        "/v1/providers/claude-sdk/gateway-import-cli".into(),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{imported}");
    assert_eq!(imported["created"], true);

    // 5. Start sign-in without redirect_uri, poll pending, then complete with pasted code (no redirect_uri).
    Mock::given(method("POST"))
        .and(path("/v1/auth/start"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "flow_id": "gw-flow-1",
            "url": "https://claude.ai/oauth/authorize?code_challenge=xyz",
            "profile": "default",
            "expires_in": 600,
        })))
        .mount(&gw_server)
        .await;
    Mock::given(method("GET"))
        .and(path("/v1/auth/flow/gw-flow-1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "status": "pending",
        })))
        .mount(&gw_server)
        .await;
    Mock::given(method("POST"))
        .and(path("/v1/auth/complete"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "ok": true,
            "profile": "default",
            "account": {"emailAddress": "browser@example.com", "subscriptionType": "pro"},
        })))
        .mount(&gw_server)
        .await;

    let (status, started) = send(
        OWNER.into(),
        "POST",
        "/v1/providers/claude-sdk/signin/start".into(),
        Some(json!({})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{started}");
    let flow_id = started["flow_id"].as_str().unwrap().to_owned();
    assert_eq!(
        started["url"],
        "https://claude.ai/oauth/authorize?code_challenge=xyz"
    );

    let (status, poll) = send(
        OWNER.into(),
        "GET",
        format!("/v1/providers/claude-sdk/signin/{flow_id}"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{poll}");
    assert_eq!(poll["status"], "pending");

    let (status, completed) = send(
        OWNER.into(),
        "POST",
        "/v1/providers/claude-sdk/signin/complete".into(),
        Some(json!({"flow_id": flow_id, "code": "pasted-auth-code"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{completed}");
    assert_eq!(completed["created"], true);
}
