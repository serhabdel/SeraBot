use crate::{
    ApiClient, ApiError, Approval, ApprovalDecision, AudioUpdate, AuthUser, BotConfigUpdate,
    BotDetail, BotSettingsUpdate, BotSummary, Catalog, ConnectorStatus, ConnectorTestResult,
    ConnectorUpdate, ConversationKind, ConversationPage, ConversationSummary, CreatedBot,
    CreatedConversation, CreatedGroup, CreatedSkill, Health, History, LibraryEntry, MemoryEntry,
    MemoryStatsView, NewGroup, NewProvider, Onboarding, ProviderStatus, Scope, Selection,
    ServerHealth, ShippedBot, SkillSummary, Task, Team, TeamMode, Transcription, UsageReport,
    VoiceSettings, VoiceUpdate, avatars::AvatarTarget,
};
use secrecy::SecretString;
use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
    mpsc::{self, Receiver, SyncSender, TrySendError},
};
use url::Url;
use zeroize::Zeroizing;

pub struct Credentials {
    pub endpoint: Url,
    pub token: SecretString,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OperationKind {
    Health,
    Me,
    Catalog,
    History,
    Apply,
    Submit,
    Task,
    Approvals,
    StandingGrants,
    RevokeStandingGrant,
    JobsReliability,
    Decide,
    CreateBot,
    CreateConversation,
    CreateSkill,
    ListSkills,
    ListBots,
    ListConversations,
    ListArchived,
    MarketplaceList,
    MarketplaceDetail,
    MarketplaceWrite,
    ListBotChats,
    CreateBotChat,
    SetBotRefresh,
    RenameConversation,
    ArchiveConversation,
    UnarchiveConversation,
    ListMemories,
    AddMemory,
    UpdateMemory,
    SetMemoryStatus,
    DeleteMemory,
    MemoryStats,
    Providers,
    Connectors,
    Team,
    TeamWrite,
    Library,
    ChatPlan,
    ChatAutoApprove,
    /// Set the open conversation's thinking level.
    Thinking,
    /// Stop the running turn(s) of the open conversation.
    StopTurn,
    FetchArtifact,
    /// A file's bytes, read back as a preview for an inline player.
    PreviewArtifact,
    /// The open conversation's background media jobs.
    MediaJobs,
    /// The owner's per-turn work limits.
    TurnLimits,
    /// Save or reset the owner's per-turn work limits.
    TurnLimitsWrite,
    /// Mem0 console settings (read or write the per-user key).
    MemorySettingsWrite,
    Transcribe,
    AdminWrite,
    /// Save the account's own key for a provider.
    ProviderKey,
    /// Ask a provider for its live model list.
    ProviderTest,
    /// Choose the account's default provider and model.
    DefaultRoute,
    ConnectorTest,
    OAuthSignIn,
    ConnectorSignOut,
    BotSettingsWrite,
    BotLifecycle,
    AdminRestart,
    BotDetail,
    BotConfigWrite,
    Usage,
    /// Owner-only server health card (GET /v1/admin/status).
    ServerHealth,
    /// `GET /v1/bots/catalog`: the shipped templates on "Meet your team".
    BotCatalog,
    /// `POST /v1/bots/catalog/{name}/install`: "Add to my team".
    BotInstall,
    Avatar,
    CreateGroup,
    OnboardingRead,
    OnboardingWrite,
    ToolsWrite,
    /// Upload of one chat attachment from the composer.
    Attach,
    /// Answer, decline or secret reply on a chat card.
    CardWrite,
    /// Tasks tab (0.25.0): list, write, prefs and sync.
    ListTasks,
    AddTask,
    SetTaskStatus,
    DeleteTask,
    TaskPrefs,
    SetTaskPrefs,
    TaskSyncStatus,
    RunTaskSync,
    TaskCalendar,
    /// The owner-only log ring buffer (0.28.0), for the Logs tab.
    Logs,
    /// Wave A visibility bundle for the Workflows tab (0.29.0).
    Workflows,
    /// Schedule write or manual watch run on the Workflows tab (0.30.0).
    WorkflowsWrite,
    /// A server-rendered artifact preview (0.28.0), for the preview pane.
    DocPreview,
}

impl OperationKind {
    /// Repeating background reads. A 429 on these backs off quietly instead
    /// of raising the error banner.
    pub fn is_background_poll(self) -> bool {
        matches!(self, Self::History | Self::Task | Self::Approvals)
    }
}

pub enum Operation {
    Health,
    /// `GET /v1/me`: who the session token belongs to.
    Me,
    Catalog,
    History,
    Apply(Selection),
    Submit(Zeroizing<String>),
    /// A message with uploaded attachment ids (text may be empty).
    SubmitWith {
        text: Zeroizing<String>,
        attachments: Vec<String>,
    },
    /// Upload one file to the open conversation's attachment area.
    UploadAttachment {
        filename: String,
        bytes: Vec<u8>,
    },
    Task(String),
    Approvals,
    StandingGrants,
    RevokeStandingGrant {
        id: String,
    },
    JobsReliability,
    Decide {
        id: String,
        decision: ApprovalDecision,
    },
    CreateBot {
        id: String,
        description: Zeroizing<String>,
        tools: Vec<String>,
        skills: Vec<String>,
    },
    CreateConversation {
        kind: ConversationKind,
        slug: String,
        parent_id: Option<String>,
        title: Option<String>,
    },
    CreateSkill {
        id: String,
        description: Zeroizing<String>,
        body: Zeroizing<String>,
    },
    ListSkills,
    ListBots,
    ListConversations {
        include_archived: bool,
    },
    ListArchived,
    MarketplaceList,
    MarketplaceDetail {
        slug: String,
    },
    MarketplaceAct {
        slug: String,
        action: crate::marketplace::PluginAction,
    },
    /// The typed values are sent once and dropped; `Zeroizing` wipes them.
    MarketplaceConfig {
        slug: String,
        values: Vec<(String, Zeroizing<String>)>,
    },
    ListBotChats {
        bot: String,
    },
    CreateBotChat {
        bot: String,
        title: Option<String>,
    },
    SetBotRefresh {
        bot: String,
        every: u32,
    },
    RenameConversation {
        id: String,
        title: Zeroizing<String>,
    },
    ArchiveConversation {
        id: String,
    },
    UnarchiveConversation {
        id: String,
    },
    ListMemories {
        query: Option<String>,
        origin: String,
        status: String,
    },
    AddMemory {
        kind: String,
        content: Zeroizing<String>,
        source: String,
    },
    UpdateMemory {
        id: String,
        content: Zeroizing<String>,
    },
    SetMemoryStatus {
        id: String,
        status: String,
    },
    DeleteMemory {
        id: String,
    },
    MemoryStats,
    ListTasks,
    AddTask {
        title: String,
        due: Option<String>,
    },
    SetTaskStatus {
        id: String,
        status: String,
    },
    DeleteTask {
        id: String,
    },
    TaskPrefs,
    SetTaskPrefs {
        provider: String,
    },
    TaskSyncStatus,
    RunTaskSync,
    TaskCalendar {
        from: String,
        to: String,
    },
    Providers,
    Connectors,
    GetTeam,
    SetTeam {
        members: Vec<String>,
        mode: TeamMode,
        rules: String,
    },
    Library,
    /// Goal, todo list and auto-approve switch of the open conversation.
    ChatPlan,
    SetAutoApprove {
        enabled: bool,
    },
    /// Set (or with None, clear) the open conversation's thinking level.
    SetThinking {
        level: Option<String>,
    },
    /// Ask the open conversation's running turns to stop.
    StopTurn,
    FetchArtifact {
        hash: String,
    },
    /// Read a media file back for an inline player. `mime` comes from the
    /// library entry; the bytes become a data URL off the UI thread.
    PreviewArtifact {
        hash: String,
        mime: String,
    },
    /// The open conversation's background media jobs.
    MediaJobs,
    /// `GET /v1/logs?level=&q=` with the Logs tab's current filter.
    Logs {
        level: Option<String>,
        query: String,
    },
    /// Routines, monitoring sources and active plans (0.29.0).
    Workflows,
    /// `PUT /v1/routines/{name}` then refresh workflows (0.30.0 B1).
    SaveRoutine {
        name: String,
        enabled: Option<bool>,
        time: Option<String>,
        timezone: Option<String>,
    },
    /// `POST /v1/monitoring/run` then refresh workflows (0.30.0 B2).
    RunAllWatchSources,
    /// `DELETE /v1/routines/{name}` then refresh workflows.
    DeleteRoutine {
        name: String,
    },
    /// `POST /v1/routines/{name}/run` then refresh workflows.
    RunRoutine {
        name: String,
    },
    /// `POST /v1/sources/rss` then refresh workflows.
    AddFeed {
        name: String,
        url: String,
    },
    /// `DELETE /v1/sources/{name}` (the caller's own feed) then refresh.
    DeleteFeed {
        name: String,
    },
    /// `PATCH /v1/sources/{name}`: enable or disable, or change the URL of the
    /// caller's own feed. Then refresh workflows.
    PatchSource {
        name: String,
        enabled: Option<bool>,
        url: Option<String>,
    },
    /// `GET /v1/artifacts/{hash}/preview` for one document artifact.
    DocPreview {
        hash: String,
    },
    /// `GET /v1/settings/turn-limits`.
    TurnLimits,
    /// `PUT /v1/settings/turn-limits` with the owner's chosen values.
    SaveTurnLimits {
        calls: usize,
        rounds: usize,
    },
    /// `DELETE /v1/settings/turn-limits`: back to the config defaults.
    ResetTurnLimits,
    /// `GET /v1/memory/settings`.
    MemorySettings,
    /// `PUT /v1/memory/mem0/key`.
    SetMem0Key {
        key: Zeroizing<String>,
    },
    /// `DELETE /v1/memory/mem0/key`.
    DeleteMem0Key,
    TranscribeAudio {
        filename: String,
        bytes: Vec<u8>,
    },
    AdminSaveProvider(NewProvider),
    /// PUT /v1/providers/{id}/key: the account's own API key.
    SaveProviderKey {
        id: String,
        key: Zeroizing<String>,
    },
    /// POST /v1/providers/{id}/test.
    TestProvider {
        id: String,
    },
    /// PUT /v1/providers/default-route.
    SetDefaultRoute {
        provider: String,
        model: String,
    },
    AdminRemoveProvider(String),
    AdminSetConnector {
        id: String,
        update: ConnectorUpdate,
    },
    AdminSetAudio(AudioUpdate),
    /// Read the voice/OCR readiness surface.
    VoiceSettings,
    /// Write the voice/OCR settings (admin only).
    AdminSetVoice(VoiceUpdate),
    TestConnector {
        id: String,
    },
    /// Browser sign-in for an OAuth provider. `kind` picks PKCE or device flow.
    OAuthSignIn {
        id: String,
        kind: String,
    },
    /// Browser sign-in for a connector (ElevenLabs CIMD/PKCE). The callback
    /// URI is fixed by the server's public client metadata document.
    ConnectorOAuthSignIn {
        id: String,
        callback_uri: String,
    },
    /// Forget this account's sign-in for a connector (ElevenLabs).
    ConnectorSignOut {
        id: String,
    },
    /// `PUT /v1/bots/{id}`: name, description, instructions, model, profile.
    UpdateBot {
        id: String,
        update: BotSettingsUpdate,
    },
    /// `PUT /v1/bots/{id}` with `enabled` only.
    SetBotEnabled {
        id: String,
        enabled: bool,
    },
    /// `DELETE /v1/bots/{id}`.
    DeleteBot {
        id: String,
    },
    AdminRestart,
    /// `GET /v1/bots/{id}`: the full record of one bot.
    BotDetail {
        id: String,
    },
    /// `PUT /v1/admin/bots/{id}/config`: a write, never replayed blindly.
    UpdateBotConfig {
        id: String,
        update: BotConfigUpdate,
    },
    /// `GET /v1/admin/status` (admin only): the owner's server health card.
    ServerHealth,
    /// `GET /v1/bots/catalog`.
    BotCatalog,
    /// `POST /v1/bots/catalog/{name}/install`.
    InstallShippedBot {
        name: String,
    },
    /// `GET /v1/usage?days=&bot=&conversation_id=`.
    Usage {
        days: u32,
        bot: Option<String>,
        conversation: Option<String>,
    },
    /// `POST /v1/bots` with a name; the server derives the id.
    CreateNamedBot {
        name: String,
        model: Option<String>,
        group: Option<String>,
    },
    /// Download one avatar. `version` is the server stamp it is fetched for.
    AvatarFetch {
        target: AvatarTarget,
        version: Option<String>,
    },
    AvatarUpload {
        target: AvatarTarget,
        bytes: Vec<u8>,
    },
    AvatarRemove {
        target: AvatarTarget,
    },
    CreateGroup(NewGroup),
    OnboardingLoad {
        id: String,
    },
    OnboardingStart {
        id: String,
    },
    OnboardingReply {
        id: String,
        text: Zeroizing<String>,
    },
    OnboardingConfirm {
        id: String,
    },
    SetBotTools {
        id: String,
        tools: Vec<String>,
    },
    /// `POST /v1/cards/{id}/answer` with the owner's answer.
    AnswerCard {
        id: String,
        answer: String,
    },
    /// `POST /v1/cards/{id}/decline`.
    DeclineCard {
        id: String,
    },
    /// `POST /v1/cards/{id}/secret`. The value is never logged or kept in state.
    SubmitSecretCard {
        id: String,
        value: Zeroizing<String>,
    },
}

impl Operation {
    pub fn kind(&self) -> OperationKind {
        match self {
            Self::Health => OperationKind::Health,
            Self::Me => OperationKind::Me,
            Self::Catalog => OperationKind::Catalog,
            Self::History => OperationKind::History,
            Self::Apply(_) => OperationKind::Apply,
            Self::Submit(_) | Self::SubmitWith { .. } => OperationKind::Submit,
            Self::UploadAttachment { .. } => OperationKind::Attach,
            Self::Task(_) => OperationKind::Task,
            Self::Approvals => OperationKind::Approvals,
            Self::StandingGrants => OperationKind::StandingGrants,
            Self::RevokeStandingGrant { .. } => OperationKind::RevokeStandingGrant,
            Self::JobsReliability => OperationKind::JobsReliability,
            Self::Decide { .. } => OperationKind::Decide,
            Self::CreateBot { .. } => OperationKind::CreateBot,
            Self::CreateConversation { .. } => OperationKind::CreateConversation,
            Self::CreateSkill { .. } => OperationKind::CreateSkill,
            Self::ListSkills => OperationKind::ListSkills,
            Self::ListBots => OperationKind::ListBots,
            Self::ListConversations { .. } => OperationKind::ListConversations,
            Self::ListArchived => OperationKind::ListArchived,
            Self::MarketplaceList => OperationKind::MarketplaceList,
            Self::MarketplaceDetail { .. } => OperationKind::MarketplaceDetail,
            Self::MarketplaceAct { .. } | Self::MarketplaceConfig { .. } => {
                OperationKind::MarketplaceWrite
            }
            Self::ListBotChats { .. } => OperationKind::ListBotChats,
            Self::CreateBotChat { .. } => OperationKind::CreateBotChat,
            Self::SetBotRefresh { .. } => OperationKind::SetBotRefresh,
            Self::RenameConversation { .. } => OperationKind::RenameConversation,
            Self::ArchiveConversation { .. } => OperationKind::ArchiveConversation,
            Self::UnarchiveConversation { .. } => OperationKind::UnarchiveConversation,
            Self::ListMemories { .. } => OperationKind::ListMemories,
            Self::AddMemory { .. } => OperationKind::AddMemory,
            Self::UpdateMemory { .. } => OperationKind::UpdateMemory,
            Self::SetMemoryStatus { .. } => OperationKind::SetMemoryStatus,
            Self::DeleteMemory { .. } => OperationKind::DeleteMemory,
            Self::MemoryStats => OperationKind::MemoryStats,
            Self::ListTasks => OperationKind::ListTasks,
            Self::AddTask { .. } => OperationKind::AddTask,
            Self::SetTaskStatus { .. } => OperationKind::SetTaskStatus,
            Self::DeleteTask { .. } => OperationKind::DeleteTask,
            Self::TaskPrefs | Self::TaskSyncStatus | Self::TaskCalendar { .. } => {
                OperationKind::TaskPrefs
            }
            Self::SetTaskPrefs { .. } | Self::RunTaskSync => OperationKind::SetTaskPrefs,
            Self::Providers => OperationKind::Providers,
            Self::Connectors => OperationKind::Connectors,
            Self::GetTeam => OperationKind::Team,
            Self::SetTeam { .. } => OperationKind::TeamWrite,
            Self::Library => OperationKind::Library,
            Self::ChatPlan => OperationKind::ChatPlan,
            Self::SetAutoApprove { .. } => OperationKind::ChatAutoApprove,
            Self::SetThinking { .. } => OperationKind::Thinking,
            Self::StopTurn => OperationKind::StopTurn,
            Self::FetchArtifact { .. } => OperationKind::FetchArtifact,
            Self::PreviewArtifact { .. } => OperationKind::PreviewArtifact,
            Self::MediaJobs => OperationKind::MediaJobs,
            Self::Logs { .. } => OperationKind::Logs,
            Self::Workflows => OperationKind::Workflows,
            Self::SaveRoutine { .. }
            | Self::RunAllWatchSources
            | Self::DeleteRoutine { .. }
            | Self::RunRoutine { .. }
            | Self::AddFeed { .. }
            | Self::DeleteFeed { .. }
            | Self::PatchSource { .. } => OperationKind::WorkflowsWrite,
            Self::DocPreview { .. } => OperationKind::DocPreview,
            Self::TurnLimits => OperationKind::TurnLimits,
            Self::SaveTurnLimits { .. } | Self::ResetTurnLimits => OperationKind::TurnLimitsWrite,
            Self::MemorySettings => OperationKind::MemorySettingsWrite,
            Self::SetMem0Key { .. } | Self::DeleteMem0Key => OperationKind::MemorySettingsWrite,
            Self::TranscribeAudio { .. } => OperationKind::Transcribe,
            Self::AdminSaveProvider(_)
            | Self::AdminRemoveProvider(_)
            | Self::AdminSetConnector { .. }
            | Self::AdminSetAudio(_) => OperationKind::AdminWrite,
            Self::SaveProviderKey { .. } => OperationKind::ProviderKey,
            Self::TestProvider { .. } => OperationKind::ProviderTest,
            Self::SetDefaultRoute { .. } => OperationKind::DefaultRoute,
            Self::VoiceSettings => OperationKind::Health,
            Self::AdminSetVoice(_) => OperationKind::AdminWrite,
            Self::TestConnector { .. } => OperationKind::ConnectorTest,
            Self::OAuthSignIn { .. } => OperationKind::OAuthSignIn,
            Self::ConnectorOAuthSignIn { .. } => OperationKind::OAuthSignIn,
            Self::ConnectorSignOut { .. } => OperationKind::ConnectorSignOut,
            Self::UpdateBot { .. } => OperationKind::BotSettingsWrite,
            Self::SetBotEnabled { .. } | Self::DeleteBot { .. } => OperationKind::BotLifecycle,
            Self::AdminRestart => OperationKind::AdminRestart,
            Self::BotDetail { .. } => OperationKind::BotDetail,
            Self::UpdateBotConfig { .. } => OperationKind::BotConfigWrite,
            Self::Usage { .. } => OperationKind::Usage,
            Self::ServerHealth => OperationKind::ServerHealth,
            Self::BotCatalog => OperationKind::BotCatalog,
            Self::InstallShippedBot { .. } => OperationKind::BotInstall,
            Self::CreateNamedBot { .. } => OperationKind::CreateBot,
            Self::AvatarFetch { .. } | Self::AvatarUpload { .. } | Self::AvatarRemove { .. } => {
                OperationKind::Avatar
            }
            Self::CreateGroup(_) => OperationKind::CreateGroup,
            Self::OnboardingLoad { .. } => OperationKind::OnboardingRead,
            Self::OnboardingStart { .. }
            | Self::OnboardingReply { .. }
            | Self::OnboardingConfirm { .. } => OperationKind::OnboardingWrite,
            Self::SetBotTools { .. } => OperationKind::ToolsWrite,
            Self::AnswerCard { .. } | Self::DeclineCard { .. } | Self::SubmitSecretCard { .. } => {
                OperationKind::CardWrite
            }
        }
    }
}

pub enum Reply {
    Health(Health),
    Me(AuthUser),
    Catalog(Catalog),
    History(History),
    Selection(Selection),
    Task(Task),
    Approvals(Vec<Approval>),
    StandingGrants(crate::standing_grants::StandingGrantsSnapshot),
    StandingGrantRevoked,
    JobsReliability(crate::jobs_reliability::JobsReliabilitySnapshot),
    Decided,
    CreatedBot(CreatedBot),
    CreatedConversation(CreatedConversation),
    CreatedSkill(CreatedSkill),
    Skills(Vec<SkillSummary>),
    BotChats(crate::bot_chats::BotChatList),
    MarketplacePlugins(Vec<crate::marketplace::PluginSummary>),
    MarketplaceDetail(Box<crate::marketplace::PluginDetail>),
    /// After a change: the refreshed catalog and the plugin that changed
    /// (absent when it was uninstalled and the page is closed).
    MarketplaceChanged {
        plugins: Vec<crate::marketplace::PluginSummary>,
        detail: Option<Box<crate::marketplace::PluginDetail>>,
    },
    BotChatCreated {
        bot: String,
        created: crate::bot_chats::CreatedBotChat,
    },
    BotRefreshSet(crate::bot_chats::BotChatSettings),
    Bots(Vec<BotSummary>),
    Conversations(ConversationPage),
    ArchivedConversations(ConversationPage),
    RenamedConversation(ConversationSummary),
    ArchivedConversation(String),
    UnarchivedConversation(String),
    Memories(Vec<MemoryEntry>),
    AddedMemory(MemoryEntry),
    UpdatedMemory(MemoryEntry),
    DeletedMemory(String),
    MemoryStats(MemoryStatsView),
    Tasks(Vec<crate::api::TaskEntry>),
    AddedTask(crate::api::TaskEntry),
    UpdatedTask(crate::api::TaskEntry),
    DeletedTask(String),
    TaskPrefs(crate::api::TaskPrefs),
    TaskSyncStatus(crate::api::TaskSyncStatus),
    TaskSyncReport(crate::api::TaskSyncReport),
    TaskEvents(Vec<crate::api::TaskEvent>),
    Providers(Vec<ProviderStatus>),
    Connectors(Vec<ConnectorStatus>),
    Team(Team),
    Library(Vec<LibraryEntry>),
    ChatPlan {
        plan: Option<crate::chat_plan::ChatPlan>,
        auto_approve: bool,
    },
    AutoApprove(bool),
    /// The thinking level the server stored for the open conversation.
    Thinking(Option<String>),
    /// Turns marked to stop (0 when nothing was running).
    Stopping(usize),
    Artifact {
        hash: String,
        filename: String,
        bytes: Vec<u8>,
    },
    Transcribed(Transcription),
    /// One uploaded chat attachment; the session keeps it on the draft.
    Attached(crate::api::AttachmentInfo),
    /// Voice/OCR readiness from `GET /v1/settings/voice`.
    Voice(VoiceSettings),
    /// The open conversation's background media jobs.
    MediaJobs(Vec<crate::api::MediaJob>),
    /// The Logs tab read. `None` when the server has no logs route yet (404).
    Logs(Option<Vec<crate::logs::LogEntry>>),
    /// The Workflows tab read (0.29.0).
    Workflows(crate::workflows::WorkflowsSnapshot),
    /// One artifact's server-rendered preview; `None` means "no preview,
    /// use the download route".
    DocPreview {
        hash: String,
        preview: Option<crate::preview::ArtifactPreview>,
    },
    /// The owner's turn limits, after a read, a save or a reset.
    TurnLimits(crate::api::TurnLimitsView),
    MemorySettings(crate::api::MemorySettingsView),
    Mem0KeySaved,
    Mem0KeyDeleted,
    /// A media file as a data URL, ready for an inline player.
    Preview {
        hash: String,
        data_url: String,
    },
    AdminSaved,
    ProviderKeySaved,
    DefaultRouteSaved,
    ProviderTested {
        id: String,
        result: crate::api::ProviderTest,
    },
    ConnectorTested(ConnectorTestResult),
    /// Sign-in finished (PKCE) or started (device, with the code to enter).
    OAuthSignedIn {
        id: String,
        device_code: Option<String>,
    },
    BotSettingsSaved,
    AdminRestarted,
    BotDetail(BotDetail),
    BotConfigSaved {
        restart_required: bool,
    },
    Usage(UsageReport),
    ServerHealth(ServerHealth),
    /// The shipped templates for "Meet your team".
    ShippedBots(Vec<ShippedBot>),
    /// The named shipped bot is now the owner's own.
    ShippedBotInstalled(String),
    /// A fetched avatar as a `data:` URL; `None` when the bot has no image.
    AvatarImage {
        target: AvatarTarget,
        version: Option<String>,
        data_url: Option<String>,
    },
    AvatarSaved(AvatarTarget),
    CreatedGroup(CreatedGroup),
    Onboarding(Onboarding),
    ToolsSaved,
    /// An answer, decline or secret was accepted. The transcript reloads next.
    CardWritten,
}

pub struct Job {
    pub scope: Scope,
    pub credentials: Arc<Credentials>,
    pub operation: Operation,
}
pub struct Completion {
    pub scope: Scope,
    pub kind: OperationKind,
    pub result: Result<Reply, ApiError>,
}

pub struct Worker {
    sender: SyncSender<Job>,
    receiver: Receiver<Completion>,
    epoch: Arc<AtomicU64>,
}

impl Worker {
    pub fn start() -> std::io::Result<Self> {
        let (sender, jobs) = mpsc::sync_channel::<Job>(1);
        let (results, receiver) = mpsc::channel();
        let epoch = Arc::new(AtomicU64::new(0));
        let current = Arc::clone(&epoch);
        std::thread::Builder::new()
            .name("serabot-http".into())
            .spawn(move || {
                // One client (and connection pool) serves every job for the
                // same endpoint and token; it is rebuilt when either changes.
                let mut client: Option<ApiClient> = None;
                while let Ok(job) = jobs.recv() {
                    if current.load(Ordering::SeqCst) != job.scope.epoch {
                        continue;
                    }
                    let kind = job.operation.kind();
                    // Blocking client construction, I/O and destruction stay off UI.
                    let result = execute(&job, &current, &mut client);
                    // Wipe/drop stale content here, not just when UI eventually polls.
                    if current.load(Ordering::SeqCst) != job.scope.epoch {
                        continue;
                    }
                    if results
                        .send(Completion {
                            scope: job.scope,
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
            sender,
            receiver,
            epoch,
        })
    }

    pub fn set_epoch(&self, epoch: u64) {
        self.epoch.store(epoch, Ordering::SeqCst);
    }
    pub fn dispatch(&self, job: Job) -> Result<(), Box<TrySendError<Job>>> {
        // The error carries the unsent job. Box it so a fleet payload does not
        // inflate the Result error variant.
        self.sender.try_send(job).map_err(Box::new)
    }
    pub fn completion(&self) -> Option<Completion> {
        self.receiver.try_recv().ok()
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        self.epoch.fetch_add(1, Ordering::SeqCst);
    }
}

fn oauth_sign_in(client: &ApiClient, id: &str, kind: &str) -> Result<Reply, ApiError> {
    use crate::oauth;
    if kind == "muse-code" {
        // Device flow: the server polls in the background. The console only
        // opens the verification page and shows the code to type there.
        let start = client.admin_oauth_start(id, &serde_json::json!({}))?;
        let _ = webbrowser::open(&start.url);
        return Ok(Reply::OAuthSignedIn {
            id: id.to_owned(),
            device_code: start.code,
        });
    }
    let (listener, port) = oauth::bind_loopback()
        .map_err(|_| ApiError::Validation("Could not listen on 127.0.0.1 for the redirect."))?;
    let pkce = oauth::new_pkce();
    if kind == "openrouter" {
        // OpenRouter documents localhost callbacks. The key is exchanged by
        // the server and stored under this account; it never comes back here.
        let redirect = oauth::redirect_uri(port).replace("127.0.0.1", "localhost");
        let start = client.provider_oauth_start(
            id,
            &serde_json::json!({
                "challenge": pkce.challenge,
                "state": pkce.state,
                "redirect_uri": redirect,
            }),
        )?;
        webbrowser::open(&start.url)
            .map_err(|_| ApiError::Validation("Could not open the browser for sign-in."))?;
        let code =
            oauth::wait_for_callback(&listener, &pkce.state, std::time::Duration::from_secs(180))
                .map_err(|error| ApiError::Rejected(error.to_string()))?;
        drop(listener);
        client.provider_oauth_complete(id, &code, &pkce.verifier, &redirect)?;
        return Ok(Reply::OAuthSignedIn {
            id: id.to_owned(),
            device_code: None,
        });
    }
    let redirect = oauth::redirect_uri(port);
    let start = client.admin_oauth_start(
        id,
        &serde_json::json!({
            "challenge": pkce.challenge,
            "state": pkce.state,
            "redirect_uri": redirect,
        }),
    )?;
    webbrowser::open(&start.url)
        .map_err(|_| ApiError::Validation("Could not open the browser for sign-in."))?;
    let code =
        oauth::wait_for_callback(&listener, &pkce.state, std::time::Duration::from_secs(180))
            .map_err(|error| ApiError::Rejected(error.to_string()))?;
    drop(listener);
    client.admin_oauth_complete(id, &code, &pkce.verifier, &redirect)?;
    Ok(Reply::OAuthSignedIn {
        id: id.to_owned(),
        device_code: None,
    })
}

/// Connector sign-in (ElevenLabs): the redirect URI is fixed by the CIMD
/// document, so the loopback listener binds exactly that port — no fallback.
fn connector_oauth_sign_in(
    client: &ApiClient,
    id: &str,
    callback_uri: &str,
) -> Result<Reply, ApiError> {
    use crate::oauth;
    let port = callback_uri
        .strip_prefix("http://127.0.0.1:")
        .and_then(|rest| rest.split('/').next())
        .and_then(|port| port.parse::<u16>().ok())
        .filter(|port| *port > 0)
        .ok_or(ApiError::Validation(
            "The server did not report a loopback callback URI.",
        ))?;
    let redirect = oauth::redirect_uri(port);
    if redirect != callback_uri {
        return Err(ApiError::Validation(
            "The server's callback URI is not a loopback redirect.",
        ));
    }
    let listener = oauth::bind_loopback_fixed(port).map_err(|_| {
        ApiError::Validation("Could not listen on 127.0.0.1 for the redirect (port busy?).")
    })?;
    let pkce = oauth::new_pkce();
    let start = client.admin_connector_oauth_start(
        id,
        &serde_json::json!({
            "challenge": pkce.challenge,
            "state": pkce.state,
            "redirect_uri": redirect,
        }),
    )?;
    webbrowser::open(&start.url)
        .map_err(|_| ApiError::Validation("Could not open the browser for sign-in."))?;
    let code =
        oauth::wait_for_callback(&listener, &pkce.state, std::time::Duration::from_secs(180))
            .map_err(|error| ApiError::Rejected(error.to_string()))?;
    drop(listener);
    // The server seals the tokens to this account; the card reads it back.
    client.admin_connector_oauth_complete(id, &code, &pkce.verifier, &redirect)?;
    Ok(Reply::OAuthSignedIn {
        id: id.to_owned(),
        device_code: None,
    })
}

/// The client for `credentials`, reusing `cache` when it targets the same
/// endpoint and token.
fn client_for<'a>(
    cache: &'a mut Option<ApiClient>,
    credentials: &Credentials,
) -> Result<&'a ApiClient, ApiError> {
    let reusable = cache
        .as_ref()
        .is_some_and(|client| client.targets(&credentials.endpoint, &credentials.token));
    if !reusable {
        *cache = None;
        *cache = Some(ApiClient::new(
            credentials.endpoint.clone(),
            credentials.token.clone(),
        )?);
    }
    cache.as_ref().ok_or(ApiError::Transport)
}

fn execute(job: &Job, epoch: &AtomicU64, cache: &mut Option<ApiClient>) -> Result<Reply, ApiError> {
    let client = client_for(cache, &job.credentials)?;
    // A queued action invalidated by disconnect/conversation switch is never sent.
    // An already in-flight HTTP request cannot be recalled or safely replayed.
    if epoch.load(Ordering::SeqCst) != job.scope.epoch {
        return Err(ApiError::Transport);
    }
    let conversation = &job.scope.conversation;
    match &job.operation {
        Operation::Health => client.health().map(Reply::Health),
        Operation::Me => client.me().map(Reply::Me),
        Operation::Catalog => client.models(conversation).map(Reply::Catalog),
        Operation::History => client.history(conversation).map(Reply::History),
        Operation::Apply(selection) => client.select(conversation, selection).map(Reply::Selection),
        Operation::Submit(text) => client.submit(conversation, text).map(Reply::Task),
        Operation::SubmitWith { text, attachments } => client
            .submit_with(conversation, text, attachments)
            .map(Reply::Task),
        Operation::UploadAttachment { filename, bytes } => client
            .upload_attachment(conversation, filename, bytes)
            .map(Reply::Attached),
        Operation::Task(id) => client.task(id).map(Reply::Task),
        Operation::Approvals => client.approvals().map(Reply::Approvals),
        Operation::StandingGrants => client.standing_grants().map(Reply::StandingGrants),
        Operation::RevokeStandingGrant { id } => client
            .revoke_standing_grant(id)
            .map(|()| Reply::StandingGrantRevoked),
        Operation::JobsReliability => client.jobs_reliability().map(Reply::JobsReliability),
        Operation::Decide { id, decision } => client
            .decide_approval(id, *decision)
            .map(|_| Reply::Decided),
        Operation::CreateBot {
            id,
            description,
            tools,
            skills,
        } => client
            .create_bot(id, description, tools, skills)
            .map(Reply::CreatedBot),
        Operation::CreateConversation {
            kind,
            slug,
            parent_id,
            title,
        } => client
            .create_conversation(*kind, slug, parent_id.as_deref(), title.as_deref())
            .map(Reply::CreatedConversation),
        Operation::CreateSkill {
            id,
            description,
            body,
        } => {
            let body = if body.is_empty() {
                None
            } else {
                Some(body.as_str())
            };
            client
                .create_skill(id, description, body)
                .map(Reply::CreatedSkill)
        }
        Operation::ListSkills => client.list_skills().map(Reply::Skills),
        Operation::ListBots => client.list_bots().map(Reply::Bots),
        Operation::ListConversations { include_archived } => client
            .list_conversations(100, None, *include_archived)
            .map(Reply::Conversations),
        Operation::ListArchived => client
            .list_conversations(100, None, true)
            .map(Reply::ArchivedConversations),
        Operation::MarketplaceList => client.marketplace_plugins().map(Reply::MarketplacePlugins),
        Operation::MarketplaceDetail { slug } => client
            .marketplace_plugin(slug)
            .map(|detail| Reply::MarketplaceDetail(Box::new(detail))),
        Operation::MarketplaceAct { slug, action } => {
            client.marketplace_act(slug, *action)?;
            marketplace_changed(client, slug)
        }
        Operation::MarketplaceConfig { slug, values } => {
            client.marketplace_config(slug, values)?;
            marketplace_changed(client, slug)
        }
        Operation::ListBotChats { bot } => client.list_bot_chats(bot).map(Reply::BotChats),
        Operation::CreateBotChat { bot, title } => client
            .create_bot_chat(bot, title.as_deref())
            .map(|created| Reply::BotChatCreated {
                bot: bot.clone(),
                created,
            }),
        Operation::SetBotRefresh { bot, every } => client
            .set_bot_refresh(bot, *every)
            .map(Reply::BotRefreshSet),
        Operation::RenameConversation { id, title } => client
            .rename_conversation(id, title)
            .map(Reply::RenamedConversation),
        Operation::ArchiveConversation { id } => client
            .archive_conversation(id)
            .map(|_| Reply::ArchivedConversation(id.clone())),
        Operation::UnarchiveConversation { id } => client
            .unarchive_conversation(id)
            .map(|_| Reply::UnarchivedConversation(id.clone())),
        Operation::ListMemories {
            query,
            origin,
            status,
        } => client
            .list_memories(query.as_deref(), origin, status)
            .map(Reply::Memories),
        Operation::AddMemory {
            kind,
            content,
            source,
        } => client
            .add_memory(kind, content, source)
            .map(Reply::AddedMemory),
        Operation::UpdateMemory { id, content } => {
            client.update_memory(id, content).map(Reply::UpdatedMemory)
        }
        Operation::SetMemoryStatus { id, status } => client
            .set_memory_status(id, status)
            .map(Reply::UpdatedMemory),
        Operation::DeleteMemory { id } => client
            .delete_memory(id)
            .map(|_| Reply::DeletedMemory(id.clone())),
        Operation::MemoryStats => client.memory_stats().map(Reply::MemoryStats),
        Operation::ListTasks => client.list_tasks(None, None, None).map(Reply::Tasks),
        Operation::AddTask { title, due } => client
            .create_task(title, due.as_deref())
            .map(Reply::AddedTask),
        Operation::SetTaskStatus { id, status } => {
            client.patch_task_status(id, status).map(Reply::UpdatedTask)
        }
        Operation::DeleteTask { id } => client
            .delete_task(id)
            .map(|_| Reply::DeletedTask(id.clone())),
        Operation::TaskPrefs => client.task_prefs().map(Reply::TaskPrefs),
        Operation::SetTaskPrefs { provider } => {
            client.put_task_prefs(provider).map(Reply::TaskPrefs)
        }
        Operation::TaskSyncStatus => client.task_sync_status().map(Reply::TaskSyncStatus),
        Operation::RunTaskSync => client.run_task_sync().map(Reply::TaskSyncReport),
        Operation::TaskCalendar { from, to } => {
            client.task_calendar_events(from, to).map(Reply::TaskEvents)
        }
        Operation::Providers => client.providers().map(|mut providers| {
            // The account's own default route marks one card with a star;
            // without one, the server default is the starred provider.
            let chosen = client.default_route().ok().flatten();
            for provider in &mut providers {
                provider.is_default = match &chosen {
                    Some(route) => provider.id == route.provider,
                    None => provider.id == "default",
                };
                if let Some(route) = chosen.as_ref().filter(|_| provider.is_default) {
                    provider.default_model = Some(route.model.clone());
                }
            }
            Reply::Providers(providers)
        }),
        Operation::Connectors => client.connectors().map(Reply::Connectors),
        Operation::GetTeam => client.get_team(conversation).map(Reply::Team),
        Operation::SetTeam {
            members,
            mode,
            rules,
        } => client
            .set_team(conversation, members, *mode, rules)
            .map(Reply::Team),
        Operation::Library => client.library(conversation).map(Reply::Library),
        Operation::ChatPlan => client
            .chat_state(conversation)
            .map(|(plan, auto_approve)| Reply::ChatPlan { plan, auto_approve }),
        Operation::SetAutoApprove { enabled } => client
            .set_auto_approve(conversation, *enabled)
            .map(Reply::AutoApprove),
        Operation::SetThinking { level } => client
            .set_thinking(conversation, level.as_deref())
            .map(Reply::Thinking),
        Operation::StopTurn => client.stop_conversation(conversation).map(Reply::Stopping),
        Operation::FetchArtifact { hash } => {
            client
                .artifact(hash)
                .map(|(filename, bytes)| Reply::Artifact {
                    hash: hash.clone(),
                    filename,
                    bytes,
                })
        }
        Operation::PreviewArtifact { hash, mime } => {
            // A type that cannot play inline is refused before any download.
            if crate::media_view::inline_player(mime).is_none() {
                return Err(ApiError::Validation("Only video and audio play inline."));
            }
            client
                .artifact_capped(hash, crate::media_view::MAX_PREVIEW_BYTES as u64)
                .and_then(|(_, bytes)| {
                    crate::media_view::preview_data_url_for(mime, &bytes).map(|data_url| {
                        Reply::Preview {
                            hash: hash.clone(),
                            data_url,
                        }
                    })
                })
        }
        Operation::MediaJobs => client.media_jobs(conversation).map(Reply::MediaJobs),
        Operation::Logs { level, query } => client.logs(level.as_deref(), query).map(Reply::Logs),
        Operation::Workflows => client.workflows_snapshot().map(Reply::Workflows),
        Operation::SaveRoutine {
            name,
            enabled,
            time,
            timezone,
        } => {
            client.put_routine(name, *enabled, time.as_deref(), timezone.as_deref())?;
            client.workflows_snapshot().map(Reply::Workflows)
        }
        Operation::RunAllWatchSources => {
            client.run_all_watch_sources()?;
            client.workflows_snapshot().map(Reply::Workflows)
        }
        Operation::DeleteRoutine { name } => {
            client.delete_routine(name)?;
            client.workflows_snapshot().map(Reply::Workflows)
        }
        Operation::RunRoutine { name } => {
            client.run_routine(name)?;
            client.workflows_snapshot().map(Reply::Workflows)
        }
        Operation::AddFeed { name, url } => {
            client.add_member_feed(name, url)?;
            client.workflows_snapshot().map(Reply::Workflows)
        }
        Operation::DeleteFeed { name } => {
            client.delete_member_feed(name)?;
            client.workflows_snapshot().map(Reply::Workflows)
        }
        Operation::PatchSource { name, enabled, url } => {
            client.patch_source(name, *enabled, url.as_deref())?;
            client.workflows_snapshot().map(Reply::Workflows)
        }
        Operation::DocPreview { hash } => {
            client
                .artifact_preview(hash)
                .map(|preview| Reply::DocPreview {
                    hash: hash.clone(),
                    preview,
                })
        }
        Operation::TurnLimits => client.turn_limits().map(Reply::TurnLimits),
        Operation::SaveTurnLimits { calls, rounds } => client
            .set_turn_limits(*calls, *rounds)
            .map(Reply::TurnLimits),
        Operation::ResetTurnLimits => client.reset_turn_limits().map(Reply::TurnLimits),
        Operation::MemorySettings => client.memory_settings().map(Reply::MemorySettings),
        Operation::SetMem0Key { key } => client.set_mem0_key(key).map(|_| Reply::Mem0KeySaved),
        Operation::DeleteMem0Key => client.delete_mem0_key().map(|_| Reply::Mem0KeyDeleted),
        Operation::TranscribeAudio { filename, bytes } => client
            .transcribe_audio(filename, bytes)
            .map(Reply::Transcribed),
        Operation::AdminSaveProvider(request) => client
            .admin_add_provider(request)
            .map(|_| Reply::AdminSaved),
        Operation::AdminRemoveProvider(id) => {
            client.admin_remove_provider(id).map(|_| Reply::AdminSaved)
        }
        Operation::SaveProviderKey { id, key } => client
            .set_provider_key(id, key)
            .map(|()| Reply::ProviderKeySaved),
        Operation::TestProvider { id } => {
            client
                .test_provider(id)
                .map(|result| Reply::ProviderTested {
                    id: id.clone(),
                    result,
                })
        }
        Operation::SetDefaultRoute { provider, model } => client
            .set_default_route(provider, model)
            .map(|()| Reply::DefaultRouteSaved),
        Operation::AdminSetConnector { id, update } => client
            .admin_set_connector(id, update)
            .map(|_| Reply::AdminSaved),
        Operation::OAuthSignIn { id, kind } => oauth_sign_in(client, id, kind),
        Operation::ConnectorOAuthSignIn { id, callback_uri } => {
            connector_oauth_sign_in(client, id, callback_uri)
        }
        Operation::ConnectorSignOut { id } => client
            .admin_connector_oauth_disconnect(id)
            .map(|_| Reply::AdminSaved),
        Operation::TestConnector { id } => {
            client.admin_test_connector(id).map(Reply::ConnectorTested)
        }
        Operation::AdminSetAudio(update) => {
            client.admin_set_audio(update).map(|_| Reply::AdminSaved)
        }
        Operation::VoiceSettings => client.voice_settings().map(Reply::Voice),
        Operation::AdminSetVoice(update) => {
            client.admin_set_voice(update).map(|_| Reply::AdminSaved)
        }
        Operation::SetBotEnabled { id, enabled } => client
            .update_bot(
                id,
                &BotSettingsUpdate {
                    enabled: Some(*enabled),
                    ..Default::default()
                },
            )
            .map(|()| Reply::BotSettingsSaved),
        Operation::DeleteBot { id } => client.delete_bot(id).map(|()| Reply::BotSettingsSaved),
        Operation::UpdateBot { id, update } => client
            .update_bot(id, update)
            .map(|()| Reply::BotSettingsSaved),
        Operation::AdminRestart => client.admin_restart().map(|ack| {
            if ack.restarting {
                Reply::AdminRestarted
            } else {
                Reply::AdminSaved
            }
        }),
        Operation::BotDetail { id } => client.bot_detail(id).map(Reply::BotDetail),
        Operation::UpdateBotConfig { id, update } => {
            client
                .update_bot_config(id, update)
                .map(|ack| Reply::BotConfigSaved {
                    restart_required: ack.restart_required,
                })
        }
        Operation::ServerHealth => client.server_health().map(Reply::ServerHealth),
        Operation::BotCatalog => client.bot_catalog().map(Reply::ShippedBots),
        Operation::InstallShippedBot { name } => client
            .install_shipped_bot(name)
            .map(|()| Reply::ShippedBotInstalled(name.clone())),
        Operation::Usage {
            days,
            bot,
            conversation,
        } => client
            .usage(*days, bot.as_deref(), conversation.as_deref())
            .map(Reply::Usage),
        Operation::CreateNamedBot { name, model, group } => client
            .create_named_bot(name, model.as_deref(), group.as_deref())
            .map(Reply::CreatedBot),
        Operation::AvatarFetch { target, version } => {
            let bytes = match target {
                AvatarTarget::Bot(id) => client.bot_avatar(id)?,
                AvatarTarget::Group(id) => client.group_avatar(id)?,
            };
            Ok(Reply::AvatarImage {
                target: target.clone(),
                version: version.clone(),
                data_url: bytes.map(|bytes| crate::avatars::png_data_url(&bytes)),
            })
        }
        Operation::AvatarUpload { target, bytes } => {
            match target {
                AvatarTarget::Bot(id) => client.set_bot_avatar(id, bytes)?,
                AvatarTarget::Group(id) => client.set_group_avatar(id, bytes)?,
            }
            Ok(Reply::AvatarSaved(target.clone()))
        }
        Operation::AvatarRemove { target } => {
            match target {
                AvatarTarget::Bot(id) => client.remove_bot_avatar(id)?,
                AvatarTarget::Group(_) => {
                    return Err(ApiError::Validation("Group images cannot be removed yet."));
                }
            }
            Ok(Reply::AvatarSaved(target.clone()))
        }
        Operation::CreateGroup(group) => client.create_group(group).map(Reply::CreatedGroup),
        Operation::OnboardingLoad { id } => client.onboarding(id).map(Reply::Onboarding),
        Operation::OnboardingStart { id } => client.onboarding_start(id).map(Reply::Onboarding),
        Operation::OnboardingReply { id, text } => {
            client.onboarding_reply(id, text).map(Reply::Onboarding)
        }
        Operation::OnboardingConfirm { id } => client.onboarding_confirm(id).map(Reply::Onboarding),
        Operation::SetBotTools { id, tools } => {
            client.set_bot_tools(id, tools).map(|_| Reply::ToolsSaved)
        }
        Operation::AnswerCard { id, answer } => {
            client.answer_card(id, answer).map(|_| Reply::CardWritten)
        }
        Operation::DeclineCard { id } => client.decline_card(id).map(|_| Reply::CardWritten),
        Operation::SubmitSecretCard { id, value } => client
            .submit_secret_card(id, value)
            .map(|_| Reply::CardWritten),
    }
}

/// The catalog and one plugin, read again after a change. An uninstalled
/// plugin still has a detail page, so the read is the same either way.
fn marketplace_changed(client: &ApiClient, slug: &str) -> Result<Reply, ApiError> {
    let plugins = client.marketplace_plugins()?;
    let detail = client.marketplace_plugin(slug).ok().map(Box::new);
    Ok(Reply::MarketplaceChanged { plugins, detail })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        io::{Read, Write},
        net::TcpListener,
        time::Duration,
    };

    #[test]
    fn epoch_change_drops_late_results_and_never_sends_queued_old_mutations() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let endpoint =
            crate::validate_endpoint(&format!("http://{}", listener.local_addr().unwrap()))
                .unwrap();
        let (started, first_request) = mpsc::channel();
        let (release, wait_release) = mpsc::channel();
        let server = std::thread::spawn(move || {
            for index in 0..2 {
                let (mut stream, _) = listener.accept().unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .unwrap();
                let mut request = Vec::new();
                let mut byte = [0];
                while !request.ends_with(b"\r\n\r\n") {
                    stream.read_exact(&mut byte).unwrap();
                    request.push(byte[0]);
                }
                assert!(
                    request.starts_with(b"GET /v1/health "),
                    "stale mutation was sent"
                );
                if index == 0 {
                    started.send(()).unwrap();
                    wait_release.recv_timeout(Duration::from_secs(5)).unwrap();
                }
                let body = r#"{"ok":true,"version":"synthetic"}"#;
                write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                )
                .unwrap();
            }
        });
        let credentials = Arc::new(Credentials {
            endpoint,
            token: SecretString::new("synthetic-token-for-worker-test-0000".into()),
        });
        let worker = Worker::start().unwrap();
        let old = Scope {
            epoch: 1,
            conversation: "old".into(),
        };
        let new = Scope {
            epoch: 2,
            conversation: "new".into(),
        };
        worker.set_epoch(old.epoch);
        assert!(
            worker
                .dispatch(Job {
                    scope: old.clone(),
                    credentials: Arc::clone(&credentials),
                    operation: Operation::Health
                })
                .is_ok()
        );
        first_request.recv_timeout(Duration::from_secs(5)).unwrap();
        assert!(
            worker
                .dispatch(Job {
                    scope: old,
                    credentials: Arc::clone(&credentials),
                    operation: Operation::Submit(Zeroizing::new("must never be sent".into()))
                })
                .is_ok()
        );
        worker.set_epoch(new.epoch);
        release.send(()).unwrap();
        // A blocking send is confined to this test, to put the new job after the
        // already-queued obsolete mutation without timing-based sleeps.
        assert!(
            worker
                .sender
                .send(Job {
                    scope: new.clone(),
                    credentials,
                    operation: Operation::Health
                })
                .is_ok()
        );
        let result = worker
            .receiver
            .recv_timeout(Duration::from_secs(5))
            .unwrap();
        assert_eq!(result.scope, new);
        assert!(matches!(result.result, Ok(Reply::Health(_))));
        assert!(worker.completion().is_none());
        server.join().unwrap();
    }

    #[test]
    fn one_client_and_connection_serve_consecutive_jobs() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let endpoint =
            crate::validate_endpoint(&format!("http://{}", listener.local_addr().unwrap()))
                .unwrap();
        // The mock accepts exactly one connection and serves two keep-alive
        // requests on it. A rebuilt client would open a second connection that
        // is never accepted, and the second job would fail.
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            for _ in 0..2 {
                let mut request = Vec::new();
                let mut byte = [0];
                while !request.ends_with(b"\r\n\r\n") {
                    stream.read_exact(&mut byte).unwrap();
                    request.push(byte[0]);
                }
                let body = r#"{"ok":true,"version":"synthetic"}"#;
                write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
                    body.len()
                )
                .unwrap();
            }
        });
        let credentials = Arc::new(Credentials {
            endpoint,
            token: SecretString::new("synthetic-token-for-worker-test-0000".into()),
        });
        let worker = Worker::start().unwrap();
        let scope = Scope {
            epoch: 1,
            conversation: "reuse".into(),
        };
        worker.set_epoch(scope.epoch);
        for _ in 0..2 {
            worker
                .sender
                .send(Job {
                    scope: scope.clone(),
                    credentials: Arc::clone(&credentials),
                    operation: Operation::Health,
                })
                .unwrap();
            let done = worker
                .receiver
                .recv_timeout(Duration::from_secs(5))
                .unwrap();
            assert!(matches!(done.result, Ok(Reply::Health(_))));
        }
        server.join().unwrap();
    }
}
