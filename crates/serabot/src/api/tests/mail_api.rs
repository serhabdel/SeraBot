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

#[tokio::test]
async fn mail_signature_update_and_scheduled_mail_crud_and_delivery() {
    let smtp_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let smtp_port = smtp_listener.local_addr().unwrap().port();
    let imap_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let imap_port = imap_listener.local_addr().unwrap().port();

    let smtp_server = tokio::spawn(async move {
        let mut received = Vec::new();
        // 1 for connect smtp_test, 1 for scheduled delivery
        for _ in 0..2 {
            let (socket, _) = smtp_listener.accept().await.unwrap();
            let (reader, mut writer) = socket.into_split();
            let mut lines = BufReader::new(reader).lines();
            let mut data = String::new();
            writer.write_all(b"220 fake ESMTP\r\n").await.unwrap();
            while let Ok(Some(line)) = lines.next_line().await {
                if line.starts_with("EHLO") {
                    writer
                        .write_all(b"250-fake\r\n250 AUTH LOGIN\r\n")
                        .await
                        .unwrap();
                } else if line == "AUTH LOGIN" {
                    writer.write_all(b"334 VXNlcm5hbWU6\r\n").await.unwrap();
                    let _ = lines.next_line().await;
                    writer.write_all(b"334 UGFzc3dvcmQ6\r\n").await.unwrap();
                    let _ = lines.next_line().await;
                    writer.write_all(b"235 ok\r\n").await.unwrap();
                } else if line.starts_with("MAIL FROM") || line.starts_with("RCPT TO") {
                    writer.write_all(b"250 ok\r\n").await.unwrap();
                } else if line == "DATA" {
                    writer.write_all(b"354 go\r\n").await.unwrap();
                    loop {
                        let l = lines.next_line().await.unwrap().unwrap();
                        if l == "." {
                            break;
                        }
                        data.push_str(&l);
                        data.push('\n');
                    }
                    writer.write_all(b"250 queued\r\n").await.unwrap();
                } else if line == "QUIT" {
                    writer.write_all(b"221 bye\r\n").await.unwrap();
                    break;
                } else {
                    writer.write_all(b"250 ok\r\n").await.unwrap();
                }
            }
            if !data.is_empty() {
                received.push(data);
            }
        }
        received
    });

    let imap_server = tokio::spawn(async move {
        let (socket, _) = imap_listener.accept().await.unwrap();
        let (reader, mut writer) = socket.into_split();
        let mut lines = BufReader::new(reader).lines();
        writer
            .write_all(b"* OK [CAPABILITY IMAP4rev1 AUTH=LOGIN] ready.\r\n")
            .await
            .unwrap();
        while let Ok(Some(line)) = lines.next_line().await {
            let tag = line.split_whitespace().next().unwrap_or("a1");
            if line.contains(" LOGOUT") {
                writer
                    .write_all(format!("* BYE\r\n{tag} OK bye\r\n").as_bytes())
                    .await
                    .unwrap();
                break;
            }
            writer
                .write_all(format!("{tag} OK\r\n").as_bytes())
                .await
                .unwrap();
        }
    });

    let root = temp_base();
    let (app, _state, store) = app_with_root(
        true,
        Policy::from_text("synthetic-forbidden-phrase"),
        root.clone(),
    );
    let alice = store
        .create_user("sched-a", "sa@example.example", "Sched A")
        .unwrap();
    let bob = store
        .create_user("sched-b", "sb@example.example", "Sched B")
        .unwrap();
    let alice_token = mint_for(&store, &alice.id, "sched-a-tok");
    let bob_token = mint_for(&store, &bob.id, "sched-b-tok");

    // Connect Alice's mailbox.
    let res = post_json(
        &app,
        &alice_token,
        "/v1/mail/connect",
        json!({
            "smtp_host": "127.0.0.1",
            "smtp_port": smtp_port,
            "smtp_security": "none",
            "username": "sa@example.example",
            "password": "app-password",
            "imap_host": "127.0.0.1",
            "imap_port": imap_port,
            "imap_security": "none"
        }),
    )
    .await;
    assert_eq!(res.status(), StatusCode::OK);

    // Upload a PNG logo for Alice.
    let upload_req = Request::builder()
        .method("POST")
        .uri("/v1/library/upload")
        .header("authorization", format!("Bearer {alice_token}"))
        .header("x-filename", "logo.png")
        .body(Body::from(b"\x89PNG\r\n\x1a\nfake-logo".to_vec()))
        .unwrap();
    let upload_res = app.clone().oneshot(upload_req).await.unwrap();
    assert_eq!(upload_res.status(), StatusCode::OK);
    let logo_hash = read_json(upload_res).await["item"]["id"]
        .as_str()
        .unwrap()
        .to_owned();

    // Update signature without password re-entry via PUT /v1/mail/signature.
    let sig_res = app
        .clone()
        .oneshot(request_with_token(
            "PUT",
            "/v1/mail/signature",
            &alice_token,
            Some(json!({
                "signature": "Alice",
                "signature_html": "<b>Alice</b>",
                "signature_logo_hash": logo_hash,
            })),
        ))
        .await
        .unwrap();
    assert_eq!(sig_res.status(), StatusCode::OK);

    let status_res = get(&app, &alice_token, "/v1/mail").await;
    assert_eq!(status_res.status(), StatusCode::OK);
    let status = read_json(status_res).await;
    assert_eq!(status["signature"], "Alice");
    assert_eq!(status["signature_html"], "<b>Alice</b>");
    assert_eq!(status["signature_logo_hash"], logo_hash);

    // Blocklisted scheduled email is rejected at schedule time.
    let bad_sched = post_json(
        &app,
        &alice_token,
        "/v1/mail/schedule",
        json!({
            "to": ["rcpt@example.com"],
            "subject": "Bad synthetic-forbidden-phrase",
            "body": "Clean",
            "send_at": 1_800_000_000i64,
        }),
    )
    .await;
    assert_eq!(bad_sched.status(), StatusCode::FORBIDDEN);

    // Schedule two valid emails (one due in the past, one to cancel).
    let s1 = post_json(
        &app,
        &alice_token,
        "/v1/mail/schedule",
        json!({
            "to": ["rcpt@example.com"],
            "cc": ["cc@example.com"],
            "subject": "Scheduled hello",
            "body": "Plain scheduled",
            "body_html": "<p>HTML scheduled</p>",
            "send_at": 1_700_000_000i64,
        }),
    )
    .await;
    assert_eq!(s1.status(), StatusCode::OK);
    let id1 = read_json(s1).await["item"]["id"]
        .as_str()
        .unwrap()
        .to_owned();

    let s2 = post_json(
        &app,
        &alice_token,
        "/v1/mail/schedule",
        json!({
            "to": ["rcpt@example.com"],
            "subject": "To cancel",
            "body": "Cancel me",
            "send_at": 1_900_000_000i64,
        }),
    )
    .await;
    assert_eq!(s2.status(), StatusCode::OK);
    let id2 = read_json(s2).await["item"]["id"]
        .as_str()
        .unwrap()
        .to_owned();

    // Bob cannot see, patch, or delete Alice's scheduled emails.
    let bob_list = get(&app, &bob_token, "/v1/mail/scheduled").await;
    assert_eq!(bob_list.status(), StatusCode::OK);
    assert!(
        read_json(bob_list).await["items"]
            .as_array()
            .unwrap()
            .is_empty()
    );

    let bob_del = app
        .clone()
        .oneshot(request_with_token(
            "DELETE",
            &format!("/v1/mail/scheduled/{id1}"),
            &bob_token,
            None,
        ))
        .await
        .unwrap();
    assert_eq!(bob_del.status(), StatusCode::NOT_FOUND);

    // Alice patches id1 and deletes id2.
    let patch_res = app
        .clone()
        .oneshot(request_with_token(
            "PATCH",
            &format!("/v1/mail/scheduled/{id1}"),
            &alice_token,
            Some(json!({ "subject": "Scheduled updated" })),
        ))
        .await
        .unwrap();
    assert_eq!(patch_res.status(), StatusCode::OK);
    assert_eq!(
        read_json(patch_res).await["item"]["subject"],
        "Scheduled updated"
    );

    let del_res = app
        .clone()
        .oneshot(request_with_token(
            "DELETE",
            &format!("/v1/mail/scheduled/{id2}"),
            &alice_token,
            None,
        ))
        .await
        .unwrap();
    assert_eq!(del_res.status(), StatusCode::OK);

    // Run scheduler delivery for due emails at now_unix = 1_750_000_000.
    let users_secrets =
        crate::user_secrets::UserSecrets::with_master_key(store.clone(), [9u8; 32]).unwrap();
    let artifacts = crate::maker::ArtifactStore::new(root.join("sched-artifacts")).unwrap();
    let logo_meta = crate::maker::ArtifactMeta {
        title: "logo.png".into(),
        filename: "logo.png".into(),
        language: crate::domain::Language::En,
        source_artifact: None,
        created_at: chrono::Utc::now(),
    };
    let saved_logo = artifacts
        .save(b"\x89PNG\r\n\x1a\nfake-logo", "logo.png", logo_meta)
        .unwrap();
    assert_eq!(saved_logo.hash, logo_hash);
    let policy = Policy::from_text("synthetic-forbidden-phrase");
    let processed = crate::scheduler::process_due_scheduled_mail(
        &store,
        &users_secrets,
        &artifacts,
        &policy,
        1_750_000_000,
    )
    .await
    .unwrap();
    assert_eq!(processed, 1);

    let alice_list = get(&app, &alice_token, "/v1/mail/scheduled").await;
    let items = read_json(alice_list).await["items"]
        .as_array()
        .unwrap()
        .clone();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0]["status"], "sent");

    let sent_payloads = smtp_server.await.unwrap();
    assert_eq!(sent_payloads.len(), 1);
    assert!(sent_payloads[0].contains("Subject: Scheduled updated"));
    assert!(sent_payloads[0].contains("Content-ID: <signature-logo@serabot>"));
    imap_server.await.unwrap();
}

// ---------------------------------------------------------------------------
// 0.74.14 mail workspace: threading, snooze, contacts, saved searches, search
// operators, drafts, undo hold.
// ---------------------------------------------------------------------------

fn ux_raw_message(uid: u32) -> String {
    match uid {
        1 => "From: Anna <anna@example.example>\r\nTo: me@example.example\r\nSubject: Plan\r\nMessage-ID: <a@x>\r\nDate: Mon, 23 Nov 2026 09:00:00 +0000\r\n\r\nLet us plan the launch.\r\n".to_owned(),
        2 => "From: Bob <bob@example.example>\r\nTo: me@example.example\r\nSubject: Re: Plan\r\nMessage-ID: <b@x>\r\nIn-Reply-To: <a@x>\r\nReferences: <a@x>\r\nList-Unsubscribe: <mailto:unsub@news.example?subject=unsubscribe>, <https://news.example/u?id=1>\r\nList-Unsubscribe-Post: List-Unsubscribe=One-Click\r\nDate: Mon, 23 Nov 2026 10:00:00 +0000\r\n\r\nSounds good.\r\n".to_owned(),
        _ => "From: Carol <carol@example.example>\r\nSubject: Lunch\r\nMessage-ID: <c@x>\r\nDate: Mon, 23 Nov 2026 11:00:00 +0000\r\n\r\nNoon?\r\n".to_owned(),
    }
}

/// A fake IMAP server that answers every connection until the test aborts it.
/// Every command line (and the lines of an APPEND literal) is appended to `log`.
async fn ux_fake_imap(listener: TcpListener, log: std::sync::Arc<std::sync::Mutex<Vec<String>>>) {
    loop {
        let (socket, _) = listener.accept().await.unwrap();
        let (reader, mut writer) = socket.into_split();
        let mut lines = BufReader::new(reader).lines();
        writer
            .write_all(b"* OK [CAPABILITY IMAP4rev1 AUTH=LOGIN] ready.\r\n")
            .await
            .unwrap();
        while let Ok(Some(line)) = lines.next_line().await {
            let tag = line.split_whitespace().next().unwrap_or("a1").to_owned();
            log.lock().unwrap().push(line.clone());
            if line.contains(" LOGIN ") {
                writer
                    .write_all(format!("{tag} OK logged in\r\n").as_bytes())
                    .await
                    .unwrap();
            } else if line.contains(" LOGOUT") {
                writer
                    .write_all(format!("* BYE\r\n{tag} OK bye\r\n").as_bytes())
                    .await
                    .unwrap();
                break;
            } else if line.contains(" LIST ") {
                writer
                    .write_all(
                        format!(
                            "* LIST (\\HasNoChildren) \".\" INBOX\r\n* LIST (\\HasNoChildren \\Drafts) \".\" INBOX.Drafts\r\n{tag} OK done\r\n"
                        )
                        .as_bytes(),
                    )
                    .await
                    .unwrap();
            } else if line.contains(" SELECT ") {
                writer
                    .write_all(
                        format!("* 3 EXISTS\r\n{tag} OK [READ-WRITE] selected\r\n").as_bytes(),
                    )
                    .await
                    .unwrap();
            } else if line.contains(" UID SEARCH ") {
                writer
                    .write_all(format!("* SEARCH 1 2 3\r\n{tag} OK done\r\n").as_bytes())
                    .await
                    .unwrap();
            } else if line.contains(" UID FETCH ") {
                // One command may name several UIDs ("3,2,1"); answer each in turn.
                let set = line.split_whitespace().nth(3).unwrap_or("1");
                let mut text = String::new();
                for uid in set.split(',').filter_map(|w| w.parse::<u32>().ok()) {
                    let raw = ux_raw_message(uid);
                    text.push_str(&format!(
                        "* {uid} FETCH (FLAGS (\\Seen) UID {uid} BODY[] {{{}}}\r\n{raw})\r\n",
                        raw.len()
                    ));
                }
                text.push_str(&format!("{tag} OK done\r\n"));
                writer.write_all(text.as_bytes()).await.unwrap();
            } else if line.contains(" APPEND ") {
                writer.write_all(b"+ go ahead\r\n").await.unwrap();
                // Collect the literal until the client goes quiet, then confirm.
                while let Ok(Ok(Some(l))) =
                    tokio::time::timeout(std::time::Duration::from_millis(400), lines.next_line())
                        .await
                {
                    log.lock().unwrap().push(format!("APPEND_BODY {l}"));
                }
                writer
                    .write_all(format!("{tag} OK [APPENDUID 1 99] done\r\n").as_bytes())
                    .await
                    .unwrap();
            } else {
                writer
                    .write_all(format!("{tag} OK\r\n").as_bytes())
                    .await
                    .unwrap();
            }
        }
    }
}

async fn delete_req(app: &Router, token: &str, uri: &str) -> Response {
    app.clone()
        .oneshot(request_with_token("DELETE", uri, token, None))
        .await
        .unwrap()
}

#[tokio::test]
async fn mail_workspace_threads_snooze_contacts_searches_drafts_and_undo() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let imap_port = listener.local_addr().unwrap().port();
    let log = std::sync::Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
    let server = tokio::spawn(ux_fake_imap(listener, log.clone()));

    let root = temp_base();
    let (app, _, store) =
        app_with_root(true, Policy::from_text("synthetic-forbidden-phrase"), root);
    let alice = store
        .create_user("ux-a", "ua@example.example", "UX A")
        .unwrap();
    let bob = store
        .create_user("ux-b", "ub@example.example", "UX B")
        .unwrap();
    let alice_token = mint_for(&store, &alice.id, "ux-a-tok");
    let bob_token = mint_for(&store, &bob.id, "ux-b-tok");

    // Store Alice's mailbox directly (the connect flow is covered elsewhere).
    let secrets =
        crate::user_secrets::UserSecrets::with_master_key(store.clone(), [9u8; 32]).unwrap();
    crate::mail::store_credentials(
        &secrets,
        &alice.id,
        None,
        &crate::mail::MailCredentials {
            smtp_host: "127.0.0.1".into(),
            smtp_port: 2525,
            smtp_security: crate::mail::Security::None,
            username: "ua@example.example".into(),
            password: "app-password".into(),
            imap_host: "127.0.0.1".into(),
            imap_port,
            imap_security: crate::mail::Security::None,
            from: None,
            signature: None,
        },
    )
    .unwrap();

    // Threading, snippet, address and unsubscribe fields on the list.
    let res = get(
        &app,
        &alice_token,
        "/v1/mail/messages?folder=INBOX&limit=10",
    )
    .await;
    assert_eq!(res.status(), StatusCode::OK);
    let listed = read_json(res).await;
    let messages = listed["messages"].as_array().unwrap();
    assert_eq!(messages.len(), 3);
    let by_uid = |uid: u64| {
        messages
            .iter()
            .find(|m| m["uid"].as_u64() == Some(uid))
            .unwrap()
            .clone()
    };
    let (m1, m2, m3) = (by_uid(1), by_uid(2), by_uid(3));
    assert_eq!(m1["thread_key"], m2["thread_key"]);
    assert_ne!(m1["thread_key"], m3["thread_key"]);
    assert_eq!(m1["snippet"], "Let us plan the launch.");
    assert_eq!(m2["in_reply_to"], "<a@x>");
    assert_eq!(
        m2["unsubscribe"]["mailto"],
        "unsub@news.example?subject=unsubscribe"
    );
    assert_eq!(m2["unsubscribe"]["url"], "https://news.example/u?id=1");
    assert_eq!(m2["unsubscribe"]["one_click"], true);
    assert!(m1["unsubscribe"].is_null());
    assert_eq!(m1["to"], "me@example.example");

    // Senders of a listing become contacts.
    let res = get(&app, &alice_token, "/v1/mail/contacts?q=ANNA").await;
    let contacts = read_json(res).await;
    assert_eq!(contacts["items"][0]["email"], "anna@example.example");
    assert_eq!(contacts["items"][0]["name"], "Anna");
    let res = get(&app, &bob_token, "/v1/mail/contacts?q=anna").await;
    assert!(read_json(res).await["items"].as_array().unwrap().is_empty());

    // Snooze hides one message, unsnooze brings it back, and Bob sees nothing.
    let past = post_json(
        &app,
        &alice_token,
        "/v1/mail/messages/3/snooze",
        json!({ "folder": "INBOX", "wake_at": 5 }),
    )
    .await;
    assert_eq!(past.status(), StatusCode::UNPROCESSABLE_ENTITY);
    let wake = chrono::Utc::now().timestamp() + 3600;
    let res = post_json(
        &app,
        &alice_token,
        "/v1/mail/messages/3/snooze",
        json!({ "folder": "INBOX", "wake_at": wake, "subject": "Lunch", "from": "carol@example.example" }),
    )
    .await;
    assert_eq!(res.status(), StatusCode::OK);
    let res = get(
        &app,
        &alice_token,
        "/v1/mail/messages?folder=INBOX&limit=10",
    )
    .await;
    let after = read_json(res).await;
    let uids: Vec<u64> = after["messages"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["uid"].as_u64().unwrap())
        .collect();
    assert!(!uids.contains(&3), "{uids:?}");
    assert_eq!(uids.len(), 2);
    let res = get(&app, &alice_token, "/v1/mail/snoozed").await;
    assert_eq!(read_json(res).await["items"].as_array().unwrap().len(), 1);
    let res = get(&app, &bob_token, "/v1/mail/snoozed").await;
    assert!(read_json(res).await["items"].as_array().unwrap().is_empty());
    let res = delete_req(
        &app,
        &alice_token,
        "/v1/mail/messages/3/snooze?folder=INBOX",
    )
    .await;
    assert_eq!(read_json(res).await["removed"], true);
    let res = get(
        &app,
        &alice_token,
        "/v1/mail/messages?folder=INBOX&limit=10",
    )
    .await;
    assert_eq!(
        read_json(res).await["messages"].as_array().unwrap().len(),
        3
    );

    // Search operators become IMAP criteria.
    let res = get(
        &app,
        &alice_token,
        "/v1/mail/search?folder=INBOX&q=from%3Aanna%20is%3Aunread",
    )
    .await;
    assert_eq!(res.status(), StatusCode::OK);
    assert!(
        log.lock()
            .unwrap()
            .iter()
            .any(|l| l.contains("UID SEARCH FROM \"anna\" UNSEEN")),
        "{:?}",
        log.lock().unwrap()
    );
    let res = get(
        &app,
        &alice_token,
        "/v1/mail/search?folder=INBOX&q=is%3Aweird",
    )
    .await;
    assert_eq!(res.status(), StatusCode::UNPROCESSABLE_ENTITY);

    // Saved searches: validate, list, cap scope, delete.
    let res = post_json(
        &app,
        &alice_token,
        "/v1/mail/saved-searches",
        json!({ "name": "From Anna", "query": "from:anna is:unread" }),
    )
    .await;
    assert_eq!(res.status(), StatusCode::OK);
    let saved_id = read_json(res).await["item"]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let res = post_json(
        &app,
        &alice_token,
        "/v1/mail/saved-searches",
        json!({ "name": "Bad", "query": "is:weird" }),
    )
    .await;
    assert_eq!(res.status(), StatusCode::UNPROCESSABLE_ENTITY);
    let res = get(&app, &alice_token, "/v1/mail/saved-searches").await;
    assert_eq!(read_json(res).await["items"].as_array().unwrap().len(), 1);
    let res = get(&app, &bob_token, "/v1/mail/saved-searches").await;
    assert!(read_json(res).await["items"].as_array().unwrap().is_empty());
    let res = delete_req(
        &app,
        &bob_token,
        &format!("/v1/mail/saved-searches/{saved_id}"),
    )
    .await;
    assert_eq!(res.status(), StatusCode::NOT_FOUND);
    let res = delete_req(
        &app,
        &alice_token,
        &format!("/v1/mail/saved-searches/{saved_id}"),
    )
    .await;
    assert_eq!(res.status(), StatusCode::OK);

    // Drafts: half-typed recipients are dropped, the Drafts folder is found, and the
    // previous autosave is deleted once the new copy is stored.
    let res = post_json(
        &app,
        &alice_token,
        "/v1/mail/drafts",
        json!({
            "to": ["bob@example.example", "half"],
            "subject": "Draft one",
            "body": "Hello draft",
            "replace_uid": 5
        }),
    )
    .await;
    assert_eq!(res.status(), StatusCode::OK);
    let saved = read_json(res).await;
    assert_eq!(saved["folder"], "INBOX.Drafts");
    assert_eq!(saved["uid"], 99);
    {
        let log = log.lock().unwrap();
        assert!(
            log.iter()
                .any(|l| l.contains("APPEND \"INBOX.Drafts\" (\\Draft \\Seen) {")),
            "{log:?}"
        );
        assert!(
            log.iter()
                .any(|l| l == "APPEND_BODY To: bob@example.example"),
            "{log:?}"
        );
        assert!(
            log.iter().any(|l| l == "APPEND_BODY Subject: Draft one"),
            "{log:?}"
        );
        assert!(!log.iter().any(|l| l.contains("half")), "{log:?}");
        assert!(
            log.iter()
                .any(|l| l.contains("UID STORE 5 +FLAGS.SILENT (\\Deleted)")),
            "{log:?}"
        );
        assert!(log.iter().any(|l| l.contains("UID EXPUNGE 5")), "{log:?}");
    }
    let res = post_json(
        &app,
        &alice_token,
        "/v1/mail/drafts",
        json!({ "subject": "Blocked synthetic-forbidden-phrase", "body": "x" }),
    )
    .await;
    assert_eq!(res.status(), StatusCode::FORBIDDEN);

    // Undo send: a short hold is accepted, hidden from the Scheduled list, and
    // anything outside the window or of an unknown kind is refused.
    let soon = chrono::Utc::now().timestamp() + 10;
    let res = post_json(
        &app,
        &alice_token,
        "/v1/mail/schedule",
        json!({
            "to": ["bob@example.example"], "subject": "Undo me", "body": "b",
            "send_at": soon, "kind": "undo"
        }),
    )
    .await;
    assert_eq!(res.status(), StatusCode::OK);
    let res = get(&app, &alice_token, "/v1/mail/scheduled").await;
    assert!(read_json(res).await["items"].as_array().unwrap().is_empty());
    let far = chrono::Utc::now().timestamp() + 3000;
    let res = post_json(
        &app,
        &alice_token,
        "/v1/mail/schedule",
        json!({
            "to": ["bob@example.example"], "subject": "Too long", "body": "b",
            "send_at": far, "kind": "undo"
        }),
    )
    .await;
    assert_eq!(res.status(), StatusCode::UNPROCESSABLE_ENTITY);
    let res = post_json(
        &app,
        &alice_token,
        "/v1/mail/schedule",
        json!({
            "to": ["bob@example.example"], "subject": "Odd", "body": "b",
            "send_at": far, "kind": "weird"
        }),
    )
    .await;
    assert_eq!(res.status(), StatusCode::UNPROCESSABLE_ENTITY);
    // Recipients of scheduled mail rank as contacts.
    let res = get(&app, &alice_token, "/v1/mail/contacts?q=bob").await;
    assert_eq!(
        read_json(res).await["items"][0]["email"],
        "bob@example.example"
    );

    server.abort();
}

#[tokio::test]
async fn scheduled_and_undo_mail_keep_owned_attachments_and_refuse_foreign_ones() {
    let root = temp_base();
    let (app, _, store) =
        app_with_root(true, Policy::from_text("synthetic-forbidden-phrase"), root);
    let alice = store
        .create_user("att-a", "aa@example.example", "Att A")
        .unwrap();
    let bob = store
        .create_user("att-b", "ab@example.example", "Att B")
        .unwrap();
    let alice_token = mint_for(&store, &alice.id, "att-a-tok");
    let bob_token = mint_for(&store, &bob.id, "att-b-tok");

    let secrets =
        crate::user_secrets::UserSecrets::with_master_key(store.clone(), [9u8; 32]).unwrap();
    crate::mail::store_credentials(
        &secrets,
        &alice.id,
        None,
        &crate::mail::MailCredentials {
            smtp_host: "127.0.0.1".into(),
            smtp_port: 2525,
            smtp_security: crate::mail::Security::None,
            username: "aa@example.example".into(),
            password: "app-password".into(),
            imap_host: "127.0.0.1".into(),
            imap_port: 1,
            imap_security: crate::mail::Security::None,
            from: None,
            signature: None,
        },
    )
    .unwrap();

    async fn upload(app: &Router, token: &str, name: &str, bytes: &[u8]) -> String {
        let req = Request::builder()
            .method("POST")
            .uri("/v1/library/upload")
            .header("authorization", format!("Bearer {token}"))
            .header("x-filename", name)
            .body(Body::from(bytes.to_vec()))
            .unwrap();
        let res = app.clone().oneshot(req).await.unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        read_json(res).await["item"]["id"]
            .as_str()
            .unwrap()
            .to_owned()
    }
    let alice_hash = upload(&app, &alice_token, "mine.txt", b"alice file").await;
    let bob_hash = upload(&app, &bob_token, "his.txt", b"bob file").await;

    let later = chrono::Utc::now().timestamp() + 3600;
    let body = |hashes: Vec<String>, kind: Option<&str>, at: i64| {
        let mut v = json!({
            "to": ["rcpt@example.com"], "subject": "With files", "body": "b",
            "send_at": at, "attachment_hashes": hashes
        });
        if let Some(kind) = kind {
            v["kind"] = json!(kind);
        }
        v
    };
    let res = post_json(
        &app,
        &alice_token,
        "/v1/mail/schedule",
        body(vec![bob_hash.clone()], None, later),
    )
    .await;
    assert_eq!(res.status(), StatusCode::FORBIDDEN, "another member's file");
    let res = post_json(
        &app,
        &alice_token,
        "/v1/mail/schedule",
        body(vec!["x".to_owned(); 11], None, later),
    )
    .await;
    assert_eq!(
        res.status(),
        StatusCode::UNPROCESSABLE_ENTITY,
        "too many files"
    );

    let res = post_json(
        &app,
        &alice_token,
        "/v1/mail/schedule",
        body(vec![alice_hash.clone()], None, later),
    )
    .await;
    assert_eq!(res.status(), StatusCode::OK);
    let id = read_json(res).await["item"]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    assert_eq!(
        store.scheduled_mail_attachments(&id).unwrap(),
        vec![alice_hash.clone()]
    );

    // The undo hold carries files too.
    let soon = chrono::Utc::now().timestamp() + 10;
    let res = post_json(
        &app,
        &alice_token,
        "/v1/mail/schedule",
        body(vec![alice_hash.clone()], Some("undo"), soon),
    )
    .await;
    assert_eq!(res.status(), StatusCode::OK);

    let res = delete_req(&app, &alice_token, &format!("/v1/mail/scheduled/{id}")).await;
    assert_eq!(res.status(), StatusCode::OK);
    assert!(store.scheduled_mail_attachments(&id).unwrap().is_empty());
}

#[tokio::test]
async fn mail_connect_password_optional_updates_and_multi_account_metadata() {
    let root = temp_base();
    let (app, _, store) = app_with_root(true, Policy::empty(), root);
    let alice = store
        .create_user("multi-a", "ma@example.example", "Multi A")
        .unwrap();
    let alice_token = mint_for(&store, &alice.id, "multi-a-tok");

    // 1. Connecting a brand-new mailbox without a password is rejected.
    let missing_pw = post_json(
        &app,
        &alice_token,
        "/v1/mail/connect",
        json!({
            "smtp_host": "127.0.0.1",
            "smtp_port": 2525,
            "smtp_security": "none",
            "username": "ma@example.example",
            "password": "",
            "imap_host": "127.0.0.1",
            "imap_port": 2143,
            "imap_security": "none"
        }),
    )
    .await;
    assert_eq!(missing_pw.status(), StatusCode::UNPROCESSABLE_ENTITY);
    let err_body = read_json(missing_pw).await;
    assert_eq!(err_body["code"], "validation");

    // Connect the default mailbox with an initial password.
    let initial = post_json(
        &app,
        &alice_token,
        "/v1/mail/connect",
        json!({
            "smtp_host": "127.0.0.1",
            "smtp_port": 2525,
            "smtp_security": "none",
            "username": "ma@example.example",
            "password": "initial-secret-password",
            "imap_host": "127.0.0.1",
            "imap_port": 2143,
            "imap_security": "none",
            "from": "Alice <ma@example.example>"
        }),
    )
    .await;
    assert_eq!(initial.status(), StatusCode::OK);

    // Save an HTML signature + logo via PUT /v1/mail/signature.
    let upload_req = Request::builder()
        .method("POST")
        .uri("/v1/library/upload")
        .header("authorization", format!("Bearer {alice_token}"))
        .header("x-filename", "sig-logo.png")
        .body(Body::from(b"\x89PNG\r\n\x1a\nsig-logo".to_vec()))
        .unwrap();
    let upload_res = app.clone().oneshot(upload_req).await.unwrap();
    assert_eq!(upload_res.status(), StatusCode::OK);
    let logo_hash = read_json(upload_res).await["item"]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let sig_res = app
        .clone()
        .oneshot(request_with_token(
            "PUT",
            "/v1/mail/signature",
            &alice_token,
            Some(json!({
                "signature": "Alice Default",
                "signature_html": "<b>Alice Default</b>",
                "signature_logo_hash": logo_hash,
            })),
        ))
        .await
        .unwrap();
    assert_eq!(sig_res.status(), StatusCode::OK);

    // 2. Updating the existing default mailbox with an empty password (and omitting signature
    //    fields) succeeds, preserves the sealed password, and preserves the HTML signature + logo.
    let update_no_sig = post_json(
        &app,
        &alice_token,
        "/v1/mail/connect",
        json!({
            "smtp_host": "127.0.0.1",
            "smtp_port": 2526,
            "smtp_security": "none",
            "username": "ma@example.example",
            "password": "",
            "imap_host": "127.0.0.1",
            "imap_port": 2144,
            "imap_security": "none",
            "from": "Alice Updated <ma@example.example>"
        }),
    )
    .await;
    assert_eq!(update_no_sig.status(), StatusCode::OK);

    let secrets =
        crate::user_secrets::UserSecrets::with_master_key(store.clone(), [9u8; 32]).unwrap();
    let saved_default = crate::mail::load_credentials(&secrets, &alice.id, None)
        .unwrap()
        .expect("default creds exist");
    assert_eq!(saved_default.password, "initial-secret-password");
    assert_eq!(saved_default.smtp_port, 2526);
    assert_eq!(
        saved_default.from.as_deref(),
        Some("Alice Updated <ma@example.example>")
    );

    let status_after_update = read_json(get(&app, &alice_token, "/v1/mail").await).await;
    assert_eq!(status_after_update["signature"], "Alice Default");
    assert_eq!(
        status_after_update["signature_html"],
        "<b>Alice Default</b>"
    );
    assert_eq!(status_after_update["signature_logo_hash"], logo_hash);

    // Updating plain signature with empty password also succeeds and keeps the sealed password.
    let update_with_sig = post_json(
        &app,
        &alice_token,
        "/v1/mail/connect",
        json!({
            "smtp_host": "127.0.0.1",
            "smtp_port": 2526,
            "smtp_security": "none",
            "username": "ma@example.example",
            "imap_host": "127.0.0.1",
            "imap_port": 2144,
            "imap_security": "none",
            "from": "Alice Updated <ma@example.example>",
            "signature": "Alice Plain Updated"
        }),
    )
    .await;
    assert_eq!(update_with_sig.status(), StatusCode::OK);
    let saved_default = crate::mail::load_credentials(&secrets, &alice.id, None)
        .unwrap()
        .expect("default creds exist");
    assert_eq!(saved_default.password, "initial-secret-password");

    // 3. Adding a second named mailbox ("work") alongside default works, and both appear in
    //    GET /v1/mail with full non-secret metadata (and never a password field).
    let work_missing_pw = post_json(
        &app,
        &alice_token,
        "/v1/mail/connect",
        json!({
            "account": "work",
            "smtp_host": "127.0.0.1",
            "smtp_port": 2587,
            "smtp_security": "none",
            "username": "work@example.example",
            "imap_host": "127.0.0.1",
            "imap_port": 2993,
            "imap_security": "none"
        }),
    )
    .await;
    assert_eq!(work_missing_pw.status(), StatusCode::UNPROCESSABLE_ENTITY);

    let work_ok = post_json(
        &app,
        &alice_token,
        "/v1/mail/connect",
        json!({
            "account": "work",
            "smtp_host": "127.0.0.1",
            "smtp_port": 2587,
            "smtp_security": "none",
            "username": "work@example.example",
            "password": "work-secret-password",
            "imap_host": "127.0.0.1",
            "imap_port": 2993,
            "imap_security": "none",
            "from": "Work <work@example.example>",
            "signature": "Work Sig"
        }),
    )
    .await;
    assert_eq!(work_ok.status(), StatusCode::OK);

    let status_res = get(&app, &alice_token, "/v1/mail").await;
    assert_eq!(status_res.status(), StatusCode::OK);
    let status = read_json(status_res).await;
    assert_eq!(status["connected"], true);
    let accounts = status["accounts"].as_array().expect("accounts array");
    assert_eq!(accounts.len(), 2);

    let def_acct = accounts
        .iter()
        .find(|a| a["account"] == "")
        .expect("default account");
    assert_eq!(def_acct["username"], "ma@example.example");
    assert_eq!(def_acct["smtp_host"], "127.0.0.1");
    assert_eq!(def_acct["smtp_port"], 2526);
    assert_eq!(def_acct["smtp_security"], "none");
    assert_eq!(def_acct["imap_host"], "127.0.0.1");
    assert_eq!(def_acct["imap_port"], 2144);
    assert_eq!(def_acct["imap_security"], "none");
    assert_eq!(def_acct["from"], "Alice Updated <ma@example.example>");
    assert_eq!(def_acct["signature"], "Alice Plain Updated");
    assert_eq!(def_acct["signature_html"], "<b>Alice Default</b>");
    assert_eq!(def_acct["signature_logo_hash"], logo_hash);
    assert!(def_acct.get("password").is_none());

    let work_acct = accounts
        .iter()
        .find(|a| a["account"] == "work")
        .expect("work account");
    assert_eq!(work_acct["username"], "work@example.example");
    assert_eq!(work_acct["smtp_host"], "127.0.0.1");
    assert_eq!(work_acct["smtp_port"], 2587);
    assert_eq!(work_acct["smtp_security"], "none");
    assert_eq!(work_acct["imap_host"], "127.0.0.1");
    assert_eq!(work_acct["imap_port"], 2993);
    assert_eq!(work_acct["imap_security"], "none");
    assert_eq!(work_acct["from"], "Work <work@example.example>");
    assert_eq!(work_acct["signature"], "Work Sig");
    assert!(work_acct.get("password").is_none());
}

#[tokio::test]
async fn mail_test_endpoint_checks_saved_default_and_named_accounts() {
    let smtp_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let smtp_port = smtp_listener.local_addr().unwrap().port();
    let imap_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let imap_port = imap_listener.local_addr().unwrap().port();

    // 2 connect calls + 2 /v1/mail/test calls = 4 SMTP & 4 IMAP connections.
    let smtp_server = tokio::spawn(async move {
        for _ in 0..4 {
            let (socket, _) = smtp_listener.accept().await.unwrap();
            let (reader, mut writer) = socket.into_split();
            let mut lines = BufReader::new(reader).lines();
            writer.write_all(b"220 fake ESMTP\r\n").await.unwrap();
            while let Ok(Some(line)) = lines.next_line().await {
                if line.starts_with("EHLO") {
                    writer
                        .write_all(b"250-fake\r\n250 AUTH LOGIN\r\n")
                        .await
                        .unwrap();
                } else if line == "AUTH LOGIN" {
                    writer.write_all(b"334 VXNlcm5hbWU6\r\n").await.unwrap();
                    let _ = lines.next_line().await;
                    writer.write_all(b"334 UGFzc3dvcmQ6\r\n").await.unwrap();
                    let _ = lines.next_line().await;
                    writer.write_all(b"235 ok\r\n").await.unwrap();
                } else if line == "QUIT" {
                    writer.write_all(b"221 bye\r\n").await.unwrap();
                    break;
                } else {
                    writer.write_all(b"250 ok\r\n").await.unwrap();
                }
            }
        }
    });

    let imap_server = tokio::spawn(async move {
        for _ in 0..4 {
            let (socket, _) = imap_listener.accept().await.unwrap();
            let (reader, mut writer) = socket.into_split();
            let mut lines = BufReader::new(reader).lines();
            writer
                .write_all(b"* OK [CAPABILITY IMAP4rev1 AUTH=LOGIN] ready.\r\n")
                .await
                .unwrap();
            while let Ok(Some(line)) = lines.next_line().await {
                let tag = line.split_whitespace().next().unwrap_or("a1");
                if line.contains(" LOGOUT") {
                    writer
                        .write_all(format!("* BYE\r\n{tag} OK bye\r\n").as_bytes())
                        .await
                        .unwrap();
                    break;
                }
                writer
                    .write_all(format!("{tag} OK\r\n").as_bytes())
                    .await
                    .unwrap();
            }
        }
    });

    let root = temp_base();
    let (app, _, store) = app_with_root(true, Policy::empty(), root);
    let alice = store
        .create_user("mail-test-a", "mta@example.example", "Mail Test A")
        .unwrap();
    let alice_token = mint_for(&store, &alice.id, "mail-test-tok");

    // 1. Not connected yet -> 404 for both default and named account.
    let res = post_json(&app, &alice_token, "/v1/mail/test", json!({})).await;
    assert_eq!(res.status(), StatusCode::NOT_FOUND);

    let res = post_json(
        &app,
        &alice_token,
        "/v1/mail/test",
        json!({ "account": "work" }),
    )
    .await;
    assert_eq!(res.status(), StatusCode::NOT_FOUND);

    // 2. Connect default mailbox and named "work" mailbox.
    let res = post_json(
        &app,
        &alice_token,
        "/v1/mail/connect",
        json!({
            "smtp_host": "127.0.0.1",
            "smtp_port": smtp_port,
            "smtp_security": "none",
            "username": "mta@example.example",
            "password": "default-password",
            "imap_host": "127.0.0.1",
            "imap_port": imap_port,
            "imap_security": "none"
        }),
    )
    .await;
    assert_eq!(res.status(), StatusCode::OK);

    let res = post_json(
        &app,
        &alice_token,
        "/v1/mail/connect",
        json!({
            "account": "work",
            "smtp_host": "127.0.0.1",
            "smtp_port": smtp_port,
            "smtp_security": "none",
            "username": "work@example.example",
            "password": "work-password",
            "imap_host": "127.0.0.1",
            "imap_port": imap_port,
            "imap_security": "none"
        }),
    )
    .await;
    assert_eq!(res.status(), StatusCode::OK);

    // 3. POST /v1/mail/test without password tests saved default and named accounts.
    let res = post_json(&app, &alice_token, "/v1/mail/test", json!({})).await;
    assert_eq!(res.status(), StatusCode::OK);
    let body = read_json(res).await;
    assert_eq!(body["account"], "");
    assert_eq!(body["smtp_test"], "ok");
    assert_eq!(body["imap_test"], "ok");

    let res = post_json(
        &app,
        &alice_token,
        "/v1/mail/test",
        json!({ "account": "work" }),
    )
    .await;
    assert_eq!(res.status(), StatusCode::OK);
    let body = read_json(res).await;
    assert_eq!(body["account"], "work");
    assert_eq!(body["smtp_test"], "ok");
    assert_eq!(body["imap_test"], "ok");

    smtp_server.await.unwrap();
    imap_server.await.unwrap();
}
