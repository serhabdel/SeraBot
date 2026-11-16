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

#[tokio::test]
async fn continue_after_limit_queues_one_resume_for_the_owner_only() {
    let (app, _, store) = app();
    let owner = store
        .create_user("subject-cont-a", "ca@example.example", "Synthetic CA")
        .unwrap();
    let other = store
        .create_user("subject-cont-b", "cb@example.example", "Synthetic CB")
        .unwrap();
    let owner_token = mint_for(&store, &owner.id, "cont-a");
    let other_token = mint_for(&store, &other.id, "cont-b");
    store
        .ensure_conversation_accepts_turns(&owner.id, "dir:limited")
        .unwrap();
    let uri = "/v1/conversations/dir:limited/continue";

    // Nothing stopped at a limit yet.
    let idle = call(&app, &owner_token, "POST", uri, None).await;
    assert!(!idle.status().is_success());

    let task = store
        .enqueue_task(
            &owner.id,
            "turn",
            "main",
            &serde_json::json!({}),
            0,
            chrono::Utc::now(),
        )
        .unwrap();
    let turn = store
        .create_turn(
            &owner.id,
            &task.id,
            "main",
            "dir:limited",
            &serde_json::json!({}),
        )
        .unwrap();
    store
        .update_turn(
            &owner.id,
            &turn.id,
            crate::runtime::LIMIT_REACHED_STATUS,
            &serde_json::json!({}),
        )
        .unwrap();

    let foreign = call(&app, &other_token, "POST", uri, None).await;
    assert_eq!(foreign.status(), StatusCode::NOT_FOUND);
    let queued = call(&app, &owner_token, "POST", uri, None).await;
    assert_eq!(queued.status(), StatusCode::OK);
    let body = read_json(queued).await;
    let resume = store
        .get_task(&owner.id, body["task_id"].as_str().unwrap())
        .unwrap();
    assert_eq!(resume.kind, "resume");
    assert_eq!(resume.payload["continue_after_limit"], true);
    // A second click finds nothing to continue.
    let again = call(&app, &owner_token, "POST", uri, None).await;
    assert!(!again.status().is_success());
}

#[tokio::test]
async fn history_pages_back_with_a_cursor_and_reports_more() {
    let (app, _, store) = app();
    let user = store
        .create_user("subject-page-a", "pa@example.example", "Synthetic Page A")
        .unwrap();
    let other = store
        .create_user("subject-page-b", "pb@example.example", "Synthetic Page B")
        .unwrap();
    let token = mint_for(&store, &user.id, "page-a");
    let other_token = mint_for(&store, &other.id, "page-b");
    store
        .ensure_conversation_accepts_turns(&user.id, "dir:paged")
        .unwrap();
    let mut ids = Vec::new();
    for n in 0..5 {
        let role = if n % 2 == 0 {
            crate::domain::Role::User
        } else {
            crate::domain::Role::Assistant
        };
        ids.push(
            store
                .append_message(&user.id, "dir:paged", None, role, &format!("synthetic {n}"))
                .unwrap()
                .id,
        );
    }
    store
        .connection()
        .unwrap()
        .execute(
            "UPDATE messages SET created_at = '2026-01-01T00:00:00.000Z'",
            [],
        )
        .unwrap();

    let newest = read_json(
        call(
            &app,
            &token,
            "GET",
            "/v1/conversations/dir:paged/messages?limit=2",
            None,
        )
        .await,
    )
    .await;
    assert_eq!(newest["has_more"], true);
    assert_eq!(newest["messages"].as_array().unwrap().len(), 2);
    assert_eq!(newest["messages"][0]["id"], ids[3].as_str());

    let older = read_json(
        call(
            &app,
            &token,
            "GET",
            &format!(
                "/v1/conversations/dir:paged/messages?limit=10&before={}",
                ids[3]
            ),
            None,
        )
        .await,
    )
    .await;
    assert_eq!(older["has_more"], false);
    let got: Vec<&str> = older["messages"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["id"].as_str().unwrap())
        .collect();
    assert_eq!(got, vec![ids[0].as_str(), ids[1].as_str(), ids[2].as_str()]);

    // Another account's cursor over the same ids finds nothing.
    let foreign = read_json(
        call(
            &app,
            &other_token,
            "GET",
            &format!("/v1/conversations/dir:paged/messages?before={}", ids[3]),
            None,
        )
        .await,
    )
    .await;
    assert!(foreign["messages"].as_array().unwrap().is_empty());

    let bad = call(
        &app,
        &token,
        "GET",
        "/v1/conversations/dir:paged/messages?before=..%2Fsecret",
        None,
    )
    .await;
    assert!(bad.status().is_client_error());
}
