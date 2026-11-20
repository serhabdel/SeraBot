//! Memory review (0.70) for the console: blocking calls on a fresh client,
//! run on a short worker thread by the UI (same pattern as `ops_view`), plus
//! the small helpers the card and its tests share.

use serde_json::Value;

use crate::api::{ApiClient, valid_memory_id};

fn client(credentials: &crate::worker::Credentials) -> Result<ApiClient, String> {
    ApiClient::new(credentials.endpoint.clone(), credentials.token.clone())
        .map_err(|error| error.to_string())
}

/// `GET /v1/memory/review`.
pub fn get_review(credentials: &crate::worker::Credentials) -> Result<Value, String> {
    client(credentials)?
        .memory_review_get()
        .map_err(|error| error.to_string())
}

/// `POST /v1/memory/review/{id}` with `keep`, `delete` or `merge_into:<id>`.
pub fn act(
    credentials: &crate::worker::Credentials,
    id: &str,
    action: &str,
) -> Result<Value, String> {
    client(credentials)?
        .memory_review_act(id, action)
        .map_err(|error| error.to_string())
}

/// `PATCH /v1/memories/{id}`: the Edit button.
pub fn edit(
    credentials: &crate::worker::Credentials,
    id: &str,
    content: &str,
) -> Result<(), String> {
    client(credentials)?
        .update_memory(id, content)
        .map(|_| ())
        .map_err(|error| error.to_string())
}

/// One card row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReviewRow {
    pub id: String,
    pub content: String,
    pub reason: String,
    pub reason_label: &'static str,
    pub detail: String,
    /// For duplicates: the memory a merge keeps, and a snippet of it.
    pub merge_target: Option<(String, String)>,
}

pub fn reason_label(reason: &str) -> &'static str {
    match reason {
        "contradicted" => "May contradict",
        "duplicate" => "Duplicate",
        "stale" => "Not checked lately",
        _ => "Review",
    }
}

/// The rows of a `GET /v1/memory/review` reply; rows without a usable id or
/// text are skipped.
pub fn rows(reply: &Value) -> Vec<ReviewRow> {
    reply["candidates"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|item| {
            let id = item["id"].as_str()?;
            let content = item["content"].as_str()?;
            if !valid_memory_id(id) || content.is_empty() {
                return None;
            }
            let reason = item["reason"].as_str().unwrap_or_default().to_string();
            let merge_target = match (
                item["similar_to"].as_str(),
                item["similar_content"].as_str(),
            ) {
                (Some(target), Some(text)) if valid_memory_id(target) => {
                    Some((target.to_string(), text.to_string()))
                }
                _ => None,
            };
            Some(ReviewRow {
                id: id.to_string(),
                content: content.to_string(),
                reason_label: reason_label(&reason),
                reason,
                detail: item["detail"].as_str().unwrap_or_default().to_string(),
                merge_target,
            })
        })
        .collect()
}

/// The card's one-line summary.
pub fn summary(count: usize) -> String {
    match count {
        0 => "Nothing to check right now.".into(),
        1 => "1 memory worth a look.".into(),
        n => format!("{n} memories worth a look."),
    }
}

/// The action string for a merge.
pub fn merge_action(target: &str) -> String {
    format!("merge_into:{target}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn rows_keep_valid_candidates_and_merge_targets() {
        let reply = json!({"candidates": [
            {"id": "m1", "content": "Alice likes tea", "reason": "duplicate",
             "detail": "Another memory says almost the same thing.",
             "similar_to": "m2", "similar_content": "Alice likes green tea"},
            {"id": "bad id", "content": "skipped", "reason": "stale"},
            {"id": "m3", "content": "", "reason": "stale"},
            {"id": "m4", "content": "Old fact", "reason": "stale", "detail": "Not checked for 90 days."},
        ]});
        let found = rows(&reply);
        assert_eq!(found.len(), 2);
        assert_eq!(found[0].reason_label, "Duplicate");
        assert_eq!(
            found[0].merge_target,
            Some(("m2".into(), "Alice likes green tea".into()))
        );
        assert_eq!(found[1].merge_target, None);
        assert_eq!(found[1].reason_label, "Not checked lately");
        assert!(rows(&json!({})).is_empty());
    }

    #[test]
    fn summary_and_merge_action_read_plainly() {
        assert_eq!(summary(0), "Nothing to check right now.");
        assert_eq!(summary(1), "1 memory worth a look.");
        assert_eq!(summary(7), "7 memories worth a look.");
        assert_eq!(merge_action("m2"), "merge_into:m2");
        assert_eq!(reason_label("contradicted"), "May contradict");
        assert_eq!(reason_label("other"), "Review");
    }
}
