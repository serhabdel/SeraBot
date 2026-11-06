use super::*;

async fn call(app: &Router, token: &str, method: &str, uri: &str, body: Option<Value>) -> Response {
    app.clone()
        .oneshot(request_with_token(method, uri, token, body))
        .await
        .unwrap()
}

fn entry_body(value: &str) -> Value {
    json!({
        "kind": "password",
        "username": "synthetic-user",
        "value": value,
        "domains": ["Example.com"],
        "note": "synthetic note"
    })
}

#[tokio::test]
async fn vault_crud_is_write_only_and_requires_auth() {
    let (app, _, store) = app();
    let denied = app
        .clone()
        .oneshot(request(&app, "GET", "/v1/vault", false, None))
        .await
        .unwrap();
    assert_eq!(denied.status(), StatusCode::UNAUTHORIZED);

    let user = store
        .create_user("subject-vault", "v@example.example", "Synthetic V")
        .unwrap();
    let token = mint_for(&store, &user.id, "vault-test");

    let put = call(
        &app,
        &token,
        "PUT",
        "/v1/vault/site",
        Some(entry_body("synthetic-http-secret")),
    )
    .await;
    assert_eq!(put.status(), StatusCode::OK);
    let put = read_json(put).await;
    assert_eq!(put["name"], "site");
    assert_eq!(put["kind"], "password");
    assert_eq!(put["domains"], json!(["example.com"]));
    assert!(!put.to_string().contains("synthetic-http-secret"));

    let listed = read_json(call(&app, &token, "GET", "/v1/vault", None).await).await;
    assert_eq!(listed["entries"].as_array().unwrap().len(), 1);
    assert_eq!(listed["entries"][0]["username"], "synthetic-user");
    assert!(!listed.to_string().contains("synthetic-http-secret"));

    for bad in [
        ("/v1/vault/Bad_Name", entry_body("synthetic-http-secret")),
        ("/v1/vault/ok", json!({"kind": "password", "value": ""})),
        (
            "/v1/vault/ok",
            json!({"kind": "other", "value": "synthetic-x"}),
        ),
        (
            "/v1/vault/ok",
            json!({"kind": "note", "value": "synthetic-x", "extra": 1}),
        ),
    ] {
        let response = call(&app, &token, "PUT", bad.0, Some(bad.1)).await;
        assert!(response.status().is_client_error(), "{}", bad.0);
    }

    let deleted = read_json(call(&app, &token, "DELETE", "/v1/vault/site", None).await).await;
    assert_eq!(deleted, json!({"deleted": true}));
    let again = call(&app, &token, "DELETE", "/v1/vault/site", None).await;
    assert_eq!(again.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn vault_is_isolated_between_users_and_from_the_admin() {
    let (app, _, store) = app();
    let alice = store
        .create_user("subject-alice", "alice@example.example", "Alice")
        .unwrap();
    let bob = store
        .create_user("subject-bob", "bob@example.example", "Bob")
        .unwrap();
    let alice_token = mint_for(&store, &alice.id, "alice");
    let bob_token = mint_for(&store, &bob.id, "bob");
    let admin_token = crate::token_cli::mint(&store, "admin-vault", None).unwrap();

    let put = call(
        &app,
        &alice_token,
        "PUT",
        "/v1/vault/private",
        Some(entry_body("synthetic-alice-secret")),
    )
    .await;
    assert_eq!(put.status(), StatusCode::OK);

    for token in [&bob_token, &admin_token] {
        let listed = read_json(call(&app, token, "GET", "/v1/vault", None).await).await;
        assert!(listed["entries"].as_array().unwrap().is_empty());
        let deleted = call(&app, token, "DELETE", "/v1/vault/private", None).await;
        assert_eq!(deleted.status(), StatusCode::NOT_FOUND);
    }
    // Bob writing the same name creates his own entry, not Alice's.
    let bob_put = call(
        &app,
        &bob_token,
        "PUT",
        "/v1/vault/private",
        Some(entry_body("synthetic-bob-secret")),
    )
    .await;
    assert_eq!(bob_put.status(), StatusCode::OK);
    let alice_view = read_json(call(&app, &alice_token, "GET", "/v1/vault", None).await).await;
    assert_eq!(alice_view["entries"].as_array().unwrap().len(), 1);
}
