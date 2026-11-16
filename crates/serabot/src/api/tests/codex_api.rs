//! Codex (ChatGPT plan) accounts through the API (v0.51): the member's own
//! browser and device sign-ins, the risk banner, and the `plan_oauth` flag. A
//! wiremock server stands in for auth.openai.com.

use super::*;
use crate::auth_flows::codex::{CodexSignIn, DEVICE_VERIFY_URL};
use crate::auth_flows::{SignInRegistry, SignInService};
use crate::config::ProviderConfig;
use crate::model_router::ModelRouter;
use crate::providers::UserProviderCredentials;
use crate::user_secrets::UserSecrets;
use base64::Engine;
use wiremock::matchers::{body_string_contains, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const REDIRECT: &str = "http://localhost:1455/auth/callback";
const OWNER: &str = "test-token-that-is-long-enough-123";

struct Env {
    app: Router,
    server: MockServer,
    alice: String,
    bob: String,
    bob_id: String,
}

async fn env() -> Env {
    let server = MockServer::start().await;
    let dir = std::env::temp_dir().join(format!("serabot-codex-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let mut config = Config::default();
    config.api.enabled = true;
    config.api.token_hash = sha256_hex(OWNER);
    config.llm.model = "test".into();
    config.llm.base_url = "https://example.com/v1".into();
    config.memory.auto_extract = false;
    config.base_dir = dir.clone();
    let models: Vec<String> = crate::auth_flows::codex::MODELS
        .iter()
        .map(|model| (*model).to_owned())
        .collect();
    // `codex` has the plan sign-in on (the default); `codex-off` turns it off.
    for (id, plan_oauth) in [("codex", true), ("codex-off", false)] {
        config.llm.providers.insert(
            id.into(),
            ProviderConfig {
                kind: "openai-codex".into(),
                api_key_secret: format!("{id}_access"),
                refresh_token_secret: format!("{id}_refresh"),
                base_url: format!("{}/backend-api/codex", server.uri()),
                models: models.clone(),
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
        "openai-codex",
        Arc::new(CodexSignIn::for_test(&server.uri())),
    );
    let signin = Arc::new(
        SignInService::new(state.accounts.clone())
            .unwrap()
            .with_registry(registry),
    );
    state = state.with_signin(signin);
    let alice_user = store
        .create_user("codex-alice", "codex-alice@example.example", "codex-alice")
        .unwrap();
    let alice = mint_for(&store, &alice_user.id, "codex-alice");
    let bob_user = store
        .create_user("codex-bob", "codex-bob@example.example", "codex-bob")
        .unwrap();
    let bob = mint_for(&store, &bob_user.id, "codex-bob");
    Env {
        app: router(state),
        server,
        alice,
        bob,
        bob_id: bob_user.id,
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

fn jwt(claims: Value) -> String {
    let part =
        |value: &Value| base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(value.to_string());
    format!("{}.{}.sig", part(&json!({"alg": "none"})), part(&claims))
}

/// A token reply naming a ChatGPT account.
async fn plan_token(server: &MockServer, code: &str, account: &str) {
    let exp = chrono::Utc::now().timestamp() + 7_200;
    let auth = json!({"chatgpt_account_id": account, "chatgpt_plan_type": "pro"});
    Mock::given(method("POST"))
        .and(path("/token"))
        .and(body_string_contains(format!("code={code}")))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "access_token": jwt(json!({
                "exp": exp, "https://api.openai.com/auth": auth.clone()
            })),
            "id_token": jwt(json!({
                "email": "Plan@Example.Example", "https://api.openai.com/auth": auth
            })),
            "refresh_token": format!("codex-refresh-{code}"),
            "expires_in": 3600
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
async fn a_member_signs_in_with_their_own_chatgpt_plan() {
    let env = env().await;
    plan_token(&env.server, "alice-code", "acct-alice").await;
    plan_token(&env.server, "alice-two", "acct-two").await;
    let (status, started) = call(
        &env,
        &env.alice,
        "POST",
        "/v1/providers/codex/signin/start",
        Some(json!({"flow": "pkce", "redirect_uri": REDIRECT})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{started}");
    let url = started["url"].as_str().unwrap();
    assert!(url.contains("id_token_add_organizations=true") && url.contains("S256"));
    // Bob cannot finish Alice's flow.
    let finish = |flow: &Value, code: &str| json!({"flow_id": flow["flow_id"], "code": code, "redirect_uri": REDIRECT});
    let (status, _) = call(
        &env,
        &env.bob,
        "POST",
        "/v1/providers/codex/signin/complete",
        Some(finish(&started, "alice-code")),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, done) = call(
        &env,
        &env.alice,
        "POST",
        "/v1/providers/codex/signin/complete",
        Some(finish(&started, "alice-code")),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{done}");
    assert_eq!(done["created"], true);
    let text = done.to_string();
    assert!(!text.contains("codex-refresh") && !text.contains("alice-code"));
    // A second ChatGPT workspace is a second account for the same member.
    let (_, second) = call(
        &env,
        &env.alice,
        "POST",
        "/v1/providers/codex/signin/start",
        Some(json!({"flow": "pkce", "redirect_uri": REDIRECT})),
    )
    .await;
    let (status, done) = call(
        &env,
        &env.alice,
        "POST",
        "/v1/providers/codex/signin/complete",
        Some(finish(&second, "alice-two")),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{done}");
    let (_, mine) = call(
        &env,
        &env.alice,
        "GET",
        "/v1/providers/codex/accounts",
        None,
    )
    .await;
    let accounts = mine["accounts"].as_array().unwrap();
    assert_eq!(accounts.len(), 2);
    assert!(
        accounts
            .iter()
            .all(|account| account["auth_kind"] == "oauth")
    );
    assert_eq!(accounts[0]["identity"]["email"], "p***@example.example");
    assert_eq!(accounts[0]["identity"]["plan"], "ChatGPT pro");
    // The workspace id and every token stay out of the listing.
    let shown = mine.to_string();
    for secret in ["acct-alice", "codex-refresh", "access"] {
        assert!(!shown.contains(secret), "{secret}");
    }
    // Nobody else sees the accounts, and the admin cannot grant them away.
    let (_, theirs) = call(&env, &env.bob, "GET", "/v1/providers/codex/accounts", None).await;
    assert!(theirs["accounts"].as_array().unwrap().is_empty());
    let (status, granted) = call(
        &env,
        OWNER,
        "POST",
        "/v1/admin/grants",
        Some(json!({"grantee": env.bob_id, "provider": "codex"})),
    )
    .await;
    assert_ne!(status, StatusCode::OK, "{granted}");
}

#[tokio::test]
async fn the_device_sign_in_returns_the_code_and_the_openai_page() {
    let env = env().await;
    Mock::given(method("POST"))
        .and(path("/usercode"))
        .respond_with(ResponseTemplate::new(200).set_body_json(
            json!({"device_auth_id": "dev-auth-api", "user_code": "WXYZ-9876", "interval": 5}),
        ))
        .mount(&env.server)
        .await;
    let (status, started) = call(
        &env,
        &env.alice,
        "POST",
        "/v1/providers/codex/signin/start",
        Some(json!({"flow": "device"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{started}");
    assert_eq!(started["user_code"], "WXYZ-9876");
    assert_eq!(started["url"], DEVICE_VERIFY_URL);
    assert!(!started.to_string().contains("dev-auth-api"));
    // Still waiting; another member finds no such flow.
    let flow = started["flow_id"].as_str().unwrap();
    let uri = format!("/v1/providers/codex/signin/{flow}");
    let (_, status) = call(&env, &env.alice, "GET", &uri, None).await;
    assert_eq!(status["status"], "pending");
    let (code, _) = call(&env, &env.bob, "GET", &uri, None).await;
    assert_eq!(code, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn an_api_key_is_not_valid_for_the_plan_provider() {
    let env = env().await;
    let (status, body) = call(
        &env,
        &env.alice,
        "POST",
        "/v1/providers/codex/accounts",
        Some(json!({"label": "Key", "api_key": "sk-synthetic"})),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert_eq!(body["code"], "signin_required");
}

#[tokio::test]
async fn the_plan_sign_in_is_off_when_the_operator_turned_it_off() {
    let env = env().await;
    for (uri, body) in [
        (
            "/v1/providers/codex-off/signin/start",
            json!({"flow": "pkce", "redirect_uri": REDIRECT}),
        ),
        (
            "/v1/providers/codex-off/signin/start",
            json!({"flow": "device"}),
        ),
    ] {
        for token in [OWNER, env.alice.as_str()] {
            let (status, body) = call(&env, token, "POST", uri, Some(body.clone())).await;
            assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
            assert_eq!(body["code"], "signin_unsupported");
        }
    }
}

#[tokio::test]
async fn listings_show_the_sign_in_flows_and_the_risk_banner_unless_it_is_off() {
    let env = env().await;
    plan_token(&env.server, "listed", "acct-listed").await;
    // A provider is listed once the caller holds an account on it.
    let (_, started) = call(
        &env,
        &env.alice,
        "POST",
        "/v1/providers/codex/signin/start",
        Some(json!({"flow": "pkce", "redirect_uri": REDIRECT})),
    )
    .await;
    let (status, done) = call(
        &env,
        &env.alice,
        "POST",
        "/v1/providers/codex/signin/complete",
        Some(json!({
            "flow_id": started["flow_id"], "code": "listed", "redirect_uri": REDIRECT
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{done}");
    let (_, listed) = call(&env, &env.alice, "GET", "/v1/providers", None).await;
    let codex = provider(&listed, "codex");
    assert_eq!(codex["auth"], "oauth");
    assert_eq!(codex["kind"], "openai-codex");
    assert_eq!(codex["signin"], json!(["pkce", "device"]));
    let risk = codex["signin_risk"].as_str().unwrap();
    assert!(risk.contains("Codex client") && risk.contains("never shared"));
    assert!(risk.contains("API keys"));
    // The admin sees the same offer, and the account list agrees.
    let (_, flows) = call(&env, OWNER, "GET", "/v1/providers/codex/accounts", None).await;
    assert_eq!(flows["signin"], json!(["pkce", "device"]));
    // The provider that turned it off offers nothing and shows no banner.
    let (_, flows) = call(
        &env,
        &env.alice,
        "GET",
        "/v1/providers/codex-off/accounts",
        None,
    )
    .await;
    assert_eq!(flows["signin"], json!([]));
    assert!(flows["signin_risk"].is_null());
}

/// What the desktop's Add provider sheet posts for the ChatGPT (Codex) card: no
/// key, no refresh token. The owner's save must work with exactly that, and a
/// member's sign-in must start at once, with no restart.
#[tokio::test]
async fn a_console_saved_codex_provider_signs_in_without_secrets_or_restart() {
    let env = env().await;
    let (status, saved) = call(
        &env,
        OWNER,
        "POST",
        "/v1/admin/providers",
        Some(json!({
            "id": "codex-console", "kind": "openai-codex",
            "base_url": "https://chatgpt.com/backend-api/codex",
            "models": crate::auth_flows::codex::MODELS,
            "api_key_secret": "codex_access_token", "catalog_fetch": false,
        })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{saved}");
    let (status, started) = call(
        &env,
        &env.alice,
        "POST",
        "/v1/providers/codex-console/signin/start",
        Some(json!({"flow": "pkce", "redirect_uri": REDIRECT})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{started}");
    assert!(
        started["url"]
            .as_str()
            .unwrap()
            .contains("id_token_add_organizations=true")
    );
    // A saved provider is a sign-in provider, never a key one.
    let (status, flows) = call(
        &env,
        &env.alice,
        "GET",
        "/v1/providers/codex-console/accounts",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{flows}");
    assert_eq!(flows["signin"], json!(["pkce", "device"]));
    let (status, refused) = call(
        &env,
        &env.alice,
        "POST",
        "/v1/providers/codex-console/accounts",
        Some(json!({"label": "Key", "api_key": "sk-synthetic"})),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{refused}");
}
