//! GitHub Copilot accounts through the API (v0.51): the member's own device
//! sign-in, the risk banner, and the `plan_oauth` flag. A wiremock server stands
//! in for github.com and api.github.com.

use super::*;
use crate::auth_flows::copilot::CopilotSignIn;
use crate::auth_flows::{SignInRegistry, SignInService};
use crate::config::ProviderConfig;
use crate::model_router::ModelRouter;
use crate::providers::UserProviderCredentials;
use crate::user_secrets::UserSecrets;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const OWNER: &str = "test-token-that-is-long-enough-123";

struct Env {
    app: Router,
    server: MockServer,
    alice: String,
    bob: String,
}

async fn env() -> Env {
    let server = MockServer::start().await;
    let dir = std::env::temp_dir().join(format!("serabot-copilot-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let mut config = Config::default();
    config.api.enabled = true;
    config.api.token_hash = sha256_hex(OWNER);
    config.llm.model = "test".into();
    config.llm.base_url = "https://example.com/v1".into();
    config.memory.auto_extract = false;
    config.base_dir = dir.clone();
    // `copilot` has the sign-in on (the default); `copilot-off` turns it off.
    for (id, plan_oauth) in [("copilot", true), ("copilot-off", false)] {
        config.llm.providers.insert(
            id.into(),
            ProviderConfig {
                kind: "github-copilot".into(),
                api_key_secret: format!("{id}_access"),
                refresh_token_secret: format!("{id}_refresh"),
                base_url: server.uri(),
                models: vec!["claude-sonnet-5.5".into(), "gpt-6.1-sol".into()],
                data_egress_approved: true,
                plan_oauth,
                ..Default::default()
            },
        );
    }
    let database = dir.join("serabot.db");
    config.database.path = database.to_string_lossy().into_owned();
    let config = Arc::new(config);
    let store = Store::open(&database, 384, 5000).unwrap();
    let users = Arc::new(UserSecrets::with_master_key(store.clone(), [7u8; 32]).unwrap());
    let age = BTreeMap::from([(config.llm.api_key_secret.clone(), "synthetic-llm".into())]);
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
        secrets,
        users,
        None,
    )
    .unwrap()
    .with_refresher(refresher)
    .unwrap();
    let mut registry = SignInRegistry::default();
    registry.register(
        "github-copilot",
        Arc::new(CopilotSignIn::for_test(&server.uri())),
    );
    let signin = Arc::new(
        SignInService::new(state.accounts.clone())
            .unwrap()
            .with_registry(registry),
    );
    state = state.with_signin(signin);
    let alice_user = store
        .create_user(
            "copilot-alice",
            "copilot-alice@example.example",
            "copilot-alice",
        )
        .unwrap();
    let alice = mint_for(&store, &alice_user.id, "copilot-alice");
    let bob_user = store
        .create_user("copilot-bob", "copilot-bob@example.example", "copilot-bob")
        .unwrap();
    let bob = mint_for(&store, &bob_user.id, "copilot-bob");
    Env {
        app: router(state),
        server,
        alice,
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

async fn mount_github(server: &MockServer) {
    Mock::given(method("POST"))
        .and(path("/device"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "device_code": "dc-api-synthetic",
            "user_code": "WXYZ-9876",
            "verification_uri": "https://github.com/login/device",
            "interval": 1,
            "expires_in": 900
        })))
        .mount(server)
        .await;
    Mock::given(method("POST"))
        .and(path("/poll"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "access_token": "gho_api_synthetic", "token_type": "bearer"
        })))
        .mount(server)
        .await;
    Mock::given(method("GET"))
        .and(path("/copilot_internal/v2/token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "token": "tid=api-synthetic",
            "expires_at": chrono::Utc::now().timestamp() + 1800,
            "endpoints": {"api": "https://api.githubcopilot.com"}
        })))
        .mount(server)
        .await;
    Mock::given(method("GET"))
        .and(path("/user"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({"id": 99, "login": "api-user"})),
        )
        .mount(server)
        .await;
}

#[tokio::test]
async fn a_member_signs_in_with_the_github_device_code_and_the_account_is_theirs_alone() {
    let env = env().await;
    mount_github(&env.server).await;
    let (status, started) = call(
        &env,
        &env.alice,
        "POST",
        "/v1/providers/copilot/signin/start",
        Some(json!({"flow": "device"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{started}");
    assert_eq!(started["user_code"], "WXYZ-9876");
    assert_eq!(started["url"], "https://github.com/login/device");
    assert!(!started.to_string().contains("dc-api-synthetic"));
    let flow = started["flow_id"].as_str().unwrap().to_owned();
    let uri = format!("/v1/providers/copilot/signin/{flow}");
    // Another member finds no such flow.
    let (code, _) = call(&env, &env.bob, "GET", &uri, None).await;
    assert_eq!(code, StatusCode::NOT_FOUND);
    // The poller waits a second between polls; give it a few.
    let mut done = json!({});
    for _ in 0..40 {
        tokio::time::sleep(std::time::Duration::from_millis(250)).await;
        let (_, state) = call(&env, &env.alice, "GET", &uri, None).await;
        if state["status"] != "pending" {
            done = state;
            break;
        }
    }
    assert_eq!(done["status"], "ok", "{done}");
    let (_, accounts) = call(
        &env,
        &env.alice,
        "GET",
        "/v1/providers/copilot/accounts",
        None,
    )
    .await;
    assert_eq!(
        accounts["accounts"].as_array().unwrap().len(),
        1,
        "{accounts}"
    );
    let shown = accounts.to_string();
    assert!(!shown.contains("gho_api_synthetic") && !shown.contains("tid=api-synthetic"));
    let (_, theirs) = call(
        &env,
        &env.bob,
        "GET",
        "/v1/providers/copilot/accounts",
        None,
    )
    .await;
    assert!(theirs["accounts"].as_array().unwrap().is_empty());
}

#[tokio::test]
async fn copilot_has_no_browser_flow_and_takes_no_api_key() {
    let env = env().await;
    let (status, body) = call(
        &env,
        &env.alice,
        "POST",
        "/v1/providers/copilot/signin/start",
        Some(json!({"flow": "pkce", "redirect_uri": "http://127.0.0.1:51121/oauth-callback"})),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    let (status, body) = call(
        &env,
        &env.alice,
        "POST",
        "/v1/providers/copilot/accounts",
        Some(json!({"label": "Key", "api_key": "ghp-synthetic"})),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert_eq!(body["code"], "signin_required");
}

#[tokio::test]
async fn the_copilot_sign_in_is_off_when_the_operator_turned_it_off() {
    let env = env().await;
    for token in [OWNER, env.alice.as_str()] {
        let (status, body) = call(
            &env,
            token,
            "POST",
            "/v1/providers/copilot-off/signin/start",
            Some(json!({"flow": "device"})),
        )
        .await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
        assert_eq!(body["code"], "signin_unsupported");
    }
    let (_, flows) = call(
        &env,
        &env.alice,
        "GET",
        "/v1/providers/copilot-off/accounts",
        None,
    )
    .await;
    assert_eq!(flows["signin"], json!([]));
    assert!(flows["signin_risk"].is_null());
}

#[tokio::test]
async fn listings_show_the_device_flow_and_the_risk_banner() {
    let env = env().await;
    let (_, flows) = call(
        &env,
        &env.alice,
        "GET",
        "/v1/providers/copilot/accounts",
        None,
    )
    .await;
    assert_eq!(flows["signin"], json!(["device"]));
    let risk = flows["signin_risk"].as_str().unwrap();
    assert!(risk.contains("GitHub") && risk.contains("never shared"));
    // The admin sees the same offer.
    let (_, flows) = call(&env, OWNER, "GET", "/v1/providers/copilot/accounts", None).await;
    assert_eq!(flows["signin"], json!(["device"]));
}
