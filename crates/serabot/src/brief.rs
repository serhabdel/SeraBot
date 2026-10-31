//! Morning brief generation.
//!
//! The brief is deterministic plain text derived only from SQLite rows: no
//! model call, no network, no invented times. Date-only deadlines are printed
//! as dates, while explicit date-times are shown in the configured timezone.

use chrono::{
    DateTime, Duration, MappedLocalTime, NaiveDate, NaiveDateTime, NaiveTime, SecondsFormat,
    TimeZone, Utc,
};
use chrono_tz::Tz;
use rusqlite::params;

use crate::error::Result;
use crate::store::Store;
use crate::util::truncate_chars;

const MAX_GROUP_ITEMS: usize = 20;
const MAX_SOURCE_WARNINGS: usize = 20;
const MAX_BRIEF_CHARS: usize = 8000;
const MAX_TITLE_CHARS: usize = 160;
const MAX_SOURCE_CHARS: usize = 60;
const STALE_SOURCE_HOURS: i64 = 48;

struct DeadlineItem {
    sort_key: String,
    line: String,
    overdue: bool,
}

struct DeadlineRow {
    due_date: String,
    due_time: Option<String>,
    stored_timezone: Option<String>,
    precision: String,
    title: String,
    source: String,
}

struct OpportunityItem {
    title: String,
    source: String,
    url: Option<String>,
}

/// A built brief plus the counts behind the optional silent-when-empty rule.
#[derive(Debug, Clone)]
pub struct BriefReport {
    pub text: String,
    /// True when the brief has no section at all (no deadlines, no new or
    /// changed items, no source warnings).
    pub empty: bool,
}

/// Build the daily brief. `lookback_hours` bounds which new/changed
/// opportunities are considered; deadlines are always evaluated relative to
/// `now`.
pub fn build_brief(
    store: &Store,
    now: DateTime<Utc>,
    tz: Tz,
    lookback_hours: u64,
) -> Result<String> {
    Ok(build_brief_report(store, now, tz, lookback_hours)?.text)
}

/// Build the daily brief text together with its emptiness flag.
pub fn build_brief_report(
    store: &Store,
    now: DateTime<Utc>,
    tz: Tz,
    lookback_hours: u64,
) -> Result<BriefReport> {
    let connection = store.connection()?;
    let local_now = now.with_timezone(&tz);
    let today = local_now.date_naive();

    let lookback = Duration::hours(
        i64::try_from(lookback_hours)
            .unwrap_or(i64::MAX)
            .clamp(0, 24 * 31),
    );
    let cutoff = utc_string(now - lookback);

    let mut overdue = Vec::new();
    let mut due_soon = Vec::new();
    {
        let mut statement = connection.prepare(
            "SELECT d.due_date, d.due_time, d.timezone, d.precision,
                    o.title, o.source
             FROM deadlines d
             JOIN opportunities o ON o.id = d.opportunity_id
             WHERE d.status <> 'superseded' AND o.status <> 'withdrawn'
             ORDER BY d.due_date ASC, d.due_time ASC
             LIMIT 500",
        )?;
        let rows = statement.query_map([], |row| {
            Ok(DeadlineRow {
                due_date: row.get(0)?,
                due_time: row.get(1)?,
                stored_timezone: row.get(2)?,
                precision: row.get(3)?,
                title: row.get(4)?,
                source: row.get(5)?,
            })
        })?;
        for row in rows {
            if let Some(item) = deadline_item(row?, now, tz, today) {
                if item.overdue {
                    overdue.push(item);
                } else {
                    due_soon.push(item);
                }
            }
        }
    }
    overdue.sort_by_key(|item| item.sort_key.clone());
    due_soon.sort_by_key(|item| item.sort_key.clone());

    let mut new_items = Vec::new();
    let mut changed_items = Vec::new();
    {
        let mut statement = connection.prepare(
            "SELECT title, source, url, revision
             FROM opportunities
             WHERE status <> 'withdrawn' AND last_seen >= ?1
             ORDER BY last_seen DESC, title ASC
             LIMIT 200",
        )?;
        let rows = statement.query_map(params![cutoff], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, Option<String>>(2)?,
                row.get::<_, i64>(3)?,
            ))
        })?;
        for row in rows {
            let (title, source, url, revision) = row?;
            let item = OpportunityItem { title, source, url };
            if revision <= 1 {
                new_items.push(item);
            } else {
                changed_items.push(item);
            }
        }
    }

    let warnings = source_warnings(&connection, now, tz)?;

    let mut out = String::new();
    out.push_str(&format!(
        "Morning brief - {} ({})\n",
        local_now.format("%Y-%m-%d %H:%M"),
        tz.name()
    ));

    let mut sections = 0usize;
    if !overdue.is_empty() {
        sections += 1;
        push_section(
            &mut out,
            "Overdue",
            overdue.len(),
            overdue.iter().map(|item| item.line.clone()),
        );
    }
    if !due_soon.is_empty() {
        sections += 1;
        push_section(
            &mut out,
            "Due soon (next 7 days)",
            due_soon.len(),
            due_soon.iter().map(|item| item.line.clone()),
        );
    }
    if !changed_items.is_empty() {
        sections += 1;
        push_section(
            &mut out,
            "Changed",
            changed_items.len(),
            changed_items.iter().map(opportunity_line),
        );
    }
    if !new_items.is_empty() {
        sections += 1;
        push_section(
            &mut out,
            "New",
            new_items.len(),
            new_items.iter().map(opportunity_line),
        );
    }
    if !warnings.is_empty() {
        sections += 1;
        push_section(
            &mut out,
            "Source warnings",
            warnings.len(),
            warnings.iter().cloned(),
        );
    }

    let empty = sections == 0;
    if empty {
        out.push_str("\nNo new items, deadline changes, or source warnings.\n");
    }

    if out.chars().count() > MAX_BRIEF_CHARS {
        let mut truncated = truncate_chars(&out, MAX_BRIEF_CHARS - 20);
        truncated.push_str("\n[brief truncated]\n");
        return Ok(BriefReport {
            text: truncated,
            empty: false,
        });
    }
    Ok(BriefReport { text: out, empty })
}

fn deadline_item(
    row: DeadlineRow,
    now: DateTime<Utc>,
    tz: Tz,
    today: NaiveDate,
) -> Option<DeadlineItem> {
    let DeadlineRow {
        due_date,
        due_time,
        stored_timezone,
        precision,
        title,
        source,
    } = row;
    let date = NaiveDate::parse_from_str(&due_date, "%Y-%m-%d").ok()?;
    let time = due_time.and_then(|value| NaiveTime::parse_from_str(&value, "%H:%M").ok());
    let stored_tz = stored_timezone
        .as_deref()
        .and_then(|value| value.parse::<Tz>().ok())
        .unwrap_or(tz);

    let (display, sort_key, overdue, due_soon) = if precision == "datetime" {
        let value = time.and_then(|time| resolve_local(date.and_time(time), stored_tz))?;
        // Convert to the configured display timezone, regardless of
        // the timezone the source used.
        let local = value.with_timezone(&tz);
        let due_utc = local.with_timezone(&Utc);
        let display = format!("{} {}", local.format("%Y-%m-%d"), local.format("%H:%M"));
        let overdue = due_utc < now;
        let due_soon = !overdue && due_utc <= now + Duration::days(7);
        (display.clone(), display, overdue, due_soon)
    } else {
        let display = date.format("%Y-%m-%d").to_string();
        let overdue = date < today;
        let due_soon = !overdue && date <= today + Duration::days(7);
        (display.clone(), display, overdue, due_soon)
    };

    if !overdue && !due_soon {
        return None;
    }

    let line = format!(
        "{}  {} [{}]",
        display,
        truncate_chars(&one_line(&title), MAX_TITLE_CHARS),
        truncate_chars(&one_line(&source), MAX_SOURCE_CHARS)
    );
    Some(DeadlineItem {
        sort_key,
        line,
        overdue,
    })
}

fn opportunity_line(item: &OpportunityItem) -> String {
    let mut line = format!(
        "{} [{}]",
        truncate_chars(&one_line(&item.title), MAX_TITLE_CHARS),
        truncate_chars(&one_line(&item.source), MAX_SOURCE_CHARS)
    );
    if let Some(url) = &item.url {
        line.push(' ');
        line.push_str(&truncate_chars(&one_line(url), 200));
    }
    line
}

fn source_warnings(
    connection: &rusqlite::Connection,
    now: DateTime<Utc>,
    tz: Tz,
) -> Result<Vec<String>> {
    let mut warnings = Vec::new();
    let mut statement = connection.prepare(
        "SELECT source,
                MAX(CASE WHEN ok = 1 THEN COALESCE(finished_at, started_at) END),
                MAX(CASE WHEN ok = 0 THEN started_at END)
         FROM source_runs
         GROUP BY source
         ORDER BY source ASC",
    )?;
    let rows = statement.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, Option<String>>(1)?,
            row.get::<_, Option<String>>(2)?,
        ))
    })?;
    for row in rows {
        let (source, last_ok, last_failure) = row?;
        let last_ok = last_ok.as_deref().and_then(parse_utc);
        let last_failure = last_failure.as_deref().and_then(parse_utc);

        let stale = match last_ok {
            None => true,
            Some(value) => now - value > Duration::hours(STALE_SOURCE_HOURS),
        };
        let recent_failure = last_failure
            .map(|failure| {
                now - failure <= Duration::hours(STALE_SOURCE_HOURS)
                    && last_ok.map(|success| failure > success).unwrap_or(true)
            })
            .unwrap_or(false);

        if !stale && !recent_failure {
            continue;
        }

        let mut detail = String::new();
        if stale {
            match last_ok {
                Some(value) => detail.push_str(&format!(
                    "no successful run since {}",
                    value.with_timezone(&tz).format("%Y-%m-%d %H:%M")
                )),
                None => detail.push_str("no successful run recorded"),
            }
        }
        if recent_failure && let Some(value) = last_failure {
            if !detail.is_empty() {
                detail.push_str("; ");
            }
            detail.push_str(&format!(
                "last run failed at {}",
                value.with_timezone(&tz).format("%Y-%m-%d %H:%M")
            ));
        }
        warnings.push(format!(
            "{}: {}",
            truncate_chars(&one_line(&source), MAX_SOURCE_CHARS),
            detail
        ));
        if warnings.len() >= MAX_SOURCE_WARNINGS {
            break;
        }
    }
    Ok(warnings)
}

fn push_section(
    out: &mut String,
    title: &str,
    total: usize,
    lines: impl IntoIterator<Item = String>,
) {
    out.push_str(&format!("\n{} ({})\n", title, total));
    for line in lines.into_iter().take(MAX_GROUP_ITEMS) {
        out.push_str("- ");
        out.push_str(&truncate_chars(&line, 400));
        out.push('\n');
    }
    if total > MAX_GROUP_ITEMS {
        out.push_str(&format!("- ... {} more\n", total - MAX_GROUP_ITEMS));
    }
}

fn parse_utc(value: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(value)
        .ok()
        .map(|value| value.with_timezone(&Utc))
}

fn utc_string(value: DateTime<Utc>) -> String {
    value.to_rfc3339_opts(SecondsFormat::Millis, true)
}

fn one_line(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn resolve_local(naive: NaiveDateTime, tz: Tz) -> Option<DateTime<Tz>> {
    match tz.from_local_datetime(&naive) {
        MappedLocalTime::Single(value) => Some(value),
        MappedLocalTime::Ambiguous(earliest, latest) => Some(earliest.min(latest)),
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::new_id;

    fn store() -> Store {
        Store::open_in_memory(384).unwrap()
    }

    fn rome() -> Tz {
        "Europe/Rome".parse().unwrap()
    }

    fn insert_opportunity(
        store: &Store,
        source: &str,
        title: &str,
        revision: i64,
        last_seen: DateTime<Utc>,
    ) -> String {
        let id = new_id();
        let connection = store.connection().unwrap();
        connection
            .execute(
                "INSERT INTO opportunities(
                    id, source, external_id, title, summary, url, status,
                    evidence, revision, first_seen, last_seen
                 ) VALUES (?1, ?2, ?3, ?4, '', ?5, 'open', '', ?6, ?7, ?7)",
                params![
                    id,
                    source,
                    format!("external-{title}"),
                    title,
                    format!("https://example.com/{title}"),
                    revision,
                    utc_string(last_seen)
                ],
            )
            .unwrap();
        id
    }

    fn insert_deadline(
        store: &Store,
        opportunity_id: &str,
        due_date: &str,
        due_time: Option<&str>,
        timezone: Option<&str>,
        precision: &str,
    ) {
        let connection = store.connection().unwrap();
        connection
            .execute(
                "INSERT INTO deadlines(
                    id, opportunity_id, due_date, due_time, timezone, precision,
                    source_text, status, observed_at
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'test', 'upcoming', ?7)",
                params![
                    new_id(),
                    opportunity_id,
                    due_date,
                    due_time,
                    timezone,
                    precision,
                    utc_string(Utc::now())
                ],
            )
            .unwrap();
    }

    fn insert_source_run(
        store: &Store,
        source: &str,
        ok: bool,
        started_at: DateTime<Utc>,
        finished_at: Option<DateTime<Utc>>,
    ) {
        let connection = store.connection().unwrap();
        connection
            .execute(
                "INSERT INTO source_runs(id, source, started_at, finished_at, ok, error, items_seen)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, 0)",
                params![
                    new_id(),
                    source,
                    utc_string(started_at),
                    finished_at.map(utc_string),
                    ok as i64,
                    if ok { None } else { Some("boom") }
                ],
            )
            .unwrap();
    }

    #[test]
    fn groups_deadlines_and_keeps_date_only_precision() {
        let store = store();
        let now = Utc.with_ymd_and_hms(2026, 10, 28, 10, 0, 0).unwrap();
        let opportunity =
            insert_opportunity(&store, "portal", "Tender A", 1, now - Duration::hours(1));
        insert_deadline(
            &store,
            &opportunity,
            "2026-10-27",
            None,
            Some("Europe/Rome"),
            "date",
        );
        insert_deadline(
            &store,
            &opportunity,
            "2026-10-30",
            None,
            Some("Europe/Rome"),
            "date",
        );

        let brief = build_brief(&store, now, rome(), 24).unwrap();
        assert!(brief.contains("Overdue (1)"), "{brief}");
        assert!(brief.contains("- 2026-10-27  Tender A [portal]"), "{brief}");
        assert!(brief.contains("Due soon (next 7 days) (1)"), "{brief}");
        assert!(brief.contains("- 2026-10-30  Tender A [portal]"), "{brief}");
        assert!(!brief.contains("00:00"), "{brief}");

        let overdue_at = brief.find("Overdue (1)").unwrap();
        let due_soon_at = brief.find("Due soon (next 7 days) (1)").unwrap();
        let new_at = brief.find("New (1)").unwrap();
        assert!(overdue_at < due_soon_at && due_soon_at < new_at, "{brief}");
    }

    #[test]
    fn converts_explicit_utc_datetimes_to_configured_timezone() {
        let store = store();
        let now = Utc.with_ymd_and_hms(2026, 7, 15, 9, 0, 0).unwrap();
        let opportunity =
            insert_opportunity(&store, "portal", "Tender B", 1, now - Duration::hours(1));
        insert_deadline(
            &store,
            &opportunity,
            "2026-07-15",
            Some("14:30"),
            Some("UTC"),
            "datetime",
        );

        let brief = build_brief(&store, now, rome(), 24).unwrap();
        // July in Rome is CEST (UTC+2).
        assert!(brief.contains("2026-07-15 16:30"), "{brief}");
        assert!(!brief.contains("14:30"), "{brief}");
    }

    #[test]
    fn separates_new_and_changed_items_and_orders_them() {
        let store = store();
        let now = Utc.with_ymd_and_hms(2026, 10, 28, 10, 0, 0).unwrap();
        insert_opportunity(
            &store,
            "portal",
            "Newer new",
            1,
            now - Duration::minutes(10),
        );
        insert_opportunity(
            &store,
            "portal",
            "Older new",
            1,
            now - Duration::minutes(20),
        );
        insert_opportunity(
            &store,
            "portal",
            "Changed item",
            3,
            now - Duration::minutes(5),
        );

        let brief = build_brief(&store, now, rome(), 24).unwrap();
        assert!(brief.contains("Changed (1)"), "{brief}");
        assert!(brief.contains("New (2)"), "{brief}");
        let changed_at = brief.find("Changed (1)").unwrap();
        let new_at = brief.find("New (2)").unwrap();
        assert!(changed_at < new_at, "{brief}");
        let newer_at = brief.find("Newer new").unwrap();
        let older_at = brief.find("Older new").unwrap();
        assert!(newer_at < older_at, "{brief}");
        assert!(!brief.contains("Old item"), "{brief}");

        // Items older than the lookback window are excluded.
        insert_opportunity(&store, "portal", "Old item", 1, now - Duration::hours(48));
        let brief = build_brief(&store, now, rome(), 24).unwrap();
        assert!(!brief.contains("Old item"), "{brief}");
    }

    #[test]
    fn reports_stale_and_failing_sources() {
        let old_store = store();
        let now = Utc.with_ymd_and_hms(2026, 10, 28, 10, 0, 0).unwrap();
        insert_source_run(
            &old_store,
            "stale",
            true,
            now - Duration::hours(72),
            Some(now - Duration::hours(72)),
        );
        insert_source_run(
            &old_store,
            "failing",
            false,
            now - Duration::hours(1),
            Some(now - Duration::hours(1)),
        );

        let brief = build_brief(&old_store, now, rome(), 24).unwrap();
        assert!(brief.contains("Source warnings (2)"), "{brief}");
        assert!(brief.contains("stale: no successful run since"), "{brief}");
        assert!(
            brief.contains("failing: no successful run recorded; last run failed at"),
            "{brief}"
        );

        drop(old_store);
        let fresh_store = store();
        insert_source_run(
            &fresh_store,
            "healthy",
            true,
            now - Duration::hours(1),
            Some(now - Duration::hours(1)),
        );
        let brief = build_brief(&fresh_store, now, rome(), 24).unwrap();
        assert!(!brief.contains("Source warnings"), "{brief}");
        assert!(brief.contains("No new items"), "{brief}");
    }
}
