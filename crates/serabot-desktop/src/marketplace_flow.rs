//! Sign-in for a marketplace plugin (system browser and loopback) and for the
//! native GitHub connector (one-time device code), run beside the screen.
//!
//! Both runners report with the same [`FlowMsg`] the provider sign-in uses, so
//! the screen folds them into one [`crate::provider_flow::ProviderFlow`] and
//! draws the same panel: the page, the code, a countdown, a clear end state.
//!
//! The plugin leg is the provider browser leg with a different server half: the
//! server runs discovery, keeps the PKCE verifier and exchanges the code, so
//! this side only opens the consent page, listens on the loopback address the
//! server named (never one a plugin named) and hands back the code. No token
//! or verifier is ever here. The GitHub leg only shows the code and asks the
//! server how the poll stands.

use crate::api::{ApiClient, ApiError, SigninStarted, SigninStatus};
use crate::marketplace::{self, GithubDeviceStatus};
use crate::oauth;
use crate::provider_flow::{FlowEnd, FlowMsg, api_end, denied_text, oauth_end, poll_device};
use std::sync::atomic::AtomicBool;
use std::time::Duration;

/// How long a plugin sign-in waits for the consent page to come back. The
/// server forgets an unfinished sign-in after ten minutes.
const PLUGIN_WAIT: Duration = Duration::from_secs(600);

/// A sentence for a failed marketplace call during sign-in.
fn plugin_end(error: ApiError) -> FlowEnd {
    match error {
        ApiError::Http(404) => FlowEnd::Failed(
            "Install the plugin first, or update the server: it has no sign-in for this plugin."
                .to_owned(),
        ),
        ApiError::Http(502..=504) => FlowEnd::Failed(
            "The service did not answer the sign-in request. Try again in a moment.".to_owned(),
        ),
        ApiError::Http(403) => {
            FlowEnd::Failed("This plugin may not be signed in to from here.".to_owned())
        }
        other => api_end(other),
    }
}

/// Run a plugin's browser sign-in. `renew` says the account already exists and
/// is being signed in again.
#[allow(clippy::too_many_arguments)]
pub fn run_plugin_signin(
    client: &ApiClient,
    slug: &str,
    account_id: &str,
    label: Option<&str>,
    renew: bool,
    cancel: &AtomicBool,
    report: &dyn Fn(FlowMsg),
    open: &dyn Fn(&str) -> bool,
) -> FlowEnd {
    let started = match client.marketplace_oauth_start(slug, account_id, label) {
        Ok(started) => started,
        Err(error) => return plugin_end(error),
    };
    let Some(port) = marketplace::loopback_port(&started.redirect_uri) else {
        return FlowEnd::Failed("The server did not name a loopback address.".to_owned());
    };
    let Ok(listener) = oauth::bind_loopback_fixed(port) else {
        return FlowEnd::Failed(format!(
            "Could not listen on port {port} for the redirect. Close what uses it, then try again."
        ));
    };
    let opened = open(&started.authorize_url);
    report(FlowMsg::Started {
        started: SigninStarted {
            flow_id: started.account_id.clone(),
            flow: "pkce".to_owned(),
            url: started.authorize_url.clone(),
            expires_in: PLUGIN_WAIT.as_secs(),
            user_code: None,
            interval: None,
            state: Some(started.state.clone()),
        },
        opened,
    });
    let code = match oauth::wait_for_callback_cancellable(
        &listener,
        &started.state,
        oauth::CALLBACK_PATH,
        PLUGIN_WAIT,
        cancel,
    ) {
        Ok(code) => code,
        Err(error) => return oauth_end(error),
    };
    drop(listener);
    match client.marketplace_oauth_complete(slug, &code, &started.state) {
        Ok(done) => FlowEnd::Done {
            account_id: done.account_id,
            created: !renew,
        },
        Err(error) => plugin_end(error),
    }
}

/// What GitHub's device poll answered, in the shape the shared poller reads.
fn github_status(status: GithubDeviceStatus) -> SigninStatus {
    match status.status.as_str() {
        "connected" => SigninStatus {
            status: "ok".to_owned(),
            account_id: Some(
                status
                    .login
                    .filter(|login| marketplace::valid_account_id(&login.to_ascii_lowercase()))
                    .unwrap_or_else(|| "github".to_owned()),
            ),
            created: true,
            reason: None,
        },
        "denied" => SigninStatus {
            status: "denied".to_owned(),
            reason: status.reason,
            ..SigninStatus::default()
        },
        "expired" => SigninStatus {
            status: "expired".to_owned(),
            ..SigninStatus::default()
        },
        _ => SigninStatus {
            status: "pending".to_owned(),
            ..SigninStatus::default()
        },
    }
}

/// Run GitHub's device sign-in: get the code, open GitHub's page, and poll the
/// server until GitHub answers.
pub fn run_github_device(
    client: &ApiClient,
    cancel: &AtomicBool,
    report: &dyn Fn(FlowMsg),
    open: &dyn Fn(&str) -> bool,
) -> FlowEnd {
    device_flow(client, cancel, report, open, &mut std::thread::sleep)
}

fn device_flow(
    client: &ApiClient,
    cancel: &AtomicBool,
    report: &dyn Fn(FlowMsg),
    open: &dyn Fn(&str) -> bool,
    sleep: &mut dyn FnMut(Duration),
) -> FlowEnd {
    let started = match client.github_device_start() {
        Ok(started) => started,
        Err(ApiError::Rejected(message)) => {
            return FlowEnd::Failed(crate::provider_accounts::plain_error(&message));
        }
        Err(ApiError::Http(404 | 409)) => {
            return FlowEnd::Failed(
                "GitHub sign-in is not set up on this server yet. Use a token instead.".to_owned(),
            );
        }
        Err(error) => return api_end(error),
    };
    let signin = SigninStarted {
        flow_id: "github".to_owned(),
        flow: "device".to_owned(),
        url: started.verification_uri.clone(),
        expires_in: started.expires_in,
        user_code: Some(started.user_code.clone()),
        interval: (started.interval > 0).then_some(started.interval),
        state: None,
    };
    let opened = open(&signin.url);
    report(FlowMsg::Started {
        started: signin.clone(),
        opened,
    });
    let end = poll_device(
        &signin,
        cancel,
        &mut || client.github_device_status().map(github_status),
        sleep,
        report,
    );
    match end {
        FlowEnd::Denied(_) => FlowEnd::Denied(denied_text(Some("denied"))),
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use secrecy::SecretString;
    use std::cell::RefCell;
    use std::collections::VecDeque;
    use std::io::{Read, Write};
    use std::net::{TcpListener, TcpStream};
    use std::sync::atomic::Ordering;
    use std::sync::{Arc, Mutex};
    use url::Url;

    /// One request the mock server saw: method, path and body.
    type Hit = (String, String, String);

    /// A loopback HTTP server that answers from a script and records requests.
    struct Mock {
        port: u16,
        hits: Arc<Mutex<Vec<Hit>>>,
        stop: Arc<AtomicBool>,
    }

    impl Drop for Mock {
        fn drop(&mut self) {
            self.stop.store(true, Ordering::SeqCst);
        }
    }

    impl Mock {
        fn start(answer: impl Fn(&str, &str, &str) -> (u16, String) + Send + 'static) -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            listener.set_nonblocking(true).unwrap();
            let port = listener.local_addr().unwrap().port();
            let hits: Arc<Mutex<Vec<Hit>>> = Arc::default();
            let stop = Arc::new(AtomicBool::new(false));
            let (thread_hits, thread_stop) = (Arc::clone(&hits), Arc::clone(&stop));
            std::thread::spawn(move || {
                while !thread_stop.load(Ordering::SeqCst) {
                    match listener.accept() {
                        Ok((mut stream, _)) => {
                            let (method, path, body) = read_request(&mut stream);
                            let (status, reply) = answer(&method, &path, &body);
                            thread_hits.lock().unwrap().push((method, path, body));
                            let _ = write!(
                                stream,
                                "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{reply}",
                                reply.len()
                            );
                        }
                        Err(_) => std::thread::sleep(Duration::from_millis(5)),
                    }
                }
            });
            Self { port, hits, stop }
        }

        fn client(&self) -> ApiClient {
            let endpoint = Url::parse(&format!("http://127.0.0.1:{}/", self.port)).unwrap();
            ApiClient::new(endpoint, SecretString::new("t".repeat(40))).unwrap()
        }

        fn hits_to(&self, path_end: &str) -> Vec<Hit> {
            self.hits
                .lock()
                .unwrap()
                .iter()
                .filter(|(_, path, _)| path.ends_with(path_end))
                .cloned()
                .collect()
        }
    }

    fn read_request(stream: &mut TcpStream) -> Hit {
        stream.set_nonblocking(false).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut head = Vec::new();
        let mut byte = [0u8];
        while !head.ends_with(b"\r\n\r\n") {
            if stream.read_exact(&mut byte).is_err() {
                break;
            }
            head.push(byte[0]);
        }
        let head = String::from_utf8_lossy(&head).into_owned();
        let mut first = head.lines().next().unwrap_or("").split(' ');
        let method = first.next().unwrap_or("").to_owned();
        let path = first.next().unwrap_or("").to_owned();
        let length = head
            .lines()
            .find_map(|line| {
                let (name, value) = line.split_once(':')?;
                name.eq_ignore_ascii_case("content-length")
                    .then(|| value.trim().parse::<usize>().ok())?
            })
            .unwrap_or(0);
        let mut body = vec![0u8; length];
        let _ = stream.read_exact(&mut body);
        (method, path, String::from_utf8_lossy(&body).into_owned())
    }

    fn free_port() -> u16 {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.local_addr().unwrap().port()
    }

    const STATE: &str = "stateabcdefghijkl";

    /// A server that plays the marketplace half of a plugin sign-in.
    fn plugin_server(callback_port: u16, complete: u16) -> Mock {
        Mock::start(move |method, path, _| match (method, path) {
            ("POST", "/v1/marketplace/plugins/canva/oauth/start") => (
                200,
                serde_json::json!({
                    "authorize_url": "https://auth.example/authorize?client=synthetic",
                    "state": STATE,
                    "account_id": "default",
                    "redirect_uri": format!("http://127.0.0.1:{callback_port}/oauth-callback"),
                })
                .to_string(),
            ),
            ("POST", "/v1/marketplace/plugins/canva/oauth/complete") => (
                complete,
                r#"{"slug":"canva","account_id":"default"}"#.to_owned(),
            ),
            _ => (404, "{}".to_owned()),
        })
    }

    /// Plays the browser: sends each query to the loopback callback in turn.
    fn browser(port: u16, queries: Vec<String>) -> impl Fn(&str) -> bool {
        move |_url: &str| {
            let queries = queries.clone();
            std::thread::spawn(move || {
                for query in queries {
                    if let Ok(mut stream) = TcpStream::connect(("127.0.0.1", port)) {
                        let _ = write!(
                            stream,
                            "GET /oauth-callback?{query} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n\r\n"
                        );
                        let mut sink = Vec::new();
                        let _ = stream.read_to_end(&mut sink);
                    }
                }
            });
            true
        }
    }

    #[test]
    fn a_plugin_sign_in_opens_the_page_waits_for_the_redirect_and_hands_the_code_over() {
        let port = free_port();
        let server = plugin_server(port, 200);
        let seen: RefCell<Vec<FlowMsg>> = RefCell::new(Vec::new());
        let end = run_plugin_signin(
            &server.client(),
            "canva",
            "default",
            Some("Work"),
            false,
            &AtomicBool::new(false),
            &|msg| seen.borrow_mut().push(msg),
            &browser(port, vec![format!("code=synthetic-code&state={STATE}")]),
        );
        assert_eq!(
            end,
            FlowEnd::Done {
                account_id: "default".into(),
                created: true
            }
        );
        let messages = seen.borrow();
        let Some(FlowMsg::Started { started, opened }) = messages.first() else {
            panic!("the page is reported first");
        };
        assert!(*opened);
        assert_eq!(
            started.url,
            "https://auth.example/authorize?client=synthetic"
        );
        assert!(started.user_code.is_none(), "a browser sign-in has no code");
        // The start carried the account and label; the server got the code back.
        let start = &server.hits_to("/oauth/start")[0].2;
        assert!(start.contains("\"account_id\":\"default\"") && start.contains("Work"));
        let complete = &server.hits_to("/oauth/complete");
        assert_eq!(complete.len(), 1);
        assert!(complete[0].2.contains("synthetic-code") && complete[0].2.contains(STATE));
    }

    #[test]
    fn signing_in_an_existing_account_again_is_not_reported_as_added() {
        let port = free_port();
        let server = plugin_server(port, 200);
        let end = run_plugin_signin(
            &server.client(),
            "canva",
            "default",
            None,
            true,
            &AtomicBool::new(false),
            &|_| {},
            &browser(port, vec![format!("code=c1&state={STATE}")]),
        );
        assert_eq!(
            end,
            FlowEnd::Done {
                account_id: "default".into(),
                created: false
            }
        );
    }

    #[test]
    fn a_redirect_with_the_wrong_state_ends_the_sign_in_and_hands_nothing_over() {
        let port = free_port();
        let server = plugin_server(port, 200);
        let end = run_plugin_signin(
            &server.client(),
            "canva",
            "default",
            None,
            false,
            &AtomicBool::new(false),
            &|_| {},
            &browser(
                port,
                vec!["code=forged&state=otherstateotherstate".to_owned()],
            ),
        );
        let FlowEnd::Failed(text) = end else {
            panic!("a forged redirect fails: {end:?}");
        };
        assert!(text.contains("did not match"), "{text}");
        assert!(server.hits_to("/oauth/complete").is_empty());
    }

    #[test]
    fn backing_out_closes_the_listener_and_never_calls_complete() {
        let port = free_port();
        let server = plugin_server(port, 200);
        let cancel = AtomicBool::new(false);
        let end = run_plugin_signin(
            &server.client(),
            "canva",
            "default",
            None,
            false,
            &cancel,
            &|_| {},
            // The member cancels instead of finishing in the browser.
            &|_| {
                cancel.store(true, Ordering::SeqCst);
                true
            },
        );
        assert_eq!(end, FlowEnd::Cancelled);
        assert!(server.hits_to("/oauth/complete").is_empty());
        // The port is free again at once.
        assert!(TcpListener::bind(("127.0.0.1", port)).is_ok());
    }

    #[test]
    fn a_busy_port_a_missing_plugin_and_a_refused_code_are_each_said_plainly() {
        // Port busy: the redirect cannot be heard, so the page is never opened.
        let held = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = held.local_addr().unwrap().port();
        let server = plugin_server(port, 200);
        let opened = std::cell::Cell::new(false);
        let end = run_plugin_signin(
            &server.client(),
            "canva",
            "default",
            None,
            false,
            &AtomicBool::new(false),
            &|_| {},
            &|_| {
                opened.set(true);
                true
            },
        );
        let FlowEnd::Failed(text) = end else {
            panic!("a busy port fails");
        };
        assert!(text.contains(&port.to_string()), "{text}");
        assert!(!opened.get());

        // Not installed: the server answers 404 to the start.
        let missing = Mock::start(|_, _, _| (404, "{}".to_owned()));
        let end = run_plugin_signin(
            &missing.client(),
            "canva",
            "default",
            None,
            false,
            &AtomicBool::new(false),
            &|_| {},
            &|_| true,
        );
        let FlowEnd::Failed(text) = end else {
            panic!("a missing plugin fails");
        };
        assert!(text.contains("Install the plugin"), "{text}");

        // The service refuses the code: a plain failure, nothing stored here.
        let port = free_port();
        let refusing = plugin_server(port, 502);
        let end = run_plugin_signin(
            &refusing.client(),
            "canva",
            "default",
            None,
            false,
            &AtomicBool::new(false),
            &|_| {},
            &browser(port, vec![format!("code=c1&state={STATE}")]),
        );
        let FlowEnd::Failed(text) = end else {
            panic!("a refused code fails");
        };
        assert!(text.contains("did not answer"), "{text}");
    }

    #[test]
    fn a_start_that_names_another_address_is_never_listened_on() {
        let server = Mock::start(|_, _, _| {
            (
                200,
                serde_json::json!({
                    "authorize_url": "https://auth.example/a",
                    "state": STATE,
                    "account_id": "default",
                    "redirect_uri": "http://evil.example:9/oauth-callback",
                })
                .to_string(),
            )
        });
        let end = run_plugin_signin(
            &server.client(),
            "canva",
            "default",
            None,
            false,
            &AtomicBool::new(false),
            &|_| {},
            &|_| panic!("the page must not open"),
        );
        assert!(matches!(end, FlowEnd::Failed(_)), "{end:?}");
    }

    /// A server that plays GitHub's device sign-in, answering the polls in turn.
    fn github_server(polls: Vec<&'static str>) -> Mock {
        let polls = Mutex::new(VecDeque::from(polls));
        Mock::start(move |method, path, _| {
            match (method, path) {
            ("POST", "/v1/connectors/github/device/start") => (
                200,
                r#"{"user_code":"ABCD-1234","verification_uri":"https://github.com/login/device","expires_in":900,"interval":5}"#.to_owned(),
            ),
            ("GET", "/v1/connectors/github/device/status") => {
                let next = polls.lock().unwrap().pop_front().unwrap_or("pending");
                (200, next.to_owned())
            }
            _ => (404, "{}".to_owned()),
        }
        })
    }

    fn run_github(server: &Mock, cancel: &AtomicBool) -> (FlowEnd, Vec<FlowMsg>) {
        let seen: RefCell<Vec<FlowMsg>> = RefCell::new(Vec::new());
        let end = device_flow(
            &server.client(),
            cancel,
            &|msg| seen.borrow_mut().push(msg),
            &|_| true,
            &mut |_| {},
        );
        (end, seen.into_inner())
    }

    #[test]
    fn github_shows_the_code_polls_while_pending_and_ends_connected() {
        let server = github_server(vec![
            r#"{"status":"pending"}"#,
            r#"{"status":"pending"}"#,
            r#"{"status":"connected","login":"octo-cat"}"#,
        ]);
        let (end, messages) = run_github(&server, &AtomicBool::new(false));
        assert_eq!(
            end,
            FlowEnd::Done {
                account_id: "octo-cat".into(),
                created: true
            }
        );
        let Some(FlowMsg::Started { started, .. }) = messages.first() else {
            panic!("the code is reported first");
        };
        assert_eq!(started.user_code.as_deref(), Some("ABCD-1234"));
        assert_eq!(started.url, "https://github.com/login/device");
        assert_eq!(started.interval, Some(5));
        assert_eq!(
            messages
                .iter()
                .filter(|msg| matches!(msg, FlowMsg::Pending))
                .count(),
            2
        );
        assert_eq!(server.hits_to("/device/status").len(), 3);
    }

    #[test]
    fn github_denied_expired_and_cancelled_end_cleanly() {
        let denied = github_server(vec![r#"{"status":"denied","reason":"access_denied"}"#]);
        let (end, _) = run_github(&denied, &AtomicBool::new(false));
        let FlowEnd::Denied(text) = end else {
            panic!("denied: {end:?}");
        };
        assert!(text.contains("without approving"), "{text}");

        let expired = github_server(vec![r#"{"status":"expired"}"#]);
        assert_eq!(
            run_github(&expired, &AtomicBool::new(false)).0,
            FlowEnd::Expired
        );

        let cancel = AtomicBool::new(true);
        let waiting = github_server(vec![]);
        assert_eq!(run_github(&waiting, &cancel).0, FlowEnd::Cancelled);
    }

    #[test]
    fn github_without_an_app_says_to_use_a_token() {
        let server = Mock::start(|_, _, _| (409, "{}".to_owned()));
        let (end, messages) = run_github(&server, &AtomicBool::new(false));
        let FlowEnd::Failed(text) = end else {
            panic!("an unset app fails");
        };
        assert!(text.contains("token"), "{text}");
        assert!(messages.is_empty(), "no code is shown");
    }

    #[test]
    fn an_unusable_github_code_is_never_shown() {
        let server = Mock::start(|_, _, _| {
            (
                200,
                r#"{"user_code":"X","verification_uri":"http://github.com/login/device","expires_in":900,"interval":5}"#
                    .to_owned(),
            )
        });
        let (end, messages) = run_github(&server, &AtomicBool::new(false));
        assert!(matches!(end, FlowEnd::Failed(_)), "{end:?}");
        assert!(messages.is_empty());
    }

    #[test]
    fn the_status_mapper_reads_every_word_the_server_sends() {
        let word = |status: &str, login: Option<&str>| {
            github_status(GithubDeviceStatus {
                status: status.into(),
                login: login.map(str::to_owned),
                reason: None,
            })
        };
        assert_eq!(word("pending", None).status, "pending");
        assert_eq!(word("expired", None).status, "expired");
        assert_eq!(word("denied", None).status, "denied");
        assert_eq!(
            word("connected", None).account_id.as_deref(),
            Some("github")
        );
        // An odd login falls back instead of becoming an id.
        assert_eq!(
            word("connected", Some("a b<")).account_id.as_deref(),
            Some("github")
        );
        // A word it does not know keeps waiting.
        assert_eq!(word("surprise", None).status, "pending");
    }
}
