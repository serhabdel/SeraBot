use crate::{
    ApiError, Approval, BotSummary, Catalog, CreatedBot, CreatedConversation, CreatedSkill,
    Message, Scope, Selection, SkillSummary, Task,
    worker::{OperationKind, Reply},
};

pub struct State {
    pub scope: Scope,
    pub connected: bool,
    pub version: String,
    pub catalog: Option<Catalog>,
    pub draft: Selection,
    pub messages: Vec<Message>,
    pub history_loaded: bool,
    pub task: Option<Task>,
    pub approvals: Vec<Approval>,
    pub approvals_loaded: bool,
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
            catalog: None,
            draft: Selection::default(),
            messages: Vec::new(),
            history_loaded: false,
            task: None,
            approvals: Vec::new(),
            approvals_loaded: false,
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
        let created_bot = self.created_bot.clone();
        let created_conversation = self.created_conversation.clone();
        let created_skill = self.created_skill.clone();
        let listed_skills = self.listed_skills.clone();
        let skills_loaded = self.skills_loaded;
        let listed_bots = self.listed_bots.clone();
        let bots_loaded = self.bots_loaded;
        *self = Self {
            scope: Scope {
                epoch,
                conversation,
            },
            connected,
            created_bot,
            created_conversation,
            created_skill,
            listed_skills,
            skills_loaded,
            listed_bots,
            bots_loaded,
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
        );
        if mutation && error.unknown_outcome() {
            self.unknown_outcome = true;
            let operation = match kind {
                OperationKind::Submit => "submission",
                OperationKind::Decide => "approval decision",
                OperationKind::CreateBot => "bot scaffold",
                OperationKind::CreateConversation => "conversation create",
                OperationKind::CreateSkill => "skill create",
                _ => "model change",
            };
            self.error = Some(format!(
                "Unknown {operation} outcome. It may have reached the server. Do not resend. Use Refresh / retry reads to inspect history and the saved selection. {error}"
            ));
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
        BotSummary, Catalog, CreatedBot, CreatedConversation, CreatedSkill, Health, Message, Role,
        Selection, SkillSummary, Task, TaskStatus,
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
                version: "late".into()
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
            tools: vec!["skill_list".into()],
            skills: vec!["ask".into()],
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
}
