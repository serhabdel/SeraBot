//! Server-rendered artifact previews (0.28.0, phase C3; upgraded v0.44.0):
//! `GET /v1/artifacts/{hash}/preview` returns a JSON envelope with metadata
//! and a rich visual preview payload for the formats the server can render —
//! image (base64 + dimensions), pdf (page text + SVG-ready geometry), pptx
//! (slide text + positioned visual shapes), xlsx (sheet grids + multi-sheet
//! array), docx (blocks + structured rich_blocks), md (text). The artifact
//! store is read only; ownership is gated exactly like the download endpoint,
//! so a foreign hash is a 404.

#[path = "preview_pdf.rs"]
mod preview_pdf;

use std::io::Cursor;
use std::panic::{AssertUnwindSafe, catch_unwind};

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;

use super::*;

/// Characters of text a preview payload carries at most.
pub(super) const MAX_PREVIEW_CHARS: usize = 8_000;
/// Maximum source artifact size for image/visual previews (20 MiB).
const PREVIEW_SOURCE_CAP_BYTES: usize = 20 * 1024 * 1024;
/// Rows/columns of a spreadsheet grid preview.
const GRID_MAX_ROWS: usize = 50;
const GRID_MAX_COLS: usize = 26;
/// Multi-sheet spreadsheet preview bounds.
const XLSX_MAX_SHEETS: usize = 12;
const XLSX_MAX_ROWS: usize = 120;
/// Blocks of a docx preview.
const DOCX_MAX_BLOCKS: usize = 50;
const DOCX_MAX_RICH_BLOCKS: usize = 150;
/// Visual PPTX preview bounds.
const PPTX_MAX_PREVIEW_SLIDES: usize = 50;
const PPTX_MAX_SHAPES_PER_SLIDE: usize = 60;
/// Embedded image bounds for PPTX/DOCX rich previews.
const MAX_EMBEDDED_IMAGE_BYTES: usize = 1_536 * 1024;
const MAX_TOTAL_EMBEDDED_IMAGE_BYTES: usize = 6 * 1024 * 1024;

#[derive(Debug, Serialize)]
struct PreviewPayload {
    /// Payload media type: `text/plain; charset=utf-8` or `application/json`.
    content_type: &'static str,
    /// `text` payloads arrive as a string, `json` payloads as a value.
    encoding: &'static str,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    truncated: bool,
    preview: Value,
}

/// `GET /v1/artifacts/{hash}/preview`: the acting user's own artifact only.
pub(super) async fn artifact_preview(
    State(state): State<Arc<ApiState>>,
    Extension(user): Extension<ActingUser>,
    Path(hash): Path<String>,
) -> Result<Json<Value>> {
    // Same gate as the download endpoint: another user's artifact is
    // indistinguishable from a missing one.
    if !state.store.user_owns_artifact(&user.id, &hash)? {
        return Err(SeraError::NotFound("artifact".into()));
    }
    let artifact = state.artifacts.load(&hash)?;
    let extension = std::path::Path::new(&artifact.filename)
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    let (kind, payload) = match extension.as_str() {
        "pdf" => (
            "pdf",
            pdf_preview(&artifact.bytes, &artifact.meta.title, &artifact.filename)?,
        ),
        "pptx" => ("pptx", pptx_preview(&artifact.bytes)?),
        "xlsx" | "xlsm" => ("xlsx", xlsx_preview(&artifact.bytes)?),
        "docx" => ("docx", docx_preview(&artifact.bytes)?),
        "md" | "markdown" => ("md", md_preview(&artifact.bytes)),
        "png" | "jpg" | "jpeg" | "webp" | "gif" | "bmp" | "svg" => {
            ("image", image_preview(&artifact.bytes, &extension)?)
        }
        _ => {
            return Err(SeraError::Validation(
                "no server preview for this artifact type".into(),
            ));
        }
    };
    let text = human_readable_text(kind, &payload);
    Ok(Json(json!({
        "hash": artifact.hash,
        "filename": artifact.filename,
        "title": artifact.meta.title,
        "kind": kind,
        "created_at": artifact.meta.created_at,
        "content_type": "text/plain; charset=utf-8",
        "text": text,
        "payload": payload,
    })))
}

fn text_payload(text: String, truncated: bool) -> PreviewPayload {
    PreviewPayload {
        content_type: "text/plain; charset=utf-8",
        encoding: "text",
        truncated,
        preview: Value::String(text),
    }
}

fn json_payload(value: Value, truncated: bool) -> PreviewPayload {
    PreviewPayload {
        content_type: "application/json",
        encoding: "json",
        truncated,
        preview: value,
    }
}

fn clip(text: &str, max: usize) -> (String, bool) {
    let mut chars = text.chars();
    let taken: String = chars.by_ref().take(max).collect();
    (taken, chars.next().is_some())
}

fn format_cell_for_text(value: &Value) -> String {
    match value {
        Value::Null => String::new(),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => n.to_string(),
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

fn human_readable_text(kind: &str, payload: &PreviewPayload) -> String {
    let raw = match &payload.preview {
        Value::String(text) => text.clone(),
        Value::Object(obj) => match kind {
            "image" => {
                let w = obj.get("width").and_then(Value::as_u64);
                let h = obj.get("height").and_then(Value::as_u64);
                match (w, h) {
                    (Some(w), Some(h)) => format!("Image ({w}×{h} px)"),
                    _ => {
                        let size = obj.get("size_bytes").and_then(Value::as_u64).unwrap_or(0);
                        format!("Image ({size} bytes)")
                    }
                }
            }
            "pdf" => obj
                .get("text")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned(),
            "xlsx" => {
                let Some(sheet) = obj.get("sheet").and_then(Value::as_str) else {
                    return "Empty workbook".to_owned();
                };
                let range = obj
                    .get("range")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .trim();
                let mut out = if range.is_empty() {
                    format!("Sheet: {sheet}\n")
                } else {
                    format!("Sheet: {sheet} ({range})\n")
                };
                if let Some(rows) = obj.get("rows").and_then(Value::as_array) {
                    let lines: Vec<String> = rows
                        .iter()
                        .filter_map(|row| {
                            let cells = row.as_array()?;
                            let mut strings: Vec<String> =
                                cells.iter().map(format_cell_for_text).collect();
                            while strings.last().is_some_and(|s| s.trim().is_empty()) {
                                strings.pop();
                            }
                            Some(strings.join(" | "))
                        })
                        .collect();
                    out.push_str(&lines.join("\n"));
                }
                out
            }
            "docx" => obj
                .get("blocks")
                .and_then(Value::as_array)
                .map(|blocks| {
                    blocks
                        .iter()
                        .filter_map(|b| {
                            b.as_str().or_else(|| b.get("text").and_then(Value::as_str))
                        })
                        .filter(|s| !s.trim().is_empty())
                        .collect::<Vec<_>>()
                        .join("\n\n")
                })
                .unwrap_or_default(),
            _ => obj
                .get("text")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned(),
        },
        _ => String::new(),
    };
    clip(&raw, MAX_PREVIEW_CHARS).0
}

/// Image: base64-encoded image bytes with detected dimensions.
fn image_preview(bytes: &[u8], extension: &str) -> Result<PreviewPayload> {
    if bytes.len() > PREVIEW_SOURCE_CAP_BYTES {
        return Err(SeraError::Validation(format!(
            "image exceeds {} MiB preview limit",
            PREVIEW_SOURCE_CAP_BYTES / (1024 * 1024)
        )));
    }
    let content_type = match extension {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "webp" => "image/webp",
        "gif" => "image/gif",
        "bmp" => "image/bmp",
        "svg" => "image/svg+xml",
        _ => "application/octet-stream",
    };
    let (width, height) = detect_image_dimensions(bytes, extension);
    let image_b64 = BASE64_STANDARD.encode(bytes);
    Ok(json_payload(
        json!({
            "content_type": content_type,
            "width": width,
            "height": height,
            "size_bytes": bytes.len(),
            "image_b64": image_b64,
        }),
        false,
    ))
}

fn detect_image_dimensions(bytes: &[u8], extension: &str) -> (Option<u32>, Option<u32>) {
    if extension == "svg" {
        return parse_svg_dimensions(bytes);
    }
    if let Ok(reader) = image::ImageReader::new(Cursor::new(bytes)).with_guessed_format()
        && let Ok((w, h)) = reader.into_dimensions()
    {
        return (Some(w), Some(h));
    }
    if extension == "gif"
        && bytes.len() >= 10
        && (&bytes[..6] == b"GIF87a" || &bytes[..6] == b"GIF89a")
    {
        let w = u16::from_le_bytes([bytes[6], bytes[7]]) as u32;
        let h = u16::from_le_bytes([bytes[8], bytes[9]]) as u32;
        return (Some(w), Some(h));
    }
    if extension == "bmp" && bytes.len() >= 26 && &bytes[..2] == b"BM" {
        let w = i32::from_le_bytes([bytes[18], bytes[19], bytes[20], bytes[21]]).unsigned_abs();
        let h = i32::from_le_bytes([bytes[22], bytes[23], bytes[24], bytes[25]]).unsigned_abs();
        return (Some(w), Some(h));
    }
    (None, None)
}

fn parse_svg_dimensions(bytes: &[u8]) -> (Option<u32>, Option<u32>) {
    let Ok(text) = std::str::from_utf8(bytes) else {
        return (None, None);
    };
    let head: String = text.chars().take(4096).collect();
    let lower = head.to_ascii_lowercase();
    if let Some(vb) = extract_xml_attr(&lower, "viewbox") {
        let nums: Vec<f64> = vb
            .split(|c: char| c.is_whitespace() || c == ',')
            .filter(|s| !s.is_empty())
            .filter_map(|s| s.parse::<f64>().ok())
            .collect();
        if nums.len() >= 4 && nums[2] > 0.0 && nums[3] > 0.0 {
            return (Some(nums[2].round() as u32), Some(nums[3].round() as u32));
        }
    }
    let w = extract_xml_attr(&lower, "width").and_then(|v| parse_svg_len(&v));
    let h = extract_xml_attr(&lower, "height").and_then(|v| parse_svg_len(&v));
    (w, h)
}

fn extract_xml_attr(xml: &str, name: &str) -> Option<String> {
    for quote in ['"', '\''] {
        let pat = format!("{name}={quote}");
        if let Some(pos) = xml.find(&pat) {
            let rest = &xml[pos + pat.len()..];
            if let Some(end) = rest.find(quote) {
                return Some(rest[..end].trim().to_owned());
            }
        }
    }
    None
}

fn parse_svg_len(val: &str) -> Option<u32> {
    let trimmed = val
        .trim()
        .trim_end_matches("px")
        .trim_end_matches("pt")
        .trim();
    if trimmed.ends_with('%') {
        return None;
    }
    trimmed
        .parse::<f64>()
        .ok()
        .filter(|v| *v > 0.0)
        .map(|v| v.round() as u32)
}

/// Best-effort fallback when `extract_pdf_text` yields no text: walk page
/// content streams with `lopdf` and extract printable literal strings from
/// `Tj` / `TJ` / `'` / `"` text-showing operators.
fn extract_pdf_literal_strings(bytes: &[u8]) -> Option<String> {
    let document = catch_unwind(AssertUnwindSafe(|| lopdf::Document::load_mem(bytes)))
        .ok()?
        .ok()?;
    if catch_unwind(AssertUnwindSafe(|| document.is_encrypted())).unwrap_or(true) {
        return None;
    }
    let pages = catch_unwind(AssertUnwindSafe(|| document.get_pages())).ok()?;
    let mut out = Vec::new();
    let mut total_chars = 0usize;

    let push_bytes = |raw: &[u8], page_parts: &mut Vec<String>, total: &mut usize| {
        if *total >= MAX_PREVIEW_CHARS {
            return;
        }
        let decoded = std::str::from_utf8(raw)
            .map(str::to_owned)
            .unwrap_or_else(|_| raw.iter().map(|&b| char::from(b)).collect());
        let cleaned: String = decoded
            .chars()
            .filter(|c| !c.is_control() || matches!(c, '\n' | '\t'))
            .collect();
        let trimmed = cleaned.trim();
        if !trimmed.is_empty() && trimmed.chars().any(|c| c.is_alphanumeric()) {
            *total += trimmed.chars().count() + 1;
            page_parts.push(trimmed.to_owned());
        }
    };

    for (_, page_id) in pages {
        if total_chars >= MAX_PREVIEW_CHARS {
            break;
        }
        let Ok(content_bytes) =
            catch_unwind(AssertUnwindSafe(|| document.get_page_content(page_id)))
                .unwrap_or(Err(lopdf::Error::ContentDecode))
        else {
            continue;
        };
        let Ok(content) = lopdf::content::Content::decode(&content_bytes) else {
            continue;
        };
        let mut page_parts = Vec::new();
        for op in content.operations {
            match op.operator.as_str() {
                "Tj" | "'" | "\"" => {
                    for operand in &op.operands {
                        if let lopdf::Object::String(raw, _) = operand {
                            push_bytes(raw, &mut page_parts, &mut total_chars);
                        }
                    }
                }
                "TJ" => {
                    if let Some(lopdf::Object::Array(items)) = op.operands.first() {
                        for item in items {
                            if let lopdf::Object::String(raw, _) = item {
                                push_bytes(raw, &mut page_parts, &mut total_chars);
                            }
                        }
                    }
                }
                _ => {}
            }
        }
        if !page_parts.is_empty() {
            out.push(page_parts.join(" "));
        }
    }
    let joined = out.join("\n");
    (!joined.trim().is_empty()).then_some(joined)
}

/// PDF: page count, bounded text, and SVG-ready page geometry (`pages`).
fn pdf_preview(bytes: &[u8], title: &str, filename: &str) -> Result<PreviewPayload> {
    let info = crate::pdf_ops::inspect(bytes)?;
    let mut text = crate::documents::extract_pdf_text(bytes.to_vec()).unwrap_or_default();
    if text.trim().is_empty()
        && let Some(literals) = extract_pdf_literal_strings(bytes)
    {
        text = literals;
    }
    if text.trim().is_empty() {
        let label = [title.trim(), filename.trim()]
            .into_iter()
            .find(|s| !s.is_empty())
            .unwrap_or("PDF document");
        text = format!(
            "{label}\nPDF document · {} page{}",
            info.page_count,
            if info.page_count == 1 { "" } else { "s" }
        );
    }
    let (text, truncated) = clip(&text, MAX_PREVIEW_CHARS);
    let pages = preview_pdf::extract_pdf_pages(bytes);
    Ok(json_payload(
        json!({"page_count": info.page_count, "text": text, "pages": pages}),
        truncated,
    ))
}

/// PPTX: slide text plus structured visual slide geometry & shapes.
#[cfg(feature = "pptx")]
fn pptx_preview(bytes: &[u8]) -> Result<PreviewPayload> {
    use deckcraft_model::{Background, Presentation, Shape, ShapeKind, resolve};

    let full_text = crate::pptx_ops::slide_plain_text(bytes, MAX_PREVIEW_CHARS)?;
    let (text, truncated) = clip(&full_text, MAX_PREVIEW_CHARS);

    let presentation: Presentation =
        catch_unwind(AssertUnwindSafe(|| deckcraft_pptx::import(bytes)))
            .map_err(|_| SeraError::Validation("malformed PPTX: importer aborted".into()))?
            .map_err(|err| SeraError::Validation(format!("cannot read PPTX: {err}")))?;

    let slide_count = presentation.slides.len();
    let slide_w =
        if presentation.slide_size.width.is_finite() && presentation.slide_size.width > 0.0 {
            presentation.slide_size.width
        } else {
            720.0
        };
    let slide_h =
        if presentation.slide_size.height.is_finite() && presentation.slide_size.height > 0.0 {
            presentation.slide_size.height
        } else {
            405.0
        };

    let mut total_image_bytes = 0usize;
    let mut slides = Vec::with_capacity(slide_count.min(PPTX_MAX_PREVIEW_SLIDES));

    for (idx, slide) in presentation
        .slides
        .iter()
        .take(PPTX_MAX_PREVIEW_SLIDES)
        .enumerate()
    {
        let ctx = resolve::Ctx::for_slide(&presentation, slide);
        let title = pptx_slide_title(slide);
        let slide_text = pptx_slide_text(slide);

        let background_hex = ctx.as_ref().and_then(|c| {
            let bg = slide
                .background
                .as_ref()
                .or_else(|| c.layout.and_then(|l| l.background.as_ref()))
                .or(c.master.background.as_ref())?;
            match bg {
                Background::Fill { fill } => pptx_fill_hex(c, fill, None),
                Background::Ref { color, .. } => Some(rgba_to_hex(c.color(color, None))),
            }
        });

        let mut flat_shapes: Vec<&Shape> = Vec::new();
        deckcraft_model::walk(&slide.shapes, &mut |shape, _| {
            if !shape.hidden {
                flat_shapes.push(shape);
            }
        });

        let mut shapes_json = Vec::new();
        for shape in flat_shapes {
            if shapes_json.len() >= PPTX_MAX_SHAPES_PER_SLIDE {
                break;
            }
            if matches!(shape.kind, ShapeKind::Group { .. }) {
                continue;
            }
            let xfrm = if let Some(c) = &ctx {
                resolve::xfrm(c, shape)
            } else {
                shape.xfrm.unwrap_or_default()
            };
            let fill_hex = ctx.as_ref().and_then(|c| {
                let (fill, ph) = resolve::fill(c, shape);
                fill.as_ref().and_then(|f| pptx_fill_hex(c, f, ph))
            });
            let stroke_hex = ctx.as_ref().and_then(|c| {
                let (line, ph) = resolve::line(c, shape);
                let width = line.width.unwrap_or(1.0);
                if width <= 0.0 {
                    return None;
                }
                line.fill.as_ref().and_then(|f| pptx_fill_hex(c, f, ph))
            });

            match &shape.kind {
                ShapeKind::Picture { fill } => {
                    let mut image_b64: Option<String> = None;
                    let mut image_mime: Option<String> = None;
                    if let Some(media) = presentation.media.iter().find(|m| m.id == fill.media)
                        && !media.data.is_empty()
                        && media.data.len() <= MAX_EMBEDDED_IMAGE_BYTES
                        && total_image_bytes + media.data.len() <= MAX_TOTAL_EMBEDDED_IMAGE_BYTES
                    {
                        total_image_bytes += media.data.len();
                        image_b64 = Some(BASE64_STANDARD.encode(media.data.as_slice()));
                        let mime = if !media.content_type.is_empty() {
                            media.content_type.clone()
                        } else {
                            crate::util::mime_for_filename(&media.name).to_owned()
                        };
                        image_mime = Some(mime);
                    }
                    shapes_json.push(json!({
                        "kind": "image",
                        "x_pt": round2_f64(xfrm.x),
                        "y_pt": round2_f64(xfrm.y),
                        "w_pt": round2_f64(xfrm.w),
                        "h_pt": round2_f64(xfrm.h),
                        "fill_hex": fill_hex,
                        "stroke_hex": stroke_hex,
                        "image_b64": image_b64,
                        "image_mime": image_mime,
                    }));
                }
                ShapeKind::Table(table) => {
                    let rows: Vec<Vec<String>> = table
                        .rows
                        .iter()
                        .take(40)
                        .map(|row| {
                            row.cells
                                .iter()
                                .take(20)
                                .map(|cell| cell.text.text().trim().to_owned())
                                .collect()
                        })
                        .collect();
                    shapes_json.push(json!({
                        "kind": "table",
                        "x_pt": round2_f64(xfrm.x),
                        "y_pt": round2_f64(xfrm.y),
                        "w_pt": round2_f64(xfrm.w),
                        "h_pt": round2_f64(xfrm.h),
                        "fill_hex": fill_hex,
                        "stroke_hex": stroke_hex,
                        "rows": rows,
                    }));
                }
                _ => {
                    let body_text = shape
                        .text
                        .as_ref()
                        .map(|t| t.text().trim().to_owned())
                        .unwrap_or_default();
                    if !body_text.is_empty() {
                        let (font_size, color_hex, bold, align) =
                            pptx_text_style(ctx.as_ref(), shape);
                        shapes_json.push(json!({
                            "kind": "text",
                            "x_pt": round2_f64(xfrm.x),
                            "y_pt": round2_f64(xfrm.y),
                            "w_pt": round2_f64(xfrm.w),
                            "h_pt": round2_f64(xfrm.h),
                            "fill_hex": fill_hex,
                            "stroke_hex": stroke_hex,
                            "text": body_text,
                            "font_size": font_size,
                            "color_hex": color_hex,
                            "bold": bold,
                            "align": align,
                        }));
                    } else if fill_hex.is_some() || stroke_hex.is_some() {
                        shapes_json.push(json!({
                            "kind": "rect",
                            "x_pt": round2_f64(xfrm.x),
                            "y_pt": round2_f64(xfrm.y),
                            "w_pt": round2_f64(xfrm.w),
                            "h_pt": round2_f64(xfrm.h),
                            "fill_hex": fill_hex,
                            "stroke_hex": stroke_hex,
                        }));
                    }
                }
            }
        }

        slides.push(json!({
            "slide": idx + 1,
            "title": title,
            "text": slide_text,
            "width_pt": round2_f64(slide_w),
            "height_pt": round2_f64(slide_h),
            "background_hex": background_hex,
            "shapes": shapes_json,
        }));
    }

    Ok(json_payload(
        json!({
            "slide_count": slide_count,
            "text": text,
            "slides": slides,
        }),
        truncated,
    ))
}

#[cfg(feature = "pptx")]
fn pptx_slide_title(slide: &deckcraft_model::Slide) -> String {
    let title = slide.title();
    let trimmed = title.trim();
    if !trimmed.is_empty() {
        return trimmed.to_owned();
    }
    let mut first_line = String::new();
    deckcraft_model::walk(&slide.shapes, &mut |shape, _| {
        if !first_line.is_empty() {
            return;
        }
        if let Some(body) = &shape.text {
            let text = body.text();
            if let Some(line) = text.lines().find(|l| !l.trim().is_empty()) {
                first_line = line.trim().to_owned();
            }
        }
    });
    first_line
}

#[cfg(feature = "pptx")]
fn pptx_slide_text(slide: &deckcraft_model::Slide) -> String {
    let mut parts = Vec::new();
    deckcraft_model::walk(&slide.shapes, &mut |shape, _| {
        if let Some(body) = &shape.text {
            let t = body.text();
            let trimmed = t.trim();
            if !trimmed.is_empty() {
                parts.push(trimmed.to_owned());
            }
        }
    });
    let notes = slide.notes.text();
    if !notes.trim().is_empty() {
        parts.push(format!("speaker notes:\n{}", notes.trim()));
    }
    parts.join("\n")
}

#[cfg(feature = "pptx")]
fn pptx_fill_hex(
    ctx: &deckcraft_model::resolve::Ctx<'_>,
    fill: &deckcraft_model::Fill,
    ph: Option<deckcraft_model::Rgba>,
) -> Option<String> {
    match fill {
        deckcraft_model::Fill::Solid { color } => Some(rgba_to_hex(ctx.color(color, ph))),
        deckcraft_model::Fill::Gradient(g) => g
            .stops
            .first()
            .map(|stop| rgba_to_hex(ctx.color(&stop.color, ph))),
        _ => None,
    }
}

#[cfg(feature = "pptx")]
fn pptx_text_style(
    ctx: Option<&deckcraft_model::resolve::Ctx<'_>>,
    shape: &deckcraft_model::Shape,
) -> (f64, String, bool, &'static str) {
    use deckcraft_model::text::Align;
    let Some(body) = &shape.text else {
        return (14.0, "#111111".to_owned(), false, "left");
    };
    let first_para = body.paragraphs.iter().find(|p| !p.text().trim().is_empty());
    let align = match first_para.and_then(|p| p.props.align) {
        Some(Align::Center) => "center",
        Some(Align::Right) => "right",
        Some(Align::Justify | Align::JustLow | Align::Distributed) => "justify",
        _ => "left",
    };
    let first_run = first_para.and_then(|p| p.runs.iter().find(|r| !r.text.trim().is_empty()));
    let mut font_size = first_run.and_then(|r| r.props.size).unwrap_or(14.0);
    let mut bold = first_run.and_then(|r| r.props.bold).unwrap_or(false);
    let mut color_hex = first_run
        .and_then(|r| r.props.fill.as_ref())
        .and_then(|f| ctx.and_then(|c| pptx_fill_hex(c, f, None)))
        .unwrap_or_else(|| "#111111".to_owned());

    if let Some(c) = ctx
        && first_run.is_some_and(|r| r.props.size.is_none() || r.props.fill.is_none())
    {
        let level = first_para.map(|p| p.level).unwrap_or(0);
        let style = deckcraft_model::resolve::level_style(c, shape, level);
        if first_run.and_then(|r| r.props.size).is_none()
            && let Some(sz) = style.run.size
        {
            font_size = sz;
        }
        if first_run.and_then(|r| r.props.bold).is_none()
            && let Some(b) = style.run.bold
        {
            bold = b;
        }
        if first_run.and_then(|r| r.props.fill.as_ref()).is_none()
            && let Some(f) = style.run.fill.as_ref()
            && let Some(hex) = pptx_fill_hex(c, f, None)
        {
            color_hex = hex;
        }
    }

    (round2_f64(font_size), color_hex, bold, align)
}

#[cfg(feature = "pptx")]
fn rgba_to_hex(c: deckcraft_model::Rgba) -> String {
    format!("#{:02X}{:02X}{:02X}", c.r, c.g, c.b)
}

fn round2_f64(v: f64) -> f64 {
    (v * 100.0).round() / 100.0
}

#[cfg(not(feature = "pptx"))]
fn pptx_preview(_bytes: &[u8]) -> Result<PreviewPayload> {
    Err(SeraError::Validation(
        "pptx support is not compiled in".into(),
    ))
}

/// XLSX: the first sheet's leading rows as a JSON grid plus multi-sheet `sheets`.
fn xlsx_preview(bytes: &[u8]) -> Result<PreviewPayload> {
    let workbook = crate::workbooks::import(bytes)?;
    let sheet_names = workbook.sheet_names();
    let Some(&first_sheet) = sheet_names.first() else {
        return Ok(json_payload(
            json!({"sheet": Value::Null, "rows": [], "sheets": [], "truncated": false}),
            false,
        ));
    };
    let read = match crate::workbooks::read_range(bytes, first_sheet) {
        Ok(read) => read,
        // A used range over the read cap still previews as bounded text.
        Err(_) => {
            let text = crate::workbooks::plain_text(bytes, MAX_PREVIEW_CHARS)?;
            let (text, truncated) = clip(&text, MAX_PREVIEW_CHARS);
            return Ok(text_payload(text, truncated));
        }
    };
    let (first_rows, first_truncated) = grid_from_range_read(&read, GRID_MAX_ROWS, GRID_MAX_COLS);

    let mut sheets = Vec::with_capacity(sheet_names.len().min(XLSX_MAX_SHEETS));
    for (idx, &sheet_name) in sheet_names.iter().take(XLSX_MAX_SHEETS).enumerate() {
        if idx == 0 {
            let (rows, trunc) = grid_from_range_read(&read, XLSX_MAX_ROWS, GRID_MAX_COLS);
            sheets.push(json!({
                "name": read.sheet,
                "range": read.range,
                "rows": rows,
                "truncated": trunc,
            }));
            continue;
        }
        let sheet_read = crate::workbooks::read_range(bytes, sheet_name).or_else(|_| {
            let escaped = sheet_name.replace('\'', "''");
            crate::workbooks::read_range(bytes, &format!("'{escaped}'!A1:Z75"))
        });
        match sheet_read {
            Ok(sr) => {
                let (rows, trunc) = grid_from_range_read(&sr, XLSX_MAX_ROWS, GRID_MAX_COLS);
                sheets.push(json!({
                    "name": sr.sheet,
                    "range": sr.range,
                    "rows": rows,
                    "truncated": trunc,
                }));
            }
            Err(_) => {
                sheets.push(json!({
                    "name": sheet_name,
                    "range": "",
                    "rows": [],
                    "truncated": true,
                }));
            }
        }
    }

    Ok(json_payload(
        json!({
            "sheet": read.sheet,
            "range": read.range,
            "rows": first_rows,
            "truncated": first_truncated,
            "sheets": sheets,
        }),
        first_truncated,
    ))
}

fn grid_from_range_read(
    read: &crate::workbooks::RangeRead,
    max_rows: usize,
    max_cols: usize,
) -> (Vec<Vec<Value>>, bool) {
    let mut rows: Vec<Vec<Value>> = Vec::new();
    let mut truncated = read.truncated_values > 0;
    for cell in &read.cells {
        let Some((col, row)) = a1_to_col_row(&cell.address) else {
            continue;
        };
        if row > max_rows || col > max_cols {
            truncated = true;
            continue;
        }
        while rows.len() < row {
            rows.push(Vec::new());
        }
        let target = &mut rows[row - 1];
        while target.len() < col {
            target.push(Value::Null);
        }
        target[col - 1] = cell
            .value
            .clone()
            .or_else(|| cell.cached.clone())
            .unwrap_or(Value::Null);
    }
    (rows, truncated)
}

/// `A1` → 1-based (column, row).
fn a1_to_col_row(address: &str) -> Option<(usize, usize)> {
    let mut col = 0usize;
    let mut row = 0usize;
    let mut seen_col = false;
    for c in address.chars() {
        if c.is_ascii_alphabetic() {
            col = col * 26 + (c.to_ascii_uppercase() as usize - 'A' as usize + 1);
            seen_col = true;
        } else if c.is_ascii_digit() {
            row = row * 10 + (c as usize - '0' as usize);
        } else {
            return None;
        }
    }
    (seen_col && row > 0).then_some((col, row))
}

/// DOCX: the first blocks, plus structured `rich_blocks`.
#[cfg(feature = "docx")]
fn docx_preview(bytes: &[u8]) -> Result<PreviewPayload> {
    let info = crate::docx_ops::inspect(bytes)?;
    if info.block_count == 0 {
        return Ok(json_payload(
            json!({"block_count": 0, "blocks": [], "rich_blocks": []}),
            false,
        ));
    }
    let last = (info.block_count - 1).min(DOCX_MAX_BLOCKS - 1);
    let text = crate::docx_ops::read_blocks(bytes, 0, last)?;
    let rich_blocks = extract_docx_rich_blocks(bytes);
    Ok(json_payload(
        json!({
            "block_count": text.block_count,
            "blocks": text.blocks,
            "rich_blocks": rich_blocks,
        }),
        info.block_count > DOCX_MAX_BLOCKS,
    ))
}

#[cfg(feature = "docx")]
fn extract_docx_rich_blocks(bytes: &[u8]) -> Vec<Value> {
    use wordcraft_doc::section::NumFormat;
    use wordcraft_doc::{Block, InlineObject};

    let Ok(Ok(document)) = catch_unwind(AssertUnwindSafe(|| wordcraft_docx::read(bytes))) else {
        return Vec::new();
    };

    let mut rich_blocks = Vec::new();
    let mut total_image_bytes = 0usize;

    for block in &document.body {
        if rich_blocks.len() >= DOCX_MAX_RICH_BLOCKS {
            break;
        }
        match block.as_ref() {
            Block::Para(paragraph) => {
                for obj in &paragraph.objects {
                    if rich_blocks.len() >= DOCX_MAX_RICH_BLOCKS {
                        break;
                    }
                    if let InlineObject::Image { media, alt, .. } = obj
                        && let Some(img_bytes) = document.media.get(media)
                        && !img_bytes.is_empty()
                        && img_bytes.len() <= MAX_EMBEDDED_IMAGE_BYTES
                        && total_image_bytes + img_bytes.len() <= MAX_TOTAL_EMBEDDED_IMAGE_BYTES
                    {
                        total_image_bytes += img_bytes.len();
                        rich_blocks.push(json!({
                            "kind": "image",
                            "content_type": crate::util::mime_for_filename(media),
                            "image_b64": BASE64_STANDARD.encode(img_bytes.as_slice()),
                            "alt": alt,
                        }));
                    }
                }
                if rich_blocks.len() >= DOCX_MAX_RICH_BLOCKS {
                    break;
                }
                let clean: String = paragraph
                    .plain_text()
                    .chars()
                    .filter(|&c| c != wordcraft_doc::para::OBJ)
                    .collect();
                let trimmed = clean.trim();
                if trimmed.is_empty() {
                    continue;
                }
                if let Some(level) = docx_heading_level(paragraph.props.style.as_deref()) {
                    rich_blocks.push(json!({
                        "kind": "heading",
                        "level": level,
                        "text": trimmed,
                    }));
                } else if let Some(num_ref) = &paragraph.props.numbering {
                    let ordered = document
                        .numbering
                        .nums
                        .iter()
                        .find(|n| n.id == num_ref.num)
                        .and_then(|n| {
                            n.level_overrides
                                .iter()
                                .find(|(lvl, _)| *lvl == num_ref.level)
                                .map(|(_, l)| l)
                                .or_else(|| {
                                    document
                                        .numbering
                                        .abstracts
                                        .iter()
                                        .find(|a| a.id == n.abstract_id)
                                        .and_then(|a| a.levels.get(num_ref.level as usize))
                                })
                        })
                        .is_some_and(|lvl| !matches!(lvl.format, NumFormat::Bullet));
                    rich_blocks.push(json!({
                        "kind": "list_item",
                        "ordered": ordered,
                        "text": trimmed,
                    }));
                } else {
                    let bold = !paragraph.runs.is_empty()
                        && paragraph.runs.iter().all(|r| r.props.bold == Some(true));
                    let italic = !paragraph.runs.is_empty()
                        && paragraph.runs.iter().all(|r| r.props.italic == Some(true));
                    rich_blocks.push(json!({
                        "kind": "paragraph",
                        "text": trimmed,
                        "bold": bold,
                        "italic": italic,
                    }));
                }
            }
            Block::Table(table) => {
                let rows: Vec<Vec<String>> = table
                    .rows
                    .iter()
                    .take(100)
                    .map(|row| {
                        row.cells
                            .iter()
                            .take(32)
                            .map(|cell| {
                                cell.blocks
                                    .iter()
                                    .filter_map(|b| b.as_para().map(|p| p.plain_text()))
                                    .map(|s| s.trim().to_owned())
                                    .filter(|s| !s.is_empty())
                                    .collect::<Vec<_>>()
                                    .join(" / ")
                            })
                            .collect()
                    })
                    .collect();
                rich_blocks.push(json!({
                    "kind": "table",
                    "rows": rows,
                }));
            }
        }
    }

    rich_blocks
}

#[cfg(feature = "docx")]
fn docx_heading_level(style: Option<&str>) -> Option<u8> {
    let s = style?.trim().to_ascii_lowercase();
    if s == "title" {
        return Some(1);
    }
    if s == "subtitle" {
        return Some(2);
    }
    let rest = s.strip_prefix("heading")?.trim();
    rest.parse::<u8>().ok().filter(|&n| (1..=6).contains(&n))
}

#[cfg(not(feature = "docx"))]
fn docx_preview(_bytes: &[u8]) -> Result<PreviewPayload> {
    Err(SeraError::Validation(
        "docx support is not compiled in".into(),
    ))
}

/// Markdown: the rendered text itself.
fn md_preview(bytes: &[u8]) -> PreviewPayload {
    let text = String::from_utf8_lossy(bytes);
    let (text, truncated) = clip(&text, MAX_PREVIEW_CHARS);
    text_payload(text, truncated)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gateway::{Gateway, ToolRegistry};
    use crate::llm::LlmClient;
    use crate::maker::ArtifactMeta;
    use crate::policy::Policy;
    use crate::runtime::NullNotifier;
    use tower::ServiceExt;

    fn build_pdf(content_stream: &[u8], pages_count: usize) -> Vec<u8> {
        use lopdf::{Dictionary, Document, Object, Stream};
        let mut document = Document::with_version("1.5");
        let pages_id = document.new_object_id();
        let mut font = Dictionary::new();
        font.set("Type", "Font");
        font.set("Subtype", "Type1");
        font.set("BaseFont", "Helvetica");
        let font_id = document.add_object(font);
        let mut fonts = Dictionary::new();
        fonts.set("F1", Object::Reference(font_id));
        let mut resources = Dictionary::new();
        resources.set("Font", fonts);

        let mut kids = Vec::new();
        for _ in 0..pages_count {
            let content_id =
                document.add_object(Stream::new(Dictionary::new(), content_stream.to_vec()));
            let mut page = Dictionary::new();
            page.set("Type", "Page");
            page.set("Parent", Object::Reference(pages_id));
            page.set("Contents", Object::Reference(content_id));
            page.set("Resources", resources.clone());
            let page_id = document.add_object(page);
            kids.push(Object::Reference(page_id));
        }

        let mut pages = Dictionary::new();
        pages.set("Type", "Pages");
        pages.set("Kids", Object::Array(kids));
        pages.set("Count", Object::Integer(pages_count as i64));
        pages.set(
            "MediaBox",
            Object::Array(vec![
                Object::Integer(0),
                Object::Integer(0),
                Object::Integer(200),
                Object::Integer(300),
            ]),
        );
        pages.set("Resources", resources);
        document.set_object(pages_id, pages);
        let mut catalog = Dictionary::new();
        catalog.set("Type", "Catalog");
        catalog.set("Pages", Object::Reference(pages_id));
        let catalog_id = document.add_object(catalog);
        document.trailer.set("Root", Object::Reference(catalog_id));
        let mut bytes = Vec::new();
        document.save_to(&mut bytes).unwrap();
        bytes
    }

    fn preview_test_app() -> (Router, Store, Arc<crate::maker::ArtifactStore>, String) {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.keep();
        let mut config = Config::default();
        config.api.enabled = true;
        config.api.token_hash = sha256_hex("test-token-that-is-long-enough-123");
        config.llm.model = "test".into();
        config.llm.base_url = "https://example.com/v1".into();
        config.base_dir = base.clone();
        let db_path = base.join("serabot.db");
        config.database.path = db_path.to_string_lossy().into_owned();
        let config = Arc::new(config);
        let store = Store::open(&db_path, 384, 5000).unwrap();
        let gateway = Gateway::new(
            store.clone(),
            Arc::new(Policy::empty()),
            ToolRegistry::default(),
            1,
            false,
        );
        let llm = LlmClient::new("https://example.com/v1", "test", 5, 50)
            .unwrap()
            .with_model("test");
        let runtime = Runtime::new(
            config.clone(),
            store.clone(),
            gateway,
            crate::model_router::ModelRouter::new(
                &config.llm,
                BTreeMap::from([("default".into(), llm)]),
            )
            .unwrap(),
            vec![crate::config::LoadedBot {
                config: BotConfig {
                    title: None,
                    description: None,
                    name: "main".into(),
                    trigger: "command".into(),
                    model: None,
                    system_prompt: "prompts/main.txt".into(),
                    skills: vec![],
                    tools: vec![],
                    require_approval: vec![],
                    enabled: true,
                    temperature: None,
                    thinking: None,
                    role: BotRole::Orchestrator,
                    delegates: vec![],
                },
                prompt: "test prompt".into(),
            }],
            Arc::new(NullNotifier),
            tokio_util::sync::CancellationToken::new(),
        )
        .unwrap();
        let artifacts = Arc::new(crate::maker::ArtifactStore::new(base.join("artifacts")).unwrap());
        let secrets =
            SharedSecrets::new(crate::secrets::SecretStore::from_map(BTreeMap::from([(
                config.llm.api_key_secret.clone(),
                "synthetic-llm".into(),
            )])));
        let users = Arc::new(UserSecrets::with_master_key(store.clone(), [8u8; 32]).unwrap());
        let state = ApiState::new(
            runtime,
            store.clone(),
            config,
            artifacts.clone(),
            secrets,
            users,
            None,
        )
        .unwrap();
        let user_id = store.admin_user_id().unwrap();
        store
            .create_conversation(&user_id, "dir:main", "direct", None)
            .unwrap();
        (crate::api::router(state), store, artifacts, user_id)
    }

    async fn fetch_preview(
        app: &Router,
        store: &Store,
        artifacts: &Arc<crate::maker::ArtifactStore>,
        user_id: &str,
        bytes: Vec<u8>,
        filename: &str,
        title: &str,
    ) -> Value {
        let meta = ArtifactMeta {
            title: title.to_owned(),
            filename: filename.to_owned(),
            language: crate::domain::Language::En,
            source_artifact: None,
            created_at: Utc::now(),
        };
        let stored = artifacts.save(&bytes, filename, meta).unwrap();
        store
            .record_conversation_artifact(
                user_id,
                "dir:main",
                None,
                "test_tool",
                &stored.hash,
                filename,
                None,
            )
            .unwrap();
        let token = store
            .mint_session(user_id, chrono::Duration::hours(1))
            .unwrap();
        let resp = app
            .clone()
            .oneshot(
                axum::http::Request::builder()
                    .method("GET")
                    .uri(format!("/v1/artifacts/{}/preview", stored.hash))
                    .header("authorization", format!("Bearer {token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK, "{filename}");
        let body = axum::body::to_bytes(resp.into_body(), 256 * 1024)
            .await
            .unwrap();
        serde_json::from_slice(&body).unwrap()
    }

    #[tokio::test]
    async fn preview_refuses_foreign_artifacts_and_path_like_hashes() {
        let (app, store, artifacts, owner) = preview_test_app();
        let meta = ArtifactMeta {
            title: "Private".into(),
            filename: "private.md".into(),
            language: crate::domain::Language::En,
            source_artifact: None,
            created_at: Utc::now(),
        };
        let stored = artifacts.save(b"# private", "private.md", meta).unwrap();
        store
            .record_conversation_artifact(
                &owner,
                "dir:main",
                None,
                "test_tool",
                &stored.hash,
                "private.md",
                None,
            )
            .unwrap();
        let other = store
            .create_user("subject-preview-b", "pb@example.example", "Preview B")
            .unwrap();
        let token = store
            .mint_session(&other.id, chrono::Duration::hours(1))
            .unwrap();
        let get = |uri: String| {
            let app = app.clone();
            let token = token.clone();
            async move {
                app.oneshot(
                    axum::http::Request::builder()
                        .method("GET")
                        .uri(uri)
                        .header("authorization", format!("Bearer {token}"))
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap()
                .status()
            }
        };
        // Another user's artifact looks missing; a traversal-shaped hash
        // never reaches the filesystem.
        assert_eq!(
            get(format!("/v1/artifacts/{}/preview", stored.hash)).await,
            StatusCode::NOT_FOUND
        );
        assert_ne!(
            get("/v1/artifacts/..%2F..%2Fserabot.db/preview".into()).await,
            StatusCode::OK
        );
    }

    #[test]
    fn pdf_preview_synthesizes_summary_when_pdf_has_no_text_layer() {
        // Pure vector path (like outline-rendered deck PDFs): no Tj/TJ text.
        let vector_pdf = build_pdf(b"0 0 100 100 re f", 2);
        let payload = pdf_preview(&vector_pdf, "Q4 Strategy Deck", "deck.pdf").unwrap();
        assert_eq!(payload.content_type, "application/json");
        assert_eq!(payload.preview["page_count"], 2);
        assert_eq!(
            payload.preview["text"],
            "Q4 Strategy Deck\nPDF document · 2 pages"
        );

        let one_page_pdf = build_pdf(b"0 0 50 50 re f", 1);
        let one_payload = pdf_preview(&one_page_pdf, "", "single.pdf").unwrap();
        assert_eq!(
            one_payload.preview["text"],
            "single.pdf\nPDF document · 1 page"
        );
    }

    #[tokio::test]
    async fn artifact_preview_includes_payload_and_top_level_content_type_and_text_for_all_types() {
        let (app, store, artifacts, user_id) = preview_test_app();

        // 1. PDF with text
        let pdf_text = build_pdf(b"BT /F1 12 Tf 20 100 Td (Hello PDF) Tj ET", 1);
        let pdf_json = fetch_preview(
            &app,
            &store,
            &artifacts,
            &user_id,
            pdf_text,
            "report.pdf",
            "Report",
        )
        .await;
        assert_eq!(pdf_json["kind"], "pdf");
        assert_eq!(pdf_json["content_type"], "text/plain; charset=utf-8");
        assert!(pdf_json["text"].as_str().unwrap().contains("Hello PDF"));
        assert!(
            pdf_json["payload"]["preview"]["text"]
                .as_str()
                .unwrap()
                .contains("Hello PDF")
        );

        // 2. PDF without text layer (vector/outline fallback)
        let pdf_vector = build_pdf(b"0 0 80 80 re S", 3);
        let pdf_vec_json = fetch_preview(
            &app,
            &store,
            &artifacts,
            &user_id,
            pdf_vector,
            "deck.pdf",
            "Outline Deck",
        )
        .await;
        assert_eq!(pdf_vec_json["kind"], "pdf");
        assert_eq!(pdf_vec_json["content_type"], "text/plain; charset=utf-8");
        assert_eq!(pdf_vec_json["text"], "Outline Deck\nPDF document · 3 pages");
        assert_eq!(
            pdf_vec_json["payload"]["preview"]["text"],
            "Outline Deck\nPDF document · 3 pages"
        );

        // 3. PPTX
        let pptx_bytes = {
            use crate::deck::{Brand, BrandPalette, DeckSection, DeckSpec};
            let brand = Brand {
                name: "Test".into(),
                palette: BrandPalette {
                    background: "#FFFFFF".into(),
                    foreground: "#111111".into(),
                    accent: "#2F6F6A".into(),
                    muted: "#6B7280".into(),
                },
                monogram_text: "T".into(),
                footer: None,
                font_family: Some("helvetica".into()),
            };
            let spec = DeckSpec {
                title: "Test Deck".into(),
                subtitle: None,
                language: crate::domain::Language::En,
                date_label: None,
                author: None,
                sections: vec![DeckSection {
                    part_title: None,
                    title: "Section One".into(),
                    cards: vec![],
                    bullets: vec!["Bullet one".into()],
                }],
                footer: None,
            };
            crate::deck_pptx::render_deck_pptx(&spec, &brand, 10)
                .unwrap()
                .bytes
        };
        let pptx_json = fetch_preview(
            &app,
            &store,
            &artifacts,
            &user_id,
            pptx_bytes,
            "slides.pptx",
            "Slides",
        )
        .await;
        assert_eq!(pptx_json["kind"], "pptx");
        assert_eq!(pptx_json["content_type"], "text/plain; charset=utf-8");
        assert!(
            pptx_json["text"]
                .as_str()
                .unwrap()
                .contains("--- slide 1 ---")
        );
        assert!(
            pptx_json["payload"]["preview"]["text"]
                .as_str()
                .unwrap()
                .contains("--- slide 1 ---")
        );
        assert_eq!(pptx_json["payload"]["preview"]["slide_count"], 3);
        assert!(pptx_json["payload"]["preview"]["slides"].is_array());

        // 4. XLSX
        let xlsx_bytes = {
            use crate::workbooks::{CellValue, SheetSpec, WorkbookSpec};
            crate::workbooks::build(&WorkbookSpec {
                title: "Sheet".into(),
                language: None,
                sheets: vec![SheetSpec {
                    name: "Data".into(),
                    header: vec!["item".into(), "qty".into()],
                    rows: vec![vec![
                        CellValue::Text("widgets".into()),
                        CellValue::Number(5.0),
                    ]],
                }],
            })
            .unwrap()
        };
        let xlsx_json = fetch_preview(
            &app,
            &store,
            &artifacts,
            &user_id,
            xlsx_bytes,
            "sheet.xlsx",
            "Sheet",
        )
        .await;
        assert_eq!(xlsx_json["kind"], "xlsx");
        assert_eq!(xlsx_json["content_type"], "text/plain; charset=utf-8");
        let xlsx_text = xlsx_json["text"].as_str().unwrap();
        assert!(xlsx_text.contains("Sheet: Data"), "{xlsx_text}");
        assert!(xlsx_text.contains("widgets | 5"), "{xlsx_text}");
        assert_eq!(xlsx_json["payload"]["preview"]["sheet"], "Data");

        // 5. DOCX
        let docx_bytes = {
            use crate::docx_ops::{DocxBlockSpec, DocxSpec};
            crate::docx_ops::build(&DocxSpec {
                title: "Doc".into(),
                subtitle: None,
                language: None,
                blocks: vec![DocxBlockSpec {
                    kind: "paragraph".into(),
                    level: None,
                    text: Some("Docx body text.".into()),
                    items: vec![],
                    header: vec![],
                    rows: vec![],
                }],
            })
            .unwrap()
        };
        let docx_json = fetch_preview(
            &app, &store, &artifacts, &user_id, docx_bytes, "doc.docx", "Doc",
        )
        .await;
        assert_eq!(docx_json["kind"], "docx");
        assert_eq!(docx_json["content_type"], "text/plain; charset=utf-8");
        assert!(
            docx_json["text"]
                .as_str()
                .unwrap()
                .contains("Docx body text.")
        );
        assert!(docx_json["payload"]["preview"]["blocks"].is_array());

        // 6. MD
        let md_json = fetch_preview(
            &app,
            &store,
            &artifacts,
            &user_id,
            b"# Markdown Heading\nBody".to_vec(),
            "notes.md",
            "Notes",
        )
        .await;
        assert_eq!(md_json["kind"], "md");
        assert_eq!(md_json["content_type"], "text/plain; charset=utf-8");
        assert_eq!(md_json["text"], "# Markdown Heading\nBody");
        assert_eq!(md_json["payload"]["preview"], "# Markdown Heading\nBody");
    }

    #[tokio::test]
    async fn rich_visual_preview_for_image_pdf_pptx_xlsx_and_docx() {
        let (app, store, artifacts, user_id) = preview_test_app();

        // 1. Image (PNG + SVG)
        let mut png_bytes = Vec::new();
        {
            let img = image::RgbaImage::from_pixel(32, 16, image::Rgba([47, 111, 106, 255]));
            img.write_to(&mut Cursor::new(&mut png_bytes), image::ImageFormat::Png)
                .unwrap();
        }
        let png_json = fetch_preview(
            &app,
            &store,
            &artifacts,
            &user_id,
            png_bytes.clone(),
            "chart.png",
            "Chart",
        )
        .await;
        assert_eq!(png_json["kind"], "image");
        assert_eq!(png_json["text"], "Image (32×16 px)");
        let png_prev = &png_json["payload"]["preview"];
        assert_eq!(png_prev["content_type"], "image/png");
        assert_eq!(png_prev["width"], 32);
        assert_eq!(png_prev["height"], 16);
        assert_eq!(png_prev["size_bytes"], png_bytes.len());
        assert_eq!(png_prev["image_b64"], BASE64_STANDARD.encode(&png_bytes));

        let svg_bytes = br##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 240 120"><rect width="240" height="120" fill="#2F6F6A"/></svg>"##.to_vec();
        let svg_json = fetch_preview(
            &app,
            &store,
            &artifacts,
            &user_id,
            svg_bytes,
            "diagram.svg",
            "Diagram",
        )
        .await;
        assert_eq!(svg_json["kind"], "image");
        assert_eq!(svg_json["text"], "Image (240×120 px)");
        assert_eq!(
            svg_json["payload"]["preview"]["content_type"],
            "image/svg+xml"
        );

        // 2. PDF visual geometry (MediaBox 200x300, background rect, colored rect, text block)
        let pdf_bytes = build_pdf(
            b"0.95 0.95 0.95 rg 0 0 200 300 re f 0.18 0.44 0.42 rg 10 200 100 40 re f BT /F1 14 Tf 1 0 0 1 20 260 Tm 0.07 0.07 0.07 rg (Visual Title) Tj ET",
            1,
        );
        let pdf_json = fetch_preview(
            &app,
            &store,
            &artifacts,
            &user_id,
            pdf_bytes,
            "visual.pdf",
            "Visual PDF",
        )
        .await;
        let pages = pdf_json["payload"]["preview"]["pages"].as_array().unwrap();
        assert_eq!(pages.len(), 1);
        let page0 = &pages[0];
        assert_eq!(page0["page"], 1);
        assert_eq!(page0["width_pt"], 200.0);
        assert_eq!(page0["height_pt"], 300.0);
        assert_eq!(page0["background_hex"], "#F2F2F2");
        let rects = page0["rects"].as_array().unwrap();
        assert!(rects.len() >= 2);
        // Second rect: pdf (10, 200, 100, 40) -> top-left y = 300 - 200 - 40 = 60.0
        assert_eq!(rects[1]["x_pt"], 10.0);
        assert_eq!(rects[1]["y_pt"], 60.0);
        assert_eq!(rects[1]["w_pt"], 100.0);
        assert_eq!(rects[1]["h_pt"], 40.0);
        assert_eq!(rects[1]["fill_hex"], "#2E706B");
        let text_blocks = page0["text_blocks"].as_array().unwrap();
        assert_eq!(text_blocks.len(), 1);
        assert_eq!(text_blocks[0]["text"], "Visual Title");
        assert_eq!(text_blocks[0]["x_pt"], 20.0);
        assert_eq!(text_blocks[0]["y_pt"], 40.0); // 300 - 260
        assert_eq!(text_blocks[0]["font_size"], 14.0);

        // 3. PPTX visual slides + shapes
        let pptx_bytes = {
            use crate::deck::{Brand, BrandPalette, DeckSection, DeckSpec};
            let brand = Brand {
                name: "Test".into(),
                palette: BrandPalette {
                    background: "#FFFFFF".into(),
                    foreground: "#111111".into(),
                    accent: "#2F6F6A".into(),
                    muted: "#6B7280".into(),
                },
                monogram_text: "T".into(),
                footer: None,
                font_family: Some("helvetica".into()),
            };
            let spec = DeckSpec {
                title: "Visual Deck".into(),
                subtitle: Some("Q4 Review".into()),
                language: crate::domain::Language::En,
                date_label: None,
                author: None,
                sections: vec![DeckSection {
                    part_title: None,
                    title: "Key Metrics".into(),
                    cards: vec![],
                    bullets: vec!["Revenue up 20%".into()],
                }],
                footer: None,
            };
            crate::deck_pptx::render_deck_pptx(&spec, &brand, 10)
                .unwrap()
                .bytes
        };
        let pptx_json = fetch_preview(
            &app,
            &store,
            &artifacts,
            &user_id,
            pptx_bytes,
            "visual.pptx",
            "Visual Deck",
        )
        .await;
        let pptx_prev = &pptx_json["payload"]["preview"];
        assert_eq!(pptx_prev["slide_count"], 3);
        let slides = pptx_prev["slides"].as_array().unwrap();
        assert_eq!(slides.len(), 3);
        assert_eq!(slides[0]["slide"], 1);
        assert_eq!(slides[0]["width_pt"], 960.0);
        assert_eq!(slides[0]["height_pt"], 540.0);
        assert_eq!(slides[0]["background_hex"], "#FFFFFF");
        let shapes0 = slides[0]["shapes"].as_array().unwrap();
        assert!(!shapes0.is_empty());
        assert!(shapes0.iter().any(|s| {
            s["kind"] == "text"
                && s["text"]
                    .as_str()
                    .is_some_and(|t| t.contains("Visual Deck"))
        }));

        // 4. XLSX multi-sheet
        let xlsx_bytes = {
            use crate::workbooks::{CellValue, SheetSpec, WorkbookSpec};
            crate::workbooks::build(&WorkbookSpec {
                title: "Multi".into(),
                language: None,
                sheets: vec![
                    SheetSpec {
                        name: "Summary".into(),
                        header: vec!["Metric".into(), "Value".into()],
                        rows: vec![vec![
                            CellValue::Text("ARR".into()),
                            CellValue::Number(100.0),
                        ]],
                    },
                    SheetSpec {
                        name: "Details".into(),
                        header: vec!["Region".into()],
                        rows: vec![vec![CellValue::Text("EMEA".into())]],
                    },
                ],
            })
            .unwrap()
        };
        let xlsx_json = fetch_preview(
            &app,
            &store,
            &artifacts,
            &user_id,
            xlsx_bytes,
            "multi.xlsx",
            "Multi",
        )
        .await;
        let sheets = xlsx_json["payload"]["preview"]["sheets"]
            .as_array()
            .unwrap();
        assert_eq!(sheets.len(), 2);
        assert_eq!(sheets[0]["name"], "Summary");
        assert_eq!(sheets[1]["name"], "Details");
        assert_eq!(sheets[1]["rows"][1][0], "EMEA");

        // 5. DOCX rich_blocks (heading, paragraph, list_item, table)
        let docx_bytes = {
            use crate::docx_ops::{DocxBlockSpec, DocxSpec};
            crate::docx_ops::build(&DocxSpec {
                title: "Rich Doc".into(),
                subtitle: None,
                language: None,
                blocks: vec![
                    DocxBlockSpec {
                        kind: "heading".into(),
                        level: Some(1),
                        text: Some("Overview".into()),
                        items: vec![],
                        header: vec![],
                        rows: vec![],
                    },
                    DocxBlockSpec {
                        kind: "paragraph".into(),
                        level: None,
                        text: Some("Body paragraph.".into()),
                        items: vec![],
                        header: vec![],
                        rows: vec![],
                    },
                    DocxBlockSpec {
                        kind: "bullets".into(),
                        level: None,
                        text: None,
                        items: vec!["First item".into()],
                        header: vec![],
                        rows: vec![],
                    },
                    DocxBlockSpec {
                        kind: "table".into(),
                        level: None,
                        text: None,
                        items: vec![],
                        header: vec!["Col A".into(), "Col B".into()],
                        rows: vec![vec!["1".into(), "2".into()]],
                    },
                ],
            })
            .unwrap()
        };
        let docx_json = fetch_preview(
            &app,
            &store,
            &artifacts,
            &user_id,
            docx_bytes,
            "rich.docx",
            "Rich Doc",
        )
        .await;
        let rich_blocks = docx_json["payload"]["preview"]["rich_blocks"]
            .as_array()
            .unwrap();
        assert!(
            rich_blocks
                .iter()
                .any(|b| b["kind"] == "heading" && b["text"] == "Overview")
        );
        assert!(
            rich_blocks
                .iter()
                .any(|b| b["kind"] == "paragraph" && b["text"] == "Body paragraph.")
        );
        assert!(rich_blocks.iter().any(|b| b["kind"] == "list_item"
            && b["ordered"] == false
            && b["text"] == "First item"));
        assert!(
            rich_blocks
                .iter()
                .any(|b| b["kind"] == "table" && b["rows"][0][0] == "Col A")
        );
    }
}
