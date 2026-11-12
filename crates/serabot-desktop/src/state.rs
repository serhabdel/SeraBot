use crate::{
    ApiError, Approval, AuthUser, BotDetail, BotSummary, Catalog, ConnectorStatus,
    ConnectorTestResult, ConversationSummary, CreatedBot, CreatedConversation, CreatedGroup,
    CreatedSkill, LibraryEntry, MemoryEntry, MemoryStatsView, Message, Onboarding, ProviderStatus,
    Role, Scope, Selection, ServerHealth, ShippedBot, SkillSummary, Task, Team, UsageReport,
    avatars::AvatarImage,
    worker::{OperationKind, Reply},
};
use std::collections::{BTreeMap, HashSet};

/// Merge a server transcript with what is already on screen. Rows with the same
/// id are updated in place; optimistic `opt-*` user rows are paired by content.
pub fn merge_transcript(current: &[Message], incoming: Vec<Message>) -> Vec<Message> {
    if current.is_empty() {
        return incoming;
    }
    let mut used_opts = HashSet::new();
    let mut merged = Vec::with_capacity(incoming.len().max(current.len()));

    for inc in incoming {
        if let Some(cur) = current.iter().find(|row| row.id == inc.id) {
            merged.push(merge_transcript_row(cur, &inc));
            continue;
        }
        if let Some((idx, _cur)) = current.iter().enumerate().find(|(i, row)| {
            row.id.starts_with("opt-") && !used_opts.contains(i) && pairs_optimistic(row, &inc)
        }) {
            used_opts.insert(idx);
            merged.push(inc);
            continue;
        }
        merged.push(inc);
    }

    for (idx, cur) in current.iter().enumerate() {
        if cur.id.starts_with("opt-")
            && !used_opts.contains(&idx)
            && !merged.iter().any(|row| row.id == cur.id)
        {
            merged.push(cur.clone());
        }
    }

    merged
}

fn transcript_row_equal(a: &Message, b: &Message) -> bool {
    a.id == b.id
        && a.role == b.role
        && a.content == b.content
        && a.created_at == b.created_at
        && a.bot == b.bot
        && a.name == b.name
        && a.limit_reached == b.limit_reached
        && a.attachments == b.attachments
}

fn pairs_optimistic(local: &Message, server: &Message) -> bool {
    local.role == server.role
        && local.role == Role::User
        && local.content == server.content
        && local.attachments.len() == server.attachments.len()
}

/// Combine one server row with what is already on screen. Assistant rows keep
/// the longer body when a stale history response would shorten streaming text.
fn merge_transcript_row(cur: &Message, inc: &Message) -> Message {
    if transcript_row_equal(cur, inc) {
        return cur.clone();
    }
    let mut row = if assistant_content_regressed(cur, inc) {
        let mut kept = inc.clone();
        kept.content = cur.content.clone();
        kept
    } else {
        inc.clone()
    };
    if inc.limit_reached {
        row.limit_reached = true;
    }
    if inc.attachments.len() > row.attachments.len() {
        row.attachments = inc.attachments.clone();
    }
    row
}

fn assistant_content_regressed(cur: &Message, inc: &Message) -> bool {
    cur.id == inc.id
        && cur.role == inc.role
        && cur.role == Role::Assistant
        && cur.content.chars().count() > inc.content.chars().count()
}

pub struct State {
    pub scope: Scope,
    pub connected: bool,
    /// The signed-in user from `GET /v1/me`; absent on an older server.
    pub user: Option<AuthUser>,
    pub version: String,
    pub audio_enabled: Option<bool>,
    pub audio_transcription_model: Option<String>,
    /// Latest voice/OCR readiness from the server, applied to the session's
    /// editable draft once.
    #[allow(dead_code)]
    pub voice: Option<crate::api::VoiceSettings>,
    pub audio_understanding_model: Option<String>,
    pub firecrawl_enabled: Option<bool>,
    pub telegram_enabled: Option<bool>,
    pub catalog_fetch: Option<bool>,
    pub bot_chat_binding: Option<bool>,
    pub bot_profiles: Option<bool>,
    pub conversation_titles: Option<bool>,
    pub catalog: Option<Catalog>,
    pub draft: Selection,
    pub messages: Vec<Message>,
    /// Chat cards of the open conversation (0.23), oldest first.
    pub cards: Vec<crate::ChatCard>,
    pub history_loaded: bool,
    pub task: Option<Task>,
    /// Live steps of the latest turn, from the event stream.
    pub activity: crate::activity::Activity,
    /// Which bots are working now (rail, header and room rings).
    pub working: crate::working::WorkingBots,
    pub approvals: Vec<Approval>,
    pub approvals_loaded: bool,
    pub standing_grants: crate::standing_grants::StandingGrantsSnapshot,
    pub standing_grants_loaded: bool,
    pub jobs_reliability: crate::jobs_reliability::JobsReliabilitySnapshot,
    pub jobs_reliability_loaded: bool,
    pub conversations: Vec<ConversationSummary>,
    pub conversations_loaded: bool,
    /// Archived threads, loaded only when the rail expands the group.
    pub archived: Vec<ConversationSummary>,
    pub archived_loaded: bool,
    pub memories: Vec<MemoryEntry>,
    pub memories_loaded: bool,
    /// Personal tasks of the Tasks tab (0.25.0), owner-scoped.
    pub tasks: Vec<crate::api::TaskEntry>,
    pub tasks_loaded: bool,
    pub task_prefs: Option<crate::api::TaskPrefs>,
    pub task_sync: Option<crate::api::TaskSyncStatus>,
    pub task_events: Vec<crate::api::TaskEvent>,
    /// Header counts for the Memory page; `None` until fetched or after a change.
    pub memory_stats: Option<MemoryStatsView>,
    pub providers: Vec<ProviderStatus>,
    pub providers_loaded: bool,
    pub connectors: Vec<ConnectorStatus>,
    pub connectors_loaded: bool,
    /// Latest "Test" result per connector id; cleared on disconnect.
    pub connector_tests: std::collections::BTreeMap<String, ConnectorTestResult>,
    /// Latest provider Test result per provider id.
    pub provider_tests: std::collections::BTreeMap<String, crate::api::ProviderTest>,
    /// Provider id and device code of the last finished sign-in step.
    pub oauth_result: Option<(String, Option<String>)>,
    /// Team of the open room conversation. Conversation-scoped: it is cleared
    /// whenever the conversation changes or the session disconnects.
    pub team: Option<Team>,
    pub team_loaded: bool,
    pub team_loading: bool,
    pub team_error: Option<String>,
    /// Artifact library of the open conversation, conversation-scoped.
    pub library: Vec<LibraryEntry>,
    pub library_loaded: bool,
    pub library_loading: bool,
    pub library_error: Option<String>,
    /// Background media jobs of the open conversation (0.24), conversation-scoped.
    pub media_jobs: Vec<crate::api::MediaJob>,
    /// The Logs tab (0.28.0): the latest read of `GET /v1/logs`.
    pub logs: Vec<crate::logs::LogEntry>,
    pub logs_loaded: bool,
    /// False once the server answers 404 on `/v1/logs`: the tab then says the
    /// route is not available instead of retrying forever.
    pub logs_supported: bool,
    /// The Workflows tab (0.29.0): latest visibility bundle.
    pub workflows: crate::workflows::WorkflowsSnapshot,
    pub workflows_loaded: bool,
    pub workflows_error: Option<String>,
    /// Server-rendered artifact previews by hash (0.28.0). `None` values mean
    /// "no preview; use download/open". A hash has at most one read in flight,
    /// tracked by the session.
    pub doc_previews: BTreeMap<String, Option<crate::preview::ArtifactPreview>>,
    /// Media files already read back for inline players (capped, 0.24).
    pub media_previews: crate::media_view::PreviewCache,
    /// The owner's per-turn work limits (0.24), once read or saved.
    pub turn_limits: Option<crate::api::TurnLimitsView>,
    pub turn_limits_error: Option<String>,
    pub memory_settings: Option<crate::api::MemorySettingsView>,
    pub memory_settings_error: Option<String>,
    /// Goal and todo list of the open conversation, conversation-scoped.
    pub plan: Option<crate::chat_plan::ChatPlan>,
    /// The owner's auto-approve switch for the open conversation. Defaults
    /// to off and is only ever set from a server reply.
    pub auto_approve: bool,
    /// Last transcript of a recorded note. Untrusted model output: it is a
    /// draft for the owner, never a sent message.
    pub transcript: Option<String>,
    pub transcript_error: Option<String>,
    pub transcript_loading: bool,
    pub created_bot: Option<CreatedBot>,
    pub created_conversation: Option<CreatedConversation>,
    /// The Marketplace catalog with this user's state (owner-scoped).
    pub marketplace: Vec<crate::marketplace::PluginSummary>,
    pub marketplace_loaded: bool,
    /// A server without the marketplace routes (404): the screen says so.
    pub marketplace_unavailable: bool,
    pub marketplace_error: Option<String>,
    /// The open plugin page.
    pub marketplace_detail: Option<crate::marketplace::PluginDetail>,
    /// Each bot's chats (owner-scoped, kept across conversation switches).
    pub bot_chats: BTreeMap<String, crate::bot_chats::BotChatList>,
    /// Bots whose chat list has been read at least once.
    pub bot_chats_loaded: std::collections::BTreeSet<String>,
    /// A chat the owner just started: `(bot, chat id, carried_from)`. The
    /// session opens it and clears this.
    pub created_bot_chat: Option<(String, String, Option<String>)>,
    pub created_skill: Option<CreatedSkill>,
    pub created_group: Option<CreatedGroup>,
    /// The bot onboarding interview, owner-scoped like the bot list.
    pub onboarding: Option<Onboarding>,
    /// Fetched avatars by `AvatarTarget::key`. Owner-scoped.
    pub avatars: BTreeMap<String, AvatarImage>,
    pub listed_skills: Vec<SkillSummary>,
    pub skills_loaded: bool,
    pub listed_bots: Vec<BotSummary>,
    pub bots_loaded: bool,
    /// Shipped templates for "Meet your team", owner-scoped like the bot list.
    pub shipped_bots: Vec<ShippedBot>,
    pub shipped_loaded: bool,
    /// The shipped bot being added right now; one write at a time.
    pub shipped_installing: Option<String>,
    /// Error text for the "Meet your team" sheet, kept out of the global banner.
    pub shipped_error: Option<String>,
    /// Cached `GET /v1/bots/{id}` records, keyed by bot id. Owner-scoped:
    /// they survive conversation switches and are cleared on disconnect.
    pub bot_details: BTreeMap<String, BotDetail>,
    /// Latest `GET /v1/usage` report. Owner-scoped like the bot details.
    pub usage: Option<UsageReport>,
    /// Latest GET /v1/admin/status (admin only). Cleared on disconnect.
    pub server_health: Option<ServerHealth>,
    pub error: Option<String>,
    pub unknown_outcome: bool,
}

impl Default for State {
    fn default() -> Self {
        Self {
            scope: Scope {
                epoch: 0,
                conversation: "windows-owner".into(),
            },
            connected: false,
            user: None,
            version: String::new(),
            audio_enabled: None,
            audio_transcription_model: None,
            audio_understanding_model: None,
            voice: None,
            firecrawl_enabled: None,
            telegram_enabled: None,
            catalog_fetch: None,
            bot_chat_binding: None,
            bot_profiles: None,
            conversation_titles: None,
            catalog: None,
            draft: Selection::default(),
            messages: Vec::new(),
            cards: Vec::new(),
            history_loaded: false,
            task: None,
            activity: Default::default(),
            working: Default::default(),
            approvals: Vec::new(),
            approvals_loaded: false,
            standing_grants: crate::standing_grants::StandingGrantsSnapshot::default(),
            standing_grants_loaded: false,
            jobs_reliability: crate::jobs_reliability::JobsReliabilitySnapshot::default(),
            jobs_reliability_loaded: false,
            conversations: Vec::new(),
            conversations_loaded: false,
            archived: Vec::new(),
            archived_loaded: false,
            memories: Vec::new(),
            memories_loaded: false,
            tasks: Vec::new(),
            tasks_loaded: false,
            task_prefs: None,
            task_sync: None,
            task_events: Vec::new(),
            memory_stats: None,
            providers: Vec::new(),
            providers_loaded: false,
            connectors: Vec::new(),
            connectors_loaded: false,
            connector_tests: std::collections::BTreeMap::new(),
            provider_tests: std::collections::BTreeMap::new(),
            oauth_result: None,
            team: None,
            team_loaded: false,
            team_loading: false,
            team_error: None,
            library: Vec::new(),
            library_loaded: false,
            library_loading: false,
            library_error: None,
            media_jobs: Vec::new(),
            logs: Vec::new(),
            logs_loaded: false,
            logs_supported: true,
            workflows: crate::workflows::WorkflowsSnapshot::default(),
            workflows_loaded: false,
            workflows_error: None,
            doc_previews: BTreeMap::new(),
            media_previews: crate::media_view::PreviewCache::default(),
            turn_limits: None,
            turn_limits_error: None,
            memory_settings: None,
            memory_settings_error: None,
            plan: None,
            auto_approve: false,
            transcript: None,
            transcript_error: None,
            transcript_loading: false,
            created_bot: None,
            created_conversation: None,
            marketplace: Vec::new(),
            marketplace_loaded: false,
            marketplace_unavailable: false,
            marketplace_error: None,
            marketplace_detail: None,
            bot_chats: BTreeMap::new(),
            bot_chats_loaded: std::collections::BTreeSet::new(),
            created_bot_chat: None,
            created_skill: None,
            created_group: None,
            onboarding: None,
            avatars: BTreeMap::new(),
            listed_skills: Vec::new(),
            skills_loaded: false,
            listed_bots: Vec::new(),
            bots_loaded: false,
            shipped_bots: Vec::new(),
            shipped_loaded: false,
            shipped_installing: None,
            shipped_error: None,
            bot_details: BTreeMap::new(),
            usage: None,
            server_health: None,
            error: None,
            unknown_outcome: false,
        }
    }
}

impl State {
    pub fn new_scope(&mut self, conversation: String, connected: bool) {
        let epoch = self.scope.epoch.wrapping_add(1);
        let user = self.user.clone();
        let version = self.version.clone();
        let audio_enabled = self.audio_enabled;
        let audio_transcription_model = self.audio_transcription_model.clone();
        let audio_understanding_model = self.audio_understanding_model.clone();
        let firecrawl_enabled = self.firecrawl_enabled;
        let telegram_enabled = self.telegram_enabled;
        let catalog_fetch = self.catalog_fetch;
        let bot_chat_binding = self.bot_chat_binding;
        let bot_profiles = self.bot_profiles;
        let conversation_titles = self.conversation_titles;
        let created_bot = self.created_bot.clone();
        let created_conversation = self.created_conversation.clone();
        let marketplace = self.marketplace.clone();
        let marketplace_loaded = self.marketplace_loaded;
        let marketplace_unavailable = self.marketplace_unavailable;
        let marketplace_error = self.marketplace_error.clone();
        let marketplace_detail = self.marketplace_detail.clone();
        let bot_chats = self.bot_chats.clone();
        let bot_chats_loaded = self.bot_chats_loaded.clone();
        let created_bot_chat = self.created_bot_chat.clone();
        let created_skill = self.created_skill.clone();
        let created_group = self.created_group.clone();
        let onboarding = self.onboarding.clone();
        let avatars = self.avatars.clone();
        let listed_skills = self.listed_skills.clone();
        let skills_loaded = self.skills_loaded;
        let listed_bots = self.listed_bots.clone();
        let bots_loaded = self.bots_loaded;
        let shipped_bots = self.shipped_bots.clone();
        let shipped_loaded = self.shipped_loaded;
        let bot_details = self.bot_details.clone();
        let usage = self.usage.clone();
        let server_health = self.server_health.clone();
        // Directory data is owner-scoped but not conversation-scoped.
        let conversations = self.conversations.clone();
        let conversations_loaded = self.conversations_loaded;
        let memories = self.memories.clone();
        let memories_loaded = self.memories_loaded;
        let tasks = self.tasks.clone();
        let tasks_loaded = self.tasks_loaded;
        let task_prefs = self.task_prefs.clone();
        let task_sync = self.task_sync.clone();
        let task_events = self.task_events.clone();
        let providers = self.providers.clone();
        let providers_loaded = self.providers_loaded;
        let connectors = self.connectors.clone();
        let connectors_loaded = self.connectors_loaded;
        let connector_tests = self.connector_tests.clone();
        let provider_tests = self.provider_tests.clone();
        *self = Self {
            scope: Scope {
                epoch,
                conversation,
            },
            connected,
            user,
            version,
            audio_enabled,
            audio_transcription_model,
            audio_understanding_model,
            firecrawl_enabled,
            telegram_enabled,
            catalog_fetch,
            bot_chat_binding,
            bot_profiles,
            conversation_titles,
            created_bot,
            created_conversation,
            marketplace,
            marketplace_loaded,
            marketplace_unavailable,
            marketplace_error,
            marketplace_detail,
            bot_chats,
            bot_chats_loaded,
            created_bot_chat,
            created_skill,
            created_group,
            onboarding,
            avatars,
            listed_skills,
            skills_loaded,
            listed_bots,
            bots_loaded,
            shipped_bots,
            shipped_loaded,
            bot_details,
            usage,
            server_health,
            conversations,
            conversations_loaded,
            memories,
            memories_loaded,
            tasks,
            tasks_loaded,
            task_prefs,
            task_sync,
            task_events,
            providers,
            providers_loaded,
            connectors,
            connectors_loaded,
            connector_tests,
            provider_tests,
            ..Self::default()
        };
    }

    pub fn disconnect(&mut self) {
        self.new_scope(self.scope.conversation.clone(), false);
        self.user = None;
        self.created_bot = None;
        self.created_conversation = None;
        self.created_skill = None;
        self.created_group = None;
        self.onboarding = None;
        self.avatars.clear();
        self.listed_skills.clear();
        self.skills_loaded = false;
        self.listed_bots.clear();
        self.bots_loaded = false;
        self.shipped_bots.clear();
        self.shipped_loaded = false;
        self.bot_details.clear();
        self.usage = None;
        self.server_health = None;
        self.conversations.clear();
        self.conversations_loaded = false;
        self.archived.clear();
        self.archived_loaded = false;
        self.memories.clear();
        self.memories_loaded = false;
        self.tasks.clear();
        self.tasks_loaded = false;
        self.task_prefs = None;
        self.task_sync = None;
        self.task_events.clear();
        self.providers.clear();
        self.providers_loaded = false;
        self.connectors.clear();
        self.connectors_loaded = false;
        self.connector_tests.clear();
        self.provider_tests.clear();
    }

    pub fn can_send(&self) -> bool {
        self.connected
            && self.history_loaded
            && !self.unknown_outcome
            && self.task.as_ref().is_none_or(|task| task.status.terminal())
            && self.catalog.as_ref().is_some_and(|catalog| {
                catalog.selection == self.draft && catalog.allows(&catalog.selection)
            })
    }

    pub fn receive(&mut self, scope: &Scope, reply: Reply) -> bool {
        if !self.scope.accepts(scope) {
            return false;
        }
        match reply {
            Reply::Health(health) => {
                self.connected = health.ok;
                self.version = health.version;
                self.audio_enabled = health.audio_enabled;
                self.audio_transcription_model = health.audio_transcription_model;
                self.audio_understanding_model = health.audio_understanding_model;
                self.firecrawl_enabled = health.firecrawl_enabled;
                self.telegram_enabled = health.telegram_enabled;
                self.catalog_fetch = health.catalog_fetch;
                self.bot_chat_binding = health.bot_chat_binding;
                self.bot_profiles = health.bot_profiles;
                self.conversation_titles = health.conversation_titles;
                if !health.ok {
                    self.error = Some("Server is not ready. Retry the connection read.".into());
                }
            }
            Reply::Me(user) => self.user = Some(user),
            Reply::Catalog(catalog) => {
                self.draft = catalog.selection.clone();
                self.catalog = Some(catalog);
            }
            Reply::History(history) => {
                self.messages = merge_transcript(&self.messages, history.messages);
                self.cards = history.cards;
                self.history_loaded = true;
                // Fetching the transcript marks the thread read on the server;
                // the badge clears locally at the same moment.
                if let Some(conversation) = self
                    .conversations
                    .iter_mut()
                    .find(|conversation| conversation.id == self.scope.conversation)
                {
                    conversation.unread_count = 0;
                }
            }
            Reply::Selection(selection) => {
                self.draft = selection.clone();
                if let Some(catalog) = &mut self.catalog {
                    catalog.selection = selection;
                }
            }
            Reply::Task(task) => self.task = Some(task),
            Reply::Approvals(approvals) => {
                self.approvals = approvals;
                self.approvals_loaded = true;
            }
            Reply::StandingGrants(snapshot) => {
                self.standing_grants = snapshot;
                self.standing_grants_loaded = true;
            }
            Reply::StandingGrantRevoked => {}
            Reply::JobsReliability(snapshot) => {
                self.jobs_reliability = snapshot;
                self.jobs_reliability_loaded = true;
            }
            Reply::Decided => {}
            Reply::CreatedBot(bot) => self.created_bot = Some(bot),
            Reply::CreatedConversation(node) => self.created_conversation = Some(node),
            Reply::CreatedSkill(skill) => self.created_skill = Some(skill),
            Reply::MarketplacePlugins(plugins) => {
                self.marketplace = plugins;
                self.marketplace_loaded = true;
                self.marketplace_unavailable = false;
                self.marketplace_error = None;
            }
            Reply::MarketplaceDetail(detail) => {
                self.marketplace_detail = Some(*detail);
                self.marketplace_error = None;
            }
            Reply::MarketplaceChanged {
                slug,
                plugins,
                detail,
            } => {
                self.marketplace = plugins;
                self.marketplace_loaded = true;
                self.marketplace_unavailable = false;
                self.marketplace_error = None;
                if let Some(detail) = detail {
                    self.marketplace_detail = Some(*detail);
                } else if self
                    .marketplace_detail
                    .as_ref()
                    .is_some_and(|page| page.plugin.slug == slug)
                {
                    // The page could not be re-read. Drop the stale copy so
                    // its buttons don't describe a state the server didn't
                    // confirm. The session says so.
                    self.marketplace_detail = None;
                }
            }
            Reply::BotChats(list) => {
                self.bot_chats_loaded.insert(list.bot.clone());
                self.bot_chats.insert(list.bot.clone(), list);
            }
            Reply::BotChatCreated { bot, created } => {
                let carried = created.carried_from.clone();
                self.created_bot_chat = Some((bot.clone(), created.chat.id.clone(), carried));
                if let Some(list) = self.bot_chats.get_mut(&bot)
                    && !list.chats.iter().any(|chat| chat.id == created.chat.id)
                {
                    list.chats.push(created.chat);
                }
            }
            Reply::BotRefreshSet(settings) => {
                if let Some(list) = self.bot_chats.get_mut(&settings.bot) {
                    list.refresh_every_turns = settings.refresh_every_turns;
                }
            }
            Reply::Skills(skills) => {
                self.listed_skills = skills;
                self.skills_loaded = true;
            }
            Reply::Bots(bots) => {
                self.listed_bots = bots;
                self.bots_loaded = true;
            }
            Reply::ShippedBots(bots) => {
                self.shipped_bots = bots;
                self.shipped_loaded = true;
                self.shipped_error = None;
            }
            Reply::ShippedBotInstalled(name) => {
                self.shipped_installing = None;
                self.shipped_error = None;
                if let Some(bot) = self.shipped_bots.iter_mut().find(|bot| bot.id == name) {
                    bot.installed = true;
                }
            }
            Reply::BotDetail(detail) => {
                self.bot_details.insert(detail.id.clone(), detail);
            }
            Reply::Usage(report) => {
                self.usage = Some(report);
            }
            Reply::ServerHealth(status) => {
                self.server_health = Some(status);
            }
            Reply::Conversations(page) => {
                self.conversations = page.conversations;
                self.conversations_loaded = true;
            }
            Reply::ArchivedConversations(page) => {
                self.archived = page
                    .conversations
                    .into_iter()
                    .filter(|conversation| conversation.archived_at.is_some())
                    .collect();
                self.archived_loaded = true;
            }
            Reply::RenamedConversation(conversation) => {
                match self
                    .conversations
                    .iter_mut()
                    .find(|existing| existing.id == conversation.id)
                {
                    Some(existing) => *existing = conversation,
                    None => self.conversations.push(conversation),
                }
            }
            Reply::ArchivedConversation(id) => {
                self.conversations
                    .retain(|conversation| conversation.id != id);
            }
            Reply::UnarchivedConversation(id) => {
                self.archived.retain(|conversation| conversation.id != id);
            }
            Reply::Memories(memories) => {
                self.memories = memories;
                self.memories_loaded = true;
            }
            Reply::Tasks(tasks) => {
                self.tasks = tasks;
                self.tasks_loaded = true;
            }
            Reply::AddedTask(task) | Reply::UpdatedTask(task) => {
                if let Some(existing) = self.tasks.iter_mut().find(|t| t.id == task.id) {
                    *existing = task;
                } else {
                    self.tasks.insert(0, task);
                }
                self.tasks_loaded = true;
            }
            Reply::DeletedTask(id) => {
                self.tasks.retain(|task| task.id != id);
            }
            Reply::TaskPrefs(prefs) => {
                self.task_prefs = Some(prefs);
            }
            Reply::TaskSyncStatus(status) => {
                self.task_sync = Some(status);
            }
            Reply::TaskSyncReport(_report) => {
                // The next ListTasks picks up whatever the sync changed.
            }
            Reply::TaskEvents(events) => {
                self.task_events = events;
            }
            Reply::AddedMemory(memory) => {
                if let Some(existing) = self
                    .memories
                    .iter_mut()
                    .find(|existing| existing.id == memory.id)
                {
                    *existing = memory;
                } else {
                    self.memories.insert(0, memory);
                }
                self.memories_loaded = true;
            }
            Reply::UpdatedMemory(memory) => {
                if let Some(existing) = self
                    .memories
                    .iter_mut()
                    .find(|existing| existing.id == memory.id)
                {
                    *existing = memory;
                }
            }
            Reply::MemoryStats(stats) => {
                self.memory_stats = Some(stats);
            }
            Reply::DeletedMemory(id) => {
                self.memories.retain(|memory| memory.id != id);
            }
            Reply::Providers(providers) => {
                self.providers = providers;
                self.providers_loaded = true;
            }
            Reply::Connectors(connectors) => {
                self.connectors = connectors;
                self.connectors_loaded = true;
            }
            Reply::Team(team) => {
                self.team = Some(team);
                self.team_loaded = true;
                self.team_loading = false;
                self.team_error = None;
            }
            Reply::Library(entries) => {
                self.library = entries;
                self.library_loaded = true;
                self.library_loading = false;
                self.library_error = None;
            }
            Reply::MediaJobs(jobs) => self.media_jobs = jobs,
            Reply::Logs(entries) => {
                match entries {
                    Some(entries) => {
                        self.logs = entries;
                        self.logs_supported = true;
                    }
                    // The server predates the logs route: the tab shows a
                    // notice instead of an empty list.
                    None => {
                        self.logs.clear();
                        self.logs_supported = false;
                    }
                }
                self.logs_loaded = true;
            }
            Reply::Workflows(snapshot) => {
                self.workflows = snapshot;
                self.workflows_loaded = true;
                self.workflows_error = None;
            }
            Reply::DocPreview { hash, preview } => {
                self.doc_previews.insert(hash, preview);
            }
            Reply::Preview { hash, data_url } => {
                self.media_previews.insert(hash, data_url);
            }
            Reply::TurnLimits(view) => {
                self.turn_limits = Some(view);
                self.turn_limits_error = None;
            }
            Reply::MemorySettings(view) => {
                self.memory_settings = Some(view);
                self.memory_settings_error = None;
            }
            Reply::Mem0KeySaved | Reply::Mem0KeyDeleted => {
                if let Some(settings) = self.memory_settings.as_mut() {
                    settings.mem0.configured = matches!(reply, Reply::Mem0KeySaved);
                }
                self.memory_settings_error = None;
            }
            Reply::ChatPlan { plan, auto_approve } => {
                self.plan = plan;
                self.auto_approve = auto_approve;
            }
            Reply::AutoApprove(enabled) => self.auto_approve = enabled,
            Reply::Thinking(level) => {
                if let Some(catalog) = self.catalog.as_mut() {
                    catalog.thinking.level = level;
                }
            }
            Reply::Transcribed(transcription) => {
                self.transcript = Some(transcription.text);
                self.transcript_error = None;
            }
            // Bytes are intercepted by the session before they reach state;
            // this arm only keeps the match exhaustive.
            Reply::Artifact { .. } => {}
            // The session keeps uploaded attachments on the composer draft.
            Reply::Attached(_) => {}
            // The stop result is shown as a toast by the session.
            Reply::Stopping(_) => {}
            Reply::OAuthSignedIn { id, device_code } => {
                self.oauth_result = Some((id, device_code));
            }
            Reply::ConnectorTested(result) => {
                self.connector_tests.insert(result.id.clone(), result);
            }
            // The session applies the optimistic cache update and toast for
            // a saved bot config; the state only keeps the match exhaustive.
            Reply::BotConfigSaved { .. } => {}
            // Applied by the session (it owns the editable draft).
            Reply::Voice(settings) => {
                self.voice = Some(settings);
            }
            Reply::AdminSaved
            | Reply::BotSettingsSaved
            | Reply::AdminRestarted
            | Reply::ProviderKeySaved
            | Reply::DefaultRouteSaved => {}
            Reply::ProviderTested { id, result } => {
                self.provider_tests.insert(id, result);
            }
            Reply::AvatarImage {
                target,
                version,
                data_url,
            } => {
                self.avatars
                    .insert(target.key(), AvatarImage { version, data_url });
            }
            // The session reacts to these (it owns the drafts they came from).
            Reply::AvatarSaved(_) | Reply::ToolsSaved | Reply::CardWritten => {}
            Reply::CreatedGroup(group) => self.created_group = Some(group),
            Reply::Onboarding(onboarding) => self.onboarding = Some(onboarding),
        }
        true
    }

    pub fn fail(&mut self, scope: &Scope, kind: OperationKind, error: ApiError) -> bool {
        if !self.scope.accepts(scope) {
            return false;
        }
        // Both belong to the sheet that asked: its error stays in the sheet.
        if matches!(kind, OperationKind::BotCatalog | OperationKind::BotInstall) {
            self.shipped_installing = None;
            self.shipped_error = Some(error.to_string());
            return true;
        }
        let mutation = matches!(
            kind,
            OperationKind::Submit
                | OperationKind::Apply
                | OperationKind::Decide
                | OperationKind::CreateBot
                | OperationKind::CreateConversation
                | OperationKind::CreateSkill
                | OperationKind::CreateBotChat
                | OperationKind::SetBotRefresh
                | OperationKind::RenameConversation
                | OperationKind::ArchiveConversation
                | OperationKind::UnarchiveConversation
                | OperationKind::AddMemory
                | OperationKind::TeamWrite
                | OperationKind::BotConfigWrite
                | OperationKind::CreateGroup
                | OperationKind::OnboardingWrite
                | OperationKind::ToolsWrite
                | OperationKind::CardWrite
        );
        if mutation && error.unknown_outcome() {
            self.unknown_outcome = true;
            let operation = match kind {
                OperationKind::Submit => "submission",
                OperationKind::Decide => "approval decision",
                OperationKind::CreateBot => "bot scaffold",
                OperationKind::CreateConversation => "conversation create",
                OperationKind::CreateSkill => "skill create",
                OperationKind::CreateBotChat => "new chat",
                OperationKind::SetBotRefresh => "memory refresh change",
                OperationKind::RenameConversation => "rename",
                OperationKind::ArchiveConversation => "archive",
                OperationKind::UnarchiveConversation => "unarchive",
                OperationKind::AddMemory => "memory write",
                OperationKind::TeamWrite => "team update",
                OperationKind::BotConfigWrite => "bot config update",
                OperationKind::CreateGroup => "group create",
                OperationKind::OnboardingWrite => "interview step",
                OperationKind::ToolsWrite => "capability change",
                OperationKind::CardWrite => "card reply",
                _ => "model change",
            };
            self.error = Some(format!(
                "Unknown {operation} outcome. It may have reached the server. Do not resend. Use Refresh / retry reads to inspect history and the saved selection. {error}"
            ));
        } else if kind == OperationKind::Apply && error == ApiError::Http(422) {
            self.error = Some(
                "The server rejected that model selection. Refresh, reopen Model routing, and pick a listed provider and model. If the list is empty, ask the admin to allow more models."
                    .into(),
            );
        } else {
            self.error = Some(error.to_string());
        }
        true
    }
}

/// The label a shipped bot wears before the owner has it: its title, or the
/// humanized id when the template has none.
pub fn shipped_label(bot: &ShippedBot) -> String {
    bot.title
        .as_deref()
        .map(str::trim)
        .filter(|title| !title.is_empty())
        .map(str::to_owned)
        .unwrap_or_else(|| crate::session::humanize_label(&bot.id))
}

/// "Meet your team" order: the bots the owner does not have yet first, then
/// by label, so the ones to add are at the top.
pub fn meet_order(bots: &[ShippedBot]) -> Vec<&ShippedBot> {
    let mut ordered: Vec<&ShippedBot> = bots.iter().collect();
    ordered.sort_by_cached_key(|bot| {
        (
            bot.installed,
            shipped_label(bot).to_lowercase(),
            bot.id.clone(),
        )
    });
    ordered
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        BotSummary, Catalog, ConnectorStatus, ConversationPage, ConversationSummary, CreatedBot,
        CreatedConversation, CreatedSkill, Health, History, MemoryEntry, Message, ProviderStatus,
        Role, SecretStatus, Selection, SkillSummary, Task, TaskStatus, Team, TeamMember, TeamMode,
    };

    fn catalog() -> Catalog {
        serde_json::from_str(r#"{"providers":[{"id":"approved","models":["small","large"],"data_egress_approved":true}],"selection":{"provider":"approved","model":"small"}}"#).unwrap()
    }

    #[test]
    fn thinking_reply_updates_the_catalog_level() {
        let mut state = State::default();
        state.new_scope("first".into(), false);
        let scope = state.scope.clone();
        state.receive(&scope, Reply::Catalog(catalog()));
        state.receive(&scope, Reply::Thinking(Some("high".into())));
        assert_eq!(
            state.catalog.as_ref().unwrap().thinking.level.as_deref(),
            Some("high")
        );
        state.receive(&scope, Reply::Thinking(None));
        assert!(state.catalog.as_ref().unwrap().thinking.level.is_none());
    }

    #[test]
    fn disconnect_and_conversation_change_ignore_late_success_and_errors() {
        let mut state = State::default();
        state.new_scope("first".into(), false);
        let first = state.scope.clone();
        state.receive(
            &first,
            Reply::Health(Health {
                ok: true,
                version: "test".into(),
                ..Health::default()
            }),
        );
        state.receive(&first, Reply::Catalog(catalog()));
        assert!(state.connected);
        state.new_scope("second".into(), true);
        assert!(!state.receive(&first, Reply::Catalog(catalog())));
        assert!(state.catalog.is_none());
        assert!(!state.fail(&first, OperationKind::Submit, ApiError::Transport));
        assert!(state.error.is_none());
        let second = state.scope.clone();
        state.disconnect();
        assert!(!state.receive(
            &second,
            Reply::Health(Health {
                ok: true,
                version: "late".into(),
                ..Health::default()
            })
        ));
        assert!(!state.connected);
        assert!(state.messages.is_empty());
    }

    #[test]
    fn selecting_a_draft_never_silently_changes_the_active_selection() {
        let mut state = State::default();
        let scope = state.scope.clone();
        state.receive(&scope, Reply::Catalog(catalog()));
        state.draft.model = "large".into();
        assert_eq!(state.catalog.as_ref().unwrap().selection.model, "small");
        assert!(!state.can_send(), "cannot send with unapplied changes");
        state.receive(
            &scope,
            Reply::Selection(Selection {
                provider: "approved".into(),
                model: "large".into(),
            }),
        );
        assert_eq!(state.catalog.as_ref().unwrap().selection.model, "large");
    }

    #[test]
    fn unknown_submit_blocks_sending_and_never_schedules_resubmission() {
        let mut state = State {
            connected: true,
            ..State::default()
        };
        let scope = state.scope.clone();
        state.receive(&scope, Reply::Catalog(catalog()));
        state.history_loaded = true;
        assert!(state.can_send());
        state.fail(&scope, OperationKind::Submit, ApiError::Transport);
        assert!(state.unknown_outcome);
        assert!(!state.can_send());
        assert!(
            state
                .error
                .as_ref()
                .unwrap()
                .contains("Unknown submission outcome")
        );
    }

    #[test]
    fn merge_transcript_keeps_stable_rows_and_pairs_optimistic_user() {
        let opt = Message {
            id: "opt-99".into(),
            role: Role::User,
            content: "hello".into(),
            created_at: "Just now".into(),
            name: None,
            bot: None,
            attachments: Vec::new(),
            limit_reached: false,
        };
        let bot = Message {
            id: "m-bot".into(),
            role: Role::Assistant,
            content: "hi there".into(),
            created_at: "2026-11-09T12:00:00Z".into(),
            name: None,
            bot: Some("main".into()),
            attachments: Vec::new(),
            limit_reached: false,
        };
        let current = vec![opt.clone(), bot.clone()];
        let incoming = vec![
            Message {
                id: "m-user".into(),
                role: Role::User,
                content: "hello".into(),
                created_at: "2026-11-09T12:00:01Z".into(),
                name: None,
                bot: None,
                attachments: Vec::new(),
                limit_reached: false,
            },
            Message {
                id: "m-bot".into(),
                role: Role::Assistant,
                content: "hi there, friend".into(),
                created_at: "2026-11-09T12:00:02Z".into(),
                name: None,
                bot: Some("main".into()),
                attachments: Vec::new(),
                limit_reached: false,
            },
        ];
        let merged = merge_transcript(&current, incoming);
        assert_eq!(merged.len(), 2);
        assert_eq!(merged[0].id, "m-user");
        assert_eq!(merged[0].content, "hello");
        assert_eq!(merged[1].id, "m-bot");
        assert_eq!(merged[1].content, "hi there, friend");
        let again = merge_transcript(&merged, merged.clone());
        assert_eq!(again[1].content, merged[1].content);
        assert_eq!(again[1].id, merged[1].id);
    }

    #[test]
    fn merge_transcript_keeps_longer_assistant_on_stale_history() {
        let live = Message {
            id: "m-bot".into(),
            role: Role::Assistant,
            content: "Hello, this is still growing on screen".into(),
            created_at: "2026-11-09T12:00:02Z".into(),
            name: None,
            bot: Some("main".into()),
            attachments: Vec::new(),
            limit_reached: false,
        };
        let current = vec![live.clone()];
        let mut stale = live.clone();
        stale.content = "Hello".into();
        let merged = merge_transcript(&current, vec![stale]);
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].content, live.content);
        let mut done = live.clone();
        done.content = "Hello, this is still growing on screen, done".into();
        let fresher = merge_transcript(&merged, vec![done]);
        assert_eq!(
            fresher[0].content,
            "Hello, this is still growing on screen, done"
        );
    }

    #[test]
    fn merge_transcript_counts_unicode_for_assistant_regression() {
        let live = Message {
            id: "m-bot".into(),
            role: Role::Assistant,
            content: "café ☕ and more".into(),
            created_at: "2026-11-09T12:00:02Z".into(),
            name: None,
            bot: Some("main".into()),
            attachments: Vec::new(),
            limit_reached: false,
        };
        let current = vec![live.clone()];
        let mut stale = live.clone();
        stale.content = "café".into();
        let merged = merge_transcript(&current, vec![stale]);
        assert_eq!(merged[0].content, live.content);
    }

    #[test]
    fn merge_transcript_shorter_user_row_takes_incoming() {
        let live = Message {
            id: "m-user".into(),
            role: Role::User,
            content: "draft with typos fixed later".into(),
            created_at: "Just now".into(),
            name: None,
            bot: None,
            attachments: Vec::new(),
            limit_reached: false,
        };
        let mut shorter = live.clone();
        shorter.content = "draft".into();
        let merged = merge_transcript(std::slice::from_ref(&live), vec![shorter.clone()]);
        assert_eq!(merged[0].content, shorter.content);
    }

    #[test]
    fn merge_transcript_keeps_limit_reached_when_preserving_longer_assistant() {
        let live = Message {
            id: "m-bot".into(),
            role: Role::Assistant,
            content: "long streaming answer".into(),
            created_at: "2026-11-09T12:00:02Z".into(),
            name: None,
            bot: Some("main".into()),
            attachments: Vec::new(),
            limit_reached: false,
        };
        let current = vec![live.clone()];
        let mut stale = live.clone();
        stale.content = "long".into();
        stale.limit_reached = true;
        let merged = merge_transcript(&current, vec![stale]);
        assert_eq!(merged[0].content, live.content);
        assert!(merged[0].limit_reached);
    }

    #[test]
    fn merge_transcript_keeps_in_flight_opt_not_yet_on_server() {
        let opt = Message {
            id: "opt-1".into(),
            role: Role::User,
            content: "wait".into(),
            created_at: "Just now".into(),
            name: None,
            bot: None,
            attachments: Vec::new(),
            limit_reached: false,
        };
        let merged = merge_transcript(std::slice::from_ref(&opt), Vec::new());
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].id, "opt-1");
    }

    #[test]
    fn disconnect_clears_visible_sensitive_state() {
        let mut state = State::default();
        state.messages.push(Message {
            id: "m".into(),
            role: Role::Assistant,
            content: "sensitive fixture".into(),
            created_at: "now".into(),
            name: Some("analyst".into()),
            bot: Some("analyst".into()),
            attachments: Vec::new(),
            limit_reached: false,
        });
        state.task = Some(Task {
            id: "t".into(),
            kind: "chat".into(),
            bot: "owner".into(),
            status: TaskStatus::AwaitingApproval,
        });
        state.error = Some("old error".into());
        state.disconnect();
        assert!(state.messages.is_empty());
        assert!(state.task.is_none());
        assert!(state.error.is_none());
    }

    #[test]
    fn unknown_scaffold_is_not_a_clean_rejection_and_disconnect_clears_fleet_results() {
        let mut state = State {
            connected: true,
            ..State::default()
        };
        let scope = state.scope.clone();
        state.fail(&scope, OperationKind::CreateBot, ApiError::Transport);
        assert!(state.unknown_outcome);
        assert!(
            state
                .error
                .as_deref()
                .unwrap()
                .contains("Unknown bot scaffold outcome")
        );
        assert!(state.error.as_deref().unwrap().contains("Do not resend"));
        let mut clean = State {
            connected: true,
            ..State::default()
        };
        let clean_scope = clean.scope.clone();
        clean.fail(&clean_scope, OperationKind::CreateBot, ApiError::Http(409));
        assert!(!clean.unknown_outcome);
        clean.fail(
            &clean_scope,
            OperationKind::CreateConversation,
            ApiError::Http(422),
        );
        assert!(!clean.unknown_outcome);
        clean.fail(
            &clean_scope,
            OperationKind::CreateSkill,
            ApiError::Http(409),
        );
        assert!(!clean.unknown_outcome);
        clean.fail(&clean_scope, OperationKind::ListSkills, ApiError::Transport);
        assert!(!clean.unknown_outcome);

        state.fail(&scope, OperationKind::CreateSkill, ApiError::Transport);
        assert!(
            state
                .error
                .as_deref()
                .unwrap()
                .contains("Unknown skill create outcome")
        );

        state.created_bot = Some(CreatedBot {
            id: "notes".into(),
            enabled: false,
            role: "specialist".into(),
            live: false,
            tools: vec!["skill_list".into()],
            skills: Vec::new(),
            require_approval: Vec::new(),
            bot_file: "bots/notes.toml".into(),
            prompt_file: "prompts/notes.txt".into(),
        });
        state.created_conversation = Some(CreatedConversation {
            id: "sec:finance".into(),
            kind: "section".into(),
            parent_id: None,
            accepts_turns: false,
        });
        state.created_skill = Some(CreatedSkill {
            id: "notes-pack".into(),
            name: "Notes Pack".into(),
            description: "Short summary".into(),
            file: "skills/notes-pack/SKILL.md".into(),
            live: false,
        });
        state.listed_skills.push(SkillSummary {
            id: "ask".into(),
            name: "Ask".into(),
            description: "Synthetic summary".into(),
            pack: None,
        });
        state.skills_loaded = true;
        state.listed_bots.push(BotSummary {
            id: "analyst".into(),
            role: "specialist".into(),
            enabled: true,
            live: true,
            model: None,
            display_name: Some("Analyst".into()),
            title: None,
            description: None,
            avatar: Some("A".into()),
            avatar_version: None,
            tools: vec!["skill_list".into()],
            skills: vec!["ask".into()],
            require_approval: Vec::new(),
        });
        state.bots_loaded = true;
        state.new_scope("dir:analyst".into(), true);
        assert!(state.created_bot.is_some());
        assert!(state.created_conversation.is_some());
        assert!(state.created_skill.is_some());
        assert!(state.skills_loaded);
        assert!(state.bots_loaded);
        state.disconnect();
        assert!(state.created_bot.is_none());
        assert!(state.created_conversation.is_none());
        assert!(state.created_skill.is_none());
        assert!(state.listed_skills.is_empty());
        assert!(!state.skills_loaded);
        assert!(state.listed_bots.is_empty());
        assert!(!state.bots_loaded);
    }

    fn summary(id: &str, kind: &str, title: &str) -> ConversationSummary {
        ConversationSummary {
            id: id.into(),
            kind: kind.into(),
            parent_id: None,
            title: Some(title.into()),
            created_at: "2026-10-29T10:00:00.000Z".into(),
            updated_at: "2026-10-29T10:05:00.000Z".into(),
            last_message_at: None,
            archived_at: None,
            unread_count: 0,
            accepts_turns: matches!(kind, "direct" | "room"),
            last_message: None,
            avatar_version: None,
        }
    }

    #[test]
    fn opening_a_thread_clears_only_that_threads_unread_badge() {
        let mut state = State::default();
        let mut opened = summary("dir:analyst", "direct", "Analyst");
        opened.unread_count = 3;
        let mut other = summary("room:review", "room", "Review");
        other.unread_count = 2;
        state.receive(
            &state.scope.clone(),
            Reply::Conversations(ConversationPage {
                conversations: vec![opened, other],
                next_cursor: None,
            }),
        );
        state.new_scope("dir:analyst".into(), true);
        let scope = state.scope.clone();
        state.receive(
            &scope,
            Reply::History(History {
                messages: Vec::new(),
                cards: Vec::new(),
            }),
        );
        let opened = state
            .conversations
            .iter()
            .find(|conversation| conversation.id == "dir:analyst")
            .unwrap();
        assert_eq!(opened.unread_count, 0);
        let other = state
            .conversations
            .iter()
            .find(|conversation| conversation.id == "room:review")
            .unwrap();
        assert_eq!(other.unread_count, 2);
    }

    #[test]
    fn archived_keeps_only_archived_rows_and_unarchive_drops_the_row() {
        let mut state = State::default();
        let scope = state.scope.clone();
        let mut archived = summary("dir:analyst", "direct", "Analyst");
        archived.archived_at = Some("2026-10-29T11:00:00.000Z".into());
        state.receive(
            &scope,
            Reply::ArchivedConversations(ConversationPage {
                conversations: vec![archived, summary("room:review", "room", "Review")],
                next_cursor: None,
            }),
        );
        assert!(state.archived_loaded);
        assert_eq!(state.archived.len(), 1);
        assert_eq!(state.archived[0].id, "dir:analyst");
        state.receive(&scope, Reply::UnarchivedConversation("dir:analyst".into()));
        assert!(state.archived.is_empty());
        assert!(state.fail(
            &scope,
            OperationKind::UnarchiveConversation,
            ApiError::Transport
        ));
        assert!(state.unknown_outcome);
        assert!(
            state
                .error
                .as_deref()
                .unwrap()
                .contains("Unknown unarchive outcome")
        );
    }

    fn bot_chat(id: &str, primary: bool) -> crate::bot_chats::BotChat {
        crate::bot_chats::BotChat {
            id: id.to_owned(),
            bot: "analyst".into(),
            title: None,
            primary,
            archived: false,
            carried_from: None,
            created_at: "2026-11-01T00:00:00Z".into(),
            updated_at: "2026-11-01T00:00:00Z".into(),
            last_message_at: None,
            turns: 0,
            has_summary: false,
        }
    }

    fn plugin(slug: &str, installed: bool) -> crate::marketplace::PluginSummary {
        crate::marketplace::PluginSummary {
            slug: slug.into(),
            name: slug.to_uppercase(),
            tagline: String::new(),
            category: "research".into(),
            version: "1.0.0".into(),
            installed,
            enabled: false,
            status: String::new(),
        }
    }

    #[test]
    fn marketplace_replies_fill_the_catalog_and_keep_it_across_switches() {
        let mut state = State::default();
        let scope = state.scope.clone();
        assert!(!state.marketplace_loaded);
        state.receive(
            &scope,
            Reply::MarketplacePlugins(vec![plugin("search", false), plugin("mail", false)]),
        );
        assert!(state.marketplace_loaded);
        assert_eq!(state.marketplace.len(), 2);

        // A change returns the refreshed list; an uninstall keeps the old page
        // until the server's detail arrives.
        state.receive(
            &scope,
            Reply::MarketplaceChanged {
                slug: "search".into(),
                plugins: vec![plugin("search", true), plugin("mail", false)],
                detail: None,
            },
        );
        assert!(state.marketplace[0].installed);
        assert!(state.marketplace_detail.is_none());

        // A failed read earlier is cleared by a good one.
        state.marketplace_error = Some("old".into());
        state.receive(
            &scope,
            Reply::MarketplacePlugins(vec![plugin("search", true)]),
        );
        assert!(state.marketplace_error.is_none());

        // The catalog belongs to the owner, not the open chat.
        state.new_scope("dir:legal".into(), true);
        assert_eq!(state.marketplace.len(), 1);
        assert!(state.marketplace_loaded);
    }

    #[test]
    fn bot_chat_replies_fill_the_list_and_survive_conversation_switches() {
        let mut state = State::default();
        let scope = state.scope.clone();
        state.receive(
            &scope,
            Reply::BotChats(crate::bot_chats::BotChatList {
                bot: "analyst".into(),
                refresh_every_turns: 40,
                chats: vec![bot_chat("dir:analyst", true)],
            }),
        );
        assert!(state.bot_chats_loaded.contains("analyst"));
        assert_eq!(state.bot_chats["analyst"].chats.len(), 1);

        // A new chat is added to the list and remembered for the session to open.
        state.receive(
            &scope,
            Reply::BotChatCreated {
                bot: "analyst".into(),
                created: crate::bot_chats::CreatedBotChat {
                    chat: bot_chat("dir:chat-0a1b2c3d4e5f", false),
                    carried_from: Some("dir:analyst".into()),
                },
            },
        );
        assert_eq!(state.bot_chats["analyst"].chats.len(), 2);
        assert_eq!(
            state.created_bot_chat,
            Some((
                "analyst".into(),
                "dir:chat-0a1b2c3d4e5f".into(),
                Some("dir:analyst".into())
            ))
        );

        state.receive(
            &scope,
            Reply::BotRefreshSet(crate::bot_chats::BotChatSettings {
                bot: "analyst".into(),
                refresh_every_turns: 25,
            }),
        );
        assert_eq!(state.bot_chats["analyst"].refresh_every_turns, 25);

        // Switching conversation keeps the lists: they belong to the owner.
        state.new_scope("dir:legal".into(), true);
        assert_eq!(state.bot_chats["analyst"].chats.len(), 2);
        assert!(state.bot_chats_loaded.contains("analyst"));
    }

    #[test]
    fn directory_and_memory_replies_update_and_survive_conversation_switches() {
        let mut state = State::default();
        let scope = state.scope.clone();
        state.receive(
            &scope,
            Reply::Conversations(ConversationPage {
                conversations: vec![
                    summary("room:review", "room", "Review room"),
                    summary("sec:finance", "section", "Finance"),
                ],
                next_cursor: Some("2026-10-29T10:05:00.000Z|sec:finance".into()),
            }),
        );
        assert!(state.conversations_loaded);
        let mut renamed = summary("room:review", "room", "Operations review");
        renamed.title = Some("Operations review".into());
        state.receive(&scope, Reply::RenamedConversation(renamed));
        assert_eq!(
            state.conversations[0].title.as_deref(),
            Some("Operations review")
        );
        state.receive(&scope, Reply::ArchivedConversation("sec:finance".into()));
        assert_eq!(state.conversations.len(), 1);

        let memory = MemoryEntry {
            id: "mem_1".into(),
            kind: "fact".into(),
            content: "Synthetic fact.".into(),
            source: "operator:desktop".into(),
            created_at: "2026-10-29T10:00:00.000Z".into(),
            updated_at: "2026-10-29T10:00:00.000Z".into(),
            origin: "explicit".into(),
            status: "active".into(),
            bot: None,
            importance: 0.5,
            last_recalled_at: None,
            recall_count: 0,
        };
        state.receive(&scope, Reply::AddedMemory(memory.clone()));
        state.receive(&scope, Reply::AddedMemory(memory.clone()));
        assert_eq!(state.memories.len(), 1);
        assert!(state.memories_loaded);
        state.receive(&scope, Reply::DeletedMemory(memory.id));
        assert!(state.memories.is_empty());

        state.receive(
            &scope,
            Reply::Providers(vec![ProviderStatus {
                id: "approved".into(),
                kind: "openai".into(),
                models: vec!["small".into()],
                routing_classes: Vec::new(),
                data_egress_approved: true,
                catalog_fetch: false,
                auth: "api_key".into(),
                catalog_verified: false,
                managed: false,
                status: "ok".into(),
                model_source: "live".into(),
                model_count: 1,
                credential: crate::api::CredentialState {
                    configured: true,
                    own: true,
                    granted: false,
                },
                is_default: false,
                default_model: None,
                secrets: std::collections::BTreeMap::from([(
                    "api_key".into(),
                    SecretStatus {
                        name: Some("OPENAI_API_KEY".into()),
                        configured: true,
                    },
                )]),
            }]),
        );
        state.receive(
            &scope,
            Reply::Connectors(vec![ConnectorStatus {
                id: "notion".into(),
                kind: "oauth".into(),
                enabled: false,
                redirect_uri: None,
                api_base: None,
                default_inbox_id: None,
                secrets: std::collections::BTreeMap::new(),
                oauth: None,
                connected: None,
                personal: None,
                key_kind: None,
            }]),
        );
        assert!(state.providers_loaded);
        assert!(state.connectors_loaded);

        state.new_scope("dir:analyst".into(), true);
        assert_eq!(state.conversations.len(), 1, "directory survives a switch");
        assert!(state.providers_loaded);
        state.disconnect();
        assert!(state.conversations.is_empty());
        assert!(state.memories.is_empty());
        assert!(state.providers.is_empty());
        assert!(!state.providers_loaded);
        assert!(state.connectors.is_empty());
    }

    #[test]
    fn plan_and_auto_approve_never_cross_conversations() {
        let mut state = State::default();
        let scope = state.scope.clone();
        assert!(!state.auto_approve, "auto-approve starts off");
        let plan = crate::chat_plan::ChatPlan::from_value(&serde_json::json!({
            "goal": "Prepare the summary",
            "items": [{"text": "Collect figures", "status": "done"}]
        }));
        state.receive(
            &scope,
            Reply::ChatPlan {
                plan,
                auto_approve: true,
            },
        );
        assert!(state.auto_approve);
        assert_eq!(state.plan.as_ref().unwrap().progress(), "1 of 1 done");

        // A late reply for another conversation never lands.
        let other = Scope {
            epoch: scope.epoch + 9,
            conversation: "room:other".into(),
        };
        assert!(!state.receive(&other, Reply::AutoApprove(false)));
        assert!(state.auto_approve);

        // Switching conversation clears both: the switch is re-read from the
        // server for the new chat and never carried over.
        state.new_scope("room:other".into(), true);
        assert!(state.plan.is_none());
        assert!(!state.auto_approve);
        state.disconnect();
        assert!(state.plan.is_none() && !state.auto_approve);
    }

    #[test]
    fn the_team_is_loaded_and_cleared_with_the_conversation() {
        let mut state = State::default();
        let scope = state.scope.clone();
        let team = Team {
            conversation_id: "room:review".into(),
            mode: TeamMode::Selected,
            members: vec![TeamMember {
                bot: "analyst".into(),
                role: "specialist".into(),
                enabled: true,
                live: true,
            }],
            rules: String::new(),
        };
        state.receive(&scope, Reply::Team(team.clone()));
        assert!(state.team_loaded);
        assert_eq!(state.team.as_ref(), Some(&team));
        assert!(state.team_error.is_none());

        // A late team from another conversation never lands.
        let other = Scope {
            epoch: scope.epoch + 9,
            conversation: "room:other".into(),
        };
        assert!(!state.receive(&other, Reply::Team(team.clone())));

        state.new_scope("room:other".into(), true);
        assert!(
            state.team.is_none(),
            "the team does not cross conversations"
        );
        assert!(!state.team_loaded);
        assert!(state.team_error.is_none());
        state.disconnect();
        assert!(state.team.is_none());
    }

    #[test]
    fn an_unknown_team_write_blocks_further_sends() {
        let mut state = State {
            connected: true,
            ..State::default()
        };
        let scope = state.scope.clone();
        state.receive(&scope, Reply::Catalog(catalog()));
        state.history_loaded = true;
        assert!(state.can_send());
        state.fail(&scope, OperationKind::TeamWrite, ApiError::Transport);
        assert!(state.unknown_outcome);
        assert!(!state.can_send());
        assert!(
            state
                .error
                .as_ref()
                .unwrap()
                .contains("Unknown team update outcome")
        );
    }

    fn shipped(id: &str, title: Option<&str>, installed: bool) -> ShippedBot {
        ShippedBot {
            id: id.into(),
            title: title.map(str::to_owned),
            description: None,
            role: "specialist".into(),
            installed,
            avatar_version: None,
        }
    }

    #[test]
    fn meet_order_puts_bots_to_add_first_then_by_title() {
        let bots = vec![
            shipped("zeta", Some("Analyst"), false),
            shipped("alpha", Some("Writer"), true),
            shipped("beta", Some("builder"), false),
            shipped("gamma", None, false),
        ];
        let order: Vec<&str> = meet_order(&bots)
            .iter()
            .map(|bot| bot.id.as_str())
            .collect();
        // Not installed first; labels compare case-insensitively ("analyst",
        // "builder", "gamma"); the id is the tie-break, so the order never
        // depends on the server's list order.
        assert_eq!(order, ["zeta", "beta", "gamma", "alpha"]);
    }

    #[test]
    fn shipped_label_prefers_the_title_and_falls_back_to_the_id() {
        assert_eq!(
            shipped_label(&shipped("marketing-lead", Some("  Marketing lead "), false)),
            "Marketing lead"
        );
        assert_eq!(
            shipped_label(&shipped("marketing-lead", Some("  "), false)),
            "Marketing Lead"
        );
    }

    #[test]
    fn installing_a_shipped_bot_flips_its_flag_and_failures_stay_in_the_sheet() {
        let mut state = State::default();
        let scope = state.scope.clone();
        state.receive(
            &scope,
            Reply::ShippedBots(vec![shipped("analyst", Some("Analyst"), false)]),
        );
        assert!(state.shipped_loaded);
        state.shipped_installing = Some("analyst".into());
        state.receive(&scope, Reply::ShippedBotInstalled("analyst".into()));
        assert!(state.shipped_bots[0].installed);
        assert!(state.shipped_installing.is_none());

        state.shipped_installing = Some("writer".into());
        state.fail(&scope, OperationKind::BotInstall, ApiError::Http(409));
        assert!(state.shipped_installing.is_none());
        assert!(state.shipped_error.as_ref().unwrap().contains("409"));
        assert!(state.error.is_none(), "the global banner stays clear");
        assert!(!state.unknown_outcome);
    }
}
