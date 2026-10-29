//! Approvals, fleet scaffolding, and the shared status banner.

use super::*;

impl Desktop {
    pub(crate) fn status_banner(&mut self, ui: &mut egui::Ui) {
        if let Some(error) = self.state.error.clone() {
            Frame::new()
                .fill(design::warning_bg())
                .stroke(Stroke::new(1.0_f32, Color32::from_rgb(88, 70, 40)))
                .corner_radius(10.0)
                .inner_margin(Margin::symmetric(12, 9))
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        design::chip(ui, "Notice", design::warning(), design::warning_bg());
                        ui.label(RichText::new(error).size(12.0).color(design::text()));
                    });
                });
            ui.add_space(8.0);
        }
        if self.state.unknown_outcome && self.reviewed_unknown {
            Frame::new()
                .fill(Color32::from_rgb(48, 40, 24))
                .stroke(Stroke::new(1.0_f32, Color32::from_rgb(88, 72, 40)))
                .corner_radius(10.0)
                .inner_margin(Margin::symmetric(12, 9))
                .show(ui, |ui| {
                    ui.label(
                        RichText::new("Reads refreshed. A missing reply does not prove failure.")
                            .size(12.0)
                            .color(design::text()),
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
        let Some(task) = self.state.task.as_ref() else {
            return;
        };
        let (fg, bg) = match task.status {
            TaskStatus::Failed => (design::danger(), design::danger_bg()),
            TaskStatus::AwaitingApproval => (design::warning(), design::warning_bg()),
            TaskStatus::Done => (design::accent(), design::ok_bg()),
            _ => (design::muted(), design::panel_alt()),
        };
        let awaiting = task.status == TaskStatus::AwaitingApproval;
        let task_id = task.id.clone();
        let bot = task.bot.clone();
        let label = task.status.label();
        Frame::new()
            .fill(design::panel_alt())
            .stroke(Stroke::new(1.0_f32, design::stroke_soft()))
            .corner_radius(10.0)
            .inner_margin(Margin::symmetric(12, 7))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    design::chip(ui, label, fg, bg);
                    ui.label(design::meta(format!("{bot} · {task_id}")));
                    if awaiting {
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui.button("Review approvals").clicked() {
                                self.section = Section::Approvals;
                                self.ensure_section_data();
                            }
                        });
                    }
                });
            });
        ui.add_space(8.0);
    }

    pub(crate) fn approvals_screen(&mut self, ui: &mut egui::Ui, _ctx: &egui::Context) {
        design::header(
            ui,
            "Approvals",
            "Gateway consent for outbound tools and effects",
        );
        self.status_banner(ui);
        ui.horizontal(|ui| {
            ui.label(
                RichText::new(
                    "Decisions bind the exact payload, destination, artifact hash, policy version and expiry.",
                )
                .size(11.0)
                .color(design::muted()),
            );
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui
                    .add_enabled(
                        self.ready() && self.state.connected && !self.state.unknown_outcome,
                        egui::Button::new(RichText::new("Refresh list").size(11.0)),
                    )
                    .clicked()
                {
                    self.start_request(Operation::Approvals);
                }
            });
        });
        ui.add_space(10.0);
        if !self.state.approvals_loaded {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label(design::meta("Loading approvals…"));
            });
            return;
        }
        if self.state.approvals.is_empty() {
            design::empty_state(
                ui,
                "Nothing pending",
                "When the gateway needs consent, actions appear here and in Telegram.",
            );
            return;
        }
        let can = self.ready() && !self.state.unknown_outcome;
        let mut decided: Option<(String, ApprovalDecision)> = None;
        egui::ScrollArea::vertical()
            .id_salt("approvals")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                for approval in &self.state.approvals {
                    design::card(ui, |ui| {
                        ui.horizontal(|ui| {
                            design::chip(ui, &approval.tool, design::accent(), design::ok_bg());
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    ui.label(design::meta(format!(
                                        "expires {}",
                                        approval.expires_at
                                    )));
                                },
                            );
                        });
                        ui.add_space(6.0);
                        ui.label(
                            RichText::new(&approval.summary)
                                .size(13.0)
                                .color(design::text()),
                        );
                        ui.add_space(2.0);
                        ui.label(
                            RichText::new(format!("Approval id · {}", approval.id))
                                .size(10.0)
                                .monospace()
                                .color(design::muted_dim()),
                        );
                        ui.add_space(8.0);
                        design::divider(ui);
                        ui.add_space(6.0);
                        ui.horizontal(|ui| {
                            for decision in [
                                ApprovalDecision::Once,
                                ApprovalDecision::Always,
                                ApprovalDecision::Deny,
                            ] {
                                let button = match decision {
                                    ApprovalDecision::Deny => design::danger_button(decision.label()),
                                    ApprovalDecision::Once => design::primary(decision.label()),
                                    ApprovalDecision::Always => {
                                        egui::Button::new(RichText::new(decision.label()).size(13.0))
                                    }
                                };
                                if ui.add_enabled(can, button).clicked() {
                                    decided = Some((approval.id.clone(), decision));
                                }
                            }
                        });
                        ui.add_space(4.0);
                        ui.label(
                            RichText::new(
                                "Once = this exact payload. Always = revocable grant for this exact payload.",
                            )
                            .size(10.0)
                            .color(design::muted_dim()),
                        );
                    });
                    ui.add_space(8.0);
                }
            });
        if let Some((id, decision)) = decided {
            self.state.error = None;
            self.start_request(Operation::Decide { id, decision });
        }
    }

    pub(crate) fn fleet_screen(&mut self, ui: &mut egui::Ui) {
        design::header(
            ui,
            "Fleet",
            "Disabled specialists, skill packs and conversation nodes",
        );
        self.status_banner(ui);
        egui::ScrollArea::vertical()
            .id_salt(("fleet", self.state.scope.epoch))
            .auto_shrink([false, false])
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new("Conversation nodes")
                            .size(13.0)
                            .strong()
                            .color(design::text()),
                    );
                    if ui
                        .small_button(RichText::new("Manage conversations").size(11.0))
                        .clicked()
                    {
                        self.section = Section::Conversations;
                        self.ensure_section_data();
                    }
                });
                ui.add_space(10.0);
                self.bot_card(ui);
                ui.add_space(14.0);
                self.skill_card(ui);
                ui.add_space(8.0);
            });
    }

    fn bot_card(&mut self, ui: &mut egui::Ui) {
        let created = self.state.created_bot.clone();
        let mut list_bots = false;
        design::card(ui, |ui| {
            ui.label(
                RichText::new("New specialist")
                    .size(15.0)
                    .strong()
                    .color(design::text()),
            );
            ui.label(
                RichText::new(
                    "The id is the bot name. The bot stays disabled until the server restarts, and it is not added to main.delegates.",
                )
                .size(11.0)
                .color(design::muted()),
            );
            ui.add_space(6.0);
            ui.horizontal_wrapped(|ui| {
                ui.add(
                    egui::TextEdit::singleline(&mut self.fleet.bot_id)
                        .desired_width(150.0)
                        .char_limit(32)
                        .hint_text("bot id"),
                );
                ui.add(
                    egui::TextEdit::singleline(&mut self.fleet.bot_skills)
                        .desired_width(260.0)
                        .char_limit(512)
                        .hint_text("skills: ask, notes"),
                );
            });
            ui.add(
                egui::TextEdit::multiline(&mut self.fleet.bot_description)
                    .desired_rows(3)
                    .desired_width(f32::INFINITY)
                    .char_limit(2000)
                    .hint_text("What this specialist is for."),
            );
            ui.add_space(4.0);
            ui.label(design::meta(
                "Tools — specialist allowlist only; delegate is not offered.",
            ));
            let columns = if ui.available_width() < 520.0 { 1 } else { 2 };
            egui::ScrollArea::vertical()
                .id_salt("specialist-tools")
                .max_height(168.0)
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    egui::Grid::new("specialist-tool-grid")
                        .num_columns(columns)
                        .spacing([12.0, 4.0])
                        .show(ui, |ui| {
                            for (index, name) in SPECIALIST_TOOLS.iter().enumerate() {
                                if let Some(flag) = self.fleet.bot_tools.get_mut(index) {
                                    ui.checkbox(flag, *name);
                                }
                                if (index + 1) % columns == 0 {
                                    ui.end_row();
                                }
                            }
                            if !SPECIALIST_TOOLS.is_empty() && SPECIALIST_TOOLS.len() % columns != 0
                            {
                                ui.end_row();
                            }
                        });
                });
            let selected = self.fleet.bot_tools.iter().filter(|on| **on).count();
            if selected == 0 {
                ui.label(
                    RichText::new("No tools selected. The bot file will list an empty tool set.")
                        .size(10.0)
                        .color(design::muted()),
                );
            }
            if let Some(bot) = &created {
                ui.add_space(6.0);
                ui.label(
                    RichText::new(format!("Created {} · disabled · not live", bot.id))
                        .size(11.0)
                        .color(design::accent()),
                );
                ui.label(design::meta(format!(
                    "{} · {}",
                    bot.bot_file, bot.prompt_file
                )));
                ui.label(
                    RichText::new("Restart the server before this bot can run.")
                        .size(10.0)
                        .color(design::warning()),
                );
            }
            ui.add_space(6.0);
            let tools = self.selected_tools();
            let can_create = self.ready()
                && !self.state.unknown_outcome
                && valid_bot_id(&self.fleet.bot_id)
                && valid_bot_description(&self.fleet.bot_description)
                && validate_tool_list(&tools).is_ok()
                && parse_skill_list(&self.fleet.bot_skills).is_ok();
            if ui
                .add_enabled(can_create, design::primary("Create disabled bot"))
                .clicked()
            {
                match parse_skill_list(&self.fleet.bot_skills) {
                    Ok(skills) => {
                        self.state.error = None;
                        self.start_request(Operation::CreateBot {
                            id: self.fleet.bot_id.clone(),
                            description: Zeroizing::new(self.fleet.bot_description.clone()),
                            tools,
                            skills,
                        });
                    }
                    Err(error) => self.state.error = Some(error.to_string()),
                }
            }
            if !self.fleet.bot_id.is_empty() && !valid_bot_id(&self.fleet.bot_id) {
                ui.label(
                    RichText::new(
                        "Use a lowercase slug (max 32). main and Windows device names are reserved.",
                    )
                    .size(10.0)
                    .color(design::warning()),
                );
            }
            ui.add_space(8.0);
            design::divider(ui);
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                ui.label(design::meta("On disk"));
                if ui
                    .add_enabled(
                        self.ready() && !self.state.unknown_outcome,
                        egui::Button::new(RichText::new("Refresh bots").size(11.0)),
                    )
                    .clicked()
                {
                    list_bots = true;
                }
            });
            ui.label(
                RichText::new("Summaries only. GET /v1/bots does not return prompts or secrets.")
                    .size(10.0)
                    .color(design::muted_dim()),
            );
            if !self.state.bots_loaded {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label(design::meta("Loading bots…"));
                });
            } else if self.state.listed_bots.is_empty() {
                ui.label(design::meta("No bot files on disk."));
            }
            let listed = &self.state.listed_bots;
            if !listed.is_empty() {
                egui::ScrollArea::vertical()
                    .id_salt("fleet-bots")
                    .max_height(180.0)
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        for bot in listed {
                            ui.horizontal(|ui| {
                                ui.label(
                                    RichText::new(format!("{} · {}", bot.id, bot.role))
                                        .size(12.0)
                                        .color(design::text()),
                                );
                                design::chip(
                                    ui,
                                    if bot.enabled { "enabled" } else { "disabled" },
                                    if bot.enabled {
                                        design::accent()
                                    } else {
                                        design::muted()
                                    },
                                    design::panel_alt(),
                                );
                                if !bot.live {
                                    design::chip(
                                        ui,
                                        "restart required",
                                        design::warning(),
                                        design::warning_bg(),
                                    );
                                }
                            });
                            ui.horizontal_wrapped(|ui| {
                                ui.label(
                                    RichText::new("tools").size(10.0).color(design::muted_dim()),
                                );
                                for tool in &bot.tools {
                                    design::chip(ui, tool, design::muted(), design::panel_alt());
                                }
                            });
                            ui.horizontal_wrapped(|ui| {
                                ui.label(
                                    RichText::new("skills")
                                        .size(10.0)
                                        .color(design::muted_dim()),
                                );
                                for skill in &bot.skills {
                                    design::chip(ui, skill, design::accent(), design::ok_bg());
                                }
                            });
                            ui.add_space(6.0);
                        }
                    });
            }
        });
        if list_bots {
            self.start_request(Operation::ListBots);
        }
    }

    fn skill_card(&mut self, ui: &mut egui::Ui) {
        let created = self.state.created_skill.clone();
        let mut list_skills = false;
        design::card(ui, |ui| {
            ui.label(
                RichText::new("New skill pack")
                    .size(15.0)
                    .strong()
                    .color(design::text()),
            );
            ui.label(
                RichText::new(
                    "The id is the pack name. The body is optional markdown. The pack stays off the running catalog until a restart, and a bot cannot load it until you add the id to that bot.",
                )
                .size(11.0)
                .color(design::muted()),
            );
            ui.add_space(6.0);
            ui.add(
                egui::TextEdit::singleline(&mut self.fleet.skill_id)
                    .desired_width(200.0)
                    .char_limit(64)
                    .hint_text("skill id"),
            );
            if !self.fleet.skill_id.is_empty() && !valid_new_skill_id(&self.fleet.skill_id) {
                ui.label(
                    RichText::new(
                        "Use a lowercase slug (max 64). Windows device names are reserved.",
                    )
                    .size(10.0)
                    .color(design::warning()),
                );
            }
            ui.add(
                egui::TextEdit::singleline(&mut self.fleet.skill_description)
                    .desired_width(f32::INFINITY)
                    .char_limit(MAX_SKILL_DESCRIPTION_CHARS)
                    .hint_text("One-line summary"),
            );
            if !valid_skill_description(&self.fleet.skill_description) {
                ui.label(
                    RichText::new("One line, at most 500 characters, no quotes or backslashes.")
                        .size(10.0)
                        .color(design::warning()),
                );
            }
            ui.add(
                egui::TextEdit::multiline(&mut self.fleet.skill_body)
                    .desired_rows(4)
                    .desired_width(f32::INFINITY)
                    .char_limit(MAX_SKILL_BODY_BYTES)
                    .hint_text("Optional markdown body"),
            );
            if !valid_skill_body(self.fleet.skill_body.trim()) {
                ui.label(
                    RichText::new(
                        "Markdown only, at most 96,000 bytes, without control characters.",
                    )
                    .size(10.0)
                    .color(design::warning()),
                );
            }
            if let Some(skill) = &created {
                ui.add_space(6.0);
                ui.label(
                    RichText::new(format!("Created {} · not live", skill.id))
                        .size(11.0)
                        .color(design::accent()),
                );
                ui.label(design::meta(&skill.file));
                ui.label(
                    RichText::new(
                        "Restart the server, then add this id to a bot before it can load the pack.",
                    )
                    .size(10.0)
                    .color(design::warning()),
                );
            }
            ui.add_space(6.0);
            let can_create = self.ready()
                && !self.state.unknown_outcome
                && valid_new_skill_id(&self.fleet.skill_id)
                && valid_skill_description(&self.fleet.skill_description)
                && valid_skill_body(self.fleet.skill_body.trim());
            if ui
                .add_enabled(can_create, design::primary("Create skill"))
                .clicked()
            {
                self.state.error = None;
                self.start_request(Operation::CreateSkill {
                    id: self.fleet.skill_id.clone(),
                    description: Zeroizing::new(self.fleet.skill_description.trim().to_owned()),
                    body: Zeroizing::new(self.fleet.skill_body.trim().to_owned()),
                });
            }
            ui.add_space(8.0);
            design::divider(ui);
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                ui.label(design::meta("On disk"));
                if ui
                    .add_enabled(
                        self.ready() && !self.state.unknown_outcome,
                        egui::Button::new(RichText::new("Refresh skills").size(11.0)),
                    )
                    .clicked()
                {
                    list_skills = true;
                }
            });
            ui.label(
                RichText::new("Summaries only. GET /v1/skills does not return bodies.")
                    .size(10.0)
                    .color(design::muted_dim()),
            );
            if !self.state.skills_loaded {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label(design::meta("Loading skills…"));
                });
            } else if self.state.listed_skills.is_empty() {
                ui.label(design::meta("No skill packs on disk."));
            }
            let listed = &self.state.listed_skills;
            if !listed.is_empty() {
                egui::ScrollArea::vertical()
                    .id_salt("fleet-skills")
                    .max_height(160.0)
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        for skill in listed {
                            ui.label(
                                RichText::new(match &skill.pack {
                                    Some(pack) => format!("{} · {pack}", skill.id),
                                    None => skill.id.clone(),
                                })
                                .size(12.0)
                                .color(design::text()),
                            );
                            if !skill.description.is_empty() {
                                ui.label(design::meta(skill_blurb(&skill.description)));
                            }
                            ui.add_space(4.0);
                        }
                    });
            }
        });
        if list_skills {
            self.start_request(Operation::ListSkills);
        }
    }
}

fn skill_blurb(text: &str) -> String {
    let mut chars = text.chars();
    let blurb: String = chars.by_ref().take(160).collect();
    if chars.next().is_some() {
        format!("{blurb}…")
    } else {
        blurb
    }
}
