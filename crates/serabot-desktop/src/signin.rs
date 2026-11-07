//! Native Microsoft sign-in: the loopback handoff of
//! `docs/specs/native-signin-contract.md`.
//!
//! The console makes a PKCE verifier/challenge and a random state, listens on
//! `127.0.0.1` at `/auth-done`, asks the server for the Microsoft sign-in URL,
//! opens it in the system browser, and trades the one-time handoff code it is
//! redirected with for the session token. The whole run is blocking and must
//! only be called from a worker thread.

use crate::{
    ApiError, HandoffStart, PublicApiClient, SignedIn,
    oauth::{self, AuthDone, OAuthError},
};
use std::{sync::atomic::AtomicBool, time::Duration};
use url::Url;

/// The only sign-in provider the desktop console offers.
pub const PROVIDER: &str = "microsoft";
/// How long the console waits for the browser to come back.
pub const SIGN_IN_TIMEOUT: Duration = Duration::from_secs(300);

/// Why a sign-in run ended without a session.
#[derive(Debug, PartialEq, Eq)]
pub enum SignInFailure {
    /// The user pressed Cancel; nothing to report.
    Cancelled,
    /// A message that is safe and useful to show.
    Message(String),
}

impl SignInFailure {
    fn message(text: impl Into<String>) -> Self {
        Self::Message(text.into())
    }
}

/// Run one sign-in against `endpoint`.
///
/// `open` shows the sign-in URL in the browser and reports whether it could;
/// `waiting` is called once the browser is open and the console is listening.
pub fn run(
    endpoint: Url,
    invite_code: Option<&str>,
    cancel: &AtomicBool,
    timeout: Duration,
    open: &dyn Fn(&str) -> bool,
    waiting: &dyn Fn(),
) -> Result<SignedIn, SignInFailure> {
    let (listener, port) = oauth::bind_loopback()
        .map_err(|_| SignInFailure::message(OAuthError::Listen.to_string()))?;
    let pkce = oauth::new_pkce();
    let return_to = oauth::handoff_return_to(port);
    let client =
        PublicApiClient::new(endpoint).map_err(|e| SignInFailure::message(e.to_string()))?;
    let url = client
        .auth_login_start(
            PROVIDER,
            invite_code,
            &HandoffStart {
                return_to: &return_to,
                challenge: &pkce.challenge,
                client_state: &pkce.state,
            },
        )
        .map_err(|error| SignInFailure::message(start_error(&error)))?;
    if cancel.load(std::sync::atomic::Ordering::SeqCst) {
        return Err(SignInFailure::Cancelled);
    }
    if !open(&url) {
        return Err(SignInFailure::message(
            "Could not open your browser. Open the default browser once and try again.",
        ));
    }
    waiting();
    let done =
        oauth::wait_for_auth_done(&listener, &pkce.state, timeout, cancel).map_err(|error| {
            match error {
                OAuthError::Cancelled => SignInFailure::Cancelled,
                other => SignInFailure::message(other.to_string()),
            }
        })?;
    drop(listener);
    match done {
        AuthDone::Failed(code) => Err(SignInFailure::message(redirect_error(&code))),
        AuthDone::Handoff(code) => client
            .auth_handoff(&code, &pkce.verifier)
            .map_err(|error| SignInFailure::message(handoff_error(&error))),
    }
}

/// The friendly sentence for an `?error=` code the server redirects with.
pub fn redirect_error(code: &str) -> &'static str {
    match code {
        "denied" => "Microsoft sign-in was cancelled or refused.",
        "invite_required" => {
            "This account needs an invite code. Choose \u{201c}I have an invite code\u{201d}, enter it, and sign in again."
        }
        "invite_invalid" => "That invite code is not valid, or it was already used.",
        "disabled" => "This account is disabled. Ask the administrator.",
        "max_users" => "This SeraBot server has reached its member limit. Ask the administrator.",
        _ => "The server could not finish signing you in. Try again in a moment.",
    }
}

/// The friendly sentence for a failed `POST /v1/auth/login/start`.
pub fn start_error(error: &ApiError) -> String {
    match error {
        ApiError::Http(400) => {
            "The server rejected this sign-in request. Update SeraBot and try again.".into()
        }
        ApiError::Http(403) => "Sign-in is not enabled on this server.".into(),
        ApiError::Http(404) => {
            "This server does not support sign-in from the app yet. Update the server.".into()
        }
        ApiError::Http(409) | ApiError::Http(422) => "Check the invite code and try again.".into(),
        ApiError::Http(429) => "Too many sign-in attempts. Wait a minute, then try again.".into(),
        ApiError::Http(500..=599) => {
            "The server could not start sign-in. Try again in a moment.".into()
        }
        other => other.sign_in_text(),
    }
}

/// The friendly sentence for a failed `POST /v1/auth/handoff`.
pub fn handoff_error(error: &ApiError) -> String {
    match error {
        ApiError::Http(400) => "That sign-in expired or was already used. Start again.".into(),
        other => start_error(other),
    }
}

/// An invite code the console will send: 1-256 characters, no control
/// characters. Empty input means "no invite code".
pub fn clean_invite(raw: &str) -> Result<Option<String>, &'static str> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }
    if trimmed.len() > 256 || trimmed.chars().any(char::is_control) {
        return Err("Invite codes are 1–256 characters, without control characters.");
    }
    Ok(Some(trimmed.to_owned()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use secrecy::ExposeSecret;
    use std::{
        io::{Read, Write},
        net::{TcpListener, TcpStream},
        sync::{Arc, Mutex},
        thread,
    };

    type Requests = Arc<Mutex<Vec<String>>>;

    /// A scripted server: answers each connection with the next canned reply.
    /// Each raw request is recorded before its reply is written.
    fn scripted(replies: Vec<(u16, String)>) -> (Url, Requests, thread::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = crate::validate_endpoint(&format!("http://{}", listener.local_addr().unwrap()))
            .unwrap();
        let requests: Requests = Arc::default();
        let record = Arc::clone(&requests);
        let handle = thread::spawn(move || {
            for (status, body) in replies {
                let (mut stream, _) = listener.accept().unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .unwrap();
                let mut bytes = Vec::new();
                let mut buf = [0u8; 4096];
                loop {
                    let n = stream.read(&mut buf).unwrap();
                    bytes.extend_from_slice(&buf[..n]);
                    if let Some(end) = bytes.windows(4).position(|w| w == b"\r\n\r\n") {
                        let head = String::from_utf8_lossy(&bytes[..end]).to_lowercase();
                        let length = head
                            .lines()
                            .find_map(|l| l.strip_prefix("content-length: "))
                            .and_then(|v| v.trim().parse::<usize>().ok())
                            .unwrap_or(0);
                        if bytes.len() >= end + 4 + length {
                            break;
                        }
                    }
                    if n == 0 {
                        break;
                    }
                }
                record
                    .lock()
                    .unwrap()
                    .push(String::from_utf8(bytes).unwrap());
                write!(
                    stream,
                    "HTTP/1.1 {status} T\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                )
                .unwrap();
            }
        });
        (url, requests, handle)
    }

    fn body_of(request: &str) -> serde_json::Value {
        serde_json::from_str(request.split("\r\n\r\n").nth(1).unwrap()).unwrap()
    }

    const START_OK: &str = r#"{"url":"https://login.example/authorize?x=1"}"#;

    fn handoff_ok() -> String {
        format!(
            r#"{{"token":"{}","expires_at":"2099-01-01T00:00:00Z","user":{{"id":"u1","email":"ada@example.test","display_name":"Ada","role":"member"}}}}"#,
            "synthetic-session-token-0123456789abcdef"
        )
    }

    /// Drive `run` with the browser played by a thread that reads the loopback
    /// port and client state out of the recorded `login/start` request and
    /// calls back with `query_for(state)`.
    fn drive(
        replies: Vec<(u16, String)>,
        invite: Option<&str>,
        query_for: impl Fn(&str) -> String + Send + 'static,
    ) -> (Result<SignedIn, SignInFailure>, Vec<String>) {
        let (endpoint, requests, server) = scripted(replies);
        let recorded = Arc::clone(&requests);
        let query_for = Arc::new(query_for);
        let open = move |url: &str| {
            assert_eq!(url, "https://login.example/authorize?x=1");
            let start = body_of(&recorded.lock().unwrap()[0]);
            let return_to = Url::parse(start["return_to"].as_str().unwrap()).unwrap();
            let port = return_to.port().unwrap();
            let query = query_for(start["client_state"].as_str().unwrap());
            thread::spawn(move || {
                let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
                write!(
                    stream,
                    "GET /auth-done?{query} HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n"
                )
                .unwrap();
                let mut sink = String::new();
                let _ = stream.read_to_string(&mut sink);
            });
            true
        };
        let cancel = AtomicBool::new(false);
        let result = run(
            endpoint,
            invite,
            &cancel,
            Duration::from_secs(10),
            &open,
            &|| {},
        );
        // Unused scripted replies (a failed run) would block `join`: drop them
        // by connecting nothing; the thread ends only when all are consumed, so
        // scripts must be exact.
        server.join().unwrap();
        let seen = requests.lock().unwrap().clone();
        (result, seen)
    }

    #[test]
    fn friendly_messages_cover_every_documented_code() {
        for code in [
            "denied",
            "invite_required",
            "invite_invalid",
            "disabled",
            "max_users",
            "server",
        ] {
            assert!(!redirect_error(code).is_empty());
        }
        assert!(redirect_error("invite_required").contains("invite code"));
        assert!(redirect_error("invite_invalid").contains("not valid"));
        assert!(redirect_error("max_users").contains("member limit"));
        assert_eq!(redirect_error("surprise"), redirect_error("server"));
        assert!(start_error(&ApiError::Http(403)).contains("not enabled"));
        assert!(start_error(&ApiError::Http(404)).contains("Update the server"));
        assert!(start_error(&ApiError::Http(429)).contains("Wait a minute"));
        assert!(start_error(&ApiError::Http(502)).contains("Try again"));
        assert_eq!(
            start_error(&ApiError::Transport),
            ApiError::Transport.sign_in_text()
        );
        assert!(handoff_error(&ApiError::Http(400)).contains("expired"));
        assert!(handoff_error(&ApiError::Http(500)).contains("Try again"));
    }

    #[test]
    fn invite_codes_are_trimmed_and_bounded() {
        assert_eq!(clean_invite("  ").unwrap(), None);
        assert_eq!(clean_invite(" abc ").unwrap().as_deref(), Some("abc"));
        assert!(clean_invite("a\nb").is_err());
        assert!(clean_invite(&"a".repeat(257)).is_err());
    }

    #[test]
    fn a_full_sign_in_trades_the_handoff_for_a_session() {
        let (result, requests) = drive(
            vec![(200, START_OK.into()), (200, handoff_ok())],
            Some("invite-123"),
            |state| format!("handoff=one-time-code&state={state}"),
        );
        let signed_in = result.expect("signed in");
        assert_eq!(
            signed_in.token.expose_secret(),
            "synthetic-session-token-0123456789abcdef"
        );
        assert_eq!(signed_in.user.label(), "Ada");
        assert_eq!(signed_in.user.role, "member");

        assert!(requests[0].starts_with("POST /v1/auth/login/start "));
        assert!(requests[1].starts_with("POST /v1/auth/handoff "));
        for request in &requests {
            assert!(
                !request.to_lowercase().contains("authorization:"),
                "sign-in routes carry no bearer token"
            );
        }
        let start = body_of(&requests[0]);
        assert_eq!(start["provider"], "microsoft");
        assert_eq!(start["invite_code"], "invite-123");
        let return_to = start["return_to"].as_str().unwrap();
        assert!(crate::valid_handoff_return_to(return_to), "{return_to}");
        let state = start["client_state"].as_str().unwrap();
        assert!(state.len() >= 16);
        let challenge = start["handoff_challenge"].as_str().unwrap();
        assert_eq!(challenge.len(), 43);

        // The verifier sent to /handoff must hash to the challenge sent to
        // /login/start (S256), and the redeemed code is the redirected one.
        let handoff = body_of(&requests[1]);
        assert_eq!(handoff["handoff"], "one-time-code");
        let verifier = handoff["verifier"].as_str().unwrap();
        use sha2::{Digest, Sha256};
        let expected = {
            let digest = Sha256::digest(verifier.as_bytes());
            crate::oauth::base64url(&digest)
        };
        assert_eq!(expected, challenge);
    }

    #[test]
    fn no_invite_code_is_omitted_from_the_request() {
        let (result, requests) = drive(
            vec![(200, START_OK.into()), (200, handoff_ok())],
            None,
            |state| format!("handoff=c&state={state}"),
        );
        assert!(result.is_ok());
        assert!(body_of(&requests[0]).get("invite_code").is_none());
    }

    #[test]
    fn a_server_error_redirect_becomes_a_friendly_message() {
        for (code, needle) in [
            ("denied", "cancelled or refused"),
            ("invite_required", "needs an invite code"),
            ("invite_invalid", "not valid"),
            ("disabled", "disabled"),
            ("max_users", "member limit"),
            ("server", "Try again"),
        ] {
            let (result, requests) = drive(vec![(200, START_OK.into())], None, move |state| {
                format!("error={code}&state={state}")
            });
            match result {
                Err(SignInFailure::Message(message)) => {
                    assert!(message.contains(needle), "{code}: {message}");
                }
                other => panic!("{code}: unexpected {}", other.is_ok()),
            }
            assert_eq!(requests.len(), 1, "no handoff after an error redirect");
        }
    }

    #[test]
    fn a_burned_handoff_asks_to_start_again() {
        let (result, _) = drive(
            vec![
                (200, START_OK.into()),
                (400, r#"{"error":"invalid handoff"}"#.into()),
            ],
            None,
            |state| format!("handoff=c&state={state}"),
        );
        match result {
            Err(SignInFailure::Message(message)) => assert!(message.contains("expired")),
            _ => panic!("expected a message"),
        }
    }

    #[test]
    fn start_failures_map_to_friendly_messages() {
        for (status, needle) in [
            (400, "Update SeraBot"),
            (403, "not enabled"),
            (404, "Update the server"),
            (422, "invite code"),
            (500, "Try again"),
        ] {
            let (endpoint, _requests, server) = scripted(vec![(status, "{}".into())]);
            let result = run(
                endpoint,
                None,
                &AtomicBool::new(false),
                Duration::from_secs(1),
                &|_| panic!("the browser must not open"),
                &|| {},
            );
            server.join().unwrap();
            match result {
                Err(SignInFailure::Message(message)) => {
                    assert!(message.contains(needle), "{status}: {message}");
                }
                _ => panic!("{status}: expected a message"),
            }
        }
    }

    #[test]
    fn a_missing_browser_is_reported_and_a_cancel_stops_the_wait() {
        let (endpoint, _requests, server) = scripted(vec![(200, START_OK.into())]);
        let result = run(
            endpoint,
            None,
            &AtomicBool::new(false),
            Duration::from_secs(1),
            &|_| false,
            &|| {},
        );
        server.join().unwrap();
        match result {
            Err(SignInFailure::Message(message)) => assert!(message.contains("browser")),
            _ => panic!("expected a message"),
        }

        let (endpoint, _requests, server) = scripted(vec![(200, START_OK.into())]);
        let cancel = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&cancel);
        let result = run(
            endpoint,
            None,
            &cancel,
            Duration::from_secs(10),
            &move |_| {
                flag.store(true, std::sync::atomic::Ordering::SeqCst);
                true
            },
            &|| {},
        );
        server.join().unwrap();
        assert!(matches!(result, Err(SignInFailure::Cancelled)));
    }

    #[test]
    fn a_wait_with_no_redirect_times_out() {
        let (endpoint, _requests, server) = scripted(vec![(200, START_OK.into())]);
        let result = run(
            endpoint,
            None,
            &AtomicBool::new(false),
            Duration::from_millis(200),
            &|_| true,
            &|| {},
        );
        server.join().unwrap();
        match result {
            Err(SignInFailure::Message(message)) => assert!(message.contains("timed out")),
            _ => panic!("expected a timeout message"),
        }
    }
}
