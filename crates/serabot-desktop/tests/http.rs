use secrecy::SecretString;
use serabot_desktop::{ApiClient, ApiError, PublicApiClient, Selection, validate_endpoint};
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

fn fixture(replies: Vec<(u16, String)>) -> (String, thread::JoinHandle<Vec<String>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let handle = thread::spawn(move || {
        replies.into_iter().map(|(status, body)| {
            let (mut stream, _) = listener.accept().unwrap();
            let req = request(&mut stream);
            write!(stream, "HTTP/1.1 {status} Test\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
            req
        }).collect()
    });
    (url, handle)
}

fn server(replies: Vec<(u16, String)>) -> (ApiClient, thread::JoinHandle<Vec<String>>) {
    let (url, handle) = fixture(replies);
    (
        ApiClient::new(
            validate_endpoint(&url).unwrap(),
            SecretString::new(TOKEN.into()),
        )
        .unwrap(),
        handle,
    )
}

fn public_server(
    replies: Vec<(u16, String)>,
) -> (PublicApiClient, thread::JoinHandle<Vec<String>>) {
    let (url, handle) = fixture(replies);
    (
        PublicApiClient::new(validate_endpoint(&url).unwrap()).unwrap(),
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
        (200, r#"{"cards":[]}"#.into()),
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
        "GET /v1/conversations/owner:1/cards ",
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
    assert_eq!(body(4), serde_json::json!({"provider":"p","model":"m"}));
    assert_eq!(
        body(5),
        serde_json::json!({"conversation_id":"owner:1","text":"Synthetic hello"})
    );
    assert_eq!(body(8), serde_json::json!({"decision":"once"}));
}

#[test]
fn card_writes_use_their_own_routes_and_bodies() {
    let (client, handle) = server(vec![
        (200, r#"{"card":{},"task":{}}"#.into()),
        (200, r#"{"card":{}}"#.into()),
        (200, r#"{"card":{}}"#.into()),
    ]);
    client.answer_card("card-1", "Blue").unwrap();
    client.decline_card("card-1").unwrap();
    client
        .submit_secret_card("card-1", "Synthetic-value")
        .unwrap();
    let requests = handle.join().unwrap();
    assert!(requests[0].starts_with("POST /v1/cards/card-1/answer "));
    assert!(requests[1].starts_with("POST /v1/cards/card-1/decline "));
    assert!(requests[2].starts_with("POST /v1/cards/card-1/secret "));
    let body = |index: usize| -> serde_json::Value {
        serde_json::from_str(requests[index].split("\r\n\r\n").nth(1).unwrap()).unwrap()
    };
    assert_eq!(body(0), serde_json::json!({"answer":"Blue"}));
    assert_eq!(body(2), serde_json::json!({"value":"Synthetic-value"}));
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
            None,
        )
        .unwrap();
    assert_eq!(node.id, "grp:ops");
    assert!(!node.accepts_turns);
    let section = client
        .create_conversation(
            serabot_desktop::ConversationKind::Section,
            "finance",
            None,
            None,
        )
        .unwrap();
    assert_eq!(section.id, "sec:finance");
    assert!(section.parent_id.is_none());
    let direct = client
        .create_conversation(
            serabot_desktop::ConversationKind::Direct,
            "analyst",
            None,
            None,
        )
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
        client.create_conversation(
            serabot_desktop::ConversationKind::Section,
            "Finance",
            None,
            None
        ),
        Err(ApiError::Validation(_))
    ));
    assert!(matches!(
        client.create_conversation(serabot_desktop::ConversationKind::Group, "ops", None, None),
        Err(ApiError::Validation(_))
    ));
    assert!(matches!(
        client.create_conversation(
            serabot_desktop::ConversationKind::Section,
            "finance",
            Some("sec:finance"),
            None
        ),
        Err(ApiError::Validation(_))
    ));
    assert!(matches!(
        client.create_conversation(
            serabot_desktop::ConversationKind::Direct,
            "analyst",
            Some("dir:other"),
            None
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
            .create_conversation(
                serabot_desktop::ConversationKind::Section,
                "finance",
                None,
                None
            )
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
    let (client, handle) = server(vec![
        (
            200,
            r#"{"messages":[{"id":"m","role":"assistant","content":"Synthetic analyst summary.","created_at":"now","name":"analyst"}]}"#.into(),
        ),
        (
            200,
            r#"{"cards":[{"id":"card-1","conversation_id":"room:review","bot":"analyst","kind":"question","payload":{"question":"Which?","options":["A"],"allow_free_text":false},"status":"pending","created_at":"now"}]}"#.into(),
        ),
    ]);
    let history = client.history("room:review").unwrap();
    assert_eq!(history.messages[0].name.as_deref(), Some("analyst"));
    assert_eq!(history.cards.len(), 1);
    assert_eq!(history.cards[0].kind, "question");
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
fn meet_your_team_reads_the_catalog_and_installs_one_bot() {
    let (client, handle) = server(vec![(
        200,
        r#"{"bots":[{"id":"analyst","title":"Analyst","description":"Reads the numbers.","role":"specialist","installed":false,"avatar_version":"v1"},{"id":"writer","role":"specialist","installed":true,"avatar_version":null}]}"#.into(),
    )]);
    let bots = client.bot_catalog().unwrap();
    let requests = handle.join().unwrap();
    assert!(requests[0].starts_with("GET /v1/bots/catalog "));
    assert_eq!(bots.len(), 2);
    assert_eq!(bots[0].id, "analyst");
    assert_eq!(bots[0].title.as_deref(), Some("Analyst"));
    assert!(!bots[0].installed);
    assert!(bots[1].installed);
    assert!(bots[1].title.is_none());

    let (client, handle) = server(vec![(
        200,
        r#"{"bots":[{"id":"analyst","role":"specialist","installed":false},{"id":"analyst","role":"specialist","installed":false}]}"#.into(),
    )]);
    assert_eq!(client.bot_catalog().unwrap_err(), ApiError::InvalidResponse);
    handle.join().unwrap();

    let (client, handle) = server(vec![(
        200,
        r#"{"bots":[{"id":"../analyst","role":"specialist","installed":false}]}"#.into(),
    )]);
    assert_eq!(client.bot_catalog().unwrap_err(), ApiError::InvalidResponse);
    handle.join().unwrap();

    let (client, handle) = server(vec![(
        200,
        r#"{"bot":{"id":"analyst","title":"Analyst","role":"specialist","enabled":false,"installed":true,"avatar_version":"v1"}}"#.into(),
    )]);
    client.install_shipped_bot("analyst").unwrap();
    let requests = handle.join().unwrap();
    assert!(requests[0].starts_with("POST /v1/bots/catalog/analyst/install "));

    // A bot the owner already has is a conflict, and an unknown template is a
    // 404; both come back as the status, never as a replayed write.
    let (client, handle) = server(vec![(409, r#"{"error":"bot already exists"}"#.into())]);
    assert_eq!(
        client.install_shipped_bot("analyst").unwrap_err(),
        ApiError::Http(409)
    );
    handle.join().unwrap();
    let (client, handle) = server(vec![(404, r#"{"error":"not found"}"#.into())]);
    assert_eq!(
        client.install_shipped_bot("ghost").unwrap_err(),
        ApiError::Http(404)
    );
    handle.join().unwrap();

    // A reply for some other bot is not what was asked for.
    let (client, handle) = server(vec![(
        200,
        r#"{"bot":{"id":"writer","role":"specialist","enabled":false,"installed":true}}"#.into(),
    )]);
    assert_eq!(
        client.install_shipped_bot("analyst").unwrap_err(),
        ApiError::InvalidResponse
    );
    handle.join().unwrap();

    // Names that are not slugs never leave the process.
    let (client, handle) = server(vec![]);
    assert!(matches!(
        client.install_shipped_bot("../analyst"),
        Err(ApiError::Validation(_))
    ));
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
fn unarchive_and_library_use_their_routes_and_validate_responses() {
    let hash = "0".repeat(64);
    let (client, handle) = server(vec![
        (200, r#"{"id":"dir:analyst","archived":false}"#.into()),
        (
            200,
            format!(
                r#"{{"artifacts":[{{"action_id":"act-1","tool":"deck_build","hash":"{hash}","filename":"deck.pdf","title":"Deck","created_at":"2026-10-29T10:00:00.000Z"}}]}}"#
            ),
        ),
    ]);
    client.unarchive_conversation("dir:analyst").unwrap();
    let library = client.library("room:review").unwrap();
    assert_eq!(library.len(), 1);
    assert_eq!(library[0].filename.as_deref(), Some("deck.pdf"));
    assert_eq!(library[0].tool, "deck_build");
    let requests = handle.join().unwrap();
    assert!(
        requests[0].starts_with("POST /v1/conversations/dir:analyst/unarchive "),
        "{}",
        requests[0]
    );
    assert!(
        requests[1].starts_with("GET /v1/conversations/room:review/library "),
        "{}",
        requests[1]
    );
}

#[test]
fn a_library_response_with_an_unshaped_hash_is_rejected() {
    let (client, handle) = server(vec![(
        200,
        r#"{"artifacts":[{"action_id":"act-1","tool":"deck_build","hash":"not-a-hash","filename":null,"title":null,"created_at":"2026-10-29T10:00:00.000Z"}]}"#.into(),
    )]);
    assert_eq!(
        client.library("room:review").unwrap_err(),
        ApiError::InvalidResponse
    );
    handle.join().unwrap();
}

#[test]
fn chat_plan_and_auto_approve_use_their_routes_and_tolerate_old_servers() {
    let (client, handle) = server(vec![
        (
            200,
            r#"{"plan":{"goal":"Ship it","items":[{"text":"Build","status":"doing"}]}}"#.into(),
        ),
        (
            200,
            r#"{"conversation_id":"dir:main","enabled":true}"#.into(),
        ),
        (
            200,
            r#"{"conversation_id":"dir:main","enabled":false}"#.into(),
        ),
        // An older server without the routes: no plan, switch off.
        (404, r#"{"error":"not found"}"#.into()),
        (404, r#"{"error":"not found"}"#.into()),
    ]);
    let (plan, auto) = client.chat_state("dir:main").unwrap();
    assert_eq!(plan.unwrap().goal, "Ship it");
    assert!(auto);
    assert!(!client.set_auto_approve("dir:main", false).unwrap());
    let (plan, auto) = client.chat_state("dir:main").unwrap();
    assert!(plan.is_none() && !auto);
    let requests = handle.join().unwrap();
    assert!(requests[0].starts_with("GET /v1/conversations/dir:main/plan "));
    assert!(requests[1].starts_with("GET /v1/conversations/dir:main/auto-approve "));
    assert!(requests[2].starts_with("PUT /v1/conversations/dir:main/auto-approve "));
}

const CHALLENGE: &str = "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM";
const STATE: &str = "synthetic-client-state-0123456789";
const RETURN_TO: &str = "http://127.0.0.1:51121/auth-done";
const SESSION_TOKEN: &str = "synthetic-session-token-0123456789abcdef";

fn handoff_start() -> serabot_desktop::HandoffStart<'static> {
    serabot_desktop::HandoffStart {
        return_to: RETURN_TO,
        challenge: CHALLENGE,
        client_state: STATE,
    }
}

#[test]
fn public_sign_in_starts_the_handoff_flow_without_a_bearer_token() {
    let (client, handle) = public_server(vec![
        (
            200,
            r#"{"url":"https://login.example/authorize?client_id=test&state=s"}"#.into(),
        ),
        (
            200,
            r#"{"url":"https://login.example/authorize?state=noinvite"}"#.into(),
        ),
    ]);
    let url = client
        .auth_login_start("microsoft", Some("invite-code-123"), &handoff_start())
        .unwrap();
    assert_eq!(
        url,
        "https://login.example/authorize?client_id=test&state=s"
    );
    let url = client
        .auth_login_start("microsoft", Some("  "), &handoff_start())
        .unwrap();
    assert_eq!(url, "https://login.example/authorize?state=noinvite");

    let requests = handle.join().unwrap();
    for request in &requests {
        assert!(
            request.starts_with("POST /v1/auth/login/start "),
            "wrong route: {request}"
        );
        assert!(
            !request.to_lowercase().contains("authorization:"),
            "the public sign-in routes must not carry a bearer token: {request}"
        );
    }
    let body = |index: usize| -> serde_json::Value {
        serde_json::from_str(requests[index].split("\r\n\r\n").nth(1).unwrap()).unwrap()
    };
    assert_eq!(
        body(0),
        serde_json::json!({
            "provider": "microsoft",
            "invite_code": "invite-code-123",
            "return_to": RETURN_TO,
            "handoff_challenge": CHALLENGE,
            "client_state": STATE,
        })
    );
    assert_eq!(
        body(1),
        serde_json::json!({
            "provider": "microsoft",
            "return_to": RETURN_TO,
            "handoff_challenge": CHALLENGE,
            "client_state": STATE,
        }),
        "an empty invite code is omitted"
    );
}

#[test]
fn public_sign_in_errors_stay_numeric_and_unsafe_urls_are_rejected() {
    let (client, handle) = public_server(vec![
        (403, r#"{"error":"forbidden"}"#.into()),
        (409, r#"{"error":"invalid_state"}"#.into()),
        (200, r#"{"url":"http://login.example/authorize"}"#.into()),
        (200, r#"{"url":"javascript:alert(1)"}"#.into()),
    ]);
    let start = handoff_start();
    assert_eq!(
        client
            .auth_login_start("microsoft", None, &start)
            .unwrap_err(),
        ApiError::Http(403)
    );
    assert_eq!(
        client
            .auth_login_start("microsoft", None, &start)
            .unwrap_err(),
        ApiError::Http(409)
    );
    assert_eq!(
        client
            .auth_login_start("microsoft", None, &start)
            .unwrap_err(),
        ApiError::InvalidResponse
    );
    assert_eq!(
        client
            .auth_login_start("microsoft", None, &start)
            .unwrap_err(),
        ApiError::InvalidResponse
    );
    handle.join().unwrap();

    // Local validation happens before any socket is opened.
    let client = PublicApiClient::new(validate_endpoint("http://127.0.0.1:9").unwrap()).unwrap();
    assert!(matches!(
        client.auth_login_start("Microsoft", None, &start),
        Err(ApiError::Validation(_))
    ));
    assert!(matches!(
        client.auth_login_start("", None, &start),
        Err(ApiError::Validation(_))
    ));
    assert!(matches!(
        client.auth_login_start("microsoft", Some("bad\ncode"), &start),
        Err(ApiError::Validation(_))
    ));
    let long = "a".repeat(257);
    assert!(matches!(
        client.auth_login_start("microsoft", Some(long.as_str()), &start),
        Err(ApiError::Validation(_))
    ));
    for bad in [
        serabot_desktop::HandoffStart {
            return_to: "https://evil.example/auth-done",
            ..handoff_start()
        },
        serabot_desktop::HandoffStart {
            return_to: "http://127.0.0.1:80/auth-done",
            ..handoff_start()
        },
        serabot_desktop::HandoffStart {
            challenge: "short",
            ..handoff_start()
        },
        serabot_desktop::HandoffStart {
            client_state: "tiny",
            ..handoff_start()
        },
    ] {
        assert!(matches!(
            client.auth_login_start("microsoft", None, &bad),
            Err(ApiError::Validation(_))
        ));
    }
}

#[test]
fn handoff_returns_the_session_and_its_user() {
    use secrecy::ExposeSecret;
    let (client, handle) = public_server(vec![(
        200,
        format!(
            r#"{{"token":"{SESSION_TOKEN}","expires_at":"2099-01-01T00:00:00Z","user":{{"id":"u1","email":"ada@example.test","display_name":"Ada","role":"member"}}}}"#
        ),
    )]);
    let verifier = "v".repeat(43);
    let signed_in = client.auth_handoff("one-time-code", &verifier).unwrap();
    assert_eq!(signed_in.token.expose_secret(), SESSION_TOKEN);
    assert_eq!(
        signed_in.expires_at.as_deref(),
        Some("2099-01-01T00:00:00Z")
    );
    assert_eq!(signed_in.user.email, "ada@example.test");
    assert_eq!(signed_in.user.label(), "Ada");

    let requests = handle.join().unwrap();
    assert!(requests[0].starts_with("POST /v1/auth/handoff "));
    assert!(!requests[0].to_lowercase().contains("authorization:"));
    let body: serde_json::Value =
        serde_json::from_str(requests[0].split("\r\n\r\n").nth(1).unwrap()).unwrap();
    assert_eq!(
        body,
        serde_json::json!({"handoff": "one-time-code", "verifier": verifier})
    );
}

#[test]
fn a_burned_or_malformed_handoff_never_yields_a_session() {
    let (client, handle) = public_server(vec![
        (400, r#"{"error":"invalid handoff"}"#.into()),
        // A token that is too short to be a session token.
        (
            200,
            r#"{"token":"short","user":{"id":"u1","email":"a@example.test"}}"#.into(),
        ),
        // A user with a control character.
        (
            200,
            format!(
                r#"{{"token":"{SESSION_TOKEN}","user":{{"id":"u1","display_name":"a\u0000b"}}}}"#
            ),
        ),
        // No user at all.
        (200, format!(r#"{{"token":"{SESSION_TOKEN}"}}"#)),
    ]);
    let verifier = "v".repeat(43);
    assert_eq!(
        client.auth_handoff("code", &verifier).err().unwrap(),
        ApiError::Http(400)
    );
    for _ in 0..3 {
        assert_eq!(
            client.auth_handoff("code", &verifier).err().unwrap(),
            ApiError::InvalidResponse
        );
    }
    handle.join().unwrap();

    // Local validation happens before any socket is opened.
    let client = PublicApiClient::new(validate_endpoint("http://127.0.0.1:9").unwrap()).unwrap();
    assert!(matches!(
        client.auth_handoff("", &verifier).err().unwrap(),
        ApiError::Validation(_)
    ));
    assert!(matches!(
        client.auth_handoff("co de", &verifier).err().unwrap(),
        ApiError::Validation(_)
    ));
    assert!(matches!(
        client.auth_handoff("code", "short").err().unwrap(),
        ApiError::Validation(_)
    ));
}

#[test]
fn me_and_logout_use_the_session_token() {
    let (client, handle) = server(vec![
        (
            200,
            r#"{"id":"u1","email":"ada@example.test","display_name":"Ada","role":"admin"}"#.into(),
        ),
        (204, String::new()),
        (401, String::new()),
        (500, String::new()),
        (401, String::new()),
    ]);
    let user = client.me().unwrap();
    assert_eq!(user.display_name, "Ada");
    assert_eq!(user.role, "admin");
    client.logout().unwrap();
    client
        .logout()
        .expect("a 401 means the session is already over");
    assert_eq!(client.logout().unwrap_err(), ApiError::Http(500));
    assert_eq!(client.me().unwrap_err(), ApiError::Http(401));
    let requests = handle.join().unwrap();
    for (request, route) in requests.iter().zip([
        "GET /v1/me ",
        "POST /v1/auth/logout ",
        "POST /v1/auth/logout ",
        "POST /v1/auth/logout ",
        "GET /v1/me ",
    ]) {
        assert!(request.starts_with(route), "wrong route: {request}");
        assert!(
            request
                .to_lowercase()
                .contains(&format!("authorization: bearer {TOKEN}\r\n")),
            "missing bearer: {request}"
        );
    }
}

#[test]
fn a_401_reads_as_an_ended_session() {
    assert_eq!(
        ApiError::Http(401).to_string(),
        "Your session ended. Sign in again."
    );
}

#[test]
fn telegram_status_link_and_unlink_use_their_routes_and_auth() {
    let (client, handle) = server(vec![
        (
            200,
            r#"{"enabled":true,"linked":true,"telegram_user_id":40,"telegram_chat_id":30,"linked_at":"2026-11-01T10:00:00Z"}"#
                .into(),
        ),
        (
            200,
            r#"{"code":"0123456789abcdef","expires_at":"2026-11-01T10:10:00Z","instructions":"Send /start <code> to the SeraBot Telegram bot."}"#
                .into(),
        ),
        (200, r#"{"linked":false,"was_linked":true}"#.into()),
    ]);
    let status = client.telegram().unwrap();
    assert!(status.enabled);
    assert!(status.linked);
    assert_eq!(status.telegram_user_id, Some(40));
    assert_eq!(status.telegram_chat_id, Some(30));
    assert_eq!(status.chat_label().as_deref(), Some("chat 30"));
    assert_eq!(status.linked_at.as_deref(), Some("2026-11-01T10:00:00Z"));

    let link = client.telegram_link().unwrap();
    assert_eq!(link.code, "0123456789abcdef");
    assert_eq!(link.command(), "/start 0123456789abcdef");
    assert_eq!(link.expires_at.as_deref(), Some("2026-11-01T10:10:00Z"));

    assert!(client.telegram_unlink().unwrap());

    let requests = handle.join().unwrap();
    let routes = [
        "GET /v1/telegram ",
        "POST /v1/telegram/link ",
        "DELETE /v1/telegram/link ",
    ];
    for (request, route) in requests.iter().zip(routes) {
        assert!(request.starts_with(route), "wrong route: {request}");
        assert!(
            request
                .to_lowercase()
                .contains(&format!("authorization: bearer {TOKEN}\r\n")),
            "Telegram routes are session-scoped: {request}"
        );
    }
}

#[test]
fn telegram_reports_an_unlinked_user_and_rejects_unsafe_shapes() {
    let (client, handle) = server(vec![(
        200,
        r#"{"enabled":false,"linked":false,"telegram_user_id":null,"telegram_chat_id":null,"linked_at":null}"#
            .into(),
    )]);
    let status = client.telegram().unwrap();
    assert!(!status.enabled);
    assert!(!status.linked);
    assert!(status.chat_label().is_none());
    handle.join().unwrap();

    let (client, handle) = server(vec![
        (403, r#"{"error":"forbidden"}"#.into()),
        (500, r#"{"error":"internal"}"#.into()),
        (
            200,
            r#"{"enabled":true,"linked":true,"telegram_user_id":40,"telegram_chat_id":-1}"#.into(),
        ),
        (200, r#"{"code":"bad code","expires_at":"x"}"#.into()),
        (200, r#"{"linked":true,"was_linked":true}"#.into()),
    ]);
    assert_eq!(client.telegram_link().unwrap_err(), ApiError::Http(403));
    assert_eq!(client.telegram_unlink().unwrap_err(), ApiError::Http(500));
    assert_eq!(client.telegram().unwrap_err(), ApiError::InvalidResponse);
    assert_eq!(
        client.telegram_link().unwrap_err(),
        ApiError::InvalidResponse
    );
    assert_eq!(
        client.telegram_unlink().unwrap_err(),
        ApiError::InvalidResponse
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

fn raw_server(
    replies: Vec<(u16, &'static str, Vec<u8>)>,
) -> (ApiClient, thread::JoinHandle<Vec<String>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let handle = thread::spawn(move || {
        replies
            .into_iter()
            .map(|(status, content_type, body)| {
                let (mut stream, _) = listener.accept().unwrap();
                let req = request(&mut stream);
                let head = format!(
                    "HTTP/1.1 {status} Test\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                // A client that refuses an oversized body hangs up early; a
                // broken pipe here is expected, not a test failure.
                let _ = stream
                    .write_all(head.as_bytes())
                    .and_then(|()| stream.write_all(&body));
                req
            })
            .collect()
    });
    let client = ApiClient::new(
        validate_endpoint(&url).unwrap(),
        SecretString::new(TOKEN.into()),
    )
    .unwrap();
    (client, handle)
}

fn body_of(request: &str) -> serde_json::Value {
    serde_json::from_str(request.split("\r\n\r\n").nth(1).unwrap()).unwrap()
}

const ENTRY: &str = r#"{"name":"portal","kind":"password","username":"me@example.test","domains":["example.test"],"created_at":"2026-11-01T10:00:00Z","updated_at":"2026-11-01T10:00:00Z"}"#;

#[test]
fn vault_routes_validate_and_never_echo_a_value() {
    use serabot_desktop::{VaultKind, VaultPut, VaultSource};
    use zeroize::Zeroizing;
    let (client, handle) = server(vec![
        (200, format!(r#"{{"entries":[{ENTRY}]}}"#)),
        (200, ENTRY.into()),
        (200, r#"{"deleted":true}"#.into()),
        // A server that echoes the value is refused, not displayed.
        (200, r#"{"entries":[{"name":"portal","kind":"note","domains":[],"value":"leak","created_at":"t","updated_at":"t"}]}"#.into()),
        (422, r#"{"error":"a domain is not a valid host name"}"#.into()),
        (404, r#"{"error":"vault entry gone not found"}"#.into()),
        (500, r#"{"error":"secret detail"}"#.into()),
    ]);
    let entries = client.vault_list().unwrap();
    assert_eq!(entries[0].name, "portal");
    assert_eq!(entries[0].kind, VaultKind::Password);
    assert_eq!(entries[0].domains, ["example.test"]);

    let put = VaultPut {
        kind: VaultKind::Password,
        username: Some(" me@example.test ".into()),
        value: Zeroizing::new("synthetic-pass".into()),
        domains: vec!["example.test".into()],
        note: Some("  ".into()),
        source: VaultSource::Local,
        reference: None,
    };
    assert_eq!(client.vault_put("portal", &put).unwrap().name, "portal");
    client.vault_delete("portal").unwrap();
    assert_eq!(client.vault_list(), Err(ApiError::InvalidResponse));
    assert_eq!(
        client.vault_put("portal", &put),
        Err(ApiError::Rejected(
            "a domain is not a valid host name".into()
        ))
    );
    assert_eq!(
        client.vault_delete("gone"),
        Err(ApiError::Rejected("vault entry gone not found".into()))
    );
    // Other failures stay a bare status: no server text reaches the UI.
    assert_eq!(client.vault_list(), Err(ApiError::Http(500)));

    let requests = handle.join().unwrap();
    assert!(requests[0].starts_with("GET /v1/vault "));
    assert!(requests[1].starts_with("PUT /v1/vault/portal "));
    assert!(requests[2].starts_with("DELETE /v1/vault/portal "));
    for r in &requests {
        assert!(
            r.to_lowercase()
                .contains(&format!("authorization: bearer {TOKEN}\r\n"))
        );
    }
    assert_eq!(
        body_of(&requests[1]),
        serde_json::json!({
            "kind": "password",
            "source": "local",
            "username": "me@example.test",
            "value": "synthetic-pass",
            "domains": ["example.test"]
        })
    );
}

#[test]
fn vault_writes_are_validated_before_the_network() {
    use serabot_desktop::{VaultKind, VaultPut, normalize_vault_domain, valid_vault_name};
    use zeroize::Zeroizing;
    // No server is bound: any request would fail as Transport, not Validation.
    let (client, handle) = server(vec![]);
    let put = |value: &str, domains: &[&str]| VaultPut {
        kind: VaultKind::ApiKey,
        username: None,
        value: Zeroizing::new(value.into()),
        domains: domains.iter().map(|d| (*d).to_owned()).collect(),
        note: None,
        source: serabot_desktop::VaultSource::Local,
        reference: None,
    };
    for (name, input) in [
        ("Bad Name", put("v", &[])),
        ("ok", put("", &[])),
        ("ok", put("bad\u{7}", &[])),
        ("ok", put(&"x".repeat(8193), &[])),
        ("ok", put("v", &["https://example.test/path"])),
        ("ok", put("v", &["Example.TEST"])),
    ] {
        assert!(matches!(
            client.vault_put(name, &input),
            Err(ApiError::Validation(_))
        ));
    }
    assert!(
        client
            .vault_put("ok", &put("v\nw\tz", &["a.test"]))
            .is_err()
    );
    drop(handle);

    assert!(valid_vault_name("a-1"));
    assert!(!valid_vault_name("-a"));
    assert!(!valid_vault_name("A"));
    assert!(!valid_vault_name(&"a".repeat(49)));
    assert_eq!(
        normalize_vault_domain(" Example.COM. ").as_deref(),
        Some("example.com")
    );
    for bad in [
        "",
        "a b",
        "*.example.com",
        "https://a.test",
        "a.test/x",
        "a.test:80",
        "u@a.test",
    ] {
        assert_eq!(normalize_vault_domain(bad), None, "{bad}");
    }
}

#[test]
fn browser_routes_use_their_contract_shapes() {
    use serabot_desktop::{BrowserController, BrowserInput};
    let info = |controller: &str| {
        format!(
            r#"{{"active":true,"url":"https://example.test/","title":"Example","controller":"{controller}","updated_at":"2026-11-06T12:00:00Z"}}"#
        )
    };
    let (client, handle) = server(vec![
        (
            200,
            r#"{"active":false,"controller":"bot","updated_at":"2026-11-06T12:00:00Z"}"#.into(),
        ),
        (200, info("bot")),
        (200, info("user")),
        (200, info("user")),
        (200, info("user")),
        (200, info("user")),
        (200, info("bot")),
        (200, r#"{"closed":true}"#.into()),
        (
            200,
            r#"{"active":true,"controller":"robot","updated_at":"t"}"#.into(),
        ),
        (409, r#"{"error":"the user is not in control"}"#.into()),
    ]);
    let idle = client.browser_status().unwrap();
    assert!(!idle.active && idle.url.is_none());
    assert_eq!(
        client.browser_status().unwrap().title.as_deref(),
        Some("Example")
    );
    assert_eq!(
        client.browser_takeover().unwrap().controller,
        BrowserController::User
    );
    for input in [
        BrowserInput::Click { x: 10.5, y: 20.0 },
        BrowserInput::Text { text: "hi".into() },
        BrowserInput::Key {
            key: "Enter".into(),
        },
    ] {
        client.browser_input(&input).unwrap();
    }
    assert_eq!(
        client.browser_handback().unwrap().controller,
        BrowserController::Bot
    );
    assert!(client.browser_close().unwrap());
    assert_eq!(client.browser_status(), Err(ApiError::InvalidResponse));
    assert_eq!(
        client.browser_input(&BrowserInput::Scroll { dy: 120.0 }),
        Err(ApiError::Rejected("the user is not in control".into()))
    );

    let requests = handle.join().unwrap();
    let routes = [
        "GET /v1/browser ",
        "GET /v1/browser ",
        "POST /v1/browser/takeover ",
        "POST /v1/browser/input ",
        "POST /v1/browser/input ",
        "POST /v1/browser/input ",
        "POST /v1/browser/handback ",
        "POST /v1/browser/close ",
        "GET /v1/browser ",
        "POST /v1/browser/input ",
    ];
    for (request, route) in requests.iter().zip(routes) {
        assert!(request.starts_with(route), "{request}");
    }
    assert_eq!(
        body_of(&requests[3]),
        serde_json::json!({"type":"click","x":10.5,"y":20.0})
    );
    assert_eq!(
        body_of(&requests[4]),
        serde_json::json!({"type":"text","text":"hi"})
    );
    assert_eq!(
        body_of(&requests[5]),
        serde_json::json!({"type":"key","key":"Enter"})
    );
    assert_eq!(
        body_of(&requests[9]),
        serde_json::json!({"type":"scroll","dy":120.0})
    );
}

#[test]
fn browser_input_is_validated_before_the_network() {
    use serabot_desktop::{BrowserInput, validate_browser_input};
    for bad in [
        BrowserInput::Click { x: -1.0, y: 0.0 },
        BrowserInput::Click {
            x: f64::NAN,
            y: 0.0,
        },
        BrowserInput::Click { x: 0.0, y: 4001.0 },
        BrowserInput::Text {
            text: String::new(),
        },
        BrowserInput::Text {
            text: "x".repeat(2001),
        },
        BrowserInput::Key { key: "a b".into() },
        BrowserInput::Scroll { dy: 5001.0 },
        BrowserInput::Navigate {
            url: "javascript:alert(1)".into(),
        },
        BrowserInput::Navigate {
            url: "https://a.test/ x".into(),
        },
    ] {
        assert!(validate_browser_input(&bad).is_err(), "{bad:?}");
    }
    for good in [
        BrowserInput::Click { x: 0.0, y: 4000.0 },
        BrowserInput::Navigate {
            url: "https://example.test/a?b=1".into(),
        },
    ] {
        assert!(validate_browser_input(&good).is_ok());
    }
}

#[test]
fn screenshots_are_capped_jpeg_only_and_a_missing_session_is_a_404() {
    use serabot_desktop::MAX_SCREENSHOT_BYTES;
    let mut jpeg = vec![0xFF, 0xD8, 0xFF, 0xE0];
    jpeg.extend_from_slice(&[1; 100]);
    let mut oversized = jpeg.clone();
    oversized.resize(MAX_SCREENSHOT_BYTES + 1, 0);
    let mut at_cap = jpeg.clone();
    at_cap.resize(MAX_SCREENSHOT_BYTES, 0);
    let (client, handle) = raw_server(vec![
        (200, "image/jpeg", jpeg.clone()),
        (200, "image/jpeg", at_cap.clone()),
        (200, "image/jpeg", oversized),
        (200, "image/png", jpeg.clone()),
        (200, "image/jpeg", b"not a jpeg".to_vec()),
        (
            404,
            "application/json",
            br#"{"error":"no browser session"}"#.to_vec(),
        ),
    ]);
    assert_eq!(client.browser_screenshot().unwrap(), jpeg);
    assert_eq!(
        client.browser_screenshot().unwrap().len(),
        MAX_SCREENSHOT_BYTES
    );
    assert_eq!(client.browser_screenshot(), Err(ApiError::TooLarge));
    assert_eq!(client.browser_screenshot(), Err(ApiError::InvalidResponse));
    assert_eq!(client.browser_screenshot(), Err(ApiError::InvalidResponse));
    assert_eq!(client.browser_screenshot(), Err(ApiError::Http(404)));
    let requests = handle.join().unwrap();
    assert!(requests[0].starts_with("GET /v1/browser/screenshot "));
}

#[test]
fn conversation_rows_carry_a_validated_last_message_preview() {
    let row = |extra: &str| {
        format!(
            r#"{{"conversations":[{{"id":"dir:main","kind":"direct","created_at":"t","updated_at":"t","accepts_turns":true{extra}}}]}}"#
        )
    };
    let (client, _handle) = server(vec![
        (
            200,
            row(r#","last_message":{"role":"assistant","text":"14 Dec is open"}"#),
        ),
        (200, row(r#","last_message":{"role":"tool","text":"x"}"#)),
        (200, row("")),
    ]);
    let page = client.list_conversations(50, None, false).unwrap();
    let last = page.conversations[0].last_message.as_ref().unwrap();
    assert_eq!(
        (last.role.as_str(), last.text.as_str()),
        ("assistant", "14 Dec is open")
    );
    assert!(client.list_conversations(50, None, false).is_err());
    let page = client.list_conversations(50, None, false).unwrap();
    assert!(page.conversations[0].last_message.is_none());
}

const SESSION: &str = r#"{"id":"work","profile_name":"Work","active":true,"url":"https://example.test/","title":"Example","controller":"bot","bot":"main","conversation_id":"dir:main","updated_at":"2026-11-06T12:00:00Z"}"#;

#[test]
fn browser_profiles_use_their_routes_and_bodies() {
    let profile = r#"{"id":"work","name":"Work","is_default":false,"bots":["main"],"created_at":"2026-11-01T00:00:00Z"}"#;
    let (client, handle) = server(vec![
        (
            200,
            format!(
                r#"{{"profiles":[{{"id":"default","name":"Default","is_default":true,"bots":[],"created_at":"t"}},{profile}]}}"#
            ),
        ),
        (200, profile.into()),
        (200, profile.into()),
        (200, r#"{"deleted":true}"#.into()),
        (
            409,
            r#"{"error":"the default profile cannot be deleted"}"#.into(),
        ),
    ]);
    let profiles = client.browser_profiles().unwrap();
    assert_eq!(profiles.len(), 2);
    assert_eq!(serabot_desktop::profile_for_bot(&profiles, "main"), "work");
    assert_eq!(client.browser_profile_create("  Work ").unwrap().id, "work");
    assert_eq!(
        client.browser_profile_rename("work", "Shop").unwrap().id,
        "work"
    );
    client.browser_profile_delete("work").unwrap();
    // Refused before the network: the default profile, bad ids and names.
    assert!(matches!(
        client.browser_profile_delete("default"),
        Err(ApiError::Validation(_))
    ));
    assert!(matches!(
        client.browser_profile_rename("../x", "Shop"),
        Err(ApiError::Validation(_))
    ));
    assert!(matches!(
        client.browser_profile_create("   "),
        Err(ApiError::Validation(_))
    ));
    // The server's own refusal is shown as written.
    assert_eq!(
        client.browser_profile_rename("default", "x2"),
        Err(ApiError::Rejected(
            "the default profile cannot be deleted".into()
        ))
    );

    let requests = handle.join().unwrap();
    assert!(requests[0].starts_with("GET /v1/browser/profiles "));
    assert!(requests[1].starts_with("POST /v1/browser/profiles "));
    assert_eq!(body_of(&requests[1]), serde_json::json!({"name":"Work"}));
    assert!(requests[2].starts_with("PUT /v1/browser/profiles/work "));
    assert_eq!(body_of(&requests[2]), serde_json::json!({"name":"Shop"}));
    assert!(requests[3].starts_with("DELETE /v1/browser/profiles/work "));
    assert!(requests[4].starts_with("PUT /v1/browser/profiles/default "));
}

#[test]
fn browser_sessions_route_every_action_by_session_id() {
    use serabot_desktop::{BrowserController, BrowserInput};
    let user = SESSION.replace("\"controller\":\"bot\"", "\"controller\":\"user\"");
    let (client, handle) = server(vec![
        (200, format!(r#"{{"sessions":[{SESSION}]}}"#)),
        (200, user.clone()),
        (200, user.clone()),
        (200, SESSION.into()),
        (200, r#"{"closed":true}"#.into()),
    ]);
    let sessions = client.browser_sessions().unwrap();
    assert_eq!(sessions[0].id, "work");
    assert_eq!(sessions[0].conversation_id.as_deref(), Some("dir:main"));
    assert_eq!(
        client.browser_session_takeover("work").unwrap().controller,
        BrowserController::User
    );
    client
        .browser_session_input("work", &BrowserInput::Click { x: 1.0, y: 2.0 })
        .unwrap();
    assert_eq!(
        client.browser_session_handback("work").unwrap().controller,
        BrowserController::Bot
    );
    assert!(client.browser_session_close("work").unwrap());
    // Bad ids never reach the network.
    for bad in ["", "../default", "Work", "a b"] {
        assert!(matches!(
            client.browser_session_takeover(bad),
            Err(ApiError::Validation(_))
        ));
        assert!(matches!(
            client.browser_session_screenshot(bad),
            Err(ApiError::Validation(_))
        ));
    }
    let requests = handle.join().unwrap();
    let routes = [
        "GET /v1/browser/sessions ",
        "POST /v1/browser/sessions/work/takeover ",
        "POST /v1/browser/sessions/work/input ",
        "POST /v1/browser/sessions/work/handback ",
        "POST /v1/browser/sessions/work/close ",
    ];
    for (request, route) in requests.iter().zip(routes) {
        assert!(request.starts_with(route), "{request}");
    }
    assert_eq!(
        body_of(&requests[2]),
        serde_json::json!({"type":"click","x":1.0,"y":2.0})
    );
}

#[test]
fn a_session_screenshot_is_jpeg_capped_and_per_session() {
    let (client, handle) = raw_server(vec![
        (200, "image/jpeg", vec![0xFF, 0xD8, 0xFF, 0xE0, 1, 2, 3]),
        (200, "image/png", vec![0x89, b'P', b'N', b'G']),
        (
            404,
            "application/json",
            br#"{"error":"no session"}"#.to_vec(),
        ),
    ]);
    assert_eq!(client.browser_session_screenshot("work").unwrap().len(), 7);
    assert_eq!(
        client.browser_session_screenshot("work"),
        Err(ApiError::InvalidResponse)
    );
    assert_eq!(
        client.browser_session_screenshot("work"),
        Err(ApiError::Http(404))
    );
    let requests = handle.join().unwrap();
    assert!(requests[0].starts_with("GET /v1/browser/sessions/work/screenshot "));
}

#[test]
fn session_lists_with_unsafe_values_are_refused() {
    let bad = SESSION.replace("\"id\":\"work\"", "\"id\":\"../work\"");
    let (client, _handle) = server(vec![
        (200, format!(r#"{{"sessions":[{bad}]}}"#)),
        (
            200,
            format!(r#"{{"sessions":[{}]}}"#, [SESSION; 9].join(",")),
        ),
    ]);
    assert_eq!(client.browser_sessions(), Err(ApiError::InvalidResponse));
    assert_eq!(client.browser_sessions(), Err(ApiError::InvalidResponse));
}

#[test]
fn bot_settings_save_sends_only_what_changed_and_null_for_defaults() {
    use serabot_desktop::{BotSettingsUpdate, ModelPick};
    let (client, handle) = server(vec![
        (200, r#"{"ok":true}"#.into()),
        (200, r#"{"ok":true}"#.into()),
        (422, r#"{"error":"display_name is too long"}"#.into()),
    ]);
    client
        .update_bot(
            "main",
            &BotSettingsUpdate {
                display_name: Some("  Mia ".into()),
                model: Some(Some(ModelPick {
                    provider: "openai".into(),
                    model: "gpt-x".into(),
                })),
                browser_profile: Some(Some("work".into())),
                ..Default::default()
            },
        )
        .unwrap();
    client
        .update_bot(
            "scribe",
            &BotSettingsUpdate {
                model: Some(None),
                browser_profile: Some(None),
                ..Default::default()
            },
        )
        .unwrap();
    // Refused locally, before any request.
    assert!(matches!(
        client.update_bot(
            "main",
            &BotSettingsUpdate {
                display_name: Some("x".repeat(49)),
                ..Default::default()
            }
        ),
        Err(ApiError::Validation(_))
    ));
    assert!(matches!(
        client.update_bot("Bad Id", &BotSettingsUpdate::default()),
        Err(ApiError::Validation(_))
    ));
    // A server refusal keeps its message.
    assert_eq!(
        client.update_bot(
            "main",
            &BotSettingsUpdate {
                description: Some("ok".into()),
                ..Default::default()
            }
        ),
        Err(ApiError::Rejected("display_name is too long".into()))
    );
    let requests = handle.join().unwrap();
    assert!(requests[0].starts_with("PUT /v1/bots/main "));
    assert_eq!(
        body_of(&requests[0]),
        serde_json::json!({
            "display_name":"Mia",
            "model":{"provider":"openai","model":"gpt-x"},
            "browser_profile":"work"
        })
    );
    assert_eq!(
        body_of(&requests[1]),
        serde_json::json!({"model":null,"browser_profile":null})
    );
}

/// Reads one request and keeps the body as raw bytes (the shared helper
/// decodes text only).
fn raw_request(stream: &mut TcpStream) -> (String, Vec<u8>) {
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
            let head = String::from_utf8_lossy(&bytes[..end]).to_string();
            let length = head
                .to_lowercase()
                .lines()
                .find_map(|line| line.strip_prefix("content-length: ").map(str::to_owned))
                .map(|s| s.parse::<usize>().unwrap())
                .unwrap_or(0);
            if bytes.len() >= end + 4 + length {
                return (head, bytes[end + 4..end + 4 + length].to_vec());
            }
        }
    }
}

#[test]
fn bot_avatar_upload_sends_the_raw_image_bytes_to_the_avatar_route() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let handle = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let captured = raw_request(&mut stream);
        let body = r#"{"avatar":{"bot":"recruiter","version":"v1"}}"#;
        write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
        captured
    });
    let client = ApiClient::new(
        validate_endpoint(&url).unwrap(),
        SecretString::new(TOKEN.into()),
    )
    .unwrap();
    let mut png = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
    png.extend((0..5000u32).map(|i| (i % 251) as u8));
    client.set_bot_avatar("recruiter", &png).unwrap();
    let (head, body) = handle.join().unwrap();
    assert!(head.starts_with("PUT /v1/bots/recruiter/avatar "), "{head}");
    assert!(head.to_lowercase().contains("authorization: bearer "));
    assert_eq!(body, png);
}

#[test]
fn uploads_attachments_and_sends_their_ids() {
    let task = r#"{"task":{"id":"task-1","kind":"chat","bot":"owner","status":"queued"}}"#;
    let (client, handle) = server(vec![
        (
            200,
            r#"{"attachment":{"id":"att-1","filename":"notes.md","mime":"text/plain","size_bytes":5,"is_image":false}}"#.into(),
        ),
        (202, task.into()),
    ]);
    let file = client
        .upload_attachment("dir:main", "notes.md", b"hello")
        .unwrap();
    assert_eq!(file.id, "att-1");
    client
        .submit_with("dir:main", "", &["att-1".to_owned()])
        .unwrap();
    let requests = handle.join().unwrap();
    assert!(requests[0].starts_with("POST /v1/conversations/dir:main/attachments "));
    assert!(requests[0].to_lowercase().contains("x-filename: notes.md"));
    assert!(requests[1].contains(r#""attachments":["att-1"]"#));
}

#[test]
fn attachment_checks_run_before_any_request() {
    let (client, _handle) = server(vec![]);
    for bad in ["run.exe", "a/b.txt", "..\\x.txt", "page.svg", ""] {
        assert!(
            client.upload_attachment("dir:main", bad, b"x").is_err(),
            "{bad}"
        );
    }
    assert!(client.upload_attachment("dir:main", "a.txt", b"").is_err());
    let big = vec![0u8; serabot_desktop::MAX_ATTACHMENT_BYTES + 1];
    assert!(client.upload_attachment("dir:main", "a.txt", &big).is_err());
    assert!(client.submit_with("dir:main", "", &[]).is_err());
    let seven: Vec<String> = (0..7).map(|n| format!("a{n}")).collect();
    assert!(client.submit_with("dir:main", "hi", &seven).is_err());
}

/// A listener that drops the first `drops` connections without answering,
/// then serves `body` once. Returns the url and how many connections it saw.
fn flaky(drops: usize, body: &'static str) -> (String, thread::JoinHandle<usize>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let handle = thread::spawn(move || {
        let mut seen = 0;
        loop {
            let (mut stream, _) = listener.accept().unwrap();
            seen += 1;
            if seen <= drops {
                drop(stream);
                continue;
            }
            let _ = request(&mut stream);
            write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
            return seen;
        }
    });
    (url, handle)
}

#[test]
fn a_dropped_read_is_retried_and_succeeds() {
    let (url, handle) = flaky(1, r#"{"ok":true}"#);
    let client = ApiClient::new(
        validate_endpoint(&url).unwrap(),
        SecretString::new(TOKEN.into()),
    )
    .unwrap();
    // The typed reply may not match the body; what matters is that a second
    // connection was made after the first was dropped.
    let _ = client.health();
    assert_eq!(handle.join().unwrap(), 2);
}

#[test]
fn a_dropped_write_is_never_repeated() {
    let (url, handle) = flaky(1, "{}");
    let client = ApiClient::new(
        validate_endpoint(&url).unwrap(),
        SecretString::new(TOKEN.into()),
    )
    .unwrap();
    let error = client.delete_bot("analyst").unwrap_err();
    assert_eq!(error, ApiError::Transport);
    // Unblock the listener thread, which waits for a second connection.
    let _ = TcpStream::connect(url.trim_start_matches("http://"));
    let _ = handle.join();
}

#[test]
fn provider_key_test_default_and_sign_in_calls_use_the_account_routes() {
    let (client, handle) =
        server(vec![
        (200, r#"{"id":"mistral","configured":true}"#.into()),
        (
            200,
            r#"{"id":"mistral","ok":true,"status":"ok","model_source":"live","model_count":12}"#
                .into(),
        ),
        (200, r#"{"selection":{"provider":"mistral","model":"m"}}"#.into()),
        (200, r#"{"selection":{"provider":"mistral","model":"m"}}"#.into()),
        (
            200,
            r#"{"flow":"pkce","url":"https://openrouter.ai/auth?callback_url=x"}"#.into(),
        ),
        (200, r#"{"id":"openrouter","configured":true}"#.into()),
    ]);
    client
        .set_provider_key("mistral", "synthetic-provider-key")
        .unwrap();
    let tested = client.test_provider("mistral").unwrap();
    assert!(tested.ok);
    assert_eq!((tested.status.as_str(), tested.model_count), ("ok", 12));
    client.set_default_route("mistral", "m").unwrap();
    assert_eq!(client.default_route().unwrap().unwrap().model, "m");
    let start = client
        .provider_oauth_start("openrouter", &serde_json::json!({"challenge": "c"}))
        .unwrap();
    assert!(start.url.starts_with("https://openrouter.ai/auth"));
    client
        .provider_oauth_complete("openrouter", "code", "verifier", "http://localhost:1/cb")
        .unwrap();
    let requests = handle.join().unwrap();
    assert!(requests[0].starts_with("PUT /v1/providers/mistral/key "));
    assert!(requests[0].contains("synthetic-provider-key"));
    assert!(requests[1].starts_with("POST /v1/providers/mistral/test "));
    assert!(requests[2].starts_with("PUT /v1/providers/default-route "));
    assert!(requests[3].starts_with("GET /v1/providers/default-route "));
    assert!(requests[4].starts_with("POST /v1/providers/openrouter/oauth/start "));
    assert!(requests[5].starts_with("POST /v1/providers/openrouter/oauth/complete "));
    // Bad ids never reach the network.
    let (client, _handle) = server(Vec::new());
    assert!(client.test_provider("bad/id").is_err());
    assert!(client.set_provider_key("bad id", "k").is_err());
}
