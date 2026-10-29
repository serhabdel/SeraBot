use eframe::egui::{self, Color32, CornerRadius, Frame, Margin, RichText, Stroke, Vec2};
use secrecy::SecretString;
use serabot_desktop::{
    ApiError, ApprovalDecision, Role, Selection, TaskStatus,
    state::State,
    valid_id, valid_text, valid_token, validate_endpoint,
    worker::{Credentials, Job, Operation, OperationKind, Worker},
};
use std::{
    sync::Arc,
    time::{Duration, Instant},
};
use zeroize::{Zeroize, Zeroizing};

const BG: Color32 = Color32::from_rgb(14, 17, 22);
const PANEL: Color32 = Color32::from_rgb(22, 27, 34);
const PANEL_ALT: Color32 = Color32::from_rgb(28, 34, 42);
const STROKE: Color32 = Color32::from_rgb(42, 50, 60);
const TEAL: Color32 = Color32::from_rgb(88, 196, 178);
const TEAL_DIM: Color32 = Color32::from_rgb(42, 92, 84);
const TEXT: Color32 = Color32::from_rgb(230, 236, 242);
const MUTED: Color32 = Color32::from_rgb(140, 152, 168);
const WARNING: Color32 = Color32::from_rgb(232, 176, 98);
const DANGER: Color32 = Color32::from_rgb(220, 110, 110);
const USER_BUBBLE: Color32 = Color32::from_rgb(28, 48, 52);
const BOT_BUBBLE: Color32 = Color32::from_rgb(30, 35, 43);
const REQUEST_GAP: Duration = Duration::from_secs(3);
const POLL_GAP: Duration = Duration::from_secs(5);
const SIDEBAR_W: f32 = 268.0;
fn status_chip(ui: &mut egui::Ui, label: &str, fg: Color32, bg: Color32) {
    Frame::new()
        .fill(bg)
        .corner_radius(12.0)
        .inner_margin(Margin::symmetric(10, 5))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 6.0;
                ui.label(RichText::new("●").size(9.0).color(fg));
                ui.label(RichText::new(label).size(11.0).strong().color(fg));
            });
        });
}

fn empty_state(ui: &mut egui::Ui, title: &str, body: &str) {
    ui.add_space(32.0);
    ui.vertical_centered(|ui| {
        Frame::new()
            .fill(PANEL_ALT)
            .stroke(Stroke::new(1.0_f32, STROKE))
            .corner_radius(14.0)
            .inner_margin(Margin::symmetric(28, 22))
            .show(ui, |ui| {
                ui.set_max_width(420.0);
                ui.label(RichText::new(title).size(20.0).strong().color(TEXT));
                ui.add_space(8.0);
                ui.label(RichText::new(body).color(MUTED));
            });
    });
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Section {
    Chat,
    Approvals,
}

pub struct Desktop {
    state: State,
    worker: Option<Worker>,
    credentials: Option<Arc<Credentials>>,
    server: String,
    token: String,
    conversation: String,
    composer: String,
    connection_open: bool,
    section: Section,
    pending: Option<OperationKind>,
    scheduled: Option<(Instant, Operation)>,
    next_request: Instant,
    refreshing: bool,
    reviewed_unknown: bool,
    confirmed_egress: bool,
    poll_history: bool,
    poll_paused: bool,
    poll_approvals: bool,
}

impl Desktop {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let mut visuals = egui::Visuals::dark();
        visuals.panel_fill = PANEL;
        visuals.window_fill = PANEL;
        visuals.extreme_bg_color = BG;
        visuals.override_text_color = Some(TEXT);
        visuals.widgets.noninteractive.bg_fill = PANEL_ALT;
        visuals.widgets.inactive.bg_fill = PANEL_ALT;
        visuals.widgets.hovered.bg_fill = Color32::from_rgb(36, 44, 54);
        visuals.widgets.active.bg_fill = TEAL_DIM;
        visuals.selection.bg_fill = TEAL_DIM;
        visuals.selection.stroke = Stroke::new(1.0_f32, TEAL);
        visuals.widgets.noninteractive.fg_stroke = Stroke::new(1.0_f32, TEXT);
        visuals.widgets.inactive.fg_stroke = Stroke::new(1.0_f32, TEXT);
        visuals.window_stroke = Stroke::new(1.0_f32, STROKE);
        cc.egui_ctx.set_visuals(visuals);
        cc.egui_ctx.style_mut(|style| {
            style.spacing.item_spacing = Vec2::new(10.0, 8.0);
            style.spacing.button_padding = Vec2::new(14.0, 7.0);
            style.spacing.interact_size.y = 32.0;
            style.spacing.window_margin = Margin::same(0);
            style.visuals.window_corner_radius = CornerRadius::same(0);
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
            section: Section::Chat,
            pending: None,
            scheduled: None,
            next_request: Instant::now(),
            refreshing: false,
            reviewed_unknown: false,
            confirmed_egress: false,
            poll_history: false,
            poll_paused: false,
            poll_approvals: false,
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
        self.poll_approvals = false;
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
                            self.schedule(Operation::Approvals, Duration::ZERO)
                        }
                        OperationKind::Approvals if self.refreshing => {
                            self.refreshing = false;
                            self.reviewed_unknown = self.state.unknown_outcome;
                        }
                        OperationKind::Submit => {
                            self.poll_history = false;
                            self.schedule(Operation::History, Duration::ZERO);
                        }
                        OperationKind::Decide => {
                            self.schedule(Operation::Approvals, Duration::ZERO);
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
                        OperationKind::Task
                            if self.state.task.as_ref().is_some_and(|t| {
                                t.status == TaskStatus::AwaitingApproval
                            }) =>
                        {
                            self.schedule(Operation::Approvals, Duration::ZERO);
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
                    let operation = if self.poll_approvals
                        && task.status == TaskStatus::AwaitingApproval
                    {
                        Operation::Approvals
                    } else if self.poll_history {
                        Operation::History
                    } else {
                        Operation::Task(task.id.clone())
                    };
                    if task.status == TaskStatus::AwaitingApproval {
                        self.poll_approvals = !self.poll_approvals;
                        if !self.poll_approvals {
                            self.poll_history = !self.poll_history;
                        }
                    } else {
                        self.poll_history = !self.poll_history;
                    }
                    self.schedule(operation, POLL_GAP);
                }
            } else if self.section == Section::Approvals {
                self.schedule(Operation::Approvals, POLL_GAP * 2);
            }
        }
    }

    fn sidebar(&mut self, ctx: &egui::Context) {
        egui::SidePanel::left("sidebar")
            .exact_width(SIDEBAR_W)
            .resizable(false)
            .frame(
                Frame::new()
                    .fill(PANEL)
                    .stroke(Stroke::new(1.0_f32, STROKE))
                    .inner_margin(Margin::symmetric(16, 16)),
            )
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.label(RichText::new("SeraBot").size(22.0).strong().color(TEXT));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if self.pending.is_some() {
                            ui.spinner();
                        }
                        let (label, fg, bg) = if self.pending.is_some() {
                            ("Syncing", WARNING, Color32::from_rgb(52, 42, 28))
                        } else if self.state.connected {
                            ("Connected", TEAL, Color32::from_rgb(24, 44, 40))
                        } else {
                            ("Offline", MUTED, Color32::from_rgb(32, 36, 42))
                        };
                        status_chip(ui, label, fg, bg);
                    });
                });
                ui.label(
                    RichText::new("Owner console · session only")
                        .size(11.0)
                        .color(MUTED),
                );
                if !self.state.version.is_empty() {
                    ui.label(
                        RichText::new(format!("server {}", self.state.version))
                            .size(11.0)
                            .color(MUTED),
                    );
                }
                ui.add_space(14.0);
                ui.separator();
                ui.add_space(10.0);

                for (section, label, hint) in [
                    (Section::Chat, "Chat", "Conversation and model routing"),
                    (
                        Section::Approvals,
                        "Approvals",
                        if self.state.approvals.is_empty() {
                            "No pending actions"
                        } else {
                            "Pending gateway decisions"
                        },
                    ),
                ] {
                    let selected = self.section == section;
                    let fill = if selected {
                        Color32::from_rgb(32, 40, 46)
                    } else {
                        Color32::TRANSPARENT
                    };
                    let response = ui
                        .horizontal(|ui| {
                            Frame::new()
                                .fill(if selected { TEAL } else { Color32::TRANSPARENT })
                                .show(ui, |ui| {
                                    ui.set_min_size(Vec2::new(3.0, 44.0));
                                });
                            Frame::new()
                                .fill(fill)
                                .corner_radius(8.0)
                                .inner_margin(Margin::symmetric(10, 8))
                                .show(ui, |ui| {
                                    ui.set_width((ui.available_width() - 6.0).max(0.0));
                                    ui.horizontal(|ui| {
                                        ui.label(
                                            RichText::new(label)
                                                .strong()
                                                .size(14.0)
                                                .color(if selected { TEAL } else { TEXT }),
                                        );
                                        if section == Section::Approvals
                                            && !self.state.approvals.is_empty()
                                        {
                                            ui.with_layout(
                                                egui::Layout::right_to_left(egui::Align::Center),
                                                |ui| {
                                                    status_chip(
                                                        ui,
                                                        &format!("{}", self.state.approvals.len()),
                                                        WARNING,
                                                        Color32::from_rgb(52, 42, 28),
                                                    );
                                                },
                                            );
                                        }
                                    });
                                    ui.label(RichText::new(hint).size(11.0).color(MUTED));
                                })
                                .response
                                .interact(egui::Sense::click())
                        })
                        .inner;
                    if response.clicked() {
                        self.section = section;
                        if section == Section::Approvals
                            && self.ready()
                            && self.state.connected
                            && !self.state.unknown_outcome
                        {
                            self.start_request(Operation::Approvals);
                        }
                    }
                    ui.add_space(4.0);
                }

                ui.add_space(12.0);
                ui.separator();
                ui.add_space(10.0);
                ui.label(RichText::new("Conversation").small().color(MUTED));
                ui.add(
                    egui::TextEdit::singleline(&mut self.conversation)
                        .desired_width(f32::INFINITY)
                        .char_limit(128)
                        .hint_text("conversation id"),
                );
                ui.horizontal(|ui| {
                    let can_open =
                        self.ready() && self.state.connected && valid_id(&self.conversation);
                    if ui
                        .add_enabled(
                            can_open && !self.active_conversation(),
                            egui::Button::new("Open"),
                        )
                        .clicked()
                    {
                        self.reset_scope(true);
                        ctx.memory_mut(|memory| *memory = Default::default());
                        self.refresh();
                    }
                    if ui
                        .add_enabled(
                            self.ready() && self.state.connected,
                            egui::Button::new("New"),
                        )
                        .clicked()
                    {
                        self.conversation = format!("windows-{}", uuid::Uuid::new_v4());
                        self.reset_scope(true);
                        ctx.memory_mut(|memory| *memory = Default::default());
                        self.refresh();
                    }
                });
                if !self.active_conversation() && self.state.connected {
                    ui.colored_label(WARNING, "Open to load this conversation id.");
                }

                ui.add_space(10.0);
                self.model_block(ui);

                ui.with_layout(egui::Layout::bottom_up(egui::Align::LEFT), |ui| {
                    ui.add_space(4.0);
                    ui.horizontal(|ui| {
                        if self.credentials.is_some() {
                            if ui.button("Disconnect").clicked() {
                                self.disconnect(ctx);
                            }
                            if ui
                                .add_enabled(
                                    self.ready()
                                        && self.state.connected
                                        && self.active_conversation(),
                                    egui::Button::new("Refresh"),
                                )
                                .clicked()
                            {
                                self.refresh();
                            }
                        }
                    });
                    ui.add_space(8.0);
                    if ui
                        .selectable_label(self.connection_open, "Connection")
                        .clicked()
                    {
                        self.connection_open = !self.connection_open;
                    }
                    if self.connection_open {
                        ui.add_space(6.0);
                        ui.add_enabled_ui(self.credentials.is_none(), |ui| {
                            ui.label(RichText::new("Server").small().color(MUTED));
                            ui.add(
                                egui::TextEdit::singleline(&mut self.server)
                                    .desired_width(f32::INFINITY)
                                    .hint_text("https://… or http://127.0.0.1:8787")
                                    .char_limit(2048),
                            );
                            ui.add_space(4.0);
                            ui.label(RichText::new("API token").small().color(MUTED));
                            ui.add(
                                egui::TextEdit::singleline(&mut self.token)
                                    .password(true)
                                    .desired_width(f32::INFINITY)
                                    .hint_text("owner token")
                                    .char_limit(512),
                            );
                            ui.add_space(6.0);
                            if ui
                                .add_enabled(self.ready(), egui::Button::new("Connect"))
                                .clicked()
                            {
                                self.connect(ctx);
                            }
                            let enter = ui.ctx().input(|input| {
                                input.key_pressed(egui::Key::Enter)
                                    && !input.modifiers.any()
                            });
                            if enter
                                && self.ready()
                                && validate_endpoint(&self.server).is_ok()
                                && valid_token(&self.token)
                                && valid_id(&self.conversation)
                            {
                                self.connect(ctx);
                            }
                        });
                        ui.label(
                            RichText::new("HTTPS, or exact loopback HTTP for tunnels. Token never leaves memory.")
                                .size(11.0)
                                .color(MUTED),
                        );
                    }
                });
            });
    }

    fn model_block(&mut self, ui: &mut egui::Ui) {
        ui.label(RichText::new("Model routing").small().color(MUTED));
        let ready = self.ready()
            && self.active_conversation()
            && !self.state.unknown_outcome
            && self.state.task.as_ref().is_none_or(|t| t.status.terminal());
        let Some(catalog) = &self.state.catalog else {
            ui.label(
                RichText::new("Connect to load providers.")
                    .size(12.0)
                    .color(MUTED),
            );
            return;
        };
        let before = self.state.draft.clone();
        ui.add_enabled_ui(ready, |ui| {
            let provider_label = if self.state.draft.is_auto() {
                "auto".to_string()
            } else {
                self.state.draft.provider.clone()
            };
            egui::ComboBox::from_id_salt("provider")
                .width(ui.available_width() - 8.0)
                .selected_text(provider_label)
                .show_ui(ui, |ui| {
                    if ui
                        .selectable_label(self.state.draft.is_auto(), "auto")
                        .clicked()
                    {
                        self.state.draft = Selection::auto();
                    }
                    for provider in &catalog.providers {
                        let label = if provider.data_egress_approved {
                            provider.id.clone()
                        } else {
                            format!("{} · blocked", provider.id)
                        };
                        if ui
                            .add_enabled(
                                provider.data_egress_approved,
                                egui::SelectableLabel::new(
                                    !self.state.draft.is_auto()
                                        && self.state.draft.provider == provider.id,
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
            let model_label = if self.state.draft.is_auto() {
                catalog
                    .effective_selection
                    .as_ref()
                    .map(|s| format!("{} / {}", s.provider, s.model))
                    .unwrap_or_else(|| "automatic".into())
            } else {
                format!(
                    "{} / {}",
                    self.state.draft.provider, self.state.draft.model
                )
            };
            egui::ComboBox::from_id_salt("model")
                .width(ui.available_width() - 8.0)
                .selected_text(model_label)
                .show_ui(ui, |ui| {
                    if self.state.draft.is_auto() {
                        ui.label(
                            RichText::new("Autoselect uses bot defaults and approved models.")
                                .size(11.0)
                                .color(MUTED),
                        );
                        return;
                    }
                    let entries = if catalog.entries.is_empty() {
                        catalog
                            .providers
                            .iter()
                            .flat_map(|provider| {
                                provider.models.iter().map(|model| {
                                    (
                                        provider.id.clone(),
                                        model.clone(),
                                        provider.data_egress_approved,
                                    )
                                })
                            })
                            .collect::<Vec<_>>()
                    } else {
                        catalog
                            .entries
                            .iter()
                            .map(|entry| {
                                (
                                    entry.provider.clone(),
                                    entry.model.clone(),
                                    entry.data_egress_approved,
                                )
                            })
                            .collect()
                    };
                    for (provider, model, approved) in entries {
                        let label = format!("{provider} / {model}");
                        ui.add_enabled_ui(approved, |ui| {
                            if ui
                                .selectable_label(
                                    self.state.draft.provider == provider
                                        && self.state.draft.model == model,
                                    label,
                                )
                                .clicked()
                            {
                                self.state.draft.provider = provider;
                                self.state.draft.model = model;
                            }
                        });
                    }
                });
        });
        if before != self.state.draft {
            self.confirmed_egress = false;
        }
        if !catalog.allows(&catalog.selection) {
            ui.colored_label(
                WARNING,
                "Saved profile unavailable or egress not approved.",
            );
        }
        if self.state.draft != catalog.selection {
            let saved = if catalog.selection.is_auto() {
                catalog
                    .effective_selection
                    .as_ref()
                    .map(|s| format!("auto → {} / {}", s.provider, s.model))
                    .unwrap_or_else(|| "auto".into())
            } else {
                format!(
                    "{} / {}",
                    catalog.selection.provider, catalog.selection.model
                )
            };
            ui.label(
                RichText::new(format!("Saved {saved}"))
                    .size(11.0)
                    .color(MUTED),
            );
            ui.checkbox(
                &mut self.confirmed_egress,
                "Provider will receive this conversation history",
            );
            let can_apply = ready && self.confirmed_egress && catalog.allows(&self.state.draft);
            ui.horizontal(|ui| {
                if ui
                    .add_enabled(can_apply, egui::Button::new("Apply"))
                    .clicked()
                {
                    self.state.error = None;
                    self.confirmed_egress = false;
                    self.start_request(Operation::Apply(self.state.draft.clone()));
                }
                if ui
                    .add_enabled(ready, egui::Button::new("Discard"))
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

    fn section_header(&self, ui: &mut egui::Ui, title: &str, subtitle: &str) {
        ui.horizontal(|ui| {
            ui.vertical(|ui| {
                ui.label(RichText::new(title).size(22.0).strong().color(TEXT));
                ui.label(RichText::new(subtitle).size(12.0).color(MUTED));
            });
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if self.section == Section::Chat && self.state.connected && self.active_conversation()
                {
                    let draft = if self.state.draft.is_auto() {
                        "auto".into()
                    } else {
                        format!(
                            "{} / {}",
                            self.state.draft.provider, self.state.draft.model
                        )
                    };
                    status_chip(
                        ui,
                        &draft,
                        TEAL,
                        Color32::from_rgb(24, 44, 40),
                    );
                }
            });
        });
        ui.add_space(10.0);
    }

    fn status_banner(&mut self, ui: &mut egui::Ui) {
        if let Some(error) = &self.state.error {
            Frame::new()
                .fill(Color32::from_rgb(48, 32, 28))
                .stroke(Stroke::new(1.0_f32, Color32::from_rgb(88, 58, 48)))
                .corner_radius(10.0)
                .inner_margin(Margin::symmetric(12, 10))
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        status_chip(ui, "Notice", WARNING, Color32::from_rgb(62, 44, 28));
                        ui.label(RichText::new(error).color(TEXT));
                    });
                });
            ui.add_space(8.0);
        }
        if self.state.unknown_outcome && self.reviewed_unknown {
            Frame::new()
                .fill(Color32::from_rgb(48, 40, 24))
                .stroke(Stroke::new(1.0_f32, Color32::from_rgb(88, 72, 40)))
                .corner_radius(10.0)
                .inner_margin(Margin::symmetric(12, 10))
                .show(ui, |ui| {
                    ui.label(
                        RichText::new(
                            "Reads refreshed. A missing reply does not prove failure.",
                        )
                        .color(TEXT),
                    );
                    if ui
                        .button("I reviewed server state — allow new actions")
                        .clicked()
                    {
                        self.state.unknown_outcome = false;
                        self.state.error = None;
                        self.reviewed_unknown = false;
                    }
                });
            ui.add_space(8.0);
        }
        if let Some(task) = &self.state.task {
            let (fg, bg) = match task.status {
                TaskStatus::Failed => (DANGER, Color32::from_rgb(52, 32, 32)),
                TaskStatus::AwaitingApproval => (WARNING, Color32::from_rgb(52, 42, 28)),
                TaskStatus::Done => (TEAL, Color32::from_rgb(24, 44, 40)),
                _ => (MUTED, Color32::from_rgb(32, 36, 42)),
            };
            Frame::new()
                .fill(PANEL_ALT)
                .stroke(Stroke::new(1.0_f32, STROKE))
                .corner_radius(10.0)
                .inner_margin(Margin::symmetric(12, 8))
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        status_chip(ui, task.status.label(), fg, bg);
                        ui.label(
                            RichText::new(format!("{} · {}", task.bot, task.id))
                                .size(12.0)
                                .color(MUTED),
                        );
                        if task.status == TaskStatus::AwaitingApproval {
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    if ui.button("Open approvals →").clicked() {
                                        self.section = Section::Approvals;
                                    }
                                },
                            );
                        }
                    });
                });
            ui.add_space(8.0);
        }
    }

    fn composer_block(&mut self, ui: &mut egui::Ui, rows: usize) {
        Frame::new()
            .fill(PANEL_ALT)
            .stroke(Stroke::new(1.0_f32, STROKE))
            .corner_radius(12.0)
            .inner_margin(Margin::symmetric(12, 10))
            .show(ui, |ui| {
                ui.set_min_width(ui.available_width());
                let edit = ui.add_enabled(
                    self.state.connected && self.active_conversation(),
                    egui::TextEdit::multiline(&mut self.composer)
                        .desired_rows(rows)
                        .desired_width(f32::INFINITY)
                        .hint_text("Message SeraBot…")
                        .char_limit(8000),
                );
                let shortcut = ui.ctx().input_mut(|input| {
                    input.consume_key(egui::Modifiers::CTRL, egui::Key::Enter)
                });
                ui.horizontal(|ui| {
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
                        ui.ctx()
                            .memory_mut(|memory| *memory = Default::default());
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.label(
                            RichText::new(format!(
                                "Ctrl+Enter · {} / 8,000",
                                self.composer.chars().count()
                            ))
                            .small()
                            .color(MUTED),
                        );
                    });
                    let _ = edit;
                });
            });
    }

    fn chat_panel(&mut self, ui: &mut egui::Ui) {
        self.section_header(
            ui,
            "Chat",
            if self.active_conversation() {
                &self.conversation
            } else {
                "Open a conversation id from the sidebar"
            },
        );
        self.status_banner(ui);
        let composer_rows = if ui.available_height() < 420.0 { 2_usize } else { 3_usize };
        ui.with_layout(egui::Layout::bottom_up(egui::Align::LEFT), |ui| {
            ui.set_width(ui.available_width());
            self.composer_block(ui, composer_rows);
            ui.add_space(8.0);
            let scroll_h = ui.available_height().max(96.0);
            egui::ScrollArea::vertical()
                .id_salt(("history", self.state.scope.epoch))
                .auto_shrink([false, false])
                .stick_to_bottom(true)
                .max_height(scroll_h)
                .show(ui, |ui| {
                    ui.set_min_height(scroll_h - 4.0);
                    ui.set_width(ui.available_width());
                    if self.state.messages.is_empty() {
                        empty_state(
                            ui,
                            if !self.state.connected {
                                "Connect to your host"
                            } else if !self.state.history_loaded {
                                "Loading conversation"
                            } else {
                                "No messages yet"
                            },
                            if !self.state.connected {
                                "Use an HTTPS origin or a loopback SSH tunnel, then paste the owner API token."
                            } else {
                                "Only visible user and assistant messages from the server appear here."
                            },
                        );
                        return;
                    }
                    for message in &self.state.messages {
                        ui.add_space(10.0);
                        let user = message.role == Role::User;
                        let max_w = (ui.available_width() * 0.88).max(200.0);
                        ui.horizontal(|ui| {
                            if user {
                                ui.add_space((ui.available_width() - max_w).max(0.0));
                            }
                            Frame::new()
                                .fill(if user { USER_BUBBLE } else { BOT_BUBBLE })
                                .stroke(Stroke::new(
                                    1.0_f32,
                                    if user {
                                        Color32::from_rgb(48, 72, 68)
                                    } else {
                                        STROKE
                                    },
                                ))
                                .corner_radius(14.0)
                                .inner_margin(Margin::symmetric(14, 12))
                                .show(ui, |ui| {
                                    ui.set_max_width(max_w);
                                    ui.horizontal(|ui| {
                                        ui.label(
                                            RichText::new(if user { "You" } else { "SeraBot" })
                                                .size(12.0)
                                                .strong()
                                                .color(if user { TEAL } else { MUTED }),
                                        );
                                        ui.with_layout(
                                            egui::Layout::right_to_left(egui::Align::Center),
                                            |ui| {
                                                ui.label(
                                                    RichText::new(&message.created_at)
                                                        .size(11.0)
                                                        .color(MUTED),
                                                );
                                            },
                                        );
                                    });
                                    ui.add_space(6.0);
                                    ui.add(
                                        egui::Label::new(
                                            RichText::new(&message.content)
                                                .size(14.0)
                                                .color(TEXT),
                                        )
                                        .wrap()
                                        .selectable(true),
                                    );
                                });
                        });
                    }
                    ui.add_space(12.0);
                });
        });
    }

    fn approvals_panel(&mut self, ui: &mut egui::Ui) {
        self.section_header(
            ui,
            "Approvals",
            "Gateway consent for outbound tools and effects",
        );
        self.status_banner(ui);
        ui.horizontal(|ui| {
            ui.label(
                RichText::new(
                    "Decisions bind the exact payload, destination, artifact hash, policy version, and expiry.",
                )
                .size(12.0)
                .color(MUTED),
            );
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui
                    .add_enabled(
                        self.ready() && self.state.connected && !self.state.unknown_outcome,
                        egui::Button::new("Refresh list"),
                    )
                    .clicked()
                {
                    self.start_request(Operation::Approvals);
                }
            });
        });
        ui.add_space(12.0);
        if !self.state.connected {
            empty_state(
                ui,
                "Connect to review approvals",
                "Pending gateway actions appear here once you are connected to the owner API.",
            );
            return;
        }
        if !self.state.approvals_loaded {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label(RichText::new("Loading approvals…").color(MUTED));
            });
            return;
        }
        if self.state.approvals.is_empty() {
            empty_state(
                ui,
                "Nothing pending",
                "When the gateway needs consent, actions appear here and in Telegram.",
            );
            return;
        }
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                for approval in self.state.approvals.clone() {
                    ui.add_space(10.0);
                    Frame::new()
                        .fill(PANEL_ALT)
                        .stroke(Stroke::new(1.0_f32, STROKE))
                        .corner_radius(14.0)
                        .inner_margin(Margin::symmetric(16, 14))
                        .show(ui, |ui| {
                            ui.set_width(ui.available_width());
                            ui.horizontal(|ui| {
                                status_chip(
                                    ui,
                                    &approval.tool,
                                    TEAL,
                                    Color32::from_rgb(24, 44, 40),
                                );
                                ui.with_layout(
                                    egui::Layout::right_to_left(egui::Align::Center),
                                    |ui| {
                                        ui.label(
                                            RichText::new(format!(
                                                "expires {}",
                                                approval.expires_at
                                            ))
                                            .size(11.0)
                                            .color(MUTED),
                                        );
                                    },
                                );
                            });
                            ui.add_space(10.0);
                            ui.label(
                                RichText::new(&approval.summary)
                                    .size(14.0)
                                    .color(TEXT),
                            );
                            ui.add_space(6.0);
                            ui.label(
                                RichText::new(format!("Approval id · {}", approval.id))
                                    .size(11.0)
                                    .color(MUTED),
                            );
                            ui.add_space(12.0);
                            ui.separator();
                            ui.add_space(10.0);
                            ui.horizontal(|ui| {
                                let can = self.ready() && !self.state.unknown_outcome;
                                for decision in [
                                    ApprovalDecision::Once,
                                    ApprovalDecision::Always,
                                    ApprovalDecision::Deny,
                                ] {
                                    let button = match decision {
                                        ApprovalDecision::Deny => egui::Button::new(
                                            RichText::new(decision.label()).color(DANGER),
                                        ),
                                        ApprovalDecision::Once => egui::Button::new(
                                            RichText::new(decision.label())
                                                .strong()
                                                .color(TEAL),
                                        ),
                                        ApprovalDecision::Always => {
                                            egui::Button::new(decision.label())
                                        }
                                    };
                                    if ui.add_enabled(can, button).clicked() {
                                        self.state.error = None;
                                        self.start_request(Operation::Decide {
                                            id: approval.id.clone(),
                                            decision,
                                        });
                                    }
                                }
                            });
                            ui.add_space(6.0);
                            ui.label(
                                RichText::new(
                                    "Once = this exact payload. Always = revocable grant for this exact payload.",
                                )
                                .size(11.0)
                                .color(MUTED),
                            );
                        });
                }
            });
    }
}

impl eframe::App for Desktop {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.receive();
        egui::CentralPanel::default()
            .frame(Frame::new().fill(BG).inner_margin(0.0))
            .show(ctx, |_| {});
        self.sidebar(ctx);
        egui::CentralPanel::default()
            .frame(
                Frame::new()
                    .fill(BG)
                    .inner_margin(Margin::symmetric(20, 16)),
            )
            .show(ctx, |ui| match self.section {
                Section::Chat => self.chat_panel(ui),
                Section::Approvals => self.approvals_panel(ui),
            });
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
            section: Section::Chat,
            pending: None,
            scheduled: None,
            next_request: Instant::now(),
            refreshing: false,
            reviewed_unknown: false,
            confirmed_egress: false,
            poll_history: false,
            poll_paused: false,
            poll_approvals: false,
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
        for size in [[860.0, 560.0], [720.0, 520.0], [1100.0, 740.0]] {
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
                app.sidebar(ctx);
                egui::CentralPanel::default().show(ctx, |ui| {
                    app.chat_panel(ui);
                    app.approvals_panel(ui);
                });
            });
            assert!(!output.shapes.is_empty());
        }
    }
}
