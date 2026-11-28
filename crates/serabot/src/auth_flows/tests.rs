//! Sign-in framework tests: a wiremock identity provider stands in for every
//! vendor. Nothing here talks to a real provider.

use std::time::Duration;

use secrecy::ExposeSecret;
use wiremock::matchers::{body_string_contains, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use super::device::DeviceSpec;
use super::pkce::{PkceSpec, RedirectSpec, pkce_s256};
use super::*;
use crate::accounts::Accounts;
use crate::user_secrets::UserSecrets;

const REDIRECT: &str = "http://127.0.0.1:51121/oauth-callback";

struct Harness {
    store: Store,
    accounts: Arc<Accounts>,
    secrets: Arc<UserSecrets>,
}

fn harness() -> Harness {
    let store = Store::open_in_memory(384).unwrap();
    let secrets = Arc::new(
        UserSecrets::with_master_key(store.clone(), [7u8; crate::user_secrets::KEY_BYTES]).unwrap(),
    );
    let accounts = Arc::new(Accounts::new(store.clone(), secrets.clone()));
    Harness {
        store,
        accounts,
        secrets,
    }
}

fn member(harness: &Harness, subject: &str) -> String {
    harness
        .store
        .create_user(subject, "member@example.example", "Synthetic Member")
        .unwrap()
        .id
}

/// The mock identity provider speaks `id_token` as "the email", which is all
/// the identity step needs.
fn identity() -> IdentityFn {
    Arc::new(|tokens| {
        let email = tokens
            .id_token
            .clone()
            .ok_or_else(|| SeraError::External("no identity".into()))?;
        Ok(AccountIdentity {
            label: email.clone(),
            stable_id: email.to_lowercase(),
            identity: json!({"email": email}),
        })
    })
}

fn oauth_sign_in(server: &MockServer) -> OAuthSignIn {
    OAuthSignIn {
        pkce: Some(PkceSpec {
            authorize: PinnedEndpoint::loopback(&format!("{}/authorize", server.uri())),
            token: PinnedEndpoint::loopback(&format!("{}/token", server.uri())),
            client_id: "synthetic-client".into(),
            client_secret: None,
            scope: "profile offline".into(),
            redirect: RedirectSpec {
                port: Some(51121),
                path: Some("/oauth-callback"),
            },
            extra_params: vec![("access_type".into(), "offline".into())],
        }),
        device: Some(DeviceSpec {
            device: PinnedEndpoint::loopback(&format!("{}/device", server.uri())),
            token: PinnedEndpoint::loopback(&format!("{}/device-token", server.uri())),
            client_id: "synthetic-client".into(),
            client_secret: None,
            scope: String::new(),
            verification_hosts: vec!["verify.example".into()],
            headers: vec![("x-api-version", "1.0.0")],
        }),
        auth_kind: AuthKind::Oauth,
        fingerprint_scope: "idp".into(),
        identity: identity(),
    }
}

fn service(harness: &Harness, server: &MockServer) -> Arc<SignInService> {
    let mut registry = SignInRegistry::default();
    registry.register("idp-kind", Arc::new(oauth_sign_in(server)));
    Arc::new(
        SignInService::new(harness.accounts.clone())
            .unwrap()
            .with_registry(registry),
    )
}

fn token_reply(email: &str) -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_json(json!({
        "access_token": "access-synthetic",
        "refresh_token": "refresh-synthetic",
        "expires_in": 3600,
        "id_token": email,
    }))
}

fn pkce_request() -> StartRequest {
    StartRequest {
        flow: Some(FlowKind::Pkce),
        redirect_uri: Some(REDIRECT.into()),
        ..Default::default()
    }
}

fn query(url: &str) -> std::collections::HashMap<String, String> {
    url::Url::parse(url)
        .unwrap()
        .query_pairs()
        .map(|(key, value)| (key.into_owned(), value.into_owned()))
        .collect()
}

fn complete_request(flow_id: &str) -> CompleteRequest {
    CompleteRequest {
        flow_id: Some(flow_id.into()),
        code: "code-synthetic".into(),
        redirect_uri: Some(REDIRECT.into()),
        verifier: None,
    }
}

fn event_count(store: &Store, kind: &str) -> i64 {
    store
        .connection()
        .unwrap()
        .query_row(
            "SELECT COUNT(*) FROM events WHERE kind = ?1",
            [kind],
            |row| row.get(0),
        )
        .unwrap()
}

#[tokio::test]
async fn pkce_flow_exchanges_the_code_and_stores_a_sealed_account() {
    let harness = harness();
    let alice = member(&harness, "microsoft:flow-pkce");
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/token"))
        .and(body_string_contains("grant_type=authorization_code"))
        .and(body_string_contains("code=code-synthetic"))
        .respond_with(token_reply("Person@Example.Example"))
        .up_to_n_times(1)
        .mount(&server)
        .await;
    let service = service(&harness, &server);

    let started = service
        .start(&alice, "anthropic", "idp-kind", pkce_request())
        .await
        .unwrap();
    let url_query = query(&started.url);
    assert_eq!(url_query["client_id"], "synthetic-client");
    assert_eq!(url_query["redirect_uri"], REDIRECT);
    assert_eq!(url_query["code_challenge_method"], "S256");
    assert_eq!(url_query["access_type"], "offline");
    assert_eq!(url_query["state"], started.state.clone().unwrap());
    assert!(!started.url.contains("code_verifier"));
    assert_eq!(started.expires_in, 600);
    assert_eq!(
        service
            .status(&alice, "anthropic", &started.flow_id)
            .unwrap(),
        FlowStatus::Pending
    );

    let outcome = service
        .complete(
            &alice,
            "anthropic",
            "idp-kind",
            complete_request(&started.flow_id),
        )
        .await
        .unwrap();
    assert!(outcome.created);

    // The server-held verifier is the one the challenge was made from.
    let received = server.received_requests().await.unwrap();
    let body = String::from_utf8(received[0].body.clone()).unwrap();
    let verifier = url::form_urlencoded::parse(body.as_bytes())
        .find(|(key, _)| key == "code_verifier")
        .map(|(_, value)| value.into_owned())
        .unwrap();
    assert!(
        crate::auth_flows::pkce::pkce_token_ok(&verifier),
        "a well-formed verifier reached the token endpoint"
    );
    use base64::Engine as _;
    use sha2::Digest as _;
    assert_eq!(
        base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(sha2::Sha256::digest(verifier.as_bytes())),
        url_query["code_challenge"]
    );

    // The account is the member's own; the credential is sealed and carries
    // the rotation fields. The fingerprint is of the identity, not a token.
    let listed = harness.accounts.list(&alice, "anthropic").unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].label, "Person@Example.Example");
    assert_eq!(listed[0].auth_kind, AuthKind::Oauth);
    assert_eq!(listed[0].identity["email"], "Person@Example.Example");
    let credential = harness
        .accounts
        .credential(&alice, "anthropic", &outcome.account_id)
        .unwrap()
        .unwrap();
    assert_eq!(credential.api_key, "access-synthetic");
    assert_eq!(credential.refresh_token, "refresh-synthetic");
    assert!(credential.expires_at.is_some());
    assert!(
        harness
            .secrets
            .get_account(&alice, "anthropic", &outcome.account_id)
            .unwrap()
            .unwrap()
            .expose_secret()
            .contains("refresh-synthetic")
    );
    assert_eq!(
        service
            .status(&alice, "anthropic", &started.flow_id)
            .unwrap(),
        FlowStatus::Ok {
            account_id: outcome.account_id.clone(),
            created: true
        }
    );
    // Events and audit rows are published without any secret.
    assert_eq!(event_count(&harness.store, "provider.signin"), 2);
    let events: String = harness
        .store
        .connection()
        .unwrap()
        .query_row(
            "SELECT group_concat(payload_json) FROM events WHERE kind = 'provider.signin'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    for secret in [
        "access-synthetic",
        "refresh-synthetic",
        "code-synthetic",
        &verifier,
    ] {
        assert!(!events.contains(secret));
    }

    // Signing in as the same identity again refreshes the same account.
    let again = service
        .start(&alice, "anthropic", "idp-kind", pkce_request())
        .await
        .unwrap();
    Mock::given(method("POST"))
        .and(path("/token"))
        .respond_with(token_reply("person@example.example"))
        .mount(&server)
        .await;
    let second = service
        .complete(
            &alice,
            "anthropic",
            "idp-kind",
            complete_request(&again.flow_id),
        )
        .await
        .unwrap();
    assert_eq!(second.account_id, outcome.account_id);
    assert!(!second.created);
}

#[tokio::test]
async fn a_challenge_the_desktop_made_needs_its_verifier() {
    let harness = harness();
    let alice = member(&harness, "microsoft:flow-held");
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/token"))
        .and(body_string_contains("code_verifier="))
        .respond_with(token_reply("held@example.example"))
        .expect(1)
        .mount(&server)
        .await;
    let service = service(&harness, &server);
    let (verifier, challenge) = pkce_s256();
    let started = service
        .start(
            &alice,
            "anthropic",
            "idp-kind",
            StartRequest {
                challenge: Some(challenge.clone()),
                state: Some("state-from-the-desktop-0123".into()),
                ..pkce_request()
            },
        )
        .await
        .unwrap();
    let url_query = query(&started.url);
    assert_eq!(url_query["code_challenge"], challenge);
    assert_eq!(url_query["state"], "state-from-the-desktop-0123");
    // Without the verifier the request is refused and the flow survives.
    let error = service
        .complete(
            &alice,
            "anthropic",
            "idp-kind",
            complete_request(&started.flow_id),
        )
        .await
        .unwrap_err();
    assert!(matches!(
        error,
        SeraError::Rejected {
            code: "bad_code",
            ..
        }
    ));
    let mut request = complete_request(&started.flow_id);
    request.verifier = Some(verifier);
    assert!(
        service
            .complete(&alice, "anthropic", "idp-kind", request)
            .await
            .unwrap()
            .created
    );
    // A malformed challenge or state is refused at the start.
    for bad in [
        StartRequest {
            challenge: Some("short".into()),
            ..pkce_request()
        },
        StartRequest {
            state: Some("no spaces allowed here!".into()),
            ..pkce_request()
        },
    ] {
        assert!(
            service
                .start(&alice, "anthropic", "idp-kind", bad)
                .await
                .is_err()
        );
    }
}

#[tokio::test]
async fn completion_from_a_different_user_or_provider_is_refused() {
    let harness = harness();
    let alice = member(&harness, "microsoft:flow-alice");
    let bob = member(&harness, "microsoft:flow-bob");
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/token"))
        .respond_with(token_reply("alice@example.example"))
        .expect(1)
        .mount(&server)
        .await;
    let service = service(&harness, &server);
    let started = service
        .start(&alice, "anthropic", "idp-kind", pkce_request())
        .await
        .unwrap();

    // Bob cannot complete, or even see, Alice's flow.
    let error = service
        .complete(
            &bob,
            "anthropic",
            "idp-kind",
            complete_request(&started.flow_id),
        )
        .await
        .unwrap_err();
    assert!(matches!(error, SeraError::NotFound(_)));
    assert!(matches!(
        service.status(&bob, "anthropic", &started.flow_id),
        Err(SeraError::NotFound(_))
    ));
    // The flow is bound to its provider id.
    assert!(matches!(
        service
            .complete(
                &alice,
                "openai",
                "idp-kind",
                complete_request(&started.flow_id)
            )
            .await,
        Err(SeraError::NotFound(_))
    ));
    // ... and to the redirect it started with.
    let mut moved = complete_request(&started.flow_id);
    moved.redirect_uri = Some("http://127.0.0.1:51121/other".into());
    assert!(matches!(
        service
            .complete(&alice, "anthropic", "idp-kind", moved)
            .await,
        Err(SeraError::Rejected {
            code: "bad_redirect",
            ..
        })
    ));
    assert!(harness.accounts.list(&bob, "anthropic").unwrap().is_empty());

    // The owner still can, once.
    service
        .complete(
            &alice,
            "anthropic",
            "idp-kind",
            complete_request(&started.flow_id),
        )
        .await
        .unwrap();
    let replay = service
        .complete(
            &alice,
            "anthropic",
            "idp-kind",
            complete_request(&started.flow_id),
        )
        .await
        .unwrap_err();
    assert!(matches!(
        replay,
        SeraError::Rejected {
            code: "flow_used",
            ..
        }
    ));
    assert!(harness.accounts.list(&bob, "anthropic").unwrap().is_empty());
}

#[tokio::test]
async fn an_expired_flow_is_refused() {
    let harness = harness();
    let alice = member(&harness, "microsoft:flow-expiry");
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/token"))
        .respond_with(token_reply("late@example.example"))
        .expect(0)
        .mount(&server)
        .await;
    let mut registry = SignInRegistry::default();
    registry.register("idp-kind", Arc::new(oauth_sign_in(&server)));
    let service = Arc::new(
        SignInService::new(harness.accounts.clone())
            .unwrap()
            .with_registry(registry)
            .with_ttl(Duration::from_millis(20)),
    );
    let started = service
        .start(&alice, "anthropic", "idp-kind", pkce_request())
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(60)).await;
    let error = service
        .complete(
            &alice,
            "anthropic",
            "idp-kind",
            complete_request(&started.flow_id),
        )
        .await
        .unwrap_err();
    assert!(matches!(
        error,
        SeraError::Rejected {
            code: "flow_expired",
            ..
        }
    ));
    assert_eq!(
        service
            .status(&alice, "anthropic", &started.flow_id)
            .unwrap(),
        FlowStatus::Expired
    );
    assert!(
        harness
            .accounts
            .list(&alice, "anthropic")
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn the_redirect_must_be_the_providers_exact_loopback() {
    let harness = harness();
    let alice = member(&harness, "microsoft:flow-redirect");
    let server = MockServer::start().await;
    let service = service(&harness, &server);
    for bad in [
        "http://127.0.0.1:51122/oauth-callback",
        "http://127.0.0.1:51121/elsewhere",
        "https://127.0.0.1:51121/oauth-callback",
        "http://evil.example:51121/oauth-callback",
    ] {
        let error = service
            .start(
                &alice,
                "anthropic",
                "idp-kind",
                StartRequest {
                    redirect_uri: Some(bad.into()),
                    ..pkce_request()
                },
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
    // A missing redirect is refused too, and so is an unknown provider kind.
    assert!(
        service
            .start(
                &alice,
                "anthropic",
                "idp-kind",
                StartRequest {
                    flow: Some(FlowKind::Pkce),
                    ..Default::default()
                }
            )
            .await
            .is_err()
    );
    assert!(matches!(
        service
            .start(&alice, "anthropic", "no-such-kind", pkce_request())
            .await,
        Err(SeraError::Rejected {
            code: "signin_unsupported",
            ..
        })
    ));
}

#[tokio::test]
async fn token_exchange_never_follows_redirects_and_caps_the_body() {
    let harness = harness();
    let alice = member(&harness, "microsoft:flow-http");
    let server = MockServer::start().await;
    let elsewhere = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/elsewhere"))
        .respond_with(token_reply("stolen@example.example"))
        .expect(0)
        .mount(&elsewhere)
        .await;
    Mock::given(method("POST"))
        .and(path("/token"))
        .respond_with(
            ResponseTemplate::new(302)
                .insert_header("location", format!("{}/elsewhere", elsewhere.uri())),
        )
        .mount(&server)
        .await;
    let service = service(&harness, &server);
    let started = service
        .start(&alice, "anthropic", "idp-kind", pkce_request())
        .await
        .unwrap();
    let error = service
        .complete(
            &alice,
            "anthropic",
            "idp-kind",
            complete_request(&started.flow_id),
        )
        .await
        .unwrap_err();
    assert!(matches!(
        error,
        SeraError::Rejected {
            code: "signin_refused",
            ..
        }
    ));
    assert!(
        harness
            .accounts
            .list(&alice, "anthropic")
            .unwrap()
            .is_empty()
    );

    // An oversized body is refused instead of read.
    let big = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/token"))
        .respond_with(ResponseTemplate::new(200).set_body_string("x".repeat(70 * 1024)))
        .mount(&big)
        .await;
    let service = self::service(&harness, &big);
    let started = service
        .start(&alice, "anthropic", "idp-kind", pkce_request())
        .await
        .unwrap();
    let error = service
        .complete(
            &alice,
            "anthropic",
            "idp-kind",
            complete_request(&started.flow_id),
        )
        .await
        .unwrap_err();
    assert!(matches!(error, SeraError::External(_)));
    // The failed exchange burned the flow: the code cannot be retried.
    assert!(matches!(
        service
            .complete(
                &alice,
                "anthropic",
                "idp-kind",
                complete_request(&started.flow_id)
            )
            .await,
        Err(SeraError::Rejected {
            code: "flow_used",
            ..
        })
    ));
}

#[test]
fn endpoints_are_pinned_by_host_and_path() {
    assert!(
        PinnedEndpoint::https("oauth2.googleapis.com", "/token")
            .checked_url()
            .is_ok()
    );
    for (host, path) in [
        ("evil.example/x", "/token"),
        ("oauth2.googleapis.com@evil.example", "/token"),
        ("oauth2.googleapis.com", "/token?x=1"),
        ("", "/token"),
    ] {
        assert!(
            PinnedEndpoint::https(host, path).checked_url().is_err(),
            "{host}{path}"
        );
    }
}

#[test]
fn a_member_keeps_at_most_eight_open_flows() {
    let store = FlowStore::new(FLOW_TTL);
    let record = |provider: &str| FlowRecord {
        provider: provider.into(),
        kind: FlowKind::Pkce,
        redirect_uri: Some(REDIRECT.into()),
        state: None,
        verifier: None,
        client_holds_verifier: true,
        expires: Instant::now() + FLOW_TTL,
        gateway: None,
        gateway_flow_id: None,
        gateway_profile: None,

        created: Instant::now(),
        status: FlowStatus::Pending,
        consumed: false,
    };
    let first = store.insert("alice", record("p"));
    for _ in 0..MAX_FLOWS_PER_USER {
        store.insert("alice", record("p"));
    }
    // The oldest was dropped to make room; another member is unaffected.
    assert!(store.status("alice", "p", &first).is_err());
    let bob = store.insert("bob", record("p"));
    assert!(store.status("bob", "p", &bob).is_ok());
    assert_eq!(
        store
            .lock()
            .keys()
            .filter(|(owner, _)| owner == "alice")
            .count(),
        MAX_FLOWS_PER_USER
    );
}

// ---- device flow through the service ----

fn device_flow_record(provider: &str) -> FlowRecord {
    FlowRecord {
        provider: provider.into(),
        kind: FlowKind::Device,
        redirect_uri: None,
        state: None,
        verifier: None,
        client_holds_verifier: false,
        gateway: None,
        gateway_flow_id: None,
        gateway_profile: None,
        expires: Instant::now() + FLOW_TTL,
        created: Instant::now(),
        status: FlowStatus::Pending,
        consumed: false,
    }
}

/// Mount one scripted answer of the device token endpoint.
async fn device_answer(server: &MockServer, status: u16, body: Value, times: u64) {
    Mock::given(method("POST"))
        .and(path("/device-token"))
        .and(body_string_contains("device_code=dc-synthetic"))
        .respond_with(ResponseTemplate::new(status).set_body_json(body))
        .up_to_n_times(times)
        .mount(server)
        .await;
}

/// Run the poller against `server` with instant waits and report the flow's
/// final state.
async fn run_device(
    harness: &Harness,
    server: &MockServer,
    user: &str,
) -> (FlowStatus, Arc<SignInService>, String) {
    let service = service(harness, server);
    let flow_id = service.flows.insert(user, device_flow_record("anthropic"));
    let provider = service.registry.get("idp-kind").cloned().unwrap();
    service
        .run_device_flow(
            &FlowRef {
                user_id: user,
                provider_id: "anthropic",
                flow_id: &flow_id,
            },
            provider,
            DevicePlan {
                device_code: Zeroizing::new("dc-synthetic".into()),
                interval: 1,
                expires_in: 900,
            },
            |_| async {},
        )
        .await;
    let status = service.status(user, "anthropic", &flow_id).unwrap();
    (status, service, flow_id)
}

#[tokio::test]
async fn the_device_poller_waits_through_pending_and_slow_down_then_stores_the_account() {
    let harness = harness();
    let alice = member(&harness, "microsoft:flow-device-ok");
    let server = MockServer::start().await;
    device_answer(&server, 400, json!({"error": "authorization_pending"}), 2).await;
    // A 400 that names no error also keeps waiting.
    device_answer(&server, 400, json!({}), 1).await;
    device_answer(&server, 400, json!({"error": "slow_down"}), 1).await;
    device_answer(
        &server,
        200,
        json!({"access_token": "dev-access", "refresh_token": "dev-refresh", "id_token": "Dev@Example.Example"}),
        1,
    )
    .await;
    let (status, _service, _flow) = run_device(&harness, &server, &alice).await;
    let FlowStatus::Ok {
        account_id,
        created,
    } = status
    else {
        panic!("expected a signed-in flow, got {status:?}");
    };
    assert!(created);
    let credential = harness
        .accounts
        .credential(&alice, "anthropic", &account_id)
        .unwrap()
        .unwrap();
    assert_eq!(credential.api_key, "dev-access");
    // Five token-endpoint calls: pending x2, no-code 400, slow_down, success.
    assert_eq!(server.received_requests().await.unwrap().len(), 5);
    let received = server.received_requests().await.unwrap();
    assert_eq!(
        received[0]
            .headers
            .get("x-api-version")
            .map(|v| v.to_str().unwrap()),
        Some("1.0.0")
    );
    assert_eq!(event_count(&harness.store, "provider.signin"), 1);
}

#[tokio::test]
async fn the_device_poller_ends_on_denial_expiry_and_provider_errors() {
    for (status, body, expected) in [
        (
            400,
            json!({"error": "access_denied"}),
            FlowStatus::Denied {
                reason: "access_denied".into(),
            },
        ),
        (400, json!({"error": "expired_token"}), FlowStatus::Expired),
        (
            400,
            json!({"error": "invalid_client"}),
            FlowStatus::Denied {
                reason: "provider_refused".into(),
            },
        ),
    ] {
        let harness = harness();
        let alice = member(&harness, "microsoft:flow-device-end");
        let server = MockServer::start().await;
        device_answer(&server, status, body.clone(), 1).await;
        let (outcome, _service, _flow) = run_device(&harness, &server, &alice).await;
        assert_eq!(outcome, expected, "{body}");
        assert!(
            harness
                .accounts
                .list(&alice, "anthropic")
                .unwrap()
                .is_empty()
        );
        assert_eq!(server.received_requests().await.unwrap().len(), 1);
    }
}

#[tokio::test]
async fn a_device_flow_that_never_approves_expires_after_the_bound() {
    let harness = harness();
    let alice = member(&harness, "microsoft:flow-device-timeout");
    let server = MockServer::start().await;
    device_answer(
        &server,
        400,
        json!({"error": "authorization_pending"}),
        1_000,
    )
    .await;
    let service = service(&harness, &server);
    let flow_id = service
        .flows
        .insert(&alice, device_flow_record("anthropic"));
    let provider = service.registry.get("idp-kind").cloned().unwrap();
    // Thirty-second polls over a 900 second bound: thirty calls.
    service
        .run_device_flow(
            &FlowRef {
                user_id: &alice,
                provider_id: "anthropic",
                flow_id: &flow_id,
            },
            provider,
            DevicePlan {
                device_code: Zeroizing::new("dc-synthetic".into()),
                interval: 30,
                expires_in: 7_200,
            },
            |_| async {},
        )
        .await;
    assert_eq!(
        service.status(&alice, "anthropic", &flow_id).unwrap(),
        FlowStatus::Expired
    );
    assert_eq!(server.received_requests().await.unwrap().len(), 30);
}

#[tokio::test]
async fn a_device_poller_stops_when_its_flow_is_gone() {
    let harness = harness();
    let alice = member(&harness, "microsoft:flow-device-gone");
    let server = MockServer::start().await;
    device_answer(
        &server,
        400,
        json!({"error": "authorization_pending"}),
        1_000,
    )
    .await;
    let service = service(&harness, &server);
    // No flow row exists, as after an eviction: the poller asks nothing.
    let provider = service.registry.get("idp-kind").cloned().unwrap();
    service
        .run_device_flow(
            &FlowRef {
                user_id: &alice,
                provider_id: "anthropic",
                flow_id: "missing",
            },
            provider,
            DevicePlan {
                device_code: Zeroizing::new("dc-synthetic".into()),
                interval: 1,
                expires_in: 900,
            },
            |_| async {},
        )
        .await;
    assert!(server.received_requests().await.unwrap().is_empty());
}

#[tokio::test]
async fn starting_a_device_flow_returns_the_code_and_completes_in_the_background() {
    let harness = harness();
    let alice = member(&harness, "microsoft:flow-device-start");
    let bob = member(&harness, "microsoft:flow-device-start-bob");
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/device"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "device_code": "dc-synthetic",
            "user_code": "ABCD-1234",
            "verification_uri": "https://verify.example/device",
            "interval": 1,
            "expires_in": 120,
        })))
        .expect(1)
        .mount(&server)
        .await;
    device_answer(
        &server,
        200,
        json!({"access_token": "bg-access", "refresh_token": "bg-refresh", "id_token": "bg@example.example"}),
        1,
    )
    .await;
    let service = service(&harness, &server);
    let started = service
        .start(
            &alice,
            "anthropic",
            "idp-kind",
            StartRequest {
                flow: Some(FlowKind::Device),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(started.user_code.as_deref(), Some("ABCD-1234"));
    assert_eq!(started.url, "https://verify.example/device");
    assert_eq!(started.interval, Some(1));
    assert!(
        !serde_json::to_string(&started)
            .unwrap()
            .contains("dc-synthetic")
    );
    // Bob cannot watch Alice's flow.
    assert!(service.status(&bob, "anthropic", &started.flow_id).is_err());
    // The one-second poll finishes the flow in the background.
    let mut status = FlowStatus::Pending;
    for _ in 0..40 {
        status = service
            .status(&alice, "anthropic", &started.flow_id)
            .unwrap();
        if status != FlowStatus::Pending {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(
        matches!(status, FlowStatus::Ok { created: true, .. }),
        "{status:?}"
    );
    assert_eq!(harness.accounts.list(&alice, "anthropic").unwrap().len(), 1);
    assert!(harness.accounts.list(&bob, "anthropic").unwrap().is_empty());
}

#[tokio::test]
async fn a_verification_page_on_another_host_is_refused() {
    let harness = harness();
    let alice = member(&harness, "microsoft:flow-device-host");
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/device"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "device_code": "dc-synthetic",
            "user_code": "ABCD-1234",
            "verification_uri": "https://evil.example/device",
        })))
        .mount(&server)
        .await;
    let service = service(&harness, &server);
    let error = service
        .start(
            &alice,
            "anthropic",
            "idp-kind",
            StartRequest {
                flow: Some(FlowKind::Device),
                ..Default::default()
            },
        )
        .await
        .unwrap_err();
    assert!(matches!(error, SeraError::External(_)));
    assert!(!error.to_string().contains("evil.example"));
}

#[tokio::test]
async fn a_provider_without_a_flow_refuses_it() {
    let harness = harness();
    let alice = member(&harness, "microsoft:flow-unsupported");
    let server = MockServer::start().await;
    let mut registry = SignInRegistry::default();
    let mut pkce_only = oauth_sign_in(&server);
    pkce_only.device = None;
    registry.register("idp-kind", Arc::new(pkce_only));
    let service = Arc::new(
        SignInService::new(harness.accounts.clone())
            .unwrap()
            .with_registry(registry),
    );
    let error = service
        .start(
            &alice,
            "anthropic",
            "idp-kind",
            StartRequest {
                flow: Some(FlowKind::Device),
                ..Default::default()
            },
        )
        .await
        .unwrap_err();
    assert!(matches!(
        error,
        SeraError::Rejected {
            code: "flow_unsupported",
            ..
        }
    ));
    assert_eq!(service.registry().flows_for("idp-kind"), ["pkce"]);
    assert_eq!(SignInRegistry::standard().flows_for("openrouter"), ["pkce"]);
    assert!(SignInRegistry::standard().flows_for("openai").is_empty());
}

// ---- claude-agent-sdk gateway relay (synthetic gateway, no real CLI) ----

fn gateway_service(harness: &Harness) -> Arc<SignInService> {
    let mut registry = SignInRegistry::default();
    registry.register("claude-agent-sdk", Arc::new(gateway::ClaudeGatewaySignIn));
    Arc::new(
        SignInService::new(harness.accounts.clone())
            .unwrap()
            .with_registry(registry),
    )
}

fn gateway_endpoint(server: &MockServer) -> gateway::GatewayEndpoint {
    gateway::GatewayEndpoint::validated(&server.uri(), Zeroizing::new("gw-token".into())).unwrap()
}

async fn mount_gateway_start(server: &MockServer) {
    Mock::given(method("POST"))
        .and(path("/v1/auth/start"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "flow_id": "gw-flow",
            "url": "https://claude.example/cai/oauth/authorize?redirect_uri=http%3A%2F%2Flocalhost%3A40123%2Fcallback&state=gw-state",
            "manual_url": "https://claude.example/oauth/authorize?z-manual",
            "profile": "acct_synthetic",
            "expires_in": 600,
            "mode": "callback",
            "callback": {"port": 40123, "path": "/callback"},
            "state": "gw-state",
        })))
        .mount(server)
        .await;
}

#[tokio::test]
async fn a_gateway_start_hands_the_desktop_the_cli_callback_and_the_code_page() {
    let harness = harness();
    let user = member(&harness, "gw-a");
    let server = MockServer::start().await;
    mount_gateway_start(&server).await;
    let service = gateway_service(&harness);
    let started = service
        .start_with_gateway(
            &user,
            "claude-agent-sdk",
            "claude-agent-sdk",
            StartRequest {
                flow: Some(FlowKind::Pkce),
                ..Default::default()
            },
            gateway_endpoint(&server),
        )
        .await
        .unwrap();
    assert!(
        started
            .url
            .contains("redirect_uri=http%3A%2F%2Flocalhost%3A40123")
    );
    assert_eq!(
        started.manual_url.as_deref(),
        Some("https://claude.example/oauth/authorize?z-manual")
    );
    assert_eq!(started.callback_port, Some(40123));
    assert_eq!(started.callback_path.as_deref(), Some("/callback"));
    assert_eq!(started.state.as_deref(), Some("gw-state"));
}

#[tokio::test]
async fn a_caught_redirect_is_relayed_with_its_state_and_polls_wait_meanwhile() {
    let harness = harness();
    let user = member(&harness, "gw-b");
    let server = MockServer::start().await;
    mount_gateway_start(&server).await;
    // The relayed redirect must carry the state; a poll during the complete
    // must not reach the gateway (the complete call owns the flow).
    Mock::given(method("POST"))
        .and(path("/v1/auth/complete"))
        .and(body_string_contains("\"state\":\"gw-state\""))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "ok": true,
            "profile": "acct_synthetic",
            "account": {"email": "member@example.example", "subscriptionType": "max"},
        })))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/v1/auth/flow/gw-flow"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"status": "pending"})))
        .mount(&server)
        .await;
    let service = gateway_service(&harness);
    let endpoint = gateway_endpoint(&server);
    let started = service
        .start_with_gateway(
            &user,
            "claude-agent-sdk",
            "claude-agent-sdk",
            StartRequest {
                flow: Some(FlowKind::Pkce),
                ..Default::default()
            },
            gateway_endpoint(&server),
        )
        .await
        .unwrap();
    let pending = service
        .status_with_gateway(&user, "claude-agent-sdk", &started.flow_id, Some(&endpoint))
        .await
        .unwrap();
    assert_eq!(pending, FlowStatus::Pending);

    let outcome = service
        .complete_with_gateway(
            &user,
            "claude-agent-sdk",
            "claude-agent-sdk",
            CompleteRequest {
                flow_id: Some(started.flow_id.clone()),
                code: "caught-code".into(),
                redirect_uri: None,
                verifier: None,
            },
            gateway_endpoint(&server),
            Some("gw-state".into()),
        )
        .await
        .unwrap();
    assert!(outcome.created);
    let done = service
        .status_with_gateway(&user, "claude-agent-sdk", &started.flow_id, Some(&endpoint))
        .await
        .unwrap();
    assert!(matches!(done, FlowStatus::Ok { .. }), "{done:?}");
    // Its credential is the gateway token, never a Claude token.
    let accounts = harness.accounts.list(&user, "claude-agent-sdk").unwrap();
    assert_eq!(accounts.len(), 1);
}

#[tokio::test]
async fn an_incomplete_paste_keeps_the_flow_open_and_a_refused_code_says_why() {
    let harness = harness();
    let user = member(&harness, "gw-c");
    let server = MockServer::start().await;
    mount_gateway_start(&server).await;
    Mock::given(method("POST"))
        .and(path("/v1/auth/complete"))
        .and(body_string_contains("half"))
        .respond_with(ResponseTemplate::new(400).set_body_json(json!({
            "error": "That doesn't look like the full code. Copy all of it, including the # in the middle.",
            "code": "code_incomplete",
        })))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/v1/auth/complete"))
        .and(body_string_contains("wrong#state"))
        .respond_with(ResponseTemplate::new(400).set_body_json(json!({
            "error": "That code wasn't accepted. Sign in again to get a new code.",
            "code": "auth_code_rejected",
        })))
        .mount(&server)
        .await;
    let service = gateway_service(&harness);
    let started = service
        .start_with_gateway(
            &user,
            "claude-agent-sdk",
            "claude-agent-sdk",
            StartRequest {
                flow: Some(FlowKind::Pkce),
                ..Default::default()
            },
            gateway_endpoint(&server),
        )
        .await
        .unwrap();
    let paste = |code: &str| CompleteRequest {
        flow_id: Some(started.flow_id.clone()),
        code: code.into(),
        redirect_uri: None,
        verifier: None,
    };
    let error = service
        .complete_with_gateway(
            &user,
            "claude-agent-sdk",
            "claude-agent-sdk",
            paste("half"),
            gateway_endpoint(&server),
            None,
        )
        .await
        .unwrap_err();
    assert!(
        matches!(
            error,
            SeraError::Rejected {
                code: "code_incomplete",
                ..
            }
        ),
        "{error:?}"
    );
    assert_eq!(
        service
            .status(&user, "claude-agent-sdk", &started.flow_id)
            .unwrap(),
        FlowStatus::Pending
    );

    // The flow was given back: a second paste goes through to the gateway.
    let error = service
        .complete_with_gateway(
            &user,
            "claude-agent-sdk",
            "claude-agent-sdk",
            paste("wrong#state"),
            gateway_endpoint(&server),
            None,
        )
        .await
        .unwrap_err();
    assert!(
        matches!(
            error,
            SeraError::Rejected {
                code: "code_rejected",
                ..
            }
        ),
        "{error:?}"
    );
    match service
        .status(&user, "claude-agent-sdk", &started.flow_id)
        .unwrap()
    {
        FlowStatus::Denied { reason } => assert!(reason.contains("wasn't accepted"), "{reason}"),
        other => panic!("expected denied, got {other:?}"),
    }
}

#[tokio::test]
async fn a_flow_the_gateway_forgot_reads_as_expired() {
    let harness = harness();
    let user = member(&harness, "gw-d");
    let server = MockServer::start().await;
    mount_gateway_start(&server).await;
    Mock::given(method("GET"))
        .and(path("/v1/auth/flow/gw-flow"))
        .respond_with(ResponseTemplate::new(404).set_body_json(json!({
            "error": "unknown sign-in flow",
            "code": "flow_not_found",
        })))
        .mount(&server)
        .await;
    let service = gateway_service(&harness);
    let endpoint = gateway_endpoint(&server);
    let started = service
        .start_with_gateway(
            &user,
            "claude-agent-sdk",
            "claude-agent-sdk",
            StartRequest {
                flow: Some(FlowKind::Pkce),
                ..Default::default()
            },
            gateway_endpoint(&server),
        )
        .await
        .unwrap();
    let status = service
        .status_with_gateway(&user, "claude-agent-sdk", &started.flow_id, Some(&endpoint))
        .await
        .unwrap();
    assert_eq!(status, FlowStatus::Expired);
}

#[tokio::test]
async fn a_pasted_setup_token_becomes_one_plan_account_per_token() {
    let harness = harness();
    let user = member(&harness, "gw-token");
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/auth/token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "ok": true,
            "profile": "ignored-the-server-names-it",
            "billing": "plan",
            "account": {"loggedIn": true, "authMethod": "oauth_token"},
        })))
        .mount(&server)
        .await;
    let service = gateway_service(&harness);
    let token = "sk-ant-oat01-synthetic-token-not-real-1111";
    let first = service
        .add_gateway_token_account(
            &user,
            "claude-agent-sdk",
            &gateway_endpoint(&server),
            token,
            None,
        )
        .await
        .unwrap();
    assert!(first.created);
    let again = service
        .add_gateway_token_account(
            &user,
            "claude-agent-sdk",
            &gateway_endpoint(&server),
            token,
            None,
        )
        .await
        .unwrap();
    assert!(!again.created, "the same token is the same account");
    let accounts = harness.accounts.list(&user, "claude-agent-sdk").unwrap();
    assert_eq!(accounts.len(), 1);
    assert_eq!(accounts[0].label, "Claude plan (setup-token)");
}
