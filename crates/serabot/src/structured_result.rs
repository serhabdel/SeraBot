//! Structured tool result envelope for clients (audio players, file cards, tables).
//!
//! Every connector still returns human-readable `content`; when absent, the
//! gateway fills `structured` from known fields so older clients keep working.

use serde_json::{Value, json};

use crate::gateway::ToolOutput;

/// Stable `kind` values for the structured envelope.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResultKind {
    Audio,
    Image,
    File,
    Table,
    LinkList,
    Text,
}

impl ResultKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Audio => "audio",
            Self::Image => "image",
            Self::File => "file",
            Self::Table => "table",
            Self::LinkList => "link_list",
            Self::Text => "text",
        }
    }
}

fn mime_from_name(name: &str) -> Option<String> {
    let ext = name.rsplit('.').next()?.to_ascii_lowercase();
    Some(
        match ext.as_str() {
            "mp3" => "audio/mpeg",
            "wav" => "audio/wav",
            "ogg" => "audio/ogg",
            "m4a" => "audio/mp4",
            "flac" => "audio/flac",
            "png" => "image/png",
            "jpg" | "jpeg" => "image/jpeg",
            "gif" => "image/gif",
            "webp" => "image/webp",
            "pdf" => "application/pdf",
            "docx" => "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
            "xlsx" => "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
            "pptx" => "application/vnd.openxmlformats-officedocument.presentationml.presentation",
            "csv" => "text/csv",
            _ => return None,
        }
        .into(),
    )
}

fn preview_text(text: &str, max: usize) -> String {
    let trimmed = text.trim();
    if trimmed.chars().count() <= max {
        return trimmed.to_string();
    }
    let mut out = String::new();
    for ch in trimmed.chars().take(max.saturating_sub(1)) {
        out.push(ch);
    }
    out.push('…');
    out
}

/// Build or merge the structured envelope on a finished tool output.
pub fn attach_structured(tool: &str, output: &mut ToolOutput) {
    if output
        .data
        .as_ref()
        .and_then(|d| d.get("structured"))
        .filter(|v| !v.is_null())
        .is_some()
    {
        return;
    }
    let inferred = infer_structured(tool, output);
    if let Some(data) = output.data.as_mut() {
        if let Some(map) = data.as_object_mut() {
            map.entry("structured").or_insert(inferred);
        }
    } else {
        output.data = Some(json!({ "structured": inferred }));
    }
}

pub fn infer_structured(tool: &str, output: &ToolOutput) -> Value {
    if let Some(data) = output.data.as_ref()
        && let Some(existing) = data.get("structured").filter(|v| !v.is_null())
    {
        return existing.clone();
    }
    let data = output.data.as_ref();
    if let Some(url) = data
        .and_then(|d| d.get("audio_url").or_else(|| d.get("url")))
        .and_then(Value::as_str)
        .filter(|u| u.starts_with("http"))
    {
        let mut envelope = json!({
            "kind": ResultKind::Audio.as_str(),
            "url": url,
        });
        for key in ["voice", "voice_id", "model", "model_id"] {
            if let Some(v) = data.and_then(|d| d.get(key)).filter(|v| v.is_string()) {
                envelope[key] = v.clone();
            }
        }
        if let Some(ms) = data
            .and_then(|d| d.get("duration_ms"))
            .filter(|v| v.is_number())
        {
            envelope["duration_ms"] = ms.clone();
        }
        if let Some(hash) = output.artifact_hash.as_deref() {
            envelope["artifact_hash"] = json!(hash);
        }
        return envelope;
    }
    if let Some(url) = data
        .and_then(|d| d.get("image_url").or_else(|| d.get("preview_url")))
        .and_then(Value::as_str)
        .filter(|u| u.starts_with("http"))
    {
        let mut envelope = json!({
            "kind": ResultKind::Image.as_str(),
            "url": url,
        });
        if let Some(hash) = output.artifact_hash.as_deref() {
            envelope["artifact_hash"] = json!(hash);
        }
        return envelope;
    }
    if let Some(links) = data
        .and_then(|d| d.get("links"))
        .and_then(Value::as_array)
        .filter(|a| !a.is_empty())
    {
        return json!({
            "kind": ResultKind::LinkList.as_str(),
            "links": links,
        });
    }
    if tool.starts_with("xlsx_") || tool == "timesheet_analyze" {
        return json!({
            "kind": ResultKind::Table.as_str(),
            "preview": preview_text(&output.content, 400),
        });
    }
    if let Some(hash) = output.artifact_hash.as_deref() {
        let filename = output.filename.as_deref().unwrap_or_default();
        let mut envelope = json!({
            "kind": ResultKind::File.as_str(),
            "artifact_hash": hash,
            "filename": filename,
        });
        if let Some(mime) = mime_from_name(filename) {
            envelope["mime_type"] = json!(mime);
        }
        return envelope;
    }
    if tool.starts_with("elevenlabs_") {
        return json!({
            "kind": ResultKind::Audio.as_str(),
            "preview": preview_text(&output.content, 400),
        });
    }
    let _ = tool;
    json!({
        "kind": ResultKind::Text.as_str(),
        "preview": preview_text(&output.content, 400),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gateway::ToolOutput;

    #[test]
    fn file_envelope_uses_artifact_hash_and_mime() {
        let output = ToolOutput {
            content: "Saved".into(),
            artifact_hash: Some("a".repeat(64)),
            filename: Some("deck.pdf".into()),
            ..ToolOutput::default()
        };
        let v = infer_structured("deck_build", &output);
        assert_eq!(v["kind"], "file");
        assert_eq!(v["mime_type"], "application/pdf");
    }

    #[test]
    fn attach_structured_merges_into_data() {
        let mut output = ToolOutput {
            content: "ok".into(),
            data: Some(json!({"rows": 1})),
            ..ToolOutput::default()
        };
        attach_structured("xlsx_read", &mut output);
        assert!(output.data.as_ref().unwrap().get("structured").is_some());
    }
}
