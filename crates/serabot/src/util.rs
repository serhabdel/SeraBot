use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::error::{Result, SeraError};

pub fn sha256_hex(bytes: impl AsRef<[u8]>) -> String {
    hex::encode(Sha256::digest(bytes.as_ref()))
}

/// Content type for a stored artifact, derived from its filename.
///
/// Every delivery path used to hardcode `application/pdf`, which mislabelled
/// Word, Excel, and PowerPoint files in Telegram, OneDrive, and the owner API.
pub fn mime_for_filename(filename: &str) -> &'static str {
    let extension = std::path::Path::new(filename)
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    match extension.as_str() {
        "pdf" => "application/pdf",
        "docx" => "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
        "xlsx" => "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
        "pptx" => "application/vnd.openxmlformats-officedocument.presentationml.presentation",
        "csv" => "text/csv",
        "txt" | "md" | "markdown" => "text/plain; charset=utf-8",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "mp3" => "audio/mpeg",
        "wav" => "audio/wav",
        "mp4" => "video/mp4",
        _ => "application/octet-stream",
    }
}

/// Produce a stable representation for hashing approval-bound arguments.
///
/// Object keys are sorted recursively. JSON numbers and strings retain their
/// exact serde representation, so callers must normalize semantic values
/// (absolute paths, timestamps) before binding them to an approval.
pub fn canonical_json(value: &Value) -> Result<String> {
    fn write(value: &Value, out: &mut String) -> Result<()> {
        match value {
            Value::Null => out.push_str("null"),
            Value::Bool(v) => out.push_str(if *v { "true" } else { "false" }),
            Value::Number(v) => out.push_str(&v.to_string()),
            Value::String(v) => {
                out.push_str(&serde_json::to_string(v)?);
            }
            Value::Array(values) => {
                out.push('[');
                for (index, value) in values.iter().enumerate() {
                    if index > 0 {
                        out.push(',');
                    }
                    write(value, out)?;
                }
                out.push(']');
            }
            Value::Object(map) => {
                let mut keys: Vec<&String> = map.keys().collect();
                keys.sort();
                out.push('{');
                for (index, key) in keys.into_iter().enumerate() {
                    if index > 0 {
                        out.push(',');
                    }
                    out.push_str(&serde_json::to_string(key)?);
                    out.push(':');
                    write(&map[key], out)?;
                }
                out.push('}');
            }
        }
        Ok(())
    }

    let mut output = String::new();
    write(value, &mut output)?;
    Ok(output)
}

pub fn canonical_hash(value: &Value) -> Result<String> {
    Ok(sha256_hex(canonical_json(value)?))
}

pub fn constant_time_eq(left: &str, right: &str) -> bool {
    use subtle::ConstantTimeEq;
    left.as_bytes().ct_eq(right.as_bytes()).into()
}

pub fn validate_id(value: &str) -> Result<()> {
    if value.len() > 128
        || value.is_empty()
        || !value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | ':'))
    {
        return Err(SeraError::Validation("invalid identifier".into()));
    }
    Ok(())
}

pub fn truncate_chars(value: &str, max: usize) -> String {
    value.chars().take(max).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mime_matches_the_extension() {
        assert_eq!(mime_for_filename("a.pdf"), "application/pdf");
        assert_eq!(
            mime_for_filename("A.DOCX"),
            "application/vnd.openxmlformats-officedocument.wordprocessingml.document"
        );
        assert_eq!(
            mime_for_filename("b.xlsx"),
            "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet"
        );
        assert_eq!(
            mime_for_filename("c.pptx"),
            "application/vnd.openxmlformats-officedocument.presentationml.presentation"
        );
        assert_eq!(mime_for_filename("d.csv"), "text/csv");
        assert_eq!(mime_for_filename("e.unknown"), "application/octet-stream");
        assert_eq!(
            mime_for_filename("no-extension"),
            "application/octet-stream"
        );
    }
}
