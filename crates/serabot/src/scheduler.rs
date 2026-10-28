//! Durable routine scheduling.
//!
//! Routines are stored in SQLite. A daily routine owns at most one logical
//! occurrence per local date, enforced by a conditional `UPDATE ... RETURNING`
//! on `last_run_for`. Downtime therefore yields exactly one catch-up
//! occurrence (the most recent one), never a burst of missed runs.
//!
//! Local-time resolution uses chrono-tz:
//! * nonexistent local times (spring forward) shift forward past the gap;
//! * ambiguous local times (fall back) resolve to the earliest instant.

use chrono::{
    DateTime, Duration, MappedLocalTime, NaiveDate, NaiveDateTime, NaiveTime, SecondsFormat,
    TimeZone, Utc,
};
use chrono_tz::Tz;
use rusqlite::{OptionalExtension, params};
use serde::Serialize;
use serde_json::{Value, json};

use crate::domain::new_id;
use crate::error::{Result, SeraError};
use crate::store::Store;

/// A routine occurrence claimed by [`due_routines`].
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct DueRoutine {
    pub id: String,
    pub name: String,
    pub kind: String,
    /// Local calendar date (`YYYY-MM-DD`) this occurrence belongs to.
    pub scheduled_for_local_date: String,
    /// Stored routine configuration (`time`, `timezone`).
    pub config: Value,
}

/// Create or update a daily routine without losing its run history.
///
/// Repeated calls keep `last_run_for` and, unless the configured time or
/// timezone changed, preserve `next_run_at`. Preserving a past `next_run_at`
/// is what allows a missed occurrence to be caught up after downtime.
pub fn ensure_daily_routine(store: &Store, name: &str, time_hhmm: &str, tz: Tz) -> Result<()> {
    if name.trim().is_empty() || name.chars().count() > 128 || name.chars().any(char::is_control) {
        return Err(SeraError::Validation("invalid routine name".into()));
    }
    let time = parse_time(time_hhmm)?;
    let now = Utc::now();
    let now_string = utc_string(now);
    let next = next_occurrence(now, time, tz)
        .ok_or_else(|| SeraError::Internal("cannot compute the next routine occurrence".into()))?;
    let config = json!({
        "time": time.format("%H:%M").to_string(),
        "timezone": tz.name(),
    })
    .to_string();

    let connection = store.connection()?;
    let existing: Option<(String, String)> = connection
        .query_row(
            "SELECT next_run_at, config_json FROM routines WHERE name = ?1",
            params![name],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;

    match existing {
        None => {
            connection.execute(
                "INSERT INTO routines(
                    id, name, kind, enabled, next_run_at, last_run_for,
                    config_json, created_at, updated_at
                 ) VALUES (?1, ?2, 'daily', 1, ?3, NULL, ?4, ?5, ?5)",
                params![new_id(), name, utc_string(next), config, now_string],
            )?;
        }
        Some((old_next, old_config)) => {
            let old: Option<Value> = serde_json::from_str(&old_config).ok();
            let old_time = old
                .as_ref()
                .and_then(|value| value.get("time"))
                .and_then(Value::as_str)
                .and_then(|value| parse_time(value).ok());
            let old_timezone = old
                .as_ref()
                .and_then(|value| value.get("timezone"))
                .and_then(Value::as_str);
            // Keep a past next_run_at when nothing scheduling-related changed
            // so a missed run can still be caught up.
            let next_run_at = if old_time == Some(time) && old_timezone == Some(tz.name()) {
                old_next
            } else {
                utc_string(next)
            };
            connection.execute(
                "UPDATE routines
                 SET kind = 'daily', config_json = ?1, next_run_at = ?2, updated_at = ?3
                 WHERE name = ?4",
                params![config, next_run_at, now_string, name],
            )?;
        }
    }
    Ok(())
}

/// Claim all enabled routines whose latest local occurrence has not been run
/// for its date yet. Claiming is atomic per routine.
pub fn due_routines(store: &Store, now: DateTime<Utc>, tz: Tz) -> Result<Vec<DueRoutine>> {
    let connection = store.connection()?;
    let routines: Vec<(String, String, String)> = {
        let mut statement = connection.prepare(
            "SELECT id, config_json, next_run_at
             FROM routines WHERE enabled = 1 ORDER BY name ASC",
        )?;
        let rows = statement.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        })?;
        rows.collect::<std::result::Result<Vec<_>, _>>()?
    };

    let now_string = utc_string(now);
    let mut due = Vec::new();

    for (id, config_json, next_run_at) in routines {
        // A future next_run_at means the routine has not become due yet.
        if parse_utc(&next_run_at).is_some_and(|next| next > now) {
            continue;
        }

        let config: Value = serde_json::from_str(&config_json).unwrap_or(Value::Null);
        let Some(time) = config
            .get("time")
            .and_then(Value::as_str)
            .and_then(|value| parse_time(value).ok())
        else {
            continue;
        };
        let Some((_occurrence, local_date)) = latest_occurrence(now, time, tz) else {
            continue;
        };
        let local_date_string = local_date.format("%Y-%m-%d").to_string();
        let next_run_at = next_occurrence(now, time, tz)
            .map(utc_string)
            .unwrap_or_else(|| now_string.clone());

        // One conditional statement both checks and claims. Concurrent
        // callers serialize in SQLite; only the first update sees a smaller
        // last_run_for and returns a row.
        let claimed: Option<(String, String, String, String)> = connection
            .query_row(
                "UPDATE routines
                 SET last_run_for = ?1, next_run_at = ?2, updated_at = ?3
                 WHERE id = ?4 AND enabled = 1
                   AND (last_run_for IS NULL OR last_run_for < ?1)
                 RETURNING id, name, kind, config_json",
                params![local_date_string, next_run_at, now_string, id],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                    ))
                },
            )
            .optional()?;

        if let Some((id, name, kind, config_json)) = claimed {
            due.push(DueRoutine {
                id,
                name,
                kind,
                scheduled_for_local_date: local_date_string,
                config: serde_json::from_str(&config_json).unwrap_or(Value::Null),
            });
        }
    }

    Ok(due)
}

fn parse_time(value: &str) -> Result<NaiveTime> {
    NaiveTime::parse_from_str(value.trim(), "%H:%M")
        .map_err(|_| SeraError::Validation(format!("invalid HH:MM routine time: {value:?}")))
}

fn parse_utc(value: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(value)
        .ok()
        .map(|value| value.with_timezone(&Utc))
}

fn utc_string(value: DateTime<Utc>) -> String {
    value.to_rfc3339_opts(SecondsFormat::Millis, true)
}

fn resolve_local(naive: NaiveDateTime, tz: Tz) -> Option<DateTime<Tz>> {
    match tz.from_local_datetime(&naive) {
        MappedLocalTime::Single(value) => Some(value),
        // Ambiguous wall clock (fall back): earliest occurrence wins.
        MappedLocalTime::Ambiguous(earliest, latest) => Some(earliest.min(latest)),
        // Nonexistent wall clock (spring forward): shift forward past the gap.
        // European gaps are one hour; try a few hours for unusual zones.
        MappedLocalTime::None => {
            for hours in 1..=4 {
                let shifted = naive.checked_add_signed(Duration::hours(hours))?;
                match tz.from_local_datetime(&shifted) {
                    MappedLocalTime::Single(value) => return Some(value),
                    MappedLocalTime::Ambiguous(earliest, latest) => {
                        return Some(earliest.min(latest));
                    }
                    MappedLocalTime::None => {}
                }
            }
            None
        }
    }
}

/// Most recent local occurrence at or before `now`, with its local date.
fn latest_occurrence(
    now: DateTime<Utc>,
    time: NaiveTime,
    tz: Tz,
) -> Option<(DateTime<Utc>, NaiveDate)> {
    let local_now = now.with_timezone(&tz);
    let today = local_now.date_naive();
    if let Some(candidate) = resolve_local(today.and_time(time), tz) {
        let candidate = candidate.with_timezone(&Utc);
        if candidate <= now {
            return Some((candidate, today));
        }
    }
    let previous = today.pred_opt()?;
    let candidate = resolve_local(previous.and_time(time), tz)?;
    Some((candidate.with_timezone(&Utc), previous))
}

/// Next local occurrence strictly after `now`.
fn next_occurrence(now: DateTime<Utc>, time: NaiveTime, tz: Tz) -> Option<DateTime<Utc>> {
    let local_now = now.with_timezone(&tz);
    let today = local_now.date_naive();
    if let Some(candidate) = resolve_local(today.and_time(time), tz) {
        let candidate = candidate.with_timezone(&Utc);
        if candidate > now {
            return Some(candidate);
        }
    }
    let tomorrow = today.checked_add_signed(Duration::days(1))?;
    resolve_local(tomorrow.and_time(time), tz).map(|value| value.with_timezone(&Utc))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> Store {
        Store::open_in_memory(384).unwrap()
    }

    fn rome() -> Tz {
        "Europe/Rome".parse().unwrap()
    }

    fn insert_routine(
        store: &Store,
        name: &str,
        time: &str,
        enabled: bool,
        next_run_at: DateTime<Utc>,
        last_run_for: Option<&str>,
    ) {
        let connection = store.connection().unwrap();
        connection
            .execute(
                "INSERT INTO routines(
                    id, name, kind, enabled, next_run_at, last_run_for,
                    config_json, created_at, updated_at
                 ) VALUES (?1, ?2, 'daily', ?3, ?4, ?5, ?6, ?7, ?7)",
                params![
                    new_id(),
                    name,
                    enabled as i64,
                    utc_string(next_run_at),
                    last_run_for,
                    json!({"time": time, "timezone": "Europe/Rome"}).to_string(),
                    utc_string(Utc::now())
                ],
            )
            .unwrap();
    }

    fn stored_next_run(store: &Store, name: &str) -> String {
        let connection = store.connection().unwrap();
        connection
            .query_row(
                "SELECT next_run_at FROM routines WHERE name = ?1",
                params![name],
                |row| row.get(0),
            )
            .unwrap()
    }

    #[test]
    fn ensure_is_idempotent_and_validates_input() {
        let store = store();
        ensure_daily_routine(&store, "morning-brief", "08:00", rome()).unwrap();
        ensure_daily_routine(&store, "morning-brief", "08:00", rome()).unwrap();

        let connection = store.connection().unwrap();
        let count: i64 = connection
            .query_row("SELECT COUNT(*) FROM routines", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 1);

        assert!(ensure_daily_routine(&store, "broken", "25:99", rome()).is_err());
        assert!(ensure_daily_routine(&store, "", "08:00", rome()).is_err());
    }

    #[test]
    fn claims_only_one_occurrence_per_local_date() {
        let store = store();
        let now = Utc.with_ymd_and_hms(2026, 10, 28, 12, 0, 0).unwrap();
        insert_routine(
            &store,
            "daily",
            "08:00",
            true,
            Utc.with_ymd_and_hms(2026, 10, 1, 6, 0, 0).unwrap(),
            None,
        );

        let first = due_routines(&store, now, rome()).unwrap();
        assert_eq!(first.len(), 1);
        assert_eq!(first[0].scheduled_for_local_date, "2026-10-28");

        let second = due_routines(&store, now, rome()).unwrap();
        assert!(second.is_empty());
    }

    #[test]
    fn catches_up_once_after_downtime() {
        let store = store();
        let now = Utc.with_ymd_and_hms(2026, 10, 28, 12, 0, 0).unwrap();
        insert_routine(
            &store,
            "daily",
            "08:00",
            true,
            Utc.with_ymd_and_hms(2026, 10, 20, 6, 0, 0).unwrap(),
            Some("2026-10-20"),
        );

        let due = due_routines(&store, now, rome()).unwrap();
        assert_eq!(due.len(), 1);
        assert_eq!(due[0].scheduled_for_local_date, "2026-10-28");
        assert!(due_routines(&store, now, rome()).unwrap().is_empty());
    }

    #[test]
    fn disabled_routines_are_never_due() {
        let store = store();
        let now = Utc.with_ymd_and_hms(2026, 10, 28, 12, 0, 0).unwrap();
        insert_routine(
            &store,
            "disabled",
            "08:00",
            false,
            Utc.with_ymd_and_hms(2026, 10, 1, 6, 0, 0).unwrap(),
            None,
        );
        assert!(due_routines(&store, now, rome()).unwrap().is_empty());
    }

    #[test]
    fn spring_forward_shifts_nonexistent_time_forward() {
        let store = store();
        // 2026-03-29 04:00 CEST = 02:00 UTC. The configured 02:30 does not
        // exist that day and must shift forward to 03:30 CEST (01:30 UTC).
        let now = Utc.with_ymd_and_hms(2026, 3, 29, 2, 0, 0).unwrap();
        insert_routine(
            &store,
            "dst",
            "02:30",
            true,
            Utc.with_ymd_and_hms(2026, 3, 1, 0, 0, 0).unwrap(),
            None,
        );

        let due = due_routines(&store, now, rome()).unwrap();
        assert_eq!(due.len(), 1);
        assert_eq!(due[0].scheduled_for_local_date, "2026-03-29");
        // Next day 02:30 CEST is 00:30 UTC.
        assert!(
            stored_next_run(&store, "dst").starts_with("2026-03-30T00:30"),
            "{}",
            stored_next_run(&store, "dst")
        );
    }

    #[test]
    fn fall_back_chooses_earliest_ambiguous_occurrence() {
        let store = store();
        // 2026-10-25 02:30 occurs twice in Rome. 00:45 UTC is 02:45 CEST,
        // after the first (earliest) occurrence but before the second, so the
        // routine must already be due.
        let now = Utc.with_ymd_and_hms(2026, 10, 25, 0, 45, 0).unwrap();
        insert_routine(
            &store,
            "dst",
            "02:30",
            true,
            Utc.with_ymd_and_hms(2026, 10, 1, 0, 0, 0).unwrap(),
            None,
        );

        let due = due_routines(&store, now, rome()).unwrap();
        assert_eq!(due.len(), 1);
        assert_eq!(due[0].scheduled_for_local_date, "2026-10-25");
        // Next occurrence is the next day at 02:30 CET (01:30 UTC).
        assert!(
            stored_next_run(&store, "dst").starts_with("2026-10-26T01:30"),
            "{}",
            stored_next_run(&store, "dst")
        );
    }
}
