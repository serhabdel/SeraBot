//! The Usage page (0.17): what the acting user's bots used in a period.
//!
//! Numbers come from `GET /v1/usage`, which only ever returns the caller's own
//! rows. The page shows summary tiles, a daily bar chart and tables by model
//! and by bot. The server does not record prices, so cost is shown as not
//! available instead of guessed.

use dioxus::prelude::*;
use serabot_desktop::UsageReport;
use serabot_desktop::session::{Session, humanize_label};

/// Ranges offered by the picker, in days.
const RANGES: [(u32, &str); 3] = [(7, "7 days"), (30, "30 days"), (90, "90 days")];

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
