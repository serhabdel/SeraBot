//! Memory console settings (`GET /v1/memory/settings`, Mem0 key routes).

use super::*;

#[tokio::test]
async fn memory_settings_reports_builtin_and_refuses_mem0_writes() {
    let (app, _, _) = app();
    let body = read_json(
        app.clone()
            .oneshot(request(&app, "GET", "/v1/memory/settings", true, None))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(body["provider"], "builtin");
    assert_eq!(body["mem0"]["available"], false);
    assert_eq!(body["mem0"]["configured"], false);

    let denied = app
        .clone()
        .oneshot(request(
            &app,
            "PUT",
            "/v1/memory/mem0/key",
            true,
            Some(json!({"api_key": "synthetic-mem0-key"})),
        ))
        .await
        .unwrap();
    assert_eq!(denied.status(), StatusCode::UNPROCESSABLE_ENTITY);
}

#[tokio::test]
async fn usage_includes_daily_task_quota_for_the_acting_user() {
    let (app, _, store) = app();
    let admin_id = admin(&store);
    let body = read_json(
        app.clone()
            .oneshot(request(&app, "GET", "/v1/usage", true, None))
            .await
            .unwrap(),
    )
    .await;
    let quota = &body["quota"]["daily_tasks"];
    assert!(quota["limit"].as_u64().unwrap() > 0);
    assert_eq!(quota["used"].as_i64().unwrap(), 0);
    assert_eq!(quota["near_limit"].as_bool().unwrap(), false);
    assert_eq!(quota["at_limit"].as_bool().unwrap(), false);

    store
        .enqueue_task(
            &admin_id,
            "turn",
            "main",
            &json!({"conversation_id": "owner"}),
            0,
            Utc::now(),
        )
        .unwrap();
    let body = read_json(
        app.clone()
            .oneshot(request(&app, "GET", "/v1/usage", true, None))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(body["quota"]["daily_tasks"]["used"].as_i64().unwrap(), 1);
}
