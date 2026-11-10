//! Big Wave B3 source overlay + B4 reliability (HTTP isolation).

use super::*;

#[tokio::test]
async fn sources_overlay_is_uid_scoped() {
    let (app, config, store) = app_with_root(true, Policy::empty(), temp_base());
    let admin = admin(&store);
    let member = store
        .create_user("sub-b3", "b3@example.example", "B3")
        .unwrap();
    let admin_token = mint_for(&store, &admin, "b3-admin");
    let member_token = mint_for(&store, &member.id, "b3-member");

    let admin_sources = crate::config::load_sources(&config).unwrap();
    let Some(first) = admin_sources.first() else {
        return;
    };
    let name = &first.name;

    let response = app
        .clone()
        .oneshot(request_with_token(
            "PATCH",
            &format!("/v1/sources/{name}"),
            &member_token,
            Some(json!({"enabled": true})),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let response = app
        .clone()
        .oneshot(request_with_token(
            "GET",
            "/v1/sources",
            &member_token,
            None,
        ))
        .await
        .unwrap();
    let body = read_json(response).await;
    let rows = body["sources"].as_array().unwrap();
    let row = rows.iter().find(|r| r["name"] == *name).unwrap();
    assert_eq!(row["enabled"], true);

    let response = app
        .clone()
        .oneshot(request_with_token("GET", "/v1/sources", &admin_token, None))
        .await
        .unwrap();
    let body = read_json(response).await;
    let row = body["sources"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["name"] == *name)
        .unwrap();
    assert_eq!(row["enabled"], first.enabled);
}

#[tokio::test]
async fn member_rss_rejects_non_allowlisted_host() {
    let (app, _, store) = app_with_root(true, Policy::empty(), temp_base());
    let member = store
        .create_user("sub-b3b", "b3b@example.example", "B3B")
        .unwrap();
    let member_token = mint_for(&store, &member.id, "b3b-member");

    let response = app
        .clone()
        .oneshot(request_with_token(
            "POST",
            "/v1/sources/rss",
            &member_token,
            Some(json!({"name": "my-feed", "url": "https://evil.example/feed"})),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
}
