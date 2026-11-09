//! The Workflows tab model (0.29.0, Wave A4): parsing responses from
//! `GET /v1/routines`, `GET /v1/monitoring/sources`, and `GET /v1/workflows/active`.
//!
//! Each endpoint is optional on older servers (404 means unsupported for that
//! panel). Strings are bounded and control characters dropped before display.

use serde_json::Value;

const MAX_NAME: usize = 120;
const MAX_TIME: usize = 64;
const MAX_GOAL: usize = 500;
const MAX_ERROR: usize = 300;
const MAX_URL: usize = 512;
const MAX_KIND: usize = 64;
const MAX_CONVERSATION: usize = 128;
const MAX_ROUTINES: usize = 64;
const MAX_SOURCES: usize = 200;
const MAX_ACTIVE: usize = 200;

/// One scheduled routine row from `GET /v1/routines`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Routine {
    pub name: String,
    pub enabled: bool,
    pub next_run_at: String,
    pub last_run_for: Option<String>,
    /// Daily time (`HH:MM`) when present in server config.
    pub time: Option<String>,
    pub timezone: Option<String>,
}

/// Last run snapshot nested under a monitoring source.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SourceLastRun {
    pub ok: bool,
    pub error: Option<String>,
    pub items_seen: i64,
    pub started_at: Option<String>,
    pub finished_at: Option<String>,
}

/// One configured watcher source from `GET /v1/monitoring/sources`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MonitoringSource {
    pub name: String,
    pub kind: String,
    pub enabled: bool,
    pub url: Option<String>,
    pub last_run: Option<SourceLastRun>,
}

/// One incomplete multi-step plan from `GET /v1/workflows/active`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ActiveWorkflow {
    pub conversation_id: String,
    pub goal: String,
    pub done: usize,
    pub total: usize,
    pub updated_at: Option<String>,
}

/// All three Wave A reads in one worker round-trip.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct WorkflowsSnapshot {
    pub routines: Vec<Routine>,
    pub sources: Vec<MonitoringSource>,
    pub active: Vec<ActiveWorkflow>,
    pub routines_supported: bool,
    pub sources_supported: bool,
    pub active_supported: bool,
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

fn opt_str(value: &Value, key: &str, max: usize) -> Option<String> {
    value.get(key).and_then(|v| {
        if v.is_null() {
            None
        } else {
            clean(value, key, max)
        }
    })
}

fn parse_routine(row: &Value) -> Option<Routine> {
    let name = clean(row, "name", MAX_NAME)?;
    let enabled = row.get("enabled")?.as_bool()?;
    let next_run_at = clean(row, "next_run_at", MAX_TIME).unwrap_or_default();
    let config = row.get("config").unwrap_or(&Value::Null);
    Some(Routine {
        name,
        enabled,
        next_run_at,
        last_run_for: opt_str(row, "last_run_for", MAX_TIME),
        time: config
            .get("time")
            .and_then(|v| v.as_str())
            .map(|s| s.chars().take(16).collect()),
        timezone: config
            .get("timezone")
            .or_else(|| config.get("tz"))
            .and_then(|v| v.as_str())
            .map(|s| s.chars().take(64).collect()),
    })
}

fn parse_last_run(row: &Value) -> Option<SourceLastRun> {
    let ok = row.get("ok")?.as_bool()?;
    let items_seen = row.get("items_seen").and_then(|v| v.as_i64()).unwrap_or(0);
    Some(SourceLastRun {
        ok,
        error: opt_str(row, "error", MAX_ERROR),
        items_seen,
        started_at: opt_str(row, "started_at", MAX_TIME),
        finished_at: opt_str(row, "finished_at", MAX_TIME),
    })
}

fn parse_source(row: &Value) -> Option<MonitoringSource> {
    let name = clean(row, "name", MAX_NAME)?;
    let kind = clean(row, "kind", MAX_KIND).unwrap_or_else(|| "unknown".into());
    let enabled = row.get("enabled").and_then(|v| v.as_bool()).unwrap_or(true);
    let last_run = row.get("last_run").and_then(parse_last_run);
    Some(MonitoringSource {
        name,
        kind,
        enabled,
        url: opt_str(row, "url", MAX_URL),
        last_run,
    })
}

fn parse_active(row: &Value) -> Option<ActiveWorkflow> {
    let conversation_id = clean(row, "conversation_id", MAX_CONVERSATION)?;
    let goal = clean(row, "goal", MAX_GOAL).unwrap_or_else(|| "(no goal)".into());
    let done = row.get("done").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
    let total = row.get("total").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
    Some(ActiveWorkflow {
        conversation_id,
        goal,
        done,
        total,
        updated_at: opt_str(row, "updated_at", MAX_TIME),
    })
}

pub fn parse_routines(value: &Value) -> Option<Vec<Routine>> {
    let rows = value.get("routines")?.as_array()?;
    if rows.len() > MAX_ROUTINES {
        return None;
    }
    Some(rows.iter().filter_map(parse_routine).collect())
}

pub fn parse_sources(value: &Value) -> Option<Vec<MonitoringSource>> {
    let rows = value.get("sources")?.as_array()?;
    if rows.len() > MAX_SOURCES {
        return None;
    }
    Some(rows.iter().filter_map(parse_source).collect())
}

pub fn parse_active_workflows(value: &Value) -> Option<Vec<ActiveWorkflow>> {
    let rows = value.get("workflows")?.as_array()?;
    if rows.len() > MAX_ACTIVE {
        return None;
    }
    Some(rows.iter().filter_map(parse_active).collect())
}

/// Short label for the next run timestamp (shown as-is after trim).
pub fn format_next_run(raw: &str) -> String {
    let flat: String = raw
        .chars()
        .filter(|c| !c.is_control())
        .take(MAX_TIME)
        .collect();
    let flat = flat.trim();
    if flat.is_empty() {
        "—".into()
    } else {
        flat.to_owned()
    }
}

/// Progress text for an active plan row.
pub fn plan_progress(done: usize, total: usize) -> String {
    if total == 0 {
        "0 steps".into()
    } else {
        format!("{done}/{total}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parses_documented_routine_shape() {
        let rows = parse_routines(&json!({
            "routines": [{
                "name": "morning_brief",
                "enabled": true,
                "next_run_at": "2026-11-10T06:30:00Z",
                "last_run_for": "2026-11-09",
                "config": { "time": "07:30", "timezone": "Europe/Rome" }
            }]
        }))
        .unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].time.as_deref(), Some("07:30"));
    }

    #[test]
    fn parses_monitoring_and_active_shapes() {
        let sources = parse_sources(&json!({
            "sources": [{
                "name": "portal",
                "kind": "rss",
                "enabled": true,
                "url": "https://example.test/feed",
                "last_run": {
                    "ok": false,
                    "error": "timeout",
                    "items_seen": 0,
                    "started_at": "2026-11-09T10:00:00Z"
                }
            }]
        }))
        .unwrap();
        assert!(!sources[0].last_run.as_ref().unwrap().ok);

        let active = parse_active_workflows(&json!({
            "workflows": [{
                "conversation_id": "dir:main",
                "goal": "Ship",
                "done": 1,
                "total": 3,
                "updated_at": "2026-11-09T12:00:00Z"
            }]
        }))
        .unwrap();
        assert_eq!(active[0].done, 1);
    }
}
