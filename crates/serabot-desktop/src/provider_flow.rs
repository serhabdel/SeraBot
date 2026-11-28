//! Sign-in for a provider account: a browser (PKCE) flow and a device-code
//! flow, as one small state machine the Add account sheet draws.
//!
//! The runners do the I/O on their own thread and report with [`FlowMsg`];
//! [`ProviderFlow`] folds those messages into what the sheet shows. Both parts
//! are testable without a window: the device poller takes its HTTP call, its
//! sleep and its cancel flag as arguments.
//!
//! No token, code or verifier is kept here beyond the browser leg: the
//! verifier lives on the runner's stack, the server swaps the code for the
//! account's tokens, and the one-time device code is the only secret-like text
//! the sheet shows (the member types it on the provider's page).

use crate::api::{ApiClient, ApiError, SigninStarted, SigninStatus};
use crate::oauth::{self, OAuthError};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

/// Which kind of sign-in is running.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FlowKind {
    /// The system browser, with a loopback redirect.
    Browser,
    /// A one-time code typed on the provider's page; the server polls.
    Device,
}

impl FlowKind {
    pub fn flow_word(self) -> &'static str {
        match self {
            Self::Browser => "pkce",
            Self::Device => "device",
        }
    }
}

/// How a sign-in ended.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FlowEnd {
    Done { account_id: String, created: bool },
    Denied(String),
    Expired,
    Cancelled,
    Failed(String),
}

/// What a runner tells the page while it works.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FlowMsg {
    /// The server issued the flow. `opened` is whether the system browser took
    /// the page; when it did not, the sheet shows the link.
    Started {
        started: SigninStarted,
        opened: bool,
    },
    /// The provider has not answered yet.
    Pending,
    Ended(FlowEnd),
}

/// Where the sheet's sign-in stands.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FlowPhase {
    /// Asking the server for the page.
    Starting,
    /// The page is open (or its link is shown); waiting for the member.
    Waiting,
    Done {
        created: bool,
    },
    Denied(String),
    Expired,
    Cancelled,
    Failed(String),
}

impl FlowPhase {
    pub fn is_final(&self) -> bool {
        !matches!(self, Self::Starting | Self::Waiting)
    }
}

/// The sign-in the sheet shows. A pure reducer of [`FlowMsg`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProviderFlow {
    pub provider: String,
    pub kind: FlowKind,
    pub phase: FlowPhase,
    /// Server flow id, kept so manual paste-code completion can target it.
    pub flow_id: Option<String>,
    /// The provider's page: opened for the member, and kept for "Open page".
    pub url: Option<String>,
    /// Claude Agent SDK gateway: the page whose last step shows a code to
    /// paste, for when the redirect to `localhost` cannot load.
    pub manual_url: Option<String>,
    pub browser_opened: bool,
    /// The one-time code to type (device flow only).
    pub user_code: Option<String>,
    /// Seconds the flow lasts, from the server.
    pub expires_in: u64,
    /// Polls that got no answer, in a row.
    pub missed_polls: u8,
}

impl ProviderFlow {
    pub fn new(provider: &str, kind: FlowKind) -> Self {
        Self {
            provider: provider.to_owned(),
            kind,
            phase: FlowPhase::Starting,
            flow_id: None,
            url: None,
            manual_url: None,
            browser_opened: false,
            user_code: None,
            expires_in: 0,
            missed_polls: 0,
        }
    }

    pub fn apply(&mut self, msg: FlowMsg) {
        if self.phase.is_final() {
            return;
        }
        match msg {
            FlowMsg::Started { started, opened } => {
                self.flow_id = Some(started.flow_id);
                self.url = Some(started.url);
                self.manual_url = started.manual_url;
                self.user_code = started.user_code;
                self.expires_in = started.expires_in;
                self.browser_opened = opened;
                self.phase = FlowPhase::Waiting;
            }
            FlowMsg::Pending => self.missed_polls = 0,
            FlowMsg::Ended(end) => {
                self.phase = match end {
                    FlowEnd::Done { created, .. } => FlowPhase::Done { created },
                    FlowEnd::Denied(reason) => FlowPhase::Denied(reason),
                    FlowEnd::Expired => FlowPhase::Expired,
                    FlowEnd::Cancelled => FlowPhase::Cancelled,
                    FlowEnd::Failed(message) => FlowPhase::Failed(message),
                };
            }
        }
    }

    /// The member backed out. The runner is told separately (its cancel flag).
    pub fn cancel(&mut self) {
        if !self.phase.is_final() {
            self.phase = FlowPhase::Cancelled;
        }
    }

    /// Seconds left after `elapsed` since the page was issued; 0 once over.
    pub fn remaining_secs(&self, elapsed: Duration) -> u64 {
        self.expires_in.saturating_sub(elapsed.as_secs())
    }

    /// The share of the time already used, 0 to 100, for a thin progress bar.
    pub fn elapsed_percent(&self, elapsed: Duration) -> u32 {
        if self.expires_in == 0 {
            return 0;
        }
        ((elapsed.as_secs().min(self.expires_in) * 100) / self.expires_in) as u32
    }

    /// The heading of the sheet's sign-in block.
    pub fn headline(&self) -> &'static str {
        match (&self.phase, self.kind) {
            (FlowPhase::Starting, _) => "Getting the sign-in page\u{2026}",
            (FlowPhase::Waiting, FlowKind::Browser) => "Finish signing in in your browser",
            (FlowPhase::Waiting, FlowKind::Device) if self.user_code.is_none() => {
                "Finish signing in in your browser"
            }
            (FlowPhase::Waiting, FlowKind::Device) => "Enter this code on the provider's page",
            (FlowPhase::Done { created: true }, _) => "Account added",
            (FlowPhase::Done { created: false }, _) => "Signed in again",
            (FlowPhase::Denied(_), _) => "Sign-in was refused",
            (FlowPhase::Expired, _) => "The sign-in timed out",
            (FlowPhase::Cancelled, _) => "Sign-in cancelled",
            (FlowPhase::Failed(_), _) => "Sign-in did not finish",
        }
    }

    /// One calm sentence under the heading.
    pub fn detail(&self) -> Option<String> {
        match &self.phase {
            FlowPhase::Starting => None,
            FlowPhase::Waiting
                if (self.kind == FlowKind::Browser || self.user_code.is_none())
                    && !self.browser_opened =>
            {
                Some("Your browser did not open. Use Open page, or copy the link below.".to_owned())
            }
            FlowPhase::Waiting if self.kind == FlowKind::Browser => {
                Some("Come back here once the page says you are done.".to_owned())
            }
            FlowPhase::Waiting => {
                Some("This window updates by itself when the provider confirms.".to_owned())
            }
            FlowPhase::Done { created: true } => {
                Some("It is ready to use. Bots can try it now.".to_owned())
            }
            FlowPhase::Done { created: false } => {
                Some("This account was already added. It is signed in again.".to_owned())
            }
            FlowPhase::Denied(reason) | FlowPhase::Failed(reason) => Some(reason.clone()),
            FlowPhase::Expired => Some("Start again to get a new page.".to_owned()),
            FlowPhase::Cancelled => None,
        }
    }
}

/// Mm:ss for a countdown: `4:32`.
pub fn countdown_text(secs: u64) -> String {
    format!("{}:{:02}", secs / 60, secs % 60)
}

/// Seconds between status polls: what the server asks for, kept within 2 to 5.
/// (The server talks to the provider; this only asks the server.)
pub fn poll_interval_secs(asked: Option<u64>) -> u64 {
    asked.unwrap_or(3).clamp(2, 5)
}

/// More than this many polls without an answer ends the wait.
pub const MAX_MISSED_POLLS: u8 = 5;

/// A plain sentence for a refused sign-in.
pub fn denied_text(reason: Option<&str>) -> String {
    match reason.map(str::trim) {
        Some("access_denied" | "denied") => {
            "The provider's page was closed without approving. Start again when you are ready."
                .to_owned()
        }
        Some("expired_token" | "expired") => "The code expired before it was entered.".to_owned(),
        Some(message) if message.contains(' ') && message.len() <= 240 => message.to_owned(),
        _ => "The provider did not approve this sign-in.".to_owned(),
    }
}

/// Where a provider's browser sign-in redirects to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RedirectPlan {
    /// A port the provider registered; `None` takes the documented port or any free one.
    pub fixed_port: Option<u16>,
    pub host: &'static str,
    pub path: &'static str,
}

/// The loopback redirect for a provider kind. Antigravity's registration is
/// `localhost:51121/oauth-callback` (51121 preferred, any free port after it,
/// exactly as the owner's HielCowork client signs in; Google matches the host
/// literally, so `127.0.0.1` is refused there). Codex's registration allows only
/// `localhost:1455/auth/callback`; OpenRouter documents `localhost`.
pub fn redirect_plan(kind: &str) -> RedirectPlan {
    match kind {
        "google-antigravity" => RedirectPlan {
            fixed_port: None,
            host: "localhost",
            path: oauth::CALLBACK_PATH,
        },
        "openai-codex" => RedirectPlan {
            fixed_port: Some(oauth::CODEX_PORT),
            host: "localhost",
            path: oauth::CODEX_CALLBACK_PATH,
        },
        "devin" => RedirectPlan {
            fixed_port: Some(oauth::DEVIN_PORT),
            host: "127.0.0.1",
            path: oauth::DEVIN_CALLBACK_PATH,
        },
        "openrouter" => RedirectPlan {
            fixed_port: None,
            host: "localhost",
            path: oauth::CALLBACK_PATH,
        },
        _ => RedirectPlan {
            fixed_port: None,
            host: "127.0.0.1",
            path: oauth::CALLBACK_PATH,
        },
    }
}

impl RedirectPlan {
    pub fn uri(&self, port: u16) -> String {
        format!("http://{}:{port}{}", self.host, self.path)
    }
}

pub(crate) fn oauth_end(error: OAuthError) -> FlowEnd {
    match error {
        OAuthError::Cancelled => FlowEnd::Cancelled,
        OAuthError::TimedOut => FlowEnd::Expired,
        OAuthError::Denied => FlowEnd::Denied(denied_text(Some("denied"))),
        other => FlowEnd::Failed(other.to_string()),
    }
}

/// A sentence for a failed call during sign-in. The server's own message is
/// shown when it sent one; a raw status never is.
pub(crate) fn api_end(error: ApiError) -> FlowEnd {
    FlowEnd::Failed(match error {
        ApiError::Coded { code, message, .. } => {
            crate::provider_accounts::plain_coded_error(&code, &message)
        }
        ApiError::Rejected(message) => crate::provider_accounts::plain_error(&message),
        ApiError::Http(404) => {
            "This server does not offer that sign-in. Update the server.".to_owned()
        }
        ApiError::Http(403) => "You are not allowed to sign in to this provider here.".to_owned(),
        ApiError::Transport => "Could not reach the server. Check the connection.".to_owned(),
        other => crate::provider_accounts::plain_error(&other.to_string()),
    })
}

/// Run a gateway browser sign-in (`claude-agent-sdk`).
///
/// The gateway runs `claude auth login`, which waits for the browser's
/// redirect on its own `localhost` port and also takes a pasted `code#state`.
/// The page opened here is the CLI's automatic one, so the member only has to
/// approve:
///
/// - gateway on this computer: the CLI holds the port, the browser reaches it
///   directly, and the status poll sees the sign-in finish;
/// - gateway on another machine (the server's): the port is free here, so the
///   desktop catches the redirect and hands it to the server, which relays it
///   to the CLI's listener on the gateway host.
///
/// The paste box in the sheet stays available for a browser on a third
/// machine.
pub fn run_gateway_browser(
    client: &ApiClient,
    provider: &str,
    cancel: &AtomicBool,
    report: &dyn Fn(FlowMsg),
    open: &dyn Fn(&str) -> bool,
) -> FlowEnd {
    let started =
        match client.provider_signin_start(provider, &serde_json::json!({ "flow": "pkce" })) {
            Ok(started) => started,
            Err(error) => return api_end(error),
        };
    let catcher = RedirectCatcher::for_gateway(&started);
    let opened = open(&started.url);
    let flow_id = started.flow_id.clone();
    let mut poll_started = started.clone();
    poll_started.interval = Some(2);
    report(FlowMsg::Started { started, opened });
    poll_device(
        &poll_started,
        cancel,
        &mut || match catcher.as_ref().and_then(RedirectCatcher::take) {
            Some((code, state)) => Ok(relayed_status(client.provider_signin_complete_gateway(
                provider,
                &flow_id,
                &code,
                Some(&state),
            ))),
            None => client.provider_signin_status(provider, &flow_id),
        },
        &mut std::thread::sleep,
        report,
    )
}

/// A relayed redirect's answer as a status: a code is single-use, so a
/// refusal ends the sign-in with the server's reason.
pub(crate) fn relayed_status(result: Result<crate::api::SigninDone, ApiError>) -> SigninStatus {
    match result {
        Ok(done) => SigninStatus {
            status: "ok".to_owned(),
            account_id: Some(done.account_id),
            created: done.created,
            reason: None,
        },
        Err(error) => SigninStatus {
            status: "denied".to_owned(),
            account_id: None,
            created: false,
            reason: Some(crate::provider_accounts::plain_api_error(&error)),
        },
    }
}

/// Listens for a browser redirect meant for `claude auth login`'s own
/// `localhost` port, on this computer, while that port is free here.
struct RedirectCatcher {
    codes: std::sync::mpsc::Receiver<zeroize::Zeroizing<String>>,
    state: String,
    stop: std::sync::Arc<AtomicBool>,
}

impl RedirectCatcher {
    fn for_gateway(started: &SigninStarted) -> Option<Self> {
        let port = started.callback_port.filter(|port| *port >= 1024)?;
        let path = started
            .callback_path
            .clone()
            .filter(|path| path.starts_with('/'))?;
        let state = started.state.clone().filter(|state| !state.is_empty())?;
        // Taken by the CLI when the gateway runs here: then the browser
        // reaches the CLI itself and there is nothing to catch.
        let listeners = oauth::bind_loopback_free(port);
        if listeners.is_empty() {
            return None;
        }
        let (sender, codes) = std::sync::mpsc::channel();
        let stop = std::sync::Arc::new(AtomicBool::new(false));
        let thread_stop = std::sync::Arc::clone(&stop);
        let expected = state.clone();
        let wait = Duration::from_secs(started.expires_in.clamp(30, 600));
        std::thread::Builder::new()
            .name("serabot-gateway-redirect".into())
            .spawn(move || {
                if let Ok(code) =
                    oauth::wait_for_callback_any(&listeners, &expected, &path, wait, &thread_stop)
                {
                    let _ = sender.send(code);
                }
            })
            .ok()?;
        Some(Self { codes, state, stop })
    }

    fn take(&self) -> Option<(zeroize::Zeroizing<String>, String)> {
        self.codes
            .try_recv()
            .ok()
            .map(|code| (code, self.state.clone()))
    }
}

impl Drop for RedirectCatcher {
    fn drop(&mut self) {
        // Closes the listener within a fraction of a second.
        self.stop.store(true, Ordering::SeqCst);
    }
}

/// Run a browser sign-in: listen on loopback, get the page from the server,
/// open it, wait for the redirect, and hand the code to the server. Cancelling
/// closes the listener within a fraction of a second.
pub fn run_browser(
    client: &ApiClient,
    provider: &str,
    kind: &str,
    cancel: &AtomicBool,
    report: &dyn Fn(FlowMsg),
    open: &dyn Fn(&str) -> bool,
) -> FlowEnd {
    if kind == "claude-agent-sdk" {
        return run_gateway_browser(client, provider, cancel, report, open);
    }
    let plan = redirect_plan(kind);
    // A `localhost` redirect may arrive on `::1` or `127.0.0.1`, whichever the
    // browser tries first: listen on both where the system has IPv6.
    let Ok(loopback) = oauth::bind_loopback_both(plan.fixed_port) else {
        return FlowEnd::Failed(match plan.fixed_port {
            Some(port) => format!(
                "Could not listen on port {port} for the redirect. Close what uses it, then try again."
            ),
            None => {
                "Could not listen on this computer's loopback address for the redirect.".to_owned()
            }
        });
    };
    let pkce = oauth::new_pkce();
    let redirect = plan.uri(loopback.port);
    let started = match client.provider_signin_start(
        provider,
        &serde_json::json!({
            "flow": "pkce",
            "challenge": pkce.challenge,
            "state": pkce.state,
            "redirect_uri": redirect,
        }),
    ) {
        Ok(started) => started,
        Err(error) => return api_end(error),
    };
    let opened = open(&started.url);
    let expected_state = started.state.clone().unwrap_or_else(|| pkce.state.clone());
    let wait = Duration::from_secs(started.expires_in.clamp(30, 600));
    let flow_id = started.flow_id.clone();
    report(FlowMsg::Started { started, opened });
    let code = match oauth::wait_for_callback_any(
        &loopback.listeners,
        &expected_state,
        plan.path,
        wait,
        cancel,
    ) {
        Ok(code) => code,
        Err(error) => return oauth_end(error),
    };
    drop(loopback);
    match client.provider_signin_complete(
        provider,
        &flow_id,
        &code,
        Some(redirect.as_str()),
        &pkce.verifier,
    ) {
        Ok(done) => FlowEnd::Done {
            account_id: done.account_id,
            created: done.created,
        },
        Err(error) => api_end(error),
    }
}

/// Run a device-code sign-in: get the code, open the provider's page, and
/// poll the server until it says the provider answered.
pub fn run_device(
    client: &ApiClient,
    provider: &str,
    cancel: &AtomicBool,
    report: &dyn Fn(FlowMsg),
    open: &dyn Fn(&str) -> bool,
) -> FlowEnd {
    let started =
        match client.provider_signin_start(provider, &serde_json::json!({ "flow": "device" })) {
            Ok(started) => started,
            Err(error) => return api_end(error),
        };
    let opened = open(&started.url);
    let flow_id = started.flow_id.clone();
    report(FlowMsg::Started {
        started: started.clone(),
        opened,
    });
    poll_device(
        &started,
        cancel,
        &mut || client.provider_signin_status(provider, &flow_id),
        &mut std::thread::sleep,
        report,
    )
}

/// The device poll loop, with its I/O passed in. `sleep` is called in short
/// slices so a cancel is seen within a quarter of a second; time is counted by
/// what was slept, so a test runs instantly.
pub fn poll_device(
    started: &SigninStarted,
    cancel: &AtomicBool,
    poll: &mut dyn FnMut() -> Result<SigninStatus, ApiError>,
    sleep: &mut dyn FnMut(Duration),
    report: &dyn Fn(FlowMsg),
) -> FlowEnd {
    const SLICE: Duration = Duration::from_millis(250);
    // A little grace: the provider may answer at the very end.
    const GRACE_SECS: u64 = 10;
    let interval = poll_interval_secs(started.interval);
    let mut waited = 0u64;
    let mut missed = 0u8;
    loop {
        if cancel.load(Ordering::SeqCst) {
            return FlowEnd::Cancelled;
        }
        match poll() {
            Ok(status) => {
                missed = 0;
                match status.status.as_str() {
                    "ok" => {
                        return match status.account_id {
                            Some(account_id) => FlowEnd::Done {
                                account_id,
                                created: status.created,
                            },
                            None => FlowEnd::Failed(
                                "The server finished the sign-in without an account.".to_owned(),
                            ),
                        };
                    }
                    "denied" => return FlowEnd::Denied(denied_text(status.reason.as_deref())),
                    "expired" => return FlowEnd::Expired,
                    _ => report(FlowMsg::Pending),
                }
            }
            // A 401 is the session ending, a 404 a flow the server forgot: both final.
            Err(ApiError::Http(401)) => {
                return FlowEnd::Failed("Your session ended. Sign in to SeraBot again.".to_owned());
            }
            Err(ApiError::Http(404)) => return FlowEnd::Expired,
            Err(error) => {
                missed += 1;
                if missed >= MAX_MISSED_POLLS {
                    return api_end(error);
                }
            }
        }
        if waited >= started.expires_in + GRACE_SECS {
            return FlowEnd::Expired;
        }
        for _ in 0..(interval * 4) {
            if cancel.load(Ordering::SeqCst) {
                return FlowEnd::Cancelled;
            }
            sleep(SLICE);
        }
        waited += interval;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::{Cell, RefCell};
    use std::collections::VecDeque;

    fn started(kind: &str) -> SigninStarted {
        SigninStarted {
            flow_id: "flow-1".into(),
            flow: kind.into(),
            url: "https://verify.example/device".into(),
            expires_in: 600,
            user_code: (kind == "device").then(|| "ABCD-1234".to_owned()),
            interval: (kind == "device").then_some(5),
            state: None,
            ..Default::default()
        }
    }

    fn status(word: &str) -> SigninStatus {
        SigninStatus {
            status: word.into(),
            ..SigninStatus::default()
        }
    }

    #[test]
    fn a_device_dialog_walks_from_starting_to_done() {
        let mut flow = ProviderFlow::new("muse-code", FlowKind::Device);
        assert_eq!(flow.phase, FlowPhase::Starting);
        assert_eq!(flow.headline(), "Getting the sign-in page\u{2026}");
        flow.apply(FlowMsg::Started {
            started: started("device"),
            opened: true,
        });
        assert_eq!(flow.phase, FlowPhase::Waiting);
        assert_eq!(flow.user_code.as_deref(), Some("ABCD-1234"));
        assert_eq!(flow.headline(), "Enter this code on the provider's page");
        flow.apply(FlowMsg::Pending);
        assert_eq!(flow.phase, FlowPhase::Waiting);
        flow.apply(FlowMsg::Ended(FlowEnd::Done {
            account_id: "a1".into(),
            created: true,
        }));
        assert_eq!(flow.phase, FlowPhase::Done { created: true });
        assert_eq!(flow.headline(), "Account added");
        // A final phase does not move again.
        flow.apply(FlowMsg::Ended(FlowEnd::Expired));
        flow.cancel();
        assert_eq!(flow.phase, FlowPhase::Done { created: true });
    }

    #[test]
    fn a_device_flow_without_a_user_code_shows_browser_headline() {
        let mut flow = ProviderFlow::new("mistral", FlowKind::Device);
        let mut s = started("device");
        s.user_code = None;
        flow.apply(FlowMsg::Started {
            started: s,
            opened: true,
        });
        assert_eq!(flow.phase, FlowPhase::Waiting);
        assert!(flow.user_code.is_none());
        assert!(flow.browser_opened);
        assert_eq!(flow.headline(), "Finish signing in in your browser");
    }

    #[test]
    fn every_end_has_its_own_words_and_cancel_stops_a_waiting_flow() {
        let cases = [
            (FlowEnd::Denied("no".into()), "Sign-in was refused"),
            (FlowEnd::Expired, "The sign-in timed out"),
            (FlowEnd::Cancelled, "Sign-in cancelled"),
            (FlowEnd::Failed("down".into()), "Sign-in did not finish"),
        ];
        for (end, headline) in cases {
            let mut flow = ProviderFlow::new("p", FlowKind::Device);
            flow.apply(FlowMsg::Ended(end));
            assert_eq!(flow.headline(), headline);
            assert!(flow.phase.is_final());
        }
        let mut waiting = ProviderFlow::new("p", FlowKind::Browser);
        waiting.apply(FlowMsg::Started {
            started: started("pkce"),
            opened: false,
        });
        assert!(waiting.detail().unwrap().contains("did not open"));
        waiting.cancel();
        assert_eq!(waiting.phase, FlowPhase::Cancelled);
        assert!(waiting.detail().is_none());
    }

    #[test]
    fn the_countdown_never_goes_below_zero() {
        let mut flow = ProviderFlow::new("p", FlowKind::Device);
        flow.apply(FlowMsg::Started {
            started: started("device"),
            opened: true,
        });
        assert_eq!(flow.remaining_secs(Duration::from_secs(0)), 600);
        assert_eq!(flow.remaining_secs(Duration::from_secs(272)), 328);
        assert_eq!(flow.remaining_secs(Duration::from_secs(9_999)), 0);
        assert_eq!(flow.elapsed_percent(Duration::from_secs(300)), 50);
        assert_eq!(flow.elapsed_percent(Duration::from_secs(9_999)), 100);
        assert_eq!(countdown_text(328), "5:28");
        assert_eq!(countdown_text(0), "0:00");
        assert_eq!(countdown_text(65), "1:05");
        // No issued flow, no bar.
        assert_eq!(
            ProviderFlow::new("p", FlowKind::Device).elapsed_percent(Duration::from_secs(5)),
            0
        );
    }

    #[test]
    fn polls_are_paced_within_two_to_five_seconds() {
        assert_eq!(poll_interval_secs(None), 3);
        assert_eq!(poll_interval_secs(Some(0)), 2);
        assert_eq!(poll_interval_secs(Some(5)), 5);
        assert_eq!(poll_interval_secs(Some(60)), 5);
    }

    #[test]
    fn redirects_follow_each_providers_registration() {
        let google = redirect_plan("google-antigravity");
        assert_eq!(google.fixed_port, None, "51121 is preferred, not required");
        assert_eq!(google.uri(51121), "http://localhost:51121/oauth-callback");
        let codex = redirect_plan("openai-codex");
        assert_eq!(codex.fixed_port, Some(1455));
        assert_eq!(codex.uri(1455), "http://localhost:1455/auth/callback");
        let devin = redirect_plan("devin");
        assert_eq!(devin.fixed_port, Some(59653));
        assert_eq!(devin.path, "/callback");
        assert_eq!(devin.uri(59653), "http://127.0.0.1:59653/callback");
        assert_eq!(
            redirect_plan("openrouter").uri(40000),
            "http://localhost:40000/oauth-callback"
        );
    }

    fn run_poll(
        script: Vec<Result<SigninStatus, ApiError>>,
        cancel: &AtomicBool,
        slept: &Cell<u32>,
        pending_seen: &Cell<u32>,
    ) -> FlowEnd {
        let mut script: VecDeque<_> = script.into();
        let started = started("device");
        poll_device(
            &started,
            cancel,
            &mut || script.pop_front().unwrap_or_else(|| Ok(status("pending"))),
            &mut |_| slept.set(slept.get() + 1),
            &|msg| {
                if msg == FlowMsg::Pending {
                    pending_seen.set(pending_seen.get() + 1);
                }
            },
        )
    }

    #[test]
    fn the_poller_waits_through_pending_then_reports_the_account() {
        let (slept, pending) = (Cell::new(0), Cell::new(0));
        let mut ok = status("ok");
        ok.account_id = Some("acct-1".into());
        ok.created = true;
        let end = run_poll(
            vec![Ok(status("pending")), Ok(status("pending")), Ok(ok)],
            &AtomicBool::new(false),
            &slept,
            &pending,
        );
        assert_eq!(
            end,
            FlowEnd::Done {
                account_id: "acct-1".into(),
                created: true
            }
        );
        assert_eq!(pending.get(), 2);
        // Two waits of 5 s, in quarter-second slices.
        assert_eq!(slept.get(), 2 * 5 * 4);
    }

    #[test]
    fn denial_expiry_and_a_forgotten_flow_are_final() {
        let (slept, pending) = (Cell::new(0), Cell::new(0));
        let mut denied = status("denied");
        denied.reason = Some("access_denied".into());
        let end = run_poll(vec![Ok(denied)], &AtomicBool::new(false), &slept, &pending);
        assert!(matches!(end, FlowEnd::Denied(text) if text.contains("closed without approving")));
        assert_eq!(
            run_poll(
                vec![Ok(status("expired"))],
                &AtomicBool::new(false),
                &slept,
                &pending
            ),
            FlowEnd::Expired
        );
        assert_eq!(
            run_poll(
                vec![Err(ApiError::Http(404))],
                &AtomicBool::new(false),
                &slept,
                &pending
            ),
            FlowEnd::Expired
        );
        assert!(matches!(
            run_poll(vec![Err(ApiError::Http(401))], &AtomicBool::new(false), &slept, &pending),
            FlowEnd::Failed(text) if text.contains("session ended")
        ));
        // `ok` without an account is a server fault, not a success.
        assert!(matches!(
            run_poll(
                vec![Ok(status("ok"))],
                &AtomicBool::new(false),
                &slept,
                &pending
            ),
            FlowEnd::Failed(_)
        ));
    }

    #[test]
    fn a_few_lost_polls_are_forgiven_and_many_end_the_wait() {
        let (slept, pending) = (Cell::new(0), Cell::new(0));
        let mut ok = status("ok");
        ok.account_id = Some("a".into());
        let flaky = vec![
            Err(ApiError::Transport),
            Err(ApiError::Transport),
            Ok(status("pending")),
            Err(ApiError::Transport),
            Ok(ok),
        ];
        assert!(matches!(
            run_poll(flaky, &AtomicBool::new(false), &slept, &pending),
            FlowEnd::Done { .. }
        ));
        let dead = (0..MAX_MISSED_POLLS)
            .map(|_| Err(ApiError::Transport))
            .collect();
        assert!(matches!(
            run_poll(dead, &AtomicBool::new(false), &slept, &pending),
            FlowEnd::Failed(text) if text.contains("Could not reach")
        ));
    }

    #[test]
    fn a_pending_flow_expires_with_the_server_clock_and_cancel_is_prompt() {
        let (slept, pending) = (Cell::new(0), Cell::new(0));
        // Always pending: ends after expires_in plus grace, without real time.
        let end = run_poll(Vec::new(), &AtomicBool::new(false), &slept, &pending);
        assert_eq!(end, FlowEnd::Expired);
        assert!(slept.get() > 0 && pending.get() > 100);

        // A cancel set before the first poll ends it without a call.
        let called = RefCell::new(0);
        let cancel = AtomicBool::new(true);
        let end = poll_device(
            &started("device"),
            &cancel,
            &mut || {
                *called.borrow_mut() += 1;
                Ok(status("pending"))
            },
            &mut |_| {},
            &|_| {},
        );
        assert_eq!(end, FlowEnd::Cancelled);
        assert_eq!(*called.borrow(), 0);

        // A cancel that arrives while sleeping is seen within a slice.
        let cancel = AtomicBool::new(false);
        let polls = Cell::new(0);
        let end = poll_device(
            &started("device"),
            &cancel,
            &mut || {
                polls.set(polls.get() + 1);
                Ok(status("pending"))
            },
            &mut |_| cancel.store(true, Ordering::SeqCst),
            &|_| {},
        );
        assert_eq!(end, FlowEnd::Cancelled);
        assert_eq!(polls.get(), 1);
    }

    #[test]
    fn denial_words_are_plain() {
        assert!(denied_text(Some("expired_token")).contains("expired"));
        assert!(denied_text(None).contains("did not approve"));
        assert_eq!(FlowKind::Browser.flow_word(), "pkce");
        assert_eq!(FlowKind::Device.flow_word(), "device");
    }

    #[test]
    fn a_gateway_redirect_on_a_free_port_is_caught_with_its_state() {
        use std::io::{Read as _, Write as _};
        let port = std::net::TcpListener::bind(("127.0.0.1", 0))
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        let started = SigninStarted {
            callback_port: Some(port),
            callback_path: Some("/callback".into()),
            state: Some("st-1".into()),
            expires_in: 60,
            ..started("pkce")
        };
        let catcher = RedirectCatcher::for_gateway(&started).expect("port is free here");
        assert!(catcher.take().is_none());
        let mut browser = std::net::TcpStream::connect(("127.0.0.1", port)).unwrap();
        browser
            .write_all(
                format!(
                    "GET /callback?code=the-code&state=st-1 HTTP/1.1\r\nHost: localhost:{port}\r\n\r\n"
                )
                .as_bytes(),
            )
            .unwrap();
        let mut page = String::new();
        let _ = browser.read_to_string(&mut page);
        assert!(page.contains("Signed in"), "{page}");
        let mut caught = None;
        for _ in 0..100 {
            caught = catcher.take();
            if caught.is_some() {
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        let (code, state) = caught.expect("the redirect was caught");
        assert_eq!(code.as_str(), "the-code");
        assert_eq!(state, "st-1");
    }

    #[test]
    fn a_gateway_start_without_a_safe_callback_catches_nothing() {
        for started in [
            SigninStarted {
                callback_port: Some(80),
                callback_path: Some("/callback".into()),
                state: Some("s".into()),
                ..started("pkce")
            },
            SigninStarted {
                callback_port: Some(40999),
                callback_path: Some("/callback".into()),
                state: None,
                ..started("pkce")
            },
        ] {
            assert!(RedirectCatcher::for_gateway(&started).is_none());
        }
    }

    #[test]
    fn a_relayed_redirect_ends_the_flow_either_way() {
        let ok = relayed_status(Ok(crate::api::SigninDone {
            account_id: "acct-1".into(),
            created: true,
        }));
        assert_eq!(ok.status, "ok");
        assert_eq!(ok.account_id.as_deref(), Some("acct-1"));
        let refused = relayed_status(Err(ApiError::Rejected(
            "Claude did not accept the sign-in.".into(),
        )));
        assert_eq!(refused.status, "denied");
        assert!(refused.reason.is_some());
    }

    #[test]
    fn a_gateway_flow_keeps_the_code_page_for_the_paste_fallback() {
        let mut flow = ProviderFlow::new("claude-agent-sdk", FlowKind::Browser);
        flow.apply(FlowMsg::Started {
            started: SigninStarted {
                manual_url: Some("https://claude.example/oauth/authorize?z-1".into()),
                ..started("pkce")
            },
            opened: true,
        });
        assert_eq!(
            flow.manual_url.as_deref(),
            Some("https://claude.example/oauth/authorize?z-1")
        );
    }
}
