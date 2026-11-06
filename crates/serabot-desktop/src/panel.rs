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
    ApiClient, ApiError, BrowserController, BrowserInfo, BrowserInput, ConnectorUpdate,
    MAX_VAULT_DOMAINS, MAX_VAULT_NOTE_CHARS, MAX_VAULT_USERNAME_CHARS, VaultEntry, VaultKind,
    VaultPut, normalize_vault_domain, valid_vault_name, validate_vault_put,
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
/// Status poll interval while the window is unfocused (screenshots stop).
pub const UNFOCUSED_STATUS_EVERY: Duration = Duration::from_secs(30);
pub const SHOT_EVERY: Duration = Duration::from_millis(500);
const MAX_QUEUED_INPUTS: usize = 64;
const MAX_INPUTS_IN_FLIGHT: usize = 4;

pub enum PanelOp {
    VaultList,
    VaultPut {
        name: String,
        input: VaultPut,
    },
    VaultDelete {
        name: String,
    },
    Status,
    Shot,
    Takeover,
    Handback,
    Close,
    Input(BrowserInput),
    OneDriveStart,
    OneDriveDisconnect,
    /// Open the consent page of a per-user connector.
    ConnectorStart(String),
    /// Connect with a pasted token (write-only).
    ConnectorToken {
        id: String,
        token: Zeroizing<String>,
    },
    ConnectorDisconnect(String),
    /// Admin: save the Google OAuth client (write-only).
    GoogleSetup {
        client_id: Zeroizing<String>,
        client_secret: Zeroizing<String>,
    },
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
    OneDriveStart,
    OneDriveDisconnect,
    ConnectorStart,
    ConnectorToken,
    ConnectorDisconnect,
    GoogleSetup,
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
            Self::OneDriveStart => PanelKind::OneDriveStart,
            Self::OneDriveDisconnect => PanelKind::OneDriveDisconnect,
            Self::ConnectorStart(_) => PanelKind::ConnectorStart,
            Self::ConnectorToken { .. } => PanelKind::ConnectorToken,
            Self::ConnectorDisconnect(_) => PanelKind::ConnectorDisconnect,
            Self::GoogleSetup { .. } => PanelKind::GoogleSetup,
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
    OneDriveUrl(String),
    OneDriveDisconnected,
    ConnectorUrl(String),
    ConnectorChanged,
    GoogleSaved,
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
        PanelOp::OneDriveStart => client.onedrive_consent_start().map(PanelReply::OneDriveUrl),
        PanelOp::OneDriveDisconnect => client
            .onedrive_disconnect()
            .map(|()| PanelReply::OneDriveDisconnected),
        PanelOp::ConnectorStart(id) => client
            .connector_consent_start(id)
            .map(PanelReply::ConnectorUrl),
        PanelOp::ConnectorToken { id, token } => client
            .connector_set_token(id, token)
            .map(|()| PanelReply::ConnectorChanged),
        PanelOp::ConnectorDisconnect(id) => client
            .connector_disconnect(id)
            .map(|()| PanelReply::ConnectorChanged),
        PanelOp::GoogleSetup {
            client_id,
            client_secret,
        } => client
            .admin_set_connector(
                GOOGLE_SETUP_CONNECTOR,
                &ConnectorUpdate {
                    enabled: false,
                    values: std::collections::BTreeMap::from([
                        ("client_id".to_owned(), client_id.to_string()),
                        ("client_secret".to_owned(), client_secret.to_string()),
                    ]),
                    auth: None,
                },
            )
            .map(|_| PanelReply::GoogleSaved),
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
            self.domain_error = Some("Type just the site address, like example.com".into());
            return false;
        };
        if self.domains.contains(&domain) {
            self.domain_error = Some("That site is already listed.".into());
            return false;
        }
        if self.domains.len() >= MAX_VAULT_DOMAINS {
            self.domain_error = Some("You can list at most 16 sites.".into());
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

/// The per-user OneDrive connect / disconnect flow behind the Connectors page.
#[derive(Default)]
pub struct OneDriveFlow {
    /// A request is on its way.
    pub busy: bool,
    /// The consent page was opened; waiting for the user to finish there.
    pub waiting: bool,
    pub error: Option<String>,
    pending_url: Option<String>,
    refresh_due: bool,
}

impl OneDriveFlow {
    /// The consent address to open in the system browser, once.
    pub fn take_url(&mut self) -> Option<String> {
        self.pending_url.take()
    }

    /// True once after a change the connector list should be re-read for.
    pub fn take_refresh(&mut self) -> bool {
        std::mem::take(&mut self.refresh_due)
    }
}

/// The Google credentials are stored under the shared Google client secrets;
/// any Google connector id addresses them.
const GOOGLE_SETUP_CONNECTOR: &str = "google_drive";

/// The per-user connector flows (Outlook, Google, Notion) and the admin
/// Google setup behind the Connectors page. One request runs at a time.
#[derive(Default)]
pub struct ConnectorFlow {
    /// The connector whose request is on its way.
    pub busy: Option<String>,
    /// The connector whose consent page was opened and is awaiting the user.
    pub waiting: Option<String>,
    /// Plain-words error per connector id.
    pub errors: std::collections::BTreeMap<String, String>,
    pub google_busy: bool,
    pub google_error: Option<String>,
    /// The Google credentials were saved.
    pub google_saved: bool,
    pending_url: Option<String>,
    refresh_due: bool,
}

impl ConnectorFlow {
    /// The consent address to open in the system browser, once.
    pub fn take_url(&mut self) -> Option<String> {
        self.pending_url.take()
    }

    /// True once after a change the connector list should be re-read for.
    pub fn take_refresh(&mut self) -> bool {
        std::mem::take(&mut self.refresh_due)
    }

    pub fn busy_with(&self, id: &str) -> bool {
        self.busy.as_deref() == Some(id)
    }
}

/// What went wrong, in everyday words.
fn connector_error_text(error: &ApiError) -> String {
    match error {
        ApiError::Http(409) => {
            "Not set up yet. Ask your admin to finish the setup, then try again.".into()
        }
        ApiError::Http(422) => "That was not accepted. Check it and try again.".into(),
        ApiError::Http(403) => "You are not allowed to do that.".into(),
        other => other.to_string(),
    }
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
    /// The user ended the session from the dock; it stays shut until the
    /// session is gone.
    dismissed: bool,
    /// The user hid the panel. The session keeps running; the header's
    /// "Watch browser" button brings the panel back.
    hidden: bool,
    status_inflight: bool,
    shot_inflight: bool,
    /// Consecutive 429s on background polls; resets on any success.
    rate_attempts: u32,
    /// No background poll is sent before this instant (429 backoff).
    poll_hold_until: Instant,
    next_status: Instant,
    next_shot: Instant,
    queue: VecDeque<PanelOp>,
    inputs_inflight: usize,
    pub control_busy: bool,
    pub vault: VaultView,
    pub onedrive: OneDriveFlow,
    pub connectors: ConnectorFlow,
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
            hidden: false,
            status_inflight: false,
            shot_inflight: false,
            rate_attempts: 0,
            poll_hold_until: now,
            next_status: now,
            next_shot: now,
            queue: VecDeque::new(),
            inputs_inflight: 0,
            control_busy: false,
            vault: VaultView::default(),
            onedrive: OneDriveFlow::default(),
            connectors: ConnectorFlow::default(),
        }
    }

    /// Forget everything tied to the previous session or server.
    pub fn reset(&mut self) {
        let epoch = self.epoch + 1;
        *self = Self::new();
        self.epoch = epoch;
    }

    /// A browser session exists for the acting user (shown or hidden).
    pub fn session_active(&self) -> bool {
        self.info.as_ref().is_some_and(|info| info.active) && !self.dismissed
    }

    /// The dock is visible: the session is active and the panel is not hidden.
    pub fn open(&self) -> bool {
        self.session_active() && !self.hidden
    }

    /// Hide the panel without touching the session.
    pub fn hide(&mut self) {
        self.hidden = true;
    }

    /// Bring the panel back and fetch a fresh frame at once.
    pub fn show(&mut self) {
        self.hidden = false;
        self.next_shot = Instant::now();
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

    /// Ask the server for the Microsoft consent page for the acting user.
    pub fn connect_onedrive(&mut self) {
        if self.onedrive.busy {
            return;
        }
        self.onedrive.busy = true;
        self.onedrive.error = None;
        self.queue.push_back(PanelOp::OneDriveStart);
    }

    /// Forget the acting user's OneDrive on the server.
    pub fn disconnect_onedrive(&mut self) {
        if self.onedrive.busy {
            return;
        }
        self.onedrive.busy = true;
        self.onedrive.error = None;
        self.queue.push_back(PanelOp::OneDriveDisconnect);
    }

    /// Open the consent page of a per-user connector (Outlook, Google, Notion).
    pub fn connect_connector(&mut self, id: &str) {
        if self.connectors.busy.is_some() {
            return;
        }
        self.connectors.busy = Some(id.to_owned());
        self.connectors.errors.remove(id);
        self.queue.push_back(PanelOp::ConnectorStart(id.to_owned()));
    }

    /// Connect with a pasted token (Notion). The value is wiped after sending.
    pub fn connect_connector_token(&mut self, id: &str, token: &str) {
        let token = token.trim();
        if self.connectors.busy.is_some() {
            return;
        }
        if token.is_empty() {
            self.connectors
                .errors
                .insert(id.to_owned(), "Paste the token first.".into());
            return;
        }
        self.connectors.busy = Some(id.to_owned());
        self.connectors.errors.remove(id);
        self.queue.push_back(PanelOp::ConnectorToken {
            id: id.to_owned(),
            token: Zeroizing::new(token.to_owned()),
        });
    }

    /// Forget the acting user's connection to one product.
    pub fn disconnect_connector(&mut self, id: &str) {
        if self.connectors.busy.is_some() {
            return;
        }
        self.connectors.busy = Some(id.to_owned());
        self.connectors.errors.remove(id);
        self.queue
            .push_back(PanelOp::ConnectorDisconnect(id.to_owned()));
    }

    pub fn clear_connector_error(&mut self, id: &str) {
        self.connectors.errors.remove(id);
    }

    /// Admin: save the Google OAuth client id and secret.
    pub fn save_google_setup(&mut self, client_id: &str, client_secret: &str) {
        let (client_id, client_secret) = (client_id.trim(), client_secret.trim());
        if self.connectors.google_busy {
            return;
        }
        self.connectors.google_saved = false;
        if !crate::valid_credential(client_id) || !crate::valid_credential(client_secret) {
            self.connectors.google_error =
                Some("Paste both the client ID and the client secret.".into());
            return;
        }
        self.connectors.google_error = None;
        self.connectors.google_busy = true;
        self.queue.push_back(PanelOp::GoogleSetup {
            client_id: Zeroizing::new(client_id.to_owned()),
            client_secret: Zeroizing::new(client_secret.to_owned()),
        });
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
        if now < self.poll_hold_until {
            return None;
        }
        if !self.status_inflight && now >= self.next_status {
            self.status_inflight = true;
            self.next_status = now
                + if self.focused {
                    STATUS_EVERY
                } else {
                    UNFOCUSED_STATUS_EVERY
                };
            return Some(PanelOp::Status);
        }
        if self.focused && self.open() && !self.shot_inflight && now >= self.next_shot {
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
            PanelKind::OneDriveStart | PanelKind::OneDriveDisconnect => self.onedrive.busy = false,
            PanelKind::GoogleSetup => self.connectors.google_busy = false,
            _ => {}
        }
        match done.result {
            Ok(reply) => {
                if matches!(done.kind, PanelKind::Status | PanelKind::Shot) {
                    self.rate_attempts = 0;
                }
                self.apply_reply(reply, now)
            }
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
            self.hidden = false;
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
            PanelReply::OneDriveUrl(url) => {
                self.onedrive.pending_url = Some(url);
                self.onedrive.waiting = true;
            }
            PanelReply::OneDriveDisconnected => {
                self.onedrive.waiting = false;
                self.onedrive.refresh_due = true;
            }
            PanelReply::ConnectorUrl(url) => {
                self.connectors.pending_url = Some(url);
                self.connectors.waiting = self.connectors.busy.take();
            }
            PanelReply::ConnectorChanged => {
                self.connectors.busy = None;
                self.connectors.waiting = None;
                self.connectors.refresh_due = true;
            }
            PanelReply::GoogleSaved => {
                self.connectors.google_saved = true;
                self.connectors.google_error = None;
                self.connectors.refresh_due = true;
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
        if error == ApiError::Http(429) && matches!(kind, PanelKind::Status | PanelKind::Shot) {
            // A rate-limited background poll backs off quietly: no banner.
            self.rate_attempts = self.rate_attempts.saturating_add(1);
            self.poll_hold_until = now
                + crate::api::poll_backoff(
                    crate::api::last_retry_after_secs(),
                    self.rate_attempts,
                    crate::api::jitter_seed(),
                );
            return;
        }
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
            PanelKind::OneDriveStart | PanelKind::OneDriveDisconnect => {
                self.onedrive.error = Some(message);
            }
            PanelKind::ConnectorStart
            | PanelKind::ConnectorToken
            | PanelKind::ConnectorDisconnect => {
                if let Some(id) = self.connectors.busy.take() {
                    self.connectors
                        .errors
                        .insert(id, connector_error_text(&error));
                }
            }
            PanelKind::GoogleSetup => {
                self.connectors.google_error = Some(connector_error_text(&error));
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
        // Blurred: the status poll slows to the unfocused interval.
        state.set_focused(false);
        assert!(matches!(
            state.next_op(t0 + Duration::from_secs(60)),
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
        assert!(state.next_op(t0 + Duration::from_secs(70)).is_none());
        assert!(matches!(
            state.next_op(t0 + Duration::from_secs(91)),
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
        // Blurred: no screenshots.
        state.apply(
            done(
                &state,
                PanelKind::Shot,
                Ok(PanelReply::Shot(vec![0xFF, 0xD8, 0xFF, 0])),
            ),
            t1,
        );
        state.set_focused(false);
        assert!(!matches!(
            state.next_op(t1 + Duration::from_secs(30)),
            Some(PanelOp::Shot)
        ));
    }

    #[test]
    fn a_429_on_a_background_poll_backs_off_without_a_banner() {
        let mut state = PanelState::new();
        let t0 = Instant::now();
        assert!(matches!(state.next_op(t0), Some(PanelOp::Status)));
        state.apply(
            done(&state, PanelKind::Status, Err(ApiError::Http(429))),
            t0,
        );
        assert!(state.browser_error.is_none());
        // Held for the 2 s backoff floor. The jitter adds 0-25 % on top, so
        // only a time below the floor is deterministic (2.1 s was flaky).
        assert!(state.next_op(t0 + Duration::from_millis(1900)).is_none());
        assert!(matches!(
            state.next_op(t0 + Duration::from_secs(130)),
            Some(PanelOp::Status)
        ));
    }

    #[test]
    fn poll_backoff_grows_honours_retry_after_and_is_capped() {
        use crate::api::poll_backoff;
        assert!(poll_backoff(1, 1, 0) >= Duration::from_secs(2));
        assert!(poll_backoff(1, 3, 0) >= Duration::from_secs(8));
        assert!(poll_backoff(30, 1, 0) >= Duration::from_secs(30));
        assert!(poll_backoff(500, 9, 999) <= Duration::from_millis(150_000));
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

    #[test]
    fn hiding_the_panel_keeps_the_session_and_stops_the_picture() {
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
        assert!(state.open() && state.session_active());
        state.hide();
        assert!(!state.open(), "the panel is hidden");
        assert!(state.session_active(), "the session is still running");
        // No screenshot is polled while hidden, and nothing is closed.
        let later = t0 + Duration::from_secs(5);
        let mut ops = Vec::new();
        while let Some(op) = state.next_op(later) {
            ops.push(op.kind());
        }
        assert!(!ops.contains(&PanelKind::Shot));
        assert!(!ops.contains(&PanelKind::Close));
        state.show();
        assert!(state.open());
        // A session that ends resets the hidden flag for the next one.
        state.hide();
        state.apply(
            done(
                &state,
                PanelKind::Status,
                Ok(PanelReply::Status(info(false, BrowserController::Bot))),
            ),
            later,
        );
        state.apply(
            done(
                &state,
                PanelKind::Status,
                Ok(PanelReply::Status(info(true, BrowserController::Bot))),
            ),
            later,
        );
        assert!(state.open(), "a fresh session opens the panel again");
    }

    #[test]
    fn onedrive_connect_hands_back_one_consent_address() {
        let mut state = PanelState::new();
        let t0 = Instant::now();
        state.connect_onedrive();
        assert!(state.onedrive.busy);
        state.connect_onedrive();
        assert!(matches!(state.next_op(t0), Some(PanelOp::OneDriveStart)));
        assert!(
            !matches!(state.next_op(t0), Some(PanelOp::OneDriveStart)),
            "a second click while busy queues nothing"
        );
        state.apply(
            done(
                &state,
                PanelKind::OneDriveStart,
                Ok(PanelReply::OneDriveUrl(
                    "https://login.example.test/consent".into(),
                )),
            ),
            t0,
        );
        assert!(!state.onedrive.busy && state.onedrive.waiting);
        assert_eq!(
            state.onedrive.take_url().as_deref(),
            Some("https://login.example.test/consent")
        );
        assert_eq!(state.onedrive.take_url(), None, "opened once");
    }

    #[test]
    fn onedrive_disconnect_asks_for_a_fresh_connector_list() {
        let mut state = PanelState::new();
        let t0 = Instant::now();
        state.disconnect_onedrive();
        assert!(matches!(
            state.next_op(t0),
            Some(PanelOp::OneDriveDisconnect)
        ));
        state.apply(
            done(
                &state,
                PanelKind::OneDriveDisconnect,
                Ok(PanelReply::OneDriveDisconnected),
            ),
            t0,
        );
        assert!(state.onedrive.take_refresh());
        assert!(!state.onedrive.take_refresh());
        state.apply(
            done(&state, PanelKind::OneDriveStart, Err(ApiError::Http(403))),
            t0,
        );
        assert!(state.onedrive.error.is_some() && !state.onedrive.busy);
    }

    #[test]
    fn domain_errors_use_plain_words() {
        let mut form = VaultForm::new();
        form.domain_draft = "https://bad".into();
        assert!(!form.commit_domain());
        assert_eq!(
            form.domain_error.as_deref(),
            Some("Type just the site address, like example.com")
        );
    }
}
