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

use crate::composio_flow::ComposioFlow;
use crate::vault_import::{self, ImportItem};
use crate::worker::Credentials;
use crate::{
    ApiClient, ApiError, BrowserController, BrowserHistory, BrowserInfo, BrowserInput,
    BrowserProfile, BrowserSessionInfo, BrowserSessionsList, ConnectorUpdate, DEFAULT_PROFILE,
    MAX_VAULT_DOMAINS, MAX_VAULT_ENTRIES, MAX_VAULT_NOTE_CHARS, MAX_VAULT_USERNAME_CHARS,
    PasswordManager, PasswordManagerProbe, SigninHelperStatus, VaultEntry, VaultKind, VaultPut,
    VaultSource, normalize_vault_domain, valid_vault_name, valid_vault_reference,
    validate_vault_put,
};
use crate::{ComposioAccount, ComposioApp};
use std::{
    collections::{HashMap, VecDeque},
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
        mpsc::{self, Receiver, Sender},
    },
    time::{Duration, Instant},
};
use zeroize::{Zeroize, Zeroizing};

/// `GET /v1/browser/sessions` while the app has focus.
pub const STATUS_EVERY: Duration = Duration::from_secs(2);
/// Status poll interval while the window is unfocused (screenshots stop).
pub const UNFOCUSED_STATUS_EVERY: Duration = Duration::from_secs(30);
/// One live view (the dock or a focused browser): at most twice a second.
pub const SHOT_EVERY: Duration = Duration::from_millis(500);
/// The Browser page grid shares this budget: each card is refreshed about
/// once a second, and the request rate never goes past 3 per second.
pub const GRID_SHOT_EVERY: Duration = Duration::from_millis(1000);
pub const GRID_SHOT_FLOOR: Duration = Duration::from_millis(330);
const MAX_QUEUED_INPUTS: usize = 64;

/// A tab operation the user asks for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TabAction {
    Switch(String),
    Close(String),
}

impl TabAction {
    fn tab(&self) -> &str {
        match self {
            Self::Switch(tab) | Self::Close(tab) => tab,
        }
    }
}
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
    /// Password managers: the status list, then save or clear the token
    /// (write-only).
    PasswordManagers,
    PasswordManagerSave {
        id: String,
        token: Zeroizing<String>,
    },
    PasswordManagerClear(String),
    PasswordManagerTest(String),
    /// List the owner's logins from a password manager on this PC (metadata).
    ImportList(VaultSource),
    /// Create the reference entries for the picked logins, as (name, write).
    VaultImport(Vec<(String, VaultPut)>),
    Status,
    Shot(String),
    Takeover(String),
    Handback(String),
    Close(String),
    Input(String, BrowserInput),
    /// Switch to or close a tab (user in control).
    Tab(String, TabAction),
    /// Open a new tab at a public address (user in control).
    TabOpen(String, String),
    /// Back, forward or reload on the active tab (user in control).
    History(String, BrowserHistory),
    ProfilesList,
    ProfileCreate(String),
    ProfileRename {
        id: String,
        name: String,
    },
    ProfileDelete(String),
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
    /// Admin: remove a connector's saved server-wide secrets (age store).
    ConnectorRemoveSecrets(String),
    /// Composio: save the user's own key (write-only).
    ComposioKey(Zeroizing<String>),
    ComposioKeyRemove,
    /// Composio: read the app list for a search text, and the linked apps.
    ComposioLoad(String),
    ComposioConnect(String),
    ComposioUnlink(String),
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
    PasswordManagers,
    PasswordManagerSave,
    PasswordManagerClear,
    PasswordManagerTest,
    ImportList,
    VaultImport,
    Status,
    Shot,
    Takeover,
    Handback,
    Close,
    Input,
    ProfilesList,
    ProfileChange,
    OneDriveStart,
    OneDriveDisconnect,
    ConnectorStart,
    ConnectorToken,
    ConnectorDisconnect,
    ConnectorRemoveSecrets,
    ComposioKey,
    ComposioKeyRemove,
    ComposioLoad,
    ComposioConnect,
    ComposioUnlink,
    GoogleSetup,
}

impl PanelOp {
    pub fn kind(&self) -> PanelKind {
        match self {
            Self::VaultList => PanelKind::VaultList,
            Self::VaultPut { .. } => PanelKind::VaultPut,
            Self::VaultDelete { .. } => PanelKind::VaultDelete,
            Self::PasswordManagers => PanelKind::PasswordManagers,
            Self::PasswordManagerSave { .. } => PanelKind::PasswordManagerSave,
            Self::PasswordManagerClear(_) => PanelKind::PasswordManagerClear,
            Self::PasswordManagerTest(_) => PanelKind::PasswordManagerTest,
            Self::ImportList(_) => PanelKind::ImportList,
            Self::VaultImport(_) => PanelKind::VaultImport,
            Self::Status => PanelKind::Status,
            Self::Shot(_) => PanelKind::Shot,
            Self::Takeover(_) => PanelKind::Takeover,
            Self::Handback(_) => PanelKind::Handback,
            Self::Close(_) => PanelKind::Close,
            Self::Input(..) | Self::Tab(..) | Self::TabOpen(..) | Self::History(..) => {
                PanelKind::Input
            }
            Self::ProfilesList => PanelKind::ProfilesList,
            Self::ProfileCreate(_) | Self::ProfileRename { .. } | Self::ProfileDelete(_) => {
                PanelKind::ProfileChange
            }
            Self::OneDriveStart => PanelKind::OneDriveStart,
            Self::OneDriveDisconnect => PanelKind::OneDriveDisconnect,
            Self::ConnectorStart(_) => PanelKind::ConnectorStart,
            Self::ConnectorToken { .. } => PanelKind::ConnectorToken,
            Self::ConnectorDisconnect(_) => PanelKind::ConnectorDisconnect,
            Self::ConnectorRemoveSecrets(_) => PanelKind::ConnectorRemoveSecrets,
            Self::ComposioKey(_) => PanelKind::ComposioKey,
            Self::ComposioKeyRemove => PanelKind::ComposioKeyRemove,
            Self::ComposioLoad(_) => PanelKind::ComposioLoad,
            Self::ComposioConnect(_) => PanelKind::ComposioConnect,
            Self::ComposioUnlink(_) => PanelKind::ComposioUnlink,
            Self::GoogleSetup { .. } => PanelKind::GoogleSetup,
        }
    }
}

pub enum PanelReply {
    VaultList(Vec<VaultEntry>),
    VaultSaved(VaultEntry),
    VaultDeleted(String),
    PasswordManagers(Vec<PasswordManager>),
    PasswordManagerSaved(PasswordManager),
    PasswordManagerCleared(String),
    PasswordManagerTested {
        id: String,
        probe: PasswordManagerProbe,
    },
    ImportList(Vec<ImportItem>),
    VaultImported(usize),
    Sessions(BrowserSessionsList),
    Shot(String, Vec<u8>),
    Control(BrowserSessionInfo),
    Closed,
    Input(BrowserSessionInfo),
    Profiles(Vec<BrowserProfile>),
    ProfileChanged(String),
    OneDriveUrl(String),
    OneDriveDisconnected,
    ConnectorUrl(String),
    ConnectorChanged,
    ComposioKeySaved,
    ComposioKeyRemoved,
    ComposioLoaded {
        query: String,
        apps: Vec<ComposioApp>,
        accounts: Vec<ComposioAccount>,
    },
    ComposioUrl {
        toolkit: String,
        url: String,
    },
    ComposioUnlinked,
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
        PanelOp::PasswordManagers => client.password_managers().map(PanelReply::PasswordManagers),
        PanelOp::PasswordManagerSave { id, token } => client
            .password_manager_save(id, token)
            .map(PanelReply::PasswordManagerSaved),
        PanelOp::PasswordManagerClear(id) => client
            .password_manager_clear(id)
            .map(|()| PanelReply::PasswordManagerCleared(id.clone())),
        PanelOp::PasswordManagerTest(id) => {
            client
                .password_manager_test(id)
                .map(|probe| PanelReply::PasswordManagerTested {
                    id: id.clone(),
                    probe,
                })
        }
        // Runs the owner's CLI on this PC: metadata only, nothing is stored.
        PanelOp::ImportList(source) => {
            vault_import::list_items(*source).map(PanelReply::ImportList)
        }
        PanelOp::VaultImport(batch) => {
            let mut written = 0;
            for (name, input) in batch {
                client.vault_put(name, input)?;
                written += 1;
            }
            Ok(PanelReply::VaultImported(written))
        }
        PanelOp::Status => client.browser_sessions().map(PanelReply::Sessions),
        PanelOp::Shot(id) => client
            .browser_session_screenshot(id)
            .map(|bytes| PanelReply::Shot(id.clone(), bytes)),
        PanelOp::Takeover(id) => client.browser_session_takeover(id).map(PanelReply::Control),
        PanelOp::Handback(id) => client.browser_session_handback(id).map(PanelReply::Control),
        PanelOp::Close(id) => client.browser_session_close(id).map(|_| PanelReply::Closed),
        PanelOp::Input(id, input) => client
            .browser_session_input(id, input)
            .map(PanelReply::Input),
        PanelOp::Tab(id, action) => match action {
            TabAction::Switch(tab) => client.browser_tab_switch(id, tab),
            TabAction::Close(tab) => client.browser_tab_close(id, tab),
        }
        .map(PanelReply::Input),
        PanelOp::TabOpen(id, url) => client.browser_tab_open(id, url).map(PanelReply::Input),
        PanelOp::History(id, action) => client
            .browser_session_history(id, *action)
            .map(PanelReply::Input),
        PanelOp::ProfilesList => client.browser_profiles().map(PanelReply::Profiles),
        PanelOp::ProfileCreate(name) => client.browser_profile_create(name).map(|profile| {
            PanelReply::ProfileChanged(format!("Added \u{201c}{}\u{201d}.", profile.name))
        }),
        PanelOp::ProfileRename { id, name } => {
            client.browser_profile_rename(id, name).map(|profile| {
                PanelReply::ProfileChanged(format!("Renamed to \u{201c}{}\u{201d}.", profile.name))
            })
        }
        PanelOp::ProfileDelete(id) => client
            .browser_profile_delete(id)
            .map(|()| PanelReply::ProfileChanged("Profile deleted.".to_owned())),
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
        PanelOp::ConnectorRemoveSecrets(id) => client
            .admin_remove_connector_secrets(id)
            .map(|()| PanelReply::ConnectorChanged),
        PanelOp::ComposioKey(key) => client
            .composio_set_key(key)
            .map(|()| PanelReply::ComposioKeySaved),
        PanelOp::ComposioKeyRemove => client
            .composio_remove_key()
            .map(|()| PanelReply::ComposioKeyRemoved),
        PanelOp::ComposioLoad(query) => client.composio_apps(query).and_then(|apps| {
            client
                .composio_accounts()
                .map(|accounts| PanelReply::ComposioLoaded {
                    query: query.clone(),
                    apps,
                    accounts,
                })
        }),
        PanelOp::ComposioConnect(toolkit) => {
            client
                .composio_connect(toolkit)
                .map(|url| PanelReply::ComposioUrl {
                    toolkit: toolkit.clone(),
                    url,
                })
        }
        PanelOp::ComposioUnlink(id) => client
            .composio_remove_account(id)
            .map(|()| PanelReply::ComposioUnlinked),
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
    /// Where the value comes from: typed here, or a password manager item.
    pub source: VaultSource,
    /// The item pointer, for a password manager source. Not a secret.
    pub reference: String,
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
            source: VaultSource::Local,
            reference: String::new(),
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
        form.source = entry.source;
        form.reference = entry.reference.clone().unwrap_or_default();
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
        let reference = self.reference.trim().to_owned();
        let is_reference = self.source.is_reference();
        let input = VaultPut {
            kind: self.kind,
            username: (self.kind == VaultKind::Password && !username.is_empty())
                .then_some(username),
            // A reference holds no value: the password manager supplies it when typed.
            value: Zeroizing::new(if is_reference {
                String::new()
            } else {
                self.value.to_string()
            }),
            domains: self.domains.clone(),
            note: (!note.is_empty()).then_some(note),
            source: self.source,
            reference: is_reference.then(|| reference.clone()),
        };
        if self.name.is_empty() {
            return Err("Give the secret a name.".into());
        }
        if is_reference {
            if self.kind == VaultKind::Note {
                return Err("A note cannot come from a password manager.".into());
            }
            if !valid_vault_reference(self.source, &reference) {
                return Err(match self.source {
                    VaultSource::ProtonPass => {
                        "Use pass://vault/item/field, the Proton Pass item and field to read."
                            .into()
                    }
                    _ => "Use op://vault/item/field, the 1Password item and field to read.".into(),
                });
            }
        } else if self.value.is_empty() {
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

/// The import picker: the logins one password manager listed, and which of
/// them the owner picked. Metadata only; nothing here holds a value.
#[derive(Default)]
pub struct ImportPicker {
    pub source: VaultSource,
    pub items: Vec<ImportItem>,
    pub picked: Vec<bool>,
    pub saving: bool,
    pub error: Option<String>,
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
    /// Password managers and their token/CLI status, once loaded.
    pub providers: Vec<PasswordManager>,
    pub providers_loaded: bool,
    /// The password manager whose token is being saved or cleared.
    pub provider_busy: Option<String>,
    /// The password manager whose server probe is in flight.
    pub provider_testing: Option<String>,
    pub provider_tests: std::collections::BTreeMap<String, PasswordManagerProbe>,
    pub provider_error: Option<String>,
    /// The import listing in flight, and the source it is for.
    pub import_loading: Option<VaultSource>,
    pub import_error: Option<String>,
    /// The logins listed for import, while the picker is open.
    pub import: Option<ImportPicker>,
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

/// The next picture is due after this long. One live view gets the full
/// rate; the grid spreads about one refresh a second over its cards.
fn shot_interval(targets: usize, grid: bool) -> Duration {
    if !grid {
        return SHOT_EVERY;
    }
    let n = u32::try_from(targets.max(1)).unwrap_or(u32::MAX);
    (GRID_SHOT_EVERY / n).max(GRID_SHOT_FLOOR)
}

fn profile_error_text(error: &ApiError) -> String {
    match error {
        ApiError::Http(409) => {
            "That can't be done: the default profile stays, and names must be different.".into()
        }
        ApiError::Http(404) => "That profile is gone. The list was refreshed.".into(),
        other => other.to_string(),
    }
}

/// What went wrong, in everyday words.
fn connector_error_text(error: &ApiError) -> String {
    match error {
        // Written for the user by the server (for example GitHub rejected the
        // token), so shown as is.
        ApiError::Coded { code, message, .. } if code.starts_with("connector_") => message.clone(),
        ApiError::Http(409) => {
            "Not set up yet. Ask your admin to finish the setup, then try again.".into()
        }
        ApiError::Http(422) => "That was not accepted. Check it and try again.".into(),
        ApiError::Http(403) => "You are not allowed to do that.".into(),
        other => other.to_string(),
    }
}

/// The codes the server sends for a Composio failure. Their messages are
/// already written for the user (with the fix Composio suggested), so they
/// are shown as they are.
const COMPOSIO_CODES: &[&str] = &[
    "composio_rejected",
    "composio_plan_required",
    "composio_not_found",
    "composio_rate_limited",
    "composio_unavailable",
    "composio_bad_request",
];

/// What went wrong with Composio, in everyday words.
fn composio_error_text(error: &ApiError) -> String {
    match error {
        ApiError::Coded { code, message, .. } if COMPOSIO_CODES.contains(&code.as_str()) => {
            message.clone()
        }
        ApiError::Http(409) => "Add your Composio key first.".into(),
        // The server answers 403 when no key is saved. It is not a policy block.
        ApiError::Http(403) => "Composio is not connected yet. Add your key first.".into(),
        ApiError::Http(422) => "Composio did not accept that key. SeraBot takes project keys (ak_), user keys (uak_) and Connect keys (ck_). Check it and try again.".into(),
        ApiError::Http(502..=504) => "Composio could not be reached. Try again in a moment.".into(),
        other => other.to_string(),
    }
}

// --------------------------------------------------------------- browser --

pub struct PanelState {
    pub epoch: u64,
    /// The app window has focus. Polling stops while it does not.
    pub focused: bool,
    /// The dock's own browser (the session of the current bot's profile).
    pub info: Option<BrowserInfo>,
    /// Every browser the server reports, one per profile.
    pub sessions: Vec<BrowserSessionInfo>,
    pub sessions_loaded: bool,
    /// From the latest `GET /v1/browser/sessions` envelope.
    pub idle_timeout_secs: Option<u64>,
    pub signin_helpers: Vec<SigninHelperStatus>,
    /// The profile (= session id) the dock watches.
    pub dock_profile: String,
    /// `data:` URI of the latest screenshot per session id.
    pub shots: HashMap<String, String>,
    /// The Browser page is the section on screen.
    pub page_open: bool,
    /// The session in the Browser page's focus view.
    pub focus: Option<String>,
    pub profiles: Vec<BrowserProfile>,
    pub profiles_loaded: bool,
    pub profiles_busy: bool,
    pub profiles_error: Option<String>,
    pub profiles_notice: Option<String>,
    want_profiles: bool,
    profiles_requested: bool,
    shot_inflight_id: Option<String>,
    shot_cursor: usize,
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
    pub composio: ComposioFlow,
}

impl PanelState {
    pub fn new() -> Self {
        let now = Instant::now();
        Self {
            epoch: 0,
            focused: true,
            info: None,
            sessions: Vec::new(),
            sessions_loaded: false,
            idle_timeout_secs: None,
            signin_helpers: Vec::new(),
            dock_profile: DEFAULT_PROFILE.to_owned(),
            shots: HashMap::new(),
            page_open: false,
            focus: None,
            profiles: Vec::new(),
            profiles_loaded: false,
            profiles_busy: false,
            profiles_error: None,
            profiles_notice: None,
            want_profiles: false,
            profiles_requested: false,
            shot_inflight_id: None,
            shot_cursor: 0,
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
            composio: ComposioFlow::default(),
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

    /// The dock is visible: the session is active and the panel is not
    /// hidden. The Browser page shows the same picture larger, so the dock
    /// steps aside while that page is open.
    pub fn open(&self) -> bool {
        self.session_active() && !self.hidden && !self.page_open
    }

    pub fn session(&self, id: &str) -> Option<&BrowserSessionInfo> {
        self.sessions.iter().find(|session| session.id == id)
    }

    /// Sessions with a browser actually open.
    pub fn active_sessions(&self) -> Vec<&BrowserSessionInfo> {
        self.sessions.iter().filter(|s| s.active).collect()
    }

    /// The session the user's buttons act on: the focused one on the
    /// Browser page, else the dock's.
    pub fn control_target(&self) -> Option<String> {
        if self.page_open {
            return self.focus.clone();
        }
        (self.session_active() && !self.hidden).then(|| self.dock_profile.clone())
    }

    /// Which browsers need a picture right now. Nothing unless the window is
    /// focused: the grid asks for every open browser, the focus view for one,
    /// the dock for its own.
    pub fn shot_targets(&self) -> Vec<String> {
        if !self.focused {
            return Vec::new();
        }
        if self.page_open {
            return match &self.focus {
                Some(id) => self
                    .session(id)
                    .filter(|s| s.active)
                    .map(|s| vec![s.id.clone()])
                    .unwrap_or_default(),
                None => self
                    .active_sessions()
                    .iter()
                    .map(|s| s.id.clone())
                    .collect(),
            };
        }
        if self.open() {
            vec![self.dock_profile.clone()]
        } else {
            Vec::new()
        }
    }

    /// The dock's latest picture.
    pub fn dock_shot(&self) -> Option<String> {
        self.shots.get(&self.dock_profile).cloned()
    }

    /// Point the dock at another profile's browser.
    pub fn set_dock_profile(&mut self, id: &str) {
        if self.dock_profile != id {
            self.dock_profile = id.to_owned();
            self.refresh_dock_info();
            self.next_shot = Instant::now();
        }
    }

    fn refresh_dock_info(&mut self) {
        let info = self
            .session(&self.dock_profile)
            .map(BrowserSessionInfo::as_info)
            .unwrap_or(BrowserInfo {
                active: false,
                url: None,
                title: None,
                controller: BrowserController::Bot,
                updated_at: String::new(),
            });
        self.set_info(info);
    }

    /// Open the focus view on one browser.
    pub fn focus_session(&mut self, id: &str) {
        self.focus = Some(id.to_owned());
        self.browser_error = None;
        self.next_shot = Instant::now();
    }

    /// Back to the grid.
    pub fn clear_focus(&mut self) {
        self.focus = None;
        self.browser_error = None;
        self.next_shot = Instant::now();
    }

    /// The Browser page came on or went off screen.
    pub fn set_page_open(&mut self, open: bool) {
        if open == self.page_open {
            return;
        }
        self.page_open = open;
        let now = Instant::now();
        if open {
            self.next_status = now;
            self.next_shot = now;
            self.ask_profiles();
        } else {
            self.focus = None;
        }
    }

    /// Re-read the profile list (the page opened, a bot's profile changed).
    pub fn ask_profiles(&mut self) {
        self.want_profiles = true;
        self.profiles_requested = true;
    }

    /// Read the profile list once per connection, so the dock knows which
    /// browser belongs to the bot being talked to.
    pub fn ensure_profiles(&mut self) {
        if !self.profiles_requested {
            self.ask_profiles();
        }
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
        self.control_target()
            .and_then(|id| self.session(&id))
            .is_some_and(|s| s.active && s.controller == BrowserController::User)
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
        if let Some(id) = self.control_target()
            && !self.control_busy
        {
            self.control_busy = true;
            self.browser_error = None;
            self.queue.push_back(PanelOp::Takeover(id));
        }
    }

    pub fn hand_back(&mut self) {
        if let Some(id) = self.control_target()
            && !self.control_busy
        {
            self.control_busy = true;
            self.browser_error = None;
            self.queue.push_back(PanelOp::Handback(id));
        }
    }

    /// Close the session on the server. From the dock the panel shuts right
    /// away; on the Browser page the card disappears with the next refresh.
    pub fn close_browser(&mut self) {
        let Some(id) = self
            .control_target()
            .or_else(|| (!self.page_open).then(|| self.dock_profile.clone()))
        else {
            return;
        };
        if !self.page_open {
            self.dismissed = true;
        }
        self.browser_error = None;
        self.queue.push_back(PanelOp::Close(id));
    }

    /// Queue an input for the page. Ignored unless the user holds control.
    pub fn send_input(&mut self, input: BrowserInput) {
        if !self.user_in_control() {
            return;
        }
        let Some(id) = self.control_target() else {
            return;
        };
        if let (
            BrowserInput::Scroll { dy },
            Some(PanelOp::Input(last_id, BrowserInput::Scroll { dy: last })),
        ) = (&input, self.queue.back_mut())
            && *last_id == id
        {
            *last = (*last + dy).clamp(-5000.0, 5000.0);
            return;
        }
        if self.queue.len() >= MAX_QUEUED_INPUTS {
            return;
        }
        self.queue.push_back(PanelOp::Input(id, input));
    }

    /// Switch to a tab. Ignored unless the user holds control.
    pub fn switch_tab(&mut self, tab: &str) {
        self.queue_tab(TabAction::Switch(tab.to_owned()));
    }

    /// Close a tab. Ignored unless the user holds control.
    pub fn close_tab(&mut self, tab: &str) {
        self.queue_tab(TabAction::Close(tab.to_owned()));
    }

    /// Open a new tab at the address typed in the address bar. Ignored
    /// unless the user holds control or the address is not a web address.
    pub fn open_tab(&mut self, raw_address: &str) -> bool {
        let Some(url) = address_to_url(raw_address) else {
            return false;
        };
        if !self.user_in_control() {
            return false;
        }
        let Some(id) = self.control_target() else {
            return false;
        };
        if self.queue.len() < MAX_QUEUED_INPUTS {
            self.queue.push_back(PanelOp::TabOpen(id, url));
        }
        true
    }

    /// Back, forward or reload. Ignored unless the user holds control.
    pub fn history(&mut self, action: BrowserHistory) {
        if !self.user_in_control() {
            return;
        }
        let Some(id) = self.control_target() else {
            return;
        };
        if self.queue.len() < MAX_QUEUED_INPUTS {
            self.queue.push_back(PanelOp::History(id, action));
        }
    }

    fn queue_tab(&mut self, action: TabAction) {
        if !self.user_in_control() || !crate::valid_tab_id(action.tab()) {
            return;
        }
        let Some(id) = self.control_target() else {
            return;
        };
        if self.queue.len() < MAX_QUEUED_INPUTS {
            self.queue.push_back(PanelOp::Tab(id, action));
        }
    }

    // ---- browser profiles ---------------------------------------------

    pub fn create_profile(&mut self, name: &str) {
        if self.profiles_busy {
            return;
        }
        if !crate::valid_profile_name(name) {
            self.profiles_error = Some("Give the profile a name of up to 40 characters.".into());
            return;
        }
        if self.profiles.len() >= crate::MAX_BROWSER_PROFILES {
            self.profiles_error = Some("You can have up to 8 profiles. Delete one first.".into());
            return;
        }
        self.profiles_busy = true;
        self.profiles_error = None;
        self.profiles_notice = None;
        self.queue
            .push_back(PanelOp::ProfileCreate(name.trim().to_owned()));
    }

    pub fn rename_profile(&mut self, id: &str, name: &str) {
        if self.profiles_busy {
            return;
        }
        if !crate::valid_profile_name(name) {
            self.profiles_error = Some("Give the profile a name of up to 40 characters.".into());
            return;
        }
        self.profiles_busy = true;
        self.profiles_error = None;
        self.profiles_notice = None;
        self.queue.push_back(PanelOp::ProfileRename {
            id: id.to_owned(),
            name: name.trim().to_owned(),
        });
    }

    pub fn delete_profile(&mut self, id: &str) {
        if self.profiles_busy {
            return;
        }
        if id == DEFAULT_PROFILE {
            self.profiles_error = Some("The default profile cannot be deleted.".into());
            return;
        }
        self.profiles_busy = true;
        self.profiles_error = None;
        self.profiles_notice = None;
        self.queue.push_back(PanelOp::ProfileDelete(id.to_owned()));
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

    /// Admin: remove the connector's saved server-side credentials (for
    /// example the stored ElevenLabs API key). Per-account sign-ins use
    /// `disconnect_connector` instead.
    pub fn remove_connector_secrets(&mut self, id: &str) {
        if self.connectors.busy.is_some() {
            return;
        }
        self.connectors.busy = Some(id.to_owned());
        self.connectors.errors.remove(id);
        self.queue
            .push_back(PanelOp::ConnectorRemoveSecrets(id.to_owned()));
    }

    /// Composio: save the user's own key. The value is wiped after sending.
    pub fn composio_save_key(&mut self, key: &str) {
        let key = key.trim();
        if self.composio.busy {
            return;
        }
        if key.is_empty() {
            self.composio.error = Some("Paste your Composio key first.".into());
            return;
        }
        self.composio.busy = true;
        self.composio.error = None;
        self.queue
            .push_back(PanelOp::ComposioKey(Zeroizing::new(key.to_owned())));
    }

    pub fn composio_remove_key(&mut self) {
        if self.composio.busy {
            return;
        }
        self.composio.busy = true;
        self.composio.error = None;
        self.queue.push_back(PanelOp::ComposioKeyRemove);
    }

    /// Open Composio's own sign-in page for one app.
    pub fn composio_connect(&mut self, toolkit: &str) {
        if self.composio.busy {
            return;
        }
        self.composio.busy = true;
        self.composio.error = None;
        self.queue
            .push_back(PanelOp::ComposioConnect(toolkit.to_owned()));
    }

    /// Unlink one app from the user's own account.
    pub fn composio_unlink(&mut self, account_id: &str) {
        if self.composio.busy {
            return;
        }
        self.composio.busy = true;
        self.composio.error = None;
        self.queue
            .push_back(PanelOp::ComposioUnlink(account_id.to_owned()));
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

    /// Load the password manager status list.
    pub fn ask_password_managers(&mut self) {
        self.queue.push_back(PanelOp::PasswordManagers);
    }

    /// Save the owner's access token for a password manager (write-only).
    pub fn save_password_manager(&mut self, id: String, token: Zeroizing<String>) {
        if self.vault.provider_busy.is_some() {
            return;
        }
        self.vault.provider_busy = Some(id.clone());
        self.vault.provider_error = None;
        self.queue
            .push_back(PanelOp::PasswordManagerSave { id, token });
    }

    /// Forget the owner's access token for a password manager.
    pub fn clear_password_manager(&mut self, id: &str) {
        if self.vault.provider_busy.is_some() {
            return;
        }
        self.vault.provider_busy = Some(id.to_owned());
        self.vault.provider_error = None;
        self.queue
            .push_back(PanelOp::PasswordManagerClear(id.to_owned()));
    }

    /// Probe the saved token against the password manager CLI on the server.
    pub fn test_password_manager(&mut self, id: String) {
        if self.vault.provider_busy.is_some() || self.vault.provider_testing.is_some() {
            return;
        }
        self.vault.provider_testing = Some(id.clone());
        self.vault.provider_error = None;
        self.queue.push_back(PanelOp::PasswordManagerTest(id));
    }

    /// List the logins of a password manager on this PC, for the picker.
    pub fn start_import(&mut self, source: VaultSource) {
        self.vault.import = None;
        self.vault.import_error = None;
        self.vault.import_loading = Some(source);
        self.queue.push_back(PanelOp::ImportList(source));
    }

    pub fn toggle_import_item(&mut self, index: usize) {
        if let Some(picked) = self
            .vault
            .import
            .as_mut()
            .and_then(|picker| picker.picked.get_mut(index))
        {
            *picked = !*picked;
        }
    }

    pub fn close_import(&mut self) {
        self.vault.import = None;
        self.vault.import_loading = None;
        self.vault.import_error = None;
    }

    /// Queue one reference entry per picked login. Each gets a unique name,
    /// its site from the item's URL, and no value: the password manager
    /// supplies it when the bot types it.
    pub fn import_picked(&mut self) {
        let Some(picker) = self.vault.import.as_mut() else {
            return;
        };
        if picker.saving {
            return;
        }
        let mut taken: Vec<String> = self.vault.entries.iter().map(|e| e.name.clone()).collect();
        let mut batch = Vec::new();
        for (item, picked) in picker.items.iter().zip(picker.picked.iter()) {
            if !picked {
                continue;
            }
            let name = vault_import::unique_name(&item.suggested_name, &taken);
            taken.push(name.clone());
            batch.push((
                name,
                VaultPut {
                    kind: VaultKind::Password,
                    username: item.username.clone(),
                    value: Zeroizing::new(String::new()),
                    domains: item.domain.iter().cloned().collect(),
                    note: None,
                    source: item.source,
                    reference: Some(item.reference.clone()),
                },
            ));
        }
        if batch.is_empty() {
            picker.error = Some("Pick at least one login to import.".into());
            return;
        }
        if taken.len() > MAX_VAULT_ENTRIES {
            picker.error = Some("The vault holds at most 64 entries. Delete some first.".into());
            return;
        }
        picker.saving = true;
        picker.error = None;
        self.queue.push_back(PanelOp::VaultImport(batch));
    }

    // ---- scheduler ----------------------------------------------------

    /// The next request to send, if any is due. Call in a loop until `None`.
    pub fn next_op(&mut self, now: Instant) -> Option<PanelOp> {
        if self.vault.want_list {
            self.vault.want_list = false;
            return Some(PanelOp::VaultList);
        }
        if let Some(front) = self.queue.front() {
            let is_input = matches!(
                front,
                PanelOp::Input(..) | PanelOp::Tab(..) | PanelOp::TabOpen(..) | PanelOp::History(..)
            );
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
        if let Some(query) = self.composio.next_load(now) {
            return Some(PanelOp::ComposioLoad(query));
        }
        if self.want_profiles && !self.profiles_busy {
            self.want_profiles = false;
            return Some(PanelOp::ProfilesList);
        }
        if !self.shot_inflight && now >= self.next_shot {
            let targets = self.shot_targets();
            if !targets.is_empty() {
                let id = targets[self.shot_cursor % targets.len()].clone();
                self.shot_cursor = self.shot_cursor.wrapping_add(1);
                self.shot_inflight = true;
                self.shot_inflight_id = Some(id.clone());
                self.next_shot =
                    now + shot_interval(targets.len(), self.page_open && self.focus.is_none());
                return Some(PanelOp::Shot(id));
            }
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
            PanelKind::Shot => {
                self.shot_inflight = false;
            }
            PanelKind::Input => {
                self.inputs_inflight = self.inputs_inflight.saturating_sub(1);
            }
            PanelKind::Takeover | PanelKind::Handback => self.control_busy = false,
            PanelKind::ProfileChange => self.profiles_busy = false,
            PanelKind::OneDriveStart | PanelKind::OneDriveDisconnect => self.onedrive.busy = false,
            PanelKind::GoogleSetup => self.connectors.google_busy = false,
            PanelKind::ComposioLoad => self.composio.loading = false,
            PanelKind::ComposioKey
            | PanelKind::ComposioKeyRemove
            | PanelKind::ComposioConnect
            | PanelKind::ComposioUnlink => self.composio.busy = false,
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
        if !info.active {
            self.dismissed = false;
            self.hidden = false;
        }
        self.info = Some(info);
    }

    /// Fold a fresh session list in: drop pictures of browsers that ended or
    /// restarted, then re-derive the dock's view.
    fn set_sessions(&mut self, sessions: Vec<BrowserSessionInfo>) {
        let previous = std::mem::take(&mut self.sessions);
        self.shots.retain(|id, _| {
            sessions.iter().any(|s| &s.id == id && s.active)
                && previous.iter().any(|s| &s.id == id && s.active)
        });
        self.sessions = sessions;
        self.sessions_loaded = true;
        if let Some(focus) = &self.focus
            && self.session(focus).is_none()
        {
            self.focus = None;
        }
        self.refresh_dock_info();
    }

    fn apply_reply(&mut self, reply: PanelReply, now: Instant) {
        match reply {
            PanelReply::Sessions(list) => {
                self.browser_error = None;
                self.idle_timeout_secs = list.idle_timeout_secs;
                self.signin_helpers = list.signin_helpers;
                self.set_sessions(list.sessions);
            }
            PanelReply::Control(session) | PanelReply::Input(session) => {
                self.browser_error = None;
                match self.sessions.iter_mut().find(|s| s.id == session.id) {
                    Some(existing) => *existing = session,
                    None => self.sessions.push(session),
                }
                self.refresh_dock_info();
            }
            PanelReply::Closed => {
                self.next_status = now;
            }
            PanelReply::Shot(id, bytes) => {
                self.shot_inflight_id = None;
                if self.session(&id).is_some_and(|s| s.active) {
                    self.shots.insert(id, jpeg_data_uri(&bytes));
                }
            }
            PanelReply::Profiles(profiles) => {
                self.profiles = profiles;
                self.profiles_loaded = true;
                self.profiles_error = None;
            }
            PanelReply::ProfileChanged(notice) => {
                self.profiles_notice = Some(notice);
                self.profiles_error = None;
                self.want_profiles = true;
                self.next_status = now;
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
            PanelReply::ComposioKeySaved => {
                self.composio.clear();
                self.composio.load("");
                self.composio.refresh_due = true;
            }
            PanelReply::ComposioKeyRemoved => {
                self.composio.clear();
                self.composio.refresh_due = true;
            }
            PanelReply::ComposioLoaded {
                query,
                apps,
                accounts,
            } => self.composio.loaded_with(query, apps, accounts),
            PanelReply::ComposioUrl { toolkit, url } => {
                self.composio.pending_url = Some(url);
                self.composio.begin_wait(&toolkit, now);
            }
            PanelReply::ComposioUnlinked => {
                let query = self.composio.query.clone();
                self.composio.load(&query);
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
            PanelReply::PasswordManagers(providers) => {
                self.vault.providers = providers;
                self.vault.providers_loaded = true;
                self.vault.provider_error = None;
            }
            PanelReply::PasswordManagerSaved(manager) => {
                self.vault.provider_busy = None;
                // A new token invalidates any previous probe result.
                self.vault.provider_tests.remove(&manager.id);
                self.vault.notice = Some(format!("{} token saved.", manager.name));
                match self.vault.providers.iter_mut().find(|p| p.id == manager.id) {
                    Some(existing) => *existing = manager,
                    None => self.vault.providers.push(manager),
                }
            }
            PanelReply::PasswordManagerCleared(id) => {
                self.vault.provider_busy = None;
                if let Some(manager) = self.vault.providers.iter_mut().find(|p| p.id == id) {
                    manager.configured = false;
                }
                self.vault.provider_tests.remove(&id);
                self.vault.notice = Some("Token removed.".into());
            }
            PanelReply::PasswordManagerTested { id, probe } => {
                self.vault.provider_testing = None;
                if let Some(manager) = self.vault.providers.iter_mut().find(|p| p.id == id) {
                    manager.configured = probe.configured;
                    manager.cli_found = probe.cli_found;
                }
                self.vault.provider_tests.insert(id, probe);
            }
            PanelReply::ImportList(items) => {
                let source = self.vault.import_loading.take().unwrap_or_default();
                let picked = vec![false; items.len()];
                self.vault.import = Some(ImportPicker {
                    source,
                    items,
                    picked,
                    saving: false,
                    error: None,
                });
            }
            PanelReply::VaultImported(count) => {
                self.vault.import = None;
                self.vault.notice = Some(format!(
                    "Imported {count} login{} as references. Add a site to any entry that has none.",
                    if count == 1 { "" } else { "s" }
                ));
                self.vault.want_list = true;
                self.vault.loading = true;
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
                let id = self.shot_inflight_id.take();
                if error == ApiError::Http(404) {
                    // The session ended between the two polls.
                    if let Some(id) = id {
                        self.shots.remove(&id);
                    }
                    self.next_status = now;
                } else if self.open() || self.focus.is_some() {
                    self.browser_error = Some(message);
                }
            }
            PanelKind::ProfilesList => {
                self.profiles_error = Some(message);
            }
            PanelKind::ProfileChange => {
                self.profiles_error = Some(profile_error_text(&error));
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
            | PanelKind::ConnectorDisconnect
            | PanelKind::ConnectorRemoveSecrets => {
                if let Some(id) = self.connectors.busy.take() {
                    self.connectors
                        .errors
                        .insert(id, connector_error_text(&error));
                }
            }
            PanelKind::ComposioKey
            | PanelKind::ComposioKeyRemove
            | PanelKind::ComposioLoad
            | PanelKind::ComposioConnect
            | PanelKind::ComposioUnlink => {
                self.composio.error = Some(composio_error_text(&error));
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
            PanelKind::PasswordManagers => {
                self.vault.provider_error = Some(message);
            }
            PanelKind::PasswordManagerSave | PanelKind::PasswordManagerClear => {
                self.vault.provider_busy = None;
                self.vault.provider_error = Some(message);
            }
            PanelKind::PasswordManagerTest => {
                self.vault.provider_testing = None;
                self.vault.provider_error = Some(message);
            }
            PanelKind::ImportList => {
                self.vault.import_loading = None;
                self.vault.import_error = Some(message);
            }
            PanelKind::VaultImport => {
                if let Some(picker) = self.vault.import.as_mut() {
                    picker.saving = false;
                    picker.error = Some(message);
                } else {
                    self.vault.error = Some(message);
                }
                self.vault.want_list = true;
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

/// The site a card shows: the host of the address, or a plain fallback.
pub fn host_label(url: Option<&str>) -> String {
    url.and_then(|raw| url::Url::parse(raw).ok())
        .and_then(|parsed| {
            parsed
                .host_str()
                .map(|h| h.trim_start_matches("www.").to_owned())
        })
        .unwrap_or_else(|| "No page open".to_owned())
}

/// "1 tab", "3 tabs": the count a grid card shows.
pub fn tab_count_label(count: usize) -> String {
    if count == 1 {
        "1 tab".to_owned()
    } else {
        format!("{count} tabs")
    }
}

/// "Idle 42s", "Idle 7m", "Idle 3h": how long since the last bot or owner
/// action. `None` when the server did not say.
pub fn idle_label(idle_secs: Option<u64>) -> Option<String> {
    let secs = idle_secs?;
    Some(match secs {
        0..=59 => "Idle under a minute".to_owned(),
        60..=3599 => format!("Idle {}m", secs / 60),
        _ => format!("Idle {}h", secs / 3600),
    })
}

/// Plain sentence for the configured session idle close time.
pub fn idle_timeout_label(secs: u64) -> String {
    match secs {
        0..=59 => format!("Sessions close after {secs}s idle"),
        60..=3599 => format!("Sessions close after {}m idle", secs / 60),
        _ => format!("Sessions close after {}h idle", secs / 3600),
    }
}

/// The bots that browse with this profile.
pub fn profile_users(profile: &BrowserProfile) -> &[String] {
    &profile.bots
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

    fn info(active: bool, controller: BrowserController) -> BrowserSessionInfo {
        session("default", active, controller)
    }

    fn session(id: &str, active: bool, controller: BrowserController) -> BrowserSessionInfo {
        BrowserSessionInfo {
            id: id.into(),
            profile_name: id.into(),
            active,
            url: active.then(|| "https://example.test/".into()),
            title: active.then(|| "Example".into()),
            controller,
            tabs: Vec::new(),
            pointer: None,
            bot: Some("main".into()),
            conversation_id: Some("dir:main".into()),
            signin_blocked: None,
            idle_secs: None,
            last_nav_host: None,
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
            source: VaultSource::Local,
            reference: None,
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
                Ok(PanelReply::Sessions(BrowserSessionsList {
                    sessions: vec![info(false, BrowserController::Bot)],
                    ..Default::default()
                })),
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
                Ok(PanelReply::Sessions(BrowserSessionsList {
                    sessions: vec![info(false, BrowserController::Bot)],
                    ..Default::default()
                })),
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
                Ok(PanelReply::Sessions(BrowserSessionsList {
                    sessions: vec![info(false, BrowserController::Bot)],
                    ..Default::default()
                })),
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
                Ok(PanelReply::Sessions(BrowserSessionsList {
                    sessions: vec![info(false, BrowserController::Bot)],
                    ..Default::default()
                })),
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
                Ok(PanelReply::Sessions(BrowserSessionsList {
                    sessions: vec![info(false, BrowserController::Bot)],
                    ..Default::default()
                })),
            ),
            t0,
        );
        // Inactive: no screenshot, ever.
        assert!(state.next_op(t0 + Duration::from_millis(600)).is_none());
        state.apply(
            done(
                &state,
                PanelKind::Status,
                Ok(PanelReply::Sessions(BrowserSessionsList {
                    sessions: vec![info(true, BrowserController::Bot)],
                    ..Default::default()
                })),
            ),
            t0,
        );
        assert!(state.open());
        let t1 = t0 + Duration::from_millis(700);
        assert!(matches!(state.next_op(t1), Some(PanelOp::Shot(_))));
        assert!(state.next_op(t1 + Duration::from_millis(100)).is_none());
        state.apply(
            done(
                &state,
                PanelKind::Shot,
                Ok(PanelReply::Shot(
                    "default".into(),
                    vec![0xFF, 0xD8, 0xFF, 0],
                )),
            ),
            t1,
        );
        assert!(state.dock_shot().is_some());
        assert!(state.next_op(t1 + Duration::from_millis(400)).is_none());
        assert!(matches!(
            state.next_op(t1 + Duration::from_millis(501)),
            Some(PanelOp::Shot(_))
        ));
        // Blurred: no screenshots.
        state.apply(
            done(
                &state,
                PanelKind::Shot,
                Ok(PanelReply::Shot(
                    "default".into(),
                    vec![0xFF, 0xD8, 0xFF, 0],
                )),
            ),
            t1,
        );
        state.set_focused(false);
        assert!(!matches!(
            state.next_op(t1 + Duration::from_secs(30)),
            Some(PanelOp::Shot(_))
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
                Ok(PanelReply::Sessions(BrowserSessionsList {
                    sessions: vec![info(true, BrowserController::Bot)],
                    ..Default::default()
                })),
            ),
            t0,
        );
        state.apply(
            done(
                &state,
                PanelKind::Shot,
                Ok(PanelReply::Shot(
                    "default".into(),
                    vec![0xFF, 0xD8, 0xFF, 0],
                )),
            ),
            t0,
        );
        assert!(state.dock_shot().is_some());
        state.close_browser();
        assert!(!state.open());
        assert!(matches!(state.next_op(t0), Some(PanelOp::Close(_))));
        // Still reported active for a moment: the dock stays shut.
        state.apply(
            done(
                &state,
                PanelKind::Status,
                Ok(PanelReply::Sessions(BrowserSessionsList {
                    sessions: vec![info(true, BrowserController::Bot)],
                    ..Default::default()
                })),
            ),
            t0,
        );
        assert!(!state.open());
        state.apply(
            done(
                &state,
                PanelKind::Status,
                Ok(PanelReply::Sessions(BrowserSessionsList {
                    sessions: vec![info(false, BrowserController::Bot)],
                    ..Default::default()
                })),
            ),
            t0,
        );
        state.apply(
            done(
                &state,
                PanelKind::Status,
                Ok(PanelReply::Sessions(BrowserSessionsList {
                    sessions: vec![info(true, BrowserController::Bot)],
                    ..Default::default()
                })),
            ),
            t0,
        );
        assert!(state.open());
        assert!(state.dock_shot().is_none());
    }

    #[test]
    fn tab_operations_need_user_control_and_a_valid_tab() {
        let mut state = PanelState::new();
        let t0 = Instant::now();
        state.apply(
            done(
                &state,
                PanelKind::Status,
                Ok(PanelReply::Sessions(BrowserSessionsList {
                    sessions: vec![info(true, BrowserController::Bot)],
                    ..Default::default()
                })),
            ),
            t0,
        );
        state.switch_tab("t2");
        assert!(state.queue.is_empty(), "bot in control: ignored");
        state.apply(
            done(
                &state,
                PanelKind::Takeover,
                Ok(PanelReply::Control(info(true, BrowserController::User))),
            ),
            t0,
        );
        state.switch_tab("../x");
        state.close_tab("t");
        assert!(state.queue.is_empty(), "invalid tab ids are never sent");
        state.switch_tab("t2");
        state.close_tab("t3");
        match state.next_op(t0) {
            Some(PanelOp::Tab(id, action)) => {
                assert_eq!(id, "default");
                assert_eq!(action, TabAction::Switch("t2".into()));
            }
            _ => panic!("expected a tab switch"),
        }
        assert!(matches!(
            state.next_op(t0),
            Some(PanelOp::Tab(_, TabAction::Close(tab))) if tab == "t3"
        ));
    }

    #[test]
    fn open_tab_and_history_need_user_control() {
        let mut state = PanelState::new();
        let t0 = Instant::now();
        state.apply(
            done(
                &state,
                PanelKind::Status,
                Ok(PanelReply::Sessions(BrowserSessionsList {
                    sessions: vec![info(true, BrowserController::Bot)],
                    ..Default::default()
                })),
            ),
            t0,
        );
        assert!(!state.open_tab("https://example.test/a"), "bot in control");
        state.history(BrowserHistory::Back);
        assert!(state.queue.is_empty(), "bot in control: ignored");
        state.apply(
            done(
                &state,
                PanelKind::Takeover,
                Ok(PanelReply::Control(info(true, BrowserController::User))),
            ),
            t0,
        );
        // A blank address is refused here; a bare host becomes https.
        assert!(!state.open_tab("   "));
        assert!(state.open_tab("example.test/a"));
        state.history(BrowserHistory::Reload);
        match state.next_op(t0) {
            Some(PanelOp::TabOpen(id, url)) => {
                assert_eq!(id, "default");
                assert_eq!(url, "https://example.test/a");
            }
            _ => panic!("expected a tab open"),
        }
        assert!(matches!(
            state.next_op(t0),
            Some(PanelOp::History(_, BrowserHistory::Reload))
        ));
    }

    #[test]
    fn labels_for_tab_count_and_idle_time() {
        assert_eq!(tab_count_label(1), "1 tab");
        assert_eq!(tab_count_label(3), "3 tabs");
        assert_eq!(idle_timeout_label(600), "Sessions close after 10m idle");
        assert_eq!(idle_label(None), None);
        assert_eq!(idle_label(Some(0)).as_deref(), Some("Idle under a minute"));
        assert_eq!(idle_label(Some(59)).as_deref(), Some("Idle under a minute"));
        assert_eq!(idle_label(Some(60)).as_deref(), Some("Idle 1m"));
        assert_eq!(idle_label(Some(3599)).as_deref(), Some("Idle 59m"));
        assert_eq!(idle_label(Some(3600)).as_deref(), Some("Idle 1h"));
    }

    #[test]
    fn input_needs_user_control_and_scroll_events_coalesce() {
        let mut state = PanelState::new();
        let t0 = Instant::now();
        state.apply(
            done(
                &state,
                PanelKind::Status,
                Ok(PanelReply::Sessions(BrowserSessionsList {
                    sessions: vec![info(true, BrowserController::Bot)],
                    ..Default::default()
                })),
            ),
            t0,
        );
        state.send_input(BrowserInput::Click { x: 1.0, y: 2.0 });
        assert!(state.queue.is_empty());
        state.take_over();
        assert!(matches!(state.next_op(t0), Some(PanelOp::Takeover(_))));
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
            Some(PanelOp::Input(_, BrowserInput::Scroll { dy })) => assert_eq!(dy, 150.0),
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
        while let Some(PanelOp::Input(..)) = state.next_op(t0) {
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
            !state.queue.iter().any(
                |op| matches!(op, PanelOp::Input(_, BrowserInput::Click { x, .. }) if *x == 1.0)
            )
        );
    }

    #[test]
    fn a_stale_epoch_is_ignored_and_401_is_reported() {
        let mut state = PanelState::new();
        let t0 = Instant::now();
        let mut old = done(
            &state,
            PanelKind::Status,
            Ok(PanelReply::Sessions(BrowserSessionsList {
                sessions: vec![info(true, BrowserController::Bot)],
                ..Default::default()
            })),
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
                Ok(PanelReply::Sessions(BrowserSessionsList {
                    sessions: vec![info(true, BrowserController::Bot)],
                    ..Default::default()
                })),
            ),
            t0,
        );
        state.apply(
            done(
                &state,
                PanelKind::Shot,
                Ok(PanelReply::Shot(
                    "default".into(),
                    vec![0xFF, 0xD8, 0xFF, 0],
                )),
            ),
            t0,
        );
        state.shot_inflight_id = Some("default".into());
        state.apply(done(&state, PanelKind::Shot, Err(ApiError::Http(404))), t0);
        assert!(state.dock_shot().is_none());
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
                Ok(PanelReply::Sessions(BrowserSessionsList {
                    sessions: vec![info(true, BrowserController::Bot)],
                    ..Default::default()
                })),
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
                Ok(PanelReply::Sessions(BrowserSessionsList {
                    sessions: vec![info(false, BrowserController::Bot)],
                    ..Default::default()
                })),
            ),
            later,
        );
        state.apply(
            done(
                &state,
                PanelKind::Status,
                Ok(PanelReply::Sessions(BrowserSessionsList {
                    sessions: vec![info(true, BrowserController::Bot)],
                    ..Default::default()
                })),
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

    // ---- 0.16: sessions, profiles, the Browser page -----------------------

    fn sessions(list: Vec<BrowserSessionInfo>) -> Result<PanelReply, ApiError> {
        Ok(PanelReply::Sessions(BrowserSessionsList {
            sessions: list,
            ..Default::default()
        }))
    }

    fn feed(
        state: &mut PanelState,
        kind: PanelKind,
        result: Result<PanelReply, ApiError>,
        now: Instant,
    ) {
        let done = done(state, kind, result);
        state.apply(done, now);
    }

    fn drain_ops(state: &mut PanelState, now: Instant) -> Vec<PanelOp> {
        let mut ops = Vec::new();
        while let Some(op) = state.next_op(now) {
            ops.push(op);
        }
        ops
    }

    #[test]
    fn the_grid_asks_for_every_open_browser_in_turn_and_never_faster_than_the_floor() {
        let mut state = PanelState::new();
        state.set_page_open(true);
        let t0 = Instant::now();
        // First pump: sessions list and profile list.
        let ops = drain_ops(&mut state, t0);
        assert!(ops.iter().any(|op| matches!(op, PanelOp::Status)));
        assert!(ops.iter().any(|op| matches!(op, PanelOp::ProfilesList)));
        feed(
            &mut state,
            PanelKind::Status,
            sessions(vec![
                session("default", true, BrowserController::Bot),
                session("work", true, BrowserController::User),
                session("idle", false, BrowserController::Bot),
            ]),
            t0,
        );
        // Two open browsers: one picture request at a time, alternating.
        let t1 = t0 + Duration::from_millis(10);
        let first = match state.next_op(t1) {
            Some(PanelOp::Shot(id)) => id,
            _ => panic!("expected a shot"),
        };
        assert!(state.next_op(t1).is_none(), "one in flight at a time");
        feed(
            &mut state,
            PanelKind::Shot,
            Ok(PanelReply::Shot(first.clone(), vec![0xFF, 0xD8, 0xFF, 0])),
            t1,
        );
        // Not before the grid interval (1 s shared by 2 cards = 500 ms).
        assert!(state.next_op(t1 + Duration::from_millis(400)).is_none());
        let second = match state.next_op(t1 + Duration::from_millis(520)) {
            Some(PanelOp::Shot(id)) => id,
            _ => panic!("expected the second card"),
        };
        assert_ne!(first, second);
        assert!(["default", "work"].contains(&second.as_str()));
        assert!(state.shots.contains_key(&first));
        // The closed browser never gets a request.
        assert!(!state.shot_targets().contains(&"idle".to_owned()));
    }

    #[test]
    fn the_grid_rate_never_passes_three_requests_a_second() {
        assert_eq!(shot_interval(1, true), GRID_SHOT_EVERY);
        assert_eq!(shot_interval(2, true), Duration::from_millis(500));
        assert_eq!(shot_interval(8, true), GRID_SHOT_FLOOR);
        assert_eq!(shot_interval(1, false), SHOT_EVERY);
        assert_eq!(shot_interval(8, false), SHOT_EVERY);
    }

    #[test]
    fn pictures_stop_when_the_window_loses_focus_and_the_dock_yields_to_the_page() {
        let mut state = PanelState::new();
        state.set_page_open(true);
        let t0 = Instant::now();
        drain_ops(&mut state, t0);
        feed(
            &mut state,
            PanelKind::Status,
            sessions(vec![session("default", true, BrowserController::Bot)]),
            t0,
        );
        assert_eq!(state.shot_targets(), vec!["default".to_owned()]);
        state.set_focused(false);
        assert!(state.shot_targets().is_empty());
        assert!(!matches!(
            state.next_op(t0 + Duration::from_secs(5)),
            Some(PanelOp::Shot(_))
        ));
        state.set_focused(true);
        // Leaving the page: the dock (open, not hidden) takes over the picture.
        state.set_page_open(false);
        assert!(state.open());
        assert_eq!(state.shot_targets(), vec!["default".to_owned()]);
        state.hide();
        assert!(
            state.shot_targets().is_empty(),
            "a hidden dock needs no picture"
        );
    }

    #[test]
    fn a_rate_limit_holds_every_background_request() {
        let mut state = PanelState::new();
        state.set_page_open(true);
        let t0 = Instant::now();
        drain_ops(&mut state, t0);
        feed(
            &mut state,
            PanelKind::Status,
            sessions(vec![session("default", true, BrowserController::Bot)]),
            t0,
        );
        let t1 = t0 + Duration::from_millis(10);
        assert!(matches!(state.next_op(t1), Some(PanelOp::Shot(_))));
        feed(&mut state, PanelKind::Shot, Err(ApiError::Http(429)), t1);
        state.ask_profiles();
        // Held: neither pictures nor the profile list nor a status poll go
        // out until the back-off is over.
        assert!(state.next_op(t1 + Duration::from_millis(100)).is_none());
        assert!(state.browser_error.is_none(), "429 backs off quietly");
        // Far enough in the future everything resumes.
        assert!(state.next_op(t1 + Duration::from_secs(600)).is_some());
    }

    #[test]
    fn the_focus_view_pictures_and_controls_target_the_focused_session() {
        let mut state = PanelState::new();
        state.set_page_open(true);
        let t0 = Instant::now();
        drain_ops(&mut state, t0);
        feed(
            &mut state,
            PanelKind::Status,
            sessions(vec![
                session("default", true, BrowserController::Bot),
                session("work", true, BrowserController::Bot),
            ]),
            t0,
        );
        state.focus_session("work");
        assert_eq!(state.shot_targets(), vec!["work".to_owned()]);
        assert_eq!(state.control_target().as_deref(), Some("work"));
        state.take_over();
        match state.next_op(t0 + Duration::from_millis(5)) {
            Some(PanelOp::Takeover(id)) => assert_eq!(id, "work"),
            _ => panic!("expected a takeover of the focused session"),
        }
        feed(
            &mut state,
            PanelKind::Takeover,
            Ok(PanelReply::Control(session(
                "work",
                true,
                BrowserController::User,
            ))),
            t0,
        );
        assert!(state.user_in_control());
        state.send_input(BrowserInput::Click { x: 3.0, y: 4.0 });
        match state.next_op(t0 + Duration::from_millis(6)) {
            Some(PanelOp::Input(id, _)) => assert_eq!(id, "work"),
            _ => panic!("expected the click to go to the focused session"),
        }
        state.hand_back();
        state.close_browser();
        let ops = drain_ops(&mut state, t0 + Duration::from_millis(7));
        assert!(
            ops.iter()
                .any(|op| matches!(op, PanelOp::Handback(id) if id == "work"))
        );
        assert!(
            ops.iter()
                .any(|op| matches!(op, PanelOp::Close(id) if id == "work"))
        );
        // The dock is not told it was dismissed by a page-side close.
        assert!(!state.dismissed);
        state.clear_focus();
        assert!(state.control_target().is_none());
    }

    #[test]
    fn the_dock_follows_the_profile_of_the_current_bot() {
        let mut state = PanelState::new();
        let t0 = Instant::now();
        drain_ops(&mut state, t0);
        feed(
            &mut state,
            PanelKind::Status,
            sessions(vec![
                session("default", true, BrowserController::Bot),
                session("work", true, BrowserController::User),
            ]),
            t0,
        );
        assert!(state.open());
        assert!(!state.user_in_control());
        state.set_dock_profile("work");
        assert!(state.user_in_control());
        assert_eq!(state.shot_targets(), vec!["work".to_owned()]);
        state.set_dock_profile("idle");
        assert!(
            !state.session_active(),
            "a profile without a browser is idle"
        );
        assert!(!state.open());
        // On the Browser page the dock steps aside.
        state.set_dock_profile("default");
        assert!(state.open());
        state.set_page_open(true);
        assert!(!state.open());
    }

    #[test]
    fn a_restarted_browser_never_shows_the_old_frame_and_a_gone_focus_returns_to_the_grid() {
        let mut state = PanelState::new();
        state.set_page_open(true);
        let t0 = Instant::now();
        drain_ops(&mut state, t0);
        feed(
            &mut state,
            PanelKind::Status,
            sessions(vec![session("default", true, BrowserController::Bot)]),
            t0,
        );
        state.focus_session("default");
        feed(
            &mut state,
            PanelKind::Shot,
            Ok(PanelReply::Shot(
                "default".into(),
                vec![0xFF, 0xD8, 0xFF, 0],
            )),
            t0,
        );
        assert!(state.shots.contains_key("default"));
        feed(
            &mut state,
            PanelKind::Status,
            sessions(vec![session("default", false, BrowserController::Bot)]),
            t0,
        );
        assert!(state.shots.is_empty());
        feed(&mut state, PanelKind::Status, sessions(vec![]), t0);
        assert!(state.focus.is_none(), "the session vanished");
    }

    #[test]
    fn profile_actions_validate_first_and_refresh_the_list_after() {
        let mut state = PanelState::new();
        let t0 = Instant::now();
        drain_ops(&mut state, t0);
        state.create_profile("   ");
        assert!(state.profiles_error.is_some());
        assert!(drain_ops(&mut state, t0).is_empty());
        state.delete_profile("default");
        assert!(state.profiles_error.as_deref().unwrap().contains("default"));
        state.create_profile(" Work ");
        assert!(matches!(
            state.next_op(t0),
            Some(PanelOp::ProfileCreate(name)) if name == "Work"
        ));
        // One change at a time.
        state.rename_profile("work", "Shop");
        assert!(state.next_op(t0).is_none());
        feed(
            &mut state,
            PanelKind::ProfileChange,
            Ok(PanelReply::ProfileChanged("Added.".into())),
            t0,
        );
        assert_eq!(state.profiles_notice.as_deref(), Some("Added."));
        assert!(matches!(state.next_op(t0), Some(PanelOp::ProfilesList)));
        // A conflict from the server is shown in words, not as a status code.
        state.delete_profile("work");
        let _ = drain_ops(&mut state, t0);
        feed(
            &mut state,
            PanelKind::ProfileChange,
            Err(ApiError::Http(409)),
            t0,
        );
        assert!(
            state
                .profiles_error
                .as_deref()
                .unwrap()
                .contains("default profile")
        );
        assert!(!state.profiles_busy);
    }

    #[test]
    fn the_profile_cap_is_enforced_before_asking_the_server() {
        let mut state = PanelState::new();
        state.profiles = (0..crate::MAX_BROWSER_PROFILES)
            .map(|n| BrowserProfile {
                id: format!("p{n}"),
                name: format!("P{n}"),
                is_default: n == 0,
                bots: vec![],
                created_at: None,
                last_used_at: None,
            })
            .collect();
        let t0 = Instant::now();
        drain_ops(&mut state, t0);
        state.create_profile("One more");
        assert!(state.profiles_error.as_deref().unwrap().contains('8'));
        assert!(drain_ops(&mut state, t0).is_empty());
    }

    #[test]
    fn card_labels_are_plain() {
        assert_eq!(
            host_label(Some("https://www.example.com/a?b=1")),
            "example.com"
        );
        assert_eq!(host_label(Some("not a url")), "No page open");
        assert_eq!(host_label(None), "No page open");
    }

    fn coded(code: &str, message: &str) -> ApiError {
        ApiError::Coded {
            status: 422,
            code: code.into(),
            message: message.into(),
        }
    }

    #[test]
    fn composio_codes_show_the_server_message_verbatim() {
        for code in COMPOSIO_CODES {
            let error = coded(code, "Composio said this, word for word.");
            assert_eq!(
                composio_error_text(&error),
                "Composio said this, word for word.",
                "{code}"
            );
        }
    }

    #[test]
    fn composio_unknown_code_falls_back_to_the_server_message() {
        let error = coded("validation", "invalid request");
        assert_eq!(composio_error_text(&error), "invalid request");
    }

    #[test]
    fn composio_not_connected_is_not_blamed_on_policy() {
        let text = composio_error_text(&ApiError::Http(403));
        assert_eq!(text, "Composio is not connected yet. Add your key first.");
        assert!(!text.contains("policy"));
        assert!(!text.contains("egress"));
    }

    #[test]
    fn composio_bare_statuses_have_everyday_text() {
        assert_eq!(
            composio_error_text(&ApiError::Http(409)),
            "Add your Composio key first."
        );
        assert_eq!(
            composio_error_text(&ApiError::Http(422)),
            "Composio did not accept that key. SeraBot takes project keys (ak_), user keys (uak_) and Connect keys (ck_). Check it and try again."
        );
        for status in [502, 503, 504] {
            assert_eq!(
                composio_error_text(&ApiError::Http(status)),
                "Composio could not be reached. Try again in a moment.",
                "{status}"
            );
        }
    }

    #[test]
    fn composio_other_errors_keep_their_own_text() {
        assert_eq!(
            composio_error_text(&ApiError::Transport),
            ApiError::Transport.to_string()
        );
        assert_eq!(
            composio_error_text(&ApiError::Rejected("name is taken".into())),
            "name is taken"
        );
    }

    #[test]
    fn coded_upstream_errors_are_not_unknown_outcomes() {
        assert!(!coded("composio_rejected", "x").unknown_outcome());
        let unavailable = ApiError::Coded {
            status: 502,
            code: "composio_unavailable".into(),
            message: "x".into(),
        };
        assert!(unavailable.unknown_outcome());
    }

    #[test]
    fn a_rate_limit_is_known_not_to_have_run() {
        let limited = ApiError::Coded {
            status: 503,
            code: "composio_rate_limited".into(),
            message: "Composio is rate limiting requests. Try again in a minute.".into(),
        };
        assert!(!limited.unknown_outcome());
        assert_eq!(
            composio_error_text(&limited),
            "Composio is rate limiting requests. Try again in a minute."
        );
    }

    fn password_manager(id: &str, configured: bool, cli_found: bool) -> PasswordManager {
        PasswordManager {
            id: id.into(),
            name: id.into(),
            configured,
            cli_found,
            enabled: true,
            install_url: "https://example.test/install".into(),
        }
    }

    #[test]
    fn vault_provider_test_stores_the_probe_and_never_keeps_a_stale_one_after_save() {
        let mut state = PanelState::new();
        let t0 = Instant::now();
        state.vault.providers = vec![password_manager("onepassword", true, false)];

        state.test_password_manager("onepassword".into());
        assert_eq!(state.vault.provider_testing.as_deref(), Some("onepassword"));
        assert!(matches!(
            state.next_op(t0),
            Some(PanelOp::PasswordManagerTest(id)) if id == "onepassword"
        ));
        // One probe at a time: a second request is ignored while one is in flight.
        // (Status/shot polls may still be due from PanelState::new — that is fine.)
        state.test_password_manager("protonpass".into());
        assert_eq!(state.vault.provider_testing.as_deref(), Some("onepassword"));
        assert!(
            state
                .queue
                .iter()
                .all(|op| !matches!(op, PanelOp::PasswordManagerTest(_))),
            "second probe must not enqueue while one is in flight"
        );

        state.apply(
            done(
                &state,
                PanelKind::PasswordManagerTest,
                Ok(PanelReply::PasswordManagerTested {
                    id: "onepassword".into(),
                    probe: PasswordManagerProbe {
                        ok: false,
                        cli_found: false,
                        configured: true,
                        message: "the 1Password CLI is not installed on the server".into(),
                    },
                }),
            ),
            t0,
        );
        assert!(state.vault.provider_testing.is_none());
        let probe = state.vault.provider_tests.get("onepassword").unwrap();
        assert!(!probe.ok && !probe.cli_found);
        assert_eq!(
            state
                .vault
                .providers
                .iter()
                .find(|p| p.id == "onepassword")
                .map(|p| p.cli_found),
            Some(false)
        );

        state.apply(
            done(
                &state,
                PanelKind::PasswordManagerSave,
                Ok(PanelReply::PasswordManagerSaved(password_manager(
                    "onepassword",
                    true,
                    true,
                ))),
            ),
            t0,
        );
        assert!(
            !state.vault.provider_tests.contains_key("onepassword"),
            "saving a new token clears the last probe"
        );
    }
}
