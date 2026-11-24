use super::*;
use base64::Engine as _;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;

async fn post_json(app: &Router, token: &str, uri: &str, body: Value) -> Response {
    app.clone()
        .oneshot(request_with_token("POST", uri, token, Some(body)))
        .await
        .unwrap()
}

async fn get(app: &Router, token: &str, uri: &str) -> Response {
    app.clone()
        .oneshot(request_with_token("GET", uri, token, None))
        .await
        .unwrap()
}

#[tokio::test]
async fn mail_send_rejects_foreign_artifact_and_blocklisted_content() {
    let root = temp_base();
    let (app, _, store) =
        app_with_root(true, Policy::from_text("synthetic-forbidden-phrase"), root);
    let alice = store
        .create_user("mail-a", "ma@example.example", "Mail A")
        .unwrap();
    let bob = store
        .create_user("mail-b", "mb@example.example", "Mail B")
        .unwrap();
    let alice_token = mint_for(&store, &alice.id, "mail-a-tok");
    let bob_token = mint_for(&store, &bob.id, "mail-b-tok");

    // Bob uploads a file to his library so he owns its artifact hash.
    let upload_req = Request::builder()
        .method("POST")
        .uri("/v1/library/upload")
        .header("authorization", format!("Bearer {bob_token}"))
        .header("x-filename", "bob-secret.txt")
        .body(Body::from(b"bob private bytes".to_vec()))
        .unwrap();
    let upload_res = app.clone().oneshot(upload_req).await.unwrap();
    assert_eq!(upload_res.status(), StatusCode::OK);
    let uploaded = read_json(upload_res).await;
    let bob_hash = uploaded["item"]["id"].as_str().unwrap().to_owned();

    // Alice tries to attach Bob's artifact hash -> 403 Forbidden.
    let res = post_json(
        &app,
        &alice_token,
        "/v1/mail/send",
        json!({
            "to": ["rcpt@example.com"],
            "subject": "Hello",
            "body": "Body",
            "artifact_hashes": [bob_hash],
        }),
    )
    .await;
    assert_eq!(res.status(), StatusCode::FORBIDDEN);
    let body = read_json(res).await;
    assert_eq!(body["code"], "forbidden");

    // Blocklisted subject is rejected with PolicyViolation (403 / policy_violation).
    let res = post_json(
        &app,
        &alice_token,
        "/v1/mail/send",
        json!({
            "to": ["rcpt@example.com"],
            "subject": "Contains synthetic-forbidden-phrase here",
            "body": "Clean body",
        }),
    )
    .await;
    assert_eq!(res.status(), StatusCode::FORBIDDEN);
    let body = read_json(res).await;
    assert_eq!(body["code"], "policy_violation");

    // Blocklisted body is rejected with PolicyViolation (403 / policy_violation).
    let res = post_json(
        &app,
        &alice_token,
        "/v1/mail/send",
        json!({
            "to": ["rcpt@example.com"],
            "subject": "Clean subject",
            "body": "Body has synthetic-forbidden-phrase inside",
        }),
    )
    .await;
    assert_eq!(res.status(), StatusCode::FORBIDDEN);
    let body = read_json(res).await;
    assert_eq!(body["code"], "policy_violation");
}

#[tokio::test]
async fn mail_flag_rejects_empty_body_and_attachments_error_when_not_connected() {
    let root = temp_base();
    let (app, _, store) = app_with_root(true, Policy::empty(), root);
    let alice = store
        .create_user("mail-c", "mc@example.example", "Mail C")
        .unwrap();
    let alice_token = mint_for(&store, &alice.id, "mail-c-tok");

    // Empty `{}` on flag -> Validation error.
    let res = post_json(&app, &alice_token, "/v1/mail/messages/42/flag", json!({})).await;
    assert_eq!(res.status(), StatusCode::UNPROCESSABLE_ENTITY);
    let body = read_json(res).await;
    assert_eq!(body["code"], "validation");

    // Attachment download when mail is not connected -> Validation error.
    let res = get(&app, &alice_token, "/v1/mail/messages/42/attachments/0").await;
    assert_eq!(res.status(), StatusCode::UNPROCESSABLE_ENTITY);
    let body = read_json(res).await;
    assert_eq!(body["code"], "validation");

    // Save-to-library when mail is not connected -> Validation error.
    let res = post_json(
        &app,
        &alice_token,
        "/v1/mail/messages/42/attachments/0/save-to-library",
        json!({}),
    )
    .await;
    assert_eq!(res.status(), StatusCode::UNPROCESSABLE_ENTITY);
    let body = read_json(res).await;
    assert_eq!(body["code"], "validation");

    // Invalid account name -> Validation error.
    let res = get(
        &app,
        &alice_token,
        "/v1/mail/messages/42/attachments/0?account=Bad.Account!",
    )
    .await;
    assert_eq!(res.status(), StatusCode::UNPROCESSABLE_ENTITY);
    let body = read_json(res).await;
    assert_eq!(body["code"], "validation");
}

#[tokio::test]
async fn mail_attachments_download_and_save_to_library_with_fake_imap() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let imap_port = listener.local_addr().unwrap().port();
    let pdf_bytes = b"%PDF-1.4 synthetic attachment";
    let raw_msg = format!(
        "From: sender@example.com\r\nSubject: With attachment\r\nContent-Type: multipart/mixed; boundary=\"bnd\"\r\n\r\n\
         --bnd\r\nContent-Type: text/plain; charset=utf-8\r\n\r\nSee attached.\r\n\
         --bnd\r\nContent-Type: application/pdf\r\nContent-Disposition: attachment; filename=\"invoice.pdf\"\r\nContent-Transfer-Encoding: base64\r\n\r\n{}\r\n\
         --bnd--\r\n",
        base64::engine::general_purpose::STANDARD.encode(pdf_bytes)
    );

    let server = tokio::spawn(async move {
        // Four connections: 1 for /v1/mail/connect imap_test, plus 3 for download/OOB/save-to-library.
        for _ in 0..4 {
            let (socket, _) = listener.accept().await.unwrap();
            let (reader, mut writer) = socket.into_split();
            let mut lines = BufReader::new(reader).lines();
            writer
                .write_all(b"* OK [CAPABILITY IMAP4rev1 AUTH=LOGIN] ready.\r\n")
                .await
                .unwrap();
            while let Ok(Some(line)) = lines.next_line().await {
                let tag = line.split_whitespace().next().unwrap_or("a1");
                if line.contains(" LOGIN ") {
                    writer
                        .write_all(format!("{tag} OK logged in\r\n").as_bytes())
                        .await
                        .unwrap();
                } else if line.contains(" SELECT ") {
                    writer
                        .write_all(
                            format!("* 1 EXISTS\r\n{tag} OK [READ-WRITE] selected\r\n").as_bytes(),
                        )
                        .await
                        .unwrap();
                } else if line.contains(" UID FETCH ") {
                    writer
                        .write_all(
                            format!(
                                "* 1 FETCH (FLAGS (\\Seen) UID 7 BODY[] {{{}}}\r\n{}\r\n)\r\n{tag} OK done\r\n",
                                raw_msg.len(),
                                raw_msg
                            )
                            .as_bytes(),
                        )
                        .await
                        .unwrap();
                } else if line.contains(" LOGOUT") {
                    writer
                        .write_all(format!("* BYE\r\n{tag} OK bye\r\n").as_bytes())
                        .await
                        .unwrap();
                    break;
                } else {
                    writer
                        .write_all(format!("{tag} OK\r\n").as_bytes())
                        .await
                        .unwrap();
                }
            }
        }
    });

    let root = temp_base();
    let (app, _, store) = app_with_root(true, Policy::empty(), root);
    let alice = store
        .create_user("mail-d", "md@example.example", "Mail D")
        .unwrap();
    let alice_token = mint_for(&store, &alice.id, "mail-d-tok");

    let res = post_json(
        &app,
        &alice_token,
        "/v1/mail/connect",
        json!({
            "smtp_host": "127.0.0.1",
            "smtp_port": 2525,
            "smtp_security": "none",
            "username": "md@example.example",
            "password": "app-password",
            "imap_host": "127.0.0.1",
            "imap_port": imap_port,
            "imap_security": "none"
        }),
    )
    .await;
    assert_eq!(res.status(), StatusCode::OK);

    // 1. Download attachment 0.
    let res = get(&app, &alice_token, "/v1/mail/messages/7/attachments/0").await;
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(res.headers()["x-content-type-options"], "nosniff");
    assert_eq!(
        res.headers()["content-disposition"],
        "attachment; filename=\"invoice.pdf\""
    );
    assert_eq!(res.headers()["content-type"], "application/pdf");
    let body_bytes = axum::body::to_bytes(res.into_body(), 1024 * 1024)
        .await
        .unwrap();
    assert_eq!(body_bytes.as_ref(), pdf_bytes);

    // 2. Out-of-bounds attachment index -> 404.
    let res = get(&app, &alice_token, "/v1/mail/messages/7/attachments/5").await;
    assert_eq!(res.status(), StatusCode::NOT_FOUND);

    // 3. Save attachment 0 to library.
    let res = post_json(
        &app,
        &alice_token,
        "/v1/mail/messages/7/attachments/0/save-to-library",
        json!({}),
    )
    .await;
    assert_eq!(res.status(), StatusCode::OK);
    let saved = read_json(res).await;
    assert_eq!(saved["filename"], "invoice.pdf");
    assert_eq!(saved["size"], pdf_bytes.len());
    assert!(saved["hash"].as_str().unwrap().len() == 64);

    // Verify it appears in the user's Library.
    let lib_res = get(&app, &alice_token, "/v1/library").await;
    assert_eq!(lib_res.status(), StatusCode::OK);
    let lib = read_json(lib_res).await;
    let items = lib["items"].as_array().unwrap();
    assert!(items.iter().any(|it| it["id"] == saved["hash"]));

    server.await.unwrap();
}
