//! Bot health (0.70): the data behind the Usage page's "Bot health" card and
//! the sidebar attention dot, from `GET /v1/bots/health?days=7|30`. Pure
//! helpers are covered by tests; the fetch runs on a short worker thread.

use serde::Deserialize;

use crate::api::ApiClient;

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq)]
pub struct ToolFailure {
    #[serde(default)]
    pub tool: String,
    #[serde(default)]
    pub failures: i64,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
pub struct BotHealth {
    #[serde(default)]
    pub bot: String,
    #[serde(default)]
    pub turns: i64,
    #[serde(default)]
    pub succeeded: i64,
    #[serde(default)]
    pub failed: i64,
    #[serde(default)]
    pub success_percent: Option<f64>,
    #[serde(default)]
    pub avg_ms: i64,
    #[serde(default)]
    pub p95_ms: i64,
    #[serde(default)]
    pub failovers: i64,
    #[serde(default)]
    pub top_failing_tools: Vec<ToolFailure>,
    #[serde(default)]
    pub needs_attention: bool,
    #[serde(default)]
    pub reason: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
pub struct BotHealthReport {
    #[serde(default)]
    pub days: i64,
    #[serde(default)]
    pub bots: Vec<BotHealth>,
    #[serde(default)]
    pub needs_attention: i64,
}

/// `GET /v1/bots/health` on a fresh client (blocking). `days` is 7 or 30.
pub fn fetch(
    credentials: &crate::worker::Credentials,
    days: u32,
) -> Result<BotHealthReport, String> {
    ApiClient::new(credentials.endpoint.clone(), credentials.token.clone())
        .map_err(|error| error.to_string())?
        .bot_health(days)
        .map_err(|error| error.to_string())
}

/// The window the card asks for: 7 days up to a week, otherwise 30.
pub fn window_days(usage_days: u32) -> u32 {
    if usage_days <= 7 { 7 } else { 30 }
}

/// "92%", or a dash when no turn has finished yet.
pub fn success_label(percent: Option<f64>) -> String {
    match percent {
        Some(value) => format!("{}%", value.round() as i64),
        None => "\u{2014}".into(),
    }
}

/// "850 ms", "2.4 s", "3 min 5 s"; a dash for zero.
pub fn human_duration(ms: i64) -> String {
    if ms <= 0 {
        "\u{2014}".into()
    } else if ms < 1000 {
        format!("{ms} ms")
    } else if ms < 60_000 {
        format!("{:.1} s", ms as f64 / 1000.0)
    } else {
        let seconds = ms / 1000;
        format!("{} min {} s", seconds / 60, seconds % 60)
    }
}

/// Short text for the top failing tool, empty when none.
pub fn top_tool_label(bot: &BotHealth) -> String {
    bot.top_failing_tools
        .first()
        .map(|tool| format!("{} ({}\u{d7})", tool.tool, tool.failures))
        .unwrap_or_default()
}

/// Whether the named bot should show an attention dot in the sidebar.
pub fn bot_needs_attention(report: &BotHealthReport, bot: &str) -> bool {
    report
        .bots
        .iter()
        .any(|entry| entry.bot == bot && entry.needs_attention)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn labels() {
        assert_eq!(success_label(Some(91.6)), "92%");
        assert_eq!(success_label(None), "\u{2014}");
        assert_eq!(human_duration(0), "\u{2014}");
        assert_eq!(human_duration(850), "850 ms");
        assert_eq!(human_duration(2400), "2.4 s");
        assert_eq!(human_duration(185_000), "3 min 5 s");
        assert_eq!(window_days(1), 7);
        assert_eq!(window_days(7), 7);
        assert_eq!(window_days(30), 30);
        assert_eq!(window_days(90), 30);
    }

    #[test]
    fn parses_the_server_reply() {
        let report: BotHealthReport = serde_json::from_value(serde_json::json!({
            "days": 7, "needs_attention": 1,
            "bots": [{
                "bot": "researcher", "turns": 6, "succeeded": 2, "failed": 3,
                "success_percent": 40.0, "avg_ms": 1000, "p95_ms": 2000,
                "failovers": 0,
                "top_failing_tools": [{"tool": "fetch_url", "failures": 4}],
                "needs_attention": true, "reason": "fetch_url failed 4 times"
            }]
        }))
        .unwrap();
        assert!(bot_needs_attention(&report, "researcher"));
        assert!(!bot_needs_attention(&report, "main"));
        assert_eq!(top_tool_label(&report.bots[0]), "fetch_url (4\u{d7})");
        assert_eq!(top_tool_label(&BotHealth::default()), "");
    }
}
