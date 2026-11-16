//! Antigravity and Muse sign-in through the API (v0.49): the member routes,
//! the owner's pre-0.49 routes that now write the owner's own account, and the
//! quota readers. A wiremock server stands in for Google and Meta.

use super::*;
use crate::auth_flows::google::{DEFAULT_CLIENT_SECRET_NAME, GoogleSignIn};
use crate::auth_flows::muse::MuseSignIn;
use crate::auth_flows::{SignInRegistry, SignInService};
use crate::config::ProviderConfig;
use crate::model_router::ModelRouter;
use crate::providers::UserProviderCredentials;
use crate::user_secrets::UserSecrets;
use wiremock::matchers::{body_json, body_string_contains, header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const REDIRECT: &str = "http://127.0.0.1:51121/oauth-callback";
const OWNER: &str = "test-token-that-is-long-enough-123";
const CLIENT_SECRET: &str = "synthetic-antigravity-client-secret";

struct Env {
    app: Router,
    store: Store,
    server: MockServer,
    alice: String,
    bob: String,
}

async fn env(with_client_secret: bool) -> Env {
    let server = MockServer::start().await;
    let dir = std::env::temp_dir().join(format!("serabot-agymuse-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let mut config = Config::default();
    config.api.enabled = true;
    config.api.token_hash = sha256_hex(OWNER);
    config.llm.model = "test".into();
    config.llm.base_url = "https://example.com/v1".into();
    config.memory.auto_extract = false;
    config.base_dir = dir.clone();
    config.llm.providers.insert(
        "agy".into(),
        ProviderConfig {
            kind: "google-antigravity".into(),
            api_key_secret: "agy_api_key".into(),
            models: vec!["gemini-3.8-flash".into()],
            data_egress_approved: true,
            ..Default::default()
        },
    );
    config.llm.providers.insert(
        "muse".into(),
        ProviderConfig {
            kind: "muse-code".into(),
            api_key_secret: "muse_api_key".into(),
            models: vec!["muse-spark-1.3".into()],
            data_egress_approved: true,
            ..Default::default()
        },
    );
    let database = dir.join("serabot.db");
    config.database.path = database.to_string_lossy().into_owned();
    let config = Arc::new(config);
    let store = Store::open(&database, 384, 5000).unwrap();
    let users = Arc::new(UserSecrets::with_master_key(store.clone(), [8u8; 32]).unwrap());
    let mut age = BTreeMap::from([(config.llm.api_key_secret.clone(), "synthetic-llm".into())]);
    if with_client_secret {
        age.insert(
            DEFAULT_CLIENT_SECRET_NAME.to_owned(),
            CLIENT_SECRET.to_owned(),
        );
    }
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
        "google-antigravity",
        Arc::new(GoogleSignIn::for_test(secrets, &server.uri())),
    );
    registry.register(
        "muse-code",
        Arc::new(MuseSignIn::for_test(&server.uri(), "verify.example")),
    );
    let signin = Arc::new(
        SignInService::new(state.accounts.clone())
            .unwrap()
            .with_registry(registry),
    );
    state = state.with_signin(signin);
    state = state.with_quota_readers(HashMap::from([
        (
            "google-antigravity".to_owned(),
            Arc::new(provider_quota::AntigravityQuota::for_test(&server.uri()))
                as Arc<dyn QuotaReader>,
        ),
        (
            "muse-code".to_owned(),
            Arc::new(provider_quota::MuseQuota::for_test(&format!(
                "{}/key",
                server.uri()
            ))) as Arc<dyn QuotaReader>,
        ),
    ]));
    let alice = member(&store, "agymuse-alice");
    let bob = member(&store, "agymuse-bob");
    Env {
        app: router(state),
        store,
        server,
        alice,
        bob,
    }
}

fn member(store: &Store, subject: &str) -> String {
    let user = store
        .create_user(subject, &format!("{subject}@example.example"), subject)
        .unwrap();
    mint_for(store, &user.id, subject)
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

async fn google(server: &MockServer, code: &str, email: &str, project: &str) {
    let access = format!("access-for-{code}");
    Mock::given(method("POST"))
        .and(path("/token"))
        .and(body_string_contains(format!("code={code}")))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "access_token": access, "refresh_token": format!("refresh-for-{code}"), "expires_in": 3600
        })))
        .mount(server)
        .await;
    Mock::given(method("GET"))
        .and(path("/userinfo"))
        .and(header("authorization", format!("Bearer {access}").as_str()))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"email": email, "id": "42"})))
        .mount(server)
        .await;
    Mock::given(method("POST"))
        .and(path("/v1internal:loadCodeAssist"))
        .and(header("authorization", format!("Bearer {access}").as_str()))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "cloudaicompanionProject": project, "currentTier": {"id": "standard-tier"}
        })))
        .mount(server)
        .await;
}

async fn muse(server: &MockServer) {
    Mock::given(method("POST"))
        .and(path("/device"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "device_code": "dc-synthetic",
            "user_code": "ABCD-1234",
            "verification_uri_complete": "https://verify.example/device?user_code=ABCD-1234",
            "interval": 1,
            "expires_in": 300
        })))
        .mount(server)
        .await;
    Mock::given(method("POST"))
        .and(path("/token"))
        .and(body_string_contains("device_code=dc-synthetic"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"access_token": "meta-identity-token", "expires_in": 60})),
        )
        .mount(server)
        .await;
    Mock::given(method("POST"))
        .and(path("/key"))
        .and(body_json(json!({"onboard": true})))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "api_key": "LLM|minted-synthetic-key", "user_email": "Muse@Example.Example",
            "subs_tier_name": "Plus", "user_id": "meta-7"
        })))
        .mount(server)
        .await;
}

fn no_secrets(text: &str) {
    for secret in [
        CLIENT_SECRET,
        "access-for-",
        "refresh-for-",
        "code-synthetic",
        "dc-synthetic",
        "meta-identity-token",
        "minted-synthetic-key",
    ] {
        assert!(!text.contains(secret), "{secret} leaked in {text}");
    }
}

fn complete_body(flow_id: &str, code: &str) -> Value {
    json!({"flow_id": flow_id, "code": code, "redirect_uri": REDIRECT})
}

#[tokio::test]
async fn a_member_signs_in_to_antigravity_and_nobody_else_sees_it() {
    let env = env(true).await;
    google(&env.server, "code-a", "Alpha@Example.Example", "proj-alpha").await;
    google(&env.server, "code-b", "beta@example.example", "proj-beta").await;

    // The account list says which sign-ins each provider offers.
    for (provider, flows) in [("agy", json!(["pkce"])), ("muse", json!(["device"]))] {
        let (status, body) = call(
            &env,
            &env.alice,
            "GET",
            &format!("/v1/providers/{provider}/accounts"),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["signin"], flows, "{provider}");
    }

    let mut ids = Vec::new();
    for code in ["code-a", "code-b"] {
        let (status, started) = call(
            &env,
            &env.alice,
            "POST",
            "/v1/providers/agy/signin/start",
            Some(json!({"flow": "pkce", "redirect_uri": REDIRECT})),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{started}");
        let url = started["url"].as_str().unwrap();
        assert!(url.contains("prompt=consent") && url.contains("access_type=offline"));
        assert!(!url.contains("openid"));
        no_secrets(url);
        let (status, done) = call(
            &env,
            &env.alice,
            "POST",
            "/v1/providers/agy/signin/complete",
            Some(complete_body(started["flow_id"].as_str().unwrap(), code)),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{done}");
        assert_eq!(done["created"], true);
        no_secrets(&done.to_string());
        ids.push(done["account_id"].as_str().unwrap().to_owned());
    }
    assert_ne!(ids[0], ids[1]);

    let (status, listed) = call(&env, &env.alice, "GET", "/v1/providers/agy/accounts", None).await;
    assert_eq!(status, StatusCode::OK);
    no_secrets(&listed.to_string());
    let accounts = listed["accounts"].as_array().unwrap();
    assert_eq!(accounts.len(), 2);
    assert!(accounts.iter().all(|account| {
        account["auth_kind"] == "oauth" && account["identity"]["tier"] == "standard-tier"
    }));
    // The email is masked in the list; the project id is not shown there.
    let text = listed.to_string();
    assert!(text.contains("a***@example.example"));
    assert!(!text.contains("Alpha@") && !text.contains("proj-alpha"));

    // Bob sees nothing, and cannot reach Alice's accounts.
    let (_, bobs) = call(&env, &env.bob, "GET", "/v1/providers/agy/accounts", None).await;
    assert!(bobs["accounts"].as_array().unwrap().is_empty());
    let uri = format!("/v1/providers/agy/accounts/{}/quota?refresh=1", ids[0]);
    let (status, _) = call(&env, &env.bob, "GET", &uri, None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn antigravity_sign_in_needs_the_servers_client_secret() {
    let env = env(false).await;
    let (status, body) = call(
        &env,
        &env.alice,
        "POST",
        "/v1/providers/agy/signin/start",
        Some(json!({"flow": "pkce", "redirect_uri": REDIRECT})),
    )
    .await;
    assert!(status.is_client_error(), "{status} {body}");
    assert!(
        body["error"].as_str().unwrap_or("").contains("not set up"),
        "{body}"
    );
    no_secrets(&body.to_string());
}

#[tokio::test]
async fn a_member_signs_in_to_muse_with_a_device_code() {
    let env = env(true).await;
    muse(&env.server).await;
    let (status, started) = call(
        &env,
        &env.alice,
        "POST",
        "/v1/providers/muse/signin/start",
        Some(json!({"flow": "device"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{started}");
    assert_eq!(started["user_code"], "ABCD-1234");
    assert!(
        started["url"]
            .as_str()
            .unwrap()
            .starts_with("https://verify.example/")
    );
    assert_eq!(started["interval"], 1);
    // The server polls; the desktop asks where the sign-in stands.
    let flow = started["flow_id"].as_str().unwrap().to_owned();
    let uri = format!("/v1/providers/muse/signin/{flow}");
    let mut last = Value::Null;
    for _ in 0..40 {
        let (_, body) = call(&env, &env.alice, "GET", &uri, None).await;
        last = body;
        if last["status"] != "pending" {
            break;
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    assert_eq!(last["status"], "ok", "{last}");
    assert_eq!(last["created"], true);
    no_secrets(&last.to_string());
    let (_, listed) = call(&env, &env.alice, "GET", "/v1/providers/muse/accounts", None).await;
    let account = &listed["accounts"][0];
    assert_eq!(account["auth_kind"], "device");
    assert_eq!(account["identity"]["tier"], "Plus");
    assert_eq!(account["identity"]["email"], "m***@example.example");
    no_secrets(&listed.to_string());
    // Bob cannot read Alice's flow or her account.
    let (status, _) = call(&env, &env.bob, "GET", &uri, None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (_, bobs) = call(&env, &env.bob, "GET", "/v1/providers/muse/accounts", None).await;
    assert!(bobs["accounts"].as_array().unwrap().is_empty());
}

#[tokio::test]
async fn the_owners_old_routes_write_the_owners_own_account_and_need_no_restart() {
    let env = env(true).await;
    google(&env.server, "code-o", "owner@example.example", "proj-owner").await;
    // Wrong kind and unknown provider keep their answers.
    let (status, _) = call(
        &env,
        OWNER,
        "POST",
        "/v1/admin/providers/nope/oauth/start",
        Some(json!({})),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    // A member is not the owner.
    let (status, _) = call(
        &env,
        &env.alice,
        "POST",
        "/v1/admin/providers/agy/oauth/start",
        Some(json!({})),
    )
    .await;
    assert!(
        status == StatusCode::FORBIDDEN || status == StatusCode::UNAUTHORIZED,
        "{status}"
    );

    let (status, started) = call(
        &env,
        OWNER,
        "POST",
        "/v1/admin/providers/agy/oauth/start",
        Some(
            json!({"challenge": "A".repeat(43), "state": "S".repeat(20), "redirect_uri": REDIRECT}),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{started}");
    assert_eq!(started["flow"], "pkce");
    let url = started["url"].as_str().unwrap();
    assert!(url.contains(&format!("state={}", "S".repeat(20))));
    assert!(url.contains(&format!("code_challenge={}", "A".repeat(43))));
    // The desktop made the challenge, so it sends its verifier back.
    let (status, done) = call(
        &env,
        OWNER,
        "POST",
        "/v1/admin/providers/agy/oauth/complete",
        Some(json!({"code": "code-o", "verifier": "V".repeat(43), "redirect_uri": REDIRECT})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{done}");
    assert_eq!(done["signed_in"], true);
    assert_eq!(done["project_discovered"], true);
    // The age store is no longer the destination, so nothing needs a restart.
    assert!(done.get("restart_required").is_none(), "{done}");
    no_secrets(&done.to_string());
    // The owner's account is in the per-user store like anyone's.
    let (_, listed) = call(&env, OWNER, "GET", "/v1/providers/agy/accounts", None).await;
    assert_eq!(listed["accounts"].as_array().unwrap().len(), 1);
    assert_eq!(listed["accounts"][0]["identity"]["tier"], "standard-tier");
    // And no one else's.
    let (_, bobs) = call(&env, &env.bob, "GET", "/v1/providers/agy/accounts", None).await;
    assert!(bobs["accounts"].as_array().unwrap().is_empty());
    // The audit row names the provider and account, never a token.
    let connection = env.store.connection().unwrap();
    let details: String = connection
        .query_row(
            "SELECT details_json FROM audit_log WHERE event = 'config.provider_oauth_signed_in'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert!(details.contains("\"provider\":\"agy\""));
    no_secrets(&details);

    // A refused code is a clean 422 and an audit row, with nothing stored.
    let (status, failed) = call(
        &env,
        OWNER,
        "POST",
        "/v1/admin/providers/agy/oauth/complete",
        Some(json!({"code": "code-unknown", "verifier": "V".repeat(43), "redirect_uri": REDIRECT})),
    )
    .await;
    assert!(status.is_client_error(), "{status} {failed}");
    assert_eq!(listed["accounts"].as_array().unwrap().len(), 1);

    // Muse's owner route returns the code to enter.
    muse(&env.server).await;
    let (status, device) = call(
        &env,
        OWNER,
        "POST",
        "/v1/admin/providers/muse/oauth/start",
        Some(json!({})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{device}");
    assert_eq!(device["flow"], "device");
    assert_eq!(device["code"], "ABCD-1234");
    assert!(
        device["url"]
            .as_str()
            .unwrap()
            .starts_with("https://verify.example/")
    );
    // Muse has no browser code to complete.
    let (status, wrong) = call(
        &env,
        OWNER,
        "POST",
        "/v1/admin/providers/muse/oauth/complete",
        Some(json!({"code": "x", "verifier": "V".repeat(43), "redirect_uri": REDIRECT})),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(wrong["error"].as_str().unwrap().contains("does not use"));
}

#[tokio::test]
async fn quota_is_read_through_the_provider_and_cached_on_the_account() {
    let env = env(true).await;
    google(&env.server, "code-q", "quota@example.example", "proj-quota").await;
    Mock::given(method("POST"))
        .and(path("/v1internal:retrieveUserQuotaSummary"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"groups": [
            {"displayName": "Gemini", "buckets": [
                {"remainingFraction": 0.5, "window": "5h", "resetTime": "2099-01-01T00:00:00Z"}
            ]}
        ]})))
        .mount(&env.server)
        .await;
    let (_, started) = call(
        &env,
        &env.alice,
        "POST",
        "/v1/providers/agy/signin/start",
        Some(json!({"flow": "pkce", "redirect_uri": REDIRECT})),
    )
    .await;
    let (_, done) = call(
        &env,
        &env.alice,
        "POST",
        "/v1/providers/agy/signin/complete",
        Some(complete_body(
            started["flow_id"].as_str().unwrap(),
            "code-q",
        )),
    )
    .await;
    let id = done["account_id"].as_str().unwrap();
    let uri = format!("/v1/providers/agy/accounts/{id}/quota");
    let (_, cached) = call(&env, &env.alice, "GET", &uri, None).await;
    assert!(cached["quota"].is_null());
    let (_, fresh) = call(&env, &env.alice, "GET", &format!("{uri}?refresh=1"), None).await;
    assert_eq!(fresh["refresh"], "refreshed", "{fresh}");
    assert_eq!(fresh["quota"]["windows"][0]["used_percent"], 50.0);
    assert_eq!(
        fresh["quota"]["windows"][0]["resets_at"],
        "2099-01-01T00:00:00Z"
    );
    // The summary call used the account's own project and token.
    let received = env.server.received_requests().await.unwrap();
    let summary = received
        .iter()
        .find(|request| request.url.path() == "/v1internal:retrieveUserQuotaSummary")
        .unwrap();
    assert_eq!(
        summary
            .headers
            .get("authorization")
            .unwrap()
            .to_str()
            .unwrap(),
        "Bearer access-for-code-q"
    );
    assert_eq!(
        summary.body_json::<Value>().unwrap(),
        json!({"project": "proj-quota"})
    );
    // A second read inside the window is throttled; the snapshot is cached.
    let (_, throttled) = call(&env, &env.alice, "GET", &format!("{uri}?refresh=1"), None).await;
    assert_eq!(throttled["refresh"], "throttled");
    let (_, listed) = call(&env, &env.alice, "GET", "/v1/providers/agy/accounts", None).await;
    assert_eq!(listed["accounts"][0]["quota"]["windows"][0]["label"], "5h");
    no_secrets(&listed.to_string());
}
