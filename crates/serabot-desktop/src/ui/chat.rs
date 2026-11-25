use super::chat_feed_view::{self as feed_view, RowView};
use super::*;

/// Whether the in-conversation search bar is open. Provided by `Shell` so the
/// topbar's search button and the chat view's bar share one flag.
#[derive(Clone, Copy)]
pub(crate) struct ChatFindOpen(pub Signal<bool>);

/// Whether the bot chat history popover is open. Provided by `Shell` so the
/// topbar switcher, its History button, and the sidebar's "All chats..." link
/// share one flag.
#[derive(Clone, Copy)]
pub(crate) struct ChatHistoryOpen(pub Signal<bool>);

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
    // Library artifacts per message, cached like the rows below: assigning
    // them sorts the whole library, so it runs only when the transcript
    // fingerprint or the library itself changed — never on a streamed token.
    let library_cache =
        use_hook(|| Rc::new(RefCell::new(None::<(u64, u64, chat_feed::AssignedLibrary)>)));
    // One prepared row per message: what the transcript draws, the day
    // heading where a new day starts, and which actions it offers. Rows are
    // rebuilt only when something they are drawn from changed: the session
    // signal is written on every poll and streamed token, and most of those
    // writes leave the transcript as it was.
    let row_cache = use_hook(|| Rc::new(RefCell::new(None::<(u64, Rc<Vec<Rc<MsgView>>>)>)));
    let (fingerprint, messages): (u64, Rc<Vec<Rc<MsgView>>>) = {
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
            state.state.turns.revision(),
            &state.state.activity,
        );
        let cached = row_cache
            .borrow()
            .as_ref()
            .filter(|(key, _)| *key == fingerprint)
            .map(|(_, rows)| Rc::clone(rows));
        match cached {
            Some(rows) => (fingerprint, rows),
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
                let turn_anchors = serabot_desktop::turn_trace::anchors(
                    state
                        .state
                        .messages
                        .iter()
                        .map(|m| (m.role == Role::User, m.turn_id.as_deref())),
                );
                let mut rows: Vec<Rc<MsgView>> = Vec::with_capacity(state.state.messages.len());
                let mut prev_user: Option<bool> = None;
                let mut prev_day: Option<String> = None;
                // A message sent again with no reply in between shows once.
                let repeats = chat_feed::retry_duplicates(
                    &state
                        .state
                        .messages
                        .iter()
                        .map(|m| {
                            (
                                m.role == Role::User,
                                m.content.as_str(),
                                m.attachments.len(),
                            )
                        })
                        .collect::<Vec<_>>(),
                );
                for (index, message) in state.state.messages.iter().enumerate() {
                    if repeats[index] {
                        continue;
                    }
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
                        haystack: chat_feed::cached_search_haystack(
                            &message.id,
                            &message.content,
                            &speaker,
                        ),
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
                        turn: turn_anchors.get(index).cloned().flatten(),
                    }));
                }
                let rows = Rc::new(rows);
                *row_cache.borrow_mut() = Some((fingerprint, Rc::clone(&rows)));
                (fingerprint, rows)
            }
        }
    };
    // Library assignment behind the row-cache hit (0.74.10): the fingerprint
    // is the transcript's, the digest is the library's (its entries' hashes
    // and stamps), so a new artifact still re-assigns while a streamed
    // token does not.
    let assigned_library = {
        let state = session.read();
        let library_digest = {
            use std::hash::{Hash, Hasher};
            let mut hasher = std::collections::hash_map::DefaultHasher::new();
            state.state.library.len().hash(&mut hasher);
            for entry in &state.state.library {
                entry.hash.hash(&mut hasher);
                entry.created_at.hash(&mut hasher);
            }
            hasher.finish()
        };
        let cached = library_cache
            .borrow()
            .as_ref()
            .filter(|(key, digest, _)| *key == fingerprint && *digest == library_digest)
            .map(|(_, _, assigned)| assigned.clone());
        match cached {
            Some(assigned) => assigned,
            None => {
                let assigned = chat_feed::assign_library_to_messages(
                    &state.state.messages,
                    &state.state.library,
                );
                *library_cache.borrow_mut() = Some((fingerprint, library_digest, assigned.clone()));
                assigned
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
    // The finished turn's steps already sit inside its reply (0.61): the
    // bottom summary stays only for a turn whose reply carries no turn id.
    let steps_attached = {
        let state = session.read();
        let task = state.state.activity.task_id.as_str();
        !task.is_empty()
            && state.state.messages.iter().rev().any(|message| {
                message.role != Role::User
                    && message
                        .turn_id
                        .as_deref()
                        .and_then(|turn| state.state.turns.get(turn))
                        .is_some_and(|trace| trace.activity.task_id == task)
            })
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
    // The one working indicator (0.74.3): a single LiveCard derived from
    // the live turn — the same single running step the step rows show, so
    // the feed never shows two "working" states at once. It collapses into
    // the reply's TurnTrace when the turn completes.
    let model_label = {
        let state = session.read();
        match state.state.catalog.as_ref().map(|c| &c.selection) {
            Some(selection) if !selection.is_auto() => short_model(&selection.model),
            _ => "Auto".to_owned(),
        }
    };
    // The latest route switch, for the failover flash (~6s) and the
    // waiting-for-a-model reason chip. Labels only: no provider error text.
    let latest_switch = {
        let state = session.read();
        state
            .state
            .route_events
            .iter()
            .filter(|event| {
                event
                    .to_provider
                    .as_deref()
                    .is_some_and(|provider| !provider.is_empty())
            })
            .max_by(|a, b| {
                a.created_at
                    .cmp(&b.created_at)
                    .then_with(|| a.id.cmp(&b.id))
            })
            .cloned()
    };
    let failover: Option<(String, String)> = {
        let state = session.read();
        let now_secs = serabot_desktop::workflows::now_secs();
        latest_switch
            .clone()
            .filter(|event| {
                serabot_desktop::workflows::epoch_secs(&event.created_at)
                    .is_some_and(|at| now_secs >= at && now_secs - at <= 6)
            })
            .map(|event| {
                let options = state.route_options();
                let from = serabot_desktop::route_editor::route_line(
                    &options,
                    event.from_provider.as_deref().unwrap_or(""),
                    event.from_account.as_deref(),
                    event.from_model.as_deref().unwrap_or(""),
                    None,
                );
                let to = serabot_desktop::route_editor::route_line(
                    &options,
                    event.to_provider.as_deref().unwrap_or(""),
                    event.to_account.as_deref(),
                    event.to_model.as_deref().unwrap_or(""),
                    None,
                );
                (from, to)
            })
    };
    let waiting_reason = latest_switch
        .as_ref()
        .map(|event| serabot_desktop::route_editor::reason_text(&event.reason).to_owned());
    // Another bot working in this conversation is a handoff, derived from
    // the handoff ledger (`runs.rows`): a row that started and has not
    // completed yet. Never from the WorkingBots rings, which also fire for
    // the chat's own turn. The card names the first handoff and counts the
    // rest, so parallel handoffs stay visible as one card.
    let (delegate_bot, delegate_count): (Option<String>, usize) = {
        let state = session.read();
        let own = state
            .conversation
            .strip_prefix("dir:")
            .unwrap_or(&state.conversation);
        let bots: Vec<String> = serabot_desktop::runs::active_handoff_bots(&state.state.runs.rows)
            .into_iter()
            .filter(|bot| bot != own)
            .collect();
        let count = bots.len();
        let first = bots
            .into_iter()
            .next()
            .map(|bot| state.bot_display_name(&bot));
        (first, count)
    };
    let needs_review = session.read().needs_review_active();
    let streaming_reply = session
        .read()
        .state
        .messages
        .last()
        .is_some_and(|message| message.role != Role::User);
    // The message still being written, if any: its markdown renders in
    // streaming mode (unfinished constructs as plain text, no Copy button).
    let streaming_message_id: Option<String> = live_task
        .is_some()
        .then(|| {
            session
                .read()
                .state
                .messages
                .last()
                .filter(|message| message.role != Role::User)
                .map(|message| message.id.clone())
        })
        .flatten();
    // Context fill of the latest turn, shown in the chat itself (not only
    // in the model popover). The ledger already skips child turns: a child
    // turn's context never becomes the latest one.
    let (chat_ctx_percent, chat_ctx_title) = {
        let state = session.read();
        match state.state.turns.latest_context() {
            Some(context) if context.percent.is_some() => {
                let window = context
                    .window
                    .map(|w| w.to_string())
                    .unwrap_or_else(|| "unknown".to_owned());
                (
                    context.percent,
                    format!(
                        "~{} chars of {} context used by the latest turn",
                        context.prompt_chars_est, window
                    ),
                )
            }
            _ => (None, String::new()),
        }
    };
    let live_card: Option<LiveCardData> = (live_task.is_some() || wait_line.is_some()).then(|| {
        let activity = session.read().state.activity.clone();
        derive_live_card(LiveCardInputs {
            bot_name: &active_name,
            model_label: Some(model_label.as_str()),
            activity: &activity,
            waiting_line: wait_line.as_deref(),
            waiting_reason: waiting_reason.as_deref(),
            failover: failover
                .as_ref()
                .map(|(from, to)| (from.as_str(), to.as_str())),
            delegate_bot: delegate_bot.as_deref(),
            delegate_count,
            needs_review,
            streaming_reply,
            failed: false,
            now: Instant::now(),
        })
    });
    let failed_live_card: Option<LiveCardData> = show_failed_card.as_ref().map(|_| {
        let activity = session.read().state.activity.clone();
        derive_live_card(LiveCardInputs {
            bot_name: &active_name,
            model_label: Some(model_label.as_str()),
            activity: &activity,
            waiting_line: None,
            waiting_reason: None,
            failover: None,
            delegate_bot: None,
            delegate_count: 0,
            needs_review: false,
            streaming_reply: false,
            failed: true,
            now: Instant::now(),
        })
    });
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
    // One tick a second while recording, for the elapsed timer. `record_tick`
    // counts ticks since the page loaded; the value at the start of a take is
    // remembered so the label shows only this recording's time.
    let mut record_tick = use_signal(|| 0u64);
    let mut record_tick_start = use_signal(|| 0u64);
    use_future(move || async move {
        let mut sink = dioxus::document::eval(crate::ui_files::RECORD_TICK_JS);
        while let Ok(true) = sink.recv::<bool>().await {
            record_tick.set(record_tick() + 1);
        }
    });
    // Up/Down history cycling: the index into the owner's messages already
    // recalled into the box (`None` while nothing is recalled).
    let mut recall_idx = use_signal(|| Option::<usize>::None);
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
    // Start of the window as last drawn: with no anchor reported yet, the
    // window keeps this instead of jumping to the tail.
    let last_feed_start = use_hook(|| Rc::new(RefCell::new(None::<usize>)));
    let last_feed_start_render = Rc::clone(&last_feed_start);
    let last_feed_start_reset = Rc::clone(&last_feed_start);
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
    // Which search match the jump buttons are on (0-based, into the
    // matching rows in transcript order).
    let mut search_idx = use_signal(|| 0usize);
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
    let mut prev_gen = use_signal(|| session.read().composer_gen);
    use_effect(move || {
        let (conversation, external, generation) = {
            let state = session.read();
            (
                state.conversation.clone(),
                state.composer.clone(),
                state.composer_gen,
            )
        };
        if *composer_room.peek() != conversation {
            composer_room.set(conversation);
            prev_gen.set(generation);
            composer_draft.set(external);
            return;
        }
        // The generation decides: only an intentional replace (chip, slash
        // insert, failed-send restore, conversation switch) syncs the box.
        // Background session writes while the owner types never erase input.
        if chat_compose::should_sync_external_composer(
            &external,
            generation,
            *prev_gen.peek(),
            *composer_focused.peek(),
            &composer_draft.peek(),
        ) {
            composer_draft.set(external.clone());
        }
        if generation != *prev_gen.peek() {
            prev_gen.set(generation);
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
    // The first visible row, reported by the page as its message id after it
    // moved a few rows. The id resolves to an index against the ids as last
    // drawn, so rows loading in front never move the window.
    use_future(move || {
        let ids = Rc::clone(&feed_ids_sink);
        async move {
            let _ = dioxus::document::eval(&feed_view::feed_window_js());
            let mut sink = dioxus::document::eval(feed_view::FEED_SINK_JS);
            while let Ok(id) = sink.recv::<String>().await {
                let Some(index) =
                    feed_view::anchor_index(ids.borrow().iter().map(String::as_str), id.as_str())
                else {
                    continue;
                };
                if !feed_view::moved_enough(*feed_reported.peek(), index) {
                    continue;
                }
                feed_reported.set(Some(index));
                if feed_anchor.peek().as_deref() != Some(id.as_str()) {
                    feed_anchor.set(Some(id));
                }
            }
        }
    });
    // A chat that was just opened or switched to is ready to type in. The view
    // is keyed by conversation, so this runs once per switch.
    use_effect(focus_composer);
    let transcript_error = session.read().state.transcript_error.clone();
    let transcript_loading = session.read().state.transcript_loading;
    let last_audio_kept = session.read().last_audio.is_some();
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
        composer.clear_last_audio();
        let next = if composer.composer.trim().is_empty() {
            text
        } else {
            format!("{}\n{}", composer.composer, text)
        };
        composer.replace_composer(next.clone());
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
        *last_feed_start_reset.borrow_mut() = None;
        feed_reported.set(None);
        mention.set(None);
        mention_idx.set(0);
        slash.set(None);
        slash_idx.set(0);
        recall_idx.set(None);
        confirm_regen.set(None);
        search_query.set(String::new());
        search_idx.set(0);
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
            // While following, the bridge already snaps to the bottom on
            // growth: a soft pin from Rust is a redundant eval per streamed
            // token. A forced pin (the owner's own send) still goes through.
            if force_pin || !*at_bottom.peek() {
                let _ = dioxus::document::eval(&chat_scroll::pin_messages_js(force_pin));
            }
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
    // The picker row keyboard focus is on, for the textarea's
    // `aria-activedescendant`.
    let active_descendant = if show_mention && mention_total > 0 {
        Some(format!("mention-item-{mention_active}"))
    } else if show_slash && slash_total > 0 {
        Some(format!("slash-item-{slash_active}"))
    } else {
        None
    };
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
    let record_elapsed = record_tick().saturating_sub(record_tick_start());
    let composer_file_count = session.read().composer_files.len();
    let busy = session.read().uploading || session.read().upload_pending() > 0;
    let send_ready = chat_compose::can_submit(&composer_text, composer_file_count, busy);
    let sensitive_draft = chat_compose::detect_sensitive_draft(&composer_text);
    let message_count = messages.len();
    let search_needle = search_query().trim().to_lowercase();
    let search_active = search_open() && !search_needle.is_empty();
    // The ids of the matching rows, in transcript order: next/prev jumps
    // walk this list and set the feed anchor to the match.
    let search_match_ids: Vec<String> = if search_active {
        messages
            .iter()
            .filter(|row| chat_feed::row_matches(&row.haystack, &search_needle))
            .map(|row| row.id.clone())
            .collect()
    } else {
        Vec::new()
    };
    let search_match_count = search_match_ids.len();
    // Copies for the search key/button closures (they move their captures).
    let search_ids_for_keys = search_match_ids.clone();
    let search_ids_for_prev = search_match_ids.clone();
    let search_ids_for_next = search_match_ids.clone();
    // Jump to one match: the feed anchor centres the window on it, follow
    // turns off, and the page scrolls the row into view once it is drawn.
    // Takes the signals as arguments so the closure captures nothing and
    // stays Fn — it is called from three separate event handlers.
    let jump_to_search_match = |index: usize,
                                ids: Vec<String>,
                                mut search_idx: Signal<usize>,
                                mut feed_anchor: Signal<Option<String>>,
                                mut at_bottom: Signal<bool>| {
        let Some(id) = ids.get(index).cloned() else {
            return;
        };
        search_idx.set(index);
        feed_anchor.set(Some(id.clone()));
        at_bottom.set(false);
        if let Ok(id_json) = serde_json::to_string(&id) {
            let _ = dioxus::document::eval(&format!(
                "requestAnimationFrame(() => requestAnimationFrame(() => {{ const el = document.querySelector('.msg[data-row-id=' + {id_json} + ']'); if (el) el.scrollIntoView({{ block: 'center' }}); }}));"
            ));
        }
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
        let window = feed_view::feed_window(
            row_views.len(),
            anchor,
            at_bottom(),
            *last_feed_start_render.borrow(),
        );
        *last_feed_start_render.borrow_mut() = Some(window.start);
        window
    };
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
                                oninput: move |event| {
                                    search_query.set(event.value());
                                    search_idx.set(0);
                                },
                                onkeydown: move |event: KeyboardEvent| {
                                    if event.key() == Key::Escape {
                                        event.prevent_default();
                                        search_query.set(String::new());
                                        search_open.set(false);
                                    } else if event.key() == Key::Enter && search_match_count > 0 {
                                        event.prevent_default();
                                        let total = search_match_count;
                                        let ids = search_ids_for_keys.clone();
                                        let next = if event.modifiers().contains(Modifiers::SHIFT) {
                                            (search_idx() + total - 1) % total
                                        } else {
                                            (search_idx() + 1) % total
                                        };
                                        jump_to_search_match(next, ids, search_idx, feed_anchor, at_bottom);
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
                                button {
                                    class: "btn ghost sm chat-find-jump",
                                    r#type: "button",
                                    title: "Previous match (Shift+Enter)",
                                    "aria-label": "Previous match",
                                    onclick: move |_| {
                                        let total = search_match_count;
                                        if total == 0 {
                                            return;
                                        }
                                        let ids = search_ids_for_prev.clone();
                                        let prev = (search_idx() + total - 1) % total;
                                        jump_to_search_match(prev, ids, search_idx, feed_anchor, at_bottom);
                                    },
                                    Icon { name: "chevron-up", size: 13 }
                                }
                                button {
                                    class: "btn ghost sm chat-find-jump",
                                    r#type: "button",
                                    title: "Next match (Enter)",
                                    "aria-label": "Next match",
                                    onclick: move |_| {
                                        let total = search_match_count;
                                        if total == 0 {
                                            return;
                                        }
                                        let ids = search_ids_for_next.clone();
                                        let next = (search_idx() + 1) % total;
                                        jump_to_search_match(next, ids, search_idx, feed_anchor, at_bottom);
                                    },
                                    Icon { name: "chevron-down", size: 13 }
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
                                                session.write().replace_composer(text.clone());
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
                                                session.write().replace_composer(text.clone());
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
                                trace: view
                                    .row
                                    .turn
                                    .as_deref()
                                    .and_then(|turn| traced_turn(&session.read(), turn)),
                                streaming: streaming_message_id.as_deref()
                                    == Some(view.row.id.as_str()),
                                row: view.row,
                                class: view.class,
                                bot_meta: view.bot_meta,
                                media: view.media,
                                accepts,
                                handwritten: session.read().prefs.handwritten_notes,
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
                            if stopping {
                                span { class: "working",
                                    span { class: "working-text", "Stopping\u{2026}" }
                                    span { class: "working-dots", "aria-hidden": "true",
                                        span {}
                                        span {}
                                        span {}
                                    }
                                }
                            } else if let Some(data) = live_card.clone() {
                                LiveCard {
                                    session,
                                    data,
                                    failed_card: None,
                                    failed_can_act: false,
                                    failed_ondismiss: move |_| {},
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
                    } else if !steps.is_empty() && history_ready && !steps_attached {
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
                // Context used by the latest turn, visible in the chat itself
                // (not only in the model popover): a thin meter above the
                // composer, so a reply that is about to run out of room is
                // obvious before the next message is sent.
                if let Some(p) = chat_ctx_percent {
                    div {
                        class: "ctx-meter chat-ctx-meter",
                        role: "progressbar",
                        "aria-label": "Context used by the latest turn",
                        "aria-valuemin": "0",
                        "aria-valuemax": "100",
                        "aria-valuenow": "{p}",
                        title: "{chat_ctx_title}",
                        div { class: "ctx-fill", style: "width: {p}%;" }
                        span { class: "tiny muted", "Context {p}%" }
                    }
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
                        if let Some(data) = failed_live_card.clone() {
                            LiveCard {
                                session,
                                data,
                                failed_card: Some(card),
                                failed_can_act: can_send_again,
                                failed_ondismiss: {
                                    let failed = failed_task.clone();
                                    move |_| dismissed_fallback.set(failed.clone())
                                },
                            }
                        }
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
                            if last_audio_kept {
                                button {
                                    class: "btn ghost sm",
                                    r#type: "button",
                                    "aria-label": "Try transcribing the voice note again",
                                    onclick: move |_| session.write().retry_transcribe(),
                                    "Retry transcription"
                                }
                            }
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
                    if transcript_loading {
                        div { class: "composer-count", role: "status",
                            span { class: "spinner", "aria-hidden": "true" }
                            "Transcribing voice note\u{2026}"
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
                                        session.write().replace_composer(redacted);
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
                            "aria-describedby": if length_note.is_some() { Some("composer-count") } else { None },
                            "aria-activedescendant": active_descendant,
                            value: "{composer_text}",
                            placeholder: if accepts { format!("Message {active_name}…") } else { "This node does not accept turns".into() },
                            disabled: !accepts,
                            onfocus: move |_| composer_focused.set(true),
                            onblur: move |_| {
                                composer_focused.set(false);
                                let draft = composer_draft();
                                if session.peek().composer != draft {
                                    let mut session = session.write();
                                    session.composer = draft.clone();
                                    // Clone the conversation first: store()
                                    // takes &mut self, so the conversation
                                    // cannot be borrowed from the same guard
                                    // in the same call.
                                    let conversation = session.conversation.clone();
                                    session.drafts.store(&conversation, &draft);
                                    session.persist_composer_state();
                                }
                            },
                            oninput: move |event| {
                                let value = event.value();
                                confirm_regen.set(None);
                                composer_draft.set(value.clone());
                                recall_idx.set(None);
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
                                    let total = if mention_open {
                                        key_mention_bots.len() + key_mention_conns.len()
                                    } else {
                                        key_slash_actions.len()
                                            + key_slash_skills.len()
                                            + key_slash_tools.len()
                                    };
                                    if total == 0 {
                                        // No match: close the picker and let the
                                        // key behave as usual — Enter sends,
                                        // Tab moves focus — instead of swallowing it.
                                        mention.set(None);
                                        slash.set(None);
                                    } else {
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
                                                    session.write().replace_composer(next);
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
                                                            session.write().replace_composer(next);
                                                            slash.set(None);
                                                            slash_idx.set(0);
                                                            focus_composer();
                                                        }
                                                    }
                                                    Some(chat_compose::SlashPick::Skill(i)) => {
                                                        if let Some((id, _, _)) = key_slash_skills.get(i) {
                                                            let next = insert_slash_text(&composer_draft(), id);
                                                            composer_draft.set(next.clone());
                                                            session.write().replace_composer(next);
                                                            slash.set(None);
                                                            slash_idx.set(0);
                                                            focus_composer();
                                                        }
                                                    }
                                                    Some(chat_compose::SlashPick::Tool(i)) => {
                                                        if let Some(tool) = key_slash_tools.get(i) {
                                                            let next = insert_slash_text(&composer_draft(), tool);
                                                            composer_draft.set(next.clone());
                                                            session.write().replace_composer(next);
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
                                // Up/Down cycles through the owner's messages:
                                // Up to older ones, Down back toward newer,
                                // and past the newest the box is empty again.
                                if (event.key() == Key::ArrowUp && composer_draft().is_empty())
                                    || (event.key() == Key::ArrowDown && recall_idx().is_some())
                                {
                                    let texts: Vec<String> = session
                                        .read()
                                        .state
                                        .messages
                                        .iter()
                                        .filter(|message| message.role == Role::User)
                                        .map(|message| message.content.clone())
                                        .filter(|text| !text.trim().is_empty())
                                        .collect();
                                    let refs: Vec<&str> = texts.iter().map(String::as_str).collect();
                                    let (index, text) = if event.key() == Key::ArrowUp {
                                        chat_compose::recall_up(&refs, recall_idx())
                                    } else {
                                        chat_compose::recall_down(&refs, recall_idx())
                                    };
                                    if index.is_some() || event.key() == Key::ArrowDown {
                                        event.prevent_default();
                                        recall_idx.set(index);
                                        let text = text.unwrap_or_default();
                                        composer_draft.set(text.clone());
                                        session.write().replace_composer(text);
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
                                        recall_idx.set(None);
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
                                id: "composer-count",
                                role: "status",
                                "{note}"
                            }
                            if over_limit {
                                button {
                                    class: "btn ghost sm",
                                    r#type: "button",
                                    title: "Send the text as a message.txt file instead",
                                    "aria-label": "Attach the message text as a file",
                                    onclick: move |_| {
                                        session.write().attach_composer_as_file();
                                        composer_draft.set(session.read().composer.clone());
                                    },
                                    "Attach as file"
                                }
                            }
                        }
                        div { class: "composer-row",
                            NewMenu { session, overlay }
                            // 0.63: the model and thinking picker moved to
                            // the chat header (HeaderModelChip); the composer
                            // keeps only writing, files, voice and send.
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
                                            let _ = dioxus::document::eval(crate::ui_files::record_tick_stop_script()).await;
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
                                                    record_tick_start.set(record_tick());
                                                    let _ = dioxus::document::eval(crate::ui_files::record_tick_start_script()).await;
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
                                if recording() {
                                    span { class: "recording-timer", role: "timer",
                                        "Recording {record_elapsed / 60}:{record_elapsed % 60:02}"
                                    }
                                    button {
                                        class: "composer-mic",
                                        title: "Cancel recording",
                                        "aria-label": "Cancel recording",
                                        onclick: move |_| async move {
                                            let mut device = recorder.take();
                                            device.cancel().await;
                                            recorder.set(device);
                                            recording.set(false);
                                            let _ = dioxus::document::eval(crate::ui_files::record_tick_stop_script()).await;
                                        },
                                        Icon { name: "x", size: 16 }
                                    }
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
                            div { class: "pop-list", role: "listbox", "aria-label": "Mention a bot or connector",
                                if mention_total == 0 {
                                    div { class: "pop-empty", "No bots or connectors match." }
                                }
                                if !mention_bots.is_empty() {
                                    div { class: "pop-group", "Bots" }
                                    for (index, (id, name, avatar)) in mention_bots.iter().enumerate() {
                                        button {
                                            key: "{id}",
                                            id: "mention-item-{index}",
                                            class: if index == mention_active { "pop-item active" } else { "pop-item" },
                                            onclick: {
                                                let id = id.clone();
                                                move |_| {
                                                    let text = composer_draft();
                                                    let next = insert_mention_text(&text, &id);
                                                    composer_draft.set(next.clone());
                                                    session.write().replace_composer(next);
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
                                            id: "mention-item-{mention_bots.len() + offset}",
                                            class: if mention_bots.len() + offset == mention_active { "pop-item active" } else { "pop-item" },
                                            onclick: {
                                                let cid = cid.clone();
                                                move |_| {
                                                    let text = composer_draft();
                                                    let next = insert_mention_text(&text, &cid);
                                                    composer_draft.set(next.clone());
                                                    session.write().replace_composer(next);
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
                            div { class: "pop-list", role: "listbox", "aria-label": "Commands, skills and tools",
                                if slash_total == 0 {
                                    div { class: "pop-empty", "No commands, skills or tools match." }
                                }
                                if !slash_actions.is_empty() {
                                    div { class: "pop-group", "Actions (pick, then Enter)" }
                                    for (index, (command, description, _)) in slash_actions.iter().enumerate() {
                                        button {
                                            key: "{command}",
                                            id: "slash-item-{index}",
                                            class: if index == slash_active { "pop-item active" } else { "pop-item" },
                                            onclick: {
                                                let command = (*command).to_owned();
                                                move |_| {
                                                    let text = format!("/{command}");
                                                    composer_draft.set(text.clone());
                                                    session.write().replace_composer(text);
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
                                            id: "slash-item-{slash_actions.len() + index}",
                                            class: if slash_actions.len() + index == slash_active { "pop-item active" } else { "pop-item" },
                                            onclick: {
                                                let id = id.clone();
                                                move |_| {
                                                    let text = composer_draft();
                                                    let next = insert_slash_text(&text, &id);
                                                    composer_draft.set(next.clone());
                                                    session.write().replace_composer(next);
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
                                            id: "slash-item-{slash_actions.len() + slash_skills.len() + offset}",
                                            class: if slash_actions.len() + slash_skills.len() + offset == slash_active { "pop-item active" } else { "pop-item" },
                                            onclick: {
                                                let tool = tool.to_string();
                                                move |_| {
                                                    let text = composer_draft();
                                                    let next = insert_slash_text(&text, &tool);
                                                    composer_draft.set(next.clone());
                                                    session.write().replace_composer(next);
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
    trace: Option<serabot_desktop::turn_trace::TurnTrace>,
    class: String,
    bot_meta: Option<(String, bool)>,
    media: Vec<serabot_desktop::LibraryEntry>,
    accepts: bool,
    handwritten: bool,
    /// This row is the reply still streaming in: unfinished markdown
    /// renders as plain text and code blocks hide their Copy button.
    streaming: bool,
    mut copied: Signal<Option<String>>,
    mut confirm_regen: Signal<Option<String>>,
    mut task_added_ids: Signal<HashSet<String>>,
    mut composer_draft: Signal<String>,
) -> Element {
    rsx! {
        div { class: "{class}", "data-row-id": "{row.id}",
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
                if let Some(trace) = trace {
                    TurnTraceBlock { session, trace }
                }
                div { class: "msg-body", title: "{row.stamp}",
                    MarkdownBody { message_id: row.id.clone(), content: row.content.clone(), handwritten, streaming }
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
                                    session.write().replace_composer(next);
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

#[component]
pub(super) fn ModelBar(
    session: Signal<Session>,
    #[props(default = false)] compact: bool,
) -> Element {
    // Picker items: (value, label). "auto" first, then the catalog entries
    // (or the static provider model lists when the catalog is empty).
    let (items, current, allowed, saved, rows, kinds, provider_accounts) = {
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
    // Only "auto" left: the server offered no concrete model.
    let no_models = session.read().state.catalog.is_some() && items.len() <= 1;
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
    let current_label = {
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
    // 0.73: context fill of the latest turn (turn.context), shown as a thin
    // meter in the model bar.
    let (ctx_percent, ctx_title) = {
        let s = session.read();
        match s.state.turns.latest_context() {
            Some(c) if c.percent.is_some() => {
                let window = c
                    .window
                    .map(|w| w.to_string())
                    .unwrap_or_else(|| "unknown".to_owned());
                (
                    c.percent,
                    format!(
                        "~{} chars of {} context used by the latest turn",
                        c.prompt_chars_est, window
                    ),
                )
            }
            _ => (None, String::new()),
        }
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
                                let account = picked_account_for(&session.read(), &picked_account.read(), item_value);
                                let mut session = session.write();
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
                            let position = items.iter().position(|(v, _)| v == value.as_str()).unwrap_or(0);
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
                            if let Some((brand, name)) = header {
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
                                    move |_| {
                                        let account = picked_account_for(&session.read(), &picked_account.read(), &item_value);
                                        let mut session = session.write();
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
            if let Some(p) = ctx_percent {
                div {
                    class: "ctx-meter",
                    role: "progressbar",
                    "aria-label": "Context used by the latest turn",
                    "aria-valuemin": "0",
                    "aria-valuemax": "100",
                    "aria-valuenow": "{p}",
                    title: "{ctx_title}",
                    div { class: "ctx-fill", style: "width: {p}%;" }
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

/// The trace a finished reply shows, or `None` while its turn is still the
/// live one (the live block at the foot of the feed draws it then).
fn traced_turn(state: &Session, turn: &str) -> Option<serabot_desktop::turn_trace::TurnTrace> {
    let trace = state.state.turns.get(turn)?;
    let running = state
        .state
        .task
        .as_ref()
        .is_some_and(|task| !task.status.terminal());
    if running
        && !trace.activity.task_id.is_empty()
        && trace.activity.task_id == state.state.activity.task_id
    {
        return None;
    }
    Some(trace.clone())
}

/// What one turn did, inside the reply it produced (0.61): its tool steps
/// and the memories it used and saved, collapsed to one line. A step that
/// failed or waits for approval opens it, so the trace cannot hide either.
#[component]
fn TurnTraceBlock(
    mut session: Signal<Session>,
    trace: serabot_desktop::turn_trace::TurnTrace,
) -> Element {
    let summary = trace.summary();
    let attention = trace.activity.needs_attention();
    let dropped = trace.activity.dropped_note();
    let memory = trace.memory.clone();
    rsx! {
        details { class: "turn-trace", open: attention,
            summary { class: "turn-trace-summary",
                Icon { name: "chevron-right", size: 12 }
                span { "{summary}" }
            }
            if !trace.activity.steps.is_empty() {
                div { class: "activity-steps",
                    for step in trace.activity.steps.iter().cloned() {
                        ActivityRow { session, step, key: "{step.call_id}" }
                    }
                    if let Some(note) = dropped {
                        p { class: "activity-more", "{note}" }
                    }
                }
            }
            if !memory.is_empty() {
                div { class: "turn-memory", role: "group", "aria-label": "Memories this reply used and saved",
                    if !memory.recalled.is_empty() {
                        p { class: "turn-memory-head", "Used" }
                        ul { class: "turn-memory-list",
                            for item in memory.recalled.iter().cloned() {
                                TurnMemoryItem { key: "u-{item.id}", item }
                            }
                        }
                    }
                    if !memory.saved.is_empty() {
                        p { class: "turn-memory-head", "Remembered" }
                        ul { class: "turn-memory-list",
                            for item in memory.saved.iter().cloned() {
                                TurnMemoryItem { key: "s-{item.id}", item }
                            }
                        }
                    }
                    button {
                        class: "btn ghost sm",
                        r#type: "button",
                        "aria-label": "Open memory to review, edit or forget these facts",
                        onclick: move |_| session.write().open_section(Section::Memory),
                        "Review in Memory"
                    }
                }
            }
        }
    }
}

#[component]
fn TurnMemoryItem(item: serabot_desktop::turn_trace::MemoryItem) -> Element {
    let text = if item.excerpt.is_empty() {
        "A saved fact (text withheld by policy)".to_owned()
    } else {
        item.excerpt.clone()
    };
    rsx! {
        li { class: "turn-memory-item",
            Icon { name: "memory", size: 12 }
            span { class: "turn-memory-text", "{text}" }
            if item.pending {
                span { class: "chip warn", "held for review" }
            } else if item.origin == "explicit" {
                span { class: "chip", "you saved" }
            }
        }
        for old in item.replaces.iter() {
            li { class: "turn-memory-item replaced",
                span { class: "turn-memory-replaced", "Replaces: " }
                s { class: "turn-memory-text", "{old}" }
            }
        }
    }
}

/// The model chip in a one-to-one chat's header (0.63; 2-column card in 0.74.13):
/// the current model at a glance; a click opens the 2-column picker (provider
/// rail on the left, search + account switcher + model list + pinned thinking
/// bar on the right).
#[component]
pub(super) fn HeaderModelChip(session: Signal<Session>) -> Element {
    let mut open = use_signal(|| false);
    let mut selected_provider = use_signal(|| String::from("__all__"));
    let mut query = use_signal(String::new);
    let mut active_idx = use_signal(|| 0usize);
    let picked_account = use_signal(BTreeMap::<String, String>::new);

    let (
        label,
        current,
        current_provider,
        rows,
        kinds,
        provider_accounts,
        thinking_levels,
        thinking_now,
    ) = {
        let state = session.read();
        let label = match state.state.catalog.as_ref().map(|c| &c.selection) {
            Some(selection) if !selection.is_auto() => short_model(&selection.model),
            _ => "Auto".to_owned(),
        };
        let mut current = "auto".to_owned();
        let mut current_provider = String::new();
        let mut rows: Vec<provider_view::PickerRow> = Vec::new();
        if let Some(catalog) = state.state.catalog.as_ref() {
            if !catalog.selection.is_auto() {
                current = format!("{}|{}", catalog.selection.provider, catalog.selection.model);
                current_provider = catalog.selection.provider.clone();
            }
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
        }
        let kinds: BTreeMap<String, String> = state
            .state
            .providers
            .iter()
            .map(|provider| (provider.id.clone(), provider.kind.clone()))
            .collect();
        let options = state.route_options();
        let mut provider_accounts: BTreeMap<String, Vec<(String, String)>> = BTreeMap::new();
        for row in &rows {
            if !provider_accounts.contains_key(&row.provider) {
                provider_accounts.insert(
                    row.provider.clone(),
                    route_chip::picker_accounts(&options, &row.provider),
                );
            }
        }
        let draft = &state.state.draft;
        let draft_levels = if !draft.provider.is_empty() && !draft.model.is_empty() {
            options.thinking_levels(&draft.provider, &draft.model)
        } else {
            Vec::new()
        };
        let (saved_levels, saved_level) = state
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
        let thinking_levels = if !draft_levels.is_empty() {
            draft_levels
        } else {
            saved_levels
        };
        let thinking_now = if thinking_levels.iter().any(|l| l == &saved_level) {
            saved_level
        } else {
            String::new()
        };
        (
            label,
            current,
            current_provider,
            rows,
            kinds,
            provider_accounts,
            thinking_levels,
            thinking_now,
        )
    };

    let segments = route_chip::thinking_segments(&thinking_levels, &thinking_now);

    // Build provider rail entries in display order.
    let mut rail_providers: Vec<(String, String, String, usize)> = Vec::new();
    for row in &rows {
        if let Some(existing) = rail_providers.iter_mut().find(|p| p.0 == row.provider) {
            existing.3 += 1;
        } else {
            let kind = kinds
                .get(&row.provider)
                .cloned()
                .unwrap_or_else(|| row.provider.clone());
            let brand = if branding::brand_label(&row.provider).is_some() {
                row.provider.clone()
            } else {
                kind.clone()
            };
            let name =
                provider_view::display_name(&row.provider, &kind, branding::brand_label(&kind));
            rail_providers.push((row.provider.clone(), brand, name, 1));
        }
    }

    let total_count = rows.len() + 1;
    let active_prov = {
        let cur = selected_provider();
        if cur == "__all__" || rail_providers.iter().any(|p| p.0 == cur) {
            cur
        } else {
            String::from("__all__")
        }
    };

    let needle = query().trim().to_lowercase();
    #[derive(Clone)]
    enum HeaderModelEntry {
        Auto,
        Model(provider_view::PickerRow),
    }
    impl HeaderModelEntry {
        fn value(&self) -> String {
            match self {
                Self::Auto => "auto".to_owned(),
                Self::Model(row) => row.value.clone(),
            }
        }
    }

    let mut filtered: Vec<HeaderModelEntry> = Vec::new();
    if active_prov == "__all__" && (needle.is_empty() || "auto approved route".contains(&needle)) {
        filtered.push(HeaderModelEntry::Auto);
    }
    // Typing ranks by how well the words match (letters may be skipped: "son5" finds
    // "claude-sonnet-5"). With an empty box, starred models lead, then recent ones.
    let (favorites, recent) = {
        let s = session.read();
        (
            s.prefs.favorite_models.clone(),
            s.prefs.recent_models.clone(),
        )
    };
    let mut scored: Vec<(
        i32,
        serabot_desktop::picker_search::Standing,
        usize,
        provider_view::PickerRow,
    )> = Vec::new();
    for (idx, row) in rows.iter().enumerate() {
        if active_prov != "__all__" && row.provider != active_prov {
            continue;
        }
        let score = if needle.is_empty() {
            Some(0)
        } else {
            serabot_desktop::picker_search::best_score(
                &needle,
                &[(&row.title, 3), (&row.model, 2), (&row.provider, 1)],
            )
        };
        let Some(score) = score else {
            continue;
        };
        let standing = if needle.is_empty() {
            serabot_desktop::picker_search::standing(&row.value, &favorites, &recent)
        } else {
            serabot_desktop::picker_search::Standing::Other
        };
        scored.push((score, standing, idx, row.clone()));
    }
    scored.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)).then(a.2.cmp(&b.2)));
    for (_, _, _, row) in scored {
        filtered.push(HeaderModelEntry::Model(row));
    }

    let active = if filtered.is_empty() {
        0
    } else {
        active_idx().min(filtered.len() - 1)
    };
    let keyed_values: Vec<String> = filtered.iter().map(HeaderModelEntry::value).collect();

    let account_provider = if active_prov != "__all__" {
        Some(active_prov.clone())
    } else if !current_provider.is_empty() {
        Some(current_provider.clone())
    } else {
        None
    };
    let account_choices = account_provider
        .as_deref()
        .and_then(|prov| provider_accounts.get(prov))
        .filter(|choices| !choices.is_empty())
        .cloned();

    rsx! {
        div { class: "header-model",
            button {
                class: if open() { "btn ghost sm header-model-chip open" } else { "btn ghost sm header-model-chip" },
                r#type: "button",
                title: "Model and thinking for this chat",
                "aria-haspopup": "dialog",
                "aria-expanded": if open() { "true" } else { "false" },
                onclick: move |_| {
                    if !open() {
                        query.set(String::new());
                        active_idx.set(0);
                    }
                    open.set(!open());
                },
                onkeydown: move |event: KeyboardEvent| {
                    if event.key() == Key::Escape && open() {
                        event.prevent_default();
                        event.stop_propagation();
                        open.set(false);
                    }
                },
                Icon { name: "sparkles", size: 14 }
                span { class: "header-model-label", "{label}" }
                Icon { name: "chevron-down", size: 12 }
            }
            if open() {
                div {
                    class: "header-model-scrim escape-close",
                    onclick: move |_| open.set(false),
                }
                div {
                    class: "header-model-pop model-pop pop open",
                    role: "dialog",
                    "aria-label": "Model and thinking",
                    onkeydown: move |event: KeyboardEvent| {
                        let count = keyed_values.len();
                        match event.key() {
                            Key::Escape => {
                                event.prevent_default();
                                event.stop_propagation();
                                open.set(false);
                            }
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
                                if let Some(item_value) = keyed_values.get(active_idx().min(count.saturating_sub(1))) {
                                    let account = picked_account_for(&session.read(), &picked_account.read(), item_value);
                                    let mut s = session.write();
                                    set_model_draft_with(&mut s, item_value, account);
                                    let parsed = s.state.draft.clone();
                                    if s.state.catalog.as_ref().is_some_and(|catalog| catalog.allows(&parsed)) {
                                        s.apply_model(parsed);
                                    }
                                    open.set(false);
                                    query.set(String::new());
                                }
                            }
                            _ => {}
                        }
                    },
                    div { class: "model-pop-grid",
                        // Left column: Providers rail
                        div { class: "model-pop-rail", role: "tablist", "aria-label": "Providers",
                            button {
                                key: "__all__",
                                r#type: "button",
                                role: "tab",
                                "aria-selected": if active_prov == "__all__" { "true" } else { "false" },
                                class: if active_prov == "__all__" { "model-rail-item active" } else { "model-rail-item" },
                                onclick: move |_| {
                                    selected_provider.set(String::from("__all__"));
                                    active_idx.set(0);
                                },
                                span { class: "model-rail-badge",
                                    Icon { name: "sparkles", size: 12 }
                                }
                                span { class: "model-rail-name", "All" }
                                span { class: "model-rail-count", "{total_count}" }
                            }
                            for (prov_id, brand, name, count) in rail_providers {
                                button {
                                    key: "{prov_id}",
                                    r#type: "button",
                                    role: "tab",
                                    "aria-selected": if active_prov == prov_id { "true" } else { "false" },
                                    class: if active_prov == prov_id { "model-rail-item active" } else { "model-rail-item" },
                                    onclick: {
                                        let prov_id = prov_id.clone();
                                        move |_| {
                                            selected_provider.set(prov_id.clone());
                                            active_idx.set(0);
                                        }
                                    },
                                    BrandLogo { brand, size: 16 }
                                    span { class: "model-rail-name", "{name}" }
                                    span { class: "model-rail-count", "{count}" }
                                }
                            }
                        }
                        // Right body: Search + Accounts + Scrollable Models + Pinned Thinking Footer
                        div { class: "model-pop-body",
                            div { class: "model-pop-search",
                                input {
                                    class: "input",
                                    placeholder: "Search models\u{2026}",
                                    value: "{query}",
                                    oninput: move |event| {
                                        query.set(event.value());
                                        active_idx.set(0);
                                    },
                                }
                                if let (Some(prov), Some(choices)) = (account_provider.clone(), account_choices) {
                                    div { class: "model-pop-accounts",
                                        PickAccounts {
                                            session,
                                            provider: prov,
                                            choices,
                                            compact: true,
                                            picked: picked_account,
                                        }
                                    }
                                }
                            }
                            div { class: "model-pop-list",
                                if filtered.is_empty() {
                                    div { class: "pop-empty", "No models match." }
                                }
                                for (pos, entry) in filtered.into_iter().enumerate() {
                                    {
                                        let item_value = entry.value();
                                        let is_selected = item_value == current;
                                        let item_class = match (pos == active, is_selected) {
                                            (true, true) => "model-pop-item active selected",
                                            (true, false) => "model-pop-item active",
                                            (false, true) => "model-pop-item selected",
                                            (false, false) => "model-pop-item",
                                        };
                                        let fav_key = item_value.clone();
                                        let is_fav = favorites.contains(&item_value);
                                        let is_recent = !is_fav && recent.contains(&item_value);
                                        rsx! {
                                            button {
                                                key: "{item_value}",
                                                r#type: "button",
                                                class: "{item_class}",
                                                onclick: {
                                                    let item_value = item_value.clone();
                                                    move |_| {
                                                        let account = picked_account_for(&session.read(), &picked_account.read(), &item_value);
                                                        let mut s = session.write();
                                                        set_model_draft_with(&mut s, &item_value, account);
                                                        let parsed = s.state.draft.clone();
                                                        if s.state.catalog.as_ref().is_some_and(|catalog| catalog.allows(&parsed)) {
                                                            s.apply_model(parsed);
                                                        }
                                                        open.set(false);
                                                        query.set(String::new());
                                                    }
                                                },
                                                match entry {
                                                    HeaderModelEntry::Auto => rsx! {
                                                        div { class: "model-row",
                                                            span { class: "model-title", "Auto" }
                                                            div { class: "model-meta",
                                                                span { class: "model-id", "Approved route" }
                                                            }
                                                        }
                                                    },
                                                    HeaderModelEntry::Model(row) => rsx! {
                                                        div { class: "model-row",
                                                            div { class: "model-title-line",
                                                                span { class: "model-title", "{row.title}" }
                                                                if is_recent {
                                                                    span { class: "chip muted", "Recent" }
                                                                }
                                                                span {
                                                                    class: if is_fav { "model-fav on" } else { "model-fav" },
                                                                    role: "button",
                                                                    title: if is_fav { "Remove from favorites" } else { "Add to favorites" },
                                                                    "aria-label": if is_fav { "Remove from favorites" } else { "Add to favorites" },
                                                                    "aria-pressed": if is_fav { "true" } else { "false" },
                                                                    onclick: move |event: MouseEvent| {
                                                                        event.stop_propagation();
                                                                        session.write().toggle_favorite_model(&fav_key);
                                                                    },
                                                                    Icon { name: "star", size: 13 }
                                                                }
                                                            }
                                                            div { class: "model-meta",
                                                                if active_prov == "__all__" {
                                                                    span { class: "chip muted", "{human_provider(&row.provider)}" }
                                                                }
                                                                if row.title != row.model {
                                                                    span { class: "model-id", "{row.model}" }
                                                                }
                                                                if let Some(context) = row.context.clone() {
                                                                    span { class: "chip muted", "{context}" }
                                                                }
                                                                for tag in row.tags.clone() {
                                                                    span { class: "chip", "{tag}" }
                                                                }
                                                            }
                                                        }
                                                    },
                                                }
                                                if is_selected {
                                                    Icon { name: "check", size: 14 }
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                            div { class: "model-pop-footer",
                                if !segments.is_empty() {
                                    span { class: "model-pop-thinking-label", "Thinking" }
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
                                } else {
                                    span { class: "tiny muted", "Applies from your next message." }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

/// "claude-sonnet-5-5" -> "claude-sonnet-5-5"; a long id keeps its tail.
fn short_model(model: &str) -> String {
    let name = model.rsplit('/').next().unwrap_or(model);
    if name.chars().count() > 26 {
        let tail: String = name
            .chars()
            .rev()
            .take(24)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();
        format!("\u{2026}{tail}")
    } else {
        name.to_owned()
    }
}
