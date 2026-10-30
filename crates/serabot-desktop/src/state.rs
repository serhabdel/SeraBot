use crate::{
    ApiError, Approval, BotSummary, Catalog, ConnectorStatus, ConversationSummary, CreatedBot,
    CreatedConversation, CreatedSkill, LibraryEntry, MemoryEntry, Message, ProviderStatus, Scope,
    Selection, SkillSummary, Task, Team,
    worker::{OperationKind, Reply},
};

pub struct State {
    pub scope: Scope,
    pub connected: bool,
    pub version: String,
    pub audio_enabled: Option<bool>,
    pub audio_transcription_model: Option<String>,
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
    pub history_loaded: bool,
    pub task: Option<Task>,
    pub approvals: Vec<Approval>,
    pub approvals_loaded: bool,
    pub conversations: Vec<ConversationSummary>,
    pub conversations_loaded: bool,
    /// Archived threads, loaded only when the rail expands the group.
    pub archived: Vec<ConversationSummary>,
    pub archived_loaded: bool,
    pub memories: Vec<MemoryEntry>,
    pub memories_loaded: bool,
    pub providers: Vec<ProviderStatus>,
    pub providers_loaded: bool,
    pub connectors: Vec<ConnectorStatus>,
    pub connectors_loaded: bool,
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
    /// Last transcript of a recorded note. Untrusted model output: it is a
    /// draft for the owner, never a sent message.
    pub transcript: Option<String>,
    pub transcript_error: Option<String>,
    pub transcript_loading: bool,
    pub created_bot: Option<CreatedBot>,
    pub created_conversation: Option<CreatedConversation>,
    pub created_skill: Option<CreatedSkill>,
    pub listed_skills: Vec<SkillSummary>,
    pub skills_loaded: bool,
    pub listed_bots: Vec<BotSummary>,
    pub bots_loaded: bool,
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
            version: String::new(),
            audio_enabled: None,
            audio_transcription_model: None,
            audio_understanding_model: None,
            firecrawl_enabled: None,
            telegram_enabled: None,
            catalog_fetch: None,
            bot_chat_binding: None,
            bot_profiles: None,
            conversation_titles: None,
            catalog: None,
            draft: Selection::default(),
            messages: Vec::new(),
            history_loaded: false,
            task: None,
            approvals: Vec::new(),
            approvals_loaded: false,
            conversations: Vec::new(),
            conversations_loaded: false,
            archived: Vec::new(),
            archived_loaded: false,
            memories: Vec::new(),
            memories_loaded: false,
            providers: Vec::new(),
            providers_loaded: false,
            connectors: Vec::new(),
            connectors_loaded: false,
            team: None,
            team_loaded: false,
            team_loading: false,
            team_error: None,
            library: Vec::new(),
            library_loaded: false,
            library_loading: false,
            library_error: None,
            transcript: None,
            transcript_error: None,
            transcript_loading: false,
            created_bot: None,
            created_conversation: None,
            created_skill: None,
            listed_skills: Vec::new(),
            skills_loaded: false,
            listed_bots: Vec::new(),
            bots_loaded: false,
            error: None,
            unknown_outcome: false,
        }
    }
}

impl State {
    pub fn new_scope(&mut self, conversation: String, connected: bool) {
        let epoch = self.scope.epoch.wrapping_add(1);
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
        let created_skill = self.created_skill.clone();
        let listed_skills = self.listed_skills.clone();
        let skills_loaded = self.skills_loaded;
        let listed_bots = self.listed_bots.clone();
        let bots_loaded = self.bots_loaded;
        // Directory data is owner-scoped but not conversation-scoped.
        let conversations = self.conversations.clone();
        let conversations_loaded = self.conversations_loaded;
        let memories = self.memories.clone();
        let memories_loaded = self.memories_loaded;
        let providers = self.providers.clone();
        let providers_loaded = self.providers_loaded;
        let connectors = self.connectors.clone();
        let connectors_loaded = self.connectors_loaded;
        *self = Self {
            scope: Scope {
                epoch,
                conversation,
            },
            connected,
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
            created_skill,
            listed_skills,
            skills_loaded,
            listed_bots,
            bots_loaded,
            conversations,
            conversations_loaded,
            memories,
            memories_loaded,
            providers,
            providers_loaded,
            connectors,
            connectors_loaded,
            ..Self::default()
        };
    }

    pub fn disconnect(&mut self) {
        self.new_scope(self.scope.conversation.clone(), false);
        self.created_bot = None;
        self.created_conversation = None;
        self.created_skill = None;
        self.listed_skills.clear();
        self.skills_loaded = false;
        self.listed_bots.clear();
        self.bots_loaded = false;
        self.conversations.clear();
        self.conversations_loaded = false;
        self.archived.clear();
        self.archived_loaded = false;
        self.memories.clear();
        self.memories_loaded = false;
        self.providers.clear();
        self.providers_loaded = false;
        self.connectors.clear();
        self.connectors_loaded = false;
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
            Reply::Catalog(catalog) => {
                self.draft = catalog.selection.clone();
                self.catalog = Some(catalog);
            }
            Reply::History(history) => {
                self.messages = history.messages;
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
            Reply::Decided => {}
            Reply::CreatedBot(bot) => self.created_bot = Some(bot),
            Reply::CreatedConversation(node) => self.created_conversation = Some(node),
            Reply::CreatedSkill(skill) => self.created_skill = Some(skill),
            Reply::Skills(skills) => {
                self.listed_skills = skills;
                self.skills_loaded = true;
            }
            Reply::Bots(bots) => {
                self.listed_bots = bots;
                self.bots_loaded = true;
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
            Reply::Transcribed(transcription) => {
                self.transcript = Some(transcription.text);
                self.transcript_error = None;
            }
            // Bytes are intercepted by the session before they reach state;
            // this arm only keeps the match exhaustive.
            Reply::Artifact { .. } => {}
            Reply::AdminSaved | Reply::BotProfileSaved | Reply::AdminRestarted => {}
        }
        true
    }

    pub fn fail(&mut self, scope: &Scope, kind: OperationKind, error: ApiError) -> bool {
        if !self.scope.accepts(scope) {
            return false;
        }
        let mutation = matches!(
            kind,
            OperationKind::Submit
                | OperationKind::Apply
                | OperationKind::Decide
                | OperationKind::CreateBot
                | OperationKind::CreateConversation
                | OperationKind::CreateSkill
                | OperationKind::RenameConversation
                | OperationKind::ArchiveConversation
                | OperationKind::UnarchiveConversation
                | OperationKind::AddMemory
                | OperationKind::TeamWrite
        );
        if mutation && error.unknown_outcome() {
            self.unknown_outcome = true;
            let operation = match kind {
                OperationKind::Submit => "submission",
                OperationKind::Decide => "approval decision",
                OperationKind::CreateBot => "bot scaffold",
                OperationKind::CreateConversation => "conversation create",
                OperationKind::CreateSkill => "skill create",
                OperationKind::RenameConversation => "rename",
                OperationKind::ArchiveConversation => "archive",
                OperationKind::UnarchiveConversation => "unarchive",
                OperationKind::AddMemory => "memory write",
                OperationKind::TeamWrite => "team update",
                _ => "model change",
            };
            self.error = Some(format!(
                "Unknown {operation} outcome. It may have reached the server. Do not resend. Use Refresh / retry reads to inspect history and the saved selection. {error}"
            ));
        } else if kind == OperationKind::Apply && error == ApiError::Http(422) {
            self.error = Some(
                "The server rejected that model selection. Refresh, reopen Model routing, and pick a listed provider and model."
                    .into(),
            );
        } else {
            self.error = Some(error.to_string());
        }
        true
    }
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
    fn disconnect_clears_visible_sensitive_state() {
        let mut state = State::default();
        state.messages.push(Message {
            id: "m".into(),
            role: Role::Assistant,
            content: "sensitive fixture".into(),
            created_at: "now".into(),
            name: Some("analyst".into()),
            bot: Some("analyst".into()),
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
            description: None,
            avatar: Some("A".into()),
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
                catalog_verified: false,
                managed: false,
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
}
