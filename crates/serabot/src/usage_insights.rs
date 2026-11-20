//! Usage insights (0.67): the numbers behind the Usage tab's new cards.
//!
//! - **Channels:** turns and tokens by where they started: desktop, mobile,
//!   Telegram, WhatsApp, the API, or automation (schedules and watches).
//! - **Tunnel:** the desktop network relay's traffic per day and per host,
//!   from its per-tunnel audit rows.
//! - **Models:** calls, average and p95 latency, share sent through the
//!   desktop, and failovers away from each model.
//! - **Server:** a 24-hour history of CPU and memory, sampled every 5 minutes
//!   in memory (nothing stored).
//!
//! Counts and sizes only; never message text. Every query is scoped to one
//! member unless the admin asks for everyone.

use std::collections::{BTreeMap, VecDeque};
use std::sync::Mutex;
use std::time::Duration;

use chrono::{DateTime, Utc};
use rusqlite::params;
use serde::Serialize;

use crate::error::Result;
use crate::store::Store;

/// The client a request came from: an explicit `X-SeraBot-Client` header
/// (`desktop` or `mobile`), the mobile app's HTTP client signature, or `api`.
pub fn client_from_headers(headers: &axum::http::HeaderMap) -> &'static str {
    let named = headers
        .get("x-serabot-client")
        .and_then(|value| value.to_str().ok())
        .map(|value| value.trim().to_ascii_lowercase());
    match named.as_deref() {
        Some("desktop") => return "desktop",
        Some("mobile") => return "mobile",
        _ => {}
    }
    let agent = headers
        .get(axum::http::header::USER_AGENT)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default();
    if agent.starts_with("Dart/") || agent.contains("serabot-mobile") {
        "mobile"
    } else {
        "api"
    }
}

#[derive(Clone, Debug, Default, Serialize, PartialEq, Eq)]
pub struct ChannelUsage {
    pub channel: String,
    pub turns: i64,
    pub tokens: i64,
    pub last_at: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, PartialEq, Eq)]
pub struct TunnelDay {
    pub day: String,
    pub tunnels: i64,
    pub bytes_to_provider: i64,
    pub bytes_from_provider: i64,
}

#[derive(Clone, Debug, Default, Serialize, PartialEq, Eq)]
pub struct TunnelHost {
    pub host: String,
    pub tunnels: i64,
    pub bytes: i64,
    pub avg_seconds: i64,
}

#[derive(Clone, Debug, Default, Serialize, PartialEq)]
pub struct TunnelUsage {
    pub tunnels: i64,
    pub bytes_to_provider: i64,
    pub bytes_from_provider: i64,
    pub per_day: Vec<TunnelDay>,
    pub per_host: Vec<TunnelHost>,
    /// Model calls that went through the desktop, and all model calls.
    pub relayed_calls: i64,
    pub total_calls: i64,
}

#[derive(Clone, Debug, Default, Serialize, PartialEq, Eq)]
pub struct ModelHealth {
    pub provider: String,
    pub model: String,
    pub calls: i64,
    pub avg_latency_ms: i64,
    pub p95_latency_ms: i64,
    pub relayed_calls: i64,
    /// Times a turn moved away from this model to the next route.
    pub failovers: i64,
}

impl Store {
    /// Remember which client started a turn.
    pub fn record_client_turn(&self, user_id: &str, task_id: &str, client: &str) -> Result<()> {
        let client = match client {
            "desktop" | "mobile" => client,
            _ => "api",
        };
        let connection = self.connection()?;
        connection.execute(
            "INSERT INTO client_turns(task_id, user_id, client, created_at)
             VALUES (?1, ?2, ?3, ?4) ON CONFLICT(task_id) DO NOTHING",
            params![task_id, user_id, client, Utc::now().to_rfc3339()],
        )?;
        Ok(())
    }

    /// Turns and tokens per channel since `since`. `user_id = None` is every
    /// member (admin only, checked by the caller).
    pub fn channel_usage(
        &self,
        user_id: Option<&str>,
        since: DateTime<Utc>,
    ) -> Result<Vec<ChannelUsage>> {
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            "SELECT
                COALESCE(ct.client,
                    CASE c.origin_channel
                        WHEN 'telegram' THEN 'telegram'
                        WHEN 'whatsapp_link' THEN 'whatsapp'
                        WHEN 'app' THEN 'app'
                    END,
                    'automation') AS channel,
                COUNT(DISTINCT t.id),
                COALESCE(SUM((SELECT SUM(CAST(json_extract(a.details_json, '$.usage.total_tokens') AS INTEGER))
                              FROM audit_log a WHERE a.task_id = t.id AND a.event = 'model.used')), 0),
                MAX(t.created_at)
             FROM tasks t
             LEFT JOIN client_turns ct ON ct.task_id = t.id AND ct.user_id = t.user_id
             LEFT JOIN conversations c
                ON c.id = json_extract(t.payload_json, '$.conversation_id') AND c.user_id = t.user_id
             WHERE t.created_at >= ?1 AND (?2 IS NULL OR t.user_id = ?2)
             GROUP BY channel
             ORDER BY 2 DESC",
        )?;
        let rows = statement.query_map(params![since.to_rfc3339(), user_id], |row| {
            Ok(ChannelUsage {
                channel: row.get(0)?,
                turns: row.get(1)?,
                tokens: row.get(2)?,
                last_at: row.get(3)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// The desktop relay's traffic since `since`.
    pub fn tunnel_usage(&self, user_id: Option<&str>, since: DateTime<Utc>) -> Result<TunnelUsage> {
        let connection = self.connection()?;
        let since = since.to_rfc3339();
        let mut usage = TunnelUsage::default();
        {
            let mut statement = connection.prepare(
                "SELECT substr(at, 1, 10) AS day, COUNT(*),
                    COALESCE(SUM(CAST(json_extract(details_json, '$.bytes_to_provider') AS INTEGER)), 0),
                    COALESCE(SUM(CAST(json_extract(details_json, '$.bytes_from_provider') AS INTEGER)), 0)
                 FROM audit_log
                 WHERE event = 'relay.tunnel' AND at >= ?1 AND (?2 IS NULL OR actor = ?2)
                 GROUP BY day ORDER BY day",
            )?;
            let rows = statement.query_map(params![since, user_id], |row| {
                Ok(TunnelDay {
                    day: row.get(0)?,
                    tunnels: row.get(1)?,
                    bytes_to_provider: row.get(2)?,
                    bytes_from_provider: row.get(3)?,
                })
            })?;
            usage.per_day = rows.collect::<rusqlite::Result<Vec<_>>>()?;
        }
        {
            let mut statement = connection.prepare(
                "SELECT COALESCE(json_extract(details_json, '$.host'), tool, '') AS host, COUNT(*),
                    COALESCE(SUM(CAST(json_extract(details_json, '$.bytes_to_provider') AS INTEGER)
                               + CAST(json_extract(details_json, '$.bytes_from_provider') AS INTEGER)), 0),
                    COALESCE(AVG(CAST(json_extract(details_json, '$.seconds') AS INTEGER)), 0)
                 FROM audit_log
                 WHERE event = 'relay.tunnel' AND at >= ?1 AND (?2 IS NULL OR actor = ?2)
                 GROUP BY host ORDER BY 3 DESC LIMIT 20",
            )?;
            let rows = statement.query_map(params![since, user_id], |row| {
                Ok(TunnelHost {
                    host: row.get(0)?,
                    tunnels: row.get(1)?,
                    bytes: row.get(2)?,
                    avg_seconds: row.get::<_, f64>(3)? as i64,
                })
            })?;
            usage.per_host = rows.collect::<rusqlite::Result<Vec<_>>>()?;
        }
        for day in &usage.per_day {
            usage.tunnels += day.tunnels;
            usage.bytes_to_provider += day.bytes_to_provider;
            usage.bytes_from_provider += day.bytes_from_provider;
        }
        let (relayed, total): (i64, i64) = connection.query_row(
            "SELECT COALESCE(SUM(CASE WHEN json_extract(details_json, '$.relayed') = 1 THEN 1 ELSE 0 END), 0),
                    COUNT(*)
             FROM audit_log
             WHERE event = 'model.used' AND at >= ?1 AND (?2 IS NULL OR actor = ?2)",
            params![since, user_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        usage.relayed_calls = relayed;
        usage.total_calls = total;
        Ok(usage)
    }

    /// Calls, latency, relay share and failovers per model since `since`.
    pub fn model_health(
        &self,
        user_id: Option<&str>,
        since: DateTime<Utc>,
    ) -> Result<Vec<ModelHealth>> {
        let connection = self.connection()?;
        let since = since.to_rfc3339();
        // Latencies per model, to compute averages and the 95th percentile.
        let mut by_model: BTreeMap<(String, String), (Vec<i64>, i64)> = BTreeMap::new();
        {
            let mut statement = connection.prepare(
                "SELECT COALESCE(json_extract(details_json, '$.provider'), ''),
                        COALESCE(json_extract(details_json, '$.model'), ''),
                        CAST(json_extract(details_json, '$.latency_ms') AS INTEGER),
                        CASE WHEN json_extract(details_json, '$.relayed') = 1 THEN 1 ELSE 0 END
                 FROM audit_log
                 WHERE event = 'model.used' AND at >= ?1 AND (?2 IS NULL OR actor = ?2)
                 ORDER BY id DESC LIMIT 20000",
            )?;
            let rows = statement.query_map(params![since, user_id], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, Option<i64>>(2)?,
                    row.get::<_, i64>(3)?,
                ))
            })?;
            for row in rows {
                let (provider, model, latency, relayed) = row?;
                let entry = by_model.entry((provider, model)).or_default();
                if let Some(latency) = latency {
                    entry.0.push(latency.max(0));
                }
                entry.1 += relayed;
            }
        }
        let mut calls: BTreeMap<(String, String), i64> = BTreeMap::new();
        {
            let mut statement = connection.prepare(
                "SELECT COALESCE(json_extract(details_json, '$.provider'), ''),
                        COALESCE(json_extract(details_json, '$.model'), ''), COUNT(*)
                 FROM audit_log
                 WHERE event = 'model.used' AND at >= ?1 AND (?2 IS NULL OR actor = ?2)
                 GROUP BY 1, 2",
            )?;
            let rows = statement.query_map(params![since, user_id], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, i64>(2)?,
                ))
            })?;
            for row in rows {
                let (provider, model, count) = row?;
                calls.insert((provider, model), count);
            }
        }
        let mut failovers: BTreeMap<(String, String), i64> = BTreeMap::new();
        {
            let mut statement = connection.prepare(
                "SELECT COALESCE(json_extract(details_json, '$.from.provider'), ''),
                        COALESCE(json_extract(details_json, '$.from.model'), ''), COUNT(*)
                 FROM audit_log
                 WHERE event = 'model.failover' AND at >= ?1 AND (?2 IS NULL OR actor = ?2)
                 GROUP BY 1, 2",
            )?;
            let rows = statement.query_map(params![since, user_id], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, i64>(2)?,
                ))
            })?;
            for row in rows {
                let (provider, model, count) = row?;
                failovers.insert((provider, model), count);
            }
        }
        let mut keys: Vec<(String, String)> = calls.keys().cloned().collect();
        for key in failovers.keys() {
            if !keys.contains(key) {
                keys.push(key.clone());
            }
        }
        let mut out: Vec<ModelHealth> = keys
            .into_iter()
            .map(|key| {
                let (mut latencies, relayed) = by_model.get(&key).cloned().unwrap_or_default();
                latencies.sort_unstable();
                let avg = if latencies.is_empty() {
                    0
                } else {
                    latencies.iter().sum::<i64>() / latencies.len() as i64
                };
                let p95 = percentile(&latencies, 95);
                ModelHealth {
                    calls: calls.get(&key).copied().unwrap_or(0),
                    failovers: failovers.get(&key).copied().unwrap_or(0),
                    provider: key.0,
                    model: key.1,
                    avg_latency_ms: avg,
                    p95_latency_ms: p95,
                    relayed_calls: relayed,
                }
            })
            .collect();
        out.sort_by(|a, b| b.calls.cmp(&a.calls).then_with(|| a.model.cmp(&b.model)));
        Ok(out)
    }
}

/// The `pct`th percentile of sorted values (nearest rank), 0 when empty.
pub fn percentile(sorted: &[i64], pct: usize) -> i64 {
    if sorted.is_empty() {
        return 0;
    }
    let rank = (pct * sorted.len()).div_ceil(100).max(1);
    sorted[rank.min(sorted.len()) - 1]
}

/// One sample of the server's load.
#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct ResourceSample {
    pub at: String,
    pub cpu_percent: f32,
    pub memory_percent: f32,
    pub process_rss_bytes: u64,
}

/// Samples kept: 24 hours at one every 5 minutes.
pub const HISTORY_SAMPLES: usize = 288;
/// How often the server's load is sampled.
pub const SAMPLE_EVERY: Duration = Duration::from_secs(300);

/// The last 24 hours of server load, in memory only.
#[derive(Default)]
pub struct ResourceHistory {
    samples: Mutex<VecDeque<ResourceSample>>,
}

impl ResourceHistory {
    pub fn push(&self, sample: ResourceSample) {
        let mut samples = self.samples.lock().unwrap_or_else(|p| p.into_inner());
        if samples.len() >= HISTORY_SAMPLES {
            samples.pop_front();
        }
        samples.push_back(sample);
    }

    pub fn snapshot(&self) -> Vec<ResourceSample> {
        self.samples
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .iter()
            .cloned()
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clients_are_named_by_header_then_signature() {
        let mut headers = axum::http::HeaderMap::new();
        assert_eq!(client_from_headers(&headers), "api");
        headers.insert("user-agent", "Dart/3.5 (dart:io)".parse().unwrap());
        assert_eq!(client_from_headers(&headers), "mobile");
        headers.insert("x-serabot-client", "Desktop".parse().unwrap());
        assert_eq!(client_from_headers(&headers), "desktop");
        headers.insert("x-serabot-client", "toaster".parse().unwrap());
        assert_eq!(client_from_headers(&headers), "mobile");
    }

    #[test]
    fn percentiles_use_nearest_rank() {
        assert_eq!(percentile(&[], 95), 0);
        assert_eq!(percentile(&[7], 95), 7);
        let values: Vec<i64> = (1..=100).collect();
        assert_eq!(percentile(&values, 95), 95);
        assert_eq!(percentile(&values, 50), 50);
    }

    #[test]
    fn history_keeps_the_last_day() {
        let history = ResourceHistory::default();
        for n in 0..(HISTORY_SAMPLES + 5) {
            history.push(ResourceSample {
                at: n.to_string(),
                cpu_percent: 1.0,
                memory_percent: 2.0,
                process_rss_bytes: 3,
            });
        }
        let kept = history.snapshot();
        assert_eq!(kept.len(), HISTORY_SAMPLES);
        assert_eq!(kept[0].at, "5");
    }

    #[test]
    fn channels_tunnels_and_models_are_counted_per_member() {
        let store = Store::open_in_memory(8).unwrap();
        let admin = store.admin_user_id().unwrap();
        let member = store
            .create_user("microsoft:insights", "i@example.example", "I")
            .unwrap()
            .id;
        let since = Utc::now() - chrono::Duration::days(1);
        // Two model calls, one relayed; one failover away from model a.
        for (latency, relayed) in [(100, true), (300, false)] {
            store
                .record_audit(
                    &admin,
                    "model.used",
                    None,
                    None,
                    None,
                    None,
                    None,
                    Some(&serde_json::json!({
                        "provider": "p", "model": "a",
                        "usage": {"total_tokens": 10},
                        "latency_ms": latency, "relayed": relayed,
                    })),
                )
                .unwrap();
        }
        store
            .record_audit(
                &admin,
                "model.failover",
                None,
                None,
                None,
                None,
                None,
                Some(&serde_json::json!({"from": {"provider": "p", "model": "a"}, "to": {"provider": "p", "model": "b"}})),
            )
            .unwrap();
        store
            .record_audit(
                &admin,
                "relay.tunnel",
                Some("api.openai.com"),
                None,
                None,
                None,
                None,
                Some(&serde_json::json!({"host": "api.openai.com", "bytes_to_provider": 1000, "bytes_from_provider": 5000, "seconds": 4})),
            )
            .unwrap();
        let models = store.model_health(Some(&admin), since).unwrap();
        assert_eq!(models.len(), 1);
        assert_eq!(models[0].calls, 2);
        assert_eq!(models[0].avg_latency_ms, 200);
        assert_eq!(models[0].p95_latency_ms, 300);
        assert_eq!(models[0].relayed_calls, 1);
        assert_eq!(models[0].failovers, 1);
        let tunnel = store.tunnel_usage(Some(&admin), since).unwrap();
        assert_eq!(tunnel.tunnels, 1);
        assert_eq!(tunnel.bytes_from_provider, 5000);
        assert_eq!(tunnel.per_host[0].host, "api.openai.com");
        assert_eq!((tunnel.relayed_calls, tunnel.total_calls), (1, 2));
        // The other member sees none of it.
        assert!(store.model_health(Some(&member), since).unwrap().is_empty());
        assert_eq!(store.tunnel_usage(Some(&member), since).unwrap().tunnels, 0);
        // Channel tagging validates the client name.
        store
            .record_client_turn(&member, "task-x", "toaster")
            .unwrap();
        let client: String = store
            .connection()
            .unwrap()
            .query_row(
                "SELECT client FROM client_turns WHERE task_id = 'task-x'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(client, "api");
        assert!(
            store
                .channel_usage(Some(&member), since)
                .unwrap()
                .is_empty()
        );
    }
}
