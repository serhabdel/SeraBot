//! Visual tokens and painted primitives shared by every desktop screen.
//!
//! The palette is dark-first with one accent. Icons are drawn with the painter
//! so the app does not depend on an emoji font or bundled image assets.

use eframe::egui::{
    self, Align2, Color32, CornerRadius, FontId, Rect, Response, RichText, Sense, Stroke,
    StrokeKind, Ui, Vec2, pos2, vec2,
};

pub const BG: Color32 = Color32::from_rgb(12, 15, 20);
pub const PANEL: Color32 = Color32::from_rgb(18, 22, 28);
pub const PANEL_ALT: Color32 = Color32::from_rgb(24, 29, 37);
pub const CARD: Color32 = Color32::from_rgb(25, 31, 39);
pub const RAIL: Color32 = Color32::from_rgb(15, 18, 24);
pub const STROKE: Color32 = Color32::from_rgb(40, 48, 58);
pub const STROKE_SOFT: Color32 = Color32::from_rgb(32, 38, 47);
pub const ACCENT: Color32 = Color32::from_rgb(92, 200, 182);
pub const ACCENT_DIM: Color32 = Color32::from_rgb(34, 74, 68);
pub const TEXT: Color32 = Color32::from_rgb(232, 238, 244);
pub const MUTED: Color32 = Color32::from_rgb(138, 150, 166);
pub const MUTED_DIM: Color32 = Color32::from_rgb(100, 110, 124);
pub const ICON: Color32 = Color32::from_rgb(150, 162, 178);
pub const WARNING: Color32 = Color32::from_rgb(232, 176, 98);
pub const WARNING_BG: Color32 = Color32::from_rgb(52, 42, 24);
pub const DANGER: Color32 = Color32::from_rgb(228, 116, 116);
pub const DANGER_BG: Color32 = Color32::from_rgb(54, 32, 32);
pub const OK_BG: Color32 = Color32::from_rgb(22, 44, 40);
pub const USER_BUBBLE: Color32 = Color32::from_rgb(30, 52, 56);
pub const BOT_BUBBLE: Color32 = Color32::from_rgb(26, 32, 40);
pub const BUBBLE_STROKE: Color32 = Color32::from_rgb(44, 56, 64);

pub const GAP: f32 = 8.0;
pub const RADIUS: u8 = 12;
pub const RAIL_W: f32 = 72.0;
pub const LIST_W: f32 = 264.0;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum NavIcon {
    Chat,
    Conversations,
    Memory,
    Providers,
    Connectors,
    Approvals,
    Fleet,
    Settings,
}

/// Install the dark theme, a single accent, focus-visible widget strokes, and
/// a compact type scale. Called once at startup.
pub fn apply_theme(ctx: &egui::Context) {
    let mut visuals = egui::Visuals::dark();
    visuals.panel_fill = PANEL;
    visuals.window_fill = PANEL;
    visuals.extreme_bg_color = BG;
    visuals.override_text_color = Some(TEXT);
    visuals.widgets.noninteractive.bg_fill = PANEL_ALT;
    visuals.widgets.noninteractive.bg_stroke = Stroke::new(1.0_f32, STROKE_SOFT);
    visuals.widgets.noninteractive.fg_stroke = Stroke::new(1.0_f32, TEXT);
    visuals.widgets.inactive.bg_fill = PANEL_ALT;
    visuals.widgets.inactive.weak_bg_fill = PANEL_ALT;
    visuals.widgets.inactive.bg_stroke = Stroke::new(1.0_f32, STROKE);
    visuals.widgets.inactive.fg_stroke = Stroke::new(1.0_f32, TEXT);
    visuals.widgets.hovered.bg_fill = Color32::from_rgb(32, 40, 50);
    visuals.widgets.hovered.weak_bg_fill = Color32::from_rgb(32, 40, 50);
    visuals.widgets.hovered.bg_stroke = Stroke::new(1.0_f32, ACCENT_DIM);
    visuals.widgets.hovered.fg_stroke = Stroke::new(1.0_f32, TEXT);
    visuals.widgets.active.bg_fill = ACCENT_DIM;
    visuals.widgets.active.weak_bg_fill = ACCENT_DIM;
    visuals.widgets.active.bg_stroke = Stroke::new(1.0_f32, ACCENT);
    visuals.widgets.active.fg_stroke = Stroke::new(1.0_f32, TEXT);
    visuals.widgets.open.bg_fill = ACCENT_DIM;
    visuals.widgets.open.bg_stroke = Stroke::new(1.0_f32, ACCENT);
    visuals.selection.bg_fill = ACCENT_DIM;
    visuals.selection.stroke = Stroke::new(1.5_f32, ACCENT);
    visuals.window_stroke = Stroke::new(1.0_f32, STROKE);
    visuals.window_corner_radius = CornerRadius::same(RADIUS);
    visuals.menu_corner_radius = CornerRadius::same(10);
    visuals.text_cursor.stroke = Stroke::new(1.5_f32, ACCENT);
    ctx.set_visuals(visuals);
    ctx.style_mut(|style| {
        style.spacing.item_spacing = Vec2::new(GAP, GAP);
        style.spacing.button_padding = Vec2::new(12.0, 6.0);
        style.spacing.interact_size.y = 30.0;
        style.spacing.window_margin = egui::Margin::same(12);
        style
            .text_styles
            .insert(egui::TextStyle::Heading, FontId::proportional(20.0));
        style
            .text_styles
            .insert(egui::TextStyle::Body, FontId::proportional(13.0));
        style
            .text_styles
            .insert(egui::TextStyle::Button, FontId::proportional(13.0));
        style
            .text_styles
            .insert(egui::TextStyle::Small, FontId::proportional(11.0));
    });
}

pub fn title(text: impl Into<String>) -> RichText {
    RichText::new(text).size(20.0).strong().color(TEXT)
}

pub fn subtitle(text: impl Into<String>) -> RichText {
    RichText::new(text).size(12.0).color(MUTED)
}

pub fn meta(text: impl Into<String>) -> RichText {
    RichText::new(text).size(11.0).color(MUTED)
}

pub fn primary(text: impl Into<String>) -> egui::Button<'static> {
    egui::Button::new(RichText::new(text.into()).size(13.0).strong().color(ACCENT))
}

pub fn secondary(text: impl Into<String>) -> egui::Button<'static> {
    egui::Button::new(RichText::new(text.into()).size(13.0).color(TEXT))
}

pub fn danger(text: impl Into<String>) -> egui::Button<'static> {
    egui::Button::new(RichText::new(text.into()).size(13.0).color(DANGER))
}

pub fn chip(ui: &mut Ui, label: &str, fg: Color32, bg: Color32) -> Response {
    egui::Frame::new()
        .fill(bg)
        .corner_radius(10.0)
        .inner_margin(egui::Margin::symmetric(8, 3))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 5.0;
                let (dot_rect, _) = ui.allocate_exact_size(vec2(6.0, 6.0), Sense::hover());
                ui.painter().circle_filled(dot_rect.center(), 3.0, fg);
                ui.label(RichText::new(label).size(11.0).strong().color(fg));
            });
        })
        .response
}

pub fn card<R>(ui: &mut Ui, add_contents: impl FnOnce(&mut Ui) -> R) -> R {
    egui::Frame::new()
        .fill(CARD)
        .stroke(Stroke::new(1.0_f32, STROKE_SOFT))
        .corner_radius(CornerRadius::same(RADIUS))
        .inner_margin(egui::Margin::symmetric(16, 14))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            add_contents(ui)
        })
        .inner
}

pub fn empty_state(ui: &mut Ui, heading: &str, body: &str) {
    ui.add_space(28.0);
    ui.vertical_centered(|ui| {
        let (rect, _) = ui.allocate_exact_size(vec2(40.0, 40.0), Sense::hover());
        nav_icon(ui.painter(), rect.shrink(6.0), NavIcon::Chat, MUTED_DIM);
        ui.add_space(6.0);
        ui.label(RichText::new(heading).size(15.0).strong().color(TEXT));
        ui.add_space(2.0);
        ui.label(RichText::new(body).size(12.0).color(MUTED));
    });
    ui.add_space(20.0);
}

pub fn header(ui: &mut Ui, heading: &str, sub: &str) {
    ui.label(title(heading));
    ui.label(subtitle(sub));
    ui.add_space(10.0);
}

pub fn divider(ui: &mut Ui) {
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 1.0), Sense::hover());
    ui.painter()
        .rect_filled(rect, CornerRadius::ZERO, STROKE_SOFT);
}

pub fn status_dot(ui: &mut Ui, color: Color32) {
    let (rect, _) = ui.allocate_exact_size(vec2(8.0, 8.0), Sense::hover());
    ui.painter().circle_filled(rect.center(), 3.5, color);
}

pub fn avatar(ui: &mut Ui, label: &str, fill: Color32) -> Response {
    let (rect, response) = ui.allocate_exact_size(vec2(30.0, 30.0), Sense::hover());
    let painter = ui.painter();
    painter.circle_filled(rect.center(), 15.0, fill);
    let initial = label
        .chars()
        .next()
        .map(|c| c.to_ascii_uppercase().to_string())
        .unwrap_or_else(|| "?".into());
    painter.text(
        rect.center(),
        Align2::CENTER_CENTER,
        initial,
        FontId::proportional(13.0),
        TEXT,
    );
    response
}

/// Copy a value to the clipboard and return whether the button was clicked.
pub fn copy_button(ui: &mut Ui, value: &str, label: &str) -> bool {
    if ui.small_button(label).clicked() {
        ui.ctx().copy_text(value.to_owned());
        true
    } else {
        false
    }
}

/// RFC 3339 to a compact local-looking stamp: `2026-10-29T14:03:12Z` becomes
/// `2026-10-29 14:03`. The server already formats timestamps in UTC.
pub fn format_stamp(raw: &str) -> String {
    let stamp: String = raw.chars().take(16).collect();
    stamp.replacen('T', " ", 1)
}

pub fn rail_item(ui: &mut Ui, icon: NavIcon, label: &str, selected: bool) -> Response {
    let width = ui.available_width();
    let (rect, response) = ui.allocate_exact_size(vec2(width, 54.0), Sense::click());
    if ui.is_rect_visible(rect) {
        let painter = ui.painter();
        let body = rect.shrink2(vec2(6.0, 2.0));
        if selected {
            painter.rect_filled(body, CornerRadius::same(10), PANEL_ALT);
            painter.rect_filled(
                Rect::from_min_size(
                    pos2(rect.left() + 2.0, rect.top() + 10.0),
                    vec2(3.0, rect.height() - 20.0),
                ),
                CornerRadius::same(2),
                ACCENT,
            );
        } else if response.hovered() {
            painter.rect_filled(body, CornerRadius::same(10), Color32::from_rgb(22, 27, 34));
        }
        let color = if selected {
            ACCENT
        } else if response.hovered() {
            TEXT
        } else {
            ICON
        };
        nav_icon(
            painter,
            Rect::from_center_size(pos2(rect.center().x, rect.top() + 20.0), vec2(20.0, 20.0)),
            icon,
            color,
        );
        painter.text(
            pos2(rect.center().x, rect.top() + 33.0),
            Align2::CENTER_TOP,
            label,
            FontId::proportional(9.5),
            color,
        );
    }
    response.on_hover_text(label)
}

pub fn nav_icon(painter: &egui::Painter, rect: Rect, icon: NavIcon, color: Color32) {
    let s = rect.width().min(rect.height());
    let c = rect.center();
    let stroke = Stroke::new(1.6_f32, color);
    match icon {
        NavIcon::Chat => {
            let bubble = Rect::from_center_size(c - vec2(0.0, s * 0.05), vec2(s * 0.66, s * 0.5));
            painter.rect_stroke(bubble, CornerRadius::same(4), stroke, StrokeKind::Inside);
            painter.line_segment(
                [
                    pos2(bubble.left() + s * 0.12, bubble.bottom()),
                    pos2(bubble.left() + s * 0.06, bubble.bottom() + s * 0.12),
                ],
                stroke,
            );
            painter.line_segment(
                [
                    pos2(bubble.left() + s * 0.12, bubble.bottom()),
                    pos2(bubble.left() + s * 0.26, bubble.bottom()),
                ],
                stroke,
            );
        }
        NavIcon::Conversations => {
            let back = Rect::from_min_size(
                pos2(c.x - s * 0.36, c.y - s * 0.30),
                vec2(s * 0.52, s * 0.44),
            );
            let front = Rect::from_min_size(
                pos2(c.x - s * 0.20, c.y - s * 0.14),
                vec2(s * 0.52, s * 0.44),
            );
            painter.rect_stroke(
                back,
                CornerRadius::same(3),
                Stroke::new(1.3_f32, color.gamma_multiply(0.7)),
                StrokeKind::Inside,
            );
            painter.rect_filled(front, CornerRadius::same(3), PANEL_ALT);
            painter.rect_stroke(front, CornerRadius::same(3), stroke, StrokeKind::Inside);
        }
        NavIcon::Memory => {
            let rx = s * 0.30;
            let ry = s * 0.11;
            ellipse(painter, c - vec2(0.0, s * 0.22), rx, ry, stroke);
            ellipse(painter, c, rx, ry, stroke);
            ellipse(painter, c + vec2(0.0, s * 0.22), rx, ry, stroke);
            for dx in [-rx, rx] {
                painter.line_segment(
                    [
                        pos2(c.x + dx, c.y - s * 0.22),
                        pos2(c.x + dx, c.y + s * 0.22),
                    ],
                    stroke,
                );
            }
        }
        NavIcon::Providers => {
            for (index, offset) in [-s * 0.22, 0.0, s * 0.22].into_iter().enumerate() {
                let y = c.y + offset;
                let dot = pos2(c.x - s * 0.28, y);
                painter.circle_filled(dot, s * 0.05, color);
                let start = pos2(c.x - s * 0.16, y);
                let end = pos2(c.x + s * 0.30 - index as f32 * s * 0.04, y);
                painter.line_segment([start, end], stroke);
            }
        }
        NavIcon::Connectors => {
            painter.circle_stroke(c - vec2(s * 0.12, 0.0), s * 0.22, stroke);
            painter.circle_stroke(c + vec2(s * 0.12, 0.0), s * 0.22, stroke);
        }
        NavIcon::Approvals => {
            painter.circle_stroke(c, s * 0.32, stroke);
            painter.line_segment(
                [c + vec2(-s * 0.14, 0.0), c + vec2(-s * 0.03, s * 0.12)],
                Stroke::new(1.8_f32, color),
            );
            painter.line_segment(
                [c + vec2(-s * 0.03, s * 0.12), c + vec2(s * 0.16, -s * 0.12)],
                Stroke::new(1.8_f32, color),
            );
        }
        NavIcon::Fleet => {
            let size = s * 0.22;
            for (dx, dy) in [(-1.0, -1.0), (0.2, -1.0), (-1.0, 0.2), (0.2, 0.2)] {
                let r = Rect::from_min_size(
                    pos2(c.x + dx * s * 0.32, c.y + dy * s * 0.32),
                    vec2(size, size),
                );
                painter.rect_stroke(r, CornerRadius::same(2), stroke, StrokeKind::Inside);
            }
        }
        NavIcon::Settings => {
            painter.circle_stroke(c, s * 0.16, stroke);
            for step in 0..6 {
                let angle = std::f32::consts::TAU * step as f32 / 6.0;
                let inner = c + vec2(angle.cos(), angle.sin()) * s * 0.24;
                let outer = c + vec2(angle.cos(), angle.sin()) * s * 0.34;
                painter.line_segment([inner, outer], stroke);
            }
        }
    }
}

fn ellipse(painter: &egui::Painter, center: egui::Pos2, rx: f32, ry: f32, stroke: Stroke) {
    const SEGMENTS: usize = 18;
    let mut points = Vec::with_capacity(SEGMENTS + 1);
    for step in 0..=SEGMENTS {
        let angle = std::f32::consts::TAU * step as f32 / SEGMENTS as f32;
        points.push(center + vec2(angle.cos() * rx, angle.sin() * ry));
    }
    painter.add(egui::Shape::line(points, stroke));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stamps_are_compact_and_never_exceed_the_source() {
        assert_eq!(format_stamp("2026-10-29T14:03:12.000Z"), "2026-10-29 14:03");
        assert_eq!(format_stamp("2026-10-29"), "2026-10-29");
        assert_eq!(format_stamp(""), "");
    }

    #[test]
    fn every_icon_paints_without_panicking() {
        let ctx = egui::Context::default();
        let input = egui::RawInput {
            screen_rect: Some(Rect::from_min_size(egui::Pos2::ZERO, vec2(320.0, 320.0))),
            ..Default::default()
        };
        let output = ctx.run(input, |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                for icon in [
                    NavIcon::Chat,
                    NavIcon::Conversations,
                    NavIcon::Memory,
                    NavIcon::Providers,
                    NavIcon::Connectors,
                    NavIcon::Approvals,
                    NavIcon::Fleet,
                    NavIcon::Settings,
                ] {
                    rail_item(ui, icon, "Item", true);
                    rail_item(ui, icon, "Item", false);
                }
                chip(ui, "Connected", ACCENT, OK_BG);
                card(ui, |ui| {
                    ui.label(title("Card"));
                    empty_state(ui, "Empty", "Nothing here");
                });
            });
        });
        assert!(!output.shapes.is_empty());
    }
}
