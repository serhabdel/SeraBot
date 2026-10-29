//! Owner-scoped memory: list, search, add and delete reviewable facts.

use super::*;
use serabot_desktop::{MEMORY_KINDS, valid_memory_content, valid_memory_kind, valid_memory_source};

impl Desktop {
    pub(crate) fn memory_screen(&mut self, ui: &mut egui::Ui) {
        design::header(
            ui,
            "Memory",
            "Owner-approved facts the orchestrator may reuse",
        );
        self.status_banner(ui);
        egui::ScrollArea::vertical()
            .id_salt("memory-screen")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                self.memory_add_card(ui);
                ui.add_space(14.0);
                self.memory_toolbar(ui);
                ui.add_space(8.0);
                self.memory_list(ui);
            });
    }

    fn memory_add_card(&mut self, ui: &mut egui::Ui) {
        design::card(ui, |ui| {
            ui.label(
                RichText::new("Add a memory")
                    .size(15.0)
                    .strong()
                    .color(design::TEXT),
            );
            ui.label(
                RichText::new(
                    "Memories are owner-scoped and reviewable. The orchestrator receives the 20 most recently updated entries as facts; specialists never list them on their own.",
                )
                .size(11.0)
                .color(design::MUTED),
            );
            ui.add_space(8.0);
            ui.horizontal_wrapped(|ui| {
                egui::ComboBox::from_id_salt("memory-kind")
                    .width(140.0)
                    .selected_text(&self.memory.kind)
                    .show_ui(ui, |ui| {
                        for kind in MEMORY_KINDS {
                            if ui
                                .selectable_label(self.memory.kind == *kind, *kind)
                                .clicked()
                            {
                                self.memory.kind = (*kind).to_owned();
                            }
                        }
                    });
                ui.add(
                    egui::TextEdit::singleline(&mut self.memory.source)
                        .desired_width(240.0)
                        .char_limit(512)
                        .hint_text("source (operator:desktop)"),
                );
            });
            ui.add(
                egui::TextEdit::multiline(&mut self.memory.content)
                    .desired_rows(3)
                    .desired_width(f32::INFINITY)
                    .char_limit(4_000)
                    .hint_text("A durable preference, fact, decision, context or note…"),
            );
            ui.horizontal(|ui| {
                let can_add = self.ready()
                    && !self.state.unknown_outcome
                    && valid_memory_kind(&self.memory.kind)
                    && valid_memory_content(&self.memory.content)
                    && valid_memory_source(&self.memory.source);
                if ui
                    .add_enabled(can_add, design::primary("Add memory"))
                    .clicked()
                {
                    self.state.error = None;
                    self.start_request(Operation::AddMemory {
                        kind: self.memory.kind.clone(),
                        content: Zeroizing::new(self.memory.content.clone()),
                        source: self.memory.source.trim().to_owned(),
                    });
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.label(
                        RichText::new(format!("{} / 4,000", self.memory.content.chars().count()))
                            .size(10.0)
                            .color(design::MUTED_DIM),
                    );
                });
            });
        });
    }

    fn memory_toolbar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.label(
                RichText::new("Saved memories")
                    .size(14.0)
                    .strong()
                    .color(design::TEXT),
            );
            if self.state.memories_loaded {
                design::chip(
                    ui,
                    &format!("{}", self.state.memories.len()),
                    design::MUTED,
                    design::PANEL_ALT,
                );
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui
                    .add_enabled(
                        self.ready() && self.state.connected,
                        egui::Button::new(RichText::new("Search").size(11.0)),
                    )
                    .clicked()
                {
                    self.start_request(Operation::ListMemories {
                        query: self.memory.query_opt(),
                    });
                }
                if ui
                    .add_enabled(
                        !self.memory.query.is_empty(),
                        egui::Button::new(RichText::new("Clear").size(11.0)),
                    )
                    .clicked()
                {
                    self.memory.query.clear();
                    self.start_request(Operation::ListMemories { query: None });
                }
                ui.add(
                    egui::TextEdit::singleline(&mut self.memory.query)
                        .desired_width(220.0)
                        .hint_text("Filter content, kind or source"),
                );
            });
        });
    }

    fn memory_list(&mut self, ui: &mut egui::Ui) {
        if !self.state.memories_loaded {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label(design::meta("Loading memories…"));
            });
            return;
        }
        if self.state.memories.is_empty() {
            design::empty_state(
                ui,
                if self.memory.query.trim().is_empty() {
                    "No memories yet"
                } else {
                    "No matching memories"
                },
                "Add a durable preference or decision above so it survives restarts.",
            );
            return;
        }
        let memories = &self.state.memories;
        let draft = &mut self.memory;
        let mut delete: Option<String> = None;
        let mut cancel = false;
        for memory in memories {
            let confirming = draft.delete_confirm.as_deref() == Some(memory.id.as_str());
            design::card(ui, |ui| {
                ui.horizontal(|ui| {
                    design::chip(ui, &memory.kind, design::ACCENT, design::OK_BG);
                    ui.label(design::meta(format!(
                        "{} · {}",
                        design::format_stamp(&memory.created_at),
                        memory.source
                    )));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if confirming {
                            if ui.add(design::danger("Confirm delete").small()).clicked() {
                                delete = Some(memory.id.clone());
                            }
                            if ui.small_button("Cancel").clicked() {
                                cancel = true;
                            }
                        } else if ui.small_button("Delete").clicked() {
                            draft.delete_confirm = Some(memory.id.clone());
                        }
                    });
                });
                ui.add_space(4.0);
                ui.label(
                    RichText::new(&memory.content)
                        .size(13.0)
                        .color(design::TEXT),
                );
                ui.add_space(2.0);
                ui.label(
                    RichText::new(&memory.id)
                        .size(9.5)
                        .monospace()
                        .color(design::MUTED_DIM),
                );
            });
            ui.add_space(6.0);
        }
        if cancel {
            self.memory.delete_confirm = None;
        }
        if let Some(id) = delete {
            self.memory.delete_confirm = None;
            self.state.error = None;
            self.start_request(Operation::DeleteMemory { id });
        }
    }
}
