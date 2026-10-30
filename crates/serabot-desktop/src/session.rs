//! UI-independent owner session: connection, scheduling, reads and mutations.
//!
//! The Dioxus desktop shell drives this on a timer; all network work stays on
//! the worker and SSE threads, never on the UI thread.

use crate::{
    ApiError, ApprovalDecision, AudioUpdate, BotProfileUpdate, ConnectorUpdate, ConversationKind,
    MAX_SKILL_BODY_BYTES, MAX_SKILL_DESCRIPTION_CHARS, Message, NewProvider, Role,
    SPECIALIST_TOOLS, Selection, TaskStatus, TeamMode,
    api::{
        MEMORY_KINDS, valid_conversation_title, valid_credential, valid_memory_content,
        valid_memory_kind, valid_memory_source, valid_model_id, valid_provider_id,
        valid_secret_name,
    },
    creds,
    events::{EventStream, EventUpdate},
    state::State,
    teams, updater, valid_bot_description, valid_bot_id, valid_conversation_parent, valid_id,
    valid_new_skill_id, valid_skill_body, valid_skill_description, valid_slug, valid_text,
    valid_token, validate_team_members, validate_tool_list,
    worker::{Credentials, Job, Operation, OperationKind, Reply, Worker},
};
use secrecy::ExposeSecret;
use std::{
    collections::BTreeMap,
    sync::Arc,
    time::{Duration, Instant},
};
use zeroize::Zeroize;

pub const REQUEST_GAP: Duration = Duration::from_millis(60);
pub const POLL_GAP: Duration = Duration::from_millis(750);
pub const POLL_SAFETY: Duration = Duration::from_secs(4);

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
    pub checking: bool,
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
    pub selected_bot: Option<String>,
    pub bot_profile: BotProfileDraft,
    pub admin_notice: Option<String>,
    /// Connector whose Test probe is in flight.
    pub connector_testing: Option<String>,
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
    pub toast: Option<String>,
    /// Artifact download in flight (worker) or file write in flight (thread).
    pub downloading: bool,
    download_rx: Option<std::sync::mpsc::Receiver<Result<String, String>>>,
    pending: Option<OperationKind>,
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
            selected_bot: None,
            bot_profile: BotProfileDraft::default(),
            admin_notice: None,
            connector_testing: None,
            admin_error: None,
            restart_pending: false,
            restarting: false,
            rename: None,
            archive_confirm: None,
            team_rules_draft: String::new(),
            update: UpdateUi::default(),
            status: StatusUi::default(),
            toast: None,
            downloading: false,
            download_rx: None,
            pending: None,
            pending_team: None,
            pending_room_refresh: false,
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
        self.pending_team = None;
        self.pending_room_refresh = false;
        if !connected {
            self.status.status = None;
            self.status.rx = None;
            self.status.loading = false;
            self.status.error = None;
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
            self.selected_bot = None;
            self.bot_profile = BotProfileDraft::default();
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
        if section != Section::Fleet {
            self.selected_bot = None;
        }
        if section == Section::Settings {
            self.refresh_status();
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
            Ok(path) => self.toast = Some(format!("Saved to {path}")),
            Err(error) => self.toast = Some(format!("Download failed: {error}")),
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
        if let Some(bot) = conversation.id.strip_prefix("dir:") {
            if self.state.listed_bots.iter().any(|item| item.id == bot) {
                return self.bot_display_name(bot);
            }
        }
        let kind = conversation.normalized_kind();
        let slug = conversation
            .id
            .split_once(':')
            .map(|(_, slug)| slug)
            .unwrap_or(&conversation.id);
        if conversation.id.chars().all(|c| c.is_ascii_digit()) {
            let tail = &conversation.id[conversation.id.len().saturating_sub(4)..];
            return format!("Chat {tail}");
        }
        match kind {
            "section" => format!("Section · {}", humanize_label(slug)),
            "group" => format!("Group · {}", humanize_label(slug)),
            "room" => format!("Room · {}", humanize_label(slug)),
            _ => humanize_label(slug),
        }
    }

    pub fn conversation_subtitle(&self, conversation: &crate::ConversationSummary) -> String {
        if conversation.id == "windows-owner" {
            return "Your direct chat".into();
        }
        if let Some(bot) = conversation.id.strip_prefix("dir:") {
            if let Some(found) = self.state.listed_bots.iter().find(|item| item.id == bot) {
                return format!("Bot · {}", found.role);
            }
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
        self.poll_status();
        self.poll_download();
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
                        OperationKind::AdminWrite
                            | OperationKind::ConnectorTest
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
                    self.scheduled = None;
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
                        self.next_request = Instant::now() + Duration::from_secs(60);
                    }
                }
                Ok(reply) => {
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
                            if let Some(id) = self.selected_bot.clone() {
                                if let Some(bot) =
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
                            }
                            if self.section == Section::Fleet && !self.state.skills_loaded {
                                self.schedule(Operation::ListSkills, Duration::ZERO);
                            }
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
        if self.downloading {
            self.toast = Some("A download is already running.".into());
            return;
        }
        if !self.ready() {
            self.toast = Some("Wait for the current request, then retry.".into());
            return;
        }
        self.start_request(Operation::FetchArtifact { hash });
        if self.pending.is_none() {
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
    let path = dir.join(&sanitized);
    std::fs::write(&path, bytes).map_err(|err| err.to_string())?;
    Ok(path.to_string_lossy().into_owned())
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

#[cfg(test)]
mod tests {
    use super::{EventRefresh, event_refresh};

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
}
