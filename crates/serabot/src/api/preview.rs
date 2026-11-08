//! Server-rendered artifact previews (0.28.0, phase C3):
//! `GET /v1/artifacts/{hash}/preview` returns a JSON envelope with metadata
//! and a preview payload for the formats the server can render — pdf (page
//! text), pptx (slide text), xlsx (first rows as a JSON grid), docx (first
//! blocks), md (text). The artifact store is read only; ownership is gated
//! exactly like the download endpoint, so a foreign hash is a 404.

use super::*;

/// Characters of text a preview payload carries at most.
pub(super) const MAX_PREVIEW_CHARS: usize = 8_000;
/// Rows/columns of a spreadsheet grid preview.
const GRID_MAX_ROWS: usize = 50;
const GRID_MAX_COLS: usize = 26;
/// Blocks of a docx preview.
const DOCX_MAX_BLOCKS: usize = 50;

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
        "pdf" => ("pdf", pdf_preview(&artifact.bytes)?),
        "pptx" => ("pptx", pptx_preview(&artifact.bytes)?),
        "xlsx" | "xlsm" => ("xlsx", xlsx_preview(&artifact.bytes)?),
        "docx" => ("docx", docx_preview(&artifact.bytes)?),
        "md" | "markdown" => ("md", md_preview(&artifact.bytes)),
        _ => {
            return Err(SeraError::Validation(
                "no server preview for this artifact type".into(),
            ));
        }
    };
    Ok(Json(json!({
        "hash": artifact.hash,
        "filename": artifact.filename,
        "title": artifact.meta.title,
        "kind": kind,
        "created_at": artifact.meta.created_at,
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

/// PDF: page count from the structural inspect, page-1-first text bounded to
/// [`MAX_PREVIEW_CHARS`]. (No rasterizer is linked, so the text form is the
/// preview the server can produce.)
fn pdf_preview(bytes: &[u8]) -> Result<PreviewPayload> {
    let info = crate::pdf_ops::inspect(bytes)?;
    // A scanned/image-only PDF has no text layer; `inspect` already proved the
    // structure parses, so the preview degrades to the page count alone.
    let text = crate::documents::extract_pdf_text(bytes.to_vec()).unwrap_or_default();
    let (text, truncated) = clip(&text, MAX_PREVIEW_CHARS);
    Ok(json_payload(
        json!({"page_count": info.page_count, "text": text}),
        truncated,
    ))
}

/// PPTX: slide text starting at slide 1, bounded.
#[cfg(feature = "pptx")]
fn pptx_preview(bytes: &[u8]) -> Result<PreviewPayload> {
    let text = crate::pptx_ops::slide_plain_text(bytes, MAX_PREVIEW_CHARS)?;
    let (text, truncated) = clip(&text, MAX_PREVIEW_CHARS);
    Ok(text_payload(text, truncated))
}

#[cfg(not(feature = "pptx"))]
fn pptx_preview(_bytes: &[u8]) -> Result<PreviewPayload> {
    Err(SeraError::Validation(
        "pptx support is not compiled in".into(),
    ))
}

/// XLSX: the first sheet's leading rows as a JSON grid.
fn xlsx_preview(bytes: &[u8]) -> Result<PreviewPayload> {
    let workbook = crate::workbooks::import(bytes)?;
    let Some(sheet) = workbook.sheet_names().first().copied() else {
        return Ok(json_payload(
            json!({"sheet": Value::Null, "rows": [], "truncated": false}),
            false,
        ));
    };
    let read = match crate::workbooks::read_range(bytes, sheet) {
        Ok(read) => read,
        // A used range over the read cap still previews as bounded text.
        Err(_) => {
            let text = crate::workbooks::plain_text(bytes, MAX_PREVIEW_CHARS)?;
            let (text, truncated) = clip(&text, MAX_PREVIEW_CHARS);
            return Ok(text_payload(text, truncated));
        }
    };
    let mut rows: Vec<Vec<Value>> = Vec::new();
    let mut truncated = read.truncated_values > 0;
    for cell in &read.cells {
        let Some((col, row)) = a1_to_col_row(&cell.address) else {
            continue;
        };
        if row > GRID_MAX_ROWS || col > GRID_MAX_COLS {
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
    Ok(json_payload(
        json!({"sheet": read.sheet, "range": read.range, "rows": rows, "truncated": truncated}),
        truncated,
    ))
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

/// DOCX: the first blocks, structured.
#[cfg(feature = "docx")]
fn docx_preview(bytes: &[u8]) -> Result<PreviewPayload> {
    let info = crate::docx_ops::inspect(bytes)?;
    if info.block_count == 0 {
        return Ok(json_payload(json!({"block_count": 0, "blocks": []}), false));
    }
    let last = (info.block_count - 1).min(DOCX_MAX_BLOCKS - 1);
    let text = crate::docx_ops::read_blocks(bytes, 0, last)?;
    Ok(json_payload(
        json!({"block_count": text.block_count, "blocks": text.blocks}),
        info.block_count > DOCX_MAX_BLOCKS,
    ))
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
