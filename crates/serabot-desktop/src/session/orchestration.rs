//! Heavy work in the open chat (v0.50): the runs list, per-run Stop and Retry,
//! the queue of messages typed while a reply runs, `@bot` routing and "Ask
//! main". The rules live in `runs` and `chat_compose`; this only wires them to
//! the worker.

use super::*;
use crate::chat_compose::{self, QueueRefusal};

/// Least time between two reads of the runs list, however many handoff events
/// arrive: the events only mark the list stale.
const RUNS_MIN_INTERVAL: Duration = Duration::from_millis(750);

impl Session {
    /// Read the open chat's subagent runs once per conversation and again,
    /// at most every `RUNS_MIN_INTERVAL`, after a handoff event.
    pub(super) fn poll_runs(&mut self) {
        if !self.state.connected
            || !self.state.history_loaded
            || self.state.unknown_outcome
            || !self.ready()
            || !(self.state.runs.stale || !self.state.runs.asked)
            || self
                .runs_last_read
                .is_some_and(|at| at.elapsed() < RUNS_MIN_INTERVAL)
        {
            return;
        }
        self.state.runs.asked = true;
        self.state.runs.stale = false;
        self.runs_last_read = Some(Instant::now());
        self.start_request(Operation::Handoffs);
    }

    /// Whether a reply, pending send, or subagent is running in this chat:
    /// the Stop button stays on screen for any of them.
    pub fn any_run_active(&self) -> bool {
        self.main_run_active()
            || crate::runs::active_count(&self.state.runs.rows) > 0
            || self.has_optimistic_pending()
    }

    /// Whether this chat's own turn is running.
    pub fn main_run_active(&self) -> bool {
        self.state
            .task
            .as_ref()
            .is_some_and(|task| !task.status.terminal())
    }

    /// The Stop button and Esc: end this chat's own reply, pending send, or
    /// any running subagents by stopping the conversation's turns cleanly.
    pub fn stop_turn_or_runs(&mut self) {
        if !self.any_run_active() {
            return;
        }
        self.state.error = None;
        self.stop_turn();
        self.run_action_done();
    }

    /// The per-run Stop.
    pub fn stop_run(&mut self, id: String) {
        if !valid_id(&id) {
            return;
        }
        self.state.error = None;
        self.start_request(Operation::StopRun { id });
    }

    /// The per-run Retry: the server resumes the child on its own chain. A run
    /// with an unknown effect is refused here as well as by the server.
    pub fn retry_run(&mut self, id: String) {
        if !valid_id(&id) {
            return;
        }
        let allowed = self
            .state
            .runs
            .rows
            .iter()
            .find(|row| row.id == id)
            .is_some_and(crate::runs::RunRow::can_retry);
        if !allowed {
            self.toast = Some("That run cannot be retried from here.".into());
            return;
        }
        self.state.error = None;
        self.start_request(Operation::RetryRun { id, route: None });
    }

    /// Open or close a collapsed group of runs.
    pub fn toggle_run_group(&mut self, key: String) {
        if !self.runs_open.remove(&key) {
            self.runs_open.insert(key);
        }
    }

    pub(super) fn run_action_done(&mut self) {
        // The server's answer is the truth: read the list again.
        self.state.runs.stale = true;
        self.runs_last_read = None;
    }

    pub(super) fn run_action_failed(&mut self, error: &ApiError) {
        self.state.runs.stale = true;
        self.runs_last_read = None;
        self.toast = Some(if error.unknown_outcome() {
            // A retry may have started: never press it again blindly.
            "That change may have reached the server. Check the run below before trying again."
                .to_owned()
        } else {
            match error {
                ApiError::Http(404) => "That run is gone.".to_owned(),
                ApiError::Http(409 | 422) | ApiError::Coded { .. } | ApiError::Rejected(_) => {
                    "That run cannot be changed now. Its state has been refreshed.".to_owned()
                }
                other => format!("Could not change that run: {other}"),
            }
        });
    }

    /// Bot ids an `@mention` may route to: the owner's enabled bots.
    fn delegate_bots(&self) -> Vec<String> {
        self.state
            .listed_bots
            .iter()
            .filter(|bot| bot.enabled)
            .map(|bot| bot.id.clone())
            .collect()
    }

    /// The mention routing for a text typed in this chat.
    pub(super) fn route_for(&self, text: &str) -> chat_compose::Routed {
        let room = matches!(self.conversation_kind(), Some("group" | "room"));
        let current = self.conversation_bot(&self.conversation);
        chat_compose::route_mention(text, &self.delegate_bots(), current.as_deref(), room)
    }

    /// Enter in the composer: send now, or queue behind the running reply.
    pub fn send_or_queue(&mut self) {
        if self.main_run_active() {
            self.queue_composer();
        } else {
            self.submit();
        }
    }

    /// Move the composer text into the queue. Attachments wait for their own
    /// send: they are uploaded to the conversation, not to the queue.
    pub fn queue_composer(&mut self) {
        if !self.composer_files.is_empty() || !self.upload_queue.is_empty() || self.uploading {
            self.toast = Some("Files can't be queued. Send them when the reply is done.".into());
            return;
        }
        let routed = self.route_for(&self.composer);
        // The owner's own words are queued; routing is decided at send time too.
        match self.queue.push(&self.composer, routed.target) {
            Ok(_) => {
                self.composer.zeroize();
                self.composer = String::new();
                self.drafts.clear(&self.conversation);
            }
            Err(refusal) => self.toast = Some(refusal.message().into()),
        }
    }

    pub fn queue_delete(&mut self, id: u64) {
        self.queue.remove(id);
    }

    pub fn queue_move(&mut self, id: u64, up: bool) {
        if up {
            self.queue.move_up(id);
        } else {
            self.queue.move_down(id);
        }
    }

    /// Start editing a queued message; the queue holds meanwhile.
    pub fn queue_begin_edit(&mut self, id: u64) -> Option<String> {
        self.queue.begin_edit(id)
    }

    pub fn queue_commit_edit(&mut self, id: u64, text: &str) -> Result<(), QueueRefusal> {
        self.queue.commit_edit(id, text)
    }

    pub fn queue_cancel_edit(&mut self) {
        self.queue.cancel_edit();
    }

    /// Steer: stop the running reply and send this message next.
    pub fn queue_steer(&mut self, id: u64) {
        if self.queue.steer(id) && self.main_run_active() {
            self.stop_turn();
        }
    }

    /// The queue does not feed a failing route: after a failed turn it waits
    /// for the owner (retry, switch route, or send one with the bolt).
    pub fn queue_paused(&self) -> bool {
        self.state
            .task
            .as_ref()
            .is_some_and(|task| task.status == TaskStatus::Failed)
            && self.queue.items().first().is_some_and(|item| !item.steer)
    }

    /// Send the next queued message once the chat is idle. Called every tick.
    pub(super) fn pump_queue(&mut self) {
        if self.queue.is_empty() {
            return;
        }
        let running = self.main_run_active();
        if running && self.queue.wants_interrupt() {
            self.stop_turn();
            return;
        }
        if running
            || self.queue_paused()
            || !self.state.can_send()
            || !self.ready()
            || self.uploading
            || !self.upload_queue.is_empty()
        {
            return;
        }
        let Some(next) = self.queue.take_next(false) else {
            return;
        };
        let routed = self.route_for(&next.text);
        self.dispatch_text(next.text, routed.target.map(|bot| (bot, routed.text)));
    }

    /// Put an owner message in the transcript and send it. `delegate` is the
    /// bot that answers and the text it gets, when a mention routed it.
    pub(super) fn dispatch_text(&mut self, shown: String, delegate: Option<(String, String)>) {
        self.state.error = None;
        self.state.chat_failure = None;
        self.state.held_draft = None;
        self.state.activity.clear();
        self.regenerating = false;
        self.state.messages.push(Message {
            id: format!("opt-{}", Instant::now().elapsed().as_nanos()),
            role: Role::User,
            content: shown.clone(),
            created_at: "Just now".into(),
            name: None,
            bot: None,
            attachments: Vec::new(),
            limit_reached: false,
            turn_id: None,
        });
        match delegate {
            Some((bot, text)) => self.start_request(Operation::SubmitTo {
                text: Zeroizing::new(text),
                bot,
            }),
            None => self.start_request(Operation::Submit(Zeroizing::new(shown))),
        }
    }

    /// "Ask main" on a failed turn: the orchestrator is asked to take over.
    /// The text is fixed words plus reason chips, never the provider's text.
    pub fn ask_main(&mut self) {
        if !self.state.connected
            || !self.state.history_loaded
            || !self.ready()
            || self.main_run_active()
        {
            return;
        }
        let Some(main) = self
            .state
            .listed_bots
            .iter()
            .find(|bot| bot.role == "orchestrator" && bot.enabled)
            .map(|bot| bot.id.clone())
        else {
            self.toast = Some("The main assistant is not available.".into());
            return;
        };
        let reasons = crate::chat_failure::recent_reasons(&self.state.route_events);
        let text = crate::chat_failure::ask_main_text(&reasons);
        let room = matches!(self.conversation_kind(), Some("group" | "room"));
        let current = self.conversation_bot(&self.conversation);
        let delegate =
            (!room && current.as_deref() != Some(main.as_str())).then(|| (main, text.clone()));
        self.dispatch_text(text, delegate);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session() -> Session {
        let mut session = Session::new();
        session.state.connected = true;
        session
    }

    #[test]
    fn a_message_typed_while_a_reply_runs_is_queued_not_sent() {
        let mut session = session();
        session.state.task = Some(crate::Task {
            id: "t1".into(),
            kind: "turn".into(),
            bot: "main".into(),
            status: TaskStatus::Running,
        });
        session.composer = "and then summarize it".into();
        session.send_or_queue();
        assert_eq!(session.queue.len(), 1);
        assert!(
            session.composer.is_empty(),
            "the box is free for the next one"
        );
        assert!(session.pending.is_none(), "nothing went to the server");
        assert!(session.state.messages.is_empty());
    }

    #[test]
    fn files_are_not_queued_and_a_blank_box_queues_nothing() {
        let mut session = session();
        session.composer = "   ".into();
        session.queue_composer();
        assert!(session.queue.is_empty());
        assert!(session.toast.is_some());

        session.toast = None;
        session.composer = "with a file".into();
        session.composer_files.push(crate::AttachmentInfo {
            id: "a1".into(),
            filename: "n.txt".into(),
            mime: "text/plain".into(),
            size_bytes: 1,
            is_image: false,
        });
        session.queue_composer();
        assert!(session.queue.is_empty());
        assert!(
            session
                .toast
                .as_deref()
                .unwrap()
                .contains("Files can't be queued")
        );
        assert_eq!(session.composer, "with a file", "the draft is kept");
    }

    #[test]
    fn the_pump_waits_for_the_running_reply_then_stops_for_a_steered_message() {
        let mut session = session();
        session.state.task = Some(crate::Task {
            id: "t1".into(),
            kind: "turn".into(),
            bot: "main".into(),
            status: TaskStatus::Running,
        });
        let first = session.queue.push("one", None).unwrap();
        session.queue.push("two", None).unwrap();
        session.pump_queue();
        assert_eq!(session.queue.len(), 2, "held while the reply runs");
        assert!(!session.stop_queued && session.pending.is_none());

        // There is no worker here to carry the stop, so the visible effect is
        // the queue: the steered message is first and asks to interrupt.
        let second = session.queue.items()[1].id;
        session.queue_steer(second);
        assert_eq!(session.queue.items()[0].id, second);
        assert!(session.queue.wants_interrupt());
        assert_ne!(first, second);
    }

    #[test]
    fn a_failed_turn_pauses_the_queue_until_the_owner_steers() {
        let mut session = session();
        session.state.task = Some(crate::Task {
            id: "t1".into(),
            kind: "turn".into(),
            bot: "main".into(),
            status: TaskStatus::Failed,
        });
        let id = session.queue.push("one", None).unwrap();
        assert!(session.queue_paused());
        session.pump_queue();
        assert_eq!(session.queue.len(), 1, "not sent into a failing route");
        // Choosing it by hand lifts the pause for that message.
        session.queue_steer(id);
        assert!(!session.queue_paused());
    }

    #[test]
    fn an_edit_hold_keeps_the_queue_from_sending() {
        let mut session = session();
        let id = session.queue.push("one", None).unwrap();
        assert_eq!(session.queue_begin_edit(id).as_deref(), Some("one"));
        session.pump_queue();
        assert_eq!(session.queue.len(), 1);
        session.queue_cancel_edit();
        assert!(session.queue.editing().is_none());
    }

    #[test]
    fn retry_refuses_a_run_the_sidebar_may_not_replay() {
        let mut session = session();
        session.state.runs.set_ledger(vec![crate::runs::Handoff {
            id: "h1".into(),
            child_task_id: "c1".into(),
            parent_task_id: "p".into(),
            to_bot: "analyst".into(),
            status: "failed_resumable".into(),
            effects_unknown: true,
            ..crate::runs::Handoff::default()
        }]);
        session.retry_run("h1".into());
        assert!(session.pending.is_none() && session.backlog.is_empty());
        assert!(session.toast.is_some());
        // Unknown ids and bad ids are ignored.
        session.toast = None;
        session.retry_run("../x".into());
        assert!(session.toast.is_none());
    }

    #[test]
    fn the_stop_button_stays_while_only_a_subagent_runs() {
        let mut session = session();
        assert!(!session.any_run_active());
        session.state.runs.set_ledger(vec![crate::runs::Handoff {
            id: "h1".into(),
            child_task_id: "c1".into(),
            parent_task_id: "p".into(),
            to_bot: "analyst".into(),
            status: "running".into(),
            ..crate::runs::Handoff::default()
        }]);
        assert!(session.any_run_active());
        assert!(!session.main_run_active());
    }

    #[test]
    fn a_mention_of_a_known_bot_routes_and_others_do_not() {
        let mut session = session();
        let bot = |id: &str, enabled: bool| crate::BotSummary {
            id: id.into(),
            role: "specialist".into(),
            enabled,
            live: enabled,
            model: None,
            display_name: None,
            title: None,
            description: None,
            avatar: None,
            avatar_version: None,
            tools: Vec::new(),
            skills: Vec::new(),
            require_approval: Vec::new(),
        };
        session.state.listed_bots = vec![bot("analyst", true), bot("maker", false)];
        let routed = session.route_for("@analyst check the totals");
        assert_eq!(routed.target.as_deref(), Some("analyst"));
        assert_eq!(routed.text, "check the totals");
        assert!(
            session.route_for("@maker hi").target.is_none(),
            "disabled bot"
        );
        assert!(session.route_for("@ghost hi").target.is_none());
    }
}
