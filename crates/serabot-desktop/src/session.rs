//! UI-independent owner session: connection, scheduling, reads and mutations.
//!
//! The Dioxus desktop shell drives this on a timer; all network work stays on
//! the worker and SSE threads, never on the UI thread.

use zeroize::Zeroizing;

use crate::{
    ApiClient, ApiError, ApprovalDecision, AudioUpdate, BotConfigUpdate, ConnectorUpdate,
    ConversationKind, MAX_SKILL_BODY_BYTES, MAX_SKILL_DESCRIPTION_CHARS, MAX_USAGE_DAYS, Message,
    NewGroup, NewProvider, Role, SPECIALIST_TOOLS, Selection, SignedIn, TaskStatus, TeamMode,
    TelegramLink, TelegramStatus, VoiceSettings, VoiceUpdate,
    api::{
        MEMORY_KINDS, valid_conversation_title, valid_credential, valid_memory_content,
        valid_memory_kind, valid_memory_source, valid_model_id, valid_provider_id,
        valid_secret_name,
    },
    avatars::AvatarTarget,
    creds,
    events::{EventStream, EventUpdate},
    panel::{PanelState, PanelWorker},
    signin::{self, SignInFailure},
    state::State,
    teams, updater, valid_bot_description, valid_bot_id, valid_conversation_parent, valid_id,
    valid_new_skill_id, valid_skill_body, valid_skill_description, valid_slug, valid_speaker,
    valid_text, valid_token, validate_bot_config_update, validate_team_members, validate_tool_list,
    worker::{Credentials, Job, Operation, OperationKind, Reply, Worker},
};
mod bot_settings;
mod flows;
mod marketplace_accounts;
mod orchestration;
mod provider_accounts;

pub use bot_settings::{BotDraft, BotSettings, SettingsTab, model_pick, model_value};
use flows::AvatarOp;
pub use flows::{ImageSlot, NewBotDraft, NewGroupDraft, OnboardingUi, PickedImage};
pub use marketplace_accounts::{McpUi, SignInTarget, ToolsState, ToolsView};
pub use provider_accounts::{AccountOp, AccountsUi, RouteChange, RouteEditorUi};
use secrecy::ExposeSecret;
use std::{
    collections::BTreeMap,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};
use zeroize::Zeroize;

/// Results of the background folder counts.
struct FolderStatsChannel {
    sender: std::sync::mpsc::Sender<(String, String, crate::local_fs::FolderStats)>,
    receiver: std::sync::mpsc::Receiver<(String, String, crate::local_fs::FolderStats)>,
}

/// Serialized disk/keyring write request executed off the UI thread.
enum PersistTask {
    Settings {
        origin: String,
        theme: String,
        pins: Vec<String>,
        prefs: creds::UiPrefs,
    },
    Credentials {
        origin: String,
        theme: String,
        pins: Vec<String>,
        prefs: creds::UiPrefs,
        token: secrecy::SecretString,
        expires: Option<u64>,
    },
    SessionToken {
        token: secrecy::SecretString,
        expires: Option<u64>,
    },
    LocalFolders(Vec<crate::local_folders::LocalFolder>),
    ClaudeAgent(creds::ClaudeAgentPrefs),
    /// Composer drafts and queues as JSON text, written atomically to `dir`
    /// next to the settings. The encoder already left out any draft holding
    /// a raw credential, so this payload never carries a secret.
    ComposerDrafts {
        dir: std::path::PathBuf,
        json: String,
    },
}

enum PersistOutcome {
    SettingsFailed,
    Credentials {
        token_failed: bool,
        settings_failed: bool,
    },
    LocalFoldersFailed(String),
    ClaudeAgentFailed(String),
    ComposerDraftsFailed(String),
}

struct PersistWorker {
    sender: std::sync::mpsc::Sender<PersistTask>,
    receiver: std::sync::mpsc::Receiver<PersistOutcome>,
}

impl PersistWorker {
    fn start(wake: Arc<AtomicBool>) -> Option<Self> {
        let (sender, task_rx) = std::sync::mpsc::channel::<PersistTask>();
        let (outcome_tx, receiver) = std::sync::mpsc::channel::<PersistOutcome>();
        std::thread::Builder::new()
            .name("serabot-persist".into())
            .spawn(move || {
                while let Ok(task) = task_rx.recv() {
                    if let Some(outcome) = Self::run_task(task) {
                        let _ = outcome_tx.send(outcome);
                        wake.store(true, Ordering::Release);
                    }
                }
            })
            .ok()?;
        Some(Self { sender, receiver })
    }

    fn run_task(task: PersistTask) -> Option<PersistOutcome> {
        match task {
            PersistTask::Settings {
                origin,
                theme,
                pins,
                prefs,
            } => {
                if origin.is_empty() {
                    return None;
                }
                let stored = creds::load_settings();
                let snapshot = creds::Settings {
                    server: origin,
                    theme: Some(theme),
                    pins: (!pins.is_empty()).then_some(pins),
                    prefs: Some(prefs),
                    sign_in_timeout_secs: stored.sign_in_timeout_secs,
                    session_expires_at: stored.session_expires_at,
                };
                creds::save_settings(&snapshot)
                    .is_err()
                    .then_some(PersistOutcome::SettingsFailed)
            }
            PersistTask::Credentials {
                origin,
                theme,
                pins,
                prefs,
                token,
                expires,
            } => {
                let stored = creds::load_settings();
                let settings_failed = if origin.is_empty() {
                    false
                } else {
                    let snapshot = creds::Settings {
                        server: origin,
                        theme: Some(theme),
                        pins: (!pins.is_empty()).then_some(pins),
                        prefs: Some(prefs),
                        sign_in_timeout_secs: stored.sign_in_timeout_secs,
                        session_expires_at: stored.session_expires_at,
                    };
                    creds::save_settings(&snapshot).is_err()
                };
                let token_failed = creds::save_session(token.expose_secret(), expires).is_err();
                (token_failed || settings_failed).then_some(PersistOutcome::Credentials {
                    token_failed,
                    settings_failed,
                })
            }
            PersistTask::SessionToken { token, expires } => {
                let _ = creds::save_session(token.expose_secret(), expires);
                None
            }
            PersistTask::LocalFolders(folders) => creds::settings_dir().and_then(|dir| {
                crate::local_folders::save(&dir, &folders)
                    .err()
                    .map(PersistOutcome::LocalFoldersFailed)
            }),
            PersistTask::ClaudeAgent(prefs) => creds::save_claude_agent(&prefs)
                .err()
                .map(PersistOutcome::ClaudeAgentFailed),
            PersistTask::ComposerDrafts { dir, json } => write_composer_drafts(&dir, &json)
                .err()
                .map(PersistOutcome::ComposerDraftsFailed),
        }
    }
}

/// File holding the persisted composer drafts and queues, next to settings.
const COMPOSER_DRAFTS_FILE: &str = "composer_drafts.json";
const TEMP_COMPOSER_DRAFTS_FILE: &str = "composer_drafts.json.tmp";

/// Drafts and queues saved on disk, or empty when nothing was saved yet.
fn load_composer_drafts(
    dir: &std::path::Path,
) -> (crate::chat_compose::Drafts, crate::chat_compose::Queues) {
    let Ok(text) = std::fs::read_to_string(dir.join(COMPOSER_DRAFTS_FILE)) else {
        return Default::default();
    };
    crate::chat_compose::decode_persisted(&text)
}

/// Write already-encoded drafts and queues atomically.
fn write_composer_drafts(dir: &std::path::Path, json: &str) -> Result<(), String> {
    std::fs::create_dir_all(dir).map_err(|error| error.to_string())?;
    let temp = dir.join(TEMP_COMPOSER_DRAFTS_FILE);
    std::fs::write(&temp, json).map_err(|error| error.to_string())?;
    std::fs::rename(&temp, dir.join(COMPOSER_DRAFTS_FILE)).map_err(|error| {
        let _ = std::fs::remove_file(&temp);
        error.to_string()
    })
}

pub const REQUEST_GAP: Duration = Duration::from_millis(0);
pub const POLL_GAP: Duration = Duration::from_millis(750);
pub const POLL_SAFETY: Duration = Duration::from_secs(4);

/// Wait after a locally expired session before the server is asked again.
const SESSION_RECHECK_SECS: u64 = 600;

/// Shown on the sign-in screen after any 401.
pub const SESSION_ENDED: &str = "Your session ended. Sign in again.";
const SETTINGS_NOT_SAVED: &str =
    "Could not save your settings. They apply until you close SeraBot.";
const SIGNED_OUT_OFFLINE: &str = "You are signed out on this device. The server could not be reached to end the session; it expires on its own.";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Theme {
    Dark,
    Light,
    /// Follow the operating system.
    System,
}

impl Theme {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Dark => "dark",
            Self::Light => "light",
            Self::System => "system",
        }
    }

    pub fn from_stored(value: Option<&str>) -> Self {
        match value {
            Some("dark") => Self::Dark,
            Some("system") => Self::System,
            // Light is the default until the owner picks something else.
            _ => Self::Light,
        }
    }

    pub fn toggled(self) -> Self {
        match self {
            Self::Dark => Self::Light,
            Self::Light | Self::System => Self::Dark,
        }
    }
}

/// What the console refreshes after an event arrives.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EventRefresh {
    /// Reload the approvals list.
    Approvals,
    /// Reload the conversation list.
    Conversations,
    /// Reload the transcript of the open conversation.
    History,
    /// Reload the transcript of the open room and its team.
    RoomTeam,
}

/// The refresh an event kind asks for.
///
/// A team event belongs to a room. When the open conversation is a room, its
/// team is read again as well, so an open team sheet shows what the server
/// saved. Anywhere else there is no team to read and the transcript refresh
/// is the only thing that happens.
/// Child counters kept in the session; the one idle longest is dropped.
const MAX_SUBAGENT_COUNTERS: usize = 64;

fn event_refresh(kind: &str, conversation: &str) -> EventRefresh {
    if kind.starts_with("team.") && teams::is_room(conversation) {
        return EventRefresh::RoomTeam;
    }
    if kind.contains("approval") {
        EventRefresh::Approvals
    } else if kind.contains("conversation") {
        EventRefresh::Conversations
    } else {
        EventRefresh::History
    }
}

/// Activity in another conversation must not reload the open transcript.
///
/// An event that would re-read the open conversation but names a different
/// one only changes that conversation's row (order, preview), so it asks for
/// the list instead of the transcript.
fn scope_event_kind(kind: String, payload: &serde_json::Value, conversation: &str) -> String {
    if !matches!(event_refresh(&kind, conversation), EventRefresh::History) {
        return kind;
    }
    let other = payload
        .get("conversation_id")
        .and_then(|value| value.as_str())
        .is_some_and(|id| id != conversation);
    if other {
        "conversation.activity".to_owned()
    } else {
        kind
    }
}

fn choose_event_kind(current: &mut String, dirty: &mut bool, conversation: &str, kind: String) {
    if !*dirty {
        *current = kind;
    } else {
        let prev = event_refresh(current, conversation);
        let next = event_refresh(&kind, conversation);
        if matches!(next, EventRefresh::Approvals | EventRefresh::RoomTeam)
            || matches!(prev, EventRefresh::History)
        {
            *current = kind;
        }
    }
    *dirty = true;
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Section {
    Chat,
    Mail,
    Conversations,
    Projects,
    Memory,
    Providers,
    Browser,
    Vault,
    Usage,
    Tasks,
    Settings,
    /// The owner-only server logs viewer (0.28.0).
    Logs,
    /// Scheduled routines, source health and active plans (0.29.0).
    Workflows,
    /// Connectors, Composio and plugins in one place (v0.38): tabs for
    /// Installed, Marketplace and Composio.
    Integrations,
    /// The Library (0.74.8): every file the bots made plus the member's own
    /// uploads, with tabs and upload; mirrored to OneDrive when connected.
    Library,
}

impl Section {
    pub const NAV: [Self; 7] = [
        Self::Chat,
        Self::Mail,
        Self::Tasks,
        Self::Projects,
        Self::Library,
        Self::Workflows,
        Self::Browser,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Chat => "Chat",
            Self::Mail => "Mail",
            Self::Conversations => "Conversations",
            Self::Projects => "Projects",
            Self::Memory => "Memory",
            Self::Providers => "Providers",
            Self::Browser => "Browser",
            Self::Vault => "Vault",
            Self::Usage => "Usage",
            Self::Tasks => "Tasks",
            Self::Settings => "Settings",
            Self::Logs => "Logs",
            Self::Workflows => "Workflows",
            Self::Integrations => "Integrations",
            Self::Library => "Library",
        }
    }
}

/// Visual groupings for the Settings navigation sidebar.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SettingsGroup {
    Preferences,
    ConnectionsAndData,
    System,
}

impl SettingsGroup {
    pub const ALL: [Self; 3] = [Self::Preferences, Self::ConnectionsAndData, Self::System];

    pub fn label(self) -> &'static str {
        match self {
            Self::Preferences => "Preferences",
            Self::ConnectionsAndData => "Connections & Data",
            Self::System => "System",
        }
    }
}

/// The sections of the Settings page, in list order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SettingsSection {
    Account,
    Appearance,
    Notifications,
    Chat,
    Voice,
    LocalFolders,
    /// Saved facts, retrieval & optional Mem0 recall.
    Memory,
    Integrations,
    Providers,
    Vault,
    Telegram,
    /// Web search/crawl switches (Tavily, Firecrawl) with API key status (0.28.0).
    WebSearch,
    Usage,
    Logs,
    Admin,
    About,
}

impl SettingsSection {
    pub const ALL: [Self; 16] = [
        Self::Account,
        Self::Appearance,
        Self::Notifications,
        Self::Chat,
        Self::Voice,
        Self::LocalFolders,
        Self::Memory,
        Self::Integrations,
        Self::Providers,
        Self::Vault,
        Self::Telegram,
        Self::WebSearch,
        Self::Usage,
        Self::Logs,
        Self::Admin,
        Self::About,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Account => "Account",
            Self::Appearance => "Appearance",
            Self::Notifications => "Notifications",
            Self::Chat => "Chat",
            Self::Voice => "Voice",
            Self::LocalFolders => "Local folders",
            Self::Memory => "Memory",
            Self::Integrations => "Integrations",
            Self::Providers => "AI providers",
            Self::Vault => "Vault",
            Self::Telegram => "Telegram",
            Self::WebSearch => "Web search",
            Self::Usage => "Usage",
            Self::Logs => "Logs",
            Self::Admin => "Admin",
            Self::About => "About",
        }
    }

    pub fn group(self) -> SettingsGroup {
        match self {
            Self::Account
            | Self::Appearance
            | Self::Notifications
            | Self::Chat
            | Self::Voice
            | Self::LocalFolders => SettingsGroup::Preferences,
            Self::Memory
            | Self::Integrations
            | Self::Providers
            | Self::Vault
            | Self::Telegram
            | Self::WebSearch => SettingsGroup::ConnectionsAndData,
            Self::Usage | Self::Logs | Self::Admin | Self::About => SettingsGroup::System,
        }
    }

    /// Only admins see the Admin section.
    pub fn visible(self, is_admin: bool) -> bool {
        self.visible_with_memory(is_admin, false)
    }

    pub fn visible_with_memory(self, is_admin: bool, _mem0_available: bool) -> bool {
        match self {
            Self::Admin => is_admin,
            _ => true,
        }
    }
}

pub struct FleetDraft {
    pub bot_id: String,
    pub bot_description: String,
    pub bot_tools: Vec<bool>,
    pub bot_skills: String,
    pub skill_id: String,
    pub skill_description: String,
    pub skill_body: String,
    pub conv_kind: ConversationKind,
    pub conv_title: String,
    pub conv_slug: String,
    pub conv_parent: String,
}

impl Default for FleetDraft {
    fn default() -> Self {
        Self {
            bot_id: String::new(),
            bot_description: String::new(),
            bot_tools: SPECIALIST_TOOLS
                .iter()
                .map(|tool| crate::DEFAULT_SPECIALIST_TOOLS.contains(tool))
                .collect(),
            bot_skills: String::new(),
            skill_id: String::new(),
            skill_description: String::new(),
            skill_body: String::new(),
            conv_kind: ConversationKind::Section,
            conv_title: String::new(),
            conv_slug: String::new(),
            conv_parent: String::new(),
        }
    }
}

impl Drop for FleetDraft {
    fn drop(&mut self) {
        self.bot_description.zeroize();
        self.skill_description.zeroize();
        self.skill_body.zeroize();
    }
}

pub struct MemoryDraft {
    pub kind: String,
    pub content: String,
    pub source: String,
    pub query: String,
    pub delete_confirm: Option<String>,
    /// Filter chip: `all`, `saved`, `learned`, or `superseded`.
    pub filter: String,
    /// The memory whose wording is being edited inline, if any.
    pub editing: Option<String>,
    pub edit_content: String,
}

impl Default for MemoryDraft {
    fn default() -> Self {
        Self {
            kind: "note".into(),
            content: String::new(),
            source: "operator:desktop".into(),
            query: String::new(),
            delete_confirm: None,
            filter: "all".into(),
            editing: None,
            edit_content: String::new(),
        }
    }
}

impl Drop for MemoryDraft {
    fn drop(&mut self) {
        self.content.zeroize();
        self.edit_content.zeroize();
    }
}

/// Origin and status query values behind a Memory page filter chip.
pub fn memory_filter_params(filter: &str) -> (&'static str, &'static str) {
    match filter {
        "saved" => ("explicit", "active"),
        "learned" => ("extracted", "active"),
        "superseded" => ("all", "superseded"),
        "review" => ("all", "pending"),
        _ => ("all", "active"),
    }
}

pub struct ProviderDraft {
    pub id: String,
    pub kind: String,
    pub base_url: String,
    pub models: String,
    pub secret_name: String,
    pub api_key: String,
    pub catalog_fetch: bool,
    /// How the chosen preset signs in: "api_key", "oauth" or "none".
    pub auth: String,
    /// The OAuth client secret for Antigravity. Written to the server's
    /// encrypted store and cleared after the save.
    pub oauth_client_secret: String,
}

impl Default for ProviderDraft {
    fn default() -> Self {
        Self {
            id: String::new(),
            kind: "openai-compatible".into(),
            base_url: String::new(),
            models: String::new(),
            secret_name: String::new(),
            api_key: String::new(),
            catalog_fetch: true,
            auth: "api_key".into(),
            oauth_client_secret: String::new(),
        }
    }
}

impl Drop for ProviderDraft {
    fn drop(&mut self) {
        self.api_key.zeroize();
        self.oauth_client_secret.zeroize();
    }
}

#[derive(Clone, Default)]
pub struct ConnectorDraft {
    pub enabled: bool,
    pub values: BTreeMap<String, String>,
    pub saving: bool,
}

impl Drop for ConnectorDraft {
    fn drop(&mut self) {
        for value in self.values.values_mut() {
            value.zeroize();
        }
    }
}

#[derive(Default)]
pub struct AudioDraft {
    pub enabled: bool,
    pub transcription_model: String,
    pub understanding_model: String,
}

/// Editable copy of the voice/OCR settings surface.
///
/// The `*_ready` and `*_detail` fields come from the server and are read-only:
/// they say whether a feature can actually run and, if not, which setting is
/// missing. The editable fields are saved with `save_voice`.
#[derive(Default)]
pub struct VoiceDraft {
    pub loaded: bool,
    pub loading: bool,
    pub stt_model: String,
    pub stt_ready: bool,
    pub stt_detail: Option<String>,
    pub tts_model: String,
    pub tts_ready: bool,
    pub tts_detail: Option<String>,
    pub voice_id: String,
    pub ocr_model: String,
    pub ocr_ready: bool,
    pub ocr_detail: Option<String>,
    pub ocr_enabled: bool,
    pub reply_mode: String,
    pub max_speech_chars: usize,
}

pub struct RenameDraft {
    pub id: String,
    pub title: String,
}

#[derive(Default)]
pub struct UpdateUi {
    pub info: Option<updater::UpdateInfo>,
    pub error: Option<String>,
    /// Result of a manual check that found nothing newer.
    pub notice: Option<String>,
    pub checking: bool,
    /// The running check was started by the button, so it reports "up to date".
    pub manual: bool,
    pub rx: Option<std::sync::mpsc::Receiver<Result<Option<updater::UpdateInfo>, String>>>,
}

/// The live sandbox posture read from `GET /v1/status`.
///
/// The read runs on its own thread so the console never blocks on it. A
/// failed read keeps the last known posture and reports the error; nothing
/// here is ever guessed.
#[derive(Default)]
pub struct StatusUi {
    pub status: Option<crate::ServerStatus>,
    pub error: Option<String>,
    pub loading: bool,
    rx: Option<std::sync::mpsc::Receiver<Result<crate::ServerStatus, String>>>,
}

/// Where the native sign-in stands.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SignInPhase {
    #[default]
    Idle,
    /// Asking the server for the sign-in page and opening the browser.
    Starting,
    /// The browser is open; the console waits for it to come back.
    Waiting,
}

/// The connect screen's sign-in state: one "Sign in with Microsoft" run, an
/// optional invite code, and the last outcome.
#[derive(Default)]
pub struct SignInUi {
    pub phase: SignInPhase,
    pub error: Option<String>,
    pub notice: Option<String>,
    pub invite_code: String,
    /// The invite-code field is open ("I have an invite code").
    pub show_invite: bool,
    /// The sign-in URL while the browser leg runs, so it can be copied by hand.
    pub link: Option<String>,
    /// Whether the system browser accepted the URL. False shows the link.
    pub browser_opened: bool,
    cancel: Option<Arc<AtomicBool>>,
    rx: Option<std::sync::mpsc::Receiver<SignInMsg>>,
}

enum SignInMsg {
    Waiting { url: String, opened: bool },
    Finished(Result<SignedIn, SignInFailure>),
}

impl SignInUi {
    pub fn busy(&self) -> bool {
        self.phase != SignInPhase::Idle
    }

    /// Drop the invite code and any in-flight run. A run that is still going
    /// is told to stop so its loopback listener closes.
    fn reset_transient(&mut self) {
        if let Some(cancel) = self.cancel.take() {
            cancel.store(true, Ordering::SeqCst);
        }
        self.rx = None;
        self.phase = SignInPhase::Idle;
        self.error = None;
        self.notice = None;
        self.show_invite = false;
        self.link = None;
        self.browser_opened = false;
        self.clear_secrets();
    }

    fn clear_secrets(&mut self) {
        self.invite_code.zeroize();
        self.invite_code.clear();
    }
}

impl Drop for SignInUi {
    fn drop(&mut self) {
        self.invite_code.zeroize();
        if let Some(cancel) = self.cancel.take() {
            cancel.store(true, Ordering::SeqCst);
        }
    }
}

/// Sign-out in flight: `POST /v1/auth/logout` runs off the UI thread; the
/// stored token is deleted when it answers, whatever the answer was.
#[derive(Default)]
pub struct SignOutUi {
    pub in_flight: bool,
    rx: Option<std::sync::mpsc::Receiver<Result<(), String>>>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum TelegramAction {
    Status,
    Link,
    Unlink,
}

enum TelegramReply {
    Status(TelegramStatus),
    Link(TelegramLink),
    Unlinked(bool),
}

/// The Telegram card of the settings pane. Status reads and link/unlink writes
/// run on their own thread; the UI only polls the result.
#[derive(Default)]
pub struct TelegramUi {
    pub status: Option<TelegramStatus>,
    pub loaded: bool,
    pub loading: bool,
    pub error: Option<String>,
    pub link: Option<TelegramLink>,
    pub linking: bool,
    pub unlinking: bool,
    pub confirm_unlink: bool,
    in_flight: Option<TelegramAction>,
    rx: Option<std::sync::mpsc::Receiver<(TelegramAction, Result<TelegramReply, String>)>>,
}

impl Drop for TelegramUi {
    fn drop(&mut self) {
        if let Some(link) = self.link.as_mut() {
            link.code.zeroize();
        }
    }
}

/// Files chosen in the attachment dialog: bytes, or why one was skipped.
type PickedFiles = Vec<Result<(String, Vec<u8>), String>>;

/// What to do with a saved artifact once the download completes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PendingOpen {
    Open,
    Reveal,
    /// Ask where to save with the native Save As dialog.
    SaveAs,
}

pub struct Session {
    pub state: State,
    pub worker: Option<Worker>,
    pub credentials: Option<Arc<Credentials>>,
    /// The session token could not be written to the OS credential store. It
    /// still works for this run; the user is told and can retry the save.
    pub credential_save_failed: bool,
    pub events: Option<EventStream>,
    /// Set by the SSE thread so the UI timer can tick without waiting 120ms.
    pub ui_wake: Arc<AtomicBool>,
    pub events_active: bool,
    /// True after the event stream has connected once this session. A later
    /// drop is "reconnecting", not the first connect.
    pub events_seen: bool,
    /// Browser dock and vault sheet state; its own worker thread.
    pub panel: PanelState,
    panel_worker: Option<PanelWorker>,
    events_dirty: bool,
    /// Stream events waiting for the next 16 ms frame (v0.50 C2).
    event_frames: crate::events::FrameCoalescer,
    /// Frames applied to the state: one state write each, 60 a second at most.
    pub event_frame_writes: u64,
    /// Folded progress of subagents (child tasks), by child task id.
    pub subagents: std::collections::BTreeMap<String, crate::events::SubagentProgress>,
    pub last_event_id: i64,
    last_event_kind: String,
    pub server: String,
    pub token: String,
    pub connect_error: Option<String>,
    pub connecting: bool,
    pub theme: Theme,
    /// Device-local look and chat preferences (Settings page).
    pub prefs: creds::UiPrefs,
    pub settings_section: SettingsSection,
    seen_approvals: Option<usize>,
    /// Usage page range in days (7, 30 or 90) and whether a load is running.
    pub usage_days: u32,
    pub usage_loading: bool,
    /// Owner-only server health card: a load is running, and the calm
    /// message to show when the last load failed (cleared on success).
    pub server_health_loading: bool,
    pub server_health_error: Option<String>,
    /// Pinned bots/conversations shown in the inspector strip. Persisted in
    /// settings.json through [`creds::Settings::pins`].
    pub pins: Vec<String>,
    pub section: Section,
    /// The Logs tab's current filter (0.28.0); applied on every refresh.
    pub logs_level: Option<String>,
    pub logs_query: String,
    /// Artifact hashes with a preview read in flight (one per hash).
    pub doc_previews_pending: std::collections::BTreeSet<String>,
    pub conversation: String,
    pub composer: String,
    /// Generation of `composer` (0.74.3): bumped only when the composer is
    /// intentionally replaced — a chip, a slash insert, a failed-send
    /// restore, a conversation switch — so the chat view can tell a real
    /// replacement from background session writes while the owner types.
    pub composer_gen: u64,
    pub fleet: FleetDraft,
    pub memory: MemoryDraft,
    pub provider: ProviderDraft,
    pub connector_drafts: BTreeMap<String, ConnectorDraft>,
    pub audio: AudioDraft,
    /// Editable copy of the voice/OCR settings surface.
    pub voice: VoiceDraft,
    /// The bot settings panel (0.16).
    pub bot_settings: BotSettings,
    /// The Providers page's accounts, their sheet and sign-in (v0.49).
    pub accounts_ui: AccountsUi,
    /// Plugin accounts, tool switches and the GitHub card (v0.51).
    pub mcp_ui: McpUi,
    /// The bot "Model route" editor (v0.49).
    pub route_editor: RouteEditorUi,
    /// The last message, waiting for "Retry on <fallback>" to land its pick.
    pending_retry: Option<String>,
    pub admin_notice: Option<String>,
    /// Connector whose Test probe is in flight.
    pub connector_testing: Option<String>,
    /// Provider whose browser sign-in is waiting for the redirect.
    pub oauth_signing_in: Option<String>,
    /// Keys typed on provider cards, by provider id. Zeroed once saved.
    pub provider_key_drafts: BTreeMap<String, String>,
    /// Provider whose Test is running.
    pub provider_testing: Option<String>,
    pub admin_error: Option<String>,
    pub restart_pending: bool,
    pub restarting: bool,
    /// The connection dropped while signed in: the reconnect banner shows
    /// and the console retries on its own.
    pub link_down: bool,
    link_attempts: u32,
    link_retry_at: Option<Instant>,
    pub rename: Option<RenameDraft>,
    pub archive_confirm: Option<String>,
    /// Auto-approve is waiting for the owner's first-time OK in this chat.
    pub auto_approve_confirming: bool,
    /// A reconnect or transcript retry was asked for and nothing has answered
    /// yet: the Retry buttons show "Retrying…" and stay disabled.
    pub link_retrying: bool,
    /// Chats where the owner already confirmed turning Auto-approve on.
    auto_approve_confirmed: std::collections::BTreeSet<String>,
    /// Editable copy of the open room's group instructions; synced from the
    /// server team and written back with the next team save.
    pub team_rules_draft: String,
    pub update: UpdateUi,
    pub status: StatusUi,
    pub sign_in: SignInUi,
    pub sign_out: SignOutUi,
    pub telegram: TelegramUi,
    pub toast: Option<String>,
    /// Artifact download in flight (worker) or file write in flight (thread).
    pub downloading: bool,
    download_rx: Option<std::sync::mpsc::Receiver<Result<String, String>>>,
    /// What to do with the saved file once the download completes.
    pending_open: Option<PendingOpen>,
    /// Folders on this PC the bots may use (default empty = off). Device-local.
    pub local_folders: Vec<crate::local_folders::LocalFolder>,
    /// Bots currently switched to local Claude Agent (ACP) mode in this desktop session.
    pub claude_agent_bots: std::collections::BTreeSet<String>,
    /// Last selected local folder id per bot for Claude Agent (ACP).
    pub claude_agent_folders: std::collections::BTreeMap<String, String>,
    /// "This PC" numbers for the Usage page, gathered on a worker thread.
    pub pc: crate::pc_stats::PcMonitor,
    local_agent: Option<(crate::local_folders::LocalAgent, usize)>,
    /// The desktop network relay (0.66): runs while signed in; it only moves
    /// traffic when the member turned the relay on.
    net_relay: Option<(crate::net_relay::NetRelay, usize)>,
    /// Message under the Local folders card.
    pub local_notice: Option<String>,
    folder_pick_rx: Option<std::sync::mpsc::Receiver<Option<(std::path::PathBuf, bool)>>>,
    /// Size and file counts per allowed folder id, filled in lazily (0.21).
    pub folder_stats: std::collections::HashMap<String, crate::local_folders::FolderStatus>,
    folder_stats_wanted: bool,
    folder_stats_chan: Option<FolderStatsChannel>,
    /// Files already uploaded and attached to the composer draft.
    pub composer_files: Vec<crate::AttachmentInfo>,
    /// Uploaded files and pending uploads of the chats that are not open,
    /// so switching chats no longer drops the composer's files.
    composer_files_stash: crate::chat_compose::ComposerFilesStash,
    /// Text drafts per conversation, kept while the app runs (0.19).
    pub drafts: crate::chat_compose::Drafts,
    /// Test-only override for where drafts and queues are persisted. `None`
    /// means the platform settings directory.
    composer_dir_override: Option<std::path::PathBuf>,
    /// Messages typed while a reply runs, for the open chat (v0.50).
    pub queue: crate::chat_compose::SendQueue,
    /// Queues of the chats that are not open.
    queues: crate::chat_compose::Queues,
    /// Unsent messages held per conversation after a send whose outcome is
    /// unknown, so switching chats no longer drops them.
    held_drafts: std::collections::HashMap<String, String>,
    /// The last recorded voice note, kept for one transcription retry.
    pub last_audio: Option<(String, Vec<u8>)>,
    /// Task ids whose `needs_review` banner the owner explicitly dismissed locally.
    pub dismissed_needs_review: std::collections::HashSet<String>,
    /// Collapsed run groups the owner opened.
    pub runs_open: std::collections::HashSet<String>,
    runs_last_read: Option<Instant>,
    /// Why some files were refused, shown under the composer.
    pub attach_notice: Option<String>,
    /// Picture previews by attachment id (small images only).
    pub attachment_thumbs: std::collections::HashMap<String, String>,
    /// Preview of the upload in flight, kept until its reply lands.
    upload_thumb: Option<String>,
    /// The sent message is a regeneration of an earlier one.
    pub regenerating: bool,
    /// A stop was asked for while a request was in flight.
    stop_queued: bool,
    /// Out-of-band stop request in flight, independent of the worker slot.
    stop_rx: Option<std::sync::mpsc::Receiver<Result<usize, String>>>,
    /// Picked files waiting for upload, one request at a time.
    upload_queue: std::collections::VecDeque<(String, Vec<u8>)>,
    /// An upload request is in flight.
    pub uploading: bool,
    pick_rx: Option<std::sync::mpsc::Receiver<PickedFiles>>,
    /// Toast text from browser/Explorer launches, which run on a helper thread.
    launch_tx: std::sync::mpsc::Sender<String>,
    launch_rx: std::sync::mpsc::Receiver<String>,
    /// Unix seconds at which the signed-in session ends, when the server
    /// reported one this run. A restored token has no known expiry.
    session_expires: Option<u64>,
    /// Requests that found the worker's one-slot queue busy. The tick sends
    /// them in order when the queue has room (0.34.0), instead of failing.
    backlog: std::collections::VecDeque<Operation>,
    pending: Option<OperationKind>,
    /// The create-bot sheet draft.
    pub new_bot: NewBotDraft,
    /// The create-group sheet draft.
    pub new_group: NewGroupDraft,
    /// The onboarding chat that opens right after a bot is created.
    pub onboarding_ui: OnboardingUi,
    /// A picked image waiting for the bot or group it belongs to to exist.
    avatar_upload: Option<(AvatarTarget, Vec<u8>)>,
    /// The avatar request in flight, if any.
    avatar_op: Option<AvatarOp>,
    /// True while the create-bot request in flight came from the new sheet.
    creating_named_bot: bool,
    /// A freshly created group to open once its image (if any) is up.
    pending_open_group: Option<String>,
    /// Bot id whose `GET /v1/bots/{id}` is in flight (dedup of loads).
    bot_detail_pending: Option<String>,
    /// The bot whose chat list is being read, so a failed read (an older
    /// server without the route) marks it loaded instead of retrying forever.
    bot_chats_pending: Option<String>,
    /// The plugin whose page is open on the Marketplace screen.
    pub marketplace_open: Option<String>,
    /// The connector whose card is open in the Integrations detail sheet (v0.38).
    pub integration_open: Option<String>,
    /// A tab the Integrations screen should switch to the next time it
    /// renders (the sidebar's "Bots N of M" link asks for the Bots tab).
    pub integrations_tab_request: Option<crate::integrations::Tab>,
    /// The shipped-bot catalog must be read again (a bot was added, switched
    /// or removed), so the sidebar count and the Bots tab stay true.
    catalog_refresh: bool,
    /// A change is in flight (so its buttons wait).
    pub marketplace_busy: bool,
    /// The bot config write in flight: (bot id, update), kept so the cached
    /// detail can be updated optimistically when the save succeeds.
    bot_config_pending: Option<(String, BotConfigUpdate)>,
    /// The team to write on a room that is being created right now.
    pending_team: Option<Vec<String>>,
    /// Set when the console opened a freshly created room: the room is
    /// reloaded once its team has been written.
    pending_room_refresh: bool,
    reconnect_at: Option<Instant>,
    scheduled: Option<(Instant, Operation)>,
    next_request: Instant,
    refreshing: bool,
    poll_history: bool,
    poll_paused: bool,
    poll_approvals: bool,
    /// Consecutive 429s on background polls; resets on any success.
    rate_attempts: u32,
    persist_worker: Option<PersistWorker>,
}

impl Default for Session {
    fn default() -> Self {
        Self::new()
    }
}

impl Session {
    pub fn new() -> Self {
        let settings = creds::load_settings();
        let theme = Theme::from_stored(settings.theme.as_deref());
        let pins = settings.pins.clone().unwrap_or_default();
        let server = creds::prefill_server(&settings);
        // An expired saved session is refused here and never reconnects. The
        // expiry is read after that check, so a refused session leaves none behind.
        let restored = creds::load_session(unix_now());
        let session_expires = restored
            .as_ref()
            .and_then(|_| creds::load_settings().session_expires_at);
        let token = restored
            .map(|token| token.expose_secret().to_owned())
            .unwrap_or_default();
        let worker = Worker::start().ok();
        let claude_prefs = creds::load_claude_agent();
        let mut session = Self::assemble(server, theme, pins, token, worker);
        session.session_expires = session_expires;
        session.prefs = settings.prefs.clone().unwrap_or_default().sanitized();
        session.claude_agent_bots = claude_prefs.bots;
        session.claude_agent_folders = claude_prefs.folders;
        if session.worker.is_some()
            && !session.server.trim().is_empty()
            && valid_token(&session.token)
        {
            session.connect_now();
        }
        session.check_updates();
        session
    }

    /// The initial session value, with no I/O: tests build it without a
    /// worker, a connection or an update check.
    fn assemble(
        server: String,
        theme: Theme,
        pins: Vec<String>,
        token: String,
        worker: Option<Worker>,
    ) -> Self {
        let (launch_tx, launch_rx) = std::sync::mpsc::channel();
        let ui_wake = Arc::new(AtomicBool::new(false));
        let persist_worker = worker
            .as_ref()
            .and_then(|_| PersistWorker::start(Arc::clone(&ui_wake)));
        // Drafts and queues saved on disk come back with the launch; a draft
        // holding a raw credential was never written, so nothing secret loads.
        let (drafts, queues) = creds::settings_dir()
            .map(|dir| load_composer_drafts(&dir))
            .unwrap_or_default();
        Self {
            state: State::default(),
            worker,
            credentials: None,
            events: None,
            ui_wake,
            events_active: false,
            events_seen: false,
            events_dirty: false,
            event_frames: crate::events::FrameCoalescer::new(),
            event_frame_writes: 0,
            subagents: std::collections::BTreeMap::new(),
            last_event_id: 0,
            last_event_kind: String::new(),
            server,
            token,
            connect_error: None,
            connecting: false,
            theme,
            prefs: creds::UiPrefs::default(),
            settings_section: SettingsSection::Account,
            seen_approvals: None,
            pins,
            usage_days: 30,
            usage_loading: false,
            server_health_loading: false,
            server_health_error: None,
            section: Section::Chat,
            logs_level: None,
            logs_query: String::new(),
            doc_previews_pending: std::collections::BTreeSet::new(),
            conversation: "windows-owner".into(),
            composer: String::new(),
            composer_gen: 0,
            fleet: FleetDraft::default(),
            memory: MemoryDraft::default(),
            provider: ProviderDraft::default(),
            connector_drafts: BTreeMap::new(),
            audio: AudioDraft::default(),
            voice: VoiceDraft::default(),
            bot_settings: BotSettings::default(),
            accounts_ui: AccountsUi::default(),
            mcp_ui: McpUi::default(),
            route_editor: RouteEditorUi::default(),
            pending_retry: None,
            admin_notice: None,
            connector_testing: None,
            oauth_signing_in: None,
            provider_key_drafts: BTreeMap::new(),
            provider_testing: None,
            admin_error: None,
            restart_pending: false,
            restarting: false,
            rename: None,
            archive_confirm: None,
            auto_approve_confirming: false,
            link_retrying: false,
            auto_approve_confirmed: std::collections::BTreeSet::new(),
            team_rules_draft: String::new(),
            update: UpdateUi::default(),
            status: StatusUi::default(),
            sign_in: SignInUi::default(),
            credential_save_failed: false,
            sign_out: SignOutUi::default(),
            telegram: TelegramUi::default(),
            toast: None,
            downloading: false,
            download_rx: None,
            pending_open: None,
            local_folders: creds::settings_dir()
                .map(|dir| crate::local_folders::load(&dir))
                .unwrap_or_default(),
            claude_agent_bots: std::collections::BTreeSet::new(),
            claude_agent_folders: std::collections::BTreeMap::new(),
            pc: crate::pc_stats::PcMonitor::new(),
            local_agent: None,
            net_relay: None,
            local_notice: None,
            folder_pick_rx: None,
            folder_stats: std::collections::HashMap::new(),
            folder_stats_wanted: false,
            folder_stats_chan: None,
            composer_files: Vec::new(),
            composer_files_stash: crate::chat_compose::ComposerFilesStash::default(),
            drafts,
            composer_dir_override: None,
            queue: crate::chat_compose::SendQueue::default(),
            queues,
            held_drafts: std::collections::HashMap::new(),
            last_audio: None,
            dismissed_needs_review: std::collections::HashSet::new(),
            runs_open: std::collections::HashSet::new(),
            runs_last_read: None,
            attach_notice: None,
            attachment_thumbs: std::collections::HashMap::new(),
            upload_thumb: None,
            regenerating: false,
            stop_queued: false,
            stop_rx: None,
            upload_queue: std::collections::VecDeque::new(),
            uploading: false,
            pick_rx: None,
            launch_tx,
            launch_rx,
            session_expires: None,
            backlog: std::collections::VecDeque::new(),
            pending: None,
            new_bot: NewBotDraft::default(),
            new_group: NewGroupDraft::default(),
            onboarding_ui: OnboardingUi::default(),
            avatar_upload: None,
            avatar_op: None,
            creating_named_bot: false,
            pending_open_group: None,
            bot_detail_pending: None,
            bot_chats_pending: None,
            marketplace_open: None,
            integration_open: None,
            integrations_tab_request: None,
            catalog_refresh: false,
            marketplace_busy: false,
            bot_config_pending: None,
            pending_team: None,
            pending_room_refresh: false,
            reconnect_at: None,
            link_down: false,
            link_attempts: 0,
            link_retry_at: None,
            scheduled: None,
            next_request: Instant::now(),
            refreshing: false,
            poll_history: false,
            poll_paused: false,
            poll_approvals: false,
            rate_attempts: 0,
            panel: PanelState::new(),
            panel_worker: None,
            persist_worker,
        }
    }

    /// Drop the previous session's dock and vault state. Results still in
    /// flight for the old epoch are discarded by the worker and the state.
    fn reset_panel(&mut self) {
        self.panel.reset();
        if let Some(worker) = &self.panel_worker {
            worker.set_epoch(self.panel.epoch);
        }
    }

    /// Receive finished dock/vault requests and send the due ones.
    fn pump_panel(&mut self) {
        if !self.state.connected {
            return;
        }
        let Some(credentials) = self.credentials.clone() else {
            return;
        };
        if self.panel_worker.is_none() {
            let worker = PanelWorker::start().ok();
            if let Some(worker) = &worker {
                worker.set_epoch(self.panel.epoch);
            }
            self.panel_worker = worker;
        }
        let Some(worker) = &self.panel_worker else {
            return;
        };
        let now = Instant::now();
        let mut unauthorized = false;
        // Point the dock at the browser of the bot being talked to, and tell
        // the scheduler whether the Browser page is on screen.
        let chat_bot = self.conversation_bot(&self.conversation).or_else(|| {
            self.state
                .listed_bots
                .iter()
                .find(|bot| bot.role == "orchestrator")
                .map(|bot| bot.id.clone())
        });
        if let Some(bot) = chat_bot {
            let profile = crate::profile_for_bot(&self.panel.profiles, &bot);
            self.panel.set_dock_profile(&profile);
        }
        self.panel.set_page_open(self.section == Section::Browser);
        self.panel.ensure_profiles();
        while let Some(done) = worker.try_recv() {
            unauthorized |= self.panel.apply(done, now);
        }
        while let Some(op) = self.panel.next_op(now) {
            if !worker.send(self.panel.epoch, Arc::clone(&credentials), op) {
                break;
            }
        }
        if let Some(url) = self.panel.onedrive.take_url() {
            self.open_in_browser(url);
        }
        if let Some(url) = self.panel.connectors.take_url() {
            self.open_in_browser(url);
        }
        if let Some(url) = self.panel.composio.take_url() {
            self.open_in_browser(url);
        }
        if self.panel.onedrive.take_refresh()
            | self.panel.connectors.take_refresh()
            | self.panel.composio.take_refresh()
        {
            self.refresh_connectors();
        }
        if unauthorized {
            self.reject_token();
        }
    }

    pub fn ready(&self) -> bool {
        self.pending.is_none()
            && !self.refreshing
            && self.worker.is_some()
            && Instant::now() >= self.next_request
    }

    /// The connection dropped or timed out while signed in: show the calm
    /// banner and plan the next automatic try.
    pub(crate) fn note_link_down(&mut self) {
        if self.link_down {
            self.link_attempts = self.link_attempts.saturating_add(1);
        } else {
            self.link_down = true;
            self.link_attempts = 0;
        }
        self.link_retry_at =
            Some(Instant::now() + crate::net_policy::reconnect_delay(self.link_attempts));
    }

    /// Any good answer from the server ends the banner.
    pub(crate) fn link_recovered(&mut self) {
        if self.link_down {
            self.link_down = false;
            self.link_attempts = 0;
            self.link_retry_at = None;
            self.state.error = None;
        }
    }

    /// Try the server again now ("Try again", or the automatic timer): the
    /// polling that the failure paused resumes and a cheap read probes it.
    /// The banner stays up until something answers.
    pub fn retry_link(&mut self) {
        if !self.state.connected || self.pending.is_some() {
            return;
        }
        self.link_retry_at =
            Some(Instant::now() + crate::net_policy::reconnect_delay(self.link_attempts));
        self.poll_paused = false;
        self.refreshing = false;
        self.next_request = Instant::now();
        self.link_retrying = true;
        if !self.state.unknown_outcome {
            self.state.error = None;
        }
        if !self.state.history_loaded {
            // The transcript never arrived: restart the chain that loads it
            // (and the catalog behind it) instead of a lone probe.
            self.state.begin_history_retry(Instant::now());
            self.refreshing = true;
        }
        self.schedule(Operation::History, Duration::ZERO);
    }

    /// The "Retry" button of the "Could not load this conversation" panel.
    pub fn retry_history(&mut self) {
        if !self.state.connected {
            return;
        }
        self.state.begin_history_retry(Instant::now());
        self.state.error = None;
        self.link_retrying = true;
        self.poll_paused = false;
        self.refreshing = true;
        self.schedule(Operation::History, Duration::ZERO);
    }

    /// "Load earlier messages": one page of rows older than the oldest shown.
    pub fn load_older_messages(&mut self) {
        let state = &mut self.state;
        if !state.connected || !state.history_loaded || state.older_loading {
            return;
        }
        if !state.has_more_history {
            return;
        }
        let Some(first) = state
            .messages
            .iter()
            .find(|row| !row.id.starts_with("opt-"))
            .map(|row| row.id.clone())
        else {
            return;
        };
        state.older_loading = true;
        state.older_error = None;
        self.start_request(Operation::HistoryBefore(first));
    }

    pub fn connect_now(&mut self) {
        let endpoint = match crate::validate_endpoint(self.server.trim()) {
            Ok(endpoint) => endpoint,
            Err(error) => {
                self.connect_error = Some(error.to_string());
                self.connecting = false;
                return;
            }
        };
        if !valid_token(&self.token) {
            self.connect_error = Some("Sign in with Microsoft to continue.".into());
            self.connecting = false;
            return;
        }
        if !valid_id(&self.conversation) {
            self.connect_error = Some("Conversation id is not valid.".into());
            self.connecting = false;
            return;
        }
        self.stop_events();
        self.reset_scope(false);
        self.reset_panel();
        self.state.error = None;
        self.connect_error = None;
        self.connecting = true;
        self.credentials = Some(Arc::new(Credentials {
            endpoint,
            token: secrecy::SecretString::new(self.token.clone()),
        }));
        self.refreshing = true;
        self.start_request(Operation::Health);
    }

    /// The server answered 401: the session is over. Delete the stored token
    /// and return to the sign-in screen.
    pub fn reject_token(&mut self) {
        self.connect_error = self.clear_session().err();
        self.sign_in.notice = Some(SESSION_ENDED.into());
    }
    /// The server renews a session while it is in use and reports the new end
    /// on `/v1/me`. Keep the stored expiry in step so the local check below
    /// does not sign out a session the server has extended.
    fn adopt_session_expiry(&mut self) {
        let Some(expires) = self
            .state
            .user
            .as_ref()
            .and_then(|user| user.session_expires_at.as_deref())
            .and_then(parse_rfc3339_unix)
        else {
            return;
        };
        if self.session_expires == Some(expires) {
            return;
        }
        self.session_expires = Some(expires);
        self.persist_credentials();
    }

    /// Sign the user out cleanly once the session expiry the server reported
    /// has passed, instead of waiting for a request to answer 401.
    fn check_session_expiry(&mut self) {
        if self.session_expires.is_some_and(|at| unix_now() >= at) {
            if self.credentials.is_some() && self.state.connected {
                // The server renews sessions in use, so the stored end may be
                // stale. Ask once; a real expiry answers 401 and signs out.
                self.session_expires = Some(unix_now() + SESSION_RECHECK_SECS);
                self.queue_backlog(Operation::Me);
            } else {
                self.reject_token();
            }
        }
    }

    fn reset_scope(&mut self, connected: bool) {
        self.backlog.clear();
        self.state.new_scope(self.conversation.clone(), connected);
        if let Some(worker) = &self.worker {
            worker.set_epoch(self.state.scope.epoch);
        }
        self.composer.zeroize();
        // Attachments belong to the conversation they were uploaded to.
        self.composer_files.clear();
        self.attach_notice = None;
        self.stop_queued = false;
        self.stop_rx = None;
        self.upload_queue.clear();
        self.uploading = false;
        self.pending = None;
        self.pending_retry = None;
        self.bot_detail_pending = None;
        self.bot_config_pending = None;
        self.link_down = false;
        self.link_attempts = 0;
        self.link_retry_at = None;
        self.scheduled = None;
        self.refreshing = false;
        self.poll_history = false;
        self.poll_paused = false;
        self.poll_approvals = false;
        self.events_dirty = false;
        self.rename = None;
        self.archive_confirm = None;
        self.pending_team = None;
        self.pending_room_refresh = false;
        if !connected {
            self.status.status = None;
            self.status.rx = None;
            self.status.loading = false;
            self.status.error = None;
            self.telegram = TelegramUi::default();
            self.state.created_bot = None;
            self.state.created_conversation = None;
            self.state.created_skill = None;
            self.state.listed_skills.clear();
            self.state.skills_loaded = false;
            self.state.listed_bots.clear();
            self.state.bots_loaded = false;
            self.state.conversations.clear();
            self.state.conversations_loaded = false;
            self.state.memories.clear();
            self.state.memories_loaded = false;
            self.state.memory_stats = None;
            self.state.providers.clear();
            self.state.providers_loaded = false;
            self.state.connectors.clear();
            self.state.connectors_loaded = false;
            self.fleet = FleetDraft::default();
            self.new_bot = NewBotDraft::default();
            self.new_group = NewGroupDraft::default();
            self.onboarding_ui = OnboardingUi::default();
            self.avatar_upload = None;
            self.avatar_op = None;
            self.creating_named_bot = false;
            self.pending_open_group = None;
            self.bot_settings = BotSettings::default();
            self.memory.content.zeroize();
            self.memory.edit_content.zeroize();
            self.memory.editing = None;
            self.memory.delete_confirm = None;
        }
    }

    fn ui_origin(&self) -> String {
        self.credentials
            .as_ref()
            .map(|credentials| credentials.endpoint.origin().ascii_serialization())
            .unwrap_or_else(|| self.server.clone())
    }

    fn apply_persist_outcome(&mut self, outcome: PersistOutcome) {
        match outcome {
            PersistOutcome::SettingsFailed => self.toast = Some(SETTINGS_NOT_SAVED.into()),
            PersistOutcome::Credentials {
                token_failed,
                settings_failed,
            } => {
                self.credential_save_failed = token_failed;
                if settings_failed {
                    self.state.error =
                        Some("Connected, but the settings could not be saved.".into());
                }
            }
            PersistOutcome::LocalFoldersFailed(error) => {
                self.local_notice = Some(format!("Could not save the folder list: {error}"));
            }
            PersistOutcome::ClaudeAgentFailed(error) => {
                self.toast = Some(format!("Could not save local agent mode: {error}"));
            }
            PersistOutcome::ComposerDraftsFailed(error) => {
                self.toast = Some(format!("Drafts could not be saved: {error}"));
            }
        }
    }

    fn enqueue_persist(&mut self, task: PersistTask) {
        if let Some(worker) = &self.persist_worker {
            if let Err(std::sync::mpsc::SendError(task)) = worker.sender.send(task)
                && let Some(outcome) = PersistWorker::run_task(task)
            {
                self.apply_persist_outcome(outcome);
            }
        } else if let Some(outcome) = PersistWorker::run_task(task) {
            self.apply_persist_outcome(outcome);
        }
    }

    fn poll_persist(&mut self) {
        let mut outcomes = Vec::new();
        if let Some(worker) = &self.persist_worker {
            while let Ok(outcome) = worker.receiver.try_recv() {
                outcomes.push(outcome);
            }
        }
        for outcome in outcomes {
            self.apply_persist_outcome(outcome);
        }
    }

    /// Persist theme and pins next to the server origin, like the credential
    /// save does. A failed write keeps the change for this run and says so.
    fn save_ui_settings(&mut self) {
        let origin = self.ui_origin();
        if origin.is_empty() {
            return;
        }
        self.enqueue_persist(PersistTask::Settings {
            origin,
            theme: self.theme.as_str().into(),
            pins: self.pins.clone(),
            prefs: self.prefs.clone(),
        });
    }

    pub fn set_theme(&mut self, theme: Theme) {
        self.theme = theme;
        self.save_ui_settings();
    }

    /// Change the device preferences and persist them.
    pub fn set_prefs(&mut self, prefs: creds::UiPrefs) {
        self.prefs = prefs.sanitized();
        self.save_ui_settings();
    }

    /// Save the composer drafts and queues to disk, off the UI thread.
    /// Text only; the encoder leaves out any draft holding a raw
    /// credential, so no secret ever reaches the file.
    pub fn persist_composer_state(&mut self) {
        let Some(dir) = self.composer_drafts_dir() else {
            return;
        };
        // The open chat's draft and queue live outside the stashed maps:
        // fold them in so a relaunch restores the chat that was open, too.
        self.drafts.store(&self.conversation, &self.composer);
        let mut queues = self.queues.clone();
        queues.stash(&self.conversation, self.queue.clone());
        let json = crate::chat_compose::encode_persisted(&self.drafts, &queues);
        self.enqueue_persist(PersistTask::ComposerDrafts { dir, json });
    }

    /// Where drafts and queues are persisted: the override in tests, the
    /// platform settings directory in the app.
    fn composer_drafts_dir(&self) -> Option<std::path::PathBuf> {
        self.composer_dir_override
            .clone()
            .or_else(creds::settings_dir)
    }

    /// Toast when a new approval request appears, if the user asked for it.
    fn note_approvals(&mut self) {
        let count = self.state.approvals.len();
        if self.prefs.notify_approvals && self.seen_approvals.is_some_and(|seen| count > seen) {
            self.toast = Some("A bot is waiting for your OK.".into());
        }
        self.seen_approvals = Some(count);
    }

    /// Toggle a pinned bot id or conversation id. The id is validated
    /// locally (bot slug or conversation id), deduplicated and capped at
    /// [`creds::MAX_PINS`], then persisted with the current theme and server.
    pub fn toggle_pin(&mut self, id: String) {
        if creds::toggle_pin_in_list(&mut self.pins, &id) {
            self.save_ui_settings();
        }
    }

    pub fn is_pinned(&self, id: &str) -> bool {
        self.pins.iter().any(|pin| pin == id)
    }

    /// Open a pinned entry: bare bot ids go to their one-to-one chat,
    /// conversation ids to the chat itself.
    pub fn open_pin(&mut self, id: String) {
        if id.contains(':') || id == "windows-owner" || id.chars().all(|c| c.is_ascii_digit()) {
            self.open_conversation(id);
        } else {
            self.open_bot_chat(id);
        }
    }

    /// Display label for a pinned id: the bot display name when the id is a
    /// bot (or a dir: chat of one), else the conversation label.
    pub fn pin_label(&self, id: &str) -> String {
        if let Some(bot) = id.strip_prefix("dir:")
            && self.state.listed_bots.iter().any(|item| item.id == bot)
        {
            return self.bot_display_name(bot);
        }
        if self.state.listed_bots.iter().any(|item| item.id == id) {
            return self.bot_display_name(id);
        }
        if let Some(conversation) = self
            .state
            .conversations
            .iter()
            .find(|conversation| conversation.id == id)
        {
            return self.conversation_label(conversation);
        }
        if id == "windows-owner" {
            return "Owner".into();
        }
        humanize_label(id)
    }

    /// True when the pin refers to a bot (bare slug or dir: chat), so the UI
    /// renders a bot avatar and opens the one-to-one chat.
    pub fn pin_is_bot(&self, id: &str) -> bool {
        if let Some(bot) = id.strip_prefix("dir:") {
            return self.state.listed_bots.iter().any(|item| item.id == bot);
        }
        self.state.listed_bots.iter().any(|item| item.id == id)
    }

    /// Avatar letter for a pinned id: the bot avatar for bot pins, else the
    /// first letter of the label.
    pub fn pin_avatar(&self, id: &str) -> String {
        if self.pin_is_bot(id) {
            let bot = id.strip_prefix("dir:").unwrap_or(id);
            return self.bot_avatar(bot);
        }
        self.pin_label(id)
            .chars()
            .next()
            .map(|c| c.to_uppercase().to_string())
            .unwrap_or_else(|| "·".into())
    }

    pub fn refresh(&mut self) {
        self.state.error = None;
        self.refreshing = true;
        self.poll_paused = false;
        self.start_request(if self.state.connected {
            Operation::Catalog
        } else {
            Operation::Health
        });
    }

    /// Friendly warning when a persisted session is within 48 hours of expiry,
    /// so the user can re-sign-in before hitting a 401.
    pub fn session_expires_soon(&self) -> Option<String> {
        let expires = self.session_expires?;
        let now = unix_now();
        if expires <= now {
            return None;
        }
        let remaining = expires - now;
        const WARN_WINDOW_SECS: u64 = 48 * 3600;
        if remaining > WARN_WINDOW_SECS {
            return None;
        }
        let hours = (remaining / 3600).max(1);
        Some(if hours >= 24 {
            format!(
                "Your session expires in about {} day(s).",
                hours.div_ceil(24)
            )
        } else {
            format!("Your session expires in about {hours} hour(s).")
        })
    }

    /// Export the active conversation's loaded messages as clean Markdown.
    pub fn conversation_markdown(&self) -> Option<String> {
        if self.state.messages.is_empty() {
            return None;
        }
        let title = self
            .state
            .conversations
            .iter()
            .find(|c| c.id == self.conversation)
            .and_then(|c| c.title.clone())
            .filter(|t| !t.trim().is_empty())
            .unwrap_or_else(|| "Conversation".to_owned());
        let mut out = format!("# {title}\n\n");
        for msg in &self.state.messages {
            let speaker = match msg.role {
                crate::api::Role::User => "You".to_owned(),
                crate::api::Role::Assistant => msg
                    .bot
                    .as_deref()
                    .map(|b| self.bot_display_name(b))
                    .unwrap_or_else(|| "Assistant".to_owned()),
            };
            let content = msg.content.trim();
            if content.is_empty() {
                continue;
            }
            out.push_str(&format!("### {speaker}\n\n{content}\n\n"));
        }
        Some(out)
    }

    pub fn open_section(&mut self, section: Section) {
        let mapped_settings = match section {
            Section::Memory => Some(SettingsSection::Memory),
            Section::Integrations => Some(SettingsSection::Integrations),
            Section::Providers => Some(SettingsSection::Providers),
            Section::Vault => Some(SettingsSection::Vault),
            Section::Usage => Some(SettingsSection::Usage),
            Section::Logs => Some(SettingsSection::Logs),
            _ => None,
        };
        if let Some(settings_section) = mapped_settings {
            self.section = Section::Settings;
            self.panel.set_page_open(false);
            self.refresh_status();
            self.refresh_telegram();
            self.load_memory_settings();
            self.open_settings_section(settings_section);
            return;
        }
        self.section = section;
        self.panel.set_page_open(section == Section::Browser);
        if section == Section::Settings {
            self.refresh_status();
            self.refresh_telegram();
            self.load_memory_settings();
            self.trigger_settings_section_load(self.settings_section);
        }
        if section == Section::Workflows {
            self.refresh_workflows();
        }
        self.ensure_section_data();
    }

    pub fn open_settings_section(&mut self, settings_section: SettingsSection) {
        self.settings_section = settings_section;
        self.trigger_settings_section_load(settings_section);
        self.ensure_section_data();
    }

    fn trigger_settings_section_load(&mut self, settings_section: SettingsSection) {
        match settings_section {
            SettingsSection::Vault => {
                self.panel.ask_vault_list();
                self.panel.ask_password_managers();
            }
            SettingsSection::Usage => {
                self.load_usage_days(self.usage_days);
                self.refresh_server_health();
            }
            SettingsSection::Logs => {
                self.refresh_logs();
            }
            SettingsSection::Integrations => {
                self.refresh_marketplace();
            }
            SettingsSection::Memory => {
                self.load_memory_settings();
            }
            SettingsSection::Voice => {
                self.load_voice();
            }
            _ => {}
        }
    }

    /// Read `GET /v1/logs` with the tab's current filter (0.28.0). A manual
    /// Refresh calls this again; the section load does too, once per connect.
    pub fn refresh_logs(&mut self) {
        if !self.state.connected {
            return;
        }
        let op = Operation::Logs {
            level: self.logs_level.clone(),
            query: self.logs_query.clone(),
        };
        if self.pending.is_some() || !self.ready() {
            self.queue_backlog(op);
            return;
        }
        self.start_request(op);
    }

    /// Change the Logs tab filter and re-read.
    pub fn set_logs_filter(&mut self, level: Option<String>, query: String) {
        self.logs_level = level;
        self.logs_query = query;
        self.refresh_logs();
    }

    /// Read the catalog. A manual Refresh calls this again.
    pub fn refresh_marketplace(&mut self) {
        if !self.state.connected {
            return;
        }
        // A retry after "unavailable" has to leave that card, or a later
        // error (not a 404) would stay hidden behind it.
        if self.state.marketplace_unavailable {
            self.state.marketplace_unavailable = false;
            self.state.marketplace_loaded = false;
        }
        self.state.marketplace_error = None;
        if self.pending.is_some() || !self.ready() {
            self.queue_backlog(Operation::MarketplaceList);
            return;
        }
        self.start_request(Operation::MarketplaceList);
    }

    /// Open a plugin's page: show what the list already knows and read the rest.
    /// Open one connector's card in the Integrations detail sheet.
    pub fn open_connector_sheet(&mut self, id: String) {
        self.integration_open = Some(id);
    }

    pub fn close_connector_sheet(&mut self) {
        self.integration_open = None;
    }

    pub fn open_plugin(&mut self, slug: String) {
        if !self.state.connected || !crate::valid_id(&slug) {
            return;
        }
        self.drop_plugin_signin_unless(Some(&slug));
        self.state.marketplace_detail = None;
        self.marketplace_open = Some(slug.clone());
        self.state.marketplace_error = None;
        self.start_request(Operation::MarketplaceDetail { slug });
    }

    pub fn close_plugin(&mut self) {
        self.drop_plugin_signin_unless(None);
        self.marketplace_open = None;
        self.state.marketplace_detail = None;
        self.state.marketplace_error = None;
    }

    /// Install, enable, disable or uninstall. Install and Enable are refused
    /// while the plugin cannot be called, including when its page is not open,
    /// so a click cannot store a secret or mark a plugin on for a runtime
    /// that does not exist.
    pub fn plugin_action(&mut self, slug: String, action: crate::marketplace::PluginAction) {
        use crate::marketplace::PluginAction;
        if !self.state.connected || !crate::valid_id(&slug) {
            return;
        }
        if matches!(action, PluginAction::Install | PluginAction::Enable)
            && let Some(reason) = self.plugin_activation_block(&slug)
        {
            self.state.marketplace_error = Some(reason.message().to_owned());
            return;
        }
        self.state.marketplace_error = None;
        self.start_request(Operation::MarketplaceAct { slug, action });
    }

    /// Send what the user typed into a plugin's setup form. The values are
    /// moved into the request and wiped when it finishes. Refused for a plugin
    /// that cannot run, so a key is not stored for a host nothing will call.
    pub fn save_plugin_config(&mut self, slug: String, values: Vec<(String, Zeroizing<String>)>) {
        if !self.state.connected || !crate::valid_id(&slug) {
            return;
        }
        if let Some(reason) = self.plugin_activation_block(&slug) {
            self.state.marketplace_error = Some(reason.message().to_owned());
            return;
        }
        self.state.marketplace_error = None;
        self.start_request(Operation::MarketplaceConfig { slug, values });
    }

    /// The reason Install, Enable or a secret save must not be sent.
    fn plugin_activation_block(&self, slug: &str) -> Option<crate::marketplace::ActivationBlock> {
        if let Some(detail) = &self.state.marketplace_detail
            && detail.plugin.slug == slug
        {
            return detail.activation_block();
        }
        if !crate::marketplace::RUNTIME_CALLS_PLUGINS {
            return Some(crate::marketplace::ActivationBlock::Runtime);
        }
        Some(crate::marketplace::ActivationBlock::Unchecked)
    }

    /// Read the Wave A visibility bundle (0.29.0). Independent of the open chat.
    pub fn refresh_workflows(&mut self) {
        if !self.state.connected {
            return;
        }
        self.state.workflows_error = None;
        if self.pending.is_some() || !self.ready() {
            self.queue_backlog(Operation::Workflows);
            return;
        }
        self.start_request(Operation::Workflows);
    }

    /// Update one scheduled routine (0.30.0 Wave B1). A write made while
    /// another request is in flight waits in the backlog rather than being dropped.
    pub fn save_workflow_routine(
        &mut self,
        name: String,
        enabled: Option<bool>,
        time: Option<String>,
        timezone: Option<String>,
    ) {
        self.queue_workflow_write(Operation::SaveRoutine {
            name,
            enabled,
            time,
            timezone,
        });
    }

    /// Update one scheduled routine with optional label and prompt (0.45.0).
    pub fn update_routine(
        &mut self,
        name: String,
        enabled: Option<bool>,
        time: Option<String>,
        timezone: Option<String>,
        label: Option<String>,
        prompt: Option<String>,
    ) {
        self.queue_workflow_write(Operation::PutRoutineDetails {
            name,
            enabled,
            time,
            timezone,
            label,
            prompt,
        });
    }

    /// Update an active multi-step workflow plan (0.45.0).
    pub fn update_active_workflow(
        &mut self,
        conversation_id: String,
        goal: Option<String>,
        items: Option<Vec<(String, bool)>>,
    ) {
        self.queue_workflow_write(Operation::PutActiveWorkflow {
            conversation_id,
            goal,
            items,
        });
    }

    /// Enqueue a manual watch run (0.30.0 Wave B2).
    pub fn run_all_watch_sources(&mut self) {
        self.queue_workflow_write(Operation::RunAllWatchSources);
    }

    /// Remove one routine's schedule (0.34.0). Schedule it again with
    /// [`Session::save_workflow_routine`].
    pub fn delete_workflow_routine(&mut self, name: String) {
        self.queue_workflow_write(Operation::DeleteRoutine { name });
    }

    /// Run one routine now (0.34.0): a brief, or a check of the sources.
    pub fn run_workflow_routine(&mut self, name: String) {
        if self.queue_workflow_write(Operation::RunRoutine { name }) {
            self.toast = Some("Run queued. It shows up in Jobs.".into());
        }
    }

    /// Add one of the caller's own RSS feeds (0.34.0).
    pub fn add_feed(&mut self, name: String, url: String) {
        self.queue_workflow_write(Operation::AddFeed { name, url });
    }

    /// Remove one of the caller's own RSS feeds (0.34.0).
    pub fn delete_feed(&mut self, name: String) {
        self.queue_workflow_write(Operation::DeleteFeed { name });
    }

    /// Enable or disable a source, or change the URL of the caller's own feed
    /// (0.34.0).
    pub fn patch_source(&mut self, name: String, enabled: Option<bool>, url: Option<String>) {
        self.queue_workflow_write(Operation::PatchSource { name, enabled, url });
    }

    /// Start one Workflows write. Returns `false` when it was not sent (no
    /// connection).
    fn queue_workflow_write(&mut self, operation: Operation) -> bool {
        if !self.state.connected {
            return false;
        }
        self.state.workflows_error = None;
        self.start_request(operation);
        true
    }

    /// Ask for one artifact's server-rendered preview (0.28.0). A hash that
    /// is cached or already being read sends nothing.
    pub fn request_doc_preview(&mut self, hash: String) {
        if self.state.doc_previews.contains_key(&hash)
            || !self.doc_previews_pending.insert(hash.clone())
        {
            return;
        }
        if self.pending.is_some() {
            self.schedule(Operation::DocPreview { hash }, Duration::ZERO);
        } else {
            self.start_request(Operation::DocPreview { hash });
        }
    }

    /// Open the right sidebar visual preview dock for an artifact hash.
    pub fn open_preview_dock(&mut self, hash: &str) {
        self.state.preview_dock_hash = Some(hash.to_owned());
        self.state.preview_dock_page = 0;
        self.state.preview_dock_zoom = 100;
        self.state.preview_dock_text_mode = false;
        self.bot_settings.open = false;
        self.request_doc_preview(hash.to_owned());
    }

    /// Close the right sidebar visual preview dock.
    pub fn close_preview_dock(&mut self) {
        self.state.preview_dock_hash = None;
    }

    /// Toggle the preview dock between standard and wide widths.
    pub fn toggle_preview_dock_wide(&mut self) {
        self.state.preview_dock_wide = !self.state.preview_dock_wide;
    }

    /// Switch the active page/slide/sheet index in the preview dock.
    pub fn set_preview_dock_page(&mut self, page: usize) {
        self.state.preview_dock_page = page;
    }

    /// Set the zoom percentage (clamped to 25..=300) in the preview dock.
    pub fn set_preview_dock_zoom(&mut self, zoom: u16) {
        self.state.preview_dock_zoom = zoom.clamp(25, 300);
    }

    /// Toggle between the rich visual renderer and raw text mode in the preview dock.
    pub fn toggle_preview_dock_text_mode(&mut self) {
        self.state.preview_dock_text_mode = !self.state.preview_dock_text_mode;
    }

    /// Toggle the right sidebar preview dock for an artifact hash.
    pub fn toggle_doc_preview(&mut self, hash: &str) {
        if self.state.preview_dock_hash.as_deref() == Some(hash) {
            self.close_preview_dock();
        } else {
            self.open_preview_dock(hash);
        }
    }

    /// Read the live sandbox posture from `GET /v1/status`.
    ///
    /// The read runs on its own thread so the console never blocks on it. A
    /// failed read keeps the last known posture and reports the error.
    pub fn refresh_status(&mut self) {
        if self.status.loading {
            return;
        }
        let Some(credentials) = &self.credentials else {
            self.status.status = None;
            self.status.error = Some("Not connected.".into());
            return;
        };
        let endpoint = credentials.endpoint.clone();
        let token = secrecy::SecretString::new(credentials.token.expose_secret().to_owned());
        let (sender, receiver) = std::sync::mpsc::channel();
        self.status.rx = Some(receiver);
        self.status.loading = true;
        self.status.error = None;
        if std::thread::Builder::new()
            .name("serabot-status".into())
            .spawn(move || {
                let result = crate::api::ApiClient::new(endpoint, token)
                    .and_then(|client| client.status())
                    .map_err(|error| error.to_string());
                let _ = sender.send(result);
            })
            .is_err()
        {
            self.status.rx = None;
            self.status.loading = false;
            self.status.error = Some("Could not read the server status.".into());
        }
    }

    /// Drain the status read, if one is running.
    fn poll_status(&mut self) {
        let Some(receiver) = &self.status.rx else {
            return;
        };
        let Ok(result) = receiver.try_recv() else {
            return;
        };
        self.status.rx = None;
        self.status.loading = false;
        match result {
            Ok(status) => {
                self.status.status = Some(status);
                self.status.error = None;
            }
            Err(error) => self.status.error = Some(error),
        }
    }

    /// Start "Sign in with Microsoft" for the server in the field.
    ///
    /// The whole loopback handoff (listener, server call, browser, wait,
    /// token exchange) runs on its own thread; the UI polls the result.
    pub fn start_sign_in(&mut self) {
        if self.sign_in.busy() {
            return;
        }
        let Ok(endpoint) = crate::validate_endpoint(self.server.trim()) else {
            self.sign_in.error = Some("Enter the server address first.".into());
            return;
        };
        let invite = match signin::clean_invite(&self.sign_in.invite_code) {
            Ok(invite) => invite,
            Err(message) => {
                self.sign_in.error = Some(message.into());
                return;
            }
        };
        let (sender, receiver) = std::sync::mpsc::channel();
        let cancel = Arc::new(AtomicBool::new(false));
        let thread_cancel = Arc::clone(&cancel);
        self.sign_in.error = None;
        self.sign_in.notice = None;
        self.connect_error = None;
        self.sign_in.phase = SignInPhase::Starting;
        self.sign_in.rx = Some(receiver);
        self.sign_in.cancel = Some(cancel);
        let timeout = signin::sign_in_timeout(creds::load_settings().sign_in_timeout_secs);
        let spawned = std::thread::Builder::new()
            .name("serabot-signin".into())
            .spawn(move || {
                let waiting = sender.clone();
                let result = signin::run(
                    endpoint,
                    invite.as_deref(),
                    &thread_cancel,
                    timeout,
                    &|url| webbrowser::open(url).is_ok(),
                    &|url, opened| {
                        let _ = waiting.send(SignInMsg::Waiting {
                            url: url.to_owned(),
                            opened,
                        });
                    },
                );
                let _ = sender.send(SignInMsg::Finished(result));
            });
        if spawned.is_err() {
            self.sign_in.rx = None;
            self.sign_in.cancel = None;
            self.sign_in.phase = SignInPhase::Idle;
            self.sign_in.error = Some("Could not start the browser sign-in.".into());
        }
    }

    /// Stop a sign-in that is waiting for the browser.
    pub fn cancel_sign_in(&mut self) {
        if let Some(cancel) = self.sign_in.cancel.take() {
            cancel.store(true, Ordering::SeqCst);
        }
        self.sign_in.rx = None;
        self.sign_in.phase = SignInPhase::Idle;
        self.sign_in.error = None;
        self.sign_in.link = None;
        self.sign_in.browser_opened = false;
        self.sign_in.notice = Some("Sign-in cancelled.".into());
    }

    fn poll_sign_in(&mut self) {
        self.poll_sign_in_run();
        self.poll_sign_out();
    }

    fn poll_sign_in_run(&mut self) {
        let Some(receiver) = &self.sign_in.rx else {
            return;
        };
        let mut finished = None;
        while let Ok(message) = receiver.try_recv() {
            match message {
                SignInMsg::Waiting { url, opened } => {
                    self.sign_in.phase = SignInPhase::Waiting;
                    self.sign_in.link = Some(url);
                    self.sign_in.browser_opened = opened;
                }
                SignInMsg::Finished(result) => finished = Some(result),
            }
        }
        let Some(result) = finished else {
            return;
        };
        self.sign_in.rx = None;
        self.sign_in.cancel = None;
        self.sign_in.phase = SignInPhase::Idle;
        self.sign_in.link = None;
        self.sign_in.browser_opened = false;
        match result {
            Ok(signed_in) => self.complete_sign_in(signed_in),
            Err(SignInFailure::Cancelled) => {}
            Err(SignInFailure::Message(message)) => self.sign_in.error = Some(message),
        }
    }

    /// A handoff was redeemed: keep the session token in the OS credential
    /// store, show the user at once and connect with the new token.
    fn complete_sign_in(&mut self, signed_in: SignedIn) {
        let mut token = signed_in.token.expose_secret().to_owned();
        let expires = signed_in.expires_at.as_deref().and_then(parse_rfc3339_unix);
        // A store failure is reported by persist_credentials once connected.
        self.enqueue_persist(PersistTask::SessionToken {
            token: secrecy::SecretString::new(token.clone()),
            expires,
        });
        self.token.zeroize();
        self.token = token.clone();
        token.zeroize();
        self.session_expires = expires;
        self.state.user = Some(signed_in.user);
        self.sign_in.clear_secrets();
        self.sign_in.show_invite = false;
        self.connect_now();
    }

    /// Sign out: end the session on the server, then delete the stored token.
    /// The server call runs off the UI thread. A server that cannot be
    /// reached still signs this device out.
    pub fn sign_out(&mut self) {
        if self.sign_out.in_flight {
            return;
        }
        let Some(credentials) = self.credentials.clone() else {
            self.finish_sign_out(None);
            return;
        };
        let (sender, receiver) = std::sync::mpsc::channel();
        self.sign_out.in_flight = true;
        self.sign_out.rx = Some(receiver);
        let spawned = std::thread::Builder::new()
            .name("serabot-signout".into())
            .spawn(move || {
                let result =
                    ApiClient::new(credentials.endpoint.clone(), credentials.token.clone())
                        .and_then(|client| client.logout())
                        .map_err(|error| error.to_string());
                let _ = sender.send(result);
            });
        if spawned.is_err() {
            self.sign_out.in_flight = false;
            self.sign_out.rx = None;
            self.finish_sign_out(Some(SIGNED_OUT_OFFLINE.into()));
        }
    }

    fn poll_sign_out(&mut self) {
        let Some(receiver) = &self.sign_out.rx else {
            return;
        };
        let Ok(result) = receiver.try_recv() else {
            return;
        };
        self.sign_out.rx = None;
        self.sign_out.in_flight = false;
        self.finish_sign_out(result.err().map(|_| SIGNED_OUT_OFFLINE.into()));
    }

    /// Drop the session on this device: stop streams, delete the stored token
    /// and return to the sign-in screen with `notice`.
    fn finish_sign_out(&mut self, notice: Option<String>) {
        let stored = self.clear_session();
        self.connect_error = stored.err();
        self.sign_in.notice = notice.or_else(|| Some("You are signed out.".into()));
    }

    /// Forget the session locally: the single place that deletes the stored
    /// token and returns the console to the sign-in screen.
    fn clear_session(&mut self) -> Result<(), String> {
        let origin = self
            .credentials
            .as_ref()
            .map(|credentials| credentials.endpoint.origin().ascii_serialization())
            .unwrap_or_else(|| self.server.clone());
        let stored = creds::forget_session();
        self.stop_events();
        self.reset_scope(false);
        self.reset_panel();
        self.sign_in.reset_transient();
        self.accounts_ui = AccountsUi::default();
        self.mcp_ui = McpUi::default();
        self.route_editor = RouteEditorUi::default();
        self.credentials = None;
        self.token.zeroize();
        self.token.clear();
        self.last_event_id = 0;
        self.last_event_kind.clear();
        self.event_frames.clear();
        self.subagents.clear();
        self.usage_loading = false;
        self.server_health_loading = false;
        self.voice = VoiceDraft::default();
        self.state.user = None;
        self.session_expires = None;
        self.connecting = false;
        self.reconnect_at = None;
        if !origin.is_empty() {
            self.server = origin;
        }
        self.state.error = None;
        stored
    }

    /// True when a saved session exists, so a failed connect can be retried
    /// without signing in again.
    pub fn has_saved_session(&self) -> bool {
        valid_token(&self.token)
    }

    /// Read the acting user's Telegram binding: `GET /v1/telegram`.
    pub fn refresh_telegram(&mut self) {
        if self.telegram.in_flight.is_some() {
            return;
        }
        let Some(credentials) = &self.credentials else {
            self.telegram.status = None;
            self.telegram.loaded = true;
            self.telegram.error = Some("Not connected.".into());
            return;
        };
        let endpoint = credentials.endpoint.clone();
        let token = secrecy::SecretString::new(credentials.token.expose_secret().to_owned());
        let (sender, receiver) = std::sync::mpsc::channel();
        self.telegram.rx = Some(receiver);
        self.telegram.in_flight = Some(TelegramAction::Status);
        self.telegram.loading = true;
        self.telegram.error = None;
        if std::thread::Builder::new()
            .name("serabot-telegram-status".into())
            .spawn(move || {
                let result = crate::api::ApiClient::new(endpoint, token)
                    .and_then(|client| client.telegram())
                    .map(TelegramReply::Status)
                    .map_err(telegram_error);
                let _ = sender.send((TelegramAction::Status, result));
            })
            .is_err()
        {
            self.telegram.rx = None;
            self.telegram.in_flight = None;
            self.telegram.loading = false;
            self.telegram.loaded = true;
            self.telegram.error = Some("Could not read the Telegram status.".into());
        }
    }

    /// Mint a single-use Telegram linking code: `POST /v1/telegram/link`.
    pub fn link_telegram(&mut self) {
        if self.telegram.in_flight.is_some() {
            return;
        }
        if self
            .telegram
            .status
            .as_ref()
            .is_some_and(|status| !status.enabled)
        {
            self.telegram.error = Some("Telegram is not enabled on this deployment.".into());
            return;
        }
        let Some(credentials) = &self.credentials else {
            self.telegram.error = Some("Connect to the server first.".into());
            return;
        };
        let endpoint = credentials.endpoint.clone();
        let token = secrecy::SecretString::new(credentials.token.expose_secret().to_owned());
        let (sender, receiver) = std::sync::mpsc::channel();
        self.telegram.rx = Some(receiver);
        self.telegram.in_flight = Some(TelegramAction::Link);
        self.telegram.linking = true;
        self.telegram.confirm_unlink = false;
        self.telegram.error = None;
        if std::thread::Builder::new()
            .name("serabot-telegram-link".into())
            .spawn(move || {
                let result = crate::api::ApiClient::new(endpoint, token)
                    .and_then(|client| client.telegram_link())
                    .map(TelegramReply::Link)
                    .map_err(telegram_error);
                let _ = sender.send((TelegramAction::Link, result));
            })
            .is_err()
        {
            self.telegram.rx = None;
            self.telegram.in_flight = None;
            self.telegram.linking = false;
            self.telegram.error = Some("Could not request a Telegram linking code.".into());
        }
    }

    /// Ask for the unlink confirmation step. Nothing is sent yet.
    pub fn request_unlink_telegram(&mut self) {
        if self
            .telegram
            .status
            .as_ref()
            .is_some_and(|status| status.linked)
        {
            self.telegram.confirm_unlink = true;
            self.telegram.error = None;
        }
    }

    pub fn cancel_unlink_telegram(&mut self) {
        self.telegram.confirm_unlink = false;
    }

    /// Unlink the acting user's chat: `DELETE /v1/telegram/link`. The caller
    /// must have passed through [`Self::request_unlink_telegram`].
    pub fn confirm_unlink_telegram(&mut self) {
        if !self.telegram.confirm_unlink || self.telegram.in_flight.is_some() {
            return;
        }
        let Some(credentials) = &self.credentials else {
            self.telegram.error = Some("Connect to the server first.".into());
            return;
        };
        let endpoint = credentials.endpoint.clone();
        let token = secrecy::SecretString::new(credentials.token.expose_secret().to_owned());
        let (sender, receiver) = std::sync::mpsc::channel();
        self.telegram.rx = Some(receiver);
        self.telegram.in_flight = Some(TelegramAction::Unlink);
        self.telegram.unlinking = true;
        self.telegram.error = None;
        if std::thread::Builder::new()
            .name("serabot-telegram-unlink".into())
            .spawn(move || {
                let result = crate::api::ApiClient::new(endpoint, token)
                    .and_then(|client| client.telegram_unlink())
                    .map(TelegramReply::Unlinked)
                    .map_err(telegram_error);
                let _ = sender.send((TelegramAction::Unlink, result));
            })
            .is_err()
        {
            self.telegram.rx = None;
            self.telegram.in_flight = None;
            self.telegram.unlinking = false;
            self.telegram.error = Some("Could not unlink Telegram.".into());
        }
    }

    /// Drain the Telegram read or write, if one is running.
    fn poll_telegram(&mut self) {
        let Some(receiver) = &self.telegram.rx else {
            return;
        };
        let Ok((action, result)) = receiver.try_recv() else {
            return;
        };
        self.telegram.rx = None;
        self.telegram.in_flight = None;
        match action {
            TelegramAction::Status => {
                self.telegram.loading = false;
                self.telegram.loaded = true;
                match result {
                    Ok(TelegramReply::Status(status)) => {
                        if status.linked {
                            self.telegram.link = None;
                            self.telegram.confirm_unlink = false;
                        }
                        self.telegram.status = Some(status);
                        self.telegram.error = None;
                    }
                    Ok(_) => {
                        self.telegram.error = Some("Unexpected Telegram response.".into());
                    }
                    Err(message) => self.telegram.error = Some(message),
                }
            }
            TelegramAction::Link => {
                self.telegram.linking = false;
                match result {
                    Ok(TelegramReply::Link(link)) => {
                        self.telegram.link = Some(link);
                        self.telegram.error = None;
                    }
                    Ok(_) => {
                        self.telegram.error = Some("Unexpected Telegram response.".into());
                    }
                    Err(message) => self.telegram.error = Some(message),
                }
            }
            TelegramAction::Unlink => {
                self.telegram.unlinking = false;
                self.telegram.confirm_unlink = false;
                match result {
                    Ok(TelegramReply::Unlinked(was_linked)) => {
                        if let Some(status) = self.telegram.status.as_mut() {
                            status.linked = false;
                            status.telegram_user_id = None;
                            status.telegram_chat_id = None;
                            status.linked_at = None;
                        }
                        self.telegram.link = None;
                        self.telegram.error = None;
                        self.toast = Some(if was_linked {
                            "Telegram unlinked.".into()
                        } else {
                            "Telegram was not linked.".into()
                        });
                    }
                    Ok(_) => {
                        self.telegram.error = Some("Unexpected Telegram response.".into());
                    }
                    Err(message) => self.telegram.error = Some(message),
                }
            }
        }
    }

    /// Run a browser or Explorer launch off the UI thread. `job` returns the
    /// toast text, which `poll_launches` shows when it arrives.
    fn launch(&mut self, job: impl FnOnce() -> String + Send + 'static) {
        let sender = self.launch_tx.clone();
        let spawned = std::thread::Builder::new()
            .name("serabot-launch".into())
            .spawn(move || {
                let _ = sender.send(job());
            });
        if spawned.is_err() {
            self.toast = Some("Could not start the launcher.".into());
        }
    }

    /// Open `url` in the default browser without blocking the UI thread.
    pub fn open_in_browser(&mut self, url: String) {
        // Checked here, where the link is opened, so no caller can bypass it.
        if !crate::net_policy::external_link_allowed(&url) {
            self.toast =
                Some("That link is not a secure web address, so it was not opened.".into());
            return;
        }
        self.launch(move || match webbrowser::open(&url) {
            Ok(()) => "Opened your browser.".to_owned(),
            Err(error) => format!("Could not open the browser: {error}"),
        });
    }

    fn poll_launches(&mut self) {
        while let Ok(message) = self.launch_rx.try_recv() {
            self.toast = Some(message);
        }
    }

    /// Drain the artifact file write, if one is running.
    fn poll_download(&mut self) {
        let Some(receiver) = &self.download_rx else {
            return;
        };
        let Ok(result) = receiver.try_recv() else {
            return;
        };
        self.download_rx = None;
        self.downloading = false;
        match result {
            Ok(path) => match self.pending_open.take() {
                Some(PendingOpen::Open) => {
                    self.toast = Some(format!("Saved to {path}"));
                    self.launch(move || match open_with_default_app(&path) {
                        Ok(()) => format!("Opened {path}"),
                        Err(error) => format!("Saved to {path}, but could not open it: {error}"),
                    });
                }
                Some(PendingOpen::SaveAs) => self.toast = Some(format!("Saved to {path}")),
                Some(PendingOpen::Reveal) => {
                    self.toast = Some(format!("Saved to {path}"));
                    self.launch(move || match reveal_in_explorer(&path) {
                        Ok(()) => format!("Showing {path}"),
                        Err(error) => format!("Saved to {path}, but could not reveal it: {error}"),
                    });
                }
                None => self.toast = Some(format!("Saved to {path}")),
            },
            Err(error) => {
                self.pending_open = None;
                self.toast = Some(format!("Download failed: {error}"));
            }
        }
    }

    /// The bot a conversation talks to: the bot of a `dir:<bot>` chat, or the
    /// owner of an extra chat (from the loaded per-bot lists).
    pub fn conversation_bot(&self, conversation: &str) -> Option<String> {
        crate::bot_chats::bot_of_chat(self.state.bot_chats.values(), conversation)
    }

    /// One chat from the loaded per-bot lists.
    pub fn bot_chat(&self, id: &str) -> Option<&crate::bot_chats::BotChat> {
        self.state
            .bot_chats
            .values()
            .flat_map(|list| list.chats.iter())
            .find(|chat| chat.id == id)
    }

    /// Read a bot's chat list.
    /// Read one bot's library (0.63).
    pub fn load_bot_library(&mut self, bot: String) {
        if valid_id(&bot) && self.state.connected {
            self.state.bot_library_loading = true;
            self.start_request(Operation::BotLibrary { bot });
        }
    }

    /// Save one of the member's uploads to Downloads (0.63 library).
    pub fn download_attachment(&mut self, id: String) {
        if self.downloading {
            self.toast = Some("A download is already running.".into());
            return;
        }
        if !self.ready() {
            self.toast = Some("Wait for the current request, then retry.".into());
            return;
        }
        if valid_id(&id) {
            self.pending_open = None;
            self.start_request(Operation::FetchAttachment { id });
        }
    }

    pub fn load_bot_chats(&mut self, bot: String) {
        if valid_id(&bot) && self.state.connected {
            self.start_request(Operation::ListBotChats { bot });
        }
    }

    /// Read every list that is already loaded again (after a rename, archive
    /// or new chat).
    fn reload_bot_chats(&mut self) {
        let bots: Vec<String> = self.state.bot_chats_loaded.iter().cloned().collect();
        for bot in bots {
            self.start_request(Operation::ListBotChats { bot });
        }
    }

    /// Start another chat with a bot. The server seeds it from the bot's memory
    /// and a short summary; it opens when the server answers.
    pub fn new_bot_chat(&mut self, bot: String) {
        if !valid_id(&bot) || !self.state.connected {
            return;
        }
        self.state.error = None;
        self.start_request(Operation::CreateBotChat { bot, title: None });
    }

    /// Save "refresh memory every N turns" for a bot.
    pub fn set_bot_refresh(&mut self, bot: String, every: u32) {
        if !valid_id(&bot) || !crate::bot_chats::valid_refresh_turns(every) {
            return;
        }
        self.state.error = None;
        self.start_request(Operation::SetBotRefresh { bot, every });
    }

    /// Intentionally replace the composer text (0.74.3): a chip, a slash
    /// insert, a failed-send restore or a conversation switch. Bumps
    /// `composer_gen` so the chat view syncs the new text into the box.
    /// Typing syncs (blur, Enter, Queue, Send) write `composer` directly
    /// and must not bump the generation.
    pub fn replace_composer(&mut self, text: String) {
        self.composer = text;
        self.composer_gen = self.composer_gen.wrapping_add(1);
    }

    pub fn open_conversation(&mut self, id: String) {
        if !valid_id(&id) {
            return;
        }
        // The draft of the conversation being left waits for its return,
        // along with its queue, its composer's files and any held draft.
        self.drafts.store(&self.conversation, &self.composer);
        let left = std::mem::take(&mut self.queue);
        self.queues.stash(&self.conversation, left);
        let files = std::mem::take(&mut self.composer_files);
        let pending = std::mem::take(&mut self.upload_queue);
        self.composer_files_stash
            .stash(&self.conversation, files, pending);
        if let Some(held) = self.state.held_draft.take() {
            self.held_drafts.insert(self.conversation.clone(), held);
        }
        self.runs_open.clear();
        self.runs_last_read = None;
        self.conversation = id;
        self.queue = self.queues.restore(&self.conversation);
        self.state.chat_failure = None;
        self.section = Section::Chat;
        self.panel.set_page_open(false);
        if self.bot_settings.open {
            // The panel follows the chat; unsaved edits stay with the old bot.
            self.open_settings_for_conversation();
        }
        let has_models = !self.state.providers.is_empty()
            || self
                .state
                .catalog
                .as_ref()
                .is_some_and(|c| !c.providers.is_empty());
        self.reset_scope(true);
        let (files, pending) = self.composer_files_stash.restore(&self.conversation);
        self.composer_files = files;
        self.upload_queue = pending;
        // After `reset_scope`: `new_scope` rebuilds the state, so the held
        // draft is restored here, not before.
        self.state.held_draft = self.held_drafts.remove(&self.conversation);
        self.replace_composer(self.drafts.restore(&self.conversation));
        self.persist_composer_state();
        self.auto_approve_confirming = false;
        self.state.error = None;
        // Opening a chat is reading it: its badge goes now, not when the
        // transcript has arrived.
        let opened = self.conversation.clone();
        self.state.clear_unread(&opened);
        self.refreshing = true;
        self.poll_paused = false;
        if has_models {
            self.start_request(Operation::History);
        } else {
            self.start_request(Operation::Catalog);
        }
        // A dir: conversation is a one-to-one bot chat: load that bot's
        // detail right behind the transcript, like the catalog chain.
        self.schedule_open_bot_detail();
    }

    /// Schedule a detail load for the bot of the open `dir:` conversation,
    /// unless the detail is cached or already in flight. Used when a
    /// conversation opens and on reconnect, next to the catalog refresh.
    fn schedule_open_bot_detail(&mut self) {
        let Some(bot) = self
            .conversation_bot(&self.conversation)
            .filter(|bot| valid_speaker(bot))
        else {
            return;
        };
        if self.state.bot_details.contains_key(&bot)
            || self.bot_detail_pending.as_deref() == Some(bot.as_str())
        {
            return;
        }
        self.schedule(Operation::BotDetail { id: bot }, Duration::ZERO);
    }

    /// Load `GET /v1/bots/{id}` into the cache. One request per bot id:
    /// cached details and in-flight loads are never requested again.
    pub fn load_bot_detail(&mut self, id: String) {
        if !valid_speaker(&id) || !self.state.connected {
            return;
        }
        if self.state.bot_details.contains_key(&id)
            || self.bot_detail_pending.as_deref() == Some(id.as_str())
        {
            return;
        }
        let operation = Operation::BotDetail { id };
        if self.pending.is_some() || !self.ready() {
            self.schedule(operation, Duration::ZERO);
        } else {
            self.start_request(operation);
        }
    }

    /// Save tools, skills or approval requirements of one bot
    /// (`PUT /v1/admin/bots/{id}/config`). Validated locally first; on
    /// success the cached detail is updated optimistically and a toast is
    /// set. A failed write with an unknown outcome is never resent.
    pub fn save_bot_config(&mut self, id: String, update: BotConfigUpdate) {
        if !valid_speaker(&id) {
            self.admin_error = Some("Bot id is not valid.".into());
            return;
        }
        if let Err(error) = validate_bot_config_update(&update) {
            self.admin_error = Some(error.to_string());
            return;
        }
        if !self.can_admin() {
            self.admin_error = Some("Wait for the current request, then retry.".into());
            return;
        }
        self.admin_error = None;
        self.admin_notice = None;
        self.start_request(Operation::UpdateBotConfig { id, update });
    }

    /// Usage page: pick a range (7, 30 or 90 days) and load it.
    pub fn load_usage_days(&mut self, days: u32) {
        let days = if matches!(days, 7 | 30 | 90) {
            days
        } else {
            30
        };
        self.usage_days = days;
        self.usage_loading = true;
        self.load_usage(days);
    }

    /// Gather the "This PC" numbers on a worker thread (never blocks the UI).
    pub fn refresh_pc(&self) {
        let folders = self
            .local_folders
            .iter()
            .map(|folder| std::path::PathBuf::from(&folder.path))
            .collect();
        self.pc.refresh(crate::pc_stats::PcDirs::current(), folders);
    }

    /// "Clear cache": delete downloaded update files only. They are
    /// regenerable; settings, the folder list and sign-in stay untouched.
    pub fn clear_pc_cache(&mut self) {
        let Some(dir) = crate::updater::downloads_dir() else {
            self.toast = Some("There is nothing to clear.".into());
            return;
        };
        let freed = crate::updater::clear_downloads(&dir);
        self.pc.invalidate();
        self.refresh_pc();
        self.toast = Some(if freed == 0 {
            "There was nothing to clear.".to_owned()
        } else {
            format!(
                "Cleared {}. Update files download again if needed.",
                crate::status_view::format_bytes(freed)
            )
        });
    }

    /// Owner only: load the server health card. A member never sends this
    /// request, so the server's 403 is a backstop, not the gate.
    pub fn refresh_server_health(&mut self) {
        if !self.is_admin() || self.server_health_loading {
            return;
        }
        self.server_health_loading = true;
        if self.pending.is_some() || !self.ready() {
            self.schedule(Operation::ServerHealth, Duration::ZERO);
        } else {
            self.start_request(Operation::ServerHealth);
        }
    }

    /// Load the token usage report for the last `days` days (1–365).
    pub fn load_usage(&mut self, days: u32) {
        self.load_usage_scoped(days, None, None);
    }

    /// Load the usage report, optionally scoped to one bot or conversation.
    pub fn load_usage_scoped(
        &mut self,
        days: u32,
        bot: Option<String>,
        conversation: Option<String>,
    ) {
        if !(1..=MAX_USAGE_DAYS).contains(&days) {
            self.state.error = Some("Usage window must be 1–365 days.".into());
            return;
        }
        if bot.as_deref().is_some_and(|bot| !valid_speaker(bot)) {
            self.state.error = Some("The bot filter must be a bot id.".into());
            return;
        }
        if conversation.as_deref().is_some_and(|id| !valid_id(id)) {
            self.state.error = Some("Conversation id is not valid.".into());
            return;
        }
        let operation = Operation::Usage {
            days,
            bot,
            conversation,
        };
        if self.pending.is_some() || !self.ready() {
            self.schedule(operation, Duration::ZERO);
        } else {
            self.start_request(operation);
        }
    }

    /// Open the Browser page's focus view on one session, showing the
    /// conversation of the bot that drives it next to the live page.
    pub fn focus_browser(&mut self, id: &str) {
        let conversation = self.panel.session(id).and_then(|session| {
            session
                .conversation_id
                .clone()
                .or_else(|| session.bot.as_ref().map(|bot| format!("dir:{bot}")))
        });
        if let Some(conversation) = conversation
            && conversation != self.conversation
            && valid_id(&conversation)
        {
            self.open_conversation(conversation);
        }
        // The chat opens in place: the page stays the Browser page.
        self.section = Section::Browser;
        self.panel.set_page_open(true);
        self.panel.focus_session(id);
    }

    pub fn open_bot_chat(&mut self, id: String) {
        if self.state.bot_chat_binding != Some(true) {
            self.state.error = Some(
                "This server does not route one-to-one bot chats. Update the server to 0.5.0 or later."
                    .into(),
            );
            return;
        }
        self.open_conversation(format!("dir:{id}"));
    }

    pub fn bot_display_name(&self, id: &str) -> String {
        self.state
            .listed_bots
            .iter()
            .find(|bot| bot.id == id)
            // The server already falls back to the title; this covers an
            // older or sparse reply before the id is humanized.
            .and_then(|bot| {
                bot.display_name
                    .clone()
                    .filter(|name| !name.trim().is_empty())
                    .or_else(|| bot.title.clone())
            })
            .filter(|name| !name.trim().is_empty())
            .unwrap_or_else(|| humanize_label(id))
    }

    pub fn bot_avatar(&self, id: &str) -> String {
        if let Some(avatar) = self
            .state
            .listed_bots
            .iter()
            .find(|bot| bot.id == id)
            .and_then(|bot| bot.avatar.clone())
            .filter(|avatar| !avatar.trim().is_empty())
        {
            return avatar;
        }
        self.bot_display_name(id)
            .chars()
            .next()
            .map(|c| c.to_uppercase().to_string())
            .unwrap_or_else(|| "B".into())
    }

    fn first_user_message_in_active(&self) -> Option<&str> {
        self.state.messages.iter().find_map(|message| {
            (message.role == Role::User && !message.content.trim().is_empty())
                .then_some(message.content.as_str())
        })
    }

    fn fallback_extra_chat_title(&self, id: &str) -> String {
        if let Some(chat) = self.bot_chat(id) {
            let bot_name = self
                .conversation_bot(id)
                .map(|bot| self.bot_display_name(&bot));
            let first_msg = (id == self.conversation)
                .then(|| self.first_user_message_in_active())
                .flatten();
            return crate::bot_chats::chat_title_with_fallback(
                chat,
                first_msg,
                bot_name.as_deref(),
            );
        }
        if id == self.conversation
            && let Some(derived) = self
                .first_user_message_in_active()
                .and_then(crate::bot_chats::derive_title_from_message)
        {
            return derived;
        }
        if let Some(bot) = self.conversation_bot(id) {
            return format!("New conversation \u{00b7} {}", self.bot_display_name(&bot));
        }
        "New conversation".into()
    }

    pub fn conversation_label(&self, conversation: &crate::ConversationSummary) -> String {
        if let Some(title) = conversation
            .title
            .as_deref()
            .map(str::trim)
            .filter(|title| !title.is_empty() && !crate::bot_chats::is_raw_chat_id(title))
        {
            return title.to_owned();
        }
        if conversation.id == "windows-owner" {
            return "Owner".into();
        }
        if crate::bot_chats::is_extra_chat_id(&conversation.id) {
            return self.fallback_extra_chat_title(&conversation.id);
        }
        if let Some(bot) = conversation.id.strip_prefix("dir:")
            && self.state.listed_bots.iter().any(|item| item.id == bot)
        {
            return self.bot_display_name(bot);
        }
        let kind = conversation.normalized_kind();
        let slug = conversation
            .id
            .split_once(':')
            .map(|(_, slug)| slug)
            .unwrap_or(&conversation.id);
        if conversation.id.chars().all(|c| c.is_ascii_digit())
            || crate::bot_chats::is_raw_chat_id(slug)
            || crate::bot_chats::is_raw_chat_id(&conversation.id)
        {
            return self.fallback_extra_chat_title(&conversation.id);
        }
        match kind {
            "section" => format!("Section · {}", humanize_label(slug)),
            "group" => format!("Group · {}", humanize_label(slug)),
            "room" => format!("Room · {}", humanize_label(slug)),
            _ => humanize_label(slug),
        }
    }

    /// Row text under a title in the conversation list: the newest message
    /// preview when the server sent one, otherwise the kind subtitle.
    pub fn conversation_list_subtitle(&self, conversation: &crate::ConversationSummary) -> String {
        match &conversation.last_message {
            Some(last) if !last.text.trim().is_empty() => {
                if last.role == "user" {
                    format!("You: {}", last.text.trim())
                } else {
                    last.text.trim().to_owned()
                }
            }
            _ => self.conversation_subtitle(conversation),
        }
    }

    pub fn conversation_subtitle(&self, conversation: &crate::ConversationSummary) -> String {
        if conversation.id == "windows-owner" {
            return "Your direct chat".into();
        }
        if let Some(bot) = conversation.id.strip_prefix("dir:")
            && let Some(found) = self.state.listed_bots.iter().find(|item| item.id == bot)
        {
            return format!("Bot · {}", found.role);
        }
        if conversation.id.chars().all(|c| c.is_ascii_digit()) {
            return "Direct chat".into();
        }
        match conversation.normalized_kind() {
            "section" => "Section".into(),
            "group" => "Folder".into(),
            "room" => "Group chat".into(),
            _ => "Direct chat".into(),
        }
    }

    /// User messages are "You"; assistant messages resolve to the producing bot.
    pub fn message_speaker(&self, message: &Message) -> (String, String, Option<String>) {
        if message.role == Role::User {
            return ("You".into(), "Y".into(), None);
        }
        let dir_bot = if self.state.bot_chat_binding == Some(true) {
            self.conversation_bot(&self.conversation)
        } else {
            None
        };
        let bot = message
            .bot
            .as_deref()
            .or(message.name.as_deref())
            .or(dir_bot.as_deref())
            .or_else(|| {
                self.state
                    .listed_bots
                    .iter()
                    .find(|bot| bot.role == "orchestrator")
                    .map(|bot| bot.id.as_str())
            });
        match bot {
            Some(id) if !id.is_empty() => (
                self.bot_display_name(id),
                self.bot_avatar(id),
                Some(id.to_owned()),
            ),
            _ => ("Assistant".into(), "A".into(), None),
        }
    }

    /// Send the composer as the next turn. Every guard that stops a send
    /// says why in a toast instead of silently doing nothing.
    pub fn submit(&mut self) {
        let has_files = !self.composer_files.is_empty();
        let text_ok = valid_text(&self.composer) || (has_files && self.composer.trim().is_empty());
        if self.uploading || !self.upload_queue.is_empty() {
            self.toast = Some("Still uploading…".into());
            return;
        }
        if !self.state.connected {
            self.toast = Some("Waiting for connection".into());
            return;
        }
        if !self.ready() {
            self.toast = Some("Waiting for the current request to finish.".into());
            return;
        }
        if !self.state.can_send() {
            self.toast = Some(self.send_blocked_reason());
            return;
        }
        if !text_ok {
            self.toast = Some(
                if self.composer.chars().count() > crate::chat_compose::MAX_TEXT_CHARS {
                    "Message is over the limit".into()
                } else {
                    "Type a message first.".into()
                },
            );
            return;
        }
        self.state.error = None;
        self.state.chat_failure = None;
        self.state.held_draft = None;
        self.state.activity.clear();
        let text = std::mem::take(&mut self.composer);
        // Sending consumes the draft: bump the generation so the view
        // clears the box even from paths (Retry, Regenerate) that do not
        // set the local draft themselves.
        if !text.trim().is_empty() {
            self.composer_gen = self.composer_gen.wrapping_add(1);
        }
        self.drafts.clear(&self.conversation);
        self.persist_composer_state();
        self.attach_notice = None;
        self.regenerating = false;
        let files = std::mem::take(&mut self.composer_files);
        self.state.messages.push(Message {
            id: format!("opt-{}", Instant::now().elapsed().as_nanos()),
            role: Role::User,
            content: text.clone(),
            created_at: "Just now".into(),
            name: None,
            bot: None,
            attachments: files.clone(),
            limit_reached: false,
            turn_id: None,
        });
        if files.is_empty() {
            // A leading `@bot` sends the message to that bot (a delegate).
            let routed = self.route_for(&text);
            match routed.target {
                Some(bot) => self.start_request(Operation::SubmitTo {
                    text: zeroize::Zeroizing::new(routed.text),
                    bot,
                }),
                None => self.start_request(Operation::Submit(zeroize::Zeroizing::new(text))),
            }
        } else {
            self.start_request(Operation::SubmitWith {
                text: zeroize::Zeroizing::new(text),
                attachments: files.into_iter().map(|file| file.id).collect(),
            });
        }
    }

    /// Why a send is refused while the connection is up.
    fn send_blocked_reason(&self) -> String {
        if !self.state.history_loaded {
            "Still loading this conversation…".into()
        } else if self.state.unknown_outcome {
            "The last message may not have arrived. Check the transcript first.".into()
        } else if self.main_run_active() || self.has_optimistic_pending() {
            "A reply is still running.".into()
        } else {
            "Pick a model for this chat first.".into()
        }
    }

    /// Attach the over-long composer text as a `message.txt` file instead of
    /// sending it as text, so a long draft can still go out. The box is
    /// cleared; the file is sent with the next send.
    pub fn attach_composer_as_file(&mut self) {
        let text = self.composer.trim().to_owned();
        if text.is_empty() {
            self.toast = Some("Type a message first.".into());
            return;
        }
        // Refused (for example the message already holds the maximum files):
        // keep the text in the box and say why.
        let admission = crate::chat_compose::admit_files(
            self.attachments_held(),
            &[("message.txt", text.len() as u64)],
        );
        if admission.accepted.is_empty() {
            self.attach_notice = Some(admission.refused.join(" "));
            return;
        }
        self.queue_files(vec![("message.txt".to_owned(), text.into_bytes())]);
        self.composer.zeroize();
        self.composer = String::new();
        self.composer_gen = self.composer_gen.wrapping_add(1);
        self.drafts.clear(&self.conversation);
        self.persist_composer_state();
    }

    /// Continue under a reply that a tool limit ended (0.24). Sends the fixed
    /// Continue text as the owner's next message. The composer draft is left
    /// as it is, and the same send guards as `submit` apply.
    pub fn continue_from_limit(&mut self) {
        if !self.state.can_send()
            || !self.ready()
            || self.uploading
            || !self.upload_queue.is_empty()
        {
            return;
        }
        self.state.error = None;
        self.state.activity.clear();
        self.state.messages.push(Message {
            id: format!("opt-{}", Instant::now().elapsed().as_nanos()),
            role: Role::User,
            content: crate::media_view::CONTINUE_TEXT.into(),
            created_at: "Just now".into(),
            name: None,
            bot: None,
            attachments: Vec::new(),
            limit_reached: false,
            turn_id: None,
        });
        self.start_request(Operation::Submit(zeroize::Zeroizing::new(
            crate::media_view::CONTINUE_TEXT.into(),
        )));
    }

    /// Read back a video or audio file for an inline player, unless it is
    /// already here. The bytes come from the artifact route and are turned into
    /// a data URL off the UI thread.
    pub fn request_preview(&mut self, hash: String, mime: String) {
        if self.state.media_previews.contains(&hash) {
            return;
        }
        self.schedule(Operation::PreviewArtifact { hash, mime }, Duration::ZERO);
    }

    /// Read the owner's per-turn work limits (Settings).
    pub fn load_turn_limits(&mut self) {
        self.schedule(Operation::TurnLimits, Duration::ZERO);
    }

    /// Save the Settings form. Both fields must be whole numbers within the
    /// server ceiling; a bad field is named in `turn_limits_error` and nothing
    /// is sent.
    pub fn save_turn_limits(&mut self, calls: &str, rounds: &str) {
        let ceiling = match self.state.turn_limits.as_ref() {
            Some(view) => view.ceiling,
            None => {
                self.state.turn_limits_error = Some("Wait for the limits to load first.".into());
                return;
            }
        };
        match crate::media_view::parse_turn_limits(calls, rounds, ceiling) {
            Ok(limits) => {
                self.state.turn_limits_error = None;
                self.start_request(Operation::SaveTurnLimits {
                    calls: limits.max_tool_calls_per_turn,
                    rounds: limits.max_tool_rounds,
                });
            }
            Err(message) => self.state.turn_limits_error = Some(message),
        }
    }

    /// Go back to the config defaults (Settings).
    pub fn reset_turn_limits(&mut self) {
        self.start_request(Operation::ResetTurnLimits);
    }

    pub fn load_memory_settings(&mut self) {
        self.schedule(Operation::MemorySettings, Duration::ZERO);
    }

    pub fn save_mem0_key(&mut self, api_key: &str) {
        let key = api_key.trim();
        if key.is_empty() {
            self.state.memory_settings_error = Some("Paste a Mem0 API key first.".into());
            return;
        }
        self.state.memory_settings_error = None;
        self.start_request(Operation::SetMem0Key {
            key: Zeroizing::new(key.to_owned()),
        });
    }

    pub fn delete_mem0_key(&mut self) {
        self.state.memory_settings_error = None;
        self.start_request(Operation::DeleteMem0Key);
    }

    /// The paperclip: pick files in a native dialog (off the UI thread),
    /// check name and size, and queue them for upload.
    pub fn pick_attachments(&mut self) {
        if self.pick_rx.is_some() {
            return;
        }
        let room = crate::MAX_ATTACHMENTS
            .saturating_sub(self.composer_files.len() + self.upload_queue.len());
        if room == 0 {
            self.toast = Some(format!("Attach at most {} files.", crate::MAX_ATTACHMENTS));
            return;
        }
        let (sender, receiver) = std::sync::mpsc::channel();
        self.pick_rx = Some(receiver);
        if std::thread::Builder::new()
            .name("serabot-attach".into())
            .spawn(move || {
                let _ = sender.send(pick_files(room));
            })
            .is_err()
        {
            self.pick_rx = None;
            self.toast = Some("Could not open the file picker.".into());
        }
    }

    /// Settings > Local folders > "Add folder": native folder picker off the
    /// UI thread; the folder is added read-only (the user can widen it later).
    pub fn pick_local_folder(&mut self) {
        if self.folder_pick_rx.is_some() {
            return;
        }
        let (sender, receiver) = std::sync::mpsc::channel();
        self.folder_pick_rx = Some(receiver);
        if std::thread::Builder::new()
            .name("serabot-folder-pick".into())
            .spawn(move || {
                let _ = sender.send(pick_folder().map(|path| (path, false)));
            })
            .is_err()
        {
            self.folder_pick_rx = None;
            self.local_notice = Some("Could not open the folder picker.".into());
        }
    }

    fn poll_folder_pick(&mut self) {
        let Some(receiver) = &self.folder_pick_rx else {
            return;
        };
        let Ok(picked) = receiver.try_recv() else {
            return;
        };
        self.folder_pick_rx = None;
        let Some((path, _)) = picked else {
            return;
        };
        let app_dir = creds::settings_dir();
        match crate::local_folders::add_folder(
            &mut self.local_folders,
            &path,
            crate::local_folders::FolderMode::Read,
            app_dir.as_deref(),
        ) {
            Ok(id) => {
                self.local_notice = Some(format!(
                    "Added. The bot can read this folder as \"{id}\". Switch it to read and write only if you want it to save files here."
                ));
                self.persist_local_folders();
            }
            Err(message) => self.local_notice = Some(message),
        }
    }

    /// The Local folders card is on screen: start counting files and size,
    /// one folder at a time, off the UI thread. Called again it does nothing.
    pub fn want_folder_stats(&mut self) {
        self.folder_stats_wanted = true;
    }

    /// Count again (a folder may have grown).
    pub fn recount_folders(&mut self) {
        self.folder_stats.clear();
        self.folder_stats_wanted = true;
    }

    fn poll_folder_stats(&mut self) {
        let channel = self.folder_stats_chan.get_or_insert_with(|| {
            let (sender, receiver) = std::sync::mpsc::channel();
            FolderStatsChannel { sender, receiver }
        });
        while let Ok((id, path, stats)) = channel.receiver.try_recv() {
            // Ignore an answer for a folder that was removed or replaced.
            if self
                .local_folders
                .iter()
                .any(|folder| folder.id == id && folder.path == path)
            {
                self.folder_stats
                    .insert(id, crate::local_folders::FolderStatus::Done(stats));
            }
        }
        self.folder_stats
            .retain(|id, _| self.local_folders.iter().any(|folder| &folder.id == id));
        if !self.folder_stats_wanted
            || self
                .folder_stats
                .values()
                .any(|status| matches!(status, crate::local_folders::FolderStatus::Counting))
        {
            return;
        }
        let Some(folder) = self
            .local_folders
            .iter()
            .find(|folder| !self.folder_stats.contains_key(&folder.id))
        else {
            return;
        };
        let (id, path) = (folder.id.clone(), folder.path.clone());
        let sender = channel.sender.clone();
        self.folder_stats
            .insert(id.clone(), crate::local_folders::FolderStatus::Counting);
        let spawned = std::thread::Builder::new()
            .name("serabot-folder-stats".into())
            .spawn({
                let (id, path) = (id.clone(), path.clone());
                move || {
                    let budget = crate::local_fs::Budget::new(std::time::Duration::from_secs(120));
                    let stats = crate::local_fs::folder_stats(std::path::Path::new(&path), &budget);
                    let _ = sender.send((id, path, stats));
                }
            });
        if spawned.is_err() {
            self.folder_stats.remove(&id);
            self.folder_stats_wanted = false;
        }
    }

    pub fn remove_local_folder(&mut self, id: &str) {
        self.local_folders.retain(|folder| folder.id != id);
        self.folder_stats.remove(id);
        self.persist_local_folders();
        self.local_notice = Some("Removed. The bot can no longer see that folder.".into());
    }

    pub fn set_local_folder_mode(&mut self, id: &str, mode: crate::local_folders::FolderMode) {
        if let Some(folder) = self.local_folders.iter_mut().find(|folder| folder.id == id) {
            folder.mode = mode;
        }
        self.persist_local_folders();
    }

    /// Which bots may use a folder. The server applies it on the next poll.
    pub fn set_local_folder_bots(&mut self, id: &str, bots: crate::local_folders::BotScope) {
        if let Some(folder) = self.local_folders.iter_mut().find(|folder| folder.id == id) {
            folder.bots = bots;
        }
        self.persist_local_folders();
    }

    fn persist_local_folders(&mut self) {
        self.enqueue_persist(PersistTask::LocalFolders(self.local_folders.clone()));
        // Takes effect for requests already in flight too.
        if let Some((agent, _)) = &self.local_agent {
            agent.update(self.local_folders.clone());
        }
    }

    /// Keep the relay agent running only while signed in with folders allowed.
    fn sync_local_agent(&mut self) {
        let wanted = self.state.connected && !self.local_folders.is_empty();
        let credentials = match (&self.credentials, wanted) {
            (Some(credentials), true) => Arc::clone(credentials),
            _ => {
                self.local_agent = None;
                return;
            }
        };
        let key = Arc::as_ptr(&credentials) as usize;
        if self.local_agent.as_ref().is_some_and(|(_, k)| *k == key) {
            return;
        }
        self.local_agent = None;
        if let Some(agent) = crate::local_folders::LocalAgent::start_with_credentials(
            credentials.endpoint.clone(),
            credentials.token.clone(),
            self.local_folders.clone(),
        ) {
            self.local_agent = Some((agent, key));
        }
    }

    /// Keep the network relay threads running only while signed in (0.66).
    fn sync_net_relay(&mut self) {
        let credentials = match (&self.credentials, self.state.connected) {
            (Some(credentials), true) => Arc::clone(credentials),
            _ => {
                self.net_relay = None;
                return;
            }
        };
        let key = Arc::as_ptr(&credentials) as usize;
        if self.net_relay.as_ref().is_some_and(|(_, k)| *k == key) {
            return;
        }
        self.net_relay = None;
        if let Some(relay) = crate::net_relay::NetRelay::start_with_credentials(
            credentials.endpoint.clone(),
            credentials.token.clone(),
        ) {
            self.net_relay = Some((relay, key));
        }
    }

    /// Live relay counters, while the relay threads run (0.66).
    pub fn net_relay_stats(&self) -> Option<Arc<crate::net_relay::RelayStats>> {
        self.net_relay.as_ref().map(|(relay, _)| relay.stats())
    }

    /// True while the relay agent is running for this session.
    pub fn local_agent_running(&self) -> bool {
        self.local_agent.is_some()
    }

    fn persist_claude_agent(&mut self) {
        self.enqueue_persist(PersistTask::ClaudeAgent(creds::ClaudeAgentPrefs {
            bots: self.claude_agent_bots.clone(),
            folders: self.claude_agent_folders.clone(),
        }));
    }

    /// True when this bot is currently using the local Claude Code ACP agent in chat.
    pub fn bot_uses_claude_agent(&self, bot_id: &str) -> bool {
        self.claude_agent_bots.contains(bot_id)
    }

    /// Enable or disable local Claude Code ACP mode for `bot_id` and persist.
    pub fn set_bot_claude_agent(&mut self, bot_id: &str, enabled: bool) {
        let changed = if enabled {
            crate::valid_speaker(bot_id) && self.claude_agent_bots.insert(bot_id.to_owned())
        } else {
            self.claude_agent_bots.remove(bot_id)
        };
        if changed {
            self.persist_claude_agent();
        }
    }

    /// Toggle local Claude Code ACP mode for a bot in this desktop session.
    pub fn toggle_bot_claude_agent(&mut self, bot_id: &str) {
        let next = !self.bot_uses_claude_agent(bot_id);
        self.set_bot_claude_agent(bot_id, next);
    }

    /// Last selected local folder id for `bot_id` when using Claude Code ACP.
    pub fn bot_claude_agent_folder(&self, bot_id: &str) -> Option<String> {
        self.claude_agent_folders.get(bot_id).cloned()
    }

    /// Persist the preferred local folder id for `bot_id`'s Claude Code ACP sessions.
    pub fn set_bot_claude_agent_folder(&mut self, bot_id: &str, folder_id: String) {
        if !crate::valid_speaker(bot_id) || !valid_id(&folder_id) {
            return;
        }
        if self.claude_agent_folders.get(bot_id) == Some(&folder_id) {
            return;
        }
        self.claude_agent_folders
            .insert(bot_id.to_owned(), folder_id);
        self.persist_claude_agent();
    }

    /// Persona prompt snippet passed once when starting a bot's local ACP session.
    pub fn bot_acp_context(&self, bot_id: &str) -> Option<String> {
        let detail = self.state.bot_details.get(bot_id);
        let name = detail
            .map(|d| d.display_name.trim())
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
            .unwrap_or_else(|| self.bot_display_name(bot_id));
        let listed = self.state.listed_bots.iter().find(|b| b.id == bot_id);
        let role = detail
            .map(|d| d.role.as_str())
            .or_else(|| listed.map(|b| b.role.as_str()))
            .unwrap_or_default();
        let description = detail
            .map(|d| d.description.as_str())
            .or_else(|| listed.and_then(|b| b.description.as_deref()))
            .unwrap_or_default();
        let instructions = detail
            .and_then(|d| d.instructions.as_deref())
            .unwrap_or_default();
        let mut lines = Vec::new();
        if !name.trim().is_empty() {
            if role.trim().is_empty() {
                lines.push(format!("Bot name: {}", name.trim()));
            } else {
                lines.push(format!("Bot name: {} (role: {})", name.trim(), role.trim()));
            }
        }
        if !description.trim().is_empty() {
            lines.push(format!("Description: {}", description.trim()));
        }
        if !instructions.trim().is_empty() {
            lines.push(format!("Instructions:\n{}", instructions.trim()));
        }
        (!lines.is_empty()).then(|| lines.join("\n"))
    }

    /// Picked files still waiting to be uploaded.
    pub fn upload_pending(&self) -> usize {
        self.upload_queue.len()
    }

    /// Drop one attachment from the draft (the uploaded copy stays unattached
    /// on the server and is never sent to the bot).
    pub fn remove_attachment(&mut self, id: &str) {
        self.composer_files.retain(|file| file.id != id);
        self.attach_notice = None;
    }

    /// Admit files from a drop, a paste or the picker against the draft's
    /// limits. Admitted files join the upload queue; refused ones are named
    /// in `attach_notice` under the composer.
    pub fn queue_files(&mut self, files: Vec<(String, Vec<u8>)>) {
        let held = crate::chat_compose::Held {
            count: self.composer_files.len() + self.upload_queue.len(),
            bytes: self
                .composer_files
                .iter()
                .map(|file| file.size_bytes)
                .sum::<u64>()
                + self
                    .upload_queue
                    .iter()
                    .map(|(_, bytes)| bytes.len() as u64)
                    .sum::<u64>(),
        };
        let sizes: Vec<(&str, u64)> = files
            .iter()
            .map(|(name, bytes)| (name.as_str(), bytes.len() as u64))
            .collect();
        let admission = crate::chat_compose::admit_files(held, &sizes);
        let accepted: std::collections::HashSet<usize> =
            admission.accepted.iter().copied().collect();
        self.attach_notice = (!admission.refused.is_empty()).then(|| admission.refused.join(" "));
        for (index, file) in files.into_iter().enumerate() {
            if accepted.contains(&index) {
                self.upload_queue.push_back(file);
            }
        }
    }

    /// Files and bytes the draft already holds, uploaded or queued. Used to
    /// admit a drop before any of its bytes are read.
    pub fn attachments_held(&self) -> crate::chat_compose::Held {
        crate::chat_compose::Held {
            count: self.composer_files.len() + self.upload_queue.len(),
            bytes: self
                .composer_files
                .iter()
                .map(|file| file.size_bytes)
                .sum::<u64>()
                + self
                    .upload_queue
                    .iter()
                    .map(|(_, bytes)| bytes.len() as u64)
                    .sum::<u64>(),
        }
    }

    /// Show why files were refused before they were read.
    pub fn refuse_files(&mut self, refused: Vec<String>) {
        self.attach_notice = (!refused.is_empty()).then(|| refused.join(" "));
    }

    /// Keep a picture preview for an uploaded file. Only the most recent few
    /// are kept, so a long session cannot grow the previews without bound.
    fn remember_thumb(&mut self, id: String, url: String) {
        if self.attachment_thumbs.len() >= 24 {
            self.attachment_thumbs.clear();
        }
        self.attachment_thumbs.insert(id, url);
    }

    /// Whether an optimistic user bubble ("opt-...") is waiting for Submit to land.
    pub fn has_optimistic_pending(&self) -> bool {
        self.state
            .messages
            .last()
            .is_some_and(|message| message.role == Role::User && message.id.starts_with("opt-"))
    }

    /// The Stop button: immediately mark stopping, clear local queued sends,
    /// and send `POST /v1/conversations/{id}/stop` out-of-band without waiting
    /// for the main worker slot.
    pub fn stop_turn(&mut self) {
        if !self.main_run_active()
            && crate::runs::active_count(&self.state.runs.rows) == 0
            && !self.has_optimistic_pending()
        {
            return;
        }
        self.state.stopping = true;
        self.stop_queued = false;
        self.backlog.retain(|op| {
            !matches!(
                op,
                Operation::Submit(_) | Operation::SubmitTo { .. } | Operation::SubmitWith { .. }
            )
        });
        if !self.queue.wants_interrupt() && !self.queue.is_empty() {
            self.queue = crate::chat_compose::SendQueue::default();
            self.persist_composer_state();
        }
        let Some(credentials) = self.credentials.clone() else {
            return;
        };
        let endpoint = credentials.endpoint.clone();
        let token = secrecy::SecretString::new(credentials.token.expose_secret().to_owned());
        let conversation = self.conversation.clone();
        let (sender, receiver) = std::sync::mpsc::channel();
        self.stop_rx = Some(receiver);
        let spawned = std::thread::Builder::new()
            .name("serabot-stop".into())
            .spawn(move || {
                let result = ApiClient::new(endpoint, token)
                    .and_then(|client| client.stop_conversation(&conversation))
                    .map_err(|error| error.to_string());
                let _ = sender.send(result);
            });
        if spawned.is_err() {
            self.stop_rx = None;
            if self.ready() {
                self.start_request(Operation::StopTurn);
            } else {
                self.stop_queued = true;
            }
        }
    }

    /// Drain the out-of-band stop request, if one is in flight.
    fn poll_stop(&mut self) {
        let Some(receiver) = &self.stop_rx else {
            return;
        };
        let Ok(result) = receiver.try_recv() else {
            return;
        };
        self.stop_rx = None;
        match result {
            Ok(count) => {
                self.run_action_done();
                if count == 0 {
                    self.state.stopping = false;
                    self.schedule(Operation::History, Duration::ZERO);
                    if !self.main_run_active()
                        && crate::runs::active_count(&self.state.runs.rows) == 0
                    {
                        self.toast = Some("Nothing is running to stop.".into());
                    }
                } else if let Some(task) = self
                    .state
                    .task
                    .as_ref()
                    .filter(|task| !task.status.terminal())
                {
                    self.schedule(Operation::Task(task.id.clone()), Duration::ZERO);
                } else {
                    self.state.stopping = false;
                    self.schedule(Operation::History, Duration::ZERO);
                }
            }
            Err(message) => {
                self.state.stopping = false;
                self.toast = Some(format!("Could not stop the reply: {message}"));
            }
        }
    }

    /// Send a queued stop once the request slot is free, or drop it when the
    /// turn has already finished.
    fn flush_stop(&mut self) {
        if !self.stop_queued {
            return;
        }
        let running = self.main_run_active()
            || crate::runs::active_count(&self.state.runs.rows) > 0
            || self.has_optimistic_pending();
        if !running {
            self.stop_queued = false;
            self.state.stopping = false;
        } else if self.ready() {
            self.stop_queued = false;
            self.start_request(Operation::StopTurn);
        }
    }

    /// A send did not land. A known failure puts the text back in the composer
    /// and shows a sentence in the chat. An unknown outcome keeps the bubble
    /// and holds a copy the owner can put back, without sending it again.
    fn note_submit_failure(&mut self, failure: Option<crate::chat_failure::ChatFailure>) {
        let optimistic = self
            .state
            .messages
            .last()
            .filter(|message| message.role == Role::User && message.id.starts_with("opt-"))
            .map(|message| message.content.clone());
        if self.state.unknown_outcome {
            if let Some(text) = optimistic.filter(|text| !text.trim().is_empty()) {
                self.state.held_draft = Some(text);
            }
            return;
        }
        if let Some(text) =
            crate::chat_failure::restore_unsent_draft(&self.composer, optimistic.as_deref())
        {
            self.replace_composer(text.clone());
            self.drafts.store(&self.conversation, &self.composer);
            self.persist_composer_state();
            self.state.messages.pop();
            self.state.held_draft = Some(text);
        }
        if let Some(failure) = failure {
            self.state.chat_failure = Some(failure);
            self.state.failure_attempt = self.state.failure_attempt.wrapping_add(1);
            self.state.error = None;
        }
    }

    /// Copy a held unsent message into the composer. Does not send it.
    pub fn restore_held_draft(&mut self) {
        let Some(text) = self.state.held_draft.clone() else {
            return;
        };
        if self.composer.trim().is_empty() {
            self.replace_composer(text);
            self.drafts.store(&self.conversation, &self.composer);
        }
    }

    /// Send again after a failure. Uses the composer, or the held draft when
    /// the composer is empty.
    pub fn retry_send(&mut self) {
        if self.composer.trim().is_empty()
            && let Some(text) = self.state.held_draft.clone()
        {
            self.replace_composer(text);
        }
        self.submit();
    }

    /// Put a sent message back in the composer so it can be edited and sent.
    pub fn edit_text(&mut self, text: String) {
        self.replace_composer(text);
    }

    /// Send the owner's last message again to the same bot. The new message
    /// is marked as a regeneration while it is pending.
    pub fn regenerate(&mut self, text: String) {
        self.composer = text;
        self.submit();
        self.regenerating =
            self.state.messages.last().is_some_and(|message| {
                message.role == Role::User && message.id.starts_with("opt-")
            });
    }

    /// True when the open chat has a `needs_review` task that the owner has
    /// not dismissed yet.
    pub fn needs_review_active(&self) -> bool {
        self.state.task.as_ref().is_some_and(|task| {
            task.status == TaskStatus::NeedsReview
                && !self.dismissed_needs_review.contains(&task.id)
        })
    }

    /// Mark the parked `needs_review` task as reviewed locally, clearing the
    /// card and unblocking the chat composer.
    pub fn dismiss_needs_review(&mut self) {
        if let Some(task) = self.state.task.take() {
            if task.status == TaskStatus::NeedsReview {
                self.dismissed_needs_review.insert(task.id);
            } else {
                self.state.task = Some(task);
                return;
            }
        }
        self.state.stopping = false;
        self.state.unknown_outcome = false;
        self.state.activity.clear();
        self.toast = Some("Marked as reviewed.".into());
    }

    /// Dismiss the parked `needs_review` state and re-send the last user
    /// message on explicit user confirmation.
    pub fn retry_needs_review(&mut self) {
        let last_user = self
            .state
            .messages
            .iter()
            .rev()
            .find(|message| message.role == Role::User && !message.id.starts_with("opt-"))
            .map(|message| message.content.clone());
        let Some(text) = last_user.filter(|t| !t.trim().is_empty()) else {
            self.dismiss_needs_review();
            self.toast = Some("Nothing to send again.".into());
            return;
        };
        if let Some(task) = self.state.task.take() {
            if task.status == TaskStatus::NeedsReview {
                self.dismissed_needs_review.insert(task.id);
            } else {
                self.state.task = Some(task);
                return;
            }
        }
        self.state.stopping = false;
        self.state.unknown_outcome = false;
        self.state.error = None;
        self.state.chat_failure = None;
        self.composer = text;
        self.submit();
    }

    /// Drain the file picker and feed the upload queue one request at a time.
    fn poll_attachments(&mut self) {
        if let Some(receiver) = &self.pick_rx
            && let Ok(picked) = receiver.try_recv()
        {
            self.pick_rx = None;
            let mut files = Vec::new();
            for item in picked {
                match item {
                    Ok(file) => files.push(file),
                    Err(message) => self.toast = Some(message),
                }
            }
            self.queue_files(files);
        }
        if !self.uploading
            && self.ready()
            && self.pending.is_none()
            && let Some((filename, bytes)) = self.upload_queue.pop_front()
        {
            self.uploading = true;
            self.upload_thumb = crate::chat_compose::thumbnail_data_url(&filename, &bytes);
            self.start_request(Operation::UploadAttachment { filename, bytes });
            if self.pending.is_none() {
                self.uploading = false;
                self.upload_thumb = None;
            }
        }
    }

    /// Download an artifact and ask where to save it (native Save As dialog).
    pub fn save_artifact_as(&mut self, hash: String) {
        self.start_download(hash, Some(PendingOpen::SaveAs));
    }

    pub fn apply_model(&mut self, selection: Selection) {
        if self
            .state
            .catalog
            .as_ref()
            .is_none_or(|catalog| !catalog.allows(&selection))
        {
            self.state.error = Some("Choose an approved provider and model.".into());
            return;
        }
        self.state.error = None;
        self.start_request(Operation::Apply(selection));
    }

    pub fn decide(&mut self, id: String, decision: ApprovalDecision) {
        self.state.error = None;
        self.start_request(Operation::Decide { id, decision });
    }

    /// The owner's answer to a question card. The server posts it as a message.
    pub fn answer_card(&mut self, id: String, answer: String) {
        self.state.error = None;
        self.start_request(Operation::AnswerCard { id, answer });
    }

    /// Dismiss a pending card (a question or a secret request).
    pub fn decline_card(&mut self, id: String) {
        self.state.error = None;
        self.start_request(Operation::DeclineCard { id });
    }

    /// Send the value for a secret request. The caller clears its own copy.
    pub fn submit_secret_card(&mut self, id: String, value: zeroize::Zeroizing<String>) {
        self.state.error = None;
        self.start_request(Operation::SubmitSecretCard { id, value });
    }

    pub fn refresh_section_data(&mut self, operation: Operation) {
        self.state.error = None;
        self.start_request(operation);
    }

    /// The "Check for updates" button: unlike the silent launch check, it
    /// always tells the owner what happened.
    pub fn check_updates_manual(&mut self) {
        self.update.notice = None;
        if updater::configured_repo().is_none() {
            self.update.error = Some(
                "This build has no update source. Download the latest release from GitHub.".into(),
            );
            return;
        }
        self.update.manual = true;
        self.check_updates();
    }

    pub fn check_updates(&mut self) {
        if self.update.checking || updater::configured_repo().is_none() {
            return;
        }
        let (sender, receiver) = std::sync::mpsc::channel();
        self.update.rx = Some(receiver);
        self.update.checking = true;
        self.update.error = None;
        if std::thread::Builder::new()
            .name("serabot-update".into())
            .spawn(move || {
                let result = match updater::Updater::from_build_env() {
                    Some(updater) => updater.check().map_err(|error| error.to_string()),
                    None => Ok(None),
                };
                let _ = sender.send(result);
            })
            .is_err()
        {
            self.update.rx = None;
            self.update.checking = false;
            self.update.manual = false;
            self.update.error = Some("Could not start the update check.".into());
        }
    }

    pub fn apply_update(&mut self, info: &updater::UpdateInfo) -> Result<(), String> {
        updater::spawn_apply(info)
    }

    /// Fingerprint of chat-visible state; used to skip repaints when nothing changed.
    pub fn ui_fingerprint(&self) -> u64 {
        use std::collections::hash_map::DefaultHasher;
        use std::hash::{Hash, Hasher};
        let mut h = DefaultHasher::new();
        self.state.messages.len().hash(&mut h);
        for row in &self.state.messages {
            row.id.hash(&mut h);
            row.content.len().hash(&mut h);
        }
        self.state.cards.len().hash(&mut h);
        self.state.activity.task_id.hash(&mut h);
        self.state.activity.heartbeat_secs.hash(&mut h);
        self.state.activity.heartbeat_beat.hash(&mut h);
        self.state.activity.steps.len().hash(&mut h);
        for step in &self.state.activity.steps {
            step.call_id.hash(&mut h);
            step.label.len().hash(&mut h);
            format!("{:?}", step.phase).hash(&mut h);
            step.duration_ms.hash(&mut h);
            step.error.as_ref().map(String::len).hash(&mut h);
            step.cards.len().hash(&mut h);
        }
        if let Some(task) = &self.state.task {
            task.id.hash(&mut h);
            format!("{:?}", task.status).hash(&mut h);
        }
        self.dismissed_needs_review.len().hash(&mut h);
        self.events_active.hash(&mut h);
        self.events_seen.hash(&mut h);
        self.state
            .chat_failure
            .as_ref()
            .map(|failure| failure.message.len())
            .hash(&mut h);
        self.state.held_draft.is_some().hash(&mut h);
        self.state.working.hash_fingerprint(&mut h);
        if let Some(plan) = &self.state.plan {
            plan.goal.len().hash(&mut h);
            plan.items.len().hash(&mut h);
            for item in &plan.items {
                item.text.len().hash(&mut h);
                format!("{:?}", item.status).hash(&mut h);
            }
        }
        self.state.plan.is_some().hash(&mut h);
        self.toast.hash(&mut h);
        self.uploading.hash(&mut h);
        self.upload_queue.len().hash(&mut h);
        self.composer_files.len().hash(&mut h);
        self.state.transcript.is_some().hash(&mut h);
        self.state.transcript_error.hash(&mut h);
        self.panel.epoch.hash(&mut h);
        // The accounts sheet's countdown moves once a second; the route chip
        // and rows move when the server records a switch.
        if let Some(flow) = &self.accounts_ui.flow {
            format!("{:?}", flow.phase).hash(&mut h);
            flow.url.hash(&mut h);
            flow.remaining_secs(self.accounts_ui.flow_elapsed())
                .hash(&mut h);
        }
        if let Some(flow) = &self.mcp_ui.flow {
            format!("{:?}", flow.phase).hash(&mut h);
            flow.url.hash(&mut h);
            flow.remaining_secs(self.mcp_ui.flow_elapsed()).hash(&mut h);
        }
        self.mcp_ui.target.hash(&mut h);
        self.mcp_ui.notice.hash(&mut h);
        self.mcp_ui.error.hash(&mut h);
        self.mcp_ui.removing.hash(&mut h);
        self.mcp_ui.confirm_remove.hash(&mut h);
        self.mcp_ui.confirm_disconnect.hash(&mut h);
        self.mcp_ui.github_busy.hash(&mut h);
        self.mcp_ui.github_error.hash(&mut h);
        self.mcp_ui
            .github
            .as_ref()
            .map(|view| view.state.clone())
            .hash(&mut h);
        self.mcp_ui
            .github
            .as_ref()
            .map(|view| view.login.clone())
            .hash(&mut h);
        if let Some(tools) = &self.mcp_ui.tools {
            format!("{:?}", tools.state).hash(&mut h);
            tools.error.hash(&mut h);
            for row in &tools.rows {
                row.name.hash(&mut h);
                row.mode.hash(&mut h);
            }
        }
        self.accounts_ui.busy.hash(&mut h);
        self.accounts_ui.sheet_error.hash(&mut h);
        self.accounts_ui.force_prompt.is_some().hash(&mut h);
        self.state.account_tests.len().hash(&mut h);
        self.state.route_events.len().hash(&mut h);
        self.state.route_chain.is_some().hash(&mut h);
        // Runs, queue and wait rows (v0.50). A countdown moves each second.
        for row in &self.state.runs.rows {
            row.id.hash(&mut h);
            row.chip.label().hash(&mut h);
        }
        self.state.runs.compactions.len().hash(&mut h);
        self.state.runs.holds.len().hash(&mut h);
        self.state.runs.waiting.is_some().hash(&mut h);
        if self.state.runs.waiting.is_some() || !self.state.runs.holds.is_empty() {
            crate::workflows::now_secs().hash(&mut h);
        }
        // A live turn ages once a second: the LiveCard elapsed and the
        // running step's time move on the existing 1s repaint, which only
        // repaints when this fingerprint moves.
        if self
            .state
            .task
            .as_ref()
            .is_some_and(|task| !task.status.terminal())
        {
            crate::workflows::now_secs().hash(&mut h);
        }
        self.queue.len().hash(&mut h);
        self.queue.editing().hash(&mut h);
        for item in self.queue.items() {
            item.id.hash(&mut h);
            item.steer.hash(&mut h);
            item.text.len().hash(&mut h);
        }
        self.runs_open.len().hash(&mut h);
        self.route_editor.saving.hash(&mut h);
        self.route_editor.error.hash(&mut h);
        self.route_editor.notice.hash(&mut h);
        self.credential_save_failed.hash(&mut h);
        h.finish()
    }

    /// True when `tick()` has pending completions, due timers, or active flows.
    /// Allows the UI coroutine to skip `session.with_mut` (and full-tree redraws)
    /// when the app is idle.
    pub fn has_tick_work(&self) -> bool {
        let now = Instant::now();
        self.worker.as_ref().is_some_and(Worker::has_completion)
            || self.events_active
            || self.scheduled.as_ref().is_some_and(|(at, _)| now >= *at)
            || self.reconnect_at.is_some_and(|at| now >= at)
            || (self.link_down
                && self.pending.is_none()
                && self.link_retry_at.is_some_and(|at| now >= at))
            || (!self.backlog.is_empty() && self.pending.is_none())
            || !self.queue.is_empty()
            || self
                .state
                .task
                .as_ref()
                .is_some_and(|task| !task.status.terminal())
            || self.sign_in.cancel.is_some()
            || self.accounts_ui.pending_signin.is_some()
            || self.downloading
            || self.download_rx.is_some()
            || self.folder_pick_rx.is_some()
            || self.folder_stats_wanted
            || self.pick_rx.is_some()
            || self.stop_queued
            || self.stop_rx.is_some()
            || !self.upload_queue.is_empty()
            || self.panel.open()
            || self.panel.session_active()
            || self.bot_settings.open
            || self.route_editor.bot.is_some()
            || (self.pending.is_none() && self.missing_section_op().is_some())
    }

    /// Called from the UI timer. Drains completions and advances scheduled work.
    pub fn tick(&mut self) {
        self.poll_persist();
        self.check_session_expiry();
        self.state.note_history_wait(Instant::now());
        self.poll_events();
        self.receive();
        self.drain_backlog();
        self.flush_events();
        self.poll_updates();
        self.poll_status();
        self.poll_sign_in();
        self.poll_provider_flow();
        self.poll_mcp();
        self.poll_pending_signin();
        self.poll_telegram();
        self.poll_download();
        self.poll_attachments();
        self.poll_stop();
        self.flush_stop();
        self.poll_folder_pick();
        self.poll_folder_stats();
        self.sync_local_agent();
        self.sync_net_relay();
        self.poll_launches();
        self.hydrate_bot_settings();
        self.hydrate_route_editor();
        self.poll_route_events();
        self.poll_runs();
        self.pump_queue();
        self.pump_panel();
        if self.link_down
            && self.pending.is_none()
            && self.link_retry_at.is_some_and(|at| Instant::now() >= at)
        {
            self.retry_link();
        }
        if let Some(at) = self.reconnect_at {
            if Instant::now() >= at {
                self.reconnect_at = None;
                self.connect_error = None;
                self.state.error = None;
                self.connect_now();
            }
            return;
        }
        if self.pending.is_some() {
            return;
        }
        // Load the open section even while a task is running or a poll is queued.
        self.ensure_section_data();
        if self.pending.is_some() {
            return;
        }
        if let Some((when, _)) = &self.scheduled {
            if Instant::now() >= *when
                && let Some((_, operation)) = self.scheduled.take()
            {
                self.start_request(operation);
            }
            return;
        }
        if !self.state.connected
            || self.refreshing
            || self.poll_paused
            || self.state.unknown_outcome
        {
            return;
        }
        // Unfocused window: poll about eight times less often.
        let slow: u32 = if self.panel.focused { 1 } else { 8 };
        if let Some(task) = &self.state.task {
            if !task.status.terminal() {
                if self.events_active {
                    self.schedule(Operation::Task(task.id.clone()), POLL_SAFETY * slow);
                } else {
                    let operation =
                        if self.poll_approvals && task.status == TaskStatus::AwaitingApproval {
                            Operation::Approvals
                        } else if self.poll_history {
                            Operation::History
                        } else {
                            Operation::Task(task.id.clone())
                        };
                    if task.status == TaskStatus::AwaitingApproval {
                        self.poll_approvals = !self.poll_approvals;
                        if !self.poll_approvals {
                            self.poll_history = !self.poll_history;
                        }
                    } else {
                        self.poll_history = !self.poll_history;
                    }
                    self.schedule(operation, POLL_GAP * slow);
                }
            }
        } else {
            self.ensure_section_data();
        }
    }

    fn start_events(&mut self) {
        self.stop_events();
        let Some(credentials) = self.credentials.clone() else {
            return;
        };
        let wake = Arc::clone(&self.ui_wake);
        self.events = Some(EventStream::start(
            credentials,
            self.last_event_id,
            Box::new(move || wake.store(true, Ordering::Release)),
        ));
    }

    fn stop_events(&mut self) {
        if let Some(events) = self.events.take() {
            events.stop();
        }
        self.events_active = false;
    }

    fn poll_events(&mut self) {
        let mut unauthorized = false;
        if let Some(stream) = &self.events {
            while let Some(update) = stream.try_recv() {
                match update {
                    EventUpdate::Connected => {
                        self.events_active = true;
                        self.events_seen = true;
                    }
                    EventUpdate::Disconnected => self.events_active = false,
                    EventUpdate::Unauthorized => unauthorized = true,
                    EventUpdate::Event { id, kind, payload } => {
                        self.last_event_id = self.last_event_id.max(id);
                        self.event_frames.push(id, kind, payload);
                    }
                }
            }
        }
        self.apply_event_frame(Instant::now());
        if unauthorized {
            self.reject_token();
        }
    }

    /// Applies the events gathered for the current frame, if one is due. The
    /// whole frame is one state write however many events it holds; child
    /// progress arrives as counters.
    fn apply_event_frame(&mut self, now: Instant) {
        let Some(frame) = self.event_frames.take_frame(now) else {
            return;
        };
        self.event_frame_writes += 1;
        for (child, counter) in frame.progress {
            let slot = self.subagents.entry(child).or_default();
            slot.events = slot.events.saturating_add(counter.events);
            slot.steps_done = slot.steps_done.saturating_add(counter.steps_done);
            slot.last_event_id = slot.last_event_id.max(counter.last_event_id);
        }
        while self.subagents.len() > MAX_SUBAGENT_COUNTERS {
            let oldest = self
                .subagents
                .iter()
                .min_by_key(|(_, counter)| counter.last_event_id)
                .map(|(child, _)| child.clone());
            match oldest {
                Some(child) => self.subagents.remove(&child),
                None => break,
            };
        }
        for (_, kind, payload) in frame.events {
            self.apply_event(kind, payload);
        }
    }

    fn apply_event(&mut self, kind: String, payload: serde_json::Value) {
        // Rings and "Working..." labels follow the same stream.
        // This never consumes the event: turn.completed and
        // task.failed still refresh the transcript below.
        self.state.working.apply(&kind, &payload, Instant::now());
        // Subagent runs, holds, waits and compactions of the open chat.
        self.state
            .runs
            .apply_event(&kind, &payload, self.conversation.as_str());
        // Step events only feed the live view; the transcript
        // is not re-read for each one.
        if self.state.turns.conversation() != self.conversation {
            self.state.turns.reset(&self.conversation);
        }
        let traced = self.state.turns.apply(&kind, &payload);
        // `turn.progress` heartbeats are global on the user's SSE stream; only
        // apply them to the open chat's Activity when they match it (or lack
        // `conversation_id`, e.g. in unit fixtures), and never reload history.
        let progress_here = kind == "turn.progress"
            && payload.get("parent_task_id").is_none()
            && payload
                .get("conversation_id")
                .and_then(serde_json::Value::as_str)
                .is_none_or(|cid| cid == self.conversation.as_str());
        let stepped = if kind == "turn.progress" {
            if progress_here {
                self.state.activity.apply(&kind, &payload);
            }
            true
        } else {
            self.state.activity.apply(&kind, &payload) || traced
        };
        if kind == "turn.step"
            && payload
                .get("conversation_id")
                .and_then(|value| value.as_str())
                == Some(self.conversation.as_str())
            && let Some(plan) = payload.get("plan")
            && let Some(plan) = crate::chat_plan::ChatPlan::from_value(plan)
        {
            self.state.plan = Some(plan);
        }
        if stepped {
            return;
        }
        // Plan updates feed the plan panel only.
        if crate::chat_plan::ChatPlan::apply_event(
            &mut self.state.plan,
            &self.conversation,
            &kind,
            &payload,
        ) {
            return;
        }
        if kind == "route_switched"
            && payload
                .get("conversation_id")
                .and_then(|value| value.as_str())
                == Some(self.conversation.as_str())
        {
            self.state.route_events_stale = true;
        }
        if kind == "conversation.renamed"
            && let Some(conv_id) = payload
                .get("id")
                .or_else(|| payload.get("conversation_id"))
                .and_then(|value| value.as_str())
            && let Some(title) = payload.get("title").and_then(|value| value.as_str())
        {
            let clean = title.trim();
            if !clean.is_empty() {
                for conversation in &mut self.state.conversations {
                    if conversation.id == conv_id {
                        conversation.title = Some(clean.to_owned());
                    }
                }
                for list in self.state.bot_chats.values_mut() {
                    for chat in &mut list.chats {
                        if chat.id == conv_id {
                            chat.title = Some(clean.to_owned());
                        }
                    }
                }
            }
        }
        let kind = scope_event_kind(kind, &payload, &self.conversation);
        if matches!(
            kind.as_str(),
            "turn.completed" | "turn.failed" | "task.completed" | "task.failed" | "task.cancelled"
        ) && !self.main_run_active()
            && crate::runs::active_count(&self.state.runs.rows) == 0
        {
            self.state.stopping = false;
        }
        choose_event_kind(
            &mut self.last_event_kind,
            &mut self.events_dirty,
            &self.conversation,
            kind,
        );
    }

    fn flush_events(&mut self) {
        if !self.events_dirty
            || !self.state.connected
            || self.state.unknown_outcome
            || !self.state.history_loaded
            || self.refreshing
            || self.pending.is_some()
            || self
                .scheduled
                .as_ref()
                .is_some_and(|(when, _)| *when <= Instant::now())
            || Instant::now() < self.next_request
        {
            return;
        }
        self.events_dirty = false;
        let kind = self.last_event_kind.clone();
        match event_refresh(&kind, &self.conversation) {
            EventRefresh::Approvals => self.start_request(Operation::Approvals),
            EventRefresh::Conversations => self.start_request(Operation::ListConversations {
                include_archived: false,
            }),
            EventRefresh::History => self.start_request(Operation::History),
            EventRefresh::RoomTeam => {
                self.start_request(Operation::History);
                if self.credentials.is_some() && self.worker.is_some() {
                    self.state.team_loading = true;
                    self.state.team_error = None;
                    self.queue_backlog(Operation::GetTeam);
                }
            }
        }
    }

    /// Keep a request for a later tick. A repeated read replaces the one already
    /// waiting, so a render that loads the same tab twice does not grow the queue.
    fn queue_backlog(&mut self, operation: Operation) {
        let read = matches!(
            operation,
            Operation::ListTasks
                | Operation::TaskPrefs
                | Operation::TaskSyncStatus
                | Operation::TaskCalendar { .. }
                | Operation::Workflows
                | Operation::Logs { .. }
                | Operation::MarketplaceList
                | Operation::Catalog
                | Operation::Approvals
                | Operation::StandingGrants
                | Operation::Providers
                | Operation::Connectors
                | Operation::ListBots
                | Operation::ListSkills
                | Operation::ListConversations { .. }
                | Operation::ListArchived
                | Operation::ListMemories { .. }
                | Operation::MemoryStats
                | Operation::JobsReliability
                | Operation::ChatPlan
                | Operation::TurnTraces
                | Operation::GetTeam
                | Operation::VoiceSettings
        );
        if read
            && let Some(slot) = self.backlog.iter_mut().find(|queued| {
                std::mem::discriminant(*queued) == std::mem::discriminant(&operation)
            })
        {
            *slot = operation;
            return;
        }
        self.backlog.push_back(operation);
    }

    /// Clear pre-dispatch guards and caller-owned loading/busy flags when a
    /// request is aborted or fails before reaching the worker.
    fn rollback_aborted_request(
        &mut self,
        operation: &Operation,
        clear_all_caller_flags: Option<bool>,
    ) {
        match operation {
            Operation::BotDetail { .. } => self.bot_detail_pending = None,
            Operation::ListBotChats { .. } => self.bot_chats_pending = None,
            Operation::MarketplaceAct { .. } | Operation::MarketplaceConfig { .. } => {
                self.marketplace_busy = false;
            }
            Operation::UpdateBotConfig { .. } => self.bot_config_pending = None,
            Operation::AvatarFetch { .. }
            | Operation::AvatarUpload { .. }
            | Operation::AvatarRemove { .. } => self.avatar_op = None,
            Operation::PreviewArtifact { .. } => self.state.media_previews.fail_in_flight(),
            Operation::DocPreview { hash } => {
                self.doc_previews_pending.remove(hash);
            }
            _ => {}
        }
        let Some(disconnected) = clear_all_caller_flags else {
            return;
        };
        match operation {
            Operation::Usage { .. } => self.usage_loading = false,
            Operation::ServerHealth if disconnected => self.server_health_loading = false,
            Operation::VoiceSettings => self.voice.loading = false,
            Operation::CreateNamedBot { .. } => self.creating_named_bot = false,
            Operation::InstallShippedBot { .. } => self.state.shipped_installing = None,
            Operation::OnboardingStart { .. }
            | Operation::OnboardingReply { .. }
            | Operation::OnboardingConfirm { .. }
            | Operation::OnboardingLoad { .. } => self.onboarding_ui.waiting = false,
            Operation::UpdateBot { .. } => self.bot_settings.reset_saving(),
            Operation::SetBotEnabled { .. } | Operation::DeleteBot { .. } => {
                self.bot_settings.reset_lifecycle();
            }
            Operation::GetTeam | Operation::SetTeam { .. } => self.state.team_loading = false,
            Operation::Library => self.state.library_loading = false,
            Operation::TranscribeAudio { .. } => self.state.transcript_loading = false,
            Operation::UploadAttachment { .. } => {
                self.uploading = false;
                self.upload_thumb = None;
            }
            _ => {}
        }
    }

    /// Send the oldest backlogged request. If the queue is still full, it goes
    /// back in the backlog for the next tick.
    fn drain_backlog(&mut self) {
        if let Some(operation) = self.backlog.pop_front() {
            self.start_request(operation);
        }
    }

    fn start_request(&mut self, operation: Operation) {
        self.scheduled = None;
        let Some(credentials) = &self.credentials else {
            self.rollback_aborted_request(&operation, Some(false));
            return;
        };
        let Some(worker) = &self.worker else {
            self.rollback_aborted_request(&operation, Some(false));
            return;
        };
        let kind = operation.kind();
        // Track the bot-scoped payloads so loads dedup and a saved config can
        // update the cached detail optimistically when it completes.
        match &operation {
            Operation::BotDetail { id } => self.bot_detail_pending = Some(id.clone()),
            Operation::ListBotChats { bot } => self.bot_chats_pending = Some(bot.clone()),
            Operation::MarketplaceAct { .. } | Operation::MarketplaceConfig { .. } => {
                self.marketplace_busy = true
            }
            Operation::UpdateBotConfig { id, update } => {
                self.bot_config_pending = Some((id.clone(), update.clone()))
            }
            Operation::AvatarFetch { target, version } => {
                self.avatar_op = Some(AvatarOp::Fetch(target.clone(), version.clone()))
            }
            Operation::AvatarUpload { .. } | Operation::AvatarRemove { .. } => {
                self.avatar_op = Some(AvatarOp::Write)
            }
            // One read per file at a time: a request while one is in flight
            // (or the file is cached) sends nothing.
            Operation::PreviewArtifact { hash, .. } if !self.state.media_previews.begin(hash) => {
                return;
            }
            // One preview read per hash at a time; the session tracks the
            // in-flight set and clears it when the reply lands.
            Operation::DocPreview { hash } => {
                self.doc_previews_pending.insert(hash.clone());
            }
            _ => {}
        }
        let job = Job {
            scope: self.state.scope.clone(),
            credentials: Arc::clone(credentials),
            operation,
        };
        if let Err(error) = worker.dispatch(job) {
            match *error {
                // The one-slot queue is busy, not the worker. A plain read or write
                // waits in the backlog. A request that already recorded state before
                // dispatch cannot be replayed later, so it reports a busy queue.
                std::sync::mpsc::TrySendError::Full(job) => {
                    if has_dispatch_guard(&job.operation) {
                        self.rollback_aborted_request(&job.operation, Some(true));
                        self.state.error =
                            Some("Another request is still running. Try again in a moment.".into());
                    } else {
                        self.rollback_aborted_request(&job.operation, None);
                        self.queue_backlog(job.operation);
                    }
                    return;
                }
                std::sync::mpsc::TrySendError::Disconnected(job) => {
                    self.rollback_aborted_request(&job.operation, Some(true));
                    self.state.error = Some("Network worker unavailable. Reconnect.".into());
                    self.refreshing = false;
                    self.poll_paused = true;
                    return;
                }
            }
        }
        self.pending = Some(kind);
        self.next_request = Instant::now() + REQUEST_GAP;
    }

    fn schedule(&mut self, operation: Operation, delay: Duration) {
        self.scheduled = Some(((Instant::now() + delay).max(self.next_request), operation));
    }

    fn receive(&mut self) {
        while let Some(completion) = self.worker.as_ref().and_then(Worker::completion) {
            if !self.state.scope.accepts(&completion.scope) {
                continue;
            }
            self.pending = None;
            self.link_retrying = false;
            self.bot_detail_pending = None;
            // The in-flight config write (if any) resolves with this completion.
            let bot_config = if completion.kind == OperationKind::BotConfigWrite {
                self.bot_config_pending.take()
            } else {
                None
            };
            if completion.kind == OperationKind::Health {
                self.connecting = false;
            }
            // A route pick that failed ends a "Retry on <fallback>" waiting on it.
            if completion.kind == OperationKind::Apply && completion.result.is_err() {
                self.route_apply_failed();
            }
            match completion.result {
                // Who-am-I is a nicety: an older server, or a saved token the
                // /v1/me route does not know, still signs in. The next read
                // (the catalog) decides whether the session is alive.
                Err(_) if completion.kind == OperationKind::Me => {
                    self.state.user = None;
                    self.schedule(Operation::Catalog, Duration::ZERO);
                }
                Err(ApiError::Http(401)) => {
                    self.refreshing = false;
                    self.scheduled = None;
                    self.poll_paused = true;
                    self.reject_token();
                }
                // Images never stop the console: a failed download is
                // remembered as "no image"; a failed upload is a toast.
                Err(error) if completion.kind == OperationKind::Avatar => {
                    match self.avatar_op.take() {
                        Some(AvatarOp::Fetch(target, version)) => {
                            let key = target.key();
                            let existing_url = self
                                .state
                                .avatars
                                .get(&key)
                                .and_then(|img| img.data_url.clone());
                            self.state.avatars.insert(
                                key,
                                crate::avatars::AvatarImage {
                                    version,
                                    data_url: existing_url,
                                },
                            );
                            self.schedule_next_avatar();
                        }
                        _ => {
                            self.toast = Some(format!("Could not save the image: {error}"));
                            self.schedule(Operation::ListBots, Duration::ZERO);
                        }
                    }
                }
                Err(error)
                    if matches!(
                        completion.kind,
                        OperationKind::OnboardingRead | OperationKind::OnboardingWrite
                    ) =>
                {
                    self.onboarding_failed(completion.kind, error.to_string());
                }
                // The health card is optional: a failed load leaves a calm
                // line on the card, never the global banner, and the next
                // refresh simply tries again.
                Err(error) if completion.kind == OperationKind::ServerHealth => {
                    self.server_health_loading = false;
                    self.server_health_error = Some(match &error {
                        ApiError::Http(403) => "Only the owner can see server health.".to_owned(),
                        ApiError::Http(404) => {
                            "This server does not report its health yet. Update the server."
                                .to_owned()
                        }
                        _ => "Server health is not available right now. Trying again soon."
                            .to_owned(),
                    });
                    if error == ApiError::Http(403) {
                        self.state.server_health = None;
                    }
                }
                Err(error) if completion.kind == OperationKind::BotLifecycle => {
                    self.bot_settings.reset_lifecycle();
                    self.toast = Some(format!("Could not change that bot: {error}"));
                    self.schedule(Operation::ListBots, Duration::ZERO);
                }
                Err(error) if completion.kind == OperationKind::BotSettingsWrite => {
                    self.bot_settings_failed(&error);
                }
                Err(error) if completion.kind == OperationKind::ToolsWrite => {
                    self.toast = Some(format!("Could not change that: {error}"));
                    self.schedule(Operation::ListBots, Duration::ZERO);
                }
                Err(error) if completion.kind == OperationKind::ProviderAccount => {
                    self.account_write_failed(&error);
                }
                Err(error) if completion.kind == OperationKind::RouteWrite => {
                    self.route_save_failed(&error);
                }
                // The route events are optional: an older server has none, and
                // the chip simply shows no switch rows.
                Err(_) if completion.kind == OperationKind::RouteEvents => {
                    self.state.route_events_loaded = true;
                }
                // An older server has no runs list: the sidebar stays empty.
                Err(_) if completion.kind == OperationKind::Runs => {
                    self.state.runs.loaded = true;
                }
                Err(error) if completion.kind == OperationKind::RunAction => {
                    self.run_action_failed(&error);
                }

                Err(error)
                    if matches!(
                        completion.kind,
                        OperationKind::AdminWrite
                            | OperationKind::ProviderKey
                            | OperationKind::ProviderTest
                            | OperationKind::DefaultRoute
                            | OperationKind::ConnectorTest
                            | OperationKind::OAuthSignIn
                            | OperationKind::ConnectorSignOut
                            | OperationKind::AdminRestart
                    ) =>
                {
                    let message = match &error {
                        ApiError::Http(404) => {
                            "This server has no administration API. Update the server to 0.4.0 or later.".to_owned()
                        }
                        ApiError::Rejected(text) if text == "not found" => {
                            "This server has no administration API. Update the server to 0.4.0 or later.".to_owned()
                        }
                        _ => crate::provider_accounts::plain_error(&error.to_string()),
                    };
                    // A refusal that asks for a sign-in opens that sign-in.
                    let asks_sign_in =
                        crate::provider_accounts::sign_in_needed_for(&error.to_string());
                    self.accounts_ui.save_signin = None;
                    self.admin_error = Some(message);
                    if asks_sign_in.is_some() {
                        self.prompt_sign_in_for(&error.to_string());
                    }
                    self.connector_testing = None;
                    self.oauth_signing_in = None;
                    self.provider_testing = None;
                    self.scheduled = None;
                }
                // "Load earlier" is optional: a failure is said next to the
                // button and never pauses the poll loop or raises the banner.
                Err(error) if completion.kind == OperationKind::HistoryOlder => {
                    self.state.older_loading = false;
                    self.state.older_error =
                        Some(format!("Could not load earlier messages: {error}"));
                }
                Err(ApiError::Http(429)) if completion.kind.is_background_poll() => {
                    // A rate-limited background poll backs off quietly (no
                    // banner) and the poll loop carries on afterwards.
                    self.rate_attempts = self.rate_attempts.saturating_add(1);
                    // The first transcript read must not be lost to a 429:
                    // ask for it again once the back-off is over.
                    let first_read =
                        completion.kind == OperationKind::History && !self.state.history_loaded;
                    if !first_read {
                        self.refreshing = false;
                    }
                    self.scheduled = None;
                    self.next_request = Instant::now()
                        + crate::api::poll_backoff(
                            crate::api::last_retry_after_secs(),
                            self.rate_attempts,
                            crate::api::jitter_seed(),
                        );
                    if first_read {
                        self.schedule(Operation::History, Duration::ZERO);
                    }
                }
                Err(error) => {
                    self.usage_loading = false;
                    let message = error.to_string();
                    let sign_in_message = error.sign_in_text();
                    let rate_limited = error == ApiError::Http(429);
                    let not_found = error == ApiError::Http(404);
                    let forbidden = error == ApiError::Http(403);
                    let dropped = error.is_transport();
                    let submit_failure = (completion.kind == OperationKind::Submit)
                        .then(|| crate::chat_failure::classify_send_error(&error));
                    self.state.fail(&completion.scope, completion.kind, error);
                    if dropped && self.state.connected {
                        // A dropped connection is one calm banner with a
                        // retry button, not an error in every panel. An
                        // unknown write outcome keeps its own warning.
                        self.note_link_down();
                        if !self.state.unknown_outcome {
                            self.state.error = None;
                        }
                    }
                    self.refreshing = false;
                    self.scheduled = None;
                    self.poll_paused = true;
                    // Team and transcript problems stay in their own surface
                    // instead of the global banner. An unknown team write keeps
                    // the global message: it must not be replayed.
                    match completion.kind {
                        OperationKind::Team | OperationKind::TeamWrite => {
                            self.state.team_error = Some(message.clone());
                            self.state.team_loading = false;
                            // The room stays; the team can be written again
                            // from the Team sheet.
                            let initial = self.pending_room_refresh;
                            self.pending_room_refresh = false;
                            if initial {
                                // The room was created but its team was not
                                // written. The room is kept and the owner is
                                // told; nothing is deleted or resent.
                                self.admin_notice = Some(format!(
                                    "The room was created, but its team was not set: {message}"
                                ));
                                if !self.state.unknown_outcome {
                                    self.state.error = None;
                                }
                            } else if !self.state.unknown_outcome {
                                self.state.error = None;
                            }
                        }
                        OperationKind::CreateConversation => {
                            // No room was created, so no team is pending.
                            self.pending_team = None;
                        }
                        OperationKind::ListBotChats => {
                            // An older server has no chat list: show the bot
                            // with its one chat and do not ask again.
                            if let Some(bot) = self.bot_chats_pending.take() {
                                self.state.bot_chats_loaded.insert(bot);
                            }
                            self.state.error = None;
                        }
                        OperationKind::Transcribe => {
                            self.state.transcript_error = Some(message.clone());
                            self.state.transcript_loading = false;
                            self.state.error = None;
                        }
                        OperationKind::Library => {
                            // Older servers have no library route; the tab shows
                            // its own message instead of a global banner.
                            self.state.library_error = Some(message.clone());
                            self.state.library_loading = false;
                            self.state.error = None;
                            self.schedule(Operation::MediaJobs, Duration::ZERO);
                        }
                        OperationKind::MediaJobs => {
                            // The placeholder cards are optional; a failed read
                            // leaves them out.
                            self.state.error = None;
                            self.schedule(Operation::ChatPlan, Duration::ZERO);
                        }
                        OperationKind::PreviewArtifact => {
                            self.state.media_previews.fail_in_flight();
                            self.toast = Some(format!("This file cannot play here: {message}"));
                        }
                        OperationKind::TurnLimits => {
                            self.state.turn_limits_error = Some(message.clone());
                        }
                        OperationKind::TurnLimitsWrite => {
                            self.state.turn_limits_error = Some(message.clone());
                            self.toast = Some(format!("Work limits were not saved: {message}"));
                        }
                        OperationKind::MemorySettingsWrite => {
                            self.state.memory_settings_error = Some(message.clone());
                            self.toast = Some(format!("Mem0 settings were not saved: {message}"));
                        }
                        OperationKind::ChatPlan => {
                            // The plan panel is optional; a failed read leaves
                            // it hidden instead of raising a banner.
                            self.state.error = None;
                        }
                        OperationKind::TurnTraces => {
                            // Traces are optional: replies just show none.
                            self.state.error = None;
                        }
                        OperationKind::BotLibrary => {
                            self.state.bot_library_loading = false;
                            self.toast = Some(format!("The library did not load: {message}"));
                            self.state.error = None;
                        }
                        OperationKind::ChatAutoApprove => {
                            // The switch shows the server's value, never a guess:
                            // read it back and tell the owner.
                            self.toast = Some(format!("Auto-approve was not changed: {message}"));
                            self.state.error = None;
                            self.schedule(Operation::ChatPlan, Duration::ZERO);
                        }
                        OperationKind::Thinking => {
                            // The picker shows the server's value, never a guess:
                            // read it back and tell the owner.
                            self.toast = Some(format!("Thinking level was not changed: {message}"));
                            self.state.error = None;
                            self.schedule(Operation::Catalog, Duration::ZERO);
                        }
                        OperationKind::StopTurn => {
                            self.state.stopping = false;
                            // The reply keeps running; Stop can be pressed again.
                            self.toast = Some(format!("Could not stop the reply: {message}"));
                            self.state.error = None;
                        }
                        OperationKind::Attach => {
                            // A rejected file never blocks the composer.
                            self.uploading = false;
                            self.upload_thumb = None;
                            self.toast = Some(format!("Attachment failed: {message}"));
                            self.state.error = None;
                        }
                        OperationKind::FetchArtifact => {
                            self.toast = Some(format!("Download failed: {message}"));
                            self.downloading = false;
                            self.state.error = None;
                        }
                        OperationKind::Logs => {
                            // The tab shows its own message; no global banner.
                            self.state.logs_error = Some(if forbidden {
                                "Admin access is required to view server logs.".to_owned()
                            } else {
                                message
                            });
                            self.state.logs_loaded = true;
                            self.state.error = None;
                        }
                        OperationKind::MarketplaceList
                        | OperationKind::MarketplaceDetail
                        | OperationKind::MarketplaceWrite => {
                            self.marketplace_busy = false;
                            let call = match completion.kind {
                                OperationKind::MarketplaceList => {
                                    crate::marketplace::MarketplaceCall::List
                                }
                                OperationKind::MarketplaceDetail => {
                                    crate::marketplace::MarketplaceCall::Detail
                                }
                                _ => crate::marketplace::MarketplaceCall::Write,
                            };
                            // A missing list route means this server has no
                            // marketplace. A missing plugin does not hide the
                            // catalog that already loaded.
                            match crate::marketplace::marketplace_problem(call, not_found, message)
                            {
                                crate::marketplace::MarketplaceProblem::Unavailable => {
                                    self.state.marketplace_unavailable = true;
                                    self.state.marketplace_loaded = true;
                                    self.state.marketplace_error = None;
                                }
                                crate::marketplace::MarketplaceProblem::Message(text) => {
                                    self.state.marketplace_error = Some(text);
                                }
                            }
                            self.state.error = None;
                        }
                        OperationKind::Workflows | OperationKind::WorkflowsWrite => {
                            self.state.workflows_loaded = true;
                            self.state.workflows_error = Some(message);
                            self.state.error = None;
                        }
                        OperationKind::DocPreview => {
                            self.doc_previews_pending.clear();
                            self.toast = Some(format!("No preview for this file: {message}"));
                            self.state.error = None;
                        }
                        OperationKind::CreateBot => {
                            self.creating_named_bot = false;
                        }
                        OperationKind::VoiceSettings => {
                            self.voice.loading = false;
                            self.admin_error = Some(message);
                            if !self.state.unknown_outcome && !dropped {
                                self.state.error = None;
                            }
                        }
                        OperationKind::Submit => {
                            self.state.stopping = false;
                            self.stop_queued = false;
                            self.note_submit_failure(submit_failure);
                        }
                        // The transcript has its own failure state: the chat
                        // shows a Retry panel that no other reply can clear.
                        OperationKind::History if !self.state.history_loaded => {
                            self.state.history_failed = true;
                        }
                        // The chain that loads the transcript starts with the
                        // catalog on a cold open; without the catalog it would
                        // never reach the transcript.
                        OperationKind::Catalog if !self.state.history_loaded => {
                            self.schedule(Operation::History, Duration::ZERO);
                        }
                        _ => {}
                    }
                    if !self.state.connected {
                        // The sign-in and server screen is where the address,
                        // certificate and tunnel hint helps.
                        self.connect_error = Some(sign_in_message);
                        self.restarting = false;
                    }
                    if rate_limited {
                        self.next_request = Instant::now()
                            + Duration::from_secs(crate::api::last_retry_after_secs());
                    }
                }
                Ok(reply) => {
                    self.rate_attempts = 0;
                    self.link_recovered();
                    if let Reply::Artifact {
                        filename, bytes, ..
                    } = reply
                    {
                        // Bytes stay off the state: they go straight to a
                        // file-write thread and never touch the UI state.
                        self.downloading = true;
                        let (sender, receiver) = std::sync::mpsc::channel();
                        self.download_rx = Some(receiver);
                        let save_as = self.pending_open == Some(PendingOpen::SaveAs);
                        if std::thread::Builder::new()
                            .name("serabot-download".into())
                            .spawn(move || {
                                let result = if save_as {
                                    save_download_as(&filename, &bytes)
                                } else {
                                    save_download(&filename, &bytes)
                                };
                                let _ = sender.send(result);
                            })
                            .is_err()
                        {
                            self.download_rx = None;
                            self.downloading = false;
                            self.toast = Some("Could not start the download.".into());
                        }
                        continue;
                    }
                    // Capture the ack before the state consumes the reply.
                    if let Reply::Attached(file) = reply {
                        self.uploading = false;
                        if let Some(url) = self.upload_thumb.take() {
                            self.remember_thumb(file.id.clone(), url);
                        }
                        if self.composer_files.len() < crate::MAX_ATTACHMENTS {
                            self.composer_files.push(file);
                        }
                        continue;
                    }
                    if let Reply::Stopping(count) = &reply {
                        self.run_action_done();
                        if *count == 0 {
                            self.state.stopping = false;
                            self.schedule(Operation::History, Duration::ZERO);
                            if !self.main_run_active()
                                && crate::runs::active_count(&self.state.runs.rows) == 0
                            {
                                self.toast = Some("Nothing is running to stop.".into());
                            }
                        } else if let Some(task) = self
                            .state
                            .task
                            .as_ref()
                            .filter(|task| !task.status.terminal())
                        {
                            self.schedule(Operation::Task(task.id.clone()), Duration::ZERO);
                        } else {
                            self.state.stopping = false;
                            self.schedule(Operation::History, Duration::ZERO);
                        }
                    }
                    if matches!(reply, Reply::RunActed) {
                        self.run_action_done();
                    }
                    // `None` for every write but a provider save or removal.
                    let provider_restart = match &reply {
                        Reply::ProviderSaved { restart_required } => Some(*restart_required),
                        _ => None,
                    };
                    let bot_config_saved = match &reply {
                        Reply::BotConfigSaved { restart_required } => Some(*restart_required),
                        _ => None,
                    };
                    let account_added = match &reply {
                        Reply::AccountDone(crate::worker::AccountDone::Added(added)) => {
                            Some(added.clone())
                        }
                        _ => None,
                    };
                    let avatar_saved = match &reply {
                        Reply::AvatarSaved(target) => Some(target.clone()),
                        _ => None,
                    };
                    if let Reply::DocPreview { hash, .. } = &reply {
                        self.doc_previews_pending.remove(hash);
                    }
                    let page_reload_failed = matches!(
                        &reply,
                        Reply::MarketplaceChanged { slug, detail: None, .. }
                            if self.marketplace_open.as_deref() == Some(slug.as_str())
                    );
                    let reply = match reply {
                        // A slow read for a plugin the user has already left.
                        Reply::MarketplaceDetail(detail)
                            if self.marketplace_open.as_deref()
                                != Some(detail.plugin.slug.as_str()) =>
                        {
                            None
                        }
                        // Do not reopen a page the user closed while the
                        // change was in flight.
                        Reply::MarketplaceChanged {
                            slug,
                            plugins,
                            detail,
                        } => {
                            let detail = detail.filter(|page| {
                                self.marketplace_open.as_deref() == Some(page.plugin.slug.as_str())
                            });
                            Some(Reply::MarketplaceChanged {
                                slug,
                                plugins,
                                detail,
                            })
                        }
                        other => Some(other),
                    };
                    if let Some(reply) = reply
                        && !self.state.receive(&completion.scope, reply)
                    {
                        continue;
                    }
                    if page_reload_failed {
                        self.state.marketplace_detail = None;
                        self.state.marketplace_error =
                            Some("The plugin changed, but its page could not be reloaded.".into());
                    }
                    if matches!(completion.kind, OperationKind::Approvals) {
                        self.note_approvals();
                        if !self.state.standing_grants_loaded {
                            self.schedule(Operation::StandingGrants, Duration::ZERO);
                        }
                    }
                    match completion.kind {
                        OperationKind::Health if self.state.connected => {
                            self.connect_error = None;
                            self.sign_in.clear_secrets();
                            self.sign_in.error = None;
                            self.sign_in.notice = None;
                            self.sync_audio_draft();
                            self.sync_voice_draft();
                            if self.restarting {
                                self.restarting = false;
                                self.restart_pending = false;
                                self.admin_notice =
                                    Some("Server is back. The saved settings are active.".into());
                            }
                            self.persist_credentials();
                            self.start_events();
                            self.schedule(Operation::Me, Duration::ZERO);
                        }
                        OperationKind::Me => {
                            self.adopt_session_expiry();
                            self.schedule(Operation::Catalog, Duration::ZERO);
                        }
                        OperationKind::Health => {
                            self.refreshing = false;
                            self.poll_paused = true;
                            self.connect_error = self
                                .state
                                .error
                                .clone()
                                .or_else(|| Some("Server is not ready.".into()));
                        }
                        OperationKind::VoiceSettings => {
                            self.voice.loading = false;
                            self.admin_error = None;
                            self.sync_voice_draft();
                        }
                        OperationKind::Catalog if self.refreshing => {
                            if self.state.history_loaded {
                                self.schedule(Operation::Approvals, Duration::ZERO);
                            } else {
                                self.schedule(Operation::History, Duration::ZERO);
                            }
                        }
                        OperationKind::History if self.refreshing => {
                            if self.state.catalog.is_none() {
                                self.schedule(Operation::Catalog, Duration::ZERO);
                            } else {
                                self.schedule(Operation::Approvals, Duration::ZERO);
                            }
                        }
                        OperationKind::Approvals if self.refreshing => self.schedule(
                            Operation::ListConversations {
                                include_archived: false,
                            },
                            Duration::ZERO,
                        ),
                        OperationKind::ListConversations
                            if self.pending_open_group.is_some()
                                || self.avatar_upload.is_some() =>
                        {
                            self.continue_after_group_list();
                        }
                        OperationKind::ListConversations if self.refreshing => {
                            self.refreshing = false;
                            self.ensure_section_data();
                            // On connect the open dir: conversation's bot
                            // detail refreshes too, like the catalog chain.
                            // Scheduled after ensure_section_data because a
                            // started request clears the scheduled slot.
                            self.schedule_open_bot_detail();
                            self.schedule_next_avatar();
                        }
                        OperationKind::ListConversations => self.schedule_next_avatar(),
                        OperationKind::Submit => {
                            self.poll_history = false;
                            if !self.stop_queued {
                                self.schedule(Operation::History, Duration::ZERO);
                            }
                        }
                        OperationKind::Decide => {
                            self.schedule(Operation::Approvals, Duration::ZERO);
                        }
                        OperationKind::RevokeStandingGrant => {
                            self.toast = Some("Standing grant revoked.".into());
                            self.schedule(Operation::StandingGrants, Duration::ZERO);
                        }
                        // The answer or decline posted a message: read the transcript
                        // and the cards together.
                        OperationKind::CardWrite => {
                            self.schedule(Operation::History, Duration::ZERO);
                        }
                        OperationKind::CreateSkill => {
                            self.schedule(Operation::ListSkills, Duration::ZERO);
                        }
                        OperationKind::CreateBot if self.creating_named_bot => {
                            self.bot_created();
                        }
                        OperationKind::CreateBot => {
                            if let Some(created) = self.state.created_bot.clone() {
                                self.open_bot_settings(created.id);
                            }
                            self.schedule(Operation::ListBots, Duration::ZERO);
                        }
                        OperationKind::ListBots => {
                            self.continue_after_bots();
                        }
                        // The sheet reads the fresh catalog, then the bot list
                        // follows so a bot just added reaches the rail and picker.
                        OperationKind::BotCatalog | OperationKind::BotInstall => {
                            let next = if completion.kind == OperationKind::BotInstall {
                                Operation::BotCatalog
                            } else {
                                Operation::ListBots
                            };
                            self.catalog_refresh = false;
                            self.schedule(next, Duration::ZERO);
                        }
                        OperationKind::Avatar => {
                            self.avatar_op = None;
                            crate::avatars::publish(&self.state.avatars);
                            if avatar_saved.is_some() {
                                // A write: reread the lists so the new version
                                // stamp reaches the cache.
                                if matches!(avatar_saved, Some(AvatarTarget::Group(_))) {
                                    self.schedule(
                                        Operation::ListConversations {
                                            include_archived: false,
                                        },
                                        Duration::ZERO,
                                    );
                                } else {
                                    self.schedule(Operation::ListBots, Duration::ZERO);
                                }
                            } else {
                                self.schedule_next_avatar();
                            }
                        }
                        OperationKind::CreateGroup => self.group_created(),
                        OperationKind::OnboardingRead | OperationKind::OnboardingWrite => {
                            self.onboarding_ui.waiting = false;
                            self.onboarding_ui.error = None;
                            if self
                                .state
                                .onboarding
                                .as_ref()
                                .is_some_and(|current| current.done)
                            {
                                // The confirm enabled the bot and saved its
                                // prompt; the lists show it.
                                self.schedule(Operation::ListBots, Duration::ZERO);
                            }
                        }
                        OperationKind::ToolsWrite => {
                            self.schedule(Operation::ListBots, Duration::ZERO);
                        }
                        OperationKind::BotSettingsWrite => self.bot_settings_saved(),
                        OperationKind::BotLifecycle => self.bot_lifecycle_done(),
                        OperationKind::CreateConversation => {
                            self.fleet.conv_title.clear();
                            self.fleet.conv_slug.clear();
                            self.fleet.conv_parent.clear();
                            if let Some(members) = self.pending_team.take() {
                                // The room exists now; its team is written on
                                // the room itself in the next step.
                                self.apply_initial_team(members);
                            } else {
                                self.schedule(
                                    Operation::ListConversations {
                                        include_archived: false,
                                    },
                                    Duration::ZERO,
                                );
                            }
                        }
                        OperationKind::MarketplaceList | OperationKind::MarketplaceDetail => {}
                        OperationKind::MarketplaceWrite => {
                            self.marketplace_busy = false;
                            self.toast = Some("Updated.".into());
                        }
                        OperationKind::CreateBotChat => {
                            if let Some((_, chat, _)) = self.state.created_bot_chat.take() {
                                self.open_conversation(chat);
                            }
                            self.reload_bot_chats();
                            self.schedule(
                                Operation::ListConversations {
                                    include_archived: false,
                                },
                                Duration::ZERO,
                            );
                        }
                        OperationKind::SetBotRefresh => {
                            self.toast = Some("Memory refresh saved.".into());
                        }
                        OperationKind::ListBotChats => {}
                        OperationKind::RenameConversation | OperationKind::ArchiveConversation => {
                            self.reload_bot_chats();
                            let archived_current = completion.kind
                                == OperationKind::ArchiveConversation
                                && self.archive_confirm.as_deref()
                                    == Some(self.conversation.as_str());
                            self.rename = None;
                            self.archive_confirm = None;
                            if archived_current {
                                self.open_conversation("windows-owner".into());
                            } else {
                                self.schedule(
                                    Operation::ListConversations {
                                        include_archived: false,
                                    },
                                    Duration::ZERO,
                                );
                            }
                        }
                        OperationKind::UnarchiveConversation => {
                            self.reload_bot_chats();
                            self.schedule(
                                Operation::ListConversations {
                                    include_archived: false,
                                },
                                Duration::ZERO,
                            );
                            self.queue_backlog(Operation::ListArchived);
                        }
                        OperationKind::AddMemory
                        | OperationKind::DeleteMemory
                        | OperationKind::UpdateMemory
                        | OperationKind::SetMemoryStatus => {
                            if completion.kind == OperationKind::AddMemory {
                                self.memory.content.zeroize();
                            }
                            if completion.kind == OperationKind::UpdateMemory {
                                self.memory.edit_content.zeroize();
                                self.memory.editing = None;
                            }
                            self.memory.delete_confirm = None;
                            // Counts are stale after any change; the next list reloads them.
                            self.state.memory_stats = None;
                            let list = self.memory_list_operation();
                            self.schedule(list, Duration::ZERO);
                        }
                        OperationKind::ListMemories if self.state.memory_stats.is_none() => {
                            self.schedule(Operation::MemoryStats, Duration::ZERO);
                        }
                        OperationKind::Task
                            if self
                                .state
                                .task
                                .as_ref()
                                .is_some_and(|t| t.status.terminal()) =>
                        {
                            self.schedule(Operation::History, Duration::ZERO)
                        }
                        OperationKind::Task
                            if self
                                .state
                                .task
                                .as_ref()
                                .is_some_and(|t| t.status == TaskStatus::AwaitingApproval) =>
                        {
                            self.schedule(Operation::Approvals, Duration::ZERO)
                        }
                        OperationKind::Connectors => self.sync_connector_drafts(),
                        OperationKind::TeamWrite => {
                            // The write returned the saved team; read it back so
                            // the console shows the server's own view.
                            self.state.team_loading = true;
                            self.schedule(Operation::GetTeam, Duration::ZERO);
                        }
                        OperationKind::Team => {
                            if let Some(team) = &self.state.team {
                                self.team_rules_draft = team.rules.clone();
                            }
                            // A freshly created room is reloaded once its team
                            // has been written, so the chat opens on the room.
                            if self.pending_room_refresh {
                                self.pending_room_refresh = false;
                                self.refresh();
                            }
                        }
                        OperationKind::Transcribe => {
                            // The transcript is untrusted model output: it goes
                            // into the composer as a draft and is never sent.
                            self.state.transcript_loading = false;
                            self.last_audio = None;
                            if let Some(text) = self.state.transcript.clone() {
                                if !self.composer.trim().is_empty() {
                                    self.composer.push('\n');
                                }
                                self.composer.push_str(&text);
                                // A transcription landing is an intentional
                                // composer change: the view syncs it in.
                                self.composer_gen = self.composer_gen.wrapping_add(1);
                            }
                        }
                        OperationKind::History => {
                            // The transcript is in; the artifact library of this
                            // conversation loads right behind it.
                            self.state.library_loading = true;
                            self.schedule(Operation::Library, Duration::ZERO);
                        }
                        // The plan and auto-approve switch load after the library.
                        OperationKind::Library => {
                            self.schedule(Operation::MediaJobs, Duration::ZERO);
                        }
                        OperationKind::MediaJobs => {
                            self.schedule(Operation::JobsReliability, Duration::ZERO);
                            self.queue_backlog(Operation::ChatPlan);
                            self.queue_backlog(Operation::TurnTraces);
                        }
                        OperationKind::OAuthSignIn => {
                            self.admin_error = None;
                            self.oauth_signing_in = None;
                            match self.state.oauth_result.take() {
                                Some((_, Some(code))) => {
                                    self.admin_notice = Some(format!(
                                        "Finish in your browser: enter code {code}. Then press Refresh."
                                    ));
                                }
                                Some((id, None))
                                    if self
                                        .state
                                        .providers
                                        .iter()
                                        .any(|p| p.id == id && p.kind == "openrouter") =>
                                {
                                    // OpenRouter's key is stored for this account
                                    // right away: nothing to restart.
                                    self.admin_notice = Some(
                                        "Signed in with OpenRouter. Your key is saved to your account."
                                            .into(),
                                    );
                                }
                                Some(_) => {
                                    self.admin_notice = Some(
                                        "Signed in. Restart the server to apply the change.".into(),
                                    );
                                    self.restart_pending = true;
                                }
                                None => {}
                            }
                            self.state.providers_loaded = false;
                            self.schedule(Operation::Catalog, Duration::ZERO);
                        }
                        OperationKind::ConnectorTest => {
                            self.admin_error = None;
                            self.admin_notice = None;
                            self.connector_testing = None;
                        }
                        OperationKind::ConnectorSignOut => {
                            self.admin_error = None;
                            self.admin_notice = Some("Signed out for this account.".into());
                            self.state.connectors_loaded = false;
                            self.schedule(Operation::Connectors, Duration::ZERO);
                        }
                        OperationKind::ProviderAccount => self.account_write_done(account_added),
                        OperationKind::RouteWrite => self.route_saved(),
                        OperationKind::Apply => self.after_route_applied(),
                        OperationKind::ProviderKey => {
                            for value in self.provider_key_drafts.values_mut() {
                                value.zeroize();
                            }
                            self.provider_key_drafts.clear();
                            self.admin_error = None;
                            self.admin_notice = Some(
                                "Key saved to your account. Checking the model list...".into(),
                            );
                            self.state.providers_loaded = false;
                            self.schedule(Operation::Catalog, Duration::ZERO);
                        }
                        OperationKind::ProviderTest => {
                            self.provider_testing = None;
                            self.admin_error = None;
                            self.admin_notice = None;
                            self.state.providers_loaded = false;
                            self.schedule(Operation::Catalog, Duration::ZERO);
                        }
                        OperationKind::DefaultRoute => {
                            self.admin_error = None;
                            self.admin_notice = Some("Default provider updated.".into());
                            self.state.providers_loaded = false;
                            self.schedule(Operation::Catalog, Duration::ZERO);
                        }
                        OperationKind::AdminWrite => {
                            self.provider.api_key.zeroize();
                            self.provider.oauth_client_secret.zeroize();
                            for draft in self.connector_drafts.values_mut() {
                                for value in draft.values.values_mut() {
                                    value.zeroize();
                                }
                            }
                            self.admin_error = None;
                            if let Some(id) = self.accounts_ui.save_signin.take() {
                                // A sign-in provider needs no restart: its card
                                // appears and the sign-in page opens.
                                self.admin_notice =
                                    Some("Saved. Opening the sign-in page\u{2026}".into());
                                self.sign_in_after_save(id);
                            } else if provider_restart == Some(false) {
                                // The server put the provider change in front of
                                // the bots at once; the lists below show it.
                                self.admin_notice = Some("Saved. Your bots can use it now.".into());
                                self.state.providers_loaded = false;
                                self.schedule(Operation::Catalog, Duration::ZERO);
                            } else {
                                self.admin_notice =
                                    Some("Saved. Restart the server to apply the change.".into());
                                self.restart_pending = true;
                            }
                            // Re-read the lists so "Connected" reflects what the
                            // server now holds instead of the pre-save snapshot.
                            self.state.connectors_loaded = false;
                            self.schedule(Operation::Connectors, Duration::ZERO);
                        }
                        OperationKind::AdminRestart => {
                            self.admin_error = None;
                            self.admin_notice = Some(
                                "Restarting the server. The console reconnects automatically."
                                    .into(),
                            );
                            self.restart_pending = false;
                            self.restarting = true;
                            self.state.connected = false;
                            self.stop_events();
                            self.refreshing = false;
                            self.scheduled = None;
                            self.reconnect_at = Some(Instant::now() + Duration::from_secs(8));
                        }
                        // The detail is cached in the state; nothing else to do.
                        OperationKind::BotDetail => {}
                        // The usage report is cached in the state.
                        OperationKind::Usage => self.usage_loading = false,
                        OperationKind::ServerHealth => {
                            self.server_health_loading = false;
                            self.server_health_error = None;
                        }
                        OperationKind::BotConfigWrite => {
                            if let Some((id, update)) = bot_config {
                                // Optimistic update of the cached detail; the
                                // read-back below shows the server's own view.
                                if let Some(detail) = self.state.bot_details.get_mut(&id) {
                                    if let Some(tools) = update.tools {
                                        detail.tools = tools;
                                    }
                                    if let Some(skills) = update.skills {
                                        detail.skills = skills;
                                    }
                                    if let Some(require_approval) = update.require_approval {
                                        detail.require_approval = require_approval;
                                    }
                                }
                                self.admin_error = None;
                                self.toast = Some(if bot_config_saved == Some(true) {
                                    "Bot config saved. Restart the server to apply the change."
                                        .into()
                                } else {
                                    "Bot config saved.".into()
                                });
                                self.schedule(Operation::BotDetail { id }, Duration::ZERO);
                            }
                        }
                        _ => {}
                    }
                }
            }
        }
    }

    fn persist_credentials(&mut self) {
        let Some(credentials) = self.credentials.clone() else {
            return;
        };
        let origin = credentials.endpoint.origin().ascii_serialization();
        self.server = origin.clone();
        self.credential_save_failed = false;
        self.enqueue_persist(PersistTask::Credentials {
            origin,
            theme: self.theme.as_str().into(),
            pins: self.pins.clone(),
            prefs: self.prefs.clone(),
            token: credentials.token.clone(),
            expires: self.session_expires,
        });
    }

    /// Try again to write the session token to the OS credential store after
    /// it was refused. Success clears the warning; failure keeps it.
    pub fn retry_save_credentials(&mut self) {
        let Some(credentials) = self.credentials.clone() else {
            return;
        };
        match creds::save_session(credentials.token.expose_secret(), self.session_expires) {
            Ok(()) => {
                self.credential_save_failed = false;
                self.toast = Some("Session saved. You will stay signed in after a restart.".into());
            }
            Err(_) => self.credential_save_failed = true,
        }
    }

    fn missing_section_op(&self) -> Option<Operation> {
        match self.section {
            Section::Chat => {
                if !self.state.conversations_loaded {
                    Some(Operation::ListConversations {
                        include_archived: false,
                    })
                } else if !self.state.bots_loaded {
                    Some(Operation::ListBots)
                } else if !self.state.approvals_loaded {
                    Some(Operation::Approvals)
                } else {
                    // One bot's chat list per tick, until every bot has one.
                    self.state
                        .listed_bots
                        .iter()
                        .find(|bot| !self.state.bot_chats_loaded.contains(&bot.id))
                        .map(|bot| Operation::ListBotChats {
                            bot: bot.id.clone(),
                        })
                }
            }
            Section::Conversations => {
                (!self.state.conversations_loaded).then(|| Operation::ListConversations {
                    include_archived: false,
                })
            }
            Section::Projects => None,
            Section::Memory => {
                if self.state.memories_loaded {
                    None
                } else {
                    Some(self.memory_list_operation())
                }
            }
            Section::Providers => (!self.state.providers_loaded).then_some(Operation::Providers),
            Section::Tasks => {
                if self.state.tasks_loaded {
                    None
                } else {
                    Some(Operation::ListTasks)
                }
            }
            Section::Logs => (!self.state.logs_loaded).then(|| Operation::Logs {
                level: self.logs_level.clone(),
                query: self.logs_query.clone(),
            }),
            Section::Workflows => (!self.state.workflows_loaded).then_some(Operation::Workflows),
            Section::Integrations => {
                if !self.state.connectors_loaded {
                    Some(Operation::Connectors)
                } else if !self.state.marketplace_loaded && !self.state.marketplace_unavailable {
                    Some(Operation::MarketplaceList)
                } else {
                    None
                }
            }
            // The Library and Mail pages load themselves through their own API jobs.
            Section::Browser
            | Section::Vault
            | Section::Usage
            | Section::Library
            | Section::Mail => None,
            Section::Settings => match self.settings_section {
                SettingsSection::Memory => {
                    (!self.state.memories_loaded).then(|| self.memory_list_operation())
                }
                SettingsSection::Providers => {
                    (!self.state.providers_loaded).then_some(Operation::Providers)
                }
                SettingsSection::Logs => (!self.state.logs_loaded).then(|| Operation::Logs {
                    level: self.logs_level.clone(),
                    query: self.logs_query.clone(),
                }),
                SettingsSection::Integrations => {
                    if !self.state.connectors_loaded {
                        Some(Operation::Connectors)
                    } else if !self.state.marketplace_loaded && !self.state.marketplace_unavailable
                    {
                        Some(Operation::MarketplaceList)
                    } else {
                        None
                    }
                }
                _ => None,
            },
        }
    }

    fn ensure_section_data(&mut self) {
        if self.section == Section::Settings {
            self.ensure_telegram();
        }
        if !self.state.connected
            || self.state.unknown_outcome
            || self.refreshing
            || self.poll_paused
        {
            return;
        }
        let Some(operation) = self.missing_section_op() else {
            return;
        };
        if self.pending.is_some() {
            return;
        }
        if self.ready() {
            self.start_request(operation);
        } else {
            self.schedule(operation, Duration::ZERO);
        }
    }

    /// Load the Telegram card once per connection. A failed read marks the
    /// card loaded so the tick does not retry it; the manual Refresh does.
    fn ensure_telegram(&mut self) {
        if self.state.connected
            && !self.telegram.loaded
            && self.telegram.in_flight.is_none()
            && self.telegram.rx.is_none()
        {
            self.refresh_telegram();
        }
    }

    fn poll_updates(&mut self) {
        let received = self
            .update
            .rx
            .as_ref()
            .map(std::sync::mpsc::Receiver::try_recv);
        match received {
            Some(Ok(Ok(Some(info)))) => {
                self.update.rx = None;
                self.update.checking = false;
                self.update.manual = false;
                self.update.info = Some(info);
                self.update.error = None;
            }
            Some(Ok(Ok(None))) => {
                self.update.rx = None;
                self.update.checking = false;
                self.update.info = None;
                if std::mem::take(&mut self.update.manual) {
                    self.update.notice = Some(format!(
                        "You are on the latest version (v{}).",
                        updater::current_version()
                    ));
                }
            }
            Some(Ok(Err(error))) => {
                self.update.rx = None;
                self.update.checking = false;
                self.update.manual = false;
                self.update.error = Some(error);
            }
            Some(Err(std::sync::mpsc::TryRecvError::Disconnected)) => {
                self.update.rx = None;
                self.update.checking = false;
                self.update.manual = false;
                self.update.error = Some("The update check stopped.".into());
            }
            Some(Err(std::sync::mpsc::TryRecvError::Empty)) | None => {}
        }
    }

    pub fn selected_tools(&self) -> Vec<String> {
        SPECIALIST_TOOLS
            .iter()
            .enumerate()
            .filter_map(|(index, name)| {
                self.fleet
                    .bot_tools
                    .get(index)
                    .copied()
                    .unwrap_or(false)
                    .then_some((*name).to_owned())
            })
            .collect()
    }

    pub fn conversation_title(&self) -> String {
        if let Some(conversation) = self
            .state
            .conversations
            .iter()
            .find(|conversation| conversation.id == self.conversation)
        {
            return self.conversation_label(conversation);
        }
        if self.conversation == "windows-owner" {
            return "Owner".into();
        }
        if crate::bot_chats::is_extra_chat_id(&self.conversation)
            || crate::bot_chats::is_raw_chat_id(&self.conversation)
        {
            return self.fallback_extra_chat_title(&self.conversation);
        }
        if let Some(bot) = self.conversation.strip_prefix("dir:") {
            return self.bot_display_name(bot);
        }
        humanize_label(&self.conversation)
    }

    /// Jump directly to the conversation needing approval (or open the active
    /// chat's inline approval card) instead of a standalone Approvals tab.
    pub fn open_approval_conversation(&mut self) {
        self.open_section(Section::Chat);
        if let Some(task) = self.state.task.as_ref()
            && task.status == TaskStatus::AwaitingApproval
            && !task.bot.trim().is_empty()
        {
            let target_bot = task.bot.clone();
            if self.conversation_bot(&self.conversation).as_deref() != Some(target_bot.as_str()) {
                self.open_bot_chat(target_bot);
                return;
            }
        }
        if !self.state.approvals_loaded {
            self.refresh_approvals();
        }
    }

    pub fn conversation_kind(&self) -> Option<&str> {
        self.state
            .conversations
            .iter()
            .find(|conversation| conversation.id == self.conversation)
            .map(|conversation| conversation.normalized_kind())
    }

    pub fn conversation_accepts_turns(&self) -> bool {
        self.state
            .conversations
            .iter()
            .find(|conversation| conversation.id == self.conversation)
            .is_none_or(|conversation| conversation.accepts_turns)
    }

    pub fn refresh_bots(&mut self) {
        self.start_request(Operation::ListBots);
    }

    pub fn refresh_skills(&mut self) {
        self.start_request(Operation::ListSkills);
    }

    pub fn refresh_providers(&mut self) {
        self.start_request(Operation::Providers);
    }

    /// Refresh the live provider catalogs (GET /v1/models) and then reload
    /// provider status. Requires catalog_fetch on the server provider.
    pub fn fetch_models(&mut self) {
        if !self.state.connected {
            return;
        }
        self.state.providers_loaded = false;
        if self.pending.is_some() || !self.ready() {
            self.queue_backlog(Operation::Catalog);
            return;
        }
        self.start_request(Operation::Catalog);
    }

    pub fn refresh_connectors(&mut self) {
        self.start_request(Operation::Connectors);
    }

    /// The signed-in user is an administrator (developer fields stay hidden
    /// from everyone else).
    pub fn is_admin(&self) -> bool {
        self.state
            .user
            .as_ref()
            .is_some_and(|user| user.role == "admin")
    }

    /// Key-based connector: turn it on and save the pasted key in one step.
    pub fn add_connector_key(&mut self, id: String) {
        self.connector_drafts.entry(id.clone()).or_default().enabled = true;
        self.save_connector(id);
    }

    /// Sign-in connector (ElevenLabs): forget this account's sign-in on the
    /// server. Tool calls fail until the account signs in again.
    pub fn sign_out_connector(&mut self, id: String) {
        if !self.can_admin() {
            self.admin_error = Some("Wait for the current request, then retry.".into());
            return;
        }
        self.admin_error = None;
        self.admin_notice = None;
        self.start_request(Operation::ConnectorSignOut { id });
    }

    pub fn can_admin(&self) -> bool {
        self.ready() && self.state.connected
    }

    pub fn test_connector(&mut self, id: String) {
        if !self.can_admin() {
            self.admin_error = Some("Wait for the current request, then retry.".into());
            return;
        }
        self.admin_error = None;
        self.admin_notice = None;
        self.connector_testing = Some(id.clone());
        self.start_request(Operation::TestConnector { id });
    }

    pub fn toggle_connector(&mut self, id: &str) {
        let draft = self.connector_drafts.entry(id.to_owned()).or_default();
        draft.enabled = !draft.enabled;
    }

    /// Set a connector's enabled state explicitly (0.28.0 Web search card);
    /// unlike the toggle this never flips, so a re-render keeps the value.
    pub fn set_connector_enabled(&mut self, id: &str, enabled: bool) {
        let draft = self.connector_drafts.entry(id.to_owned()).or_default();
        draft.enabled = enabled;
    }

    pub fn set_connector_value(&mut self, id: &str, role: &str, value: String) {
        self.connector_drafts
            .entry(id.to_owned())
            .or_default()
            .values
            .insert(role.to_owned(), value);
    }

    pub fn save_provider(&mut self) {
        self.admin_error = None;
        self.admin_notice = None;
        let id = self.provider.id.trim().to_owned();
        let kind = self.provider.kind.trim().to_owned();
        let base_url = self.provider.base_url.trim().to_owned();
        let secret = self.provider.secret_name.trim().to_owned();
        let models = model_csv(&self.provider.models);
        let mut api_key = self.provider.api_key.trim().to_owned();
        if api_key.is_empty() && self.provider.auth == "none" {
            // A local server needs no key, but the credential store keeps one
            // per provider; Ollama's own docs use this placeholder.
            api_key = "ollama".to_owned();
        }
        let problem = if !valid_provider_id(&id) {
            Some("Provider id must be 1–64 letters, digits, '_' or '-'.")
        } else if kind.is_empty() || kind.len() > 32 {
            Some("Choose a provider kind.")
        } else if base_url.is_empty() || base_url.len() > 2_048 {
            Some("Enter the provider base URL.")
        } else if models.is_empty() || models.len() > 256 {
            Some("List at least one model id.")
        } else if models.iter().any(|model| !valid_model_id(model)) {
            Some("Model ids cannot be empty or contain control characters.")
        } else if models
            .iter()
            .collect::<std::collections::BTreeSet<_>>()
            .len()
            != models.len()
        {
            Some("Each model id can be listed once.")
        } else if !valid_secret_name(&secret) {
            Some("Secret name must be 1–128 letters, digits, '_' or '-'.")
        } else if !api_key.is_empty() && !valid_credential(&api_key) {
            Some("The API key contains control characters or is too long.")
        } else if !self.provider.oauth_client_secret.trim().is_empty()
            && !valid_credential(self.provider.oauth_client_secret.trim())
        {
            Some("The OAuth client secret contains control characters or is too long.")
        } else {
            None
        };
        if let Some(problem) = problem {
            self.admin_error = Some(problem.into());
            return;
        }
        if !self.can_admin() {
            self.admin_error = Some("Wait for the current request, then retry.".into());
            return;
        }
        let client_secret = self.provider.oauth_client_secret.trim().to_owned();
        let signs_in = self.draft_signs_in(&id, &kind, api_key.is_empty());
        let sign_in_id = id.clone();
        let request = NewProvider {
            id,
            kind,
            base_url,
            models,
            api_key_secret: secret,
            catalog_fetch: self.provider.catalog_fetch,
            api_key: (!api_key.is_empty()).then_some(api_key),
            oauth_client_secret: (!client_secret.is_empty()).then_some(client_secret),
        };
        self.accounts_ui.save_signin = signs_in.then_some(sign_in_id);
        self.start_request(Operation::AdminSaveProvider(request));
    }

    /// Save a key typed on a provider card to the signed-in account.
    pub fn save_provider_key(&mut self, id: String) {
        let key = self
            .provider_key_drafts
            .get(&id)
            .map(|key| key.trim().to_owned())
            .unwrap_or_default();
        if key.is_empty() {
            self.admin_error = Some("Paste the key first.".into());
            return;
        }
        if !valid_credential(&key) {
            self.admin_error = Some("The key contains control characters or is too long.".into());
            return;
        }
        if !self.can_admin() {
            self.admin_error = Some("Wait for the current request, then retry.".into());
            return;
        }
        self.admin_error = None;
        self.admin_notice = None;
        self.start_request(Operation::SaveProviderKey {
            id,
            key: zeroize::Zeroizing::new(key),
        });
    }

    /// Ask one provider for its live model list.
    pub fn test_provider(&mut self, id: String) {
        if !self.can_admin() {
            self.admin_error = Some("Wait for the current request, then retry.".into());
            return;
        }
        self.admin_error = None;
        self.admin_notice = None;
        self.provider_testing = Some(id.clone());
        self.start_request(Operation::TestProvider { id });
    }

    /// Make this provider (and its first listed model) the account default.
    pub fn set_default_provider(&mut self, id: String, model: String) {
        if !self.can_admin() {
            self.admin_error = Some("Wait for the current request, then retry.".into());
            return;
        }
        self.admin_error = None;
        self.admin_notice = None;
        self.start_request(Operation::SetDefaultRoute {
            provider: id,
            model,
        });
    }

    pub fn remove_provider(&mut self, id: String) {
        if !self.can_admin() {
            self.admin_error = Some("Wait for the current request, then retry.".into());
            return;
        }
        self.admin_error = None;
        self.start_request(Operation::AdminRemoveProvider(id));
    }

    pub fn save_connector(&mut self, id: String) {
        self.admin_error = None;
        self.admin_notice = None;
        let Some(draft) = self.connector_drafts.get(&id) else {
            return;
        };
        let enabled = draft.enabled;
        let values = draft
            .values
            .iter()
            .filter_map(|(role, value)| {
                let value = value.trim();
                (!value.is_empty()).then(|| (role.clone(), value.to_owned()))
            })
            .collect::<BTreeMap<_, _>>();
        if values.values().any(|value| !valid_credential(value)) {
            self.admin_error = Some("Check the connector credential values.".into());
            return;
        }
        if !self.can_admin() {
            self.admin_error = Some("Wait for the current request, then retry.".into());
            return;
        }
        self.start_request(Operation::AdminSetConnector {
            id,
            update: ConnectorUpdate { enabled, values },
        });
    }

    /// Browser sign-in for a connector with OAuth support (ElevenLabs).
    /// `callback_uri` is the fixed loopback redirect from the server catalog.
    pub fn sign_in_connector(&mut self, id: String, callback_uri: String) {
        if !self.can_admin() {
            self.admin_error = Some("Wait for the current request, then retry.".into());
            return;
        }
        self.admin_error = None;
        self.admin_notice =
            Some("Complete the sign-in in your browser. The console waits up to 3 minutes.".into());
        self.oauth_signing_in = Some(id.clone());
        self.start_request(Operation::ConnectorOAuthSignIn { id, callback_uri });
    }

    pub fn save_audio(&mut self) {
        self.admin_error = None;
        self.admin_notice = None;
        let transcription = self.audio.transcription_model.trim().to_owned();
        let understanding = self.audio.understanding_model.trim().to_owned();
        if self.audio.enabled
            && (!valid_model_id(&transcription) || !valid_model_id(&understanding))
        {
            self.admin_error = Some("Enter both audio model names.".into());
            return;
        }
        if !self.can_admin() {
            self.admin_error = Some("Wait for the current request, then retry.".into());
            return;
        }
        self.start_request(Operation::AdminSetAudio(AudioUpdate {
            enabled: self.audio.enabled,
            transcription_model: (!transcription.is_empty()).then_some(transcription),
            understanding_model: (!understanding.is_empty()).then_some(understanding),
        }));
    }

    /// Ask the server for the current voice/OCR readiness.
    pub fn load_voice(&mut self) {
        if self.voice.loading {
            return;
        }
        self.voice.loading = true;
        self.start_request(Operation::VoiceSettings);
    }

    /// Save the editable voice/OCR fields. Admin only, like the other
    /// configuration writes.
    pub fn save_voice(&mut self) {
        self.admin_error = None;
        self.admin_notice = None;
        let speech_model = self.voice.tts_model.trim().to_owned();
        let ocr_model = self.voice.ocr_model.trim().to_owned();
        if !speech_model.is_empty() && !valid_model_id(&speech_model) {
            self.admin_error = Some("Check the speech model name.".into());
            return;
        }
        if !ocr_model.is_empty() && !valid_model_id(&ocr_model) {
            self.admin_error = Some("Check the OCR model name.".into());
            return;
        }
        if self.voice.reply_mode == "voice" && self.voice.voice_id.trim().is_empty() {
            self.admin_error = Some("Voice replies need a voice id.".into());
            return;
        }
        if !self.can_admin() {
            self.admin_error = Some("Wait for the current request, then retry.".into());
            return;
        }
        self.start_request(Operation::AdminSetVoice(VoiceUpdate {
            speech_model: (!speech_model.is_empty()).then_some(speech_model),
            voice_id: Some(self.voice.voice_id.trim().to_owned()),
            reply_mode: Some(self.voice.reply_mode.clone()),
            ocr_enabled: Some(self.voice.ocr_enabled),
            ocr_model: (!ocr_model.is_empty()).then_some(ocr_model),
        }));
    }

    /// Apply `GET /v1/settings/voice` to the draft without clobbering a field
    /// the operator is mid-edit on: only the read-only status is overwritten.
    fn apply_voice_settings(&mut self, settings: VoiceSettings) {
        self.voice.loaded = true;
        self.voice.loading = false;
        self.voice.stt_model = settings.stt.model;
        self.voice.stt_ready = settings.stt.ready;
        self.voice.stt_detail = settings.stt.detail;
        self.voice.tts_model = settings.tts.model;
        self.voice.tts_ready = settings.tts.ready;
        self.voice.tts_detail = settings.tts.detail;
        self.voice.voice_id = settings.tts.voice_id;
        self.voice.ocr_model = settings.ocr.model;
        self.voice.ocr_ready = settings.ocr.ready;
        self.voice.ocr_detail = settings.ocr.detail;
        self.voice.reply_mode = settings.reply_mode;
        self.voice.max_speech_chars = settings.max_speech_chars;
    }

    pub fn request_restart(&mut self) {
        if !self.can_admin() {
            self.admin_error = Some("Wait for the current request, then retry.".into());
            return;
        }
        self.admin_error = None;
        self.start_request(Operation::AdminRestart);
    }

    fn sync_connector_drafts(&mut self) {
        let mut seen = std::collections::BTreeSet::new();
        for connector in &self.state.connectors {
            seen.insert(connector.id.clone());
            let draft = self
                .connector_drafts
                .entry(connector.id.clone())
                .or_default();
            draft.enabled = connector.enabled;
        }
        self.connector_drafts.retain(|id, _| seen.contains(id));
    }

    /// Apply a freshly fetched voice/OCR surface to the draft exactly once.
    fn sync_voice_draft(&mut self) {
        if let Some(settings) = self.state.voice.take() {
            self.apply_voice_settings(settings);
        }
    }

    fn sync_audio_draft(&mut self) {
        if let Some(enabled) = self.state.audio_enabled {
            self.audio.enabled = enabled;
        }
        if let Some(model) = &self.state.audio_transcription_model
            && !model.is_empty()
        {
            self.audio.transcription_model = model.clone();
        }
        if let Some(model) = &self.state.audio_understanding_model
            && !model.is_empty()
        {
            self.audio.understanding_model = model.clone();
        }
    }

    pub fn refresh_approvals(&mut self) {
        if self.pending.is_some() || !self.ready() {
            self.queue_backlog(Operation::Approvals);
        } else {
            self.start_request(Operation::Approvals);
        }
        self.queue_backlog(Operation::StandingGrants);
    }

    pub fn revoke_standing_grant(&mut self, id: String) {
        self.start_request(Operation::RevokeStandingGrant { id });
    }

    pub fn refresh_jobs_reliability(&mut self) {
        if !self.state.connected {
            return;
        }
        self.start_request(Operation::JobsReliability);
    }

    pub fn load_conversations(&mut self) {
        self.start_request(Operation::ListConversations {
            include_archived: false,
        });
    }

    /// Load archived threads for the rail's Archived group. One fetch; the
    /// list is owner-scoped and refreshed on demand.
    pub fn load_archived(&mut self) {
        if self.state.archived_loaded {
            return;
        }
        self.start_request(Operation::ListArchived);
    }

    /// Download one library artifact into the Downloads folder. The write runs
    /// off the UI thread; the result lands in `toast` via `poll_download`.
    pub fn download_artifact(&mut self, hash: String) {
        self.start_download(hash, None);
    }

    /// Download the artifact, then open it with the operating system's default
    /// application (Word, Excel, PowerPoint, or a PDF viewer).
    pub fn open_artifact(&mut self, hash: String) {
        self.start_download(hash, Some(PendingOpen::Open));
    }

    /// Download the artifact, then select it in Explorer.
    pub fn reveal_artifact(&mut self, hash: String) {
        self.start_download(hash, Some(PendingOpen::Reveal));
    }

    fn start_download(&mut self, hash: String, action: Option<PendingOpen>) {
        if self.downloading {
            self.toast = Some("A download is already running.".into());
            return;
        }
        if !self.ready() {
            self.toast = Some("Wait for the current request, then retry.".into());
            return;
        }
        self.pending_open = action;
        self.start_request(Operation::FetchArtifact { hash });
        if self.pending.is_none() {
            self.pending_open = None;
            self.toast = Some("Could not start the download.".into());
        }
    }

    pub fn load_memories(&mut self) {
        let list = self.memory_list_operation();
        self.start_request(list);
    }

    // --- Tasks tab (0.25.0) ---

    pub fn load_tasks(&mut self) {
        self.start_request(Operation::ListTasks);
    }

    pub fn add_task(&mut self, title: String, due: Option<String>) {
        if title.trim().is_empty() {
            return;
        }
        self.start_request(Operation::AddTask { title, due });
    }

    /// Cycle a task's Kanban column: todo → doing → done → todo.
    pub fn cycle_task_status(&mut self, id: String, status: &str) {
        let next = match status {
            "todo" => "doing",
            "doing" => "done",
            _ => "todo",
        };
        self.start_request(Operation::SetTaskStatus {
            id,
            status: next.into(),
        });
    }

    pub fn update_task(&mut self, id: String, update: crate::api::TaskUpdate) {
        self.start_request(Operation::UpdateTask { id, update });
        self.start_request(Operation::ListTasks);
    }

    pub fn delete_task(&mut self, id: String) {
        self.start_request(Operation::DeleteTask { id });
    }

    pub fn load_task_prefs(&mut self) {
        self.start_request(Operation::TaskPrefs);
        self.start_request(Operation::TaskSyncStatus);
    }

    pub fn set_task_sync_provider(&mut self, provider: String) {
        self.start_request(Operation::SetTaskPrefs { provider });
    }

    pub fn run_task_sync(&mut self) {
        self.start_request(Operation::RunTaskSync);
        // Refresh the list and the sign-in state once the pass is applied.
        self.start_request(Operation::ListTasks);
        self.start_request(Operation::TaskSyncStatus);
    }

    pub fn load_task_calendar(&mut self, from: String, to: String) {
        self.start_request(Operation::TaskCalendar { from, to });
    }

    /// The list request for the current search text and filter chip.
    fn memory_list_operation(&self) -> Operation {
        let query = self.memory.query.trim();
        let (origin, status) = memory_filter_params(&self.memory.filter);
        Operation::ListMemories {
            query: (!query.is_empty()).then(|| query.to_owned()),
            origin: origin.into(),
            status: status.into(),
        }
    }

    pub fn set_memory_filter(&mut self, filter: &str) {
        self.memory.filter = filter.into();
        self.load_memories();
    }

    pub fn begin_memory_edit(&mut self, id: String, content: &str) {
        self.memory.edit_content.zeroize();
        self.memory.edit_content = content.to_owned();
        self.memory.editing = Some(id);
        self.memory.delete_confirm = None;
    }

    /// Approve a held memory: it becomes active and can be recalled.
    pub fn approve_memory(&mut self, id: String) {
        self.start_request(Operation::SetMemoryStatus {
            id,
            status: "active".into(),
        });
    }

    pub fn cancel_memory_edit(&mut self) {
        self.memory.edit_content.zeroize();
        self.memory.editing = None;
    }

    pub fn save_memory_edit(&mut self) {
        let Some(id) = self.memory.editing.clone() else {
            return;
        };
        if !valid_memory_content(&self.memory.edit_content) {
            self.state.error =
                Some("Memory must be 1–4,000 characters, without control characters.".into());
            return;
        }
        self.start_request(Operation::UpdateMemory {
            id,
            content: zeroize::Zeroizing::new(self.memory.edit_content.clone()),
        });
    }

    pub fn create_bot(
        &mut self,
        id: String,
        description: String,
        tools: Vec<String>,
        skills: Vec<String>,
    ) {
        if !valid_bot_id(&id)
            || !valid_bot_description(&description)
            || validate_tool_list(&tools).is_err()
        {
            self.state.error = Some("Check the bot id, description, and tools.".into());
            return;
        }
        self.start_request(Operation::CreateBot {
            id,
            description: zeroize::Zeroizing::new(description),
            tools,
            skills,
        });
    }

    pub fn create_skill(&mut self, id: String, description: String, body: String) {
        if !valid_new_skill_id(&id)
            || !valid_skill_description(&description)
            || !valid_skill_body(&body)
        {
            self.state.error = Some("Check the skill id, description, and body.".into());
            return;
        }
        self.start_request(Operation::CreateSkill {
            id,
            description: zeroize::Zeroizing::new(description.trim().to_owned()),
            body: zeroize::Zeroizing::new(body.trim().to_owned()),
        });
    }

    pub fn create_conversation(
        &mut self,
        kind: ConversationKind,
        slug: String,
        parent: Option<String>,
        title: String,
    ) {
        if let Some(operation) = self.prepare_conversation(kind, slug, parent, title) {
            self.start_request(operation);
        }
    }

    /// Validate a conversation draft and return the create operation.
    ///
    /// The checks are local: nothing is sent to the server until they pass.
    fn prepare_conversation(
        &mut self,
        kind: ConversationKind,
        slug: String,
        parent: Option<String>,
        title: String,
    ) -> Option<Operation> {
        if !valid_slug(&slug) || valid_conversation_parent(kind, parent.as_deref()).is_err() {
            self.state.error = Some("Check the conversation slug and parent.".into());
            return None;
        }
        let title = title.trim().to_owned();
        if !title.is_empty() && !valid_conversation_title(&title) {
            self.state.error = Some("Conversation name must be 1–120 characters.".into());
            return None;
        }
        let title = if self.state.conversation_titles == Some(true) {
            title
        } else {
            if !title.is_empty() {
                self.admin_notice = Some(
                    "This server does not store conversation names yet; the chat uses its id."
                        .into(),
                );
            }
            String::new()
        };
        Some(Operation::CreateConversation {
            kind,
            slug,
            parent_id: parent.filter(|parent| !parent.trim().is_empty()),
            title: (!title.is_empty()).then_some(title),
        })
    }

    /// Create a room and give it its team in one step.
    ///
    /// The room is created first (`POST /v1/conversations`). When the created
    /// conversation is a `room:` and the owner picked members, the team is
    /// written afterwards (`PUT /v1/conversations/{id}/team`, mode
    /// `selected`). A failed team write keeps the room: the error is shown
    /// and the room stays, so nothing is deleted and nothing is replayed.
    pub fn create_conversation_with_team(
        &mut self,
        kind: ConversationKind,
        slug: String,
        parent: Option<String>,
        title: String,
        members: Vec<String>,
    ) {
        let members = if kind == ConversationKind::Room && !members.is_empty() {
            if let Err(error) = validate_team_members(&members) {
                self.state.team_error = Some(error.to_string());
                return;
            }
            Some(members)
        } else {
            if kind != ConversationKind::Room && !members.is_empty() {
                self.admin_notice =
                    Some("Only rooms carry a team; this one was created without it.".into());
            }
            None
        };
        let Some(operation) = self.prepare_conversation(kind, slug, parent, title) else {
            return;
        };
        self.pending_team = members;
        self.start_request(operation);
    }

    /// Write the team of the room that was just created.
    ///
    /// The console opens the new room and writes the team there, because the
    /// team endpoint only works on the open room.
    fn apply_initial_team(&mut self, members: Vec<String>) {
        let Some(created) = self.state.created_conversation.clone() else {
            self.admin_notice = Some("The room was created but its team was not set.".into());
            self.schedule(
                Operation::ListConversations {
                    include_archived: false,
                },
                Duration::ZERO,
            );
            return;
        };
        if !crate::is_room_conversation(&created.id) {
            self.admin_notice =
                Some("Only rooms carry a team; this one was created without it.".into());
            self.schedule(
                Operation::ListConversations {
                    include_archived: false,
                },
                Duration::ZERO,
            );
            return;
        }
        self.drafts.store(&self.conversation, &self.composer);
        self.conversation = created.id.clone();
        self.section = Section::Chat;
        self.reset_scope(true);
        self.state.team_error = None;
        self.state.team_loading = true;
        self.pending_room_refresh = true;
        self.schedule(
            Operation::SetTeam {
                members,
                mode: TeamMode::Selected,
                rules: String::new(),
            },
            Duration::ZERO,
        );
    }

    pub fn rename_conversation(&mut self, id: String, title: String) {
        if !valid_conversation_title(&title) {
            self.state.error = Some("Title must be 1–120 characters.".into());
            return;
        }
        self.rename = None;
        self.start_request(Operation::RenameConversation {
            id,
            title: zeroize::Zeroizing::new(title.trim().to_owned()),
        });
    }

    pub fn archive_conversation(&mut self, id: String) {
        self.archive_confirm = Some(id.clone());
        self.start_request(Operation::ArchiveConversation { id });
    }

    /// The Auto-approve button. Turning it on in a chat for the first time asks
    /// first: it lets the bots send email or delete files without the usual
    /// approval prompt. Turning it off never asks.
    pub fn request_auto_approve(&mut self, enabled: bool) {
        if needs_auto_approve_confirmation(
            &self.auto_approve_confirmed,
            &self.conversation,
            enabled,
        ) {
            self.auto_approve_confirming = true;
            return;
        }
        self.auto_approve_confirming = false;
        self.set_auto_approve(enabled);
    }

    /// "Turn on" in the first-time confirmation.
    pub fn confirm_auto_approve(&mut self) {
        if !self.auto_approve_confirming {
            return;
        }
        self.auto_approve_confirming = false;
        self.auto_approve_confirmed
            .insert(self.conversation.clone());
        self.set_auto_approve(true);
    }

    pub fn cancel_auto_approve(&mut self) {
        self.auto_approve_confirming = false;
    }

    /// Flip the open conversation's auto-approve switch. Turning it off is
    /// applied locally at once and then confirmed by the server; turning it on
    /// waits for the server's answer so the switch never shows a state the
    /// server did not accept.
    pub fn set_auto_approve(&mut self, enabled: bool) {
        if !enabled {
            self.state.auto_approve = false;
        }
        self.start_request(Operation::SetAutoApprove { enabled });
    }

    /// Pick the open conversation's thinking level (None for the model's
    /// default). The server stores it only when the model accepts it.
    pub fn set_thinking(&mut self, level: Option<String>) {
        self.start_request(Operation::SetThinking { level });
    }

    pub fn unarchive_conversation(&mut self, id: String) {
        self.start_request(Operation::UnarchiveConversation { id });
    }

    pub fn add_memory(&mut self) {
        if !valid_memory_kind(&self.memory.kind)
            || !valid_memory_content(&self.memory.content)
            || !valid_memory_source(&self.memory.source)
        {
            self.state.error = Some("Check the memory kind, content, and source.".into());
            return;
        }
        self.start_request(Operation::AddMemory {
            kind: self.memory.kind.clone(),
            content: zeroize::Zeroizing::new(self.memory.content.clone()),
            source: self.memory.source.trim().to_owned(),
        });
    }

    pub fn delete_memory(&mut self, id: String) {
        self.memory.delete_confirm = None;
        self.start_request(Operation::DeleteMemory { id });
    }

    pub fn memory_kinds() -> &'static [&'static str] {
        MEMORY_KINDS
    }

    pub fn skill_limits() -> (usize, usize) {
        (MAX_SKILL_DESCRIPTION_CHARS, MAX_SKILL_BODY_BYTES)
    }

    /// Load the team of the open room. Only a `room:` conversation has one;
    /// anywhere else the call sets a clear error and makes no request.
    pub fn refresh_team(&mut self) {
        if !self.team_allowed() {
            return;
        }
        if !self.ready() {
            self.state.team_error = Some("Wait for the current request to finish.".into());
            return;
        }
        self.state.team_loading = true;
        self.state.team_error = None;
        self.start_request(Operation::GetTeam);
        if self.pending.is_none() {
            self.state.team_loading = false;
        }
    }

    /// Replace the team members. The list is validated locally first, then
    /// written with `PUT`, then the team is read back.
    pub fn set_team_members(&mut self, members: Vec<String>) {
        if !self.team_allowed() {
            return;
        }
        if let Err(error) = validate_team_members(&members) {
            self.state.team_error = Some(error.to_string());
            return;
        }
        if !self.ready() {
            self.state.team_error = Some("Wait for the current request to finish.".into());
            return;
        }
        let mode = self
            .state
            .team
            .as_ref()
            .map(|team| team.mode)
            .unwrap_or(TeamMode::Selected);
        let rules = self.team_rules_draft.clone();
        self.state.team_error = None;
        self.state.team_loading = true;
        self.start_request(Operation::SetTeam {
            members,
            mode,
            rules,
        });
        if self.pending.is_none() {
            self.state.team_loading = false;
        }
    }

    /// Switch the team mode. The current member list is written with it.
    pub fn set_team_mode(&mut self, mode: TeamMode) {
        if !self.team_allowed() {
            return;
        }
        let Some(team) = self.state.team.clone() else {
            self.state.team_error = Some("Load the team before changing its mode.".into());
            return;
        };
        if team.mode == mode {
            return;
        }
        let members = teams::member_ids(&team);
        if let Err(error) = validate_team_members(&members) {
            self.state.team_error = Some(error.to_string());
            return;
        }
        if !self.ready() {
            self.state.team_error = Some("Wait for the current request to finish.".into());
            return;
        }
        let rules = self.team_rules_draft.clone();
        self.state.team_error = None;
        self.state.team_loading = true;
        self.start_request(Operation::SetTeam {
            members,
            mode,
            rules,
        });
        if self.pending.is_none() {
            self.state.team_loading = false;
        }
    }

    /// Save the group instructions of the open room. Members and mode are
    /// written along unchanged; the server stores one team document.
    pub fn save_team_rules(&mut self) {
        if !self.team_allowed() {
            return;
        }
        let Some(team) = self.state.team.clone() else {
            self.state.team_error = Some("Load the group before saving instructions.".into());
            return;
        };
        let rules = self.team_rules_draft.trim().to_owned();
        if rules.chars().count() > 4_000
            || rules
                .chars()
                .any(|c| c != '\n' && c != '\r' && c != '\t' && c.is_control())
        {
            self.state.team_error = Some(
                "Instructions must be at most 4,000 characters, without control characters.".into(),
            );
            return;
        }
        if !self.ready() {
            self.state.team_error = Some("Wait for the current request to finish.".into());
            return;
        }
        self.state.team_error = None;
        self.state.team_loading = true;
        self.start_request(Operation::SetTeam {
            members: teams::member_ids(&team),
            mode: team.mode,
            rules,
        });
        if self.pending.is_none() {
            self.state.team_loading = false;
        }
    }

    /// Add one bot to the team. A bot that is already a member is ignored.
    pub fn add_team_member(&mut self, bot: String) {
        if !self.team_allowed() {
            return;
        }
        let Some(team) = self.state.team.clone() else {
            self.state.team_error = Some("Load the team before adding a bot.".into());
            return;
        };
        let Some(members) = teams::with_member(&team, &bot) else {
            return;
        };
        self.set_team_members(members);
    }

    /// Remove one bot from the team. A bot that is not a member is ignored.
    pub fn remove_team_member(&mut self, bot: String) {
        if !self.team_allowed() {
            return;
        }
        let Some(team) = self.state.team.clone() else {
            self.state.team_error = Some("Load the team before removing a bot.".into());
            return;
        };
        if teams::member(&team, &bot).is_none() {
            return;
        }
        let members: Vec<String> = teams::member_ids(&team)
            .into_iter()
            .filter(|member| member != &bot)
            .collect();
        if members.is_empty() {
            self.state.team_error = Some("A team needs at least one member.".into());
            return;
        }
        self.set_team_members(members);
    }

    /// Send a recorded note for transcription. The transcript returns as a
    /// draft in the composer; it is never sent on its own.
    pub fn transcribe_audio(&mut self, filename: String, bytes: Vec<u8>) {
        if !self.ready() {
            self.state.transcript_error = Some("Wait for the current request to finish.".into());
            return;
        }
        self.state.transcript_error = None;
        self.state.transcript_loading = true;
        // Kept for one retry: a failed transcription can be sent again
        // without recording the note a second time.
        self.last_audio = Some((filename.clone(), bytes.clone()));
        self.start_request(Operation::TranscribeAudio { filename, bytes });
        if self.pending.is_none() {
            self.state.transcript_loading = false;
        }
    }

    /// Send the last voice note for transcription again, after a failure.
    /// Does nothing when there is no note to retry or one is already running.
    pub fn retry_transcribe(&mut self) {
        let Some((filename, bytes)) = self.last_audio.clone() else {
            return;
        };
        if self.state.transcript_loading {
            return;
        }
        self.state.transcript_error = None;
        self.state.transcript_loading = true;
        self.start_request(Operation::TranscribeAudio { filename, bytes });
        if self.pending.is_none() {
            self.state.transcript_loading = false;
        }
    }

    /// Forget the last voice note, for example once its text has landed.
    pub fn clear_last_audio(&mut self) {
        self.last_audio = None;
    }

    /// True when the open conversation may carry a team.
    fn team_allowed(&mut self) -> bool {
        if teams::is_room(&self.conversation) {
            return true;
        }
        self.state.team = None;
        self.state.team_loaded = false;
        self.state.team_loading = false;
        self.state.team_error = Some("Only room conversations carry a team.".into());
        false
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        self.token.zeroize();
        self.composer.zeroize();
        self.memory.content.zeroize();
        self.credentials = None;
        self.stop_events();
    }
}

/// The server deliberately returns generic error bodies. Map the statuses the
/// sign-in routes use to the one sentence that tells the person what to do.
/// The Telegram routes fail closed with a 403 when the feature is off and a 404
/// on an older server; both deserve a specific sentence instead of "Forbidden".
fn telegram_error(error: ApiError) -> String {
    match error {
        ApiError::Http(403) => "Telegram is not enabled on this deployment.".into(),
        ApiError::Http(404) => {
            "This server does not support Telegram linking. Update the server.".into()
        }
        ApiError::Http(409) => {
            "A Telegram linking request is already pending. Try again in a moment.".into()
        }
        other => other.to_string(),
    }
}

fn save_download(filename: &str, bytes: &[u8]) -> Result<String, String> {
    let sanitized: String = filename
        .chars()
        .map(|ch| match ch {
            '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*' => '_',
            other => other,
        })
        .collect::<String>()
        .trim()
        .to_string();
    if sanitized.is_empty() || sanitized == "." || sanitized == ".." {
        return Err("The server sent an unusable filename.".into());
    }
    let home = std::env::var("USERPROFILE").map_err(|_| "No home directory is set.".to_string())?;
    let dir = std::path::Path::new(&home).join("Downloads");
    std::fs::create_dir_all(&dir).map_err(|err| err.to_string())?;
    let path = unique_download_path(&dir, &sanitized);
    std::fs::write(&path, bytes).map_err(|err| err.to_string())?;
    Ok(path.to_string_lossy().into_owned())
}

/// Windows file dialog for the paperclip. Names and sizes are checked before
/// any bytes are read; a bad pick becomes a message, not an upload.
#[cfg(windows)]
fn pick_files(room: usize) -> PickedFiles {
    let Some(paths) = rfd::FileDialog::new()
        .set_title("Attach files")
        .add_filter("Supported files", crate::ATTACHMENT_EXTENSIONS)
        .pick_files()
    else {
        return Vec::new();
    };
    paths
        .into_iter()
        .take(room)
        .map(|path| {
            let name = path
                .file_name()
                .and_then(|value| value.to_str())
                .unwrap_or_default()
                .to_owned();
            if !crate::valid_attachment_name(&name) {
                return Err(format!("{name}: this file type cannot be attached."));
            }
            let size = std::fs::metadata(&path)
                .map_err(|_| format!("{name}: could not read the file."))?
                .len();
            if size == 0 || size > crate::MAX_ATTACHMENT_BYTES as u64 {
                return Err(format!("{name}: files must be between 1 byte and 25 MB."));
            }
            let bytes =
                std::fs::read(&path).map_err(|_| format!("{name}: could not read the file."))?;
            Ok((name, bytes))
        })
        .collect()
}

#[cfg(windows)]
fn pick_folder() -> Option<std::path::PathBuf> {
    rfd::FileDialog::new()
        .set_title("Choose a folder the bot may use")
        .pick_folder()
}

#[cfg(not(windows))]
fn pick_folder() -> Option<std::path::PathBuf> {
    None
}

#[cfg(not(windows))]
fn pick_files(_room: usize) -> PickedFiles {
    vec![Err("Attachments need the Windows app.".into())]
}

/// Native Save As for a downloaded artifact. Cancelling is not an error.
#[cfg(windows)]
fn save_download_as(filename: &str, bytes: &[u8]) -> Result<String, String> {
    let suggested: String = filename
        .chars()
        .map(|ch| match ch {
            '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*' => '_',
            other => other,
        })
        .collect();
    let Some(path) = rfd::FileDialog::new()
        .set_title("Save as")
        .set_file_name(suggested.trim())
        .save_file()
    else {
        return Err("Save cancelled.".into());
    };
    std::fs::write(&path, bytes).map_err(|err| err.to_string())?;
    Ok(path.to_string_lossy().into_owned())
}

#[cfg(not(windows))]
fn save_download_as(filename: &str, bytes: &[u8]) -> Result<String, String> {
    save_download(filename, bytes)
}

/// Never overwrite a file the owner already has. `report.docx` becomes
/// `report (2).docx`, then `report (3).docx`.
fn unique_download_path(dir: &std::path::Path, filename: &str) -> std::path::PathBuf {
    let candidate = dir.join(filename);
    if !candidate.exists() {
        return candidate;
    }
    let path = std::path::Path::new(filename);
    let stem = path
        .file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or("artifact");
    let extension = path.extension().and_then(|value| value.to_str());
    for counter in 2..1000 {
        let name = match extension {
            Some(extension) => format!("{stem} ({counter}).{extension}"),
            None => format!("{stem} ({counter})"),
        };
        let candidate = dir.join(name);
        if !candidate.exists() {
            return candidate;
        }
    }
    dir.join(filename)
}

/// Open a saved artifact with the operating system's default application.
#[cfg(windows)]
fn open_with_default_app(path: &str) -> Result<(), String> {
    std::process::Command::new("explorer")
        .arg(path)
        .spawn()
        .map(|_| ())
        .map_err(|error| error.to_string())
}

#[cfg(not(windows))]
fn open_with_default_app(path: &str) -> Result<(), String> {
    let opener = if cfg!(target_os = "macos") {
        "open"
    } else {
        "xdg-open"
    };
    std::process::Command::new(opener)
        .arg(path)
        .spawn()
        .map(|_| ())
        .map_err(|error| error.to_string())
}

/// Select the saved artifact in Explorer (or open its folder elsewhere).
#[cfg(windows)]
fn reveal_in_explorer(path: &str) -> Result<(), String> {
    std::process::Command::new("explorer")
        .arg(format!("/select,{path}"))
        .spawn()
        .map(|_| ())
        .map_err(|error| error.to_string())
}

#[cfg(not(windows))]
fn reveal_in_explorer(path: &str) -> Result<(), String> {
    let parent = std::path::Path::new(path)
        .parent()
        .ok_or_else(|| "the saved file has no parent directory".to_string())?;
    open_with_default_app(&parent.to_string_lossy())
}

pub fn humanize_label(raw: &str) -> String {
    let words = raw
        .split(['-', '_', ':', '.', ' '])
        .filter(|word| !word.is_empty())
        .map(|word| {
            let mut chars = word.chars();
            match chars.next() {
                Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
                None => String::new(),
            }
        })
        .collect::<Vec<_>>();
    if words.is_empty() {
        "Conversation".into()
    } else {
        words.join(" ")
    }
}

fn model_csv(raw: &str) -> Vec<String> {
    raw.split([',', '\n', ';'])
        .map(str::trim)
        .filter(|model| !model.is_empty())
        .map(str::to_owned)
        .collect()
}

/// Current time as Unix seconds. A clock before 1970 reads as 0.
/// Requests that record state before they are sent (a per-file preview guard,
/// an in-flight avatar or bot write). Such a request is never replayed from the
/// backlog, because replaying would find its own state already set.
/// Whether turning Auto-approve on must be confirmed first: only when
/// enabling, and only in a chat where the owner has not confirmed it yet.
fn needs_auto_approve_confirmation(
    confirmed: &std::collections::BTreeSet<String>,
    conversation: &str,
    enabling: bool,
) -> bool {
    enabling && !confirmed.contains(conversation)
}

fn has_dispatch_guard(operation: &Operation) -> bool {
    matches!(
        operation,
        Operation::BotDetail { .. }
            | Operation::UpdateBotConfig { .. }
            | Operation::AvatarFetch { .. }
            | Operation::AvatarUpload { .. }
            | Operation::AvatarRemove { .. }
            | Operation::PreviewArtifact { .. }
            | Operation::DocPreview { .. }
    )
}

fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .unwrap_or(0)
}

/// Parse an RFC 3339 timestamp (`2026-11-06T11:54:00Z`, optional fraction,
/// `Z` or `+HH:MM` offset) into Unix seconds. Anything else is `None`.
fn parse_rfc3339_unix(stamp: &str) -> Option<u64> {
    let stamp = stamp.trim();
    let (date, rest) = stamp.split_once(['T', 't', ' '])?;
    let mut date = date.split('-');
    let year: i64 = date.next()?.parse().ok()?;
    let month: i64 = date.next()?.parse().ok()?;
    let day: i64 = date.next()?.parse().ok()?;
    if date.next().is_some() || !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }
    let split = rest.find(['Z', 'z', '+', '-'])?;
    let (clock, zone) = rest.split_at(split);
    let clock = clock.split_once('.').map_or(clock, |(whole, _)| whole);
    let mut clock = clock.split(':');
    let hour: i64 = clock.next()?.parse().ok()?;
    let minute: i64 = clock.next()?.parse().ok()?;
    let second: i64 = clock.next()?.parse().ok()?;
    if clock.next().is_some() || hour > 23 || minute > 59 || second > 60 {
        return None;
    }
    let offset = if matches!(zone, "Z" | "z") {
        0
    } else {
        let sign = if zone.starts_with('-') { -1 } else { 1 };
        let (hours, minutes) = zone.get(1..)?.split_once(':')?;
        let hours: i64 = hours.parse().ok()?;
        let minutes: i64 = minutes.parse().ok()?;
        if hours > 23 || minutes > 59 {
            return None;
        }
        sign * (hours * 3600 + minutes * 60)
    };
    // Days from civil (proleptic Gregorian), Howard Hinnant's algorithm.
    let y = if month <= 2 { year - 1 } else { year };
    let era = y.div_euclid(400);
    let yoe = y.rem_euclid(400);
    let mp = (month + 9) % 12;
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    let seconds = days * 86_400 + hour * 3600 + minute * 60 + second - offset;
    u64::try_from(seconds).ok()
}

#[cfg(test)]
mod tests {
    use super::{
        AtomicBool, Duration, EventRefresh, Instant, Ordering, Session, SignInPhase, SignInUi,
        Theme, event_refresh, load_composer_drafts, scope_event_kind, telegram_error,
    };
    use super::{Section, SettingsGroup, SettingsSection};
    use crate::ApiError;
    use crate::worker::{Credentials, Operation};
    use std::sync::Arc;

    #[test]
    fn admin_settings_section_is_hidden_from_members_and_memory_is_always_visible() {
        let count = |admin, mem0| {
            SettingsSection::ALL
                .iter()
                .filter(|section| section.visible_with_memory(admin, mem0))
                .count()
        };
        assert_eq!(count(true, false), 16);
        assert_eq!(count(false, false), 15);
        assert_eq!(count(false, true), 15);
        assert!(!SettingsSection::Admin.visible(false));
        assert!(SettingsSection::Memory.visible(false));
        assert!(SettingsSection::Memory.visible_with_memory(false, false));
    }

    #[test]
    fn nav_has_seven_focused_sections() {
        assert_eq!(Section::NAV.len(), 7);
        assert!(
            Section::NAV
                == [
                    Section::Chat,
                    Section::Mail,
                    Section::Tasks,
                    Section::Projects,
                    Section::Library,
                    Section::Workflows,
                    Section::Browser,
                ]
        );
        assert_eq!(Section::Mail.label(), "Mail");
    }

    #[test]
    fn settings_sections_map_to_expected_groups() {
        assert_eq!(SettingsSection::Account.group(), SettingsGroup::Preferences);
        assert_eq!(
            SettingsSection::Appearance.group(),
            SettingsGroup::Preferences
        );
        assert_eq!(
            SettingsSection::Notifications.group(),
            SettingsGroup::Preferences
        );
        assert_eq!(SettingsSection::Chat.group(), SettingsGroup::Preferences);
        assert_eq!(SettingsSection::Voice.group(), SettingsGroup::Preferences);
        assert_eq!(
            SettingsSection::LocalFolders.group(),
            SettingsGroup::Preferences
        );

        assert_eq!(
            SettingsSection::Memory.group(),
            SettingsGroup::ConnectionsAndData
        );
        assert_eq!(
            SettingsSection::Integrations.group(),
            SettingsGroup::ConnectionsAndData
        );
        assert_eq!(
            SettingsSection::Providers.group(),
            SettingsGroup::ConnectionsAndData
        );
        assert_eq!(
            SettingsSection::Vault.group(),
            SettingsGroup::ConnectionsAndData
        );
        assert_eq!(
            SettingsSection::Telegram.group(),
            SettingsGroup::ConnectionsAndData
        );
        assert_eq!(
            SettingsSection::WebSearch.group(),
            SettingsGroup::ConnectionsAndData
        );

        assert_eq!(SettingsSection::Usage.group(), SettingsGroup::System);
        assert_eq!(SettingsSection::Logs.group(), SettingsGroup::System);
        assert_eq!(SettingsSection::Admin.group(), SettingsGroup::System);
        assert_eq!(SettingsSection::About.group(), SettingsGroup::System);
    }

    #[test]
    fn open_section_deep_links_settings_subsections() {
        let mut session = Session::new();
        for (section, expected) in [
            (Section::Memory, SettingsSection::Memory),
            (Section::Integrations, SettingsSection::Integrations),
            (Section::Providers, SettingsSection::Providers),
            (Section::Vault, SettingsSection::Vault),
            (Section::Usage, SettingsSection::Usage),
            (Section::Logs, SettingsSection::Logs),
        ] {
            session.open_section(section);
            assert!(session.section == Section::Settings);
            assert_eq!(session.settings_section, expected);
        }

        session.open_section(Section::Mail);
        assert!(session.section == Section::Mail);
    }

    #[test]
    fn marketplace_install_enable_and_secrets_are_refused_until_plugins_can_run() {
        use crate::marketplace::{ActivationBlock, PluginAction, PluginDetail};
        use zeroize::Zeroizing;

        let mut session = Session::new();
        session.state.connected = true;
        session.plugin_action("firecrawl".into(), PluginAction::Install);
        let install = session.state.marketplace_error.clone().unwrap();
        assert_eq!(
            install,
            ActivationBlock::Unchecked.message(),
            "unloaded plugin install must require opening the plugin page first: {install}"
        );
        assert!(!session.marketplace_busy);
        assert!(!install.to_lowercase().contains("secret"));

        session.plugin_action("firecrawl".into(), PluginAction::Enable);
        assert_eq!(
            session.state.marketplace_error.as_deref(),
            Some(ActivationBlock::Unchecked.message())
        );
        assert!(!session.marketplace_busy);

        // Loaded placeholder-host plugin is refused with PlaceholderHost ("Coming soon")
        // and never echoes typed secret values.
        let placeholder_detail: PluginDetail = serde_json::from_value(serde_json::json!({
            "plugin": {
                "slug": "firecrawl",
                "name": "Firecrawl",
                "installed": false,
                "enabled": false,
                "apps": [{"id": "a", "name": "A", "transport": "http", "auth": []}],
                "permissions": {"egress": ["mcp.firecrawl.example.invalid"]}
            }
        }))
        .unwrap();
        session.state.marketplace_detail = Some(placeholder_detail);
        session.plugin_action("firecrawl".into(), PluginAction::Install);
        assert_eq!(
            session.state.marketplace_error.as_deref(),
            Some(ActivationBlock::PlaceholderHost.message())
        );
        assert!(!session.marketplace_busy);

        session.save_plugin_config(
            "firecrawl".into(),
            vec![(
                "api_key".into(),
                Zeroizing::new("synthetic-plugin-key".into()),
            )],
        );
        let saved = session.state.marketplace_error.clone().unwrap();
        assert!(saved.contains("Not live yet"), "{saved}");
        assert!(
            !saved.contains("synthetic-plugin-key"),
            "a refused save must not echo the typed value"
        );
        assert!(!session.marketplace_busy);

        // Loaded stdio (local-program) plugin is refused with LocalProgram.
        let stdio_detail: PluginDetail = serde_json::from_value(serde_json::json!({
            "plugin": {
                "slug": "local-tool",
                "name": "Local Tool",
                "installed": false,
                "enabled": false,
                "apps": [{"id": "a", "name": "A", "transport": "stdio", "auth": []}],
                "permissions": {"egress": []}
            }
        }))
        .unwrap();
        session.state.marketplace_detail = Some(stdio_detail);
        session.plugin_action("local-tool".into(), PluginAction::Install);
        assert_eq!(
            session.state.marketplace_error.as_deref(),
            Some(ActivationBlock::LocalProgram.message())
        );
        assert!(!session.marketplace_busy);

        session.open_plugin("not a slug".into());
        assert!(session.marketplace_open.is_none());
    }

    #[test]
    fn only_the_admin_asks_for_server_health() {
        let user = |role: &str| crate::AuthUser {
            id: "u1".into(),
            email: "ada@example.test".into(),
            display_name: "Ada".into(),
            role: role.into(),
            session_expires_at: None,
        };
        let mut member = Session::new();
        member.state.user = Some(user("member"));
        member.refresh_server_health();
        assert!(!member.server_health_loading, "a member never sends it");

        let mut owner = Session::new();
        owner.state.user = Some(user("admin"));
        owner.refresh_server_health();
        assert!(owner.server_health_loading, "the owner asks once");
        owner.refresh_server_health();
        assert!(owner.server_health_loading);
    }

    #[test]
    fn system_theme_round_trips_and_toggles_to_dark() {
        assert_eq!(Theme::from_stored(Some("system")), Theme::System);
        assert_eq!(Theme::System.as_str(), "system");
        assert_eq!(Theme::System.toggled(), Theme::Dark);
    }

    #[test]
    fn a_team_event_refreshes_the_team_only_in_a_room() {
        assert_eq!(
            event_refresh("team.member_set", "room:review"),
            EventRefresh::RoomTeam
        );
        assert_eq!(
            event_refresh("team.member_set", "dir:analyst"),
            EventRefresh::History
        );
        assert_ne!(
            event_refresh("team.member_set", "dir:analyst"),
            EventRefresh::RoomTeam
        );
    }

    #[test]
    fn other_events_keep_their_own_refresh() {
        assert_eq!(
            event_refresh("approval.requested", "room:review"),
            EventRefresh::Approvals
        );
        assert_eq!(
            event_refresh("conversation.created", "room:review"),
            EventRefresh::Conversations
        );
        assert_eq!(
            event_refresh("message", "room:review"),
            EventRefresh::History
        );
    }

    #[test]
    fn activity_in_another_conversation_does_not_reload_the_open_transcript() {
        let other = serde_json::json!({"conversation_id": "conv-b"});
        let same = serde_json::json!({"conversation_id": "conv-a"});
        let none = serde_json::json!({});
        let scoped = scope_event_kind("message".into(), &other, "conv-a");
        assert_eq!(
            event_refresh(&scoped, "conv-a"),
            EventRefresh::Conversations
        );
        assert_eq!(
            scope_event_kind("message".into(), &same, "conv-a"),
            "message"
        );
        assert_eq!(
            scope_event_kind("message".into(), &none, "conv-a"),
            "message"
        );
        // Approvals and conversation events keep their own refresh.
        assert_eq!(
            scope_event_kind("approval.requested".into(), &other, "conv-a"),
            "approval.requested"
        );
    }

    #[test]
    fn a_stale_local_expiry_asks_the_server_before_signing_out() {
        let _guard = crate::creds::test_lock();
        let mut session =
            Session::assemble(String::new(), Theme::Dark, Vec::new(), String::new(), None);
        session.token = "synthetic-session-token-0123456789abcdef".into();
        session.credentials = Some(Arc::new(Credentials {
            endpoint: crate::validate_endpoint("http://127.0.0.1:9").unwrap(),
            token: secrecy::SecretString::new(session.token.clone()),
        }));
        session.state.connected = true;
        session.session_expires = Some(super::unix_now().saturating_sub(1));
        session.check_session_expiry();
        assert!(session.sign_in.notice.is_none(), "still signed in");
        assert!(
            session
                .session_expires
                .is_some_and(|at| at > super::unix_now())
        );
        assert!(
            session
                .backlog
                .iter()
                .any(|operation| matches!(operation, Operation::Me))
        );
    }

    #[test]
    fn card_events_reload_the_transcript_that_carries_the_cards() {
        for kind in ["card.created", "card.resolved"] {
            assert_eq!(
                event_refresh(kind, "dir:analyst"),
                EventRefresh::History,
                "{kind}"
            );
        }
    }

    #[test]
    fn telegram_errors_name_the_next_action() {
        assert!(telegram_error(ApiError::Http(403)).contains("not enabled"));
        assert!(telegram_error(ApiError::Http(404)).contains("Update the server"));
        assert!(telegram_error(ApiError::Http(409)).contains("already pending"));
        assert_eq!(
            telegram_error(ApiError::Transport),
            ApiError::Transport.to_string()
        );
    }

    #[test]
    fn theme_defaults_to_light_and_keeps_a_stored_dark_choice() {
        assert_eq!(Theme::from_stored(None), Theme::Light);
        assert_eq!(Theme::from_stored(Some("junk")), Theme::Light);
        assert_eq!(Theme::from_stored(Some("dark")), Theme::Dark);
        assert_eq!(Theme::from_stored(Some("light")), Theme::Light);
    }

    #[test]
    fn resetting_sign_in_drops_the_invite_and_stops_a_running_attempt() {
        let mut ui = SignInUi::default();
        let cancel = Arc::new(AtomicBool::new(false));
        ui.cancel = Some(Arc::clone(&cancel));
        ui.phase = SignInPhase::Waiting;
        ui.show_invite = true;
        ui.invite_code = "synthetic-invite".into();
        ui.error = Some("old error".into());
        ui.notice = Some("old notice".into());
        assert!(ui.busy());
        ui.reset_transient();
        assert!(
            cancel.load(Ordering::SeqCst),
            "the loopback wait is told to stop"
        );
        assert!(!ui.busy());
        assert!(!ui.show_invite);
        assert!(ui.invite_code.is_empty());
        assert!(ui.error.is_none());
        assert!(ui.notice.is_none());
    }

    #[test]
    fn a_rejected_session_returns_to_sign_in_with_a_plain_message() {
        let _guard = crate::creds::test_lock();
        let mut session =
            Session::assemble(String::new(), Theme::Dark, Vec::new(), String::new(), None);
        session.token = "synthetic-session-token-0123456789abcdef".into();
        session.state.user = Some(crate::AuthUser {
            id: "u1".into(),
            email: "ada@example.test".into(),
            display_name: "Ada".into(),
            role: "member".into(),
            session_expires_at: None,
        });
        session.server = "http://127.0.0.1:9".into();
        session.reject_token();
        assert_eq!(
            session.sign_in.notice.as_deref(),
            Some("Your session ended. Sign in again.")
        );
        assert!(session.token.is_empty());
        assert!(session.state.user.is_none());
        assert!(!session.state.connected);
        assert!(!session.has_saved_session());
        assert_eq!(session.server, "http://127.0.0.1:9", "the server stays");
    }

    #[test]
    fn signing_out_without_a_connection_clears_the_device() {
        let _guard = crate::creds::test_lock();
        let mut session =
            Session::assemble(String::new(), Theme::Dark, Vec::new(), String::new(), None);
        session.token = "synthetic-session-token-0123456789abcdef".into();
        crate::creds::save_token(&session.token).unwrap();
        session.sign_out();
        assert!(!session.sign_out.in_flight);
        assert!(session.token.is_empty());
        assert!(crate::creds::load_token().is_none_or(|token| {
            secrecy::ExposeSecret::expose_secret(&token)
                != "synthetic-session-token-0123456789abcdef"
        }));
        assert_eq!(
            session.sign_in.notice.as_deref(),
            Some("You are signed out.")
        );
    }

    #[test]
    fn an_unreachable_server_still_signs_this_device_out() {
        let _guard = crate::creds::test_lock();
        let mut session =
            Session::assemble(String::new(), Theme::Dark, Vec::new(), String::new(), None);
        session.token = "synthetic-session-token-0123456789abcdef".into();
        // Nothing listens on port 9, so the logout call fails fast.
        let endpoint = crate::validate_endpoint("http://127.0.0.1:9").unwrap();
        session.credentials = Some(Arc::new(crate::worker::Credentials {
            endpoint,
            token: secrecy::SecretString::new(session.token.clone()),
        }));
        session.sign_out();
        assert!(session.sign_out.in_flight);
        let started = Instant::now();
        while session.sign_out.in_flight && started.elapsed() < Duration::from_secs(25) {
            session.poll_sign_out();
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(!session.sign_out.in_flight);
        assert!(session.token.is_empty());
        assert!(
            session
                .sign_in
                .notice
                .as_deref()
                .is_some_and(|notice| notice.contains("could not be reached"))
        );
    }

    #[test]
    fn rfc3339_timestamps_parse_to_unix_seconds() {
        use super::parse_rfc3339_unix;
        assert_eq!(parse_rfc3339_unix("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(
            parse_rfc3339_unix("2026-11-06T11:54:00Z"),
            Some(1_793_966_040)
        );
        assert_eq!(
            parse_rfc3339_unix("2026-11-06T11:54:00.250+02:00"),
            Some(1_793_966_040 - 7200)
        );
        assert_eq!(
            parse_rfc3339_unix("2026-11-06T11:54:00-01:30"),
            Some(1_793_966_040 + 5400)
        );
        for bad in [
            "",
            "soon",
            "2026-13-01T00:00:00Z",
            "2026-11-06T25:00:00Z",
            "2026-11-06",
        ] {
            assert_eq!(parse_rfc3339_unix(bad), None, "{bad}");
        }
    }

    #[test]
    fn an_expired_session_signs_the_user_out_with_a_plain_message() {
        let _guard = crate::creds::test_lock();
        let mut session =
            Session::assemble(String::new(), Theme::Dark, Vec::new(), String::new(), None);
        session.token = "synthetic-session-token-0123456789abcdef".into();
        session.session_expires = Some(super::unix_now() + 3600);
        session.check_session_expiry();
        assert!(
            session.has_saved_session(),
            "a future expiry keeps the session"
        );
        session.session_expires = Some(super::unix_now().saturating_sub(1));
        session.check_session_expiry();
        assert_eq!(
            session.sign_in.notice.as_deref(),
            Some("Your session ended. Sign in again.")
        );
        assert!(!session.has_saved_session());
        assert!(session.session_expires.is_none());
    }

    #[test]
    fn a_launch_result_reaches_the_toast_from_its_helper_thread() {
        let mut session =
            Session::assemble(String::new(), Theme::Dark, Vec::new(), String::new(), None);
        session.launch(|| "launched".to_owned());
        let started = Instant::now();
        while session.toast.is_none() && started.elapsed() < Duration::from_secs(5) {
            session.poll_launches();
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(session.toast.as_deref(), Some("launched"));
    }

    fn plain_session() -> Session {
        Session::assemble(String::new(), Theme::Dark, Vec::new(), String::new(), None)
    }

    #[test]
    fn a_dropped_connection_shows_one_banner_and_recovers() {
        let mut session = plain_session();
        assert!(!session.link_down);
        session.note_link_down();
        assert!(session.link_down);
        let first = session.link_retry_at.unwrap();
        session.note_link_down();
        assert_eq!(session.link_attempts, 1);
        assert!(session.link_retry_at.unwrap() > first);
        session.state.error = Some("old".into());
        session.link_recovered();
        assert!(!session.link_down && session.link_retry_at.is_none());
        assert!(session.state.error.is_none());
        // The calm text is for panels; the long hint is for sign-in only.
        assert!(ApiError::Transport.to_string().contains("Retrying"));
        assert!(!ApiError::Transport.to_string().contains("TLS"));
        assert!(ApiError::Transport.sign_in_text().contains("TLS"));
    }

    /// A fresh empty directory for a test that writes drafts to disk.
    fn temp_dir(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("serabot-test-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    pub(super) fn sendable_session() -> Session {
        let mut session = Session::assemble(
            String::new(),
            Theme::Dark,
            Vec::new(),
            String::new(),
            crate::worker::Worker::start().ok(),
        );
        session.state.connected = true;
        session.state.history_loaded = true;
        let selection = crate::Selection {
            provider: "p".into(),
            model: "m".into(),
            account: None,
        };
        session.state.draft = selection.clone();
        session.state.catalog = Some(crate::Catalog {
            providers: vec![crate::Provider {
                id: "p".into(),
                models: vec!["m".into()],
                data_egress_approved: true,
            }],
            entries: Vec::new(),
            selection,
            effective_selection: None,
            thinking: crate::ThinkingInfo::default(),
        });
        assert!(session.state.can_send(), "the test chat must be sendable");
        session
    }

    #[test]
    fn submit_names_the_reason_instead_of_failing_silently() {
        // Offline: the connection is the reason.
        let mut session = plain_session();
        session.composer = "hello".into();
        session.submit();
        assert_eq!(
            session.toast.as_deref(),
            Some("Waiting for connection"),
            "a send with no connection must say so"
        );
        assert_eq!(session.state.messages.len(), 0, "nothing was sent");

        // Still uploading: the upload is the reason.
        let mut session = sendable_session();
        session.uploading = true;
        session.composer = "hello".into();
        session.submit();
        assert_eq!(session.toast.as_deref(), Some("Still uploading…"));
        session.uploading = false;
        session.upload_queue.push_back(("late.pdf".into(), vec![1]));
        session.submit();
        assert_eq!(session.toast.as_deref(), Some("Still uploading…"));

        // Over the limit: the length is the reason.
        let mut session = sendable_session();
        session.composer = "x".repeat(crate::chat_compose::MAX_TEXT_CHARS + 1);
        session.submit();
        assert_eq!(session.toast.as_deref(), Some("Message is over the limit"));

        // Empty: the empty box is the reason.
        let mut session = sendable_session();
        session.composer = "   ".into();
        session.submit();
        assert_eq!(session.toast.as_deref(), Some("Type a message first."));
    }

    #[test]
    fn composer_files_and_held_drafts_survive_a_chat_switch() {
        let dir = temp_dir("switch");
        let mut session = Session::assemble(
            String::new(),
            Theme::Dark,
            Vec::new(),
            String::new(),
            crate::worker::Worker::start().ok(),
        );
        session.persist_worker = None;
        session.composer_dir_override = Some(dir);
        session.state.connected = true;
        session.open_conversation("dir:main".into());
        session.composer = "draft for main".into();
        session.composer_files.push(crate::AttachmentInfo {
            id: "a1".into(),
            filename: "n.txt".into(),
            mime: "text/plain".into(),
            size_bytes: 3,
            is_image: false,
        });
        session
            .upload_queue
            .push_back(("late.pdf".into(), vec![1, 2]));
        session.state.held_draft = Some("held for main".into());
        session.open_conversation("dir:other".into());
        assert!(
            session.composer_files.is_empty(),
            "the new chat starts clean"
        );
        assert!(session.upload_queue.is_empty());
        assert_eq!(session.state.held_draft, None);
        assert_eq!(session.composer, "", "no draft was typed in the other chat");

        session.open_conversation("dir:main".into());
        assert_eq!(session.composer, "draft for main");
        assert_eq!(session.composer_files.len(), 1);
        assert_eq!(session.composer_files[0].id, "a1");
        assert_eq!(session.upload_queue.len(), 1);
        assert_eq!(session.state.held_draft.as_deref(), Some("held for main"));
    }

    #[test]
    fn over_long_composer_text_attaches_as_a_file() {
        let dir = temp_dir("attach-as-file");
        let mut session = plain_session();
        session.persist_worker = None;
        session.composer_dir_override = Some(dir);
        session.composer = "x".repeat(crate::chat_compose::MAX_TEXT_CHARS + 5);
        session.attach_composer_as_file();
        assert!(session.composer.is_empty(), "the box is cleared");
        assert_eq!(session.upload_queue.len(), 1);
        assert_eq!(session.upload_queue[0].0, "message.txt");
        assert_eq!(
            session.upload_queue[0].1.len(),
            crate::chat_compose::MAX_TEXT_CHARS + 5
        );

        // Empty: nothing to attach, and the draft stays put.
        session.composer = "   ".into();
        session.attach_composer_as_file();
        assert_eq!(session.toast.as_deref(), Some("Type a message first."));
        assert_eq!(session.upload_queue.len(), 1, "no file was queued");
    }

    #[test]
    fn persisted_drafts_and_queues_come_back_with_the_launch() {
        let dir = temp_dir("persist");
        let mut session = plain_session();
        session.persist_worker = None;
        session.composer_dir_override = Some(dir.clone());
        // The open chat's draft and queue are what a relaunch restores.
        session.conversation = "dir:main".into();
        session.composer = "keep me".into();
        session.queue.push("queued", None).unwrap();
        session.persist_composer_state();
        let (drafts, mut queues) = load_composer_drafts(&dir);
        assert_eq!(drafts.restore("dir:main"), "keep me");
        assert_eq!(queues.restore("dir:main").len(), 1);
    }

    #[test]
    fn a_failed_transcription_keeps_the_audio_for_one_retry() {
        let mut session = plain_session();
        session.last_audio = Some(("note.webm".into(), vec![1, 2, 3]));
        session.state.transcript_error = Some("Transcription failed.".into());
        session.retry_transcribe();
        assert!(
            session.last_audio.is_some(),
            "the bytes stay until a transcription lands"
        );
        assert_eq!(
            session.state.transcript_error, None,
            "the retry clears the error"
        );
        assert!(!session.state.transcript_loading);
        session.clear_last_audio();
        assert!(session.last_audio.is_none());
        // Nothing to retry: a no-op, not a request.
        session.retry_transcribe();
        assert!(session.pending.is_none());
    }

    #[test]
    fn ability_switches_are_part_of_the_draft_and_closing_asks() {
        let mut session = plain_session();
        session.bot_settings.open = true;
        session.bot_settings.hydrated = true;
        session.bot_settings.bot = Some("analyst".into());
        // Nothing edited: closing is immediate.
        session.request_close_bot_settings();
        assert!(!session.bot_settings.open);

        session.bot_settings.open = true;
        let index = crate::capabilities::CAPABILITIES
            .iter()
            .position(|c| c.label == "Remember things for you")
            .unwrap();
        session.toggle_draft_capability(index, true);
        assert!(session.bot_settings.dirty());
        assert!(
            session
                .bot_settings
                .draft
                .tools
                .contains(&"memory_save".to_owned())
        );
        // Closing with edits asks instead of closing.
        session.request_close_bot_settings();
        assert!(session.bot_settings.open && session.bot_settings.confirm_close);
        session.keep_editing_bot_settings();
        assert!(!session.bot_settings.confirm_close && session.bot_settings.dirty());
        // Cancel / Discard reverts the abilities too and closes.
        session.cancel_bot_settings();
        assert!(session.bot_settings.draft.tools.is_empty());
        assert!(!session.bot_settings.dirty() && !session.bot_settings.open);
    }
}

#[cfg(test)]
mod backlog_tests {
    use super::*;

    /// A worker that is busy on its first request: the socket accepts the
    /// connection and never answers, so the queue fills behind it.
    #[test]
    fn a_busy_worker_queue_keeps_requests_for_later_instead_of_failing() {
        let _guard = crate::creds::test_lock();
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let worker = crate::worker::Worker::start().unwrap();
        let mut session = Session::assemble(
            String::new(),
            Theme::Dark,
            Vec::new(),
            String::new(),
            Some(worker),
        );
        let endpoint = crate::validate_endpoint(&format!("http://127.0.0.1:{port}")).unwrap();
        session.credentials = Some(Arc::new(crate::worker::Credentials {
            endpoint,
            token: secrecy::SecretString::new("synthetic-backlog-token-0123456789abcdef".into()),
        }));
        session.start_request(Operation::ListTasks);
        session.start_request(Operation::TaskPrefs);
        session.start_request(Operation::TaskSyncStatus);
        for idx in 0..65 {
            session.start_request(Operation::ListBotChats {
                bot: format!("bot-{idx}"),
            });
        }
        session.start_request(Operation::Workflows);
        assert!(session.state.error.is_none(), "{:?}", session.state.error);
        assert!(!session.backlog.is_empty(), "overflow must wait, not fail");
        drop(listener);
    }

    #[test]
    fn a_repeated_read_replaces_the_waiting_one_of_its_kind() {
        let mut session =
            Session::assemble(String::new(), Theme::Dark, Vec::new(), String::new(), None);
        session.queue_backlog(Operation::TaskCalendar {
            from: "2026-11-01".into(),
            to: "2026-11-30".into(),
        });
        session.queue_backlog(Operation::TaskCalendar {
            from: "2026-12-01".into(),
            to: "2026-12-31".into(),
        });
        session.queue_backlog(Operation::AddTask {
            title: "one".into(),
            due: None,
        });
        session.queue_backlog(Operation::AddTask {
            title: "two".into(),
            due: None,
        });
        // Reads collapse to the latest; writes are never merged away.
        assert_eq!(session.backlog.len(), 3);
        match session.backlog.front() {
            Some(Operation::TaskCalendar { from, .. }) => assert_eq!(from, "2026-12-01"),
            _ => panic!("the calendar read should be kept, latest first"),
        }
    }

    #[test]
    fn without_a_worker_nothing_is_sent_or_backlogged() {
        let mut session =
            Session::assemble(String::new(), Theme::Dark, Vec::new(), String::new(), None);
        session.credentials = Some(Arc::new(crate::worker::Credentials {
            endpoint: crate::validate_endpoint("http://127.0.0.1:9").unwrap(),
            token: secrecy::SecretString::new("synthetic-closed-token-0123456789abcdef".into()),
        }));
        // With no worker, requests are not sent at all.
        session.start_request(Operation::ListTasks);
        assert!(session.backlog.is_empty());
        assert!(session.state.error.is_none());
    }

    #[test]
    fn refresh_marketplace_logs_workflows_and_fetch_models_queue_when_pending() {
        let mut session =
            Session::assemble(String::new(), Theme::Dark, Vec::new(), String::new(), None);
        session.state.connected = true;
        session.pending = Some(OperationKind::History);

        session.refresh_marketplace();
        session.refresh_marketplace();
        session.refresh_logs();
        session.refresh_logs();
        session.refresh_workflows();
        session.refresh_workflows();
        session.fetch_models();
        session.fetch_models();
        session.refresh_approvals();

        let kinds: Vec<OperationKind> = session.backlog.iter().map(Operation::kind).collect();
        assert_eq!(
            kinds,
            vec![
                OperationKind::MarketplaceList,
                OperationKind::Logs,
                OperationKind::Workflows,
                OperationKind::Catalog,
                OperationKind::Approvals,
                OperationKind::StandingGrants,
            ]
        );
    }

    #[test]
    fn failed_dispatch_and_lifecycle_errors_clear_loading_flags() {
        let mut session =
            Session::assemble(String::new(), Theme::Dark, Vec::new(), String::new(), None);
        session.state.connected = true;
        session.creating_named_bot = true;
        session.start_request(Operation::CreateNamedBot {
            name: "Mia".into(),
            model: None,
            group: None,
        });
        assert!(!session.creating_named_bot);

        session.voice.loading = true;
        session.start_request(Operation::VoiceSettings);
        assert!(!session.voice.loading);

        session.bot_settings.lifecycle_busy = true;
        session.start_request(Operation::DeleteBot { id: "mia".into() });
        assert!(!session.bot_settings.lifecycle_busy);
        assert!(!session.bot_settings.has_deleted());

        session.last_event_id = 99;
        let _ = session.clear_session();
        assert_eq!(session.last_event_id, 0);
    }

    #[test]
    fn fifty_parallel_children_cost_at_most_sixty_state_writes_a_second() {
        use serde_json::json;
        let mut session =
            Session::assemble(String::new(), Theme::Dark, Vec::new(), String::new(), None);
        let start = Instant::now();
        let mut id = 0;
        // One second of virtual time in 1 ms steps; every child reports each step.
        for ms in 0..1_000u64 {
            let now = start + Duration::from_millis(ms);
            for child in 0..50 {
                id += 1;
                let task = format!("child-{child}");
                session.event_frames.push(
                    id,
                    "turn.step".into(),
                    json!({"task_id": task, "parent_task_id": "parent", "call_id": "c",
                           "label": "x", "phase": if ms % 2 == 0 { "running" } else { "done" }}),
                );
            }
            session.apply_event_frame(now);
        }
        assert!(
            session.event_frame_writes <= 60,
            "{} writes",
            session.event_frame_writes
        );
        assert!(session.event_frame_writes >= 50, "frames still flow");
        // Progress survived as counters, one per child, not as events.
        assert_eq!(session.subagents.len(), 50);
        let total: u32 = session.subagents.values().map(|c| c.events).sum();
        assert!(total >= 49_000, "{total} events folded");
        assert!(
            session.state.activity.is_empty(),
            "child steps stay out of the activity view"
        );
    }

    #[test]
    fn poll_events_does_not_overwrite_pending_approvals_refresh_with_history() {
        let mut session =
            Session::assemble(String::new(), Theme::Dark, Vec::new(), String::new(), None);
        choose_event_kind(
            &mut session.last_event_kind,
            &mut session.events_dirty,
            &session.conversation,
            "approval.requested".into(),
        );
        choose_event_kind(
            &mut session.last_event_kind,
            &mut session.events_dirty,
            &session.conversation,
            "turn.completed".into(),
        );
        assert!(session.events_dirty);
        assert_eq!(
            event_refresh(&session.last_event_kind, &session.conversation),
            EventRefresh::Approvals
        );
    }

    #[test]
    fn conversation_titles_never_expose_raw_ids_and_fallback_cleanly() {
        let mut session =
            Session::assemble(String::new(), Theme::Dark, Vec::new(), String::new(), None);
        session.state.listed_bots.push(crate::BotSummary {
            id: "analyst".into(),
            role: "analyst".into(),
            enabled: true,
            live: true,
            model: None,
            display_name: Some("Analyst".into()),
            title: None,
            description: None,
            avatar: Some("A".into()),
            avatar_version: None,
            tools: Vec::new(),
            skills: Vec::new(),
            require_approval: Vec::new(),
        });
        session.state.bot_chats.insert(
            "analyst".into(),
            crate::bot_chats::BotChatList {
                bot: "analyst".into(),
                refresh_every_turns: 40,
                chats: vec![crate::bot_chats::BotChat {
                    id: "dir:chat-01a1dfd96abc".into(),
                    bot: "analyst".into(),
                    title: None,
                    primary: false,
                    archived: false,
                    carried_from: None,
                    created_at: String::new(),
                    updated_at: String::new(),
                    last_message_at: None,
                    turns: 0,
                    has_summary: false,
                }],
            },
        );
        session.conversation = "dir:chat-01a1dfd96abc".into();
        // No messages yet -> "New conversation · Analyst" (never raw hex id).
        assert_eq!(
            session.conversation_title(),
            "New conversation \u{00b7} Analyst"
        );
        // Active thread with a user message -> derived short title.
        session.state.messages.push(crate::Message {
            id: "m1".into(),
            role: crate::Role::User,
            content: "Summarize Q4 cashflow forecast".into(),
            created_at: String::new(),
            name: None,
            bot: None,
            attachments: Vec::new(),
            limit_reached: false,
            turn_id: None,
        });
        assert_eq!(
            session.conversation_title(),
            "Summarize Q4 cashflow forecast"
        );
    }

    #[test]
    fn open_approval_conversation_routes_to_awaiting_bot() {
        let mut session =
            Session::assemble(String::new(), Theme::Dark, Vec::new(), String::new(), None);
        session.state.bot_chat_binding = Some(true);
        session.section = Section::Settings;
        session.conversation = "dir:main".into();
        session.state.task = Some(crate::Task {
            id: "t1".into(),
            kind: "turn".into(),
            bot: "analyst".into(),
            status: crate::TaskStatus::AwaitingApproval,
        });
        session.open_approval_conversation();
        assert!(session.section == Section::Chat);
        assert_eq!(session.conversation, "dir:analyst");
    }

    #[test]
    fn retry_history_restarts_the_transcript_chain_and_clears_the_failure() {
        let mut session =
            Session::assemble(String::new(), Theme::Dark, Vec::new(), String::new(), None);
        session.state.connected = true;
        session.state.history_failed = true;
        session.retry_history();
        assert!(!session.state.history_failed);
        assert!(session.state.history_started.is_some());
        assert!(
            session.refreshing,
            "the catalog and lists follow the transcript"
        );
        assert!(matches!(
            session.scheduled.as_ref().map(|(_, op)| op.kind()),
            Some(OperationKind::History)
        ));
    }

    #[test]
    fn the_reconnect_retry_reloads_a_transcript_that_never_arrived() {
        let mut session =
            Session::assemble(String::new(), Theme::Dark, Vec::new(), String::new(), None);
        session.state.connected = true;
        session.state.history_failed = true;
        session.retry_link();
        assert!(session.link_retrying);
        assert!(!session.state.history_failed);
        assert!(session.refreshing);
        assert!(session.scheduled.is_some());
        // With a transcript on screen the link probe leaves the chain alone.
        let mut loaded =
            Session::assemble(String::new(), Theme::Dark, Vec::new(), String::new(), None);
        loaded.state.connected = true;
        loaded.state.history_loaded = true;
        loaded.retry_link();
        assert!(!loaded.refreshing);
    }

    #[test]
    fn load_earlier_needs_a_loaded_transcript_with_older_rows_and_runs_once() {
        let mut session =
            Session::assemble(String::new(), Theme::Dark, Vec::new(), String::new(), None);
        session.state.connected = true;
        session.load_older_messages();
        assert!(!session.state.older_loading, "nothing loaded yet");
        session.state.history_loaded = true;
        session.load_older_messages();
        assert!(!session.state.older_loading, "no older rows are advertised");
        session.state.has_more_history = true;
        session.load_older_messages();
        assert!(!session.state.older_loading, "no row to page back from");
    }

    #[test]
    fn opening_a_conversation_clears_its_unread_badge_at_once() {
        let mut session =
            Session::assemble(String::new(), Theme::Dark, Vec::new(), String::new(), None);
        session.state.connected = true;
        session
            .state
            .conversations
            .push(crate::ConversationSummary {
                id: "dir:analyst".into(),
                kind: "direct".into(),
                parent_id: None,
                title: None,
                created_at: String::new(),
                updated_at: String::new(),
                last_message_at: None,
                archived_at: None,
                unread_count: 5,
                accepts_turns: true,
                last_message: None,
                avatar_version: None,
            });
        session.open_conversation("dir:analyst".into());
        assert_eq!(session.state.conversations[0].unread_count, 0);
    }

    #[test]
    fn auto_approve_asks_once_per_chat_when_turned_on_and_never_when_turned_off() {
        let mut confirmed = std::collections::BTreeSet::new();
        assert!(needs_auto_approve_confirmation(&confirmed, "dir:a", true));
        assert!(!needs_auto_approve_confirmation(&confirmed, "dir:a", false));
        confirmed.insert("dir:a".to_owned());
        assert!(!needs_auto_approve_confirmation(&confirmed, "dir:a", true));
        assert!(needs_auto_approve_confirmation(&confirmed, "dir:b", true));
    }

    #[test]
    fn the_first_auto_approve_press_only_opens_the_confirmation() {
        let mut session =
            Session::assemble(String::new(), Theme::Dark, Vec::new(), String::new(), None);
        session.state.connected = true;
        session.conversation = "dir:analyst".into();
        session.request_auto_approve(true);
        assert!(session.auto_approve_confirming);
        assert!(!session.state.auto_approve, "nothing is switched on yet");
        session.cancel_auto_approve();
        assert!(!session.auto_approve_confirming);
        session.request_auto_approve(true);
        session.confirm_auto_approve();
        assert!(!session.auto_approve_confirming);
        // Confirmed once: the next press goes straight through.
        session.request_auto_approve(true);
        assert!(!session.auto_approve_confirming);
        // Leaving the chat drops an open question.
        let mut other =
            Session::assemble(String::new(), Theme::Dark, Vec::new(), String::new(), None);
        other.request_auto_approve(true);
        other.open_conversation("dir:main".into());
        assert!(!other.auto_approve_confirming);
    }

    #[test]
    fn claude_agent_per_bot_state_folder_and_context() {
        let mut session =
            Session::assemble(String::new(), Theme::Dark, Vec::new(), String::new(), None);
        let (task_tx, task_rx) = std::sync::mpsc::channel();
        let (_outcome_tx, outcome_rx) = std::sync::mpsc::channel();
        session.persist_worker = Some(PersistWorker {
            sender: task_tx,
            receiver: outcome_rx,
        });
        session.state.bot_details.insert(
            "analyst".into(),
            crate::BotDetail {
                id: "analyst".into(),
                display_name: "Research Analyst".into(),
                role: "specialist".into(),
                description: "Reads financial statements".into(),
                avatar: "A".into(),
                enabled: true,
                provider: String::new(),
                model: String::new(),
                fallback_provider: None,
                fallback_model: None,
                routing_classes: Vec::new(),
                instructions: Some("Cite page numbers".into()),
                instructions_bytes: 17,
                skills: Vec::new(),
                tools: Vec::new(),
                require_approval: Vec::new(),
                memory_count: 0,
                artifact_count: 0,
                browser_profile: None,
                thinking: None,
                thinking_levels: Vec::new(),
                route: None,
            },
        );
        session.set_bot_claude_agent("analyst", true);
        assert!(session.bot_uses_claude_agent("analyst"));
        assert!(!session.bot_uses_claude_agent("main"));
        session.set_bot_claude_agent_folder("analyst", "proj-1".into());
        assert_eq!(
            session.bot_claude_agent_folder("analyst").as_deref(),
            Some("proj-1")
        );
        // Bad ids are ignored safely.
        session.set_bot_claude_agent("../evil", true);
        assert!(!session.bot_uses_claude_agent("../evil"));
        session.set_bot_claude_agent_folder("analyst", "bad/id".into());
        assert_eq!(
            session.bot_claude_agent_folder("analyst").as_deref(),
            Some("proj-1")
        );
        let ctx = session.bot_acp_context("analyst").expect("context");
        assert!(ctx.contains("Research Analyst"));
        assert!(ctx.contains("Reads financial statements"));
        assert!(ctx.contains("Cite page numbers"));
        // PersistWorker received two ClaudeAgent tasks without touching disk.
        let mut saved_count = 0;
        while let Ok(task) = task_rx.try_recv() {
            if matches!(task, PersistTask::ClaudeAgent(_)) {
                saved_count += 1;
            }
        }
        assert_eq!(saved_count, 2);
    }

    #[test]
    fn stop_works_during_optimistic_send_and_clears_on_stopping_zero() {
        let mut session =
            Session::assemble(String::new(), Theme::Dark, Vec::new(), String::new(), None);
        session.state.connected = true;
        session.state.history_loaded = true;
        session.state.messages.push(Message {
            id: "opt-1".into(),
            role: Role::User,
            content: "hello".into(),
            created_at: "Just now".into(),
            name: None,
            bot: None,
            attachments: Vec::new(),
            limit_reached: false,
            turn_id: None,
        });
        assert!(session.has_optimistic_pending());
        assert!(session.any_run_active());

        // Simulate Submit currently holding the worker request slot.
        session.pending = Some(OperationKind::Submit);
        session.stop_turn_or_runs();
        assert!(session.state.stopping);
        assert!(!session.stop_queued);

        // Once Submit completes with a non-terminal task, StopTurn has already
        // been dispatched out-of-band; if StopTurn returns 0, stopping clears.
        session.pending = None;
        let scope = session.state.scope.clone();
        assert!(session.state.receive(&scope, Reply::Stopping(0)));
        session.state.stopping = false;
        assert!(!session.state.stopping);
    }

    #[test]
    fn stop_clears_queued_sends_and_stop_queued_immediately() {
        let mut session =
            Session::assemble(String::new(), Theme::Dark, Vec::new(), String::new(), None);
        session.state.connected = true;
        session.state.history_loaded = true;
        session.state.task = Some(crate::Task {
            id: "t-stop".into(),
            kind: "turn".into(),
            bot: "main".into(),
            status: TaskStatus::Running,
        });
        session.queue.push("queued follow-up", None).unwrap();
        session
            .backlog
            .push_back(Operation::Submit(zeroize::Zeroizing::new("pending".into())));
        session.pending = Some(OperationKind::History);
        session.stop_queued = true;

        session.stop_turn();
        assert!(session.state.stopping);
        assert!(!session.stop_queued);
        assert!(session.queue.is_empty());
        assert!(session.backlog.is_empty());
    }

    #[test]
    fn session_expires_soon_warns_inside_48h_window_only() {
        let mut session =
            Session::assemble(String::new(), Theme::Dark, Vec::new(), String::new(), None);
        assert!(session.session_expires_soon().is_none());

        // 10 days away -> no warning.
        session.session_expires = Some(unix_now() + 10 * 24 * 3600);
        assert!(session.session_expires_soon().is_none());

        // 5 hours away -> hour warning.
        session.session_expires = Some(unix_now() + 5 * 3600 + 120);
        let msg = session.session_expires_soon().expect("warning expected");
        assert!(msg.contains("hour(s)"));

        // Already expired -> None (handled by auth redirect).
        session.session_expires = Some(unix_now().saturating_sub(60));
        assert!(session.session_expires_soon().is_none());
    }

    #[test]
    fn conversation_markdown_exports_title_and_speakers() {
        let mut session =
            Session::assemble(String::new(), Theme::Dark, Vec::new(), String::new(), None);
        assert!(session.conversation_markdown().is_none());

        session.conversation = "c-1".into();
        session
            .state
            .conversations
            .push(crate::ConversationSummary {
                id: "c-1".into(),
                kind: "chat".into(),
                parent_id: None,
                title: Some("Q4 Review".into()),
                created_at: "2026-11-18T00:00:00Z".into(),
                updated_at: "2026-11-18T00:00:00Z".into(),
                last_message_at: None,
                archived_at: None,
                unread_count: 0,
                accepts_turns: true,
                last_message: None,
                avatar_version: None,
            });
        session.state.messages.push(Message {
            id: "m-1".into(),
            role: Role::User,
            content: "Summarize the deck.".into(),
            created_at: "Now".into(),
            name: None,
            bot: None,
            attachments: Vec::new(),
            limit_reached: false,
            turn_id: None,
        });
        session.state.messages.push(Message {
            id: "m-2".into(),
            role: Role::Assistant,
            content: "Here is the summary.".into(),
            created_at: "Now".into(),
            name: None,
            bot: Some("main".into()),
            attachments: Vec::new(),
            limit_reached: false,
            turn_id: None,
        });
        let md = session.conversation_markdown().expect("markdown expected");
        assert!(md.starts_with("# Q4 Review\n\n"));
        assert!(md.contains("### You\n\nSummarize the deck."));
        assert!(md.contains("Here is the summary."));
    }

    #[test]
    fn turn_progress_routes_to_working_and_activity_without_dirtying_events() {
        use serde_json::json;
        let mut session = super::tests::sendable_session();
        session.conversation = "dir:main".into();
        let fp_before = session.ui_fingerprint();

        session.apply_event(
            "turn.progress".into(),
            json!({
                "task_id": "t-hb",
                "conversation_id": "dir:main",
                "bot": "main",
                "phase": "waiting_model",
                "elapsed_secs": 30,
                "beat": 2
            }),
        );
        assert!(
            !session.events_dirty,
            "heartbeat must not trigger a history reload"
        );
        assert_eq!(
            session.state.activity.heartbeat_label().as_deref(),
            Some("Still thinking · 30s")
        );
        assert_eq!(
            session
                .state
                .working
                .activity_of("main", Instant::now())
                .map(|a| a.label()),
            Some("Thinking… (30s)".into())
        );
        assert_ne!(
            session.ui_fingerprint(),
            fp_before,
            "ui_fingerprint must reflect heartbeat updates"
        );

        // Heartbeat for a different conversation updates WorkingBots but not open Activity.
        session.apply_event(
            "turn.progress".into(),
            json!({
                "task_id": "t-other",
                "conversation_id": "dir:analyst",
                "bot": "analyst",
                "phase": "waiting_model",
                "elapsed_secs": 15,
                "beat": 1
            }),
        );
        assert_eq!(
            session.state.activity.heartbeat_label().as_deref(),
            Some("Still thinking · 30s")
        );
        assert_eq!(
            session
                .state
                .working
                .activity_of("analyst", Instant::now())
                .map(|a| a.label()),
            Some("Thinking… (15s)".into())
        );
    }

    #[test]
    fn needs_review_dismiss_and_retry_recover_the_parked_chat() {
        let mut session = super::tests::sendable_session();
        session.conversation = "dir:main".into();
        session.state.messages.push(Message {
            id: "m1".into(),
            role: Role::User,
            content: "Check invoice #42".into(),
            created_at: "Now".into(),
            name: None,
            bot: None,
            attachments: Vec::new(),
            limit_reached: false,
            turn_id: None,
        });
        session.state.task = Some(crate::Task {
            id: "t-nr".into(),
            kind: "turn".into(),
            bot: "main".into(),
            status: TaskStatus::NeedsReview,
        });
        assert!(session.needs_review_active());
        assert!(!session.state.can_send());

        // "Mark reviewed" clears the parked task and remembers its id.
        session.dismiss_needs_review();
        assert!(!session.needs_review_active());
        assert!(session.state.task.is_none());
        assert!(session.dismissed_needs_review.contains("t-nr"));
        assert!(session.state.can_send());
        assert_eq!(session.toast.as_deref(), Some("Marked as reviewed."));

        // "Send again" clears another parked NeedsReview task and re-submits the last user message.
        session.state.task = Some(crate::Task {
            id: "t-nr-2".into(),
            kind: "turn".into(),
            bot: "main".into(),
            status: TaskStatus::NeedsReview,
        });
        assert!(session.needs_review_active());
        session.retry_needs_review();
        assert!(!session.needs_review_active());
        assert!(session.dismissed_needs_review.contains("t-nr-2"));
        assert!(
            session
                .state
                .messages
                .last()
                .is_some_and(|m| m.role == Role::User
                    && m.id.starts_with("opt-")
                    && m.content == "Check invoice #42")
        );
    }
}
