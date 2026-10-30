//! Browser sign-in helpers for OAuth providers.
//!
//! The console owns the browser leg of a PKCE sign-in: it makes the verifier,
//! listens on `127.0.0.1` for the redirect, and hands the code to the server.
//! The server holds the OAuth client secret and does the token exchange, so
//! tokens and the secret never reach this process.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::time::{Duration, Instant};

use sha2::{Digest, Sha256};
use zeroize::Zeroizing;

pub const CALLBACK_PATH: &str = "/oauth-callback";
const PREFERRED_PORT: u16 = 51121;
const MAX_REQUEST_HEAD: usize = 8 * 1024;

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
}

impl std::fmt::Display for OAuthError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Listen => "Could not listen on 127.0.0.1 for the sign-in redirect.",
            Self::TimedOut => "Sign-in timed out. Start it again.",
            Self::Denied => "Sign-in was cancelled or refused.",
            Self::StateMismatch => "Sign-in redirect did not match this request. Start it again.",
        })
    }
}

/// PKCE material for one sign-in. Dropped values are zeroed.
pub struct Pkce {
    pub verifier: Zeroizing<String>,
    pub challenge: String,
    pub state: String,
}

fn base64url(bytes: &[u8]) -> String {
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
        "HTTP/1.1 {status}\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let _ = stream.flush();
}

fn handle_connection(
    mut stream: TcpStream,
    expected_state: &str,
) -> Option<Result<Zeroizing<String>, OAuthError>> {
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
        respond(&mut stream, "400 Bad Request", "Bad request");
        return None;
    };
    let Ok(url) = url::Url::parse(&format!("http://127.0.0.1{target}")) else {
        respond(&mut stream, "400 Bad Request", "Bad request");
        return None;
    };
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
        write!(stream, "GET {target} HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n").unwrap();
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
