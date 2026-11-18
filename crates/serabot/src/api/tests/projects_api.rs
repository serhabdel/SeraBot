//! `/v1/projects` (0.59): per-user project folders, file upload/download/delete,
//! cross-user isolation, and chat-attachment relocation.

use super::*;

const ADMIN_TOKEN: &str = "test-token-that-is-long-enough-123";

#[tokio::test]
async fn projects_crud_and_cross_user_isolation() {
    let (app, _config, store) = app_with_root(true, Policy::empty(), temp_base());
    let bob = store
        .create_user("subject-bob-proj", "bob@example.example", "Synthetic Bob")
        .unwrap();
    let bob_token = mint_for(&store, &bob.id, "bob-proj");

    // Create a project as admin (Alice).
    let res = app
        .clone()
        .oneshot(request_with_token(
            "POST",
            "/v1/projects",
            ADMIN_TOKEN,
            Some(json!({ "name": "Q4 Launch" })),
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let body = read_json(res).await;
    assert_eq!(body["project"], "q4-launch");

    // Upload a file into a subfolder.
    let upload_req = Request::builder()
        .method("POST")
        .uri("/v1/projects/q4-launch/upload")
        .header("authorization", format!("Bearer {ADMIN_TOKEN}"))
        .header("x-filename", "notes.md")
        .header("x-subfolder", "drafts")
        .body(Body::from("# Launch Plan\nReady."))
        .unwrap();
    let res = app.clone().oneshot(upload_req).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let body = read_json(res).await;
    assert_eq!(body["file"]["path"], "drafts/notes.md");

    // Admin sees 1 project with 1 file.
    let res = app
        .clone()
        .oneshot(request_with_token("GET", "/v1/projects", ADMIN_TOKEN, None))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let body = read_json(res).await;
    assert_eq!(body["projects"].as_array().unwrap().len(), 1);
    assert_eq!(body["projects"][0]["file_count"], 1);

    // Download returns the exact uploaded bytes.
    let res = app
        .clone()
        .oneshot(request_with_token(
            "GET",
            "/v1/projects/q4-launch/files/drafts/notes.md",
            ADMIN_TOKEN,
            None,
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(res.into_body(), 1024 * 64)
        .await
        .unwrap();
    assert_eq!(&bytes[..], b"# Launch Plan\nReady.");

    // Cross-user isolation: Bob sees zero projects and cannot read or delete Alice's file.
    let res = app
        .clone()
        .oneshot(request_with_token("GET", "/v1/projects", &bob_token, None))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let body = read_json(res).await;
    assert_eq!(body["projects"].as_array().unwrap().len(), 0);

    let res = app
        .clone()
        .oneshot(request_with_token(
            "GET",
            "/v1/projects/q4-launch/files/drafts/notes.md",
            &bob_token,
            None,
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::NOT_FOUND);

    let res = app
        .clone()
        .oneshot(request_with_token(
            "DELETE",
            "/v1/projects/q4-launch/files/drafts/notes.md",
            &bob_token,
            None,
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::NOT_FOUND);

    // Admin deletes the file.
    let res = app
        .clone()
        .oneshot(request_with_token(
            "DELETE",
            "/v1/projects/q4-launch/files/drafts/notes.md",
            ADMIN_TOKEN,
            None,
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);

    // File is gone.
    let res = app
        .clone()
        .oneshot(request_with_token(
            "GET",
            "/v1/projects/q4-launch/files/drafts/notes.md",
            ADMIN_TOKEN,
            None,
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn project_file_move_updates_chat_attachment_download_path() {
    let (app, config, store) = app_with_root(true, Policy::empty(), temp_base());
    let uid = admin(&store);

    // Upload a chat attachment via the HTTP endpoint.
    let conv_id = "conv-proj-move";
    let upload_req = Request::builder()
        .method("POST")
        .uri(format!("/v1/conversations/{conv_id}/attachments"))
        .header("authorization", format!("Bearer {ADMIN_TOKEN}"))
        .header("x-filename", "brief.txt")
        .body(Body::from("Synthetic brief content"))
        .unwrap();
    let res = app.clone().oneshot(upload_req).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let body = read_json(res).await;
    let att_id = body["attachment"]["id"].as_str().unwrap().to_owned();
    let original_rel = store
        .get_chat_attachment(&uid, &att_id)
        .unwrap()
        .unwrap()
        .rel_path;

    // Move it into a project using ProjectFileTool wired with the store.
    let mut reg = ToolRegistry::default();
    crate::projects::register_with_store(&mut reg, config.clone(), Some(Arc::new(store.clone())))
        .unwrap();
    let tool = reg.get("project_file").unwrap();
    let ctx = crate::gateway::ToolContext {
        user_id: uid.clone(),
        actor: "test".into(),
        bot: "main".into(),
        conversation_id: Some(conv_id.to_owned()),
        task_id: None,
        turn_id: None,
        allowed_tools: Default::default(),
        require_approval: Default::default(),
    };
    let moved = tool
        .execute(
            json!({
                "path": original_rel,
                "project": "client-alpha",
            }),
            &ctx,
        )
        .await
        .unwrap();
    assert!(
        moved.data.unwrap()["path"]
            .as_str()
            .unwrap()
            .contains("/projects/client-alpha/")
    );

    // The attachment row now points into projects/ and GET /v1/attachments/{id} still serves 200.
    let updated = store.get_chat_attachment(&uid, &att_id).unwrap().unwrap();
    assert!(updated.rel_path.contains("/projects/client-alpha/"));

    let res = app
        .clone()
        .oneshot(request_with_token(
            "GET",
            &format!("/v1/attachments/{att_id}"),
            ADMIN_TOKEN,
            None,
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(res.into_body(), 1024 * 64)
        .await
        .unwrap();
    assert_eq!(&bytes[..], b"Synthetic brief content");
}
