//! Visual tokens and painted primitives shared by every desktop screen.
//!
//! Two palettes (dark and light) live behind a thread-local so the whole shell
//! can switch themes at runtime without threading a palette through every
//! widget. Icons and accents are painted, so the build ships no image assets.

use eframe::egui::{
    self, Align2, Color32, CornerRadius, FontId, Rect, Response, RichText, Sense, Stroke,
    StrokeKind, Ui, Vec2, pos2, vec2,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ThemeMode {
    Dark,
    Light,
}

impl ThemeMode {
    pub fn label(self) -> &'static str {
        match self {
            Self::Dark => "Dark",
            Self::Light => "Light",
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Dark => "dark",
            Self::Light => "light",
        }
    }

    pub fn from_stored(value: Option<&str>) -> Self {
        match value {
            Some("light") => Self::Light,
            _ => Self::Dark,
        }
    }
}

#[derive(Clone, Copy)]
pub struct Palette {
    pub bg: Color32,
    pub bg_top: Color32,
    pub bg_bottom: Color32,
    pub panel: Color32,
    pub panel_alt: Color32,
    pub card: Color32,
    pub rail: Color32,
    pub rail_top: Color32,
    pub rail_bottom: Color32,
    pub stroke: Color32,
    pub stroke_soft: Color32,
    pub accent: Color32,
    pub accent_dim: Color32,
    pub accent_amber: Color32,
    pub text: Color32,
    pub muted: Color32,
    pub muted_dim: Color32,
    pub icon: Color32,
    pub warning: Color32,
    pub warning_bg: Color32,
    pub danger: Color32,
    pub danger_bg: Color32,
    pub ok_bg: Color32,
    pub user_bubble: Color32,
    pub bot_bubble: Color32,
    pub bubble_stroke: Color32,
    pub shadow: Color32,
    pub avatar_text: Color32,
}

impl Palette {
    pub const DARK: Self = Self {
        bg: Color32::from_rgb(11, 15, 20),
        bg_top: Color32::from_rgb(14, 20, 27),
        bg_bottom: Color32::from_rgb(8, 11, 15),
        panel: Color32::from_rgb(17, 22, 29),
        panel_alt: Color32::from_rgb(26, 33, 43),
        card: Color32::from_rgb(21, 28, 37),
        rail: Color32::from_rgb(10, 14, 19),
        rail_top: Color32::from_rgb(12, 18, 25),
        rail_bottom: Color32::from_rgb(7, 10, 14),
        stroke: Color32::from_rgb(44, 55, 68),
        stroke_soft: Color32::from_rgb(32, 42, 53),
        accent: Color32::from_rgb(94, 234, 212),
        accent_dim: Color32::from_rgb(19, 78, 74),
        accent_amber: Color32::from_rgb(245, 158, 11),
        text: Color32::from_rgb(237, 242, 247),
        muted: Color32::from_rgb(154, 167, 180),
        muted_dim: Color32::from_rgb(107, 119, 133),
        icon: Color32::from_rgb(138, 152, 166),
        warning: Color32::from_rgb(251, 191, 36),
        warning_bg: Color32::from_rgb(59, 42, 18),
        danger: Color32::from_rgb(248, 113, 113),
        danger_bg: Color32::from_rgb(59, 29, 29),
        ok_bg: Color32::from_rgb(18, 51, 44),
        user_bubble: Color32::from_rgb(18, 59, 58),
        bot_bubble: Color32::from_rgb(21, 28, 37),
        bubble_stroke: Color32::from_rgb(42, 57, 69),
        shadow: Color32::from_black_alpha(110),
        avatar_text: Color32::from_rgb(6, 42, 38),
    };

    pub const LIGHT: Self = Self {
        bg: Color32::from_rgb(245, 243, 238),
        bg_top: Color32::from_rgb(250, 248, 243),
        bg_bottom: Color32::from_rgb(239, 235, 226),
        panel: Color32::from_rgb(251, 249, 244),
        panel_alt: Color32::from_rgb(240, 236, 227),
        card: Color32::from_rgb(255, 255, 255),
        rail: Color32::from_rgb(236, 231, 220),
        rail_top: Color32::from_rgb(240, 236, 227),
        rail_bottom: Color32::from_rgb(230, 224, 211),
        stroke: Color32::from_rgb(215, 208, 194),
        stroke_soft: Color32::from_rgb(230, 224, 212),
        accent: Color32::from_rgb(15, 118, 110),
        accent_dim: Color32::from_rgb(204, 251, 241),
        accent_amber: Color32::from_rgb(180, 83, 9),
        text: Color32::from_rgb(27, 36, 33),
        muted: Color32::from_rgb(88, 100, 95),
        muted_dim: Color32::from_rgb(137, 147, 142),
        icon: Color32::from_rgb(95, 107, 102),
        warning: Color32::from_rgb(180, 83, 9),
        warning_bg: Color32::from_rgb(254, 243, 199),
        danger: Color32::from_rgb(185, 28, 28),
        danger_bg: Color32::from_rgb(254, 226, 226),
        ok_bg: Color32::from_rgb(220, 252, 231),
        user_bubble: Color32::from_rgb(223, 247, 243),
        bot_bubble: Color32::from_rgb(255, 255, 255),
        bubble_stroke: Color32::from_rgb(217, 230, 225),
        shadow: Color32::from_black_alpha(28),
        avatar_text: Color32::WHITE,
    };
}

thread_local! {
    static CURRENT: std::cell::Cell<Palette> = const { std::cell::Cell::new(Palette::DARK) };
}

pub fn palette() -> Palette {
    CURRENT.with(|current| current.get())
}

macro_rules! palette_color {
    ($($name:ident => $field:ident),* $(,)?) => {
        $(
            pub fn $name() -> Color32 { palette().$field }
        )*
    };
}

palette_color!(
    bg => bg,
    panel => panel,
    panel_alt => panel_alt,
    card_color => card,
    rail_color => rail,
    stroke_color => stroke,
    stroke_soft => stroke_soft,
    accent => accent,
    accent_dim => accent_dim,
    accent_amber => accent_amber,
    text => text,
    muted => muted,
    muted_dim => muted_dim,
    icon => icon,
    warning => warning,
    warning_bg => warning_bg,
    danger => danger,
    danger_bg => danger_bg,
    ok_bg => ok_bg,
    user_bubble => user_bubble,
    bot_bubble => bot_bubble,
    bubble_stroke => bubble_stroke,
    shadow_color => shadow,
);

pub const GAP: f32 = 8.0;
pub const RADIUS: u8 = 14;
pub const RAIL_W: f32 = 76.0;
pub const LIST_W: f32 = 268.0;

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

/// Install the selected palette and matching egui visuals.
pub fn apply_theme(ctx: &egui::Context, mode: ThemeMode) {
    let palette = match mode {
        ThemeMode::Dark => Palette::DARK,
        ThemeMode::Light => Palette::LIGHT,
    };
    CURRENT.with(|current| current.set(palette));
    let mut visuals = match mode {
        ThemeMode::Dark => egui::Visuals::dark(),
        ThemeMode::Light => egui::Visuals::light(),
    };
    visuals.panel_fill = palette.panel;
    visuals.window_fill = palette.panel;
    visuals.extreme_bg_color = palette.bg;
    visuals.override_text_color = Some(palette.text);
    visuals.widgets.noninteractive.bg_fill = palette.panel_alt;
    visuals.widgets.noninteractive.bg_stroke = Stroke::new(1.0_f32, palette.stroke_soft);
    visuals.widgets.noninteractive.fg_stroke = Stroke::new(1.0_f32, palette.text);
    visuals.widgets.inactive.bg_fill = palette.panel_alt;
    visuals.widgets.inactive.weak_bg_fill = palette.panel_alt;
    visuals.widgets.inactive.bg_stroke = Stroke::new(1.0_f32, palette.stroke);
    visuals.widgets.inactive.fg_stroke = Stroke::new(1.0_f32, palette.text);
    visuals.widgets.hovered.bg_fill = palette.accent_dim;
    visuals.widgets.hovered.weak_bg_fill = palette.accent_dim;
    visuals.widgets.hovered.bg_stroke = Stroke::new(1.0_f32, palette.accent);
    visuals.widgets.hovered.fg_stroke = Stroke::new(1.0_f32, palette.text);
    visuals.widgets.active.bg_fill = palette.accent_dim;
    visuals.widgets.active.weak_bg_fill = palette.accent_dim;
    visuals.widgets.active.bg_stroke = Stroke::new(1.0_f32, palette.accent);
    visuals.widgets.active.fg_stroke = Stroke::new(1.0_f32, palette.text);
    visuals.widgets.open.bg_fill = palette.accent_dim;
    visuals.widgets.open.bg_stroke = Stroke::new(1.0_f32, palette.accent);
    visuals.selection.bg_fill = palette.accent_dim;
    visuals.selection.stroke = Stroke::new(1.5_f32, palette.accent);
    visuals.window_stroke = Stroke::new(1.0_f32, palette.stroke);
    visuals.window_corner_radius = CornerRadius::same(RADIUS);
    visuals.menu_corner_radius = CornerRadius::same(12);
    visuals.text_cursor.stroke = Stroke::new(1.5_f32, palette.accent);
    ctx.set_visuals(visuals);
    ctx.style_mut(|style| {
        style.spacing.item_spacing = Vec2::new(GAP, GAP);
        style.spacing.button_padding = Vec2::new(12.0, 6.0);
        style.spacing.interact_size.y = 30.0;
        style.spacing.window_margin = egui::Margin::same(12);
        style
            .text_styles
            .insert(egui::TextStyle::Heading, FontId::proportional(21.0));
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
    RichText::new(text).size(21.0).strong().color(self::text())
}

pub fn subtitle(text: impl Into<String>) -> RichText {
    RichText::new(text).size(12.0).color(muted())
}

pub fn meta(text: impl Into<String>) -> RichText {
    RichText::new(text).size(11.0).color(muted())
}

pub fn primary(text: impl Into<String>) -> egui::Button<'static> {
    egui::Button::new(
        RichText::new(text.into())
            .size(13.0)
            .strong()
            .color(accent()),
    )
}

pub fn secondary(text: impl Into<String>) -> egui::Button<'static> {
    egui::Button::new(RichText::new(text.into()).size(13.0).color(self::text()))
}

pub fn danger_button(text: impl Into<String>) -> egui::Button<'static> {
    egui::Button::new(RichText::new(text.into()).size(13.0).color(danger()))
}

/// Soft vertical gradient used behind the shell. Cheap enough per frame.
pub fn paint_backdrop(ui: &egui::Ui) {
    let palette = palette();
    paint_vertical(ui, palette.bg_top, palette.bg_bottom);
}

pub fn paint_rail_backdrop(ui: &egui::Ui) {
    let palette = palette();
    paint_vertical(ui, palette.rail_top, palette.rail_bottom);
}

fn paint_vertical(ui: &egui::Ui, top: Color32, bottom: Color32) {
    let rect = ui.max_rect();
    let mut mesh = egui::epaint::Mesh::default();
    mesh.colored_vertex(rect.left_top(), top);
    mesh.colored_vertex(rect.right_top(), top);
    mesh.colored_vertex(rect.right_bottom(), bottom);
    mesh.colored_vertex(rect.left_bottom(), bottom);
    mesh.add_triangle(0, 1, 2);
    mesh.add_triangle(0, 2, 3);
    ui.painter().add(egui::Shape::mesh(mesh));
}

pub fn chip(ui: &mut Ui, label: &str, fg: Color32, bg: Color32) -> Response {
    egui::Frame::new()
        .fill(bg)
        .corner_radius(11.0)
        .inner_margin(egui::Margin::symmetric(9, 3))
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
        .fill(card_color())
        .stroke(Stroke::new(1.0_f32, stroke_soft()))
        .corner_radius(CornerRadius::same(RADIUS))
        .inner_margin(egui::Margin::symmetric(18, 16))
        .shadow(egui::epaint::Shadow {
            offset: [0, 6],
            blur: 20,
            spread: 0,
            color: shadow_color(),
        })
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            add_contents(ui)
        })
        .inner
}

pub fn empty_state(ui: &mut Ui, heading: &str, body: &str) {
    ui.add_space(26.0);
    ui.vertical_centered(|ui| {
        let (rect, _) = ui.allocate_exact_size(vec2(72.0, 72.0), Sense::hover());
        ui.painter()
            .circle_filled(rect.center(), 32.0, accent_dim());
        nav_icon(ui.painter(), rect.shrink(21.0), NavIcon::Chat, accent());
        ui.add_space(8.0);
        ui.label(RichText::new(heading).size(16.0).strong().color(text()));
        ui.add_space(2.0);
        ui.label(RichText::new(body).size(12.0).color(muted()));
    });
    ui.add_space(20.0);
}

pub fn header(ui: &mut Ui, heading: &str, sub: &str) {
    ui.horizontal(|ui| {
        let (bar, _) = ui.allocate_exact_size(vec2(3.0, 38.0), Sense::hover());
        ui.painter()
            .rect_filled(bar, CornerRadius::same(2), accent());
        ui.add_space(7.0);
        ui.vertical(|ui| {
            ui.label(RichText::new(heading).size(22.0).strong().color(text()));
            ui.label(RichText::new(sub).size(12.0).color(muted()));
        });
    });
    ui.add_space(12.0);
}

pub fn divider(ui: &mut Ui) {
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 1.0), Sense::hover());
    ui.painter()
        .rect_filled(rect, CornerRadius::ZERO, stroke_soft());
}

pub fn status_dot(ui: &mut Ui, color: Color32) {
    let (rect, _) = ui.allocate_exact_size(vec2(8.0, 8.0), Sense::hover());
    ui.painter().circle_filled(rect.center(), 3.5, color);
}

pub fn paint_avatar(
    painter: &egui::Painter,
    center: egui::Pos2,
    radius: f32,
    label: &str,
    fill: Color32,
    fg: Color32,
) {
    painter.circle_filled(center, radius, fill);
    let initial = label
        .chars()
        .next()
        .map(|c| c.to_ascii_uppercase().to_string())
        .unwrap_or_else(|| "?".into());
    painter.text(
        center,
        Align2::CENTER_CENTER,
        initial,
        FontId::proportional(radius * 0.95),
        fg,
    );
}

pub fn avatar(ui: &mut Ui, label: &str, fill: Color32) -> Response {
    let (rect, response) = ui.allocate_exact_size(vec2(30.0, 30.0), Sense::hover());
    paint_avatar(
        ui.painter(),
        rect.center(),
        15.0,
        label,
        fill,
        avatar_text(),
    );
    response
}

pub fn avatar_text() -> Color32 {
    palette().avatar_text
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

/// RFC 3339 to a compact stamp: `2026-10-29T14:03:12Z` becomes
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
        let body = rect.shrink2(vec2(7.0, 2.0));
        if selected {
            painter.rect_filled(body, CornerRadius::same(12), panel_alt());
            painter.circle_filled(pos2(rect.center().x, rect.top() + 20.0), 15.0, accent_dim());
            painter.rect_filled(
                Rect::from_min_size(
                    pos2(rect.left() + 2.0, rect.top() + 10.0),
                    vec2(3.0, rect.height() - 20.0),
                ),
                CornerRadius::same(2),
                accent(),
            );
        } else if response.hovered() {
            painter.rect_filled(body, CornerRadius::same(12), panel_alt());
        }
        let color = if selected {
            accent()
        } else if response.hovered() {
            text()
        } else {
            self::icon()
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
            painter.rect_filled(front, CornerRadius::same(3), panel_alt());
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
                painter.circle_filled(pos2(c.x - s * 0.28, y), s * 0.05, color);
                painter.line_segment(
                    [
                        pos2(c.x - s * 0.16, y),
                        pos2(c.x + s * 0.30 - index as f32 * s * 0.04, y),
                    ],
                    stroke,
                );
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
    fn themes_switch_the_palette() {
        assert_eq!(ThemeMode::from_stored(Some("light")), ThemeMode::Light);
        assert_eq!(ThemeMode::from_stored(Some("dark")), ThemeMode::Dark);
        assert_eq!(ThemeMode::from_stored(Some("nope")), ThemeMode::Dark);
        let ctx = egui::Context::default();
        apply_theme(&ctx, ThemeMode::Light);
        assert_ne!(bg(), Palette::DARK.bg);
        assert_eq!(bg(), Palette::LIGHT.bg);
        apply_theme(&ctx, ThemeMode::Dark);
        assert_eq!(bg(), Palette::DARK.bg);
    }

    #[test]
    fn every_icon_paints_in_both_themes() {
        let ctx = egui::Context::default();
        for mode in [ThemeMode::Dark, ThemeMode::Light] {
            apply_theme(&ctx, mode);
            let input = egui::RawInput {
                screen_rect: Some(Rect::from_min_size(egui::Pos2::ZERO, vec2(360.0, 360.0))),
                ..Default::default()
            };
            let output = ctx.run(input, |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    paint_backdrop(ui);
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
                    chip(ui, "Connected", accent(), ok_bg());
                    card(ui, |ui| {
                        ui.label(title("Card"));
                        empty_state(ui, "Empty", "Nothing here");
                    });
                });
            });
            assert!(!output.shapes.is_empty());
        }
    }
}
