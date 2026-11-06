//! UI-independent owner session: connection, scheduling, reads and mutations.
//!
//! The Dioxus desktop shell drives this on a timer; all network work stays on
//! the worker and SSE threads, never on the UI thread.

use crate::{
    ApiClient, ApiError, ApprovalDecision, AudioUpdate, BotConfigUpdate, BotProfileUpdate,
    ConnectorUpdate, ConversationKind, MAX_SKILL_BODY_BYTES, MAX_SKILL_DESCRIPTION_CHARS,
    MAX_USAGE_DAYS, Message, NewGroup, NewProvider, Role, SPECIALIST_TOOLS, Selection, SignedIn,
    TaskStatus, TeamMode, TelegramLink, TelegramStatus, VoiceSettings, VoiceUpdate,
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
mod flows;

use flows::AvatarOp;
pub use flows::{ImageSlot, NewBotDraft, NewGroupDraft, OnboardingUi, PickedImage};
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

pub const REQUEST_GAP: Duration = Duration::from_millis(60);
pub const POLL_GAP: Duration = Duration::from_millis(750);
pub const POLL_SAFETY: Duration = Duration::from_secs(4);

/// Shown on the sign-in screen after any 401.
pub const SESSION_ENDED: &str = "Your session ended. Sign in again.";
const SETTINGS_NOT_SAVED: &str =
    "Could not save your settings. They apply until you close SeraBot.";
const SIGNED_OUT_OFFLINE: &str = "You are signed out on this device. The server could not be reached to end the session; it expires on its own.";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Theme {
    Dark,
    Light,
}

impl Theme {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Dark => "dark",
            Self::Light => "light",
        }
    }

    pub fn from_stored(value: Option<&str>) -> Self {
        match value {
            Some("light") => Self::Light,
            _ => Self::Dark,
        }
    }

    pub fn toggled(self) -> Self {
        match self {
            Self::Dark => Self::Light,
            Self::Light => Self::Dark,
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

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Section {
    Chat,
    Conversations,
    Memory,
    Providers,
    Connectors,
    Approvals,
    Fleet,
    Settings,
}

impl Section {
    pub const NAV: [Self; 7] = [
        Self::Chat,
        Self::Conversations,
        Self::Memory,
        Self::Providers,
        Self::Connectors,
        Self::Approvals,
        Self::Fleet,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Chat => "Chat",
            Self::Conversations => "Conversations",
            Self::Memory => "Memory",
            Self::Providers => "Providers",
            Self::Connectors => "Connectors",
            Self::Approvals => "Approvals",
            Self::Fleet => "Fleet",
            Self::Settings => "Settings",
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
}

impl Default for MemoryDraft {
    fn default() -> Self {
        Self {
            kind: "note".into(),
            content: String::new(),
            source: "operator:desktop".into(),
            query: String::new(),
            delete_confirm: None,
        }
    }
}

impl Drop for MemoryDraft {
    fn drop(&mut self) {
        self.content.zeroize();
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
        }
    }
}

impl Drop for ProviderDraft {
    fn drop(&mut self) {
        self.api_key.zeroize();
    }
}

#[derive(Clone, Default)]
pub struct ConnectorDraft {
    pub enabled: bool,
    pub values: BTreeMap<String, String>,
    pub saving: bool,
    /// ElevenLabs only: "api_key" or "oauth" (sent on Save connector).
    pub auth: Option<String>,
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

#[derive(Default)]
pub struct BotProfileDraft {
    pub id: String,
    pub display_name: String,
    pub description: String,
    pub avatar: String,
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
    cancel: Option<Arc<AtomicBool>>,
    rx: Option<std::sync::mpsc::Receiver<SignInMsg>>,
}

enum SignInMsg {
    Waiting,
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

/// What to do with a saved artifact once the download completes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PendingOpen {
    Open,
    Reveal,
}

pub struct Session {
    pub state: State,
    pub worker: Option<Worker>,
    pub credentials: Option<Arc<Credentials>>,
    pub events: Option<EventStream>,
    pub events_active: bool,
    /// Browser dock and vault sheet state; its own worker thread.
    pub panel: PanelState,
    panel_worker: Option<PanelWorker>,
    events_dirty: bool,
    pub last_event_id: i64,
    last_event_kind: String,
    pub server: String,
    pub token: String,
    pub connect_error: Option<String>,
    pub connecting: bool,
    pub theme: Theme,
    /// Pinned bots/conversations shown in the inspector strip. Persisted in
    /// settings.json through [`creds::Settings::pins`].
    pub pins: Vec<String>,
    pub section: Section,
    pub conversation: String,
    pub composer: String,
    pub fleet: FleetDraft,
    pub memory: MemoryDraft,
    pub provider: ProviderDraft,
    pub connector_drafts: BTreeMap<String, ConnectorDraft>,
    pub audio: AudioDraft,
    /// Editable copy of the voice/OCR settings surface.
    pub voice: VoiceDraft,
    pub selected_bot: Option<String>,
    pub bot_profile: BotProfileDraft,
    pub admin_notice: Option<String>,
    /// Connector whose Test probe is in flight.
    pub connector_testing: Option<String>,
    /// Provider whose browser sign-in is waiting for the redirect.
    pub oauth_signing_in: Option<String>,
    pub admin_error: Option<String>,
    pub restart_pending: bool,
    pub restarting: bool,
    pub rename: Option<RenameDraft>,
    pub archive_confirm: Option<String>,
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
    /// Toast text from browser/Explorer launches, which run on a helper thread.
    launch_tx: std::sync::mpsc::Sender<String>,
    launch_rx: std::sync::mpsc::Receiver<String>,
    /// Unix seconds at which the signed-in session ends, when the server
    /// reported one this run. A restored token has no known expiry.
    session_expires: Option<u64>,
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
        let token = creds::load_token()
            .map(|token| token.expose_secret().to_owned())
            .unwrap_or_default();
        let worker = Worker::start().ok();
        let mut session = Self::assemble(server, theme, pins, token, worker);
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
        Self {
            state: State::default(),
            worker,
            credentials: None,
            events: None,
            events_active: false,
            events_dirty: false,
            last_event_id: 0,
            last_event_kind: String::new(),
            server,
            token,
            connect_error: None,
            connecting: false,
            theme,
            pins,
            section: Section::Chat,
            conversation: "windows-owner".into(),
            composer: String::new(),
            fleet: FleetDraft::default(),
            memory: MemoryDraft::default(),
            provider: ProviderDraft::default(),
            connector_drafts: BTreeMap::new(),
            audio: AudioDraft::default(),
            voice: VoiceDraft::default(),
            selected_bot: None,
            bot_profile: BotProfileDraft::default(),
            admin_notice: None,
            connector_testing: None,
            oauth_signing_in: None,
            admin_error: None,
            restart_pending: false,
            restarting: false,
            rename: None,
            archive_confirm: None,
            team_rules_draft: String::new(),
            update: UpdateUi::default(),
            status: StatusUi::default(),
            sign_in: SignInUi::default(),
            sign_out: SignOutUi::default(),
            telegram: TelegramUi::default(),
            toast: None,
            downloading: false,
            download_rx: None,
            pending_open: None,
            launch_tx,
            launch_rx,
            session_expires: None,
            pending: None,
            new_bot: NewBotDraft::default(),
            new_group: NewGroupDraft::default(),
            onboarding_ui: OnboardingUi::default(),
            avatar_upload: None,
            avatar_op: None,
            creating_named_bot: false,
            pending_open_group: None,
            bot_detail_pending: None,
            bot_config_pending: None,
            pending_team: None,
            pending_room_refresh: false,
            reconnect_at: None,
            scheduled: None,
            next_request: Instant::now(),
            refreshing: false,
            poll_history: false,
            poll_paused: false,
            poll_approvals: false,
            rate_attempts: 0,
            panel: PanelState::new(),
            panel_worker: None,
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
        if self.panel.onedrive.take_refresh() | self.panel.connectors.take_refresh() {
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
    /// Sign the user out cleanly once the session expiry the server reported
    /// has passed, instead of waiting for a request to answer 401.
    fn check_session_expiry(&mut self) {
        if self.session_expires.is_some_and(|at| unix_now() >= at) {
            self.reject_token();
        }
    }

    fn reset_scope(&mut self, connected: bool) {
        self.state.new_scope(self.conversation.clone(), connected);
        if let Some(worker) = &self.worker {
            worker.set_epoch(self.state.scope.epoch);
        }
        self.composer.zeroize();
        self.pending = None;
        self.bot_detail_pending = None;
        self.bot_config_pending = None;
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
            crate::avatars::publish(&std::collections::BTreeMap::new());
            self.selected_bot = None;
            self.bot_profile = BotProfileDraft::default();
            self.memory.content.zeroize();
            self.memory.delete_confirm = None;
        }
    }

    /// Build the settings snapshot to persist: the current server origin,
    /// theme and pins. `None` when no origin is known yet.
    fn ui_settings_snapshot(&self) -> Option<creds::Settings> {
        let origin = self
            .credentials
            .as_ref()
            .map(|credentials| credentials.endpoint.origin().ascii_serialization())
            .unwrap_or_else(|| self.server.clone());
        (!origin.is_empty()).then(|| creds::Settings {
            server: origin,
            theme: Some(self.theme.as_str().into()),
            pins: (!self.pins.is_empty()).then(|| self.pins.clone()),
        })
    }

    /// Persist theme and pins next to the server origin, like the credential
    /// save does. A failed write keeps the change for this run and says so.
    fn save_ui_settings(&mut self) {
        if let Some(settings) = self.ui_settings_snapshot()
            && creds::save_settings(&settings).is_err()
        {
            self.toast = Some(SETTINGS_NOT_SAVED.into());
        }
    }

    pub fn set_theme(&mut self, theme: Theme) {
        self.theme = theme;
        self.save_ui_settings();
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

    pub fn open_section(&mut self, section: Section) {
        self.section = section;
        if section != Section::Fleet {
            self.selected_bot = None;
        }
        if section == Section::Settings {
            self.refresh_status();
            self.refresh_telegram();
        }
        self.ensure_section_data();
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
        let spawned = std::thread::Builder::new()
            .name("serabot-signin".into())
            .spawn(move || {
                let waiting = sender.clone();
                let result = signin::run(
                    endpoint,
                    invite.as_deref(),
                    &thread_cancel,
                    signin::SIGN_IN_TIMEOUT,
                    &|url| webbrowser::open(url).is_ok(),
                    &|| {
                        let _ = waiting.send(SignInMsg::Waiting);
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
                SignInMsg::Waiting => self.sign_in.phase = SignInPhase::Waiting,
                SignInMsg::Finished(result) => finished = Some(result),
            }
        }
        let Some(result) = finished else {
            return;
        };
        self.sign_in.rx = None;
        self.sign_in.cancel = None;
        self.sign_in.phase = SignInPhase::Idle;
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
        // A store failure is reported by persist_credentials once connected.
        let _ = creds::save_token(&token);
        self.token.zeroize();
        self.token = token.clone();
        token.zeroize();
        self.session_expires = signed_in.expires_at.as_deref().and_then(parse_rfc3339_unix);
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
        let stored = creds::forget_token();
        self.stop_events();
        self.reset_scope(false);
        self.reset_panel();
        self.sign_in.reset_transient();
        self.credentials = None;
        self.token.zeroize();
        self.token.clear();
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

    pub fn open_conversation(&mut self, id: String) {
        if !valid_id(&id) {
            return;
        }
        self.conversation = id;
        self.section = Section::Chat;
        self.selected_bot = None;
        let has_models = !self.state.providers.is_empty()
            || self
                .state
                .catalog
                .as_ref()
                .is_some_and(|c| !c.providers.is_empty());
        self.reset_scope(true);
        self.state.error = None;
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

    pub fn open_bot(&mut self, id: String) {
        if !valid_id(&id) {
            return;
        }
        self.selected_bot = Some(id.clone());
        self.section = Section::Fleet;
        self.bot_profile = match self.state.listed_bots.iter().find(|bot| bot.id == id) {
            Some(bot) => BotProfileDraft {
                id: bot.id.clone(),
                display_name: self.bot_display_name(&bot.id),
                description: bot.description.clone().unwrap_or_default(),
                avatar: bot.avatar.clone().unwrap_or_default(),
            },
            None => BotProfileDraft {
                id,
                ..BotProfileDraft::default()
            },
        };
        self.ensure_section_data();
    }

    pub fn close_bot(&mut self) {
        self.selected_bot = None;
        self.bot_profile = BotProfileDraft::default();
    }

    /// Schedule a detail load for the bot of the open `dir:` conversation,
    /// unless the detail is cached or already in flight. Used when a
    /// conversation opens and on reconnect, next to the catalog refresh.
    fn schedule_open_bot_detail(&mut self) {
        let Some(bot) = self
            .conversation
            .strip_prefix("dir:")
            .filter(|bot| valid_speaker(bot))
            .map(str::to_owned)
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

    pub fn open_bot_chat(&mut self, id: String) {
        if self.state.bot_chat_binding != Some(true) {
            self.state.error = Some(
                "This server does not route one-to-one bot chats. Update the server to 0.5.0 or later."
                    .into(),
            );
            return;
        }
        self.selected_bot = None;
        self.open_conversation(format!("dir:{id}"));
    }

    pub fn save_bot_profile(&mut self) {
        self.admin_error = None;
        self.admin_notice = None;
        let name = self.bot_profile.display_name.trim().to_owned();
        if name.is_empty() || name.chars().count() > 64 || name.chars().any(char::is_control) {
            self.admin_error = Some("Bot display name must be 1–64 visible characters.".into());
            return;
        }
        if self.bot_profile.avatar.chars().count() > 16
            || self.bot_profile.avatar.chars().any(char::is_control)
        {
            self.admin_error = Some("Bot avatar must be a short label or emoji.".into());
            return;
        }
        if self.state.bot_profiles != Some(true) {
            self.admin_error = Some(
                "This server does not store bot profiles. Update the server to 0.5.0 or later."
                    .into(),
            );
            return;
        }
        if !self.can_admin() {
            self.admin_error = Some("Wait for the current request, then retry.".into());
            return;
        }
        self.start_request(Operation::AdminSetBotProfile {
            id: self.bot_profile.id.clone(),
            update: BotProfileUpdate {
                display_name: name,
                description: self.bot_profile.description.trim().to_owned(),
                avatar: self.bot_profile.avatar.trim().to_owned(),
            },
        });
    }

    pub fn bot_display_name(&self, id: &str) -> String {
        self.state
            .listed_bots
            .iter()
            .find(|bot| bot.id == id)
            .and_then(|bot| bot.display_name.clone())
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

    pub fn conversation_label(&self, conversation: &crate::ConversationSummary) -> String {
        if let Some(title) = conversation
            .title
            .as_deref()
            .map(str::trim)
            .filter(|title| !title.is_empty())
        {
            return title.to_owned();
        }
        if conversation.id == "windows-owner" {
            return "Owner".into();
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
        if conversation.id.chars().all(|c| c.is_ascii_digit()) {
            let tail = conversation
                .id
                .get(conversation.id.len().saturating_sub(4)..)
                .unwrap_or(&conversation.id);
            return format!("Chat {tail}");
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
            self.conversation.strip_prefix("dir:")
        } else {
            None
        };
        let bot = message
            .bot
            .as_deref()
            .or(message.name.as_deref())
            .or(dir_bot)
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

    pub fn submit(&mut self) {
        if !self.state.can_send() || !valid_text(&self.composer) || !self.ready() {
            return;
        }
        self.state.error = None;
        let text = std::mem::take(&mut self.composer);
        self.state.messages.push(Message {
            id: format!("opt-{}", Instant::now().elapsed().as_nanos()),
            role: Role::User,
            content: text.clone(),
            created_at: "Just now".into(),
            name: None,
            bot: None,
        });
        self.start_request(Operation::Submit(zeroize::Zeroizing::new(text)));
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

    /// Called from the UI timer. Drains completions and advances scheduled work.
    pub fn tick(&mut self) {
        self.check_session_expiry();
        self.poll_events();
        self.receive();
        self.flush_events();
        self.poll_updates();
        self.poll_status();
        self.poll_sign_in();
        self.poll_telegram();
        self.poll_download();
        self.poll_launches();
        self.pump_panel();
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
        } else if self.section == Section::Approvals {
            self.schedule(Operation::Approvals, POLL_GAP * 2 * slow);
        } else {
            self.ensure_section_data();
        }
    }

    fn start_events(&mut self) {
        self.stop_events();
        let Some(credentials) = self.credentials.clone() else {
            return;
        };
        self.events = Some(EventStream::start(
            credentials,
            self.last_event_id,
            Box::new(|| {}),
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
                    EventUpdate::Connected => self.events_active = true,
                    EventUpdate::Disconnected => self.events_active = false,
                    EventUpdate::Unauthorized => unauthorized = true,
                    EventUpdate::Event { id, kind, .. } => {
                        self.last_event_id = self.last_event_id.max(id);
                        self.last_event_kind = kind;
                        self.events_dirty = true;
                    }
                }
            }
        }
        if unauthorized {
            self.reject_token();
        }
    }

    fn flush_events(&mut self) {
        if !self.events_dirty
            || !self.state.connected
            || self.state.unknown_outcome
            || !self.state.history_loaded
            || self.refreshing
            || self.pending.is_some()
            || self.scheduled.is_some()
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
                self.state.team_loading = true;
                self.state.team_error = None;
                self.schedule(Operation::GetTeam, Duration::ZERO);
            }
        }
    }

    fn start_request(&mut self, operation: Operation) {
        self.scheduled = None;
        let Some(credentials) = &self.credentials else {
            return;
        };
        let Some(worker) = &self.worker else {
            return;
        };
        let kind = operation.kind();
        // Track the bot-scoped payloads so loads dedup and a saved config can
        // update the cached detail optimistically when it completes.
        match &operation {
            Operation::BotDetail { id } => self.bot_detail_pending = Some(id.clone()),
            Operation::UpdateBotConfig { id, update } => {
                self.bot_config_pending = Some((id.clone(), update.clone()))
            }
            Operation::AvatarFetch { target, version } => {
                self.avatar_op = Some(AvatarOp::Fetch(target.clone(), version.clone()))
            }
            Operation::AvatarUpload { .. } | Operation::AvatarRemove { .. } => {
                self.avatar_op = Some(AvatarOp::Write)
            }
            _ => {}
        }
        let job = Job {
            scope: self.state.scope.clone(),
            credentials: Arc::clone(credentials),
            operation,
        };
        if worker.dispatch(job).is_err() {
            self.state.error = Some("Network worker unavailable. Reconnect.".into());
            self.refreshing = false;
            self.poll_paused = true;
            return;
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
                            self.state.avatars.insert(
                                target.key(),
                                crate::avatars::AvatarImage {
                                    version,
                                    data_url: None,
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
                Err(error) if completion.kind == OperationKind::ToolsWrite => {
                    self.toast = Some(format!("Could not change that: {error}"));
                    self.schedule(Operation::ListBots, Duration::ZERO);
                }
                Err(error)
                    if matches!(
                        completion.kind,
                        OperationKind::AdminWrite
                            | OperationKind::ConnectorTest
                            | OperationKind::OAuthSignIn
                            | OperationKind::AdminProfile
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
                        _ => error.to_string(),
                    };
                    self.admin_error = Some(message);
                    self.connector_testing = None;
                    self.oauth_signing_in = None;
                    self.scheduled = None;
                }
                Err(ApiError::Http(429)) if completion.kind.is_background_poll() => {
                    // A rate-limited background poll backs off quietly (no
                    // banner) and the poll loop carries on afterwards.
                    self.rate_attempts = self.rate_attempts.saturating_add(1);
                    self.refreshing = false;
                    self.scheduled = None;
                    self.next_request = Instant::now()
                        + crate::api::poll_backoff(
                            crate::api::last_retry_after_secs(),
                            self.rate_attempts,
                            crate::api::jitter_seed(),
                        );
                }
                Err(error) => {
                    let message = error.to_string();
                    let rate_limited = error == ApiError::Http(429);
                    self.state.fail(&completion.scope, completion.kind, error);
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
                        }
                        OperationKind::FetchArtifact => {
                            self.toast = Some(format!("Download failed: {message}"));
                            self.downloading = false;
                            self.state.error = None;
                        }
                        _ => {}
                    }
                    if !self.state.connected {
                        self.connect_error = Some(message);
                        self.restarting = false;
                    }
                    if rate_limited {
                        self.next_request = Instant::now()
                            + Duration::from_secs(crate::api::last_retry_after_secs());
                    }
                }
                Ok(reply) => {
                    self.rate_attempts = 0;
                    if let Reply::Artifact {
                        filename, bytes, ..
                    } = reply
                    {
                        // Bytes stay off the state: they go straight to a
                        // file-write thread and never touch the UI state.
                        self.downloading = true;
                        let (sender, receiver) = std::sync::mpsc::channel();
                        self.download_rx = Some(receiver);
                        if std::thread::Builder::new()
                            .name("serabot-download".into())
                            .spawn(move || {
                                let result = save_download(&filename, &bytes);
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
                    let bot_config_saved = match &reply {
                        Reply::BotConfigSaved { restart_required } => Some(*restart_required),
                        _ => None,
                    };
                    let avatar_saved = match &reply {
                        Reply::AvatarSaved(target) => Some(target.clone()),
                        _ => None,
                    };
                    if !self.state.receive(&completion.scope, reply) {
                        continue;
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
                            self.schedule(Operation::History, Duration::ZERO);
                        }
                        OperationKind::Decide => {
                            self.schedule(Operation::Approvals, Duration::ZERO);
                        }
                        OperationKind::CreateSkill => {
                            self.schedule(Operation::ListSkills, Duration::ZERO);
                        }
                        OperationKind::CreateBot if self.creating_named_bot => {
                            self.bot_created();
                        }
                        OperationKind::CreateBot => {
                            if let Some(created) = self.state.created_bot.clone() {
                                self.selected_bot = Some(created.id.clone());
                                self.section = Section::Fleet;
                                self.bot_profile = BotProfileDraft {
                                    id: created.id.clone(),
                                    display_name: humanize_label(&created.id),
                                    description: self.fleet.bot_description.clone(),
                                    avatar: String::new(),
                                };
                            }
                            self.schedule(Operation::ListBots, Duration::ZERO);
                        }
                        OperationKind::ListBots => {
                            if let Some(id) = self.selected_bot.clone()
                                && let Some(bot) =
                                    self.state.listed_bots.iter().find(|bot| bot.id == id)
                            {
                                if self.bot_profile.display_name.trim().is_empty() {
                                    self.bot_profile.display_name = self.bot_display_name(&id);
                                }
                                if self.bot_profile.avatar.trim().is_empty() {
                                    self.bot_profile.avatar =
                                        bot.avatar.clone().unwrap_or_default();
                                }
                            }
                            if self.section == Section::Fleet && !self.state.skills_loaded {
                                self.schedule(Operation::ListSkills, Duration::ZERO);
                            }
                            self.continue_after_bots();
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
                        OperationKind::AdminProfile => {
                            if let Some(bot) = self
                                .state
                                .listed_bots
                                .iter_mut()
                                .find(|bot| bot.id == self.bot_profile.id)
                            {
                                let name = self.bot_profile.display_name.trim().to_owned();
                                let description = self.bot_profile.description.trim().to_owned();
                                let avatar = self.bot_profile.avatar.trim().to_owned();
                                bot.display_name = (!name.is_empty()).then_some(name);
                                bot.description = (!description.is_empty()).then_some(description);
                                bot.avatar = (!avatar.is_empty()).then_some(avatar);
                            }
                            self.admin_error = None;
                            self.admin_notice = Some("Bot profile saved.".into());
                        }
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
                        OperationKind::RenameConversation | OperationKind::ArchiveConversation => {
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
                            self.schedule(
                                Operation::ListConversations {
                                    include_archived: false,
                                },
                                Duration::ZERO,
                            );
                            self.schedule(Operation::ListArchived, Duration::ZERO);
                        }
                        OperationKind::AddMemory | OperationKind::DeleteMemory => {
                            if completion.kind == OperationKind::AddMemory {
                                self.memory.content.zeroize();
                            }
                            self.memory.delete_confirm = None;
                            let query = self.memory.query.trim();
                            let query = (!query.is_empty()).then(|| query.to_owned());
                            self.schedule(Operation::ListMemories { query }, Duration::ZERO);
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
                            if let Some(text) = self.state.transcript.clone() {
                                if !self.composer.trim().is_empty() {
                                    self.composer.push('\n');
                                }
                                self.composer.push_str(&text);
                            }
                        }
                        OperationKind::History => {
                            // The transcript is in; the artifact library of this
                            // conversation loads right behind it.
                            self.state.library_loading = true;
                            self.schedule(Operation::Library, Duration::ZERO);
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
                        OperationKind::AdminWrite => {
                            self.provider.api_key.zeroize();
                            for draft in self.connector_drafts.values_mut() {
                                for value in draft.values.values_mut() {
                                    value.zeroize();
                                }
                            }
                            self.admin_error = None;
                            self.admin_notice =
                                Some("Saved. Restart the server to apply the change.".into());
                            self.restart_pending = true;
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
                        OperationKind::Usage => {}
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
        let settings = self
            .ui_settings_snapshot()
            .map(|snapshot| creds::save_settings(&snapshot));
        let token = creds::save_token(credentials.token.expose_secret());
        self.server = origin;
        if settings.as_ref().is_some_and(|result| result.is_err()) || token.is_err() {
            self.state.error = Some("Connected, but the credential could not be saved.".into());
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
                } else {
                    None
                }
            }
            Section::Conversations => {
                (!self.state.conversations_loaded).then(|| Operation::ListConversations {
                    include_archived: false,
                })
            }
            Section::Memory => {
                if self.state.memories_loaded {
                    None
                } else {
                    let query = self.memory.query.trim();
                    Some(Operation::ListMemories {
                        query: (!query.is_empty()).then(|| query.to_owned()),
                    })
                }
            }
            Section::Providers => (!self.state.providers_loaded).then_some(Operation::Providers),
            Section::Connectors => (!self.state.connectors_loaded).then_some(Operation::Connectors),
            Section::Approvals => (!self.state.approvals_loaded).then_some(Operation::Approvals),
            Section::Fleet => {
                if !self.state.bots_loaded {
                    Some(Operation::ListBots)
                } else if !self.state.skills_loaded {
                    Some(Operation::ListSkills)
                } else {
                    None
                }
            }
            Section::Settings => None,
        }
    }

    fn ensure_section_data(&mut self) {
        if self.section == Section::Settings {
            self.ensure_telegram();
        }
        if !self.state.connected || self.state.unknown_outcome || self.refreshing {
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
        if let Some(bot) = self.conversation.strip_prefix("dir:") {
            return self.bot_display_name(bot);
        }
        humanize_label(&self.conversation)
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
        if self.pending.is_some() {
            return;
        }
        self.state.providers_loaded = false;
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

    pub fn can_admin(&self) -> bool {
        self.ready() && self.state.connected
    }

    pub fn sign_in_provider(&mut self, id: String, kind: String) {
        if !self.can_admin() {
            self.admin_error = Some("Wait for the current request, then retry.".into());
            return;
        }
        self.admin_error = None;
        self.admin_notice =
            Some("Complete the sign-in in your browser. The console waits up to 3 minutes.".into());
        self.oauth_signing_in = Some(id.clone());
        self.start_request(Operation::OAuthSignIn { id, kind });
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
        let api_key = self.provider.api_key.trim().to_owned();
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
        let request = NewProvider {
            id,
            kind,
            base_url,
            models,
            api_key_secret: secret,
            catalog_fetch: self.provider.catalog_fetch,
            api_key: (!api_key.is_empty()).then_some(api_key),
        };
        self.start_request(Operation::AdminSaveProvider(request));
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
        let auth = draft.auth.clone();
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
            update: ConnectorUpdate {
                enabled,
                values,
                auth,
            },
        });
    }

    pub fn set_connector_auth(&mut self, id: &str, auth: String) {
        let draft = self.connector_drafts.entry(id.to_owned()).or_default();
        draft.auth = Some(auth);
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
            if draft.auth.is_none() {
                draft.auth = connector.auth.clone();
            }
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
        self.start_request(Operation::Approvals);
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
        let query = self.memory.query.trim();
        self.start_request(Operation::ListMemories {
            query: (!query.is_empty()).then(|| query.to_owned()),
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
        self.start_request(Operation::TranscribeAudio { filename, bytes });
        if self.pending.is_none() {
            self.state.transcript_loading = false;
        }
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
        Theme, event_refresh, telegram_error,
    };
    use crate::ApiError;
    use std::sync::Arc;

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
}
