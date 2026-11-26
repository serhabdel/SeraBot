//! The Usage page (0.17; redesigned 0.74): what the acting user's bots used in
//! a period, organized into a 4-card KPI hero strip and segmented sub-views.
//!
//! Numbers come from `GET /v1/usage`, which only ever returns the caller's own
//! rows. Cost and alert summaries come from `/v1/ops/costs` and `/v1/ops/alerts`.

use dioxus::prelude::*;
use serabot_desktop::ops_view as ops;
use serabot_desktop::pc_stats::FoldersSize;
use serabot_desktop::session::{Session, humanize_label};
use serabot_desktop::status_view::{
    Health, bar_percent, format_bytes, format_percent, format_uptime, health, member_label, percent,
};
use serabot_desktop::theme::{Level, Tone};
use serabot_desktop::worker::Credentials;
use serabot_desktop::{UsageBucket, UsageModel, UsageReport, UsageTool};
use serde_json::Value;

use crate::ui::{Icon, human_provider};
use crate::ui_bot_health::BotHealthCard;
use crate::ui_job_search::JobSearchCard;
use crate::ui_ops::{AlertsCard, BackupsCard, CostsCard};
use crate::ui_usage_insights::UsageInsights;
use crate::ui_widgets::{LevelChip, StatusChip};

/// How often the health cards refresh while the Usage page is open.
#[cfg(windows)]
const REFRESH_SECS: u64 = 30;

/// Ranges offered by the picker, in days.
const RANGES: [(u32, &str); 3] = [(7, "7 days"), (30, "30 days"), (90, "90 days")];

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum UsageTab {
    Overview,
    ModelsAndBots,
    InsightsAndRelay,
    CostsAndAlerts,
    Health,
}

impl UsageTab {
    const ALL: [Self; 5] = [
        Self::Overview,
        Self::ModelsAndBots,
        Self::InsightsAndRelay,
        Self::CostsAndAlerts,
        Self::Health,
    ];

    fn label(self) -> &'static str {
        match self {
            Self::Overview => "Overview",
            Self::ModelsAndBots => "Models & Bots",
            Self::InsightsAndRelay => "Insights & Relay",
            Self::CostsAndAlerts => "Costs & Alerts",
            Self::Health => "Health",
        }
    }
}

fn quota_percent(used: i64, limit: u64) -> u32 {
    if limit == 0 {
        return 0;
    }
    ((used.max(0) as u64)
        .saturating_mul(100)
        .saturating_div(limit)
        .min(100)) as u32
}

/// Percentage share of `part` in `total`, clamped to `0..=100`.
pub fn share_percent(part: i64, total: i64) -> u32 {
    if part <= 0 || total <= 0 {
        return 0;
    }
    ((part as u64)
        .saturating_mul(100)
        .saturating_div(total as u64)
        .min(100)) as u32
}

/// `(completed, failed, total)` across all tool rows.
pub fn tool_summary(tools: &[UsageTool]) -> (i64, i64, i64) {
    let completed: i64 = tools.iter().map(|t| t.completed.max(0)).sum();
    let failed: i64 = tools.iter().map(|t| t.failed.max(0)).sum();
    (completed, failed, completed + failed)
}

async fn fetch_ops_json(
    session: Signal<Session>,
    job: impl FnOnce(&Credentials) -> Result<Value, String> + Send + 'static,
) -> Option<Value> {
    let credentials = session.read().credentials.clone()?;
    let (tx, rx) = tokio::sync::oneshot::channel();
    let spawned = std::thread::Builder::new()
        .name("serabot-usage-ops".into())
        .spawn(move || {
            let _ = tx.send(job(&credentials));
        });
    if spawned.is_err() {
        return None;
    }
    rx.await.ok().and_then(Result::ok)
}

/// `12345` -> `12,345`.
pub fn group_digits(value: i64) -> String {
    let digits = value.max(0).to_string();
    let mut out = String::new();
    for (index, ch) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index).is_multiple_of(3) {
            out.push(',');
        }
        out.push(ch);
    }
    out
}

/// Bar heights in percent (minimum 4 so a non-zero day stays visible),
/// oldest day first.
pub fn chart_bars(report: &UsageReport) -> Vec<(String, i64, u32)> {
    let mut days: Vec<(String, i64)> = report
        .per_day
        .iter()
        .map(|bucket| (bucket.day.clone().unwrap_or_default(), bucket.total_tokens))
        .collect();
    days.sort();
    let max = days.iter().map(|(_, tokens)| *tokens).max().unwrap_or(0);
    days.into_iter()
        .map(|(day, tokens)| {
            let height = if max <= 0 || tokens <= 0 {
                0
            } else {
                ((tokens * 100 / max) as u32).clamp(4, 100)
            };
            (day, tokens, height)
        })
        .collect()
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum UsageSort {
    TokensDesc,
    CallsDesc,
    NameAsc,
}

fn cmp_usage_row(
    a_tokens: i64,
    a_calls: i64,
    a_name: &str,
    b_tokens: i64,
    b_calls: i64,
    b_name: &str,
    sort: UsageSort,
) -> std::cmp::Ordering {
    match sort {
        UsageSort::TokensDesc => b_tokens
            .cmp(&a_tokens)
            .then_with(|| b_calls.cmp(&a_calls))
            .then_with(|| a_name.cmp(b_name)),
        UsageSort::CallsDesc => b_calls
            .cmp(&a_calls)
            .then_with(|| b_tokens.cmp(&a_tokens))
            .then_with(|| a_name.cmp(b_name)),
        UsageSort::NameAsc => a_name
            .to_ascii_lowercase()
            .cmp(&b_name.to_ascii_lowercase()),
    }
}

fn filter_sort_models(rows: &[UsageModel], query: &str, sort: UsageSort) -> Vec<UsageModel> {
    let needle = query.trim().to_ascii_lowercase();
    let mut out: Vec<UsageModel> = rows
        .iter()
        .filter(|row| {
            needle.is_empty()
                || row.provider.to_ascii_lowercase().contains(&needle)
                || row.model.to_ascii_lowercase().contains(&needle)
                || human_provider(&row.provider)
                    .to_ascii_lowercase()
                    .contains(&needle)
        })
        .cloned()
        .collect();
    out.sort_by(|a, b| {
        let a_key = format!("{}/{}", a.provider, a.model);
        let b_key = format!("{}/{}", b.provider, b.model);
        cmp_usage_row(
            a.total_tokens,
            a.calls,
            &a_key,
            b.total_tokens,
            b.calls,
            &b_key,
            sort,
        )
    });
    out
}

fn filter_sort_bots(rows: &[UsageBucket], query: &str, sort: UsageSort) -> Vec<UsageBucket> {
    let needle = query.trim().to_ascii_lowercase();
    let mut out: Vec<UsageBucket> = rows
        .iter()
        .filter(|row| {
            let bot = row.bot.as_deref().unwrap_or_default();
            needle.is_empty()
                || bot.to_ascii_lowercase().contains(&needle)
                || humanize_label(bot).to_ascii_lowercase().contains(&needle)
        })
        .cloned()
        .collect();
    out.sort_by(|a, b| {
        let a_bot = a.bot.as_deref().unwrap_or_default();
        let b_bot = b.bot.as_deref().unwrap_or_default();
        cmp_usage_row(
            a.total_tokens,
            a.calls,
            a_bot,
            b.total_tokens,
            b.calls,
            b_bot,
            sort,
        )
    });
    out
}

fn chart_axis_summary(bars: &[(String, i64, u32)]) -> (String, String, String) {
    let Some(first) = bars.first() else {
        return (String::new(), "Peak 0 tokens".to_owned(), String::new());
    };
    let last = bars.last().unwrap_or(first);
    let peak = bars.iter().map(|(_, tokens, _)| *tokens).max().unwrap_or(0);
    (
        first.0.clone(),
        format!("Peak {} tokens", group_digits(peak)),
        if last.0 == first.0 {
            String::new()
        } else {
            last.0.clone()
        },
    )
}

#[component]
pub fn UsagePage(session: Signal<Session>) -> Element {
    let mut session = session;
    let mut active_tab = use_signal(|| UsageTab::Overview);
    let mut filter = use_signal(String::new);
    let mut sort_by = use_signal(|| UsageSort::TokensDesc);
    let mut refreshed_at = use_signal(|| Option::<std::time::Instant>::None);
    let mut costs_summary = use_signal(|| Option::<Value>::None);
    let mut alerts_summary = use_signal(|| Option::<Value>::None);
    use_hook(move || {
        spawn(async move {
            if let Some(value) = fetch_ops_json(session, ops::get_costs).await {
                costs_summary.set(Some(value));
            }
        });
        spawn(async move {
            if let Some(value) = fetch_ops_json(session, ops::get_alerts).await {
                alerts_summary.set(Some(value));
            }
        });
    });
    use_effect(move || {
        let s = session.read();
        if s.state.usage.is_some() && !s.usage_loading && s.state.error.is_none() {
            refreshed_at.set(Some(std::time::Instant::now()));
        }
    });
    let (days, loading, report, error, server_health, is_admin) = {
        let s = session.read();
        (
            s.usage_days,
            s.usage_loading,
            s.state.usage.clone(),
            s.state.error.clone(),
            s.state.server_health.clone(),
            s.is_admin(),
        )
    };
    let bars = report.as_ref().map(chart_bars).unwrap_or_default();
    let bar_count = bars.len();
    let label_step = (bar_count / 6).max(1);
    let (axis_start, axis_peak, axis_end) = chart_axis_summary(&bars);
    let (tool_ok, tool_fail, tool_runs) = report
        .as_ref()
        .map(|r| tool_summary(&r.tools))
        .unwrap_or((0, 0, 0));
    let total_tokens = report.as_ref().map(|r| r.totals.total_tokens).unwrap_or(0);
    let empty = report
        .as_ref()
        .is_some_and(|report| report.totals.calls == 0);
    let filter_text = filter.read().clone();
    let model_rows = report
        .as_ref()
        .map(|r| filter_sort_models(&r.per_model, &filter_text, sort_by()))
        .unwrap_or_default();
    let bot_rows = report
        .as_ref()
        .map(|r| filter_sort_bots(&r.per_bot, &filter_text, sort_by()))
        .unwrap_or_default();
    let top_models = report
        .as_ref()
        .map(|r| filter_sort_models(&r.per_model, "", UsageSort::TokensDesc))
        .unwrap_or_default();
    let top_bots = report
        .as_ref()
        .map(|r| filter_sort_bots(&r.per_bot, "", UsageSort::TokensDesc))
        .unwrap_or_default();
    let updated_secs = refreshed_at().map(|at| at.elapsed().as_secs());

    let costs_val = costs_summary();
    let cached_m = costs_val
        .as_ref()
        .and_then(|c| c["totals"]["month"]["cached_m"].as_f64())
        .unwrap_or(0.0);
    let month_usd = costs_val.as_ref().and_then(|c| c["month_usd"].as_f64());
    let budget_usd = costs_val
        .as_ref()
        .and_then(|c| c["budget_usd"].as_f64())
        .filter(|b| *b > 0.0);
    let budget_level = costs_val.as_ref().and_then(|c| c["budget_level"].as_u64());
    let unpriced_tokens = costs_val
        .as_ref()
        .and_then(|c| c["month_unpriced_tokens"].as_i64())
        .unwrap_or(0);
    let budget_pct = match (month_usd, budget_usd) {
        (Some(m), Some(b)) if b > 0.0 => Some(((m / b) * 100.0).round().min(100.0) as u32),
        _ => None,
    };

    let alerts_val = alerts_summary();
    let health_summary_line = {
        let mut parts: Vec<String> = Vec::new();
        if is_admin {
            if let Some(status) = &server_health {
                let (lvl, _) = health(status);
                parts.push(if lvl == Health::Ok {
                    "Server OK".to_owned()
                } else {
                    "Server needs a look".to_owned()
                });
            } else {
                parts.push("Checking server".to_owned());
            }
        }
        if let Some(alerts) = &alerts_val {
            let recent_count = alerts["recent"].as_array().map(|a| a.len()).unwrap_or(0);
            let alerts_on = alerts["enabled"].as_bool().unwrap_or(true);
            if recent_count > 0 {
                parts.push(format!("{recent_count} recent alerts"));
            } else if alerts_on {
                parts.push("Alerts on".to_owned());
            } else {
                parts.push("Alerts off".to_owned());
            }
        }
        if parts.is_empty() {
            "All systems nominal".to_owned()
        } else {
            parts.join(" \u{b7} ")
        }
    };
    let server_needs_look = is_admin
        && server_health
            .as_ref()
            .is_some_and(|s| health(s).0 != Health::Ok);

    // Refresh the health cards every 30 s, only while this page is open
    // (the future ends when the page is left).
    use_future(move || async move {
        loop {
            session.write().refresh_server_health();
            session.read().refresh_pc();
            #[cfg(windows)]
            tokio::time::sleep(std::time::Duration::from_secs(REFRESH_SECS)).await;
            #[cfg(not(windows))]
            std::future::pending::<()>().await;
        }
    });
    rsx! {
        div { class: "pane usage-page",
            div { class: "pane-head",
                div {
                    h1 { class: "pane-title", "Usage" }
                    p { class: "muted", "What your bots used. Only your own activity is counted." }
                    if let Some(secs) = updated_secs {
                        p { class: "muted usage-updated", "Updated {secs}s ago" }
                    }
                }
                div { class: "seg usage-range", role: "group", "aria-label": "Period",
                    for (value, label) in RANGES {
                        button {
                            key: "{value}",
                            class: if days == value { "seg-btn active" } else { "seg-btn" },
                            r#type: "button",
                            "aria-pressed": if days == value { "true" } else { "false" },
                            disabled: loading,
                            onclick: move |_| session.write().load_usage_days(value),
                            "Last {label}"
                        }
                    }
                }
            }
            div { class: "pane-body",
                // 1. 4-Card KPI Hero Strip
                if loading && report.is_none() {
                    div { class: "usage-hero-grid", role: "status", "aria-busy": "true", "aria-label": "Loading usage summary",
                        for n in 0..4 {
                            div { key: "{n}", class: "usage-card usage-kpi-card skeleton usage-skeleton", " " }
                        }
                    }
                } else {
                    div { class: "usage-hero-grid",
                        // Card 1: Tokens
                        div { class: "usage-card usage-kpi-card",
                            div { class: "usage-kpi-head",
                                span { class: "usage-kpi-label", "Tokens" }
                                span { class: "usage-kpi-badge", "Last {days}d" }
                            }
                            span { class: "usage-kpi-value",
                                if let Some(ref r) = report {
                                    "{group_digits(r.totals.total_tokens)}"
                                } else {
                                    "\u{2014}"
                                }
                            }
                            span { class: "usage-kpi-sub",
                                if let Some(ref r) = report {
                                    "{group_digits(r.totals.prompt_tokens)} in \u{b7} {group_digits(r.totals.completion_tokens)} out \u{b7} {ops::millions(cached_m)} cached"
                                } else {
                                    "Input, output and cached"
                                }
                            }
                        }
                        // Card 2: Estimated Cost
                        div { class: "usage-card usage-kpi-card",
                            div { class: "usage-kpi-head",
                                span { class: "usage-kpi-label", "Estimated Cost" }
                                span { class: "usage-kpi-badge", "This month" }
                            }
                            span {
                                class: if month_usd.is_some() { "usage-kpi-value" } else { "usage-kpi-value muted-value" },
                                if let Some(usd) = month_usd {
                                    "{ops::usd(usd)}"
                                } else {
                                    "\u{2014}"
                                }
                            }
                            if let (Some(pct), Some(budget)) = (budget_pct, budget_usd) {
                                Meter {
                                    percent: pct,
                                    label: "Monthly budget".to_owned(),
                                    warn: budget_level == Some(80) || (80..100).contains(&pct),
                                    danger: budget_level == Some(100) || pct >= 100,
                                }
                                span { class: "usage-kpi-sub", "{pct}% of {ops::usd(budget)} monthly budget" }
                            } else if unpriced_tokens > 0 {
                                span { class: "usage-kpi-sub", "{group_digits(unpriced_tokens)} unpriced tokens" }
                            } else {
                                span { class: "usage-kpi-sub", "No monthly budget set" }
                            }
                        }
                        // Card 3: Turns & Tools
                        div { class: "usage-card usage-kpi-card",
                            div { class: "usage-kpi-head",
                                span { class: "usage-kpi-label", "Turns & Tools" }
                                span { class: "usage-kpi-badge", "{group_digits(tool_runs)} tools" }
                            }
                            span { class: "usage-kpi-value",
                                if let Some(ref r) = report {
                                    "{group_digits(r.totals.calls)}"
                                } else {
                                    "\u{2014}"
                                }
                            }
                            span { class: "usage-kpi-sub",
                                if report.is_some() {
                                    "LLM calls \u{b7} {group_digits(tool_ok)} ok \u{b7} {group_digits(tool_fail)} failed"
                                } else {
                                    "LLM calls and tool runs"
                                }
                            }
                        }
                        // Card 4: Health & Quota
                        div { class: "usage-card usage-kpi-card",
                            div { class: "usage-kpi-head",
                                span { class: "usage-kpi-label", "Health & Quota" }
                                if let Some(quota) = report.as_ref().map(|r| &r.quota.daily_tasks).filter(|q| q.limit > 0) {
                                    LevelChip { level: Level::from_flags(quota.near_limit, quota.at_limit) }
                                } else if server_needs_look {
                                    StatusChip { tone: Tone::Warn, text: "Attention".to_owned() }
                                } else {
                                    StatusChip { tone: Tone::Ok, text: "OK".to_owned() }
                                }
                            }
                            if let Some(quota) = report.as_ref().map(|r| &r.quota.daily_tasks).filter(|q| q.limit > 0) {
                                span { class: "usage-kpi-value",
                                    "{group_digits(quota.used)} / {group_digits(quota.limit as i64)}"
                                }
                                Meter {
                                    percent: quota_percent(quota.used, quota.limit),
                                    label: "Daily task quota".to_owned(),
                                    warn: quota.near_limit,
                                    danger: quota.at_limit,
                                }
                            } else {
                                span { class: "usage-kpi-value",
                                    if server_needs_look { "Needs a look" } else { "No daily cap" }
                                }
                            }
                            span { class: "usage-kpi-sub", "{health_summary_line}" }
                        }
                    }
                }

                // 2. Segmented Sub-View Tabs
                div { class: "seg usage-tabs", role: "tablist", "aria-label": "Usage views",
                    for tab in UsageTab::ALL {
                        button {
                            key: "{tab.label()}",
                            class: if active_tab() == tab { "seg-btn active" } else { "seg-btn" },
                            r#type: "button",
                            role: "tab",
                            "aria-selected": if active_tab() == tab { "true" } else { "false" },
                            onclick: move |_| active_tab.set(tab),
                            "{tab.label()}"
                        }
                    }
                }

                // 3. Active Sub-View Content
                match active_tab() {
                    UsageTab::Overview => rsx! {
                        if loading && report.is_none() {
                            div { class: "usage-card skeleton usage-chart-skeleton", role: "status", "aria-busy": "true", " " }
                        } else if report.is_some() {
                            if empty {
                                div { class: "usage-card empty",
                                    div { class: "empty-icon", Icon { name: "bolt", size: 20 } }
                                    h3 { class: "empty-title", "Nothing used in the last {days} days" }
                                    p { class: "empty-text", "Once your bots answer, the numbers show up here." }
                                }
                            } else {
                                div { class: "card usage-card usage-chart-card",
                                    div { class: "card-head",
                                        h3 { class: "card-title", "Tokens per day" }
                                        if loading {
                                            span { class: "spinner", title: "Refreshing" }
                                        } else if !axis_peak.is_empty() {
                                            span { class: "page-count", "{axis_peak}" }
                                        }
                                    }
                                    if bars.is_empty() {
                                        p { class: "tiny", "No daily breakdown reported for this period." }
                                    } else {
                                        div { class: "usage-chart-wrap",
                                            div { class: "usage-chart", role: "img", "aria-label": "Tokens used per day",
                                                for (index, (day, tokens, height)) in bars.iter().enumerate() {
                                                    div {
                                                        key: "{day}",
                                                        class: "usage-bar-col",
                                                        title: "{day}: {group_digits(*tokens)} tokens",
                                                        div {
                                                            class: "usage-bar",
                                                            style: "height: {height}%; --bar-delay: {index.min(40) * 12}ms;",
                                                        }
                                                    }
                                                }
                                            }
                                            div { class: "usage-chart-labels", "aria-hidden": "true",
                                                for (index, (day, _, _)) in bars.iter().enumerate() {
                                                    {
                                                        let label = if index % label_step == 0 || index == bar_count - 1 {
                                                            day.rsplit('-').next().unwrap_or("").to_owned()
                                                        } else {
                                                            String::new()
                                                        };
                                                        rsx! {
                                                            span { key: "{index}", class: "usage-bar-label", "{label}" }
                                                        }
                                                    }
                                                }
                                            }
                                            div { class: "usage-chart-axis",
                                                span { "{axis_start}" }
                                                span { "{axis_end}" }
                                            }
                                        }
                                    }
                                }
                                div { class: "usage-overview-grid",
                                    div { class: "card usage-card",
                                        div { class: "card-head",
                                            h3 { class: "card-title", "Top models" }
                                            button {
                                                class: "btn ghost sm",
                                                r#type: "button",
                                                onclick: move |_| active_tab.set(UsageTab::ModelsAndBots),
                                                "View all"
                                            }
                                        }
                                        if top_models.is_empty() {
                                            p { class: "tiny", "No per-model breakdown reported for this period." }
                                        } else {
                                            div { class: "usage-summary-list",
                                                for row in top_models.iter().take(5) {
                                                    {
                                                        let pct = share_percent(row.total_tokens, total_tokens);
                                                        let bar_w = if row.total_tokens > 0 { pct.max(2) } else { 0 };
                                                        rsx! {
                                                            div { key: "{row.provider}/{row.model}", class: "usage-summary-row",
                                                                div { class: "usage-summary-meta",
                                                                    span { class: "usage-model", "{row.model}" }
                                                                    span { class: "tiny", "{human_provider(&row.provider)} \u{b7} {group_digits(row.calls)} calls" }
                                                                }
                                                                div { class: "usage-share-bar",
                                                                    div { class: "usage-share-fill", style: "width: {bar_w}%;" }
                                                                }
                                                                span { class: "usage-summary-val", "{group_digits(row.total_tokens)}" }
                                                            }
                                                        }
                                                    }
                                                }
                                            }
                                        }
                                    }
                                    div { class: "card usage-card",
                                        div { class: "card-head",
                                            h3 { class: "card-title", "Top bots" }
                                            button {
                                                class: "btn ghost sm",
                                                r#type: "button",
                                                onclick: move |_| active_tab.set(UsageTab::ModelsAndBots),
                                                "View all"
                                            }
                                        }
                                        if top_bots.is_empty() {
                                            p { class: "tiny", "No per-bot breakdown reported for this period." }
                                        } else {
                                            div { class: "usage-summary-list",
                                                for row in top_bots.iter().take(5) {
                                                    {
                                                        let bot_id = row.bot.clone().unwrap_or_default();
                                                        let pct = share_percent(row.total_tokens, total_tokens);
                                                        let bar_w = if row.total_tokens > 0 { pct.max(2) } else { 0 };
                                                        rsx! {
                                                            div { key: "{bot_id}", class: "usage-summary-row",
                                                                div { class: "usage-summary-meta",
                                                                    span { class: "usage-model", "{humanize_label(&bot_id)}" }
                                                                    span { class: "tiny", "{group_digits(row.calls)} replies \u{b7} {pct}%" }
                                                                }
                                                                div { class: "usage-share-bar",
                                                                    div { class: "usage-share-fill", style: "width: {bar_w}%;" }
                                                                }
                                                                span { class: "usage-summary-val", "{group_digits(row.total_tokens)}" }
                                                            }
                                                        }
                                                    }
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        } else {
                            div { class: "usage-card empty usage-error", role: "alert",
                                div { class: "empty-icon danger", Icon { name: "alert", size: 20 } }
                                h3 { class: "empty-title", "Usage could not be loaded" }
                                p { class: "empty-text",
                                    "{error.as_deref().unwrap_or(\"The server did not answer. Try again in a moment.\")}"
                                }
                                button {
                                    class: "btn primary sm",
                                    r#type: "button",
                                    onclick: move |_| session.write().load_usage_days(days),
                                    Icon { name: "refresh", size: 14 }
                                    "Retry"
                                }
                            }
                        }
                    },
                    UsageTab::ModelsAndBots => rsx! {
                        if loading && report.is_none() {
                            div { class: "usage-card skeleton usage-chart-skeleton", role: "status", "aria-busy": "true", " " }
                        } else if report.is_none() {
                            div { class: "usage-card empty usage-error", role: "alert",
                                div { class: "empty-icon danger", Icon { name: "alert", size: 20 } }
                                h3 { class: "empty-title", "Usage could not be loaded" }
                                p { class: "empty-text",
                                    "{error.as_deref().unwrap_or(\"The server did not answer. Try again in a moment.\")}"
                                }
                                button {
                                    class: "btn primary sm",
                                    r#type: "button",
                                    onclick: move |_| session.write().load_usage_days(days),
                                    Icon { name: "refresh", size: 14 }
                                    "Retry"
                                }
                            }
                        } else if empty {
                            div { class: "usage-card empty",
                                div { class: "empty-icon", Icon { name: "bolt", size: 20 } }
                                h3 { class: "empty-title", "Nothing used in the last {days} days" }
                                p { class: "empty-text", "Once your bots answer, per-model and per-bot tables appear here." }
                            }
                        } else {
                            section { class: "usage-section",
                                div { class: "page-section-head",
                                    div {
                                        h2 { class: "usage-section-title", "Models & Bots" }
                                        p { class: "usage-section-sub", "Where the tokens went in the last {days} days." }
                                    }
                                    div { class: "usage-toolbar",
                                        div { class: "search-wrap",
                                            input {
                                                class: "input",
                                                r#type: "search",
                                                placeholder: "Filter models or bots\u{2026}",
                                                "aria-label": "Filter usage tables",
                                                value: "{filter}",
                                                oninput: move |event| filter.set(event.value()),
                                            }
                                            if !filter.read().trim().is_empty() {
                                                button {
                                                    class: "btn ghost sm",
                                                    r#type: "button",
                                                    onclick: move |_| filter.set(String::new()),
                                                    "Clear"
                                                }
                                            }
                                        }
                                        div { class: "seg", role: "group", "aria-label": "Sort usage rows",
                                            button {
                                                class: if sort_by() == UsageSort::TokensDesc { "seg-btn active" } else { "seg-btn" },
                                                r#type: "button",
                                                onclick: move |_| sort_by.set(UsageSort::TokensDesc),
                                                "By tokens"
                                            }
                                            button {
                                                class: if sort_by() == UsageSort::CallsDesc { "seg-btn active" } else { "seg-btn" },
                                                r#type: "button",
                                                onclick: move |_| sort_by.set(UsageSort::CallsDesc),
                                                "By replies"
                                            }
                                            button {
                                                class: if sort_by() == UsageSort::NameAsc { "seg-btn active" } else { "seg-btn" },
                                                r#type: "button",
                                                onclick: move |_| sort_by.set(UsageSort::NameAsc),
                                                "Name"
                                            }
                                        }
                                    }
                                }
                                div { class: "usage-breakdown",
                                    div { class: "card usage-card",
                                        div { class: "card-head",
                                            h3 { class: "card-title", "Per-Model" }
                                            span { class: "page-count", "{model_rows.len()} models" }
                                        }
                                        if model_rows.is_empty() {
                                            p { class: "tiny",
                                                if filter.read().trim().is_empty() {
                                                    "No per-model breakdown reported for this period."
                                                } else {
                                                    "No models match \u{201c}{filter}\u{201d}."
                                                }
                                            }
                                        } else {
                                            div { class: "usage-table-wrap",
                                                table { class: "usage-table",
                                                    thead {
                                                        tr {
                                                            th { "Provider" }
                                                            th { "Model" }
                                                            th { class: "num", "Replies" }
                                                            th { "Share" }
                                                            th { class: "num", "Tokens" }
                                                        }
                                                    }
                                                    tbody {
                                                        for row in model_rows {
                                                            {
                                                                let pct = share_percent(row.total_tokens, total_tokens);
                                                                let bar_w = if row.total_tokens > 0 { pct.max(2) } else { 0 };
                                                                rsx! {
                                                                    tr { key: "{row.provider}/{row.model}",
                                                                        td { "{human_provider(&row.provider)}" }
                                                                        td { class: "usage-model", "{row.model}" }
                                                                        td { class: "num", "{group_digits(row.calls)}" }
                                                                        td { class: "usage-share-cell",
                                                                            div { class: "usage-share-wrap",
                                                                                div { class: "usage-share-bar",
                                                                                    div { class: "usage-share-fill", style: "width: {bar_w}%;" }
                                                                                }
                                                                                span { class: "usage-share-pct", "{pct}%" }
                                                                            }
                                                                        }
                                                                        td { class: "num", "{group_digits(row.total_tokens)}" }
                                                                    }
                                                                }
                                                            }
                                                        }
                                                    }
                                                }
                                            }
                                        }
                                    }
                                    div { class: "card usage-card",
                                        div { class: "card-head",
                                            h3 { class: "card-title", "Per-Bot" }
                                            span { class: "page-count", "{bot_rows.len()} bots" }
                                        }
                                        if bot_rows.is_empty() {
                                            p { class: "tiny",
                                                if filter.read().trim().is_empty() {
                                                    "No per-bot breakdown reported for this period."
                                                } else {
                                                    "No bots match \u{201c}{filter}\u{201d}."
                                                }
                                            }
                                        } else {
                                            div { class: "usage-table-wrap",
                                                table { class: "usage-table",
                                                    thead {
                                                        tr {
                                                            th { "Bot" }
                                                            th { class: "num", "Replies" }
                                                            th { "Share" }
                                                            th { class: "num", "Tokens" }
                                                        }
                                                    }
                                                    tbody {
                                                        for row in bot_rows {
                                                            {
                                                                let bot_id = row.bot.clone().unwrap_or_default();
                                                                let pct = share_percent(row.total_tokens, total_tokens);
                                                                let bar_w = if row.total_tokens > 0 { pct.max(2) } else { 0 };
                                                                rsx! {
                                                                    tr { key: "{bot_id}",
                                                                        td { "{humanize_label(&bot_id)}" }
                                                                        td { class: "num", "{group_digits(row.calls)}" }
                                                                        td { class: "usage-share-cell",
                                                                            div { class: "usage-share-wrap",
                                                                                div { class: "usage-share-bar",
                                                                                    div { class: "usage-share-fill", style: "width: {bar_w}%;" }
                                                                                }
                                                                                span { class: "usage-share-pct", "{pct}%" }
                                                                            }
                                                                        }
                                                                        td { class: "num", "{group_digits(row.total_tokens)}" }
                                                                    }
                                                                }
                                                            }
                                                        }
                                                    }
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    },
                    UsageTab::InsightsAndRelay => rsx! {
                        UsageInsights { session, days: days.min(90) }
                    },
                    UsageTab::CostsAndAlerts => rsx! {
                        section { class: "usage-section",
                            div { class: "page-section-head",
                                div {
                                    h2 { class: "usage-section-title", "Costs & Alerts" }
                                    p { class: "usage-section-sub", "Where SeraBot warns you, your monthly spend, and whether your data is safe." }
                                }
                            }
                            div { class: "insights-grid",
                                AlertsCard { session }
                                CostsCard { session, is_admin }
                                if is_admin {
                                    BackupsCard { session }
                                }
                            }
                        }
                    },
                    UsageTab::Health => rsx! {
                        if let Some(quota) = report
                            .as_ref()
                            .map(|report| report.quota.daily_tasks.clone())
                            .filter(|quota| quota.limit > 0)
                        {
                            section { class: "usage-section",
                                div { class: "page-section-head",
                                    div {
                                        h2 { class: "usage-section-title", "Quota" }
                                        p { class: "usage-section-sub", "Your daily budget of bot tasks." }
                                    }
                                }
                                DailyTaskQuotaCard { quota }
                            }
                        }
                        if is_admin {
                            section { class: "usage-section",
                                div { class: "page-section-head",
                                    div {
                                        h2 { class: "usage-section-title", "Reliability" }
                                        p { class: "usage-section-sub", "How your server is doing. Only you, the owner, see this." }
                                    }
                                }
                                ServerHealthCard { session }
                            }
                        }
                        section { class: "usage-section",
                            div { class: "page-section-head",
                                div {
                                    h2 { class: "usage-section-title", "Job search" }
                                    p { class: "usage-section-sub", "What JobFinder watches for you, and where your applications stand." }
                                }
                            }
                            div { class: "insights-grid",
                                JobSearchCard { session }
                            }
                        }
                        BotHealthCard { session, days }
                        section { class: "usage-section",
                            div { class: "page-section-head",
                                div {
                                    h2 { class: "usage-section-title", "This computer" }
                                    p { class: "usage-section-sub", "What SeraBot uses on this computer." }
                                }
                                button {
                                    class: "btn ghost sm",
                                    r#type: "button",
                                    onclick: move |_| session.read().refresh_pc(),
                                    Icon { name: "refresh", size: 14 }
                                    "Refresh"
                                }
                            }
                            ThisPcCard { session }
                        }
                    },
                }
            }
        }
    }
}

/// Daily per-user task quota (Big Wave E1), shipped on `GET /v1/usage`.
/// The state is colour + icon + word: OK, Near limit or At limit.
#[component]
fn DailyTaskQuotaCard(quota: serabot_desktop::UsageDailyTaskQuota) -> Element {
    if quota.limit == 0 {
        return rsx! {};
    }
    let fill = quota_percent(quota.used, quota.limit);
    let level = Level::from_flags(quota.near_limit, quota.at_limit);
    let note = match level {
        Level::AtLimit => {
            "You reached today's task limit. New bot turns wait until UTC midnight.".to_owned()
        }
        Level::Near => "You are close to today's task limit.".to_owned(),
        Level::Ok => "Queued and completed tasks count toward this UTC-day budget.".to_owned(),
    };
    let class = match level {
        Level::Ok => "card usage-card quota-card",
        Level::Near => "card usage-card quota-card near",
        Level::AtLimit => "card usage-card quota-card at-limit",
    };
    rsx! {
        div {
            class: "{class}",
            role: if level == Level::AtLimit { "alert" } else { "status" },
            div { class: "quota-head",
                h3 { class: "card-title", "Daily task quota" }
                LevelChip { level }
            }
            div { class: "quota-body",
                span { class: "stat-tile-label", "Tasks today (UTC)" }
                span { class: "health-value",
                    "{group_digits(quota.used)} of {group_digits(quota.limit as i64)}"
                }
                Meter {
                    percent: fill,
                    label: "Daily task quota".to_owned(),
                    warn: level == Level::Near,
                    danger: level == Level::AtLimit,
                }
                span { class: "tiny", "{note}" }
            }
        }
    }
}

/// A thin progress bar. `warn` turns it amber when something is nearly full.
#[component]
fn Meter(
    percent: u32,
    label: String,
    warn: bool,
    #[props(default = false)] danger: bool,
) -> Element {
    let class = if danger {
        "meter-fill danger"
    } else if warn {
        "meter-fill warn"
    } else {
        "meter-fill"
    };
    rsx! {
        div {
            class: "meter",
            role: "progressbar",
            "aria-label": "{label}",
            "aria-valuemin": "0",
            "aria-valuemax": "100",
            "aria-valuenow": "{percent}",
            div {
                class: "{class}",
                style: "width: {percent}%;",
            }
        }
    }
}

/// One small label/value block of a health card.
#[component]
fn HealthItem(
    label: String,
    value: String,
    note: Option<String>,
    bar: Option<(u32, bool)>,
    #[props(default)] level: Option<Level>,
) -> Element {
    rsx! {
        div { class: "health-item",
            span { class: "stat-tile-label", "{label}" }
            span { class: "health-value", "{value}" }
            if let Some(level) = level {
                LevelChip { level }
            }
            if let Some((fill, warn)) = bar {
                Meter { percent: fill, label: label.clone(), warn }
            }
            if let Some(note) = note {
                span { class: "tiny", "{note}" }
            }
        }
    }
}

/// Owner only: how the server is doing. A short summary that opens into the
/// full picture. Refreshes itself every 30 seconds while the page is open.
#[component]
fn ServerHealthCard(session: Signal<Session>) -> Element {
    let mut expanded = use_signal(|| false);
    let (status, error, loading) = {
        let s = session.read();
        (
            s.state.server_health.clone(),
            s.server_health_error.clone(),
            s.server_health_loading,
        )
    };
    let Some(status) = status else {
        return rsx! {
            div { class: "card usage-card health-card",
                div { class: "card-head",
                    h3 { class: "card-title", "Your server" }
                    if loading {
                        span { class: "spinner", title: "Checking" }
                    }
                }
                if let Some(error) = error {
                    p { class: "caption", "{error}" }
                } else {
                    p { class: "caption", "Checking your server…" }
                }
            }
        };
    };
    let (level, word) = health(&status);
    let ram_pct = percent(
        status.host.memory_used_bytes,
        status.host.memory_total_bytes,
    );
    let (storage_used, storage_total) = match &status.disk {
        Some(disk) => (disk.used_bytes, disk.total_bytes),
        None => (status.storage.total_bytes, status.storage.total_bytes),
    };
    let storage_pct = percent(storage_used, storage_total);
    let open = expanded();
    let mut categories = status.storage.categories.clone();
    categories.sort_by_key(|category| std::cmp::Reverse(category.bytes));
    let biggest = categories.first().map(|c| c.bytes).unwrap_or(0);
    let process_pct = percent(status.process.memory_bytes, status.host.memory_total_bytes);
    rsx! {
        div { class: "card usage-card health-card",
            div { class: "card-head",
                h3 { class: "card-title", "Your server" }
                if loading {
                    span { class: "spinner", title: "Refreshing" }
                }
                button {
                    class: "btn ghost sm",
                    r#type: "button",
                    "aria-expanded": if open { "true" } else { "false" },
                    onclick: move |_| expanded.set(!open),
                    if open { "Hide details" } else { "Show details" }
                }
            }
            div { class: "health-summary",
                div { class: "health-item",
                    span { class: "stat-tile-label", "Server" }
                    div { class: "health-status",
                        if level == Health::Ok {
                            StatusChip { tone: Tone::Ok, text: "OK".to_owned() }
                        } else {
                            StatusChip { tone: Tone::Warn, text: "Needs a look".to_owned() }
                            span { class: "tiny", "{word}" }
                        }
                    }
                }
                HealthItem {
                    label: "Memory",
                    value: format!(
                        "{} of {}",
                        format_bytes(status.host.memory_used_bytes),
                        format_bytes(status.host.memory_total_bytes)
                    ),
                    note: None,
                    bar: Some((ram_pct, ram_pct >= 90)),
                    level: Level::from_percent(ram_pct, 90).flagged(),
                }
                HealthItem {
                    label: "Processor",
                    value: format_percent(status.host.cpu_percent),
                    note: None,
                    bar: Some((status.host.cpu_percent.clamp(0.0, 100.0) as u32, status.host.cpu_percent >= 90.0)),
                    level: Level::from_percent(status.host.cpu_percent.clamp(0.0, 100.0) as u32, 90).flagged(),
                }
                HealthItem {
                    label: "Storage",
                    value: format!("{} of {}", format_bytes(storage_used), format_bytes(storage_total)),
                    note: None,
                    bar: Some((storage_pct, storage_pct >= 90)),
                    level: Level::from_percent(storage_pct, 90).flagged(),
                }
            }
            h4 { class: "health-heading", "Background jobs" }
            div { class: "health-summary",
                HealthItem {
                    label: "Scheduler lag",
                    value: format!(
                        "{} routine{}",
                        status.reliability.scheduler_lag_routines,
                        if status.reliability.scheduler_lag_routines == 1 { "" } else { "s" }
                    ),
                    note: if status.reliability.scheduler_lag_max_secs > 0 {
                        Some(format!(
                            "Longest overdue: {}s",
                            status.reliability.scheduler_lag_max_secs
                        ))
                    } else {
                        None
                    },
                    bar: None,
                    level: Some(Level::from_problem_count(status.reliability.scheduler_lag_routines)),
                }
                HealthItem {
                    label: "Watcher uncertainty",
                    value: status.reliability.routine_claim_failures.to_string(),
                    note: Some("Uncertain scheduled tasks".into()),
                    bar: None,
                    level: Some(Level::from_problem_count(status.reliability.routine_claim_failures)),
                }
                HealthItem {
                    label: "Stale sources",
                    value: status.reliability.source_staleness.to_string(),
                    note: Some("Enabled sources past failure threshold".into()),
                    bar: None,
                    level: Some(Level::from_problem_count(status.reliability.source_staleness)),
                }
            }
            if let Some(error) = error {
                p { class: "tiny", "{error}" }
            }
            if open {
                div { class: "health-details",
                    p { class: "caption",
                        "Version {status.version} · running for {format_uptime(status.uptime_secs)} · {status.host.cpu_cores} processor cores"
                    }
                    h4 { class: "health-heading", "Memory and processor" }
                    div { class: "health-split",
                        HealthItem {
                            label: "SeraBot itself",
                            value: format_bytes(status.process.memory_bytes),
                            note: Some(format!("Processor {}", format_percent(status.process.cpu_percent))),
                            bar: Some((process_pct.max(if status.process.memory_bytes > 0 { 1 } else { 0 }), false)),
                        }
                        HealthItem {
                            label: "The whole server",
                            value: format!(
                                "{} of {}",
                                format_bytes(status.host.memory_used_bytes),
                                format_bytes(status.host.memory_total_bytes)
                            ),
                            note: Some(format!("Processor {}", format_percent(status.host.cpu_percent))),
                            bar: Some((ram_pct, ram_pct >= 90)),
                        }
                    }
                    h4 { class: "health-heading", "What takes up space" }
                    p { class: "tiny",
                        if status.storage.truncated {
                            "SeraBot data: at least {format_bytes(status.storage.total_bytes)} (counting stopped early)."
                        } else {
                            "SeraBot data: {format_bytes(status.storage.total_bytes)} in {status.storage.total_files} files."
                        }
                    }
                    div { class: "cat-list",
                        for category in categories {
                            div { key: "{category.id}", class: "cat-row",
                                span { class: "cat-label", "{category.label}" }
                                Meter {
                                    percent: bar_percent(category.bytes, biggest),
                                    label: category.label.clone(),
                                    warn: false,
                                }
                                span { class: "cat-size", "{format_bytes(category.bytes)}" }
                            }
                        }
                    }
                    h4 { class: "health-heading", "By person" }
                    p { class: "tiny", "Counts and sizes only. No messages, files or logins are shown." }
                    table { class: "usage-table",
                        thead {
                            tr {
                                th { "Person" }
                                th { class: "num", "Chats" }
                                th { class: "num", "Messages" }
                                th { class: "num", "Documents" }
                                th { class: "num", "Memories" }
                                th { class: "num", "Made files" }
                                th { class: "num", "Space" }
                            }
                        }
                        tbody {
                            for person in status.users.clone() {
                                tr { key: "{person.id}",
                                    td { "{member_label(&person.display_name, &person.email)}" }
                                    td { class: "num", "{person.conversations}" }
                                    td { class: "num", "{person.messages}" }
                                    td { class: "num", "{person.documents}" }
                                    td { class: "num", "{person.memories}" }
                                    td { class: "num", "{person.artifacts}" }
                                    td { class: "num", "{format_bytes(person.file_bytes)}" }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

/// For everyone: what this app uses on this computer.
#[component]
fn ThisPcCard(session: Signal<Session>) -> Element {
    let mut confirming = use_signal(|| false);
    let view = session.read().pc.view();
    let Some(quick) = view.quick else {
        return rsx! {
            div { class: "card usage-card health-card", "aria-busy": "true",
                div { class: "health-summary",
                    for n in 0..3 {
                        div { key: "{n}", class: "skeleton usage-skeleton", " " }
                    }
                }
            }
        };
    };
    let folders_value = match view.folders {
        Some(FoldersSize::Done {
            count,
            bytes,
            truncated,
        }) => {
            let noun = if count == 1 { "folder" } else { "folders" };
            let size = if truncated {
                format!("at least {}", format_bytes(bytes))
            } else {
                format_bytes(bytes)
            };
            if count == 0 {
                "None allowed".to_owned()
            } else {
                format!("{count} {noun}, {size}")
            }
        }
        _ => "Counting…".to_owned(),
    };
    let disk = quick.disk.map(|(total, free)| {
        let used = total.saturating_sub(free);
        (total, free, percent(used, total))
    });
    let clearable = quick.data.clearable();
    let open_confirm = confirming();
    rsx! {
        div { class: "card usage-card health-card",
            div { class: "health-summary",
                HealthItem {
                    label: "App memory",
                    value: format_bytes(quick.app_memory_bytes),
                    note: None,
                    bar: None,
                }
                HealthItem {
                    label: "App processor",
                    value: format_percent(quick.app_cpu_percent),
                    note: None,
                    bar: None,
                }
                HealthItem {
                    label: "App files",
                    value: format_bytes(quick.data.total()),
                    note: Some(format!(
                        "Settings {} · update files {} · log {}",
                        format_bytes(quick.data.settings_bytes),
                        format_bytes(quick.data.updates_bytes),
                        format_bytes(quick.data.log_bytes)
                    )),
                    bar: None,
                }
                HealthItem {
                    label: "Folders you allowed",
                    value: folders_value,
                    note: None,
                    bar: None,
                }
                if let Some((total, free, used_pct)) = disk {
                    HealthItem {
                        label: "Free disk space",
                        value: format!("{} of {}", format_bytes(free), format_bytes(total)),
                        note: None,
                        bar: Some((used_pct, used_pct >= 90)),
                    }
                }
            }
            if clearable > 0 {
                if open_confirm {
                    div { class: "vault-confirm", role: "group", "aria-label": "Confirm clear cache",
                        span { class: "caption",
                            "Delete downloaded update files ({format_bytes(clearable)})? They download again if needed. Your settings stay."
                        }
                        button {
                            class: "btn danger sm",
                            r#type: "button",
                            onclick: move |_| {
                                confirming.set(false);
                                session.write().clear_pc_cache();
                            },
                            "Clear"
                        }
                        button {
                            class: "btn ghost sm",
                            r#type: "button",
                            onclick: move |_| confirming.set(false),
                            "Keep"
                        }
                    }
                } else {
                    div { class: "card-actions",
                        button {
                            class: "btn sm",
                            r#type: "button",
                            onclick: move |_| confirming.set(true),
                            "Clear cache ({format_bytes(clearable)})"
                        }
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serabot_desktop::UsageBucket;

    #[test]
    fn digits_are_grouped() {
        assert_eq!(group_digits(0), "0");
        assert_eq!(group_digits(999), "999");
        assert_eq!(group_digits(1000), "1,000");
        assert_eq!(group_digits(1234567), "1,234,567");
        assert_eq!(group_digits(-5), "0");
    }

    #[test]
    fn share_and_tool_summaries_handle_bounds() {
        assert_eq!(share_percent(0, 100), 0);
        assert_eq!(share_percent(50, 0), 0);
        assert_eq!(share_percent(25, 100), 25);
        assert_eq!(share_percent(150, 100), 100);

        let tools = vec![
            UsageTool {
                tool: "web_search".into(),
                completed: 8,
                failed: 2,
            },
            UsageTool {
                tool: "file_read".into(),
                completed: 5,
                failed: 0,
            },
        ];
        assert_eq!(tool_summary(&tools), (13, 2, 15));
        assert_eq!(tool_summary(&[]), (0, 0, 0));

        assert_eq!(UsageTab::ALL.len(), 5);
        assert_eq!(UsageTab::Overview.label(), "Overview");
        assert_eq!(UsageTab::ModelsAndBots.label(), "Models & Bots");
        assert_eq!(UsageTab::InsightsAndRelay.label(), "Insights & Relay");
        assert_eq!(UsageTab::CostsAndAlerts.label(), "Costs & Alerts");
        assert_eq!(UsageTab::Health.label(), "Health");
    }

    #[test]
    fn bars_are_sorted_and_scaled_to_the_busiest_day() {
        let day = |d: &str, tokens: i64| UsageBucket {
            day: Some(d.into()),
            total_tokens: tokens,
            ..UsageBucket::default()
        };
        let report = UsageReport {
            per_day: vec![
                day("2026-01-02", 100),
                day("2026-01-01", 1),
                day("2026-01-03", 0),
            ],
            ..UsageReport::default()
        };
        let bars = chart_bars(&report);
        assert_eq!(bars[0], ("2026-01-01".into(), 1, 4));
        assert_eq!(bars[1].2, 100);
        assert_eq!(bars[2].2, 0);
        assert!(chart_bars(&UsageReport::default()).is_empty());
        assert_eq!(
            chart_axis_summary(&bars),
            (
                "2026-01-01".into(),
                "Peak 100 tokens".into(),
                "2026-01-03".into()
            )
        );
    }

    #[test]
    fn filter_and_sort_buckets_works_for_models_and_bots() {
        let model = |provider: &str, name: &str, calls: i64, tokens: i64| UsageModel {
            provider: provider.into(),
            model: name.into(),
            calls,
            total_tokens: tokens,
            ..UsageModel::default()
        };
        let rows = vec![
            model("openai", "gpt-5", 2, 100),
            model("mistral", "mistral-large", 5, 50),
            model("anthropic", "claude-sonnet", 1, 300),
        ];
        let by_calls = filter_sort_models(&rows, "", UsageSort::CallsDesc);
        assert_eq!(by_calls[0].model, "mistral-large");
        let filtered = filter_sort_models(&rows, "mistral", UsageSort::TokensDesc);
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].provider, "mistral");
        let by_name = filter_sort_models(&rows, "", UsageSort::NameAsc);
        assert_eq!(by_name[0].provider, "anthropic");

        let bots = vec![
            UsageBucket {
                bot: Some("research".into()),
                calls: 3,
                total_tokens: 90,
                ..UsageBucket::default()
            },
            UsageBucket {
                bot: Some("analyst".into()),
                calls: 1,
                total_tokens: 200,
                ..UsageBucket::default()
            },
        ];
        let sorted_bots = filter_sort_bots(&bots, "ana", UsageSort::TokensDesc);
        assert_eq!(sorted_bots.len(), 1);
        assert_eq!(sorted_bots[0].bot.as_deref(), Some("analyst"));
    }
}
