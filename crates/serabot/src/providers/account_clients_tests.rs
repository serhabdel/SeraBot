//! Member accounts and the model clients built from them: Antigravity
//! projects, session rotation, thinking and resampling; Muse headers and
//! parking. Synthetic fixtures only.

use super::*;
use crate::accounts::{AccountStatus, AddAccount, AuthKind, identity_fingerprint};
use crate::domain::LlmMessage;
use serde_json::json;
use wiremock::matchers::{body_string_contains, header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const AGY: &str = "agy";
const MUSE: &str = "muse";

struct Env {
    credentials: UserProviderCredentials,
    accounts: Arc<Accounts>,
    store: Store,
    alice: String,
}

fn env() -> Env {
    let store = Store::open_in_memory(384).unwrap();
    let users = Arc::new(UserSecrets::with_master_key(store.clone(), [7u8; 32]).unwrap());
    let server_secrets = SharedSecrets::new(crate::secrets::SecretStore::from_map(
        std::collections::BTreeMap::from([(
            "agy_secret".to_owned(),
            "synthetic-secret".to_owned(),
        )]),
    ));
    let credentials = UserProviderCredentials::new(users, server_secrets, 10, 4_096);
    let accounts = credentials.refresher().accounts().clone();
    let alice = store
        .create_user("microsoft:agy-alice", "alice@example.example", "Alice")
        .unwrap()
        .id;
    Env {
        credentials,
        accounts,
        store,
        alice,
    }
}

fn agy_profile(server: &MockServer) -> ProviderConfig {
    ProviderConfig {
        kind: "google-antigravity".into(),
        base_url: server.uri(),
        models: vec!["gemini-3.8-flash".into()],
        data_egress_approved: true,
        oauth_client_secret_secret: "agy_secret".into(),
        project_id: "profile-project".into(),
        ..Default::default()
    }
}

fn add_google(env: &Env, email: &str, identity: serde_json::Value) -> String {
    env.accounts
        .add(
            &env.alice,
            AddAccount {
                provider: AGY.into(),
                label: email.into(),
                auth_kind: AuthKind::Oauth,
                fingerprint: identity_fingerprint("google-antigravity", email),
                identity,
                credential: ProviderCredential {
                    api_key: format!("access-{email}"),
                    refresh_token: format!("refresh-{email}"),
                    expires_at: Some(chrono::Utc::now().timestamp() + 3_600),
                    generation: None,
                },
                priority: 0,
            },
        )
        .unwrap()
        .account_id
}

fn ok_stream() -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_string(
        "data: {\"response\":{\"candidates\":[{\"content\":{\"parts\":[{\"text\":\"ok\"}]},\"finishReason\":\"STOP\"}]}}\n\n",
    )
}

async fn chat(client: &LlmClient) -> Result<crate::llm::LlmResponse> {
    client
        .clone()
        .with_model("gemini-3.8-flash")
        .complete(&[LlmMessage::user("hi")], &[], None, None)
        .await
}

fn chat_bodies(requests: &[wiremock::Request]) -> Vec<Value> {
    requests
        .iter()
        .filter(|request| request.url.path() == "/v1internal:streamGenerateContent")
        .map(|request| request.body_json().unwrap())
        .collect()
}

fn count(requests: &[wiremock::Request], rpc: &str) -> usize {
    requests
        .iter()
        .filter(|request| request.url.path() == format!("/v1internal:{rpc}"))
        .count()
}

#[tokio::test]
async fn a_members_account_uses_its_own_project_never_the_providers() {
    let env = env();
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1internal:streamGenerateContent"))
        .respond_with(ok_stream())
        .mount(&server)
        .await;
    let profile = agy_profile(&server);
    let a = add_google(
        &env,
        "a@example.example",
        json!({"project_id": "proj-alpha"}),
    );
    let b = add_google(
        &env,
        "b@example.example",
        json!({"project_id": "proj-beta"}),
    );
    for (account, project) in [(&a, "proj-alpha"), (&b, "proj-beta")] {
        let client = env
            .credentials
            .client_for_account(&env.alice, AGY, &profile, account)
            .unwrap();
        chat(&client).await.unwrap();
        let requests = server.received_requests().await.unwrap();
        let bodies = chat_bodies(&requests);
        assert_eq!(bodies.last().unwrap()["project"], project);
    }
    // The provider's own project is only for the operator's (`default`)
    // credential, never a member's account.
    assert!(
        chat_bodies(&server.received_requests().await.unwrap())
            .iter()
            .all(|body| body["project"] != "profile-project")
    );
    // The legacy credential keeps using the provider's project.
    let legacy = ProviderCredential {
        api_key: "legacy-token".into(),
        ..Default::default()
    };
    env.credentials
        .users
        .set(&env.alice, "legacy", &legacy.encode().unwrap())
        .unwrap();
    let client = env
        .credentials
        .client_for_account(&env.alice, "legacy", &profile, DEFAULT_ACCOUNT)
        .unwrap();
    chat(&client).await.unwrap();
    let bodies = chat_bodies(&server.received_requests().await.unwrap());
    assert_eq!(bodies.last().unwrap()["project"], "profile-project");
}

#[tokio::test]
async fn a_missing_project_is_found_lazily_and_remembered() {
    let env = env();
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1internal:streamGenerateContent"))
        .respond_with(ok_stream())
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/v1internal:loadCodeAssist"))
        .and(header(
            "authorization",
            "Bearer access-lazy@example.example",
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "cloudaicompanionProject": "proj-found",
            "currentTier": {"id": "standard-tier"}
        })))
        .mount(&server)
        .await;
    let profile = agy_profile(&server);
    let account = add_google(
        &env,
        "lazy@example.example",
        json!({"email": "lazy@example.example"}),
    );

    let client = env
        .credentials
        .client_for_account(&env.alice, AGY, &profile, &account)
        .unwrap();
    chat(&client).await.unwrap();
    let requests = server.received_requests().await.unwrap();
    assert_eq!(chat_bodies(&requests)[0]["project"], "proj-found");
    assert_eq!(count(&requests, "loadCodeAssist"), 1);
    let identity = env
        .accounts
        .get(&env.alice, AGY, &account)
        .unwrap()
        .unwrap()
        .identity;
    assert_eq!(identity["project_id"], "proj-found");
    assert_eq!(identity["tier"], "standard-tier");
    assert_eq!(identity["email"], "lazy@example.example");
    assert!(identity["project_checked_at"].as_i64().is_some());

    // The next client knows it: no second discovery.
    let client = env
        .credentials
        .client_for_account(&env.alice, AGY, &profile, &account)
        .unwrap();
    chat(&client).await.unwrap();
    let requests = server.received_requests().await.unwrap();
    assert_eq!(count(&requests, "loadCodeAssist"), 1);
    assert_eq!(chat_bodies(&requests)[1]["project"], "proj-found");
}

#[tokio::test]
async fn an_account_with_no_project_omits_it_and_does_not_retry_at_once() {
    let env = env();
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1internal:streamGenerateContent"))
        .respond_with(ok_stream())
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/v1internal:loadCodeAssist"))
        .respond_with(ResponseTemplate::new(500))
        .mount(&server)
        .await;
    let profile = agy_profile(&server);
    let account = add_google(&env, "none@example.example", json!({}));
    for _ in 0..2 {
        let client = env
            .credentials
            .client_for_account(&env.alice, AGY, &profile, &account)
            .unwrap();
        chat(&client).await.unwrap();
    }
    let requests = server.received_requests().await.unwrap();
    // Neither request carried `project`; discovery ran once, not per client.
    assert!(
        chat_bodies(&requests)
            .iter()
            .all(|body| body.get("project").is_none())
    );
    assert_eq!(count(&requests, "loadCodeAssist"), 1);
    let identity = env
        .accounts
        .get(&env.alice, AGY, &account)
        .unwrap()
        .unwrap()
        .identity;
    assert!(identity.get("project_id").is_none());
    assert!(identity["project_checked_at"].as_i64().is_some());
}

#[tokio::test]
async fn the_session_rotates_once_on_the_token_wall_and_is_remembered() {
    let env = env();
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1internal:streamGenerateContent"))
        .respond_with(ResponseTemplate::new(400).set_body_string(
            "{\"error\":{\"message\":\"Request exceeds the maximum number of tokens allowed\"}}",
        ))
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/v1internal:streamGenerateContent"))
        .respond_with(ok_stream())
        .mount(&server)
        .await;
    let profile = agy_profile(&server);
    let account = add_google(
        &env,
        "wall@example.example",
        json!({"project_id": "proj-wall"}),
    );
    let client = env
        .credentials
        .client_for_account(&env.alice, AGY, &profile, &account)
        .unwrap();
    crate::llm::with_conversation("conv-wall", chat(&client))
        .await
        .unwrap();
    let bodies = chat_bodies(&server.received_requests().await.unwrap());
    assert_eq!(bodies.len(), 2);
    assert_eq!(
        bodies[0]["request"]["sessionId"],
        json!(crate::cloud_code::session_id("proj-wall", "conv-wall", 0))
    );
    assert_eq!(
        bodies[1]["request"]["sessionId"],
        json!(crate::cloud_code::session_id("proj-wall", "conv-wall", 1))
    );
    // The generation lives in the account row, so it survives a restart.
    let row = env
        .accounts
        .get(&env.alice, AGY, &account)
        .unwrap()
        .unwrap();
    assert_eq!(row.session_generation, 1);
    let next = env
        .credentials
        .client_for_account(&env.alice, AGY, &profile, &account)
        .unwrap();
    crate::llm::with_conversation("conv-wall", chat(&next))
        .await
        .unwrap();
    let bodies = chat_bodies(&server.received_requests().await.unwrap());
    assert_eq!(
        bodies[2]["request"]["sessionId"],
        bodies[1]["request"]["sessionId"]
    );
    // Rotating from a generation a sibling already moved past changes nothing.
    assert_eq!(
        env.store
            .bump_session_generation(&env.alice, AGY, &account, 0)
            .unwrap(),
        1
    );
    assert_eq!(
        env.store
            .bump_session_generation(&env.alice, AGY, &account, 1)
            .unwrap(),
        2
    );
}

#[tokio::test]
async fn a_malformed_tool_call_is_resampled_up_to_three_times() {
    let env = env();
    let profile_server = MockServer::start().await;
    let malformed = || {
        ResponseTemplate::new(200).set_body_string(
            "data: {\"response\":{\"candidates\":[{\"finishReason\":\"MALFORMED_FUNCTION_CALL\"}]}}\n\n",
        )
    };
    Mock::given(method("POST"))
        .and(path("/v1internal:streamGenerateContent"))
        .respond_with(malformed())
        .up_to_n_times(2)
        .mount(&profile_server)
        .await;
    Mock::given(method("POST"))
        .and(path("/v1internal:streamGenerateContent"))
        .respond_with(ok_stream())
        .mount(&profile_server)
        .await;
    let profile = agy_profile(&profile_server);
    let account = add_google(&env, "m@example.example", json!({"project_id": "proj-m"}));
    let client = env
        .credentials
        .client_for_account(&env.alice, AGY, &profile, &account)
        .unwrap();
    assert_eq!(
        chat(&client).await.unwrap().message.content.as_deref(),
        Some("ok")
    );
    assert_eq!(
        chat_bodies(&profile_server.received_requests().await.unwrap()).len(),
        3
    );

    // Three malformed answers in a row is an error, after exactly three tries.
    let stubborn = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1internal:streamGenerateContent"))
        .respond_with(malformed())
        .mount(&stubborn)
        .await;
    let profile = agy_profile(&stubborn);
    let client = env
        .credentials
        .client_for_account(&env.alice, AGY, &profile, &account)
        .unwrap();
    let error = chat(&client).await.unwrap_err().to_string();
    assert!(error.contains("MALFORMED_FUNCTION_CALL"), "{error}");
    assert_eq!(
        chat_bodies(&stubborn.received_requests().await.unwrap()).len(),
        3
    );
}

#[tokio::test]
async fn the_bots_thinking_level_reaches_the_envelope_and_signatures_round_trip() {
    let env = env();
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1internal:streamGenerateContent"))
        .respond_with(ResponseTemplate::new(200).set_body_string(
            "data: {\"response\":{\"candidates\":[{\"content\":{\"parts\":[{\"thought\":true,\"text\":\"pondering\"},{\"thoughtSignature\":\"c2lnLXN5bnRoZXRpYw==\",\"functionCall\":{\"id\":\"call_1\",\"name\":\"rag_search\",\"args\":{\"query\":\"q\"}}}]},\"finishReason\":\"STOP\"}]}}\n\n",
        ))
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/v1internal:streamGenerateContent"))
        .respond_with(ok_stream())
        .mount(&server)
        .await;
    let profile = agy_profile(&server);
    let account = add_google(&env, "t@example.example", json!({"project_id": "proj-t"}));
    let client = env
        .credentials
        .client_for_account(&env.alice, AGY, &profile, &account)
        .unwrap()
        .with_model("gemini-3.8-flash");
    let level = json!({"thinking_level": "high"});
    let extra = level.as_object().unwrap();
    let tools = [crate::gateway::ToolSpec {
        name: "rag_search".into(),
        description: "search".into(),
        parameters: json!({"type": "object", "properties": {"query": {"type": "string"}}}),
        external_effect: false,
    }];
    let first = client
        .complete_with(&[LlmMessage::user("go")], &tools, None, None, Some(extra))
        .await
        .unwrap();
    // The thinking text is reasoning (display data), not the answer; the call
    // carries Google's signature.
    assert_eq!(first.reasoning.as_deref(), Some("pondering"));
    assert!(first.message.content.is_none());
    let call = &first.message.tool_calls[0];
    assert_eq!(call.name, "rag_search");
    assert_eq!(
        call.reasoning_signature.as_deref(),
        Some("c2lnLXN5bnRoZXRpYw==")
    );
    // The signature goes back with the call on the next request.
    let history = [
        LlmMessage::user("go"),
        LlmMessage::assistant(None, first.message.tool_calls.clone()),
        LlmMessage {
            role: crate::domain::Role::Tool,
            content: Some("result".into()),
            tool_call_id: Some(call.id.clone()),
            name: Some(call.name.clone()),
            ..Default::default()
        },
    ];
    client
        .complete_with(&history, &tools, None, None, Some(extra))
        .await
        .unwrap();
    let bodies = chat_bodies(&server.received_requests().await.unwrap());
    assert_eq!(
        bodies[0]["request"]["generationConfig"]["thinkingConfig"],
        json!({"thinkingLevel": "high", "includeThoughts": true})
    );
    assert_eq!(
        bodies[1]["request"]["contents"][1]["parts"][0]["thoughtSignature"],
        "c2lnLXN5bnRoZXRpYw=="
    );
}

fn muse_profile(server: &MockServer) -> ProviderConfig {
    ProviderConfig {
        kind: "muse-code".into(),
        base_url: format!("{}/v1", server.uri()),
        models: vec!["muse-spark-1.3".into()],
        data_egress_approved: true,
        ..Default::default()
    }
}

fn add_muse(env: &Env, email: &str, meta_token: &str) -> String {
    env.accounts
        .add(
            &env.alice,
            AddAccount {
                provider: MUSE.into(),
                label: email.into(),
                auth_kind: AuthKind::Device,
                fingerprint: identity_fingerprint("muse-code", email),
                identity: json!({"email": email}),
                credential: ProviderCredential {
                    api_key: format!("LLM|key-{email}"),
                    refresh_token: meta_token.into(),
                    expires_at: None,
                    generation: None,
                },
                priority: 0,
            },
        )
        .unwrap()
        .account_id
}

#[tokio::test]
async fn every_muse_call_carries_the_api_version() {
    let env = env();
    let server = MockServer::start().await;
    // Muse chats on the Responses transport.
    Mock::given(method("POST"))
        .and(path("/v1/responses"))
        .and(header("x-api-version", "1.0.0"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(
            "event: response.output_text.delta
data: {\"type\":\"response.output_text.delta\",\"delta\":\"hi\"}

event: response.completed
data: {\"type\":\"response.completed\",\"response\":{}}

",
            "text/event-stream",
        ))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/v1/models"))
        .and(header("x-api-version", "1.0.0"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "object": "list", "data": [{"id": "muse-spark-1.3", "object": "model"}]
        })))
        .expect(1)
        .mount(&server)
        .await;
    let profile = muse_profile(&server);
    let account = add_muse(&env, "m@example.example", "meta-token");
    let client = env
        .credentials
        .client_for_account(&env.alice, MUSE, &profile, &account)
        .unwrap();
    let reply = client
        .clone()
        .with_model("muse-spark-1.3")
        .complete(&[LlmMessage::user("hi")], &[], None, None)
        .await
        .unwrap();
    assert_eq!(reply.message.content.as_deref(), Some("hi"));
    assert!(
        client
            .list_models()
            .await
            .unwrap()
            .contains("muse-spark-1.3")
    );
    // Providers that are not Muse never get the header.
    let other = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/models"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "object": "list", "data": [{"id": "m", "object": "model"}]
        })))
        .mount(&other)
        .await;
    let plain = ProviderConfig {
        base_url: other.uri(),
        models: vec!["m".into()],
        data_egress_approved: true,
        ..Default::default()
    };
    let key = env
        .accounts
        .add_api_key(&env.alice, "plain", "Key", "sk-plain")
        .unwrap();
    env.credentials
        .client_for_account(&env.alice, "plain", &plain, &key.account_id)
        .unwrap()
        .list_models()
        .await
        .unwrap();
    for request in other.received_requests().await.unwrap() {
        assert!(request.headers.get("x-api-version").is_none());
    }
}

#[tokio::test]
async fn a_rejected_muse_key_with_no_meta_token_parks_the_account() {
    let env = env();
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/responses"))
        .respond_with(ResponseTemplate::new(401))
        .mount(&server)
        .await;
    let profile = muse_profile(&server);
    let account = add_muse(&env, "old@example.example", "");
    let client = env
        .credentials
        .client_for_account(&env.alice, MUSE, &profile, &account)
        .unwrap();
    let error = client
        .with_model("muse-spark-1.3")
        .complete(&[LlmMessage::user("hi")], &[], None, None)
        .await
        .unwrap_err();
    assert!(matches!(error, SeraError::Upstream { status: 401, .. }));
    let row = env
        .accounts
        .get(&env.alice, MUSE, &account)
        .unwrap()
        .unwrap();
    assert_eq!(row.status, AccountStatus::NeedsReauth);
    // Signing in again would make it usable; another member's row is untouched.
    let bob = env
        .store
        .create_user("microsoft:agy-bob", "bob@example.example", "Bob")
        .unwrap()
        .id;
    assert!(env.accounts.list(&bob, MUSE).unwrap().is_empty());
}

#[tokio::test]
async fn muse_remint_sends_an_empty_body_and_never_retries() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/muse-code/key"))
        .and(header("authorization", "Bearer meta-token"))
        .and(header("x-api-version", "1.0.0"))
        .and(wiremock::matchers::body_json(json!({})))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"api_key": "re-minted"})))
        .expect(1)
        .mount(&server)
        .await;
    let http = reqwest::Client::new();
    let url = format!("{}/muse-code/key", server.uri());
    assert_eq!(
        muse_mint_api_key(&http, &url, "meta-token").await.unwrap(),
        "re-minted"
    );
    // A 5xx on a re-mint is not waited out: the caller decides.
    let failing = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(503))
        .mount(&failing)
        .await;
    let started = std::time::Instant::now();
    let error = muse_mint_api_key(&http, &format!("{}/muse-code/key", failing.uri()), "t")
        .await
        .unwrap_err();
    assert!(matches!(error, SeraError::Upstream { status: 503, .. }));
    assert!(started.elapsed() < Duration::from_secs(2));
    assert_eq!(failing.received_requests().await.unwrap().len(), 1);
    // The sign-in mint onboards and retries; the body says so.
    let onboarding = MockServer::start().await;
    Mock::given(method("POST"))
        .and(body_string_contains("\"onboard\":true"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "api_key": "first-key", "user_id": "u1"
        })))
        .expect(1)
        .mount(&onboarding)
        .await;
    let mint = muse_mint(
        &http,
        &format!("{}/muse-code/key", onboarding.uri()),
        "meta-token",
        true,
        &[],
    )
    .await
    .unwrap();
    assert_eq!(mint.api_key, "first-key");
    assert_eq!(mint.user_id.as_deref(), Some("u1"));
    assert!(!format!("{mint:?}").contains("first-key"));
}

fn age_fixture(refresh: &str) -> (Config, SharedSecrets, Store, Arc<UserSecrets>, String) {
    let mut config = Config::default();
    config.llm.api_key_secret = "llm_api_key".into();
    config.llm.providers.insert(
        "antigravity".into(),
        ProviderConfig {
            api_key_secret: "antigravity_access_token".into(),
            models: vec!["gemini-3-pro".into()],
            data_egress_approved: true,
            kind: "google-antigravity".into(),
            auth: "oauth".into(),
            refresh_token_secret: "antigravity_refresh_token".into(),
            project_id: "operator-project".into(),
            ..ProviderConfig::default()
        },
    );
    let secrets = SharedSecrets::new(crate::secrets::SecretStore::from_map(
        std::collections::BTreeMap::from([
            ("llm_api_key".to_owned(), "sk-operator".to_owned()),
            (
                "antigravity_access_token".to_owned(),
                "access-one".to_owned(),
            ),
            ("antigravity_refresh_token".to_owned(), refresh.to_owned()),
        ]),
    ));
    let store = Store::open_in_memory(384).unwrap();
    let admin = store.admin_user_id().unwrap();
    let users = Arc::new(UserSecrets::with_master_key(store.clone(), [3u8; 32]).unwrap());
    (config, secrets, store, users, admin)
}

#[test]
fn a_new_age_store_sign_in_becomes_another_admin_account() {
    let (config, secrets, store, users, admin) = age_fixture("refresh-one");
    let accounts = Accounts::new(store.clone(), users.clone());
    // The once-only import copies the first sign-in as the default row.
    assert_eq!(
        import_age_provider_credentials(&config, &secrets, &users, &store, &admin).unwrap(),
        2
    );
    // Seeing it again records it and adds nothing.
    assert_eq!(
        import_age_oauth_sign_ins(&config, &secrets, &accounts, &store, &admin).unwrap(),
        0
    );
    assert_eq!(
        import_age_oauth_sign_ins(&config, &secrets, &accounts, &store, &admin).unwrap(),
        0
    );
    assert_eq!(accounts.list(&admin, "antigravity").unwrap().len(), 1);

    // `serabot providers login` stored a different refresh token: it is added.
    let secrets = SharedSecrets::new(crate::secrets::SecretStore::from_map(
        std::collections::BTreeMap::from([
            ("llm_api_key".to_owned(), "sk-operator".to_owned()),
            (
                "antigravity_access_token".to_owned(),
                "access-two".to_owned(),
            ),
            (
                "antigravity_refresh_token".to_owned(),
                "refresh-two".to_owned(),
            ),
        ]),
    ));
    assert_eq!(
        import_age_oauth_sign_ins(&config, &secrets, &accounts, &store, &admin).unwrap(),
        1
    );
    let listed = accounts.list(&admin, "antigravity").unwrap();
    assert_eq!(listed.len(), 2);
    let added = listed
        .iter()
        .find(|account| account.label == "Imported sign-in")
        .unwrap();
    // The operator's project travels with it; nothing was overwritten.
    assert_eq!(added.identity["project_id"], "operator-project");
    let credential = accounts
        .credential(&admin, "antigravity", &added.account_id)
        .unwrap()
        .unwrap();
    assert_eq!(credential.refresh_token, "refresh-two");
    assert_eq!(
        accounts
            .credential(&admin, "antigravity", DEFAULT_ACCOUNT)
            .unwrap()
            .unwrap()
            .refresh_token,
        "refresh-one"
    );
    // And only once.
    assert_eq!(
        import_age_oauth_sign_ins(&config, &secrets, &accounts, &store, &admin).unwrap(),
        0
    );
    // A member never gets the operator's sign-in.
    let member = store
        .create_user("microsoft:age-member", "m@example.example", "M")
        .unwrap()
        .id;
    assert!(accounts.list(&member, "antigravity").unwrap().is_empty());
}
