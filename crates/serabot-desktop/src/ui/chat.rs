use super::*;

#[component]
pub(crate) fn ChatView(session: Signal<Session>, overlay: Signal<Overlay>) -> Element {
    let conversation_kind = session.read().conversation_kind().map(str::to_owned);
    let group_like = matches!(conversation_kind.as_deref(), Some("group") | Some("room"));
    let orchestrator = session
        .read()
        .state
        .listed_bots
        .iter()
        .find(|bot| bot.role == "orchestrator")
        .map(|bot| bot.id.clone());
    let today = today_utc_date();
    let idle = session
        .read()
        .state
        .task
        .as_ref()
        .is_none_or(|task| task.status.terminal());
    let awaiting_approval = session
        .read()
        .state
        .task
        .as_ref()
        .is_some_and(|task| task.status == TaskStatus::AwaitingApproval);
    // One prepared row per message: what the transcript draws, the day
    // heading where a new day starts, and which actions it offers.
    let messages: Vec<MsgView> = {
        let state = session.read();
        let last_user = state
            .state
            .messages
            .iter()
            .rposition(|m| m.role == Role::User);
        let last_bot = state
            .state
            .messages
            .iter()
            .rposition(|m| m.role != Role::User);
        let regen_text = last_user.map(|index| state.state.messages[index].content.clone());
        let mut rows: Vec<MsgView> = Vec::with_capacity(state.state.messages.len());
        let mut prev_user: Option<bool> = None;
        let mut prev_day: Option<String> = None;
        for (index, message) in state.state.messages.iter().enumerate() {
            let user = message.role == Role::User;
            let (speaker, avatar, bot) = state.message_speaker(message);
            let peer = !user
                && group_like
                && bot.as_deref().is_some()
                && bot.as_deref() != orchestrator.as_deref();
            let day_now = chat_feed::day_key(&message.created_at, &today);
            let day = match &day_now {
                Some(now) if prev_day.as_deref() != Some(now.as_str()) => {
                    Some(chat_feed::day_heading(now, &today))
                }
                _ => None,
            };
            if day_now.is_some() {
                prev_day = day_now;
            }
            let speaker_change = prev_user != Some(user);
            prev_user = Some(user);
            let mut class = String::from(if user {
                "msg user"
            } else if peer {
                "msg bot peer"
            } else {
                "msg bot"
            });
            if speaker_change {
                class.push_str(" speaker-change");
            }
            rows.push(MsgView {
                id: message.id.clone(),
                user,
                speaker,
                avatar,
                bot,
                stamp: msg_stamp(&message.created_at),
                content: message.content.clone(),
                peer,
                class,
                day,
                edit: idle && Some(index) == last_user,
                regen: if idle && Some(index) == last_bot && !user {
                    regen_text.clone()
                } else {
                    None
                },
                continue_offer: idle
                    && serabot_desktop::media_view::offers_continue(&state.state.messages, index),
            });
        }
        rows
    };
    // Chat cards (0.23). A pending card sits at the foot of the feed. A resolved
    // card keeps its place in the transcript: before the first message made after it.
    let cards: Vec<CardView> = session
        .read()
        .state
        .cards
        .iter()
        .filter_map(CardView::from_card)
        .collect();
    let (pending_cards, resolved_cards): (Vec<CardView>, Vec<CardView>) = cards
        .into_iter()
        .partition(|card| card.state == CardState::Pending);
    let message_times: Vec<String> = session
        .read()
        .state
        .messages
        .iter()
        .map(|message| message.created_at.clone())
        .collect();
    let mut placed: Vec<Vec<CardView>> = vec![Vec::new(); message_times.len() + 1];
    for card in resolved_cards {
        let index = chat_cards::insert_index(&message_times, &card.created_at);
        placed[index].push(card);
    }
    let task = session
        .read()
        .state
        .task
        .as_ref()
        .map(|task| (task.status, task.status.label().to_owned()));
    let live_task: Option<TaskStatus> = session
        .read()
        .state
        .task
        .as_ref()
        .and_then(|task| (!task.status.terminal()).then_some(task.status));
    let (steps, steps_summary, sending) = {
        let state = session.read();
        let sending =
            state.state.messages.last().is_some_and(|message| {
                message.role == Role::User && message.id.starts_with("opt-")
            });
        (
            state.state.activity.steps.clone(),
            state.state.activity.summary(),
            sending,
        )
    };
    let chat_plan = session.read().state.plan.clone();
    let mut composer_draft = use_signal(|| session.read().composer.clone());
    let can_send = session.read().state.can_send();
    let accepts = session.read().conversation_accepts_turns();
    let (active_bot, active_name, active_avatar, roster) = {
        let state = session.read();
        let active = state
            .conversation
            .strip_prefix("dir:")
            .map(str::to_owned)
            .or_else(|| {
                state
                    .state
                    .listed_bots
                    .iter()
                    .find(|bot| bot.role == "orchestrator")
                    .map(|bot| bot.id.clone())
            });
        let name = active
            .as_deref()
            .map(|id| state.bot_display_name(id))
            .unwrap_or_else(|| "Assistant".into());
        let avatar = active
            .as_deref()
            .map(|id| state.bot_avatar(id))
            .unwrap_or_else(|| "A".into());
        let roster = state
            .state
            .listed_bots
            .iter()
            .map(|bot| {
                (
                    bot.id.clone(),
                    state.bot_display_name(&bot.id),
                    state.bot_avatar(&bot.id),
                    bot.enabled,
                    bot.live,
                    bot.role.clone(),
                )
            })
            .collect::<Vec<RosterRow>>();
        (active, name, avatar, roster)
    };
    // "Main is working", plus the step that is running right now.
    let running_step = steps
        .iter()
        .find(|step| step.phase == StepPhase::Running)
        .map(|step| step.label.clone());
    let working = chat_feed::working_line(&active_name, running_step.as_deref());
    let bot_meta: HashMap<String, (String, bool)> = roster
        .iter()
        .map(|(id, _, _, _, live, role)| (id.clone(), (role.clone(), *live)))
        .collect();
    let talking_to: Vec<(String, String, String)> = {
        let state = session.read();
        let member_ids = state
            .state
            .team
            .as_ref()
            .map(teams::member_ids)
            .unwrap_or_default();
        let ids = if member_ids.is_empty() {
            roster
                .iter()
                .filter(|(_, _, _, enabled, _, _)| *enabled)
                .map(|(id, _, _, _, _, _)| id.clone())
                .take(5)
                .collect::<Vec<_>>()
        } else {
            member_ids
        };
        ids.into_iter()
            .map(|id| {
                (
                    id.clone(),
                    state.bot_display_name(&id),
                    state.bot_avatar(&id),
                )
            })
            .collect::<Vec<_>>()
    };
    let member_acts: HashMap<String, Option<BotActivity>> = {
        let state = session.read();
        let now = Instant::now();
        talking_to
            .iter()
            .map(|(id, _, _)| (id.clone(), state.state.working.activity_of(id, now)))
            .collect()
    };
    // The team row is collapsed by default: routing state changes rarely, so it
    // sits behind one caption chip instead of a permanent row of pills.
    let mut team_collapsed = use_signal(|| true);
    let mut recording = use_signal(|| false);
    // The recorder is owned by the signal but taken out for every capture, so
    // no borrow of the signal is alive across an await point.
    let mut recorder = use_signal(MicRecorder::new);
    // Composer pickers: `Some(query)` while a bare `@query` (mention) or a
    // line-leading `/query` (skill/tool) is being typed.
    let mut mention = use_signal(|| Option::<String>::None);
    let mut mention_idx = use_signal(|| 0usize);
    let mut slash = use_signal(|| Option::<String>::None);
    let mut slash_idx = use_signal(|| 0usize);
    // Message list and composer state (0.19).
    let mut at_bottom = use_signal(|| true);
    let mut unseen = use_signal(|| 0usize);
    let mut last_count = use_signal(|| 0usize);
    // The conversation the scroll state was last reset for, and the last
    // transcript fingerprint. `use_effect` closures that read `session` re-run
    // on EVERY session write, so resets and growth checks compare against these.
    let mut seen_conversation = use_signal(|| session.peek().conversation.clone());
    let last_mark = use_hook(|| Rc::new(RefCell::new(chat_scroll::TranscriptMark::default())));
    let last_mark_reset = Rc::clone(&last_mark);
    let last_mark_effect = Rc::clone(&last_mark);
    // Ids that already had their entrance animation in this conversation.
    let entered_messages =
        use_hook(|| Rc::new(RefCell::new((String::new(), HashSet::<String>::new()))));
    let entered_messages_effect = Rc::clone(&entered_messages);
    let mut dragging = use_signal(|| false);
    // The composer grows to about 40% of the window; Expand lifts that to 70%.
    let mut composer_expanded = use_signal(|| false);
    let mut composer_focused = use_signal(|| false);
    let mut copied = use_signal(|| Option::<String>::None);
    let mut composer_room = use_signal(|| session.read().conversation.clone());
    use_effect(move || {
        let conversation = session.read().conversation.clone();
        if composer_room() == conversation {
            return;
        }
        composer_room.set(conversation);
        composer_draft.set(session.read().composer.clone());
    });
    use_effect(move || {
        let external = session.read().composer.clone();
        if chat_compose::sync_composer_from_session(composer_focused(), external.is_empty()) {
            composer_draft.set(external);
        }
    });
    // Files pasted into the composer arrive through the JS bridge.
    use_future(move || async move {
        let mut bridge = dioxus::document::eval(PASTE_BRIDGE_JS);
        while let Ok(pasted) = bridge.recv::<PastedFile>().await {
            session.write().queue_files(vec![decode_pasted(pasted)]);
        }
    });
    use_future(move || async move {
        let _ = dioxus::document::eval(chat_scroll::CHAT_SCROLL_BRIDGE_JS);
        let _ = dioxus::document::eval(COMPOSER_CARET_SETUP_JS);
        // Follow state comes from the page, only when it changes. There is no
        // per-scroll-event round trip and no scroll-position maths in Rust.
        let mut follow = dioxus::document::eval(chat_scroll::FOLLOW_SINK_JS);
        while let Ok(following) = follow.recv::<bool>().await {
            if *at_bottom.peek() != following {
                at_bottom.set(following);
            }
            if following && *unseen.peek() != 0 {
                unseen.set(0);
            }
        }
    });
    let transcript_error = session.read().state.transcript_error.clone();
    use_effect(move || {
        let transcript = session.read().state.transcript.clone();
        let Some(text) = transcript else {
            return;
        };
        let mut session = session;
        let mut composer = session.write();
        composer.state.transcript = None;
        let next = if composer.composer.trim().is_empty() {
            text
        } else {
            format!("{}\n{}", composer.composer, text)
        };
        composer.composer = next.clone();
        composer_draft.set(next);
    });
    use_effect(move || {
        let conversation = session.read().conversation.clone();
        // Reset only when the conversation really changed. Running this on
        // every session write replayed every entrance animation and forced a
        // jump to the bottom, which read as glitching and endless scrolling.
        if !chat_scroll::conversation_switched(&conversation, &seen_conversation.peek()) {
            return;
        }
        seen_conversation.set(conversation.clone());
        {
            let mut guard = entered_messages_effect.borrow_mut();
            guard.0 = conversation;
            guard.1.clear();
        }
        *last_mark_reset.borrow_mut() = chat_scroll::TranscriptMark::default();
        last_count.set(0);
        at_bottom.set(true);
        unseen.set(0);
    });
    use_effect(move || {
        let count = session.read().state.messages.len();
        let own_send =
            session.read().state.messages.last().is_some_and(|message| {
                message.role == Role::User && message.id.starts_with("opt-")
            });
        let tail = session
            .read()
            .state
            .messages
            .last()
            .map(|message| (message.id.clone(), message.content.len()));
        let activity_steps = session.read().state.activity.steps.len();
        let activity_tail = session
            .read()
            .state
            .activity
            .steps
            .last()
            .map(|step| (step.call_id.clone(), step.label.len()));
        let mark = chat_scroll::TranscriptMark {
            count,
            tail,
            steps: activity_steps,
            step_tail: activity_tail,
        };
        // Real growth only: this effect also runs for unrelated session writes.
        let streaming_growth = {
            let mut last = last_mark_effect.borrow_mut();
            let grew = *last != mark;
            *last = mark;
            grew
        };
        let previous = *last_count.peek();
        if count != previous {
            if count > previous && !*at_bottom.peek() && !own_send {
                let added = *unseen.peek() + (count - previous);
                unseen.set(added);
            }
            last_count.set(count);
        }
        if own_send {
            if !*at_bottom.peek() {
                at_bottom.set(true);
            }
            if *unseen.peek() != 0 {
                unseen.set(0);
            }
        }
        if let Some(force_pin) =
            chat_scroll::transcript_pin_action(previous, count, own_send, streaming_growth)
        {
            let _ = dioxus::document::eval(&chat_scroll::pin_messages_js(force_pin));
        }
    });
    {
        let conversation = session.read().conversation.clone();
        let mut guard = entered_messages.borrow_mut();
        if guard.0 != conversation {
            guard.0 = conversation;
            guard.1.clear();
        }
    }
    let empty = messages.is_empty();
    let is_one_to_one = session.read().conversation.starts_with("dir:");
    let room = teams::is_room(&session.read().conversation);
    let in_team: Vec<String> = session
        .read()
        .state
        .team
        .as_ref()
        .map(teams::member_ids)
        .unwrap_or_default();
    // Picker contents for the open pop (empty when no pop is open). The slash
    // pop wins over the mention pop when both triggers are present.
    let show_mention = mention().is_some() && slash().is_none();
    let show_slash = slash().is_some();
    let mention_q = mention().unwrap_or_default();
    let slash_q = slash().unwrap_or_default();
    let mention_bots = if show_mention {
        mention_bot_matches(&session.read(), &mention_q)
    } else {
        Vec::new()
    };
    let mention_conns = if show_mention {
        mention_connector_matches(&session.read(), &mention_q)
    } else {
        Vec::new()
    };
    let mention_total = mention_bots.len() + mention_conns.len();
    let mention_active = mention_idx().min(mention_total.saturating_sub(1));
    let slash_skills = if show_slash {
        slash_skill_matches(&session.read(), &slash_q)
    } else {
        Vec::new()
    };
    let slash_tools = if show_slash {
        slash_tool_matches(&slash_q)
    } else {
        Vec::new()
    };
    let slash_actions = if show_slash {
        chat_compose::slash_actions(&slash_q)
    } else {
        Vec::new()
    };
    let slash_total = slash_skills.len() + slash_tools.len() + slash_actions.len();
    let slash_active = slash_idx().min(slash_total.saturating_sub(1));
    // Copies for the key handler closures (they move their captures).
    let key_mention_bots = mention_bots.clone();
    let key_mention_conns = mention_conns.clone();
    let key_slash_skills = slash_skills.clone();
    let key_slash_tools = slash_tool_matches(&slash_q);
    let composer_text = composer_draft();
    let composer_chars = composer_text.chars().count();
    let length_note = chat_compose::length_message(composer_chars);
    let over_limit = composer_chars > chat_compose::MAX_TEXT_CHARS;
    let composer_file_count = session.read().composer_files.len();
    let busy = session.read().uploading || session.read().upload_pending() > 0;
    let send_ready = chat_compose::can_submit(&composer_text, composer_file_count, busy);
    let run_active = live_task.is_some();
    rsx! {
        div { class: "pane-body chat-body",
            ondragover: move |event: DragEvent| {
                event.prevent_default();
                dragging.set(true);
            },
            ondragleave: move |_| dragging.set(false),
            ondrop: move |event: DragEvent| async move {
                event.prevent_default();
                dragging.set(false);
                // Admit by name and size first: a refused file is never read.
                let dropped = event.data_transfer().files();
                let meta: Vec<(String, u64)> = dropped.iter().map(|file| (file.name(), file.size())).collect();
                let sizes: Vec<(&str, u64)> = meta.iter().map(|(name, size)| (name.as_str(), *size)).collect();
                let held = session.read().attachments_held();
                let admission = chat_compose::admit_files(held, &sizes);
                let mut picked = Vec::new();
                for (index, file) in dropped.iter().enumerate() {
                    if admission.accepted.contains(&index) {
                        let bytes = file.read_bytes().await.map(|data| data.to_vec()).unwrap_or_default();
                        picked.push((file.name(), bytes));
                    }
                }
                session.write().queue_files(picked);
                session.write().refuse_files(admission.refused);
            },
            if dragging() {
                div { class: "drop-zone", "aria-hidden": "true",
                    div { class: "drop-zone-card",
                        span { class: "drop-zone-title", "Drop files to attach them" }
                        span { class: "drop-zone-meta", "Up to 6 files, 25 MB in total" }
                    }
                }
            }
            div { class: "chat-column",
                if let Some((status, label)) = task {
                    if status == TaskStatus::AwaitingApproval {
                        div { class: "banner warn",
                            span { "{label}" }
                            button {
                                class: "btn ghost sm",
                                onclick: move |_| session.write().open_section(Section::Approvals),
                                "Review approvals"
                            }
                        }
                    }
                }
                if let Some(plan) = chat_plan.clone() {
                    PlanPanel { plan }
                }
                // The Jobs/Activity panel (0.28.0): running/queued turns,
                // media jobs and handoffs, collapsed to one line when idle.
                crate::ui_jobs::JobsPanel { session }
                div {
                    class: if group_like { "messages group" } else { "messages" },
                    id: "messages",
                    // A log landmark for screen readers. Live announcements are
                    // off: announcing every streamed token would be noise.
                    role: "log",
                    "aria-label": "Conversation",
                    "aria-live": "off",
                    if empty {
                        div { class: "empty",
                            div {
                                class: "empty-icon",
                                style: active_bot.as_deref().map(avatar_style).unwrap_or_default(),
                                span { class: "avatar lg bot", style: active_bot.as_deref().map(avatar_style).unwrap_or_default(), "{active_avatar}" }
                            }
                            h2 { class: "empty-title",
                                if !accepts {
                                    "This node is read-only."
                                } else if is_one_to_one {
                                    "Chat with {active_name}"
                                } else {
                                    "Start a focused turn."
                                }
                            }
                            p { class: "empty-text",
                                if !accepts {
                                    "Sections and folders organize the rail. Open a chat or a group to talk."
                                } else if is_one_to_one {
                                    "A one-to-one conversation. Replies come from {active_name}, not the orchestrator."
                                } else {
                                    "Ask the orchestrator, or open a group with your specialists."
                                }
                            }
                            if accepts {
                                div { class: "suggestions", role: "group", "aria-label": "Starter prompts",
                                    for text in chat_feed::suggestions(active_bot.as_deref()).iter().copied() {
                                        button {
                                            class: "suggestion",
                                            onclick: move |_| {
                                                let text = text.to_owned();
                                                session.write().composer = text.clone();
                                                composer_draft.set(text);
                                                focus_composer();
                                            },
                                            "{text}"
                                        }
                                    }
                                }
                            }
                            div { class: "roster",
                                for (bot_id, bot_name, bot_avatar, bot_enabled, bot_live, bot_role) in roster.clone() {
                                    div {
                                        key: "{bot_id}",
                                        class: "roster-row",
                                        span { class: "avatar bot", style: avatar_style(&bot_id), "{bot_avatar}" }
                                        span { class: "thread-title", "{bot_name}" }
                                        span { class: "thread-meta", "{bot_role}" }
                                        if !bot_enabled {
                                            span { class: "chip warn", "disabled" }
                                        } else if !bot_live {
                                            span { class: "chip warn", "restart required" }
                                        }
                                        button {
                                            class: "btn ghost sm",
                                            onclick: {
                                                let bot_id = bot_id.clone();
                                                move |_| session.write().open_bot_chat(bot_id.clone())
                                            },
                                            "1:1"
                                        }
                                        if room {
                                            button {
                                                class: "btn ghost sm",
                                                disabled: !bot_enabled || in_team.contains(&bot_id),
                                                title: if in_team.contains(&bot_id) { "Already in the group" } else { "Add to the group" },
                                                onclick: {
                                                    let bot_id = bot_id.clone();
                                                    move |_| session.write().add_team_member(bot_id.clone())
                                                },
                                                "add to group"
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                    for (index, row) in messages.into_iter().enumerate() {
                        Fragment { key: "{row.id}",
                            for card in placed[index].clone() {
                                ChatCardRow { session, key: "{card.id}", card }
                            }
                            if let Some(heading) = row.day.clone() {
                                div { class: "day-sep", role: "separator",
                                    span { "{heading}" }
                                }
                            }
                            div {
                                class: {
                                    let mut guard = entered_messages.borrow_mut();
                                    let enter = if guard.1.contains(&row.id) {
                                        false
                                    } else {
                                        guard.1.insert(row.id.clone());
                                        true
                                    };
                                    if enter {
                                        format!("{} msg-enter", row.class)
                                    } else {
                                        row.class.clone()
                                    }
                                },
                                span {
                                    class: if row.user { "msg-avatar avatar user" } else { "msg-avatar avatar bot" },
                                    style: row.bot.as_deref().map(avatar_style).unwrap_or_default(),
                                    "{row.avatar}"
                                }
                                div { class: "msg-main",
                                    div { class: "msg-head",
                                        // The own-message label restates what the
                                        // right-aligned bubble already says.
                                        if !row.user {
                                            span { class: "msg-name", "{row.speaker}" }
                                        }
                                        span { class: "msg-time", "{row.stamp}" }
                                        if let Some(id) = row.bot.as_ref() {
                                            if let Some((role, live)) = bot_meta.get(id) {
                                                span { class: "team-chip", "{role}" }
                                                span {
                                                    class: if *live { "chip ok" } else { "chip warn" },
                                                    if *live { "live" } else { "not live" }
                                                }
                                            }
                                        }
                                        if row.peer {
                                            span { class: "team-chip", "peer bot" }
                                        }
                                    }
                                    div { class: "msg-body", title: "{row.stamp}",
                                        MarkdownBody { message_id: row.id.clone(), content: row.content.clone() }
                                        if row.user {
                                            MessageFiles { session, id: row.id.clone() }
                                        }
                                    }
                                    div { class: "msg-actions", role: "toolbar", "aria-label": "Message actions",
                                        button {
                                            class: "msg-action",
                                            "aria-label": "Copy message",
                                            title: "Copy",
                                            onclick: {
                                                let text = row.content.clone();
                                                let id = row.id.clone();
                                                move |_| {
                                                    let _ = dioxus::document::eval(&copy_script(&text));
                                                    copied.set(Some(id.clone()));
                                                    let id = id.clone();
                                                    spawn(async move {
                                                        let mut done = dioxus::document::eval(CONFIRM_RESET_JS);
                                                        let _ = done.recv::<bool>().await;
                                                        if copied.peek().as_deref() == Some(id.as_str()) {
                                                            copied.set(None);
                                                        }
                                                    });
                                                }
                                            },
                                            Icon { name: "copy", size: 13 }
                                            span { class: "msg-action-label",
                                                if copied().as_deref() == Some(row.id.as_str()) { "Copied" } else { "Copy" }
                                            }
                                        }
                                        if row.edit {
                                            button {
                                                class: "msg-action",
                                                "aria-label": "Edit message",
                                                title: "Put this message back in the composer",
                                                onclick: {
                                                    let text = row.content.clone();
                                                    move |_| {
                                                        composer_draft.set(text.clone());
                                                        session.write().edit_text(text.clone());
                                                    }
                                                },
                                                Icon { name: "edit", size: 13 }
                                                span { class: "msg-action-label", "Edit" }
                                            }
                                        }
                                        if let Some(text) = row.regen.clone() {
                                            button {
                                                class: "msg-action",
                                                "aria-label": "Regenerate reply",
                                                title: "Send your last message again",
                                                onclick: move |_| session.write().regenerate(text.clone()),
                                                Icon { name: "refresh", size: 13 }
                                                span { class: "msg-action-label", "Regenerate" }
                                            }
                                        }
                                        if row.continue_offer {
                                            button {
                                                class: "msg-action",
                                                "aria-label": "Continue from where the bot stopped",
                                                title: "Ask the bot to carry on from its tool limit",
                                                onclick: move |_| session.write().continue_from_limit(),
                                                Icon { name: "refresh", size: 13 }
                                                span { class: "msg-action-label", "Continue" }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                    // Resolved cards that come after the newest message.
                    for card in placed.last().cloned().unwrap_or_default() {
                        ChatCardRow { session, key: "{card.id}", card }
                    }
                    // Files the bots produced in this chat (Open / Save as).
                    MediaCards { session }
                    // Cards waiting for the owner: questions and secret requests.
                    for card in pending_cards.clone() {
                        ChatCardRow { session, key: "{card.id}", card }
                    }
                    // What the bot is doing right now, in plain words: a
                    // shimmer while it thinks, one row per step, and a small
                    // "N steps" summary once the reply has landed.
                    if sending && live_task.is_none() {
                        div { class: "activity", role: "status",
                            span { class: "shimmer", "Sending…" }
                        }
                    }
                    if live_task.is_some() {
                        div { class: "activity", role: "status", "aria-live": "polite",
                            span { class: "working",
                                span { class: "working-text", "{working}" }
                                span { class: "working-dots", "aria-hidden": "true",
                                    span {}
                                    span {}
                                    span {}
                                }
                            }
                            for step in steps.iter().cloned() {
                                ActivityRow { session, step, key: "{step.call_id}" }
                            }
                        }
                    } else if !steps.is_empty() {
                        details { class: "activity done",
                            summary { class: "activity-summary",
                                Icon { name: "chevron-right", size: 12 }
                                "{steps_summary}"
                            }
                            for step in steps.iter().cloned() {
                                ActivityRow { session, step, key: "{step.call_id}" }
                            }
                        }
                    }
                    // Bottom sentinel: the page watches it to know the reader
                    // is at the end of the conversation.
                    div { id: "messages-end", class: "messages-end", "aria-hidden": "true" }
                }
                if !at_bottom() {
                    button {
                        class: "scroll-bottom jump-pill",
                        r#type: "button",
                        "aria-label": if live_task.is_some() { "Jump to the latest message, a reply is still coming in" } else { "Jump to the latest message" },
                        title: "Jump to latest",
                        onclick: move |_| {
                            let _ = dioxus::document::eval(chat_scroll::SCROLL_TO_BOTTOM_JS);
                            at_bottom.set(true);
                            unseen.set(0);
                        },
                        Icon { name: "arrow-down", size: 16 }
                        span { class: "jump-label", "Jump to latest" }
                        if let Some(badge) = chat_feed::unseen_badge(unseen()) {
                            span { class: "scroll-badge", "aria-label": "{badge} unseen messages", "{badge}" }
                        }
                        if live_task.is_some() {
                            span { class: "jump-live", "aria-hidden": "true" }
                        }
                    }
                }
                div { class: "composer-wrap",
                    if let Some(message) = transcript_error {
                        div { class: "banner err", span { "{message}" } }
                    }
                    div { class: if composer_expanded() { "composer expanded" } else { "composer" },
                        ComposerFiles { session }
                        textarea {
                            class: "composer-input",
                            id: "composer-input",
                            rows: 1,
                            "aria-label": "Message",
                            value: "{composer_text}",
                            placeholder: if accepts { format!("Message {active_name}…") } else { "This node does not accept turns".into() },
                            disabled: !accepts,
                            onfocus: move |_| composer_focused.set(true),
                            onblur: move |_| composer_focused.set(false),
                            oninput: move |event| {
                                let value = event.value();
                                composer_draft.set(value.clone());
                                session.write().composer = value.clone();
                                mention.set(mention_query(&value));
                                mention_idx.set(0);
                                slash.set(slash_query(&value));
                                slash_idx.set(0);
                            },
                            onkeydown: move |event: KeyboardEvent| {
                                // `/new`, `/stop` and friends run as actions. Checked
                                // first so an open picker with no match cannot swallow Enter.
                                if event.key() == Key::Enter
                                    && !event.is_composing()
                                    && let Some(action) = chat_compose::parse_slash_action(&composer_draft())
                                {
                                        event.prevent_default();
                                        composer_draft.set(String::new());
                                        session.write().composer = String::new();
                                        slash.set(None);
                                        match action {
                                            chat_compose::SlashAction::Attach => session.write().pick_attachments(),
                                            chat_compose::SlashAction::Stop => session.write().stop_turn(),
                                            chat_compose::SlashAction::Expand => {
                                                composer_expanded.set(!composer_expanded());
                                            }
                                        }
                                        return;
                                }
                                // Same priority as the render below: slash wins.
                                let mention_open = mention().is_some() && slash().is_none();
                                let slash_open = slash().is_some();
                                if mention_open || slash_open {
                                    match event.key() {
                                        Key::ArrowDown => {
                                            event.prevent_default();
                                            if mention_open {
                                                let count = key_mention_bots.len() + key_mention_conns.len();
                                                if count > 0 {
                                                    mention_idx.set((mention_idx() + 1).min(count - 1));
                                                }
                                            } else {
                                                let count = key_slash_skills.len() + key_slash_tools.len();
                                                if count > 0 {
                                                    slash_idx.set((slash_idx() + 1).min(count - 1));
                                                }
                                            }
                                        }
                                        Key::ArrowUp => {
                                            event.prevent_default();
                                            if mention_open {
                                                mention_idx.set(mention_idx().saturating_sub(1));
                                            } else {
                                                slash_idx.set(slash_idx().saturating_sub(1));
                                            }
                                        }
                                        Key::Enter => {
                                            event.prevent_default();
                                            event.stop_propagation();
                                            if mention_open {
                                                let count = key_mention_bots.len() + key_mention_conns.len();
                                                let idx = mention_idx().min(count.saturating_sub(1));
                                                let id = if idx < key_mention_bots.len() {
                                                    key_mention_bots.get(idx).map(|b| b.0.clone())
                                                } else {
                                                    key_mention_conns.get(idx - key_mention_bots.len()).cloned()
                                                };
                                                if let Some(id) = id {
                                                    let text = composer_draft();
                                                    let next = insert_mention_text(&text, &id);
                                                    composer_draft.set(next.clone());
                                                    session.write().composer = next;
                                                    mention.set(None);
                                                }
                                            } else {
                                                let count = key_slash_skills.len() + key_slash_tools.len();
                                                let idx = slash_idx().min(count.saturating_sub(1));
                                                let id = if idx < key_slash_skills.len() {
                                                    key_slash_skills.get(idx).map(|s| s.0.clone())
                                                } else {
                                                    key_slash_tools.get(idx - key_slash_skills.len()).map(|t| t.to_string())
                                                };
                                                if let Some(id) = id {
                                                    let text = composer_draft();
                                                    let next = insert_slash_text(&text, &id);
                                                    composer_draft.set(next.clone());
                                                    session.write().composer = next;
                                                    slash.set(None);
                                                }
                                            }
                                        }
                                        Key::Escape => {
                                            event.prevent_default();
                                            event.stop_propagation();
                                            mention.set(None);
                                            slash.set(None);
                                        }
                                        _ => {}
                                    }
                                    return;
                                }
                                // Esc stops a running reply, like the Stop button.
                                if event.key() == Key::Escape
                                    && chat_compose::escape_stops_run(run_active, awaiting_approval)
                                {
                                    event.prevent_default();
                                    session.write().stop_turn();
                                    return;
                                }
                                // Up in an empty composer recalls the last message.
                                if event.key() == Key::ArrowUp && composer_draft().is_empty() {
                                    let texts: Vec<String> = session
                                        .read()
                                        .state
                                        .messages
                                        .iter()
                                        .filter(|message| message.role == Role::User)
                                        .map(|message| message.content.clone())
                                        .collect();
                                    let refs: Vec<&str> = texts.iter().map(String::as_str).collect();
                                    if let Some(text) = chat_compose::recall_text(&refs) {
                                        event.prevent_default();
                                        composer_draft.set(text.clone());
                                        session.write().composer = text;
                                    }
                                }
                                if event.key() == Key::Enter {
                                    let modifiers = event.modifiers();
                                    let command = modifiers.contains(Modifiers::CONTROL)
                                        || modifiers.contains(Modifiers::META);
                                    let enter_sends = session.read().prefs.enter_sends;
                                    if chat_compose::submit_on_enter(
                                        enter_sends,
                                        modifiers.contains(Modifiers::SHIFT),
                                        command,
                                        event.is_composing(),
                                    ) {
                                        event.prevent_default();
                                        session.write().submit();
                                    }
                                }
                            },
                        }
                        if composer_focused() {
                            div { class: "composer-hint", "{chat_compose::COMPOSER_HINT}" }
                        }
                        if let Some(note) = length_note.clone() {
                            div {
                                class: if over_limit { "composer-count over" } else { "composer-count" },
                                role: "status",
                                "{note}"
                            }
                        }
                        div { class: "composer-row",
                            NewMenu { session, overlay }
                            // The model chip only applies to one-to-one bot
                            // chats; rooms and the owner chat stay
                            // auto-routed (the Providers page keeps the full
                            // ModelBar).
                            if is_one_to_one {
                                ModelBar { session, compact: true }
                            }
                            div { class: "team-row",
                                if group_like {
                                    // Collapsed: one caption chip with a count.
                                    // The member pills only render expanded,
                                    // so the resting composer has no pill row.
                                    button {
                                        class: "chip muted team-chip",
                                        title: if team_collapsed() { "Show members" } else { "Hide members" },
                                        onclick: move |_| team_collapsed.set(!team_collapsed()),
                                        "Talking to {talking_to.len()}"
                                        Icon { name: if team_collapsed() { "chevron-right" } else { "chevron-down" }, size: 14 }
                                    }
                                    if !team_collapsed() {
                                        for (bot_id, bot_name, bot_avatar) in talking_to.iter() {
                                            span {
                                                key: "{bot_id}",
                                                class: "team-chip",
                                                "data-activity": activity_attr(member_acts.get(bot_id).and_then(|a| a.as_ref())),
                                                span {
                                                    class: "avatar-wrap sm",
                                                    "data-activity": activity_attr(member_acts.get(bot_id).and_then(|a| a.as_ref())),
                                                    span { class: "avatar sm bot", style: avatar_style(bot_id), "{bot_avatar}" }
                                                }
                                                "{bot_name}"
                                                if let Some(text) = member_acts.get(bot_id).and_then(|a| a.as_ref()).map(BotActivity::label) {
                                                    span { class: "working-label", role: "status", "{text}" }
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                            div { class: "composer-actions",
                                button {
                                    title: if recording() { "Stop recording" } else { "Record a voice note" },
                                    "aria-label": if recording() { "Stop recording" } else { "Record a voice note" },
                                    class: if recording() { "composer-mic recording" } else { "composer-mic" },
                                    onclick: move |_| async move {
                                        if recording() {
                                            // Take the recorder out of the signal
                                            // for the capture and put it back
                                            // afterwards: a borrow that spans
                                            // an await is not allowed.
                                            let mut device = recorder.take();
                                            let result = device.stop().await;
                                            let filename = device.filename().to_owned();
                                            recorder.set(device);
                                            recording.set(false);
                                            match result {
                                                Ok(bytes) => {
                                                    session.write().transcribe_audio(filename, bytes);
                                                }
                                                Err(message) => {
                                                    session.write().state.transcript_error = Some(message);
                                                }
                                            }
                                        } else {
                                            let mut device = recorder.take();
                                            let result = device.start().await;
                                            recorder.set(device);
                                            match result {
                                                Ok(()) => {
                                                    recording.set(true);
                                                    session.write().state.transcript_error = None;
                                                }
                                                Err(message) => {
                                                    session.write().state.transcript_error = Some(message);
                                                }
                                            }
                                        }
                                    },
                                    Icon { name: if recording() { "check" } else { "mic" }, size: 16 }
                                }
                                button {
                                    class: "composer-mic",
                                    title: if composer_expanded() { "Make the message box smaller" } else { "Make the message box bigger" },
                                    "aria-label": if composer_expanded() { "Make the message box smaller" } else { "Make the message box bigger" },
                                    "aria-pressed": if composer_expanded() { "true" } else { "false" },
                                    onclick: move |_| composer_expanded.set(!composer_expanded()),
                                    Icon { name: if composer_expanded() { "minimize" } else { "maximize" }, size: 16 }
                                }
                                button {
                                    class: "composer-mic",
                                    title: "Attach files (images, PDF, Office, text; up to 25 MB)",
                                    "aria-label": "Attach files",
                                    disabled: !accepts,
                                    onclick: move |_| session.write().pick_attachments(),
                                    Icon { name: "paperclip", size: 16 }
                                }
                                if awaiting_approval {
                                    span { class: "composer-waiting", role: "status",
                                        "Waiting for your approval. Decline it to stop."
                                    }
                                } else if live_task.is_some() {
                                    button {
                                        class: "btn stop-btn",
                                        title: "Stop the reply",
                                        "aria-label": "Stop the reply",
                                        onclick: move |_| session.write().stop_turn(),
                                        Icon { name: "stop", size: 14 }
                                    }
                                } else {
                                    button {
                                        class: "btn primary send-btn",
                                        title: "Send",
                                        "aria-label": "Send message",
                                        disabled: !can_send || !send_ready,
                                        onclick: move |_| session.write().submit(),
                                        Icon { name: "send", size: 16 }
                                    }
                                }
                            }
                        }
                    }
                    if show_mention {
                        div { class: "mention-pop pop above open",
                            // Hidden closer: GLOBAL_KEYS_JS clicks it on Esc /
                            // outside pointerdown so the signal stays in sync.
                            button {
                                class: "escape-close",
                                style: "display: none;",
                                tabindex: -1,
                                onclick: move |_| {
                                    mention.set(None);
                                    mention_idx.set(0);
                                },
                            }
                            div { class: "pop-list",
                                if mention_total == 0 {
                                    div { class: "pop-empty", "No bots or connectors match." }
                                }
                                if !mention_bots.is_empty() {
                                    div { class: "pop-group", "Bots" }
                                    for (index, (id, name, avatar)) in mention_bots.iter().enumerate() {
                                        button {
                                            key: "{id}",
                                            class: if index == mention_active { "pop-item active" } else { "pop-item" },
                                            onclick: {
                                                let id = id.clone();
                                                move |_| {
                                                    let text = composer_draft();
                                                    let next = insert_mention_text(&text, &id);
                                                    composer_draft.set(next.clone());
                                                    session.write().composer = next;
                                                    mention.set(None);
                                                    mention_idx.set(0);
                                                }
                                            },
                                            span { class: "avatar sm bot", style: avatar_style(id), "{avatar}" }
                                            span { "{name}" }
                                            span { class: "micro", "@{id}" }
                                        }
                                    }
                                }
                                if !mention_conns.is_empty() {
                                    div { class: "pop-group", "Connectors" }
                                    for (offset, cid) in mention_conns.iter().enumerate() {
                                        button {
                                            key: "{cid}",
                                            class: if mention_bots.len() + offset == mention_active { "pop-item active" } else { "pop-item" },
                                            onclick: {
                                                let cid = cid.clone();
                                                move |_| {
                                                    let text = composer_draft();
                                                    let next = insert_mention_text(&text, &cid);
                                                    composer_draft.set(next.clone());
                                                    session.write().composer = next;
                                                    mention.set(None);
                                                    mention_idx.set(0);
                                                }
                                            },
                                            BrandLogo { brand: cid.clone(), size: 18 }
                                            span { "{cid}" }
                                        }
                                    }
                                }
                            }
                        }
                    }
                    if show_slash {
                        div { class: "mention-pop pop above open",
                            button {
                                class: "escape-close",
                                style: "display: none;",
                                tabindex: -1,
                                onclick: move |_| {
                                    slash.set(None);
                                    slash_idx.set(0);
                                },
                            }
                            div { class: "pop-list",
                                if slash_total == 0 {
                                    div { class: "pop-empty", "No commands, skills or tools match." }
                                }
                                if !slash_actions.is_empty() {
                                    div { class: "pop-group", "Actions (type the command, then Enter)" }
                                    for (command, description, _) in slash_actions.iter() {
                                        button {
                                            key: "{command}",
                                            class: "pop-item",
                                            onclick: {
                                                let command = (*command).to_owned();
                                                move |_| {
                                                    let text = format!("/{command}");
                                                    composer_draft.set(text.clone());
                                                    session.write().composer = text;
                                                    focus_composer();
                                                }
                                            },
                                            span { "/{command}" }
                                            span { class: "micro", "{description}" }
                                        }
                                    }
                                }
                                if !slash_skills.is_empty() {
                                    div { class: "pop-group", "Skills" }
                                    for (index, (id, name, description)) in slash_skills.iter().enumerate() {
                                        button {
                                            key: "{id}",
                                            class: if index == slash_active { "pop-item active" } else { "pop-item" },
                                            onclick: {
                                                let id = id.clone();
                                                move |_| {
                                                    let text = composer_draft();
                                                    let next = insert_slash_text(&text, &id);
                                                    composer_draft.set(next.clone());
                                                    session.write().composer = next;
                                                    slash.set(None);
                                                    slash_idx.set(0);
                                                }
                                            },
                                            div { style: "display: flex; flex-direction: column; align-items: flex-start; min-width: 0;",
                                                span { "{name}" }
                                                if !description.trim().is_empty() {
                                                    span { class: "caption", "{description}" }
                                                }
                                                span { class: "micro", "{id}" }
                                            }
                                        }
                                    }
                                }
                                if !slash_tools.is_empty() {
                                    div { class: "pop-group", "Tools" }
                                    for (offset, tool) in slash_tools.iter().enumerate() {
                                        button {
                                            key: "{tool}",
                                            class: if slash_skills.len() + offset == slash_active { "pop-item active" } else { "pop-item" },
                                            onclick: {
                                                let tool = tool.to_string();
                                                move |_| {
                                                    let text = composer_draft();
                                                    let next = insert_slash_text(&text, &tool);
                                                    composer_draft.set(next.clone());
                                                    session.write().composer = next;
                                                    slash.set(None);
                                                    slash_idx.set(0);
                                                }
                                            },
                                            div { style: "display: flex; flex-direction: column; align-items: flex-start; min-width: 0;",
                                                span { "{tool}" }
                                                if let Some(label) = capability_label(tool) {
                                                    span { class: "caption", "{label}" }
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

/// Shown under the picker when the server lists no concrete model.
pub(super) const NO_MODELS_HINT: &str = "Ask the admin to allow more models.";

#[component]
pub(super) fn ModelBar(
    session: Signal<Session>,
    #[props(default = false)] compact: bool,
) -> Element {
    // Picker items: (value, label). "auto" first, then the catalog entries
    // (or the static provider model lists when the catalog is empty).
    let (items, current, allowed, saved, rows, kinds) = {
        let session = session.read();
        let catalog = session.state.catalog.as_ref();
        let mut items: Vec<(String, String)> =
            vec![("auto".to_owned(), "Auto · approved route".to_owned())];
        let mut current = "auto".to_owned();
        let mut allowed = false;
        let mut saved = "auto".to_owned();
        let mut rows: Vec<provider_view::PickerRow> = Vec::new();
        let kinds: BTreeMap<String, String> = session
            .state
            .providers
            .iter()
            .map(|provider| (provider.id.clone(), provider.kind.clone()))
            .collect();
        if let Some(catalog) = catalog {
            if !catalog.selection.is_auto() {
                current = format!("{}|{}", catalog.selection.provider, catalog.selection.model);
            }
            // The server's own list, with friendly names, sizes and tags;
            // the plain provider model lists only when it sent no entries.
            if catalog.entries.is_empty() {
                let plain: Vec<serabot_desktop::CatalogEntry> = catalog
                    .providers
                    .iter()
                    .flat_map(|provider| {
                        provider
                            .models
                            .iter()
                            .map(|model| serabot_desktop::CatalogEntry {
                                provider: provider.id.clone(),
                                model: model.clone(),
                                data_egress_approved: provider.data_egress_approved,
                                name: None,
                                context_length: None,
                                tags: Vec::new(),
                                thinking: Vec::new(),
                            })
                    })
                    .collect();
                rows = provider_view::picker_rows(&plain, human_model);
            } else {
                rows = provider_view::picker_rows(&catalog.entries, human_model);
            }
            for row in &rows {
                items.push((
                    row.value.clone(),
                    format!("{} \u{b7} {}", row.title, human_provider(&row.provider)),
                ));
            }
            allowed = catalog.allows(&session.state.draft);
            saved = if catalog.selection.is_auto() {
                "Auto-routing across approved models".into()
            } else {
                format!(
                    "Using {} via {}",
                    human_model(&catalog.selection.model),
                    human_provider(&catalog.selection.provider)
                )
            };
        }
        (items, current, allowed, saved, rows, kinds)
    };
    let value = {
        let draft = session.read().state.draft.clone();
        if draft.is_auto() {
            "auto".to_owned()
        } else {
            format!("{}|{}", draft.provider, draft.model)
        }
    };
    let changed = value != current;
    // Only "auto" is left: the server offered no concrete model.
    let no_models = session.read().state.catalog.is_some() && items.len() <= 1;
    let current_label = items
        .iter()
        .find(|(item_value, _)| *item_value == value)
        .map(|(_, label)| label.clone())
        .unwrap_or_else(|| "Auto · approved route".to_owned());
    // Thinking levels the saved model accepts, and the one in force. The
    // picker is absent for a model with no thinking control.
    let (thinking_levels, thinking_now) = {
        let session = session.read();
        session
            .state
            .catalog
            .as_ref()
            .map(|catalog| {
                (
                    catalog.thinking.levels.clone(),
                    catalog.thinking.level.clone().unwrap_or_default(),
                )
            })
            .unwrap_or_default()
    };
    let mut open = use_signal(|| false);
    let mut query = use_signal(String::new);
    let mut active_idx = use_signal(|| 0usize);
    // Case-insensitive substring match on the human label and on the raw
    // provider/model ids carried by the item value.
    let needle = query().trim().to_lowercase();
    let filtered: Vec<(usize, String, String)> = items
        .iter()
        .enumerate()
        .filter(|(_, item)| {
            needle.is_empty()
                || item.1.to_lowercase().contains(&needle)
                || item.0.to_lowercase().contains(&needle)
        })
        .map(|(index, item)| (index, item.0.clone(), item.1.clone()))
        .collect();
    let active = if filtered.is_empty() {
        0
    } else {
        active_idx().min(filtered.len() - 1)
    };
    // The key handler runs from its own closure: give it its own copy so the
    // pop-list below can still use `filtered`.
    let keyed = filtered.clone();
    // Each visible row with its provider header (drawn when the provider
    // changes) and the metadata the provider's own list gave us.
    let render_rows: Vec<PickerItem> = {
        let mut previous: Option<String> = None;
        filtered
            .iter()
            .map(|(index, value, label)| {
                let row = rows.iter().find(|row| &row.value == value).cloned();
                let header = row.as_ref().and_then(|row| {
                    if previous.as_deref() == Some(row.provider.as_str()) {
                        return None;
                    }
                    previous = Some(row.provider.clone());
                    let kind = kinds
                        .get(&row.provider)
                        .cloned()
                        .unwrap_or_else(|| row.provider.clone());
                    let brand = if branding::brand_label(&row.provider).is_some() {
                        row.provider.clone()
                    } else {
                        kind.clone()
                    };
                    let name = provider_view::display_name(
                        &row.provider,
                        &kind,
                        branding::brand_label(&kind),
                    );
                    Some((brand, name))
                });
                (*index, value.clone(), label.clone(), header, row)
            })
            .collect()
    };
    rsx! {
        div { class: if compact { "model-bar compact" } else { "model-bar" },
            if !compact {
                span { class: "micro", "Model" }
            }
            div {
                class: "menu-wrap",
                onkeydown: move |event: KeyboardEvent| {
                    if !open() {
                        return;
                    }
                    let count = keyed.len();
                    match event.key() {
                        Key::ArrowDown => {
                            event.prevent_default();
                            if count > 0 {
                                active_idx.set((active_idx() + 1).min(count - 1));
                            }
                        }
                        Key::ArrowUp => {
                            event.prevent_default();
                            active_idx.set(active_idx().saturating_sub(1));
                        }
                        Key::Enter => {
                            event.prevent_default();
                            event.stop_propagation();
                            if let Some((_, item_value, _)) =
                                keyed.get(active_idx().min(count.saturating_sub(1)))
                            {
                                let mut session = session.write();
                                set_model_draft(&mut session, item_value);
                                open.set(false);
                                query.set(String::new());
                            }
                        }
                        Key::Escape => {
                            event.prevent_default();
                            event.stop_propagation();
                            open.set(false);
                        }
                        _ => {}
                    }
                },
                button {
                    class: "btn ghost sm menu-trigger",
                    title: "Pick the model route for the next turn",
                    onclick: move |_| {
                        if !open() {
                            query.set(String::new());
                            let position = items.iter().position(|(v, _)| *v == value).unwrap_or(0);
                            active_idx.set(position);
                        }
                        open.set(!open());
                    },
                    span { "{current_label}" }
                    Icon { name: if open() { "chevron-up" } else { "chevron-down" }, size: 14 }
                }
                div {
                    class: if open() { "pop open" } else { "pop" },
                    style: "min-width: 280px;",
                    div { class: "pop-search",
                        input {
                            class: "input",
                            placeholder: "Search models…",
                            value: "{query}",
                            oninput: move |event| {
                                query.set(event.value());
                                active_idx.set(0);
                            },
                        }
                    }
                    div { class: "pop-list",
                        if no_models {
                            div { class: "pop-empty",
                                "No models are available for your providers."
                            }
                            div { class: "pop-empty tiny", "{NO_MODELS_HINT}" }
                        } else if filtered.is_empty() {
                            div { class: "pop-empty", "No models match." }
                        }
                        for (index, item_value, label, header, row) in render_rows {
                            div { key: "{item_value}",
                            if let Some((brand, name)) = header {
                                div { class: "pop-group model-group",
                                    BrandLogo { brand, size: 16 }
                                    span { "{name}" }
                                }
                            }
                            button {
                                class: if index == active { "pop-item active" } else { "pop-item" },
                                onclick: {
                                    let item_value = item_value.clone();
                                    move |_| {
                                        let mut session = session.write();
                                        set_model_draft(&mut session, &item_value);
                                        open.set(false);
                                        query.set(String::new());
                                    }
                                },
                                if let Some(row) = row {
                                    div { class: "model-row",
                                        span { class: "model-title", "{row.title}" }
                                        div { class: "model-meta",
                                            if row.title != row.model {
                                                span { class: "model-id", "{row.model}" }
                                            }
                                            if let Some(context) = row.context.clone() {
                                                span { class: "chip muted", "{context} context" }
                                            }
                                            for tag in row.tags.clone() {
                                                span { class: "chip", "{tag}" }
                                            }
                                        }
                                    }
                                } else {
                                    span { "{label}" }
                                }
                                if item_value == current {
                                    Icon { name: "check", size: 14 }
                                }
                            }
                            }
                        }
                    }
                }
            }
            if !thinking_levels.is_empty() && !changed {
                select {
                    class: "select thinking-select",
                    title: "How hard the model thinks before it answers",
                    "aria-label": "Thinking level",
                    value: "{thinking_now}",
                    onchange: move |event| {
                        let picked = event.value();
                        session
                            .write()
                            .set_thinking((!picked.is_empty()).then_some(picked));
                    },
                    option { value: "", selected: thinking_now.is_empty(), "Thinking: default" }
                    for level in thinking_levels.clone() {
                        option {
                            key: "{level}",
                            value: "{level}",
                            selected: level == thinking_now,
                            "Thinking: {serabot_desktop::thinking_label(&level)}"
                        }
                    }
                }
            }
            if changed && !compact {
                span { class: "tiny", "Egress-approved providers only" }
                button {
                    class: "btn primary sm",
                    disabled: !allowed,
                    onclick: move |_| {
                        let draft = session.read().state.draft.clone();
                        session.write().apply_model(draft);
                    },
                    "Apply"
                }
            } else if changed {
                button {
                    class: "btn primary sm",
                    disabled: !allowed,
                    onclick: move |_| {
                        let draft = session.read().state.draft.clone();
                        session.write().apply_model(draft);
                    },
                    "Apply"
                }
            } else if !compact {
                span { class: "tiny", "Saved {saved}" }
            }
        }
    }
}
