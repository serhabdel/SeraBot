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
    assert!(alice_rows[0].enabled);
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

#[tokio::test]
async fn multiple_users_run_monitoring_and_routines_without_dir_main_collision() {
    let (app, _, store) = app();
    let admin_id = admin(&store);
    let alice = store
        .create_user(
            "subject-b1-alice",
            "b1a2@example.example",
            "Synthetic Alice",
        )
        .unwrap();
    let bob = store
        .create_user("subject-b1-bob", "b1b2@example.example", "Synthetic Bob")
        .unwrap();
    store
        .ensure_conversation_accepts_turns(&admin_id, "dir:main")
        .unwrap();
    let alice_token = mint_for(&store, &alice.id, "b1a2");
    let bob_token = mint_for(&store, &bob.id, "b1b2");

    // Both non-admin users trigger manual watch and routine runs without colliding on `dir:main`.
    for (token, user_id) in [(&alice_token, &alice.id), (&bob_token, &bob.id)] {
        let watch_res = app
            .clone()
            .oneshot(request_with_token(
                "POST",
                "/v1/monitoring/run",
                token,
                None,
            ))
            .await
            .unwrap();
        assert_eq!(watch_res.status(), StatusCode::OK);
        let watch_body = read_json(watch_res).await;
        let watch_task = store
            .get_task(user_id, watch_body["task_id"].as_str().unwrap())
            .unwrap();
        let conv = watch_task.payload["conversation_id"].as_str().unwrap();
        assert_ne!(conv, "dir:main");
        assert!(conv.starts_with("dir:main-"));

        let put_res = app
            .clone()
            .oneshot(request_with_token(
                "PUT",
                "/v1/routines/morning_brief",
                token,
                Some(json!({"time": "07:30", "timezone": "Europe/Rome", "enabled": true})),
            ))
            .await
            .unwrap();
        assert_eq!(put_res.status(), StatusCode::OK);

        let run_res = app
            .clone()
            .oneshot(request_with_token(
                "POST",
                "/v1/routines/morning_brief/run",
                token,
                None,
            ))
            .await
            .unwrap();
        assert_eq!(run_res.status(), StatusCode::OK);
        let run_body = read_json(run_res).await;
        let brief_task = store
            .get_task(user_id, run_body["task_id"].as_str().unwrap())
            .unwrap();
        assert_eq!(brief_task.payload["conversation_id"].as_str(), Some(conv));
    }
}

#[tokio::test]
async fn get_models_enforces_conversation_ownership_and_isolates_settings() {
    let (app, _, store) = app();
    let alice = store
        .create_user("subject-models-a", "ma@example.example", "Models A")
        .unwrap();
    let bob = store
        .create_user("subject-models-b", "mb@example.example", "Models B")
        .unwrap();
    let alice_token = mint_for(&store, &alice.id, "models-a");
    let bob_token = mint_for(&store, &bob.id, "models-b");

    store
        .ensure_conversation_accepts_turns(&alice.id, "dir:alice-chat")
        .unwrap();
    store
        .ensure_conversation_accepts_turns(&bob.id, "dir:bob-chat")
        .unwrap();

    // Alice selects an alternative model on her conversation.
    let select = app
        .clone()
        .oneshot(request_with_token(
            "PUT",
            "/v1/conversations/dir:alice-chat/model",
            &alice_token,
            Some(json!({"provider": "default", "model": "test-alternative"})),
        ))
        .await
        .unwrap();
    assert_eq!(select.status(), StatusCode::OK);

    // Alice sees her own selection.
    let own = app
        .clone()
        .oneshot(request_with_token(
            "GET",
            "/v1/models?conversation_id=dir:alice-chat",
            &alice_token,
            None,
        ))
        .await
        .unwrap();
    assert_eq!(own.status(), StatusCode::OK);
    let own_body = read_json(own).await;
    assert_eq!(own_body["selection"]["model"], "test-alternative");

    // Bob querying Alice's conversation fails closed with 404.
    let foreign = app
        .clone()
        .oneshot(request_with_token(
            "GET",
            "/v1/models?conversation_id=dir:alice-chat",
            &bob_token,
            None,
        ))
        .await
        .unwrap();
    assert_eq!(foreign.status(), StatusCode::NOT_FOUND);

    // Bob querying his own conversation gets his default, unaffected by Alice.
    let bob_own = app
        .clone()
        .oneshot(request_with_token(
            "GET",
            "/v1/models?conversation_id=dir:bob-chat",
            &bob_token,
            None,
        ))
        .await
        .unwrap();
    assert_eq!(bob_own.status(), StatusCode::OK);
    let bob_body = read_json(bob_own).await;
    assert_ne!(bob_body["selection"]["model"], "test-alternative");
}
