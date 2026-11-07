//! Chat plan and auto-approve routes (no network).

use super::*;

async fn call(app: &Router, token: &str, method: &str, uri: &str, body: Option<Value>) -> Response {
    app.clone()
        .oneshot(request_with_token(method, uri, token, body))
        .await
        .unwrap()
}

#[tokio::test]
async fn auto_approve_is_owner_only_audited_and_off_by_default() {
    let (app, _, store) = app();
    let owner = store
        .create_user("subject-yolo-a", "a@example.example", "Synthetic A")
        .unwrap();
    let other = store
        .create_user("subject-yolo-b", "b@example.example", "Synthetic B")
        .unwrap();
    let owner_token = mint_for(&store, &owner.id, "yolo-a");
    let other_token = mint_for(&store, &other.id, "yolo-b");
    store
        .ensure_conversation_accepts_turns(&owner.id, "dir:owned")
        .unwrap();

    let uri = "/v1/conversations/dir:owned/auto-approve";
    let off = read_json(call(&app, &owner_token, "GET", uri, None).await).await;
    assert_eq!(off["enabled"], false);

    // Another account cannot read or flip the owner's switch.
    for method in ["GET", "PUT"] {
        let response = call(
            &app,
            &other_token,
            method,
            uri,
            Some(json!({"enabled": true})),
        )
        .await;
        assert_eq!(response.status(), StatusCode::NOT_FOUND, "{method}");
    }
    assert!(!store.yolo_enabled(&owner.id, "dir:owned"));
    assert!(!store.yolo_enabled(&other.id, "dir:owned"));

    let on = call(
        &app,
        &owner_token,
        "PUT",
        uri,
        Some(json!({"enabled": true})),
    )
    .await;
    assert_eq!(on.status(), StatusCode::OK);
    assert!(store.yolo_enabled(&owner.id, "dir:owned"));
    // Unknown fields are rejected.
    let bad = call(
        &app,
        &owner_token,
        "PUT",
        uri,
        Some(json!({"enabled": true, "user": "x"})),
    )
    .await;
    assert!(bad.status().is_client_error());

    let off = call(
        &app,
        &owner_token,
        "PUT",
        uri,
        Some(json!({"enabled": false})),
    )
    .await;
    assert_eq!(off.status(), StatusCode::OK);
    assert!(!store.yolo_enabled(&owner.id, "dir:owned"));
    let events: Vec<String> = store
        .list_audit_for_user(&owner.id, 50)
        .unwrap()
        .into_iter()
        .map(|entry| entry.event)
        .collect();
    assert!(events.contains(&"yolo.enabled".to_string()));
    assert!(events.contains(&"yolo.disabled".to_string()));
}

#[tokio::test]
async fn plan_route_returns_only_the_owners_plan() {
    let (app, _, store) = app();
    let owner = store
        .create_user("subject-plan-a", "pa@example.example", "Synthetic PA")
        .unwrap();
    let other = store
        .create_user("subject-plan-b", "pb@example.example", "Synthetic PB")
        .unwrap();
    let owner_token = mint_for(&store, &owner.id, "plan-a");
    let other_token = mint_for(&store, &other.id, "plan-b");
    store
        .ensure_conversation_accepts_turns(&owner.id, "dir:planned")
        .unwrap();
    let uri = "/v1/conversations/dir:planned/plan";

    let none = read_json(call(&app, &owner_token, "GET", uri, None).await).await;
    assert!(none["plan"].is_null());

    let items = vec![
        crate::plans::PlanItem {
            text: "Collect the figures".into(),
            status: crate::plans::PlanStatus::Done,
        },
        crate::plans::PlanItem {
            text: "Write the summary".into(),
            status: crate::plans::PlanStatus::Doing,
        },
    ];
    store
        .set_plan(&owner.id, "dir:planned", "Quarterly summary", &items)
        .unwrap();
    let body = read_json(call(&app, &owner_token, "GET", uri, None).await).await;
    assert_eq!(body["plan"]["goal"], "Quarterly summary");
    assert_eq!(body["plan"]["done"], 1);
    assert_eq!(body["plan"]["total"], 2);
    assert_eq!(body["plan"]["items"][1]["status"], "doing");

    let foreign = call(&app, &other_token, "GET", uri, None).await;
    assert_eq!(foreign.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn cancel_is_owner_scoped_and_reports_nothing_running_when_idle() {
    let (app, _, store) = app();
    let owner = store
        .create_user("subject-stop-a", "sa@example.example", "Synthetic SA")
        .unwrap();
    let other = store
        .create_user("subject-stop-b", "sb@example.example", "Synthetic SB")
        .unwrap();
    let owner_token = mint_for(&store, &owner.id, "stop-a");
    let other_token = mint_for(&store, &other.id, "stop-b");
    store
        .ensure_conversation_accepts_turns(&owner.id, "dir:stoppable")
        .unwrap();
    let uri = "/v1/conversations/dir:stoppable/cancel";

    let idle = call(&app, &owner_token, "POST", uri, None).await;
    assert_eq!(idle.status(), StatusCode::OK);
    assert_eq!(read_json(idle).await["stopping"], 0);

    // Another account cannot stop the owner's conversation.
    let foreign = call(&app, &other_token, "POST", uri, None).await;
    assert_eq!(foreign.status(), StatusCode::NOT_FOUND);
}
