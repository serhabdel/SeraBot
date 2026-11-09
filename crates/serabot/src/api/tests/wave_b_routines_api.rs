//! Wave B routines CRUD and scheduled delivery (0.30.0).

use super::*;
use chrono_tz::Europe::Rome;

#[tokio::test]
async fn put_routine_updates_only_the_acting_users_row() {
    let (app, _, store) = app();
    let alice = store
        .create_user("subject-b1a", "b1a@example.example", "Synthetic B1A")
        .unwrap();
    let bob = store
        .create_user("subject-b1b", "b1b@example.example", "Synthetic B1B")
        .unwrap();
    let alice_token = mint_for(&store, &alice.id, "b1a");
    let bob_token = mint_for(&store, &bob.id, "b1b");

    let body = read_json(
        app.clone()
            .oneshot(request_with_token(
                "PUT",
                "/v1/routines/morning_brief",
                &alice_token,
                Some(json!({"time": "06:15", "timezone": "Europe/Rome", "enabled": true})),
            ))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(body["routine"]["config"]["time"], "06:15");

    let denied = app
        .clone()
        .oneshot(request_with_token(
            "PUT",
            "/v1/routines/morning_brief",
            &bob_token,
            Some(json!({"enabled": false})),
        ))
        .await
        .unwrap();
    assert_eq!(denied.status(), StatusCode::NOT_FOUND);

    let alice_rows = store.list_routines(&alice.id).unwrap();
    assert_eq!(alice_rows[0].enabled, true);
    assert_eq!(alice_rows[0].config["time"], "06:15");
    assert!(store.list_routines(&bob.id).unwrap().is_empty());
}

#[tokio::test]
async fn manual_watch_enqueue_targets_dir_main() {
    let (app, _, store) = app();
    let member = store
        .create_user("subject-b1m", "b1m@example.example", "Synthetic B1M")
        .unwrap();
    store
        .ensure_conversation_accepts_turns(&member.id, "dir:main")
        .unwrap();
    let token = mint_for(&store, &member.id, "b1m");

    let body = read_json(
        app.clone()
            .oneshot(request_with_token(
                "POST",
                "/v1/monitoring/run",
                &token,
                None,
            ))
            .await
            .unwrap(),
    )
    .await;
    let task_id = body["task_id"].as_str().unwrap();
    let task = store.get_task(&member.id, task_id).unwrap();
    assert_eq!(task.kind, "watch");
    let conversation_id = task.payload["conversation_id"].as_str().unwrap();
    assert_eq!(conversation_id, "dir:main");
}

#[tokio::test]
async fn scheduled_enqueue_uses_member_default_conversation() {
    let store = Store::open_in_memory(384).unwrap();
    let member = store
        .create_user("subject-b1s", "b1s@example.example", "Synthetic B1S")
        .unwrap();
    store
        .ensure_conversation_accepts_turns(&member.id, "dir:main")
        .unwrap();
    crate::scheduler::ensure_daily_routine(&store, &member.id, "morning_brief", "08:00", Rome)
        .unwrap();
    let conversation_id = store.default_scheduled_conversation_id(&member.id).unwrap();
    assert_eq!(conversation_id, "dir:main");
    let task = store
        .enqueue_task(
            &member.id,
            "brief",
            "watcher",
            &json!({
                "conversation_id": conversation_id,
                "user_id": member.id,
            }),
            5,
            chrono::Utc::now(),
        )
        .unwrap();
    assert_eq!(task.payload["conversation_id"], "dir:main");
}
