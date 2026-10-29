use eframe::egui::{self, Align2, Color32, FontId, Frame, Margin, RichText, Sense, Stroke, Vec2};
use secrecy::{ExposeSecret, SecretString};
use serabot_desktop::{
    ApiError, ApprovalDecision, ConversationKind, DEFAULT_SPECIALIST_TOOLS, MAX_SKILL_BODY_BYTES,
    MAX_SKILL_DESCRIPTION_CHARS, Role, SPECIALIST_TOOLS, TaskStatus,
    connect::{ConnectState, connect_screen},
    conversation_node_id, creds, design,
    events::{EventStream, EventUpdate},
    parse_skill_list,
    state::State,
    updater, valid_bot_description, valid_bot_id, valid_conversation_parent, valid_id,
    valid_new_skill_id, valid_skill_body, valid_skill_description, valid_slug, valid_speaker,
    valid_text, valid_token, validate_tool_list,
    worker::{Credentials, Job, Operation, OperationKind, Worker},
};
use std::{
    sync::Arc,
    time::{Duration, Instant},
};
use zeroize::{Zeroize, Zeroizing};

mod chat;
mod conversations;
mod fleet;
mod memory;
mod settings;
mod status;

pub(crate) use chat::ChatView;

const REQUEST_GAP: Duration = Duration::from_secs(3);
const POLL_GAP: Duration = Duration::from_secs(5);
const POLL_SAFETY: Duration = Duration::from_secs(20);
const TOAST_GAP: Duration = Duration::from_secs(4);

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Section {
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

    pub fn icon(self) -> design::NavIcon {
        match self {
            Self::Chat => design::NavIcon::Chat,
            Self::Conversations => design::NavIcon::Conversations,
            Self::Memory => design::NavIcon::Memory,
            Self::Providers => design::NavIcon::Providers,
            Self::Connectors => design::NavIcon::Connectors,
            Self::Approvals => design::NavIcon::Approvals,
            Self::Fleet => design::NavIcon::Fleet,
            Self::Settings => design::NavIcon::Settings,
        }
    }

    pub fn hint(self) -> &'static str {
        match self {
            Self::Chat => "Talk to the orchestrator",
            Self::Conversations => "Sections, groups, direct threads and rooms",
            Self::Memory => "Owner-approved facts",
            Self::Providers => "Models, routing and egress consent",
            Self::Connectors => "Google, Microsoft, Notion, AgentMail",
            Self::Approvals => "Gateway consent for outbound effects",
            Self::Fleet => "Bots, skills and conversation nodes",
            Self::Settings => "Connection, updates and shortcuts",
        }
    }
}

pub(crate) struct FleetDraft {
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
            bot_tools: default_tool_flags(),
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

pub(crate) struct MemoryDraft {
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

impl MemoryDraft {
    pub fn query_opt(&self) -> Option<String> {
        let query = self.query.trim();
        (!query.is_empty()).then(|| query.to_owned())
    }
}

impl Drop for MemoryDraft {
    fn drop(&mut self) {
        self.content.zeroize();
    }
}

pub(crate) struct RenameDraft {
    pub id: String,
    pub title: String,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum PaletteAction {
    Go(Section),
    NewConversation,
    Refresh,
    CheckUpdates,
    Disconnect,
    ForgetToken,
}

#[derive(Default)]
pub(crate) struct UpdateUi {
    pub info: Option<updater::UpdateInfo>,
    pub error: Option<String>,
    pub checking: bool,
    pub dismissed: bool,
    pub rx: Option<std::sync::mpsc::Receiver<Result<Option<updater::UpdateInfo>, String>>>,
}

pub struct Desktop {
    pub(crate) state: State,
    worker: Option<Worker>,
    credentials: Option<Arc<Credentials>>,
    events: Option<EventStream>,
    pub(crate) events_active: bool,
    events_dirty: bool,
    pub(crate) last_event_id: i64,
    last_event_kind: String,
    connect: ConnectState,
    pub(crate) conversation: String,
    pub(crate) section: Section,
    pub(crate) composer: String,
    pub(crate) conv_search: String,
    pub(crate) fleet: FleetDraft,
    pub(crate) memory: MemoryDraft,
    pub(crate) rename: Option<RenameDraft>,
    pub(crate) archive_confirm: Option<String>,
    pub(crate) chat: ChatView,
    pending: Option<OperationKind>,
    scheduled: Option<(Instant, Operation)>,
    next_request: Instant,
    refreshing: bool,
    reviewed_unknown: bool,
    confirmed_egress: bool,
    poll_history: bool,
    poll_paused: bool,
    poll_approvals: bool,
    model_open: bool,
    palette_open: bool,
    palette_focus: bool,
    palette_query: String,
    pub(crate) toast: Option<(String, Instant)>,
    pub(crate) update: UpdateUi,
}

fn default_tool_flags() -> Vec<bool> {
    SPECIALIST_TOOLS
        .iter()
        .map(|tool| DEFAULT_SPECIALIST_TOOLS.contains(tool))
        .collect()
}

impl Desktop {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        design::apply_theme(&cc.egui_ctx);
        let worker = Worker::start().ok();
        let mut state = State::default();
        if worker.is_none() {
            state.error =
                Some("Could not start the network worker. Close and reopen SeraBot.".into());
        }
        let mut desktop = Self {
            state,
            worker,
            credentials: None,
            events: None,
            events_active: false,
            events_dirty: false,
            last_event_id: 0,
            last_event_kind: String::new(),
            connect: ConnectState::load(),
            conversation: "windows-owner".into(),
            section: Section::Chat,
            composer: String::new(),
            conv_search: String::new(),
            fleet: FleetDraft::default(),
            memory: MemoryDraft::default(),
            rename: None,
            archive_confirm: None,
            chat: ChatView::default(),
            pending: None,
            scheduled: None,
            next_request: Instant::now(),
            refreshing: false,
            reviewed_unknown: false,
            confirmed_egress: false,
            poll_history: false,
            poll_paused: false,
            poll_approvals: false,
            model_open: false,
            palette_open: false,
            palette_focus: false,
            palette_query: String::new(),
            toast: None,
            update: UpdateUi::default(),
        };
        // Auto-connect only with a saved credential and a valid server origin.
        if desktop.worker.is_some()
            && desktop.connect.ready()
            && valid_id(&desktop.conversation)
            && desktop.connect_now()
        {
            cc.egui_ctx.request_repaint();
        }
        desktop.check_updates();
        desktop
    }

    fn ready(&self) -> bool {
        self.pending.is_none()
            && !self.refreshing
            && self.worker.is_some()
            && Instant::now() >= self.next_request
    }

    pub(crate) fn active_conversation(&self) -> bool {
        self.conversation == self.state.scope.conversation
    }

    fn reset_scope(&mut self, connected: bool) {
        self.state.new_scope(self.conversation.clone(), connected);
        if let Some(worker) = &self.worker {
            worker.set_epoch(self.state.scope.epoch);
        }
        self.composer.zeroize();
        self.chat.reset();
        self.pending = None;
        self.scheduled = None;
        self.refreshing = false;
        self.reviewed_unknown = false;
        self.confirmed_egress = false;
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
            self.clear_fleet_drafts();
            self.memory.content.zeroize();
            self.memory.delete_confirm = None;
        }
    }

    fn clear_fleet_drafts(&mut self) {
        self.fleet = FleetDraft::default();
    }

    pub(crate) fn selected_tools(&self) -> Vec<String> {
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

    fn start_events(&mut self, ctx: &egui::Context) {
        self.stop_events();
        let Some(credentials) = self.credentials.clone() else {
            return;
        };
        let wake: Box<dyn Fn() + Send> = {
            let ctx = ctx.clone();
            Box::new(move || ctx.request_repaint())
        };
        self.events = Some(EventStream::start(credentials, self.last_event_id, wake));
    }

    fn stop_events(&mut self) {
        if let Some(events) = self.events.take() {
            events.stop();
        }
        self.events_active = false;
    }

    fn poll_events(&mut self, ctx: &egui::Context) {
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
            self.stop_events();
            self.reject_token(ctx);
        }
    }

    /// Coalesce server events into at most one refresh per request gap.
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

    /// Validate the connect card and start the authenticated health read.
    fn connect_now(&mut self) -> bool {
        let endpoint = match self.connect.endpoint() {
            Ok(endpoint) => endpoint,
            Err(error) => {
                self.connect.error = Some(error.to_string());
                self.connect.connecting = false;
                return false;
            }
        };
        if !valid_token(&self.connect.token) {
            self.connect.error =
                Some("Paste the token minted on the server (32–512 characters, no spaces).".into());
            self.connect.connecting = false;
            return false;
        }
        if !valid_id(&self.conversation) {
            self.connect.error = Some(
                "Conversation IDs use letters, digits, _ : - and are at most 128 characters."
                    .into(),
            );
            self.connect.connecting = false;
            return false;
        }
        self.stop_events();
        self.reset_scope(false);
        self.state.error = None;
        self.connect.error = None;
        self.connect.connecting = true;
        self.credentials = Some(Arc::new(Credentials {
            endpoint,
            token: SecretString::new(self.connect.token.to_string()),
        }));
        self.refreshing = true;
        self.start_request(Operation::Health);
        true
    }

    fn disconnect(&mut self, ctx: &egui::Context) {
        self.stop_events();
        self.reset_scope(false);
        self.credentials = None;
        self.connect.clear_token();
        self.connect.connecting = false;
        self.connect.persisted = false;
        self.connect.error = None;
        self.state.error = None;
        self.close_overlays();
        ctx.request_repaint();
    }

    /// Delete the stored credential, clear the field, and keep the server URL.
    fn forget_token(&mut self, ctx: &egui::Context) {
        let origin = self
            .credentials
            .as_ref()
            .map(|credentials| credentials.endpoint.origin().ascii_serialization())
            .or_else(|| {
                self.connect
                    .endpoint()
                    .ok()
                    .map(|endpoint| endpoint.origin().ascii_serialization())
            });
        let result = creds::forget_token();
        self.stop_events();
        self.reset_scope(false);
        self.credentials = None;
        self.connect.clear_token();
        self.connect.connecting = false;
        self.connect.persisted = false;
        if let Some(origin) = origin {
            self.connect.server = origin;
        }
        self.connect.error = result.err();
        self.state.error = None;
        self.close_overlays();
        ctx.request_repaint();
    }

    /// A revoked or rejected token never stays in memory or the credential
    /// store. The server URL is kept so only a new token is required.
    fn reject_token(&mut self, ctx: &egui::Context) {
        let origin = self
            .credentials
            .as_ref()
            .map(|credentials| credentials.endpoint.origin().ascii_serialization())
            .or_else(|| {
                self.connect
                    .endpoint()
                    .ok()
                    .map(|url| url.origin().ascii_serialization())
            });
        let _ = creds::forget_token();
        self.stop_events();
        self.reset_scope(false);
        self.credentials = None;
        self.connect.clear_token();
        self.connect.connecting = false;
        self.connect.persisted = false;
        if let Some(origin) = origin {
            self.connect.server = origin;
        }
        self.connect.error =
            Some("Token rejected or revoked. Mint a new token on the server.".into());
        self.state.error = None;
        self.close_overlays();
        ctx.request_repaint();
    }

    fn close_overlays(&mut self) {
        self.model_open = false;
        self.palette_open = false;
        self.palette_focus = false;
        self.palette_query.clear();
    }

    /// Persist only after the first authenticated Health reply.
    fn persist_credentials(&mut self) {
        let Some(credentials) = self.credentials.clone() else {
            return;
        };
        let origin = credentials.endpoint.origin().ascii_serialization();
        let settings = creds::save_settings(&creds::Settings {
            server: origin.clone(),
        });
        let token = creds::save_token(credentials.token.expose_secret());
        self.connect.server = origin;
        self.connect.persisted = settings.is_ok() && token.is_ok();
        if let Err(error) = settings {
            self.state.error = Some(format!(
                "Connected, but the server URL could not be saved: {error}"
            ));
        } else if let Err(error) = token {
            self.state.error = Some(format!(
                "Connected, but the token could not be saved: {error}"
            ));
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
        let job = Job {
            scope: self.state.scope.clone(),
            credentials: Arc::clone(credentials),
            operation,
        };
        if worker.dispatch(job).is_err() {
            self.state.error = Some("Network worker unavailable. This request was not sent. Disconnect, then reconnect.".into());
            self.refreshing = false;
            self.poll_paused = true;
            self.connect.connecting = false;
            if !self.state.connected {
                self.connect.error =
                    Some("Network worker unavailable. Close and reopen SeraBot.".into());
            }
            return;
        }
        self.pending = Some(kind);
        self.next_request = Instant::now() + REQUEST_GAP;
    }

    fn schedule(&mut self, operation: Operation, delay: Duration) {
        self.scheduled = Some(((Instant::now() + delay).max(self.next_request), operation));
    }

    fn refresh(&mut self) {
        self.state.error = None;
        self.refreshing = true;
        self.reviewed_unknown = false;
        self.poll_paused = false;
        self.ensure_fresh_section();
        self.start_request(if self.state.connected {
            Operation::Catalog
        } else {
            Operation::Health
        });
    }

    /// Drop the cached list for the open section so refresh reloads it.
    fn ensure_fresh_section(&mut self) {
        match self.section {
            Section::Memory => self.state.memories_loaded = false,
            Section::Providers => self.state.providers_loaded = false,
            Section::Connectors => self.state.connectors_loaded = false,
            Section::Approvals => self.state.approvals_loaded = false,
            _ => {}
        }
    }

    fn receive(&mut self, ctx: &egui::Context) {
        while let Some(completion) = self.worker.as_ref().and_then(Worker::completion) {
            if !self.state.scope.accepts(&completion.scope) {
                continue;
            }
            self.pending = None;
            if completion.kind == OperationKind::Health {
                self.connect.connecting = false;
            }
            match completion.result {
                Err(ApiError::Http(401)) => {
                    self.refreshing = false;
                    self.scheduled = None;
                    self.poll_paused = true;
                    self.reject_token(ctx);
                }
                Err(error) => {
                    self.state.fail(&completion.scope, completion.kind, error);
                    self.refreshing = false;
                    self.scheduled = None;
                    self.poll_paused = true;
                    if !self.state.connected {
                        self.connect.error = Some(error.to_string());
                    }
                    if error == ApiError::Http(429) {
                        self.next_request = Instant::now() + Duration::from_secs(60);
                    }
                }
                Ok(reply) => {
                    if !self.state.receive(&completion.scope, reply) {
                        continue;
                    }
                    match completion.kind {
                        OperationKind::Health if self.state.connected => {
                            self.connect.error = None;
                            self.persist_credentials();
                            self.start_events(ctx);
                            self.schedule(Operation::Catalog, Duration::ZERO);
                        }
                        OperationKind::Health => {
                            self.refreshing = false;
                            self.poll_paused = true;
                            self.connect.error = Some(
                                self.state
                                    .error
                                    .clone()
                                    .unwrap_or_else(|| "Server is not ready. Retry.".into()),
                            );
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
                            self.reviewed_unknown = self.state.unknown_outcome;
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
                            self.schedule(
                                Operation::ListMemories {
                                    query: self.memory.query_opt(),
                                },
                                Duration::ZERO,
                            );
                        }
                        OperationKind::Task
                            if self
                                .state
                                .task
                                .as_ref()
                                .is_some_and(|t| t.status.terminal()) =>
                        {
                            self.schedule(Operation::History, Duration::ZERO);
                        }
                        OperationKind::Task
                            if self
                                .state
                                .task
                                .as_ref()
                                .is_some_and(|t| t.status == TaskStatus::AwaitingApproval) =>
                        {
                            self.schedule(Operation::Approvals, Duration::ZERO);
                        }
                        _ => {}
                    }
                }
            }
        }
        if self.pending.is_some() {
            return;
        }
        if self
            .scheduled
            .as_ref()
            .is_some_and(|(when, _)| Instant::now() >= *when)
        {
            if let Some((_, operation)) = self.scheduled.take() {
                self.start_request(operation);
            }
        } else if self.scheduled.is_none()
            && self.state.connected
            && !self.refreshing
            && !self.poll_paused
            && !self.state.unknown_outcome
        {
            if let Some(task) = &self.state.task {
                if !task.status.terminal() {
                    if self.events_active {
                        // The stream drives updates; this is a stuck-stream guard.
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
            }
        }
    }

    fn missing_section_op(&self) -> Option<Operation> {
        match self.section {
            Section::Chat | Section::Conversations => {
                (!self.state.conversations_loaded).then(|| Operation::ListConversations {
                    include_archived: false,
                })
            }
            Section::Memory => (!self.state.memories_loaded).then(|| Operation::ListMemories {
                query: self.memory.query_opt(),
            }),
            Section::Providers => (!self.state.providers_loaded).then_some(Operation::Providers),
            Section::Connectors => (!self.state.connectors_loaded).then_some(Operation::Connectors),
            Section::Approvals => (!self.state.approvals_loaded).then_some(Operation::Approvals),
            Section::Fleet | Section::Settings => None,
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

    fn open_section(&mut self, section: Section) {
        if self.section != section {
            self.section = section;
            self.model_open = false;
        }
        self.ensure_section_data();
    }

    pub(crate) fn open_conversation(&mut self, id: String) {
        if !valid_id(&id) {
            return;
        }
        self.conversation = id;
        self.section = Section::Chat;
        self.reset_scope(true);
        self.refresh();
    }

    pub(crate) fn conversation_title(&self) -> String {
        self.state
            .conversations
            .iter()
            .find(|conversation| conversation.id == self.conversation)
            .map(|conversation| conversation.display_title().to_owned())
            .unwrap_or_else(|| self.conversation.clone())
    }

    pub(crate) fn conversation_kind(&self) -> Option<&str> {
        self.state
            .conversations
            .iter()
            .find(|conversation| conversation.id == self.conversation)
            .map(|conversation| conversation.kind.as_str())
    }

    pub(crate) fn conversation_accepts_turns(&self) -> bool {
        self.state
            .conversations
            .iter()
            .find(|conversation| conversation.id == self.conversation)
            .is_none_or(|conversation| conversation.accepts_turns)
    }

    fn check_updates(&mut self) {
        if self.update.checking || updater::configured_repo().is_none() {
            return;
        }
        let (sender, receiver) = std::sync::mpsc::channel();
        self.update.rx = Some(receiver);
        self.update.checking = true;
        self.update.error = None;
        let spawned = std::thread::Builder::new()
            .name("serabot-update".into())
            .spawn(move || {
                let result = match updater::Updater::from_build_env() {
                    Some(updater) => updater.check().map_err(|error| error.to_string()),
                    None => Ok(None),
                };
                let _ = sender.send(result);
            });
        if spawned.is_err() {
            self.update.rx = None;
            self.update.checking = false;
            self.update.error = Some("Could not start the update check.".into());
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
                self.update.dismissed = false;
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
                self.update.error = Some("The update check stopped unexpectedly.".into());
            }
            Some(Err(std::sync::mpsc::TryRecvError::Empty)) | None => {}
        }
    }

    fn rail(&mut self, ctx: &egui::Context) {
        egui::SidePanel::left("rail")
            .exact_width(design::RAIL_W)
            .resizable(false)
            .frame(
                Frame::new()
                    .fill(design::RAIL)
                    .stroke(Stroke::new(1.0_f32, design::STROKE_SOFT))
                    .inner_margin(Margin::symmetric(0, 8)),
            )
            .show(ctx, |ui| {
                ui.vertical_centered(|ui| {
                    let (rect, _) = ui.allocate_exact_size(Vec2::new(36.0, 36.0), Sense::hover());
                    ui.painter()
                        .circle_filled(rect.center(), 15.0, design::ACCENT_DIM);
                    ui.painter().circle_stroke(
                        rect.center(),
                        15.0,
                        Stroke::new(1.0_f32, design::ACCENT),
                    );
                    ui.painter().text(
                        rect.center(),
                        Align2::CENTER_CENTER,
                        "S",
                        FontId::proportional(15.0),
                        design::ACCENT,
                    );
                });
                ui.add_space(6.0);
                for section in Section::NAV {
                    if design::rail_item(
                        ui,
                        section.icon(),
                        section.label(),
                        self.section == section,
                    )
                    .on_hover_text(section.hint())
                    .clicked()
                    {
                        self.open_section(section);
                    }
                }
                ui.with_layout(egui::Layout::bottom_up(egui::Align::Center), |ui| {
                    ui.add_space(4.0);
                    ui.label(
                        RichText::new(format!("v{}", env!("CARGO_PKG_VERSION")))
                            .size(9.5)
                            .color(design::MUTED_DIM),
                    );
                    if design::rail_item(
                        ui,
                        design::NavIcon::Settings,
                        "Settings",
                        self.section == Section::Settings,
                    )
                    .clicked()
                    {
                        self.open_section(Section::Settings);
                    }
                    let color = if self.pending.is_some() {
                        design::WARNING
                    } else if self.state.connected {
                        design::ACCENT
                    } else {
                        design::MUTED
                    };
                    let (rect, response) =
                        ui.allocate_exact_size(Vec2::new(14.0, 14.0), Sense::hover());
                    ui.painter().circle_filled(rect.center(), 4.0, color);
                    let host = self
                        .credentials
                        .as_ref()
                        .and_then(|credentials| credentials.endpoint.host_str())
                        .unwrap_or("not connected");
                    response.on_hover_text(host);
                    ui.add_space(4.0);
                });
            });
    }

    fn top_bar(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        ui.horizontal(|ui| {
            ui.add_space(4.0);
            let heading = if self.section == Section::Chat {
                self.conversation_title()
            } else {
                self.section.label().to_owned()
            };
            ui.label(
                RichText::new(heading)
                    .size(15.0)
                    .strong()
                    .color(design::TEXT),
            );
            if self.section == Section::Chat {
                if let Some(kind) = self.conversation_kind() {
                    design::chip(ui, kind, design::MUTED, design::PANEL_ALT);
                }
                if !self.active_conversation() {
                    design::chip(ui, "opening…", design::WARNING, design::WARNING_BG);
                }
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let (label, color) = if self.pending.is_some() {
                    ("Syncing", design::WARNING)
                } else if self.state.connected {
                    ("Online", design::ACCENT)
                } else {
                    ("Offline", design::MUTED)
                };
                Frame::new()
                    .fill(design::PANEL_ALT)
                    .stroke(Stroke::new(1.0_f32, design::STROKE_SOFT))
                    .corner_radius(10.0)
                    .inner_margin(Margin::symmetric(9, 4))
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            ui.spacing_mut().item_spacing.x = 5.0;
                            let (rect, _) =
                                ui.allocate_exact_size(Vec2::new(6.0, 6.0), Sense::hover());
                            ui.painter().circle_filled(rect.center(), 3.0, color);
                            ui.label(RichText::new(label).size(11.0).color(color));
                        });
                    });
                if ui
                    .small_button(RichText::new("Ctrl K").size(10.0).color(design::MUTED))
                    .on_hover_text("Command palette")
                    .clicked()
                {
                    self.open_palette();
                }
                if self.update.info.is_some()
                    && ui
                        .small_button(RichText::new("Update").size(10.0).color(design::ACCENT))
                        .on_hover_text("Open Settings to install")
                        .clicked()
                {
                    self.open_section(Section::Settings);
                }
                if self.section == Section::Chat
                    && ui
                        .small_button(
                            RichText::new(self.model_label())
                                .size(10.0)
                                .color(design::MUTED),
                        )
                        .on_hover_text("Model routing for this conversation")
                        .clicked()
                {
                    self.model_open = true;
                }
                let _ = ctx;
            });
        });
    }

    fn model_label(&self) -> String {
        match &self.state.catalog {
            Some(catalog) if catalog.selection.is_auto() => "auto".into(),
            Some(catalog) => catalog.selection.model.clone(),
            None => "model".into(),
        }
    }

    fn open_palette(&mut self) {
        self.palette_open = true;
        self.palette_focus = true;
        self.palette_query.clear();
    }

    fn palette_commands(&self) -> Vec<(String, PaletteAction)> {
        let mut commands = Section::NAV
            .into_iter()
            .map(|section| {
                (
                    format!("Go to {}", section.label()),
                    PaletteAction::Go(section),
                )
            })
            .collect::<Vec<_>>();
        commands.push((
            "Go to Settings".into(),
            PaletteAction::Go(Section::Settings),
        ));
        commands.push(("New conversation".into(), PaletteAction::NewConversation));
        commands.push(("Refresh".into(), PaletteAction::Refresh));
        if updater::configured_repo().is_some() {
            commands.push(("Check for updates".into(), PaletteAction::CheckUpdates));
        }
        commands.push(("Disconnect".into(), PaletteAction::Disconnect));
        commands.push(("Forget token".into(), PaletteAction::ForgetToken));
        commands
    }

    fn run_palette(&mut self, action: PaletteAction, ctx: &egui::Context) {
        match action {
            PaletteAction::Go(section) => self.open_section(section),
            PaletteAction::NewConversation => {
                self.open_conversation(format!("windows-{}", uuid::Uuid::new_v4()));
            }
            PaletteAction::Refresh => self.refresh(),
            PaletteAction::CheckUpdates => self.check_updates(),
            PaletteAction::Disconnect => self.disconnect(ctx),
            PaletteAction::ForgetToken => self.forget_token(ctx),
        }
    }

    fn palette_window(&mut self, ctx: &egui::Context) {
        if !self.palette_open {
            return;
        }
        let mut action = None;
        let mut close = false;
        egui::Window::new("Command palette")
            .id(egui::Id::new("serabot-palette"))
            .title_bar(false)
            .resizable(false)
            .collapsible(false)
            .anchor(Align2::CENTER_TOP, [0.0, 84.0])
            .default_width(440.0)
            .resizable(false)
            .frame(
                Frame::new()
                    .fill(design::PANEL)
                    .stroke(Stroke::new(1.0_f32, design::STROKE))
                    .corner_radius(14.0)
                    .inner_margin(Margin::symmetric(14, 12))
                    .shadow(egui::epaint::Shadow {
                        offset: [0, 10],
                        blur: 30,
                        spread: 0,
                        color: Color32::from_black_alpha(120),
                    }),
            )
            .show(ctx, |ui| {
                let response = ui.add(
                    egui::TextEdit::singleline(&mut self.palette_query)
                        .desired_width(f32::INFINITY)
                        .hint_text("Type a command…"),
                );
                if self.palette_focus {
                    response.request_focus();
                    self.palette_focus = false;
                }
                ui.add_space(6.0);
                let needle = self.palette_query.trim().to_lowercase();
                let commands = self
                    .palette_commands()
                    .into_iter()
                    .filter(|(label, _)| {
                        needle.is_empty() || label.to_lowercase().contains(&needle)
                    })
                    .collect::<Vec<_>>();
                if commands.is_empty() {
                    ui.label(
                        RichText::new("No matching command.")
                            .size(12.0)
                            .color(design::MUTED),
                    );
                }
                for (label, item) in commands {
                    if ui
                        .add(
                            egui::Button::new(RichText::new(&label).size(13.0).color(design::TEXT))
                                .frame(false)
                                .min_size(Vec2::new(ui.available_width(), 28.0)),
                        )
                        .clicked()
                    {
                        action = Some(item);
                    }
                }
                let enter = ui
                    .ctx()
                    .input_mut(|input| input.consume_key(egui::Modifiers::NONE, egui::Key::Enter));
                if enter && action.is_none() {
                    action = self
                        .palette_commands()
                        .into_iter()
                        .find(|(label, _)| {
                            needle.is_empty() || label.to_lowercase().contains(&needle)
                        })
                        .map(|(_, item)| item);
                }
                if ui.ctx().input(|input| input.key_pressed(egui::Key::Escape)) {
                    close = true;
                }
            });
        if close {
            self.palette_open = false;
        }
        if let Some(action) = action {
            self.palette_open = false;
            self.run_palette(action, ctx);
        }
    }

    fn model_window(&mut self, ctx: &egui::Context) {
        if !self.model_open {
            return;
        }
        let mut open = true;
        egui::Window::new("Model routing")
            .id(egui::Id::new("serabot-model"))
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .default_width(360.0)
            .show(ctx, |ui| self.model_block(ui));
        if !open {
            self.model_open = false;
        }
    }

    fn handle_shortcuts(&mut self, ctx: &egui::Context) {
        let palette = ctx.input_mut(|input| input.consume_key(egui::Modifiers::CTRL, egui::Key::K));
        if palette {
            self.open_palette();
        }
        if self.palette_open {
            return;
        }
        let mut jump = None;
        for (index, section) in Section::NAV.into_iter().enumerate() {
            let key = match index {
                0 => egui::Key::Num1,
                1 => egui::Key::Num2,
                2 => egui::Key::Num3,
                3 => egui::Key::Num4,
                4 => egui::Key::Num5,
                5 => egui::Key::Num6,
                _ => egui::Key::Num7,
            };
            if ctx.input_mut(|input| input.consume_key(egui::Modifiers::CTRL, key)) {
                jump = Some(section);
            }
        }
        if let Some(section) = jump {
            self.open_section(section);
        }
        if ctx.input_mut(|input| input.consume_key(egui::Modifiers::CTRL, egui::Key::R)) {
            self.refresh();
        }
        if ctx.input(|input| input.key_pressed(egui::Key::Escape)) {
            self.model_open = false;
        }
    }

    pub(crate) fn show_toast(&mut self, message: impl Into<String>) {
        self.toast = Some((message.into(), Instant::now()));
    }

    pub(crate) fn toast_message(&self) -> Option<&str> {
        self.toast.as_ref().and_then(|(message, at)| {
            (Instant::now().duration_since(*at) < TOAST_GAP).then_some(message.as_str())
        })
    }
}

impl eframe::App for Desktop {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        if self.state.connected {
            self.handle_shortcuts(ctx);
            self.rail(ctx);
            if self.section == Section::Chat {
                self.conversation_column(ctx);
            }
            egui::TopBottomPanel::top("topbar")
                .exact_height(50.0)
                .frame(
                    Frame::new()
                        .fill(design::BG)
                        .stroke(Stroke::new(1.0_f32, design::STROKE_SOFT))
                        .inner_margin(Margin::symmetric(14, 9)),
                )
                .show(ctx, |ui| self.top_bar(ui, ctx));
            egui::CentralPanel::default()
                .frame(
                    Frame::new()
                        .fill(design::BG)
                        .inner_margin(Margin::symmetric(18, 14)),
                )
                .show(ctx, |ui| {
                    self.update_banner(ui, ctx);
                    match self.section {
                        Section::Chat => self.chat_screen(ui),
                        Section::Conversations => self.conversations_screen(ui),
                        Section::Memory => self.memory_screen(ui),
                        Section::Providers => self.providers_screen(ui),
                        Section::Connectors => self.connectors_screen(ui),
                        Section::Approvals => self.approvals_screen(ui, ctx),
                        Section::Fleet => self.fleet_screen(ui),
                        Section::Settings => self.settings_screen(ui, ctx),
                    }
                });
            self.model_window(ctx);
            self.palette_window(ctx);
        } else {
            let footer = build_footer();
            if connect_screen(ctx, &mut self.connect, &footer) {
                self.connect_now();
            }
        }

        self.poll_events(ctx);
        self.receive(ctx);
        self.flush_events();
        self.poll_updates();

        if self.pending.is_some() {
            ctx.request_repaint_after(Duration::from_millis(100));
        } else if let Some((when, _)) = &self.scheduled {
            let now = Instant::now();
            let wake = if self.next_request > now {
                (*when).min(self.next_request)
            } else {
                *when
            };
            ctx.request_repaint_after(wake.saturating_duration_since(now));
        } else if Instant::now() < self.next_request {
            ctx.request_repaint_after(self.next_request.saturating_duration_since(Instant::now()));
        }
        if self.update.checking {
            ctx.request_repaint_after(Duration::from_millis(250));
        }
        if self
            .toast
            .as_ref()
            .is_some_and(|(_, at)| at.elapsed() >= TOAST_GAP)
        {
            self.toast = None;
        }
        if self.toast.is_some() {
            ctx.request_repaint_after(Duration::from_millis(250));
        }
    }

    fn persist_egui_memory(&self) -> bool {
        false
    }
}

impl Drop for Desktop {
    fn drop(&mut self) {
        self.composer.zeroize();
        self.memory.content.zeroize();
        self.credentials = None;
        self.stop_events();
    }
}

fn build_footer() -> String {
    let version = env!("CARGO_PKG_VERSION");
    if updater::configured_repo().is_none() {
        return format!("v{version} · developer build");
    }
    match updater::build_sha().and_then(|sha| sha.get(..8)) {
        Some(sha) => format!("v{version} · {sha}"),
        None => format!("v{version}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serabot_desktop::{
        ConnectorStatus, ConversationSummary, MemoryEntry, Message, ProviderStatus, SecretStatus,
        validate_endpoint,
    };

    fn app() -> Desktop {
        Desktop {
            state: State::default(),
            worker: Some(Worker::start().unwrap()),
            credentials: None,
            events: None,
            events_active: false,
            events_dirty: false,
            last_event_id: 0,
            last_event_kind: String::new(),
            connect: ConnectState::new(String::new()),
            conversation: "windows-owner".into(),
            section: Section::Chat,
            composer: String::new(),
            conv_search: String::new(),
            fleet: FleetDraft::default(),
            memory: MemoryDraft::default(),
            rename: None,
            archive_confirm: None,
            chat: ChatView::default(),
            pending: None,
            scheduled: None,
            next_request: Instant::now(),
            refreshing: false,
            reviewed_unknown: false,
            confirmed_egress: false,
            poll_history: false,
            poll_paused: false,
            poll_approvals: false,
            model_open: false,
            palette_open: false,
            palette_focus: false,
            palette_query: String::new(),
            toast: None,
            update: UpdateUi::default(),
        }
    }

    fn summary(id: &str, kind: &str, title: &str) -> ConversationSummary {
        ConversationSummary {
            id: id.into(),
            kind: kind.into(),
            parent_id: (kind == "group").then(|| "sec:finance".into()),
            title: Some(title.into()),
            created_at: "2026-10-29T10:00:00.000Z".into(),
            updated_at: "2026-10-29T10:05:00.000Z".into(),
            last_message_at: Some("2026-10-29T10:05:00.000Z".into()),
            archived_at: None,
            accepts_turns: matches!(kind, "direct" | "room"),
        }
    }

    fn connected_app() -> Desktop {
        let mut app = app();
        app.state.connected = true;
        app.credentials = Some(Arc::new(Credentials {
            endpoint: validate_endpoint("http://127.0.0.1:1").unwrap(),
            token: SecretString::new("x".repeat(32)),
        }));
        app.state.history_loaded = true;
        app.state.catalog = Some(
            serde_json::from_str(r#"{"providers":[{"id":"approved","models":["small","large"],"data_egress_approved":true}],"selection":{"provider":"approved","model":"small"}}"#).unwrap(),
        );
        app.state.messages = vec![
            Message {
                id: "m1".into(),
                role: Role::User,
                content: "Synthetic question about a spreadsheet.".into(),
                created_at: "2026-10-29T10:00:00.000Z".into(),
                name: None,
            },
            Message {
                id: "m2".into(),
                role: Role::Assistant,
                content: "Synthetic answer with a longer body so the bubble wraps across the available width and the virtualized layout has to measure a multi-line galley.".into(),
                created_at: "2026-10-29T10:01:00.000Z".into(),
                name: Some("analyst".into()),
            },
        ];
        app.state.conversations = vec![
            summary("windows-owner", "direct", "Owner"),
            summary("room:review", "room", "Review room"),
            summary("grp:ops", "group", "Operations"),
        ];
        app.state.conversations_loaded = true;
        app.state.memories_loaded = true;
        app.state.memories = vec![MemoryEntry {
            id: "mem_1".into(),
            kind: "fact".into(),
            content: "Synthetic memory fixture.".into(),
            source: "operator:desktop".into(),
            created_at: "2026-10-29T10:00:00.000Z".into(),
            updated_at: "2026-10-29T10:00:00.000Z".into(),
        }];
        app.state.providers = vec![ProviderStatus {
            id: "approved".into(),
            kind: "openai".into(),
            models: vec!["small".into(), "large".into()],
            routing_classes: vec!["default".into()],
            data_egress_approved: true,
            catalog_fetch: true,
            catalog_verified: true,
            secrets: [(
                "api_key".into(),
                SecretStatus {
                    name: Some("OPENAI_API_KEY".into()),
                    configured: true,
                },
            )]
            .into_iter()
            .collect(),
        }];
        app.state.providers_loaded = true;
        app.state.connectors = vec![ConnectorStatus {
            id: "notion".into(),
            kind: "oauth".into(),
            enabled: false,
            redirect_uri: Some("https://bot.example/v1/oauth/notion/callback".into()),
            api_base: None,
            default_inbox_id: None,
            secrets: [(
                "client_id".into(),
                SecretStatus {
                    name: Some("NOTION_CLIENT_ID".into()),
                    configured: false,
                },
            )]
            .into_iter()
            .collect(),
        }];
        app.state.connectors_loaded = true;
        app
    }

    #[test]
    fn scheduled_background_poll_does_not_lock_out_manual_reads_or_new_conversation() {
        let mut app = app();
        app.schedule(Operation::Task("task".into()), POLL_GAP);
        assert!(app.ready());
        app.pending = Some(OperationKind::Task);
        assert!(!app.ready());
        app.pending = None;
        app.refreshing = true;
        assert!(!app.ready());
    }

    #[test]
    fn scheduled_requests_respect_global_cooldown() {
        let mut app = app();
        app.next_request = Instant::now() + REQUEST_GAP;
        app.schedule(Operation::History, Duration::ZERO);
        assert!(app.scheduled.as_ref().unwrap().0 >= app.next_request);
        assert!(!app.ready());
        assert!(REQUEST_GAP >= Duration::from_secs(3));
        assert!(POLL_GAP >= Duration::from_secs(5));
    }

    #[test]
    fn disconnect_wipes_inputs_credentials_and_pending_actions() {
        let mut app = app();
        app.connect.token = Zeroizing::new("synthetic-token".into());
        app.composer = "sensitive synthetic text".into();
        app.fleet.bot_id = "notes".into();
        app.fleet.bot_description = "synthetic description".into();
        app.fleet.skill_id = "notes-pack".into();
        app.fleet.skill_description = "synthetic summary".into();
        app.fleet.skill_body = "synthetic skill body".into();
        app.memory.content = "synthetic memory".into();
        app.credentials = Some(Arc::new(Credentials {
            endpoint: validate_endpoint("http://127.0.0.1:1").unwrap(),
            token: SecretString::new("x".repeat(32)),
        }));
        app.schedule(Operation::History, POLL_GAP);
        let old = app.state.scope.clone();
        app.disconnect(&egui::Context::default());
        assert!(app.connect.token.is_empty());
        assert!(app.composer.is_empty());
        assert!(app.fleet.bot_id.is_empty());
        assert!(app.fleet.bot_description.is_empty());
        assert!(app.fleet.skill_id.is_empty());
        assert!(app.fleet.skill_description.is_empty());
        assert!(app.fleet.skill_body.is_empty());
        assert!(app.memory.content.is_empty());
        assert!(app.credentials.is_none());
        assert!(app.scheduled.is_none());
        assert!(!app.state.scope.accepts(&old));
    }

    #[cfg(not(windows))]
    #[test]
    fn forgetting_the_token_returns_to_the_connect_screen_and_keeps_the_server() {
        let mut app = app();
        app.state.connected = true;
        app.connect.server = "https://owner.example".into();
        app.connect.token = Zeroizing::new("synthetic-forget-token-000000000000".into());
        app.connect.persisted = true;
        app.credentials = Some(Arc::new(Credentials {
            endpoint: validate_endpoint("https://owner.example").unwrap(),
            token: SecretString::new("synthetic-forget-token-000000000000".into()),
        }));
        app.forget_token(&egui::Context::default());
        assert!(app.credentials.is_none());
        assert!(!app.state.connected);
        assert!(app.connect.token.is_empty());
        assert!(!app.connect.persisted);
        assert_eq!(app.connect.server, "https://owner.example");
        assert!(app.connect.error.is_none());
    }

    #[cfg(not(windows))]
    #[test]
    fn a_rejected_token_is_dropped_and_explained() {
        let mut app = app();
        app.state.connected = true;
        app.connect.token = Zeroizing::new("synthetic-revoked-token-00000000000".into());
        app.connect.persisted = true;
        app.credentials = Some(Arc::new(Credentials {
            endpoint: validate_endpoint("https://owner.example").unwrap(),
            token: SecretString::new("synthetic-revoked-token-00000000000".into()),
        }));
        app.reject_token(&egui::Context::default());
        assert!(app.credentials.is_none());
        assert!(!app.state.connected);
        assert!(app.connect.token.is_empty());
        assert!(!app.connect.persisted);
        assert_eq!(app.connect.server, "https://owner.example");
        assert!(
            app.connect
                .error
                .as_deref()
                .unwrap()
                .contains("Token rejected or revoked")
        );
    }

    #[test]
    fn opening_a_conversation_keeps_fleet_drafts_and_directory_data() {
        let mut app = connected_app();
        app.fleet.bot_id = "notes".into();
        app.fleet.bot_description = "synthetic description".into();
        app.fleet.skill_id = "notes-pack".into();
        app.fleet.skill_body = "synthetic skill body".into();
        app.fleet.conv_slug = "finance".into();
        app.open_conversation("dir:analyst".into());
        assert_eq!(app.fleet.bot_id, "notes");
        assert_eq!(app.fleet.bot_description, "synthetic description");
        assert_eq!(app.fleet.skill_id, "notes-pack");
        assert_eq!(app.fleet.skill_body, "synthetic skill body");
        assert_eq!(app.fleet.conv_slug, "finance");
        assert_eq!(app.state.conversations.len(), 3);
        assert_eq!(app.state.memories.len(), 1);
        assert_eq!(app.state.providers.len(), 1);
        assert_eq!(app.section, Section::Chat);
        app.reset_scope(false);
        assert!(app.fleet.bot_id.is_empty());
        assert!(app.fleet.bot_description.is_empty());
        assert!(app.fleet.skill_id.is_empty());
        assert!(app.fleet.skill_body.is_empty());
        assert!(app.fleet.conv_slug.is_empty());
        assert!(app.state.conversations.is_empty());
    }

    #[test]
    fn conversation_titles_and_kinds_come_from_the_directory() {
        let mut app = connected_app();
        app.conversation = "room:review".into();
        assert_eq!(app.conversation_title(), "Review room");
        assert_eq!(app.conversation_kind(), Some("room"));
        app.conversation = "windows-missing".into();
        assert_eq!(app.conversation_title(), "windows-missing");
        assert_eq!(app.conversation_kind(), None);
        assert!(app.conversation_accepts_turns());
    }

    #[test]
    fn palette_filters_commands_and_runs_section_jumps() {
        let mut app = connected_app();
        app.open_palette();
        app.palette_query = "memory".into();
        let matches = app
            .palette_commands()
            .into_iter()
            .filter(|(label, _)| label.to_lowercase().contains("memory"))
            .collect::<Vec<_>>();
        assert_eq!(matches.len(), 1);
        assert!(matches!(matches[0].1, PaletteAction::Go(Section::Memory)));
        app.run_palette(
            PaletteAction::Go(Section::Memory),
            &egui::Context::default(),
        );
        assert_eq!(app.section, Section::Memory);
        assert!(app.state.memories_loaded, "cached data stays");
    }

    #[test]
    fn events_coalesce_into_a_single_refresh_and_ignore_reads_while_disconnected() {
        let mut app = connected_app();
        app.events_dirty = true;
        app.last_event_kind = "approval.requested".into();
        app.flush_events();
        assert!(!app.events_dirty);
        assert!(matches!(app.pending, Some(OperationKind::Approvals)));
        let mut offline = self::app();
        offline.events_active = true;
        offline.events_dirty = true;
        offline.flush_events();
        assert!(offline.pending.is_none());
        assert!(
            offline.scheduled.is_none(),
            "offline events never schedule reads"
        );
    }

    #[test]
    fn native_screens_render_at_minimum_and_default_window_sizes() {
        for size in [[860.0, 560.0], [1280.0, 800.0]] {
            let mut app = connected_app();
            for section in [
                Section::Chat,
                Section::Conversations,
                Section::Memory,
                Section::Providers,
                Section::Connectors,
                Section::Approvals,
                Section::Fleet,
                Section::Settings,
            ] {
                app.section = section;
                let ctx = egui::Context::default();
                design::apply_theme(&ctx);
                let input = egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(size[0], size[1]),
                    )),
                    ..Default::default()
                };
                let output = ctx.run(input, |ctx| {
                    app.rail(ctx);
                    if app.section == Section::Chat {
                        app.conversation_column(ctx);
                    }
                    egui::TopBottomPanel::top("topbar").show(ctx, |ui| app.top_bar(ui, ctx));
                    egui::CentralPanel::default().show(ctx, |ui| match app.section {
                        Section::Chat => app.chat_screen(ui),
                        Section::Conversations => app.conversations_screen(ui),
                        Section::Memory => app.memory_screen(ui),
                        Section::Providers => app.providers_screen(ui),
                        Section::Connectors => app.connectors_screen(ui),
                        Section::Approvals => app.approvals_screen(ui, ctx),
                        Section::Fleet => app.fleet_screen(ui),
                        Section::Settings => app.settings_screen(ui, ctx),
                    });
                });
                assert!(!output.shapes.is_empty(), "{section:?} at {size:?}");
            }
        }
    }

    #[test]
    fn the_footer_always_carries_the_active_build() {
        let footer = build_footer();
        assert!(footer.contains(env!("CARGO_PKG_VERSION")));
        assert!(footer.contains("developer build") || footer.contains(" · "));
    }
}
