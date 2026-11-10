//! Deterministic recovery enqueue when sources stay failed (Big Wave B4).
//!
//! When the latest `source_runs` row for any configured source has `ok = 0` and
//! is older than the configured threshold, enqueue **at most one** `watch` task
//! per user per UTC day — no LLM turn.

use chrono::{DateTime, Duration, Utc};

use crate::config::Config;
use crate::error::Result;
use crate::runtime::ScheduledPayload;
use crate::sources::effective_sources_for_user;
use crate::store::Store;

/// Scan every active user and enqueue capped recovery watch tasks.
pub fn enqueue_source_reliability_recoveries(
    store: &Store,
    config: &Config,
    now: DateTime<Utc>,
) -> Result<usize> {
    if config.watcher.reliability_max_per_day == 0 {
        return Ok(0);
    }
    let hours = config.watcher.reliability_failed_hours.max(1);
    let cutoff = now - Duration::hours(hours as i64);
    let mut enqueued = 0usize;
    for user_id in store.list_active_user_ids()? {
        if store.reliability_enqueue_count_today(&user_id, now)?
            >= config.watcher.reliability_max_per_day
        {
            continue;
        }
        let sources = match effective_sources_for_user(config, store, &user_id) {
            Ok(s) => s,
            Err(_) => continue,
        };
        let enabled_names: Vec<String> = sources
            .iter()
            .filter(|s| s.enabled)
            .map(|s| s.name.clone())
            .collect();
        if enabled_names.is_empty() {
            continue;
        }
        if !store.user_has_stale_failed_sources(&user_id, &enabled_names, cutoff)? {
            continue;
        }
        let payload = ScheduledPayload {
            conversation_id: "dir:main".into(),
            user_id: user_id.clone(),
        };
        store.enqueue_task(
            &user_id,
            "watch",
            "watcher",
            &serde_json::to_value(&payload)?,
            3,
            now,
        )?;
        store.record_reliability_enqueue(&user_id, now, None)?;
        let details = serde_json::json!({
            "reason": "source_run_failure",
            "failed_hours": hours,
        });
        store.append_event(&user_id, "watch.reliability_enqueue", &details)?;
        enqueued += 1;
    }
    Ok(enqueued)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::domain::new_id;
    use crate::store::Store;

    fn store() -> Store {
        Store::open_in_memory(8).unwrap()
    }

    fn config() -> Config {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .parent()
            .unwrap();
        Config::load(root).unwrap()
    }

    #[test]
    fn enqueues_once_per_day_cap() {
        let store = store();
        let admin = store.admin_user_id().unwrap();
        let config = config();
        let admin_sources = crate::config::load_sources(&config).unwrap();
        let Some(source_name) = admin_sources.first().map(|s| s.name.clone()) else {
            return;
        };
        let kind = admin_sources
            .first()
            .map(|s| s.kind.as_str())
            .unwrap_or("rss");
        store
            .upsert_user_source_overlay(&admin, &source_name, true, kind)
            .unwrap();
        let now = Utc::now();
        let old = (now - chrono::Duration::hours(72))
            .to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
        store
            .connection()
            .unwrap()
            .execute(
                "INSERT INTO source_runs(id, source, started_at, finished_at, ok, error, items_seen, user_id)
                 VALUES (?1, ?2, ?3, ?3, 0, 'synthetic', 0, ?4)",
                rusqlite::params![new_id(), source_name, old, admin],
            )
            .unwrap();

        let first = enqueue_source_reliability_recoveries(&store, &config, now).unwrap();
        assert_eq!(first, 1);
        let queued: i64 = store
            .connection()
            .unwrap()
            .query_row(
                "SELECT COUNT(*) FROM tasks WHERE user_id = ?1 AND kind = 'watch'",
                rusqlite::params![admin],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(queued, 1);

        let second = enqueue_source_reliability_recoveries(&store, &config, now).unwrap();
        assert_eq!(second, 0);
        let queued: i64 = store
            .connection()
            .unwrap()
            .query_row(
                "SELECT COUNT(*) FROM tasks WHERE user_id = ?1 AND kind = 'watch'",
                rusqlite::params![admin],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(queued, 1);
    }
}
