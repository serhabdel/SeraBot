//! Wave A visibility APIs (0.29.0): routines, monitoring, workflows, traces.

use super::*;
use crate::plans::{PlanItem, PlanStatus};
use crate::scheduler::ensure_daily_routine;
use chrono_tz::Europe::Rome;

#[tokio::test]
async fn routines_are_scoped_to_the_acting_user() {
    let (app, _, store) = app();
    let alice = store
        .create_user("subject-ra", "ra@example.example", "Synthetic RA")
        .unwrap();
    let bob = store
        .create_user("subject-rb", "rb@example.example", "Synthetic RB")
        .unwrap();
    ensure_daily_routine(&store, &alice.id, "morning_brief", "07:30", Rome).unwrap();
    ensure_daily_routine(&store, &bob.id, "morning_brief", "08:00", Rome).unwrap();
    let alice_token = mint_for(&store, &alice.id, "ra");
    let bob_token = mint_for(&store, &bob.id, "rb");

    let alice_body = read_json(
        app.clone()
            .oneshot(request_with_token("GET", "/v1/routines", &alice_token, None))
            .await
            .unwrap(),
    )
    .await;
    let rows = alice_body["routines"].as_array().unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["name"], "morning_brief");
    assert_eq!(rows[0]["config"]["time"], "07:30");

    let bob_body = read_json(
        app.clone()
            .oneshot(request_with_token("GET", "/v1/routines", &bob_token, None))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(bob_body["routines"].as_array().unwrap().len(), 1);
    assert_eq!(
        bob_body["routines"][0]["config"]["time"],
        "08:00"
    );
}

#[tokio::test]
async fn monitoring_sources_never_return_html_errors() {
    let (app, _, store) = app();
    let user = store
        .create_user("subject-mon", "mon@example.example", "Synthetic Mon")
        .unwrap();
    let token = mint_for(&store, &user.id, "mon");
    store
        .connection()
        .unwrap()
        .execute(
            "INSERT INTO source_runs(id, source, started_at, finished_at, ok, error, items_seen, user_id)
             VALUES ('run-html', 'portal', '2026-11-09T10:00:00Z', '2026-11-09T10:01:00Z', 0,
                     '<html><body>secret page</body></html>', 0, ?1)",
            rusqlite::params![user.id],
        )
        .unwrap();

    let body = read_json(
        app.clone()
            .oneshot(request_with_token(
                "GET",
                "/v1/monitoring/sources",
                &token,
                None,
            ))
            .await
            .unwrap(),
    )
    .await;
    let text = body.to_string();
    assert!(!text.contains("<html"));
    assert!(!text.contains("secret page"));
}

#[tokio::test]
async fn workflows_list_only_incomplete_plans_for_the_acting_user() {
    let (app, _, store) = app();
    let alice = store
        .create_user("subject-wa", "wa@example.example", "Synthetic WA")
        .unwrap();
    let bob = store
        .create_user("subject-wb", "wb@example.example", "Synthetic WB")
        .unwrap();
    store
        .create_conversation(&alice.id, "dir:main", "direct", None)
        .unwrap();
    store
        .create_conversation(&bob.id, "dir:other", "direct", None)
        .unwrap();
    store
        .set_plan(
            &alice.id,
            "dir:main",
            "Ship feature",
            &[PlanItem {
                text: "draft".into(),
                status: PlanStatus::Doing,
            }],
        )
        .unwrap();
    store
        .create_conversation(&alice.id, "dir:done", "direct", None)
        .unwrap();
    store
        .set_plan(
            &alice.id,
            "dir:done",
            "Already finished",
            &[PlanItem {
                text: "step".into(),
                status: PlanStatus::Done,
            }],
        )
        .unwrap();
    store
        .set_plan(
            &bob.id,
            "dir:other",
            "Bob plan",
            &[PlanItem {
                text: "x".into(),
                status: PlanStatus::Pending,
            }],
        )
        .unwrap();

    let alice_token = mint_for(&store, &alice.id, "wa");
    let body = read_json(
        app.clone()
            .oneshot(request_with_token(
                "GET",
                "/v1/workflows/active",
                &alice_token,
                None,
            ))
            .await
            .unwrap(),
    )
    .await;
    let rows = body["workflows"].as_array().unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["conversation_id"], "dir:main");
    assert_eq!(rows[0]["done"], 0);
    assert_eq!(rows[0]["total"], 1);
    assert!(!body.to_string().contains("Bob plan"));
}

#[tokio::test]
async fn traces_are_redacted_and_scoped_by_conversation() {
    let (app, _, store) = app();
    let alice = store
        .create_user("subject-tr", "tr@example.example", "Synthetic TR")
        .unwrap();
    let bob = store
        .create_user("subject-trb", "trb@example.example", "Synthetic TRB")
        .unwrap();
    store
        .create_conversation(&alice.id, "dir:trace-a", "direct", None)
        .unwrap();
    store
        .create_conversation(&bob.id, "dir:trace-b", "direct", None)
        .unwrap();
    let task = store
        .enqueue_task(
            &alice.id,
            "turn",
            "main",
            &json!({"conversation_id": "dir:trace-a"}),
            0,
            chrono::Utc::now(),
        )
        .unwrap();
    let turn = store
        .create_turn(&alice.id, &task.id, "main", "dir:trace-a", &json!({}))
        .unwrap();
    let action = store
        .create_action(
            &alice.id,
            "browser_fill",
            Some(&task.id),
            Some(&turn.id),
            r#"{"secret":"synthetic-password"}"#,
            "hash-synthetic",
            Some("https://bank.example"),
            None,
            "policy-synthetic",
        )
        .unwrap();
    store
        .record_audit(
            &alice.id,
            "tool.completed",
            Some("browser_fill"),
            Some(&task.id),
            None,
            Some(&action.id),
            Some("policy-synthetic"),
            Some(&json!({"elapsed_ms": 42, "artifact_hash": "abc123"})),
        )
        .unwrap();
    store
        .record_audit(
            &alice.id,
            "approval.requested",
            Some("browser_fill"),
            Some(&task.id),
            Some("apr-synthetic"),
            Some(&action.id),
            Some("policy-synthetic"),
            Some(&json!({
                "destination": "https://bank.example",
                "artifact_hash": "abc123"
            })),
        )
        .unwrap();

    let token = mint_for(&store, &alice.id, "tr");
    let body = read_json(
        app.clone()
            .oneshot(request_with_token(
                "GET",
                "/v1/traces?conversation_id=dir%3Atrace-a",
                &token,
                None,
            ))
            .await
            .unwrap(),
    )
    .await;
    let traces = body["traces"].as_array().unwrap();
    assert_eq!(traces.len(), 2);
    let text = body.to_string();
    assert!(!text.contains("synthetic-password"));
    assert!(text.contains("hash-synthetic"));
    assert!(text.contains("https://bank.example"));
    assert_eq!(traces[0]["elapsed_ms"], 42);

    let bob_token = mint_for(&store, &bob.id, "trb");
    let denied = app
        .clone()
        .oneshot(request_with_token(
            "GET",
            "/v1/traces?conversation_id=dir%3Atrace-a",
            &bob_token,
            None,
        ))
        .await
        .unwrap();
    assert_eq!(denied.status(), StatusCode::NOT_FOUND);
}
