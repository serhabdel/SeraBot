//! State and network pump for the browser dock and the vault sheet.
//!
//! Both panels talk to the server through one dedicated worker thread (the
//! same shape as [`crate::worker::Worker`]: blocking client, epoch-scoped
//! results, nothing on the UI thread). They get their own thread so a slow
//! screenshot or a failed poll can never pause chat polling, and so the
//! chat's single-request pipeline never delays a click on the live page.
//!
//! [`PanelState`] is plain data plus a scheduler. [`PanelState::next_op`]
//! decides what to send and when; [`PanelState::apply`] folds a result back
//! in. Neither touches the network, so both are unit-tested.

use crate::worker::Credentials;
use crate::{
    ApiClient, ApiError, BrowserController, BrowserInfo, BrowserInput, MAX_VAULT_DOMAINS,
    MAX_VAULT_NOTE_CHARS, MAX_VAULT_USERNAME_CHARS, VaultEntry, VaultKind, VaultPut,
    normalize_vault_domain, valid_vault_name, validate_vault_put,
};
use std::{
    collections::VecDeque,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
        mpsc::{self, Receiver, Sender},
    },
    time::{Duration, Instant},
};
use zeroize::{Zeroize, Zeroizing};

/// `GET /v1/browser` while the app has focus.
pub const STATUS_EVERY: Duration = Duration::from_secs(2);
/// Screenshots are polled at most twice a second, and only while visible.
pub const SHOT_EVERY: Duration = Duration::from_millis(500);
const MAX_QUEUED_INPUTS: usize = 64;
const MAX_INPUTS_IN_FLIGHT: usize = 4;

pub enum PanelOp {
    VaultList,
    VaultPut { name: String, input: VaultPut },
    VaultDelete { name: String },
    Status,
    Shot,
    Takeover,
    Handback,
    Close,
    Input(BrowserInput),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PanelKind {
    VaultList,
    VaultPut,
    VaultDelete,
    Status,
    Shot,
    Takeover,
    Handback,
    Close,
    Input,
}

impl PanelOp {
    pub fn kind(&self) -> PanelKind {
        match self {
            Self::VaultList => PanelKind::VaultList,
            Self::VaultPut { .. } => PanelKind::VaultPut,
            Self::VaultDelete { .. } => PanelKind::VaultDelete,
            Self::Status => PanelKind::Status,
            Self::Shot => PanelKind::Shot,
            Self::Takeover => PanelKind::Takeover,
            Self::Handback => PanelKind::Handback,
            Self::Close => PanelKind::Close,
            Self::Input(_) => PanelKind::Input,
        }
    }
}

pub enum PanelReply {
    VaultList(Vec<VaultEntry>),
    VaultSaved(VaultEntry),
    VaultDeleted(String),
    Status(BrowserInfo),
    Shot(Vec<u8>),
    Control(BrowserInfo),
    Closed,
    Input(BrowserInfo),
}

pub struct PanelDone {
    pub epoch: u64,
    pub kind: PanelKind,
    pub result: Result<PanelReply, ApiError>,
}

struct PanelJob {
    epoch: u64,
    credentials: Arc<Credentials>,
    op: PanelOp,
}

/// One background thread for the dock and the vault.
pub struct PanelWorker {
    jobs: Sender<PanelJob>,
    results: Receiver<PanelDone>,
    epoch: Arc<AtomicU64>,
}

impl PanelWorker {
    pub fn start() -> std::io::Result<Self> {
        let (jobs, job_rx) = mpsc::channel::<PanelJob>();
        let (done_tx, results) = mpsc::channel::<PanelDone>();
        let epoch = Arc::new(AtomicU64::new(0));
        let current = Arc::clone(&epoch);
        std::thread::Builder::new()
            .name("serabot-panel".into())
            .spawn(move || {
                let mut client: Option<ApiClient> = None;
                while let Ok(job) = job_rx.recv() {
                    if current.load(Ordering::SeqCst) != job.epoch {
                        continue;
                    }
                    let kind = job.op.kind();
                    let result = execute(&job, &mut client);
                    if current.load(Ordering::SeqCst) != job.epoch {
                        continue;
                    }
                    if done_tx
                        .send(PanelDone {
                            epoch: job.epoch,
                            kind,
                            result,
                        })
                        .is_err()
                    {
                        break;
                    }
                }
            })?;
        Ok(Self {
            jobs,
            results,
            epoch,
        })
    }

    pub fn set_epoch(&self, epoch: u64) {
        self.epoch.store(epoch, Ordering::SeqCst);
    }

    pub fn send(&self, epoch: u64, credentials: Arc<Credentials>, op: PanelOp) -> bool {
        self.jobs
            .send(PanelJob {
                epoch,
                credentials,
                op,
            })
            .is_ok()
    }

    pub fn try_recv(&self) -> Option<PanelDone> {
        self.results.try_recv().ok()
    }
}

impl Drop for PanelWorker {
    fn drop(&mut self) {
        self.epoch.fetch_add(1, Ordering::SeqCst);
    }
}

fn execute(job: &PanelJob, cache: &mut Option<ApiClient>) -> Result<PanelReply, ApiError> {
    let credentials = &job.credentials;
    let reuse = cache
        .as_ref()
        .is_some_and(|client| client.targets(&credentials.endpoint, &credentials.token));
    if !reuse {
        *cache = Some(ApiClient::new(
            credentials.endpoint.clone(),
            secrecy::SecretString::new(
                secrecy::ExposeSecret::expose_secret(&credentials.token).clone(),
            ),
        )?);
    }
    let client = cache.as_ref().ok_or(ApiError::Transport)?;
    match &job.op {
        PanelOp::VaultList => client.vault_list().map(PanelReply::VaultList),
        PanelOp::VaultPut { name, input } => {
            client.vault_put(name, input).map(PanelReply::VaultSaved)
        }
        PanelOp::VaultDelete { name } => client
            .vault_delete(name)
            .map(|()| PanelReply::VaultDeleted(name.clone())),
        PanelOp::Status => client.browser_status().map(PanelReply::Status),
        PanelOp::Shot => client.browser_screenshot().map(PanelReply::Shot),
        PanelOp::Takeover => client.browser_takeover().map(PanelReply::Control),
        PanelOp::Handback => client.browser_handback().map(PanelReply::Control),
        PanelOp::Close => client.browser_close().map(|_| PanelReply::Closed),
        PanelOp::Input(input) => client.browser_input(input).map(PanelReply::Input),
    }
}

// ----------------------------------------------------------------- vault --

/// The Add / Replace form. The value is write-only: it is wiped after a save
/// and never filled from a server response.
pub struct VaultForm {
    pub replacing: bool,
    pub name: String,
    pub kind: VaultKind,
    pub username: String,
    pub value: Zeroizing<String>,
    pub domains: Vec<String>,
    pub domain_draft: String,
    pub note: String,
    pub error: Option<String>,
    pub domain_error: Option<String>,
    pub saving: bool,
}

impl VaultForm {
    pub fn new() -> Self {
        Self {
            replacing: false,
            name: String::new(),
            kind: VaultKind::Password,
            username: String::new(),
            value: Zeroizing::new(String::new()),
            domains: Vec::new(),
            domain_draft: String::new(),
            note: String::new(),
            error: None,
            domain_error: None,
            saving: false,
        }
    }

    /// Replace an entry: metadata prefilled, the value left empty.
    pub fn replacing(entry: &VaultEntry) -> Self {
        let mut form = Self::new();
        form.replacing = true;
        form.name = entry.name.clone();
        form.kind = entry.kind;
        form.username = entry.username.clone().unwrap_or_default();
        form.domains = entry.domains.clone();
        form.note = entry.note.clone().unwrap_or_default();
        form
    }

    /// Inline hint for the name field; empty input is not an error yet.
    pub fn name_hint(&self) -> Option<&'static str> {
        if self.name.is_empty() || valid_vault_name(&self.name) {
            None
        } else {
            Some(
                "Use lower-case letters, digits and dashes (up to 48), starting with a letter or digit.",
            )
        }
    }

    /// Commit the domain draft as a chip. Returns true when a chip was added
    /// or the draft was empty.
    pub fn commit_domain(&mut self) -> bool {
        let draft = self.domain_draft.trim().to_owned();
        if draft.is_empty() {
            self.domain_error = None;
            return true;
        }
        let Some(domain) = normalize_vault_domain(&draft) else {
            self.domain_error = Some("Enter a host name such as example.com.".into());
            return false;
        };
        if self.domains.contains(&domain) {
            self.domain_error = Some("That domain is already listed.".into());
            return false;
        }
        if self.domains.len() >= MAX_VAULT_DOMAINS {
            self.domain_error = Some("At most 16 domains.".into());
            return false;
        }
        self.domains.push(domain);
        self.domain_draft.clear();
        self.domain_error = None;
        true
    }

    pub fn remove_domain(&mut self, domain: &str) {
        self.domains.retain(|d| d != domain);
        self.domain_error = None;
    }

    /// The request this form describes, or the inline message to show.
    pub fn build(&mut self) -> Result<(String, VaultPut), String> {
        if !self.commit_domain() {
            return Err("Fix the domain first, or clear it.".into());
        }
        let username = self.username.trim().to_owned();
        let note = self.note.trim().to_owned();
        let input = VaultPut {
            kind: self.kind,
            username: (self.kind == VaultKind::Password && !username.is_empty())
                .then_some(username),
            value: Zeroizing::new(self.value.to_string()),
            domains: self.domains.clone(),
            note: (!note.is_empty()).then_some(note),
        };
        if self.name.is_empty() {
            return Err("Give the secret a name.".into());
        }
        if self.value.is_empty() {
            return Err(match self.kind {
                VaultKind::Note => "Write the note text.".into(),
                _ => "Enter the value. It is stored encrypted and never shown again.".into(),
            });
        }
        if self.username.chars().count() > MAX_VAULT_USERNAME_CHARS {
            return Err("Username is too long.".into());
        }
        if self.note.chars().count() > MAX_VAULT_NOTE_CHARS {
            return Err("Note is too long (500 characters).".into());
        }
        validate_vault_put(&self.name, &input).map_err(|e| e.to_string())?;
        Ok((self.name.clone(), input))
    }
}

impl Default for VaultForm {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for VaultForm {
    fn drop(&mut self) {
        self.value.zeroize();
    }
}

#[derive(Default)]
pub struct VaultView {
    pub entries: Vec<VaultEntry>,
    pub loaded: bool,
    pub loading: bool,
    pub error: Option<String>,
    pub form: Option<VaultForm>,
    /// The entry whose Delete is waiting for its confirm step.
    pub confirm_delete: Option<String>,
    pub deleting: Option<String>,
    pub notice: Option<String>,
    want_list: bool,
}

// --------------------------------------------------------------- browser --

pub struct PanelState {
    pub epoch: u64,
    /// The app window has focus. Polling stops while it does not.
    pub focused: bool,
    pub info: Option<BrowserInfo>,
    /// `data:` URI of the latest screenshot.
    pub shot: Option<String>,
    pub browser_error: Option<String>,
    /// The user pressed Close; the dock stays shut until the session ends.
    dismissed: bool,
    status_inflight: bool,
    shot_inflight: bool,
    next_status: Instant,
    next_shot: Instant,
    queue: VecDeque<PanelOp>,
    inputs_inflight: usize,
    pub control_busy: bool,
    pub vault: VaultView,
}

impl PanelState {
    pub fn new() -> Self {
        let now = Instant::now();
        Self {
            epoch: 0,
            focused: true,
            info: None,
            shot: None,
            browser_error: None,
            dismissed: false,
            status_inflight: false,
            shot_inflight: false,
            next_status: now,
            next_shot: now,
            queue: VecDeque::new(),
            inputs_inflight: 0,
            control_busy: false,
            vault: VaultView::default(),
        }
    }

    /// Forget everything tied to the previous session or server.
    pub fn reset(&mut self) {
        let epoch = self.epoch + 1;
        *self = Self::new();
        self.epoch = epoch;
    }

    /// The dock is visible: the user's browser session is active.
    pub fn open(&self) -> bool {
        self.info.as_ref().is_some_and(|info| info.active) && !self.dismissed
    }

    pub fn user_in_control(&self) -> bool {
        self.info
            .as_ref()
            .is_some_and(|info| info.active && info.controller == BrowserController::User)
    }

    pub fn set_focused(&mut self, focused: bool) {
        if focused && !self.focused {
            // Catch up at once instead of waiting out the last interval.
            let now = Instant::now();
            self.next_status = now;
            self.next_shot = now;
        }
        self.focused = focused;
    }

    // ---- user actions -------------------------------------------------

    pub fn take_over(&mut self) {
        if self.open() && !self.control_busy {
            self.control_busy = true;
            self.browser_error = None;
            self.queue.push_back(PanelOp::Takeover);
        }
    }

    pub fn hand_back(&mut self) {
        if self.open() && !self.control_busy {
            self.control_busy = true;
            self.browser_error = None;
            self.queue.push_back(PanelOp::Handback);
        }
    }

    /// Close the session on the server and shut the dock right away.
    pub fn close_browser(&mut self) {
        self.dismissed = true;
        self.browser_error = None;
        self.queue.push_back(PanelOp::Close);
    }

    /// Queue an input for the page. Ignored unless the user holds control.
    pub fn send_input(&mut self, input: BrowserInput) {
        if !self.user_in_control() {
            return;
        }
        if let (
            BrowserInput::Scroll { dy },
            Some(PanelOp::Input(BrowserInput::Scroll { dy: last })),
        ) = (&input, self.queue.back_mut())
        {
            *last = (*last + dy).clamp(-5000.0, 5000.0);
            return;
        }
        if self.queue.len() >= MAX_QUEUED_INPUTS {
            return;
        }
        self.queue.push_back(PanelOp::Input(input));
    }

    pub fn ask_vault_list(&mut self) {
        self.vault.want_list = true;
        self.vault.loading = true;
        self.vault.error = None;
    }

    pub fn open_add_form(&mut self) {
        self.vault.notice = None;
        self.vault.form = Some(VaultForm::new());
    }

    pub fn open_replace_form(&mut self, name: &str) {
        if let Some(entry) = self.vault.entries.iter().find(|e| e.name == name) {
            self.vault.notice = None;
            self.vault.confirm_delete = None;
            self.vault.form = Some(VaultForm::replacing(entry));
        }
    }

    pub fn close_form(&mut self) {
        self.vault.form = None;
    }

    /// Validate and queue the form's write.
    pub fn save_vault_form(&mut self) {
        let Some(form) = self.vault.form.as_mut() else {
            return;
        };
        if form.saving {
            return;
        }
        match form.build() {
            Ok((name, input)) => {
                form.saving = true;
                form.error = None;
                self.queue.push_back(PanelOp::VaultPut { name, input });
            }
            Err(message) => form.error = Some(message),
        }
    }

    pub fn ask_delete(&mut self, name: &str) {
        self.vault.confirm_delete = Some(name.to_owned());
    }

    pub fn cancel_delete(&mut self) {
        self.vault.confirm_delete = None;
    }

    pub fn confirm_delete(&mut self) {
        if let Some(name) = self.vault.confirm_delete.take() {
            self.vault.deleting = Some(name.clone());
            self.queue.push_back(PanelOp::VaultDelete { name });
        }
    }

    // ---- scheduler ----------------------------------------------------

    /// The next request to send, if any is due. Call in a loop until `None`.
    pub fn next_op(&mut self, now: Instant) -> Option<PanelOp> {
        if self.vault.want_list {
            self.vault.want_list = false;
            return Some(PanelOp::VaultList);
        }
        if let Some(front) = self.queue.front() {
            let is_input = matches!(front, PanelOp::Input(_));
            if !is_input || self.inputs_inflight < MAX_INPUTS_IN_FLIGHT {
                let op = self.queue.pop_front()?;
                if is_input {
                    self.inputs_inflight += 1;
                }
                return Some(op);
            }
        }
        if !self.focused {
            return None;
        }
        if !self.status_inflight && now >= self.next_status {
            self.status_inflight = true;
            self.next_status = now + STATUS_EVERY;
            return Some(PanelOp::Status);
        }
        if self.open() && !self.shot_inflight && now >= self.next_shot {
            self.shot_inflight = true;
            self.next_shot = now + SHOT_EVERY;
            return Some(PanelOp::Shot);
        }
        None
    }

    /// Fold a result in. Returns true when the server rejected the session
    /// token (the caller signs the user out).
    pub fn apply(&mut self, done: PanelDone, now: Instant) -> bool {
        if done.epoch != self.epoch {
            return false;
        }
        let unauthorized = matches!(done.result, Err(ApiError::Http(401)));
        match done.kind {
            PanelKind::Status => self.status_inflight = false,
            PanelKind::Shot => self.shot_inflight = false,
            PanelKind::Input => {
                self.inputs_inflight = self.inputs_inflight.saturating_sub(1);
            }
            PanelKind::Takeover | PanelKind::Handback => self.control_busy = false,
            _ => {}
        }
        match done.result {
            Ok(reply) => self.apply_reply(reply, now),
            Err(error) => self.apply_error(done.kind, error, now),
        }
        unauthorized
    }

    fn set_info(&mut self, info: BrowserInfo) {
        let was_active = self.info.as_ref().is_some_and(|i| i.active);
        if info.active && !was_active {
            // A fresh session: never show the previous one's last frame.
            self.shot = None;
        }
        if !info.active {
            self.dismissed = false;
        }
        self.info = Some(info);
    }

    fn apply_reply(&mut self, reply: PanelReply, now: Instant) {
        match reply {
            PanelReply::Status(info) => {
                self.browser_error = None;
                self.set_info(info);
            }
            PanelReply::Control(info) | PanelReply::Input(info) => {
                self.browser_error = None;
                self.set_info(info);
            }
            PanelReply::Closed => {
                self.next_status = now;
            }
            PanelReply::Shot(bytes) => {
                if self.open() {
                    self.shot = Some(jpeg_data_uri(&bytes));
                }
            }
            PanelReply::VaultList(entries) => {
                self.vault.entries = entries;
                self.vault.loaded = true;
                self.vault.loading = false;
                self.vault.error = None;
            }
            PanelReply::VaultSaved(entry) => {
                // Wipe the value now; the form closes with the saved notice.
                self.vault.form = None;
                self.vault.notice = Some(format!("Saved \u{201c}{}\u{201d}.", entry.name));
                match self.vault.entries.iter_mut().find(|e| e.name == entry.name) {
                    Some(existing) => *existing = entry,
                    None => {
                        self.vault.entries.push(entry);
                        self.vault.entries.sort_by(|a, b| a.name.cmp(&b.name));
                    }
                }
                self.vault.loaded = true;
            }
            PanelReply::VaultDeleted(name) => {
                self.vault.deleting = None;
                self.vault.entries.retain(|e| e.name != name);
                self.vault.notice = Some(format!("Deleted \u{201c}{name}\u{201d}."));
            }
        }
    }

    fn apply_error(&mut self, kind: PanelKind, error: ApiError, now: Instant) {
        let message = error.to_string();
        match kind {
            PanelKind::Status => {
                // A flaky poll keeps the last good view; it only matters
                // once the dock is up.
                if self.open() {
                    self.browser_error = Some(message);
                }
            }
            PanelKind::Shot => {
                if error == ApiError::Http(404) {
                    // The session ended between the two polls.
                    self.shot = None;
                    self.next_status = now;
                } else if self.open() {
                    self.browser_error = Some(message);
                }
            }
            PanelKind::Takeover | PanelKind::Handback | PanelKind::Input | PanelKind::Close => {
                self.browser_error = Some(message);
                self.next_status = now;
            }
            PanelKind::VaultList => {
                self.vault.loading = false;
                self.vault.error = Some(message);
            }
            PanelKind::VaultPut => {
                if let Some(form) = self.vault.form.as_mut() {
                    form.saving = false;
                    form.error = Some(message);
                } else {
                    self.vault.error = Some(message);
                }
            }
            PanelKind::VaultDelete => {
                self.vault.deleting = None;
                self.vault.error = Some(message);
            }
        }
    }
}

impl Default for PanelState {
    fn default() -> Self {
        Self::new()
    }
}

// ------------------------------------------------------------- encoding --

const B64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

fn base64(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let n = (u32::from(chunk[0]) << 16)
            | (u32::from(*chunk.get(1).unwrap_or(&0)) << 8)
            | u32::from(*chunk.get(2).unwrap_or(&0));
        out.push(B64[(n >> 18) as usize & 63] as char);
        out.push(B64[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 {
            B64[(n >> 6) as usize & 63] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            B64[n as usize & 63] as char
        } else {
            '='
        });
    }
    out
}

pub fn jpeg_data_uri(bytes: &[u8]) -> String {
    format!("data:image/jpeg;base64,{}", base64(bytes))
}

/// Address-field text to a navigable URL: a bare host gets `https://`.
pub fn address_to_url(raw: &str) -> Option<String> {
    let text = raw.trim();
    if text.is_empty() || text.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return None;
    }
    let lower = text.to_ascii_lowercase();
    let url = if lower.starts_with("http://") || lower.starts_with("https://") {
        text.to_owned()
    } else if lower.contains("://") {
        return None;
    } else {
        format!("https://{text}")
    };
    (url.chars().count() <= 2048).then_some(url)
}

/// Short, plain label for a vault entry's last-used stamp.
pub fn last_used_label(stamp: Option<&str>) -> String {
    match stamp.and_then(|s| s.get(..10)) {
        Some(date) => format!("Last used {date}"),
        None => "Never used".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info(active: bool, controller: BrowserController) -> BrowserInfo {
        BrowserInfo {
            active,
            url: active.then(|| "https://example.test/".into()),
            title: active.then(|| "Example".into()),
            controller,
            updated_at: "2026-11-06T12:00:00Z".into(),
        }
    }

    fn done(
        state: &PanelState,
        kind: PanelKind,
        result: Result<PanelReply, ApiError>,
    ) -> PanelDone {
        PanelDone {
            epoch: state.epoch,
            kind,
            result,
        }
    }

    fn entry(name: &str) -> VaultEntry {
        VaultEntry {
            name: name.into(),
            kind: VaultKind::ApiKey,
            username: None,
            domains: vec![],
            note: None,
            created_at: "2026-11-01T10:00:00Z".into(),
            updated_at: "2026-11-01T10:00:00Z".into(),
            last_used_at: None,
        }
    }

    #[test]
    fn base64_matches_the_rfc_vectors() {
        for (input, expected) in [
            ("", ""),
            ("f", "Zg=="),
            ("fo", "Zm8="),
            ("foo", "Zm9v"),
            ("foob", "Zm9vYg=="),
            ("fooba", "Zm9vYmE="),
            ("foobar", "Zm9vYmFy"),
        ] {
            assert_eq!(base64(input.as_bytes()), expected);
        }
        assert!(jpeg_data_uri(&[0xFF, 0xD8, 0xFF]).starts_with("data:image/jpeg;base64,/9j/"));
    }

    #[test]
    fn status_polls_every_two_seconds_and_only_while_focused() {
        let mut state = PanelState::new();
        let t0 = Instant::now();
        assert!(matches!(state.next_op(t0), Some(PanelOp::Status)));
        // One in flight at a time.
        assert!(state.next_op(t0 + Duration::from_secs(5)).is_none());
        state.apply(
            done(
                &state,
                PanelKind::Status,
                Ok(PanelReply::Status(info(false, BrowserController::Bot))),
            ),
            t0,
        );
        assert!(state.next_op(t0 + Duration::from_millis(1900)).is_none());
        assert!(matches!(
            state.next_op(t0 + Duration::from_millis(2100)),
            Some(PanelOp::Status)
        ));
        state.apply(
            done(
                &state,
                PanelKind::Status,
                Ok(PanelReply::Status(info(false, BrowserController::Bot))),
            ),
            t0,
        );
        state.set_focused(false);
        assert!(state.next_op(t0 + Duration::from_secs(60)).is_none());
        state.set_focused(true);
        assert!(matches!(
            state.next_op(Instant::now() + Duration::from_millis(1)),
            Some(PanelOp::Status)
        ));
    }

    #[test]
    fn screenshots_poll_at_most_twice_a_second_and_only_while_the_dock_is_open() {
        let mut state = PanelState::new();
        let t0 = Instant::now();
        state.next_op(t0); // status
        state.apply(
            done(
                &state,
                PanelKind::Status,
                Ok(PanelReply::Status(info(false, BrowserController::Bot))),
            ),
            t0,
        );
        // Inactive: no screenshot, ever.
        assert!(state.next_op(t0 + Duration::from_millis(600)).is_none());
        state.apply(
            done(
                &state,
                PanelKind::Status,
                Ok(PanelReply::Status(info(true, BrowserController::Bot))),
            ),
            t0,
        );
        assert!(state.open());
        let t1 = t0 + Duration::from_millis(700);
        assert!(matches!(state.next_op(t1), Some(PanelOp::Shot)));
        assert!(state.next_op(t1 + Duration::from_millis(100)).is_none());
        state.apply(
            done(
                &state,
                PanelKind::Shot,
                Ok(PanelReply::Shot(vec![0xFF, 0xD8, 0xFF, 0])),
            ),
            t1,
        );
        assert!(state.shot.is_some());
        assert!(state.next_op(t1 + Duration::from_millis(400)).is_none());
        assert!(matches!(
            state.next_op(t1 + Duration::from_millis(501)),
            Some(PanelOp::Shot)
        ));
        // Blurred: nothing is polled.
        state.apply(
            done(
                &state,
                PanelKind::Shot,
                Ok(PanelReply::Shot(vec![0xFF, 0xD8, 0xFF, 0])),
            ),
            t1,
        );
        state.set_focused(false);
        assert!(state.next_op(t1 + Duration::from_secs(30)).is_none());
    }

    #[test]
    fn a_new_session_drops_the_old_frame_and_close_shuts_the_dock() {
        let mut state = PanelState::new();
        let t0 = Instant::now();
        state.apply(
            done(
                &state,
                PanelKind::Status,
                Ok(PanelReply::Status(info(true, BrowserController::Bot))),
            ),
            t0,
        );
        state.apply(
            done(
                &state,
                PanelKind::Shot,
                Ok(PanelReply::Shot(vec![0xFF, 0xD8, 0xFF, 0])),
            ),
            t0,
        );
        assert!(state.shot.is_some());
        state.close_browser();
        assert!(!state.open());
        assert!(matches!(state.next_op(t0), Some(PanelOp::Close)));
        // Still reported active for a moment: the dock stays shut.
        state.apply(
            done(
                &state,
                PanelKind::Status,
                Ok(PanelReply::Status(info(true, BrowserController::Bot))),
            ),
            t0,
        );
        assert!(!state.open());
        state.apply(
            done(
                &state,
                PanelKind::Status,
                Ok(PanelReply::Status(info(false, BrowserController::Bot))),
            ),
            t0,
        );
        state.apply(
            done(
                &state,
                PanelKind::Status,
                Ok(PanelReply::Status(info(true, BrowserController::Bot))),
            ),
            t0,
        );
        assert!(state.open());
        assert!(state.shot.is_none());
    }

    #[test]
    fn input_needs_user_control_and_scroll_events_coalesce() {
        let mut state = PanelState::new();
        let t0 = Instant::now();
        state.apply(
            done(
                &state,
                PanelKind::Status,
                Ok(PanelReply::Status(info(true, BrowserController::Bot))),
            ),
            t0,
        );
        state.send_input(BrowserInput::Click { x: 1.0, y: 2.0 });
        assert!(state.queue.is_empty());
        state.take_over();
        assert!(matches!(state.next_op(t0), Some(PanelOp::Takeover)));
        state.take_over(); // busy: ignored
        assert!(state.queue.is_empty());
        state.apply(
            done(
                &state,
                PanelKind::Takeover,
                Ok(PanelReply::Control(info(true, BrowserController::User))),
            ),
            t0,
        );
        assert!(state.user_in_control());
        state.send_input(BrowserInput::Scroll { dy: 100.0 });
        state.send_input(BrowserInput::Scroll { dy: 50.0 });
        assert_eq!(state.queue.len(), 1);
        match state.next_op(t0) {
            Some(PanelOp::Input(BrowserInput::Scroll { dy })) => assert_eq!(dy, 150.0),
            _ => panic!("expected the merged scroll"),
        }
        // At most four inputs in flight.
        for i in 0..6 {
            state.send_input(BrowserInput::Click {
                x: f64::from(i),
                y: 0.0,
            });
        }
        let mut sent = 1; // the scroll above is still in flight
        while let Some(PanelOp::Input(_)) = state.next_op(t0) {
            sent += 1;
        }
        assert_eq!(sent, MAX_INPUTS_IN_FLIGHT);
        // Handing back ends input.
        state.apply(
            done(
                &state,
                PanelKind::Input,
                Ok(PanelReply::Input(info(true, BrowserController::Bot))),
            ),
            t0,
        );
        assert!(!state.user_in_control());
        state.send_input(BrowserInput::Click { x: 1.0, y: 2.0 });
        assert!(
            !state
                .queue
                .iter()
                .any(|op| matches!(op, PanelOp::Input(BrowserInput::Click { x, .. }) if *x == 1.0))
        );
    }

    #[test]
    fn a_stale_epoch_is_ignored_and_401_is_reported() {
        let mut state = PanelState::new();
        let t0 = Instant::now();
        let mut old = done(
            &state,
            PanelKind::Status,
            Ok(PanelReply::Status(info(true, BrowserController::Bot))),
        );
        state.reset();
        old.epoch = 0;
        assert!(!state.apply(old, t0));
        assert!(state.info.is_none());
        let unauthorized = state.apply(
            done(&state, PanelKind::VaultList, Err(ApiError::Http(401))),
            t0,
        );
        assert!(unauthorized);
    }

    #[test]
    fn missing_screenshot_clears_the_frame_and_refreshes_status() {
        let mut state = PanelState::new();
        let t0 = Instant::now();
        state.apply(
            done(
                &state,
                PanelKind::Status,
                Ok(PanelReply::Status(info(true, BrowserController::Bot))),
            ),
            t0,
        );
        state.apply(
            done(
                &state,
                PanelKind::Shot,
                Ok(PanelReply::Shot(vec![0xFF, 0xD8, 0xFF, 0])),
            ),
            t0,
        );
        state.apply(done(&state, PanelKind::Shot, Err(ApiError::Http(404))), t0);
        assert!(state.shot.is_none());
        assert!(state.browser_error.is_none());
    }

    #[test]
    fn vault_form_validates_inline_and_wipes_the_value_after_a_save() {
        let mut state = PanelState::new();
        let t0 = Instant::now();
        state.open_add_form();
        state.save_vault_form();
        assert!(state.vault.form.as_ref().unwrap().error.is_some());
        assert!(state.queue.is_empty());

        let form = state.vault.form.as_mut().unwrap();
        form.name = "Bad Name".into();
        assert!(form.name_hint().is_some());
        form.name = "portal".into();
        assert!(form.name_hint().is_none());
        form.value = Zeroizing::new("synthetic-pass".into());
        form.domain_draft = "https://nope".into();
        assert!(!form.commit_domain());
        assert!(form.domain_error.is_some());
        form.domain_draft = " Example.COM ".into();
        state.save_vault_form(); // commits the pending chip, then queues
        let form = state.vault.form.as_ref().unwrap();
        assert_eq!(form.domains, ["example.com"]);
        assert!(form.saving);
        match state.next_op(t0) {
            Some(PanelOp::VaultPut { name, input }) => {
                assert_eq!(name, "portal");
                assert_eq!(input.value.as_str(), "synthetic-pass");
            }
            _ => panic!("expected the write"),
        }
        state.apply(
            PanelDone {
                epoch: 0,
                kind: PanelKind::VaultPut,
                result: Ok(PanelReply::VaultSaved(entry("portal"))),
            },
            t0,
        );
        // The form, and the value in it, is gone.
        assert!(state.vault.form.is_none());
        assert_eq!(state.vault.entries[0].name, "portal");
    }

    #[test]
    fn a_failed_save_keeps_the_form_and_shows_the_message() {
        let mut state = PanelState::new();
        let t0 = Instant::now();
        state.open_add_form();
        let form = state.vault.form.as_mut().unwrap();
        form.name = "portal".into();
        form.value = Zeroizing::new("v".into());
        state.save_vault_form();
        state.next_op(t0);
        state.apply(
            PanelDone {
                epoch: 0,
                kind: PanelKind::VaultPut,
                result: Err(ApiError::Rejected("name is taken".into())),
            },
            t0,
        );
        let form = state.vault.form.as_ref().unwrap();
        assert!(!form.saving);
        assert_eq!(form.error.as_deref(), Some("name is taken"));
    }

    #[test]
    fn replace_prefills_metadata_but_never_the_value_and_delete_needs_confirm() {
        let mut state = PanelState::new();
        let t0 = Instant::now();
        let mut stored = entry("portal");
        stored.kind = VaultKind::Password;
        stored.username = Some("me".into());
        stored.domains = vec!["example.com".into()];
        state.apply(
            done(
                &state,
                PanelKind::VaultList,
                Ok(PanelReply::VaultList(vec![stored])),
            ),
            t0,
        );
        state.open_replace_form("portal");
        let form = state.vault.form.as_ref().unwrap();
        assert!(form.replacing);
        assert_eq!(
            (form.name.as_str(), form.username.as_str()),
            ("portal", "me")
        );
        assert_eq!(form.domains, ["example.com"]);
        assert!(form.value.is_empty());

        state.ask_delete("portal");
        assert!(
            state
                .next_op(t0)
                .is_some_and(|op| matches!(op, PanelOp::Status))
        );
        assert!(
            !state
                .queue
                .iter()
                .any(|op| matches!(op, PanelOp::VaultDelete { .. }))
        );
        state.cancel_delete();
        assert!(state.vault.confirm_delete.is_none());
        state.ask_delete("portal");
        state.confirm_delete();
        assert!(matches!(
            state.next_op(t0),
            Some(PanelOp::VaultDelete { .. })
        ));
        state.apply(
            done(
                &state,
                PanelKind::VaultDelete,
                Ok(PanelReply::VaultDeleted("portal".into())),
            ),
            t0,
        );
        assert!(state.vault.entries.is_empty());
    }

    #[test]
    fn address_and_last_used_helpers() {
        assert_eq!(
            address_to_url("example.com/a").as_deref(),
            Some("https://example.com/a")
        );
        assert_eq!(
            address_to_url("http://example.com").as_deref(),
            Some("http://example.com")
        );
        assert_eq!(address_to_url("ftp://example.com"), None);
        assert_eq!(address_to_url("a b"), None);
        assert_eq!(address_to_url("  "), None);
        assert_eq!(last_used_label(None), "Never used");
        assert_eq!(
            last_used_label(Some("2026-11-05T09:00:00Z")),
            "Last used 2026-11-05"
        );
    }
}
