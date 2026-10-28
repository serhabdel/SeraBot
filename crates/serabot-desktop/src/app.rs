use eframe::egui::{self, Color32, RichText};
use secrecy::SecretString;
use serabot_desktop::{
    ApiError, Role, TaskStatus,
    state::State,
    valid_id, valid_text, valid_token, validate_endpoint,
    worker::{Credentials, Job, Operation, OperationKind, Worker},
};
use std::{
    sync::Arc,
    time::{Duration, Instant},
};
use zeroize::{Zeroize, Zeroizing};

const TEAL: Color32 = Color32::from_rgb(100, 213, 194);
const MUTED: Color32 = Color32::from_rgb(163, 174, 187);
const WARNING: Color32 = Color32::from_rgb(245, 193, 117);
const REQUEST_GAP: Duration = Duration::from_secs(3);
const POLL_GAP: Duration = Duration::from_secs(5);

pub struct Desktop {
    state: State,
    worker: Option<Worker>,
    credentials: Option<Arc<Credentials>>,
    server: String,
    token: String,
    conversation: String,
    composer: String,
    connection_open: bool,
    pending: Option<OperationKind>,
    scheduled: Option<(Instant, Operation)>,
    next_request: Instant,
    refreshing: bool,
    reviewed_unknown: bool,
    confirmed_egress: bool,
    poll_history: bool,
    poll_paused: bool,
}

impl Desktop {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let mut visuals = egui::Visuals::dark();
        visuals.panel_fill = Color32::from_rgb(23, 27, 33);
        visuals.window_fill = Color32::from_rgb(29, 34, 41);
        visuals.extreme_bg_color = Color32::from_rgb(17, 21, 26);
        visuals.selection.bg_fill = Color32::from_rgb(39, 92, 85);
        visuals.selection.stroke = egui::Stroke::new(1.0_f32, TEAL);
        cc.egui_ctx.set_visuals(visuals);
        cc.egui_ctx.style_mut(|style| {
            style.spacing.item_spacing = egui::vec2(10.0, 8.0);
            style.spacing.button_padding = egui::vec2(12.0, 6.0);
            style.spacing.interact_size.y = 30.0;
        });
        let worker = Worker::start().ok();
        let mut state = State::default();
        if worker.is_none() {
            state.error =
                Some("Could not start the network worker. Close and reopen SeraBot.".into());
        }
        Self {
            state,
            worker,
            credentials: None,
            server: String::new(),
            token: String::new(),
            conversation: "windows-owner".into(),
            composer: String::new(),
            connection_open: true,
            pending: None,
            scheduled: None,
            next_request: Instant::now(),
            refreshing: false,
            reviewed_unknown: false,
            confirmed_egress: false,
            poll_history: false,
            poll_paused: false,
        }
    }

    fn ready(&self) -> bool {
        self.pending.is_none()
            && !self.refreshing
            && self.worker.is_some()
            && Instant::now() >= self.next_request
    }
    fn active_conversation(&self) -> bool {
        self.conversation == self.state.scope.conversation
    }

    fn reset_scope(&mut self, connected: bool) {
        self.state.new_scope(self.conversation.clone(), connected);
        if let Some(worker) = &self.worker {
            worker.set_epoch(self.state.scope.epoch);
        }
        self.composer.zeroize();
        self.pending = None;
        self.scheduled = None;
        self.refreshing = false;
        self.reviewed_unknown = false;
        self.confirmed_egress = false;
        self.poll_history = false;
        self.poll_paused = false;
    }

    fn connect(&mut self, ctx: &egui::Context) {
        let endpoint = match validate_endpoint(&self.server) {
            Ok(endpoint) => endpoint,
            Err(error) => {
                self.state.error = Some(error.to_string());
                return;
            }
        };
        if !valid_token(&self.token) || !valid_id(&self.conversation) {
            self.state.error = Some("Enter a 32–512 character API token and a valid conversation ID (letters, digits, _ : -; max 128).".into());
            return;
        }
        self.reset_scope(false);
        self.credentials = Some(Arc::new(Credentials {
            endpoint,
            token: SecretString::new(std::mem::take(&mut self.token)),
        }));
        // Drop text-edit undo buffers containing the pasted token.
        ctx.memory_mut(|memory| *memory = Default::default());
        self.refreshing = true;
        self.start_request(Operation::Health);
    }

    fn disconnect(&mut self, ctx: &egui::Context) {
        self.reset_scope(false);
        self.credentials = None;
        self.token.zeroize();
        self.connection_open = true;
        ctx.memory_mut(|memory| *memory = Default::default());
    }

    fn start_request(&mut self, operation: Operation) {
        self.scheduled = None;
        let Some(credentials) = &self.credentials else {
            return;
        };
        let Some(worker) = &self.worker else {
            return;
        };
        let kind = operation.kind();
        let job = Job {
            scope: self.state.scope.clone(),
            credentials: Arc::clone(credentials),
            operation,
        };
        if worker.dispatch(job).is_err() {
            self.state.error = Some("Network worker unavailable. This request was not sent. Disconnect, then reconnect.".into());
            self.refreshing = false;
            self.poll_paused = true;
            return;
        }
        self.pending = Some(kind);
        self.next_request = Instant::now() + REQUEST_GAP;
    }

    fn schedule(&mut self, operation: Operation, delay: Duration) {
        self.scheduled = Some(((Instant::now() + delay).max(self.next_request), operation));
    }

    fn refresh(&mut self) {
        self.state.error = None;
        self.refreshing = true;
        self.reviewed_unknown = false;
        self.poll_paused = false;
        self.start_request(if self.state.connected {
            Operation::Catalog
        } else {
            Operation::Health
        });
    }

    fn receive(&mut self) {
        while let Some(completion) = self.worker.as_ref().and_then(Worker::completion) {
            if !self.state.scope.accepts(&completion.scope) {
                continue;
            }
            self.pending = None;
            match completion.result {
                Err(error) => {
                    self.state.fail(&completion.scope, completion.kind, error);
                    self.refreshing = false;
                    self.scheduled = None;
                    self.poll_paused = true;
                    if error == ApiError::Http(429) {
                        self.next_request = Instant::now() + Duration::from_secs(60);
                    }
                }
                Ok(reply) => {
                    self.state.receive(&completion.scope, reply);
                    match completion.kind {
                        OperationKind::Health if self.state.connected => {
                            self.connection_open = false;
                            self.schedule(Operation::Catalog, Duration::ZERO);
                        }
                        OperationKind::Health => {
                            self.refreshing = false;
                            self.poll_paused = true;
                        }
                        OperationKind::Catalog if self.refreshing => {
                            self.schedule(Operation::History, Duration::ZERO)
                        }
                        OperationKind::History if self.refreshing => {
                            self.refreshing = false;
                            self.reviewed_unknown = self.state.unknown_outcome;
                        }
                        OperationKind::Submit => {
                            self.poll_history = false;
                            self.schedule(Operation::History, Duration::ZERO);
                        }
                        OperationKind::Task
                            if self
                                .state
                                .task
                                .as_ref()
                                .is_some_and(|t| t.status.terminal()) =>
                        {
                            self.schedule(Operation::History, Duration::ZERO);
                        }
                        _ => {}
                    }
                }
            }
        }
        if self.pending.is_some() {
            return;
        }
        if self
            .scheduled
            .as_ref()
            .is_some_and(|(when, _)| Instant::now() >= *when)
        {
            if let Some((_, operation)) = self.scheduled.take() {
                self.start_request(operation);
            }
        } else if self.scheduled.is_none()
            && self.state.connected
            && !self.refreshing
            && !self.poll_paused
            && !self.state.unknown_outcome
        {
            if let Some(task) = &self.state.task {
                if !task.status.terminal() {
                    let operation = if self.poll_history {
                        Operation::History
                    } else {
                        Operation::Task(task.id.clone())
                    };
                    self.poll_history = !self.poll_history;
                    self.schedule(operation, POLL_GAP);
                }
            }
        }
    }

    fn connection_panel(&mut self, ctx: &egui::Context) {
        egui::TopBottomPanel::top("connection").show(ctx, |ui| {
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                ui.label(RichText::new("SeraBot").size(24.0).strong());
                ui.label(RichText::new("OWNER WORKSPACE").size(10.0).color(MUTED));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let label = if self.pending.is_some() {
                        "Connecting / syncing"
                    } else if self.state.connected {
                        "Connected"
                    } else {
                        "Disconnected"
                    };
                    ui.label(RichText::new(label).color(if self.state.connected {
                        TEAL
                    } else {
                        MUTED
                    }));
                    if self.pending.is_some() {
                        ui.spinner();
                    }
                });
            });
            ui.horizontal(|ui| {
                if ui
                    .selectable_label(self.connection_open, "Connection settings")
                    .clicked()
                {
                    self.connection_open = !self.connection_open;
                }
                if self.credentials.is_some() && ui.button("Disconnect").clicked() {
                    self.disconnect(ctx);
                }
                ui.label(
                    RichText::new("Session only · token and chat are not saved on this device")
                        .small()
                        .color(MUTED),
                );
            });
            if self.connection_open {
                ui.add_enabled_ui(self.credentials.is_none(), |ui| {
                    egui::Grid::new("credentials")
                        .num_columns(2)
                        .spacing([12.0, 8.0])
                        .show(ui, |ui| {
                            let label = ui.label("Server URL");
                            ui.add(
                                egui::TextEdit::singleline(&mut self.server)
                                    .hint_text("https://your-server or http://127.0.0.1:port")
                                    .desired_width(ui.available_width())
                                    .char_limit(2048),
                            )
                            .labelled_by(label.id);
                            ui.end_row();
                            let label = ui.label("API token");
                            ui.add(
                                egui::TextEdit::singleline(&mut self.token)
                                    .password(true)
                                    .hint_text("Paste owner API token")
                                    .char_limit(512)
                                    .desired_width(ui.available_width()),
                            )
                            .labelled_by(label.id);
                            ui.end_row();
                        });
                    ui.horizontal(|ui| {
                        if ui
                            .add_enabled(self.ready(), egui::Button::new("Connect"))
                            .clicked()
                        {
                            self.connect(ctx);
                        }
                        ui.label(
                            RichText::new(
                                "HTTPS required, except exact loopback HTTP for SSH tunnels.",
                            )
                            .small()
                            .color(MUTED),
                        );
                    });
                });
            }
            ui.add_space(4.0);
        });
    }

    fn conversation_panel(&mut self, ctx: &egui::Context) {
        egui::TopBottomPanel::top("conversation").show(ctx, |ui| {
            ui.horizontal_wrapped(|ui| {
                let label = ui.label("Conversation");
                ui.add(egui::TextEdit::singleline(&mut self.conversation).desired_width(205.0).char_limit(128)).labelled_by(label.id);
                let can_open = self.ready() && self.state.connected && valid_id(&self.conversation);
                if ui.add_enabled(can_open && !self.active_conversation(), egui::Button::new("Open")).clicked() {
                    self.reset_scope(true);
                    ctx.memory_mut(|memory| *memory = Default::default());
                    self.refresh();
                }
                if ui.add_enabled(self.ready() && self.state.connected, egui::Button::new("New")).clicked() {
                    self.conversation = format!("windows-{}", uuid::Uuid::new_v4());
                    self.reset_scope(true);
                    ctx.memory_mut(|memory| *memory = Default::default());
                    self.refresh();
                }
                if ui.add_enabled(self.ready() && self.credentials.is_some() && self.active_conversation(), egui::Button::new("Refresh / retry reads")).clicked() { self.refresh(); }
            });
            if !self.active_conversation() {
                ui.colored_label(WARNING, "Press Open to load this conversation. Chat below still belongs to the previous ID.");
            }
            self.selector(ui);
            if let Some(error) = &self.state.error { ui.colored_label(WARNING, error); }
            if self.state.unknown_outcome && self.reviewed_unknown {
                ui.label("Reads refreshed. A missing reply does not prove the request failed; it may still be running.");
                if ui.button("I reviewed the server state — allow new actions (do not resend)").clicked() {
                    self.state.unknown_outcome = false;
                    self.state.error = None;
                    self.reviewed_unknown = false;
                }
            }
            if self.pending.is_some() || self.scheduled.is_some() {
                ui.label(RichText::new(if self.refreshing { "Loading server selection and conversation…" } else { "Syncing task / conversation…" }).small().color(MUTED));
            } else if Instant::now() < self.next_request {
                ui.label(RichText::new("Brief request cooldown — protecting the server rate limit.").small().color(MUTED));
            }
        });
    }

    fn selector(&mut self, ui: &mut egui::Ui) {
        let ready = self.ready()
            && self.active_conversation()
            && !self.state.unknown_outcome
            && self.state.task.as_ref().is_none_or(|t| t.status.terminal());
        let Some(catalog) = &self.state.catalog else {
            ui.label(RichText::new("Connect to load available providers and models.").color(MUTED));
            return;
        };
        let before = self.state.draft.clone();
        ui.add_enabled_ui(ready, |ui| {
            ui.horizontal_wrapped(|ui| {
                egui::ComboBox::from_label("Provider")
                    .width(150.0)
                    .selected_text(&self.state.draft.provider)
                    .show_ui(ui, |ui| {
                        for provider in &catalog.providers {
                            let label = if provider.data_egress_approved {
                                provider.id.clone()
                            } else {
                                format!("{} — egress blocked", provider.id)
                            };
                            if ui
                                .add_enabled(
                                    provider.data_egress_approved,
                                    egui::SelectableLabel::new(
                                        self.state.draft.provider == provider.id,
                                        label,
                                    ),
                                )
                                .clicked()
                            {
                                self.state.draft.provider = provider.id.clone();
                                self.state.draft.model =
                                    provider.models.first().cloned().unwrap_or_default();
                            }
                        }
                    });
                egui::ComboBox::from_label("Model")
                    .width(180.0)
                    .selected_text(&self.state.draft.model)
                    .show_ui(ui, |ui| {
                        if let Some(provider) = catalog
                            .providers
                            .iter()
                            .find(|p| p.id == self.state.draft.provider)
                        {
                            for model in &provider.models {
                                ui.add_enabled_ui(provider.data_egress_approved, |ui| {
                                    ui.selectable_value(
                                        &mut self.state.draft.model,
                                        model.clone(),
                                        model,
                                    );
                                });
                            }
                        }
                    });
            });
        });
        if before != self.state.draft {
            self.confirmed_egress = false;
        }
        if !catalog.allows(&catalog.selection) {
            ui.colored_label(WARNING, "Sending blocked: the saved profile is unavailable or data egress is not approved. Choose an approved profile, or ask the operator to review server policy.");
        }
        if self.state.draft != catalog.selection {
            ui.label(
                RichText::new(format!(
                    "Saved: {} / {} · proposed change not yet applied",
                    catalog.selection.provider, catalog.selection.model
                ))
                .small()
                .color(MUTED),
            );
            ui.checkbox(&mut self.confirmed_egress, "I understand: this provider will receive this conversation's history with future requests.");
            let can_apply = ready && self.confirmed_egress && catalog.allows(&self.state.draft);
            ui.horizontal(|ui| {
                if ui
                    .add_enabled(can_apply, egui::Button::new("Apply confirmed change"))
                    .clicked()
                {
                    self.state.error = None;
                    self.confirmed_egress = false;
                    self.start_request(Operation::Apply(self.state.draft.clone()));
                }
                if ui
                    .add_enabled(ready, egui::Button::new("Discard change"))
                    .clicked()
                {
                    if let Some(catalog) = &self.state.catalog {
                        self.state.draft = catalog.selection.clone();
                    }
                    self.confirmed_egress = false;
                }
            });
        }
    }

    fn composer_panel(&mut self, ctx: &egui::Context) {
        egui::TopBottomPanel::bottom("composer").show(ctx, |ui| {
            ui.add_space(4.0);
            if let Some(task) = &self.state.task {
                ui.horizontal_wrapped(|ui| {
                    ui.label(RichText::new(task.status.label()).color(
                        if task.status == TaskStatus::Failed {
                            WARNING
                        } else {
                            TEAL
                        },
                    ));
                    ui.label(
                        RichText::new(format!("{} · {}", task.bot, task.id))
                            .small()
                            .color(MUTED),
                    );
                });
            }
            let label = ui.label("Message");
            let edit = ui.add_enabled(
                self.state.connected && self.active_conversation(),
                egui::TextEdit::multiline(&mut self.composer)
                    .desired_rows(2)
                    .desired_width(f32::INFINITY)
                    .hint_text("Ask SeraBot…")
                    .char_limit(8000)
                    .lock_focus(false),
            );
            edit.labelled_by(label.id);
            let shortcut =
                ctx.input_mut(|input| input.consume_key(egui::Modifiers::CTRL, egui::Key::Enter));
            ui.horizontal_wrapped(|ui| {
                let enabled = self.ready()
                    && self.active_conversation()
                    && self.state.can_send()
                    && valid_text(&self.composer);
                if ui
                    .add_enabled(
                        enabled,
                        egui::Button::new(RichText::new("Send").strong().color(TEAL)),
                    )
                    .clicked()
                    || (enabled && shortcut)
                {
                    self.state.error = None;
                    let text = Zeroizing::new(std::mem::take(&mut self.composer));
                    self.start_request(Operation::Submit(text));
                    // No optimistic/fake message; only server history is rendered.
                    ctx.memory_mut(|memory| *memory = Default::default());
                }
                ui.label(
                    RichText::new(format!(
                        "Ctrl+Enter · {} / 8,000",
                        self.composer.chars().count()
                    ))
                    .small()
                    .color(MUTED),
                );
                ui.label(
                    RichText::new("Approvals stay in Telegram.")
                        .small()
                        .color(MUTED),
                );
            });
        });
    }

    fn history_panel(&self, ctx: &egui::Context) {
        egui::CentralPanel::default().show(ctx, |ui| {
            if self.state.messages.is_empty() {
                ui.add_space(24.0);
                ui.heading(if !self.state.connected { "Your workspace, one conversation away." } else if !self.state.history_loaded { "Loading conversation" } else { "No visible messages yet" });
                ui.label(RichText::new(if !self.state.connected { "Connect to your self-hosted SeraBot to load its provider catalog and chat history." } else { "Only safe, visible messages returned by your server appear here." }).color(MUTED));
            }
            egui::ScrollArea::vertical().id_salt(("history", self.state.scope.epoch)).auto_shrink([false, false]).stick_to_bottom(true).show(ui, |ui| {
                ui.set_max_width(ui.available_width());
                for message in &self.state.messages {
                    ui.add_space(4.0);
                    egui::Frame::new().fill(if message.role == Role::User { Color32::from_rgb(30, 42, 45) } else { Color32::from_rgb(30, 35, 42) })
                        .inner_margin(12.0).corner_radius(6.0).show(ui, |ui| {
                            ui.set_width((ui.available_width() - 4.0).max(100.0));
                            ui.horizontal_wrapped(|ui| {
                                ui.label(RichText::new(if message.role == Role::User { "YOU" } else { "SERABOT" }).small().strong().color(TEAL));
                                ui.label(RichText::new(&message.created_at).small().color(MUTED));
                            });
                            ui.add(egui::Label::new(&message.content).wrap().selectable(true));
                        });
                }
            });
        });
    }
}

impl eframe::App for Desktop {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.receive();
        self.connection_panel(ctx);
        self.conversation_panel(ctx);
        self.composer_panel(ctx);
        self.history_panel(ctx);
        if self.pending.is_some() {
            ctx.request_repaint_after(Duration::from_millis(100));
        } else if let Some((when, _)) = &self.scheduled {
            let now = Instant::now();
            let wake = if self.next_request > now {
                (*when).min(self.next_request)
            } else {
                *when
            };
            ctx.request_repaint_after(wake.saturating_duration_since(now));
        } else if Instant::now() < self.next_request {
            ctx.request_repaint_after(self.next_request.saturating_duration_since(Instant::now()));
        }
    }

    fn persist_egui_memory(&self) -> bool {
        false
    }
}

impl Drop for Desktop {
    fn drop(&mut self) {
        self.token.zeroize();
        self.composer.zeroize();
        self.credentials = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app() -> Desktop {
        Desktop {
            state: State::default(),
            worker: Some(Worker::start().unwrap()),
            credentials: None,
            server: String::new(),
            token: String::new(),
            conversation: "windows-owner".into(),
            composer: String::new(),
            connection_open: true,
            pending: None,
            scheduled: None,
            next_request: Instant::now(),
            refreshing: false,
            reviewed_unknown: false,
            confirmed_egress: false,
            poll_history: false,
            poll_paused: false,
        }
    }

    #[test]
    fn scheduled_background_poll_does_not_lock_out_manual_reads_or_new_conversation() {
        let mut app = app();
        app.schedule(Operation::Task("task".into()), POLL_GAP);
        assert!(app.ready());
        app.pending = Some(OperationKind::Task);
        assert!(!app.ready());
        app.pending = None;
        app.refreshing = true;
        assert!(!app.ready());
    }

    #[test]
    fn scheduled_requests_respect_global_cooldown() {
        let mut app = app();
        app.next_request = Instant::now() + REQUEST_GAP;
        app.schedule(Operation::History, Duration::ZERO);
        assert!(app.scheduled.as_ref().unwrap().0 >= app.next_request);
        assert!(!app.ready());
        assert!(REQUEST_GAP >= Duration::from_secs(3));
        assert!(POLL_GAP >= Duration::from_secs(5));
    }

    #[test]
    fn disconnect_wipes_inputs_credentials_and_pending_actions() {
        let mut app = app();
        app.token = "synthetic-token".into();
        app.composer = "sensitive synthetic text".into();
        app.credentials = Some(Arc::new(Credentials {
            endpoint: validate_endpoint("http://127.0.0.1:1").unwrap(),
            token: SecretString::new("x".repeat(32)),
        }));
        app.schedule(Operation::History, POLL_GAP);
        let old = app.state.scope.clone();
        app.disconnect(&egui::Context::default());
        assert!(app.token.is_empty());
        assert!(app.composer.is_empty());
        assert!(app.credentials.is_none());
        assert!(app.scheduled.is_none());
        assert!(!app.state.scope.accepts(&old));
    }

    #[test]
    fn native_panels_render_at_minimum_and_default_window_sizes() {
        for size in [[650.0, 480.0], [1000.0, 720.0]] {
            let mut app = app();
            let ctx = egui::Context::default();
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(size[0], size[1]),
                )),
                ..Default::default()
            };
            let output = ctx.run(input, |ctx| {
                app.connection_panel(ctx);
                app.conversation_panel(ctx);
                app.composer_panel(ctx);
                app.history_panel(ctx);
            });
            assert!(!output.shapes.is_empty());
        }
    }
}
