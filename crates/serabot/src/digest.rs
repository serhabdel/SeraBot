//! On-demand digest of new items from the caller's watched sources (v0.33.0).
//!
//! Items come from `opportunities`, where each row records when this user
//! first saw it. Source health comes from the latest `source_runs` row per
//! source. `GET /v1/digest` and the `/digest` Telegram command both use this
//! module, so they give the same answer.

use chrono::{DateTime, Duration, SecondsFormat, Utc};
use serde_json::{Value, json};

use crate::error::Result;
use crate::store::{DigestItem, Store};

/// Window used when the caller gives no hours.
pub const DEFAULT_HOURS: u32 = 24;
/// Longest window a digest covers (one week).
pub const MAX_HOURS: u32 = 168;
/// Most items one digest returns.
pub const MAX_ITEMS: usize = 50;
/// Items shown in a Telegram reply; the rest are counted, not listed.
const TELEGRAM_ITEMS: usize = 15;

/// Start of a window that reaches `hours` back from `now`, clamped to 1..=MAX_HOURS.
pub fn window_start(now: DateTime<Utc>, hours: u32) -> DateTime<Utc> {
    now - Duration::hours(i64::from(hours.clamp(1, MAX_HOURS)))
}

/// A timestamp in the stored format: RFC 3339 with milliseconds, which sorts as text.
pub fn stored_time(value: DateTime<Utc>) -> String {
    value.to_rfc3339_opts(SecondsFormat::Millis, true)
}

/// A digest for one user since a point in time.
pub struct Digest {
    pub since: DateTime<Utc>,
    pub items: Vec<DigestItem>,
    /// Sources whose latest run failed. Their errors are not repeated here.
    pub failing: Vec<String>,
}

/// Build the digest for `user_id` covering everything first seen after `since`.
pub fn build(store: &Store, user_id: &str, since: DateTime<Utc>) -> Result<Digest> {
    let items = store.items_since(user_id, &stored_time(since), MAX_ITEMS)?;
    let failing = store
        .list_source_run_status(user_id)?
        .into_iter()
        .filter(|run| !run.ok)
        .map(|run| run.source)
        .collect();
    Ok(Digest {
        since,
        items,
        failing,
    })
}

/// JSON body for `GET /v1/digest`.
pub fn to_json(digest: &Digest) -> Value {
    json!({
        "since": stored_time(digest.since),
        "count": digest.items.len(),
        "items": digest.items,
        "failing_sources": digest.failing,
    })
}

/// The hours argument of `/digest`: empty means the default, otherwise a whole
/// number from 1 to [`MAX_HOURS`]. Anything else is `None`.
pub fn parse_hours(arg: &str) -> Option<u32> {
    let arg = arg.trim();
    if arg.is_empty() {
        return Some(DEFAULT_HOURS);
    }
    let hours: u32 = arg.parse().ok()?;
    (1..=MAX_HOURS).contains(&hours).then_some(hours)
}

/// Plain-text reply for the `/digest [hours]` command.
pub fn command_reply(
    store: &Store,
    user_id: &str,
    arg: &str,
    now: DateTime<Utc>,
) -> Result<String> {
    let Some(hours) = parse_hours(arg) else {
        return Ok(format!(
            "Use /digest or /digest <hours>, with hours from 1 to {MAX_HOURS}."
        ));
    };
    let digest = build(store, user_id, window_start(now, hours))?;
    Ok(telegram_text(&digest, hours))
}

/// Render a digest as a short chat message.
pub fn telegram_text(digest: &Digest, hours: u32) -> String {
    let mut lines = Vec::new();
    if digest.items.is_empty() {
        lines.push(format!("No new items in the last {hours} hour(s)."));
    } else {
        lines.push(format!(
            "{} new item(s) in the last {hours} hour(s):",
            digest.items.len()
        ));
        for item in digest.items.iter().take(TELEGRAM_ITEMS) {
            let link = item
                .url
                .as_deref()
                .map(|url| format!(" {url}"))
                .unwrap_or_default();
            lines.push(format!("- {} ({}){link}", item.title, item.source));
        }
        if digest.items.len() > TELEGRAM_ITEMS {
            lines.push(format!(
                "…and {} more.",
                digest.items.len() - TELEGRAM_ITEMS
            ));
        }
    }
    if !digest.failing.is_empty() {
        lines.push(format!(
            "Failing sources (last run): {}",
            digest.failing.join(", ")
        ));
    }
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn item(source: &str, title: &str, url: Option<&str>) -> DigestItem {
        DigestItem {
            source: source.into(),
            title: title.into(),
            url: url.map(str::to_owned),
            first_seen: "2026-11-11T10:00:00.000Z".into(),
        }
    }

    #[test]
    fn hours_are_parsed_and_bounded() {
        assert_eq!(parse_hours(""), Some(DEFAULT_HOURS));
        assert_eq!(parse_hours(" 6 "), Some(6));
        assert_eq!(parse_hours("168"), Some(168));
        assert_eq!(parse_hours("0"), None);
        assert_eq!(parse_hours("169"), None);
        assert_eq!(parse_hours("two"), None);
    }

    #[test]
    fn window_is_clamped_to_one_week() {
        let now = Utc.with_ymd_and_hms(2026, 11, 11, 12, 0, 0).unwrap();
        assert_eq!(window_start(now, 24), now - Duration::hours(24));
        assert_eq!(window_start(now, 10_000), now - Duration::hours(168));
        assert_eq!(window_start(now, 0), now - Duration::hours(1));
    }

    #[test]
    fn telegram_text_lists_items_and_failing_sources() {
        let since = Utc.with_ymd_and_hms(2026, 11, 10, 12, 0, 0).unwrap();
        let digest = Digest {
            since,
            items: vec![
                item("news", "Grant opens", Some("https://example.invalid/a")),
                item("jobs", "Role posted", None),
            ],
            failing: vec!["council".into()],
        };
        let text = telegram_text(&digest, 24);
        assert!(text.starts_with("2 new item(s) in the last 24 hour(s):"));
        assert!(text.contains("- Grant opens (news) https://example.invalid/a"));
        assert!(text.contains("- Role posted (jobs)\nFailing"));
        assert!(text.ends_with("Failing sources (last run): council"));
    }

    #[test]
    fn empty_digest_says_so_and_caps_long_lists() {
        let since = Utc.with_ymd_and_hms(2026, 11, 10, 12, 0, 0).unwrap();
        let empty = Digest {
            since,
            items: Vec::new(),
            failing: Vec::new(),
        };
        assert_eq!(
            telegram_text(&empty, 6),
            "No new items in the last 6 hour(s)."
        );

        let many = Digest {
            since,
            items: (0..TELEGRAM_ITEMS + 4)
                .map(|n| item("news", &format!("Item {n}"), None))
                .collect(),
            failing: Vec::new(),
        };
        let text = telegram_text(&many, 24);
        assert!(text.contains("…and 4 more."));
        assert_eq!(text.matches("\n- ").count(), TELEGRAM_ITEMS);
    }
}
