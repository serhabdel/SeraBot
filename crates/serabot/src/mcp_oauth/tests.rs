//! Tests run against a wiremock authorization server on loopback. The
//! synthetic tokens below are fixtures, not credentials.

use wiremock::matchers::{body_string_contains, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use super::*;

const MCP_PATH: &str = "/mcp";

fn spec() -> OauthSpec {
    OauthSpec {
        scopes: vec!["read".into(), "write".into()],
        ..OauthSpec::default()
    }
}

/// A server that is both the MCP resource and its authorization server.
async fn authorization_server(extra_meta: Value) -> MockServer {
    let server = MockServer::start().await;
    let base = server.uri();
    Mock::given(method("GET"))
        .and(path("/.well-known/oauth-protected-resource/mcp"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "resource": format!("{base}{MCP_PATH}"),
            "authorization_servers": [base],
        })))
        .mount(&server)
        .await;
    let mut meta = json!({
        "issuer": base,
        "authorization_endpoint": format!("{base}/authorize"),
        "token_endpoint": format!("{base}/token"),
        "revocation_endpoint": format!("{base}/revoke"),
        "code_challenge_methods_supported": ["S256"],
    });
    for (key, value) in extra_meta.as_object().unwrap() {
        meta[key] = value.clone();
    }
    Mock::given(method("GET"))
        .and(path("/.well-known/oauth-authorization-server"))
        .respond_with(ResponseTemplate::new(200).set_body_json(meta))
        .mount(&server)
        .await;
    server
}

fn mcp_url(server: &MockServer) -> String {
    format!("{}{MCP_PATH}", server.uri())
}

async fn discovered(server: &MockServer) -> Discovery {
    discover(&OauthNet::loopback_for_tests(), &mcp_url(server), None)
        .await
        .expect("discovery")
}

fn record_for(server: &MockServer) -> OauthRecord {
    OauthRecord {
        access_token: "synthetic-access-old".into(),
        refresh_token: Some("synthetic-refresh-old".into()),
        expires_at: Some(Utc::now().timestamp() - 5),
        scope: Some("read".into()),
        client: ClientRegistration {
            client_id: "synthetic-client".into(),
            client_secret: None,
            source: ClientSource::Dcr,
        },
        token_endpoint: format!("{}/token", server.uri()),
        revocation_endpoint: Some(format!("{}/revoke", server.uri())),
        resource: mcp_url(server),
    }
}

struct Fixture {
    accounts: OauthAccounts,
    store: Store,
    user: String,
}

fn fixture() -> Fixture {
    let store = Store::open_in_memory(384).unwrap();
    let secrets = Arc::new(UserSecrets::with_master_key(store.clone(), [9u8; 32]).unwrap());
    let user = store
        .create_user("microsoft:oauth-test", "oauth@example.example", "O")
        .unwrap()
        .id;
    Fixture {
        accounts: OauthAccounts::new(store.clone(), secrets, OauthNet::loopback_for_tests()),
        store,
        user,
    }
}

fn state_of(fixture: &Fixture, account: &str) -> (String, Option<String>) {
    let row = fixture
        .store
        .marketplace_accounts(&fixture.user, "canva")
        .unwrap()
        .into_iter()
        .find(|row| row.account_id == account)
        .expect("account row");
    (row.state, row.last_error)
}

fn signed_in(fixture: &Fixture, server: &MockServer, account: &str) {
    fixture
        .store
        .marketplace_account_upsert(&fixture.user, "canva", account, "Work")
        .unwrap();
    fixture
        .accounts
        .save_signed_in(&fixture.user, "canva", account, &record_for(server))
        .unwrap();
}

#[tokio::test]
async fn discovery_follows_rfc_9728_then_8414() {
    let server = authorization_server(json!({})).await;
    let found = discovered(&server).await;
    assert_eq!(found.resource, mcp_url(&server));
    assert_eq!(found.token_endpoint, format!("{}/token", server.uri()));
    assert!(found.registration_endpoint.is_none());
    assert!(!found.cimd_supported);
}

#[tokio::test]
async fn discovery_falls_back_to_openid_configuration() {
    let server = MockServer::start().await;
    let base = server.uri();
    Mock::given(method("GET"))
        .and(path("/.well-known/oauth-protected-resource/mcp"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "resource": format!("{base}/mcp"),
            "authorization_servers": [base],
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/.well-known/openid-configuration"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "issuer": base,
            "authorization_endpoint": format!("{base}/a"),
            "token_endpoint": format!("{base}/t"),
        })))
        .mount(&server)
        .await;
    let found = discovered(&server).await;
    assert_eq!(found.authorization_endpoint, format!("{base}/a"));
}

#[tokio::test]
async fn discovery_refuses_metadata_for_another_resource_or_issuer() {
    let server = MockServer::start().await;
    let base = server.uri();
    Mock::given(method("GET"))
        .and(path("/.well-known/oauth-protected-resource/mcp"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "resource": "https://elsewhere.example.invalid/mcp",
            "authorization_servers": [base],
        })))
        .mount(&server)
        .await;
    assert!(
        discover(&OauthNet::loopback_for_tests(), &mcp_url(&server), None)
            .await
            .is_err()
    );

    let server = MockServer::start().await;
    let base = server.uri();
    Mock::given(method("GET"))
        .and(path("/.well-known/oauth-protected-resource/mcp"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "resource": format!("{base}/mcp"),
            "authorization_servers": [base],
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/.well-known/oauth-authorization-server"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "issuer": "https://other.example.invalid",
            "authorization_endpoint": format!("{base}/a"),
            "token_endpoint": format!("{base}/t"),
        })))
        .mount(&server)
        .await;
    assert!(
        discover(&OauthNet::loopback_for_tests(), &mcp_url(&server), None)
            .await
            .is_err()
    );
}

#[tokio::test]
async fn discovery_requires_pkce_s256_when_methods_are_listed() {
    let server = authorization_server(json!({"code_challenge_methods_supported": ["plain"]})).await;
    assert!(
        discover(&OauthNet::loopback_for_tests(), &mcp_url(&server), None)
            .await
            .is_err()
    );
}

#[tokio::test]
async fn an_oversized_metadata_document_is_refused() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(vec![b'a'; 70 * 1024]))
        .mount(&server)
        .await;
    assert!(
        discover(&OauthNet::loopback_for_tests(), &mcp_url(&server), None)
            .await
            .is_err()
    );
}

#[tokio::test]
async fn client_identity_prefers_cimd_then_dcr_then_configured() {
    let net = OauthNet::loopback_for_tests();
    // CIMD: supported and asked for.
    let server = authorization_server(json!({
        "client_id_metadata_document_supported": true,
    }))
    .await;
    let mut found = discovered(&server).await;
    let mut want = spec();
    want.uses_cimd = true;
    let client = register(&net, &found, &want).await.unwrap();
    assert_eq!(client.source, ClientSource::Cimd);
    assert_eq!(client.client_id, cimd_client_id());

    // DCR: no CIMD, a registration endpoint that answers.
    Mock::given(method("POST"))
        .and(path("/register"))
        .and(body_string_contains("SeraBot"))
        .respond_with(ResponseTemplate::new(201).set_body_json(json!({
            "client_id": "registered-1", "client_secret": "synthetic-client-secret"
        })))
        .expect(1)
        .mount(&server)
        .await;
    found.registration_endpoint = Some(format!("{}/register", server.uri()));
    want.uses_cimd = false;
    let client = register(&net, &found, &want).await.unwrap();
    assert_eq!(client.source, ClientSource::Dcr);
    assert_eq!(client.client_id, "registered-1");
    assert_eq!(
        client.client_secret.as_deref(),
        Some("synthetic-client-secret")
    );
    assert!(!format!("{client:?}").contains("synthetic-client-secret"));

    // Configured: registration fails, a client id is configured.
    found.registration_endpoint = Some(format!("{}/nowhere", server.uri()));
    want.client_id = Some("configured-1".into());
    let client = register(&net, &found, &want).await.unwrap();
    assert_eq!(client.source, ClientSource::Configured);

    // None at all.
    want.client_id = None;
    assert!(register(&net, &found, &want).await.is_err());
}

#[tokio::test]
async fn the_authorize_url_carries_pkce_s256_and_the_resource() {
    let server = authorization_server(json!({})).await;
    let found = discovered(&server).await;
    let client = ClientRegistration {
        client_id: "synthetic-client".into(),
        client_secret: None,
        source: ClientSource::Configured,
    };
    let request = authorize_request(&found, &client, &["read".into()]).unwrap();
    let url = Url::parse(&request.url).unwrap();
    let query: std::collections::HashMap<_, _> = url.query_pairs().into_owned().collect();
    assert_eq!(query["response_type"], "code");
    assert_eq!(query["code_challenge_method"], "S256");
    assert_eq!(query["resource"], mcp_url(&server));
    assert_eq!(query["redirect_uri"], oauth_callback_uri());
    assert_eq!(query["state"], request.state);
    assert_eq!(query["scope"], "read");
    assert!(pkce_token_ok(&request.verifier));
    assert!(!request.url.contains(&request.verifier));
}

#[tokio::test]
async fn the_code_exchange_sends_the_verifier_and_resource() {
    let server = authorization_server(json!({})).await;
    Mock::given(method("POST"))
        .and(path("/token"))
        .and(body_string_contains("grant_type=authorization_code"))
        .and(body_string_contains("code_verifier=v-synthetic"))
        .and(body_string_contains("resource="))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "access_token": "synthetic-access-1",
            "refresh_token": "synthetic-refresh-1",
            "token_type": "Bearer",
            "expires_in": 3600,
            "scope": "read write",
        })))
        .expect(1)
        .mount(&server)
        .await;
    let found = discovered(&server).await;
    let client = ClientRegistration {
        client_id: "synthetic-client".into(),
        client_secret: None,
        source: ClientSource::Configured,
    };
    let verifier = "v-synthetic".to_owned() + &"x".repeat(40);
    let record = exchange_code(
        &OauthNet::loopback_for_tests(),
        &found,
        &client,
        "code-1",
        &verifier,
        &["read".into()],
    )
    .await
    .unwrap();
    assert_eq!(record.access_token, "synthetic-access-1");
    assert_eq!(record.refresh_token.as_deref(), Some("synthetic-refresh-1"));
    assert_eq!(record.scope.as_deref(), Some("read write"));
    assert!(record.expires_at.unwrap() > Utc::now().timestamp() + 3000);
    let debug = format!("{record:?}");
    assert!(!debug.contains("synthetic-access-1") && !debug.contains("synthetic-refresh-1"));
}

#[tokio::test]
async fn a_good_refresh_stores_the_rotated_tokens_and_marks_ready() {
    let server = authorization_server(json!({})).await;
    Mock::given(method("POST"))
        .and(path("/token"))
        .and(body_string_contains("grant_type=refresh_token"))
        .and(body_string_contains("refresh_token=synthetic-refresh-old"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "access_token": "synthetic-access-new",
            "refresh_token": "synthetic-refresh-new",
            "expires_in": 600,
        })))
        .mount(&server)
        .await;
    let fixture = fixture();
    signed_in(&fixture, &server, "a1");
    let outcome = fixture
        .accounts
        .refresh_account(&fixture.user, "canva", "a1")
        .await
        .unwrap();
    assert_eq!(outcome, RefreshOutcome::Ready);
    let record = fixture
        .accounts
        .load(&fixture.user, "canva", "a1")
        .unwrap()
        .unwrap();
    assert_eq!(record.access_token, "synthetic-access-new");
    assert_eq!(
        record.refresh_token.as_deref(),
        Some("synthetic-refresh-new")
    );
    assert_eq!(state_of(&fixture, "a1").0, "ready");
}

#[tokio::test]
async fn invalid_grant_drops_tokens_but_keeps_the_registration() {
    let server = authorization_server(json!({})).await;
    Mock::given(method("POST"))
        .and(path("/token"))
        .respond_with(ResponseTemplate::new(400).set_body_json(json!({"error": "invalid_grant"})))
        .mount(&server)
        .await;
    let fixture = fixture();
    signed_in(&fixture, &server, "a1");
    let outcome = fixture
        .accounts
        .refresh_account(&fixture.user, "canva", "a1")
        .await
        .unwrap();
    assert_eq!(outcome, RefreshOutcome::NeedsSignIn);
    let record = fixture
        .accounts
        .load(&fixture.user, "canva", "a1")
        .unwrap()
        .unwrap();
    assert!(record.access_token.is_empty() && record.refresh_token.is_none());
    assert_eq!(record.client.client_id, "synthetic-client");
    assert_eq!(state_of(&fixture, "a1").0, "needs_sign_in");
}

#[tokio::test]
async fn invalid_client_drops_tokens_and_the_registration() {
    let server = authorization_server(json!({})).await;
    Mock::given(method("POST"))
        .and(path("/token"))
        .respond_with(ResponseTemplate::new(401).set_body_json(json!({"error": "invalid_client"})))
        .mount(&server)
        .await;
    let fixture = fixture();
    signed_in(&fixture, &server, "a1");
    let outcome = fixture
        .accounts
        .refresh_account(&fixture.user, "canva", "a1")
        .await
        .unwrap();
    assert_eq!(outcome, RefreshOutcome::ClientDropped);
    assert!(
        fixture
            .accounts
            .load(&fixture.user, "canva", "a1")
            .unwrap()
            .is_none()
    );
    assert_eq!(state_of(&fixture, "a1").0, "needs_sign_in");
}

#[tokio::test]
async fn a_server_error_or_dead_network_keeps_everything() {
    let server = authorization_server(json!({})).await;
    Mock::given(method("POST"))
        .and(path("/token"))
        .respond_with(ResponseTemplate::new(503).set_body_string("down"))
        .mount(&server)
        .await;
    let fixture = fixture();
    signed_in(&fixture, &server, "a1");
    let outcome = fixture
        .accounts
        .refresh_account(&fixture.user, "canva", "a1")
        .await
        .unwrap();
    assert_eq!(outcome, RefreshOutcome::Unreachable);
    let kept = fixture
        .accounts
        .load(&fixture.user, "canva", "a1")
        .unwrap()
        .unwrap();
    assert_eq!(kept, record_for(&server));
    assert_eq!(state_of(&fixture, "a1").0, "unreachable");

    // Nothing listens on port 1: a network failure behaves the same.
    let mut dead = record_for(&server);
    dead.token_endpoint = "http://127.0.0.1:1/token".into();
    fixture
        .accounts
        .save_signed_in(&fixture.user, "canva", "a1", &dead)
        .unwrap();
    let outcome = fixture
        .accounts
        .refresh_account(&fixture.user, "canva", "a1")
        .await
        .unwrap();
    assert_eq!(outcome, RefreshOutcome::Unreachable);
    assert_eq!(
        fixture
            .accounts
            .load(&fixture.user, "canva", "a1")
            .unwrap()
            .unwrap(),
        dead
    );
}

#[tokio::test]
async fn accounts_are_scoped_to_their_user() {
    let server = authorization_server(json!({})).await;
    let fixture = fixture();
    signed_in(&fixture, &server, "a1");
    let other = fixture
        .store
        .create_user("microsoft:oauth-other", "other@example.example", "P")
        .unwrap()
        .id;
    assert!(
        fixture
            .accounts
            .load(&other, "canva", "a1")
            .unwrap()
            .is_none()
    );
    assert!(
        fixture
            .store
            .marketplace_accounts(&other, "canva")
            .unwrap()
            .is_empty()
    );
    assert!(
        fixture
            .accounts
            .refresh_account(&other, "canva", "a1")
            .await
            .is_err()
    );
}

#[tokio::test]
async fn forgetting_an_account_revokes_best_effort_and_deletes_locally() {
    let server = authorization_server(json!({})).await;
    Mock::given(method("POST"))
        .and(path("/revoke"))
        .and(body_string_contains("token=synthetic-refresh-old"))
        .and(body_string_contains("token_type_hint=refresh_token"))
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(&server)
        .await;
    let fixture = fixture();
    signed_in(&fixture, &server, "a1");
    assert!(
        fixture
            .accounts
            .forget_account(&fixture.user, "canva", "a1")
            .await
            .unwrap()
    );
    assert!(
        fixture
            .accounts
            .load(&fixture.user, "canva", "a1")
            .unwrap()
            .is_none()
    );
    assert!(
        fixture
            .store
            .marketplace_accounts(&fixture.user, "canva")
            .unwrap()
            .is_empty()
    );

    // A failing revocation endpoint does not stop the local delete.
    let failing = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(500))
        .mount(&failing)
        .await;
    signed_in(&fixture, &failing, "a2");
    assert!(
        !fixture
            .accounts
            .forget_account(&fixture.user, "canva", "a2")
            .await
            .unwrap()
    );
    assert!(
        fixture
            .accounts
            .load(&fixture.user, "canva", "a2")
            .unwrap()
            .is_none()
    );
}

fn lookup_to(address: &'static str) -> Lookup {
    Arc::new(move |_host, port| {
        Box::pin(async move { Ok(vec![SocketAddr::new(address.parse().unwrap(), port)]) })
    })
}

#[tokio::test]
async fn a_host_that_resolves_to_a_private_address_is_refused() {
    for address in ["10.0.0.5", "127.0.0.1", "169.254.169.254", "::1"] {
        let net = OauthNet::with_lookup(lookup_to(address));
        let error = discover(&net, "https://mcp.example.invalid/mcp", None)
            .await
            .unwrap_err();
        assert!(
            matches!(error, SeraError::Forbidden(_)),
            "{address}: {error}"
        );
    }
}

#[tokio::test]
async fn plain_http_and_literal_private_hosts_are_refused_in_production() {
    let server = authorization_server(json!({})).await;
    // Production rules: no http, even to a server that would answer.
    let error = discover(&OauthNet::system(), &mcp_url(&server), None)
        .await
        .unwrap_err();
    assert!(matches!(error, SeraError::Forbidden(_)), "{error}");
    assert!(server.received_requests().await.unwrap().is_empty());
    for url in [
        "https://127.0.0.1/mcp",
        "https://10.1.2.3/mcp",
        "https://[::1]/mcp",
        "https://user:pw@mcp.example.invalid/mcp",
    ] {
        assert!(
            discover(&OauthNet::system(), url, None).await.is_err(),
            "{url}"
        );
    }
}

#[tokio::test]
async fn an_advertised_endpoint_on_a_private_address_fails_discovery() {
    let server = authorization_server(json!({"token_endpoint": "https://10.0.0.1/token"})).await;
    let error = discover(&OauthNet::loopback_for_tests(), &mcp_url(&server), None)
        .await
        .unwrap_err();
    assert!(matches!(error, SeraError::Forbidden(_)), "{error}");

    let server =
        authorization_server(json!({"registration_endpoint": "https://192.168.1.9/r"})).await;
    assert!(
        discover(&OauthNet::loopback_for_tests(), &mcp_url(&server), None)
            .await
            .is_err()
    );
}

#[tokio::test]
async fn a_redirect_from_the_token_endpoint_is_not_followed() {
    let server = authorization_server(json!({})).await;
    let target = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/token"))
        .respond_with(
            ResponseTemplate::new(307).insert_header("location", format!("{}/steal", target.uri())),
        )
        .mount(&server)
        .await;
    let fixture = fixture();
    signed_in(&fixture, &server, "a1");
    let outcome = fixture
        .accounts
        .refresh_account(&fixture.user, "canva", "a1")
        .await
        .unwrap();
    assert_eq!(outcome, RefreshOutcome::Unreachable);
    assert!(target.received_requests().await.unwrap().is_empty());
}

#[test]
fn marketplace_oauth_ids_cannot_be_reached_through_the_public_credential_api() {
    let store = Store::open_in_memory(384).unwrap();
    let secrets = UserSecrets::with_master_key(store.clone(), [9u8; 32]).unwrap();
    let user = store
        .create_user("microsoft:oauth-api", "api@example.example", "A")
        .unwrap()
        .id;
    assert!(
        secrets
            .set(&user, "marketplace.canva.a1.oauth", "x")
            .is_err()
    );
    secrets
        .set_marketplace_oauth(&user, "canva", "a1", "{}")
        .unwrap();
    assert!(
        !secrets
            .providers_for(&user)
            .unwrap()
            .iter()
            .any(|n| n.starts_with("marketplace."))
    );
    assert!(
        secrets
            .set_marketplace_oauth(&user, "bad.slug", "a1", "{}")
            .is_err()
    );
    assert!(
        secrets
            .set_marketplace_oauth(&user, "canva", "a.1", "{}")
            .is_err()
    );
}
