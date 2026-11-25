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
