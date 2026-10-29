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
fn catalog_without_egress_approval_fails_closed() {
    let (client, handle) = server(vec![(200, r#"{"providers":[{"id":"test","models":["small"]}],"selection":{"provider":"test","model":"small"}}"#.into())]);
    let catalog = client.models("owner").unwrap();
    assert!(!catalog.allows(&catalog.selection));
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
