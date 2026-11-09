use super::*;
use crate::config::{BotConfig, LoadedBot};
use crate::gateway::{Gateway, ToolRegistry};
use crate::llm::LlmClient;
use crate::policy::Policy;
use crate::runtime::{NullNotifier, Runtime};
use axum::body::Body;
use axum::http::Request;
use tower::ServiceExt;

mod big_wave_b3_b4;
mod browser_api;
mod cards_api;
mod chat_api;
mod composio_api;
mod files_api;
mod jobs_api;
mod logs_api;
mod personal_api;
mod preview_api;
mod profiles_api;
mod standing_grants_api;
mod status_api;
mod vault_http;
mod wave_a_visibility_api;
mod wave_b_routines_api;

/// The bootstrap admin's id â€” the acting user behind the config token
/// the tests use.
fn admin(store: &Store) -> String {
    store.admin_user_id().unwrap()
}

fn app() -> (Router, Arc<Config>, Store) {
    app_with_policy(true, Policy::empty())
}

fn app_with_policy(egress_approved: bool, policy: Policy) -> (Router, Arc<Config>, Store) {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .map(std::path::Path::to_path_buf)
        .unwrap_or_else(|| std::env::current_dir().unwrap());
    app_with_root(egress_approved, policy, root)
}

fn app_with_root(
    egress_approved: bool,
    policy: Policy,
    base_dir: std::path::PathBuf,
) -> (Router, Arc<Config>, Store) {
    app_with_fleet(
        egress_approved,
        policy,
        base_dir,
        vec![test_bot(
            "main",
            crate::orchestration::BotRole::Orchestrator,
        )],
    )
}

fn test_bot(name: &str, role: crate::orchestration::BotRole) -> LoadedBot {
    LoadedBot {
        config: BotConfig {
            title: None,
            description: None,
            name: name.into(),
            trigger: "command".into(),
            model: None,
            system_prompt: format!("prompts/{name}.txt"),
            skills: vec![],
            tools: vec![],
            require_approval: vec![],
            enabled: true,
            temperature: None,
            thinking: None,
            role,
            delegates: vec![],
        },
        prompt: "test prompt".into(),
    }
}

fn app_with_fleet(
    egress_approved: bool,
    policy: Policy,
    base_dir: std::path::PathBuf,
    bots: Vec<LoadedBot>,
) -> (Router, Arc<Config>, Store) {
    app_with_auth_stack(egress_approved, policy, base_dir, bots, None, None)
}

/// Full app stack, optionally with sign-in enabled and a custom
/// per-minute rate limit. The auth service is built against the same
/// store as the API and pointed at `auth_issuer` (a loopback mock
/// identity provider in tests).
fn app_with_auth_stack(
    egress_approved: bool,
    policy: Policy,
    base_dir: std::path::PathBuf,
    bots: Vec<LoadedBot>,
    auth_issuer: Option<&str>,
    rate_limit: Option<u32>,
) -> (Router, Arc<Config>, Store) {
    app_with_stack(
        egress_approved,
        policy,
        base_dir,
        bots,
        auth_issuer,
        rate_limit,
        None,
        None,
    )
}

/// `composio_base` points Composio at a mock upstream; `None` keeps the default.
#[allow(clippy::too_many_arguments)]
fn app_with_stack(
    egress_approved: bool,
    policy: Policy,
    base_dir: std::path::PathBuf,
    bots: Vec<LoadedBot>,
    auth_issuer: Option<&str>,
    rate_limit: Option<u32>,
    browser: Option<Arc<crate::browser::BrowserManager>>,
    composio_base: Option<&str>,
) -> (Router, Arc<Config>, Store) {
    let mut config = Config::default();
    config.api.enabled = true;
    config.api.token_hash = sha256_hex("test-token-that-is-long-enough-123");
    if let Some(limit) = rate_limit {
        config.server.rate_limit_per_minute = limit;
        config.server.user_rate_limit_per_minute = limit;
        config.server.poll_rate_limit_per_minute = limit;
    }
    config.llm.data_egress_approved = egress_approved;
    config.llm.model = "test".into();
    config.memory.auto_extract = false;
    config.llm.base_url = "https://example.com/v1".into();
    config.llm.models = vec!["test-alternative".into()];
    if auth_issuer.is_some() {
        config.auth.enabled = true;
        config.auth.providers = vec!["microsoft".into()];
        config.auth.base_url = "https://bot.example.example".into();
        config.auth.max_users = 10;
    }
    config.base_dir = base_dir.canonicalize().unwrap_or_else(|_| base_dir.clone());
    let directory = std::env::temp_dir().join(format!("serabot-api-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&directory).unwrap();
    let database = directory.join("serabot.db");
    config.database.path = database.to_string_lossy().into_owned();
    let config = Arc::new(config);
    let store = Store::open(&database, 384, 5000).unwrap();
    let gateway = Gateway::new(
        store.clone(),
        Arc::new(policy),
        ToolRegistry::default(),
        1,
        false,
    );
    let llm = LlmClient::new("https://example.com/v1", "test", 5, 50)
        .unwrap()
        .with_model("test");
    let runtime = Runtime::new(
        config.clone(),
        store.clone(),
        gateway,
        crate::model_router::ModelRouter::new(
            &config.llm,
            std::collections::BTreeMap::from([("default".into(), llm)]),
        )
        .unwrap(),
        bots,
        Arc::new(NullNotifier),
        tokio_util::sync::CancellationToken::new(),
    )
    .unwrap();
    let artifacts = Arc::new(
        ArtifactStore::new(
            std::env::temp_dir().join(format!("serabot-api-test-{}", uuid::Uuid::new_v4())),
        )
        .unwrap(),
    );
    let mut secrets = std::collections::BTreeMap::from([(
        config.llm.api_key_secret.clone(),
        "synthetic-test-key".to_string(),
    )]);
    if auth_issuer.is_some() {
        secrets.insert(
            config.auth.client_id_secret.clone(),
            crate::auth::test_idp::TEST_CLIENT_ID.to_string(),
        );
    }
    let secrets = SharedSecrets::new(crate::secrets::SecretStore::from_map(secrets));
    let users = Arc::new(
        crate::user_secrets::UserSecrets::with_master_key(store.clone(), [9u8; 32]).unwrap(),
    );
    let composio_users = users.clone();
    let fleet = runtime.fleet();
    let auth = match auth_issuer {
        Some(issuer) => {
            let mut service = AuthService::new(&config, store.clone(), &secrets)
                .unwrap()
                .with_fleet(fleet);
            service.set_issuer_url(issuer).unwrap();
            Some(service)
        }
        None => None,
    };
    let mut state = ApiState::new(
        runtime,
        store.clone(),
        config.clone(),
        artifacts,
        secrets,
        users,
        None,
    )
    .unwrap()
    .with_browser(browser)
    .with_onboarding_drafter(Some(Arc::new(profiles_api::ScriptedDrafter)));
    if let Some(base) = composio_base {
        let mut settings = config.composio.clone();
        settings.api_base = base.to_owned();
        state.composio = Arc::new(
            crate::composio::ComposioService::with_settings(&settings, composio_users).unwrap(),
        );
    }
    if let Some(service) = auth {
        state.auth = Some(Arc::new(service));
    }
    (router(state), config, store)
}

/// Same stack, also returning the artifact store so a test can save
/// synthetic artifacts into the same store the handlers read.
fn app_with_artifacts(
    egress_approved: bool,
    policy: Policy,
    base_dir: std::path::PathBuf,
) -> (Router, Arc<Config>, Store, Arc<ArtifactStore>) {
    let mut config = Config::default();
    config.api.enabled = true;
    config.api.token_hash = sha256_hex("test-token-that-is-long-enough-123");
    config.llm.data_egress_approved = egress_approved;
    config.llm.model = "test".into();
    config.memory.auto_extract = false;
    config.llm.base_url = "https://example.com/v1".into();
    config.llm.models = vec!["test-alternative".into()];
    config.base_dir = base_dir.canonicalize().unwrap_or_else(|_| base_dir.clone());
    let directory = std::env::temp_dir().join(format!("serabot-api-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&directory).unwrap();
    config.database.path = directory.join("serabot.db").to_string_lossy().into_owned();
    let config = Arc::new(config);
    let store = Store::open(&config.database.path, 384, 5000).unwrap();
    let gateway = Gateway::new(
        store.clone(),
        Arc::new(policy),
        ToolRegistry::default(),
        1,
        false,
    );
    let llm = LlmClient::new("https://example.com/v1", "test", 5, 50)
        .unwrap()
        .with_model("test");
    let runtime = Runtime::new(
        config.clone(),
        store.clone(),
        gateway,
        crate::model_router::ModelRouter::new(
            &config.llm,
            std::collections::BTreeMap::from([("default".into(), llm)]),
        )
        .unwrap(),
        vec![test_bot(
            "main",
            crate::orchestration::BotRole::Orchestrator,
        )],
        Arc::new(NullNotifier),
        tokio_util::sync::CancellationToken::new(),
    )
    .unwrap();
    let artifacts = Arc::new(
        ArtifactStore::new(
            std::env::temp_dir().join(format!("serabot-api-test-{}", uuid::Uuid::new_v4())),
        )
        .unwrap(),
    );
    let secrets = SharedSecrets::new(crate::secrets::SecretStore::from_map(
        std::collections::BTreeMap::from([(
            config.llm.api_key_secret.clone(),
            "synthetic-test-key".to_string(),
        )]),
    ));
    let users = Arc::new(
        crate::user_secrets::UserSecrets::with_master_key(store.clone(), [9u8; 32]).unwrap(),
    );
    let state = ApiState::new(
        runtime,
        store.clone(),
        config.clone(),
        artifacts.clone(),
        secrets,
        users,
        None,
    )
    .unwrap();
    (router(state), config, store, artifacts)
}

/// An app with sign-in enabled and the auth service pointed at a
/// synthetic mock identity provider, for exercising the auth endpoints.
async fn auth_app() -> (Router, Arc<Config>, Store, crate::auth::test_idp::MockIdp) {
    let idp = crate::auth::test_idp::mock_idp().await;
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .map(std::path::Path::to_path_buf)
        .unwrap_or_else(|| std::env::current_dir().unwrap());
    let (app, config, store) = app_with_auth_stack(
        true,
        Policy::empty(),
        root,
        vec![test_bot(
            "main",
            crate::orchestration::BotRole::Orchestrator,
        )],
        Some(&idp.issuer),
        None,
    );
    (app, config, store, idp)
}

/// An app whose per-minute API limit is low enough to exercise the
/// per-user buckets without hundreds of requests.
fn app_with_rate_limit(limit: u32) -> (Router, Arc<Config>, Store) {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .map(std::path::Path::to_path_buf)
        .unwrap_or_else(|| std::env::current_dir().unwrap());
    app_with_auth_stack(
        true,
        Policy::empty(),
        root,
        vec![test_bot(
            "main",
            crate::orchestration::BotRole::Orchestrator,
        )],
        None,
        Some(limit),
    )
}

fn request(
    _app: &Router,
    method: &str,
    uri: &str,
    auth: bool,
    body: Option<Value>,
) -> Request<Body> {
    let mut builder = Request::builder().method(method).uri(uri);
    if auth {
        builder = builder.header(
            "authorization",
            format!("Bearer {}", "test-token-that-is-long-enough-123"),
        );
    }
    builder = builder.header("content-type", "application/json");
    builder
        .body(
            body.map(|value| Body::from(value.to_string()))
                .unwrap_or_else(Body::empty),
        )
        .unwrap()
}

fn request_with_token(method: &str, uri: &str, token: &str, body: Option<Value>) -> Request<Body> {
    Request::builder()
        .method(method)
        .uri(uri)
        .header("authorization", format!("Bearer {token}"))
        .header("content-type", "application/json")
        .body(
            body.map(|value| Body::from(value.to_string()))
                .unwrap_or_else(Body::empty),
        )
        .unwrap()
}

/// Mint a device token owned by a specific user (the CLI helper always
/// mints for the admin).
fn mint_for(store: &Store, user_id: &str, label: &str) -> String {
    let mut bytes = [0u8; crate::token_cli::TOKEN_BYTES];
    rand::rngs::OsRng.fill_bytes(&mut bytes);
    let token = hex::encode(bytes);
    store
        .insert_device_token(
            &crate::domain::new_id(),
            label,
            &sha256_hex(&token),
            Some(user_id),
            None,
        )
        .unwrap();
    token
}

#[tokio::test]
async fn requires_owner_token() {
    let (app, _, _) = app();
    let router = app.clone();
    let response = router
        .oneshot(request(&app, "GET", "/v1/health", false, None))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn accepts_owner_token_and_reports_health() {
    let (app, _, store) = app();
    let response = app
        .clone()
        .oneshot(request(&app, "GET", "/v1/health", true, None))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = http_body_util::BodyExt::collect(response.into_body())
        .await
        .unwrap()
        .to_bytes();
    let text = String::from_utf8_lossy(&bytes);
    assert!(!text.contains("database"));
    assert!(!text.contains(&store.path().display().to_string()));
    let value: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(value["ok"], true);
    assert_eq!(value["version"], env!("CARGO_PKG_VERSION"));
    assert_eq!(value["build"], crate::web::build_sha());
}

#[tokio::test]
async fn accepts_minted_device_tokens_and_records_use() {
    let (app, _, store) = app();
    let token = crate::token_cli::mint(&store, "api-test", None).unwrap();
    let response = app
        .clone()
        .oneshot(request_with_token("GET", "/v1/health", &token, None))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let listed = store.list_device_tokens().unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].label, "api-test");
    assert!(listed[0].last_used_at.is_some());
}

#[tokio::test]
async fn rejects_revoked_expired_and_unknown_device_tokens() {
    let (app, _, store) = app();
    let revoked = crate::token_cli::mint(&store, "revoked", None).unwrap();
    let record = store
        .device_token_by_hash(&sha256_hex(&revoked))
        .unwrap()
        .unwrap();
    assert!(store.revoke_device_token(&record.id).unwrap());
    let expired = crate::token_cli::mint(&store, "expired", Some(0)).unwrap();
    for token in [
        revoked,
        expired,
        "unknown-token-that-is-long-enough".to_string(),
    ] {
        let response = app
            .clone()
            .oneshot(request_with_token("GET", "/v1/health", &token, None))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED, "{token}");
    }
}

/// The per-minute limit is per acting user, not one global counter:
/// exhausting the admin's budget leaves a member's session untouched, and
/// a second admin session shares the admin's budget instead of doubling
/// it.
#[tokio::test]
async fn rate_limit_is_per_user_not_global() {
    let (app, _, store) = app_with_rate_limit(1);
    let admin_id = admin(&store);
    let member = store
        .create_user("microsoft:rate-member", "r@example.example", "R")
        .unwrap();
    let admin_token = mint_for(&store, &admin_id, "admin-session");
    let member_token = mint_for(&store, &member.id, "member-session");

    let first = app
        .clone()
        .oneshot(request_with_token("GET", "/v1/status", &admin_token, None))
        .await
        .unwrap();
    assert_eq!(first.status(), StatusCode::OK);
    let second = app
        .clone()
        .oneshot(request_with_token("GET", "/v1/status", &admin_token, None))
        .await
        .unwrap();
    assert_eq!(second.status(), StatusCode::TOO_MANY_REQUESTS);

    // A second admin session shares the admin's budget.
    let second_admin_session = mint_for(&store, &admin_id, "admin-session-2");
    let shared = app
        .clone()
        .oneshot(request_with_token(
            "GET",
            "/v1/status",
            &second_admin_session,
            None,
        ))
        .await
        .unwrap();
    assert_eq!(shared.status(), StatusCode::TOO_MANY_REQUESTS);

    // The member's budget is independent of the admin's.
    let member_response = app
        .clone()
        .oneshot(request_with_token("GET", "/v1/status", &member_token, None))
        .await
        .unwrap();
    assert_eq!(member_response.status(), StatusCode::OK);
}

/// Polling reads have their own bucket: exhausting it never blocks real
/// actions, and a flood of actions never blocks polling.
#[tokio::test]
async fn polling_reads_use_a_separate_bucket() {
    let (app, _, store) = app_with_rate_limit(2);
    let admin_id = admin(&store);
    let token = mint_for(&store, &admin_id, "poll-session");
    for _ in 0..2 {
        let r = app
            .clone()
            .oneshot(request_with_token("GET", "/v1/health", &token, None))
            .await
            .unwrap();
        assert_eq!(r.status(), StatusCode::OK);
    }
    let limited = app
        .clone()
        .oneshot(request_with_token("GET", "/v1/health", &token, None))
        .await
        .unwrap();
    assert_eq!(limited.status(), StatusCode::TOO_MANY_REQUESTS);
    assert!(limited.headers().contains_key("retry-after"));
    let action = app
        .clone()
        .oneshot(request_with_token("GET", "/v1/status", &token, None))
        .await
        .unwrap();
    assert_eq!(action.status(), StatusCode::OK);
}

#[test]
fn poll_route_classification() {
    use axum::http::Method;
    assert!(is_poll_request(&Method::GET, "/v1/browser/screenshot"));
    assert!(is_poll_request(
        &Method::GET,
        "/v1/conversations/c1/messages"
    ));
    assert!(!is_poll_request(&Method::POST, "/v1/messages"));
    assert!(!is_poll_request(&Method::GET, "/v1/memories"));
}

#[tokio::test]
async fn serves_public_landing_page_without_authentication() {
    let (app, _, _) = app();
    for path in ["/", "/app"] {
        let response = app
            .clone()
            .oneshot(request(&app, "GET", path, false, None))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response
                .headers()
                .get("content-security-policy")
                .and_then(|value| value.to_str().ok()),
            Some(crate::web::CSP)
        );
        assert_eq!(
            response
                .headers()
                .get("content-type")
                .and_then(|value| value.to_str().ok()),
            Some("text/html; charset=utf-8")
        );
        assert_eq!(
            response
                .headers()
                .get("x-content-type-options")
                .and_then(|value| value.to_str().ok()),
            Some("nosniff")
        );
        let bytes = http_body_util::BodyExt::collect(response.into_body())
            .await
            .unwrap()
            .to_bytes();
        let body = String::from_utf8(bytes.to_vec()).unwrap();
        assert!(!body.contains("<script"));
        assert!(body.contains("SeraBot"));
        assert!(body.contains("Invitation-only"));
    }
}

#[tokio::test]
async fn public_downloads_redirect_or_report_unconfigured() {
    let (app, _, _) = app();
    for (path, asset) in [
        ("/app/latest.apk", crate::web::APK_ASSET),
        ("/app/SHA256SUMS", crate::web::SHA256SUMS_ASSET),
    ] {
        let response = app
            .clone()
            .oneshot(request(&app, "GET", path, false, None))
            .await
            .unwrap();
        match crate::web::releases_repo() {
            Some(repo) => {
                assert_eq!(response.status(), StatusCode::FOUND);
                let expected =
                    format!("https://github.com/{repo}/releases/latest/download/{asset}");
                assert_eq!(
                    response
                        .headers()
                        .get("location")
                        .and_then(|value| value.to_str().ok()),
                    Some(expected.as_str())
                );
                assert_eq!(
                    response
                        .headers()
                        .get("cache-control")
                        .and_then(|value| value.to_str().ok()),
                    Some("no-store")
                );
            }
            None => assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE),
        }
    }
}

#[tokio::test]
async fn public_health_reports_build_without_database_details() {
    let (app, _, store) = app();
    let response = app
        .clone()
        .oneshot(request(&app, "GET", "/healthz", false, None))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = http_body_util::BodyExt::collect(response.into_body())
        .await
        .unwrap()
        .to_bytes();
    let text = String::from_utf8_lossy(&bytes);
    assert!(!text.contains("database"));
    assert!(!text.contains(&store.path().display().to_string()));
    let value: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(value["ok"], true);
    assert_eq!(value["version"], env!("CARGO_PKG_VERSION"));
    assert_eq!(value["build"], crate::web::build_sha());
    assert!(value.get("database").is_none());
}

#[tokio::test]
async fn enqueues_owner_message() {
    let (app, _, store) = app();
    let response = app
        .clone()
        .oneshot(request(
            &app,
            "POST",
            "/v1/messages",
            true,
            Some(json!({"conversation_id": "api-owner", "text": "hello"})),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(store.pending_task_count(&admin(&store)).unwrap(), 1);
}

#[tokio::test]
async fn rejects_wrong_token() {
    let (app, _, _) = app();
    let request = Request::builder()
        .method("GET")
        .uri("/v1/health")
        .header(
            "authorization",
            "Bearer wrong-token-that-is-also-long-enough",
        )
        .body(Body::empty())
        .unwrap();
    let response = app.oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn rejects_invalid_conversation_identifier() {
    let (app, _, _) = app();
    let response = app
        .clone()
        .oneshot(request(
            &app,
            "POST",
            "/v1/messages",
            true,
            Some(json!({"conversation_id": "../etc", "text": "hello"})),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
}

#[tokio::test]
async fn decides_unknown_approval_as_not_found() {
    let (app, _, _) = app();
    let response = app
        .clone()
        .oneshot(request(
            &app,
            "POST",
            "/v1/approvals/missing/decide",
            true,
            Some(json!({"decision": "deny"})),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn selection_is_persistent_and_applies_only_to_future_tasks() {
    let (app, _, store) = app();
    let response = app
        .clone()
        .oneshot(request(
            &app,
            "POST",
            "/v1/messages",
            true,
            Some(json!({"conversation_id":"owner", "text":"first"})),
        ))
        .await
        .unwrap();
    let bytes = http_body_util::BodyExt::collect(response.into_body())
        .await
        .unwrap()
        .to_bytes();
    let value: Value = serde_json::from_slice(&bytes).unwrap();
    let original = store
        .get_task(&admin(&store), value["task"]["id"].as_str().unwrap())
        .unwrap();
    let selection = json!({"provider":"default", "model":"test-alternative"});
    let response = app
        .clone()
        .oneshot(request(
            &app,
            "PUT",
            "/v1/conversations/owner/model",
            true,
            Some(selection.clone()),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        store
            .get_task(&admin(&store), &original.id)
            .unwrap()
            .payload,
        original.payload
    );
    assert!(store.kv_get("model_selection:owner").unwrap().is_some());
    let response = app
        .clone()
        .oneshot(request(
            &app,
            "GET",
            "/v1/models?conversation_id=owner",
            true,
            None,
        ))
        .await
        .unwrap();
    let bytes = http_body_util::BodyExt::collect(response.into_body())
        .await
        .unwrap()
        .to_bytes();
    let value: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(value["selection"], selection);
}

#[tokio::test]
async fn selection_does_not_grant_egress_consent() {
    let (app, _, store) = app_with_policy(false, Policy::empty());
    let response = app
        .clone()
        .oneshot(request(
            &app,
            "PUT",
            "/v1/conversations/owner/model",
            true,
            Some(json!({"provider":"default", "model":"test-alternative"})),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    assert!(store.kv_get("model_selection:owner").unwrap().is_none());
    let response = app
        .clone()
        .oneshot(request(
            &app,
            "GET",
            "/v1/models?conversation_id=owner",
            true,
            None,
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn history_redacts_content_blocked_by_current_policy() {
    let (app, _, store) = app_with_policy(true, Policy::from_text("synthetic blocked phrase"));
    store
        .append_message(
            &admin(&store),
            "owner",
            None,
            crate::domain::Role::User,
            "synthetic blocked phrase",
        )
        .unwrap();
    let response = app
        .clone()
        .oneshot(request(
            &app,
            "GET",
            "/v1/conversations/owner/messages",
            true,
            None,
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = http_body_util::BodyExt::collect(response.into_body())
        .await
        .unwrap()
        .to_bytes();
    let value: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(
        value["messages"][0]["content"],
        "[Content withheld by policy]"
    );
}

#[tokio::test]
async fn selection_rejects_client_supplied_endpoint() {
    let (app, _, store) = app();
    let response = app
        .clone()
        .oneshot(request(
            &app,
            "PUT",
            "/v1/conversations/owner/model",
            true,
            Some(json!({"provider":"default", "model":"test", "base_url":"https://example.com"})),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
    assert!(store.kv_get("model_selection:owner").unwrap().is_none());
}

#[tokio::test]
async fn model_catalog_requires_authentication() {
    let (app, _, _) = app();
    let response = app
        .clone()
        .oneshot(request(
            &app,
            "GET",
            "/v1/models?conversation_id=windows-owner",
            false,
            None,
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn lists_configured_models_without_endpoint_or_credentials() {
    let (app, config, _) = app();
    let response = app
        .clone()
        .oneshot(request(
            &app,
            "GET",
            "/v1/models?conversation_id=windows-owner",
            true,
            None,
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = http_body_util::BodyExt::collect(response.into_body())
        .await
        .unwrap()
        .to_bytes();
    let value: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(value["selection"]["provider"], "default");
    assert_eq!(value["selection"]["model"], config.llm.model);
    assert_eq!(value["effective_selection"], value["selection"]);
    assert_eq!(value["providers"][0]["id"], "default");
    assert_eq!(value["entries"][0]["provider"], "default");
    assert_eq!(value["entries"][0]["model"], config.llm.model);
    let text = String::from_utf8(bytes.to_vec()).unwrap();
    assert!(!text.contains("base_url"));
    assert!(!text.contains("api_key"));
    assert!(!text.contains(&config.llm.base_url));
}

#[tokio::test]
async fn model_selection_rejects_unknown_provider_without_changing_state() {
    let (app, _, store) = app();
    let response = app
        .clone()
        .oneshot(request(
            &app,
            "PUT",
            "/v1/conversations/owner/model",
            true,
            Some(json!({"provider":"missing", "model":"invented"})),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
    assert!(store.kv_get("model_selection:owner").unwrap().is_none());
}

#[tokio::test]
async fn history_returns_only_visible_messages_from_requested_conversation() {
    let (app, _, store) = app();
    store
        .append_message(
            &admin(&store),
            "owner",
            None,
            crate::domain::Role::User,
            "hello",
        )
        .unwrap();
    store
        .append_message(
            &admin(&store),
            "owner",
            None,
            crate::domain::Role::Assistant,
            "bonjour",
        )
        .unwrap();
    store
        .append_message(
            &admin(&store),
            "owner",
            None,
            crate::domain::Role::System,
            "private system prompt",
        )
        .unwrap();
    store
        .append_message(
            &admin(&store),
            "other",
            None,
            crate::domain::Role::User,
            "other conversation",
        )
        .unwrap();
    let response = app
        .clone()
        .oneshot(request(
            &app,
            "GET",
            "/v1/conversations/owner/messages?limit=100",
            true,
            None,
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = http_body_util::BodyExt::collect(response.into_body())
        .await
        .unwrap()
        .to_bytes();
    let value: Value = serde_json::from_slice(&bytes).unwrap();
    let messages = value["messages"].as_array().unwrap();
    assert_eq!(messages.len(), 2);
    assert_eq!(messages[0]["content"], "hello");
    assert_eq!(messages[1]["content"], "bonjour");
    assert!(messages.iter().all(|message| message.get("name").is_none()));
    let text = String::from_utf8(bytes.to_vec()).unwrap();
    assert!(!text.contains("private system prompt"));
    assert!(!text.contains("other conversation"));
}

#[tokio::test]
async fn room_history_returns_handoff_speaker_and_drops_unsafe_names() {
    let (app, _, store) = app();
    store
        .append_message_named(
            &admin(&store),
            "room:review",
            None,
            crate::domain::Role::Assistant,
            "Synthetic analyst summary.",
            Some("analyst"),
        )
        .unwrap();
    store
        .append_message_named(
            &admin(&store),
            "room:review",
            None,
            crate::domain::Role::Assistant,
            "orchestrator note",
            Some("bot999:secretvalue"),
        )
        .unwrap();
    let response = app
        .clone()
        .oneshot(request(
            &app,
            "GET",
            "/v1/conversations/room:review/messages",
            true,
            None,
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let value = read_json(response).await;
    let messages = value["messages"].as_array().unwrap();
    assert_eq!(messages.len(), 2);
    assert_eq!(messages[0]["name"], "analyst");
    assert!(messages[1].get("name").is_none());
    let text = value.to_string();
    assert!(!text.contains("secretvalue"));
    assert!(!text.contains("bot999"));
}

#[tokio::test]
async fn lists_configured_bots_without_prompts_or_secrets() {
    let root = temp_base();
    let secret = "super-secret-prompt-body-should-not-leak";
    let mut notes = test_bot("notes", crate::orchestration::BotRole::Specialist);
    notes.config.skills = vec!["ask".into()];
    notes.config.tools = vec!["skill_list".into()];
    notes.config.enabled = false;
    notes.prompt = secret.to_string();
    let (app, _, _) = app_with_fleet(
        true,
        Policy::empty(),
        root.clone(),
        vec![
            test_bot("main", crate::orchestration::BotRole::Orchestrator),
            notes,
        ],
    );
    let denied = app
        .clone()
        .oneshot(request(&app, "GET", "/v1/bots", false, None))
        .await
        .unwrap();
    assert_eq!(denied.status(), StatusCode::UNAUTHORIZED);

    let listed = app
        .clone()
        .oneshot(request(&app, "GET", "/v1/bots", true, None))
        .await
        .unwrap();
    assert_eq!(listed.status(), StatusCode::OK);
    let listed = read_json(listed).await;
    let bots = listed["bots"].as_array().unwrap();
    assert_eq!(bots.len(), 2);
    let notes = bots.iter().find(|bot| bot["id"] == "notes").unwrap();
    assert_eq!(notes["role"], "specialist");
    assert_eq!(notes["enabled"], false);
    assert_eq!(notes["live"], true);
    assert_eq!(notes["tools"][0], "skill_list");
    assert_eq!(notes["skills"][0], "ask");
    let text = listed.to_string();
    assert!(!text.contains(secret));
    assert!(!text.contains("system_prompt"));
    assert!(!text.contains("prompts/notes.txt"));
    let _ = std::fs::remove_dir_all(&root);
}

#[tokio::test]
async fn new_conversation_routes_validate_identity_and_require_authentication() {
    let (app, _, _) = app();
    for uri in [
        "/v1/models?conversation_id=bad%20id",
        "/v1/conversations/bad%20id/messages",
    ] {
        let response = app
            .clone()
            .oneshot(request(&app, "GET", uri, true, None))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
    }
    for (method, uri, body) in [
        ("GET", "/v1/conversations/owner/messages", None),
        (
            "PUT",
            "/v1/conversations/owner/model",
            Some(json!({"provider":"default", "model":"test"})),
        ),
    ] {
        let response = app
            .clone()
            .oneshot(request(&app, method, uri, false, body))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }
}

#[tokio::test]
async fn connector_catalog_lists_only_the_elevenlabs_subscription_in_voice_group() {
    let (app, _, _) = app();
    let response = app
        .clone()
        .oneshot(request(&app, "GET", "/v1/connectors", true, None))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = read_json(response).await;
    let list = body["connectors"].as_array().unwrap();
    // 0.28: the API-key connector is gone; only the account sign-in remains.
    assert!(
        !list
            .iter()
            .any(|connector| connector["id"] == "elevenlabs_api_key"),
        "elevenlabs_api_key must not be listed"
    );
    // The account sign-in: per-account state, no server-wide secrets.
    let signin = list
        .iter()
        .find(|connector| connector["id"] == "elevenlabs")
        .cloned()
        .expect("elevenlabs listed");
    assert_eq!(signin["kind"], "oauth");
    assert_eq!(signin["enabled"], false);
    assert_eq!(signin["connected"], false);
    assert_eq!(signin["group"], "Voice & Media");
    assert!(signin.get("auth").is_none());
    assert_eq!(signin["auth_modes"], json!(["oauth"]));
}

#[tokio::test]
async fn connector_test_is_owner_only_and_reports_missing_secrets_without_network() {
    let (app, _, _) = app();
    let response = app
        .clone()
        .oneshot(request(
            &app,
            "POST",
            "/v1/admin/connectors/elevenlabs/test",
            false,
            None,
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

    let response = app
        .clone()
        .oneshot(request(
            &app,
            "POST",
            "/v1/admin/connectors/no-such-connector/test",
            true,
            None,
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);

    for id in ["tavily", "firecrawl", "agentmail", "gmail"] {
        let response = app
            .clone()
            .oneshot(request(
                &app,
                "POST",
                &format!("/v1/admin/connectors/{id}/test"),
                true,
                None,
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK, "{id}");
        let body = read_json(response).await;
        assert_eq!(body["id"], id);
        assert_eq!(body["ok"], false, "{id} has no stored secrets");
        assert!(
            body["message"]
                .as_str()
                .unwrap()
                .starts_with("Missing secret: "),
            "{id}: {body}"
        );
        assert!(body["checked_at"].as_str().is_some());
    }
}

#[tokio::test]
async fn admin_can_remove_saved_connector_secrets_but_not_per_account_signins() {
    let (app, _, _) = app();
    // Non-admin is refused.
    let denied = app
        .clone()
        .oneshot(request(
            &app,
            "DELETE",
            "/v1/admin/connectors/elevenlabs_api_key/secrets",
            false,
            None,
        ))
        .await
        .unwrap();
    assert_eq!(denied.status(), StatusCode::UNAUTHORIZED);
    // Unknown connector.
    let missing = app
        .clone()
        .oneshot(request(
            &app,
            "DELETE",
            "/v1/admin/connectors/no-such/secrets",
            true,
            None,
        ))
        .await
        .unwrap();
    assert_eq!(missing.status(), StatusCode::NOT_FOUND);
    // The sign-in connector keeps its tokens per account: refused here.
    let per_account = app
        .clone()
        .oneshot(request(
            &app,
            "DELETE",
            "/v1/admin/connectors/elevenlabs/secrets",
            true,
            None,
        ))
        .await
        .unwrap();
    assert_eq!(per_account.status(), StatusCode::UNPROCESSABLE_ENTITY);
    // 0.28: the API-key connector no longer exists, so its secrets route is
    // gone too.
    let gone = app
        .clone()
        .oneshot(request(
            &app,
            "DELETE",
            "/v1/admin/connectors/elevenlabs_api_key/secrets",
            true,
            None,
        ))
        .await
        .unwrap();
    assert_eq!(gone.status(), StatusCode::NOT_FOUND);
    // Another API-key connector's server-wide secret is removed; only its
    // name comes back, never a value.
    let removed = app
        .clone()
        .oneshot(request(
            &app,
            "DELETE",
            "/v1/admin/connectors/tavily/secrets",
            true,
            None,
        ))
        .await
        .unwrap();
    assert_eq!(removed.status(), StatusCode::OK);
    let body = read_json(removed).await;
    assert_eq!(body["id"], "tavily");
    assert_eq!(body["removed"], json!(["tavily_api_key"]));
    assert!(!body.to_string().contains("sk_"));
}

async fn read_json(response: Response) -> Value {
    let bytes = http_body_util::BodyExt::collect(response.into_body())
        .await
        .unwrap()
        .to_bytes();
    serde_json::from_slice(&bytes).unwrap()
}

async fn read_body(response: Response) -> String {
    let bytes = http_body_util::BodyExt::collect(response.into_body())
        .await
        .unwrap()
        .to_bytes();
    String::from_utf8(bytes.to_vec()).unwrap()
}

fn temp_base() -> std::path::PathBuf {
    let root = std::env::temp_dir().join(format!("serabot-api-fleet-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(root.join("skills").join("ask")).unwrap();
    std::fs::write(
        root.join("skills").join("ask").join("SKILL.md"),
        "---\nname: \"Ask\"\ndescription: \"Synthetic ask skill\"\n---\n\n# Ask\n",
    )
    .unwrap();
    root.canonicalize().unwrap()
}

#[tokio::test]
async fn scaffolds_bot_from_template_without_owner_paths() {
    let root = temp_base();
    let (app, _, store) = app_with_root(true, Policy::empty(), root.clone());
    let denied = app
        .clone()
        .oneshot(request(
            &app,
            "POST",
            "/v1/bots",
            false,
            Some(json!({"id": "notes"})),
        ))
        .await
        .unwrap();
    assert_eq!(denied.status(), StatusCode::UNAUTHORIZED);

    let response = app
        .clone()
        .oneshot(request(
            &app,
            "POST",
            "/v1/bots",
            true,
            Some(json!({
                "id": "notes",
                "description": "Synthetic notes specialist.",
                "tools": ["rag_search", "skill_list"],
                "skills": ["ask"]
            })),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let value = read_json(response).await;
    assert_eq!(value["bot"]["id"], "notes");
    assert_eq!(value["bot"]["enabled"], false);
    assert_eq!(value["bot"]["role"], "specialist");
    assert_eq!(value["bot"]["live"], false);
    assert_eq!(value["bot"]["bot_file"], "db/bots/notes.toml");
    assert_eq!(value["bot"]["prompt_file"], "db/prompts/notes.txt");
    assert!(!value.to_string().contains("delegate"));
    let row = store
        .get_bot(&admin(&store), "notes")
        .unwrap()
        .expect("notes row");
    assert!(!row.enabled);
    assert!(row.prompt.contains("do not delegate"));
    assert!(row.prompt.contains("Synthetic notes specialist."));
    assert_eq!(row.config["role"], "specialist");
    assert!(
        store
            .list_audit(10)
            .unwrap()
            .iter()
            .any(|entry| entry.event == "bot.scaffolded")
    );

    let again = app
        .clone()
        .oneshot(request(
            &app,
            "POST",
            "/v1/bots",
            true,
            Some(json!({"id": "notes"})),
        ))
        .await
        .unwrap();
    assert_eq!(again.status(), StatusCode::CONFLICT);

    let escaped = app
        .clone()
        .oneshot(request(
            &app,
            "POST",
            "/v1/bots",
            true,
            Some(json!({"id": "../outside", "path": "C:/secret"})),
        ))
        .await
        .unwrap();
    assert_eq!(escaped.status(), StatusCode::UNPROCESSABLE_ENTITY);
    assert!(!root.join("outside.toml").exists());
    assert!(store.get_bot(&admin(&store), "outside").unwrap().is_none());

    let delegate = app
        .clone()
        .oneshot(request(
            &app,
            "POST",
            "/v1/bots",
            true,
            Some(json!({"id": "other", "tools": ["delegate"]})),
        ))
        .await
        .unwrap();
    assert_eq!(delegate.status(), StatusCode::UNPROCESSABLE_ENTITY);
    assert!(!root.join("bots").join("other.toml").exists());
    assert!(store.get_bot(&admin(&store), "other").unwrap().is_none());
    let _ = std::fs::remove_dir_all(&root);
}

#[tokio::test]
async fn bot_scaffold_rejects_policy_violations() {
    let root = temp_base();
    let (app, _, store) = app_with_root(
        true,
        Policy::from_text("synthetic blocked phrase"),
        root.clone(),
    );
    let response = app
        .clone()
        .oneshot(request(
            &app,
            "POST",
            "/v1/bots",
            true,
            Some(json!({
                "id": "notes",
                "description": "mentions synthetic blocked phrase"
            })),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    assert!(!root.join("bots").join("notes.toml").exists());
    assert!(store.get_bot(&admin(&store), "notes").unwrap().is_none());
    let _ = std::fs::remove_dir_all(&root);
}

#[tokio::test]
async fn registers_skill_pack_without_returning_or_logging_the_body() {
    let root = temp_base();
    let (app, _, store) = app_with_root(true, Policy::empty(), root.clone());
    let denied = app
        .clone()
        .oneshot(request(
            &app,
            "POST",
            "/v1/skills",
            false,
            Some(json!({"id": "notes-pack"})),
        ))
        .await
        .unwrap();
    assert_eq!(denied.status(), StatusCode::UNAUTHORIZED);

    let marker = "Unique workflow sentence alpha-bridge-7.";
    let response = app
        .clone()
        .oneshot(request(
            &app,
            "POST",
            "/v1/skills",
            true,
            Some(json!({
                "id": "notes-pack",
                "description": "Public summary",
                "body": marker
            })),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let value = read_json(response).await;
    assert_eq!(value["skill"]["id"], "notes-pack");
    assert_eq!(value["skill"]["file"], "user_skills/notes-pack/SKILL.md");
    assert_eq!(value["skill"]["live"], false);
    assert!(!value.to_string().contains(marker));
    let row = store
        .get_user_skill(&admin(&store), "notes-pack")
        .unwrap()
        .expect("user skill row");
    assert!(row.body.contains(marker));
    assert_eq!(row.description, "Public summary");
    assert!(!root.join("skills").join("notes-pack").exists());
    assert!(
        store
            .list_audit(10)
            .unwrap()
            .iter()
            .any(|entry| entry.event == "skill.created")
    );
    let audit = app
        .clone()
        .oneshot(request(&app, "GET", "/v1/audit?limit=20", true, None))
        .await
        .unwrap();
    let audit = read_json(audit).await;
    assert!(!audit.to_string().contains(marker));

    let listed = app
        .clone()
        .oneshot(request(&app, "GET", "/v1/skills", true, None))
        .await
        .unwrap();
    assert_eq!(listed.status(), StatusCode::OK);
    let listed = read_json(listed).await;
    let skills = listed["skills"].as_array().unwrap();
    assert!(skills.iter().any(|skill| skill["id"] == "notes-pack"));
    assert!(skills.iter().any(|skill| skill["id"] == "ask"));
    assert!(!listed.to_string().contains(marker));

    let again = app
        .clone()
        .oneshot(request(
            &app,
            "POST",
            "/v1/skills",
            true,
            Some(json!({"id": "notes-pack"})),
        ))
        .await
        .unwrap();
    assert_eq!(again.status(), StatusCode::CONFLICT);

    let escaped = app
        .clone()
        .oneshot(request(
            &app,
            "POST",
            "/v1/skills",
            true,
            Some(json!({"id": "../outside", "path": "C:/secret"})),
        ))
        .await
        .unwrap();
    assert_eq!(escaped.status(), StatusCode::UNPROCESSABLE_ENTITY);
    assert!(!root.join("outside").exists());
    assert!(!root.join("skills").join("outside").exists());
    let _ = std::fs::remove_dir_all(&root);
}

#[tokio::test]
async fn skill_registration_rejects_policy_violations() {
    let root = temp_base();
    let (app, _, store) = app_with_root(
        true,
        Policy::from_text("synthetic blocked phrase"),
        root.clone(),
    );
    let response = app
        .clone()
        .oneshot(request(
            &app,
            "POST",
            "/v1/skills",
            true,
            Some(json!({
                "id": "notes-pack",
                "description": "Public summary",
                "body": "mentions synthetic blocked phrase"
            })),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    assert!(!root.join("skills").join("notes-pack").exists());
    assert!(
        store
            .get_user_skill(&admin(&store), "notes-pack")
            .unwrap()
            .is_none()
    );
    let _ = std::fs::remove_dir_all(&root);
}

#[tokio::test]
async fn creates_conversation_tree_and_keeps_containers_turnless() {
    let (app, _, store) = app();
    let denied = app
        .clone()
        .oneshot(request(
            &app,
            "POST",
            "/v1/conversations",
            false,
            Some(json!({"kind": "section", "slug": "finance"})),
        ))
        .await
        .unwrap();
    assert_eq!(denied.status(), StatusCode::UNAUTHORIZED);

    let section = app
        .clone()
        .oneshot(request(
            &app,
            "POST",
            "/v1/conversations",
            true,
            Some(json!({"kind": "section", "slug": "finance"})),
        ))
        .await
        .unwrap();
    assert_eq!(section.status(), StatusCode::OK);
    let section = read_json(section).await;
    assert_eq!(section["conversation"]["id"], "sec:finance");
    assert_eq!(section["conversation"]["accepts_turns"], false);
    assert!(section["conversation"]["parent_id"].is_null());

    let group = app
        .clone()
        .oneshot(request(
            &app,
            "POST",
            "/v1/conversations",
            true,
            Some(json!({
                "kind": "group",
                "slug": "ops",
                "parent_id": "sec:finance"
            })),
        ))
        .await
        .unwrap();
    assert_eq!(group.status(), StatusCode::OK);
    let group = read_json(group).await;
    assert_eq!(group["conversation"]["id"], "grp:ops");
    assert_eq!(group["conversation"]["parent_id"], "sec:finance");
    assert_eq!(group["conversation"]["accepts_turns"], false);

    let direct = app
        .clone()
        .oneshot(request(
            &app,
            "POST",
            "/v1/conversations",
            true,
            Some(json!({
                "kind": "direct",
                "slug": "analyst",
                "parent_id": "grp:ops"
            })),
        ))
        .await
        .unwrap();
    assert_eq!(direct.status(), StatusCode::OK);
    let direct = read_json(direct).await;
    assert_eq!(direct["conversation"]["id"], "dir:analyst");
    assert_eq!(direct["conversation"]["accepts_turns"], true);

    let room = app
        .clone()
        .oneshot(request(
            &app,
            "POST",
            "/v1/conversations",
            true,
            Some(json!({
                "kind": "room",
                "slug": "review",
                "parent_id": "sec:finance"
            })),
        ))
        .await
        .unwrap();
    assert_eq!(room.status(), StatusCode::OK);
    assert_eq!(read_json(room).await["conversation"]["id"], "room:review");

    let orphan_group = app
        .clone()
        .oneshot(request(
            &app,
            "POST",
            "/v1/conversations",
            true,
            Some(json!({"kind": "group", "slug": "loose"})),
        ))
        .await
        .unwrap();
    assert_eq!(orphan_group.status(), StatusCode::UNPROCESSABLE_ENTITY);

    let bad_parent = app
        .clone()
        .oneshot(request(
            &app,
            "POST",
            "/v1/conversations",
            true,
            Some(json!({
                "kind": "direct",
                "slug": "bad",
                "parent_id": "dir:analyst"
            })),
        ))
        .await
        .unwrap();
    assert_eq!(bad_parent.status(), StatusCode::UNPROCESSABLE_ENTITY);

    let duplicate = app
        .clone()
        .oneshot(request(
            &app,
            "POST",
            "/v1/conversations",
            true,
            Some(json!({"kind": "section", "slug": "finance"})),
        ))
        .await
        .unwrap();
    assert_eq!(duplicate.status(), StatusCode::CONFLICT);

    let section_turn = app
        .clone()
        .oneshot(request(
            &app,
            "POST",
            "/v1/messages",
            true,
            Some(json!({"conversation_id": "sec:finance", "text": "hello"})),
        ))
        .await
        .unwrap();
    assert_eq!(section_turn.status(), StatusCode::UNPROCESSABLE_ENTITY);

    let group_turn = app
        .clone()
        .oneshot(request(
            &app,
            "POST",
            "/v1/messages",
            true,
            Some(json!({"conversation_id": "grp:ops", "text": "hello"})),
        ))
        .await
        .unwrap();
    assert_eq!(group_turn.status(), StatusCode::UNPROCESSABLE_ENTITY);

    let direct_turn = app
        .clone()
        .oneshot(request(
            &app,
            "POST",
            "/v1/messages",
            true,
            Some(json!({"conversation_id": "dir:analyst", "text": "hello"})),
        ))
        .await
        .unwrap();
    assert_eq!(direct_turn.status(), StatusCode::OK);
    assert_eq!(store.pending_task_count(&admin(&store)).unwrap(), 1);
    assert_eq!(
        store
            .get_conversation(&admin(&store), "dir:analyst")
            .unwrap()
            .unwrap()
            .parent_id
            .as_deref(),
        Some("grp:ops")
    );
}

#[tokio::test]
async fn lists_renames_archives_and_paginates_conversations() {
    let (app, _, _) = app();
    for (kind, slug, parent) in [
        ("section", "alpha", None),
        ("section", "beta", None),
        ("direct", "analyst", Some("sec:beta")),
    ] {
        let mut body = json!({"kind": kind, "slug": slug});
        if let Some(parent) = parent {
            body["parent_id"] = json!(parent);
        }
        let response = app
            .clone()
            .oneshot(request(&app, "POST", "/v1/conversations", true, Some(body)))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK, "{kind} {slug}");
    }

    let first = read_json(
        app.clone()
            .oneshot(request(
                &app,
                "GET",
                "/v1/conversations?limit=2",
                true,
                None,
            ))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(first["conversations"].as_array().unwrap().len(), 2);
    let cursor = first["next_cursor"]
        .as_str()
        .expect("next cursor")
        .to_owned();
    let encoded = url::form_urlencoded::byte_serialize(cursor.as_bytes()).collect::<String>();
    let second = read_json(
        app.clone()
            .oneshot(request(
                &app,
                "GET",
                &format!("/v1/conversations?limit=2&cursor={encoded}"),
                true,
                None,
            ))
            .await
            .unwrap(),
    )
    .await;
    let mut ids = first["conversations"]
        .as_array()
        .unwrap()
        .iter()
        .chain(second["conversations"].as_array().unwrap().iter())
        .map(|conversation| conversation["id"].as_str().unwrap().to_owned())
        .collect::<Vec<_>>();
    ids.sort();
    assert_eq!(ids, ["dir:analyst", "sec:alpha", "sec:beta"]);

    let renamed = read_json(
        app.clone()
            .oneshot(request(
                &app,
                "PATCH",
                "/v1/conversations/sec:beta",
                true,
                Some(json!({"title": "  Finance   reviews  "})),
            ))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(renamed["conversation"]["title"], "Finance reviews");

    let empty_title = app
        .clone()
        .oneshot(request(
            &app,
            "PATCH",
            "/v1/conversations/sec:beta",
            true,
            Some(json!({"title": "   "})),
        ))
        .await
        .unwrap();
    assert_eq!(empty_title.status(), StatusCode::UNPROCESSABLE_ENTITY);

    let missing = app
        .clone()
        .oneshot(request(
            &app,
            "PATCH",
            "/v1/conversations/sec:missing",
            true,
            Some(json!({"title": "Nope"})),
        ))
        .await
        .unwrap();
    assert_eq!(missing.status(), StatusCode::NOT_FOUND);

    let archived = read_json(
        app.clone()
            .oneshot(request(
                &app,
                "DELETE",
                "/v1/conversations/dir:analyst",
                true,
                None,
            ))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(archived["archived"], true);

    let visible = read_json(
        app.clone()
            .oneshot(request(&app, "GET", "/v1/conversations", true, None))
            .await
            .unwrap(),
    )
    .await;
    assert!(!visible.to_string().contains("dir:analyst"));

    let with_archived = read_json(
        app.clone()
            .oneshot(request(
                &app,
                "GET",
                "/v1/conversations?include_archived=true",
                true,
                None,
            ))
            .await
            .unwrap(),
    )
    .await;
    assert!(with_archived.to_string().contains("dir:analyst"));

    let restored = read_json(
        app.clone()
            .oneshot(request(
                &app,
                "POST",
                "/v1/conversations/dir:analyst/unarchive",
                true,
                None,
            ))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(restored["archived"], false);

    let visible = read_json(
        app.clone()
            .oneshot(request(&app, "GET", "/v1/conversations", true, None))
            .await
            .unwrap(),
    )
    .await;
    assert!(visible.to_string().contains("dir:analyst"));

    let missing_restore = app
        .clone()
        .oneshot(request(
            &app,
            "POST",
            "/v1/conversations/dir:missing/unarchive",
            true,
            None,
        ))
        .await
        .unwrap();
    assert_eq!(missing_restore.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn library_lists_no_rows_for_a_fresh_conversation_and_404s_unknown_ones() {
    let (app, _, _) = app();
    let created = app
        .clone()
        .oneshot(request(
            &app,
            "POST",
            "/v1/conversations",
            true,
            Some(json!({"kind": "room", "slug": "review"})),
        ))
        .await
        .unwrap();
    assert_eq!(created.status(), StatusCode::OK);

    let empty = read_json(
        app.clone()
            .oneshot(request(
                &app,
                "GET",
                "/v1/conversations/room:review/library",
                true,
                None,
            ))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(empty["artifacts"], json!([]));

    let missing = app
        .clone()
        .oneshot(request(
            &app,
            "GET",
            "/v1/conversations/room:ghost/library",
            true,
            None,
        ))
        .await
        .unwrap();
    assert_eq!(missing.status(), StatusCode::NOT_FOUND);

    let unauthenticated = app
        .clone()
        .oneshot(request(
            &app,
            "GET",
            "/v1/conversations/room:review/library",
            false,
            None,
        ))
        .await
        .unwrap();
    assert_eq!(unauthenticated.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn memory_crud_over_http() {
    let (app, _, _) = app();
    let denied = app
        .clone()
        .oneshot(request(&app, "GET", "/v1/memories", false, None))
        .await
        .unwrap();
    assert_eq!(denied.status(), StatusCode::UNAUTHORIZED);

    let created = read_json(
        app.clone()
            .oneshot(request(
                &app,
                "POST",
                "/v1/memories",
                true,
                Some(json!({
                    "kind": "preference",
                    "content": "Prefers concise summaries"
                })),
            ))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(created["memory"]["kind"], "preference");
    assert_eq!(created["memory"]["source"], "operator:desktop");
    let id = created["memory"]["id"].as_str().unwrap().to_owned();

    let matched = read_json(
        app.clone()
            .oneshot(request(
                &app,
                "GET",
                "/v1/memories?query=concise",
                true,
                None,
            ))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(matched["memories"].as_array().unwrap().len(), 1);

    let unmatched = read_json(
        app.clone()
            .oneshot(request(
                &app,
                "GET",
                "/v1/memories?query=absent-term",
                true,
                None,
            ))
            .await
            .unwrap(),
    )
    .await;
    assert!(unmatched["memories"].as_array().unwrap().is_empty());

    let bad_kind = app
        .clone()
        .oneshot(request(
            &app,
            "POST",
            "/v1/memories",
            true,
            Some(json!({"kind": "guess", "content": "x"})),
        ))
        .await
        .unwrap();
    assert_eq!(bad_kind.status(), StatusCode::UNPROCESSABLE_ENTITY);

    let deleted = app
        .clone()
        .oneshot(request(
            &app,
            "DELETE",
            &format!("/v1/memories/{id}"),
            true,
            None,
        ))
        .await
        .unwrap();
    assert_eq!(deleted.status(), StatusCode::OK);
    let missing = app
        .clone()
        .oneshot(request(
            &app,
            "DELETE",
            &format!("/v1/memories/{id}"),
            true,
            None,
        ))
        .await
        .unwrap();
    assert_eq!(missing.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn memory_writes_respect_the_content_policy() {
    let (app, _, _) = app_with_policy(true, Policy::from_text("synthetic blocked phrase"));
    let response = app
        .clone()
        .oneshot(request(
            &app,
            "POST",
            "/v1/memories",
            true,
            Some(json!({
                "kind": "fact",
                "content": "mentions synthetic blocked phrase"
            })),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    let listed = read_json(
        app.clone()
            .oneshot(request(&app, "GET", "/v1/memories", true, None))
            .await
            .unwrap(),
    )
    .await;
    assert!(listed["memories"].as_array().unwrap().is_empty());
}

/// Memory v2 over HTTP: hybrid search finds an old fact, filters and paging
/// work, PATCH edits only named fields, stats count by origin, and a member
/// never sees, edits, or deletes the owner's memories.
#[tokio::test]
async fn memory_v2_api_searches_filters_edits_and_isolates_users() {
    let (app, _, store) = app();
    let admin_id = admin(&store);
    let (_member, member_token) = member_with_session(&store);

    let created = read_json(
        app.clone()
            .oneshot(request(
                &app,
                "POST",
                "/v1/memories",
                true,
                Some(json!({
                    "kind": "fact",
                    "content": "Dentist appointment is on the ninth floor"
                })),
            ))
            .await
            .unwrap(),
    )
    .await;
    let explicit_id = created["memory"]["id"].as_str().unwrap().to_owned();
    assert_eq!(created["memory"]["origin"], "explicit");
    assert_eq!(created["memory"]["status"], "active");

    let learned = crate::memory::write(
        &store,
        &admin_id,
        &crate::memory::Candidate {
            origin: crate::memory::ORIGIN_EXTRACTED.into(),
            ..crate::memory::Candidate::explicit("fact", "Prefers train travel to Milan", "turn:1")
        },
    )
    .unwrap();
    let learned_id = learned.memory().id.clone();

    // Search finds the owner's facts; the member's search finds nothing.
    let found = read_json(
        app.clone()
            .oneshot(request_with_token_auth("GET", "/v1/memories?q=dentist"))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(found["memories"].as_array().unwrap().len(), 1);
    assert_eq!(found["memories"][0]["id"], explicit_id);
    let member_found = read_json(
        app.clone()
            .oneshot(request_with_token(
                "GET",
                "/v1/memories?q=dentist",
                &member_token,
                None,
            ))
            .await
            .unwrap(),
    )
    .await;
    assert!(member_found["memories"].as_array().unwrap().is_empty());

    // Origin filter and the console's status filter.
    let learned_only = read_json(
        app.clone()
            .oneshot(request(
                &app,
                "GET",
                "/v1/memories?origin=extracted",
                true,
                None,
            ))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(learned_only["memories"].as_array().unwrap().len(), 1);
    assert_eq!(learned_only["memories"][0]["id"], learned_id);

    // PATCH changes only the named field and keeps provenance.
    let patched = read_json(
        app.clone()
            .oneshot(request(
                &app,
                "PATCH",
                &format!("/v1/memories/{explicit_id}"),
                true,
                Some(json!({"content": "Dentist appointment moved to the second floor"})),
            ))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(
        patched["memory"]["content"],
        "Dentist appointment moved to the second floor"
    );
    assert_eq!(patched["memory"]["origin"], "explicit");

    // The member cannot patch or delete the owner's memory.
    let member_patch = app
        .clone()
        .oneshot(request_with_token(
            "PATCH",
            &format!("/v1/memories/{explicit_id}"),
            &member_token,
            Some(json!({"importance": 0.9})),
        ))
        .await
        .unwrap();
    assert_eq!(member_patch.status(), StatusCode::NOT_FOUND);
    let member_delete = app
        .clone()
        .oneshot(request_with_token(
            "DELETE",
            &format!("/v1/memories/{explicit_id}"),
            &member_token,
            None,
        ))
        .await
        .unwrap();
    assert_eq!(member_delete.status(), StatusCode::NOT_FOUND);

    // Unknown fields and statuses outside active/superseded are rejected.
    let unknown_field = app
        .clone()
        .oneshot(request(
            &app,
            "PATCH",
            &format!("/v1/memories/{explicit_id}"),
            true,
            Some(json!({"origin": "extracted"})),
        ))
        .await
        .unwrap();
    assert_eq!(unknown_field.status(), StatusCode::UNPROCESSABLE_ENTITY);
    let bad_status = app
        .clone()
        .oneshot(request(
            &app,
            "PATCH",
            &format!("/v1/memories/{explicit_id}"),
            true,
            Some(json!({"status": "pending"})),
        ))
        .await
        .unwrap();
    assert!(!bad_status.status().is_success());

    // Stats count the owner's rows only.
    let stats = read_json(
        app.clone()
            .oneshot(request(&app, "GET", "/v1/memories/stats", true, None))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(stats["stats"]["active"], 2);
    assert_eq!(stats["stats"]["by_origin"]["extracted"], 1);
    let member_stats = read_json(
        app.clone()
            .oneshot(request_with_token(
                "GET",
                "/v1/memories/stats",
                &member_token,
                None,
            ))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(member_stats["stats"]["active"], 0);

    // Paging: one row per page, offset moves the window.
    let page = read_json(
        app.clone()
            .oneshot(request(
                &app,
                "GET",
                "/v1/memories?limit=1&offset=1",
                true,
                None,
            ))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(page["memories"].as_array().unwrap().len(), 1);
    assert_eq!(page["offset"], 1);
}

/// Owner bearer request helper for the memory v2 test.
fn request_with_token_auth(method: &str, uri: &str) -> Request<Body> {
    request_with_token(method, uri, "test-token-that-is-long-enough-123", None)
}

#[tokio::test]
async fn admin_provider_writes_are_owner_gated_and_persist_an_overlay() {
    let (app, config, _) = app();
    let payload = json!({
        "id": "aux",
        "kind": "openai-compatible",
        "base_url": "https://api.example.com/v1",
        "models": ["aux-model"],
        "api_key_secret": "llm_api_key"
    });
    let unauthorized = app
        .clone()
        .oneshot(request(
            &app,
            "POST",
            "/v1/admin/providers",
            false,
            Some(payload.clone()),
        ))
        .await
        .unwrap();
    assert_eq!(unauthorized.status(), StatusCode::UNAUTHORIZED);

    let missing = app
        .clone()
        .oneshot(request(
            &app,
            "POST",
            "/v1/admin/providers",
            true,
            Some(json!({
                "id": "aux",
                "kind": "openai-compatible",
                "base_url": "https://api.example.com/v1",
                "models": ["aux-model"],
                "api_key_secret": "NOT_CONFIGURED"
            })),
        ))
        .await
        .unwrap();
    assert_eq!(missing.status(), StatusCode::UNPROCESSABLE_ENTITY);
    let body = read_json(missing).await;
    assert!(body["error"].as_str().unwrap().contains("NOT_CONFIGURED"));

    let saved = app
        .clone()
        .oneshot(request(
            &app,
            "POST",
            "/v1/admin/providers",
            true,
            Some(payload),
        ))
        .await
        .unwrap();
    let status = saved.status();
    let saved_body = read_json(saved).await;
    assert_eq!(status, StatusCode::CREATED, "{saved_body}");
    let overlay =
        crate::admin::AdminOverlay::load(&crate::admin::AdminOverlay::path_for(&config)).unwrap();
    assert!(overlay.providers.contains_key("aux"));
    assert!(overlay.providers["aux"].data_egress_approved);

    let removed = app
        .clone()
        .oneshot(request(
            &app,
            "DELETE",
            "/v1/admin/providers/aux",
            true,
            None,
        ))
        .await
        .unwrap();
    assert_eq!(removed.status(), StatusCode::OK);

    let unconfirmed = app
        .clone()
        .oneshot(request(
            &app,
            "POST",
            "/v1/admin/restart",
            true,
            Some(json!({"confirm": false})),
        ))
        .await
        .unwrap();
    assert_eq!(unconfirmed.status(), StatusCode::UNPROCESSABLE_ENTITY);
}

#[tokio::test]
async fn provider_oauth_endpoints_are_owner_gated_and_validate_before_any_network_call() {
    let (app, _config, _) = app();
    let unauthorized = app
        .clone()
        .oneshot(request(
            &app,
            "POST",
            "/v1/admin/providers/aux/oauth/start",
            false,
            Some(json!({})),
        ))
        .await
        .unwrap();
    assert_eq!(unauthorized.status(), StatusCode::UNAUTHORIZED);

    let unknown = app
        .clone()
        .oneshot(request(
            &app,
            "POST",
            "/v1/admin/providers/nope/oauth/start",
            true,
            Some(json!({})),
        ))
        .await
        .unwrap();
    assert_eq!(unknown.status(), StatusCode::NOT_FOUND);

    let saved = app
        .clone()
        .oneshot(request(
            &app,
            "POST",
            "/v1/admin/providers",
            true,
            Some(json!({
                "id": "aux",
                "kind": "openai-compatible",
                "base_url": "https://api.example.com/v1",
                "models": ["aux-model"],
                "api_key_secret": "llm_api_key"
            })),
        ))
        .await
        .unwrap();
    assert_eq!(saved.status(), StatusCode::CREATED);
    for path in ["start", "complete"] {
        let wrong_kind = app
            .clone()
            .oneshot(request(
                &app,
                "POST",
                &format!("/v1/admin/providers/aux/oauth/{path}"),
                true,
                Some(json!({
                    "code": "synthetic-auth-code",
                    "verifier": "synthetic-verifier-synthetic-verifier-0123456789",
                    "redirect_uri": "http://127.0.0.1:51121/oauth-callback"
                })),
            ))
            .await
            .unwrap();
        assert_eq!(wrong_kind.status(), StatusCode::UNPROCESSABLE_ENTITY);
        let body = read_json(wrong_kind).await;
        assert!(body["error"].as_str().unwrap().contains("does not use"));
    }
}

#[tokio::test]
async fn replacing_tools_prunes_stale_require_approval_instead_of_rejecting() {
    let root = temp_base();
    let mut notes = test_bot("notes", crate::orchestration::BotRole::Specialist);
    notes.config.tools = vec!["skill_list".into(), "memory_save".into()];
    notes.config.require_approval = vec!["memory_save".into()];
    let (app, _, _) = app_with_fleet(
        true,
        Policy::empty(),
        root,
        vec![
            test_bot("main", crate::orchestration::BotRole::Orchestrator),
            notes,
        ],
    );
    // Toggling the memory capability off removes memory_save; the stored
    // approval entry must be pruned, not fail the whole save (0.26.0).
    let saved = app
        .clone()
        .oneshot(request(
            &app,
            "PUT",
            "/v1/bots/notes",
            true,
            Some(json!({"tools": ["skill_list"]})),
        ))
        .await
        .unwrap();
    let status = saved.status();
    let body = read_json(saved).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["bot"]["tools"], json!(["skill_list"]));
    assert_eq!(body["bot"]["require_approval"], json!([]));
}

#[tokio::test]
async fn bot_profiles_and_titled_conversations_are_owner_editable() {
    let (app, _, _) = app();
    let saved = app
        .clone()
        .oneshot(request(
            &app,
            "PUT",
            "/v1/admin/bots/main",
            true,
            Some(json!({
                "display_name": "Chief of Staff",
                "description": "Synthetic orchestrator profile.",
                "avatar": "CS"
            })),
        ))
        .await
        .unwrap();
    let status = saved.status();
    let saved_body = read_json(saved).await;
    assert_eq!(status, StatusCode::OK, "{saved_body}");
    let bots = read_json(
        app.clone()
            .oneshot(request(&app, "GET", "/v1/bots", true, None))
            .await
            .unwrap(),
    )
    .await;
    let main = bots["bots"]
        .as_array()
        .unwrap_or_else(|| panic!("bots payload: {bots}"))
        .iter()
        .find(|bot| bot["id"] == "main")
        .unwrap_or_else(|| panic!("bots payload: {bots}"));
    assert_eq!(main["display_name"], "Chief of Staff");
    assert_eq!(main["avatar"], "CS");
    assert_eq!(main["description"], "Synthetic orchestrator profile.");

    let missing = app
        .clone()
        .oneshot(request(
            &app,
            "PUT",
            "/v1/admin/bots/nobody",
            true,
            Some(json!({"display_name": "Nobody"})),
        ))
        .await
        .unwrap();
    assert_eq!(missing.status(), StatusCode::NOT_FOUND);

    let created = app
        .clone()
        .oneshot(request(
            &app,
            "POST",
            "/v1/conversations",
            true,
            Some(json!({
                "kind": "section",
                "slug": "finance",
                "title": "Finance"
            })),
        ))
        .await
        .unwrap();
    assert_eq!(created.status(), StatusCode::OK);
    let body = read_json(created).await;
    assert_eq!(body["conversation"]["title"], "Finance");
    assert_eq!(body["conversation"]["id"], "sec:finance");
}

#[tokio::test]
async fn usage_endpoint_rolls_up_model_used_rows_clamps_days_and_filters() {
    let (app, _, store) = app();
    let denied = app
        .clone()
        .oneshot(request(&app, "GET", "/v1/usage", false, None))
        .await
        .unwrap();
    assert_eq!(denied.status(), StatusCode::UNAUTHORIZED);

    let empty = read_json(
        app.clone()
            .oneshot(request(&app, "GET", "/v1/usage", true, None))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(empty["totals"]["calls"], 0);
    assert_eq!(empty["totals"]["prompt_tokens"], 0);
    assert!(empty["per_day"].as_array().unwrap().is_empty());
    assert!(empty["per_bot"].as_array().unwrap().is_empty());

    let task = store
        .enqueue_task(
            &admin(&store),
            "turn",
            "main",
            &json!({"conversation_id": "owner"}),
            0,
            Utc::now(),
        )
        .unwrap();
    store
        .record_audit(
            "system",
            "model.used",
            None,
            Some(&task.id),
            None,
            None,
            Some("policy-synthetic"),
            Some(&json!({
                "provider": "default",
                "model": "test",
                "usage": {"prompt_tokens": 10, "completion_tokens": 5, "total_tokens": 15},
                "history_messages": 2,
                "prompt_chars_est": 40,
            })),
        )
        .unwrap();
    store
        .record_audit(
            "system",
            "model.used",
            None,
            Some(&task.id),
            None,
            None,
            Some("policy-synthetic"),
            Some(&json!({
                "usage": {"prompt_tokens": 100, "completion_tokens": 100, "total_tokens": 200}
            })),
        )
        .unwrap();
    let old =
        (Utc::now() - chrono::Duration::days(100)).to_rfc3339_opts(SecondsFormat::Millis, true);
    store
        .connection()
        .unwrap()
        .execute(
            &format!(
                "UPDATE audit_log SET at = '{old}' WHERE json_extract(details_json, '$.usage.total_tokens') = 200"
            ),
            [],
        )
        .unwrap();

    // days=500 clamps to 90, so the 100-day-old row is excluded.
    let body = read_json(
        app.clone()
            .oneshot(request(&app, "GET", "/v1/usage?days=500", true, None))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(body["totals"]["calls"], 1);
    assert_eq!(body["totals"]["prompt_tokens"], 10);
    assert_eq!(body["totals"]["completion_tokens"], 5);
    assert_eq!(body["totals"]["total_tokens"], 15);
    assert_eq!(body["per_day"].as_array().unwrap().len(), 1);
    assert_eq!(
        body["per_day"][0]["day"],
        Utc::now().format("%Y-%m-%d").to_string()
    );
    assert_eq!(body["per_bot"].as_array().unwrap().len(), 1);
    assert_eq!(body["per_bot"][0]["bot"], "main");
    assert_eq!(body["per_bot"][0]["calls"], 1);

    // days=0 clamps to 1; today's row is still inside the window.
    let body = read_json(
        app.clone()
            .oneshot(request(&app, "GET", "/v1/usage?days=0", true, None))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(body["totals"]["calls"], 1);

    // Bot and conversation filters.
    let body = read_json(
        app.clone()
            .oneshot(request(&app, "GET", "/v1/usage?bot=analyst", true, None))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(body["totals"]["calls"], 0);
    let body = read_json(
        app.clone()
            .oneshot(request(
                &app,
                "GET",
                "/v1/usage?conversation_id=owner",
                true,
                None,
            ))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(body["totals"]["calls"], 1);
    let body = read_json(
        app.clone()
            .oneshot(request(
                &app,
                "GET",
                "/v1/usage?conversation_id=dir%3Aother",
                true,
                None,
            ))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(body["totals"]["calls"], 0);

    // An unsafe bot filter value is rejected.
    let invalid = app
        .clone()
        .oneshot(request(&app, "GET", "/v1/usage?bot=bad%20id", true, None))
        .await
        .unwrap();
    assert_eq!(invalid.status(), StatusCode::UNPROCESSABLE_ENTITY);
}

#[tokio::test]
async fn bot_detail_returns_owner_fields_and_caps_instructions() {
    let root = temp_base();
    let mut notes = test_bot("notes", crate::orchestration::BotRole::Specialist);
    notes.config.skills = vec!["ask".into()];
    notes.config.tools = vec!["skill_list".into()];
    notes.config.enabled = true;
    notes.prompt = "Synthetic notes prompt.".into();
    let mut big = test_bot("big", crate::orchestration::BotRole::Specialist);
    big.config.enabled = true;
    big.prompt = "x".repeat(40_000);
    let (app, _, store) = app_with_fleet(
        true,
        Policy::empty(),
        root.clone(),
        vec![
            test_bot("main", crate::orchestration::BotRole::Orchestrator),
            notes,
            big,
        ],
    );

    let denied = app
        .clone()
        .oneshot(request(&app, "GET", "/v1/bots/notes", false, None))
        .await
        .unwrap();
    assert_eq!(denied.status(), StatusCode::UNAUTHORIZED);

    let body = read_json(
        app.clone()
            .oneshot(request(&app, "GET", "/v1/bots/notes", true, None))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(body["id"], "notes");
    assert_eq!(body["role"], "specialist");
    assert_eq!(body["enabled"], true);
    assert_eq!(body["provider"], "default");
    assert_eq!(body["model"], "test");
    assert_eq!(body["fallback_provider"], Value::Null);
    assert_eq!(body["fallback_model"], Value::Null);
    assert_eq!(body["routing_classes"].as_array().unwrap().len(), 0);
    assert_eq!(body["instructions"], "Synthetic notes prompt.");
    assert_eq!(body["instructions_bytes"], "Synthetic notes prompt.".len());
    assert_eq!(body["skills"][0], "ask");
    assert_eq!(body["tools"][0], "skill_list");
    assert_eq!(body["memory_count"], 0);
    assert_eq!(body["artifact_count"], 0);
    // No prompt path or secret material leaks.
    let text = body.to_string();
    assert!(!text.contains("prompts/notes.txt"));
    assert!(!text.contains("synthetic-test-key"));

    crate::memory::add(&store, &admin(&store), "fact", "synthetic fact", "test").unwrap();
    let body = read_json(
        app.clone()
            .oneshot(request(&app, "GET", "/v1/bots/notes", true, None))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(body["memory_count"], 1);

    // Over the 32 KB cap the instructions content is withheld, bytes kept.
    let big = read_json(
        app.clone()
            .oneshot(request(&app, "GET", "/v1/bots/big", true, None))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(big["instructions"], Value::Null);
    assert_eq!(big["instructions_bytes"], 40_000);

    let missing = app
        .clone()
        .oneshot(request(&app, "GET", "/v1/bots/nobody", true, None))
        .await
        .unwrap();
    assert_eq!(missing.status(), StatusCode::NOT_FOUND);
    let _ = std::fs::remove_dir_all(&root);
}

#[tokio::test]
async fn admin_bot_config_update_validates_persists_and_requires_restart() {
    let root = temp_base();
    let mut notes = test_bot("notes", crate::orchestration::BotRole::Specialist);
    notes.config.tools = vec!["skill_list".into()];
    notes.config.enabled = true;
    notes.prompt = "Synthetic notes prompt.".into();
    let (app, config, store) = app_with_fleet(
        true,
        Policy::empty(),
        root.clone(),
        vec![
            test_bot("main", crate::orchestration::BotRole::Orchestrator),
            notes,
        ],
    );

    let denied = app
        .clone()
        .oneshot(request(
            &app,
            "PUT",
            "/v1/admin/bots/notes/config",
            false,
            Some(json!({"tools": []})),
        ))
        .await
        .unwrap();
    assert_eq!(denied.status(), StatusCode::UNAUTHORIZED);

    let missing = app
        .clone()
        .oneshot(request(
            &app,
            "PUT",
            "/v1/admin/bots/nobody/config",
            true,
            Some(json!({"tools": []})),
        ))
        .await
        .unwrap();
    assert_eq!(missing.status(), StatusCode::NOT_FOUND);

    let cases: Vec<(Value, StatusCode)> = vec![
        (json!({}), StatusCode::UNPROCESSABLE_ENTITY),
        (
            json!({"tools": ["not_a_tool"]}),
            StatusCode::UNPROCESSABLE_ENTITY,
        ),
        (
            json!({"tools": ["delegate"]}),
            StatusCode::UNPROCESSABLE_ENTITY,
        ),
        (
            json!({"tools": (0..33).map(|i| format!("t{i}")).collect::<Vec<_>>()}),
            StatusCode::UNPROCESSABLE_ENTITY,
        ),
        (
            json!({"skills": ["nope"]}),
            StatusCode::UNPROCESSABLE_ENTITY,
        ),
        (
            json!({"skills": (0..17).map(|i| format!("s{i}")).collect::<Vec<_>>()}),
            StatusCode::UNPROCESSABLE_ENTITY,
        ),
        (
            json!({"tools": [], "require_approval": ["skill_list"]}),
            StatusCode::UNPROCESSABLE_ENTITY,
        ),
    ];
    for (payload, expected) in cases {
        let response = app
            .clone()
            .oneshot(request(
                &app,
                "PUT",
                "/v1/admin/bots/notes/config",
                true,
                Some(payload),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), expected);
    }

    // Valid partial update: empty tools, one known skill, empty approvals.
    let saved = app
        .clone()
        .oneshot(request(
            &app,
            "PUT",
            "/v1/admin/bots/notes/config",
            true,
            Some(json!({"tools": [], "skills": ["ask"], "require_approval": []})),
        ))
        .await
        .unwrap();
    let status = saved.status();
    let saved_body = read_json(saved).await;
    assert_eq!(status, StatusCode::OK, "{saved_body}");
    assert_eq!(saved_body["saved"], true);
    assert_eq!(saved_body["restart_required"], true);

    let overlay =
        crate::admin::AdminOverlay::load(&crate::admin::AdminOverlay::path_for(&config)).unwrap();
    let entry = overlay.bot_configs.get("notes").expect("overlay entry");
    assert_eq!(entry.tools, Some(vec![]));
    assert_eq!(entry.skills, Some(vec!["ask".to_string()]));
    assert_eq!(entry.require_approval, Some(vec![]));
    assert!(
        store
            .list_audit(100)
            .unwrap()
            .iter()
            .any(|entry| entry.event == "config.bot_config_saved")
    );

    // The detail endpoint reflects the saved override before the restart.
    let body = read_json(
        app.clone()
            .oneshot(request(&app, "GET", "/v1/bots/notes", true, None))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(body["tools"].as_array().unwrap().len(), 0);
    assert_eq!(body["skills"][0], "ask");
    let _ = std::fs::remove_dir_all(&root);
}

#[tokio::test]
async fn admin_bot_config_allows_delegate_for_the_orchestrator_only() {
    let (app, config, _) = app();
    let saved = app
        .clone()
        .oneshot(request(
            &app,
            "PUT",
            "/v1/admin/bots/main/config",
            true,
            Some(json!({"tools": ["delegate"]})),
        ))
        .await
        .unwrap();
    let status = saved.status();
    let body = read_json(saved).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["restart_required"], true);
    let overlay =
        crate::admin::AdminOverlay::load(&crate::admin::AdminOverlay::path_for(&config)).unwrap();
    assert_eq!(
        overlay
            .bot_configs
            .get("main")
            .and_then(|e| e.tools.clone()),
        Some(vec!["delegate".to_string()])
    );
}

#[tokio::test]
async fn providers_and_connectors_report_status_without_secret_values() {
    let (app, _, _) = app();
    let providers = read_json(
        app.clone()
            .oneshot(request(&app, "GET", "/v1/providers", true, None))
            .await
            .unwrap(),
    )
    .await;
    let list = providers["providers"].as_array().unwrap();
    let default = list
        .iter()
        .find(|provider| provider["id"] == "default")
        .expect("default provider");
    assert_eq!(default["secrets"]["api_key"]["name"], "llm_api_key");
    assert_eq!(default["secrets"]["api_key"]["configured"], true);
    assert!(
        default["models"]
            .as_array()
            .unwrap()
            .iter()
            .any(|model| model == "test")
    );
    assert!(!providers.to_string().contains("synthetic-test-key"));

    let connectors = read_json(
        app.clone()
            .oneshot(request(&app, "GET", "/v1/connectors", true, None))
            .await
            .unwrap(),
    )
    .await;
    let list = connectors["connectors"].as_array().unwrap();
    // 0.28: the ElevenLabs API-key connector was removed (17 → 16).
    assert_eq!(list.len(), 16);
    let gmail = list
        .iter()
        .find(|connector| connector["id"] == "gmail")
        .expect("gmail connector");
    assert_eq!(gmail["enabled"], false);
    assert_eq!(gmail["redirect_uri"], Value::Null);
    assert_eq!(
        gmail["secrets"]["client_id"]["name"],
        "google_oauth_client_id"
    );
    assert_eq!(gmail["secrets"]["client_id"]["configured"], false);
    let tavily = list
        .iter()
        .find(|connector| connector["id"] == "tavily")
        .expect("tavily connector");
    assert_eq!(tavily["kind"], "api_key");
    assert_eq!(tavily["secrets"]["api_key"]["name"], "tavily_api_key");
    assert!(list.iter().any(|connector| connector["id"] == "firecrawl"));
    assert!(!connectors.to_string().contains("synthetic-test-key"));
}

/// 0.14.0 regression: a bot holding a tool whose service is switched off
/// (unregistered, like `tavily_search` while Tavily is disabled) could not
/// have its tool list edited at all. Kept tools pass; new unknown ones fail.
#[tokio::test]
async fn bot_tool_edits_keep_tools_whose_service_is_switched_off() {
    let root = temp_base();
    let mut notes = test_bot("notes", crate::orchestration::BotRole::Specialist);
    notes.config.tools = vec!["switched_off_tool".into()];
    notes.config.enabled = true;
    notes.prompt = "Synthetic notes prompt.".into();
    let (app, _, _) = app_with_fleet(
        true,
        Policy::empty(),
        root.clone(),
        vec![
            test_bot("main", crate::orchestration::BotRole::Orchestrator),
            notes,
        ],
    );
    let kept = app
        .clone()
        .oneshot(request(
            &app,
            "PUT",
            "/v1/bots/notes",
            true,
            Some(json!({"tools": ["switched_off_tool", "rag_search", "tavily_search"]})),
        ))
        .await
        .unwrap();
    assert_eq!(kept.status(), StatusCode::OK);
    let body = read_json(kept).await;
    assert_eq!(
        body["bot"]["tools"],
        json!(["switched_off_tool", "rag_search", "tavily_search"])
    );

    let added = app
        .clone()
        .oneshot(request(
            &app,
            "PUT",
            "/v1/bots/notes",
            true,
            Some(json!({"tools": ["switched_off_tool", "another_unknown_tool"]})),
        ))
        .await
        .unwrap();
    assert_eq!(added.status(), StatusCode::UNPROCESSABLE_ENTITY);
    let _ = std::fs::remove_dir_all(&root);
}

#[tokio::test]
async fn providers_report_status_default_route_and_sign_in_only_where_one_exists() {
    let (app, _, _) = app();
    let body = read_json(
        app.clone()
            .oneshot(request(&app, "GET", "/v1/providers", true, None))
            .await
            .unwrap(),
    )
    .await;
    let default = body["providers"]
        .as_array()
        .unwrap()
        .iter()
        .find(|provider| provider["id"] == "default")
        .expect("default provider")
        .clone();
    assert!(default["status"].is_string());
    assert!(default["model_source"].is_string());
    assert!(default["model_count"].as_u64().unwrap() >= 1);

    // No default chosen yet.
    let none = read_json(
        app.clone()
            .oneshot(request(
                &app,
                "GET",
                "/v1/providers/default-route",
                true,
                None,
            ))
            .await
            .unwrap(),
    )
    .await;
    assert!(none["selection"].is_null());
    // A model the provider does not list cannot become the default.
    let refused = app
        .clone()
        .oneshot(request(
            &app,
            "PUT",
            "/v1/providers/default-route",
            true,
            Some(json!({"provider": "default", "model": "no-such-model"})),
        ))
        .await
        .unwrap();
    assert!(refused.status().is_client_error());

    // The default provider is not OpenRouter, so it has no sign-in.
    let challenge = "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM";
    let start = app
        .clone()
        .oneshot(request(
            &app,
            "POST",
            "/v1/providers/default/oauth/start",
            true,
            Some(json!({
                "challenge": challenge,
                "state": "state_1",
                "redirect_uri": "http://localhost:51423/callback"
            })),
        ))
        .await
        .unwrap();
    assert_eq!(start.status(), StatusCode::UNPROCESSABLE_ENTITY);
}
#[tokio::test]
async fn provider_credentials_are_per_user_and_grants_are_admin_controlled() {
    let (app, _, store) = app();
    let admin = admin(&store);
    // The admin stores a key through the API; the value never comes back.
    let response = app
        .clone()
        .oneshot(request(
            &app,
            "PUT",
            "/v1/providers/default/key",
            true,
            Some(json!({"api_key": "sk-admin-credential"})),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = read_json(
        app.clone()
            .oneshot(request(&app, "GET", "/v1/providers", true, None))
            .await
            .unwrap(),
    )
    .await;
    let default = body["providers"]
        .as_array()
        .unwrap()
        .iter()
        .find(|provider| provider["id"] == "default")
        .expect("default provider");
    assert_eq!(default["credential"]["own"], true);
    assert_eq!(default["credential"]["configured"], true);
    assert!(!body.to_string().contains("sk-admin-credential"));

    // A member has no credential of their own and cannot grant one.
    let member = store
        .create_user("microsoft:member", "member@example.example", "Member")
        .unwrap();
    let token = store
        .mint_session(&member.id, chrono::Duration::hours(1))
        .unwrap();
    let body = read_json(
        app.clone()
            .oneshot(request_with_token("GET", "/v1/providers", &token, None))
            .await
            .unwrap(),
    )
    .await;
    let default = body["providers"]
        .as_array()
        .unwrap()
        .iter()
        .find(|provider| provider["id"] == "default")
        .expect("default provider");
    assert_eq!(default["credential"]["configured"], false);
    assert_eq!(default["credential"]["granted"], false);
    let response = app
        .clone()
        .oneshot(request_with_token(
            "POST",
            "/v1/admin/grants",
            &token,
            Some(json!({"grantee": member.id, "provider": "default"})),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);

    // The admin grants the provider: the member can use it, never read it.
    let response = app
        .clone()
        .oneshot(request(
            &app,
            "POST",
            "/v1/admin/grants",
            true,
            Some(json!({"grantee": member.id, "provider": "default"})),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = read_json(
        app.clone()
            .oneshot(request_with_token("GET", "/v1/providers", &token, None))
            .await
            .unwrap(),
    )
    .await;
    let default = body["providers"]
        .as_array()
        .unwrap()
        .iter()
        .find(|provider| provider["id"] == "default")
        .expect("default provider");
    assert_eq!(default["credential"]["granted"], true);
    assert_eq!(default["credential"]["configured"], true);

    // The grant list is admin-only and carries no secret.
    let response = app
        .clone()
        .oneshot(request_with_token("GET", "/v1/admin/grants", &token, None))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    let body = read_json(
        app.clone()
            .oneshot(request(&app, "GET", "/v1/admin/grants", true, None))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(body["grants"].as_array().unwrap().len(), 1);
    assert_eq!(body["grants"][0]["grantee"], member.id);
    assert!(!body.to_string().contains("sk-admin-credential"));

    // Revocation takes effect immediately.
    let response = app
        .clone()
        .oneshot(request(
            &app,
            "DELETE",
            &format!("/v1/admin/grants/{}/default", member.id),
            true,
            None,
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = read_json(
        app.clone()
            .oneshot(request_with_token("GET", "/v1/providers", &token, None))
            .await
            .unwrap(),
    )
    .await;
    let default = body["providers"]
        .as_array()
        .unwrap()
        .iter()
        .find(|provider| provider["id"] == "default")
        .expect("default provider");
    assert_eq!(default["credential"]["granted"], false);
    assert_eq!(default["credential"]["configured"], false);

    // A member deleting their own credential never touches the admin's.
    let response = app
        .clone()
        .oneshot(request_with_token(
            "DELETE",
            "/v1/providers/default",
            &token,
            None,
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = read_json(
        app.clone()
            .oneshot(request(&app, "GET", "/v1/providers", true, None))
            .await
            .unwrap(),
    )
    .await;
    let default = body["providers"]
        .as_array()
        .unwrap()
        .iter()
        .find(|provider| provider["id"] == "default")
        .expect("default provider");
    assert_eq!(default["credential"]["own"], true);
    assert!(store.user_credential(&admin, "default").unwrap().is_some());
}

#[tokio::test]
async fn telegram_status_is_per_user_and_linking_fails_closed_when_disabled() {
    let (app, _, store) = app();
    let body = read_json(
        app.clone()
            .oneshot(request(&app, "GET", "/v1/telegram", true, None))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(body["enabled"], false);
    assert_eq!(body["linked"], false);
    let response = app
        .clone()
        .oneshot(request(&app, "POST", "/v1/telegram/link", true, None))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);

    // A stored binding is reported to its owner only.
    let member = store
        .create_user("microsoft:tg", "tg@example.example", "TG")
        .unwrap();
    store.link_telegram(&member.id, 40, 30).unwrap();
    let token = store
        .mint_session(&member.id, chrono::Duration::hours(1))
        .unwrap();
    let body = read_json(
        app.clone()
            .oneshot(request_with_token("GET", "/v1/telegram", &token, None))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(body["linked"], true);
    assert_eq!(body["telegram_user_id"], 40);
    assert_eq!(body["telegram_chat_id"], 30);
    let admin_body = read_json(
        app.clone()
            .oneshot(request(&app, "GET", "/v1/telegram", true, None))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(admin_body["linked"], false);

    // Unlinking is scoped: the member's own binding goes away.
    let response = app
        .clone()
        .oneshot(request_with_token(
            "DELETE",
            "/v1/telegram/link",
            &token,
            None,
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = read_json(response).await;
    assert_eq!(body["was_linked"], true);
    assert!(store.telegram_binding(40, 30).unwrap().is_none());
}

#[tokio::test]
async fn onedrive_endpoints_fail_closed_when_disabled() {
    let (app, _, _) = app();
    let response = app
        .clone()
        .oneshot(request(
            &app,
            "POST",
            "/v1/connectors/onedrive/start",
            true,
            None,
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    // Disconnecting when nothing is connected is idempotent and scoped.
    let response = app
        .clone()
        .oneshot(request(
            &app,
            "DELETE",
            "/v1/connectors/onedrive",
            true,
            None,
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = read_json(response).await;
    assert_eq!(body["connected"], false);
    assert_eq!(body["was_connected"], false);
}

#[tokio::test]
async fn legacy_conversations_list_as_turn_capable_threads() {
    let (app, _, store) = app();
    store
        .ensure_conversation_accepts_turns(&admin(&store), "windows-owner")
        .unwrap();
    let response = app
        .clone()
        .oneshot(request(&app, "GET", "/v1/conversations", true, None))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = read_json(response).await;
    let legacy = body["conversations"]
        .as_array()
        .unwrap()
        .iter()
        .find(|conversation| conversation["id"] == "windows-owner")
        .expect("legacy conversation");
    assert_eq!(legacy["kind"], "legacy");
    assert_eq!(legacy["accepts_turns"], true);
}

#[test]
fn event_resume_prefers_query_and_accepts_last_event_id() {
    let mut headers = HeaderMap::new();
    headers.insert("last-event-id", "7".parse().unwrap());
    assert_eq!(resume_cursor(&headers, None), 7);
    assert_eq!(resume_cursor(&headers, Some(12)), 12);
    assert_eq!(resume_cursor(&HeaderMap::new(), None), 0);
    assert_eq!(resume_cursor(&HeaderMap::new(), Some(-3)), 0);
    headers.insert("last-event-id", "not-a-number".parse().unwrap());
    assert_eq!(resume_cursor(&headers, None), 0);
}

/// A running fleet that matches the on-disk fleet directory used by
/// `list_configured_bots` (analyst, maker, watcher enabled; legal disabled).
fn app_with_team() -> (Router, Arc<Config>, Store) {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .map(std::path::Path::to_path_buf)
        .unwrap_or_else(|| std::env::current_dir().unwrap());
    app_with_fleet(
        true,
        Policy::empty(),
        root,
        vec![
            test_bot("main", crate::orchestration::BotRole::Orchestrator),
            test_bot("analyst", crate::orchestration::BotRole::Specialist),
            test_bot("maker", crate::orchestration::BotRole::Specialist),
            test_bot("watcher", crate::orchestration::BotRole::Specialist),
        ],
    )
}

#[tokio::test]
async fn team_endpoint_round_trip_and_member_validation() {
    let (app, _, store) = app_with_team();
    let created = app
        .clone()
        .oneshot(request(
            &app,
            "POST",
            "/v1/conversations",
            true,
            Some(json!({"kind": "room", "slug": "review"})),
        ))
        .await
        .unwrap();
    assert_eq!(created.status(), StatusCode::OK);

    // No team yet: an empty shape, never a 404.
    let empty = app
        .clone()
        .oneshot(request(
            &app,
            "GET",
            "/v1/conversations/room:review/team",
            true,
            None,
        ))
        .await
        .unwrap();
    assert_eq!(empty.status(), StatusCode::OK);
    let body = read_json(empty).await;
    assert_eq!(body["conversation_id"], "room:review");
    assert_eq!(body["mode"], "selected");
    assert_eq!(body["members"], json!([]));
    assert_eq!(body["max_members"], 5);

    let response = app
        .clone()
        .oneshot(request(
            &app,
            "PUT",
            "/v1/conversations/room:review/team",
            true,
            Some(json!({
                "members": ["analyst", "maker", "watcher"],
                "mode": "selected"
            })),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = read_json(response).await;
    assert_eq!(
        body["members"][0],
        json!({"bot": "analyst", "role": "specialist", "enabled": true, "live": true})
    );
    assert_eq!(body["members"][1]["bot"], "maker");
    assert_eq!(body["members"][2]["bot"], "watcher");
    assert_eq!(body["mode"], "selected");

    let stored = app
        .clone()
        .oneshot(request(
            &app,
            "GET",
            "/v1/conversations/room:review/team",
            true,
            None,
        ))
        .await
        .unwrap();
    let body = read_json(stored).await;
    assert_eq!(body["members"].as_array().unwrap().len(), 3);
    assert_eq!(body["members"][1]["live"], true);

    let audit = store.list_audit(50).unwrap();
    let entry = audit
        .iter()
        .find(|entry| entry.event == "team.member_set")
        .expect("team.member_set audit");
    let details = entry.details.as_ref().unwrap();
    assert_eq!(details["members"], json!(["analyst", "maker", "watcher"]));
    assert_eq!(details["mode"], "selected");
    assert!(
        store
            .events_after(&admin(&store), 0, 100)
            .unwrap()
            .iter()
            .any(|event| event.kind == "team.member_set")
    );

    let cases = [
        (
            json!({"members": [], "mode": "selected"}),
            "at least one member",
        ),
        (
            json!({"members": ["analyst", "analyst"], "mode": "selected"}),
            "unique",
        ),
        (
            json!({"members": ["bot-1", "bot-2", "bot-3", "bot-4", "bot-5", "bot-6"], "mode": "selected"}),
            "at most 5",
        ),
        (json!({"members": ["ghost"], "mode": "selected"}), "ghost"),
        (json!({"members": ["main"], "mode": "selected"}), "main"),
        (json!({"members": ["legal"], "mode": "selected"}), "legal"),
        (
            json!({"members": ["analyst"], "mode": "everyone"}),
            "selected or all",
        ),
    ];
    for (payload, needle) in cases {
        let response = app
            .clone()
            .oneshot(request(
                &app,
                "PUT",
                "/v1/conversations/room:review/team",
                true,
                Some(payload),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let value = read_json(response).await;
        let message = value["error"].as_str().unwrap().to_owned();
        assert!(
            message.contains(needle),
            "{message} should mention {needle}"
        );
    }

    // Mode `all` is stored as given.
    let response = app
        .clone()
        .oneshot(request(
            &app,
            "PUT",
            "/v1/conversations/room:review/team",
            true,
            Some(json!({"members": ["analyst", "maker"], "mode": "all"})),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = read_json(response).await;
    assert_eq!(body["mode"], "all");
    assert_eq!(body["members"].as_array().unwrap().len(), 2);

    // Group instructions round-trip, newlines included.
    let response = app
        .clone()
        .oneshot(request(
            &app,
            "PUT",
            "/v1/conversations/room:review/team",
            true,
            Some(json!({
                "members": ["analyst"],
                "mode": "selected",
                "rules": "Line one.\nLine two.",
            })),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = read_json(response).await;
    assert_eq!(body["rules"], "Line one.\nLine two.");
    let stored = app
        .clone()
        .oneshot(request(
            &app,
            "GET",
            "/v1/conversations/room:review/team",
            true,
            None,
        ))
        .await
        .unwrap();
    assert_eq!(read_json(stored).await["rules"], "Line one.\nLine two.");

    let over = "x".repeat(4001);
    for (rules, needle) in [(over.as_str(), "4000"), ("bad \u{7} char", "control")] {
        let response = app
            .clone()
            .oneshot(request(
                &app,
                "PUT",
                "/v1/conversations/room:review/team",
                true,
                Some(json!({"members": ["analyst"], "mode": "selected", "rules": rules})),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let message = read_json(response).await["error"]
            .as_str()
            .unwrap()
            .to_owned();
        assert!(
            message.contains(needle),
            "{message} should mention {needle}"
        );
    }

    // Unknown conversation, non-room conversation, unauthenticated call.
    let missing = app
        .clone()
        .oneshot(request(
            &app,
            "PUT",
            "/v1/conversations/room:ghost/team",
            true,
            Some(json!({"members": ["analyst"], "mode": "selected"})),
        ))
        .await
        .unwrap();
    assert_eq!(missing.status(), StatusCode::NOT_FOUND);

    let direct = app
        .clone()
        .oneshot(request(
            &app,
            "POST",
            "/v1/conversations",
            true,
            Some(json!({"kind": "direct", "slug": "analyst"})),
        ))
        .await
        .unwrap();
    assert_eq!(direct.status(), StatusCode::OK);
    let wrong_kind = app
        .clone()
        .oneshot(request(
            &app,
            "PUT",
            "/v1/conversations/dir:analyst/team",
            true,
            Some(json!({"members": ["analyst"], "mode": "selected"})),
        ))
        .await
        .unwrap();
    assert_eq!(wrong_kind.status(), StatusCode::BAD_REQUEST);

    let unauthorized = app
        .clone()
        .oneshot(request(
            &app,
            "GET",
            "/v1/conversations/room:review/team",
            false,
            None,
        ))
        .await
        .unwrap();
    assert_eq!(unauthorized.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn audio_transcribe_requires_a_configured_provider() {
    let (app, _, _) = app();
    let no_filename = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/audio/transcribe")
                .header("authorization", "Bearer test-token-that-is-long-enough-123")
                .body(Body::from(vec![0u8, 1, 2]))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(no_filename.status(), StatusCode::BAD_REQUEST);

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/audio/transcribe")
                .header("authorization", "Bearer test-token-that-is-long-enough-123")
                .header("x-audio-filename", "note.webm")
                .body(Body::from(vec![0u8, 1, 2]))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    let body = read_json(response).await;
    let message = body["error"].as_str().unwrap().to_owned();
    assert!(message.contains("audio.enabled"), "{message}");
    assert!(!message.contains("synthetic-test-key"), "{message}");
}

/// The audio route documents a 20 MiB payload ceiling. The JSON body
/// limit (2 MiB by default) must not apply to it: a 3 MiB body has to
/// reach the handler, which then refuses for an unrelated reason.
#[tokio::test]
async fn audio_transcribe_accepts_payloads_above_the_json_body_limit() {
    let (app, _, _) = app();
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/audio/transcribe")
                .header("authorization", "Bearer test-token-that-is-long-enough-123")
                .header("x-audio-filename", "note.webm")
                .body(Body::from(vec![0u8; 3 * 1024 * 1024]))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_ne!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
    // No audio provider is configured in the test fixture, so the handler
    // gets as far as the provider check: the payload was not truncated or
    // rejected by a body limit.
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    let body = read_json(response).await;
    let message = body["error"].as_str().unwrap().to_owned();
    assert!(message.contains("audio.enabled"), "{message}");
}

/// A payload above the documented 20 MiB ceiling is rejected with a
/// message that names the limit, never with the raw body.
#[tokio::test]
async fn audio_transcribe_rejects_payloads_above_the_telegram_audio_limit() {
    let (app, _, _) = app();
    let limit = crate::audio::DEFAULT_TELEGRAM_AUDIO_MAX_BYTES as usize;
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/audio/transcribe")
                .header("authorization", "Bearer test-token-that-is-long-enough-123")
                .header("x-audio-filename", "note.webm")
                .body(Body::from(vec![0u8; limit + 1024]))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
    let body = read_json(response).await;
    let message = body["error"].as_str().unwrap().to_owned();
    assert!(message.contains("20971520"), "{message}");
    assert!(message.contains("byte limit"), "{message}");
}

/// The JSON endpoints keep the smaller server body limit.
#[tokio::test]
async fn json_routes_keep_the_server_body_limit() {
    let (app, _, _) = app();
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/conversations")
                .header("authorization", "Bearer test-token-that-is-long-enough-123")
                .header("content-type", "application/json")
                .body(Body::from(vec![b'a'; 3 * 1024 * 1024]))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
}

// ---- Auth endpoints ----

/// A member account plus a session token minted directly in the store,
/// for exercising role gates without the OIDC flow.
fn member_with_session(store: &Store) -> (User, String) {
    let user = store
        .create_user("microsoft:api-member", "member@example.example", "Member")
        .unwrap();
    let token = store
        .mint_session(&user.id, chrono::Duration::hours(1))
        .unwrap();
    (user, token)
}

#[tokio::test]
async fn providers_endpoint_is_public_and_matches_the_config() {
    // Default config: sign-in off, every provider reports disabled.
    let (app, _, _) = app();
    let response = app
        .clone()
        .oneshot(request(&app, "GET", "/v1/auth/providers", false, None))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = read_json(response).await;
    let providers = body["providers"].as_array().unwrap().clone();
    assert_eq!(providers.len(), 3);
    for row in &providers {
        assert_eq!(row["enabled"], json!(false), "auth disabled ⇒ all off");
    }
    assert_eq!(providers[0]["id"], json!("microsoft"));
    assert_eq!(providers[1]["id"], json!("google"));
    assert_eq!(providers[2]["id"], json!("github"));

    // Auth enabled: microsoft on, the others visibly disabled.
    let (app, _, _, _) = auth_app().await;
    let response = app
        .clone()
        .oneshot(request(&app, "GET", "/v1/auth/providers", false, None))
        .await
        .unwrap();
    let body = read_json(response).await;
    let providers = body["providers"].as_array().unwrap().clone();
    assert_eq!(
        providers,
        vec![
            json!({"id": "microsoft", "enabled": true}),
            json!({"id": "google", "enabled": false}),
            json!({"id": "github", "enabled": false}),
        ]
    );
}

/// The voice settings surface reports readiness without leaking anything,
/// and only the admin may change it.
#[tokio::test]
async fn voice_settings_report_readiness_and_are_admin_writable() {
    let (router, _, store) = app();
    let response = router
        .clone()
        .oneshot(request(&router, "GET", "/v1/settings/voice", true, None))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = read_json(response).await;
    // Defaults: audio and OCR are off, so nothing is ready.
    assert_eq!(body["stt"]["ready"], json!(false));
    assert_eq!(body["tts"]["ready"], json!(false));
    assert_eq!(body["ocr"]["ready"], json!(false));
    assert_eq!(body["reply_mode"], json!("text"));
    assert_eq!(body["ocr"]["model"], json!("mistral-ocr-latest"));
    assert_eq!(body["tts"]["model"], json!("voxtral-mini-tts-2603"));
    // The reason names a setting, never a secret.
    let detail = body["ocr"]["detail"].as_str().unwrap_or_default();
    assert!(detail.contains("ocr.enabled"), "{detail}");

    // A member cannot write it.
    let member = store
        .create_user("microsoft:voice-member", "v@example.example", "V")
        .unwrap();
    let member_token = store
        .mint_session(&member.id, chrono::Duration::hours(1))
        .unwrap();
    let response = router
        .clone()
        .oneshot(request_with_token(
            "PUT",
            "/v1/settings/voice",
            &member_token,
            Some(json!({"reply_mode": "voice"})),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);

    // The admin can, but an invalid mode is rejected.
    let response = router
        .clone()
        .oneshot(request(
            &router,
            "PUT",
            "/v1/settings/voice",
            true,
            Some(json!({"reply_mode": "shout"})),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
}

/// Per-turn work limits: the owner reads, sets, validates and resets their own
/// override; a member's override is separate.
#[tokio::test]
async fn turn_limits_are_per_user_validated_and_resettable() {
    let (router, _, store) = app();
    let response = router
        .clone()
        .oneshot(request(
            &router,
            "GET",
            "/v1/settings/turn-limits",
            true,
            None,
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = read_json(response).await;
    assert_eq!(body["max_tool_calls_per_turn"], json!(1500));
    assert_eq!(body["max_tool_rounds"], json!(200));
    assert_eq!(body["ceiling"]["max_tool_calls_per_turn"], json!(1500));
    assert_eq!(body["ceiling"]["max_tool_rounds"], json!(1500));
    assert_eq!(body["overridden"], json!(false));

    let response = router
        .clone()
        .oneshot(request(
            &router,
            "PUT",
            "/v1/settings/turn-limits",
            true,
            Some(json!({"max_tool_calls_per_turn": 40, "max_tool_rounds": 12})),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = read_json(response).await;
    assert_eq!(body["max_tool_calls_per_turn"], json!(40));
    assert_eq!(body["max_tool_rounds"], json!(12));
    assert_eq!(body["overridden"], json!(true));

    // Above the ceiling, or zero, is refused and nothing changes.
    for bad in [
        json!({"max_tool_calls_per_turn": 1501, "max_tool_rounds": 12}),
        json!({"max_tool_calls_per_turn": 40, "max_tool_rounds": 0}),
        json!({"max_tool_calls_per_turn": 40, "max_tool_rounds": 1501}),
    ] {
        let response = router
            .clone()
            .oneshot(request(
                &router,
                "PUT",
                "/v1/settings/turn-limits",
                true,
                Some(bad),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
    }
    let response = router
        .clone()
        .oneshot(request(
            &router,
            "GET",
            "/v1/settings/turn-limits",
            true,
            None,
        ))
        .await
        .unwrap();
    assert_eq!(
        read_json(response).await["max_tool_calls_per_turn"],
        json!(40)
    );

    // A member sees the defaults, not the owner's override.
    let member = store
        .create_user("microsoft:limits-member", "l@example.example", "L")
        .unwrap();
    let member_token = store
        .mint_session(&member.id, chrono::Duration::hours(1))
        .unwrap();
    let response = router
        .clone()
        .oneshot(request_with_token(
            "GET",
            "/v1/settings/turn-limits",
            &member_token,
            None,
        ))
        .await
        .unwrap();
    let body = read_json(response).await;
    assert_eq!(body["max_tool_calls_per_turn"], json!(1500));
    assert_eq!(body["overridden"], json!(false));

    // Reset drops the owner's override.
    let response = router
        .clone()
        .oneshot(request(
            &router,
            "DELETE",
            "/v1/settings/turn-limits",
            true,
            None,
        ))
        .await
        .unwrap();
    let body = read_json(response).await;
    assert_eq!(body["max_tool_calls_per_turn"], json!(1500));
    assert_eq!(body["overridden"], json!(false));
}

/// Reply mode is per conversation and per user.
#[tokio::test]
async fn reply_mode_round_trips_per_conversation() {
    let (app, _, _) = app();
    let response = app
        .clone()
        .oneshot(request(
            &app,
            "GET",
            "/v1/conversations/dir:main/reply-mode",
            true,
            None,
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = read_json(response).await;
    assert_eq!(body["mode"], json!("text"));
    assert_eq!(body["voice_ready"], json!(false));

    let response = app
        .clone()
        .oneshot(request(
            &app,
            "PUT",
            "/v1/conversations/dir:main/reply-mode",
            true,
            Some(json!({"mode": "voice"})),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = read_json(response).await;
    assert_eq!(body["mode"], json!("voice"));

    let response = app
        .clone()
        .oneshot(request(
            &app,
            "PUT",
            "/v1/conversations/dir:main/reply-mode",
            true,
            Some(json!({"mode": "shout"})),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
}

#[tokio::test]
async fn login_start_and_callback_fail_closed_when_auth_is_disabled() {
    let (app, _, _) = app();
    let response = app
        .clone()
        .oneshot(request(
            &app,
            "POST",
            "/v1/auth/login/start",
            false,
            Some(json!({"provider": "microsoft"})),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);

    let response = app
        .clone()
        .oneshot(request(
            &app,
            "GET",
            "/v1/auth/callback?state=s&code=c",
            false,
            None,
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    let page = read_body(response).await;
    assert!(page.contains("Sign-in is not enabled"));
    assert!(!page.contains("<script"));
}

#[tokio::test]
async fn login_start_returns_the_authorize_url_and_rejects_bad_providers() {
    let (app, _, store, _) = auth_app().await;
    let admin = store.admin_user_id().unwrap();
    store
        .mint_invite(
            "api-invite-code-0123456789",
            1,
            Utc::now() + chrono::Duration::hours(1),
            &admin,
        )
        .unwrap();
    let response = app
        .clone()
        .oneshot(request(
            &app,
            "POST",
            "/v1/auth/login/start",
            false,
            Some(json!({"provider": "microsoft", "invite_code": "api-invite-code-0123456789"})),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = read_json(response).await;
    let url = body["url"].as_str().unwrap();
    assert!(url.contains("/authorize?"), "{url}");
    assert!(url.contains("code_challenge_method=S256"), "{url}");

    // Unknown provider and spent invites fail closed.
    let response = app
        .clone()
        .oneshot(request(
            &app,
            "POST",
            "/v1/auth/login/start",
            false,
            Some(json!({"provider": "google"})),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);

    let response = app
        .clone()
        .oneshot(request(
            &app,
            "POST",
            "/v1/auth/login/start",
            false,
            Some(json!({"provider": "microsoft", "invite_code": "not-a-real-invite"})),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CONFLICT);
}

fn s256(verifier: &str) -> String {
    use base64::Engine as _;
    use sha2::Digest as _;
    base64::engine::general_purpose::URL_SAFE_NO_PAD
        .encode(sha2::Sha256::digest(verifier.as_bytes()))
}

/// Run `login/start` for a new member with a spare invite and the given
/// extra body fields, returning `(state, nonce)` from the authorize URL.
async fn start_login(
    app: &Router,
    store: &Store,
    invite: Option<&str>,
    extra: Value,
) -> (String, String) {
    let mut body = json!({"provider": "microsoft"});
    if let Some(invite) = invite {
        let admin = store.admin_user_id().unwrap();
        store
            .mint_invite(invite, 1, Utc::now() + chrono::Duration::hours(1), &admin)
            .unwrap();
        body["invite_code"] = json!(invite);
    }
    for (key, value) in extra.as_object().unwrap() {
        body[key] = value.clone();
    }
    let response = app
        .clone()
        .oneshot(request(
            app,
            "POST",
            "/v1/auth/login/start",
            false,
            Some(body),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let start = read_json(response).await;
    crate::auth::test_idp::url_params(start["url"].as_str().unwrap())
}

async fn finish_callback(
    app: &Router,
    idp: &crate::auth::test_idp::MockIdp,
    state: &str,
    nonce: &str,
    subject: &str,
) -> Response {
    let code = format!("api-code-{subject}-{state}");
    let id_token = crate::auth::test_idp::synthetic_id_token(&idp.issuer, nonce, subject);
    crate::auth::test_idp::stub_token(&idp.server, &code, &id_token).await;
    app.clone()
        .oneshot(request(
            app,
            "GET",
            &format!("/v1/auth/callback?state={state}&code={code}"),
            false,
            None,
        ))
        .await
        .unwrap()
}

fn location(response: &Response) -> String {
    response
        .headers()
        .get("location")
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_owned()
}

#[tokio::test]
async fn web_callback_shows_no_token_and_still_creates_the_account() {
    let (app, _, store, idp) = auth_app().await;
    let (state, nonce) =
        start_login(&app, &store, Some("callback-invite-0123456789"), json!({})).await;
    let response = finish_callback(&app, &idp, &state, &nonce, "subject-api").await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok()),
        Some("text/html; charset=utf-8")
    );
    assert_eq!(
        response
            .headers()
            .get("content-security-policy")
            .and_then(|v| v.to_str().ok()),
        Some(crate::web::CSP)
    );
    let page = read_body(response).await;
    assert!(!page.contains("<script"));
    assert!(!page.contains("<code>"), "no token is displayed");
    assert!(page.contains("Sign-in succeeded"));
    assert!(page.contains("return to the SeraBot app"));

    let account = store
        .get_user_by_subject("microsoft:subject-api")
        .unwrap()
        .unwrap();
    // No session was minted for the plain web flow.
    let connection = store.connection().unwrap();
    let sessions: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM device_tokens WHERE user_id = ?1",
            [&account.id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(sessions, 0);
    // The post-login hook seeded the default fleet in the same request.
    let bots = store.list_bots(&account.id).unwrap();
    assert_eq!(bots.len(), 1);
    assert_eq!(bots[0].name, "main");
    assert!(!bots[0].prompt.is_empty());

    // An unknown state renders the error page, not a stack trace.
    let response = app
        .clone()
        .oneshot(request(
            &app,
            "GET",
            "/v1/auth/callback?state=bogus&code=whatever",
            false,
            None,
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let page = read_body(response).await;
    assert!(page.contains("could not be completed"));
    assert!(!page.contains("<script"));
}

#[tokio::test]
async fn native_handoff_flow_issues_a_session_exactly_once() {
    let (app, _, store, idp) = auth_app().await;
    let verifier = {
        use base64::Engine as _;
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode([7u8; 32])
    };
    assert_eq!(verifier.len(), 43);
    let (state, nonce) = start_login(
        &app,
        &store,
        Some("handoff-invite-0123456789"),
        json!({
            "return_to": "http://127.0.0.1:51121/auth-done",
            "handoff_challenge": s256(&verifier),
            "client_state": "client-state-0123456789",
        }),
    )
    .await;
    let response = finish_callback(&app, &idp, &state, &nonce, "subject-native").await;
    assert_eq!(response.status(), StatusCode::FOUND);
    assert_eq!(
        response
            .headers()
            .get("cache-control")
            .and_then(|v| v.to_str().ok()),
        Some("no-store")
    );
    let target = url::Url::parse(&location(&response)).unwrap();
    assert_eq!(target.host_str(), Some("127.0.0.1"));
    assert_eq!(target.port(), Some(51121));
    assert_eq!(target.path(), "/auth-done");
    let params: std::collections::HashMap<_, _> = target.query_pairs().into_owned().collect();
    assert_eq!(params["state"], "client-state-0123456789");
    let handoff = params["handoff"].clone();
    assert!(!params.contains_key("error"));
    // Only the hash of the code is stored.
    let raw = std::fs::read(store.path()).unwrap();
    assert!(!String::from_utf8_lossy(&raw).contains(&handoff));

    // Wrong verifier fails and burns the code, so the right one is too late.
    let wrong = app
        .clone()
        .oneshot(request(
            &app,
            "POST",
            "/v1/auth/handoff",
            false,
            Some(json!({"handoff": handoff, "verifier": "x".repeat(43)})),
        ))
        .await
        .unwrap();
    assert_eq!(wrong.status(), StatusCode::BAD_REQUEST);
    assert_eq!(read_json(wrong).await["error"], json!("invalid handoff"));
    let burned = app
        .clone()
        .oneshot(request(
            &app,
            "POST",
            "/v1/auth/handoff",
            false,
            Some(json!({"handoff": handoff, "verifier": verifier})),
        ))
        .await
        .unwrap();
    assert_eq!(burned.status(), StatusCode::BAD_REQUEST);

    // A second sign-in redeems cleanly, once.
    let (state, nonce) = start_login(
        &app,
        &store,
        None,
        json!({
            "return_to": "serabot://auth",
            "handoff_challenge": s256(&verifier),
            "client_state": "client-state-0123456789",
        }),
    )
    .await;
    let response = finish_callback(&app, &idp, &state, &nonce, "subject-native").await;
    assert_eq!(response.status(), StatusCode::FOUND);
    let target = url::Url::parse(&location(&response)).unwrap();
    assert_eq!(target.scheme(), "serabot");
    let params: std::collections::HashMap<_, _> = target.query_pairs().into_owned().collect();
    let handoff = params["handoff"].clone();
    let redeem = app
        .clone()
        .oneshot(request(
            &app,
            "POST",
            "/v1/auth/handoff",
            false,
            Some(json!({"handoff": handoff, "verifier": verifier})),
        ))
        .await
        .unwrap();
    assert_eq!(redeem.status(), StatusCode::OK);
    let body = read_json(redeem).await;
    let token = body["token"].as_str().unwrap().to_owned();
    assert_eq!(token.len(), Store::SESSION_TOKEN_BYTES * 2);
    assert_eq!(body["user"]["role"], json!("member"));
    assert_eq!(body["user"]["email"], json!("member@example.example"));
    assert!(body["expires_at"].as_str().is_some());

    let me = app
        .clone()
        .oneshot(request_with_token("GET", "/v1/me", &token, None))
        .await
        .unwrap();
    assert_eq!(me.status(), StatusCode::OK);
    assert_eq!(read_json(me).await, body["user"]);
    let anonymous = app
        .clone()
        .oneshot(request(&app, "GET", "/v1/me", false, None))
        .await
        .unwrap();
    assert_eq!(anonymous.status(), StatusCode::UNAUTHORIZED);

    let again = app
        .clone()
        .oneshot(request(
            &app,
            "POST",
            "/v1/auth/handoff",
            false,
            Some(json!({"handoff": handoff, "verifier": verifier})),
        ))
        .await
        .unwrap();
    assert_eq!(again.status(), StatusCode::BAD_REQUEST);

    // Audited without leaking the code or token.
    let audit = store.list_audit(100).unwrap();
    for event in [
        "auth.handoff.issued",
        "auth.handoff.redeemed",
        "auth.handoff.failed",
    ] {
        assert!(audit.iter().any(|entry| entry.event == event), "{event}");
    }
    let dump = format!("{audit:?}");
    assert!(!dump.contains(&token) && !dump.contains(&handoff));
}

#[tokio::test]
async fn native_failures_redirect_with_a_short_error_code() {
    let (app, _, store, idp) = auth_app().await;
    let extra = json!({
        "return_to": "http://127.0.0.1:51122/auth-done",
        "handoff_challenge": s256(&"v".repeat(43)),
        "client_state": "client-state-abcdefgh",
    });
    // A new subject without an invite.
    let (state, nonce) = start_login(&app, &store, None, extra.clone()).await;
    let response = finish_callback(&app, &idp, &state, &nonce, "subject-uninvited").await;
    assert_eq!(response.status(), StatusCode::FOUND);
    let target = location(&response);
    assert!(
        target.starts_with(
            "http://127.0.0.1:51122/auth-done?error=invite_required&state=client-state-abcdefgh"
        ),
        "{target}"
    );
    assert!(!target.contains("handoff="));

    // The provider reports that the user declined.
    let (state, _) = start_login(&app, &store, None, extra).await;
    let response = app
        .clone()
        .oneshot(request(
            &app,
            "GET",
            &format!("/v1/auth/callback?state={state}&error=access_denied"),
            false,
            None,
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::FOUND);
    assert!(location(&response).contains("error=denied"));
}

#[tokio::test]
async fn login_start_enforces_the_return_to_allowlist_and_all_or_none() {
    let (app, _, _, _) = auth_app().await;
    let challenge = s256(&"c".repeat(43));
    let post = |body: Value| {
        let app = app.clone();
        async move {
            app.clone()
                .oneshot(request(
                    &app,
                    "POST",
                    "/v1/auth/login/start",
                    false,
                    Some(body),
                ))
                .await
                .unwrap()
                .status()
        }
    };
    for return_to in [
        "https://evil.example/auth-done",
        "http://127.0.0.1:80/auth-done",
        "http://127.0.0.1:1023/auth-done",
        "http://127.0.0.1:51121/other",
        "http://127.0.0.1:51121/auth-done?x=1",
        "http://localhost:51121/auth-done",
        "http://127.0.0.1.evil.example:51121/auth-done",
        "http://user@127.0.0.1:51121/auth-done",
        "serabot://auth/extra",
        "serabot://other",
    ] {
        assert_eq!(
            post(json!({
                "provider": "microsoft",
                "return_to": return_to,
                "handoff_challenge": challenge,
                "client_state": "client-state-0123456789",
            }))
            .await,
            StatusCode::BAD_REQUEST,
            "{return_to}"
        );
    }
    // Partial sets and malformed values are 400s too.
    for body in [
        json!({"provider": "microsoft", "return_to": "serabot://auth"}),
        json!({"provider": "microsoft", "handoff_challenge": challenge}),
        json!({"provider": "microsoft", "return_to": "serabot://auth", "handoff_challenge": challenge}),
        json!({"provider": "microsoft", "return_to": "serabot://auth", "handoff_challenge": "short", "client_state": "client-state-0123456789"}),
        json!({"provider": "microsoft", "return_to": "serabot://auth", "handoff_challenge": challenge, "client_state": "no"}),
    ] {
        assert_eq!(post(body).await, StatusCode::BAD_REQUEST);
    }
    assert_eq!(
        post(json!({
            "provider": "microsoft",
            "return_to": "serabot://auth",
            "handoff_challenge": challenge,
            "client_state": "client-state-0123456789",
        }))
        .await,
        StatusCode::OK
    );
}

#[tokio::test]
async fn member_sessions_are_rejected_on_admin_endpoints() {
    let (app, _, store, _) = auth_app().await;
    let (_, member_token) = member_with_session(&store);

    // Unauthenticated: 401 everywhere.
    for (method, uri, body) in [
        ("GET", "/v1/users", None),
        ("POST", "/v1/auth/invites", Some(json!({"uses": 1}))),
    ] {
        let response = app
            .clone()
            .oneshot(request(&app, method, uri, false, body))
            .await
            .unwrap();
        assert_eq!(
            response.status(),
            StatusCode::UNAUTHORIZED,
            "{method} {uri}"
        );
    }

    // Member: 403 on admin endpoints, 200 on ordinary ones.
    for (method, uri, body) in [
        ("GET", "/v1/users", None),
        ("POST", "/v1/auth/invites", Some(json!({"uses": 1}))),
    ] {
        let response = app
            .clone()
            .oneshot(request_with_token(method, uri, &member_token, body))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::FORBIDDEN, "{method} {uri}");
    }
    let response = app
        .clone()
        .oneshot(request_with_token("GET", "/v1/health", &member_token, None))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    // The operator's static config token is the admin.
    let response = app
        .clone()
        .oneshot(request(&app, "GET", "/v1/users", true, None))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = read_json(response).await;
    let users = body["users"].as_array().unwrap();
    assert_eq!(users.len(), 2, "bootstrap admin + member");
    assert!(users.iter().any(|user| user["role"] == json!("admin")));
}

#[tokio::test]
async fn invites_are_minted_once_and_stored_hashed() {
    let (app, _, store, _) = auth_app().await;
    let response = app
        .clone()
        .oneshot(request(
            &app,
            "POST",
            "/v1/auth/invites",
            true,
            Some(json!({"uses": 3})),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = read_json(response).await;
    let code = body["code"].as_str().unwrap().to_owned();
    assert!(!code.is_empty());
    assert!(store.invite_is_redeemable(&sha256_hex(&code)).unwrap());

    // Bounds are enforced.
    for uses in [0, 101] {
        let response = app
            .clone()
            .oneshot(request(
                &app,
                "POST",
                "/v1/auth/invites",
                true,
                Some(json!({"uses": uses})),
            ))
            .await
            .unwrap();
        assert_eq!(
            response.status(),
            StatusCode::UNPROCESSABLE_ENTITY,
            "{uses}"
        );
    }

    // The audit row names the actor and uses, never the code.
    let rows: Vec<(String, String)> = store
        .connection()
        .unwrap()
        .prepare("SELECT actor, details_json FROM audit_log WHERE event = 'auth.invite_minted'")
        .unwrap()
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })
        .unwrap()
        .collect::<std::result::Result<Vec<_>, _>>()
        .unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].0, store.admin_user_id().unwrap());
    assert!(!rows[0].1.contains(&code));
}

#[tokio::test]
async fn logout_revokes_sessions_but_not_the_config_token() {
    let (app, _, store, _) = auth_app().await;
    let (_, member_token) = member_with_session(&store);

    let response = app
        .clone()
        .oneshot(request_with_token(
            "POST",
            "/v1/auth/logout",
            &member_token,
            None,
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    // The revoked session no longer authorizes.
    let response = app
        .clone()
        .oneshot(request_with_token("GET", "/v1/health", &member_token, None))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

    // The static config token has no session row: fail closed.
    let response = app
        .clone()
        .oneshot(request(&app, "POST", "/v1/auth/logout", true, None))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CONFLICT);
}

#[tokio::test]
async fn admin_can_disable_and_enable_members() {
    let (app, _, store, _) = auth_app().await;
    let (member, member_token) = member_with_session(&store);

    // Disabling a member stops their sessions immediately.
    let response = app
        .clone()
        .oneshot(request(
            &app,
            "POST",
            &format!("/v1/users/{}/disable", member.id),
            true,
            None,
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let response = app
        .clone()
        .oneshot(request_with_token("GET", "/v1/health", &member_token, None))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

    // The directory reflects the status.
    let response = app
        .clone()
        .oneshot(request(&app, "GET", "/v1/users", true, None))
        .await
        .unwrap();
    let body = read_json(response).await;
    let users = body["users"].as_array().unwrap().clone();
    let entry = users
        .iter()
        .find(|user| user["id"] == json!(member.id))
        .unwrap();
    assert_eq!(entry["status"], json!("disabled"));

    // Re-enabling restores access.
    let response = app
        .clone()
        .oneshot(request(
            &app,
            "POST",
            &format!("/v1/users/{}/enable", member.id),
            true,
            None,
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let response = app
        .clone()
        .oneshot(request_with_token("GET", "/v1/health", &member_token, None))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    // Unknown users and the admin account itself are rejected.
    let response = app
        .clone()
        .oneshot(request(
            &app,
            "POST",
            "/v1/users/no-such-user/disable",
            true,
            None,
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    let admin_id = store.admin_user_id().unwrap();
    let response = app
        .clone()
        .oneshot(request(
            &app,
            "POST",
            &format!("/v1/users/{admin_id}/disable"),
            true,
            None,
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);

    // Members cannot flip statuses at all.
    let response = app
        .clone()
        .oneshot(request_with_token(
            "POST",
            &format!("/v1/users/{}/disable", member.id),
            &member_token,
            None,
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn login_page_link_starts_the_microsoft_flow() {
    let (app, _, _, idp) = auth_app().await;
    let response = app
        .clone()
        .oneshot(request(&app, "GET", "/login/microsoft", false, None))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::FOUND);
    let location = response
        .headers()
        .get("location")
        .and_then(|value| value.to_str().ok())
        .unwrap()
        .to_owned();
    assert!(
        location.starts_with(&format!("{}/authorize", idp.issuer)),
        "{location}"
    );
}

// ---- Per-user fleets and skills (phase 3) ----

/// A member's fleet seeds from the templates independently of the admin's,
/// and no bot or skill of one user is readable, updatable, or deletable
/// from the other's session.
#[tokio::test]
async fn member_bots_and_skills_are_isolated_and_validate_per_user() {
    let (app, _, store) = app_with_team();
    let admin_id = admin(&store);
    let (member, member_token) = member_with_session(&store);

    // The admin creates a bot and a skill.
    let created = app
        .clone()
        .oneshot(request(
            &app,
            "POST",
            "/v1/bots",
            true,
            Some(json!({"id": "admin-notes", "description": "Admin notes."})),
        ))
        .await
        .unwrap();
    assert_eq!(created.status(), StatusCode::OK);
    let created = app
        .clone()
        .oneshot(request(
            &app,
            "POST",
            "/v1/skills",
            true,
            Some(json!({"id": "admin-skill", "description": "Admin only"})),
        ))
        .await
        .unwrap();
    assert_eq!(created.status(), StatusCode::OK);

    // The member's first list seeds the default fleet from the templates.
    let listed = read_json(
        app.clone()
            .oneshot(request_with_token("GET", "/v1/bots", &member_token, None))
            .await
            .unwrap(),
    )
    .await;
    let ids = listed["bots"]
        .as_array()
        .unwrap()
        .iter()
        .map(|bot| bot["id"].as_str().unwrap().to_owned())
        .collect::<Vec<_>>();
    assert_eq!(ids, vec!["analyst", "main", "maker", "watcher"]);
    assert_eq!(store.count_bots(&member.id).unwrap(), 4);
    assert_eq!(store.count_bots(&admin_id).unwrap(), 5);

    // The member's own detail works without an admin profile; the
    // display name falls back to the bot id.
    let own = read_json(
        app.clone()
            .oneshot(request_with_token(
                "GET",
                "/v1/bots/main",
                &member_token,
                None,
            ))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(own["id"], "main");
    assert_eq!(own["display_name"], "main");
    assert_eq!(own["role"], "orchestrator");

    // The admin's bot is invisible to the member through every verb.
    for (method, body) in [
        ("GET", None),
        ("PUT", Some(json!({"prompt": "hijacked"}))),
        ("DELETE", None),
    ] {
        let response = app
            .clone()
            .oneshot(request_with_token(
                method,
                "/v1/bots/admin-notes",
                &member_token,
                body,
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND, "{method}");
    }
    assert!(
        store
            .get_bot(&admin_id, "admin-notes")
            .unwrap()
            .unwrap()
            .prompt
            .contains("Admin notes.")
    );

    // The admin's skill is invisible; the member's own skill is not.
    let listed = read_json(
        app.clone()
            .oneshot(request_with_token("GET", "/v1/skills", &member_token, None))
            .await
            .unwrap(),
    )
    .await;
    assert!(
        !listed["skills"]
            .as_array()
            .unwrap()
            .iter()
            .any(|skill| skill["id"] == "admin-skill")
    );
    let created = app
        .clone()
        .oneshot(request_with_token(
            "POST",
            "/v1/skills",
            &member_token,
            Some(json!({"id": "member-skill", "description": "Member only"})),
        ))
        .await
        .unwrap();
    assert_eq!(created.status(), StatusCode::OK);
    assert!(
        store
            .get_user_skill(&member.id, "member-skill")
            .unwrap()
            .is_some()
    );
    let admin_skills = read_json(
        app.clone()
            .oneshot(request(&app, "GET", "/v1/skills", true, None))
            .await
            .unwrap(),
    )
    .await;
    assert!(
        !admin_skills["skills"]
            .as_array()
            .unwrap()
            .iter()
            .any(|skill| skill["id"] == "member-skill")
    );

    // The orchestrator rule is per user: the member cannot disable main,
    // and the admin's main is untouched.
    let disable = app
        .clone()
        .oneshot(request_with_token(
            "PUT",
            "/v1/bots/main",
            &member_token,
            Some(json!({"enabled": false})),
        ))
        .await
        .unwrap();
    assert_eq!(disable.status(), StatusCode::UNPROCESSABLE_ENTITY);
    assert!(store.get_bot(&member.id, "main").unwrap().unwrap().enabled);
    assert!(store.get_bot(&admin_id, "main").unwrap().unwrap().enabled);

    // A member's prompt edit and specialist disable/delete are scoped.
    let updated = app
        .clone()
        .oneshot(request_with_token(
            "PUT",
            "/v1/bots/analyst",
            &member_token,
            Some(json!({"enabled": false, "prompt": "Member analyst prompt."})),
        ))
        .await
        .unwrap();
    assert_eq!(updated.status(), StatusCode::OK);
    let member_analyst = store.get_bot(&member.id, "analyst").unwrap().unwrap();
    assert!(!member_analyst.enabled);
    assert_eq!(member_analyst.prompt, "Member analyst prompt.");
    let admin_analyst = store.get_bot(&admin_id, "analyst").unwrap().unwrap();
    assert!(admin_analyst.enabled);
    assert_eq!(admin_analyst.prompt, "test prompt");

    let deleted = app
        .clone()
        .oneshot(request_with_token(
            "DELETE",
            "/v1/bots/maker",
            &member_token,
            None,
        ))
        .await
        .unwrap();
    assert_eq!(deleted.status(), StatusCode::OK);
    assert!(store.get_bot(&member.id, "maker").unwrap().is_none());
    assert!(store.get_bot(&admin_id, "maker").unwrap().is_some());

    // The orchestrator cannot be deleted either.
    let delete_main = app
        .clone()
        .oneshot(request_with_token(
            "DELETE",
            "/v1/bots/main",
            &member_token,
            None,
        ))
        .await
        .unwrap();
    assert_eq!(delete_main.status(), StatusCode::UNPROCESSABLE_ENTITY);
}

#[tokio::test]
async fn conversation_list_carries_a_plain_last_message_preview_for_the_acting_user_only() {
    let (app, _, store) = app_with_policy(true, Policy::from_text("synthetic blocked phrase"));
    let admin_id = admin(&store);
    store
        .create_conversation(&admin_id, "dir:analyst", "direct", None)
        .unwrap();
    store
        .create_conversation(&admin_id, "dir:maker", "direct", None)
        .unwrap();
    store
        .append_message(
            &admin_id,
            "dir:analyst",
            None,
            crate::domain::Role::User,
            "first",
        )
        .unwrap();
    let long = format!("**Bold** `code`\n\n# heading {}", "x".repeat(300));
    store
        .append_message(
            &admin_id,
            "dir:analyst",
            None,
            crate::domain::Role::Assistant,
            &long,
        )
        .unwrap();
    store
        .append_message(
            &admin_id,
            "dir:maker",
            None,
            crate::domain::Role::User,
            "synthetic blocked phrase",
        )
        .unwrap();

    // Another user's chat with the same id never shows up for the admin.
    let member = store
        .create_user("microsoft:preview", "p@example.example", "P")
        .unwrap();
    store
        .create_conversation(&member.id, "dir:other", "direct", None)
        .unwrap();
    store
        .append_message(
            &member.id,
            "dir:other",
            None,
            crate::domain::Role::User,
            "member secret chatter",
        )
        .unwrap();

    let body = read_json(
        app.clone()
            .oneshot(request(&app, "GET", "/v1/conversations", true, None))
            .await
            .unwrap(),
    )
    .await;
    let rows = body["conversations"].as_array().unwrap();
    let row = |id: &str| rows.iter().find(|row| row["id"] == id).unwrap();
    let analyst = &row("dir:analyst")["last_message"];
    assert_eq!(analyst["role"], "assistant");
    let text = analyst["text"].as_str().unwrap();
    assert_eq!(text.chars().count(), 120);
    assert!(text.starts_with("Bold code heading x"));
    assert!(!text.contains(['*', '`', '#', '\n']));
    assert_eq!(
        row("dir:maker")["last_message"]["text"],
        "[Content withheld by policy]"
    );
    assert!(!body.to_string().contains("member secret chatter"));

    let token = store
        .mint_session(&member.id, chrono::Duration::hours(1))
        .unwrap();
    let theirs = read_json(
        app.clone()
            .oneshot(request_with_token("GET", "/v1/conversations", &token, None))
            .await
            .unwrap(),
    )
    .await;
    assert!(theirs.to_string().contains("member secret chatter"));
    assert!(!theirs.to_string().contains("Bold code"));
}

#[tokio::test]
async fn usage_scope_all_is_admin_only_and_members_only_see_their_own_rows() {
    let (app, _, store) = app();
    let admin_id = store.admin_user_id().unwrap();
    let (member, member_token) = member_with_session(&store);
    let record = |actor: &str, task_id: Option<&str>, tokens: i64, model: &str| {
        store
            .record_audit(
                actor,
                "model.used",
                None,
                task_id,
                None,
                None,
                Some("policy-synthetic"),
                Some(&json!({
                    "provider": "default",
                    "model": model,
                    "usage": {"prompt_tokens": tokens, "completion_tokens": 1, "total_tokens": tokens + 1},
                })),
            )
            .unwrap();
    };
    record(&admin_id, None, 10, "admin-model");
    record(&member.id, None, 100, "member-model");
    for (actor, tool, event) in [
        (&admin_id, "web_search", "tool.completed"),
        (&member.id, "read_file", "tool.completed"),
        (&member.id, "read_file", "tool.failed"),
    ] {
        store
            .record_audit(actor, event, Some(tool), None, None, None, None, None)
            .unwrap();
    }

    // The member sees only their own model and tool rows.
    let own = read_json(
        app.clone()
            .oneshot(request_with_token("GET", "/v1/usage", &member_token, None))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(own["scope"], "me");
    assert_eq!(own["totals"]["calls"], 1);
    assert_eq!(own["per_model"].as_array().unwrap().len(), 1);
    assert_eq!(own["per_model"][0]["model"], "member-model");
    assert_eq!(own["per_model"][0]["total_tokens"], 101);
    assert_eq!(own["tools"].as_array().unwrap().len(), 1);
    assert_eq!(own["tools"][0]["tool"], "read_file");
    assert_eq!(own["tools"][0]["completed"], 1);
    assert_eq!(own["tools"][0]["failed"], 1);

    // A member cannot widen the scope; nothing leaks in the error.
    let denied = app
        .clone()
        .oneshot(request_with_token(
            "GET",
            "/v1/usage?scope=all",
            &member_token,
            None,
        ))
        .await
        .unwrap();
    assert_eq!(denied.status(), StatusCode::FORBIDDEN);

    // An unknown scope is rejected.
    let bad = app
        .clone()
        .oneshot(request(&app, "GET", "/v1/usage?scope=everyone", true, None))
        .await
        .unwrap();
    assert_eq!(bad.status(), StatusCode::UNPROCESSABLE_ENTITY);

    // The admin sees only their own rows by default and everyone's on request.
    let admin_own = read_json(
        app.clone()
            .oneshot(request(&app, "GET", "/v1/usage", true, None))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(admin_own["totals"]["calls"], 1);
    assert_eq!(admin_own["per_model"][0]["model"], "admin-model");
    let all = read_json(
        app.clone()
            .oneshot(request(&app, "GET", "/v1/usage?scope=all", true, None))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(all["scope"], "all");
    assert_eq!(all["totals"]["calls"], 2);
    assert_eq!(all["per_model"].as_array().unwrap().len(), 2);
    assert_eq!(all["tools"].as_array().unwrap().len(), 2);
}
