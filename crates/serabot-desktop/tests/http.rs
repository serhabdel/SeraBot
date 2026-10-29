use secrecy::SecretString;
use serabot_desktop::{ApiClient, ApiError, Selection, validate_endpoint};
use std::{
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    thread,
    time::Duration,
};

const TOKEN: &str = "synthetic-desktop-test-token-00000000";

fn request(stream: &mut TcpStream) -> String {
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    let mut bytes = Vec::new();
    let mut buf = [0; 4096];
    loop {
        let n = stream.read(&mut buf).unwrap();
        assert!(n > 0);
        bytes.extend_from_slice(&buf[..n]);
        if let Some(end) = bytes.windows(4).position(|w| w == b"\r\n\r\n") {
            let headers = String::from_utf8_lossy(&bytes[..end]).to_lowercase();
            let length = headers
                .lines()
                .find_map(|line| line.strip_prefix("content-length: "))
                .map(|s| s.parse::<usize>().unwrap())
                .unwrap_or(0);
            if bytes.len() >= end + 4 + length {
                break;
            }
        }
        assert!(bytes.len() < 100_000);
    }
    String::from_utf8(bytes).unwrap()
}

fn server(replies: Vec<(u16, String)>) -> (ApiClient, thread::JoinHandle<Vec<String>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = validate_endpoint(&format!("http://{}", listener.local_addr().unwrap())).unwrap();
    let handle = thread::spawn(move || {
        replies.into_iter().map(|(status, body)| {
            let (mut stream, _) = listener.accept().unwrap();
            let req = request(&mut stream);
            write!(stream, "HTTP/1.1 {status} Test\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
            req
        }).collect()
    });
    (
        ApiClient::new(url, SecretString::new(TOKEN.into())).unwrap(),
        handle,
    )
}

#[test]
fn authenticates_and_routes_every_contract_endpoint() {
    use serabot_desktop::ApprovalDecision;
    let task = r#"{"task":{"id":"task-1","kind":"chat","bot":"owner","status":"queued"}}"#;
    let (client, handle) = server(vec![
        (200, r#"{"ok":true,"version":"test"}"#.into()),
        (200, r#"{"providers":[],"selection":{"provider":"p","model":"m"}}"#.into()),
        (200, r#"{"messages":[{"id":"msg-1","role":"assistant","content":"Synthetic reply","created_at":"test-time"}]}"#.into()),
        (200, r#"{"selection":{"provider":"p","model":"m"}}"#.into()),
        (202, task.into()),
        (200, task.into()),
        (200, r#"{"approvals":[{"id":"appr-1","action_id":"act-1","tool":"web.fetch","summary":"Fetch fixture","expires_at":"2099-01-01T00:00:00Z"}]}"#.into()),
        (200, r#"{"approval_id":"appr-1","approval_state":"approved","action_id":"act-1","action_status":"approved","resume_task":null}"#.into()),
    ]);
    assert!(client.health().unwrap().ok);
    client.models("owner:1").unwrap();
    assert_eq!(
        client.history("owner:1").unwrap().messages[0].content,
        "Synthetic reply"
    );
    client
        .select(
            "owner:1",
            &Selection {
                provider: "p".into(),
                model: "m".into(),
            },
        )
        .unwrap();
    client.submit("owner:1", "Synthetic hello").unwrap();
    client.task("task-1").unwrap();
    assert_eq!(client.approvals().unwrap().len(), 1);
    client
        .decide_approval("appr-1", ApprovalDecision::Once)
        .unwrap();
    let requests = handle.join().unwrap();
    let routes = [
        "GET /v1/health ",
        "GET /v1/models?conversation_id=owner%3A1 ",
        "GET /v1/conversations/owner:1/messages?limit=100 ",
        "PUT /v1/conversations/owner:1/model ",
        "POST /v1/messages ",
        "GET /v1/tasks/task-1 ",
        "GET /v1/approvals ",
        "POST /v1/approvals/appr-1/decide ",
    ];
    for (request, route) in requests.iter().zip(routes) {
        assert!(request.starts_with(route), "wrong route: {request}");
        assert!(
            request
                .to_lowercase()
                .contains(&format!("authorization: bearer {TOKEN}\r\n"))
        );
    }
    let body = |index: usize| -> serde_json::Value {
        serde_json::from_str(requests[index].split("\r\n\r\n").nth(1).unwrap()).unwrap()
    };
    assert_eq!(body(3), serde_json::json!({"provider":"p","model":"m"}));
    assert_eq!(
        body(4),
        serde_json::json!({"conversation_id":"owner:1","text":"Synthetic hello"})
    );
    assert_eq!(body(7), serde_json::json!({"decision":"once"}));
}

#[test]
fn safe_errors_never_include_response_bodies() {
    for status in [401, 403, 429, 500] {
        let (client, handle) = server(vec![(status, "secret-server-body".into())]);
        let error = client.health().unwrap_err();
        assert_eq!(error, ApiError::Http(status));
        assert!(!error.to_string().contains("secret-server-body"));
        assert_eq!(error.unknown_outcome(), status >= 500);
        handle.join().unwrap();
    }
}

#[test]
fn bounds_response_and_rejects_unexpected_json() {
    let (client, handle) = server(vec![(200, "x".repeat(2 * 1024 * 1024 + 1))]);
    assert_eq!(client.health().unwrap_err(), ApiError::TooLarge);
    // Early rejection may close the socket while the fixture is still writing.
    let _ = handle.join();
    let (client, handle) = server(vec![(200, r#"{"ok":"not-a-bool"}"#.into())]);
    assert_eq!(client.health().unwrap_err(), ApiError::InvalidResponse);
    handle.join().unwrap();
}

#[test]
fn redirect_is_not_followed_and_mutation_is_not_retried() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = validate_endpoint(&format!("http://{}", listener.local_addr().unwrap())).unwrap();
    let redirect = format!("http://{}/leak", listener.local_addr().unwrap());
    let handle = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let req = request(&mut stream);
        assert!(req.starts_with("POST /v1/messages "));
        write!(stream, "HTTP/1.1 307 Temporary Redirect\r\nLocation: {redirect}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").unwrap();
        drop(stream);
        listener.set_nonblocking(true).unwrap();
        // Keep the listener until the client returns; any automatic redirect/retry
        // would need a response and hit the client's timeout instead of Http(307).
        listener
    });
    let client = ApiClient::new(url, SecretString::new(TOKEN.into())).unwrap();
    assert_eq!(
        client.submit("owner", "hello").unwrap_err(),
        ApiError::Http(307)
    );
    let listener = handle.join().unwrap();
    assert!(listener.accept().is_err());
}

#[test]
fn creates_bot_and_conversation_with_owner_auth() {
    let bot = r#"{"bot":{"id":"notes","enabled":false,"role":"specialist","live":false,"tools":["skill_list","rag_search"],"skills":["ask"],"require_approval":[],"bot_file":"bots/notes.toml","prompt_file":"prompts/notes.txt"}}"#;
    let group = r#"{"conversation":{"id":"grp:ops","kind":"group","parent_id":"sec:finance","accepts_turns":false}}"#;
    let section = r#"{"conversation":{"id":"sec:finance","kind":"section","parent_id":null,"accepts_turns":false}}"#;
    let direct = r#"{"conversation":{"id":"dir:analyst","kind":"direct","accepts_turns":true}}"#;
    let (client, handle) = server(vec![
        (200, bot.into()),
        (200, group.into()),
        (200, section.into()),
        (200, direct.into()),
    ]);
    let created = client
        .create_bot(
            "notes",
            "Synthetic notes specialist.",
            &["skill_list".into(), "rag_search".into()],
            &["ask".into()],
        )
        .unwrap();
    assert!(!created.enabled);
    assert!(!created.live);
    assert_eq!(created.bot_file, "bots/notes.toml");
    let node = client
        .create_conversation(
            serabot_desktop::ConversationKind::Group,
            "ops",
            Some("sec:finance"),
        )
        .unwrap();
    assert_eq!(node.id, "grp:ops");
    assert!(!node.accepts_turns);
    let section = client
        .create_conversation(serabot_desktop::ConversationKind::Section, "finance", None)
        .unwrap();
    assert_eq!(section.id, "sec:finance");
    assert!(section.parent_id.is_none());
    let direct = client
        .create_conversation(serabot_desktop::ConversationKind::Direct, "analyst", None)
        .unwrap();
    assert!(direct.accepts_turns);
    let requests = handle.join().unwrap();
    let routes = [
        "POST /v1/bots ",
        "POST /v1/conversations ",
        "POST /v1/conversations ",
        "POST /v1/conversations ",
    ];
    for (request, route) in requests.iter().zip(routes) {
        assert!(request.starts_with(route), "wrong route: {request}");
        assert!(
            request
                .to_lowercase()
                .contains(&format!("authorization: bearer {TOKEN}\r\n"))
        );
    }
    let body = |index: usize| -> serde_json::Value {
        serde_json::from_str(requests[index].split("\r\n\r\n").nth(1).unwrap()).unwrap()
    };
    assert_eq!(
        body(0),
        serde_json::json!({
            "id": "notes",
            "description": "Synthetic notes specialist.",
            "tools": ["skill_list", "rag_search"],
            "skills": ["ask"]
        })
    );
    assert!(body(0).get("path").is_none());
    assert_eq!(
        body(1),
        serde_json::json!({"kind": "group", "slug": "ops", "parent_id": "sec:finance"})
    );
    assert_eq!(
        body(2),
        serde_json::json!({"kind": "section", "slug": "finance"})
    );
    assert!(body(2).get("parent_id").is_none());
    assert_eq!(
        body(3),
        serde_json::json!({"kind": "direct", "slug": "analyst"})
    );
}

#[test]
fn scaffold_requests_reject_unsafe_inputs_before_the_network() {
    let client = ApiClient::new(
        validate_endpoint("http://127.0.0.1:9").unwrap(),
        SecretString::new(TOKEN.into()),
    )
    .unwrap();
    assert!(matches!(
        client.create_bot("main", "", &[], &[]),
        Err(ApiError::Validation(_))
    ));
    assert!(matches!(
        client.create_bot("../notes", "", &[], &[]),
        Err(ApiError::Validation(_))
    ));
    assert!(matches!(
        client.create_bot("notes", "", &["delegate".into()], &[]),
        Err(ApiError::Validation(_))
    ));
    assert!(matches!(
        client.create_bot("notes", "", &["skill_list".into()], &["../ask".into()]),
        Err(ApiError::Validation(_))
    ));
    assert!(matches!(
        client.create_conversation(serabot_desktop::ConversationKind::Section, "Finance", None),
        Err(ApiError::Validation(_))
    ));
    assert!(matches!(
        client.create_conversation(serabot_desktop::ConversationKind::Group, "ops", None),
        Err(ApiError::Validation(_))
    ));
    assert!(matches!(
        client.create_conversation(
            serabot_desktop::ConversationKind::Section,
            "finance",
            Some("sec:finance")
        ),
        Err(ApiError::Validation(_))
    ));
    assert!(matches!(
        client.create_conversation(
            serabot_desktop::ConversationKind::Direct,
            "analyst",
            Some("dir:other")
        ),
        Err(ApiError::Validation(_))
    ));
    assert!(matches!(
        client.create_skill("../notes", "Short summary", None),
        Err(ApiError::Validation(_))
    ));
    assert!(matches!(
        client.create_skill("con", "Short summary", None),
        Err(ApiError::Validation(_))
    ));
    assert!(matches!(
        client.create_skill("com1", "Short summary", None),
        Err(ApiError::Validation(_))
    ));
    assert!(matches!(
        client.create_skill("notes-pack", "say \"hi\"", None),
        Err(ApiError::Validation(_))
    ));
    assert!(matches!(
        client.create_skill("notes-pack", "Short summary", Some("a\u{0}b")),
        Err(ApiError::Validation(_))
    ));
    assert!(matches!(
        client.create_skill("notes-pack", &"a".repeat(501), None),
        Err(ApiError::Validation(_))
    ));
}

#[test]
fn scaffold_responses_must_stay_disabled_and_relative() {
    let bodies = [
        r#"{"bot":{"id":"notes","enabled":true,"role":"specialist","live":false,"tools":["skill_list"],"skills":["ask"],"require_approval":[],"bot_file":"bots/notes.toml","prompt_file":"prompts/notes.txt"}}"#,
        r#"{"bot":{"id":"notes","enabled":false,"role":"specialist","live":true,"tools":["skill_list"],"skills":["ask"],"require_approval":[],"bot_file":"bots/notes.toml","prompt_file":"prompts/notes.txt"}}"#,
        r#"{"bot":{"id":"notes","enabled":false,"role":"orchestrator","live":false,"tools":["skill_list"],"skills":["ask"],"require_approval":[],"bot_file":"bots/notes.toml","prompt_file":"prompts/notes.txt"}}"#,
        r#"{"bot":{"id":"other","enabled":false,"role":"specialist","live":false,"tools":["skill_list"],"skills":["ask"],"require_approval":[],"bot_file":"bots/notes.toml","prompt_file":"prompts/notes.txt"}}"#,
        r#"{"bot":{"id":"notes","enabled":false,"role":"specialist","live":false,"tools":["delegate"],"skills":["ask"],"require_approval":[],"bot_file":"bots/notes.toml","prompt_file":"prompts/notes.txt"}}"#,
        r#"{"bot":{"id":"notes","enabled":false,"role":"specialist","live":false,"tools":["skill_list"],"skills":["ask"],"require_approval":[],"bot_file":"../notes.toml","prompt_file":"prompts/notes.txt"}}"#,
        r#"{"bot":{"id":"notes","enabled":false,"role":"specialist","live":false,"tools":["skill_list"],"skills":["ask"],"require_approval":[],"bot_file":"C:/secret/notes.toml","prompt_file":"prompts/notes.txt"}}"#,
        r#"{"bot":{"id":"notes","enabled":false,"role":"specialist","live":false,"tools":["skill_list"],"skills":["ask"],"require_approval":[],"bot_file":"bots/notes.toml","prompt_file":"/etc/notes.txt"}}"#,
        r#"{"bot":{"id":"notes","enabled":false,"role":"specialist","live":false,"tools":["skill_list"],"skills":["../ask"],"require_approval":[],"bot_file":"bots/notes.toml","prompt_file":"prompts/notes.txt"}}"#,
    ];
    for body in bodies {
        let (client, handle) = server(vec![(200, body.into())]);
        assert_eq!(
            client
                .create_bot(
                    "notes",
                    "Synthetic.",
                    &["skill_list".into()],
                    &["ask".into()]
                )
                .unwrap_err(),
            ApiError::InvalidResponse
        );
        handle.join().unwrap();
    }
    let (client, handle) = server(vec![(
        200,
        r#"{"conversation":{"id":"sec:other","kind":"section","parent_id":null,"accepts_turns":false}}"#
            .into(),
    )]);
    assert_eq!(
        client
            .create_conversation(serabot_desktop::ConversationKind::Section, "finance", None)
            .unwrap_err(),
        ApiError::InvalidResponse
    );
    handle.join().unwrap();
}

#[test]
fn creates_and_lists_skills_without_a_path_or_echoed_body() {
    let created = r#"{"skill":{"id":"notes-pack","name":"Notes Pack","description":"Short summary","file":"skills/notes-pack/SKILL.md","live":false}}"#;
    let listed = r#"{"skills":[{"id":"ask","name":"Ask","description":"Synthetic summary"},{"id":"notes-pack","name":"Notes Pack","description":"Short summary","pack":"office"}]}"#;
    let (client, handle) = server(vec![
        (200, created.into()),
        (200, created.into()),
        (200, listed.into()),
    ]);
    let skill = client
        .create_skill(
            "notes-pack",
            "  Short summary  ",
            Some("  # Notes\n\nUse allowlisted tools.\n  "),
        )
        .unwrap();
    assert_eq!(skill.id, "notes-pack");
    assert!(!skill.live);
    assert_eq!(skill.file, "skills/notes-pack/SKILL.md");
    let stub = client
        .create_skill("notes-pack", "Short summary", Some("   "))
        .unwrap();
    assert!(!stub.live);
    let skills = client.list_skills().unwrap();
    assert_eq!(skills.len(), 2);
    assert_eq!(skills[1].pack.as_deref(), Some("office"));
    let requests = handle.join().unwrap();
    assert!(requests[0].starts_with("POST /v1/skills "));
    assert!(requests[1].starts_with("POST /v1/skills "));
    assert!(requests[2].starts_with("GET /v1/skills "));
    for request in &requests {
        assert!(
            request
                .to_lowercase()
                .contains(&format!("authorization: bearer {TOKEN}\r\n"))
        );
    }
    let body = |index: usize| -> serde_json::Value {
        serde_json::from_str(requests[index].split("\r\n\r\n").nth(1).unwrap()).unwrap()
    };
    assert_eq!(
        body(0),
        serde_json::json!({
            "id": "notes-pack",
            "description": "Short summary",
            "body": "# Notes\n\nUse allowlisted tools."
        })
    );
    assert!(body(0).get("path").is_none());
    assert_eq!(
        body(1),
        serde_json::json!({"id": "notes-pack", "description": "Short summary"})
    );
    assert!(body(1).get("body").is_none());
    assert!(body(1).get("path").is_none());
}

#[test]
fn skill_responses_must_stay_offline_and_relative() {
    let bodies = [
        r#"{"skill":{"id":"notes-pack","name":"Notes Pack","description":"Short summary","file":"skills/notes-pack/SKILL.md","live":true}}"#,
        r#"{"skill":{"id":"other","name":"Other","description":"Short summary","file":"skills/other/SKILL.md","live":false}}"#,
        r#"{"skill":{"id":"notes-pack","name":"Notes Pack","description":"Short summary","file":"../notes-pack/SKILL.md","live":false}}"#,
        r#"{"skill":{"id":"notes-pack","name":"Notes Pack","description":"Short summary","file":"C:/skills/notes-pack/SKILL.md","live":false}}"#,
        r#"{"skill":{"id":"notes-pack","name":"../Notes","description":"Short summary","file":"skills/notes-pack/SKILL.md","live":false}}"#,
    ];
    for body in bodies {
        let (client, handle) = server(vec![(200, body.into())]);
        assert_eq!(
            client
                .create_skill("notes-pack", "Short summary", None)
                .unwrap_err(),
            ApiError::InvalidResponse
        );
        handle.join().unwrap();
    }
    let (client, handle) = server(vec![(
        200,
        r#"{"skills":[{"id":"../ask","name":"Ask","description":"Synthetic summary"}]}"#.into(),
    )]);
    assert_eq!(client.list_skills().unwrap_err(), ApiError::InvalidResponse);
    handle.join().unwrap();
}

#[test]
fn catalog_without_egress_approval_fails_closed() {
    let (client, handle) = server(vec![(200, r#"{"providers":[{"id":"test","models":["small"]}],"selection":{"provider":"test","model":"small"}}"#.into())]);
    let catalog = client.models("owner").unwrap();
    assert!(!catalog.allows(&catalog.selection));
    handle.join().unwrap();
}

#[test]
fn history_keeps_a_bot_speaker_and_rejects_an_unsafe_name() {
    let (client, handle) = server(vec![(
        200,
        r#"{"messages":[{"id":"m","role":"assistant","content":"Synthetic analyst summary.","created_at":"now","name":"analyst"}]}"#.into(),
    )]);
    let history = client.history("room:review").unwrap();
    assert_eq!(history.messages[0].name.as_deref(), Some("analyst"));
    handle.join().unwrap();

    let (client, handle) = server(vec![(
        200,
        r#"{"messages":[{"id":"m","role":"assistant","content":"note","created_at":"now","name":"bot999:secretvalue"}]}"#.into(),
    )]);
    assert!(matches!(
        client.history("room:review"),
        Err(ApiError::InvalidResponse)
    ));
    handle.join().unwrap();
}

#[test]
fn list_bots_rejects_secret_shaped_ids() {
    let (client, handle) = server(vec![(
        200,
        r#"{"bots":[{"id":"notes","role":"specialist","enabled":false,"live":false,"tools":["skill_list"],"skills":["ask"]}]}"#.into(),
    )]);
    let bots = client.list_bots().unwrap();
    assert_eq!(bots[0].id, "notes");
    assert!(!bots[0].live);
    handle.join().unwrap();

    let (client, handle) = server(vec![(
        200,
        r#"{"bots":[{"id":"../notes","role":"specialist","enabled":false,"live":false,"tools":[],"skills":[]}]}"#.into(),
    )]);
    assert_eq!(client.list_bots().unwrap_err(), ApiError::InvalidResponse);
    handle.join().unwrap();
}

#[test]
fn unexpected_roles_and_overlong_history_are_rejected() {
    for body in [r#"{"messages":[{"id":"m","role":"tool","content":"untrusted internal details","created_at":"now"}]}"#.to_string(),
        serde_json::json!({"messages": (0..101).map(|_| serde_json::json!({"id":"m","role":"user","content":"fixture","created_at":"now"})).collect::<Vec<_>>()}).to_string()] {
        let (client, handle) = server(vec![(200, body)]);
        assert!(matches!(client.history("owner"), Err(ApiError::InvalidResponse)));
        handle.join().unwrap();
    }
}

#[test]
fn admin_rejections_keep_auth_status_and_surface_safe_messages() {
    use serabot_desktop::NewProvider;
    let provider = NewProvider {
        id: "work".into(),
        kind: "openai-compatible".into(),
        base_url: "https://api.example.com/v1".into(),
        models: vec!["model-a".into()],
        api_key_secret: "WORK_API_KEY".into(),
        catalog_fetch: false,
        api_key: None,
    };
    let (client, handle) = server(vec![
        (401, r#"{"error":"unauthorized"}"#.into()),
        (
            422,
            r#"{"error":"secret WORK_API_KEY is empty; provide an API key","code":"admin"}"#.into(),
        ),
        (201, r#"{"saved":true,"restart_required":true}"#.into()),
    ]);
    assert!(matches!(
        client.admin_remove_provider("work"),
        Err(ApiError::Http(401))
    ));
    match client.admin_add_provider(&provider).unwrap_err() {
        ApiError::Rejected(message) => assert!(message.contains("WORK_API_KEY")),
        other => panic!("expected a safe rejection, got {other:?}"),
    }
    assert!(
        client
            .admin_add_provider(&provider)
            .unwrap()
            .restart_required
    );
    handle.join().unwrap();
}

#[test]
fn dropped_submit_response_is_unknown_and_never_resent() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = validate_endpoint(&format!("http://{}", listener.local_addr().unwrap())).unwrap();
    let handle = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        request(&mut stream);
        drop(stream);
        listener
    });
    let client = ApiClient::new(url, SecretString::new(TOKEN.into())).unwrap();
    assert!(
        client
            .submit("owner", "hello")
            .unwrap_err()
            .unknown_outcome()
    );
    let listener = handle.join().unwrap();
    listener.set_nonblocking(true).unwrap();
    assert!(listener.accept().is_err());
}
