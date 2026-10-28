//! Deterministic PDF rendering primitives.
//!
//! Layout is fixed and content-driven: the same deck always produces the same
//! page order, line breaks, and extracted text. The renderer only emits
//! vector shapes and WinAnsi text, so there are no images, links, scripts,
//! attachments, or embedded files.

use std::f32::consts::TAU;

use printpdf::path::{PaintMode, WindingOrder};
use printpdf::{
    BuiltinFont, Color, IndirectFontRef, Mm, OffsetDateTime, PdfDocument, PdfLayerReference, Point,
    Polygon, Rect, Rgb,
};
use unicode_normalization::{UnicodeNormalization, char::is_combining_mark};

use crate::deck::{Brand, DeckCard, DeckPage, DeckSpec, paginate, parse_hex_color, validate};
use crate::domain::Language;
use crate::error::{Result, SeraError};
use crate::util::sha256_hex;

const PAGE_WIDTH_MM: f32 = 297.0;
const PAGE_HEIGHT_MM: f32 = 210.0;
const MARGIN_MM: f32 = 18.0;
const PT_PER_MM: f32 = 2.834_646;

const CARD_WIDTH_MM: f32 = 126.5;
const CARD_HEIGHT_MM: f32 = 66.0;
const CARD_COLUMN_GAP_MM: f32 = 8.0;
const CARD_ROW_GAP_MM: f32 = 7.0;
const CARD_TOP_MM: f32 = 168.0;
const CARD_TEXT_TOP_MM: f32 = 31.0;
const CARD_TEXT_LINES: usize = 7;

#[derive(Debug, Clone)]
pub struct RenderedDeck {
    pub pdf: Vec<u8>,
    pub text: String,
    pub pages: usize,
    pub sha256: String,
    pub filename: String,
}

pub fn render_deck(spec: &DeckSpec, brand: &Brand, max_pages: usize) -> Result<RenderedDeck> {
    validate(spec, brand, max_pages)?;

    let palette = Palette::from_brand(brand)?;
    let footer = spec.footer.as_deref().or(brand.footer.as_deref());
    let plans = paginate(spec);
    let total_pages = plans.len();

    let (doc, first_page, first_layer) = PdfDocument::new(
        spec.title.clone(),
        Mm(PAGE_WIDTH_MM),
        Mm(PAGE_HEIGHT_MM),
        "deck",
    );
    let author = spec
        .author
        .as_deref()
        .unwrap_or(brand.name.as_str())
        .to_string();
    let doc = doc
        .with_author(author)
        .with_creator("SeraBot deck renderer (alpha)".to_string())
        .with_producer("SeraBot".to_string())
        .with_subject(spec.subtitle.clone().unwrap_or_default())
        .with_keywords(vec![spec.language.code(), "deck", "serabot"])
        // printpdf defaults these timestamps to the wall clock. Pinning them to
        // the epoch keeps the document info stable across runs.
        .with_creation_date(OffsetDateTime::UNIX_EPOCH)
        .with_mod_date(OffsetDateTime::UNIX_EPOCH)
        .with_metadata_date(OffsetDateTime::UNIX_EPOCH);

    let fonts = Fonts {
        regular: doc
            .add_builtin_font(BuiltinFont::Helvetica)
            .map_err(|error| pdf_error("cannot load Helvetica", error))?,
        bold: doc
            .add_builtin_font(BuiltinFont::HelveticaBold)
            .map_err(|error| pdf_error("cannot load Helvetica-Bold", error))?,
    };

    for (index, plan) in plans.iter().enumerate() {
        let (page_index, layer_index) = if index == 0 {
            (first_page, first_layer)
        } else {
            doc.add_page(Mm(PAGE_WIDTH_MM), Mm(PAGE_HEIGHT_MM), "deck")
        };
        let canvas = Canvas {
            layer: doc.get_page(page_index).get_layer(layer_index),
            fonts: &fonts,
            palette: &palette,
        };
        let page_number = index + 1;

        match plan {
            DeckPage::Cover => {
                draw_cover(&canvas, spec, brand, footer, total_pages)?;
            }
            DeckPage::Part { title } => {
                draw_part(
                    &canvas,
                    spec.language,
                    title,
                    brand,
                    footer,
                    page_number,
                    total_pages,
                )?;
            }
            DeckPage::Section { part, title } => {
                draw_section(
                    &canvas,
                    spec.language,
                    *part,
                    title,
                    brand,
                    footer,
                    page_number,
                    total_pages,
                )?;
            }
            DeckPage::Cards { part, title, cards } => {
                draw_cards(
                    &canvas,
                    spec,
                    brand,
                    *part,
                    title,
                    cards,
                    footer,
                    page_number,
                    total_pages,
                )?;
            }
            DeckPage::Bullets {
                part,
                title,
                bullets,
            } => {
                draw_bullets(
                    &canvas,
                    spec.language,
                    *part,
                    title,
                    bullets,
                    footer,
                    page_number,
                    total_pages,
                )?;
            }
        }
    }

    let pdf = doc
        .save_to_bytes()
        .map_err(|error| pdf_error("cannot serialize deck", error))?;
    let (text, pages) = inspect_pdf(&pdf)?;
    if pages != total_pages {
        return Err(SeraError::Internal(format!(
            "rendered {pages} pages but planned {total_pages}"
        )));
    }

    Ok(RenderedDeck {
        sha256: sha256_hex(&pdf),
        text,
        pages,
        filename: deck_filename(spec),
        pdf,
    })
}

struct Fonts {
    regular: IndirectFontRef,
    bold: IndirectFontRef,
}

#[derive(Clone)]
struct Palette {
    background: Color,
    foreground: Color,
    accent: Color,
    muted: Color,
    background_rgb: (u8, u8, u8),
    foreground_rgb: (u8, u8, u8),
    accent_rgb: (u8, u8, u8),
}

impl Palette {
    fn from_brand(brand: &Brand) -> Result<Self> {
        let background = color_from_brand("brand.palette.background", &brand.palette.background)?;
        let foreground = color_from_brand("brand.palette.foreground", &brand.palette.foreground)?;
        let accent = color_from_brand("brand.palette.accent", &brand.palette.accent)?;
        let muted = color_from_brand("brand.palette.muted", &brand.palette.muted)?;
        Ok(Self {
            background: rgb_color(background),
            foreground: rgb_color(foreground),
            accent: rgb_color(accent),
            muted: rgb_color(muted),
            background_rgb: background,
            foreground_rgb: foreground,
            accent_rgb: accent,
        })
    }

    fn contrast_on(&self, background: (u8, u8, u8)) -> Color {
        if luminance(background) < 0.5 {
            if luminance(self.background_rgb) > 0.55 {
                self.background.clone()
            } else {
                rgb_color((255, 255, 255))
            }
        } else if luminance(self.foreground_rgb) < 0.45 {
            self.foreground.clone()
        } else {
            rgb_color((0, 0, 0))
        }
    }

    fn on_accent(&self) -> Color {
        self.contrast_on(self.accent_rgb)
    }
}

fn color_from_brand(label: &str, value: &str) -> Result<(u8, u8, u8)> {
    parse_hex_color(value)
        .ok_or_else(|| SeraError::Validation(format!("{label} must be a #RRGGBB colour")))
}

fn rgb_color((red, green, blue): (u8, u8, u8)) -> Color {
    Color::Rgb(Rgb::new(
        red as f32 / 255.0,
        green as f32 / 255.0,
        blue as f32 / 255.0,
        None,
    ))
}

fn luminance((red, green, blue): (u8, u8, u8)) -> f32 {
    fn channel(value: u8) -> f32 {
        let value = value as f32 / 255.0;
        if value <= 0.04045 {
            value / 12.92
        } else {
            ((value + 0.055) / 1.055).powf(2.4)
        }
    }
    0.2126 * channel(red) + 0.7152 * channel(green) + 0.0722 * channel(blue)
}

struct Canvas<'a> {
    layer: PdfLayerReference,
    fonts: &'a Fonts,
    palette: &'a Palette,
}

impl Canvas<'_> {
    fn fill_rect(&self, x: f32, y: f32, width: f32, height: f32, color: Color) {
        self.layer.set_fill_color(color);
        self.layer.add_rect(
            Rect::new(Mm(x), Mm(y), Mm(x + width), Mm(y + height)).with_mode(PaintMode::Fill),
        );
    }

    fn stroke_rect(&self, x: f32, y: f32, width: f32, height: f32, color: Color, width_pt: f32) {
        self.layer.set_outline_color(color);
        self.layer.set_outline_thickness(width_pt);
        self.layer.add_rect(
            Rect::new(Mm(x), Mm(y), Mm(x + width), Mm(y + height)).with_mode(PaintMode::Stroke),
        );
    }

    fn fill_circle(&self, cx: f32, cy: f32, radius: f32, color: Color) {
        let steps = 24;
        let mut points = Vec::with_capacity(steps);
        for step in 0..steps {
            let angle = TAU * step as f32 / steps as f32;
            points.push((
                Point::new(Mm(cx + radius * angle.cos()), Mm(cy + radius * angle.sin())),
                false,
            ));
        }
        self.layer.set_fill_color(color);
        self.layer.add_polygon(Polygon {
            rings: vec![points],
            mode: PaintMode::Fill,
            winding_order: WindingOrder::NonZero,
        });
    }

    fn text(&self, value: &str, size: f32, x: f32, y: f32, bold: bool, color: Color) {
        if value.is_empty() {
            return;
        }
        self.layer.set_fill_color(color);
        let font = if bold {
            &self.fonts.bold
        } else {
            &self.fonts.regular
        };
        self.layer.use_text(value, size, Mm(x), Mm(y), font);
    }
}

fn draw_cover(
    canvas: &Canvas<'_>,
    spec: &DeckSpec,
    brand: &Brand,
    footer: Option<&str>,
    total_pages: usize,
) -> Result<()> {
    let palette = canvas.palette;
    canvas.fill_rect(
        0.0,
        0.0,
        PAGE_WIDTH_MM,
        PAGE_HEIGHT_MM,
        palette.background.clone(),
    );
    canvas.fill_rect(0.0, 0.0, 10.0, PAGE_HEIGHT_MM, palette.accent.clone());

    draw_monogram(
        canvas,
        31.0,
        181.0,
        10.0,
        brand.monogram_text.as_str(),
        palette.accent.clone(),
        palette.on_accent(),
    );

    let brand_lines = wrap_text(brand.name.as_str(), 10.5, 238.0, 2, true, "brand name")?;
    let mut y = 160.0;
    for line in &brand_lines {
        canvas.text(line, 10.5, 22.0, y, true, palette.foreground.clone());
        y -= 5.4;
    }
    canvas.fill_rect(22.0, 150.0, 40.0, 1.0, palette.accent.clone());

    let title_lines = wrap_text(&spec.title, 30.0, 238.0, 4, true, "deck title")?;
    let title_start = 122.0;
    for (index, line) in title_lines.iter().enumerate() {
        canvas.text(
            line,
            30.0,
            22.0,
            title_start - index as f32 * 13.5,
            true,
            palette.foreground.clone(),
        );
    }

    let next_y = title_start - title_lines.len() as f32 * 13.5 + 1.0;
    if let Some(subtitle) = &spec.subtitle {
        let subtitle_lines = wrap_text(subtitle, 15.0, 230.0, 3, false, "subtitle")?;
        for (index, line) in subtitle_lines.iter().enumerate() {
            canvas.text(
                line,
                15.0,
                22.0,
                next_y - index as f32 * 7.2,
                false,
                palette.muted.clone(),
            );
        }
    }

    let mut metadata = Vec::new();
    if let Some(date) = &spec.date_label {
        metadata.push(date.as_str());
    }
    if let Some(author) = &spec.author {
        metadata.push(author.as_str());
    }
    if !metadata.is_empty() {
        let joined = metadata.join("  •  ");
        let lines = wrap_text(&joined, 10.0, 235.0, 1, false, "cover metadata")?;
        canvas.text(&lines[0], 10.0, 22.0, 40.0, false, palette.muted.clone());
    }

    draw_footer(canvas, footer, 1, total_pages, palette.muted.clone())
}

fn draw_part(
    canvas: &Canvas<'_>,
    language: Language,
    title: &str,
    brand: &Brand,
    footer: Option<&str>,
    page: usize,
    total_pages: usize,
) -> Result<()> {
    let palette = canvas.palette;
    canvas.fill_rect(
        0.0,
        0.0,
        PAGE_WIDTH_MM,
        PAGE_HEIGHT_MM,
        palette.accent.clone(),
    );
    canvas.fill_rect(0.0, 0.0, PAGE_WIDTH_MM, 22.0, palette.background.clone());

    let on_accent = palette.on_accent();
    canvas.text(
        part_label(language),
        11.0,
        24.0,
        150.0,
        true,
        on_accent.clone(),
    );
    let lines = wrap_text(title, 32.0, 240.0, 3, true, "part title")?;
    for (index, line) in lines.iter().enumerate() {
        canvas.text(
            line,
            32.0,
            24.0,
            126.0 - index as f32 * 14.0,
            true,
            on_accent.clone(),
        );
    }

    draw_monogram(
        canvas,
        PAGE_WIDTH_MM - MARGIN_MM - 10.0,
        PAGE_HEIGHT_MM - 28.0,
        10.0,
        brand.monogram_text.as_str(),
        palette.background.clone(),
        palette.contrast_on(palette.background_rgb),
    );

    draw_footer(
        canvas,
        footer,
        page,
        total_pages,
        palette.contrast_on(palette.background_rgb),
    )
}

#[allow(clippy::too_many_arguments)]
fn draw_section(
    canvas: &Canvas<'_>,
    language: Language,
    part: Option<&str>,
    title: &str,
    brand: &Brand,
    footer: Option<&str>,
    page: usize,
    total_pages: usize,
) -> Result<()> {
    let palette = canvas.palette;
    canvas.fill_rect(
        0.0,
        0.0,
        PAGE_WIDTH_MM,
        PAGE_HEIGHT_MM,
        palette.background.clone(),
    );
    canvas.fill_rect(0.0, 0.0, 14.0, PAGE_HEIGHT_MM, palette.accent.clone());
    canvas.fill_rect(36.0, 168.0, 243.0, 0.6, palette.accent.clone());

    canvas.text(
        section_label(language),
        10.5,
        36.0,
        152.0,
        true,
        palette.accent.clone(),
    );
    let lines = wrap_text(title, 28.0, 230.0, 3, true, "section title")?;
    for (index, line) in lines.iter().enumerate() {
        canvas.text(
            line,
            28.0,
            36.0,
            132.0 - index as f32 * 12.5,
            true,
            palette.foreground.clone(),
        );
    }

    if let Some(part) = part {
        let label = format!("{}  {}", part_label(language), part);
        let part_lines = wrap_text(&label, 10.0, 230.0, 2, false, "part label")?;
        for (index, line) in part_lines.iter().enumerate() {
            canvas.text(
                line,
                10.0,
                36.0,
                58.0 - index as f32 * 5.5,
                false,
                palette.muted.clone(),
            );
        }
    } else {
        let lines = wrap_text(brand.name.as_str(), 10.0, 230.0, 1, false, "brand name")?;
        canvas.text(&lines[0], 10.0, 36.0, 58.0, false, palette.muted.clone());
    }

    draw_monogram(
        canvas,
        PAGE_WIDTH_MM - MARGIN_MM - 10.0,
        PAGE_HEIGHT_MM - 30.0,
        10.0,
        brand.monogram_text.as_str(),
        palette.accent.clone(),
        palette.on_accent(),
    );

    draw_footer(canvas, footer, page, total_pages, palette.muted.clone())
}

#[allow(clippy::too_many_arguments)]
fn draw_cards(
    canvas: &Canvas<'_>,
    spec: &DeckSpec,
    brand: &Brand,
    part: Option<&str>,
    title: &str,
    cards: &[DeckCard],
    footer: Option<&str>,
    page: usize,
    total_pages: usize,
) -> Result<()> {
    draw_content_header(canvas, spec.language, part, title)?;
    for (index, card) in cards.iter().enumerate() {
        let column = index % 2;
        let row = index / 2;
        let x = MARGIN_MM + column as f32 * (CARD_WIDTH_MM + CARD_COLUMN_GAP_MM);
        let top = CARD_TOP_MM - row as f32 * (CARD_HEIGHT_MM + CARD_ROW_GAP_MM);
        draw_card(canvas, brand, card, x, top)?;
    }
    draw_footer(
        canvas,
        footer,
        page,
        total_pages,
        canvas.palette.muted.clone(),
    )
}

fn draw_card(canvas: &Canvas<'_>, brand: &Brand, card: &DeckCard, x: f32, top: f32) -> Result<()> {
    let palette = canvas.palette;
    let bottom = top - CARD_HEIGHT_MM;
    canvas.fill_rect(
        x,
        bottom,
        CARD_WIDTH_MM,
        CARD_HEIGHT_MM,
        palette.background.clone(),
    );
    canvas.stroke_rect(
        x,
        bottom,
        CARD_WIDTH_MM,
        CARD_HEIGHT_MM,
        palette.muted.clone(),
        0.6,
    );
    canvas.fill_rect(x, top - 2.0, CARD_WIDTH_MM, 2.0, palette.accent.clone());

    let monogram = card
        .monogram
        .as_deref()
        .unwrap_or(brand.monogram_text.as_str());
    draw_monogram(
        canvas,
        x + 14.0,
        top - 14.0,
        5.5,
        monogram,
        palette.accent.clone(),
        palette.on_accent(),
    );

    let heading_lines = wrap_text(
        &card.heading,
        11.5,
        CARD_WIDTH_MM - 31.0,
        2,
        true,
        "card heading",
    )?;
    for (index, line) in heading_lines.iter().enumerate() {
        canvas.text(
            line,
            11.5,
            x + 24.0,
            top - 17.0 - index as f32 * 5.2,
            true,
            palette.foreground.clone(),
        );
    }

    let mut cursor = top - CARD_TEXT_TOP_MM;
    let mut used_lines = 0usize;
    if !card.body.is_empty() {
        let lines = wrap_text(&card.body, 9.2, CARD_WIDTH_MM - 16.0, 5, false, "card body")?;
        used_lines = lines.len();
        for line in &lines {
            canvas.text(
                line,
                9.2,
                x + 8.0,
                cursor,
                false,
                palette.foreground.clone(),
            );
            cursor -= 4.6;
        }
    }

    for bullet in &card.bullets {
        let remaining = CARD_TEXT_LINES.saturating_sub(used_lines);
        if remaining == 0 {
            return Err(SeraError::Validation(
                "card text does not fit the card layout".into(),
            ));
        }
        let lines = wrap_text(
            bullet,
            8.4,
            CARD_WIDTH_MM - 22.0,
            remaining,
            false,
            "card bullet",
        )?;
        for (index, line) in lines.iter().enumerate() {
            if index == 0 {
                canvas.text("•", 8.4, x + 8.0, cursor, false, palette.accent.clone());
            }
            canvas.text(
                line,
                8.4,
                x + 13.0,
                cursor,
                false,
                palette.foreground.clone(),
            );
            cursor -= 4.2;
        }
        used_lines += lines.len();
    }

    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn draw_bullets(
    canvas: &Canvas<'_>,
    language: Language,
    part: Option<&str>,
    title: &str,
    bullets: &[String],
    footer: Option<&str>,
    page: usize,
    total_pages: usize,
) -> Result<()> {
    draw_content_header(canvas, language, part, title)?;
    let palette = canvas.palette;
    let mut y = 159.0;
    for bullet in bullets {
        let lines = wrap_text(bullet, 10.5, 248.0, 3, false, "bullet")?;
        for (index, line) in lines.iter().enumerate() {
            if index == 0 {
                canvas.fill_circle(MARGIN_MM + 2.0, y + 1.2, 1.2, palette.accent.clone());
            }
            canvas.text(
                line,
                10.5,
                MARGIN_MM + 8.0,
                y,
                false,
                palette.foreground.clone(),
            );
            y -= 5.6;
        }
        y -= 4.2;
    }
    draw_footer(canvas, footer, page, total_pages, palette.muted.clone())
}

fn draw_content_header(
    canvas: &Canvas<'_>,
    language: Language,
    part: Option<&str>,
    title: &str,
) -> Result<()> {
    let palette = canvas.palette;
    canvas.fill_rect(
        MARGIN_MM,
        201.0,
        PAGE_WIDTH_MM - 2.0 * MARGIN_MM,
        1.2,
        palette.accent.clone(),
    );

    let label = match part {
        Some(part) => format!("{}  /  {}", part_label(language), part),
        None => section_label(language).to_string(),
    };
    let label_lines = wrap_text(&label, 8.0, 261.0, 1, true, "content label")?;
    canvas.text(
        &label_lines[0],
        8.0,
        MARGIN_MM,
        194.5,
        true,
        palette.accent.clone(),
    );

    let title_lines = wrap_text(title, 18.0, 261.0, 2, true, "section title")?;
    for (index, line) in title_lines.iter().enumerate() {
        canvas.text(
            line,
            18.0,
            MARGIN_MM,
            187.0 - index as f32 * 7.0,
            true,
            palette.foreground.clone(),
        );
    }
    canvas.fill_rect(
        MARGIN_MM,
        176.0,
        PAGE_WIDTH_MM - 2.0 * MARGIN_MM,
        0.4,
        palette.muted.clone(),
    );
    Ok(())
}

fn draw_monogram(
    canvas: &Canvas<'_>,
    cx: f32,
    cy: f32,
    radius: f32,
    text: &str,
    fill: Color,
    text_color: Color,
) {
    canvas.fill_circle(cx, cy, radius, fill);
    let characters = text.chars().count();
    let size = match characters {
        0 | 1 => radius * 1.6,
        2 => radius * 1.4,
        3 => radius * 1.15,
        _ => radius * 0.95,
    }
    .clamp(7.0, 18.0);
    let width = text_width_mm(text, size, true);
    let height = size / PT_PER_MM;
    canvas.text(
        text,
        size,
        cx - width / 2.0,
        cy - height * 0.35,
        true,
        text_color,
    );
}

fn draw_footer(
    canvas: &Canvas<'_>,
    footer: Option<&str>,
    page: usize,
    total_pages: usize,
    color: Color,
) -> Result<()> {
    if let Some(footer) = footer {
        let lines = wrap_text(footer, 7.5, 215.0, 1, false, "footer")?;
        canvas.text(&lines[0], 7.5, MARGIN_MM, 11.5, false, color.clone());
    }
    let label = format!("{page} / {total_pages}");
    let width = text_width_mm(&label, 7.5, false);
    canvas.text(
        &label,
        7.5,
        PAGE_WIDTH_MM - MARGIN_MM - width,
        11.5,
        false,
        color,
    );
    Ok(())
}

fn part_label(language: Language) -> &'static str {
    match language {
        Language::It => "PARTE",
        Language::Fr => "PARTIE",
        Language::En => "PART",
    }
}

fn section_label(language: Language) -> &'static str {
    match language {
        Language::It => "SEZIONE",
        Language::Fr => "SECTION",
        Language::En => "SECTION",
    }
}

fn wrap_text(
    text: &str,
    font_size: f32,
    width_mm: f32,
    max_lines: usize,
    bold: bool,
    label: &str,
) -> Result<Vec<String>> {
    if max_lines == 0 {
        return Err(SeraError::Validation(format!(
            "{label} does not fit the layout"
        )));
    }
    let max_width = width_mm * PT_PER_MM;
    let mut lines = Vec::new();
    let mut current = String::new();
    let mut current_width = 0.0f32;

    for word in text.split_whitespace() {
        let word_width = text_width_pt(word, font_size, bold);
        if word_width > max_width {
            return Err(SeraError::Validation(format!(
                "{label} contains a word wider than its layout box"
            )));
        }
        if current.is_empty() {
            current.push_str(word);
            current_width = word_width;
        } else {
            let space_width = glyph_width_pt(' ', font_size, bold);
            if current_width + space_width + word_width <= max_width {
                current.push(' ');
                current.push_str(word);
                current_width += space_width + word_width;
            } else {
                if lines.len() == max_lines {
                    return Err(SeraError::Validation(format!(
                        "{label} requires more than {max_lines} lines"
                    )));
                }
                lines.push(std::mem::take(&mut current));
                current.push_str(word);
                current_width = word_width;
            }
        }
    }

    if !current.is_empty() {
        if lines.len() == max_lines {
            return Err(SeraError::Validation(format!(
                "{label} requires more than {max_lines} lines"
            )));
        }
        lines.push(current);
    }
    if lines.is_empty() {
        return Err(SeraError::Validation(format!("{label} must not be empty")));
    }
    Ok(lines)
}

fn text_width_mm(text: &str, font_size: f32, bold: bool) -> f32 {
    text_width_pt(text, font_size, bold) / PT_PER_MM
}

fn text_width_pt(text: &str, font_size: f32, bold: bool) -> f32 {
    text.chars()
        .map(|character| glyph_width_pt(character, font_size, bold))
        .sum()
}

/// Conservative Helvetica advance widths. The built-in faces have no font
/// metrics handle here, so wide glyphs are deliberately over-estimated to
/// guarantee a line accepted by the wrapper cannot overflow its box.
fn glyph_width_pt(character: char, font_size: f32, bold: bool) -> f32 {
    let em = match character {
        ' ' => 0.28,
        'i' | 'l' | 'j' | 'I' | '.' | ',' | ';' | ':' | '!' | '\'' | '|' => 0.28,
        'f' | 't' | 'r' | '(' | ')' | '[' | ']' | '{' | '}' | '-' | '/' | '\\' => 0.36,
        'W' | 'M' | 'm' | 'w' | 'œ' | 'Œ' | 'æ' | 'Æ' => 0.95,
        'A'..='Z' => 0.75,
        '0'..='9' => 0.56,
        '@' => 1.05,
        '…' | '‰' | '™' | '—' => 1.0,
        '%' => 0.90,
        'Ø' => 0.80,
        '&' => 0.70,
        '–' => 0.60,
        character if character.is_ascii() => 0.60,
        _ => 0.75,
    };
    let weight = if bold { 1.1 } else { 1.0 };
    em * font_size * weight
}

fn deck_filename(spec: &DeckSpec) -> String {
    format!("{}-{}.pdf", slugify(&spec.title), spec.language.code())
}

fn slugify(value: &str) -> String {
    let mut raw = String::new();
    for character in value.nfd() {
        if is_combining_mark(character) {
            continue;
        }
        if character.is_ascii_alphanumeric() {
            raw.push(character.to_ascii_lowercase());
        } else if matches!(character, 'œ' | 'Œ') {
            raw.push_str("oe");
        } else if matches!(character, 'æ' | 'Æ') {
            raw.push_str("ae");
        } else if character == 'ß' {
            raw.push_str("ss");
        } else {
            raw.push('-');
        }
    }

    let mut collapsed = String::with_capacity(raw.len());
    for character in raw.chars() {
        if character == '-' {
            if !collapsed.ends_with('-') {
                collapsed.push('-');
            }
        } else {
            collapsed.push(character);
        }
    }

    let limited: String = collapsed.trim_matches('-').chars().take(80).collect();
    let limited = limited.trim_end_matches('-');
    if limited.is_empty() {
        "deck".into()
    } else {
        limited.to_string()
    }
}

fn inspect_pdf(pdf: &[u8]) -> Result<(String, usize)> {
    let document = lopdf::Document::load_mem(pdf)
        .map_err(|error| SeraError::Internal(format!("cannot parse generated pdf: {error}")))?;
    let pages = document.get_pages();
    let count = pages.len();
    let mut text = String::new();
    for page_id in pages.values() {
        let content = document
            .get_page_content(*page_id)
            .map_err(|error| SeraError::Internal(format!("cannot read page content: {error}")))?;
        text.push_str(&extract_content(&content));
    }
    Ok((text, count))
}

fn extract_content(content: &[u8]) -> String {
    let mut output = String::new();
    let mut operands: Vec<Operand> = Vec::new();
    let mut index = 0;

    while index < content.len() {
        match content[index] {
            b'%' => {
                while index < content.len() && content[index] != b'\n' {
                    index += 1;
                }
            }
            b'(' => {
                let (bytes, next) = read_literal_string(content, index);
                operands.push(Operand::Text(bytes));
                index = next;
            }
            b'<' if content.get(index + 1) == Some(&b'<') => {
                index = skip_dictionary(content, index);
            }
            b'<' => {
                let (bytes, next) = read_hex_string(content, index);
                operands.push(Operand::Text(bytes));
                index = next;
            }
            b'[' => {
                let (strings, next) = read_text_array(content, index);
                operands.push(Operand::Array(strings));
                index = next;
            }
            b']' | b'>' => {
                index += 1;
            }
            b'/' => {
                let (_, next) = read_token(content, index + 1);
                operands.push(Operand::Other);
                index = next;
            }
            b' ' | b'\t' | b'\r' | b'\n' | b'\x0c' | b'\0' => {
                index += 1;
            }
            _ => {
                let (token, next) = read_token(content, index);
                if token.is_empty() {
                    index += 1;
                    continue;
                }
                match std::str::from_utf8(token).unwrap_or("") {
                    "Tj" => {
                        if let Some(Operand::Text(bytes)) = operands.pop() {
                            output.push_str(&decode_winansi(&bytes));
                            output.push('\n');
                        }
                    }
                    "TJ" => {
                        if let Some(Operand::Array(strings)) = operands.pop() {
                            for bytes in strings {
                                output.push_str(&decode_winansi(&bytes));
                            }
                            output.push('\n');
                        }
                    }
                    _ => {}
                }
                operands.clear();
                index = next;
            }
        }
    }

    output
}

enum Operand {
    Text(Vec<u8>),
    Array(Vec<Vec<u8>>),
    Other,
}

fn read_token(content: &[u8], mut index: usize) -> (&[u8], usize) {
    let start = index;
    while index < content.len() && !is_delimiter(content[index]) {
        index += 1;
    }
    (&content[start..index], index)
}

fn is_delimiter(byte: u8) -> bool {
    matches!(
        byte,
        b'(' | b')'
            | b'<'
            | b'>'
            | b'['
            | b']'
            | b'{'
            | b'}'
            | b'/'
            | b'%'
            | b' '
            | b'\t'
            | b'\r'
            | b'\n'
            | b'\x0c'
            | b'\0'
    )
}

fn read_text_array(content: &[u8], start: usize) -> (Vec<Vec<u8>>, usize) {
    let mut strings = Vec::new();
    let mut index = start + 1;
    while index < content.len() {
        match content[index] {
            b']' => return (strings, index + 1),
            b'(' => {
                let (bytes, next) = read_literal_string(content, index);
                strings.push(bytes);
                index = next;
            }
            b'<' if content.get(index + 1) == Some(&b'<') => {
                index = skip_dictionary(content, index);
            }
            b'<' => {
                let (bytes, next) = read_hex_string(content, index);
                strings.push(bytes);
                index = next;
            }
            b'%' => {
                while index < content.len() && content[index] != b'\n' {
                    index += 1;
                }
            }
            _ => index += 1,
        }
    }
    (strings, index)
}

fn read_hex_string(content: &[u8], start: usize) -> (Vec<u8>, usize) {
    let mut bytes = Vec::new();
    let mut high: Option<u8> = None;
    let mut index = start + 1;
    while index < content.len() && content[index] != b'>' {
        if let Some(nibble) = hex_nibble(content[index]) {
            if let Some(high) = high.take() {
                bytes.push((high << 4) | nibble);
            } else {
                high = Some(nibble);
            }
        }
        index += 1;
    }
    (bytes, (index + 1).min(content.len()))
}

fn read_literal_string(content: &[u8], start: usize) -> (Vec<u8>, usize) {
    let mut bytes = Vec::new();
    let mut depth = 1usize;
    let mut index = start + 1;
    while index < content.len() {
        match content[index] {
            b'\\' => {
                index += 1;
                if index >= content.len() {
                    break;
                }
                match content[index] {
                    b'n' => bytes.push(b'\n'),
                    b'r' => bytes.push(b'\r'),
                    b't' => bytes.push(b'\t'),
                    b'b' => bytes.push(0x08),
                    b'f' => bytes.push(0x0c),
                    b'(' | b')' | b'\\' => bytes.push(content[index]),
                    b'\r' => {
                        if content.get(index + 1) == Some(&b'\n') {
                            index += 1;
                        }
                    }
                    b'\n' => {}
                    digit @ b'0'..=b'7' => {
                        let mut value = (digit - b'0') as u16;
                        let mut digits = 1;
                        while digits < 3
                            && content
                                .get(index + 1)
                                .is_some_and(|next| matches!(*next, b'0'..=b'7'))
                        {
                            index += 1;
                            value = value * 8 + (content[index] - b'0') as u16;
                            digits += 1;
                        }
                        bytes.push(value as u8);
                    }
                    other => bytes.push(other),
                }
                index += 1;
            }
            b'(' => {
                depth += 1;
                bytes.push(b'(');
                index += 1;
            }
            b')' => {
                depth -= 1;
                index += 1;
                if depth == 0 {
                    return (bytes, index);
                }
                bytes.push(b')');
            }
            byte => {
                bytes.push(byte);
                index += 1;
            }
        }
    }
    (bytes, index)
}

fn skip_dictionary(content: &[u8], start: usize) -> usize {
    let mut depth = 0usize;
    let mut index = start;
    while index < content.len() {
        if content[index] == b'(' {
            let (_, next) = read_literal_string(content, index);
            index = next;
        } else if content.get(index) == Some(&b'<') && content.get(index + 1) == Some(&b'<') {
            depth += 1;
            index += 2;
        } else if content.get(index) == Some(&b'>') && content.get(index + 1) == Some(&b'>') {
            depth = depth.saturating_sub(1);
            index += 2;
            if depth == 0 {
                return index;
            }
        } else {
            index += 1;
        }
    }
    index
}

fn hex_nibble(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

fn decode_winansi(bytes: &[u8]) -> String {
    bytes
        .iter()
        .filter_map(|byte| winansi_char(*byte))
        .collect()
}

fn winansi_char(byte: u8) -> Option<char> {
    match byte {
        0x09 | 0x0a | 0x0d => Some(' '),
        0x20..=0x7e => Some(byte as char),
        0xa0..=0xff => Some(char::from(byte)),
        0x80 => Some('€'),
        0x82 => Some('‚'),
        0x83 => Some('ƒ'),
        0x84 => Some('„'),
        0x85 => Some('…'),
        0x86 => Some('†'),
        0x87 => Some('‡'),
        0x88 => Some('ˆ'),
        0x89 => Some('‰'),
        0x8a => Some('Š'),
        0x8b => Some('‹'),
        0x8c => Some('Œ'),
        0x8e => Some('Ž'),
        0x91 => Some('‘'),
        0x92 => Some('’'),
        0x93 => Some('“'),
        0x94 => Some('”'),
        0x95 => Some('•'),
        0x96 => Some('–'),
        0x97 => Some('—'),
        0x98 => Some('˜'),
        0x99 => Some('™'),
        0x9a => Some('š'),
        0x9b => Some('›'),
        0x9c => Some('œ'),
        0x9e => Some('ž'),
        0x9f => Some('Ÿ'),
        _ => None,
    }
}

fn pdf_error(context: &str, error: printpdf::Error) -> SeraError {
    SeraError::Internal(format!("{context}: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::deck::{BrandPalette, MAX_DECK_TITLE_CHARS};

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

    fn fr_spec() -> DeckSpec {
        DeckSpec {
            title: "Étude de marché".into(),
            subtitle: Some("Synthèse bilingue pour décideurs".into()),
            language: Language::Fr,
            date_label: Some("Octobre 2026".into()),
            author: Some("SeraBot".into()),
            sections: vec![crate::deck::DeckSection {
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

    #[test]
    fn renders_french_accents_and_expected_pages() {
        let rendered = render_deck(&fr_spec(), &brand(), 30).unwrap();
        assert!(rendered.pdf.starts_with(b"%PDF-1."));
        assert_eq!(rendered.pages, 5);
        assert_eq!(rendered.sha256, sha256_hex(&rendered.pdf));
        assert_eq!(rendered.filename, "etude-de-marche-fr.pdf");

        for needle in [
            "Étude de marché",
            "Partenariats",
            "PARTIE",
            "Opportunités à l'étranger",
            "Café",
            "œuvres",
            "leçon",
            "à Paris",
            "Réduction",
        ] {
            assert!(
                rendered.text.contains(needle),
                "missing {needle:?} in extracted text: {}",
                rendered.text
            );
        }

        let document = lopdf::Document::load_mem(&rendered.pdf).unwrap();
        assert_eq!(document.get_pages().len(), rendered.pages);
    }

    #[test]
    fn layout_and_text_are_deterministic() {
        // printpdf 0.7 emits a random /ID trailer, so byte hashes can differ
        // between runs; page order, line breaks, and extracted text do not.
        let first = render_deck(&fr_spec(), &brand(), 30).unwrap();
        let second = render_deck(&fr_spec(), &brand(), 30).unwrap();
        assert_eq!(first.text, second.text);
        assert_eq!(first.pages, second.pages);
        assert_eq!(first.filename, second.filename);
    }

    #[test]
    fn rejects_oversized_text() {
        let mut spec = fr_spec();
        spec.title = "x".repeat(MAX_DECK_TITLE_CHARS + 1);
        assert!(render_deck(&spec, &brand(), 30).is_err());
    }

    #[test]
    fn rejects_text_that_does_not_fit_the_layout() {
        let mut spec = fr_spec();
        spec.sections[0].cards[0].body = "mot ".repeat(100);
        assert!(render_deck(&spec, &brand(), 30).is_err());
    }

    #[test]
    fn rejects_too_many_pages() {
        assert!(render_deck(&fr_spec(), &brand(), 3).is_err());
    }

    #[test]
    fn rejects_empty_deck() {
        let mut spec = fr_spec();
        spec.sections.clear();
        assert!(render_deck(&spec, &brand(), 30).is_err());
    }

    #[test]
    fn sanitizes_the_filename() {
        let mut spec = fr_spec();
        spec.title = "../../etc/passwd".into();
        let rendered = render_deck(&spec, &brand(), 30).unwrap();
        assert_eq!(rendered.filename, "etc-passwd-fr.pdf");
        assert!(!rendered.filename.contains('/'));
        assert!(!rendered.filename.contains('\\'));
        assert!(!rendered.filename.contains(".."));
    }

    #[test]
    fn extracts_tj_and_tj_arrays() {
        let content =
            br"BT /F1 9 Tf <43 61 66 e9> Tj ET BT [<41> -20 <42>] TJ ET BT (Caf\351) Tj ET";
        let text = extract_content(content);
        assert!(text.contains("Café"), "extracted: {text}");
        assert!(text.contains("AB"), "extracted: {text}");
        assert_eq!(text.matches("Café").count(), 2);
    }
}
