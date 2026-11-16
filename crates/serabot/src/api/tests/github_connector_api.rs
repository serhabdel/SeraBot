//! The native GitHub routes (v0.51, M5) with no GitHub App configured. The
//! device flow itself is covered against a mock server in `github_connector`.

use super::*;

async fn call(app: &Router, token: &str, method: &str, uri: &str) -> Response {
    app.clone()
        .oneshot(request_with_token(method, uri, token, None))
        .await
        .unwrap()
}

fn member(store: &Store, subject: &str) -> String {
    let user = store
        .create_user(subject, &format!("{subject}@example.example"), subject)
        .unwrap();
    mint_for(store, &user.id, subject)
}

#[tokio::test]
async fn without_a_client_id_the_routes_say_not_configured_and_disconnect_is_harmless() {
    let (app, _, store) = app_with_root(true, Policy::empty(), std::env::temp_dir());
    let token = member(&store, "gh-api-a");

    let status = read_json(call(&app, &token, "GET", "/v1/connectors/github/native").await).await;
    assert_eq!(status["state"], "not_configured");
    assert_eq!(status["configured"], false);
    assert!(status["manage_url"].is_null());

    let started = call(&app, &token, "POST", "/v1/connectors/github/device/start").await;
    assert!(!started.status().is_success());

    let nothing = call(&app, &token, "GET", "/v1/connectors/github/device/status").await;
    assert_eq!(nothing.status(), StatusCode::NOT_FOUND);

    let gone = read_json(call(&app, &token, "DELETE", "/v1/connectors/github/native").await).await;
    assert_eq!(gone["was_connected"], false);
    assert_eq!(gone["connected"], false);

    // The existing GitHub card carries the native block beside the token one.
    let list = read_json(call(&app, &token, "GET", "/v1/connectors").await).await;
    let card = list["connectors"]
        .as_array()
        .unwrap()
        .iter()
        .find(|view| view["id"] == "github")
        .expect("github card");
    assert_eq!(card["kind"], "token");
    assert_eq!(card["native"]["state"], "not_configured");
}
