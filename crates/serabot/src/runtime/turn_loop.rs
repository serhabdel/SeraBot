//! The durable turn loop: model rounds, tool execution, approvals and replies.

use super::*;

impl Runtime {
    pub(super) async fn run_turn(&self, task: &Task) -> Result<TurnOutcome> {
        let payload: TurnPayload = serde_json::from_value(task.payload.clone())
            .map_err(|error| SeraError::Validation(format!("invalid turn payload: {error}")))?;
        validate_conversation_id(&payload.conversation_id)?;
        // The worker processes the turn under the task's owning user (phase
        // 2): every read and write below is scoped to it.
        let user = task.user_id.clone();
        self.store
            .ensure_conversation_accepts_turns(&user, &payload.conversation_id)?;
        self.checked_route(&user, &payload.route)?;
        let fleet = self.fleet.load(&user)?;
        let bot = fleet
            .get(&payload.bot)
            .ok_or_else(|| SeraError::NotFound(format!("bot {}", payload.bot)))?;
        if !bot.config.enabled {
            return Err(SeraError::Forbidden(format!(
                "bot {} is disabled",
                payload.bot
            )));
        }
        if let Some(existing) = self.store.get_turn_by_task(&user, &task.id)? {
            if existing.status == "sending_response" {
                return Err(SeraError::External(
                    "response delivery has an unknown outcome; not resending automatically".into(),
                ));
            }
            let state: TurnState = serde_json::from_value(existing.state.clone())
                .map_err(|_| SeraError::InvalidState("stored turn state is invalid".into()))?;
            self.checked_route(&user, &state.route)?;
            if state.route != payload.route {
                return Err(SeraError::Forbidden(
                    "turn route does not match queued route".into(),
                ));
            }
            if let Some(pending) = &state.pending {
                let action = self.store.get_action(&user, &pending.action_id)?;
                if action.status == "outcome_unknown" {
                    return Err(SeraError::External(
                        "a previous external action has an unknown outcome; reconcile it before resuming".into(),
                    ));
                }
            }
            return self
                .advance(task, &existing, state, &payload.conversation_id)
                .await;
        }

        let history = if payload.handoff.is_some() {
            Vec::new()
        } else {
            self.store.history_for_llm(
                &user,
                &payload.conversation_id,
                self.config.runtime.history_limit,
            )?
        };
        let history_messages = history.len();
        let turn = self.store.create_turn(
            &user,
            &task.id,
            &payload.bot,
            &payload.conversation_id,
            &json!({}),
        )?;
        // A handoff or team turn reposts an objective the room already holds as
        // the owner's message; only an owner turn appends it again.
        if payload.handoff.is_none() && payload.team.is_none() {
            self.store.append_message(
                &user,
                &payload.conversation_id,
                Some(&turn.id),
                Role::User,
                &payload.text,
            )?;
        }
        let mut state = TurnState {
            messages: Vec::new(),
            tool_calls: 0,
            pending: None,
            evidence: Vec::new(),
            ask_mode: payload.ask_mode,
            route: payload.route.clone(),
            handoff_queued: false,
            handoff_to_bot: None,
            handoff: payload.handoff.clone(),
            team: payload.team.clone(),
            team_asked: false,
            history_messages,
        };
        // Recalled facts answer the owner's message (or the objective of a
        // handoff or team turn). Only owner turns also get the conversation's
        // rolling summary.
        let owner_turn = payload.handoff.is_none() && payload.team.is_none();
        let memory_block = self
            .memory_context(
                &user,
                owner_turn.then_some(payload.conversation_id.as_str()),
                bot,
                &payload.text,
            )
            .await;
        let system = if let Some(ref handoff) = payload.handoff {
            self.handoff_system_prompt(&user, bot, &payload.text, handoff, &memory_block)?
        } else if let Some(team) = &payload.team {
            self.team_system_prompt(
                &user,
                &payload.conversation_id,
                bot,
                &payload.text,
                team,
                &memory_block,
            )?
        } else {
            self.system_prompt(
                &user,
                &payload.conversation_id,
                bot,
                payload.ask_mode,
                &payload.mentions,
                &memory_block,
            )
        };
        state.messages.push(LlmMessage::system(system));
        state.messages.extend(history);
        let mut model_text = payload.text.clone();
        if !payload.attachments.is_empty() {
            let attachments = self.store.pending_chat_attachments(
                &user,
                &payload.conversation_id,
                &payload.attachments,
            )?;
            self.store
                .attach_chat_attachments_to_turn(&user, &turn.id, &payload.attachments)?;
            let block = crate::attachments::render_for_model(&self.config, &user, &attachments);
            // The block carries file content: the same blocklist as every model request.
            if !block.is_empty() && self.scan_owner_output(&user, &block).is_ok() {
                model_text.push_str(&block);
            }
        }
        state.messages.push(LlmMessage::user(model_text));
        self.store
            .update_turn(&user, &turn.id, "active", &serde_json::to_value(&state)?)?;
        self.advance(task, &turn, state, &payload.conversation_id)
            .await
    }

    pub(super) async fn resume_turn(&self, task: &Task) -> Result<TurnOutcome> {
        let payload: ResumePayload = serde_json::from_value(task.payload.clone())
            .map_err(|error| SeraError::Validation(format!("invalid resume payload: {error}")))?;
        // The resume task is enqueued under the approval owner's user (see
        // `handle_approval`), so the whole resume runs in that scope.
        let user = task.user_id.clone();
        let turn = self.store.get_turn(&user, &payload.turn_id)?;
        let mut state: TurnState = serde_json::from_value(turn.state.clone())
            .map_err(|_| SeraError::InvalidState("stored turn state is invalid".into()))?;
        self.checked_route(&user, &state.route)?;
        let fleet = self.fleet.load(&user)?;
        let bot = fleet
            .get(&turn.bot)
            .ok_or_else(|| SeraError::NotFound(format!("bot {}", turn.bot)))?;
        let pending = state
            .pending
            .clone()
            .ok_or_else(|| SeraError::InvalidState("turn has no pending approval".into()))?;
        let context = self.tool_context(task, &turn, bot, &state)?;
        if payload.decision == "approved" {
            match self
                .gateway
                .execute_approved(&payload.action_id, &context)
                .await
            {
                Ok(ToolOutcome::Completed(output)) => {
                    self.record_tool_result(&mut state, &pending.call, &output);
                    self.emit_step(
                        &user,
                        task,
                        &turn,
                        &pending.call,
                        StepPhase::Done,
                        None,
                        state.tool_calls,
                    );
                }
                Ok(ToolOutcome::PendingApproval { .. }) => {
                    return Err(SeraError::Internal(
                        "approved action unexpectedly requested another approval".into(),
                    ));
                }
                Err(error) => {
                    self.emit_step(
                        &user,
                        task,
                        &turn,
                        &pending.call,
                        StepPhase::Failed,
                        None,
                        state.tool_calls,
                    );
                    state.messages.push(LlmMessage {
                        role: Role::Tool,
                        content: Some(format!("The approved tool failed: {error}")),
                        tool_calls: Vec::new(),
                        tool_call_id: Some(pending.call.id.clone()),
                        name: Some(pending.call.name.clone()),
                    });
                }
            }
        } else {
            self.emit_step(
                &user,
                task,
                &turn,
                &pending.call,
                StepPhase::Failed,
                None,
                state.tool_calls,
            );
            state.messages.push(LlmMessage {
                role: Role::Tool,
                content: Some("The owner denied this action. Do not retry it.".into()),
                tool_calls: Vec::new(),
                tool_call_id: Some(pending.call.id.clone()),
                name: Some(pending.call.name.clone()),
            });
        }
        state.pending = None;
        if payload.decision != "approved" {
            for call in &pending.remaining {
                state.messages.push(LlmMessage {
                    role: Role::Tool,
                    content: Some("Not executed because the owner denied the batch.".into()),
                    tool_calls: Vec::new(),
                    tool_call_id: Some(call.id.clone()),
                    name: Some(call.name.clone()),
                });
            }
        } else if !pending.remaining.is_empty()
            && let Some(next) = self
                .execute_calls(task, &turn, bot, &mut state, pending.remaining, &context)
                .await?
        {
            return self.suspend_for_approval(task, &turn, state, next).await;
        }
        self.store
            .update_turn(&user, &turn.id, "active", &serde_json::to_value(&state)?)?;
        self.advance(task, &turn, state, &turn.conversation_id)
            .await
    }

    fn record_assistant(
        &self,
        user_id: &str,
        conversation_id: &str,
        turn: &Turn,
        text: &str,
        handoff: bool,
    ) -> Result<()> {
        let name = assistant_speaker(conversation_id, &turn.bot, handoff);
        self.store.append_message_named(
            user_id,
            conversation_id,
            Some(&turn.id),
            Role::Assistant,
            text,
            name,
        )?;
        Ok(())
    }

    /// Deliver the final reply in the conversation's selected mode.
    ///
    /// Voice mode synthesizes speech and sends it as a voice note. Every
    /// failure path falls back to text so the content is never lost: a reply
    /// that is too long for the speech cap, a provider error, or a transport
    /// that cannot carry audio all end with the same text the user would have
    /// received anyway, plus one line saying why.
    async fn deliver_reply(&self, user_id: &str, conversation_id: &str, text: &str) -> Result<()> {
        if self.store.reply_mode(user_id, conversation_id)? != "voice" {
            return self
                .notifier
                .send_text(user_id, conversation_id, text)
                .await;
        }
        match crate::audio::synthesize_speech(&self.config, text).await {
            Ok(audio) => {
                if self
                    .notifier
                    .send_voice(user_id, conversation_id, text, &audio)
                    .await
                    .is_ok()
                {
                    return Ok(());
                }
                self.notifier
                    .send_text(user_id, conversation_id, text)
                    .await
            }
            Err(error) => {
                let note = format!("{text}\n\n_voice reply unavailable: {error}_");
                self.notifier
                    .send_text(user_id, conversation_id, &note)
                    .await
            }
        }
    }

    /// Post one final assistant reply on every finish path, under the task's
    /// owning user. A handoff turn keeps the specialist name in every
    /// conversation and records `handoff.completed`, including when a limit
    /// stops the turn. Other turns are labeled only in rooms (see
    /// `assistant_speaker`).
    async fn finish_reply(
        &self,
        user_id: &str,
        conversation_id: &str,
        task: &Task,
        turn: &Turn,
        state: &TurnState,
        text: &str,
    ) -> Result<()> {
        self.deliver_reply(user_id, conversation_id, text).await?;
        if let Some(handoff) = &state.handoff {
            self.record_assistant(user_id, conversation_id, turn, text, true)?;
            self.store.record_audit(
                user_id,
                "handoff.completed",
                None,
                Some(&task.id),
                None,
                None,
                Some(self.gateway.policy().version()),
                Some(&json!({
                    "handoff_id": handoff.handoff_id,
                    "parent_task_id": handoff.parent_task_id,
                    "specialist": turn.bot,
                })),
            )?;
        } else {
            self.record_assistant(user_id, conversation_id, turn, text, false)?;
        }
        if let Some(team) = &state.team {
            self.record_team_completion(user_id, task, turn, team)?;
        }
        Ok(())
    }

    /// `team.round_completed` closes one member contribution; `team.turn_completed`
    /// closes the room turn once no team task of it is still pending.
    fn record_team_completion(
        &self,
        user_id: &str,
        task: &Task,
        turn: &Turn,
        team: &TeamContext,
    ) -> Result<()> {
        let details = json!({
            "root_task_id": team.root_task_id,
            "root_turn_id": team.root_turn_id,
            "bot": turn.bot,
            "from_bot": team.from_bot,
            "round": team.round,
        });
        self.store.record_audit(
            user_id,
            AUDIT_ROUND_COMPLETED,
            None,
            Some(&task.id),
            None,
            None,
            Some(self.gateway.policy().version()),
            Some(&details),
        )?;
        self.store
            .append_event(user_id, AUDIT_ROUND_COMPLETED, &details)?;
        if self
            .store
            .count_pending_team_tasks(user_id, &team.root_task_id)?
            <= 1
        {
            let closing = json!({
                "root_task_id": team.root_task_id,
                "bot": turn.bot,
                "round": team.round,
            });
            self.store.record_audit(
                user_id,
                AUDIT_TURN_COMPLETED,
                None,
                Some(&task.id),
                None,
                None,
                Some(self.gateway.policy().version()),
                Some(&closing),
            )?;
            self.store
                .append_event(user_id, AUDIT_TURN_COMPLETED, &closing)?;
        }
        Ok(())
    }

    /// End a turn the owner stopped. The reply is one short line; no further
    /// model or tool step runs. Tool calls that already finished keep their
    /// results, and nothing that had an external effect is retried.
    async fn finish_stopped(
        &self,
        task: &Task,
        turn: &Turn,
        state: &TurnState,
        conversation_id: &str,
    ) -> Result<TurnOutcome> {
        let user = task.user_id.clone();
        self.clear_stop(&turn.id);
        let text = "Stopped.";
        self.notifier
            .send_text(&user, conversation_id, text)
            .await?;
        self.record_assistant(&user, conversation_id, turn, text, false)?;
        self.store
            .update_turn(&user, &turn.id, "stopped", &serde_json::to_value(state)?)?;
        self.store.record_audit(
            &user,
            "turn.stopped",
            None,
            Some(&task.id),
            None,
            None,
            Some(self.gateway.policy().version()),
            Some(&json!({"turn_id": turn.id})),
        )?;
        self.store.append_event(
            &user,
            "turn.completed",
            &json!({"turn_id": turn.id, "task_id": task.id, "stopped": true}),
        )?;
        Ok(TurnOutcome::Completed)
    }

    async fn advance(
        &self,
        task: &Task,
        turn: &Turn,
        mut state: TurnState,
        conversation_id: &str,
    ) -> Result<TurnOutcome> {
        let bot = {
            let fleet = self.fleet.load(&task.user_id)?;
            fleet
                .get(&turn.bot)
                .cloned()
                .ok_or_else(|| SeraError::NotFound(format!("bot {}", turn.bot)))?
        };
        let user = task.user_id.clone();
        let context = self.tool_context(task, turn, &bot, &state)?;
        let mut tool_names = bot.config.tools.clone();
        for extra in [TEAM_ASK_TOOL, TEAM_REPLY_TOOL, crate::plans::PLAN_TOOL] {
            if context.allowed_tools.contains(extra) && !tool_names.iter().any(|name| name == extra)
            {
                tool_names.push(extra.to_string());
            }
        }
        let unregistered: Vec<&str> = tool_names
            .iter()
            .map(String::as_str)
            .filter(|name| !self.gateway.registry().is_registered(name))
            .collect();
        // A known tool whose connector is simply not connected is expected
        // (the default bots list every optional connector) and is skipped
        // quietly. Only a name the server has never heard of is worth a
        // warning: that is a typo or a stale allowlist.
        let known = crate::tools::known_tool_names();
        let unknown: Vec<&str> = unregistered
            .iter()
            .copied()
            .filter(|name| !known.contains(name))
            .collect();
        if !unknown.is_empty() {
            warn!(
                bot = %turn.bot,
                tools = ?unknown,
                "bot lists tools the server does not know; they are absent from this turn's tool notice"
            );
        } else if !unregistered.is_empty() {
            debug!(
                bot = %turn.bot,
                tools = ?unregistered,
                "bot lists tools whose connector is not connected; skipped"
            );
        }
        let tool_specs = self
            .gateway
            .registry()
            .tools_for(&tool_names)
            .into_iter()
            .filter(|spec| {
                if spec.name == DELEGATE_TOOL && bot.config.role != BotRole::Orchestrator {
                    return false;
                }
                context.allowed_tools.contains(&spec.name)
            })
            .collect::<Vec<_>>();
        upsert_tool_notice(&mut state.messages, &tool_specs);
        let temperature = bot.config.temperature;
        self.checked_route(&user, &state.route)?;

        let tool_round_cap = self
            .config
            .runtime
            .max_tool_iterations
            .max(self.config.llm.max_tool_rounds)
            .max(1);
        for _ in 0..tool_round_cap {
            if self.shutdown.is_cancelled() {
                return Err(SeraError::Internal("shutting down".into()));
            }
            if self.stop_requested(&turn.id) {
                return self
                    .finish_stopped(task, turn, &state, conversation_id)
                    .await;
            }
            let route = self.checked_route(&user, &state.route)?;
            self.scan_model_messages(&user, &state.messages)?;
            self.gateway
                .scan_egress(&user, &serde_json::to_string(&tool_specs)?)?;
            // Vault values never reach a model: redact a copy of the request.
            let outgoing = self.gateway.redact_messages(&user, &state.messages)?;
            let thinking = self.turn_thinking(conversation_id, &bot);
            let response = self
                .router
                .complete_with_thinking(
                    &user,
                    route,
                    &outgoing,
                    &tool_specs,
                    temperature,
                    thinking.as_deref(),
                )
                .await?;
            if let Some(reasoning) = response.reasoning.as_deref() {
                self.emit_thinking(&user, task, &turn, reasoning);
            }
            self.store.record_audit(
                &user,
                "model.used",
                None,
                Some(&task.id),
                None,
                None,
                Some(self.gateway.policy().version()),
                Some(&json!({
                    "provider": route.selection.provider, "model": route.selection.model,
                    "usage": response.usage,
                    "history_messages": state.history_messages,
                    "prompt_chars_est": estimate_prompt_chars(&state.messages),
                })),
            )?;
            let assistant = response.message;
            // A stop that arrived during the model call discards its answer.
            if self.stop_requested(&turn.id) {
                return self
                    .finish_stopped(task, turn, &state, conversation_id)
                    .await;
            }
            if assistant.tool_calls.is_empty() {
                let mut text = assistant.content.unwrap_or_default();
                if text.trim().is_empty() {
                    text = "I could not produce a response.".into();
                }
                if state.ask_mode {
                    let report = validate_citations(&text, &state.evidence);
                    if !report.valid {
                        self.store.record_audit(
                            &user,
                            "ask.citations_rejected",
                            None,
                            Some(&task.id),
                            None,
                            None,
                            Some(self.gateway.policy().version()),
                            Some(&json!({
                                "invalid_count": report.invalid.len(),
                                "had_citations": report.had_citations,
                            })),
                        )?;
                        text = ABSTENTION.to_string();
                    }
                }
                if self.gateway.scan_egress(&user, &text).is_err() {
                    text =
                        "I cannot share that output because it violates the content policy.".into();
                }
                self.store.update_turn(
                    &user,
                    &turn.id,
                    "sending_response",
                    &serde_json::to_value(&state)?,
                )?;
                self.finish_reply(&user, conversation_id, task, turn, &state, &text)
                    .await?;
                self.store.update_turn(
                    &user,
                    &turn.id,
                    "completed",
                    &serde_json::to_value(&state)?,
                )?;
                self.store.append_event(
                    &user,
                    "turn.completed",
                    &json!({"turn_id": turn.id, "task_id": task.id}),
                )?;
                // Owner turns only: extraction and summaries run after the reply
                // is stored, in the background, and never fail the turn.
                if state.handoff.is_none()
                    && state.team.is_none()
                    && !state.ask_mode
                    && let Some(owner) = extract::owner_text(&state.messages)
                {
                    let untrusted = extract::turn_used_untrusted_source(&state.messages);
                    self.spawn_post_turn_memory(
                        &user,
                        &turn.bot,
                        conversation_id,
                        owner,
                        &text,
                        untrusted,
                    );
                }
                return Ok(TurnOutcome::Completed);
            }

            let tool_calls = assistant.tool_calls.clone();
            let requested = state.tool_calls + tool_calls.len();
            if requested > self.config.runtime.max_tool_calls_per_turn {
                let text = "I stopped because this turn exceeded the tool-call limit.".to_string();
                self.finish_reply(&user, conversation_id, task, turn, &state, &text)
                    .await?;
                self.store.update_turn(
                    &user,
                    &turn.id,
                    "stopped",
                    &serde_json::to_value(&state)?,
                )?;
                return Ok(TurnOutcome::Completed);
            }
            state
                .messages
                .push(LlmMessage::assistant(assistant.content, tool_calls.clone()));
            if let Some(pending) = self
                .execute_calls(task, turn, &bot, &mut state, tool_calls, &context)
                .await?
            {
                return self.suspend_for_approval(task, turn, state, pending).await;
            }
            if state.handoff_queued {
                let label = state.handoff_to_bot.as_deref().unwrap_or("specialist");
                let text = format!("Queued handoff to {label}.");
                self.notifier
                    .send_text(&user, conversation_id, &text)
                    .await?;
                self.record_assistant(&user, conversation_id, turn, &text, false)?;
                self.store.update_turn(
                    &user,
                    &turn.id,
                    "completed",
                    &serde_json::to_value(&state)?,
                )?;
                self.store.append_event(
                    &user,
                    "turn.completed",
                    &json!({"turn_id": turn.id, "task_id": task.id, "handoff_queued": true}),
                )?;
                return Ok(TurnOutcome::Completed);
            }
            if state.team_asked {
                let text = "Asked the room team to answer.".to_string();
                self.notifier
                    .send_text(&user, conversation_id, &text)
                    .await?;
                self.record_assistant(&user, conversation_id, turn, &text, false)?;
                self.store.update_turn(
                    &user,
                    &turn.id,
                    "completed",
                    &serde_json::to_value(&state)?,
                )?;
                self.store.append_event(
                    &user,
                    "turn.completed",
                    &json!({"turn_id": turn.id, "task_id": task.id, "team_asked": true}),
                )?;
                return Ok(TurnOutcome::Completed);
            }
            self.store
                .update_turn(&user, &turn.id, "active", &serde_json::to_value(&state)?)?;
        }
        let text = "I stopped because the turn reached its tool-iteration limit.".to_string();
        self.finish_reply(&user, conversation_id, task, turn, &state, &text)
            .await?;
        self.store
            .update_turn(&user, &turn.id, "stopped", &serde_json::to_value(&state)?)?;
        Ok(TurnOutcome::Completed)
    }

    async fn execute_calls(
        &self,
        task: &Task,
        turn: &Turn,
        bot: &LoadedBot,
        state: &mut TurnState,
        calls: Vec<ToolCall>,
        context: &ToolContext,
    ) -> Result<Option<PendingTool>> {
        for (index, call) in calls.iter().enumerate() {
            if state.tool_calls >= self.config.runtime.max_tool_calls_per_turn {
                break;
            }
            // Calls not yet started are dropped: the owner stopped the turn.
            if self.stop_requested(&turn.id) {
                break;
            }
            if call.name == DELEGATE_TOOL {
                if context.bot != ORCHESTRATOR_BOT {
                    return Err(SeraError::Forbidden(
                        "only the orchestrator may delegate".into(),
                    ));
                }
                if state.handoff_queued {
                    return Err(SeraError::Validation(
                        "only one handoff is allowed per turn".into(),
                    ));
                }
            }
            if call.name == TEAM_ASK_TOOL {
                if context.bot != ORCHESTRATOR_BOT {
                    return Err(SeraError::Forbidden(
                        "only the orchestrator may ask the team".into(),
                    ));
                }
                if state.team_asked {
                    return Err(SeraError::Validation(
                        "only one team_ask is allowed per turn".into(),
                    ));
                }
            }
            if call.name == TEAM_REPLY_TOOL && state.team.is_none() {
                return Err(SeraError::Forbidden(
                    "team_reply is only available to bots taking part in a team turn".into(),
                ));
            }
            state.tool_calls += 1;
            let ordinal = state.tool_calls;
            self.emit_step(
                &task.user_id,
                task,
                turn,
                call,
                StepPhase::Running,
                None,
                ordinal,
            );
            match self
                .gateway
                .execute(&call.name, call.arguments.clone(), context)
                .await
            {
                Ok(ToolOutcome::Completed(output)) => {
                    if call.name == DELEGATE_TOOL
                        && output
                            .data
                            .as_ref()
                            .and_then(|data| data.get(HANDOFF_QUEUED_KEY))
                            .and_then(Value::as_bool)
                            == Some(true)
                    {
                        state.handoff_queued = true;
                        state.handoff_to_bot = output
                            .data
                            .as_ref()
                            .and_then(|data| data.get("to_bot"))
                            .and_then(Value::as_str)
                            .map(str::to_owned);
                    }
                    if call.name == TEAM_ASK_TOOL
                        && output
                            .data
                            .as_ref()
                            .and_then(|data| data.get(TEAM_ASK_QUEUED_KEY))
                            .and_then(Value::as_bool)
                            == Some(true)
                    {
                        state.team_asked = true;
                    }
                    self.record_tool_result(state, call, &output);
                    self.emit_step(
                        &task.user_id,
                        task,
                        turn,
                        call,
                        StepPhase::Done,
                        None,
                        ordinal,
                    );
                }
                Ok(ToolOutcome::PendingApproval {
                    approval_id,
                    action_id,
                    ..
                }) => {
                    self.emit_step(
                        &task.user_id,
                        task,
                        turn,
                        call,
                        StepPhase::WaitingApproval,
                        Some(&approval_id),
                        ordinal,
                    );
                    return Ok(Some(PendingTool {
                        call: call.clone(),
                        remaining: calls[index + 1..].to_vec(),
                        approval_id,
                        action_id,
                    }));
                }
                Err(SeraError::PolicyViolation) => {
                    self.emit_step(
                        &task.user_id,
                        task,
                        turn,
                        call,
                        StepPhase::Failed,
                        None,
                        ordinal,
                    );
                    state.messages.push(LlmMessage {
                        role: Role::Tool,
                        content: Some(
                            "The tool output was blocked by the content policy and cannot be used."
                                .into(),
                        ),
                        tool_calls: Vec::new(),
                        tool_call_id: Some(call.id.clone()),
                        name: Some(call.name.clone()),
                    });
                }
                Err(error) => {
                    self.emit_step(
                        &task.user_id,
                        task,
                        turn,
                        call,
                        StepPhase::Failed,
                        None,
                        ordinal,
                    );
                    state.messages.push(LlmMessage {
                        role: Role::Tool,
                        content: Some(format!(
                            "Tool '{}' failed: {}",
                            call.name,
                            truncate_chars(&error.to_string(), 500)
                        )),
                        tool_calls: Vec::new(),
                        tool_call_id: Some(call.id.clone()),
                        name: Some(call.name.clone()),
                    });
                }
            }
        }
        let _ = bot;
        Ok(None)
    }

    fn record_tool_result(&self, state: &mut TurnState, call: &ToolCall, output: &ToolOutput) {
        if call.name == "rag_search"
            && let Some(chunks) = output
                .data
                .as_ref()
                .and_then(|data| data.get("chunks"))
                .and_then(Value::as_array)
        {
            for chunk in chunks {
                if let Ok(chunk) = serde_json::from_value::<RetrievedChunk>(chunk.clone())
                    && !state
                        .evidence
                        .iter()
                        .any(|existing| existing.chunk_id == chunk.chunk_id)
                {
                    state.evidence.push(chunk);
                }
            }
        }
        state.messages.push(LlmMessage {
            role: Role::Tool,
            content: Some(output.content.clone()),
            tool_calls: Vec::new(),
            tool_call_id: Some(call.id.clone()),
            name: Some(call.name.clone()),
        });
    }

    async fn suspend_for_approval(
        &self,
        task: &Task,
        turn: &Turn,
        mut state: TurnState,
        pending: PendingTool,
    ) -> Result<TurnOutcome> {
        let user = task.user_id.clone();
        let approval = self.store.get_approval(&user, &pending.approval_id)?;
        let action = self.store.get_action(&user, &pending.action_id)?;
        let notice = ApprovalNotice {
            approval: approval.clone(),
            action: action.clone(),
        };
        state.pending = Some(pending);
        self.store.update_turn(
            &user,
            &turn.id,
            "awaiting_approval",
            &serde_json::to_value(&state)?,
        )?;
        self.store
            .await_approval_task(&user, &task.id, task.fencing_token)?;
        self.store.append_event(
            &user,
            "approval.requested",
            &json!({
                "approval_id": approval.id,
                "action_id": action.id,
                "turn_id": turn.id,
            }),
        )?;
        self.notifier
            .approval_requested(&user, &turn.conversation_id, &notice)
            .await?;
        Ok(TurnOutcome::Suspended)
    }
}
