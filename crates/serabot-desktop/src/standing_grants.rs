//! Standing grants list (0.30.0, B5) from `GET /v1/standing-grants`.

use serde_json::Value;

const MAX_GRANTS: usize = 200;
const MAX_ID: usize = 64;
const MAX_TOOL: usize = 128;
const MAX_HASH: usize = 128;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StandingGrantRow {
    pub id: String,
    pub tool: String,
    pub canonical_hash: String,
    pub destination: Option<String>,
    pub created_at: String,
    pub active: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct StandingGrantsSnapshot {
    pub grants: Vec<StandingGrantRow>,
    pub supported: bool,
}

fn clean(value: &Value, key: &str, max: usize) -> Option<String> {
    let text: String = value
        .get(key)?
        .as_str()?
        .chars()
        .filter(|c| !c.is_control())
        .take(max)
        .collect();
    let text = text.trim().to_owned();
    (!text.is_empty()).then_some(text)
}

pub fn parse_standing_grants(body: &Value) -> Option<StandingGrantsSnapshot> {
    let rows = body.get("grants")?.as_array()?;
    let mut grants = Vec::new();
    for row in rows.iter().take(MAX_GRANTS) {
        let Some(id) = clean(row, "id", MAX_ID) else {
            continue;
        };
        let Some(tool) = clean(row, "tool", MAX_TOOL) else {
            continue;
        };
        let canonical_hash = clean(row, "canonical_hash", MAX_HASH).unwrap_or_default();
        let active = row.get("active").and_then(|v| v.as_bool()).unwrap_or(false);
        grants.push(StandingGrantRow {
            id,
            tool,
            canonical_hash,
            destination: clean(row, "destination", 256),
            created_at: clean(row, "created_at", 64).unwrap_or_default(),
            active,
        });
    }
    Some(StandingGrantsSnapshot {
        grants,
        supported: true,
    })
}

/// Active standing grants in the snapshot.
pub fn active_grants(snapshot: &StandingGrantsSnapshot) -> Vec<&StandingGrantRow> {
    snapshot.grants.iter().filter(|g| g.active).collect()
}

/// Whether `grant` matches a user filter query across tool, destination, hash, or id.
pub fn grant_matches(grant: &StandingGrantRow, query: &str) -> bool {
    let needle = query.trim().to_lowercase();
    if needle.is_empty() {
        return true;
    }
    grant.tool.to_lowercase().contains(&needle)
        || grant
            .destination
            .as_deref()
            .is_some_and(|dest| dest.to_lowercase().contains(&needle))
        || grant.canonical_hash.to_lowercase().contains(&needle)
        || grant.id.to_lowercase().contains(&needle)
}

/// Human-readable destination label for a standing grant.
pub fn grant_destination_label(grant: &StandingGrantRow) -> &str {
    grant.destination.as_deref().unwrap_or("Any destination")
}

/// Shorten a canonical hash to 12 characters with an ellipsis for compact UI chips.
pub fn short_hash(hash: &str) -> String {
    let chars: String = hash.chars().take(12).collect();
    if hash.chars().count() > 12 {
        format!("{chars}\u{2026}")
    } else {
        chars
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parses_valid_and_skips_incomplete_rows() {
        let body = json!({
            "grants": [
                {
                    "id": "sg-1",
                    "tool": "web.search",
                    "canonical_hash": "abcdef1234567890",
                    "destination": "api.example.com",
                    "created_at": "2026-11-01T10:00:00Z",
                    "active": true
                },
                {
                    "id": "sg-2",
                    "tool": "file.read",
                    "canonical_hash": "deadbeef",
                    "active": false
                },
                {
                    "id": "",
                    "tool": "bad.row",
                    "active": true
                }
            ]
        });
        let snap = parse_standing_grants(&body).unwrap();
        assert!(snap.supported);
        assert_eq!(snap.grants.len(), 2);
        assert_eq!(active_grants(&snap).len(), 1);
        assert_eq!(grant_destination_label(&snap.grants[0]), "api.example.com");
        assert_eq!(grant_destination_label(&snap.grants[1]), "Any destination");
    }

    #[test]
    fn grant_matches_searches_tool_destination_hash_and_id() {
        let grant = StandingGrantRow {
            id: "sg-42".into(),
            tool: "outlook.send".into(),
            canonical_hash: "cafe1234deadbeef".into(),
            destination: Some("team@acme.example".into()),
            created_at: String::new(),
            active: true,
        };
        assert!(grant_matches(&grant, ""));
        assert!(grant_matches(&grant, "OUTLOOK"));
        assert!(grant_matches(&grant, "acme"));
        assert!(grant_matches(&grant, "cafe1234"));
        assert!(grant_matches(&grant, "sg-42"));
        assert!(!grant_matches(&grant, "slack"));
    }

    #[test]
    fn short_hash_truncates_beyond_twelve_chars() {
        assert_eq!(short_hash("abcdef123456"), "abcdef123456");
        assert_eq!(short_hash("abcdef1234567890"), "abcdef123456\u{2026}");
    }
}
