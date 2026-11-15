//! Server-rendered artifact previews (0.28.0, upgraded in 0.44.0):
//! `GET /v1/artifacts/{hash}/preview` returns rich visual geometry for images,
//! PDFs, PowerPoint decks, Excel workbooks, Word documents, and Markdown, while
//! remaining backwards-compatible with older flat `{content_type, text?, image_b64?}`
//! or `{payload: {preview: ...}}` envelopes.
//!
//! Payloads are untrusted: text is bounded and control characters dropped,
//! hex colors are validated, and base64 images are validated and size-capped
//! before becoming data URLs.

use base64::Engine;
use serde_json::Value;

/// File extensions the preview dock offers for.
pub const PREVIEWABLE_EXTENSIONS: [&str; 12] = [
    "pdf", "pptx", "xlsx", "docx", "md", "png", "jpg", "jpeg", "webp", "gif", "bmp", "svg",
];

/// Longest preview text kept (chars).
pub const MAX_PREVIEW_TEXT: usize = 200_000;
/// Longest base64 image accepted (chars), about 6 MiB of PNG.
pub const MAX_IMAGE_B64: usize = 8 * 1024 * 1024;
/// Longest content type accepted.
const MAX_CONTENT_TYPE: usize = 128;

/// The format a previewable file name maps to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PreviewKind {
    Pdf,
    Slide,
    Sheet,
    Doc,
    Markdown,
    Image,
}

impl PreviewKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::Pdf => "PDF",
            Self::Slide => "Slides",
            Self::Sheet => "Workbook",
            Self::Doc => "Document",
            Self::Markdown => "Markdown",
            Self::Image => "Image",
        }
    }
}

/// Whether a file name is one the preview endpoint can render.
pub fn preview_kind_for_name(name: &str) -> Option<PreviewKind> {
    let lower = name.trim().to_ascii_lowercase();
    let (stem, ext) = lower.rsplit_once('.')?;
    if stem.is_empty() {
        return None;
    }
    match ext {
        "pdf" => Some(PreviewKind::Pdf),
        "pptx" => Some(PreviewKind::Slide),
        "xlsx" | "xlsm" => Some(PreviewKind::Sheet),
        "docx" => Some(PreviewKind::Doc),
        "md" | "markdown" => Some(PreviewKind::Markdown),
        "png" | "jpg" | "jpeg" | "webp" | "gif" | "bmp" | "svg" => Some(PreviewKind::Image),
        _ => None,
    }
}

/// Rich visual model for the Right Sidebar Preview Dock.
#[derive(Clone, Debug, PartialEq)]
pub enum PreviewVisual {
    Image {
        data_url: String,
        width: Option<u32>,
        height: Option<u32>,
        size_bytes: Option<u64>,
    },
    Pdf {
        page_count: u64,
        pages: Vec<PdfPageVisual>,
    },
    Pptx {
        slide_count: usize,
        slides: Vec<SlideVisual>,
    },
    Xlsx {
        sheets: Vec<SheetVisual>,
    },
    Docx {
        blocks: Vec<DocBlockVisual>,
    },
    Markdown {
        markdown: String,
    },
    PlainText,
}

#[derive(Clone, Debug, PartialEq)]
pub struct PdfPageVisual {
    pub page: u64,
    pub width_pt: f64,
    pub height_pt: f64,
    pub text: String,
    pub background_hex: Option<String>,
    pub rects: Vec<PdfRect>,
    pub text_blocks: Vec<PdfTextBlock>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct PdfRect {
    pub x_pt: f64,
    pub y_pt: f64,
    pub w_pt: f64,
    pub h_pt: f64,
    pub fill_hex: Option<String>,
    pub stroke_hex: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct PdfTextBlock {
    pub x_pt: f64,
    pub y_pt: f64,
    pub font_size: f64,
    pub color_hex: Option<String>,
    pub bold: bool,
    pub text: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct SlideVisual {
    pub slide: usize,
    pub title: Option<String>,
    pub text: String,
    pub width_pt: f64,
    pub height_pt: f64,
    pub background_hex: Option<String>,
    pub shapes: Vec<SlideShape>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SlideShapeKind {
    Text,
    Rect,
    Table,
    Image,
}

#[derive(Clone, Debug, PartialEq)]
pub struct SlideShape {
    pub kind: SlideShapeKind,
    pub x_pt: f64,
    pub y_pt: f64,
    pub w_pt: f64,
    pub h_pt: f64,
    pub fill_hex: Option<String>,
    pub stroke_hex: Option<String>,
    pub text: Option<String>,
    pub font_size: Option<f64>,
    pub color_hex: Option<String>,
    pub bold: bool,
    pub align: Option<String>,
    pub rows: Vec<Vec<String>>,
    pub image_data_url: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SheetVisual {
    pub name: String,
    pub range: String,
    pub rows: Vec<Vec<String>>,
    pub truncated: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DocBlockVisual {
    Heading {
        level: u8,
        text: String,
    },
    Paragraph {
        text: String,
    },
    ListItem {
        text: String,
        ordered: bool,
    },
    Table {
        rows: Vec<Vec<String>>,
    },
    Image {
        data_url: String,
        alt: Option<String>,
    },
}

/// One parsed preview response.
#[derive(Clone, Debug, PartialEq)]
pub struct ArtifactPreview {
    pub content_type: String,
    pub text: Option<String>,
    pub image_b64: Option<String>,
    pub kind: Option<PreviewKind>,
    pub filename: Option<String>,
    pub title: Option<String>,
    pub size_bytes: Option<u64>,
    pub truncated: bool,
    pub visual: PreviewVisual,
}

/// Alias for callers that refer to `DocPreview`.
pub type DocPreview = ArtifactPreview;

/// What the legacy pane can draw from a preview.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PreviewContent {
    /// A `data:` URL for an `<img>`.
    Image(String),
    /// Plain text (markdown, extracted blocks, grid rows).
    Text(String),
}

/// Sanitize preview text by dropping control characters (except `\n` and `\t`)
/// and capping length at `max_chars`.
fn sanitize_text(raw: &str, max_chars: usize) -> String {
    raw.chars()
        .filter(|c| !c.is_control() || matches!(c, '\n' | '\t'))
        .take(max_chars)
        .collect()
}

/// Sanitize a MIME/content-type string.
fn sanitize_content_type(raw: &str) -> String {
    raw.chars()
        .filter(|c| !c.is_control())
        .take(MAX_CONTENT_TYPE)
        .collect()
}

/// Validate a CSS/SVG hex color (`#rgb`, `#rgba`, `#rrggbb`, `#rrggbbaa`).
fn sanitize_hex_color(raw: Option<&str>) -> Option<String> {
    let s = raw?.trim();
    let hex = s.strip_prefix('#')?;
    if matches!(hex.len(), 3 | 4 | 6 | 8) && hex.chars().all(|c| c.is_ascii_hexdigit()) {
        Some(format!("#{hex}"))
    } else {
        None
    }
}

fn positive_f64(val: Option<&Value>, default: f64) -> f64 {
    val.and_then(Value::as_f64)
        .filter(|v| v.is_finite() && *v > 0.0)
        .unwrap_or(default)
}

fn finite_f64(val: Option<&Value>, default: f64) -> f64 {
    val.and_then(Value::as_f64)
        .filter(|v| v.is_finite())
        .unwrap_or(default)
}

/// Format a cell value from an XLSX preview row into a display string.
fn format_cell(cell: &Value) -> String {
    match cell {
        Value::Null => String::new(),
        Value::String(s) => sanitize_text(s, 4_000),
        Value::Number(n) => n.to_string(),
        Value::Bool(b) => b.to_string(),
        other => sanitize_text(&other.to_string(), 4_000),
    }
}

/// Validate a base64 image field if present and non-null.
fn parse_image_b64(raw_val: Option<&Value>) -> Result<Option<String>, ()> {
    match raw_val {
        None | Some(Value::Null) => Ok(None),
        Some(raw) => {
            let raw = raw.as_str().ok_or(())?;
            let trimmed = raw.trim();
            if trimmed.is_empty() || trimmed.chars().count() > MAX_IMAGE_B64 {
                return Err(());
            }
            if !trimmed
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '/' | '='))
            {
                return Err(());
            }
            Ok(Some(trimmed.to_owned()))
        }
    }
}

fn build_image_data_url(b64: &str, mime: Option<&str>, allow_svg_and_bmp: bool) -> Option<String> {
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(b64.as_bytes())
        .ok()?;
    if bytes.is_empty() || bytes.len() > MAX_IMAGE_B64 {
        return None;
    }
    let lower = mime.unwrap_or("").trim().to_ascii_lowercase();
    let safe_mime = match lower.as_str() {
        "image/png" => "image/png",
        "image/jpeg" | "image/jpg" => "image/jpeg",
        "image/webp" => "image/webp",
        "image/gif" => "image/gif",
        "image/bmp" if allow_svg_and_bmp => "image/bmp",
        "image/svg+xml" if allow_svg_and_bmp => "image/svg+xml",
        _ => "image/png",
    };
    Some(format!("data:{safe_mime};base64,{b64}"))
}

fn parse_kind_str(kind: &str, filename: Option<&str>) -> Option<PreviewKind> {
    match kind.trim().to_ascii_lowercase().as_str() {
        "image" => Some(PreviewKind::Image),
        "pdf" => Some(PreviewKind::Pdf),
        "pptx" | "slide" => Some(PreviewKind::Slide),
        "xlsx" | "xlsm" | "sheet" => Some(PreviewKind::Sheet),
        "docx" | "doc" => Some(PreviewKind::Doc),
        "md" | "markdown" => Some(PreviewKind::Markdown),
        _ => filename.and_then(preview_kind_for_name),
    }
}

fn parse_rows(val: Option<&Value>) -> Vec<Vec<String>> {
    let Some(rows) = val.and_then(Value::as_array) else {
        return Vec::new();
    };
    rows.iter()
        .filter_map(|row| {
            let cells = row.as_array()?;
            Some(cells.iter().map(format_cell).collect())
        })
        .collect()
}

/// Format the `preview` field into legacy plain display text.
fn format_envelope_preview(value: &Value, container: &Value) -> Option<String> {
    let preview = container.get("preview")?;
    let kind = value.get("kind").and_then(Value::as_str).unwrap_or("");
    let title_or_filename = value
        .get("title")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .or_else(|| {
            value
                .get("filename")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|s| !s.is_empty())
        })
        .unwrap_or("Document");

    match preview {
        Value::String(s) => Some(s.clone()),
        Value::Object(obj) => {
            if kind == "pdf" || obj.contains_key("page_count") {
                let page_count = obj.get("page_count").and_then(Value::as_u64).unwrap_or(1);
                let header = format!("{title_or_filename} \u{2014} {page_count} page(s)");
                let body = obj
                    .get("text")
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(ToOwned::to_owned)
                    .or_else(|| {
                        obj.get("pages").and_then(Value::as_array).map(|pages| {
                            pages
                                .iter()
                                .filter_map(|p| p.get("text").and_then(Value::as_str))
                                .map(str::trim)
                                .filter(|s| !s.is_empty())
                                .collect::<Vec<_>>()
                                .join("\n\n")
                        })
                    })
                    .unwrap_or_default();
                if body.trim().is_empty() {
                    Some(format!(
                        "{header}\n\nSlide/page graphics rendered as vector outlines. Click Open to view the full PDF."
                    ))
                } else {
                    Some(format!("{header}\n\n{body}"))
                }
            } else if kind == "pptx" || obj.contains_key("slides") {
                if let Some(slides) = obj.get("slides").and_then(Value::as_array) {
                    let parts: Vec<String> = slides
                        .iter()
                        .enumerate()
                        .map(|(i, s)| {
                            let n = s
                                .get("slide")
                                .and_then(Value::as_u64)
                                .unwrap_or((i + 1) as u64);
                            let t = s.get("text").and_then(Value::as_str).unwrap_or("").trim();
                            if t.is_empty() {
                                format!("Slide {n}")
                            } else {
                                format!("Slide {n}: {t}")
                            }
                        })
                        .collect();
                    Some(parts.join("\n\n"))
                } else {
                    obj.get("text")
                        .and_then(Value::as_str)
                        .map(ToOwned::to_owned)
                }
            } else if kind == "xlsx" || obj.contains_key("rows") || obj.contains_key("sheets") {
                let first_sheet = obj
                    .get("sheets")
                    .and_then(Value::as_array)
                    .and_then(|s| s.first())
                    .and_then(Value::as_object)
                    .unwrap_or(obj);
                let sheet = first_sheet
                    .get("name")
                    .or_else(|| first_sheet.get("sheet"))
                    .and_then(Value::as_str)
                    .filter(|s| !s.trim().is_empty())
                    .unwrap_or("Sheet1");
                let range = first_sheet
                    .get("range")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .trim();
                let mut out = if range.is_empty() {
                    format!("Sheet: {sheet}\n")
                } else {
                    format!("Sheet: {sheet} ({range})\n")
                };
                if let Some(rows) = first_sheet.get("rows").and_then(Value::as_array) {
                    let formatted_rows: Vec<String> = rows
                        .iter()
                        .filter_map(|row| {
                            let cells = row.as_array()?;
                            let mut strings: Vec<String> = cells.iter().map(format_cell).collect();
                            while strings.last().is_some_and(|s| s.trim().is_empty()) {
                                strings.pop();
                            }
                            Some(strings.join(" | "))
                        })
                        .collect();
                    out.push_str(&formatted_rows.join("\n"));
                }
                Some(out)
            } else if kind == "docx"
                || obj.contains_key("blocks")
                || obj.contains_key("rich_blocks")
            {
                if let Some(blocks) = obj.get("blocks").and_then(Value::as_array) {
                    let parts: Vec<&str> = blocks
                        .iter()
                        .filter_map(|b| {
                            b.as_str().or_else(|| b.get("text").and_then(Value::as_str))
                        })
                        .filter(|s| !s.trim().is_empty())
                        .collect();
                    return Some(parts.join("\n\n"));
                }
                if let Some(rich) = obj.get("rich_blocks").and_then(Value::as_array) {
                    let parts: Vec<&str> = rich
                        .iter()
                        .filter_map(|b| b.get("text").and_then(Value::as_str))
                        .filter(|s| !s.trim().is_empty())
                        .collect();
                    return Some(parts.join("\n\n"));
                }
                None
            } else {
                obj.get("text")
                    .and_then(Value::as_str)
                    .map(ToOwned::to_owned)
            }
        }
        _ => None,
    }
}

fn parse_pdf_visual(
    preview_obj: Option<&serde_json::Map<String, Value>>,
    fallback_text: &str,
) -> PreviewVisual {
    let page_count = preview_obj
        .and_then(|o| o.get("page_count"))
        .and_then(Value::as_u64)
        .unwrap_or(1)
        .max(1);

    let mut pages = Vec::new();
    if let Some(raw_pages) = preview_obj
        .and_then(|o| o.get("pages"))
        .and_then(Value::as_array)
    {
        for (idx, p) in raw_pages.iter().enumerate() {
            let page_num = p
                .get("page")
                .and_then(Value::as_u64)
                .unwrap_or((idx + 1) as u64);
            let width_pt = positive_f64(p.get("width_pt"), 612.0);
            let height_pt = positive_f64(p.get("height_pt"), 792.0);
            let text = sanitize_text(
                p.get("text").and_then(Value::as_str).unwrap_or(""),
                MAX_PREVIEW_TEXT,
            );
            let background_hex =
                sanitize_hex_color(p.get("background_hex").and_then(Value::as_str));

            let rects = p
                .get("rects")
                .and_then(Value::as_array)
                .map(|arr| {
                    arr.iter()
                        .map(|r| PdfRect {
                            x_pt: finite_f64(r.get("x_pt"), 0.0),
                            y_pt: finite_f64(r.get("y_pt"), 0.0),
                            w_pt: finite_f64(r.get("w_pt"), 0.0).max(0.0),
                            h_pt: finite_f64(r.get("h_pt"), 0.0).max(0.0),
                            fill_hex: sanitize_hex_color(r.get("fill_hex").and_then(Value::as_str)),
                            stroke_hex: sanitize_hex_color(
                                r.get("stroke_hex").and_then(Value::as_str),
                            ),
                        })
                        .collect()
                })
                .unwrap_or_default();

            let text_blocks = p
                .get("text_blocks")
                .and_then(Value::as_array)
                .map(|arr| {
                    arr.iter()
                        .filter_map(|tb| {
                            let text = tb.get("text").and_then(Value::as_str)?;
                            Some(PdfTextBlock {
                                x_pt: finite_f64(tb.get("x_pt"), 0.0),
                                y_pt: finite_f64(tb.get("y_pt"), 0.0),
                                font_size: positive_f64(tb.get("font_size"), 11.0),
                                color_hex: sanitize_hex_color(
                                    tb.get("color_hex").and_then(Value::as_str),
                                ),
                                bold: tb.get("bold").and_then(Value::as_bool).unwrap_or(false),
                                text: sanitize_text(text, 4_000),
                            })
                        })
                        .collect()
                })
                .unwrap_or_default();

            pages.push(PdfPageVisual {
                page: page_num,
                width_pt,
                height_pt,
                text,
                background_hex,
                rects,
                text_blocks,
            });
        }
    }

    if pages.is_empty() {
        let doc_text = preview_obj
            .and_then(|o| o.get("text"))
            .and_then(Value::as_str)
            .map(|s| sanitize_text(s, MAX_PREVIEW_TEXT))
            .unwrap_or_else(|| fallback_text.to_owned());
        let chunks: Vec<&str> = doc_text.split('\u{000C}').collect();
        if chunks.len() > 1 {
            for (idx, chunk) in chunks.into_iter().enumerate() {
                pages.push(PdfPageVisual {
                    page: (idx + 1) as u64,
                    width_pt: 612.0,
                    height_pt: 792.0,
                    text: chunk.trim().to_owned(),
                    background_hex: None,
                    rects: Vec::new(),
                    text_blocks: Vec::new(),
                });
            }
        } else {
            pages.push(PdfPageVisual {
                page: 1,
                width_pt: 612.0,
                height_pt: 792.0,
                text: doc_text,
                background_hex: None,
                rects: Vec::new(),
                text_blocks: Vec::new(),
            });
        }
    }

    PreviewVisual::Pdf {
        page_count: page_count.max(pages.len() as u64),
        pages,
    }
}

fn parse_pptx_visual(
    preview_obj: Option<&serde_json::Map<String, Value>>,
    fallback_text: &str,
) -> PreviewVisual {
    let mut slides = Vec::new();
    if let Some(raw_slides) = preview_obj
        .and_then(|o| o.get("slides"))
        .and_then(Value::as_array)
    {
        for (idx, s) in raw_slides.iter().enumerate() {
            let slide_num = s
                .get("slide")
                .and_then(Value::as_u64)
                .map(|n| n as usize)
                .unwrap_or(idx + 1);
            let title = s
                .get("title")
                .and_then(Value::as_str)
                .map(|t| sanitize_text(t.trim(), 500))
                .filter(|t| !t.is_empty());
            let text = sanitize_text(
                s.get("text").and_then(Value::as_str).unwrap_or(""),
                MAX_PREVIEW_TEXT,
            );
            let width_pt = positive_f64(s.get("width_pt"), 720.0);
            let height_pt = positive_f64(s.get("height_pt"), 405.0);
            let background_hex =
                sanitize_hex_color(s.get("background_hex").and_then(Value::as_str));

            let shapes = s
                .get("shapes")
                .and_then(Value::as_array)
                .map(|arr| {
                    arr.iter()
                        .map(|sh| {
                            let kind = match sh
                                .get("kind")
                                .and_then(Value::as_str)
                                .unwrap_or("text")
                                .to_ascii_lowercase()
                                .as_str()
                            {
                                "rect" => SlideShapeKind::Rect,
                                "table" => SlideShapeKind::Table,
                                "image" => SlideShapeKind::Image,
                                _ => SlideShapeKind::Text,
                            };
                            let image_data_url = parse_image_b64(sh.get("image_b64"))
                                .ok()
                                .flatten()
                                .and_then(|b64| {
                                    let mime = sh.get("image_mime").and_then(Value::as_str);
                                    build_image_data_url(&b64, mime, true)
                                });
                            SlideShape {
                                kind,
                                x_pt: finite_f64(sh.get("x_pt"), 0.0),
                                y_pt: finite_f64(sh.get("y_pt"), 0.0),
                                w_pt: finite_f64(sh.get("w_pt"), 100.0).max(0.0),
                                h_pt: finite_f64(sh.get("h_pt"), 40.0).max(0.0),
                                fill_hex: sanitize_hex_color(
                                    sh.get("fill_hex").and_then(Value::as_str),
                                ),
                                stroke_hex: sanitize_hex_color(
                                    sh.get("stroke_hex").and_then(Value::as_str),
                                ),
                                text: sh
                                    .get("text")
                                    .and_then(Value::as_str)
                                    .map(|t| sanitize_text(t, 8_000)),
                                font_size: sh
                                    .get("font_size")
                                    .and_then(Value::as_f64)
                                    .filter(|v| v.is_finite() && *v > 0.0),
                                color_hex: sanitize_hex_color(
                                    sh.get("color_hex").and_then(Value::as_str),
                                ),
                                bold: sh.get("bold").and_then(Value::as_bool).unwrap_or(false),
                                align: sh
                                    .get("align")
                                    .and_then(Value::as_str)
                                    .map(|a| a.to_ascii_lowercase())
                                    .filter(|a| matches!(a.as_str(), "left" | "center" | "right")),
                                rows: parse_rows(sh.get("rows")),
                                image_data_url,
                            }
                        })
                        .collect()
                })
                .unwrap_or_default();

            slides.push(SlideVisual {
                slide: slide_num,
                title,
                text,
                width_pt,
                height_pt,
                background_hex,
                shapes,
            });
        }
    }

    if slides.is_empty() && !fallback_text.trim().is_empty() {
        slides.push(SlideVisual {
            slide: 1,
            title: None,
            text: fallback_text.to_owned(),
            width_pt: 720.0,
            height_pt: 405.0,
            background_hex: None,
            shapes: Vec::new(),
        });
    }

    let slide_count = preview_obj
        .and_then(|o| o.get("slide_count"))
        .and_then(Value::as_u64)
        .map(|n| n as usize)
        .unwrap_or(slides.len())
        .max(slides.len());

    PreviewVisual::Pptx {
        slide_count,
        slides,
    }
}

fn parse_xlsx_visual(preview_obj: Option<&serde_json::Map<String, Value>>) -> PreviewVisual {
    let mut sheets = Vec::new();
    if let Some(raw_sheets) = preview_obj
        .and_then(|o| o.get("sheets"))
        .and_then(Value::as_array)
    {
        for (idx, s) in raw_sheets.iter().enumerate() {
            let name = s
                .get("name")
                .or_else(|| s.get("sheet"))
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|n| !n.is_empty())
                .map(|n| sanitize_text(n, 128))
                .unwrap_or_else(|| format!("Sheet {}", idx + 1));
            let range = s
                .get("range")
                .and_then(Value::as_str)
                .map(|r| sanitize_text(r.trim(), 64))
                .unwrap_or_default();
            let rows = parse_rows(s.get("rows"));
            let truncated = s.get("truncated").and_then(Value::as_bool).unwrap_or(false);
            sheets.push(SheetVisual {
                name,
                range,
                rows,
                truncated,
            });
        }
    }

    if sheets.is_empty()
        && let Some(obj) = preview_obj
    {
        let name = obj
            .get("sheet")
            .or_else(|| obj.get("name"))
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|n| !n.is_empty())
            .map(|n| sanitize_text(n, 128))
            .unwrap_or_else(|| "Sheet1".to_owned());
        let range = obj
            .get("range")
            .and_then(Value::as_str)
            .map(|r| sanitize_text(r.trim(), 64))
            .unwrap_or_default();
        let rows = parse_rows(obj.get("rows"));
        let truncated = obj
            .get("truncated")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        sheets.push(SheetVisual {
            name,
            range,
            rows,
            truncated,
        });
    }

    PreviewVisual::Xlsx { sheets }
}

fn parse_docx_visual(
    preview_obj: Option<&serde_json::Map<String, Value>>,
    fallback_text: &str,
) -> PreviewVisual {
    let mut blocks = Vec::new();
    if let Some(rich) = preview_obj
        .and_then(|o| o.get("rich_blocks"))
        .and_then(Value::as_array)
    {
        for b in rich {
            let kind = b
                .get("kind")
                .and_then(Value::as_str)
                .unwrap_or("paragraph")
                .to_ascii_lowercase();
            match kind.as_str() {
                "heading" => {
                    let level = b
                        .get("level")
                        .and_then(Value::as_u64)
                        .unwrap_or(1)
                        .clamp(1, 6) as u8;
                    if let Some(text) = b.get("text").and_then(Value::as_str) {
                        let clean = sanitize_text(text.trim(), 4_000);
                        if !clean.is_empty() {
                            blocks.push(DocBlockVisual::Heading { level, text: clean });
                        }
                    }
                }
                "list_item" => {
                    if let Some(text) = b.get("text").and_then(Value::as_str) {
                        let clean = sanitize_text(text.trim(), 4_000);
                        if !clean.is_empty() {
                            blocks.push(DocBlockVisual::ListItem {
                                text: clean,
                                ordered: b.get("ordered").and_then(Value::as_bool).unwrap_or(false),
                            });
                        }
                    }
                }
                "table" => {
                    let rows = parse_rows(b.get("rows"));
                    if !rows.is_empty() {
                        blocks.push(DocBlockVisual::Table { rows });
                    }
                }
                "image" => {
                    if let Ok(Some(b64)) = parse_image_b64(b.get("image_b64")) {
                        let mime = b.get("image_mime").and_then(Value::as_str);
                        if let Some(data_url) = build_image_data_url(&b64, mime, true) {
                            let alt = b
                                .get("alt")
                                .and_then(Value::as_str)
                                .map(|a| sanitize_text(a, 256));
                            blocks.push(DocBlockVisual::Image { data_url, alt });
                        }
                    }
                }
                _ => {
                    if let Some(text) = b.get("text").and_then(Value::as_str) {
                        let clean = sanitize_text(text.trim(), 8_000);
                        if !clean.is_empty() {
                            blocks.push(DocBlockVisual::Paragraph { text: clean });
                        }
                    }
                }
            }
        }
    }

    if blocks.is_empty()
        && let Some(plain) = preview_obj
            .and_then(|o| o.get("blocks"))
            .and_then(Value::as_array)
    {
        for b in plain {
            let text = b.as_str().or_else(|| b.get("text").and_then(Value::as_str));
            if let Some(t) = text {
                let clean = sanitize_text(t.trim(), 8_000);
                if !clean.is_empty() {
                    blocks.push(DocBlockVisual::Paragraph { text: clean });
                }
            }
        }
    }

    if blocks.is_empty() && !fallback_text.trim().is_empty() {
        for para in fallback_text.split("\n\n") {
            let clean = para.trim();
            if !clean.is_empty() {
                blocks.push(DocBlockVisual::Paragraph {
                    text: clean.to_owned(),
                });
            }
        }
    }

    PreviewVisual::Docx { blocks }
}

impl ArtifactPreview {
    /// Parse and validate:
    /// - the v0.44.0 flat-envelope `{ok, kind, filename, title, size_bytes, text, preview}`
    /// - the v0.28.0 envelope `{kind, title, filename, payload: {...}}`
    /// - the flat `{content_type, text?, image_b64?}` shape
    ///
    /// Returns `None` when malformed.
    pub fn parse(value: &Value) -> Option<Self> {
        let payload = value.get("payload").filter(|p| p.is_object());
        let filename = value
            .get("filename")
            .and_then(Value::as_str)
            .map(|s| sanitize_text(s.trim(), 256))
            .filter(|s| !s.is_empty());
        let title = value
            .get("title")
            .and_then(Value::as_str)
            .map(|s| sanitize_text(s.trim(), 256))
            .filter(|s| !s.is_empty());
        let size_bytes = value.get("size_bytes").and_then(Value::as_u64);
        let kind_str = value.get("kind").and_then(Value::as_str).unwrap_or("");
        let kind = parse_kind_str(kind_str, filename.as_deref());

        let top_preview = value.get("preview");
        let payload_preview = payload.and_then(|p| p.get("preview"));
        let preview_val = payload_preview.or(top_preview);
        let preview_obj = preview_val.and_then(Value::as_object);

        // A present-but-invalid image field rejects the whole payload: the
        // caller must fall back to download rather than show half a preview.
        let image_val = value
            .get("image_b64")
            .filter(|v| !v.is_null())
            .or_else(|| payload.and_then(|p| p.get("image_b64")))
            .or_else(|| preview_obj.and_then(|o| o.get("image_b64")));
        let image_b64 = parse_image_b64(image_val).ok()?;

        // Check if this is an envelope (either v0.28 `payload` or v0.44 top-level `preview`).
        if payload.is_some() || top_preview.is_some() {
            let container = payload.unwrap_or(value);
            let formatted = format_envelope_preview(value, container).or_else(|| {
                value
                    .get("text")
                    .and_then(Value::as_str)
                    .or_else(|| container.get("text").and_then(Value::as_str))
                    .map(ToOwned::to_owned)
            });

            let truncated = container
                .get("truncated")
                .and_then(Value::as_bool)
                .or_else(|| {
                    preview_obj
                        .and_then(|o| o.get("truncated"))
                        .and_then(Value::as_bool)
                })
                .unwrap_or(false);

            let text = formatted.map(|raw| {
                let suffix = "\n\n[Preview truncated \u{2014} click Open for the full file]";
                if truncated {
                    let suffix_len = suffix.chars().count();
                    let body = sanitize_text(&raw, MAX_PREVIEW_TEXT.saturating_sub(suffix_len));
                    format!("{body}{suffix}")
                } else {
                    sanitize_text(&raw, MAX_PREVIEW_TEXT)
                }
            });

            let content_type = if image_b64.is_some() {
                let raw_ct = preview_obj
                    .and_then(|o| o.get("content_type"))
                    .and_then(Value::as_str)
                    .or_else(|| value.get("content_type").and_then(Value::as_str))
                    .or_else(|| container.get("content_type").and_then(Value::as_str))
                    .unwrap_or("image/png");
                let ct = sanitize_content_type(raw_ct);
                if ct.trim().is_empty() {
                    "image/png".to_owned()
                } else {
                    ct
                }
            } else if text.is_some() {
                "text/plain".to_owned()
            } else {
                let raw_ct = value
                    .get("content_type")
                    .and_then(Value::as_str)
                    .or_else(|| container.get("content_type").and_then(Value::as_str))
                    .unwrap_or("application/json");
                let ct = sanitize_content_type(raw_ct);
                if ct.trim().is_empty() {
                    return None;
                }
                ct
            };

            let visual = if let Some(b64) = &image_b64 {
                let data_url = build_image_data_url(b64, Some(&content_type), true)?;
                let width = preview_obj
                    .and_then(|o| o.get("width"))
                    .and_then(Value::as_u64)
                    .and_then(|v| u32::try_from(v).ok());
                let height = preview_obj
                    .and_then(|o| o.get("height"))
                    .and_then(Value::as_u64)
                    .and_then(|v| u32::try_from(v).ok());
                let img_size = preview_obj
                    .and_then(|o| o.get("size_bytes"))
                    .and_then(Value::as_u64)
                    .or(size_bytes);
                PreviewVisual::Image {
                    data_url,
                    width,
                    height,
                    size_bytes: img_size,
                }
            } else {
                let fallback_text = text.as_deref().unwrap_or("");
                match kind {
                    Some(PreviewKind::Pdf) => parse_pdf_visual(preview_obj, fallback_text),
                    Some(PreviewKind::Slide) => parse_pptx_visual(preview_obj, fallback_text),
                    Some(PreviewKind::Sheet) => parse_xlsx_visual(preview_obj),
                    Some(PreviewKind::Doc) => parse_docx_visual(preview_obj, fallback_text),
                    Some(PreviewKind::Markdown) => {
                        let md = preview_val
                            .and_then(Value::as_str)
                            .or_else(|| {
                                preview_obj
                                    .and_then(|o| o.get("text"))
                                    .and_then(Value::as_str)
                            })
                            .or_else(|| value.get("text").and_then(Value::as_str))
                            .map(|s| sanitize_text(s, MAX_PREVIEW_TEXT))
                            .unwrap_or_else(|| fallback_text.to_owned());
                        PreviewVisual::Markdown { markdown: md }
                    }
                    _ => PreviewVisual::PlainText,
                }
            };

            return Some(Self {
                content_type,
                text,
                image_b64,
                kind,
                filename,
                title,
                size_bytes,
                truncated,
                visual,
            });
        }

        let content_type = sanitize_content_type(value.get("content_type")?.as_str()?);
        if content_type.trim().is_empty() {
            return None;
        }
        let text = value
            .get("text")
            .and_then(Value::as_str)
            .map(|raw| sanitize_text(raw, MAX_PREVIEW_TEXT));
        let visual = if let Some(b64) = &image_b64 {
            let data_url = build_image_data_url(b64, Some(&content_type), true)?;
            PreviewVisual::Image {
                data_url,
                width: None,
                height: None,
                size_bytes,
            }
        } else if content_type.eq_ignore_ascii_case("text/markdown")
            || kind == Some(PreviewKind::Markdown)
        {
            PreviewVisual::Markdown {
                markdown: text.clone().unwrap_or_default(),
            }
        } else {
            PreviewVisual::PlainText
        };
        Some(Self {
            content_type,
            text,
            image_b64,
            kind,
            filename,
            title,
            size_bytes,
            truncated: false,
            visual,
        })
    }

    /// What the legacy pane should draw. An image wins when the server sent a valid
    /// one; otherwise text; `None` means "fall back to download/open".
    pub fn render(&self) -> Option<PreviewContent> {
        if let Some(b64) = &self.image_b64 {
            let url = build_image_data_url(b64, Some(&self.content_type), false)?;
            return Some(PreviewContent::Image(url));
        }
        self.text
            .clone()
            .filter(|text| !text.trim().is_empty())
            .map(PreviewContent::Text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn previewable_names_cover_all_supported_formats() {
        assert_eq!(preview_kind_for_name("deck.PPTX"), Some(PreviewKind::Slide));
        assert_eq!(preview_kind_for_name("report.pdf"), Some(PreviewKind::Pdf));
        assert_eq!(
            preview_kind_for_name("ledger.xlsx"),
            Some(PreviewKind::Sheet)
        );
        assert_eq!(preview_kind_for_name("notes.docx"), Some(PreviewKind::Doc));
        assert_eq!(
            preview_kind_for_name("README.md"),
            Some(PreviewKind::Markdown)
        );
        assert_eq!(preview_kind_for_name("photo.png"), Some(PreviewKind::Image));
        assert_eq!(preview_kind_for_name("photo.JPG"), Some(PreviewKind::Image));
        assert_eq!(preview_kind_for_name("hero.webp"), Some(PreviewKind::Image));
        assert_eq!(preview_kind_for_name("anim.gif"), Some(PreviewKind::Image));
        assert_eq!(preview_kind_for_name("icon.svg"), Some(PreviewKind::Image));
        assert_eq!(preview_kind_for_name(".pdf"), None);
        assert_eq!(preview_kind_for_name("noextension"), None);
        assert_eq!(preview_kind_for_name(""), None);
        for kind in [
            PreviewKind::Pdf,
            PreviewKind::Slide,
            PreviewKind::Sheet,
            PreviewKind::Doc,
            PreviewKind::Markdown,
            PreviewKind::Image,
        ] {
            assert!(!kind.label().is_empty());
        }
    }

    #[test]
    fn text_previews_parse_and_sanitize() {
        let preview = ArtifactPreview::parse(&json!({
            "content_type": "text/markdown",
            "text": "# Title\n\u{7}body\ttext"
        }))
        .unwrap();
        assert_eq!(preview.content_type, "text/markdown");
        assert_eq!(
            preview.render(),
            Some(PreviewContent::Text("# Title\nbody\ttext".into()))
        );
    }

    #[test]
    fn image_previews_become_a_data_url() {
        // "hi" in base64.
        let preview = ArtifactPreview::parse(&json!({
            "content_type": "image/png",
            "image_b64": "aGk="
        }))
        .unwrap();
        assert_eq!(
            preview.render(),
            Some(PreviewContent::Image("data:image/png;base64,aGk=".into()))
        );
        // A missing image/* content type falls back to png.
        let other = ArtifactPreview::parse(&json!({
            "content_type": "application/octet-stream",
            "image_b64": "aGk="
        }))
        .unwrap();
        assert!(
            matches!(other.render(), Some(PreviewContent::Image(url)) if url.starts_with("data:image/png;base64,"))
        );
        // An untrusted or active image MIME (e.g. svg+xml) falls back to png in legacy render().
        let svg = ArtifactPreview::parse(&json!({
            "content_type": "image/svg+xml",
            "image_b64": "aGk="
        }))
        .unwrap();
        assert!(
            matches!(svg.render(), Some(PreviewContent::Image(url)) if url.starts_with("data:image/png;base64,"))
        );
    }

    #[test]
    fn malformed_payloads_are_refused() {
        assert!(ArtifactPreview::parse(&json!({})).is_none());
        assert!(ArtifactPreview::parse(&json!({"content_type": ""})).is_none());
        assert!(
            ArtifactPreview::parse(
                &json!({"content_type": "text/plain", "image_b64": "!!not base64!!"})
            )
            .is_none()
        );
        assert!(
            ArtifactPreview::parse(
                &json!({"content_type": "text/plain", "image_b64": "a".repeat(MAX_IMAGE_B64 + 1)})
            )
            .is_none()
        );
        // Valid shape but nothing to draw: render falls back to None.
        let empty = ArtifactPreview::parse(&json!({"content_type": "text/plain"})).unwrap();
        assert_eq!(empty.render(), None);
        let blank =
            ArtifactPreview::parse(&json!({"content_type": "text/plain", "text": "   "})).unwrap();
        assert_eq!(blank.render(), None);
    }

    #[test]
    fn long_text_is_capped() {
        let preview = ArtifactPreview::parse(&json!({
            "content_type": "text/plain",
            "text": "x".repeat(MAX_PREVIEW_TEXT * 3)
        }))
        .unwrap();
        match preview.render() {
            Some(PreviewContent::Text(text)) => assert!(text.chars().count() <= MAX_PREVIEW_TEXT),
            other => panic!("expected text, got {other:?}"),
        }
    }

    #[test]
    fn envelope_pdf_with_text_and_empty_text() {
        let with_text = ArtifactPreview::parse(&json!({
            "hash": "abc",
            "filename": "report.pdf",
            "title": "Q4 Report",
            "kind": "pdf",
            "payload": {
                "content_type": "application/json",
                "encoding": "utf-8",
                "truncated": false,
                "preview": {
                    "page_count": 4,
                    "text": "Executive summary."
                }
            }
        }))
        .unwrap();
        assert_eq!(with_text.content_type, "text/plain");
        assert_eq!(
            with_text.render(),
            Some(PreviewContent::Text(
                "Q4 Report \u{2014} 4 page(s)\n\nExecutive summary.".into()
            ))
        );

        let empty_text = ArtifactPreview::parse(&json!({
            "filename": "deck.pdf",
            "kind": "pdf",
            "payload": {
                "content_type": "application/json",
                "truncated": false,
                "preview": {
                    "page_count": 12,
                    "text": "   "
                }
            }
        }))
        .unwrap();
        assert_eq!(
            empty_text.render(),
            Some(PreviewContent::Text(
                "deck.pdf \u{2014} 12 page(s)\n\nSlide/page graphics rendered as vector outlines. Click Open to view the full PDF.".into()
            ))
        );
    }

    #[test]
    fn envelope_pptx_and_md_strings() {
        let pptx = ArtifactPreview::parse(&json!({
            "filename": "slides.pptx",
            "kind": "pptx",
            "payload": {
                "content_type": "text/plain",
                "truncated": true,
                "preview": "Slide 1: Overview"
            }
        }))
        .unwrap();
        assert_eq!(pptx.content_type, "text/plain");
        assert_eq!(
            pptx.render(),
            Some(PreviewContent::Text(
                "Slide 1: Overview\n\n[Preview truncated \u{2014} click Open for the full file]"
                    .into()
            ))
        );

        let md = ArtifactPreview::parse(&json!({
            "filename": "README.md",
            "kind": "md",
            "payload": {
                "content_type": "text/markdown",
                "truncated": false,
                "preview": "# Hello\n\nWorld"
            }
        }))
        .unwrap();
        assert_eq!(md.content_type, "text/plain");
        assert_eq!(
            md.render(),
            Some(PreviewContent::Text("# Hello\n\nWorld".into()))
        );
    }

    #[test]
    fn envelope_xlsx_and_docx_objects() {
        let xlsx = ArtifactPreview::parse(&json!({
            "filename": "ledger.xlsx",
            "kind": "xlsx",
            "payload": {
                "content_type": "application/json",
                "truncated": false,
                "preview": {
                    "sheet": "Revenue",
                    "range": "A1:C2",
                    "rows": [
                        ["Month", "Amount", null, ""],
                        ["Jan", 1200, true]
                    ]
                }
            }
        }))
        .unwrap();
        assert_eq!(xlsx.content_type, "text/plain");
        assert_eq!(
            xlsx.render(),
            Some(PreviewContent::Text(
                "Sheet: Revenue (A1:C2)\nMonth | Amount\nJan | 1200 | true".into()
            ))
        );

        let docx = ArtifactPreview::parse(&json!({
            "filename": "memo.docx",
            "kind": "docx",
            "payload": {
                "content_type": "application/json",
                "truncated": false,
                "preview": {
                    "blocks": [
                        {"text": "First paragraph."},
                        "Second paragraph.",
                        {"text": "  "}
                    ]
                }
            }
        }))
        .unwrap();
        assert_eq!(docx.content_type, "text/plain");
        assert_eq!(
            docx.render(),
            Some(PreviewContent::Text(
                "First paragraph.\n\nSecond paragraph.".into()
            ))
        );
    }

    #[test]
    fn v44_image_preview_parses_dimensions_and_data_url() {
        let parsed = ArtifactPreview::parse(&json!({
            "ok": true,
            "kind": "image",
            "filename": "render.png",
            "title": "Render",
            "size_bytes": 4096,
            "preview": {
                "content_type": "image/png",
                "width": 1024,
                "height": 768,
                "size_bytes": 4096,
                "image_b64": "aGk="
            }
        }))
        .unwrap();
        assert_eq!(parsed.kind, Some(PreviewKind::Image));
        assert_eq!(
            parsed.visual,
            PreviewVisual::Image {
                data_url: "data:image/png;base64,aGk=".into(),
                width: Some(1024),
                height: Some(768),
                size_bytes: Some(4096),
            }
        );
    }

    #[test]
    fn v44_pdf_multipage_geometry_parses_cleanly() {
        let parsed = ArtifactPreview::parse(&json!({
            "ok": true,
            "kind": "pdf",
            "filename": "deck.pdf",
            "preview": {
                "page_count": 2,
                "text": "Page 1\nPage 2",
                "pages": [
                    {
                        "page": 1,
                        "width_pt": 720.0,
                        "height_pt": 405.0,
                        "text": "Cover",
                        "background_hex": "#101820",
                        "rects": [{ "x_pt": 10.0, "y_pt": 20.0, "w_pt": 100.0, "h_pt": 40.0, "fill_hex": "#ffffff", "stroke_hex": "javascript:alert(1)" }],
                        "text_blocks": [{ "x_pt": 24.0, "y_pt": 60.0, "font_size": 24.0, "color_hex": "#ffffff", "bold": true, "text": "Q4 Deck" }]
                    },
                    {
                        "page": 2,
                        "width_pt": 720.0,
                        "height_pt": 405.0,
                        "text": "Summary"
                    }
                ]
            }
        }))
        .unwrap();
        match parsed.visual {
            PreviewVisual::Pdf { page_count, pages } => {
                assert_eq!(page_count, 2);
                assert_eq!(pages.len(), 2);
                assert_eq!(pages[0].background_hex.as_deref(), Some("#101820"));
                assert_eq!(pages[0].rects[0].stroke_hex, None);
                assert_eq!(pages[0].text_blocks[0].text, "Q4 Deck");
                assert!(pages[0].text_blocks[0].bold);
                assert_eq!(pages[1].text, "Summary");
            }
            other => panic!("expected Pdf visual, got {other:?}"),
        }
    }

    #[test]
    fn v44_pptx_shapes_parse_cleanly() {
        let parsed = ArtifactPreview::parse(&json!({
            "ok": true,
            "kind": "pptx",
            "filename": "pitch.pptx",
            "preview": {
                "slide_count": 1,
                "slides": [{
                    "slide": 1,
                    "title": "Overview",
                    "text": "Overview",
                    "width_pt": 720.0,
                    "height_pt": 405.0,
                    "background_hex": "#f8fafc",
                    "shapes": [
                        { "kind": "rect", "x_pt": 0.0, "y_pt": 0.0, "w_pt": 720.0, "h_pt": 60.0, "fill_hex": "#0f172a" },
                        { "kind": "text", "x_pt": 36.0, "y_pt": 16.0, "w_pt": 600.0, "h_pt": 32.0, "text": "Overview", "font_size": 20.0, "color_hex": "#ffffff", "bold": true, "align": "left" },
                        { "kind": "table", "x_pt": 36.0, "y_pt": 90.0, "w_pt": 400.0, "h_pt": 120.0, "rows": [["Metric", "Value"], ["ARR", "$1.2M"]] },
                        { "kind": "image", "x_pt": 460.0, "y_pt": 90.0, "w_pt": 200.0, "h_pt": 120.0, "image_b64": "aGk=", "image_mime": "image/png" }
                    ]
                }]
            }
        }))
        .unwrap();
        match parsed.visual {
            PreviewVisual::Pptx {
                slide_count,
                slides,
            } => {
                assert_eq!(slide_count, 1);
                assert_eq!(slides[0].shapes.len(), 4);
                assert_eq!(slides[0].shapes[0].kind, SlideShapeKind::Rect);
                assert_eq!(slides[0].shapes[2].rows.len(), 2);
                assert_eq!(
                    slides[0].shapes[3].image_data_url.as_deref(),
                    Some("data:image/png;base64,aGk=")
                );
            }
            other => panic!("expected Pptx visual, got {other:?}"),
        }
    }

    #[test]
    fn v44_xlsx_multisheet_parses_cleanly() {
        let parsed = ArtifactPreview::parse(&json!({
            "ok": true,
            "kind": "xlsx",
            "filename": "model.xlsx",
            "preview": {
                "sheet": "Summary",
                "range": "A1:B2",
                "rows": [["K", "V"], ["Rev", 100]],
                "truncated": false,
                "sheets": [
                    { "name": "Summary", "range": "A1:B2", "rows": [["K", "V"], ["Rev", 100]], "truncated": false },
                    { "name": "Details", "range": "A1:C3", "rows": [["ID", "Name", "Val"]], "truncated": true }
                ]
            }
        }))
        .unwrap();
        match parsed.visual {
            PreviewVisual::Xlsx { sheets } => {
                assert_eq!(sheets.len(), 2);
                assert_eq!(sheets[0].name, "Summary");
                assert_eq!(sheets[1].name, "Details");
                assert!(sheets[1].truncated);
            }
            other => panic!("expected Xlsx visual, got {other:?}"),
        }
    }

    #[test]
    fn v44_docx_rich_blocks_parse_cleanly() {
        let parsed = ArtifactPreview::parse(&json!({
            "ok": true,
            "kind": "docx",
            "filename": "spec.docx",
            "preview": {
                "blocks": ["Heading", "Body"],
                "truncated": false,
                "rich_blocks": [
                    { "kind": "heading", "level": 1, "text": "Heading" },
                    { "kind": "paragraph", "text": "Body" },
                    { "kind": "list_item", "text": "Point A", "ordered": true },
                    { "kind": "table", "rows": [["Col 1", "Col 2"], ["A", "B"]] },
                    { "kind": "image", "image_b64": "aGk=", "image_mime": "image/png", "alt": "Figure 1" }
                ]
            }
        }))
        .unwrap();
        match parsed.visual {
            PreviewVisual::Docx { blocks } => {
                assert_eq!(blocks.len(), 5);
                assert_eq!(
                    blocks[0],
                    DocBlockVisual::Heading {
                        level: 1,
                        text: "Heading".into()
                    }
                );
                assert_eq!(
                    blocks[2],
                    DocBlockVisual::ListItem {
                        text: "Point A".into(),
                        ordered: true
                    }
                );
                assert!(matches!(blocks[3], DocBlockVisual::Table { .. }));
                assert!(matches!(blocks[4], DocBlockVisual::Image { .. }));
            }
            other => panic!("expected Docx visual, got {other:?}"),
        }
    }
}
