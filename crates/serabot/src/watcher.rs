//! Source monitoring and opportunity extraction.
//!
//! Each watched source is fetched through [`SafeFetcher`], parsed into a small
//! bounded set of items, and upserted into `opportunities` / `deadlines` using
//! a stable SHA-256 key. A run is always recorded in `source_runs`, including
//! failures, so the morning brief can warn about broken sources.
//!
//! `last_seen` is bumped only for new or changed items. That keeps the brief's
//! "changed" window meaningful: merely re-observing identical content does not
//! make an old item look new again.

use std::collections::HashSet;
use std::io::Cursor;

use chrono::{
    DateTime, MappedLocalTime, NaiveDate, NaiveDateTime, NaiveTime, SecondsFormat, TimeZone, Utc,
};
use chrono_tz::Tz;
use reqwest::Url;
use rusqlite::{OptionalExtension, params};
use scraper::{ElementRef, Html, Selector};
use serde::Serialize;
use serde_json::json;
use unicode_normalization::UnicodeNormalization;
use unicode_normalization::char::is_combining_mark;

use crate::config::{Config, SourceConfig};
use crate::domain::{new_id, utc_now};
use crate::error::{Result, SeraError};
use crate::fetch::SafeFetcher;
use crate::store::Store;
use crate::util::{sha256_hex, truncate_chars};

/// Default item bound used by [`watch_all`].
pub const DEFAULT_MAX_ITEMS: usize = 200;

const MAX_SUMMARY_CHARS: usize = 800;
const MAX_EVIDENCE_CHARS: usize = 500;
const MAX_WARNINGS: usize = 50;
const MAX_ERROR_CHARS: usize = 1000;

/// Outcome of watching one source.
#[derive(Debug, Clone, Default, Serialize)]
pub struct WatchReport {
    pub source: String,
    /// Items first observed during this run.
    pub new: usize,
    /// Existing items whose title, summary, URL, status or deadline changed.
    pub changed: usize,
    /// Malformed items that were ignored, or one for a failed source run.
    pub failed: usize,
    /// Items extracted from the page (including malformed ones).
    pub items_seen: usize,
    /// False when the fetch or parse of the source itself failed.
    pub ok: bool,
    /// Non-fatal parse problems and the fatal source error, if any.
    pub warnings: Vec<String>,
}

impl WatchReport {
    fn warn(&mut self, message: impl Into<String>) {
        if self.warnings.len() < MAX_WARNINGS {
            self.warnings.push(message.into());
        }
    }
}

#[derive(Debug)]
struct RawItem {
    title: String,
    link: Option<String>,
    date_text: Option<String>,
    summary: String,
}

#[derive(Debug)]
struct ExistingOpportunity {
    id: String,
    title: String,
    summary: String,
    url: Option<String>,
    status: String,
}

#[derive(Debug)]
struct CurrentDeadline {
    due_date: String,
    due_time: Option<String>,
    precision: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ParsedDeadline {
    due_date: String,
    due_time: Option<String>,
    precision: String,
    source_text: String,
}

/// Watch every enabled source in order. A failure of one source is recorded
/// in its [`WatchReport`] and never stops the remaining sources.
pub async fn watch_all(
    store: &Store,
    fetcher: &SafeFetcher,
    sources: &[SourceConfig],
    config: &Config,
) -> Result<Vec<WatchReport>> {
    let mut reports = Vec::new();
    for source in sources.iter().filter(|source| source.enabled) {
        match watch_source(
            store,
            fetcher,
            source,
            &config.schedule.timezone,
            DEFAULT_MAX_ITEMS,
        )
        .await
        {
            Ok(report) => reports.push(report),
            Err(error) => {
                let mut report = WatchReport {
                    source: source.name.clone(),
                    failed: 1,
                    ..WatchReport::default()
                };
                report.warn(format!("cannot watch source: {error}"));
                reports.push(report);
            }
        }
    }
    Ok(reports)
}

/// Fetch, parse and persist a single source.
///
/// Source-level fetch/parse failures are recorded with `ok = 0` in
/// `source_runs` and reported via [`WatchReport::ok`] / `failed`; only database
/// failures (or an invalid configured timezone) are returned as `Err`.
pub async fn watch_source(
    store: &Store,
    fetcher: &SafeFetcher,
    source: &SourceConfig,
    timezone: &str,
    max_items: usize,
) -> Result<WatchReport> {
    let tz: Tz = timezone
        .parse()
        .map_err(|_| SeraError::Config(format!("invalid schedule timezone: {timezone}")))?;

    let run_id = new_id();
    let started_at = utc_now();
    {
        let connection = store.connection()?;
        connection.execute(
            "INSERT INTO source_runs(id, source, started_at, ok, items_seen)
             VALUES (?1, ?2, ?3, 0, 0)",
            params![run_id, source.name, utc_string(started_at)],
        )?;
    }

    let mut report = WatchReport {
        source: source.name.clone(),
        ..WatchReport::default()
    };

    let page = match fetcher.fetch(&source.url).await {
        Ok(page) => page,
        Err(error) => {
            let message = format!("fetch failed: {error}");
            finish_source_run(store, &run_id, false, 0, Some(&message))?;
            report.failed = 1;
            report.warn(message);
            return Ok(report);
        }
    };

    if !(200..300).contains(&page.status) {
        let message = format!("unexpected HTTP status {}", page.status);
        finish_source_run(store, &run_id, false, 0, Some(&message))?;
        report.failed = 1;
        report.warn(message);
        return Ok(report);
    }

    let raw_items = match source.kind.as_str() {
        "rss" => parse_rss(&page.body, max_items),
        "html" => parse_html(&page.body, source, max_items),
        other => Err(SeraError::Config(format!(
            "source {}: unsupported kind {other}",
            source.name
        ))),
    };
    let raw_items = match raw_items {
        Ok(items) => items,
        Err(error) => {
            let message = format!("parse failed: {error}");
            finish_source_run(store, &run_id, false, 0, Some(&message))?;
            report.failed = 1;
            report.warn(message);
            return Ok(report);
        }
    };

    report.items_seen = raw_items.len();
    let base = match Url::parse(&page.final_url) {
        Ok(url) => url,
        Err(error) => {
            let message = format!("invalid final URL {}: {error}", page.final_url);
            finish_source_run(store, &run_id, false, 0, Some(&message))?;
            report.failed = 1;
            report.warn(message);
            return Ok(report);
        }
    };

    let mut seen = HashSet::new();
    for item in raw_items.into_iter().take(max_items) {
        ingest_item(store, source, tz, &base, item, &mut seen, &mut report)?;
    }

    finish_source_run(store, &run_id, true, report.items_seen, None)?;
    report.ok = true;
    Ok(report)
}

fn ingest_item(
    store: &Store,
    source: &SourceConfig,
    tz: Tz,
    base: &Url,
    item: RawItem,
    seen: &mut HashSet<String>,
    report: &mut WatchReport,
) -> Result<()> {
    let title = truncate_chars(&normalize_ws(&item.title), 500);
    if title.is_empty() {
        report.failed += 1;
        report.warn(format!(
            "ignored an item without a title from {}",
            source.name
        ));
        return Ok(());
    }

    let link = item
        .link
        .as_deref()
        .and_then(|href| resolve_link(base, href));
    if item.link.is_some() && link.is_none() {
        report.warn(format!("ignored an invalid link for item {title:?}"));
    }

    let external_id = match &link {
        Some(link) => sha256_hex(link.as_bytes()),
        None => sha256_hex(normalized_key(&title).as_bytes()),
    };
    if !seen.insert(external_id.clone()) {
        report.warn(format!("ignored a duplicate item {title:?}"));
        return Ok(());
    }

    let summary = truncate_chars(&normalize_ws(&item.summary), MAX_SUMMARY_CHARS);
    let date_text = item
        .date_text
        .as_deref()
        .map(normalize_ws)
        .filter(|text| !text.is_empty());
    let parsed_deadline = date_text
        .as_deref()
        .and_then(|text| parse_deadline(text, tz));
    if date_text.is_some() && parsed_deadline.is_none() {
        report.warn(format!(
            "could not parse deadline {:?} for item {title:?}",
            truncate_chars(date_text.as_deref().unwrap_or_default(), 120)
        ));
    }

    let withdrawn = looks_withdrawn(&title);
    let parsed_deadline = if withdrawn { None } else { parsed_deadline };

    let connection = store.connection()?;
    let existing: Option<ExistingOpportunity> = connection
        .query_row(
            "SELECT id, title, summary, url, status
             FROM opportunities WHERE source = ?1 AND external_id = ?2",
            params![source.name, external_id],
            |row| {
                Ok(ExistingOpportunity {
                    id: row.get(0)?,
                    title: row.get(1)?,
                    summary: row.get(2)?,
                    url: row.get(3)?,
                    status: row.get(4)?,
                })
            },
        )
        .optional()?;

    let observed_at = utc_string(utc_now());
    let evidence = json!({
        "url": link,
        "observed_at": observed_at,
        "item": truncate_chars(if summary.is_empty() { &title } else { &summary }, MAX_EVIDENCE_CHARS),
    })
    .to_string();

    let Some(existing) = existing else {
        let opportunity_id = new_id();
        let status = if withdrawn { "withdrawn" } else { "open" };
        connection.execute(
            "INSERT INTO opportunities(
                id, source, external_id, title, summary, url, status, evidence,
                revision, first_seen, last_seen
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, 1, ?9, ?9)",
            params![
                opportunity_id,
                source.name,
                external_id,
                title,
                summary,
                link,
                status,
                evidence,
                observed_at
            ],
        )?;
        if let Some(deadline) = &parsed_deadline {
            insert_deadline(&connection, &opportunity_id, deadline, tz, &observed_at)?;
        }
        report.new += 1;
        return Ok(());
    };

    let previous_deadline = active_deadline(&connection, &existing.id)?;
    let deadline_changed = match (&parsed_deadline, &previous_deadline) {
        (Some(new), Some(old)) => {
            new.due_date != old.due_date
                || new.due_time != old.due_time
                || new.precision != old.precision
        }
        (Some(_), None) => true,
        (None, _) => false,
    };
    let status = if withdrawn {
        "withdrawn"
    } else if existing.status == "withdrawn" {
        "open"
    } else {
        existing.status.as_str()
    };
    let changed = existing.title != title
        || existing.summary != summary
        || existing.url != link
        || existing.status != status
        || deadline_changed;

    if !changed {
        return Ok(());
    }

    connection.execute(
        "UPDATE opportunities
         SET title = ?1, summary = ?2, url = ?3, status = ?4, evidence = ?5,
             revision = revision + 1, last_seen = ?6
         WHERE id = ?7",
        params![
            title,
            summary,
            link,
            status,
            evidence,
            observed_at,
            existing.id
        ],
    )?;

    if withdrawn {
        supersede_deadlines(&connection, &existing.id)?;
    } else if deadline_changed {
        supersede_deadlines(&connection, &existing.id)?;
        if let Some(deadline) = &parsed_deadline {
            insert_deadline(&connection, &existing.id, deadline, tz, &observed_at)?;
        }
    }

    report.changed += 1;
    Ok(())
}

fn active_deadline(
    connection: &rusqlite::Connection,
    opportunity_id: &str,
) -> Result<Option<CurrentDeadline>> {
    Ok(connection
        .query_row(
            "SELECT due_date, due_time, precision
             FROM deadlines
             WHERE opportunity_id = ?1 AND status <> 'superseded'
             ORDER BY observed_at DESC LIMIT 1",
            params![opportunity_id],
            |row| {
                Ok(CurrentDeadline {
                    due_date: row.get(0)?,
                    due_time: row.get(1)?,
                    precision: row.get(2)?,
                })
            },
        )
        .optional()?)
}

fn insert_deadline(
    connection: &rusqlite::Connection,
    opportunity_id: &str,
    deadline: &ParsedDeadline,
    tz: Tz,
    observed_at: &str,
) -> Result<()> {
    connection.execute(
        "INSERT INTO deadlines(
            id, opportunity_id, due_date, due_time, timezone, precision,
            source_text, status, observed_at
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 'upcoming', ?8)",
        params![
            new_id(),
            opportunity_id,
            deadline.due_date,
            deadline.due_time,
            tz.name(),
            deadline.precision,
            deadline.source_text,
            observed_at
        ],
    )?;
    Ok(())
}

fn supersede_deadlines(connection: &rusqlite::Connection, opportunity_id: &str) -> Result<()> {
    connection.execute(
        "UPDATE deadlines SET status = 'superseded'
         WHERE opportunity_id = ?1 AND status <> 'superseded'",
        params![opportunity_id],
    )?;
    Ok(())
}

fn finish_source_run(
    store: &Store,
    run_id: &str,
    ok: bool,
    items_seen: usize,
    error: Option<&str>,
) -> Result<()> {
    let connection = store.connection()?;
    connection.execute(
        "UPDATE source_runs
         SET finished_at = ?1, ok = ?2, items_seen = ?3, error = ?4
         WHERE id = ?5",
        params![
            utc_string(utc_now()),
            ok as i64,
            items_seen as i64,
            error.map(|value| truncate_chars(value, MAX_ERROR_CHARS)),
            run_id
        ],
    )?;
    Ok(())
}

fn parse_html(body: &str, source: &SourceConfig, max_items: usize) -> Result<Vec<RawItem>> {
    let document = Html::parse_document(body);
    let item_selector = parse_selector(&source.item_selector, "item_selector", source)?;
    let title_selector = parse_selector(&source.title_selector, "title_selector", source)?;
    let date_selector = parse_selector(&source.date_selector, "date_selector", source)?;
    let link_selector = parse_selector(&source.link_selector, "link_selector", source)?;
    let paragraph_selector = Selector::parse("p").ok();

    let mut items = Vec::new();
    for element in document.select(&item_selector).take(max_items) {
        let title = element
            .select(&title_selector)
            .next()
            .map(|element| element_text(&element))
            .unwrap_or_default();
        let link = element
            .select(&link_selector)
            .next()
            .and_then(|element| element.value().attr("href"))
            .or_else(|| element.value().attr("href"))
            .map(str::to_owned);
        let date_text = element.select(&date_selector).next().map(|element| {
            element
                .value()
                .attr("datetime")
                .map(str::to_owned)
                .unwrap_or_else(|| element_text(&element))
        });

        let mut summary = paragraph_selector
            .as_ref()
            .map(|selector| {
                element
                    .select(selector)
                    .map(|element| element_text(&element))
                    .filter(|text| !text.is_empty())
                    .collect::<Vec<_>>()
                    .join(" ")
            })
            .unwrap_or_default();
        if summary.is_empty() {
            summary = element_text(&element);
        }
        let title_trimmed = title.trim();
        if !title_trimmed.is_empty() {
            if let Some(rest) = summary.strip_prefix(title_trimmed).map(str::to_owned) {
                summary = rest.trim().to_owned();
            }
        }

        items.push(RawItem {
            title,
            link,
            date_text: date_text.filter(|text| !text.is_empty()),
            summary,
        });
    }
    Ok(items)
}

fn parse_rss(body: &str, max_items: usize) -> Result<Vec<RawItem>> {
    let channel = rss::Channel::read_from(Cursor::new(body.as_bytes()))
        .map_err(|error| SeraError::External(format!("invalid RSS: {error}")))?;
    let mut items = Vec::new();
    for item in channel.items().iter().take(max_items) {
        items.push(RawItem {
            title: truncate_chars(item.title().unwrap_or_default().trim(), 500),
            link: item
                .link()
                .map(str::trim)
                .filter(|link| !link.is_empty())
                .map(str::to_owned),
            date_text: item
                .pub_date()
                .map(str::trim)
                .filter(|date| !date.is_empty())
                .map(str::to_owned),
            summary: item.description().map(strip_html).unwrap_or_default(),
        });
    }
    Ok(items)
}

fn parse_selector(value: &str, field: &str, source: &SourceConfig) -> Result<Selector> {
    Selector::parse(value).map_err(|error| {
        SeraError::Config(format!(
            "source {}: invalid {field} {value:?}: {error}",
            source.name
        ))
    })
}

fn element_text(element: &ElementRef<'_>) -> String {
    truncate_chars(
        &normalize_ws(&element.text().collect::<Vec<_>>().join(" ")),
        4000,
    )
}

fn strip_html(value: &str) -> String {
    if !value.contains('<') {
        return truncate_chars(&normalize_ws(value), 4000);
    }
    let fragment = Html::parse_fragment(value);
    truncate_chars(
        &normalize_ws(&fragment.root_element().text().collect::<Vec<_>>().join(" ")),
        4000,
    )
}

fn resolve_link(base: &Url, href: &str) -> Option<String> {
    let mut joined = base.join(href.trim()).ok()?;
    if !matches!(joined.scheme(), "http" | "https") {
        return None;
    }
    joined.set_fragment(None);
    Some(joined.to_string())
}

fn normalize_ws(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn normalized_key(value: &str) -> String {
    normalize_ws(value).to_lowercase()
}

/// Conservative withdrawal detection: only the title is inspected so an
/// unrelated phrase in a long summary cannot silently hide an opportunity.
fn looks_withdrawn(title: &str) -> bool {
    const MARKERS: [&str; 7] = [
        "withdrawn",
        "cancelled",
        "canceled",
        "annullato",
        "annullata",
        "ritirato",
        "revocato",
    ];
    title
        .split(|character: char| !character.is_alphanumeric())
        .map(str::to_ascii_lowercase)
        .any(|word| MARKERS.contains(&word.as_str()))
}

fn utc_string(value: DateTime<Utc>) -> String {
    value.to_rfc3339_opts(SecondsFormat::Millis, true)
}

fn resolve_local(naive: NaiveDateTime, tz: Tz) -> Option<DateTime<Tz>> {
    match tz.from_local_datetime(&naive) {
        MappedLocalTime::Single(value) => Some(value),
        // Ambiguous local times (fall back) resolve to the earliest instant.
        MappedLocalTime::Ambiguous(earliest, latest) => Some(earliest.min(latest)),
        // Nonexistent local times (spring forward) shift forward by the gap.
        // The gap is one hour in the zones this bot targets; try a few hours
        // to stay safe for unusual transitions.
        MappedLocalTime::None => {
            for hours in 1..=4 {
                let shifted = naive.checked_add_signed(chrono::Duration::hours(hours))?;
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

/// Parse a deadline string without inventing a time of day.
///
/// Supported shapes: RFC 3339, RFC 2822 (RSS `pubDate`), ISO date and
/// date-time, `DD/MM/YYYY`, `DD.MM.YYYY`, `DD-MM-YYYY`, and day + month name +
/// year in English, French or Italian (including `1er` and abbreviations).
/// Date-times with an explicit offset are converted to `tz`.
fn parse_deadline(text: &str, tz: Tz) -> Option<ParsedDeadline> {
    let source_text = normalize_ws(text);
    if source_text.is_empty() || source_text.chars().count() > 200 {
        return None;
    }

    if let Ok(value) = DateTime::parse_from_rfc3339(&source_text) {
        return Some(from_offset(value, tz, &source_text));
    }
    if let Ok(value) = DateTime::parse_from_rfc2822(&source_text) {
        return Some(from_offset(value, tz, &source_text));
    }

    const DATETIME_FORMATS: [&str; 11] = [
        "%Y-%m-%dT%H:%M:%S%.f",
        "%Y-%m-%dT%H:%M",
        "%Y-%m-%d %H:%M:%S",
        "%Y-%m-%d %H:%M",
        "%d/%m/%Y %H:%M:%S",
        "%d/%m/%Y %H:%M",
        "%d.%m.%Y %H:%M",
        "%d-%m-%Y %H:%M",
        "%d %B %Y %H:%M",
        "%d %b %Y %H:%M",
        "%d/%m/%Y %H:%M:%S%.f",
    ];
    for format in DATETIME_FORMATS {
        if let Ok(naive) = NaiveDateTime::parse_from_str(&source_text, format) {
            if let Some(local) = resolve_local(naive, tz) {
                return Some(from_local(local, &source_text));
            }
        }
    }

    const DATE_FORMATS: [&str; 6] = [
        "%Y-%m-%d", "%d/%m/%Y", "%d.%m.%Y", "%d-%m-%Y", "%d %B %Y", "%d %b %Y",
    ];
    for format in DATE_FORMATS {
        if let Ok(date) = NaiveDate::parse_from_str(&source_text, format) {
            return Some(from_date(date, &source_text));
        }
    }

    parse_textual_date(&source_text, tz)
}

fn from_offset(value: DateTime<chrono::FixedOffset>, tz: Tz, source_text: &str) -> ParsedDeadline {
    let local = value.with_timezone(&tz);
    from_local(local, source_text)
}

fn from_local(value: DateTime<Tz>, source_text: &str) -> ParsedDeadline {
    ParsedDeadline {
        due_date: value.format("%Y-%m-%d").to_string(),
        due_time: Some(value.format("%H:%M").to_string()),
        precision: "datetime".to_owned(),
        source_text: source_text.to_owned(),
    }
}

fn from_date(value: NaiveDate, source_text: &str) -> ParsedDeadline {
    ParsedDeadline {
        due_date: value.format("%Y-%m-%d").to_string(),
        due_time: None,
        precision: "date".to_owned(),
        source_text: source_text.to_owned(),
    }
}

fn parse_textual_date(text: &str, tz: Tz) -> Option<ParsedDeadline> {
    let folded = fold_accents(&text.to_lowercase()).replace("1er ", "1 ");
    let tokens: Vec<&str> = folded.split_whitespace().collect();
    for (index, token) in tokens.iter().enumerate() {
        let Some(month) = month_number(token) else {
            continue;
        };
        let day = index
            .checked_sub(1)
            .and_then(|index| leading_u32(tokens[index]))?;
        let year_token = tokens.get(index + 1)?;
        let year_text: String = year_token
            .chars()
            .filter(|character| character.is_ascii_digit())
            .collect();
        if year_text.len() != 4 {
            return None;
        }
        let year: i32 = year_text.parse().ok()?;
        if !(1900..=2200).contains(&year) {
            return None;
        }
        let date = NaiveDate::from_ymd_opt(year, month, day)?;
        let time = tokens
            .iter()
            .skip(index + 2)
            .take(3)
            .find_map(|token| parse_hhmm(token));
        if let Some(time) = time {
            let local = resolve_local(date.and_time(time), tz)?;
            return Some(from_local(local, text));
        }
        return Some(from_date(date, text));
    }
    None
}

fn leading_u32(value: &str) -> Option<u32> {
    let digits: String = value
        .chars()
        .take_while(|character| character.is_ascii_digit())
        .collect();
    if digits.is_empty() {
        None
    } else {
        digits.parse().ok()
    }
}

fn parse_hhmm(value: &str) -> Option<NaiveTime> {
    let trimmed = value.trim_matches(|character: char| {
        matches!(character, ',' | '.' | ';' | ':' | '-' | '(' | ')')
    });
    if trimmed.is_empty() {
        return None;
    }
    let normalized = trimmed.replace(['h', 'H'], ":");
    let normalized = normalized.trim_end_matches(':');
    let parts: Vec<&str> = normalized.split(':').collect();
    if parts.len() != 2 {
        return None;
    }
    let hour: u32 = parts[0].parse().ok()?;
    let minute: u32 = parts[1].parse().ok()?;
    NaiveTime::from_hms_opt(hour, minute, 0)
}

fn fold_accents(value: &str) -> String {
    value
        .nfd()
        .filter(|character| !is_combining_mark(*character))
        .collect()
}

fn month_number(token: &str) -> Option<u32> {
    let token = token.trim_matches(|character: char| !character.is_alphabetic());
    let month = match token {
        "janvier" | "janv" | "gennaio" | "gen" | "january" | "jan" => 1,
        "fevrier" | "fevr" | "febbraio" | "february" | "feb" => 2,
        "mars" | "marzo" | "mar" | "march" => 3,
        "avril" | "avr" | "aprile" | "apr" | "april" => 4,
        "mai" | "maggio" | "mag" | "may" => 5,
        "juin" | "giugno" | "giu" | "june" | "jun" => 6,
        "juillet" | "juil" | "luglio" | "lug" | "july" | "jul" => 7,
        "aout" | "agosto" | "ago" | "august" | "aug" => 8,
        "septembre" | "sept" | "settembre" | "sett" | "set" | "september" | "sep" => 9,
        "octobre" | "oct" | "ottobre" | "ott" | "october" => 10,
        "novembre" | "nov" | "november" => 11,
        "decembre" | "dec" | "dicembre" | "dic" | "december" => 12,
        _ => return None,
    };
    Some(month)
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    const HTML: &str = r#"<!doctype html><html><body>
        <article>
            <h2>Tender A</h2>
            <a href="/tender/a">Details A</a>
            <time datetime="28/10/2026">28 October 2026</time>
            <p>Summary A.</p>
        </article>
        <article>
            <h2>Tender B</h2>
            <a href="https://example.com/b">Details B</a>
            <time>2026-12-01T14:30:00Z</time>
            <p>Summary B.</p>
        </article>
    </body></html>"#;

    const RSS: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
        <rss version="2.0"><channel>
            <title>Feed</title><link>http://localhost/</link>
            <item>
                <title>Grant A</title>
                <link>http://localhost/grant-a</link>
                <pubDate>Wed, 28 Oct 2026 08:00:00 GMT</pubDate>
                <description><![CDATA[<p>Description A</p>]]></description>
            </item>
            <item>
                <title>Grant B</title>
                <link>/grant-b</link>
                <description>Description B</description>
            </item>
        </channel></rss>"#;

    fn store() -> Store {
        Store::open_in_memory(384).unwrap()
    }

    fn fetcher() -> SafeFetcher {
        SafeFetcher::new("SeraBot-test/0.1", 5, 2 * 1024 * 1024)
            .unwrap()
            .allow_insecure_localhost()
    }

    fn server_url(server: &MockServer, path: &str) -> String {
        let uri = Url::parse(&server.uri()).unwrap();
        format!("http://localhost:{}{}", uri.port().unwrap(), path)
    }

    fn source(name: &str, url: String, kind: &str) -> SourceConfig {
        SourceConfig {
            name: name.to_owned(),
            url,
            kind: kind.to_owned(),
            enabled: true,
            item_selector: "article".into(),
            title_selector: "h2".into(),
            date_selector: "time".into(),
            link_selector: "a[href]".into(),
        }
    }

    async fn mount_html(server: &MockServer, mock_path: &str, body: &str) {
        Mock::given(method("GET"))
            .and(path(mock_path))
            .respond_with(ResponseTemplate::new(200).set_body_raw(body.to_owned(), "text/html"))
            .mount(server)
            .await;
    }

    #[tokio::test]
    async fn first_run_creates_items_and_date_only_deadlines() {
        let server = MockServer::start().await;
        mount_html(&server, "/tenders", HTML).await;
        let store = store();
        let source = source("portal", server_url(&server, "/tenders"), "html");

        let report = watch_source(&store, &fetcher(), &source, "Europe/Rome", 10)
            .await
            .unwrap();
        assert!(report.ok);
        assert_eq!(report.new, 2);
        assert_eq!(report.changed, 0);
        assert_eq!(report.items_seen, 2);
        assert_eq!(report.failed, 0);

        let connection = store.connection().unwrap();
        let count: i64 = connection
            .query_row("SELECT COUNT(*) FROM opportunities", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 2);

        // Date-only: no invented due_time, precision date, original text kept.
        let (due_date, due_time, precision, timezone, source_text): (
            String,
            Option<String>,
            String,
            String,
            String,
        ) = connection
            .query_row(
                "SELECT due_date, due_time, precision, timezone, source_text
                 FROM deadlines ORDER BY due_date ASC LIMIT 1",
                [],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                    ))
                },
            )
            .unwrap();
        assert_eq!(due_date, "2026-10-28");
        assert_eq!(due_time, None);
        assert_eq!(precision, "date");
        assert_eq!(timezone, "Europe/Rome");
        assert_eq!(source_text, "28/10/2026");

        // Explicit UTC datetime is converted to the configured timezone
        // (December is CET, so 14:30 UTC becomes 15:30).
        let (due_date, due_time, precision): (String, Option<String>, String) = connection
            .query_row(
                "SELECT due_date, due_time, precision FROM deadlines
                 WHERE due_date = '2026-12-01'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(due_date, "2026-12-01");
        assert_eq!(due_time.as_deref(), Some("15:30"));
        assert_eq!(precision, "datetime");
    }

    #[tokio::test]
    async fn unchanged_items_do_not_increment_revision() {
        let server = MockServer::start().await;
        mount_html(&server, "/tenders", HTML).await;
        let store = store();
        let source = source("portal", server_url(&server, "/tenders"), "html");

        watch_source(&store, &fetcher(), &source, "Europe/Rome", 10)
            .await
            .unwrap();
        let second = watch_source(&store, &fetcher(), &source, "Europe/Rome", 10)
            .await
            .unwrap();

        assert_eq!(second.new, 0);
        assert_eq!(second.changed, 0);
        assert_eq!(second.items_seen, 2);

        let connection = store.connection().unwrap();
        let revisions: Vec<i64> = {
            let mut statement = connection
                .prepare("SELECT revision FROM opportunities ORDER BY title")
                .unwrap();
            let rows = statement.query_map([], |row| row.get(0)).unwrap();
            rows.collect::<std::result::Result<_, _>>().unwrap()
        };
        assert_eq!(revisions, vec![1, 1]);
    }

    #[tokio::test]
    async fn changed_item_increments_revision_and_supersedes_deadline() {
        let server = MockServer::start().await;
        mount_html(
            &server,
            "/v1",
            r#"<article><h2>Tender A</h2><a href="/tender/a">Details</a>
               <time>2026-10-28</time><p>Summary A.</p></article>"#,
        )
        .await;
        mount_html(
            &server,
            "/v2",
            r#"<article><h2>Tender A updated</h2><a href="/tender/a">Details</a>
               <time>2026-11-05</time><p>Summary A changed.</p></article>"#,
        )
        .await;
        let store = store();
        let mut source = source("portal", server_url(&server, "/v1"), "html");
        watch_source(&store, &fetcher(), &source, "Europe/Rome", 10)
            .await
            .unwrap();

        source.url = server_url(&server, "/v2");
        let report = watch_source(&store, &fetcher(), &source, "Europe/Rome", 10)
            .await
            .unwrap();
        assert_eq!(report.new, 0);
        assert_eq!(report.changed, 1);

        let connection = store.connection().unwrap();
        let (title, revision): (String, i64) = connection
            .query_row(
                "SELECT title, revision FROM opportunities WHERE source = 'portal'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(title, "Tender A updated");
        assert_eq!(revision, 2);

        let statuses: Vec<(String, String)> = {
            let mut statement = connection
                .prepare("SELECT due_date, status FROM deadlines ORDER BY due_date")
                .unwrap();
            let rows = statement
                .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
                .unwrap();
            rows.collect::<std::result::Result<_, _>>().unwrap()
        };
        assert_eq!(
            statuses,
            vec![
                ("2026-10-28".to_owned(), "superseded".to_owned()),
                ("2026-11-05".to_owned(), "upcoming".to_owned()),
            ]
        );
    }

    #[tokio::test]
    async fn malformed_date_warns_but_keeps_item() {
        let server = MockServer::start().await;
        mount_html(
            &server,
            "/tenders",
            r#"<article><h2>Tender C</h2><a href="/tender/c">Details</a>
               <time>sometime soon</time><p>Summary C.</p></article>"#,
        )
        .await;
        let store = store();
        let source = source("portal", server_url(&server, "/tenders"), "html");

        let report = watch_source(&store, &fetcher(), &source, "Europe/Rome", 10)
            .await
            .unwrap();
        assert_eq!(report.new, 1);
        assert_eq!(report.failed, 0);
        assert!(
            report
                .warnings
                .iter()
                .any(|warning| warning.contains("could not parse deadline")),
            "{:?}",
            report.warnings
        );

        let connection = store.connection().unwrap();
        let deadlines: i64 = connection
            .query_row("SELECT COUNT(*) FROM deadlines", [], |row| row.get(0))
            .unwrap();
        assert_eq!(deadlines, 0);
    }

    #[tokio::test]
    async fn rss_items_and_rfc2822_dates_are_parsed() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/feed"))
            .respond_with(
                ResponseTemplate::new(200).set_body_raw(RSS.to_owned(), "application/rss+xml"),
            )
            .mount(&server)
            .await;
        let store = store();
        let source = source("feed", server_url(&server, "/feed"), "rss");

        let report = watch_source(&store, &fetcher(), &source, "Europe/Rome", 10)
            .await
            .unwrap();
        assert!(report.ok);
        assert_eq!(report.new, 2);
        assert_eq!(report.failed, 0);

        let connection = store.connection().unwrap();
        let (due_date, due_time, precision): (String, Option<String>, String) = connection
            .query_row(
                "SELECT due_date, due_time, precision FROM deadlines",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(due_date, "2026-10-28");
        assert_eq!(due_time.as_deref(), Some("09:00"));
        assert_eq!(precision, "datetime");

        let summary: String = connection
            .query_row(
                "SELECT summary FROM opportunities WHERE title = 'Grant A'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(summary, "Description A");
    }

    #[tokio::test]
    async fn source_failure_is_recorded() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/broken"))
            .respond_with(ResponseTemplate::new(500).set_body_string("boom"))
            .mount(&server)
            .await;
        let store = store();
        let source = source("portal", server_url(&server, "/broken"), "html");

        let report = watch_source(&store, &fetcher(), &source, "Europe/Rome", 10)
            .await
            .unwrap();
        assert!(!report.ok);
        assert_eq!(report.failed, 1);
        assert!(!report.warnings.is_empty());

        let connection = store.connection().unwrap();
        let (ok, error, finished): (i64, Option<String>, Option<String>) = connection
            .query_row(
                "SELECT ok, error, finished_at FROM source_runs WHERE source = 'portal'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(ok, 0);
        assert!(error.unwrap_or_default().contains("500"));
        assert!(finished.is_some());
    }

    #[test]
    fn textual_dates_in_french_italian_and_english() {
        let tz: Tz = "Europe/Rome".parse().unwrap();
        let french = parse_deadline("1er février 2027", tz).unwrap();
        assert_eq!(french.due_date, "2027-02-01");
        assert_eq!(french.due_time, None);
        assert_eq!(french.precision, "date");

        let italian = parse_deadline("12 gennaio 2027 14:30", tz).unwrap();
        assert_eq!(italian.due_date, "2027-01-12");
        assert_eq!(italian.due_time.as_deref(), Some("14:30"));
        assert_eq!(italian.precision, "datetime");

        let english = parse_deadline("28 October 2026", tz).unwrap();
        assert_eq!(english.due_date, "2026-10-28");
        assert!(english.due_time.is_none());
    }
}
