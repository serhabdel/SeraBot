//! The Composio card's state: the user's key, the app list, and the apps
//! they have linked. It sends nothing itself; `panel.rs` queues the requests
//! and folds the replies in, so this stays unit-testable.

use crate::{ComposioAccount, ComposioApp};
use std::time::{Duration, Instant};

/// How often linked apps are re-read while a sign-in page is open.
pub const POLL_EVERY: Duration = Duration::from_secs(4);
/// The sign-in wait ends after this long without the app showing up.
pub const WAIT_LIMIT: Duration = Duration::from_secs(300);

/// A short note under the key box for what has been typed so far. It reads
/// only the public prefix, so nothing secret is kept or shown.
pub fn key_hint(key: &str) -> &'static str {
    let key = key.trim();
    if key.starts_with("uak_") {
        "This looks like a user key (uak_)."
    } else if key.starts_with("ak_") {
        "This looks like a project key (ak_)."
    } else if key.starts_with("ck_") {
        "This looks like a Composio Connect key (ck_). Your apps are managed in Composio Connect."
    } else {
        "SeraBot accepts project keys (ak_), user keys (uak_) and Connect keys (ck_)."
    }
}

pub struct ComposioFlow {
    /// A change (key, connect, disconnect) is on its way.
    pub busy: bool,
    /// The app list is being read.
    pub loading: bool,
    /// Plain-words error for the card.
    pub error: Option<String>,
    pub apps: Vec<ComposioApp>,
    pub accounts: Vec<ComposioAccount>,
    pub loaded: bool,
    /// The search text behind `apps`.
    pub query: String,
    /// The app whose sign-in page was opened and is awaiting the user.
    pub waiting: Option<String>,
    waiting_since: Option<Instant>,
    next_poll: Instant,
    pub(crate) want_load: Option<String>,
    pub(crate) pending_url: Option<String>,
    pub(crate) refresh_due: bool,
}

impl Default for ComposioFlow {
    fn default() -> Self {
        Self {
            busy: false,
            loading: false,
            error: None,
            apps: Vec::new(),
            accounts: Vec::new(),
            loaded: false,
            query: String::new(),
            waiting: None,
            waiting_since: None,
            next_poll: Instant::now(),
            want_load: None,
            pending_url: None,
            refresh_due: false,
        }
    }
}

impl ComposioFlow {
    /// The sign-in address to open in the system browser, once.
    pub fn take_url(&mut self) -> Option<String> {
        self.pending_url.take()
    }

    /// True once after a change the connector list should be re-read for.
    pub fn take_refresh(&mut self) -> bool {
        std::mem::take(&mut self.refresh_due)
    }

    /// Ask for the app list (and linked apps) for a search text.
    pub fn load(&mut self, query: &str) {
        self.want_load = Some(query.trim().to_owned());
    }

    /// The linked account for one app, if any.
    pub fn account_for(&self, toolkit: &str) -> Option<&ComposioAccount> {
        self.accounts
            .iter()
            .find(|a| a.toolkit.eq_ignore_ascii_case(toolkit))
    }

    pub(crate) fn begin_wait(&mut self, toolkit: &str, now: Instant) {
        self.waiting = Some(toolkit.to_owned());
        self.waiting_since = Some(now);
        self.next_poll = now + POLL_EVERY;
    }

    /// The next load to send: a requested one, or a poll while waiting.
    pub(crate) fn next_load(&mut self, now: Instant) -> Option<String> {
        if self.busy || self.loading {
            return None;
        }
        if let Some(query) = self.want_load.take() {
            self.loading = true;
            return Some(query);
        }
        if self.waiting.is_some() && now >= self.next_poll {
            if self
                .waiting_since
                .is_some_and(|since| now.duration_since(since) > WAIT_LIMIT)
            {
                self.waiting = None;
                self.waiting_since = None;
                return None;
            }
            self.next_poll = now + POLL_EVERY;
            self.loading = true;
            return Some(self.query.clone());
        }
        None
    }

    pub(crate) fn loaded_with(
        &mut self,
        query: String,
        apps: Vec<ComposioApp>,
        accounts: Vec<ComposioAccount>,
    ) {
        self.loading = false;
        self.loaded = true;
        self.query = query;
        self.apps = apps;
        self.accounts = accounts;
        let done = self
            .waiting
            .as_deref()
            .and_then(|slug| self.account_for(slug))
            .is_some_and(ComposioAccount::is_active);
        if done {
            self.waiting = None;
            self.waiting_since = None;
            self.refresh_due = true;
        }
    }

    /// Back to a clean slate (key removed or changed).
    pub(crate) fn clear(&mut self) {
        let keep = self.pending_url.take();
        *self = Self::default();
        self.pending_url = keep;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn account(toolkit: &str, status: &str) -> ComposioAccount {
        ComposioAccount {
            id: "ca_1".into(),
            toolkit: toolkit.into(),
            status: status.into(),
        }
    }

    #[test]
    fn the_key_hint_names_the_kind_from_the_prefix() {
        assert!(key_hint("ck_abc").contains("Connect"));
        assert!(key_hint(" ak_abc").contains("project"));
        assert!(key_hint("uak_abc").contains("user key"));
        assert!(key_hint("zzz").contains("ck_"));
    }

    #[test]
    fn a_requested_load_goes_out_once() {
        let mut flow = ComposioFlow::default();
        let now = Instant::now();
        flow.load(" mail ");
        assert_eq!(flow.next_load(now).as_deref(), Some("mail"));
        assert!(flow.next_load(now).is_none());
    }

    #[test]
    fn waiting_polls_until_the_app_is_active() {
        let mut flow = ComposioFlow::default();
        let now = Instant::now();
        flow.begin_wait("gmail", now);
        assert!(flow.next_load(now).is_none());
        let later = now + POLL_EVERY;
        assert!(flow.next_load(later).is_some());
        flow.loaded_with(String::new(), vec![], vec![account("gmail", "INITIATED")]);
        assert!(flow.waiting.is_some());
        assert!(!flow.take_refresh());
        flow.loaded_with(String::new(), vec![], vec![account("gmail", "ACTIVE")]);
        assert!(flow.waiting.is_none());
        assert!(flow.take_refresh());
    }

    #[test]
    fn waiting_gives_up_after_the_limit() {
        let mut flow = ComposioFlow::default();
        let now = Instant::now();
        flow.begin_wait("gmail", now);
        let late = now + WAIT_LIMIT + POLL_EVERY;
        assert!(flow.next_load(late).is_none());
        assert!(flow.waiting.is_none());
    }
}
