//! Chat pane: conversation rail, virtualized message list, composer.

use super::*;
use eframe::egui::{Align2, CornerRadius, Rect, StrokeKind, pos2, vec2};
use serabot_desktop::Message;
use std::sync::Arc;

const ROW_GAP: f32 = 10.0;
const BUBBLE_PAD_X: f32 = 14.0;
const BUBBLE_TOP: f32 = 43.0;

#[derive(Default)]
pub(crate) struct ChatView {
    layout: MessageLayout,
    scroll_to_bottom: bool,
}

impl ChatView {
    pub fn reset(&mut self) {
        *self = Self::default();
    }

    pub fn jump(&mut self) {
        self.scroll_to_bottom = true;
    }
}

struct RowLayout {
    id: String,
    content: Arc<str>,
    user: bool,
    speaker: String,
    stamp: String,
    bubble_w: f32,
    height: f32,
    galley: Arc<egui::Galley>,
}

struct MessageLayout {
    epoch: u64,
    width_bits: u32,
    fingerprint: (usize, u64, u64),
    rows: Vec<RowLayout>,
}

impl Default for MessageLayout {
    fn default() -> Self {
        Self {
            epoch: u64::MAX,
            width_bits: 0,
            fingerprint: (usize::MAX, 0, 0),
            rows: Vec::new(),
        }
    }
}

impl MessageLayout {
    /// Rebuild wrapped galleys only when the epoch, width, or message set
    /// changes. Steady-state frames reuse the cache.
    fn ensure(&mut self, ui: &egui::Ui, messages: &[Message], width: f32, epoch: u64) {
        let fingerprint = (
            messages.len(),
            messages.last().map_or(0, |message| message.id.len() as u64),
            messages
                .iter()
                .map(|message| message.content.len() as u64)
                .sum(),
        );
        let width_bits = width.to_bits();
        if self.epoch == epoch && self.width_bits == width_bits && self.fingerprint == fingerprint {
            return;
        }
        self.rows.clear();
        let bubble_max = (width * 0.78).clamp(220.0, 760.0);
        let wrap = (bubble_max - BUBBLE_PAD_X * 2.0).max(80.0);
        let font = FontId::proportional(13.5);
        for message in messages {
            let user = message.role == Role::User;
            let speaker = if user {
                "You"
            } else {
                message
                    .name
                    .as_deref()
                    .filter(|name| valid_speaker(name))
                    .unwrap_or("SeraBot")
            }
            .to_owned();
            let galley = ui.fonts(|fonts| {
                fonts.layout(message.content.clone(), font.clone(), design::TEXT, wrap)
            });
            let bubble_w = (galley.size().x + BUBBLE_PAD_X * 2.0)
                .min(bubble_max)
                .max(150.0);
            let height = galley.size().y + BUBBLE_TOP;
            self.rows.push(RowLayout {
                id: message.id.clone(),
                content: Arc::from(message.content.as_str()),
                user,
                speaker,
                stamp: design::format_stamp(&message.created_at),
                bubble_w,
                height,
                galley,
            });
        }
        self.epoch = epoch;
        self.width_bits = width_bits;
        self.fingerprint = fingerprint;
    }
}

fn kind_icon(kind: &str) -> design::NavIcon {
    match kind {
        "room" => design::NavIcon::Fleet,
        "section" | "group" => design::NavIcon::Conversations,
        _ => design::NavIcon::Chat,
    }
}

fn kind_label(kind: &str) -> String {
    let mut chars = kind.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
        None => "Thread".into(),
    }
}

impl Desktop {
    pub(crate) fn conversation_column(&mut self, ctx: &egui::Context) {
        egui::SidePanel::left("conversations")
            .exact_width(design::LIST_W)
            .resizable(false)
            .frame(
                Frame::new()
                    .fill(design::PANEL)
                    .stroke(Stroke::new(1.0_f32, design::STROKE_SOFT))
                    .inner_margin(Margin::symmetric(12, 12)),
            )
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new("Conversations")
                            .size(14.0)
                            .strong()
                            .color(design::TEXT),
                    );
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui
                            .small_button(RichText::new("New").size(11.0).color(design::ACCENT))
                            .on_hover_text("Open a fresh direct conversation")
                            .clicked()
                        {
                            self.open_conversation(format!("windows-{}", uuid::Uuid::new_v4()));
                        }
                    });
                });
                ui.add_space(4.0);
                ui.add(
                    egui::TextEdit::singleline(&mut self.conv_search)
                        .desired_width(f32::INFINITY)
                        .hint_text("Search threads"),
                );
                ui.add_space(6.0);

                let needle = self.conv_search.trim().to_lowercase();
                let current = self.conversation.clone();
                let conversations = &self.state.conversations;
                let mut open = None;
                ui.add_space(2.0);
                if !self.state.conversations_loaded {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label(design::meta("Loading threads…"));
                    });
                }
                let list_height = (ui.available_height() - 26.0).max(80.0);
                egui::ScrollArea::vertical()
                    .id_salt("conversation-list")
                    .auto_shrink([false, false])
                    .max_height(list_height)
                    .show(ui, |ui| {
                        if conversations.is_empty() && self.state.conversations_loaded {
                            design::empty_state(
                                ui,
                                "No threads yet",
                                "Create a section, group, direct thread or room in Conversations.",
                            );
                        }
                        for conversation in conversations.iter().filter(|conversation| {
                            needle.is_empty()
                                || conversation.id.to_lowercase().contains(&needle)
                                || conversation
                                    .display_title()
                                    .to_lowercase()
                                    .contains(&needle)
                        }) {
                            let selected = conversation.id == current;
                            let icon = kind_icon(&conversation.kind);
                            let response = Frame::new()
                                .fill(if selected {
                                    design::PANEL_ALT
                                } else {
                                    Color32::TRANSPARENT
                                })
                                .corner_radius(9.0)
                                .inner_margin(Margin::symmetric(8, 6))
                                .show(ui, |ui| {
                                    ui.set_width(ui.available_width());
                                    ui.horizontal(|ui| {
                                        let (rect, _) = ui.allocate_exact_size(
                                            Vec2::new(18.0, 18.0),
                                            Sense::hover(),
                                        );
                                        design::nav_icon(
                                            ui.painter(),
                                            rect,
                                            icon,
                                            if selected {
                                                design::ACCENT
                                            } else {
                                                design::ICON
                                            },
                                        );
                                        ui.vertical(|ui| {
                                            ui.spacing_mut().item_spacing.y = 1.0;
                                            ui.label(
                                                RichText::new(conversation.display_title())
                                                    .size(12.5)
                                                    .strong()
                                                    .color(design::TEXT),
                                            );
                                            let stamp = conversation
                                                .last_message_at
                                                .as_deref()
                                                .unwrap_or(&conversation.updated_at);
                                            ui.label(
                                                RichText::new(format!(
                                                    "{} · {}",
                                                    kind_label(&conversation.kind),
                                                    design::format_stamp(stamp)
                                                ))
                                                .size(10.0)
                                                .color(design::MUTED_DIM),
                                            );
                                        });
                                    });
                                })
                                .response
                                .interact(Sense::click())
                                .on_hover_text(&conversation.id);
                            if response.clicked() {
                                open = Some(conversation.id.clone());
                            }
                            ui.add_space(2.0);
                        }
                    });
                ui.with_layout(egui::Layout::bottom_up(egui::Align::LEFT), |ui| {
                    if ui
                        .add(egui::Button::new(
                            RichText::new("Manage conversations")
                                .size(11.0)
                                .color(design::MUTED),
                        ))
                        .clicked()
                    {
                        self.section = Section::Conversations;
                        self.ensure_section_data();
                    }
                });
                if let Some(id) = open {
                    self.open_conversation(id);
                }
            });
    }

    pub(crate) fn chat_screen(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.label(
                RichText::new(&self.conversation)
                    .size(11.0)
                    .monospace()
                    .color(design::MUTED_DIM),
            );
            if self.conversation_accepts_turns() {
                design::chip(ui, "accepts turns", design::MUTED, design::PANEL_ALT);
            } else {
                design::chip(ui, "read-only node", design::WARNING, design::WARNING_BG);
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui
                    .add_enabled(
                        self.ready() && self.state.connected && self.active_conversation(),
                        egui::Button::new(RichText::new("Refresh").size(11.0)),
                    )
                    .clicked()
                {
                    self.refresh();
                }
                if ui
                    .add(egui::Button::new(RichText::new("Latest").size(11.0)))
                    .on_hover_text("Jump to the newest message")
                    .clicked()
                {
                    self.chat.jump();
                }
            });
        });
        ui.add_space(6.0);
        self.status_banner(ui);
        let composer_rows = if ui.available_height() < 420.0 { 2 } else { 3 };
        ui.with_layout(egui::Layout::bottom_up(egui::Align::LEFT), |ui| {
            ui.set_width(ui.available_width());
            self.composer_block(ui, composer_rows);
            if let Some(message) = self.toast_message() {
                ui.add_space(4.0);
                ui.label(design::meta(message));
            }
            ui.add_space(6.0);
            let scroll_h = ui.available_height().max(120.0);
            self.message_area(ui, scroll_h);
        });
    }

    fn message_area(&mut self, ui: &mut egui::Ui, height: f32) {
        let epoch = self.state.scope.epoch;
        let messages = &self.state.messages;
        let loaded = self.state.history_loaded;
        let chat = &mut self.chat;
        let jump = std::mem::take(&mut chat.scroll_to_bottom);
        let mut copy: Option<Arc<str>> = None;
        egui::ScrollArea::vertical()
            .id_salt(("history", epoch))
            .auto_shrink([false, false])
            .max_height(height)
            .stick_to_bottom(true)
            .show_viewport(ui, |ui, viewport| {
                let width = ui.available_width();
                if messages.is_empty() {
                    design::empty_state(
                        ui,
                        if loaded { "No messages yet" } else { "Loading conversation" },
                        if loaded {
                            "Send a message below. Only visible user and assistant messages appear here."
                        } else {
                            "Fetching the recent history from your host."
                        },
                    );
                    return;
                }
                chat.layout.ensure(ui, messages, width, epoch);
                let origin = ui.cursor().min;
                let mut y = 0.0;
                for row in &chat.layout.rows {
                    let rect = Rect::from_min_size(
                        pos2(origin.x, origin.y + y),
                        vec2(width, row.height),
                    );
                    if rect.max.y >= viewport.min.y && rect.min.y <= viewport.max.y
                        && paint_row(ui, rect, row, epoch)
                    {
                        copy = Some(Arc::clone(&row.content));
                    }
                    y += row.height + ROW_GAP;
                }
                let content_height = y + 4.0;
                ui.allocate_space(vec2(width, content_height));
                if jump {
                    ui.scroll_to_rect(
                        Rect::from_min_size(
                            pos2(origin.x, origin.y + content_height - 1.0),
                            vec2(width, 1.0),
                        ),
                        Some(egui::Align::BOTTOM),
                    );
                }
            });
        if let Some(content) = copy {
            ui.ctx().copy_text(content.to_string());
            self.show_toast("Copied message");
        }
    }

    fn composer_block(&mut self, ui: &mut egui::Ui, rows: usize) {
        let working = self
            .state
            .task
            .as_ref()
            .is_some_and(|task| !task.status.terminal());
        if working {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label(design::meta("Working…"));
            });
            ui.add_space(4.0);
        }
        let accepts = self.conversation_accepts_turns();
        let can_type = self.state.connected
            && self.active_conversation()
            && accepts
            && !self.state.unknown_outcome;
        Frame::new()
            .fill(design::PANEL_ALT)
            .stroke(Stroke::new(1.0_f32, design::STROKE_SOFT))
            .corner_radius(12.0)
            .inner_margin(Margin::symmetric(12, 10))
            .show(ui, |ui| {
                ui.set_min_width(ui.available_width());
                ui.add_enabled(
                    can_type,
                    egui::TextEdit::multiline(&mut self.composer)
                        .desired_rows(rows)
                        .desired_width(f32::INFINITY)
                        .hint_text(if accepts {
                            "Message SeraBot…"
                        } else {
                            "This node does not accept turns"
                        })
                        .char_limit(8000),
                );
                let shortcut = ui
                    .ctx()
                    .input_mut(|input| input.consume_key(egui::Modifiers::CTRL, egui::Key::Enter));
                ui.horizontal(|ui| {
                    let enabled = self.ready()
                        && can_type
                        && self.state.can_send()
                        && valid_text(&self.composer);
                    if ui
                        .add_enabled(
                            enabled,
                            egui::Button::new(
                                RichText::new("Send").strong().color(design::ACCENT),
                            ),
                        )
                        .clicked()
                        || (enabled && shortcut)
                    {
                        self.state.error = None;
                        let text = Zeroizing::new(std::mem::take(&mut self.composer));
                        self.start_request(Operation::Submit(text));
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.label(
                            RichText::new(format!(
                                "Ctrl+Enter · {} / 8,000",
                                self.composer.chars().count()
                            ))
                            .size(10.0)
                            .color(design::MUTED_DIM),
                        );
                    });
                });
                if !accepts {
                    ui.label(
                        RichText::new(
                            "Sections and groups organize the tree. Open a direct thread or a room to talk.",
                        )
                        .size(10.0)
                        .color(design::WARNING),
                    );
                }
            });
    }
}

fn paint_row(ui: &egui::Ui, rect: Rect, row: &RowLayout, epoch: u64) -> bool {
    let x = if row.user {
        rect.right() - row.bubble_w
    } else {
        rect.left()
    };
    let bubble = Rect::from_min_size(pos2(x, rect.top()), vec2(row.bubble_w, row.height));
    let painter = ui.painter();
    painter.rect_filled(
        bubble,
        CornerRadius::same(12),
        if row.user {
            design::USER_BUBBLE
        } else {
            design::BOT_BUBBLE
        },
    );
    painter.rect_stroke(
        bubble,
        CornerRadius::same(12),
        Stroke::new(1.0_f32, design::BUBBLE_STROKE),
        StrokeKind::Inside,
    );
    painter.text(
        pos2(bubble.left() + BUBBLE_PAD_X, bubble.top() + 9.0),
        Align2::LEFT_TOP,
        &row.speaker,
        FontId::proportional(11.0),
        if row.user {
            design::ACCENT
        } else {
            design::MUTED
        },
    );
    painter.text(
        pos2(bubble.right() - BUBBLE_PAD_X, bubble.top() + 10.0),
        Align2::RIGHT_TOP,
        &row.stamp,
        FontId::proportional(10.0),
        design::MUTED_DIM,
    );
    painter.galley(
        pos2(bubble.left() + BUBBLE_PAD_X, bubble.top() + 31.0),
        Arc::clone(&row.galley),
        design::TEXT,
    );
    ui.interact(
        bubble,
        egui::Id::new(("message", epoch, &row.id)),
        Sense::click(),
    )
    .on_hover_cursor(egui::CursorIcon::PointingHand)
    .on_hover_text("Click to copy")
    .clicked()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serabot_desktop::Role;

    fn message(id: &str, role: Role, content: &str) -> Message {
        Message {
            id: id.into(),
            role,
            content: content.into(),
            created_at: "2026-10-29T10:00:00.000Z".into(),
            name: None,
        }
    }

    fn layout_for(messages: &[Message], width: f32) -> (egui::Context, MessageLayout) {
        let ctx = egui::Context::default();
        design::apply_theme(&ctx);
        let mut layout = MessageLayout::default();
        let input = egui::RawInput {
            screen_rect: Some(Rect::from_min_size(egui::Pos2::ZERO, vec2(900.0, 600.0))),
            ..Default::default()
        };
        let _ = ctx.run(input, |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                layout.ensure(ui, messages, width, 7);
            });
        });
        (ctx, layout)
    }

    #[test]
    fn rows_measure_and_reuse_until_width_or_messages_change() {
        let messages = vec![
            message("a", Role::User, "Synthetic question."),
            message(
                "b",
                Role::Assistant,
                "A longer synthetic answer that must wrap over several lines at a narrow width.",
            ),
        ];
        let (_ctx, layout) = layout_for(&messages, 420.0);
        assert_eq!(layout.rows.len(), 2);
        assert!(layout.rows.iter().all(|row| row.height > 40.0));
        assert!(layout.rows[1].height > layout.rows[0].height);
        let fingerprint = layout.fingerprint;
        assert_eq!(fingerprint.0, 2);
        assert_eq!(layout.rows[0].speaker, "You");
        assert_eq!(layout.rows[1].speaker, "SeraBot");
        let mut reused = layout;
        let input = egui::RawInput {
            screen_rect: Some(Rect::from_min_size(egui::Pos2::ZERO, vec2(900.0, 600.0))),
            ..Default::default()
        };
        let ctx = egui::Context::default();
        design::apply_theme(&ctx);
        let _ = ctx.run(input, |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                reused.ensure(ui, &messages, 420.0, 7);
            });
        });
        assert_eq!(reused.fingerprint, fingerprint);
    }

    #[test]
    fn a_new_epoch_forces_a_rebuild() {
        let messages = vec![message("a", Role::User, "Synthetic.")];
        let (ctx, mut layout) = layout_for(&messages, 500.0);
        assert_eq!(layout.epoch, 7);
        let input = egui::RawInput {
            screen_rect: Some(Rect::from_min_size(egui::Pos2::ZERO, vec2(900.0, 600.0))),
            ..Default::default()
        };
        let _ = ctx.run(input, |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                layout.ensure(ui, &messages, 500.0, 8);
            });
        });
        assert_eq!(layout.epoch, 8);
    }
}
