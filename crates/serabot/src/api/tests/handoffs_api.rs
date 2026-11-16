//! Subagent handoff ledger API (v0.50): list, scope and owner retry.
//! Synthetic data only.

use super::*;
use rusqlite::params;

async fn call(app: &Router, token: &str, method: &str, uri: &str, body: Option<Value>) -> Response {
    app.clone()
        .oneshot(request_with_token(method, uri, token, body))
        .await
        .unwrap()
}

/// A member with an `analyst` bot and one handoff ledger row whose child task
/// has the given status.
async fn member_with_handoff(
    app: &Router,
    store: &Store,
    subject: &str,
    child_status: &str,
) -> (String, String, String, String) {
    let user = store
        .create_user(subject, &format!("{subject}@example.example"), subject)
        .unwrap();
    let token = mint_for(store, &user.id, subject);
    let created = call(
        app,
        &token,
        "POST",
        "/v1/bots",
        Some(json!({"name": "Analyst"})),
    )
    .await;
    assert_eq!(created.status(), StatusCode::OK);
    let parent = store
        .enqueue_task(&user.id, "turn", "main", &json!({}), 0, chrono::Utc::now())
        .unwrap();
    let child = store
        .enqueue_task(
            &user.id,
            "turn",
            "analyst",
            &json!({
                "conversation_id": "dir:main",
                "text": "Summarize the synthetic notes.",
                "bot": "analyst",
                "source": "handoff",
            }),
            0,
            chrono::Utc::now(),
        )
        .unwrap();
    let handoff = crate::domain::new_id();
    let route = json!({
        "selection": {"provider": "default", "model": "test"},
        "fingerprint": "stale",
    });
    {
        let connection = store.connection().unwrap();
        connection
            .execute(
                "INSERT INTO handoffs(id, user_id, parent_task_id, parent_turn_id, child_task_id,
                     to_bot, key_hash, status, effects_unknown, resume_count, route_json,
                     created_at, updated_at)
                 VALUES (?1, ?2, ?3, 'turn', ?4, 'analyst', 'hash', 'queued', 0, 0, ?5, 'now', 'now')",
                params![handoff, user.id, parent.id, child.id, route.to_string()],
            )
            .unwrap();
        connection
            .execute(
                "UPDATE tasks SET status = ?2 WHERE id = ?1",
                params![child.id, child_status],
            )
            .unwrap();
    }
    (user.id, token, handoff, child.id)
}

#[tokio::test]
async fn handoffs_are_listed_per_user_and_conversation() {
    let (app, _, store) = app_with_root(true, Policy::empty(), temp_base());
    let (_, alice, handoff, _) = member_with_handoff(&app, &store, "v050-alice", "running").await;
    let (_, bob, _, _) = member_with_handoff(&app, &store, "v050-bob", "done").await;

    let listed = read_json(call(&app, &alice, "GET", "/v1/handoffs", None).await).await;
    let rows = listed["handoffs"].as_array().unwrap();
    assert_eq!(rows.len(), 1, "{listed}");
    assert_eq!(rows[0]["id"], json!(handoff));
    assert_eq!(rows[0]["status"], "running");
    assert_eq!(rows[0]["to_bot"], "analyst");
    assert_eq!(rows[0]["resume_count"], 0);
    assert_eq!(rows[0]["route"]["selection"]["model"], "test");
    // The ledger's internals and secrets are not part of the view.
    assert!(rows[0].get("key_hash").is_none(), "{listed}");

    let scoped = read_json(
        call(
            &app,
            &alice,
            "GET",
            "/v1/handoffs?conversation_id=dir:main",
            None,
        )
        .await,
    )
    .await;
    assert_eq!(scoped["handoffs"].as_array().unwrap().len(), 1);
    let other = read_json(
        call(
            &app,
            &alice,
            "GET",
            "/v1/handoffs?conversation_id=dir:elsewhere",
            None,
        )
        .await,
    )
    .await;
    assert!(other["handoffs"].as_array().unwrap().is_empty(), "{other}");
    let bad = call(
        &app,
        &alice,
        "GET",
        "/v1/handoffs?conversation_id=bad%20id",
        None,
    )
    .await;
    assert!(!bad.status().is_success());

    let bobs = read_json(call(&app, &bob, "GET", "/v1/handoffs", None).await).await;
    assert_eq!(bobs["handoffs"][0]["status"], "completed", "{bobs}");
    assert_ne!(bobs["handoffs"][0]["id"], json!(handoff));
    let unauthenticated = app
        .clone()
        .oneshot(request(&app, "GET", "/v1/handoffs", false, None))
        .await
        .unwrap();
    assert_eq!(unauthenticated.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn the_owner_retries_a_failed_handoff_once_per_call_and_only_their_own() {
    let (app, _, store) = app_with_root(true, Policy::empty(), temp_base());
    let (alice_id, alice, handoff, child) =
        member_with_handoff(&app, &store, "v050-retry-a", "failed").await;
    let (_, bob, bobs_handoff, _) =
        member_with_handoff(&app, &store, "v050-retry-b", "failed").await;
    let uri = format!("/v1/handoffs/{handoff}/retry");

    // Another user's handoff is a plain 404, and so is their id under mine.
    let response = call(&app, &bob, "POST", &uri, Some(json!({}))).await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    let response = call(
        &app,
        &alice,
        "POST",
        &format!("/v1/handoffs/{bobs_handoff}/retry"),
        Some(json!({})),
    )
    .await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);

    let response = call(&app, &alice, "POST", &uri, Some(json!({}))).await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = read_json(response).await;
    assert_eq!(body["handoff"]["status"], "resumed", "{body}");
    assert_eq!(body["handoff"]["resume_count"], 1);
    let new_child = body["handoff"]["child_task_id"]
        .as_str()
        .unwrap()
        .to_owned();
    assert_ne!(new_child, child);
    let task = store.get_task(&alice_id, &new_child).unwrap();
    assert_eq!(task.bot, "analyst");
    assert_eq!(task.payload["text"], "Summarize the synthetic notes.");
    assert_eq!(task.status, "queued");
    // The audit names the resume.
    assert!(
        store
            .list_audit_for_user(&alice_id, 50)
            .unwrap()
            .iter()
            .any(|entry| entry.event == "handoff.resumed")
    );
    // A running (resumed) child cannot be retried again.
    let again = call(&app, &alice, "POST", &uri, Some(json!({}))).await;
    assert!(!again.status().is_success());
    // A route override outside the catalog is refused.
    store
        .connection()
        .unwrap()
        .execute(
            "UPDATE tasks SET status = 'failed' WHERE id = ?1",
            params![new_child],
        )
        .unwrap();
    let bad = call(
        &app,
        &alice,
        "POST",
        &uri,
        Some(json!({"route_override": {"provider": "nope", "model": "x"}})),
    )
    .await;
    assert!(!bad.status().is_success());
}

#[tokio::test]
async fn an_unknown_effect_needs_the_owners_confirmation_to_retry() {
    let (app, _, store) = app_with_root(true, Policy::empty(), temp_base());
    let (_, token, handoff, child) =
        member_with_handoff(&app, &store, "v050-unknown", "failed").await;
    store
        .connection()
        .unwrap()
        .execute(
            "UPDATE tasks SET uncertain = 1 WHERE id = ?1",
            params![child],
        )
        .unwrap();
    let uri = format!("/v1/handoffs/{handoff}/retry");
    let refused = call(&app, &token, "POST", &uri, Some(json!({}))).await;
    assert!(!refused.status().is_success());
    let accepted = call(
        &app,
        &token,
        "POST",
        &uri,
        Some(json!({"confirm_effects_unknown": true})),
    )
    .await;
    assert_eq!(accepted.status(), StatusCode::OK);
}

#[tokio::test]
async fn the_owner_stops_one_run_and_only_their_own() {
    let (app, _, store) = app_with_root(true, Policy::empty(), temp_base());
    let (alice_id, alice, handoff, child) =
        member_with_handoff(&app, &store, "v050-stop-a", "queued").await;
    let (_, bob, bobs_handoff, _) =
        member_with_handoff(&app, &store, "v050-stop-b", "queued").await;
    let uri = format!("/v1/handoffs/{handoff}/cancel");

    // Another user's handoff reads as unknown, both ways round.
    let response = call(&app, &bob, "POST", &uri, None).await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    let response = call(
        &app,
        &alice,
        "POST",
        &format!("/v1/handoffs/{bobs_handoff}/cancel"),
        None,
    )
    .await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);

    let response = call(&app, &alice, "POST", &uri, None).await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = read_json(response).await;
    assert_eq!(body["cancelled"], true, "{body}");
    assert_eq!(
        store.get_task(&alice_id, &child).unwrap().status,
        "cancelled"
    );

    // A finished run cannot be stopped again.
    let again = call(&app, &alice, "POST", &uri, None).await;
    assert!(!again.status().is_success());
}
