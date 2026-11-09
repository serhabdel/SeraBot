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
