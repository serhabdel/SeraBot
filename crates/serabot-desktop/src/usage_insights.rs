//! Usage insights (0.67): the data behind the Usage tab's Channels, Tunnel,
//! Models and Server load cards, from `GET /v1/usage/insights`. Pure helpers
//! here are shared by the UI and covered by tests; the fetch runs on a short
//! worker thread like the relay card's.

use serde::Deserialize;

use crate::api::ApiClient;

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq)]
pub struct ChannelUsage {
    #[serde(default)]
    pub channel: String,
    #[serde(default)]
    pub turns: i64,
    #[serde(default)]
    pub tokens: i64,
    #[serde(default)]
    pub last_at: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq)]
pub struct TunnelDay {
    #[serde(default)]
    pub day: String,
    #[serde(default)]
    pub tunnels: i64,
    #[serde(default)]
    pub bytes_to_provider: i64,
    #[serde(default)]
    pub bytes_from_provider: i64,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq)]
pub struct TunnelHost {
    #[serde(default)]
    pub host: String,
    #[serde(default)]
    pub tunnels: i64,
    #[serde(default)]
    pub bytes: i64,
    #[serde(default)]
    pub avg_seconds: i64,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq)]
pub struct TunnelUsage {
    #[serde(default)]
    pub tunnels: i64,
    #[serde(default)]
    pub bytes_to_provider: i64,
    #[serde(default)]
    pub bytes_from_provider: i64,
    #[serde(default)]
    pub per_day: Vec<TunnelDay>,
    #[serde(default)]
    pub per_host: Vec<TunnelHost>,
    #[serde(default)]
    pub relayed_calls: i64,
    #[serde(default)]
    pub total_calls: i64,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq)]
pub struct ModelHealth {
    #[serde(default)]
    pub provider: String,
    #[serde(default)]
    pub model: String,
    #[serde(default)]
    pub calls: i64,
    #[serde(default)]
    pub avg_latency_ms: i64,
    #[serde(default)]
    pub p95_latency_ms: i64,
    #[serde(default)]
    pub relayed_calls: i64,
    #[serde(default)]
    pub failovers: i64,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
pub struct ResourceSample {
    #[serde(default)]
    pub at: String,
    #[serde(default)]
    pub cpu_percent: f32,
    #[serde(default)]
    pub memory_percent: f32,
    #[serde(default)]
    pub process_rss_bytes: u64,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
pub struct Insights {
    #[serde(default)]
    pub days: u32,
    #[serde(default)]
    pub channels: Vec<ChannelUsage>,
    #[serde(default)]
    pub tunnel: TunnelUsage,
    #[serde(default)]
    pub relay_online: bool,
    #[serde(default)]
    pub models: Vec<ModelHealth>,
    /// Admin only; `None` for members.
    #[serde(default)]
    pub resources: Option<Vec<ResourceSample>>,
}

/// `GET /v1/usage/insights` on a fresh client (blocking).
pub fn fetch(
    credentials: &crate::worker::Credentials,
    days: u32,
    everyone: bool,
) -> Result<Insights, String> {
    ApiClient::new(credentials.endpoint.clone(), credentials.token.clone())
        .map_err(|error| error.to_string())?
        .usage_insights(days.clamp(1, 90), everyone)
        .map_err(|error| error.to_string())
}

/// What a channel is called on the card, and its icon.
pub fn channel_label(channel: &str) -> (&'static str, &'static str) {
    match channel {
        "desktop" => ("This PC app", "grid"),
        "mobile" => ("Phone app", "user"),
        "telegram" => ("Telegram", "send"),
        "whatsapp" => ("WhatsApp", "chat"),
        "app" => ("App (before 0.67)", "chat"),
        "api" => ("API and scripts", "code"),
        _ => ("Schedules and watches", "refresh"),
    }
}

/// Whole-number share of `part` in `total`, 0 when there is no total.
pub fn share_percent(part: i64, total: i64) -> u32 {
    if total <= 0 || part <= 0 {
        return 0;
    }
    ((part as f64 * 100.0 / total as f64).round() as u32).min(100)
}

/// Readable byte size: B, KB, MB or GB.
pub fn human_bytes(bytes: i64) -> String {
    let bytes = bytes.max(0) as f64;
    const KB: f64 = 1024.0;
    if bytes >= KB * KB * KB {
        format!("{:.1} GB", bytes / (KB * KB * KB))
    } else if bytes >= KB * KB {
        format!("{:.1} MB", bytes / (KB * KB))
    } else if bytes >= KB {
        format!("{:.0} KB", bytes / KB)
    } else {
        format!("{bytes:.0} B")
    }
}

/// Readable latency: "850 ms" or "2.4 s".
pub fn human_latency(ms: i64) -> String {
    if ms <= 0 {
        "\u{2014}".into()
    } else if ms < 1000 {
        format!("{ms} ms")
    } else {
        format!("{:.1} s", ms as f64 / 1000.0)
    }
}

/// SVG `points` for a sparkline of `values` (0..=100) in a `width` x
/// `height` box; empty when fewer than two values.
pub fn sparkline_points(values: &[f32], width: f32, height: f32) -> String {
    if values.len() < 2 {
        return String::new();
    }
    let step = width / (values.len() - 1) as f32;
    values
        .iter()
        .enumerate()
        .map(|(index, value)| {
            let y = height - (value.clamp(0.0, 100.0) / 100.0) * height;
            format!("{:.1},{:.1}", index as f32 * step, y)
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// Bar heights (percent of the busiest day) for the tunnel chart.
pub fn tunnel_bars(days: &[TunnelDay]) -> Vec<(String, i64, u32)> {
    let peak = days
        .iter()
        .map(|day| day.bytes_to_provider + day.bytes_from_provider)
        .max()
        .unwrap_or(0);
    days.iter()
        .map(|day| {
            let bytes = day.bytes_to_provider + day.bytes_from_provider;
            let height = if peak > 0 {
                ((bytes as f64 * 100.0 / peak as f64).round() as u32).max(2)
            } else {
                0
            };
            (day.day.clone(), bytes, height)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn insights_parse_with_missing_fields() {
        let parsed: Insights = serde_json::from_str(
            r#"{"days":7,"channels":[{"channel":"mobile","turns":3}],"resources":null}"#,
        )
        .unwrap();
        assert_eq!(parsed.channels[0].turns, 3);
        assert!(parsed.resources.is_none());
        assert_eq!(parsed.tunnel.tunnels, 0);
    }

    #[test]
    fn labels_and_shares() {
        assert_eq!(channel_label("mobile").0, "Phone app");
        assert_eq!(channel_label("automation").0, "Schedules and watches");
        assert_eq!(share_percent(1, 3), 33);
        assert_eq!(share_percent(5, 0), 0);
        assert_eq!(share_percent(9, 4), 100);
    }

    #[test]
    fn sizes_and_latency_read_well() {
        assert_eq!(human_bytes(512), "512 B");
        assert_eq!(human_bytes(2048), "2 KB");
        assert_eq!(human_bytes(3 * 1024 * 1024), "3.0 MB");
        assert_eq!(human_latency(0), "\u{2014}");
        assert_eq!(human_latency(850), "850 ms");
        assert_eq!(human_latency(2400), "2.4 s");
    }

    #[test]
    fn sparkline_spans_the_box() {
        assert!(sparkline_points(&[50.0], 100.0, 20.0).is_empty());
        assert_eq!(
            sparkline_points(&[0.0, 100.0, 50.0], 100.0, 20.0),
            "0.0,20.0 50.0,0.0 100.0,10.0"
        );
    }

    #[test]
    fn tunnel_bars_scale_to_the_busiest_day() {
        let days = vec![
            TunnelDay {
                day: "2026-11-19".into(),
                tunnels: 1,
                bytes_to_provider: 100,
                bytes_from_provider: 100,
            },
            TunnelDay {
                day: "2026-11-20".into(),
                tunnels: 1,
                bytes_to_provider: 1,
                bytes_from_provider: 0,
            },
        ];
        let bars = tunnel_bars(&days);
        assert_eq!(bars[0].2, 100);
        assert_eq!(bars[1].2, 2);
    }
}
