//! The jobs panel feed (0.28.0 C1): `GET /v1/jobs` lists the acting user's
//! in-flight tasks and media jobs across conversations; another user's work
//! never appears.

use super::*;

#[tokio::test]
async fn jobs_lists_only_the_acting_users_work() {
    let (app, _, store) = app_with_root(true, Policy::empty(), temp_base());
    let alice = store
        .create_user("subject-ja", "ja@example.example", "Synthetic A")
        .unwrap();
    let bob = store
        .create_user("subject-jb", "jb@example.example", "Synthetic B")
        .unwrap();
    let alice_token = mint_for(&store, &alice.id, "jobs-a");
    let bob_token = mint_for(&store, &bob.id, "jobs-b");
    // Conversation ids are global; each user gets their own set.
    store
        .create_conversation(&alice.id, "dir:main", "direct", None)
        .unwrap();
    store
        .create_conversation(&alice.id, "dir:second", "direct", None)
        .unwrap();
    store
        .create_conversation(&bob.id, "dir:other", "direct", None)
        .unwrap();

    let alice_task = store
        .enqueue_task(
            &alice.id,
            "turn",
            "main",
            &json!({"conversation_id": "dir:main"}),
            0,
            chrono::Utc::now(),
        )
        .unwrap();
    store
        .enqueue_task(&bob.id, "turn", "main", &json!({}), 0, chrono::Utc::now())
        .unwrap();
    let alice_job = store
        .create_media_job(
            &alice.id,
            "dir:second",
            "main",
            "synthetic",
            crate::media_jobs::MediaKind::Image,
            "ext-1",
        )
        .unwrap();
    store
        .create_media_job(
            &bob.id,
            "dir:other",
            "main",
            "synthetic",
            crate::media_jobs::MediaKind::Video,
            "ext-2",
        )
        .unwrap();

    let response = app
        .clone()
        .oneshot(request_with_token("GET", "/v1/jobs", &alice_token, None))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = read_json(response).await;
    let tasks = body["tasks"].as_array().unwrap();
    assert_eq!(tasks.len(), 1);
    assert_eq!(tasks[0]["id"], alice_task.id);
    assert_eq!(tasks[0]["status"], "queued");
    let jobs = body["media_jobs"].as_array().unwrap();
    assert_eq!(jobs.len(), 1);
    assert_eq!(jobs[0]["id"], alice_job.id);
    assert_eq!(jobs[0]["conversation_id"], "dir:second");
    assert_eq!(jobs[0]["status"], "pending");

    // Bob sees his own task and his own job — never Alice's.
    let response = app
        .clone()
        .oneshot(request_with_token("GET", "/v1/jobs", &bob_token, None))
        .await
        .unwrap();
    let body = read_json(response).await;
    assert_eq!(body["tasks"].as_array().unwrap().len(), 1);
    let jobs = body["media_jobs"].as_array().unwrap();
    assert_eq!(jobs.len(), 1);
    assert_eq!(jobs[0]["kind"], "video");

    // The per-conversation endpoint keeps working alongside.
    let response = app
        .clone()
        .oneshot(request_with_token(
            "GET",
            "/v1/conversations/dir:second/media-jobs",
            &alice_token,
            None,
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        read_json(response).await["media_jobs"]
            .as_array()
            .unwrap()
            .len(),
        1
    );

    // A bogus status filter is a 422; a valid one narrows the tasks.
    let response = app
        .clone()
        .oneshot(request_with_token(
            "GET",
            "/v1/jobs?status=bogus",
            &alice_token,
            None,
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
    let response = app
        .clone()
        .oneshot(request_with_token(
            "GET",
            "/v1/jobs?status=running",
            &alice_token,
            None,
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        read_json(response).await["tasks"].as_array().unwrap().len(),
        0
    );
}
