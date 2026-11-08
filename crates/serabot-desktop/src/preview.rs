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
    let ext = lower.rsplit('.').next()?;
    if ext == lower {
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

impl ArtifactPreview {
    /// Parse and validate the documented shape; `None` when malformed.
    pub fn parse(value: &Value) -> Option<Self> {
        let content_type = value
            .get("content_type")?
            .as_str()?
            .chars()
            .filter(|c| !c.is_control())
            .take(MAX_CONTENT_TYPE)
            .collect::<String>();
        if content_type.trim().is_empty() {
            return None;
        }
        let text = value.get("text").and_then(Value::as_str).map(|raw| {
            raw.chars()
                .filter(|c| !c.is_control() || matches!(c, '\n' | '\t'))
                .take(MAX_PREVIEW_TEXT)
                .collect::<String>()
        });
        // A present-but-invalid image field rejects the whole payload: the
        // caller must fall back to download rather than show half a preview.
        let image_b64 = match value.get("image_b64") {
            None => None,
            Some(Value::Null) => None,
            Some(raw) => {
                let raw = raw.as_str()?;
                let trimmed = raw.trim();
                if trimmed.is_empty() || trimmed.chars().count() > MAX_IMAGE_B64 {
                    return None;
                }
                if !trimmed
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '/' | '='))
                {
                    return None;
                }
                Some(trimmed.to_owned())
            }
        };
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
            let mime = if self.content_type.starts_with("image/") {
                self.content_type.trim()
            } else {
                "image/png"
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
}
