//! Typed cards a tool result can carry beside its text.
//!
//! The model still reads [`crate::gateway::ToolOutput::content`]. Clients
//! render `cards` (on the tool output and on `turn.step` events) as audio,
//! file, image, link, and table cards. Card text is bounded and never holds
//! a secret: the gateway redacts vault values before the output is stored,
//! and the activity feed drops a card the content policy rejects.
//!
//! A tool may set [`ToolOutput::cards`] itself (ElevenLabs does). When it
//! does not, [`cards_from_output`] derives cards from the artifact fields and
//! from structured `data` (hits, results, links, tables).

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::gateway::ToolOutput;
use crate::util::truncate_chars;

/// Most cards one tool result may carry.
pub const MAX_CARDS: usize = 8;
const MAX_TEXT: usize = 160;
const MAX_TABLE_ROWS: usize = 12;
const MAX_TABLE_COLS: usize = 8;
const MAX_CELL: usize = 80;

/// One rich result a client can render without parsing the tool's prose.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ResultCard {
    Audio {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        url: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        voice: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        model: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        duration_ms: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        artifact_hash: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        filename: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        mime: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        title: Option<String>,
    },
    File {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        url: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        artifact_hash: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        filename: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        mime: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        bytes: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        title: Option<String>,
    },
    Image {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        url: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        artifact_hash: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        filename: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        mime: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        alt: Option<String>,
    },
    Link {
        url: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        title: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        description: Option<String>,
    },
    Table {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        title: Option<String>,
        columns: Vec<String>,
        rows: Vec<Vec<String>>,
    },
}

/// A saved media file, borrowed so ElevenLabs can build cards without this
/// module depending on the connector.
pub struct MediaRef<'a> {
    pub hash: &'a str,
    pub filename: &'a str,
    pub mime: &'a str,
    pub title: &'a str,
}

fn clip(text: &str) -> Option<String> {
    let cleaned: String = text
        .chars()
        .filter(|character| !character.is_control())
        .collect();
    let trimmed = cleaned.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(truncate_chars(trimmed, MAX_TEXT))
    }
}

fn sha256_hex(text: &str) -> bool {
    text.len() == 64 && text.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn artifact_url(hash: &str) -> Option<String> {
    sha256_hex(hash).then(|| format!("/v1/artifacts/{hash}"))
}

/// A public https URL with no userinfo and no IP host. Anything else is dropped.
fn https_url(raw: &str) -> Option<String> {
    let parsed = url::Url::parse(raw.trim()).ok()?;
    if parsed.scheme() != "https" || !parsed.username().is_empty() || parsed.password().is_some() {
        return None;
    }
    let host = parsed.host_str()?;
    if host.parse::<std::net::IpAddr>().is_ok() {
        return None;
    }
    clip(parsed.as_str())
}

fn label_arg(value: &Value, keys: &[&str]) -> Option<String> {
    for key in keys {
        let Some(text) = value.get(*key).and_then(Value::as_str) else {
            continue;
        };
        if let Some(clipped) = clip(text) {
            return Some(clipped);
        }
    }
    None
}

fn duration_ms(value: &Value) -> Option<u64> {
    if let Some(ms) = value.get("duration_ms").and_then(Value::as_u64) {
        return Some(ms.min(24 * 60 * 60 * 1000));
    }
    for key in ["duration_seconds", "duration"] {
        if let Some(seconds) = value.get(key).and_then(Value::as_f64)
            && seconds.is_finite()
            && (0.0..24.0 * 60.0 * 60.0).contains(&seconds)
        {
            return Some((seconds * 1000.0) as u64);
        }
    }
    None
}

fn extension_of(filename: &str) -> Option<&str> {
    let file = filename.rsplit(['/', '\\']).next()?;
    let (_, ext) = file.rsplit_once('.')?;
    let ext = ext.trim();
    if ext.is_empty() || ext.len() > 8 || !ext.chars().all(|c| c.is_ascii_alphanumeric()) {
        None
    } else {
        Some(ext)
    }
}

fn is_audio(mime: &str, filename: &str) -> bool {
    mime.starts_with("audio/")
        || matches!(
            extension_of(filename)
                .map(|ext| ext.to_ascii_lowercase())
                .as_deref(),
            Some("mp3" | "wav" | "ogg" | "m4a" | "flac")
        )
}

fn is_image(mime: &str, filename: &str) -> bool {
    mime.starts_with("image/")
        || matches!(
            extension_of(filename)
                .map(|ext| ext.to_ascii_lowercase())
                .as_deref(),
            Some("png" | "jpg" | "jpeg" | "gif" | "webp" | "svg")
        )
}

fn hash_of(card: &ResultCard) -> Option<&str> {
    match card {
        ResultCard::Audio { artifact_hash, .. }
        | ResultCard::File { artifact_hash, .. }
        | ResultCard::Image { artifact_hash, .. } => artifact_hash.as_deref(),
        _ => None,
    }
}

fn link_of(card: &ResultCard) -> Option<&str> {
    match card {
        ResultCard::Link { url, .. } => Some(url.as_str()),
        _ => None,
    }
}

/// One card for a saved file. Audio and images get their own card type.
#[allow(clippy::too_many_arguments)]
pub fn card_for_file(
    hash: &str,
    filename: Option<&str>,
    mime: Option<&str>,
    title: Option<&str>,
    bytes: Option<u64>,
    voice: Option<String>,
    model: Option<String>,
    duration: Option<u64>,
) -> Option<ResultCard> {
    if !sha256_hex(hash) {
        return None;
    }
    let filename = filename.and_then(clip);
    let mime = mime.and_then(clip);
    let title = title.and_then(clip);
    let url = artifact_url(hash);
    let name = filename.as_deref().unwrap_or("");
    let mime_str = mime.as_deref().unwrap_or("");
    Some(if is_audio(mime_str, name) {
        ResultCard::Audio {
            url,
            voice,
            model,
            duration_ms: duration,
            artifact_hash: Some(hash.to_owned()),
            filename,
            mime,
            title,
        }
    } else if is_image(mime_str, name) {
        ResultCard::Image {
            url,
            artifact_hash: Some(hash.to_owned()),
            filename,
            mime,
            alt: title,
        }
    } else {
        ResultCard::File {
            url,
            artifact_hash: Some(hash.to_owned()),
            filename,
            mime,
            bytes,
            title,
        }
    })
}

/// A card for a generation that has not saved a file yet. No artifact hash,
/// so clients show a placeholder instead of a download.
pub fn pending_media_card(kind: &str, arguments: &Value) -> ResultCard {
    let voice = label_arg(arguments, &["voice_name", "voice", "voice_id"]);
    let model = label_arg(arguments, &["model_id", "model"]);
    let duration = duration_ms(arguments);
    let title = Some(format!("Generating {kind}"));
    match kind {
        "audio" => ResultCard::Audio {
            url: None,
            voice,
            model,
            duration_ms: duration,
            artifact_hash: None,
            filename: None,
            mime: Some("audio/mpeg".into()),
            title,
        },
        "image" => ResultCard::Image {
            url: None,
            artifact_hash: None,
            filename: None,
            mime: Some("image/png".into()),
            alt: title,
        },
        _ => ResultCard::File {
            url: None,
            artifact_hash: None,
            filename: None,
            mime: None,
            bytes: None,
            title,
        },
    }
}

/// Cards for an ElevenLabs (or similar) generation: voice, model and duration
/// come from the call arguments, never from the provider's free-form text.
pub fn cards_for_media(arguments: &Value, media: &[MediaRef<'_>]) -> Vec<ResultCard> {
    let voice = label_arg(arguments, &["voice_name", "voice", "voice_id"]);
    let model = label_arg(arguments, &["model_id", "model"]);
    let duration = duration_ms(arguments);
    let mut cards = Vec::new();
    for item in media {
        if let Some(card) = card_for_file(
            item.hash,
            Some(item.filename),
            Some(item.mime),
            clip(item.title).as_deref(),
            None,
            voice.clone(),
            model.clone(),
            duration,
        ) {
            cards.push(card);
        }
        if cards.len() == MAX_CARDS {
            break;
        }
    }
    cards
}

fn push_link(
    cards: &mut Vec<ResultCard>,
    url: &str,
    title: Option<&str>,
    description: Option<&str>,
) {
    if cards.len() >= MAX_CARDS {
        return;
    }
    let Some(url) = https_url(url) else {
        return;
    };
    if cards.iter().any(|card| link_of(card) == Some(url.as_str())) {
        return;
    }
    cards.push(ResultCard::Link {
        url,
        title: title.and_then(clip),
        description: description.and_then(clip),
    });
}

fn links_from_array(cards: &mut Vec<ResultCard>, items: &Vec<Value>) {
    for item in items {
        let Some(object) = item.as_object() else {
            continue;
        };
        let Some(url) = object.get("url").and_then(Value::as_str) else {
            continue;
        };
        push_link(
            cards,
            url,
            object
                .get("title")
                .or_else(|| object.get("name"))
                .and_then(Value::as_str),
            object
                .get("description")
                .or_else(|| object.get("snippet"))
                .and_then(Value::as_str),
        );
    }
}

fn table_from(value: &Value) -> Option<ResultCard> {
    let columns = value.get("columns")?.as_array()?;
    let rows = value.get("rows")?.as_array()?;
    let columns: Vec<String> = columns
        .iter()
        .filter_map(Value::as_str)
        .filter_map(clip)
        .take(MAX_TABLE_COLS)
        .collect();
    if columns.is_empty() {
        return None;
    }
    let width = columns.len();
    let rows = rows
        .iter()
        .filter_map(Value::as_array)
        .take(MAX_TABLE_ROWS)
        .map(|row| {
            row.iter()
                .take(width)
                .map(|cell| {
                    cell.as_str()
                        .and_then(clip)
                        .map(|text| truncate_chars(&text, MAX_CELL))
                        .unwrap_or_default()
                })
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>();
    if rows.is_empty() {
        return None;
    }
    Some(ResultCard::Table {
        title: value.get("title").and_then(Value::as_str).and_then(clip),
        columns,
        rows,
    })
}

fn artifacts_from(cards: &mut Vec<ResultCard>, data: &Value) {
    let Some(items) = data.get("artifacts").and_then(Value::as_array) else {
        return;
    };
    for item in items {
        let Some(hash) = item.get("artifact_hash").and_then(Value::as_str) else {
            continue;
        };
        if cards.iter().any(|card| hash_of(card) == Some(hash)) {
            continue;
        }
        let bytes = item.get("bytes").and_then(Value::as_u64);
        if let Some(card) = card_for_file(
            hash,
            item.get("filename").and_then(Value::as_str),
            item.get("mime")
                .or_else(|| item.get("content_mime_type"))
                .and_then(Value::as_str),
            item.get("title").and_then(Value::as_str),
            bytes,
            None,
            None,
            None,
        ) {
            cards.push(card);
        }
        if cards.len() == MAX_CARDS {
            return;
        }
    }
}

/// Explicit cards plus cards derived from the artifact and structured data.
/// Deduped by artifact hash and link URL, capped at [`MAX_CARDS`].
pub fn cards_from_output(output: &ToolOutput) -> Vec<ResultCard> {
    let mut cards = output.cards.clone();
    cards.truncate(MAX_CARDS);
    if let Some(hash) = output.artifact_hash.as_deref()
        && cards.iter().all(|card| hash_of(card) != Some(hash))
        && let Some(card) = card_for_file(
            hash,
            output.filename.as_deref(),
            None,
            None,
            None,
            None,
            None,
            None,
        )
    {
        cards.push(card);
    }
    if let Some(data) = output.data.as_ref() {
        artifacts_from(&mut cards, data);
        if let Some(url) = data.get("url").and_then(Value::as_str) {
            push_link(
                &mut cards,
                url,
                data.get("title").and_then(Value::as_str),
                None,
            );
        }
        for key in ["hits", "results", "links"] {
            if let Some(items) = data.get(key).and_then(Value::as_array) {
                links_from_array(&mut cards, items);
            }
        }
        if cards
            .iter()
            .all(|card| !matches!(card, ResultCard::Table { .. }))
            && cards.len() < MAX_CARDS
        {
            let source = data.get("table").unwrap_or(data);
            if let Some(table) = table_from(source) {
                cards.push(table);
            }
        }
    }
    cards.truncate(MAX_CARDS);
    cards
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn audio_card_carries_voice_model_and_duration_without_the_prompt() {
        let cards = cards_for_media(
            &json!({
                "text": "secret prompt the owner typed",
                "voice_id": "rachel",
                "model_id": "eleven_flash",
                "duration_seconds": 2.5
            }),
            &[MediaRef {
                hash: &"ab".repeat(32),
                filename: "line.mp3",
                mime: "audio/mpeg",
                title: "Line",
            }],
        );
        let ResultCard::Audio {
            url,
            voice,
            model,
            duration_ms,
            artifact_hash,
            filename,
            ..
        } = &cards[0]
        else {
            panic!("expected an audio card");
        };
        assert_eq!(voice.as_deref(), Some("rachel"));
        assert_eq!(model.as_deref(), Some("eleven_flash"));
        assert_eq!(*duration_ms, Some(2500));
        assert_eq!(filename.as_deref(), Some("line.mp3"));
        assert_eq!(artifact_hash.as_deref(), Some("ab".repeat(32).as_str()));
        assert_eq!(
            url.as_deref(),
            Some(format!("/v1/artifacts/{}", "ab".repeat(32)).as_str())
        );
        let encoded = serde_json::to_string(&cards[0]).unwrap();
        assert!(!encoded.contains("secret prompt"));
        assert!(encoded.contains("\"type\":\"audio\""));
    }

    #[test]
    fn derived_cards_cover_files_links_and_tables_and_drop_unsafe_urls() {
        let output = ToolOutput {
            content: "done".into(),
            artifact_hash: Some("cd".repeat(32)),
            filename: Some("chart.png".into()),
            data: Some(json!({
                "hits": [
                    {"url": "https://example.com/a", "title": "A"},
                    {"url": "http://example.com/insecure", "title": "no"},
                    {"url": "https://user:pass@example.com/x", "title": "no"}
                ],
                "columns": ["Name", "Qty"],
                "rows": [["Bolt", "2"], ["Washer", "4"]]
            })),
            ..ToolOutput::default()
        };
        let cards = cards_from_output(&output);
        assert!(
            cards
                .iter()
                .any(|card| matches!(card, ResultCard::Image { .. }))
        );
        let links: Vec<_> = cards
            .iter()
            .filter_map(|card| match card {
                ResultCard::Link { url, .. } => Some(url.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(links, vec!["https://example.com/a"]);
        assert!(
            cards
                .iter()
                .any(|card| matches!(card, ResultCard::Table { .. }))
        );
    }
}
