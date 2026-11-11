//! The Usage page (0.17): what the acting user's bots used in a period.
//!
//! Numbers come from `GET /v1/usage`, which only ever returns the caller's own
//! rows. The page shows summary tiles, a daily bar chart and tables by model
//! and by bot. The server does not record prices, so cost is shown as not
//! available instead of guessed.

use dioxus::prelude::*;
use serabot_desktop::UsageReport;
use serabot_desktop::pc_stats::FoldersSize;
use serabot_desktop::session::{Session, humanize_label};
use serabot_desktop::status_view::{
    Health, bar_percent, format_bytes, format_percent, format_uptime, health, member_label, percent,
};
use serabot_desktop::theme::{Level, Tone};

use crate::ui_widgets::{LevelChip, StatusChip};

/// How often the health cards refresh while the Usage page is open.
#[cfg(windows)]
const REFRESH_SECS: u64 = 30;

/// Ranges offered by the picker, in days.
const RANGES: [(u32, &str); 3] = [(7, "7 days"), (30, "30 days"), (90, "90 days")];

fn quota_percent(used: i64, limit: u64) -> u32 {
    if limit == 0 {
        return 0;
    }
    ((used.max(0) as u64)
        .saturating_mul(100)
        .saturating_div(limit)
        .min(100)) as u32
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

#[component]
pub fn UsagePage(session: Signal<Session>) -> Element {
    let mut session = session;
    let (days, loading, report) = {
        let s = session.read();
        (s.usage_days, s.usage_loading, s.state.usage.clone())
    };
    let bars = report.as_ref().map(chart_bars).unwrap_or_default();
    let tool_runs: i64 = report
        .as_ref()
        .map(|report| report.tools.iter().map(|t| t.completed + t.failed).sum())
        .unwrap_or(0);
    let empty = report
        .as_ref()
        .is_some_and(|report| report.totals.calls == 0);
    let is_admin = session.read().is_admin();
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
                }
                div { class: "seg usage-range", role: "group", "aria-label": "Period",
                    for (value, label) in RANGES {
                        button {
                            key: "{value}",
                            class: if days == value { "seg-btn active" } else { "seg-btn" },
                            "aria-pressed": if days == value { "true" } else { "false" },
                            disabled: loading,
                            onclick: move |_| session.write().load_usage_days(value),
                            "Last {label}"
                        }
                    }
                }
            }
            div { class: "pane-body",
                if let Some(quota) = report
                    .as_ref()
                    .map(|report| report.quota.daily_tasks.clone())
                    .filter(|quota| quota.limit > 0)
                {
                    section { class: "usage-section", "aria-labelledby": "usage-quota-title",
                        h2 { class: "usage-section-title", id: "usage-quota-title", "Quota" }
                        DailyTaskQuotaCard { quota }
                    }
                }
                if is_admin {
                    section { class: "usage-section", "aria-labelledby": "usage-reliability-title",
                        h2 { class: "usage-section-title", id: "usage-reliability-title", "Reliability" }
                        p { class: "usage-section-sub", "Only you, the owner, see this." }
                        ServerHealthCard { session }
                    }
                }
                h2 { class: "usage-section-title", "Activity" }
                if loading && report.is_none() {
                    p { class: "caption", "Loading your usage…" }
                } else if let Some(report) = report {
                    if empty {
                        div { class: "card",
                            p { class: "card-sub", "Nothing used in the last {days} days." }
                            p { class: "tiny", "Once your bots answer, the numbers show up here." }
                        }
                    } else {
                        div { class: "usage-tiles",
                            div { class: "usage-tile",
                                span { class: "usage-tile-label", "AI replies" }
                                span { class: "usage-tile-value", "{group_digits(report.totals.calls)}" }
                            }
                            div { class: "usage-tile",
                                span { class: "usage-tile-label", "Tokens used" }
                                span { class: "usage-tile-value", "{group_digits(report.totals.total_tokens)}" }
                                span { class: "tiny",
                                    "{group_digits(report.totals.prompt_tokens)} in · {group_digits(report.totals.completion_tokens)} out"
                                }
                            }
                            div { class: "usage-tile",
                                span { class: "usage-tile-label", "Tool runs" }
                                span { class: "usage-tile-value", "{group_digits(tool_runs)}" }
                            }
                            div { class: "usage-tile",
                                span { class: "usage-tile-label", "Estimated cost" }
                                span { class: "usage-tile-value", "Not available" }
                                span { class: "tiny", "Your server does not record prices." }
                            }
                        }
                        div { class: "card",
                            div { class: "card-head",
                                h3 { class: "card-title", "Tokens per day" }
                                if loading {
                                    span { class: "spinner", title: "Refreshing" }
                                }
                            }
                            div { class: "usage-chart", role: "img", "aria-label": "Tokens used per day",
                                for (day, tokens, height) in bars {
                                    div {
                                        key: "{day}",
                                        class: "usage-bar-col",
                                        title: "{day}: {group_digits(tokens)} tokens",
                                        div { class: "usage-bar", style: "height: {height}%;" }
                                    }
                                }
                            }
                        }
                        div { class: "card",
                            div { class: "card-head",
                                h3 { class: "card-title", "By model" }
                            }
                            table { class: "usage-table",
                                thead {
                                    tr {
                                        th { "Model" }
                                        th { class: "num", "Replies" }
                                        th { class: "num", "Tokens" }
                                    }
                                }
                                tbody {
                                    for row in report.per_model.clone() {
                                        tr { key: "{row.provider}/{row.model}",
                                            td { "{row.model}" }
                                            td { class: "num", "{group_digits(row.calls)}" }
                                            td { class: "num", "{group_digits(row.total_tokens)}" }
                                        }
                                    }
                                }
                            }
                        }
                        div { class: "card",
                            div { class: "card-head",
                                h3 { class: "card-title", "By bot" }
                            }
                            table { class: "usage-table",
                                thead {
                                    tr {
                                        th { "Bot" }
                                        th { class: "num", "Replies" }
                                        th { class: "num", "Tokens" }
                                    }
                                }
                                tbody {
                                    for row in report.per_bot.clone() {
                                        tr { key: "{row.bot.clone().unwrap_or_default()}",
                                            td { "{humanize_label(&row.bot.clone().unwrap_or_default())}" }
                                            td { class: "num", "{group_digits(row.calls)}" }
                                            td { class: "num", "{group_digits(row.total_tokens)}" }
                                        }
                                    }
                                }
                            }
                        }
                    }
                } else {
                    p { class: "caption", "Usage could not be loaded. Pick a period to try again." }
                }
                h2 { class: "usage-section-title", "This computer" }
                ThisPcCard { session }
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
        Level::Ok => "card quota-card",
        Level::Near => "card quota-card near",
        Level::AtLimit => "card quota-card at-limit",
    };
    rsx! {
        div {
            class: "{class}",
            role: if level == Level::AtLimit { "alert" } else { "status" },
            div { class: "quota-head",
                h3 { class: "card-title", "Daily task quota" }
                LevelChip { level }
            }
            div { class: "health-item",
                span { class: "usage-tile-label", "Tasks today (UTC)" }
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
            span { class: "usage-tile-label", "{label}" }
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
            div { class: "card health-card",
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
        div { class: "card health-card",
            div { class: "card-head",
                div {
                    h3 { class: "card-title", "Your server" }
                    p { class: "tiny", "Only you, the owner, see this." }
                }
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
                    span { class: "usage-tile-label", "Server" }
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
            h4 { class: "health-heading", "Reliability" }
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
            div { class: "card health-card",
                div { class: "card-head",
                    h3 { class: "card-title", "This PC" }
                    span { class: "spinner", title: "Reading" }
                }
                p { class: "caption", "Reading this computer…" }
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
        div { class: "card health-card",
            div { class: "card-head",
                div {
                    h3 { class: "card-title", "This PC" }
                    p { class: "tiny", "What SeraBot uses on this computer." }
                }
            }
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
    }
}
