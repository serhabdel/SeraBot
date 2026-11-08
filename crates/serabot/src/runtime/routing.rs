//! Enqueue paths: user text, team fan-out and approval decisions.

use super::*;

impl Runtime {
    /// Queue the acting user's message. The conversation must not belong to
    /// another user (lazy creation happens under the acting user).
    pub fn submit_text(
        &self,
        user_id: &str,
        conversation_id: &str,
        text: &str,
        bot_override: Option<&str>,
    ) -> Result<Task> {
        self.submit_text_attached(user_id, conversation_id, text, bot_override, &[])
    }

    /// Like `submit_text`, with uploaded chat attachments. Each id must be an
    /// unattached upload of the acting user in this conversation; a message
    /// may carry attachments with no text.
    pub fn submit_text_attached(
        &self,
        user_id: &str,
        conversation_id: &str,
        text: &str,
        bot_override: Option<&str>,
        attachment_ids: &[String],
    ) -> Result<Task> {
        validate_conversation_id(conversation_id)?;
        self.store
            .ensure_conversation_accepts_turns(user_id, conversation_id)?;
        if attachment_ids.len() > crate::attachments::MAX_ATTACHMENTS_PER_MESSAGE {
            return Err(SeraError::Validation("too many attachments".into()));
        }
        let unique: HashSet<&String> = attachment_ids.iter().collect();
        if unique.len() != attachment_ids.len() {
            return Err(SeraError::Validation("duplicate attachment".into()));
        }
        self.store
            .pending_chat_attachments(user_id, conversation_id, attachment_ids)?;
        let text = text.trim();
        let text = if text.is_empty() && !attachment_ids.is_empty() {
            "Please review the attached file(s)."
        } else {
            text
        };
        if text.is_empty() {
            return Err(SeraError::Validation("message is empty".into()));
        }
        if text.chars().count() > 8000 {
            return Err(SeraError::Validation("message is too long".into()));
        }
        if text == "/brief" || text.starts_with("/brief ") {
            let task = self.store.enqueue_task_with_quota(
                user_id,
                "brief",
                "watcher",
                &serde_json::to_value(ScheduledPayload {
                    conversation_id: conversation_id.to_owned(),
                    user_id: user_id.to_owned(),
                })?,
                5,
                Utc::now(),
                self.config.users.tasks_per_user_per_day,
            )?;
            self.store.append_event(
                user_id,
                "task.queued",
                &json!({"task_id": task.id, "kind": "brief", "bot": "watcher"}),
            )?;
            return Ok(task);
        }
        if text == "/watch" || text.starts_with("/watch ") {
            let task = self.store.enqueue_task_with_quota(
                user_id,
                "watch",
                "watcher",
                &serde_json::to_value(ScheduledPayload {
                    conversation_id: conversation_id.to_owned(),
                    user_id: user_id.to_owned(),
                })?,
                5,
                Utc::now(),
                self.config.users.tasks_per_user_per_day,
            )?;
            self.store.append_event(
                user_id,
                "task.queued",
                &json!({"task_id": task.id, "kind": "watch", "bot": "watcher"}),
            )?;
            return Ok(task);
        }
        let fleet = self.fleet.load(user_id)?;
        let (bot, text, ask_mode) = if let Some(rest) = text.strip_prefix("/ask") {
            let question = rest.trim();
            if question.is_empty() {
                return Err(SeraError::Validation("usage: /ask <question>".into()));
            }
            ("main".to_string(), question.to_string(), true)
        } else if let Some(rest) = text.strip_prefix("/deck") {
            let brief = rest.trim();
            if brief.is_empty() {
                return Err(SeraError::Validation("usage: /deck <brief>".into()));
            }
            ("maker".to_string(), brief.to_string(), false)
        } else if let Some(rest) = text.strip_prefix("/xlsx") {
            let request = rest.trim();
            if request.is_empty() {
                return Err(SeraError::Validation(
                    "usage: /xlsx <path or question>".into(),
                ));
            }
            ("analyst".to_string(), request.to_string(), false)
        } else if let Some(rest) = text.strip_prefix("/docx") {
            let request = rest.trim();
            if request.is_empty() {
                return Err(SeraError::Validation("usage: /docx <brief>".into()));
            }
            ("maker".to_string(), request.to_string(), false)
        } else if let Some(rest) = text.strip_prefix("/pptx") {
            let request = rest.trim();
            if request.is_empty() {
                return Err(SeraError::Validation("usage: /pptx <brief>".into()));
            }
            ("maker".to_string(), request.to_string(), false)
        } else if let Some(rest) = text.strip_prefix("/pdf") {
            let request = rest.trim();
            if request.is_empty() {
                return Err(SeraError::Validation("usage: /pdf <brief>".into()));
            }
            ("maker".to_string(), request.to_string(), false)
        } else if let Some(rest) = text.strip_prefix("/timesheets") {
            let request = rest.trim();
            if request.is_empty() {
                return Err(SeraError::Validation("usage: /timesheets <request>".into()));
            }
            ("analyst".to_string(), request.to_string(), false)
        } else if let Some(rest) = text.strip_prefix("/brief") {
            let request = rest.trim();
            (
                "watcher".to_string(),
                if request.is_empty() {
                    "Prepare the current morning brief.".to_string()
                } else {
                    request.to_string()
                },
                false,
            )
        } else {
            // Rooms always run as the orchestrator; only a handoff names a specialist.
            // A `dir:<bot>` conversation talks to that bot one-to-one.
            let bot = match parse_conversation_scope(conversation_id) {
                Ok(scope) if scope.kind == ConversationKind::Room => ORCHESTRATOR_BOT,
                Ok(scope) if scope.kind == ConversationKind::Direct && bot_override.is_none() => {
                    match conversation_id.strip_prefix("dir:") {
                        Some(id) if fleet.contains_key(id) => {
                            if !fleet[id].config.enabled {
                                return Err(SeraError::InvalidState(format!(
                                    "bot {id} is disabled"
                                )));
                            }
                            id
                        }
                        _ => ORCHESTRATOR_BOT,
                    }
                }
                _ => bot_override.unwrap_or(ORCHESTRATOR_BOT),
            };
            (bot.to_string(), text.to_string(), false)
        };
        if !fleet.contains_key(&bot) {
            return Err(SeraError::NotFound(format!("bot {bot}")));
        }
        self.heal_conversation_model(user_id, conversation_id, &fleet[&bot])?;
        let selection = self.effective_model_selection(user_id, conversation_id, &bot)?;
        let route = self.router.pin(&selection)?;
        let owner_text = text.clone();
        // In rooms, `@mention` tokens are parsed from the owner text and
        // validated against the acting user's bot ids. They are routing
        // metadata only: never instructions, and never routed to non-members.
        let mentions = match parse_conversation_scope(conversation_id) {
            Ok(scope) if scope.kind == ConversationKind::Room => {
                let known: HashSet<String> = fleet.keys().cloned().collect();
                parse_mentions(&text, &known)
            }
            _ => Vec::new(),
        };
        let payload = TurnPayload {
            conversation_id: conversation_id.to_owned(),
            text,
            bot: bot.clone(),
            source: "owner".into(),
            ask_mode,
            route: Some(route),
            handoff: None,
            team: None,
            mentions: mentions.clone(),
            attachments: attachment_ids.to_vec(),
        };
        // User-initiated work: the daily quota is checked atomically with the
        // insert, so a user at the cap gets an explicit validation error.
        let task = self.store.enqueue_task_with_quota(
            user_id,
            "turn",
            &bot,
            &serde_json::to_value(payload)?,
            0,
            Utc::now(),
            self.config.users.tasks_per_user_per_day,
        )?;
        self.store.append_event(
            user_id,
            "task.queued",
            &json!({"task_id": task.id, "kind": "turn", "bot": bot}),
        )?;
        // Room mode `all`: every team member answers the owner's message without
        // waiting for the orchestrator to select. When the owner mentioned
        // bots, the fan-out is restricted to mentioned ∩ current members;
        // mentions naming non-members are ignored (never routed).
        if !ask_mode
            && bot == ORCHESTRATOR_BOT
            && let Some(team) = self.store.get_conversation_team(user_id, conversation_id)?
            && team.mode == TeamMode::All.as_str()
        {
            let mut members = team.members.clone();
            if !mentions.is_empty() {
                let kept: Vec<String> = members
                    .iter()
                    .filter(|member| mentions.contains(member))
                    .cloned()
                    .collect();
                let details = json!({
                    "root_task_id": task.id,
                    "conversation_id": conversation_id,
                    "mentions": mentions,
                    "members": team.members,
                    "kept": kept,
                });
                if let Err(error) = self.store.record_audit(
                    user_id,
                    "team.mention_filtered",
                    None,
                    Some(&task.id),
                    None,
                    None,
                    Some(self.gateway.policy().version()),
                    Some(&details),
                ) {
                    warn!(%error, "mention-filtered room fan-out was not audited");
                }
                members = kept;
            }
            self.enqueue_team_fanout(user_id, conversation_id, &owner_text, &task.id, &members);
        }
        Ok(task)
    }

    /// Queue one turn per room team member for the owner message that opened
    /// this room turn, under the acting user. Members that cannot be routed
    /// are skipped: the owner's own message never fails because of the team.
    fn enqueue_team_fanout(
        &self,
        user_id: &str,
        conversation_id: &str,
        text: &str,
        root_task_id: &str,
        members: &[String],
    ) {
        let fleet = match self.fleet.load(user_id) {
            Ok(fleet) => fleet,
            Err(error) => {
                warn!(%error, "room team fleet is invalid; fan-out skipped");
                return;
            }
        };
        for member in members {
            let Some(bot) = fleet.get(member) else {
                warn!(bot = %member, "room team member is not loaded; skipped");
                continue;
            };
            if !bot.config.enabled || member == ORCHESTRATOR_BOT {
                warn!(bot = %member, "room team member cannot take a turn; skipped");
                continue;
            }
            let frame = TeamContext {
                root_task_id: root_task_id.to_owned(),
                root_turn_id: None,
                from_bot: ORCHESTRATOR_BOT.to_owned(),
                round: 0,
            };
            let payload = (|| -> Result<serde_json::Value> {
                let selection = self.effective_model_selection(user_id, conversation_id, member)?;
                let route = self.router.pin(&selection)?;
                Ok(serde_json::to_value(TurnPayload {
                    conversation_id: conversation_id.to_owned(),
                    text: text.to_owned(),
                    bot: member.clone(),
                    source: "team".into(),
                    ask_mode: false,
                    route: Some(route),
                    handoff: None,
                    team: Some(frame.clone()),
                    mentions: Vec::new(),
                    attachments: Vec::new(),
                })?)
            })();
            let payload = match payload {
                Ok(payload) => payload,
                Err(error) => {
                    warn!(bot = %member, %error, "room team member turn was not queued");
                    continue;
                }
            };
            // The team ask is audited before the member turn is queued: a failed
            // audit must never leave an untracked team message behind.
            let child = new_id();
            let details = json!({
                "root_task_id": root_task_id,
                "conversation_id": conversation_id,
                "from_bot": ORCHESTRATOR_BOT,
                "to_bot": member,
                "child_task_id": child,
                "round": 0,
                "mode": TeamMode::All.as_str(),
            });
            if let Err(error) = self
                .store
                .record_audit(
                    user_id,
                    AUDIT_ASKED,
                    None,
                    Some(root_task_id),
                    None,
                    None,
                    Some(self.gateway.policy().version()),
                    Some(&details),
                )
                .and_then(|_| self.store.append_event(user_id, AUDIT_ASKED, &details))
            {
                warn!(bot = %member, %error, "room team ask was not audited; member turn was not queued");
                continue;
            }
            if let Err(error) =
                enqueue_turn_task_with_id(&self.store, user_id, &child, member, &payload)
            {
                warn!(bot = %member, %error, "audited room team ask was not queued");
            }
        }
    }

    /// Apply the acting user's approval decision and queue the resume turn.
    /// The decision resolves within the acting user's rows (see
    /// [`Gateway::decide`]); the resume task is enqueued under the same
    /// user.
    pub async fn handle_approval(
        &self,
        user_id: &str,
        approval_id: &str,
        decision: Decision,
    ) -> Result<DecisionResult> {
        let result = self.gateway.decide(user_id, approval_id, decision)?;
        let Some(turn_id) = result.action.turn_id.clone() else {
            return Ok(result);
        };
        let turn = self.store.get_turn(user_id, &turn_id)?;
        let decision_label = match decision {
            Decision::Deny => "denied",
            Decision::Once | Decision::Always => "approved",
        };
        let payload = ResumePayload {
            turn_id: turn.id.clone(),
            action_id: result.action.id.clone(),
            decision: decision_label.into(),
        };
        // A resume continues the turn that already consumed a quota slot at
        // submit time; it is not a new user request and is not re-counted.
        self.store.enqueue_task(
            user_id,
            "resume",
            &turn.bot,
            &serde_json::to_value(payload)?,
            10,
            Utc::now(),
        )?;
        if let Some(task_id) = result.action.task_id.as_deref() {
            self.store
                .cancel_task(user_id, task_id, "superseded by approval decision")?;
        }
        self.store.append_event(
            user_id,
            "approval.decided",
            &json!({
                "approval_id": approval_id,
                "decision": decision_label,
                "turn_id": turn_id,
            }),
        )?;
        Ok(result)
    }
}
