//! Conversation directory: create sections, groups, direct threads and rooms.

use super::*;
use serabot_desktop::{ConversationSummary, valid_conversation_title};

enum Action {
    Open(String),
    Rename(ConversationSummary),
    SaveRename,
    CancelRename,
    Archive(String),
}

impl Desktop {
    pub(crate) fn conversations_screen(&mut self, ui: &mut egui::Ui) {
        design::header(
            ui,
            "Conversations",
            "Sections, groups, direct threads and rooms",
        );
        self.status_banner(ui);
        egui::ScrollArea::vertical()
            .id_salt("conversation-directory")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                self.create_conversation_card(ui);
                ui.add_space(14.0);
                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new("Directory")
                            .size(14.0)
                            .strong()
                            .color(design::text()),
                    );
                    if !self.state.conversations.is_empty() {
                        design::chip(
                            ui,
                            &format!("{} threads", self.state.conversations.len()),
                            design::muted(),
                            design::panel_alt(),
                        );
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui
                            .add_enabled(
                                self.ready() && self.state.connected,
                                egui::Button::new(RichText::new("Refresh").size(11.0)),
                            )
                            .clicked()
                        {
                            self.start_request(Operation::ListConversations {
                                include_archived: false,
                            });
                        }
                    });
                });
                ui.add_space(4.0);
                ui.add(
                    egui::TextEdit::singleline(&mut self.conv_search)
                        .desired_width(f32::INFINITY)
                        .hint_text("Filter by title or id"),
                );
                ui.add_space(8.0);
                self.conversation_directory(ui);
            });
    }

    fn conversation_directory(&mut self, ui: &mut egui::Ui) {
        if !self.state.conversations_loaded {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label(design::meta("Loading the directory…"));
            });
            return;
        }
        if self.state.conversations.is_empty() {
            design::empty_state(
                ui,
                "No conversations yet",
                "Create a section first, then a group or a direct thread beneath it.",
            );
            return;
        }
        let needle = self.conv_search.trim().to_lowercase();
        let conversations = &self.state.conversations;
        let rename = &mut self.rename;
        let archive_confirm = &mut self.archive_confirm;
        let mut action: Option<Action> = None;
        for conversation in conversations.iter().filter(|conversation| {
            needle.is_empty()
                || conversation.id.to_lowercase().contains(&needle)
                || conversation
                    .display_title()
                    .to_lowercase()
                    .contains(&needle)
        }) {
            let renaming = rename
                .as_ref()
                .is_some_and(|draft| draft.id == conversation.id);
            let confirming = archive_confirm.as_deref() == Some(conversation.id.as_str());
            design::card(ui, |ui| {
                ui.horizontal(|ui| {
                    let (rect, _) = ui.allocate_exact_size(Vec2::new(20.0, 20.0), Sense::hover());
                    design::nav_icon(
                        ui.painter(),
                        rect,
                        match conversation.kind.as_str() {
                            "room" => design::NavIcon::Fleet,
                            "section" | "group" => design::NavIcon::Conversations,
                            _ => design::NavIcon::Chat,
                        },
                        design::accent(),
                    );
                    if renaming {
                        if let Some(draft) = rename.as_mut() {
                            ui.add(
                                egui::TextEdit::singleline(&mut draft.title)
                                    .desired_width(260.0)
                                    .char_limit(120)
                                    .hint_text("Conversation title"),
                            );
                        }
                        if ui.small_button("Save").clicked() {
                            action = Some(Action::SaveRename);
                        }
                        if ui.small_button("Cancel").clicked() {
                            action = Some(Action::CancelRename);
                        }
                    } else {
                        ui.label(
                            RichText::new(conversation.display_title())
                                .size(14.0)
                                .strong()
                                .color(design::text()),
                        );
                        design::chip(
                            ui,
                            &kind_title(&conversation.kind),
                            design::muted(),
                            design::panel_alt(),
                        );
                        if conversation.archived_at.is_some() {
                            design::chip(ui, "archived", design::warning(), design::warning_bg());
                        }
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui
                            .add_enabled(
                                !renaming,
                                egui::Button::new(RichText::new("Open").size(11.0)),
                            )
                            .clicked()
                        {
                            action = Some(Action::Open(conversation.id.clone()));
                        }
                        let archive_label = if confirming {
                            "Confirm archive"
                        } else {
                            "Archive"
                        };
                        let archive = if confirming {
                            design::danger_button(archive_label).small()
                        } else {
                            egui::Button::new(RichText::new(archive_label).size(11.0)).small()
                        };
                        if ui.add_enabled(!renaming, archive).clicked() {
                            action = Some(Action::Archive(conversation.id.clone()));
                        }
                        if ui
                            .add_enabled(
                                !renaming,
                                egui::Button::new(RichText::new("Rename").size(11.0)),
                            )
                            .clicked()
                        {
                            action = Some(Action::Rename(conversation.clone()));
                        }
                    });
                });
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new(&conversation.id)
                            .size(10.0)
                            .monospace()
                            .color(design::muted_dim()),
                    );
                    if let Some(parent) = &conversation.parent_id {
                        ui.label(design::meta(format!("· under {parent}")));
                    }
                    ui.label(design::meta(format!(
                        "· updated {}",
                        design::format_stamp(&conversation.updated_at)
                    )));
                    if conversation.kind == "room" {
                        ui.label(design::meta("· orchestrator + specialists"));
                    }
                    ui.label(design::meta(if conversation.accepts_turns {
                        "· accepts turns"
                    } else {
                        "· structure only"
                    }));
                });
                if renaming {
                    ui.add_space(4.0);
                    ui.label(
                        RichText::new("Titles are stored on the server and shown in every client.")
                            .size(10.0)
                            .color(design::muted()),
                    );
                }
            });
            ui.add_space(6.0);
        }
        if let Some(action) = action {
            match action {
                Action::Open(id) => self.open_conversation(id),
                Action::Rename(conversation) => {
                    self.rename = Some(RenameDraft {
                        id: conversation.id.clone(),
                        title: conversation.display_title().to_owned(),
                    });
                }
                Action::CancelRename => self.rename = None,
                Action::SaveRename => {
                    if let Some(draft) = self.rename.as_ref() {
                        let title = draft.title.trim();
                        if !valid_conversation_title(title) {
                            self.show_toast("Title must be 1–120 characters.");
                        } else {
                            let id = draft.id.clone();
                            let title = Zeroizing::new(title.to_owned());
                            self.state.error = None;
                            self.start_request(Operation::RenameConversation { id, title });
                        }
                    }
                }
                Action::Archive(id) => {
                    if self.archive_confirm.as_deref() == Some(id.as_str()) {
                        // Keep the id until the reply so a successful archive of
                        // the open thread can switch back to the owner thread.
                        self.state.error = None;
                        self.start_request(Operation::ArchiveConversation { id });
                    } else {
                        self.archive_confirm = Some(id);
                    }
                }
            }
        }
    }

    fn create_conversation_card(&mut self, ui: &mut egui::Ui) {
        design::card(ui, |ui| {
            ui.label(
                RichText::new("New conversation")
                    .size(15.0)
                    .strong()
                    .color(design::text()),
            );
            ui.label(
                RichText::new(
                    "Sections and groups organize the tree. Direct threads and rooms accept turns. A room talks to the orchestrator and the specialists it delegates to; it is not a multi-person chat.",
                )
                .size(11.0)
                .color(design::muted()),
            );
            ui.add_space(8.0);
            ui.horizontal_wrapped(|ui| {
                egui::ComboBox::from_id_salt("conversation-kind")
                    .width(140.0)
                    .selected_text(self.fleet.conv_kind.label())
                    .show_ui(ui, |ui| {
                        for kind in ConversationKind::ALL {
                            if ui
                                .selectable_label(self.fleet.conv_kind == kind, kind.label())
                                .clicked()
                            {
                                self.fleet.conv_kind = kind;
                                if kind == ConversationKind::Section {
                                    self.fleet.conv_parent.clear();
                                }
                            }
                        }
                    });
                ui.add(
                    egui::TextEdit::singleline(&mut self.fleet.conv_slug)
                        .desired_width(160.0)
                        .char_limit(32)
                        .hint_text("slug"),
                );
                if self.fleet.conv_kind != ConversationKind::Section {
                    ui.add(
                        egui::TextEdit::singleline(&mut self.fleet.conv_parent)
                            .desired_width(180.0)
                            .char_limit(128)
                            .hint_text(if self.fleet.conv_kind == ConversationKind::Group {
                                "parent sec:finance"
                            } else {
                                "parent sec:finance or grp:ops"
                            }),
                    );
                }
            });
            if let Some(id) = conversation_node_id(self.fleet.conv_kind, &self.fleet.conv_slug) {
                ui.label(design::meta(format!("Server id {id}")));
            }
            let parent_owned = if self.fleet.conv_kind == ConversationKind::Section {
                None
            } else {
                let trimmed = self.fleet.conv_parent.trim();
                (!trimmed.is_empty()).then(|| trimmed.to_owned())
            };
            let parent_ok =
                valid_conversation_parent(self.fleet.conv_kind, parent_owned.as_deref()).is_ok();
            if self.fleet.conv_kind != ConversationKind::Section
                && !self.fleet.conv_parent.trim().is_empty()
                && !parent_ok
            {
                ui.label(
                    RichText::new("Parent must be an existing sec: or grp: id.")
                        .size(10.0)
                        .color(design::warning()),
                );
            }
            ui.add_space(6.0);
            let can_create = self.ready()
                && !self.state.unknown_outcome
                && valid_slug(&self.fleet.conv_slug)
                && parent_ok;
            if ui
                .add_enabled(can_create, design::primary("Create conversation"))
                .clicked()
            {
                self.state.error = None;
                self.start_request(Operation::CreateConversation {
                    kind: self.fleet.conv_kind,
                    slug: self.fleet.conv_slug.clone(),
                    parent_id: parent_owned,
                });
            }
            if let Some(node) = &self.state.created_conversation {
                ui.add_space(6.0);
                ui.label(
                    RichText::new(format!(
                        "Created {} · {}",
                        node.id,
                        if node.accepts_turns {
                            "accepts turns"
                        } else {
                            "structure only"
                        }
                    ))
                    .size(11.0)
                    .color(design::accent()),
                );
                if node.accepts_turns && ui.small_button("Open conversation").clicked() {
                    self.open_conversation(node.id.clone());
                }
            }
        });
    }
}

fn kind_title(kind: &str) -> String {
    let kind = if kind == "legacy" { "direct" } else { kind };
    let mut chars = kind.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
        None => "Thread".into(),
    }
}
