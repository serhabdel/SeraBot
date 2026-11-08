//! The Logs tab model (0.28.0): parsing, validating and filtering the ring
//! buffer served by `GET /v1/logs?level=&q=` (`{entries:[{ts,level,target,message}]}`).
//!
//! The endpoint is new in 0.28.0: until the server ships it the client treats
//! a 404 as "not available" (see `ApiClient::logs`), and this module never
//! invents rows. Entries are untrusted display data: strings are bounded and
//! control characters dropped before they reach the UI.

use serde_json::Value;

/// Levels the filter offers, lowest to highest.
pub const LEVELS: [&str; 5] = ["trace", "debug", "info", "warn", "error"];

const MAX_TS: usize = 64;
const MAX_TARGET: usize = 256;
const MAX_MESSAGE: usize = 2_000;
/// Most entries kept after a read; the panel never holds more.
pub const MAX_ENTRIES: usize = 500;

/// One log line. `level` is a known level or, for forward compatibility, any
/// short lowercase word the server sends.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LogEntry {
    pub ts: String,
    pub level: String,
    pub target: String,
    pub message: String,
}

fn clean(value: &Value, key: &str, max: usize, keep_newlines: bool) -> Option<String> {
    let text: String = value
        .get(key)?
        .as_str()?
        .chars()
        .filter(|c| !c.is_control() || (keep_newlines && matches!(c, '\n' | '\t')))
        .take(max)
        .collect();
    let text = text.trim().to_owned();
    (!text.is_empty()).then_some(text)
}

/// Parse the documented response shape; `None` when anything is off.
pub fn parse_entries(value: &Value) -> Option<Vec<LogEntry>> {
    let entries = value.get("entries")?.as_array()?;
    if entries.len() > MAX_ENTRIES {
        return None;
    }
    let mut parsed = Vec::with_capacity(entries.len());
    for entry in entries {
        let level = clean(entry, "level", 16, false)?;
        parsed.push(LogEntry {
            ts: clean(entry, "ts", MAX_TS, false).unwrap_or_default(),
            level: level.to_ascii_lowercase(),
            target: clean(entry, "target", MAX_TARGET, false).unwrap_or_default(),
            message: clean(entry, "message", MAX_MESSAGE, true).unwrap_or_default(),
        });
    }
    Some(parsed)
}

/// The filter value for a level chip: `None` means "all levels".
pub fn parse_level_filter(raw: &str) -> Option<&'static str> {
    let raw = raw.trim().to_ascii_lowercase();
    match raw.as_str() {
        "" | "all" => None,
        other => LEVELS.iter().find(|level| **level == other).copied(),
    }
}

/// Rank for "at least this level" filtering done client-side when the server
/// ignores `level`; unknown levels rank below `trace` only when unfiltered.
fn level_rank(level: &str) -> u8 {
    LEVELS
        .iter()
        .position(|known| *known == level)
        .map(|index| index as u8)
        .unwrap_or(u8::MAX)
}

/// The query pairs for `GET /v1/logs`: `level` and `q`, omitted when empty.
pub fn logs_query<'a>(level: Option<&'a str>, query: &'a str) -> Vec<(&'a str, &'a str)> {
    let mut params = Vec::new();
    if let Some(level) = level {
        params.push(("level", level));
    }
    let query = query.trim();
    if !query.is_empty() {
        params.push(("q", query));
    }
    params
}

/// Client-side filter: a known level keeps entries of that level or higher,
/// the search matches case-insensitively against message and target.
pub fn filter_entries<'a>(
    entries: &'a [LogEntry],
    level: Option<&str>,
    query: &str,
) -> Vec<&'a LogEntry> {
    let min_rank = level.map(level_rank);
    let query = query.trim().to_ascii_lowercase();
    entries
        .iter()
        .filter(|entry| {
            min_rank.is_none_or(|min| level_rank(&entry.level) >= min || entry.level.is_empty())
                && (query.is_empty()
                    || entry.message.to_ascii_lowercase().contains(&query)
                    || entry.target.to_ascii_lowercase().contains(&query))
        })
        .collect()
}

/// Short clock time for a row: the `HH:MM:SS` part of an ISO timestamp.
pub fn time_of(ts: &str) -> &str {
    let bytes = ts.as_bytes();
    if bytes.len() >= 19 && bytes[10] == b'T' && bytes[13] == b':' && bytes[16] == b':' {
        &ts[11..19]
    } else {
        ts
    }
}

/// The CSS-ish tone class per level for the row.
pub fn level_class(level: &str) -> &'static str {
    match level {
        "trace" | "debug" => "log-trace",
        "info" => "log-info",
        "warn" => "log-warn",
        "error" => "log-error",
        _ => "log-trace",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn entries() -> Vec<LogEntry> {
        parse_entries(&json!({"entries": [
            {"ts": "2026-11-08T10:00:00.000Z", "level": "INFO", "target": "serabot::api", "message": "listening on :8080"},
            {"ts": "2026-11-08T10:00:01.000Z", "level": "warn", "target": "serabot::gateway", "message": "slow approval"},
            {"ts": "2026-11-08T10:00:02.000Z", "level": "error", "target": "serabot::runtime", "message": "turn failed\u{7}"},
            {"ts": "", "level": "debug", "target": "", "message": "boot"}
        ]}))
        .unwrap()
    }

    #[test]
    fn parses_the_documented_shape_and_normalizes_levels() {
        let parsed = entries();
        assert_eq!(parsed.len(), 4);
        assert_eq!(parsed[0].level, "info");
        assert_eq!(parsed[0].ts, "2026-11-08T10:00:00.000Z");
        // Control characters are dropped, newlines in messages survive.
        assert!(!parsed[2].message.contains('\u{7}'));
        assert_eq!(parsed[3].target, "");
    }

    #[test]
    fn rejects_malformed_payloads() {
        assert!(parse_entries(&json!({"entries": "nope"})).is_none());
        assert!(parse_entries(&json!({})).is_none());
        assert!(parse_entries(&json!({"entries": [{"ts": "x"}]})).is_none());
        // A level alone is a valid (if quiet) row; ts/target/message default.
        let bare = parse_entries(&json!({"entries": [{"level": "info"}]})).unwrap();
        assert_eq!(bare[0].message, "");
        let huge = json!({"entries": (0..=MAX_ENTRIES).map(|i| json!({"level": format!("l{i}"), "message": "x"})).collect::<Vec<_>>()});
        assert!(parse_entries(&huge).is_none());
        let long = json!({"entries": [{"level": "info", "message": "x".repeat(MAX_MESSAGE + 1)}]});
        let parsed = parse_entries(&long).unwrap();
        assert!(parsed[0].message.chars().count() <= MAX_MESSAGE);
    }

    #[test]
    fn level_filter_accepts_only_known_levels() {
        assert_eq!(parse_level_filter(""), None);
        assert_eq!(parse_level_filter("all"), None);
        assert_eq!(parse_level_filter(" WARN "), Some("warn"));
        assert_eq!(parse_level_filter("verbose"), None);
    }

    #[test]
    fn query_pairs_omit_empty_values() {
        assert!(logs_query(None, "  ").is_empty());
        assert_eq!(
            logs_query(Some("error"), "turn"),
            vec![("level", "error"), ("q", "turn")]
        );
        assert_eq!(logs_query(None, "gateway"), vec![("q", "gateway")]);
    }

    #[test]
    fn filtering_applies_level_floor_and_case_insensitive_search() {
        let parsed = entries();
        let all = filter_entries(&parsed, None, "");
        assert_eq!(all.len(), 4);
        let warn_up = filter_entries(&parsed, Some("warn"), "");
        assert_eq!(warn_up.len(), 2);
        assert!(
            warn_up
                .iter()
                .all(|e| e.level == "warn" || e.level == "error")
        );
        let search = filter_entries(&parsed, None, "GATEWAY");
        assert_eq!(search.len(), 1);
        assert_eq!(search[0].message, "slow approval");
        let both = filter_entries(&parsed, Some("error"), "turn");
        assert_eq!(both.len(), 1);
        assert_eq!(both[0].level, "error");
    }

    #[test]
    fn time_of_extracts_the_clock_part() {
        assert_eq!(time_of("2026-11-08T10:00:02.000Z"), "10:00:02");
        assert_eq!(time_of("short"), "short");
    }

    #[test]
    fn level_classes_cover_every_known_level() {
        for level in LEVELS {
            assert!(!level_class(level).is_empty());
        }
        assert_eq!(level_class("something-else"), "log-trace");
    }
}
