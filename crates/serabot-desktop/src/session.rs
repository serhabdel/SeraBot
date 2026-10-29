//! UI-independent owner session: connection, scheduling, reads and mutations.
//!
//! The Dioxus desktop shell drives this on a timer; all network work stays on
//! the worker and SSE threads, never on the UI thread.

use crate::{
    ApiError, ApprovalDecision, AudioUpdate, ConnectorUpdate, ConversationKind,
    MAX_SKILL_BODY_BYTES, MAX_SKILL_DESCRIPTION_CHARS, NewProvider, SPECIALIST_TOOLS, Selection,
    TaskStatus,
    api::{
        MEMORY_KINDS, valid_conversation_title, valid_credential, valid_memory_content,
        valid_memory_kind, valid_memory_source, valid_model_id, valid_provider_id,
        valid_secret_name,
    },
    creds,
    events::{EventStream, EventUpdate},
    state::State,
    updater, valid_bot_description, valid_bot_id, valid_conversation_parent, valid_id,
    valid_new_skill_id, valid_skill_body, valid_skill_description, valid_slug, valid_text,
    valid_token, validate_tool_list,
    worker::{Credentials, Job, Operation, OperationKind, Worker},
};
use secrecy::ExposeSecret;
use std::{
    collections::BTreeMap,
    sync::Arc,
    time::{Duration, Instant},
};
use zeroize::Zeroize;

pub const REQUEST_GAP: Duration = Duration::from_secs(2);
pub const POLL_GAP: Duration = Duration::from_secs(5);
pub const POLL_SAFETY: Duration = Duration::from_secs(20);

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

pub struct RenameDraft {
    pub id: String,
    pub title: String,
}

#[derive(Default)]
pub struct UpdateUi {
    pub info: Option<updater::UpdateInfo>,
    pub error: Option<String>,
    pub checking: bool,
    pub rx: Option<std::sync::mpsc::Receiver<Result<Option<updater::UpdateInfo>, String>>>,
}

pub struct Session {
    pub state: State,
    pub worker: Option<Worker>,
    pub credentials: Option<Arc<Credentials>>,
    pub events: Option<EventStream>,
    pub events_active: bool,
    events_dirty: bool,
    pub last_event_id: i64,
    last_event_kind: String,
    pub server: String,
    pub token: String,
    pub connect_error: Option<String>,
    pub connecting: bool,
    pub theme: Theme,
    pub section: Section,
    pub conversation: String,
    pub composer: String,
    pub fleet: FleetDraft,
    pub memory: MemoryDraft,
    pub provider: ProviderDraft,
    pub connector_drafts: BTreeMap<String, ConnectorDraft>,
    pub audio: AudioDraft,
    pub admin_notice: Option<String>,
    pub admin_error: Option<String>,
    pub restart_pending: bool,
    pub restarting: bool,
    pub rename: Option<RenameDraft>,
    pub archive_confirm: Option<String>,
    pub update: UpdateUi,
    pub toast: Option<String>,
    pending: Option<OperationKind>,
    reconnect_at: Option<Instant>,
    scheduled: Option<(Instant, Operation)>,
    next_request: Instant,
    refreshing: bool,
    poll_history: bool,
    poll_paused: bool,
    poll_approvals: bool,
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
        let server = creds::prefill_server(&settings);
        let token = creds::load_token()
            .map(|token| token.expose_secret().to_owned())
            .unwrap_or_default();
        let worker = Worker::start().ok();
        let mut session = Self {
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
            section: Section::Chat,
            conversation: "windows-owner".into(),
            composer: String::new(),
            fleet: FleetDraft::default(),
            memory: MemoryDraft::default(),
            provider: ProviderDraft::default(),
            connector_drafts: BTreeMap::new(),
            audio: AudioDraft::default(),
            admin_notice: None,
            admin_error: None,
            restart_pending: false,
            restarting: false,
            rename: None,
            archive_confirm: None,
            update: UpdateUi::default(),
            toast: None,
            pending: None,
            reconnect_at: None,
            scheduled: None,
            next_request: Instant::now(),
            refreshing: false,
            poll_history: false,
            poll_paused: false,
            poll_approvals: false,
        };
        if session.worker.is_some()
            && !session.server.trim().is_empty()
            && valid_token(&session.token)
        {
            session.connect_now();
        }
        session.check_updates();
        session
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
            self.connect_error =
                Some("Paste the token minted on the server (32–512 characters).".into());
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

    pub fn disconnect(&mut self) {
        self.stop_events();
        self.reset_scope(false);
        self.credentials = None;
        self.token.zeroize();
        self.connecting = false;
        self.connect_error = None;
        self.state.error = None;
        self.reconnect_at = None;
        self.admin_notice = None;
        self.admin_error = None;
        self.restart_pending = false;
        self.restarting = false;
    }

    pub fn forget_token(&mut self) {
        let origin = self
            .credentials
            .as_ref()
            .map(|credentials| credentials.endpoint.origin().ascii_serialization())
            .unwrap_or_else(|| self.server.clone());
        let result = creds::forget_token();
        self.stop_events();
        self.reset_scope(false);
        self.credentials = None;
        self.token.zeroize();
        self.connecting = false;
        self.reconnect_at = None;
        if !origin.is_empty() {
            self.server = origin;
        }
        self.connect_error = result.err();
        self.state.error = None;
    }

    pub fn reject_token(&mut self) {
        let origin = self
            .credentials
            .as_ref()
            .map(|credentials| credentials.endpoint.origin().ascii_serialization())
            .unwrap_or_else(|| self.server.clone());
        let _ = creds::forget_token();
        self.stop_events();
        self.reset_scope(false);
        self.credentials = None;
        self.token.zeroize();
        self.connecting = false;
        self.reconnect_at = None;
        if !origin.is_empty() {
            self.server = origin;
        }
        self.connect_error =
            Some("Token rejected or revoked. Mint a new token on the server.".into());
        self.state.error = None;
    }

    fn reset_scope(&mut self, connected: bool) {
        self.state.new_scope(self.conversation.clone(), connected);
        if let Some(worker) = &self.worker {
            worker.set_epoch(self.state.scope.epoch);
        }
        self.composer.zeroize();
        self.pending = None;
        self.scheduled = None;
        self.refreshing = false;
        self.poll_history = false;
        self.poll_paused = false;
        self.poll_approvals = false;
        self.events_dirty = false;
        self.rename = None;
        self.archive_confirm = None;
        if !connected {
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
            self.memory.content.zeroize();
            self.memory.delete_confirm = None;
        }
    }

    pub fn set_theme(&mut self, theme: Theme) {
        self.theme = theme;
        let origin = self
            .credentials
            .as_ref()
            .map(|credentials| credentials.endpoint.origin().ascii_serialization())
            .unwrap_or_else(|| self.server.clone());
        if !origin.is_empty() {
            let _ = creds::save_settings(&creds::Settings {
                server: origin,
                theme: Some(theme.as_str().into()),
            });
        }
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
        self.ensure_section_data();
    }

    pub fn open_conversation(&mut self, id: String) {
        if !valid_id(&id) {
            return;
        }
        self.conversation = id;
        self.section = Section::Chat;
        self.reset_scope(true);
        self.refresh();
    }

    pub fn submit(&mut self) {
        if !self.state.can_send() || !valid_text(&self.composer) || !self.ready() {
            return;
        }
        self.state.error = None;
        let text = std::mem::take(&mut self.composer);
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
            self.update.error = Some("Could not start the update check.".into());
        }
    }

    pub fn apply_update(&mut self, info: &updater::UpdateInfo) -> Result<(), String> {
        updater::spawn_apply(info)
    }

    /// Called from the UI timer. Drains completions and advances scheduled work.
    pub fn tick(&mut self) {
        self.poll_events();
        self.receive();
        self.flush_events();
        self.poll_updates();
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
            if Instant::now() >= *when {
                if let Some((_, operation)) = self.scheduled.take() {
                    self.start_request(operation);
                }
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
        if let Some(task) = &self.state.task {
            if !task.status.terminal() {
                if self.events_active {
                    self.schedule(Operation::Task(task.id.clone()), POLL_SAFETY);
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
                    self.schedule(operation, POLL_GAP);
                }
            }
        } else if self.section == Section::Approvals {
            self.schedule(Operation::Approvals, POLL_GAP * 2);
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
        let operation = if kind.contains("approval") {
            Operation::Approvals
        } else if kind.contains("conversation") {
            Operation::ListConversations {
                include_archived: false,
            }
        } else {
            Operation::History
        };
        self.start_request(operation);
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
            if completion.kind == OperationKind::Health {
                self.connecting = false;
            }
            match completion.result {
                Err(ApiError::Http(401)) => {
                    self.refreshing = false;
                    self.scheduled = None;
                    self.poll_paused = true;
                    self.reject_token();
                }
                Err(error)
                    if matches!(
                        completion.kind,
                        OperationKind::AdminWrite | OperationKind::AdminRestart
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
                    self.scheduled = None;
                }
                Err(error) => {
                    let message = error.to_string();
                    let rate_limited = error == ApiError::Http(429);
                    self.state.fail(&completion.scope, completion.kind, error);
                    self.refreshing = false;
                    self.scheduled = None;
                    self.poll_paused = true;
                    if !self.state.connected {
                        self.connect_error = Some(message);
                        self.restarting = false;
                    }
                    if rate_limited {
                        self.next_request = Instant::now() + Duration::from_secs(60);
                    }
                }
                Ok(reply) => {
                    if !self.state.receive(&completion.scope, reply) {
                        continue;
                    }
                    match completion.kind {
                        OperationKind::Health if self.state.connected => {
                            self.connect_error = None;
                            self.sync_audio_draft();
                            if self.restarting {
                                self.restarting = false;
                                self.restart_pending = false;
                                self.admin_notice =
                                    Some("Server is back. The saved settings are active.".into());
                            }
                            self.persist_credentials();
                            self.start_events();
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
                            self.schedule(Operation::History, Duration::ZERO)
                        }
                        OperationKind::History if self.refreshing => {
                            self.schedule(Operation::Approvals, Duration::ZERO)
                        }
                        OperationKind::Approvals if self.refreshing => self.schedule(
                            Operation::ListConversations {
                                include_archived: false,
                            },
                            Duration::ZERO,
                        ),
                        OperationKind::ListConversations if self.refreshing => {
                            self.refreshing = false;
                            self.ensure_section_data();
                        }
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
                        OperationKind::CreateBot => {
                            self.schedule(Operation::ListBots, Duration::ZERO);
                        }
                        OperationKind::ListBots => {
                            if self.section == Section::Fleet && !self.state.skills_loaded {
                                self.schedule(Operation::ListSkills, Duration::ZERO);
                            }
                        }
                        OperationKind::CreateConversation => {
                            self.fleet.conv_slug.clear();
                            self.fleet.conv_parent.clear();
                            self.schedule(
                                Operation::ListConversations {
                                    include_archived: false,
                                },
                                Duration::ZERO,
                            );
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
        let settings = creds::save_settings(&creds::Settings {
            server: origin.clone(),
            theme: Some(self.theme.as_str().into()),
        });
        let token = creds::save_token(credentials.token.expose_secret());
        self.server = origin;
        if settings.is_err() || token.is_err() {
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
                self.update.info = Some(info);
                self.update.error = None;
            }
            Some(Ok(Ok(None))) => {
                self.update.rx = None;
                self.update.checking = false;
                self.update.info = None;
            }
            Some(Ok(Err(error))) => {
                self.update.rx = None;
                self.update.checking = false;
                self.update.error = Some(error);
            }
            Some(Err(std::sync::mpsc::TryRecvError::Disconnected)) => {
                self.update.rx = None;
                self.update.checking = false;
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
        self.state
            .conversations
            .iter()
            .find(|conversation| conversation.id == self.conversation)
            .map(|conversation| conversation.display_title().to_owned())
            .unwrap_or_else(|| self.conversation.clone())
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

    pub fn can_admin(&self) -> bool {
        self.ready() && self.state.connected
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

    fn sync_audio_draft(&mut self) {
        if let Some(enabled) = self.state.audio_enabled {
            self.audio.enabled = enabled;
        }
        if let Some(model) = &self.state.audio_transcription_model {
            if !model.is_empty() {
                self.audio.transcription_model = model.clone();
            }
        }
        if let Some(model) = &self.state.audio_understanding_model {
            if !model.is_empty() {
                self.audio.understanding_model = model.clone();
            }
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
    ) {
        if !valid_slug(&slug) || valid_conversation_parent(kind, parent.as_deref()).is_err() {
            self.state.error = Some("Check the conversation slug and parent.".into());
            return;
        }
        self.start_request(Operation::CreateConversation {
            kind,
            slug,
            parent_id: parent.filter(|parent| !parent.trim().is_empty()),
        });
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

fn model_csv(raw: &str) -> Vec<String> {
    raw.split([',', '\n', ';'])
        .map(str::trim)
        .filter(|model| !model.is_empty())
        .map(str::to_owned)
        .collect()
}
