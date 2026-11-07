//! Chat attachments: upload, chips, scoping (synthetic data only).

use super::*;

fn upload_request(token: &str, conversation: &str, name: &str, body: Vec<u8>) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri(format!("/v1/conversations/{conversation}/attachments"))
        .header("authorization", format!("Bearer {token}"))
        .header("x-filename", name)
        .body(Body::from(body))
        .unwrap()
}

async fn get(app: &Router, token: &str, uri: &str) -> Response {
    app.clone()
        .oneshot(request_with_token("GET", uri, token, None))
        .await
        .unwrap()
}

#[tokio::test]
async fn upload_validates_and_is_scoped_to_the_uploader() {
    let root = temp_base();
    let (app, config, store) = app_with_root(true, Policy::empty(), root.clone());
    let alice = store
        .create_user("subject-fa", "fa@example.example", "Synthetic A")
        .unwrap();
    let bob = store
        .create_user("subject-fb", "fb@example.example", "Synthetic B")
        .unwrap();
    let alice_token = mint_for(&store, &alice.id, "files-a");
    let bob_token = mint_for(&store, &bob.id, "files-b");

    // Disallowed type, mislabelled content, and missing header are 400s.
    for (name, body) in [
        ("tool.exe", b"MZ".to_vec()),
        ("fake.pdf", b"plain text".to_vec()),
        ("page.svg", b"<svg/>".to_vec()),
    ] {
        let response = app
            .clone()
            .oneshot(upload_request(&alice_token, "dir:main", name, body))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST, "{name}");
    }
    // Oversized.
    let response = app
        .clone()
        .oneshot(upload_request(
            &alice_token,
            "dir:main",
            "big.txt",
            vec![b'a'; crate::attachments::MAX_ATTACHMENT_BYTES as usize + 1],
        ))
        .await
        .unwrap();
    assert!(
        matches!(
            response.status(),
            StatusCode::PAYLOAD_TOO_LARGE | StatusCode::BAD_REQUEST
        ),
        "{}",
        response.status()
    );

    let response = app
        .clone()
        .oneshot(upload_request(
            &alice_token,
            "dir:main",
            "notes.md",
            b"# Synthetic notes\nhello".to_vec(),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = read_json(response).await;
    let id = body["attachment"]["id"].as_str().unwrap().to_owned();
    assert_eq!(body["attachment"]["filename"], "notes.md");
    assert!(body["attachment"].get("rel_path").is_none());

    // Stored under the uploader's own inbox directory.
    let stored = store.get_chat_attachment(&alice.id, &id).unwrap().unwrap();
    assert!(stored.rel_path.starts_with(&format!("{}/chat/", alice.id)));
    let on_disk = config
        .path(&config.documents.watch_dir)
        .join(&stored.rel_path);
    assert_eq!(std::fs::read(on_disk).unwrap(), b"# Synthetic notes\nhello");

    // Owner can download; another user cannot see it.
    let own = get(&app, &alice_token, &format!("/v1/attachments/{id}")).await;
    assert_eq!(own.status(), StatusCode::OK);
    let other = get(&app, &bob_token, &format!("/v1/attachments/{id}")).await;
    assert_eq!(other.status(), StatusCode::NOT_FOUND);
    assert!(store.get_chat_attachment(&bob.id, &id).unwrap().is_none());

    // Another user cannot attach it to their own message, nor can the owner
    // attach it to a different conversation.
    let stolen = app
        .clone()
        .oneshot(request_with_token(
            "POST",
            "/v1/messages",
            &bob_token,
            Some(json!({"conversation_id": "dir:main", "text": "hi", "attachments": [id]})),
        ))
        .await
        .unwrap();
    assert!(!stolen.status().is_success(), "{}", stolen.status());
    let wrong_conversation = app
        .clone()
        .oneshot(request_with_token(
            "POST",
            "/v1/messages",
            &alice_token,
            Some(json!({"conversation_id": "dir:other", "text": "hi", "attachments": [id]})),
        ))
        .await
        .unwrap();
    assert!(!wrong_conversation.status().is_success());

    // The owner can send it.
    let sent = app
        .clone()
        .oneshot(request_with_token(
            "POST",
            "/v1/messages",
            &alice_token,
            Some(json!({"conversation_id": "dir:main", "text": "", "attachments": [id]})),
        ))
        .await
        .unwrap();
    assert_eq!(sent.status(), StatusCode::OK);
}

#[test]
fn artifact_ownership_is_per_user() {
    let root = temp_base();
    let (_, _, store) = app_with_root(true, Policy::empty(), root);
    let alice = store
        .create_user("subject-oa", "oa@example.example", "Synthetic A")
        .unwrap();
    let bob = store
        .create_user("subject-ob", "ob@example.example", "Synthetic B")
        .unwrap();
    store
        .create_conversation(&alice.id, "dir:main", "direct", None)
        .unwrap();
    store
        .record_conversation_artifact(
            &alice.id,
            "dir:main",
            None,
            "deck_build",
            "hash-one",
            "deck.pdf",
            None,
        )
        .unwrap();
    assert!(store.user_owns_artifact(&alice.id, "hash-one").unwrap());
    assert!(!store.user_owns_artifact(&bob.id, "hash-one").unwrap());
    assert!(!store.user_owns_artifact(&alice.id, "other").unwrap());
}

#[tokio::test]
async fn local_relay_routes_are_per_user() {
    let root = temp_base();
    let (app, _, store) = app_with_root(true, Policy::empty(), root);
    let alice = store
        .create_user("subject-la", "la@example.example", "Synthetic A")
        .unwrap();
    let bob = store
        .create_user("subject-lb", "lb@example.example", "Synthetic B")
        .unwrap();
    let alice_token = mint_for(&store, &alice.id, "local-a");
    let bob_token = mint_for(&store, &bob.id, "local-b");

    let status = read_json(get(&app, &alice_token, "/v1/local/status").await).await;
    assert_eq!(status["online"], false);

    let poll = app
        .clone()
        .oneshot(request_with_token(
            "POST",
            "/v1/local/poll",
            &alice_token,
            Some(json!({"roots": [{"id": "docs", "label": "Docs", "mode": "read"}], "wait_secs": 0})),
        ))
        .await
        .unwrap();
    assert_eq!(poll.status(), StatusCode::OK);
    assert!(read_json(poll).await["request"].is_null());

    let status = read_json(get(&app, &alice_token, "/v1/local/status").await).await;
    assert_eq!(status["online"], true);
    assert_eq!(status["roots"][0]["id"], "docs");
    // Bob sees nothing of Alice's desktop.
    let status = read_json(get(&app, &bob_token, "/v1/local/status").await).await;
    assert_eq!(status["online"], false);
    assert_eq!(status["roots"], json!([]));

    // Hostile root ids and modes are refused; unknown results are 404.
    for roots in [
        json!([{"id": "../x", "label": "x", "mode": "read"}]),
        json!([{"id": "docs", "label": "x", "mode": "admin"}]),
    ] {
        let bad = app
            .clone()
            .oneshot(request_with_token(
                "POST",
                "/v1/local/poll",
                &alice_token,
                Some(json!({"roots": roots, "wait_secs": 0})),
            ))
            .await
            .unwrap();
        assert_eq!(bad.status(), StatusCode::UNPROCESSABLE_ENTITY);
    }
    let unknown = app
        .clone()
        .oneshot(request_with_token(
            "POST",
            "/v1/local/result/nope",
            &bob_token,
            Some(json!({"ok": true, "text": "x"})),
        ))
        .await
        .unwrap();
    assert_eq!(unknown.status(), StatusCode::NOT_FOUND);
}