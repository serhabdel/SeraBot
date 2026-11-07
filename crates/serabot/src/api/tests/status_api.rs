//! Owner server status: admin only, numbers only (synthetic data only).

use super::*;

fn put(path: &std::path::Path, bytes: usize) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, vec![b'x'; bytes]).unwrap();
}

#[tokio::test]
async fn status_is_admin_only_and_audited() {
    let (app, _, store) = app();
    let (member, member_token) = member_with_session(&store);

    let denied = app
        .clone()
        .oneshot(request_with_token(
            "GET",
            "/v1/admin/status",
            &member_token,
            None,
        ))
        .await
        .unwrap();
    assert_eq!(denied.status(), StatusCode::FORBIDDEN);
    let connection = store.connection().unwrap();
    let denials: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM audit_log WHERE event = 'admin.status.denied' AND actor = ?1",
            [&member.id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(denials, 1);
    drop(connection);

    let unauthenticated = app
        .clone()
        .oneshot(request(&app, "GET", "/v1/admin/status", false, None))
        .await
        .unwrap();
    assert_eq!(unauthenticated.status(), StatusCode::UNAUTHORIZED);

    for _ in 0..2 {
        let response = app
            .clone()
            .oneshot(request(&app, "GET", "/v1/admin/status", true, None))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    }
    let connection = store.connection().unwrap();
    let views: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM audit_log WHERE event = 'admin.status.viewed'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(views, 1, "polling is audited once per window");
}

#[tokio::test]
async fn status_reports_health_storage_and_members_without_content() {
    let (app, _, store) = app();
    let (member, _) = member_with_session(&store);
    let data = store.path().parent().unwrap().to_path_buf();
    put(
        &data.join("inbox").join(&member.id).join("secret-notes.txt"),
        2048,
    );
    put(&data.join("artifacts/h1/deck.pptx"), 4096);
    put(&data.join("voice.mp3"), 512);
    put(&data.join("logs/server.log"), 64);

    let body = read_json(
        app.clone()
            .oneshot(request(&app, "GET", "/v1/admin/status", true, None))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(body["ok"], true);
    assert!(body["version"].as_str().is_some_and(|v| !v.is_empty()));
    assert!(body["uptime_secs"].as_u64().is_some());
    assert!(body["process"]["memory_bytes"].as_u64().unwrap() > 0);
    assert!(body["host"]["memory_total_bytes"].as_u64().unwrap() > 0);
    assert!(body["host"]["cpu_cores"].as_u64().unwrap() >= 1);
    assert_eq!(body["storage"]["truncated"], false);

    let category = |id: &str| {
        body["storage"]["categories"]
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["id"] == id)
            .unwrap_or_else(|| panic!("category {id}"))
            .clone()
    };
    assert!(category("database")["bytes"].as_u64().unwrap() > 0);
    assert_eq!(category("artifacts")["bytes"], 4096);
    assert_eq!(category("documents")["bytes"], 2048);
    assert_eq!(category("media")["bytes"], 512);
    assert_eq!(category("logs")["bytes"], 64);
    assert_eq!(category("browser")["bytes"], 0);
    assert_eq!(body["storage"]["categories"].as_array().unwrap().len(), 9);

    let users = body["users"].as_array().unwrap();
    let row = users
        .iter()
        .find(|u| u["id"] == member.id.as_str())
        .expect("member row");
    assert_eq!(row["email"], member.email.as_str());
    assert_eq!(row["file_bytes"], 2048);
    assert_eq!(row["messages"], 0);
    assert!(users.len() >= 2);

    // Counts and sizes only: no file names, no content, no credentials.
    let text = body.to_string();
    assert!(!text.contains("secret-notes"));
    assert!(!text.contains("deck.pptx"));
    assert!(!text.contains("credential"));
}
