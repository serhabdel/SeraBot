//! Per-user connector routes: Outlook, Google, Notion (no network).

use super::*;

async fn call(app: &Router, token: &str, method: &str, uri: &str, body: Option<Value>) -> Response {
    app.clone()
        .oneshot(request_with_token(method, uri, token, body))
        .await
        .unwrap()
}

fn connector<'a>(listing: &'a Value, id: &str) -> &'a Value {
    listing["connectors"]
        .as_array()
        .unwrap()
        .iter()
        .find(|connector| connector["id"] == id)
        .unwrap_or_else(|| panic!("connector {id} listed"))
}

#[tokio::test]
async fn unconfigured_providers_report_not_set_up_and_never_start_a_flow() {
    let (app, _, store) = app();
    let member = store
        .create_user("subject-conn", "c@example.example", "Synthetic C")
        .unwrap();
    let token = mint_for(&store, &member.id, "conn-test");

    let listing = read_json(call(&app, &token, "GET", "/v1/connectors", None).await).await;
    for id in ["outlook", "outlook_calendar", "google_drive", "gmail"] {
        let view = connector(&listing, id);
        assert_eq!(view["connected"], false, "{id}");
        assert_eq!(view["personal"]["state"], "not_set_up", "{id}");
        assert!(view["personal"]["hint"].as_str().is_some(), "{id}");
        assert_eq!(view["personal"]["methods"], json!([]), "{id}");
    }
    // Notion needs no admin setup: the user pastes an integration token.
    let notion = connector(&listing, "notion");
    assert_eq!(notion["personal"]["state"], "ready");
    assert_eq!(notion["personal"]["methods"], json!(["token"]));
    assert_eq!(
        notion["personal"]["help_url"],
        "https://www.notion.so/profile/integrations"
    );

    for id in [
        "outlook",
        "outlook_calendar",
        "google_drive",
        "gmail",
        "notion",
    ] {
        let response = call(
            &app,
            &token,
            "POST",
            &format!("/v1/connectors/{id}/start"),
            None,
        )
        .await;
        assert_eq!(response.status(), StatusCode::CONFLICT, "{id}");
        let body = read_json(response).await;
        assert_eq!(body["code"], "not_set_up", "{id}");
        assert!(body.get("url").is_none(), "{id} produced a URL");
    }
}

#[tokio::test]
async fn microsoft_consent_starts_with_the_auth_client_id_and_no_secret() {
    let (app, _, store, _idp) = auth_app().await;
    let member = store
        .create_user("subject-ms", "m@example.example", "Synthetic M")
        .unwrap();
    let token = mint_for(&store, &member.id, "ms-test");

    let listing = read_json(call(&app, &token, "GET", "/v1/connectors", None).await).await;
    let outlook = connector(&listing, "outlook");
    assert_eq!(outlook["personal"]["state"], "ready");
    assert_eq!(
        outlook["personal"]["redirect_uri"],
        "https://bot.example.example/v1/connectors/onedrive/callback"
    );

    let response = call(&app, &token, "POST", "/v1/connectors/outlook/start", None).await;
    assert_eq!(response.status(), StatusCode::OK);
    let url = read_json(response).await["url"]
        .as_str()
        .unwrap()
        .to_owned();
    let parsed = url::Url::parse(&url).unwrap();
    let pairs: std::collections::HashMap<_, _> = parsed.query_pairs().into_owned().collect();
    assert_eq!(pairs["client_id"], crate::auth::test_idp::TEST_CLIENT_ID);
    assert!(!pairs["client_id"].trim().is_empty());
    assert!(pairs["scope"].contains("Mail.ReadWrite") && pairs["scope"].contains("Mail.Send"));
    assert!(!pairs.contains_key("client_secret"));
    assert_eq!(pairs["code_challenge_method"], "S256");
}

#[tokio::test]
async fn connections_are_per_user_and_disconnect_is_scoped() {
    let (app, _, store, _idp) = auth_app().await;
    let alice = store
        .create_user("subject-a", "a@example.example", "Synthetic A")
        .unwrap();
    let bob = store
        .create_user("subject-b", "b@example.example", "Synthetic B")
        .unwrap();
    let alice_token = mint_for(&store, &alice.id, "alice");
    let bob_token = mint_for(&store, &bob.id, "bob");

    // Only token connect and consent are write paths; other ids and wrong
    // products are rejected before anything is stored.
    let response = call(
        &app,
        &alice_token,
        "PUT",
        "/v1/connectors/outlook/token",
        Some(json!({"token": "ntn_synthetictoken"})),
    )
    .await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    let response = call(&app, &alice_token, "DELETE", "/v1/connectors/tavily", None).await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);

    let response = call(&app, &bob_token, "DELETE", "/v1/connectors/outlook", None).await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = read_json(response).await;
    assert_eq!(body["connected"], false);
    assert_eq!(body["was_connected"], false);
    assert!(
        !call(&app, &alice_token, "GET", "/v1/connectors", None)
            .await
            .status()
            .is_server_error()
    );
}

#[tokio::test]
async fn token_connectors_list_per_user_and_refuse_malformed_tokens_offline() {
    let (app, _, store) = app();
    let member = store
        .create_user("subject-tok", "t@example.example", "Synthetic T")
        .unwrap();
    let token = mint_for(&store, &member.id, "tok-test");

    let listing = read_json(call(&app, &token, "GET", "/v1/connectors", None).await).await;
    let github = connector(&listing, "github");
    assert_eq!(github["kind"], "token");
    assert_eq!(github["group"], "Code and tasks");
    assert_eq!(github["connected"], false);
    assert_eq!(github["personal"]["state"], "ready");
    assert_eq!(github["personal"]["methods"], json!(["token"]));
    assert!(github.get("secrets").is_none());

    // A malformed token is refused before any request leaves the host.
    let response = call(
        &app,
        &token,
        "PUT",
        "/v1/connectors/github/token",
        Some(json!({"token": "short"})),
    )
    .await;
    assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
    // There is no sign-in flow for a token connector.
    let response = call(&app, &token, "POST", "/v1/connectors/github/start", None).await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    let response = call(&app, &token, "DELETE", "/v1/connectors/github", None).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(read_json(response).await["was_connected"], false);
}
#[tokio::test]
async fn the_connectors_routes_require_a_session() {
    let (app, _, _) = app();
    for (method, uri) in [
        ("POST", "/v1/connectors/outlook/start"),
        ("DELETE", "/v1/connectors/outlook"),
        ("PUT", "/v1/connectors/notion/token"),
    ] {
        let response = app
            .clone()
            .oneshot(request(&app, method, uri, false, None))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED, "{uri}");
    }
}
