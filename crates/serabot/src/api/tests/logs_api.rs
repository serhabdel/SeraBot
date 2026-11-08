//! The logs viewer (0.28.0 C4): `GET /v1/logs` is owner-only, redacted, and
//! filterable by level and substring.

use super::*;

const ADMIN_TOKEN: &str = "test-token-that-is-long-enough-123";

#[tokio::test]
async fn logs_are_owner_only_and_filterable() {
    let (app, _, store) = app_with_root(true, Policy::empty(), temp_base());
    let member = store
        .create_user("subject-la", "la@example.example", "Synthetic Member")
        .unwrap();
    let member_token = mint_for(&store, &member.id, "logs-member");

    // A member gets an explicit 403; the bootstrap admin token gets the feed.
    let response = app
        .clone()
        .oneshot(request_with_token("GET", "/v1/logs", &member_token, None))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);

    let response = app
        .clone()
        .oneshot(request_with_token("GET", "/v1/logs", ADMIN_TOKEN, None))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = read_json(response).await;
    assert!(body["logs"].is_array());
    assert_eq!(body["capacity"], crate::logging::RING_CAPACITY as u64);

    // Emit a synthetic event through the installed ring layer and find it
    // by substring; bearer-shaped secrets must be scrubbed on the way in.
    let log_dir = std::env::temp_dir().join(format!("serabot-logs-test-{}", uuid::Uuid::new_v4()));
    crate::logging::install_default(&log_dir).unwrap();
    let marker = format!("synthetic-marker-{}", uuid::Uuid::new_v4());
    tracing::warn!(target: "serabot::test", "{marker} Bearer abcdef0123456789abcdef");

    let response = app
        .clone()
        .oneshot(request_with_token(
            "GET",
            &format!("/v1/logs?q={marker}"),
            ADMIN_TOKEN,
            None,
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = read_json(response).await;
    let logs = body["logs"].as_array().unwrap();
    assert_eq!(logs.len(), 1, "{body}");
    assert_eq!(logs[0]["level"], "warn");
    assert!(logs[0]["message"].as_str().unwrap().contains(&marker));
    assert!(
        !logs[0]["message"]
            .as_str()
            .unwrap()
            .contains("abcdef0123456789abcdef")
    );

    // The level floor hides the warn event at ?level=error.
    let response = app
        .clone()
        .oneshot(request_with_token(
            "GET",
            &format!("/v1/logs?q={marker}&level=error"),
            ADMIN_TOKEN,
            None,
        ))
        .await
        .unwrap();
    let body = read_json(response).await;
    assert_eq!(body["logs"].as_array().unwrap().len(), 0);

    // The daily file writer landed the event under data/logs-style dir.
    let today = chrono::Utc::now().date_naive();
    let log_file = log_dir.join(format!("serabot-{today}.log"));
    let contents = std::fs::read_to_string(&log_file).unwrap();
    assert!(contents.contains(&marker));
    assert!(!contents.contains("abcdef0123456789abcdef"));
    let _ = std::fs::remove_dir_all(&log_dir);
}
