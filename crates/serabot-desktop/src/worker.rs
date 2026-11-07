use crate::{
    ApiClient, ApiError, Approval, ApprovalDecision, AudioUpdate, AuthUser, BotConfigUpdate,
    BotDetail, BotSettingsUpdate, BotSummary, Catalog, ConnectorStatus, ConnectorTestResult,
    ConnectorUpdate, ConversationKind, ConversationPage, ConversationSummary, CreatedBot,
    CreatedConversation, CreatedGroup, CreatedSkill, Health, History, LibraryEntry, MemoryEntry,
    NewGroup, NewProvider, Onboarding, ProviderStatus, Scope, Selection, SkillSummary, Task, Team,
    TeamMode, Transcription, UsageReport, VoiceSettings, VoiceUpdate, avatars::AvatarTarget,
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
    Decide,
    CreateBot,
    CreateConversation,
    CreateSkill,
    ListSkills,
    ListBots,
    ListConversations,
    ListArchived,
    RenameConversation,
    ArchiveConversation,
    UnarchiveConversation,
    ListMemories,
    AddMemory,
    DeleteMemory,
    Providers,
    Connectors,
    Team,
    TeamWrite,
    Library,
    FetchArtifact,
    Transcribe,
    AdminWrite,
    ConnectorTest,
    OAuthSignIn,
    BotSettingsWrite,
    BotLifecycle,
    AdminRestart,
    BotDetail,
    BotConfigWrite,
    Usage,
    Avatar,
    CreateGroup,
    OnboardingRead,
    OnboardingWrite,
    ToolsWrite,
    /// Upload of one chat attachment from the composer.
    Attach,
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
    },
    AddMemory {
        kind: String,
        content: Zeroizing<String>,
        source: String,
    },
    DeleteMemory {
        id: String,
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
    FetchArtifact {
        hash: String,
    },
    TranscribeAudio {
        filename: String,
        bytes: Vec<u8>,
    },
    AdminSaveProvider(NewProvider),
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
            Self::Decide { .. } => OperationKind::Decide,
            Self::CreateBot { .. } => OperationKind::CreateBot,
            Self::CreateConversation { .. } => OperationKind::CreateConversation,
            Self::CreateSkill { .. } => OperationKind::CreateSkill,
            Self::ListSkills => OperationKind::ListSkills,
            Self::ListBots => OperationKind::ListBots,
            Self::ListConversations { .. } => OperationKind::ListConversations,
            Self::ListArchived => OperationKind::ListArchived,
            Self::RenameConversation { .. } => OperationKind::RenameConversation,
            Self::ArchiveConversation { .. } => OperationKind::ArchiveConversation,
            Self::UnarchiveConversation { .. } => OperationKind::UnarchiveConversation,
            Self::ListMemories { .. } => OperationKind::ListMemories,
            Self::AddMemory { .. } => OperationKind::AddMemory,
            Self::DeleteMemory { .. } => OperationKind::DeleteMemory,
            Self::Providers => OperationKind::Providers,
            Self::Connectors => OperationKind::Connectors,
            Self::GetTeam => OperationKind::Team,
            Self::SetTeam { .. } => OperationKind::TeamWrite,
            Self::Library => OperationKind::Library,
            Self::FetchArtifact { .. } => OperationKind::FetchArtifact,
            Self::TranscribeAudio { .. } => OperationKind::Transcribe,
            Self::AdminSaveProvider(_)
            | Self::AdminRemoveProvider(_)
            | Self::AdminSetConnector { .. }
            | Self::AdminSetAudio(_) => OperationKind::AdminWrite,
            Self::VoiceSettings => OperationKind::Health,
            Self::AdminSetVoice(_) => OperationKind::AdminWrite,
            Self::TestConnector { .. } => OperationKind::ConnectorTest,
            Self::OAuthSignIn { .. } => OperationKind::OAuthSignIn,
            Self::ConnectorOAuthSignIn { .. } => OperationKind::OAuthSignIn,
            Self::UpdateBot { .. } => OperationKind::BotSettingsWrite,
            Self::SetBotEnabled { .. } | Self::DeleteBot { .. } => OperationKind::BotLifecycle,
            Self::AdminRestart => OperationKind::AdminRestart,
            Self::BotDetail { .. } => OperationKind::BotDetail,
            Self::UpdateBotConfig { .. } => OperationKind::BotConfigWrite,
            Self::Usage { .. } => OperationKind::Usage,
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
    Decided,
    CreatedBot(CreatedBot),
    CreatedConversation(CreatedConversation),
    CreatedSkill(CreatedSkill),
    Skills(Vec<SkillSummary>),
    Bots(Vec<BotSummary>),
    Conversations(ConversationPage),
    ArchivedConversations(ConversationPage),
    RenamedConversation(ConversationSummary),
    ArchivedConversation(String),
    UnarchivedConversation(String),
    Memories(Vec<MemoryEntry>),
    AddedMemory(MemoryEntry),
    DeletedMemory(String),
    Providers(Vec<ProviderStatus>),
    Connectors(Vec<ConnectorStatus>),
    Team(Team),
    Library(Vec<LibraryEntry>),
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
    AdminSaved,
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
        Operation::RenameConversation { id, title } => client
            .rename_conversation(id, title)
            .map(Reply::RenamedConversation),
        Operation::ArchiveConversation { id } => client
            .archive_conversation(id)
            .map(|_| Reply::ArchivedConversation(id.clone())),
        Operation::UnarchiveConversation { id } => client
            .unarchive_conversation(id)
            .map(|_| Reply::UnarchivedConversation(id.clone())),
        Operation::ListMemories { query } => {
            client.list_memories(query.as_deref()).map(Reply::Memories)
        }
        Operation::AddMemory {
            kind,
            content,
            source,
        } => client
            .add_memory(kind, content, source)
            .map(Reply::AddedMemory),
        Operation::DeleteMemory { id } => client
            .delete_memory(id)
            .map(|_| Reply::DeletedMemory(id.clone())),
        Operation::Providers => client.providers().map(Reply::Providers),
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
        Operation::FetchArtifact { hash } => {
            client
                .artifact(hash)
                .map(|(filename, bytes)| Reply::Artifact {
                    hash: hash.clone(),
                    filename,
                    bytes,
                })
        }
        Operation::TranscribeAudio { filename, bytes } => client
            .transcribe_audio(filename, bytes)
            .map(Reply::Transcribed),
        Operation::AdminSaveProvider(request) => client
            .admin_add_provider(request)
            .map(|_| Reply::AdminSaved),
        Operation::AdminRemoveProvider(id) => {
            client.admin_remove_provider(id).map(|_| Reply::AdminSaved)
        }
        Operation::AdminSetConnector { id, update } => client
            .admin_set_connector(id, update)
            .map(|_| Reply::AdminSaved),
        Operation::OAuthSignIn { id, kind } => oauth_sign_in(client, id, kind),
        Operation::ConnectorOAuthSignIn { id, callback_uri } => {
            connector_oauth_sign_in(client, id, callback_uri)
        }
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
    }
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
