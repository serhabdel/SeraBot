//! PPTX export for bounded decks.
//!
//! Export only. `deckcraft_pptx::import` is never called outside `#[cfg(test)]`:
//! a deck is written from a validated [`DeckSpec`] and never read back in
//! production.
//!
//! The package holds text, autoshapes, and theme colours derived from
//! [`Brand`]. It has no images, media parts, hyperlinks, embedded workbooks,
//! attachments, charts, transitions, or animations, and no font bytes: runs
//! name the Helvetica family, the only family [`Brand::validate`] accepts.
//!
//! Every string written here has already passed [`crate::deck::validate`],
//! which runs first and is the same gate the PDF renderer uses.

use std::sync::Arc;

use deckcraft_model::defaults;
use deckcraft_model::style::{ColorRef, Fill, Line};
use deckcraft_model::text::{
    Align, Anchor, AutoFit, BodyProps, Bullet, ParaProps, Paragraph, Run, RunProps, Spacing,
    TextBody,
};
use deckcraft_model::theme::{FontScheme, FontSet, Theme};
use deckcraft_model::{
    Background, ColorScheme, Geom, LayoutType, Rgba, SchemeSlot, Shape, ShapeId, Size, Slide, Xfrm,
};

use crate::deck::{Brand, DeckCard, DeckPage, DeckSpec, paginate, parse_hex_color, validate};
use crate::domain::Language;
use crate::error::{Result, SeraError};
use crate::render::{part_label, section_label, slugify};
use crate::util::sha256_hex;

const SLIDE_WIDTH_PT: f64 = 960.0;
const SLIDE_HEIGHT_PT: f64 = 540.0;
const MARGIN_PT: f64 = 56.0;

const CARD_WIDTH_PT: f64 = 412.0;
const CARD_HEIGHT_PT: f64 = 150.0;
const CARD_COLUMN_GAP_PT: f64 = 24.0;
const CARD_ROW_GAP_PT: f64 = 20.0;
const CARD_TOP_PT: f64 = 136.0;

/// `Brand::validate` only accepts this family, and no font bytes are ever
/// written: the run names the family and the reader supplies the face.
const FONT_FAMILY: &str = "Helvetica";

/// Pinned document timestamps, so the same spec always produces the same bytes.
const EPOCH: &str = "1970-01-01T00:00:00Z";

#[derive(Debug, Clone)]
pub struct RenderedPptx {
    pub bytes: Vec<u8>,
    pub text: String,
    pub slides: usize,
    pub sha256: String,
    pub filename: String,
}

pub fn render_deck_pptx(spec: &DeckSpec, brand: &Brand, max_pages: usize) -> Result<RenderedPptx> {
    validate(spec, brand, max_pages)?;

    let palette = Palette::from_brand(brand)?;
    let footer = spec.footer.as_deref().or(brand.footer.as_deref());
    let plans = paginate(spec);
    let total_pages = plans.len();

    let mut presentation = defaults::blank_presentation(
        Size::new(SLIDE_WIDTH_PT, SLIDE_HEIGHT_PT),
        palette.theme(brand),
        false,
    );
    presentation.props.title = spec.title.clone();
    presentation.props.subject = spec.subtitle.clone().unwrap_or_default();
    presentation.props.author = spec.author.clone().unwrap_or_else(|| brand.name.clone());
    presentation.props.keywords = format!("{},deck,serabot", spec.language.code());
    presentation.props.last_modified_by = "SeraBot".into();
    presentation.props.created = EPOCH.into();
    presentation.props.modified = EPOCH.into();

    let layout = defaults::layout_of_kind(&presentation, LayoutType::Blank)
        .or_else(|| {
            presentation
                .masters
                .first()
                .and_then(|master| master.layouts.first().map(|layout| layout.id))
        })
        .ok_or_else(|| SeraError::Internal("deckcraft presentation has no layout".into()))?;

    for (index, plan) in plans.iter().enumerate() {
        let mut slide = defaults::new_slide(&mut presentation, layout)
            .ok_or_else(|| SeraError::Internal("deckcraft could not add a slide".into()))?;
        slide.name = format!("Slide {}", index + 1);
        // Every page is drawn from scratch on the blank layout, so inherited
        // master placeholder prompts must not leak into the package.
        slide.show_master_shapes = false;
        let page = Page {
            number: index + 1,
            total: total_pages,
            footer,
        };

        match plan {
            DeckPage::Cover => draw_cover(&mut slide, spec, brand, &palette, page),
            DeckPage::Part { title } => {
                draw_part(&mut slide, spec.language, title, brand, &palette, page);
            }
            DeckPage::Section { part, title } => {
                draw_section(
                    &mut slide,
                    spec.language,
                    *part,
                    title,
                    brand,
                    &palette,
                    page,
                );
            }
            DeckPage::Cards { part, title, cards } => {
                draw_cards(
                    &mut slide,
                    spec.language,
                    *part,
                    title,
                    cards,
                    brand,
                    &palette,
                    page,
                );
            }
            DeckPage::Bullets {
                part,
                title,
                bullets,
            } => {
                draw_bullets(&mut slide, spec.language, *part, title, bullets, page);
            }
        }

        presentation.slides.push(Arc::new(slide));
    }

    let bytes = deckcraft_pptx::export(&presentation)
        .map_err(|error| SeraError::Internal(format!("cannot serialize deck as pptx: {error}")))?;
    if !deckcraft_pptx::sniff(&bytes) {
        return Err(SeraError::Internal(
            "exported deck is not an OOXML package".into(),
        ));
    }

    // The text the writer just emitted, for the tool reply. The round-trip test
    // below is what proves it survives a reader.
    let mut text = String::new();
    for slide in &presentation.slides {
        deckcraft_model::walk(&slide.shapes, &mut |shape, _| {
            if let Some(body) = &shape.text {
                text.push_str(&body.text());
                text.push('\n');
            }
        });
    }

    Ok(RenderedPptx {
        sha256: sha256_hex(&bytes),
        slides: presentation.slides.len(),
        filename: format!("{}-{}.pptx", slugify(&spec.title), spec.language.code()),
        text,
        bytes,
    })
}

/// The brand palette as theme slots.
///
/// Text, fills, and rules all reference slots, so the deck stays recolourable
/// and no colour is invented outside the configured brand.
struct Palette {
    background: Rgba,
    foreground: Rgba,
    accent: Rgba,
    muted: Rgba,
    /// The page colour nudged towards the text colour: a card surface that is
    /// visible on the page without introducing a new hue.
    surface: Rgba,
}

impl Palette {
    fn from_brand(brand: &Brand) -> Result<Self> {
        let background = parse_color("brand.palette.background", &brand.palette.background)?;
        let foreground = parse_color("brand.palette.foreground", &brand.palette.foreground)?;
        let accent = parse_color("brand.palette.accent", &brand.palette.accent)?;
        let muted = parse_color("brand.palette.muted", &brand.palette.muted)?;
        Ok(Self {
            background,
            foreground,
            accent,
            muted,
            surface: background.lerp(foreground, 0.06),
        })
    }

    fn theme(&self, brand: &Brand) -> Theme {
        let fonts = FontSet {
            latin: FONT_FAMILY.into(),
            ..Default::default()
        };
        Theme {
            name: brand.name.clone(),
            colors: ColorScheme {
                name: brand.name.clone(),
                colors: [
                    self.foreground, // dk1 — body text
                    self.background, // lt1 — page
                    self.muted,      // dk2 — secondary text
                    self.surface,    // lt2 — card surface
                    self.accent,     // accent1
                    self.muted,      // accent2
                    self.accent,     // accent3
                    self.accent,     // accent4
                    self.accent,     // accent5
                    self.accent,     // accent6
                    self.accent,     // hlink — unused, no hyperlink is ever written
                    self.muted,      // folHlink
                ],
            },
            fonts: FontScheme {
                name: brand.name.clone(),
                major: fonts.clone(),
                minor: fonts,
            },
            ..Default::default()
        }
    }

    fn slot(&self, slot: SchemeSlot) -> Rgba {
        match slot {
            SchemeSlot::Bg1 | SchemeSlot::Lt1 => self.background,
            SchemeSlot::Tx1 | SchemeSlot::Dk1 => self.foreground,
            SchemeSlot::Dk2 | SchemeSlot::Tx2 => self.muted,
            SchemeSlot::Lt2 | SchemeSlot::Bg2 => self.surface,
            _ => self.accent,
        }
    }

    /// The slot with the better contrast against `background`.
    fn on(&self, background: Rgba) -> SchemeSlot {
        if background.contrast(self.background) >= background.contrast(self.foreground) {
            SchemeSlot::Bg1
        } else {
            SchemeSlot::Tx1
        }
    }
}

fn parse_color(label: &str, value: &str) -> Result<Rgba> {
    let (red, green, blue) = parse_hex_color(value)
        .ok_or_else(|| SeraError::Validation(format!("{label} must be a #RRGGBB colour")))?;
    Ok(Rgba::rgb(red, green, blue))
}

#[derive(Clone, Copy)]
struct TextStyle {
    size: f64,
    bold: bool,
    color: SchemeSlot,
    align: Align,
    anchor: Anchor,
}

impl TextStyle {
    fn new(size: f64, bold: bool, color: SchemeSlot) -> Self {
        Self {
            size,
            bold,
            color,
            align: Align::Left,
            anchor: Anchor::Top,
        }
    }

    fn align(mut self, align: Align) -> Self {
        self.align = align;
        self
    }

    fn anchor(mut self, anchor: Anchor) -> Self {
        self.anchor = anchor;
        self
    }
}

#[derive(Clone, Copy)]
struct Page<'a> {
    number: usize,
    total: usize,
    footer: Option<&'a str>,
}

fn solid(slot: SchemeSlot) -> Fill {
    Fill::solid(ColorRef::scheme(slot))
}

fn background(slot: SchemeSlot) -> Background {
    Background::Fill { fill: solid(slot) }
}

fn run_props(style: TextStyle) -> RunProps {
    RunProps {
        font: Some(FONT_FAMILY.into()),
        size: Some(style.size),
        bold: Some(style.bold),
        fill: Some(solid(style.color)),
        kern: Some(12.0),
        ..Default::default()
    }
}

fn paragraph(text: &str, style: TextStyle) -> Paragraph {
    Paragraph {
        runs: vec![Run::with(text, run_props(style))],
        props: ParaProps {
            align: Some(style.align),
            line_spacing: Some(Spacing::Pct(0.95)),
            ..Default::default()
        },
        ..Default::default()
    }
}

fn bullet_paragraph(text: &str, style: TextStyle) -> Paragraph {
    Paragraph {
        runs: vec![Run::with(text, run_props(style))],
        props: ParaProps {
            align: Some(style.align),
            margin_left: Some(20.0),
            indent: Some(-20.0),
            line_spacing: Some(Spacing::Pct(0.95)),
            space_before: Some(Spacing::Pts(8.0)),
            bullet: Some(Bullet::Char { char: "•".into() }),
            bullet_font: Some(FONT_FAMILY.into()),
            bullet_color: Some(ColorRef::scheme(SchemeSlot::Accent1)),
            ..Default::default()
        },
        ..Default::default()
    }
}

fn text_body(paragraphs: Vec<Paragraph>, style: TextStyle) -> TextBody {
    TextBody {
        body: BodyProps {
            inset_l: Some(0.0),
            inset_t: Some(0.0),
            inset_r: Some(0.0),
            inset_b: Some(0.0),
            anchor: Some(style.anchor),
            wrap: Some(true),
            autofit: Some(AutoFit::Shrink {
                font_scale: Some(1.0),
                line_reduction: 0.0,
            }),
            ..Default::default()
        },
        paragraphs,
        ..Default::default()
    }
}

fn single_body(text: &str, style: TextStyle) -> TextBody {
    text_body(vec![paragraph(text, style)], style)
}

/// A shape's box on the slide, in points.
#[derive(Clone, Copy)]
struct Frame {
    x: f64,
    y: f64,
    w: f64,
    h: f64,
}

impl Frame {
    fn new(x: f64, y: f64, w: f64, h: f64) -> Self {
        Self { x, y, w, h }
    }

    fn square(x: f64, y: f64, size: f64) -> Self {
        Self::new(x, y, size, size)
    }

    fn xfrm(self) -> Xfrm {
        Xfrm::new(self.x, self.y, self.w, self.h)
    }
}

/// Shapes for one slide, with ids unique inside that slide.
struct Canvas<'a> {
    slide: &'a mut Slide,
    next_id: u32,
}

impl<'a> Canvas<'a> {
    fn new(slide: &'a mut Slide) -> Self {
        Self { slide, next_id: 1 }
    }

    fn add(&mut self, mut shape: Shape) {
        self.next_id += 1;
        shape.id = ShapeId(self.next_id);
        self.slide.shapes.push(shape);
    }

    fn rect(&mut self, name: &str, frame: Frame, fill: Fill, line: Line) {
        self.add(Shape {
            name: name.into(),
            xfrm: Some(frame.xfrm()),
            geom: Geom::preset("rect"),
            fill: Some(fill),
            line: Some(line),
            ..Default::default()
        });
    }

    fn ellipse(&mut self, name: &str, frame: Frame, fill: Fill, text: Option<TextBody>) {
        self.add(Shape {
            name: name.into(),
            xfrm: Some(frame.xfrm()),
            geom: Geom::preset("ellipse"),
            fill: Some(fill),
            line: Some(Line::none()),
            text,
            ..Default::default()
        });
    }

    fn text(&mut self, name: &str, frame: Frame, body: TextBody) {
        self.add(Shape {
            name: name.into(),
            xfrm: Some(frame.xfrm()),
            geom: Geom::preset("rect"),
            fill: Some(Fill::None),
            line: Some(Line::none()),
            text: Some(body),
            text_box: true,
            ..Default::default()
        });
    }

    fn label(&mut self, name: &str, frame: Frame, text: &str, style: TextStyle) {
        self.text(name, frame, single_body(text, style));
    }

    /// A brand chip: a filled circle with centred initials.
    fn monogram(
        &mut self,
        name: &str,
        frame: Frame,
        text: &str,
        palette: &Palette,
        fill: SchemeSlot,
    ) {
        let ratio = match text.chars().count() {
            0 | 1 => 0.42,
            2 => 0.34,
            3 => 0.28,
            _ => 0.22,
        };
        let style = TextStyle::new(frame.w * ratio, true, palette.on(palette.slot(fill)))
            .align(Align::Center)
            .anchor(Anchor::Middle);
        self.ellipse(name, frame, solid(fill), Some(single_body(text, style)));
    }
}

fn draw_cover(
    slide: &mut Slide,
    spec: &DeckSpec,
    brand: &Brand,
    palette: &Palette,
    page: Page<'_>,
) {
    slide.background = Some(background(SchemeSlot::Bg1));
    let mut canvas = Canvas::new(slide);
    canvas.rect(
        "Accent bar",
        Frame::new(0.0, 0.0, 32.0, SLIDE_HEIGHT_PT),
        solid(SchemeSlot::Accent1),
        Line::none(),
    );
    canvas.monogram(
        "Brand monogram",
        Frame::square(82.0, 70.0, 60.0),
        &brand.monogram_text,
        palette,
        SchemeSlot::Accent1,
    );

    let brand_style = TextStyle::new(12.0, true, SchemeSlot::Tx1);
    canvas.label(
        "Brand name",
        Frame::new(112.0, 150.0, 700.0, 26.0),
        &brand.name,
        brand_style,
    );
    canvas.rect(
        "Brand rule",
        Frame::new(112.0, 186.0, 120.0, 2.0),
        solid(SchemeSlot::Accent1),
        Line::none(),
    );

    canvas.label(
        "Deck title",
        Frame::new(112.0, 208.0, 780.0, 150.0),
        &spec.title,
        TextStyle::new(40.0, true, SchemeSlot::Tx1),
    );

    if let Some(subtitle) = &spec.subtitle {
        canvas.label(
            "Deck subtitle",
            Frame::new(112.0, 366.0, 760.0, 64.0),
            subtitle,
            TextStyle::new(18.0, false, SchemeSlot::Dk2),
        );
    }

    let mut metadata = Vec::new();
    if let Some(date) = &spec.date_label {
        metadata.push(date.as_str());
    }
    if let Some(author) = &spec.author {
        metadata.push(author.as_str());
    }
    if !metadata.is_empty() {
        canvas.label(
            "Deck metadata",
            Frame::new(112.0, 452.0, 760.0, 24.0),
            &metadata.join("  •  "),
            TextStyle::new(11.0, false, SchemeSlot::Dk2),
        );
    }

    draw_footer(&mut canvas, page, SchemeSlot::Dk2);
}

fn draw_part(
    slide: &mut Slide,
    language: Language,
    title: &str,
    brand: &Brand,
    palette: &Palette,
    page: Page<'_>,
) {
    slide.background = Some(background(SchemeSlot::Accent1));
    let on = palette.on(palette.accent);
    let mut canvas = Canvas::new(slide);
    canvas.rect(
        "Top band",
        Frame::new(0.0, 0.0, SLIDE_WIDTH_PT, 56.0),
        solid(SchemeSlot::Bg1),
        Line::none(),
    );
    canvas.label(
        "Part label",
        Frame::new(MARGIN_PT, 196.0, 500.0, 24.0),
        part_label(language),
        TextStyle::new(12.0, true, on),
    );
    canvas.label(
        "Part title",
        Frame::new(MARGIN_PT, 228.0, 760.0, 160.0),
        title,
        TextStyle::new(44.0, true, on),
    );
    canvas.monogram(
        "Brand monogram",
        Frame::square(
            SLIDE_WIDTH_PT - MARGIN_PT - 60.0,
            SLIDE_HEIGHT_PT - 116.0,
            60.0,
        ),
        &brand.monogram_text,
        palette,
        SchemeSlot::Bg1,
    );
    draw_footer(&mut canvas, page, on);
}

#[allow(clippy::too_many_arguments)]
fn draw_section(
    slide: &mut Slide,
    language: Language,
    part: Option<&str>,
    title: &str,
    brand: &Brand,
    palette: &Palette,
    page: Page<'_>,
) {
    slide.background = Some(background(SchemeSlot::Bg1));
    let mut canvas = Canvas::new(slide);
    canvas.rect(
        "Accent bar",
        Frame::new(0.0, 0.0, 44.0, SLIDE_HEIGHT_PT),
        solid(SchemeSlot::Accent1),
        Line::none(),
    );
    canvas.rect(
        "Section rule",
        Frame::new(96.0, 120.0, SLIDE_WIDTH_PT - 96.0 - MARGIN_PT, 2.0),
        solid(SchemeSlot::Accent1),
        Line::none(),
    );
    canvas.label(
        "Section label",
        Frame::new(96.0, 140.0, 500.0, 22.0),
        section_label(language),
        TextStyle::new(11.0, true, SchemeSlot::Accent1),
    );
    canvas.label(
        "Section title",
        Frame::new(96.0, 168.0, 780.0, 120.0),
        title,
        TextStyle::new(34.0, true, SchemeSlot::Tx1),
    );

    let caption = match part {
        Some(part) => format!("{}  {}", part_label(language), part),
        None => brand.name.clone(),
    };
    canvas.label(
        "Section caption",
        Frame::new(96.0, 430.0, 700.0, 22.0),
        &caption,
        TextStyle::new(11.0, false, SchemeSlot::Dk2),
    );
    canvas.monogram(
        "Brand monogram",
        Frame::square(
            SLIDE_WIDTH_PT - MARGIN_PT - 60.0,
            SLIDE_HEIGHT_PT - 116.0,
            60.0,
        ),
        &brand.monogram_text,
        palette,
        SchemeSlot::Accent1,
    );
    draw_footer(&mut canvas, page, SchemeSlot::Dk2);
}

#[allow(clippy::too_many_arguments)]
fn draw_cards(
    slide: &mut Slide,
    language: Language,
    part: Option<&str>,
    title: &str,
    cards: &[DeckCard],
    brand: &Brand,
    palette: &Palette,
    page: Page<'_>,
) {
    slide.background = Some(background(SchemeSlot::Bg1));
    let mut canvas = Canvas::new(slide);
    draw_content_header(&mut canvas, language, part, title);

    for (index, card) in cards.iter().enumerate() {
        let column = index % 2;
        let row = index / 2;
        let x = MARGIN_PT + column as f64 * (CARD_WIDTH_PT + CARD_COLUMN_GAP_PT);
        let y = CARD_TOP_PT + row as f64 * (CARD_HEIGHT_PT + CARD_ROW_GAP_PT);
        draw_card(
            &mut canvas,
            card,
            &brand.monogram_text,
            palette,
            Frame::new(x, y, CARD_WIDTH_PT, CARD_HEIGHT_PT),
        );
    }

    draw_footer(&mut canvas, page, SchemeSlot::Dk2);
}

fn draw_card(
    canvas: &mut Canvas<'_>,
    card: &DeckCard,
    brand_monogram: &str,
    palette: &Palette,
    frame: Frame,
) {
    canvas.rect(
        "Card",
        frame,
        solid(SchemeSlot::Lt2),
        Line::solid(ColorRef::scheme(SchemeSlot::Dk2), 0.75),
    );
    canvas.rect(
        "Card accent",
        Frame::new(frame.x, frame.y, frame.w, 3.0),
        solid(SchemeSlot::Accent1),
        Line::none(),
    );

    let monogram = card.monogram.as_deref().unwrap_or(brand_monogram);
    canvas.monogram(
        "Card monogram",
        Frame::square(frame.x + 16.0, frame.y + 16.0, 24.0),
        monogram,
        palette,
        SchemeSlot::Accent1,
    );

    canvas.label(
        "Card heading",
        Frame::new(frame.x + 48.0, frame.y + 14.0, frame.w - 64.0, 40.0),
        &card.heading,
        TextStyle::new(13.0, true, SchemeSlot::Tx1),
    );

    let body_style = TextStyle::new(10.0, false, SchemeSlot::Tx1);
    let mut paragraphs = Vec::new();
    if !card.body.is_empty() {
        paragraphs.push(paragraph(&card.body, body_style));
    }
    for bullet in &card.bullets {
        paragraphs.push(bullet_paragraph(bullet, body_style));
    }
    if !paragraphs.is_empty() {
        canvas.text(
            "Card text",
            Frame::new(
                frame.x + 16.0,
                frame.y + 58.0,
                frame.w - 32.0,
                frame.h - 70.0,
            ),
            text_body(paragraphs, body_style),
        );
    }
}

fn draw_bullets(
    slide: &mut Slide,
    language: Language,
    part: Option<&str>,
    title: &str,
    bullets: &[String],
    page: Page<'_>,
) {
    slide.background = Some(background(SchemeSlot::Bg1));
    let mut canvas = Canvas::new(slide);
    draw_content_header(&mut canvas, language, part, title);

    let style = TextStyle::new(16.0, false, SchemeSlot::Tx1);
    let paragraphs = bullets
        .iter()
        .map(|bullet| bullet_paragraph(bullet, style))
        .collect();
    canvas.text(
        "Bullets",
        Frame::new(
            MARGIN_PT,
            CARD_TOP_PT,
            SLIDE_WIDTH_PT - 2.0 * MARGIN_PT,
            340.0,
        ),
        text_body(paragraphs, style),
    );

    draw_footer(&mut canvas, page, SchemeSlot::Dk2);
}

fn draw_content_header(
    canvas: &mut Canvas<'_>,
    language: Language,
    part: Option<&str>,
    title: &str,
) {
    let width = SLIDE_WIDTH_PT - 2.0 * MARGIN_PT;
    canvas.rect(
        "Header rule",
        Frame::new(MARGIN_PT, 36.0, width, 2.0),
        solid(SchemeSlot::Accent1),
        Line::none(),
    );

    let label = match part {
        Some(part) => format!("{}  /  {}", part_label(language), part),
        None => section_label(language).to_string(),
    };
    canvas.label(
        "Content label",
        Frame::new(MARGIN_PT, 46.0, 700.0, 18.0),
        &label,
        TextStyle::new(10.0, true, SchemeSlot::Accent1),
    );
    canvas.label(
        "Section title",
        Frame::new(MARGIN_PT, 66.0, width, 44.0),
        title,
        TextStyle::new(22.0, true, SchemeSlot::Tx1),
    );
    canvas.rect(
        "Header divider",
        Frame::new(MARGIN_PT, 116.0, width, 1.0),
        solid(SchemeSlot::Lt2),
        Line::none(),
    );
}

fn draw_footer(canvas: &mut Canvas<'_>, page: Page<'_>, color: SchemeSlot) {
    if let Some(footer) = page.footer {
        canvas.label(
            "Footer",
            Frame::new(MARGIN_PT, 505.0, 620.0, 18.0),
            footer,
            TextStyle::new(9.0, false, color),
        );
    }
    canvas.label(
        "Page number",
        Frame::new(SLIDE_WIDTH_PT - MARGIN_PT - 140.0, 505.0, 140.0, 18.0),
        &format!("{} / {}", page.number, page.total),
        TextStyle::new(9.0, false, color).align(Align::Right),
    );
}

#[cfg(test)]
mod tests {
    use std::io::Read;

    use super::*;
    use crate::deck::{BrandPalette, DeckSection};

    fn brand() -> Brand {
        Brand {
            name: "Synthetic Example Brand".into(),
            palette: BrandPalette {
                background: "#F5F1E8".into(),
                foreground: "#1F2933".into(),
                accent: "#2F6F6A".into(),
                muted: "#6B7280".into(),
            },
            monogram_text: "SB".into(),
            footer: Some("Alpha placeholder branding".into()),
            font_family: Some("helvetica".into()),
        }
    }

    fn spec() -> DeckSpec {
        DeckSpec {
            title: "Étude de marché".into(),
            subtitle: Some("Synthèse bilingue pour décideurs".into()),
            language: Language::Fr,
            date_label: Some("Octobre 2026".into()),
            author: Some("SeraBot".into()),
            sections: vec![DeckSection {
                part_title: Some("Partenariats".into()),
                title: "Opportunités à l'étranger".into(),
                cards: vec![
                    DeckCard {
                        heading: "Offre".into(),
                        body: "Café, œuvres, leçon et croissance à Paris.".into(),
                        bullets: vec!["Réduction des coûts".into()],
                        monogram: Some("OF".into()),
                    },
                    DeckCard {
                        heading: "Marché".into(),
                        body: "Une demande croissante en Europe.".into(),
                        bullets: Vec::new(),
                        monogram: None,
                    },
                ],
                bullets: vec![
                    "Priorité aux marchés francophones".into(),
                    "Déploiement progressif à Lyon et à Montréal".into(),
                ],
            }],
            footer: None,
        }
    }

    fn parts(bytes: &[u8]) -> Vec<(String, Vec<u8>)> {
        let mut archive = zip::ZipArchive::new(std::io::Cursor::new(bytes)).unwrap();
        let mut out = Vec::new();
        for index in 0..archive.len() {
            let mut file = archive.by_index(index).unwrap();
            let name = file.name().to_owned();
            let mut data = Vec::new();
            file.read_to_end(&mut data).unwrap();
            out.push((name, data));
        }
        out
    }

    #[test]
    fn exports_a_bounded_deck() {
        let rendered = render_deck_pptx(&spec(), &brand(), 30).unwrap();
        assert!(rendered.bytes.starts_with(b"PK"));
        assert!(deckcraft_pptx::sniff(&rendered.bytes));
        // Cover, part divider, section divider, one card page, one bullet page.
        assert_eq!(rendered.slides, 5);
        assert_eq!(rendered.sha256, sha256_hex(&rendered.bytes));
        assert_eq!(rendered.filename, "etude-de-marche-fr.pptx");

        for needle in [
            "Étude de marché",
            "Partenariats",
            "PARTIE",
            "Opportunités à l'étranger",
            "Café",
            "œuvres",
            "leçon",
            "Réduction",
            "Alpha placeholder branding",
            "1 / 5",
        ] {
            assert!(
                rendered.text.contains(needle),
                "missing {needle:?} in deck text: {}",
                rendered.text
            );
        }
    }

    #[test]
    fn package_holds_only_text_shapes_and_theme() {
        let rendered = render_deck_pptx(&spec(), &brand(), 30).unwrap();
        let parts = parts(&rendered.bytes);

        for (name, _) in &parts {
            for forbidden in [
                "ppt/media/",
                "ppt/embeddings/",
                "ppt/charts/",
                "ppt/notesSlides/",
                "ppt/comments/",
                "ppt/fonts/",
            ] {
                assert!(!name.contains(forbidden), "unexpected part {name}");
            }
        }

        for (name, data) in &parts {
            if !(name.ends_with(".xml") || name.ends_with(".rels")) {
                continue;
            }
            let xml = String::from_utf8_lossy(data);
            for forbidden in [
                "<a:blip",
                "hlinkClick",
                "hlinkHover",
                "TargetMode=\"External\"",
                "r:embed",
                "r:link",
                "oleObject",
                "audioFile",
                "videoFile",
                "<p:timing",
                "<p:transition",
                "image/",
            ] {
                assert!(
                    !xml.contains(forbidden),
                    "part {name} contains {forbidden:?}"
                );
            }
            // Slides carry text boxes and autoshapes only: no picture frame, no
            // table, no chart frame. (`ppt/tableStyles.xml` is the package's
            // default table-style list, not a table on a slide.)
            if name.starts_with("ppt/slides/") {
                for forbidden in ["<p:pic", "<a:tbl", "<p:graphicFrame"] {
                    assert!(
                        !xml.contains(forbidden),
                        "slide part {name} contains {forbidden:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn text_survives_a_round_trip() {
        let rendered = render_deck_pptx(&spec(), &brand(), 30).unwrap();
        let imported = deckcraft_pptx::import(&rendered.bytes).unwrap();
        assert!(imported.validate().is_empty(), "{:?}", imported.validate());
        assert_eq!(imported.slides.len(), rendered.slides);

        let text = imported
            .slides
            .iter()
            .flat_map(|slide| slide.shapes.iter())
            .filter_map(|shape| shape.text.as_ref())
            .map(|body| body.text())
            .collect::<Vec<_>>()
            .join("\n");

        for needle in [
            "Étude de marché",
            "Synthèse bilingue pour décideurs",
            "Octobre 2026",
            "Partenariats",
            "Opportunités à l'étranger",
            "Café, œuvres, leçon et croissance à Paris.",
            "Réduction des coûts",
            "Priorité aux marchés francophones",
            "Alpha placeholder branding",
            "5 / 5",
        ] {
            assert!(
                text.contains(needle),
                "missing {needle:?} after re-import: {text}"
            );
        }
    }

    #[test]
    fn export_is_deterministic() {
        let first = render_deck_pptx(&spec(), &brand(), 30).unwrap();
        let second = render_deck_pptx(&spec(), &brand(), 30).unwrap();
        assert_eq!(first.text, second.text);
        assert_eq!(first.slides, second.slides);
        assert_eq!(first.filename, second.filename);
        // Timestamps are pinned, so the artifact hash is stable too.
        assert_eq!(first.sha256, second.sha256);
        assert_eq!(first.bytes, second.bytes);
    }

    #[test]
    fn rejects_what_the_pdf_renderer_rejects() {
        assert!(render_deck_pptx(&spec(), &brand(), 3).is_err());

        let mut empty = spec();
        empty.sections.clear();
        assert!(render_deck_pptx(&empty, &brand(), 30).is_err());

        let mut invalid_brand = brand();
        invalid_brand.palette.accent = "teal".into();
        assert!(render_deck_pptx(&spec(), &invalid_brand, 30).is_err());

        let mut url = spec();
        url.title = "Voir https://example.invalid".into();
        assert!(render_deck_pptx(&url, &brand(), 30).is_err());
    }

    #[test]
    fn sanitizes_the_filename() {
        let mut spec = spec();
        spec.title = "../../etc/passwd".into();
        let rendered = render_deck_pptx(&spec, &brand(), 30).unwrap();
        assert_eq!(rendered.filename, "etc-passwd-fr.pptx");
        assert!(!rendered.filename.contains('/'));
        assert!(!rendered.filename.contains('\\'));
        assert!(!rendered.filename.contains(".."));
    }
}
