//! Marketplace OAuth routes (v0.51, M3) against one wiremock server that plays
//! the authorization server and the MCP server. Synthetic tokens only.

use super::*;
use crate::plugin_runtime::oauth_tests::{FIRST_TOKEN, hits, oauth_manifest, oauth_server};

async fn call(app: &Router, token: &str, method: &str, uri: &str, body: Option<Value>) -> Response {
    app.clone()
        .oneshot(request_with_token(method, uri, token, body))
        .await
        .unwrap()
}

fn member(store: &Store, subject: &str) -> String {
    let user = store
        .create_user(subject, &format!("{subject}@example.example"), subject)
        .unwrap();
    mint_for(store, &user.id, subject)
}

fn repo_root() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap()
}

const BASE: &str = "/v1/marketplace/plugins/fixture";

async fn sign_in(app: &Router, token: &str) -> Value {
    let started = read_json(
        call(
            app,
            token,
            "POST",
            &format!("{BASE}/oauth/start"),
            Some(json!({})),
        )
        .await,
    )
    .await;
    let state = started["state"].as_str().expect("state").to_owned();
    read_json(
        call(
            app,
            token,
            "POST",
            &format!("{BASE}/oauth/complete"),
            Some(json!({"code": "synthetic-code", "state": state})),
        )
        .await,
    )
    .await
}

#[tokio::test]
async fn sign_in_accounts_tool_override_sign_out_and_uninstall() {
    let server = oauth_server(FIRST_TOKEN, true).await;
    MARKETPLACE_OVERRIDE.with(|slot| {
        *slot.borrow_mut() = Some((
            vec![oauth_manifest(&server)],
            crate::mcp_oauth::OauthNet::loopback_for_tests(),
        ));
    });
    let (app, _, store) = app_with_root(true, Policy::empty(), repo_root());
    let first = member(&store, "v051-first");
    let second = member(&store, "v051-second");

    // Not installed: no sign-in.
    let refused = call(
        &app,
        &first,
        "POST",
        &format!("{BASE}/oauth/start"),
        Some(json!({})),
    )
    .await;
    assert_eq!(refused.status(), StatusCode::NOT_FOUND);

    for token in [&first, &second] {
        let installed =
            read_json(call(&app, token, "POST", &format!("{BASE}/install"), None).await).await;
        assert_eq!(installed["status"], "needs_sign_in", "{installed}");
    }

    // The first member starts; the second cannot finish that attempt.
    let started = read_json(
        call(
            &app,
            &first,
            "POST",
            &format!("{BASE}/oauth/start"),
            Some(json!({"label": "Work"})),
        )
        .await,
    )
    .await;
    let url = started["authorize_url"].as_str().unwrap();
    assert!(url.contains("code_challenge_method=S256"), "{started}");
    assert!(!started.to_string().contains("verifier"), "{started}");
    let state = started["state"].as_str().unwrap().to_owned();
    let stolen = call(
        &app,
        &second,
        "POST",
        &format!("{BASE}/oauth/complete"),
        Some(json!({"code": "synthetic-code", "state": state})),
    )
    .await;
    assert_eq!(stolen.status(), StatusCode::NOT_FOUND);

    let done = read_json(
        call(
            &app,
            &first,
            "POST",
            &format!("{BASE}/oauth/complete"),
            Some(json!({"code": "synthetic-code", "state": state})),
        )
        .await,
    )
    .await;
    assert_eq!(done["status"], "installed", "{done}");
    assert_eq!(done["accounts"][0]["state"], "ready", "{done}");
    assert_eq!(done["accounts"][0]["label"], "Work", "{done}");
    assert!(!done.to_string().contains(FIRST_TOKEN), "{done}");

    // Accounts are per user.
    let listed =
        read_json(call(&app, &first, "GET", &format!("{BASE}/accounts"), None).await).await;
    assert_eq!(listed["accounts"].as_array().unwrap().len(), 1, "{listed}");
    let other =
        read_json(call(&app, &second, "GET", &format!("{BASE}/accounts"), None).await).await;
    assert!(other["accounts"].as_array().unwrap().is_empty(), "{other}");
    let detail = read_json(call(&app, &first, "GET", BASE, None).await).await;
    assert_eq!(detail["plugin"]["status"], "installed", "{detail}");
    assert_eq!(
        detail["plugin"]["apps"][0]["auth"][0]["kind"], "oauth",
        "{detail}"
    );
    assert_eq!(
        detail["plugin"]["accounts"][0]["account_id"], "default",
        "{detail}"
    );

    // Per-tool override: validated.
    let tool = |name: &str| format!("{BASE}/tools/{name}");
    for (mode, expected) in [
        ("ask", StatusCode::OK),
        ("default", StatusCode::OK),
        ("always", StatusCode::UNPROCESSABLE_ENTITY),
    ] {
        let response = call(
            &app,
            &first,
            "PUT",
            &tool("lookup"),
            Some(json!({"mode": mode})),
        )
        .await;
        assert_eq!(response.status(), expected, "{mode}");
    }
    let bad = call(
        &app,
        &first,
        "PUT",
        &tool("a%2Fb"),
        Some(json!({"mode": "ask"})),
    )
    .await;
    assert_eq!(bad.status(), StatusCode::UNPROCESSABLE_ENTITY);

    // Sign out revokes at the server and removes the account.
    let missing = call(
        &app,
        &first,
        "DELETE",
        &format!("{BASE}/accounts/nope"),
        None,
    )
    .await;
    assert_eq!(missing.status(), StatusCode::NOT_FOUND);
    let out = read_json(
        call(
            &app,
            &first,
            "DELETE",
            &format!("{BASE}/accounts/default"),
            None,
        )
        .await,
    )
    .await;
    assert_eq!(out["revoked"], true, "{out}");
    assert_eq!(out["status"], "needs_sign_in", "{out}");
    assert_eq!(hits(&server, "/revoke").await, 1);

    // Uninstall also signs out.
    sign_in(&app, &first).await;
    let gone = call(&app, &first, "DELETE", BASE, None).await;
    assert_eq!(gone.status(), StatusCode::OK);
    assert_eq!(hits(&server, "/revoke").await, 2);
    assert!(
        store
            .marketplace_accounts(&store.admin_user_id().unwrap(), "fixture")
            .unwrap()
            .is_empty()
    );
}
