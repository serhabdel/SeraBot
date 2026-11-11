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
    /// A feed this user added, as opposed to an admin-defined one (0.34.0).
    pub own: bool,
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
        own: row.get("own").and_then(|v| v.as_bool()).unwrap_or(false),
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

/// Morning brief row and `watch_{n}` rows for the merged schedule panel (B2).
pub fn split_schedule_routines(routines: &[Routine]) -> (Option<Routine>, Vec<Routine>) {
    let mut brief = None;
    let mut watch = Vec::new();
    for routine in routines {
        if routine.name == "morning_brief" {
            brief = Some(routine.clone());
        } else if routine.name.starts_with("watch_") {
            watch.push(routine.clone());
        }
    }
    watch.sort_by(|left, right| left.name.cmp(&right.name));
    (brief, watch)
}

/// Placeholder watch slot names when the server has not created rows yet.
pub fn default_watch_slot_names() -> Vec<String> {
    (0..3).map(|index| format!("watch_{index}")).collect()
}

/// Every routine name a member can schedule: the morning brief and the watch
/// slots. Matches the server's limit of twelve watch slots.
pub fn member_routine_slots() -> Vec<String> {
    std::iter::once("morning_brief".to_owned())
        .chain((0..12).map(|index| format!("watch_{index}")))
        .collect()
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

/// Seconds since the Unix epoch for an RFC 3339 UTC stamp such as
/// `2026-11-09T10:00:00Z` (fractions and a `+00:00` offset are accepted).
/// Other offsets are not guessed at: they return `None`.
pub fn epoch_secs(raw: &str) -> Option<i64> {
    let raw = raw.trim();
    let date = raw.get(..10)?;
    let time = raw.get(11..19)?;
    if raw.get(10..11)? != "T" && raw.get(10..11)? != " " {
        return None;
    }
    let rest = raw.get(19..)?;
    let rest = rest.trim_start_matches(|c: char| c == '.' || c.is_ascii_digit());
    if !(rest == "Z" || rest == "z" || rest == "+00:00" || rest.is_empty()) {
        return None;
    }
    let y: i64 = date.get(..4)?.parse().ok()?;
    let m: i64 = date.get(5..7)?.parse().ok()?;
    let d: i64 = date.get(8..10)?.parse().ok()?;
    let h: i64 = time.get(..2)?.parse().ok()?;
    let mi: i64 = time.get(3..5)?.parse().ok()?;
    let se: i64 = time.get(6..8)?.parse().ok()?;
    if !(1..=12).contains(&m) || !(1..=31).contains(&d) || h > 23 || mi > 59 || se > 60 {
        return None;
    }
    // Days from civil (Howard Hinnant's algorithm).
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    Some(days * 86_400 + h * 3_600 + mi * 60 + se)
}

fn span(secs: i64) -> String {
    match secs {
        0..=44 => "a moment".into(),
        45..=3_599 => format!("{}m", ((secs + 30) / 60).max(1)),
        3_600..=86_399 => format!("{}h", secs / 3_600),
        _ => format!("{}d", secs / 86_400),
    }
}

/// "5m ago" or "in 2h" for a stamp, given the current time. A stamp that
/// cannot be read is shown trimmed as-is, so nothing is hidden.
pub fn relative_time(raw: &str, now_secs: i64) -> String {
    let Some(at) = epoch_secs(raw) else {
        return format_next_run(raw);
    };
    let diff = at - now_secs;
    if diff.abs() < 45 {
        return "just now".into();
    }
    if diff < 0 {
        format!("{} ago", span(-diff))
    } else {
        format!("in {}", span(diff))
    }
}

/// The current time in whole seconds since the Unix epoch.
pub fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// The host of a feed URL for a short subtitle; `None` when it is not a URL.
pub fn source_host(url: &str) -> Option<String> {
    let parsed = url::Url::parse(url.trim()).ok()?;
    parsed.host_str().map(str::to_owned)
}

/// A conversation id cut short for display, ending in an ellipsis.
pub fn short_id(id: &str, max: usize) -> String {
    if id.chars().count() <= max {
        id.to_owned()
    } else {
        let head: String = id.chars().take(max.saturating_sub(1)).collect();
        format!("{head}\u{2026}")
    }
}

/// The label of an active plan's conversation: its name when known, else a
/// shortened id.
pub fn plan_conversation_label(name: Option<&str>, id: &str) -> String {
    match name.map(str::trim).filter(|n| !n.is_empty()) {
        Some(name) => name.to_owned(),
        None => short_id(id, 14),
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

    #[test]
    fn epoch_parses_utc_stamps() {
        assert_eq!(epoch_secs("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(epoch_secs("2026-11-01T00:00:00Z"), Some(20_758 * 86_400));
        assert_eq!(
            epoch_secs("2026-11-01T10:30:15.250Z"),
            Some(20_758 * 86_400 + 37_815)
        );
        assert_eq!(
            epoch_secs("2026-11-01T00:00:00+00:00"),
            Some(20_758 * 86_400)
        );
        assert_eq!(epoch_secs("2026-11-01T00:00:00+02:00"), None);
        assert_eq!(epoch_secs("nope"), None);
        assert_eq!(epoch_secs("2026-13-01T00:00:00Z"), None);
    }

    #[test]
    fn relative_times_read_naturally() {
        let now = epoch_secs("2026-11-10T12:00:00Z").unwrap();
        assert_eq!(relative_time("2026-11-10T11:55:00Z", now), "5m ago");
        assert_eq!(relative_time("2026-11-10T09:00:00Z", now), "3h ago");
        assert_eq!(relative_time("2026-11-08T12:00:00Z", now), "2d ago");
        assert_eq!(relative_time("2026-11-10T14:00:00Z", now), "in 2h");
        assert_eq!(relative_time("2026-11-10T12:00:20Z", now), "just now");
        assert_eq!(relative_time("garbage", now), "garbage");
        assert_eq!(relative_time("", now), "\u{2014}");
    }

    #[test]
    fn source_host_and_short_id() {
        assert_eq!(
            source_host("https://example.test/feed?k=1").as_deref(),
            Some("example.test")
        );
        assert_eq!(source_host("not a url"), None);
        assert_eq!(short_id("abc", 14), "abc");
        assert_eq!(
            short_id("0123456789abcdefghij", 14),
            "0123456789abc\u{2026}"
        );
        assert_eq!(plan_conversation_label(Some("  Trip  "), "x"), "Trip");
        assert_eq!(plan_conversation_label(Some(" "), "dir:main"), "dir:main");
        assert_eq!(
            plan_conversation_label(None, "0123456789abcdefghij"),
            "0123456789abc\u{2026}"
        );
    }
}
