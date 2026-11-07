//! Composio over HTTP. A refused or unreachable upstream reaches the desktop
//! as 422 or 502 with a safe message and a stable code. It is never 401 (the
//! desktop signs the user out on 401), and the key never appears in a body.

use super::*;
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const KEY: &str = "cmp_synthetic_api_key_0003";

fn app_against(server: &MockServer) -> (Router, Store) {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .map(std::path::Path::to_path_buf)
        .unwrap();
    let (app, _, store) = app_with_stack(
        true,
        Policy::empty(),
        root,
        vec![test_bot(
            "main",
            crate::orchestration::BotRole::Orchestrator,
        )],
        None,
        None,
        None,
        Some(&server.uri()),
    );
    (app, store)
}

async fn send(
    app: &Router,
    token: &str,
    method: &str,
    uri: &str,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let response = app
        .clone()
        .oneshot(request_with_token(method, uri, token, body))
        .await
        .unwrap();
    let status = response.status();
    (status, read_json(response).await)
}

#[tokio::test]
async fn refused_key_is_422_with_the_reason_and_never_401() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v3/toolkits"))
        .respond_with(
            ResponseTemplate::new(401)
                .set_body_json(json!({"error": {"message": "Invalid API key"}})),
        )
        .mount(&server)
        .await;
    let (app, store) = app_against(&server);
    let member = store
        .create_user("subject-composio-422", "c422@example.example", "C")
        .unwrap();
    let token = mint_for(&store, &member.id, "composio-422");

    let (status, body) = send(
        &app,
        &token,
        "PUT",
        "/v1/connectors/composio/key",
        Some(json!({"key": KEY})),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(body["code"], "composio_rejected");
    assert_eq!(
        body["error"],
        "Composio rejected this API key (Invalid API key). Use the project API key from platform.composio.dev → Settings → API keys."
    );
    assert!(!body.to_string().contains(KEY));
}

#[tokio::test]
async fn upstream_outage_is_502_with_a_safe_message() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v3/toolkits"))
        .and(header("x-api-key", KEY))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"items": []})))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v3/connected_accounts"))
        .respond_with(ResponseTemplate::new(503).set_body_json(json!({
            "error": {"message": "maintenance", "suggested_fix": "Retry in a while"}
        })))
        .mount(&server)
        .await;
    let (app, store) = app_against(&server);
    let member = store
        .create_user("subject-composio-502", "c502@example.example", "C")
        .unwrap();
    let token = mint_for(&store, &member.id, "composio-502");

    let (status, _) = send(
        &app,
        &token,
        "PUT",
        "/v1/connectors/composio/key",
        Some(json!({"key": KEY})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let (status, body) = send(
        &app,
        &token,
        "GET",
        "/v1/connectors/composio/accounts",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_GATEWAY);
    assert_eq!(body["code"], "composio_unavailable");
    assert_eq!(
        body["error"],
        "Composio could not be reached (HTTP 503). Composio suggests: Retry in a while"
    );
    assert!(!body.to_string().contains(KEY));
}

#[tokio::test]
async fn rate_limit_is_503_not_502_so_it_is_not_an_unknown_outcome() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v3/toolkits"))
        .and(header("x-api-key", KEY))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"items": []})))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v3/connected_accounts"))
        .respond_with(ResponseTemplate::new(429).set_body_json(json!({"error": {}})))
        .mount(&server)
        .await;
    let (app, store) = app_against(&server);
    let member = store
        .create_user("subject-composio-503", "c503@example.example", "C")
        .unwrap();
    let token = mint_for(&store, &member.id, "composio-503");
    let (status, _) = send(
        &app,
        &token,
        "PUT",
        "/v1/connectors/composio/key",
        Some(json!({"key": KEY})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let (status, body) = send(
        &app,
        &token,
        "GET",
        "/v1/connectors/composio/accounts",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(body["code"], "composio_rate_limited");
    assert!(!body.to_string().contains(KEY));
}
