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

#[tokio::test]
async fn put_routine_edits_label_and_prompt_and_preserves_them_across_schedule_updates() {
    let (app, _, store) = app();
    let member = store
        .create_user("subject-rout-meta", "rmeta@example.example", "Routine Meta")
        .unwrap();
    let token = mint_for(&store, &member.id, "rmeta");

    // Set label and prompt without changing time/timezone.
    let res = app
        .clone()
        .oneshot(request_with_token(
            "PUT",
            "/v1/routines/morning_brief",
            &token,
            Some(json!({
                "label": "Executive Brief",
                "prompt": "Summarize top priorities and deadlines."
            })),
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let body = read_json(res).await;
    assert_eq!(body["routine"]["config"]["label"], "Executive Brief");
    assert_eq!(
        body["routine"]["config"]["prompt"],
        "Summarize top priorities and deadlines."
    );

    // Updating time via ensure_daily_routine preserves label and prompt.
    crate::scheduler::ensure_daily_routine(&store, &member.id, "morning_brief", "09:15", Rome)
        .unwrap();
    let listed = store.list_routines(&member.id).unwrap();
    let row = listed.iter().find(|r| r.name == "morning_brief").unwrap();
    assert_eq!(row.config["time"], "09:15");
    assert_eq!(row.config["label"], "Executive Brief");
    assert_eq!(
        row.config["prompt"],
        "Summarize top priorities and deadlines."
    );

    // Empty string clears label.
    let clear = app
        .clone()
        .oneshot(request_with_token(
            "PUT",
            "/v1/routines/morning_brief",
            &token,
            Some(json!({ "label": "" })),
        ))
        .await
        .unwrap();
    assert_eq!(clear.status(), StatusCode::OK);
    let cleared = read_json(clear).await;
    assert!(cleared["routine"]["config"].get("label").is_none());
    assert_eq!(
        cleared["routine"]["config"]["prompt"],
        "Summarize top priorities and deadlines."
    );

    // Control characters are rejected.
    let bad = app
        .clone()
        .oneshot(request_with_token(
            "PUT",
            "/v1/routines/morning_brief",
            &token,
            Some(json!({ "label": "bad\nlabel" })),
        ))
        .await
        .unwrap();
    assert_eq!(bad.status(), StatusCode::UNPROCESSABLE_ENTITY);
}

#[tokio::test]
async fn active_workflows_return_items_and_support_editing() {
    let (app, _, store) = app();
    let member = store
        .create_user("subject-wf-edit", "wfe@example.example", "Workflow Edit")
        .unwrap();
    let token = mint_for(&store, &member.id, "wfe");
    store
        .ensure_conversation_accepts_turns(&member.id, "dir:wf1")
        .unwrap();
    store
        .set_plan(
            &member.id,
            "dir:wf1",
            "Initial goal",
            &[
                crate::plans::PlanItem {
                    text: "Step one".into(),
                    status: crate::plans::PlanStatus::Pending,
                },
                crate::plans::PlanItem {
                    text: "Step two".into(),
                    status: crate::plans::PlanStatus::Done,
                },
            ],
        )
        .unwrap();

    // GET /v1/workflows/active includes items.
    let list_res = app
        .clone()
        .oneshot(request_with_token(
            "GET",
            "/v1/workflows/active",
            &token,
            None,
        ))
        .await
        .unwrap();
    assert_eq!(list_res.status(), StatusCode::OK);
    let list_body = read_json(list_res).await;
    assert_eq!(
        list_body["workflows"][0]["items"].as_array().unwrap().len(),
        2
    );

    // PUT /v1/workflows/active/{conversation_id} updates goal and items.
    let put_res = app
        .clone()
        .oneshot(request_with_token(
            "PUT",
            "/v1/workflows/active/dir:wf1",
            &token,
            Some(json!({
                "goal": "Updated goal",
                "items": [
                    {"text": "Revised step", "status": "doing"},
                    {"text": "Second step", "status": "done"}
                ]
            })),
        ))
        .await
        .unwrap();
    assert_eq!(put_res.status(), StatusCode::OK);
    let put_body = read_json(put_res).await;
    assert_eq!(put_body["workflow"]["goal"], "Updated goal");
    assert_eq!(put_body["workflow"]["done"], 1);
    assert_eq!(put_body["workflow"]["total"], 2);
    assert_eq!(put_body["workflow"]["items"][0]["text"], "Revised step");

    // Empty goal is rejected.
    let bad_res = app
        .clone()
        .oneshot(request_with_token(
            "PUT",
            "/v1/workflows/active/dir:wf1",
            &token,
            Some(json!({ "goal": "   " })),
        ))
        .await
        .unwrap();
    assert_eq!(bad_res.status(), StatusCode::UNPROCESSABLE_ENTITY);
}

#[tokio::test]
async fn workflows_and_routines_never_cross_users() {
    let (app, _, store) = app();
    let alice = store
        .create_user("subject-wf-a", "wfa@example.example", "Workflow A")
        .unwrap();
    let bob = store
        .create_user("subject-wf-b", "wfb@example.example", "Workflow B")
        .unwrap();
    let bob_token = mint_for(&store, &bob.id, "wf-b");
    store
        .ensure_conversation_accepts_turns(&alice.id, "dir:wf-private")
        .unwrap();
    store
        .set_plan(
            &alice.id,
            "dir:wf-private",
            "Alice goal",
            &[crate::plans::PlanItem {
                text: "Alice step".into(),
                status: crate::plans::PlanStatus::Pending,
            }],
        )
        .unwrap();

    // Bob's list is empty and a goal-only edit of Alice's plan is a 404.
    let list = app
        .clone()
        .oneshot(request_with_token(
            "GET",
            "/v1/workflows/active",
            &bob_token,
            None,
        ))
        .await
        .unwrap();
    assert_eq!(list.status(), StatusCode::OK);
    assert!(
        read_json(list).await["workflows"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    let edit = app
        .clone()
        .oneshot(request_with_token(
            "PUT",
            "/v1/workflows/active/dir:wf-private",
            &bob_token,
            Some(json!({"goal": "Hijacked"})),
        ))
        .await
        .unwrap();
    assert_eq!(edit.status(), StatusCode::NOT_FOUND);
    assert_eq!(
        store
            .get_plan(&alice.id, "dir:wf-private")
            .unwrap()
            .unwrap()
            .goal,
        "Alice goal"
    );

    // Routines: Bob cannot delete Alice's schedule.
    crate::scheduler::ensure_daily_routine(
        &store,
        &alice.id,
        "morning_brief",
        "07:00",
        chrono_tz::UTC,
    )
    .unwrap();
    assert!(store.routine_exists(&alice.id, "morning_brief").unwrap());
    let delete = app
        .clone()
        .oneshot(request_with_token(
            "DELETE",
            "/v1/routines/morning_brief",
            &bob_token,
            None,
        ))
        .await
        .unwrap();
    assert_eq!(delete.status(), StatusCode::NOT_FOUND);
    assert!(store.routine_exists(&alice.id, "morning_brief").unwrap());
}
