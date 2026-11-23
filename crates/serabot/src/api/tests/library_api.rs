use super::*;

fn upload_request(token: &str, name: Option<&str>, body: Vec<u8>) -> Request<Body> {
    let mut builder = Request::builder()
        .method("POST")
        .uri("/v1/library/upload")
        .header("authorization", format!("Bearer {token}"));
    if let Some(name) = name {
        builder = builder.header("x-filename", name);
    }
    builder.body(Body::from(body)).unwrap()
}

async fn get(app: &Router, token: &str, uri: &str) -> Response {
    app.clone()
        .oneshot(request_with_token("GET", uri, token, None))
        .await
        .unwrap()
}

#[tokio::test]
async fn library_lists_uploads_and_generated_items_per_user() {
    let root = temp_base();
    let (app, _, store) = app_with_root(true, Policy::empty(), root);
    let alice = store
        .create_user("subject-la", "la@example.example", "Synthetic A")
        .unwrap();
    let bob = store
        .create_user("subject-lb", "lb@example.example", "Synthetic B")
        .unwrap();
    let alice_token = mint_for(&store, &alice.id, "lib-a");
    let bob_token = mint_for(&store, &bob.id, "lib-b");

    // A bot-made artifact in Alice's conversation shows up as `generated`,
    // with the conversation title.
    store
        .create_conversation_titled(&alice.id, "dir:main", "direct", None, Some("Main chat"))
        .unwrap();
    store
        .record_conversation_artifact(
            &alice.id,
            "dir:main",
            None,
            "deck_build",
            &"a".repeat(64),
            "deck.pdf",
            Some("Quarterly deck"),
        )
        .unwrap();

    // Upload validation: missing header and empty body are 400s, oversized 413.
    let response = app
        .clone()
        .oneshot(upload_request(&alice_token, None, b"hello".to_vec()))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let response = app
        .clone()
        .oneshot(upload_request(&alice_token, Some("empty.txt"), Vec::new()))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let response = app
        .clone()
        .oneshot(upload_request(
            &alice_token,
            Some("big.bin"),
            vec![b'a'; crate::maker::MAX_ARTIFACT_BYTES + 1],
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);

    // A real upload lands in the artifact store and the library list.
    let response = app
        .clone()
        .oneshot(upload_request(
            &alice_token,
            Some("brand notes.md"),
            b"# Synthetic brand notes".to_vec(),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = read_json(response).await;
    let item = &body["item"];
    assert_eq!(item["kind"], "uploaded");
    assert_eq!(item["filename"], "brand notes.md");
    assert_eq!(item["mime"], "text/plain; charset=utf-8");
    assert_eq!(item["size_bytes"], 23);
    let hash = item["id"].as_str().unwrap().to_owned();
    // OneDrive is not set up in tests: the mirror reports it, the upload still succeeds.
    assert_eq!(body["onedrive"], "OneDrive is not set up on this server");

    let list = read_json(get(&app, &alice_token, "/v1/library").await).await;
    assert_eq!(list["enabled"], true);
    assert_eq!(list["onedrive"]["mirror"], true);
    assert_eq!(list["onedrive"]["connected"], false);
    let items = list["items"].as_array().unwrap();
    assert_eq!(items.len(), 2, "{items:?}");
    let uploaded = items.iter().find(|i| i["kind"] == "uploaded").unwrap();
    assert_eq!(uploaded["id"], hash);
    assert_eq!(uploaded["tool"], "library_upload");
    let generated = items.iter().find(|i| i["kind"] == "generated").unwrap();
    assert_eq!(generated["tool"], "deck_build");
    assert_eq!(generated["conversation_title"], "Main chat");

    // The uploaded artifact is downloadable by its owner through the existing
    // per-user artifact endpoint, and invisible to another member.
    let own = get(&app, &alice_token, &format!("/v1/artifacts/{hash}")).await;
    assert_eq!(own.status(), StatusCode::OK);
    let other = get(&app, &bob_token, &format!("/v1/artifacts/{hash}")).await;
    assert_eq!(other.status(), StatusCode::NOT_FOUND);

    // Bob's library is empty; Alice's items never leak into it.
    let bob_list = read_json(get(&app, &bob_token, "/v1/library").await).await;
    assert_eq!(bob_list["items"].as_array().unwrap().len(), 0);

    // The upload is audited under the acting user.
    let audit = store.list_audit(10).unwrap();
    assert!(
        audit
            .iter()
            .any(|entry| entry.actor == alice.id && entry.event == "library.upload")
    );
}

#[test]
fn library_items_are_scoped_in_the_store() {
    let root = temp_base();
    let (_, _, store) = app_with_root(true, Policy::empty(), root);
    let alice = store
        .create_user("subject-sa", "sa@example.example", "Synthetic A")
        .unwrap();
    let bob = store
        .create_user("subject-sb", "sb@example.example", "Synthetic B")
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
            &"b".repeat(64),
            "deck.pdf",
            None,
        )
        .unwrap();
    store
        .record_conversation_artifact(
            &alice.id,
            crate::library::LIBRARY_SCOPE,
            None,
            crate::library::LIBRARY_UPLOAD_TOOL,
            &"c".repeat(64),
            "notes.md",
            None,
        )
        .unwrap();

    let items = store.list_library_items(&alice.id, 100).unwrap();
    assert_eq!(items.len(), 2);
    let kinds: std::collections::HashSet<&str> =
        items.iter().map(|item| item.kind.as_str()).collect();
    assert!(kinds.contains("generated") && kinds.contains("uploaded"));
    assert!(store.list_library_items(&bob.id, 100).unwrap().is_empty());
}
