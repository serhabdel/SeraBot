//! Server-rendered artifact previews (0.28.0): `GET /v1/artifacts/{hash}/preview`
//! answers `{content_type, text?, image_b64?}` for the formats the server can
//! render (pdf page 1 raster, pptx slide 1, xlsx first rows, docx first blocks,
//! md text). Clients code against that shape and fall back to the download
//! route when the endpoint is missing (404) or the payload is malformed.
//!
//! Payloads are untrusted: text is bounded and control characters dropped,
//! and the base64 image is validated and size-capped before it becomes a
//! data URL.

use base64::Engine;
use serde_json::Value;

/// File extensions the preview pane offers for.
pub const PREVIEWABLE_EXTENSIONS: [&str; 5] = ["pdf", "pptx", "xlsx", "docx", "md"];

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
}

impl PreviewKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::Pdf => "PDF page 1",
            Self::Slide => "Slide 1",
            Self::Sheet => "First rows",
            Self::Doc => "First blocks",
            Self::Markdown => "Markdown",
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
        "xlsx" => Some(PreviewKind::Sheet),
        "docx" => Some(PreviewKind::Doc),
        "md" => Some(PreviewKind::Markdown),
        _ => None,
    }
}

/// One parsed preview response.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ArtifactPreview {
    pub content_type: String,
    pub text: Option<String>,
    pub image_b64: Option<String>,
}

/// What the pane can draw from a preview.
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

/// Format a cell value from an XLSX preview row into a display string.
fn format_cell(cell: &Value) -> String {
    match cell {
        Value::Null => String::new(),
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        Value::Bool(b) => b.to_string(),
        other => other.to_string(),
    }
}

/// Format the `payload.preview` field of a server envelope into display text.
fn format_envelope_preview(value: &Value, payload: &Value) -> Option<String> {
    let preview = payload.get("preview")?;
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
                    .unwrap_or("");
                if body.is_empty() {
                    Some(format!(
                        "{header}\n\nSlide/page graphics rendered as vector outlines. Click Open to view the full PDF."
                    ))
                } else {
                    Some(format!("{header}\n\n{body}"))
                }
            } else if kind == "xlsx" || obj.contains_key("rows") {
                let sheet = obj
                    .get("sheet")
                    .and_then(Value::as_str)
                    .filter(|s| !s.trim().is_empty())
                    .unwrap_or("Sheet1");
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
            } else if kind == "docx" || obj.contains_key("blocks") {
                let blocks = obj.get("blocks").and_then(Value::as_array)?;
                let parts: Vec<&str> = blocks
                    .iter()
                    .filter_map(|b| b.as_str().or_else(|| b.get("text").and_then(Value::as_str)))
                    .filter(|s| !s.trim().is_empty())
                    .collect();
                Some(parts.join("\n\n"))
            } else {
                obj.get("text")
                    .and_then(Value::as_str)
                    .map(ToOwned::to_owned)
            }
        }
        _ => None,
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

impl ArtifactPreview {
    /// Parse and validate both the flat `{content_type, text?, image_b64?}`
    /// shape and the server envelope `{kind, title, filename, payload: {...}}`;
    /// returns `None` when malformed.
    pub fn parse(value: &Value) -> Option<Self> {
        let payload = value.get("payload").filter(|p| p.is_object());

        // A present-but-invalid image field rejects the whole payload: the
        // caller must fall back to download rather than show half a preview.
        let image_val = value
            .get("image_b64")
            .filter(|v| !v.is_null())
            .or_else(|| payload.and_then(|p| p.get("image_b64")));
        let image_b64 = parse_image_b64(image_val).ok()?;

        if let Some(p) = payload {
            let formatted = format_envelope_preview(value, p).or_else(|| {
                value
                    .get("text")
                    .and_then(Value::as_str)
                    .or_else(|| p.get("text").and_then(Value::as_str))
                    .map(ToOwned::to_owned)
            });

            let truncated = p.get("truncated").and_then(Value::as_bool).unwrap_or(false);
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
                let raw_ct = value
                    .get("content_type")
                    .and_then(Value::as_str)
                    .or_else(|| p.get("content_type").and_then(Value::as_str))
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
                    .or_else(|| p.get("content_type").and_then(Value::as_str))?;
                let ct = sanitize_content_type(raw_ct);
                if ct.trim().is_empty() {
                    return None;
                }
                ct
            };

            return Some(Self {
                content_type,
                text,
                image_b64,
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
        Some(Self {
            content_type,
            text,
            image_b64,
        })
    }

    /// What the pane should draw. An image wins when the server sent a valid
    /// one; otherwise text; `None` means "fall back to download/open".
    pub fn render(&self) -> Option<PreviewContent> {
        if let Some(b64) = &self.image_b64 {
            let bytes = base64::engine::general_purpose::STANDARD
                .decode(b64.as_bytes())
                .ok()?;
            if bytes.is_empty() || bytes.len() > MAX_IMAGE_B64 {
                return None;
            }
            let mime = match self.content_type.trim().to_ascii_lowercase().as_str() {
                "image/png" => "image/png",
                "image/jpeg" | "image/jpg" => "image/jpeg",
                "image/webp" => "image/webp",
                "image/gif" => "image/gif",
                _ => "image/png",
            };
            return Some(PreviewContent::Image(format!("data:{mime};base64,{b64}")));
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
    fn previewable_names_cover_the_five_formats() {
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
        assert_eq!(preview_kind_for_name("photo.png"), None);
        assert_eq!(preview_kind_for_name(".pdf"), None);
        assert_eq!(preview_kind_for_name("noextension"), None);
        assert_eq!(preview_kind_for_name(""), None);
        for kind in [
            PreviewKind::Pdf,
            PreviewKind::Slide,
            PreviewKind::Sheet,
            PreviewKind::Doc,
            PreviewKind::Markdown,
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
        // An untrusted or active image MIME (e.g. svg+xml) falls back to png.
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
}
