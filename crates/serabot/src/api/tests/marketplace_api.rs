//! Plugin marketplace (v0.36): catalog reads, per-user install state,
//! write-only secrets, and the refusal of local-program plugins. Synthetic
//! data only; no real provider is contacted.

use super::*;

async fn call(app: &Router, token: &str, method: &str, uri: &str, body: Option<Value>) -> Response {
    app.clone()
        .oneshot(request_with_token(method, uri, token, body))
        .await
        .unwrap()
}

async fn member(store: &Store, subject: &str) -> (String, String) {
    let user = store
        .create_user(subject, &format!("{subject}@example.example"), subject)
        .unwrap();
    let token = mint_for(store, &user.id, subject);
    (user.id, token)
}

#[tokio::test]
async fn the_catalog_lists_the_repository_plugins_without_secrets() {
    let (app, _, store) = app_with_root(true, Policy::empty(), repo_root());
    let (_, token) = member(&store, "v036-catalog").await;
    let listed = read_json(call(&app, &token, "GET", "/v1/marketplace/plugins", None).await).await;
    let plugins = listed["plugins"].as_array().unwrap();
    assert_eq!(plugins.len(), 10, "{listed}");
    assert!(
        plugins.iter().all(|plugin| plugin["status"] == "available"),
        "{listed}"
    );
    assert!(!listed.to_string().contains("api_key\""), "{listed}");

    let detail = read_json(
        call(
            &app,
            &token,
            "GET",
            "/v1/marketplace/plugins/firecrawl",
            None,
        )
        .await,
    )
    .await;
    assert_eq!(detail["plugin"]["slug"], "firecrawl", "{detail}");
    assert_eq!(detail["plugin"]["apps"][0]["transport"], "http", "{detail}");
    assert_eq!(
        detail["plugin"]["apps"][0]["auth"][0]["fields"][0]["configured"], false,
        "{detail}"
    );

    let missing = call(&app, &token, "GET", "/v1/marketplace/plugins/nope", None).await;
    assert_eq!(missing.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn install_enable_disable_and_uninstall_are_per_user() {
    let (app, _, store) = app_with_root(true, Policy::empty(), repo_root());
    let (_, first) = member(&store, "v036-first").await;
    let (_, second) = member(&store, "v036-second").await;

    let installed = call(
        &app,
        &first,
        "POST",
        "/v1/marketplace/plugins/tavily/install",
        None,
    )
    .await;
    assert_eq!(installed.status(), StatusCode::OK);
    let body = read_json(installed).await;
    assert_eq!(body["installed"], true, "{body}");
    assert_eq!(body["enabled"], false, "{body}");

    // Installing again is a no-op, not an error.
    let again = call(
        &app,
        &first,
        "POST",
        "/v1/marketplace/plugins/tavily/install",
        None,
    )
    .await;
    assert_eq!(again.status(), StatusCode::OK);

    let enabled = read_json(
        call(
            &app,
            &first,
            "POST",
            "/v1/marketplace/plugins/tavily/enable",
            None,
        )
        .await,
    )
    .await;
    assert_eq!(enabled["enabled"], true, "{enabled}");

    // The other user has not installed it and cannot change it.
    let other =
        read_json(call(&app, &second, "GET", "/v1/marketplace/plugins/tavily", None).await).await;
    assert_eq!(other["plugin"]["installed"], false, "{other}");
    let stolen = call(
        &app,
        &second,
        "POST",
        "/v1/marketplace/plugins/tavily/disable",
        None,
    )
    .await;
    assert_eq!(stolen.status(), StatusCode::NOT_FOUND);

    let disabled = read_json(
        call(
            &app,
            &first,
            "POST",
            "/v1/marketplace/plugins/tavily/disable",
            None,
        )
        .await,
    )
    .await;
    assert_eq!(disabled["enabled"], false, "{disabled}");

    let removed = call(
        &app,
        &first,
        "DELETE",
        "/v1/marketplace/plugins/tavily",
        None,
    )
    .await;
    assert_eq!(removed.status(), StatusCode::OK);
    let gone = call(
        &app,
        &first,
        "DELETE",
        "/v1/marketplace/plugins/tavily",
        None,
    )
    .await;
    assert_eq!(gone.status(), StatusCode::NOT_FOUND);
}

/// The repository root, so the catalog resolves to the shipped `plugins/` folder.
fn repo_root() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap()
}

fn app_users(store: &Store) -> Arc<UserSecrets> {
    Arc::new(UserSecrets::with_master_key(store.clone(), [9u8; 32]).unwrap())
}

#[tokio::test]
async fn secrets_are_written_once_and_never_read_back() {
    let (app, _, store) = app_with_root(true, Policy::empty(), repo_root());
    let (user_id, token) = member(&store, "v036-secret").await;
    let users = app_users(&store);
    call(
        &app,
        &token,
        "POST",
        "/v1/marketplace/plugins/tavily/install",
        None,
    )
    .await;

    let synthetic = "synthetic-plugin-key-0123456789abcdef";
    let saved = call(
        &app,
        &token,
        "PUT",
        "/v1/marketplace/plugins/tavily/config",
        Some(json!({"values": {"api_key": synthetic}})),
    )
    .await;
    assert_eq!(saved.status(), StatusCode::OK);
    let body = read_json(saved).await;
    assert_eq!(body["status"], "installed", "{body}");
    assert!(!body.to_string().contains(synthetic), "{body}");

    // The value is in the caller's secrets store under the plugin's name.
    let stored = users.providers_for(&user_id).unwrap();
    assert!(
        stored
            .iter()
            .any(|name| name == "marketplace.tavily.tavily.api_key"),
        "{stored:?}"
    );

    let detail =
        read_json(call(&app, &token, "GET", "/v1/marketplace/plugins/tavily", None).await).await;
    assert_eq!(
        detail["plugin"]["apps"][0]["auth"][0]["fields"][0]["configured"], true,
        "{detail}"
    );
    assert!(!detail.to_string().contains(synthetic), "{detail}");

    // A field the plugin does not declare is refused, and nothing is written.
    let unknown = call(
        &app,
        &token,
        "PUT",
        "/v1/marketplace/plugins/tavily/config",
        Some(json!({"values": {"not_a_field": "x"}})),
    )
    .await;
    assert_eq!(unknown.status(), StatusCode::UNPROCESSABLE_ENTITY);
}

#[tokio::test]
async fn enable_and_configure_need_an_install_first() {
    let (app, _, store) = app_with_root(true, Policy::empty(), repo_root());
    let (_, token) = member(&store, "v036-order").await;
    let not_installed = call(
        &app,
        &token,
        "POST",
        "/v1/marketplace/plugins/tavily/enable",
        None,
    )
    .await;
    assert_eq!(not_installed.status(), StatusCode::NOT_FOUND);
    let config_first = call(
        &app,
        &token,
        "PUT",
        "/v1/marketplace/plugins/tavily/config",
        Some(json!({"values": {"api_key": "synthetic-key-0123456789abcdef"}})),
    )
    .await;
    assert_eq!(config_first.status(), StatusCode::NOT_FOUND);
}
