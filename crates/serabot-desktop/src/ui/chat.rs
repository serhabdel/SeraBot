use super::chat_feed_view::{self as feed_view, RowView};
use super::*;

/// Whether the in-conversation search bar is open. Provided by `Shell` so the
/// topbar's search button and the chat view's bar share one flag.
#[derive(Clone, Copy)]
pub(crate) struct ChatFindOpen(pub Signal<bool>);

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
    let assigned_library = {
        let state = session.read();
        chat_feed::assign_library_to_messages(&state.state.messages, &state.state.library)
    };
    let awaiting_approval = session
        .read()
        .state
        .task
        .as_ref()
        .is_some_and(|task| task.status == TaskStatus::AwaitingApproval);
    // One prepared row per message: what the transcript draws, the day
    // heading where a new day starts, and which actions it offers. Rows are
    // rebuilt only when something they are drawn from changed: the session
    // signal is written on every poll and streamed token, and most of those
    // writes leave the transcript as it was.
    let row_cache = use_hook(|| Rc::new(RefCell::new(None::<(u64, Rc<Vec<Rc<MsgView>>>)>)));
    let messages: Rc<Vec<Rc<MsgView>>> = {
        let state = session.read();
        let fingerprint = chat_feed::transcript_fingerprint(
            &state.state.messages,
            (
                state.conversation.as_str(),
                group_like,
                orchestrator.as_deref(),
                idle,
                today.as_str(),
                state.state.bot_chat_binding,
                state
                    .state
                    .listed_bots
                    .iter()
                    .map(|bot| {
                        (
                            bot.id.as_str(),
                            bot.display_name.as_deref(),
                            bot.title.as_deref(),
                            bot.role.as_str(),
                        )
                    })
                    .collect::<Vec<_>>(),
            ),
        );
        let cached = row_cache
            .borrow()
            .as_ref()
            .filter(|(key, _)| *key == fingerprint)
            .map(|(_, rows)| Rc::clone(rows));
        match cached {
            Some(rows) => rows,
            None => {
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
                let mut rows: Vec<Rc<MsgView>> = Vec::with_capacity(state.state.messages.len());
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
                    if !user && Some(index) == last_bot {
                        // The newest reply keeps its actions in view.
                        class.push_str(" last-reply");
                    }
                    rows.push(Rc::new(MsgView {
                        id: message.id.clone(),
                        user,
                        haystack: chat_feed::search_haystack(&message.content, &speaker),
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
                            && serabot_desktop::media_view::offers_continue(
                                &state.state.messages,
                                index,
                            ),
                    }));
                }
                let rows = Rc::new(rows);
                *row_cache.borrow_mut() = Some((fingerprint, Rc::clone(&rows)));
                rows
            }
        }
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
    // "Switched to X (reason)" rows from the server's route events, placed like
    // resolved cards: before the first message made after them.
    let switch_placed: Vec<Vec<route_chip::SwitchRow>> = {
        let state = session.read();
        let options = state.route_options();
        route_chip::place_rows(
            &message_times,
            route_chip::switch_rows(&options, &state.state.route_events),
        )
    };
    // "Earlier messages were summarized" rows, placed like the switch rows.
    let compaction_placed: Vec<Vec<serabot_desktop::runs::CompactionRow>> =
        chat_cards::place_compactions(&message_times, &session.read().state.runs.compactions);
    // A failed turn offers the next route of the bot's chain.
    let (fallback_offer, failed_task) = {
        let state = session.read();
        let options = state.route_options();
        let offer = route_chip::fallback_offer(
            &options,
            state.state.route_chain.as_deref(),
            &state.state.draft,
        );
        let task = state.state.task.as_ref();
        (
            offer
                .filter(|offer| route_chip::offer_retry(task.map(|task| task.status), Some(offer))),
            task.filter(|task| task.status == TaskStatus::Failed)
                .map(|task| task.id.clone()),
        )
    };
    let mut dismissed_fallback = use_signal(|| Option::<String>::None);
    // The failed-turn card: reason chips, Retry on the next route, Retry on
    // another route and Ask main. It shows for a failed turn that has any way
    // forward, until dismissed.
    let show_failed_card = if failed_task.is_some() && dismissed_fallback() != failed_task {
        let state = session.read();
        let options = state.route_options();
        let ask_main = state
            .state
            .listed_bots
            .iter()
            .any(|bot| bot.role == "orchestrator" && bot.enabled);
        let card = chat_failure::failed_card(
            &options,
            state.state.route_chain.as_deref(),
            &state.state.draft,
            &state.state.route_events,
            fallback_offer.clone(),
            ask_main,
        );
        (card.fallback.is_some() || !card.alternatives.is_empty() || card.ask_main).then_some(card)
    } else {
        None
    };
    let live_task: Option<TaskStatus> = session
        .read()
        .state
        .task
        .as_ref()
        .and_then(|task| (!task.status.terminal()).then_some(task.status));
    // The waiting-for-a-model line, with its countdown, while the turn is parked.
    let wait_line = {
        let state = session.read();
        chat_cards::wait_banner(
            state.state.task.as_ref().map(|task| task.status),
            state.state.runs.waiting.as_ref(),
            serabot_desktop::workflows::now_secs(),
        )
    };
    // Stop stays on screen while the reply or any subagent runs.
    let (any_run_active, stopping) = {
        let s = session.read();
        (s.any_run_active(), s.state.stopping)
    };
    let queued_count = session.read().queue.len();
    let (steps, steps_summary, steps_dropped, steps_attention, sending, history_ready) = {
        let state = session.read();
        let sending =
            state.state.messages.last().is_some_and(|message| {
                message.role == Role::User && message.id.starts_with("opt-")
            });
        (
            state.state.activity.steps.clone(),
            state.state.activity.summary(),
            state.state.activity.dropped_note(),
            state.state.activity.needs_attention(),
            sending,
            state.state.history_loaded,
        )
    };
    let chat_plan = session.read().state.plan.clone();
    let mut composer_draft = use_signal(|| session.read().composer.clone());
    let can_send = session.read().state.can_send();
    // "Retry on <fallback>" first picks the route, so it needs only an idle,
    // connected chat (not that the draft matches the saved route).
    let can_send_again = {
        let state = session.read();
        state.state.connected && state.state.history_loaded && state.ready()
    };
    let accepts = session.read().conversation_accepts_turns();
    let (active_bot, active_name, active_avatar, roster) = {
        let state = session.read();
        let active = state.conversation_bot(&state.conversation).or_else(|| {
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
    // The message the reader is looking at while scrolled up (C1), kept as an
    // id so rows loading in front never move the drawn window, and the ids of
    // the rows as last drawn for the page's reports to resolve against.
    let mut feed_anchor = use_signal(|| Option::<String>::None);
    let feed_ids = use_hook(|| Rc::new(RefCell::new(Vec::<String>::new())));
    let feed_ids_sink = Rc::clone(&feed_ids);
    let mut feed_reported = use_signal(|| Option::<usize>::None);
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
    let copied = use_signal(|| Option::<String>::None);
    let mut confirm_regen = use_signal(|| Option::<String>::None);
    let task_added_ids = use_signal(HashSet::<String>::new);
    // The topbar owns the search button, so the open flag is shared; it closes
    // with the conversation (the view is keyed by it).
    let ChatFindOpen(mut search_open) = use_context::<ChatFindOpen>();
    use_drop(move || search_open.set(false));
    let mut search_query = use_signal(String::new);
    // Keyed by (message, attempt): the same sentence after another failed
    // send is a new failure and shows again.
    let mut dismissed_failure = use_signal(|| Option::<(String, u64)>::None);
    let mut dismissed_held = use_signal(|| false);
    // One polite announcement per finished reply, never per streamed token.
    let mut announcement = use_signal(String::new);
    let mut was_running = use_signal(|| false);
    use_effect(move || {
        let (running, done, spoken) = {
            let state = session.read();
            let task = state.state.task.as_ref();
            let running = task.is_some_and(|task| !task.status.terminal());
            let done = task.is_some_and(|task| task.status == TaskStatus::Done);
            let spoken = state
                .state
                .messages
                .iter()
                .rev()
                .find(|message| message.role != Role::User)
                .map(|message| chat_feed::reply_announcement(&state.message_speaker(message).0));
            (running, done, spoken)
        };
        let was = *was_running.peek();
        if was == running {
            return;
        }
        was_running.set(running);
        if was
            && done
            && let Some(text) = spoken
        {
            announcement.set(text);
        }
    });
    let mut dismissed_transcript_err = use_signal(|| Option::<String>::None);
    let enter_sends_pref = session.read().prefs.enter_sends;
    let mut composer_room = use_signal(|| session.read().conversation.clone());
    let mut prev_external = use_signal(|| session.read().composer.clone());
    use_effect(move || {
        let (conversation, external) = {
            let state = session.read();
            (state.conversation.clone(), state.composer.clone())
        };
        if *composer_room.peek() != conversation {
            composer_room.set(conversation);
            prev_external.set(external.clone());
            composer_draft.set(external);
            return;
        }
        let prev = prev_external.peek().clone();
        if chat_compose::should_sync_external_composer(
            &external,
            &prev,
            *composer_focused.peek(),
            &composer_draft.peek(),
        ) {
            composer_draft.set(external.clone());
        }
        if external != prev {
            prev_external.set(external);
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
    // The first visible row, reported by the page after it moved a few rows.
    use_future(move || {
        let ids = Rc::clone(&feed_ids_sink);
        async move {
            let _ = dioxus::document::eval(&feed_view::feed_window_js());
            let mut sink = dioxus::document::eval(feed_view::FEED_SINK_JS);
            while let Ok(index) = sink.recv::<i64>().await {
                let index = usize::try_from(index).unwrap_or(0);
                if !feed_view::moved_enough(*feed_reported.peek(), index) {
                    continue;
                }
                feed_reported.set(Some(index));
                let id = ids.borrow().get(index).cloned();
                if *feed_anchor.peek() != id {
                    feed_anchor.set(id);
                }
            }
        }
    });
    // A chat that was just opened or switched to is ready to type in. The view
    // is keyed by conversation, so this runs once per switch.
    use_effect(focus_composer);
    let transcript_error = session.read().state.transcript_error.clone();
    let chat_failure = session.read().state.chat_failure.clone();
    let held_draft = session.read().state.held_draft.is_some();
    let failure_attempt = session.read().state.failure_attempt;
    use_effect(move || {
        let s = session.read();
        let no_fail = s.state.chat_failure.is_none();
        let no_held = s.state.held_draft.is_none();
        let no_err = s.state.transcript_error.is_none();
        drop(s);
        if no_fail && dismissed_failure.peek().is_some() {
            dismissed_failure.set(None);
        }
        if no_held && *dismissed_held.peek() {
            dismissed_held.set(false);
        }
        if no_err && dismissed_transcript_err.peek().is_some() {
            dismissed_transcript_err.set(None);
        }
    });
    let show_failure = chat_failure
        .as_ref()
        .filter(|f| dismissed_failure() != Some((f.message.clone(), failure_attempt)))
        .cloned();
    let show_held = held_draft && !dismissed_held();
    let show_transcript_error = transcript_error
        .as_ref()
        .filter(|msg| dismissed_transcript_err().as_deref() != Some(msg.as_str()))
        .cloned();
    let reconnecting = {
        let state = session.read();
        state.state.connected && state.events_seen && !state.events_active
    };
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
            guard.0 = conversation.clone();
            guard.1.clear();
        }
        *last_mark_reset.borrow_mut() = chat_scroll::TranscriptMark::default();
        last_count.set(0);
        at_bottom.set(true);
        unseen.set(0);
        feed_anchor.set(None);
        feed_reported.set(None);
        mention.set(None);
        mention_idx.set(0);
        slash.set(None);
        slash_idx.set(0);
        confirm_regen.set(None);
        search_query.set(String::new());
        search_open.set(false);
        dismissed_failure.set(None);
        dismissed_held.set(false);
        dismissed_transcript_err.set(None);
        dismissed_fallback.set(None);
        was_running.set(false);
        announcement.set(String::new());
        composer_expanded.set(false);
        if let Ok(conv_json) = serde_json::to_string(&conversation) {
            let _ =
                dioxus::document::eval(&format!("window.__serabotOnConvSwitch?.({conv_json});"));
        }
        focus_composer();
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
        let head = session
            .read()
            .state
            .messages
            .iter()
            .find(|message| !message.id.starts_with("opt-"))
            .map(|message| message.id.clone());
        let mark = chat_scroll::TranscriptMark {
            count,
            tail,
            steps: activity_steps,
            step_tail: activity_tail,
            head,
        };
        // Real growth only: this effect also runs for unrelated session writes.
        let (streaming_growth, prepended) = {
            let mut last = last_mark_effect.borrow_mut();
            let grew = *last != mark;
            let prepended = chat_scroll::rows_prepended(&last, &mark);
            *last = mark;
            (grew, prepended)
        };
        let previous = *last_count.peek();
        if count != previous {
            // Rows added in front ("Load earlier") are history, not news.
            if count > previous && !*at_bottom.peek() && !own_send && !prepended {
                let incoming = session
                    .read()
                    .state
                    .messages
                    .iter()
                    .skip(previous)
                    .filter(|message| message.role != Role::User)
                    .count();
                if incoming > 0 {
                    let added = *unseen.peek() + incoming;
                    unseen.set(added);
                }
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
        if prepended {
            // Keep the row the reader was on where it was.
            let _ = dioxus::document::eval(chat_scroll::ANCHOR_RESTORE_JS);
        } else if let Some(force_pin) =
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
    // Until this chat's transcript has arrived, show placeholders instead of
    // the "start a conversation" empty state (it used to flash on every open).
    // A transcript that failed or stalled gets its own panel with Retry; it
    // must never fall through to the "start a conversation" empty state.
    let (
        history_loading,
        history_failed,
        has_more_history,
        older_loading,
        older_error,
        cards_degraded,
    ) = {
        let state = session.read();
        let waiting = empty && state.state.connected && !state.state.history_loaded;
        (
            waiting && !state.state.history_failed,
            waiting && state.state.history_failed,
            state.state.has_more_history && state.state.history_loaded,
            state.state.older_loading,
            state.state.older_error.clone(),
            state.state.cards_degraded,
        )
    };
    let link_retrying = session.read().link_retrying;
    let conversation_id = session.read().conversation.clone();
    // On a new chat: where it started from (the bot's memory and a summary).
    let memory_note = {
        let state = session.read();
        state.bot_chat(&state.conversation).and_then(|chat| {
            let bot = state.conversation_bot(&chat.id)?;
            let previous = chat
                .carried_from
                .as_deref()
                .and_then(|id| state.bot_chat(id))
                .map(|prev| match prev.title.as_deref().map(str::trim) {
                    Some(title) if !title.is_empty() => title.to_owned(),
                    _ => "the main chat".to_owned(),
                });
            serabot_desktop::bot_chats::memory_divider(
                chat,
                previous.as_deref(),
                &state.bot_display_name(&bot),
            )
        })
    };
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
    let key_slash_actions = slash_actions.clone();
    let key_slash_skills = slash_skills.clone();
    let key_slash_tools = slash_tools.clone();
    let composer_text = composer_draft();
    let composer_chars = composer_text.chars().count();
    let length_note = chat_compose::length_message(composer_chars);
    let over_limit = composer_chars > chat_compose::MAX_TEXT_CHARS;
    let composer_file_count = session.read().composer_files.len();
    let busy = session.read().uploading || session.read().upload_pending() > 0;
    let send_ready = chat_compose::can_submit(&composer_text, composer_file_count, busy);
    let sensitive_draft = chat_compose::detect_sensitive_draft(&composer_text);
    let message_count = messages.len();
    let search_needle = search_query().trim().to_lowercase();
    let search_active = search_open() && !search_needle.is_empty();
    let search_match_count = if search_active {
        messages
            .iter()
            .filter(|row| chat_feed::row_matches(&row.haystack, &search_needle))
            .count()
    } else {
        0
    };
    // Each row with the class it is drawn with this time (entrance animation
    // once per row, search highlight), its bot chips and its media cards.
    let row_views: Vec<RowView> = {
        let mut guard = entered_messages.borrow_mut();
        chat_feed::trim_entered(
            &mut guard.1,
            messages.iter().map(|row| row.id.as_str()),
            messages.len(),
        );
        messages
            .iter()
            .map(|row| {
                let enter = !guard.1.contains(&row.id);
                if enter {
                    guard.1.insert(row.id.clone());
                }
                let mut class = if enter {
                    format!("{} msg-enter", row.class)
                } else {
                    row.class.clone()
                };
                if search_active {
                    class.push_str(if chat_feed::row_matches(&row.haystack, &search_needle) {
                        " msg-match"
                    } else {
                        " msg-dim"
                    });
                }
                RowView {
                    row: Rc::clone(row),
                    class,
                    bot_meta: row.bot.as_ref().and_then(|id| bot_meta.get(id).cloned()),
                    media: if row.user {
                        Vec::new()
                    } else {
                        assigned_library
                            .by_message
                            .get(&row.id)
                            .cloned()
                            .unwrap_or_default()
                    },
                }
            })
            .collect()
    };
    // Draw the rows near the viewport only; spacers stand in for the rest.
    let feed = {
        *feed_ids.borrow_mut() = row_views.iter().map(|view| view.row.id.clone()).collect();
        let anchor = feed_anchor.read().as_deref().and_then(|id| {
            feed_view::anchor_index(row_views.iter().map(|view| view.row.id.as_str()), id)
        });
        feed_view::feed_window(row_views.len(), anchor, at_bottom())
    };
    if is_one_to_one
        && let Some(bot_id) = active_bot.clone()
        && session.read().bot_uses_claude_agent(&bot_id)
    {
        return rsx! {
            crate::ui_claude_code::ClaudeAgentBotView { session, bot_id }
        };
    }
    rsx! {
        div { class: "pane-body chat-body",
            onkeydown: move |event: KeyboardEvent| {
                if event.is_composing() {
                    return;
                }
                let command = event.modifiers().contains(Modifiers::CONTROL)
                    || event.modifiers().contains(Modifiers::META);
                // Ctrl+F searches the conversation whenever it has a message;
                // only an empty chat leaves the key to the browser.
                if command
                    && matches!(event.key(), Key::Character(ref c) if c.eq_ignore_ascii_case("f"))
                    && (message_count >= 1 || search_open())
                {
                    event.prevent_default();
                    search_open.set(true);
                    let _ = dioxus::document::eval(
                        "requestAnimationFrame(() => { const el = document.getElementById('chat-find-input'); if (el) { el.focus(); el.select(); } })",
                    );
                } else if command && event.key() == Key::End && !composer_focused() {
                    // Ctrl+End: jump to the latest message.
                    event.prevent_default();
                    let _ = dioxus::document::eval(chat_scroll::SCROLL_TO_BOTTOM_JS);
                    at_bottom.set(true);
                    unseen.set(0);
                } else if command
                    && matches!(event.key(), Key::Character(ref c) if c == "/")
                {
                    // Ctrl+/: back to the composer from anywhere in the chat.
                    event.prevent_default();
                    focus_composer();
                } else if event.key() == Key::F1 {
                    event.prevent_default();
                    overlay.set(Overlay::Shortcuts);
                } else if event.key() == Key::Escape
                    && !command
                    && !stopping
                    && any_run_active
                    && !search_open()
                    && !show_mention
                    && !show_slash
                {
                    // Esc stops the reply that is being written.
                    event.prevent_default();
                    session.write().stop_turn_or_runs();
                }
            },
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
                if search_open() {
                    div { class: "chat-find-bar", role: "search", "aria-label": "Search in conversation",
                            Icon { name: "search", size: 14 }
                            input {
                                id: "chat-find-input",
                                class: "chat-find-input",
                                r#type: "text",
                                placeholder: "Search in conversation\u{2026}",
                                value: "{search_query()}",
                                oninput: move |event| search_query.set(event.value()),
                                onkeydown: move |event: KeyboardEvent| {
                                    if event.key() == Key::Escape {
                                        event.prevent_default();
                                        search_query.set(String::new());
                                        search_open.set(false);
                                    }
                                },
                            }
                            if search_active {
                                span { class: "chat-find-count",
                                    if search_match_count == 1 {
                                        "1 match"
                                    } else {
                                        "{search_match_count} matches"
                                    }
                                }
                            }
                            button {
                                class: "btn ghost sm chat-find-close",
                                r#type: "button",
                                title: "Clear search",
                                "aria-label": "Clear search",
                                onclick: move |_| {
                                    search_query.set(String::new());
                                    search_open.set(false);
                                },
                                Icon { name: "x", size: 13 }
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
                    // The page keeps each chat's scroll position by this id.
                    "data-conv": "{conversation_id}",
                    // A log landmark for screen readers. Live announcements are
                    // off: announcing every streamed token would be noise.
                    role: "log",
                    "aria-label": "Conversation",
                    "aria-live": "off",
                    if let Some(note) = memory_note.clone() {
                        div { class: "memory-divider", role: "note",
                            span { "{note}" }
                            button {
                                class: "btn ghost sm",
                                onclick: move |_| session.write().open_section(Section::Memory),
                                "View"
                            }
                        }
                    }
                    if has_more_history {
                        div { class: "load-earlier-row",
                            button {
                                class: "btn ghost sm",
                                r#type: "button",
                                disabled: older_loading,
                                onclick: move |_| {
                                    // Remember the scroll position so the row
                                    // being read stays put when rows are added above it.
                                    let _ = dioxus::document::eval(chat_scroll::ANCHOR_CAPTURE_JS);
                                    session.write().load_older_messages();
                                },
                                if older_loading {
                                    "Loading earlier messages…"
                                } else {
                                    "Load earlier messages"
                                }
                            }
                            if let Some(message) = older_error.clone() {
                                span { class: "load-earlier-error", role: "alert", "{message}" }
                            }
                        }
                    }
                    if history_loading {
                        div { class: "transcript-skeleton", role: "status", "aria-label": "Loading messages",
                            div { class: "sk-row user", span { class: "sk-bar w40" } }
                            div { class: "sk-row", span { class: "sk-bar w90" } span { class: "sk-bar w75" } span { class: "sk-bar w55" } }
                            div { class: "sk-row user", span { class: "sk-bar w25" } }
                            div { class: "sk-row", span { class: "sk-bar w80" } span { class: "sk-bar w60" } }
                        }
                    } else if history_failed {
                        div { class: "history-failed", role: "alert",
                            h2 { class: "history-failed-title", "Could not load this conversation" }
                            p { class: "history-failed-text",
                                "The messages did not arrive. Nothing is lost: they are still on the server."
                            }
                            button {
                                class: "btn primary sm",
                                r#type: "button",
                                disabled: link_retrying,
                                onclick: move |_| session.write().retry_history(),
                                if link_retrying {
                                    "Retrying…"
                                } else {
                                    "Retry"
                                }
                            }
                        }
                    } else if empty {
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
                                div { class: "starter-quick-chips", role: "group", "aria-label": "Quick actions",
                                    for text in chat_feed::quick_chips(active_bot.as_deref()).iter().copied() {
                                        button {
                                            key: "{text}",
                                            class: "starter-quick-chip",
                                            r#type: "button",
                                            onclick: move |_| {
                                                let text = text.to_owned();
                                                session.write().composer = text.clone();
                                                composer_draft.set(text);
                                                focus_composer();
                                            },
                                            Icon { name: "sparkles", size: 12 }
                                            " {text}"
                                        }
                                    }
                                }
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
                            if !is_one_to_one {
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
                    }
                    if feed.top_spacer_px() > 0 {
                        div {
                            id: "feed-spacer-top",
                            class: "feed-spacer",
                            "aria-hidden": "true",
                            "data-start": "{feed.start}",
                            style: "height: {feed.top_spacer_px()}px",
                        }
                    }
                    for (index, view) in row_views.into_iter().enumerate().skip(feed.start).take(feed.end - feed.start) {
                        Fragment { key: "{view.row.id}",
                            for card in placed[index].clone() {
                                ChatCardRow { session, key: "{card.id}", card }
                            }
                            for row in switch_placed[index].clone() {
                                RouteSwitchRow { key: "{row.id}", text: row.text }
                            }
                            for (n, row) in compaction_placed[index].clone().into_iter().enumerate() {
                                CompactionNote { key: "c{index}-{n}", text: row.text }
                            }
                            if let Some(heading) = view.row.day.clone() {
                                div { class: "day-sep", role: "separator",
                                    span { "{heading}" }
                                }
                            }
                            MessageRow {
                                session,
                                row: view.row,
                                class: view.class,
                                bot_meta: view.bot_meta,
                                media: view.media,
                                accepts,
                                copied,
                                confirm_regen,
                                task_added_ids,
                                composer_draft,
                            }
                        }
                    }
                    if feed.bottom_spacer_px() > 0 {
                        div {
                            class: "feed-spacer",
                            "aria-hidden": "true",
                            style: "height: {feed.bottom_spacer_px()}px",
                        }
                    }
                    // Resolved cards that come after the newest message.
                    for card in placed.last().cloned().unwrap_or_default() {
                        ChatCardRow { session, key: "{card.id}", card }
                    }
                    for row in switch_placed.last().cloned().unwrap_or_default() {
                        RouteSwitchRow { key: "{row.id}", text: row.text }
                    }
                    for (n, row) in compaction_placed.last().cloned().unwrap_or_default().into_iter().enumerate() {
                        CompactionNote { key: "c-last-{n}", text: row.text }
                    }
                    // Files produced in an active turn that has no reply yet,
                    // plus any in-flight media jobs.
                    TrailingMediaCards {
                        session,
                        entries: assigned_library.trailing,
                    }
                    // Cards waiting for the owner: questions and secret requests.
                    for card in pending_cards.clone() {
                        ChatCardRow { session, key: "{card.id}", card }
                    }
                    // What the bot is doing right now, in plain words: a
                    // shimmer while it thinks, one row per step, and a small
                    // "N steps" summary once the reply has landed.
                    if reconnecting {
                        div { class: "banner warn", role: "status", "aria-live": "polite",
                            span { class: "step-spin", "aria-hidden": "true" }
                            "Reconnecting to live updates…"
                            button {
                                class: "btn ghost sm",
                                r#type: "button",
                                disabled: link_retrying,
                                "aria-label": "Try the live connection again",
                                onclick: move |_| session.write().retry_link(),
                                if link_retrying {
                                    "Retrying…"
                                } else {
                                    "Retry"
                                }
                            }
                        }
                    }
                    if cards_degraded {
                        div { class: "banner info", role: "status",
                            "Some questions or requests from your bots could not be refreshed. Reload to try again."
                        }
                    }
                    if sending && live_task.is_none() {
                        div { class: "activity", role: "status",
                            span { class: "shimmer", if stopping { "Stopping\u{2026}" } else { "Sending\u{2026}" } }
                        }
                    }
                    if live_task.is_some() {
                        div { class: "activity", role: "status", "aria-live": "polite",
                            span { class: "working",
                                span { class: "working-text",
                                    if stopping {
                                        "Stopping\u{2026}"
                                    } else {
                                        "{working}"
                                    }
                                }
                                span { class: "working-dots", "aria-hidden": "true",
                                    span {}
                                    span {}
                                    span {}
                                }
                            }
                            if !steps.is_empty() {
                                div { class: "activity-steps",
                                    for step in steps.iter().cloned() {
                                        ActivityRow { session, step, key: "{step.call_id}" }
                                    }
                                    if let Some(note) = steps_dropped.clone() {
                                        p { class: "activity-more", "{note}" }
                                    }
                                }
                            }
                        }
                    } else if !steps.is_empty() && history_ready {
                        // The summary describes a reply: it waits for the
                        // transcript, and opens by itself when a step needs
                        // approval or failed, so it cannot hide either.
                        details {
                            class: "activity done",
                            open: steps_attention,
                            ontoggle: move |_| {
                                let _ = dioxus::document::eval(
                                    "requestAnimationFrame(() => { const el = document.querySelector('details.activity.done[open]'); if (el) el.scrollIntoView({ block: 'nearest', behavior: 'smooth' }); })",
                                );
                            },
                            summary { class: "activity-summary",
                                Icon { name: "chevron-right", size: 12 }
                                "{steps_summary}"
                            }
                            div { class: "activity-steps",
                                for step in steps.iter().cloned() {
                                    ActivityRow { session, step, key: "{step.call_id}" }
                                }
                                if let Some(note) = steps_dropped.clone() {
                                    p { class: "activity-more", "{note}" }
                                }
                            }
                        }
                    }
                    // One polite announcement per finished reply.
                    div {
                        class: "visually-hidden",
                        role: "status",
                        "aria-live": "polite",
                        "aria-atomic": "true",
                        "{announcement()}"
                    }
                    // Bottom sentinel: the page watches it to know the reader
                    // is at the end of the conversation.
                    div { id: "messages-end", class: "messages-end", "aria-hidden": "true" }
                }
                div { class: "composer-wrap",
                    // Inside the composer block, so it always sits just above it, at
                    // any composer height.
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
                    crate::ui_cards::InlineApprovals { session }
                    if let Some(card) = show_failed_card.clone() {
                        FailedTurnCard {
                            session,
                            card,
                            can_act: can_send_again,
                            ondismiss: {
                                let failed = failed_task.clone();
                                move |_| dismissed_fallback.set(failed.clone())
                            },
                        }
                    }
                    if let Some(line) = wait_line.clone() {
                        WaitBanner { line }
                    }
                    if let Some(failure) = show_failure.clone() {
                        div { class: "banner err", role: "alert",
                            span { "{failure.message}" }
                            match failure.action {
                                ChatAction::Retry => rsx! {
                                    button {
                                        class: "btn primary sm",
                                        r#type: "button",
                                        "aria-label": "Send the message again",
                                        onclick: move |_| session.write().retry_send(),
                                        "Retry"
                                    }
                                },
                                ChatAction::Reconnect => rsx! {
                                    button {
                                        class: "btn ghost sm",
                                        r#type: "button",
                                        disabled: link_retrying,
                                        "aria-label": "Try the connection again",
                                        onclick: move |_| session.write().retry_link(),
                                        if link_retrying {
                                            "Retrying…"
                                        } else {
                                            "Reconnect"
                                        }
                                    }
                                },
                                ChatAction::OpenKeys => rsx! {
                                    button {
                                        class: "btn ghost sm",
                                        r#type: "button",
                                        "aria-label": "Open AI providers",
                                        onclick: move |_| session.write().open_section(Section::Providers),
                                        "Open AI providers"
                                    }
                                },
                                ChatAction::None => rsx! {},
                            }
                            button {
                                class: "btn ghost sm chat-banner-close",
                                r#type: "button",
                                title: "Dismiss",
                                "aria-label": "Dismiss error",
                                onclick: move |_| dismissed_failure.set(Some((failure.message.clone(), failure_attempt))),
                                Icon { name: "x", size: 13 }
                            }
                        }
                    } else if show_held {
                        div { class: "banner warn", role: "status",
                            span { "That message may have reached SeraBot. It is kept here if you want it back in the box." }
                            button {
                                class: "btn ghost sm",
                                r#type: "button",
                                "aria-label": "Put the message back in the box",
                                onclick: move |_| session.write().restore_held_draft(),
                                "Put it back"
                            }
                            button {
                                class: "btn ghost sm",
                                r#type: "button",
                                "aria-label": "Check the transcript for the message",
                                onclick: move |_| session.write().retry_history(),
                                "Check transcript"
                            }
                            button {
                                class: "btn ghost sm chat-banner-close",
                                r#type: "button",
                                title: "Dismiss",
                                "aria-label": "Dismiss draft notice",
                                onclick: move |_| dismissed_held.set(true),
                                Icon { name: "x", size: 13 }
                            }
                        }
                    }
                    if let Some(message) = show_transcript_error {
                        div { class: "banner err", role: "alert",
                            span { "{message}" }
                            button {
                                class: "btn ghost sm chat-banner-close",
                                r#type: "button",
                                title: "Dismiss",
                                "aria-label": "Dismiss voice notice",
                                onclick: move |_| dismissed_transcript_err.set(Some(message.clone())),
                                Icon { name: "x", size: 13 }
                            }
                        }
                    }
                    if let Some(matched) = sensitive_draft.clone() {
                        div { class: "chat-secret-guard", role: "alert",
                            div { class: "chat-secret-guard-copy",
                                Icon { name: "shield", size: 15 }
                                span {
                                    "Sensitive credential detected ({matched.summary()}) \u{2014} never send raw secrets in chat."
                                }
                            }
                            div { class: "chat-secret-guard-actions",
                                button {
                                    class: "btn sm",
                                    r#type: "button",
                                    onclick: move |_| {
                                        let redacted = chat_compose::redact_sensitive_draft(&composer_draft());
                                        composer_draft.set(redacted.clone());
                                        session.write().composer = redacted;
                                        focus_composer();
                                    },
                                    "Redact before sending"
                                }
                                button {
                                    class: "btn primary sm",
                                    r#type: "button",
                                    onclick: move |_| session.write().open_section(Section::Vault),
                                    Icon { name: "lock", size: 12 }
                                    " Save in Vault"
                                }
                            }
                        }
                    }
                    QueuePanel { session }
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
                            onblur: move |_| {
                                composer_focused.set(false);
                                let draft = composer_draft();
                                if session.peek().composer != draft {
                                    session.write().composer = draft;
                                }
                            },
                            oninput: move |event| {
                                let value = event.value();
                                confirm_regen.set(None);
                                composer_draft.set(value.clone());
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
                                            chat_compose::SlashAction::NewChat => {
                                                let bot = {
                                                    let state = session.read();
                                                    state.conversation_bot(&state.conversation)
                                                };
                                                if let Some(bot) = bot {
                                                    session.write().new_bot_chat(bot);
                                                }
                                            }
                                            chat_compose::SlashAction::Memory => {
                                                session.write().open_section(Section::Memory);
                                            }
                                            chat_compose::SlashAction::Vault => {
                                                session.write().open_section(Section::Vault);
                                            }
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
                                if (mention_open || slash_open) && !event.is_composing() {
                                    match event.key() {
                                        Key::ArrowDown => {
                                            event.prevent_default();
                                            if mention_open {
                                                let count = key_mention_bots.len() + key_mention_conns.len();
                                                if count > 0 {
                                                    mention_idx.set((mention_idx() + 1).min(count - 1));
                                                }
                                            } else {
                                                let count = key_slash_actions.len()
                                                    + key_slash_skills.len()
                                                    + key_slash_tools.len();
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
                                        Key::Enter | Key::Tab => {
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
                                                    mention_idx.set(0);
                                                    focus_composer();
                                                }
                                            } else {
                                                let a_len = key_slash_actions.len();
                                                let s_len = key_slash_skills.len();
                                                let t_len = key_slash_tools.len();
                                                let total = a_len + s_len + t_len;
                                                let idx = slash_idx().min(total.saturating_sub(1));
                                                match chat_compose::slash_pick_at(idx, a_len, s_len, t_len) {
                                                    Some(chat_compose::SlashPick::Action(i)) => {
                                                        if let Some((command, _, _)) = key_slash_actions.get(i) {
                                                            let next = format!("/{command}");
                                                            composer_draft.set(next.clone());
                                                            session.write().composer = next;
                                                            slash.set(None);
                                                            slash_idx.set(0);
                                                            focus_composer();
                                                        }
                                                    }
                                                    Some(chat_compose::SlashPick::Skill(i)) => {
                                                        if let Some((id, _, _)) = key_slash_skills.get(i) {
                                                            let next = insert_slash_text(&composer_draft(), id);
                                                            composer_draft.set(next.clone());
                                                            session.write().composer = next;
                                                            slash.set(None);
                                                            slash_idx.set(0);
                                                            focus_composer();
                                                        }
                                                    }
                                                    Some(chat_compose::SlashPick::Tool(i)) => {
                                                        if let Some(tool) = key_slash_tools.get(i) {
                                                            let next = insert_slash_text(&composer_draft(), tool);
                                                            composer_draft.set(next.clone());
                                                            session.write().composer = next;
                                                            slash.set(None);
                                                            slash_idx.set(0);
                                                            focus_composer();
                                                        }
                                                    }
                                                    None => {}
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
                                    && !stopping
                                    && chat_compose::escape_stops_run(any_run_active, awaiting_approval)
                                {
                                    event.prevent_default();
                                    session.write().stop_turn_or_runs();
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
                                        // While a reply runs, Enter queues the message.
                                        let mut s = session.write();
                                        s.composer = composer_draft();
                                        s.send_or_queue();
                                        composer_draft.set(s.composer.clone());
                                    }
                                }
                            },
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
                                    disabled: !accepts,
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
                                } else if any_run_active {
                                    // Typing while a reply runs queues the message; Stop is
                                    // always within reach while anything runs.
                                    if send_ready && live_task.is_some() {
                                        button {
                                            class: "btn ghost queue-btn",
                                            r#type: "button",
                                            title: "Add to the queue; it is sent when the reply is done",
                                            "aria-label": "Add the message to the queue",
                                            disabled: !accepts || queued_count >= chat_compose::MAX_QUEUED,
                                            onclick: move |_| {
                                                let mut s = session.write();
                                                s.composer = composer_draft();
                                                s.queue_composer();
                                                composer_draft.set(s.composer.clone());
                                            },
                                            Icon { name: "list", size: 14 }
                                            "Queue"
                                        }
                                    }
                                    button {
                                        class: if stopping { "btn stop-btn composer-stop stopping" } else { "btn stop-btn composer-stop" },
                                        title: if stopping { "Stopping reply\u{2026}" } else { "Stop (Esc)" },
                                        "aria-label": if stopping { "Stopping reply" } else { "Stop generating" },
                                        disabled: stopping,
                                        onclick: move |_| session.write().stop_turn_or_runs(),
                                        Icon { name: "stop", size: 14 }
                                        if stopping {
                                            span { "Stopping\u{2026}" }
                                        }
                                    }
                                } else {
                                    button {
                                        class: "btn primary send-btn",
                                        title: "Send",
                                        "aria-label": "Send message",
                                        disabled: !can_send || !send_ready,
                                        onclick: move |_| {
                                            let mut s = session.write();
                                            s.composer = composer_draft();
                                            s.submit();
                                            composer_draft.set(s.composer.clone());
                                        },
                                        Icon { name: "send", size: 16 }
                                    }
                                }
                            }
                        }
                    }
                    // The key hint sits under the container, always, so it never
                    // overlaps the text and the box never jumps when focus moves.
                    div { class: "composer-hint",
                        "{chat_compose::composer_hint(enter_sends_pref)}"
                        button {
                            class: "link-btn shortcuts-link",
                            r#type: "button",
                            title: "Keyboard shortcuts (F1)",
                            onclick: move |_| overlay.set(Overlay::Shortcuts),
                            "Shortcuts"
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
                                    div { class: "pop-group", "Actions (pick, then Enter)" }
                                    for (index, (command, description, _)) in slash_actions.iter().enumerate() {
                                        button {
                                            key: "{command}",
                                            class: if index == slash_active { "pop-item active" } else { "pop-item" },
                                            onclick: {
                                                let command = (*command).to_owned();
                                                move |_| {
                                                    let text = format!("/{command}");
                                                    composer_draft.set(text.clone());
                                                    session.write().composer = text;
                                                    slash.set(None);
                                                    slash_idx.set(0);
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
                                            class: if slash_actions.len() + index == slash_active { "pop-item active" } else { "pop-item" },
                                            onclick: {
                                                let id = id.clone();
                                                move |_| {
                                                    let text = composer_draft();
                                                    let next = insert_slash_text(&text, &id);
                                                    composer_draft.set(next.clone());
                                                    session.write().composer = next;
                                                    slash.set(None);
                                                    slash_idx.set(0);
                                                    focus_composer();
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
                                            class: if slash_actions.len() + slash_skills.len() + offset == slash_active { "pop-item active" } else { "pop-item" },
                                            onclick: {
                                                let tool = tool.to_string();
                                                move |_| {
                                                    let text = composer_draft();
                                                    let next = insert_slash_text(&text, &tool);
                                                    composer_draft.set(next.clone());
                                                    session.write().composer = next;
                                                    slash.set(None);
                                                    slash_idx.set(0);
                                                    focus_composer();
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

/// One message of the transcript. A component of its own so that a row whose
/// props did not change is not re-diffed when the session is written for
/// something else (a poll, a toast, a token of the streaming reply).
#[component]
fn MessageRow(
    mut session: Signal<Session>,
    row: Rc<MsgView>,
    class: String,
    bot_meta: Option<(String, bool)>,
    media: Vec<serabot_desktop::LibraryEntry>,
    accepts: bool,
    mut copied: Signal<Option<String>>,
    mut confirm_regen: Signal<Option<String>>,
    mut task_added_ids: Signal<HashSet<String>>,
    mut composer_draft: Signal<String>,
) -> Element {
    rsx! {
        div { class: "{class}",
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
                    if row.bot.is_some() {
                        if let Some((role, live)) = bot_meta.clone() {
                            span { class: "team-chip", "{role}" }
                            span {
                                class: if live { "chip ok" } else { "chip warn" },
                                if live { "live" } else { "not live" }
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
                if !media.is_empty() {
                    TurnMediaCards { session, entries: media.clone() }
                }
                div {
                    class: "msg-actions",
                    role: "toolbar",
                    "aria-label": "Message actions",
                    "aria-orientation": "horizontal",
                    // One tab stop per toolbar; the arrow keys move inside it.
                    onkeydown: move |event: KeyboardEvent| {
                        let direction = match event.key() {
                            Key::ArrowRight | Key::ArrowDown => "next",
                            Key::ArrowLeft | Key::ArrowUp => "prev",
                            Key::Home => "first",
                            Key::End => "last",
                            _ => return,
                        };
                        event.prevent_default();
                        let _ = dioxus::document::eval(&chat_scroll::toolbar_move_js(direction));
                    },
                    // The time shows with the actions, on hover or focus.
                    span { class: "msg-action-time", "{row.stamp}" }
                    button {
                        class: "msg-action",
                        r#type: "button",
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
                        Icon {
                            name: if copied().as_deref() == Some(row.id.as_str()) { "check" } else { "copy" },
                            size: 13
                        }
                        span { class: "msg-action-label",
                            if copied().as_deref() == Some(row.id.as_str()) { "Copied" } else { "Copy" }
                        }
                    }
                    if accepts && !row.content.trim().is_empty() {
                        button {
                            class: "msg-action",
                            r#type: "button",
                            "aria-label": "Quote message in composer",
                            title: "Quote in composer",
                            onclick: {
                                let text = row.content.clone();
                                move |_| {
                                    confirm_regen.set(None);
                                    let next = chat_compose::quote_reply_text(
                                        &composer_draft(),
                                        &text,
                                    );
                                    composer_draft.set(next.clone());
                                    session.write().composer = next;
                                    focus_composer();
                                }
                            },
                            Icon { name: "chat", size: 13 }
                            span { class: "msg-action-label", "Quote" }
                        }
                    }
                    if !row.content.trim().is_empty() {
                        if task_added_ids().contains(&row.id) {
                            span { class: "msg-task-added",
                                Icon { name: "check", size: 12 }
                                " Task added"
                            }
                        } else {
                            button {
                                class: "msg-action",
                                r#type: "button",
                                "aria-label": "Create task from message",
                                title: "Create task from this message",
                                onclick: {
                                    let text = row.content.clone();
                                    let id = row.id.clone();
                                    move |_| {
                                        let title = chat_compose::task_title_from_message(&text);
                                        session.write().add_task(title, None);
                                        task_added_ids.write().insert(id.clone());
                                    }
                                },
                                Icon { name: "plus", size: 13 }
                                span { class: "msg-action-label", "Create task" }
                            }
                        }
                    }
                    if row.edit {
                        button {
                            class: "msg-action",
                            r#type: "button",
                            "aria-label": "Edit message",
                            title: "Put this message back in the composer",
                            onclick: {
                                let text = row.content.clone();
                                move |_| {
                                    confirm_regen.set(None);
                                    composer_draft.set(text.clone());
                                    session.write().edit_text(text.clone());
                                    focus_composer();
                                }
                            },
                            Icon { name: "edit", size: 13 }
                            span { class: "msg-action-label", "Edit" }
                        }
                    }
                    if let Some(text) = row.regen.clone() {
                        {
                            let confirming = confirm_regen().as_deref() == Some(row.id.as_str());
                            let row_id = row.id.clone();
                            rsx! {
                                button {
                                    class: if confirming { "msg-action warn" } else { "msg-action" },
                                    r#type: "button",
                                    "aria-label": if confirming { "Confirm overwrite of draft and regenerate reply" } else { "Regenerate reply" },
                                    title: if confirming { "Click again to replace your unsent draft and resend" } else { "Send your last message again" },
                                    onclick: move |_| {
                                        if composer_draft().trim().is_empty() || confirming {
                                            confirm_regen.set(None);
                                            session.write().regenerate(text.clone());
                                        } else {
                                            confirm_regen.set(Some(row_id.clone()));
                                        }
                                    },
                                    Icon { name: "refresh", size: 13 }
                                    span { class: "msg-action-label",
                                        if confirming { "Replace draft?" } else { "Regenerate" }
                                    }
                                }
                            }
                        }
                    }
                    if row.continue_offer {
                        button {
                            class: "msg-action",
                            r#type: "button",
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

/// Shown under the picker when the server lists no concrete model.
pub(super) const NO_MODELS_HINT: &str = "Ask the admin to allow more models.";
const LOCAL_ACP_VALUE: &str = "local-acp|claude-code";

#[component]
pub(super) fn ModelBar(
    session: Signal<Session>,
    #[props(default = false)] compact: bool,
) -> Element {
    // Picker items: (value, label). "auto" first, optional local Claude Code
    // (ACP) entry in a 1:1 bot chat, then the catalog entries (or the static
    // provider model lists when the catalog is empty).
    let (active_bot, acp_active, items, current, allowed, saved, rows, kinds, provider_accounts) = {
        let session = session.read();
        let active_bot = session.conversation_bot(&session.conversation);
        let acp_active = active_bot
            .as_deref()
            .is_some_and(|bot| session.bot_uses_claude_agent(bot));
        let catalog = session.state.catalog.as_ref();
        let mut items: Vec<(String, String)> =
            vec![("auto".to_owned(), "Auto · approved route".to_owned())];
        if active_bot.is_some() {
            items.push((
                LOCAL_ACP_VALUE.to_owned(),
                "Claude Code · Local desktop (ACP)".to_owned(),
            ));
        }
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
                                account_ids: Vec::new(),
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
        // The account step of the picker: only for a provider where the
        // member has two or more usable accounts.
        let options = session.route_options();
        let mut provider_accounts: BTreeMap<String, Vec<(String, String)>> = BTreeMap::new();
        for row in &rows {
            if !provider_accounts.contains_key(&row.provider) {
                provider_accounts.insert(
                    row.provider.clone(),
                    route_chip::picker_accounts(&options, &row.provider),
                );
            }
        }
        (
            active_bot,
            acp_active,
            items,
            current,
            allowed,
            saved,
            rows,
            kinds,
            provider_accounts,
        )
    };
    let value = {
        let draft = session.read().state.draft.clone();
        if draft.is_auto() {
            "auto".to_owned()
        } else {
            format!("{}|{}", draft.provider, draft.model)
        }
    };
    let (draft_account, saved_account) = {
        let s = session.read();
        (
            s.state.draft.account.clone(),
            s.state
                .catalog
                .as_ref()
                .and_then(|catalog| catalog.selection.account.clone()),
        )
    };
    let changed = value != current || draft_account != saved_account;
    // Only "auto" (plus local ACP in bot chat) left: the server offered no concrete model.
    let no_models = session.read().state.catalog.is_some()
        && items.len() <= if active_bot.is_some() { 2 } else { 1 };
    // Thinking levels the active model accepts, and the one in force. Updates
    // immediately when switching models instead of waiting on a catalog reload.
    let (thinking_levels, thinking_now) = {
        let session = session.read();
        let draft = &session.state.draft;
        let draft_levels = if !draft.provider.is_empty() && !draft.model.is_empty() {
            session
                .route_options()
                .thinking_levels(&draft.provider, &draft.model)
        } else {
            Vec::new()
        };
        let (saved_levels, saved_level) = session
            .state
            .catalog
            .as_ref()
            .map(|catalog| {
                (
                    catalog.thinking.levels.clone(),
                    catalog.thinking.level.clone().unwrap_or_default(),
                )
            })
            .unwrap_or_default();
        let levels = if !draft_levels.is_empty() {
            draft_levels
        } else if !changed {
            saved_levels
        } else {
            Vec::new()
        };
        let active_level = if levels.iter().any(|l| l == &saved_level) {
            saved_level
        } else {
            String::new()
        };
        (levels, active_level)
    };
    // The chip: `provider/account · model · thinking`.
    let current_label = if acp_active {
        "Claude Code · Local ACP".to_owned()
    } else {
        let s = session.read();
        let level = (!changed && !thinking_now.is_empty()).then_some(thinking_now.as_str());
        route_chip::chip_label(&s.route_options(), &s.state.draft, level)
    };
    let segments = route_chip::thinking_segments(&thinking_levels, &thinking_now);
    let mut open = use_signal(|| false);
    let mut query = use_signal(String::new);
    let mut active_idx = use_signal(|| 0usize);
    // The account the member chose for a provider in this picker; a provider
    // never touched here keeps the draft's account when it is the draft's own.
    let picked_account = use_signal(BTreeMap::<String, String>::new);
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
    let bot_for_enter = active_bot.clone();
    let bot_for_click = active_bot.clone();
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
                                if item_value == LOCAL_ACP_VALUE {
                                    if let Some(bot) = bot_for_enter.as_deref() {
                                        session.write().set_bot_claude_agent(bot, true);
                                    }
                                    open.set(false);
                                    query.set(String::new());
                                    return;
                                }
                                let account = picked_account_for(&session.read(), &picked_account.read(), item_value);
                                let mut session = session.write();
                                if let Some(bot) = bot_for_enter.as_deref() {
                                    session.set_bot_claude_agent(bot, false);
                                }
                                set_model_draft_with(&mut session, item_value, account);
                                if compact {
                                    let parsed = session.state.draft.clone();
                                    if session
                                        .state
                                        .catalog
                                        .as_ref()
                                        .is_some_and(|catalog| catalog.allows(&parsed))
                                    {
                                        session.apply_model(parsed);
                                    }
                                }
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
                    class: "btn ghost sm menu-trigger route-chip",
                    title: "Pick the provider, account, model and thinking for the next turn",
                    "aria-label": "Route: {current_label}",
                    onclick: move |_| {
                        if !open() {
                            query.set(String::new());
                            let target = if acp_active { LOCAL_ACP_VALUE } else { value.as_str() };
                            let position = items.iter().position(|(v, _)| v == target).unwrap_or(0);
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
                        for (pos, (_index, item_value, label, header, row)) in render_rows.into_iter().enumerate() {
                            div { key: "{item_value}",
                            if item_value == LOCAL_ACP_VALUE {
                                div { class: "pop-group model-group",
                                    BrandLogo { brand: "claude".to_owned(), size: 16 }
                                    span { "Local \u{00b7} Desktop (ACP)" }
                                }
                            } else if let Some((brand, name)) = header {
                                div { class: "pop-group model-group",
                                    BrandLogo { brand, size: 16 }
                                    span { "{name}" }
                                }
                                if let Some(choices) = row
                                    .as_ref()
                                    .and_then(|row| provider_accounts.get(&row.provider).cloned())
                                    .filter(|choices| !choices.is_empty())
                                {
                                    PickAccounts {
                                        session,
                                        provider: row.as_ref().map(|row| row.provider.clone()).unwrap_or_default(),
                                        choices,
                                        compact,
                                        picked: picked_account,
                                    }
                                }
                            }
                            button {
                                class: if pos == active { "pop-item active" } else { "pop-item" },
                                onclick: {
                                    let item_value = item_value.clone();
                                    let bot_for_click = bot_for_click.clone();
                                    move |_| {
                                        if item_value == LOCAL_ACP_VALUE {
                                            if let Some(bot) = bot_for_click.as_deref() {
                                                session.write().set_bot_claude_agent(bot, true);
                                            }
                                            open.set(false);
                                            query.set(String::new());
                                            return;
                                        }
                                        let account = picked_account_for(&session.read(), &picked_account.read(), &item_value);
                                        let mut session = session.write();
                                        if let Some(bot) = bot_for_click.as_deref() {
                                            session.set_bot_claude_agent(bot, false);
                                        }
                                        set_model_draft_with(&mut session, &item_value, account);
                                        if compact {
                                            let parsed = session.state.draft.clone();
                                            if session
                                                .state
                                                .catalog
                                                .as_ref()
                                                .is_some_and(|catalog| catalog.allows(&parsed))
                                            {
                                                session.apply_model(parsed);
                                            }
                                        }
                                        open.set(false);
                                        query.set(String::new());
                                    }
                                },
                                if item_value == LOCAL_ACP_VALUE {
                                    div { class: "model-row",
                                        span { class: "model-title", "Claude Code" }
                                        div { class: "model-meta",
                                            span { class: "chip", "Local ACP" }
                                            span { class: "model-id", "Runs locally in allowed folder" }
                                        }
                                    }
                                } else if let Some(row) = row {
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
                                if (item_value == LOCAL_ACP_VALUE && acp_active) || (!acp_active && item_value == current) {
                                    Icon { name: "check", size: 14 }
                                }
                            }
                            }
                        }
                    }
                }
            }
            if !segments.is_empty() && !changed {
                div {
                    class: "seg thinking-seg",
                    role: "group",
                    title: "How hard the model thinks before it answers",
                    "aria-label": "Thinking level",
                    for segment in segments {
                        button {
                            key: "{segment.label}",
                            r#type: "button",
                            class: if segment.active { "seg-btn active" } else { "seg-btn" },
                            "aria-pressed": if segment.active { "true" } else { "false" },
                            onclick: {
                                let value = segment.value.clone();
                                move |_| {
                                    session
                                        .write()
                                        .set_thinking((!value.is_empty()).then(|| value.clone()));
                                }
                            },
                            "{segment.label}"
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
                    r#type: "button",
                    disabled: !allowed,
                    title: if allowed {
                        "Apply the selected model before sending"
                    } else {
                        "This provider is not approved for egress"
                    },
                    onclick: move |_| {
                        let draft = session.read().state.draft.clone();
                        session.write().apply_model(draft);
                    },
                    if allowed { "Apply model" } else { "Not approved" }
                }
            } else if !compact {
                span { class: "tiny", "Saved {saved}" }
            }
        }
    }
}

/// The account step of the model picker: "Any account" or one of the member's
/// own, for one provider. Picking one for the provider the conversation is on
/// moves the draft at once.
#[component]
fn PickAccounts(
    session: Signal<Session>,
    provider: String,
    choices: Vec<(String, String)>,
    compact: bool,
    picked: Signal<BTreeMap<String, String>>,
) -> Element {
    let mut picked = picked;
    let shown = {
        let s = session.read();
        picked.read().get(&provider).cloned().unwrap_or_else(|| {
            if s.state.draft.provider == provider {
                s.state.draft.account.clone().unwrap_or_default()
            } else {
                String::new()
            }
        })
    };
    rsx! {
        div { class: "pick-accounts", role: "group", "aria-label": "Account",
            for (account_id, account_label) in choices {
                button {
                    key: "{account_id}",
                    r#type: "button",
                    class: if shown == account_id { "seg-btn active" } else { "seg-btn" },
                    "aria-pressed": if shown == account_id { "true" } else { "false" },
                    onclick: {
                        let provider = provider.clone();
                        let account_id = account_id.clone();
                        move |_| {
                            picked.write().insert(provider.clone(), account_id.clone());
                            let mut s = session.write();
                            if s.state.draft.provider == provider {
                                s.state.draft.account =
                                    (!account_id.is_empty()).then(|| account_id.clone());
                                if compact {
                                    let draft = s.state.draft.clone();
                                    if s
                                        .state
                                        .catalog
                                        .as_ref()
                                        .is_some_and(|catalog| catalog.allows(&draft))
                                    {
                                        s.apply_model(draft);
                                    }
                                }
                            }
                        }
                    },
                    "{account_label}"
                }
            }
        }
    }
}

/// One quiet "Switched to X (reason)" line in the transcript.
#[component]
fn RouteSwitchRow(text: String) -> Element {
    rsx! {
        div { class: "route-switch", role: "status",
            Icon { name: "refresh", size: 12 }
            span { "{text}" }
        }
    }
}
