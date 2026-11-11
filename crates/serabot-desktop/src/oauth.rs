//! Browser sign-in helpers for OAuth providers.
//!
//! The console owns the browser leg of a PKCE sign-in: it makes the verifier,
//! listens on `127.0.0.1` for the redirect, and hands the code to the server.
//! The server holds the OAuth client secret and does the token exchange, so
//! tokens and the secret never reach this process.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use sha2::{Digest, Sha256};
use zeroize::Zeroizing;

pub const CALLBACK_PATH: &str = "/oauth-callback";
/// Where the server sends the browser after a native sign-in.
pub const AUTH_DONE_PATH: &str = "/auth-done";
const PREFERRED_PORT: u16 = 51121;
const MAX_REQUEST_HEAD: usize = 8 * 1024;
/// Requests the sign-in listener ignores (wrong path, state, or host) before it
/// gives up. A stray local process can knock on the port, but cannot keep the
/// sign-in waiting forever.
const MAX_IGNORED_REQUESTS: usize = 50;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OAuthError {
    /// The loopback listener could not be opened.
    Listen,
    /// No redirect arrived before the deadline.
    TimedOut,
    /// The provider reported an error, or the redirect carried no code.
    Denied,
    /// The redirect did not carry the state this sign-in started with.
    StateMismatch,
    /// The user cancelled the sign-in from the console.
    Cancelled,
    /// The listener received too many requests that were not the sign-in redirect.
    TooManyRequests,
}

impl std::fmt::Display for OAuthError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Listen => "Could not listen on 127.0.0.1 for the sign-in redirect.",
            Self::TimedOut => "Sign-in timed out. Start it again.",
            Self::Denied => "Sign-in was cancelled or refused.",
            Self::StateMismatch => "Sign-in redirect did not match this request. Start it again.",
            Self::Cancelled => "Sign-in was cancelled.",
            Self::TooManyRequests => {
                "Too many unexpected requests reached the sign-in listener. Start sign-in again."
            }
        })
    }
}

/// PKCE material for one sign-in. Dropped values are zeroed.
pub struct Pkce {
    pub verifier: Zeroizing<String>,
    pub challenge: String,
    pub state: String,
}

pub(crate) fn base64url(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let n = (u32::from(chunk[0]) << 16)
            | (u32::from(*chunk.get(1).unwrap_or(&0)) << 8)
            | u32::from(*chunk.get(2).unwrap_or(&0));
        out.push(ALPHABET[(n >> 18) as usize & 63] as char);
        out.push(ALPHABET[(n >> 12) as usize & 63] as char);
        if chunk.len() > 1 {
            out.push(ALPHABET[(n >> 6) as usize & 63] as char);
        }
        if chunk.len() > 2 {
            out.push(ALPHABET[n as usize & 63] as char);
        }
    }
    out
}

pub fn new_pkce() -> Pkce {
    let mut bytes = Zeroizing::new([0u8; 32]);
    bytes[..16].copy_from_slice(uuid::Uuid::new_v4().as_bytes());
    bytes[16..].copy_from_slice(uuid::Uuid::new_v4().as_bytes());
    let verifier = Zeroizing::new(base64url(&bytes[..]));
    let challenge = base64url(&Sha256::digest(verifier.as_bytes()));
    Pkce {
        verifier,
        challenge,
        state: uuid::Uuid::new_v4().simple().to_string(),
    }
}

/// Listen on the loopback interface only: the documented port first, then any
/// free one.
pub fn bind_loopback() -> Result<(TcpListener, u16), OAuthError> {
    let listener = TcpListener::bind(("127.0.0.1", PREFERRED_PORT))
        .or_else(|_| TcpListener::bind(("127.0.0.1", 0)))
        .map_err(|_| OAuthError::Listen)?;
    let port = listener
        .local_addr()
        .map_err(|_| OAuthError::Listen)?
        .port();
    Ok((listener, port))
}

/// Bind exactly `port` on loopback, with no fallback. Used when the redirect
/// URI is fixed in a publicly served client metadata document (CIMD), so the
/// consent response must arrive on that exact port.
pub fn bind_loopback_fixed(port: u16) -> Result<TcpListener, OAuthError> {
    TcpListener::bind(("127.0.0.1", port)).map_err(|_| OAuthError::Listen)
}

pub fn redirect_uri(port: u16) -> String {
    format!("http://127.0.0.1:{port}{CALLBACK_PATH}")
}

/// Wait for the provider redirect and return its authorization code. Requests
/// for any other path are answered 404 and ignored; a wrong state is refused.
pub fn wait_for_callback(
    listener: &TcpListener,
    expected_state: &str,
    timeout: Duration,
) -> Result<Zeroizing<String>, OAuthError> {
    listener
        .set_nonblocking(true)
        .map_err(|_| OAuthError::Listen)?;
    let deadline = Instant::now() + timeout;
    loop {
        match listener.accept() {
            Ok((stream, _)) => {
                if let Some(result) = handle_connection(stream, expected_state) {
                    return result;
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                if Instant::now() >= deadline {
                    return Err(OAuthError::TimedOut);
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(_) => return Err(OAuthError::Listen),
        }
    }
}

fn respond(stream: &mut TcpStream, status: &str, body: &str) {
    let _ = write!(
        stream,
        "HTTP/1.1 {status}\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nCache-Control: no-store\r\nReferrer-Policy: no-referrer\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let _ = stream.flush();
}

/// Read the request head of one loopback connection and return its target as
/// a URL. Anything that is not a well-formed `GET` is answered 400.
fn read_request_url(stream: &mut TcpStream) -> Option<url::Url> {
    let _ = stream.set_nonblocking(false);
    let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
    let mut buffer = vec![0u8; MAX_REQUEST_HEAD];
    let mut filled = 0;
    while filled < buffer.len() {
        match stream.read(&mut buffer[filled..]) {
            Ok(0) => break,
            Ok(n) => {
                filled += n;
                if buffer[..filled].windows(4).any(|w| w == b"\r\n\r\n") {
                    break;
                }
            }
            Err(_) => break,
        }
    }
    let head = String::from_utf8_lossy(&buffer[..filled]).into_owned();
    let target = head
        .lines()
        .next()
        .and_then(|line| line.strip_prefix("GET "))
        .and_then(|rest| rest.split(' ').next());
    let Some(target) = target else {
        respond(stream, "400 Bad Request", "Bad request");
        return None;
    };
    let host = head
        .lines()
        .skip(1)
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.trim()
                .eq_ignore_ascii_case("host")
                .then(|| value.trim().to_owned())
        })
        .unwrap_or_default();
    let port = stream.local_addr().map(|addr| addr.port()).unwrap_or(0);
    if !host_allowed(&host, port) {
        respond(stream, "400 Bad Request", "Bad request");
        return None;
    }
    let Ok(url) = url::Url::parse(&format!("http://127.0.0.1{target}")) else {
        respond(stream, "400 Bad Request", "Bad request");
        return None;
    };
    Some(url)
}

/// Only the loopback names the redirect can carry, on the port this listener
/// owns. A `Host` naming any other site means a browser page on another origin
/// (DNS rebinding) or a crafted request, and it is refused before parsing.
fn host_allowed(host: &str, port: u16) -> bool {
    host == format!("127.0.0.1:{port}") || host == format!("localhost:{port}")
}

/// What the browser brought back to `/auth-done`.
#[derive(Debug, PartialEq, Eq)]
pub enum AuthDone {
    /// The one-time handoff code; the PKCE verifier redeems it.
    Handoff(Zeroizing<String>),
    /// The server's short error code (`denied`, `invite_required`, ...).
    Failed(String),
}

/// The loopback return address for one native sign-in.
pub fn handoff_return_to(port: u16) -> String {
    format!("http://127.0.0.1:{port}{AUTH_DONE_PATH}")
}

/// Wait for the browser to come back to `/auth-done`. Other paths get 404; a
/// request whose `state` is not this sign-in's is answered with the failure
/// page and ignored, so a stray local request cannot end the wait. The wait
/// also ends when `cancel` is set.
pub fn wait_for_auth_done(
    listener: &TcpListener,
    expected_state: &str,
    timeout: Duration,
    cancel: &AtomicBool,
) -> Result<AuthDone, OAuthError> {
    listener
        .set_nonblocking(true)
        .map_err(|_| OAuthError::Listen)?;
    let deadline = Instant::now() + timeout;
    let mut ignored = 0usize;
    loop {
        if cancel.load(Ordering::SeqCst) {
            return Err(OAuthError::Cancelled);
        }
        match listener.accept() {
            Ok((stream, _)) => {
                if let Some(done) = handle_auth_done(stream, expected_state) {
                    return Ok(done);
                }
                ignored += 1;
                if ignored >= MAX_IGNORED_REQUESTS {
                    return Err(OAuthError::TooManyRequests);
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                if Instant::now() >= deadline {
                    return Err(OAuthError::TimedOut);
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(_) => return Err(OAuthError::Listen),
        }
    }
}

const SERVER_ERROR_CODES: [&str; 6] = [
    "denied",
    "invite_required",
    "invite_invalid",
    "disabled",
    "max_users",
    "server",
];

fn handle_auth_done(mut stream: TcpStream, expected_state: &str) -> Option<AuthDone> {
    let url = read_request_url(&mut stream)?;
    if url.path() != AUTH_DONE_PATH {
        respond(&mut stream, "404 Not Found", "Not found");
        return None;
    }
    let mut handoff = None;
    let mut state = None;
    let mut error = None;
    for (key, value) in url.query_pairs() {
        match key.as_ref() {
            "handoff" => handoff = Some(Zeroizing::new(value.into_owned())),
            "state" => state = Some(value.into_owned()),
            "error" => error = Some(value.into_owned()),
            _ => {}
        }
    }
    let state_ok = state.as_deref().is_some_and(|got| got == expected_state);
    let done = if !state_ok {
        None
    } else if let Some(code) = error {
        let code = if SERVER_ERROR_CODES.contains(&code.as_str()) {
            code
        } else {
            "server".to_owned()
        };
        Some(AuthDone::Failed(code))
    } else {
        match handoff {
            Some(code) if !code.is_empty() && code.len() <= 512 => Some(AuthDone::Handoff(code)),
            _ => Some(AuthDone::Failed("server".to_owned())),
        }
    };
    let page = match &done {
        Some(AuthDone::Handoff(_)) => PAGE_SIGNED_IN,
        _ => PAGE_FAILED,
    };
    respond(&mut stream, "200 OK", page);
    done
}

const PAGE_SIGNED_IN: &str = "<!doctype html><meta charset=\"utf-8\"><title>SeraBot</title><body style=\"font-family:system-ui;margin:3rem\"><h2>You are signed in</h2><p>You can close this tab and return to SeraBot.</p></body>";
const PAGE_FAILED: &str = "<!doctype html><meta charset=\"utf-8\"><title>SeraBot</title><body style=\"font-family:system-ui;margin:3rem\"><h2>Sign-in did not finish</h2><p>You can close this tab. Return to SeraBot to see what happened.</p></body>";

fn handle_connection(
    mut stream: TcpStream,
    expected_state: &str,
) -> Option<Result<Zeroizing<String>, OAuthError>> {
    let url = read_request_url(&mut stream)?;
    if url.path() != CALLBACK_PATH {
        respond(&mut stream, "404 Not Found", "Not found");
        return None;
    }
    let mut code = None;
    let mut state = None;
    let mut failed = false;
    for (key, value) in url.query_pairs() {
        match key.as_ref() {
            "code" => code = Some(Zeroizing::new(value.into_owned())),
            "state" => state = Some(value.into_owned()),
            "error" => failed = true,
            _ => {}
        }
    }
    let state_ok = state.as_deref().is_some_and(|got| got == expected_state);
    let outcome = if !state_ok {
        Err(OAuthError::StateMismatch)
    } else if failed {
        Err(OAuthError::Denied)
    } else {
        match code {
            Some(code) if !code.is_empty() && code.len() <= 2_048 => Ok(code),
            _ => Err(OAuthError::Denied),
        }
    };
    let page = match &outcome {
        Ok(_) => {
            "<!doctype html><title>SeraBot</title><body style=\"font-family:system-ui;margin:3rem\"><h2>Signed in</h2><p>You can close this tab and return to SeraBot.</p></body>"
        }
        Err(_) => {
            "<!doctype html><title>SeraBot</title><body style=\"font-family:system-ui;margin:3rem\"><h2>Sign-in failed</h2><p>Return to SeraBot and start again.</p></body>"
        }
    };
    respond(&mut stream, "200 OK", page);
    Some(outcome)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn get(port: u16, target: &str) {
        let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
        write!(
            stream,
            "GET {target} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\n\r\n"
        )
        .unwrap();
        let mut sink = String::new();
        let _ = stream.read_to_string(&mut sink);
    }

    #[test]
    fn pkce_matches_the_rfc_shape() {
        let pkce = new_pkce();
        assert_eq!(pkce.verifier.len(), 43);
        assert!(
            pkce.verifier
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        );
        assert_eq!(pkce.challenge.len(), 43);
        assert_eq!(
            pkce.challenge,
            base64url(&Sha256::digest(pkce.verifier.as_bytes()))
        );
        assert!(pkce.state.len() >= 16);
        assert_ne!(new_pkce().verifier.as_str(), pkce.verifier.as_str());
    }

    #[test]
    fn base64url_matches_known_vectors() {
        assert_eq!(base64url(b""), "");
        assert_eq!(base64url(b"f"), "Zg");
        assert_eq!(base64url(b"fo"), "Zm8");
        assert_eq!(base64url(b"foo"), "Zm9v");
        assert_eq!(base64url(&[0xfb, 0xff]), "-_8");
    }

    #[test]
    fn callback_returns_the_code_after_ignoring_other_paths() {
        let (listener, port) = bind_loopback().unwrap();
        let client = std::thread::spawn(move || {
            get(port, "/favicon.ico");
            get(
                port,
                "/oauth-callback?code=synthetic-code&state=expected-state",
            );
        });
        let code = wait_for_callback(&listener, "expected-state", Duration::from_secs(5)).unwrap();
        assert_eq!(code.as_str(), "synthetic-code");
        client.join().unwrap();
    }

    #[test]
    fn callback_refuses_a_wrong_state_and_a_provider_error() {
        for (query, expected) in [
            ("code=c&state=other", OAuthError::StateMismatch),
            ("code=c", OAuthError::StateMismatch),
            (
                "error=access_denied&state=expected-state",
                OAuthError::Denied,
            ),
            ("state=expected-state", OAuthError::Denied),
        ] {
            let (listener, port) = bind_loopback().unwrap();
            let target = format!("/oauth-callback?{query}");
            let client = std::thread::spawn(move || get(port, &target));
            let error = wait_for_callback(&listener, "expected-state", Duration::from_secs(5))
                .err()
                .unwrap();
            assert_eq!(error, expected, "{query}");
            client.join().unwrap();
        }
    }

    #[test]
    fn callback_times_out_without_a_redirect() {
        let (listener, _) = bind_loopback().unwrap();
        let error = wait_for_callback(&listener, "s", Duration::from_millis(200))
            .err()
            .unwrap();
        assert_eq!(error, OAuthError::TimedOut);
    }

    fn wait_done(listener: &TcpListener, cancel: &AtomicBool) -> Result<AuthDone, OAuthError> {
        wait_for_auth_done(listener, "expected-state", Duration::from_secs(5), cancel)
    }

    #[test]
    fn auth_done_returns_the_handoff_and_ignores_other_paths_and_wrong_state() {
        let (listener, port) = bind_loopback().unwrap();
        let client = std::thread::spawn(move || {
            get(port, "/favicon.ico");
            get(port, "/oauth-callback?handoff=nope&state=expected-state");
            get(port, "/auth-done?handoff=stray&state=other");
            get(
                port,
                "/auth-done?handoff=synthetic-handoff&state=expected-state",
            );
        });
        let done = wait_done(&listener, &AtomicBool::new(false)).unwrap();
        assert_eq!(
            done,
            AuthDone::Handoff(Zeroizing::new("synthetic-handoff".into()))
        );
        client.join().unwrap();
    }

    #[test]
    fn auth_done_reports_known_server_errors_and_folds_unknown_ones() {
        for (query, expected) in [
            ("error=denied&state=expected-state", "denied"),
            (
                "error=invite_required&state=expected-state",
                "invite_required",
            ),
            (
                "error=invite_invalid&state=expected-state",
                "invite_invalid",
            ),
            ("error=disabled&state=expected-state", "disabled"),
            ("error=max_users&state=expected-state", "max_users"),
            ("error=server&state=expected-state", "server"),
            ("error=%3Cscript%3E&state=expected-state", "server"),
            ("state=expected-state", "server"),
        ] {
            let (listener, port) = bind_loopback().unwrap();
            let target = format!("/auth-done?{query}");
            let client = std::thread::spawn(move || get(port, &target));
            let done = wait_done(&listener, &AtomicBool::new(false)).unwrap();
            assert_eq!(done, AuthDone::Failed(expected.into()), "{query}");
            client.join().unwrap();
        }
    }

    #[test]
    fn auth_done_page_says_the_tab_can_be_closed_and_stops_referrers() {
        let (listener, port) = bind_loopback().unwrap();
        let client = std::thread::spawn(move || {
            let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
            write!(
                stream,
                "GET /auth-done?handoff=h&state=expected-state HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\n\r\n"
            )
            .unwrap();
            let mut reply = String::new();
            let _ = stream.read_to_string(&mut reply);
            reply
        });
        wait_done(&listener, &AtomicBool::new(false)).unwrap();
        let reply = client.join().unwrap();
        assert!(reply.contains("You can close this tab"));
        assert!(reply.contains("Referrer-Policy: no-referrer"));
    }

    #[test]
    fn auth_done_can_be_cancelled_and_times_out() {
        let (listener, _) = bind_loopback().unwrap();
        let cancel = AtomicBool::new(true);
        assert_eq!(wait_done(&listener, &cancel), Err(OAuthError::Cancelled));
        let error = wait_for_auth_done(
            &listener,
            "s",
            Duration::from_millis(200),
            &AtomicBool::new(false),
        );
        assert_eq!(error, Err(OAuthError::TimedOut));
    }

    #[test]
    fn handoff_return_to_is_the_loopback_auth_done_address() {
        assert_eq!(handoff_return_to(51121), "http://127.0.0.1:51121/auth-done");
        assert!(crate::valid_handoff_return_to(&handoff_return_to(51121)));
    }

    #[test]
    fn auth_done_refuses_a_host_that_is_not_this_listener() {
        let (listener, port) = bind_loopback().unwrap();
        // A correct state with a foreign Host (DNS rebinding) must not complete the wait.
        let client = std::thread::spawn(move || {
            let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
            write!(
                stream,
                "GET /auth-done?handoff=h&state=expected-state HTTP/1.1\r\nHost: evil.example:{port}\r\n\r\n"
            )
            .unwrap();
            let mut reply = String::new();
            let _ = stream.read_to_string(&mut reply);
            reply
        });
        let result = wait_for_auth_done(
            &listener,
            "expected-state",
            Duration::from_millis(300),
            &AtomicBool::new(false),
        );
        assert_eq!(result, Err(OAuthError::TimedOut));
        assert!(client.join().unwrap().starts_with("HTTP/1.1 400"));
    }

    #[test]
    fn host_must_name_loopback_and_this_port() {
        assert!(host_allowed("127.0.0.1:51121", 51121));
        assert!(host_allowed("localhost:51121", 51121));
        assert!(!host_allowed("127.0.0.1", 51121));
        assert!(!host_allowed("127.0.0.1:51122", 51121));
        assert!(!host_allowed("evil.example:51121", 51121));
        assert!(!host_allowed("", 51121));
    }

    #[test]
    fn auth_done_gives_up_after_the_ignored_request_budget() {
        let (listener, port) = bind_loopback().unwrap();
        // The wait must run while the requests arrive: each `get` blocks on its reply.
        let waiter = std::thread::spawn(move || {
            wait_for_auth_done(
                &listener,
                "expected-state",
                Duration::from_secs(10),
                &AtomicBool::new(false),
            )
        });
        for _ in 0..MAX_IGNORED_REQUESTS {
            get(port, "/somewhere-else");
        }
        assert_eq!(waiter.join().unwrap(), Err(OAuthError::TooManyRequests));
    }

    #[test]
    fn listener_is_loopback_only() {
        let (listener, port) = bind_loopback().unwrap();
        assert!(listener.local_addr().unwrap().ip().is_loopback());
        assert_eq!(
            redirect_uri(port),
            format!("http://127.0.0.1:{port}/oauth-callback")
        );
    }
}
