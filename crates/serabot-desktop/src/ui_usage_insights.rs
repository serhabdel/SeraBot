//! Usage insights cards (0.67): where turns come from (PC, phone, Telegram,
//! WhatsApp, schedules), what the desktop tunnel carried, how each model
//! performs (latency, failovers, share relayed) and, for the owner, the
//! server's load over the last 24 hours. Data: `GET /v1/usage/insights`.

use dioxus::prelude::*;

use crate::ui::{Icon, human_provider};
use crate::ui_usage::group_digits;
use crate::ui_widgets::StatusChip;
use serabot_desktop::session::Session;
use serabot_desktop::theme::Tone;
use serabot_desktop::usage_insights::{
    self as insights, Insights, channel_label, human_bytes, human_latency, share_percent,
    sparkline_points, tunnel_bars,
};

const SPARK_W: f32 = 280.0;
const SPARK_H: f32 = 48.0;

async fn load(session: Signal<Session>, days: u32) -> Result<Insights, String> {
    let Some(credentials) = session.read().credentials.clone() else {
        return Err("Not connected to the server.".into());
    };
    let (tx, rx) = tokio::sync::oneshot::channel();
    let spawned = std::thread::Builder::new()
        .name("serabot-usage-insights".into())
        .spawn(move || {
            let _ = tx.send(insights::fetch(&credentials, days, false));
        });
    if spawned.is_err() {
        return Err("Could not start the request.".into());
    }
    rx.await
        .unwrap_or_else(|_| Err("The request was interrupted.".into()))
}

#[component]
pub fn UsageInsights(session: Signal<Session>, days: u32) -> Element {
    let mut data = use_signal(|| Option::<Insights>::None);
    let mut error = use_signal(|| Option::<String>::None);
    let mut loaded_for = use_signal(|| 0u32);
    if loaded_for() != days {
        loaded_for.set(days);
        spawn(async move {
            match load(session, days.min(90)).await {
                Ok(value) => {
                    error.set(None);
                    data.set(Some(value));
                }
                Err(message) => error.set(Some(message)),
            }
        });
    }
    let Some(current) = data() else {
        return rsx! {
            section { class: "usage-section", "aria-labelledby": "usage-insights-title",
                div { class: "page-section-head",
                    div {
                        h2 { class: "usage-section-title", id: "usage-insights-title", "Insights" }
                        p { class: "usage-section-sub", "Channels, the PC tunnel, model speed and server load." }
                    }
                }
                if let Some(message) = error() {
                    p { class: "tiny", role: "alert", "Insights could not be loaded: {message}" }
                } else {
                    div { class: "usage-tiles", role: "status", "aria-busy": "true", "aria-label": "Loading insights",
                        for n in 0..3 {
                            div { key: "{n}", class: "stat-tile skeleton usage-skeleton", " " }
                        }
                    }
                }
            }
        };
    };
    let total_turns: i64 = current.channels.iter().map(|row| row.turns).sum();
    let channels: Vec<(String, &'static str, &'static str, i64, i64, u32)> = current
        .channels
        .iter()
        .map(|row| {
            let (label, icon) = channel_label(&row.channel);
            (
                row.channel.clone(),
                label,
                icon,
                row.turns,
                row.tokens,
                share_percent(row.turns, total_turns),
            )
        })
        .collect();
    let tunnel = current.tunnel.clone();
    let bars = tunnel_bars(&tunnel.per_day);
    let relayed_share = share_percent(tunnel.relayed_calls, tunnel.total_calls);
    let models = current.models.clone();
    let resources = current.resources.clone();
    rsx! {
        section { class: "usage-section", "aria-labelledby": "usage-insights-title",
            div { class: "page-section-head",
                div {
                    h2 { class: "usage-section-title", id: "usage-insights-title", "Insights" }
                    p { class: "usage-section-sub", "Channels, the PC tunnel, model speed and server load over {days.min(90)} days." }
                }
            }
            div { class: "insights-grid",
                div { class: "card insights-card",
                    div { class: "card-head",
                        h3 { class: "card-title", "Where you talk to your bots" }
                        span { class: "page-count", "{group_digits(total_turns)} turns" }
                    }
                    if channels.is_empty() {
                        p { class: "tiny", "No turns in this period." }
                    } else {
                        div { class: "insights-stack", role: "img", "aria-label": "Share of turns per channel",
                            for (key, label, _icon, turns, _tokens, share) in channels.clone() {
                                div {
                                    key: "{key}",
                                    class: "insights-stack-seg ch-{key}",
                                    style: "flex-grow: {turns.max(1)};",
                                    title: "{label}: {share}%",
                                }
                            }
                        }
                        div { class: "cat-list",
                            for (key, label, icon, turns, tokens, share) in channels {
                                div { key: "{key}", class: "cat-row insights-row",
                                    span { class: "cat-label insights-label",
                                        span { class: "insights-dot ch-{key}" }
                                        Icon { name: icon, size: 14 }
                                        "{label}"
                                    }
                                    div { class: "meter", div { class: "meter-fill ch-{key}", style: "width: {share}%;" } }
                                    span { class: "cat-size", title: "{group_digits(tokens)} tokens", "{group_digits(turns)} \u{b7} {share}%" }
                                }
                            }
                        }
                    }
                }
                div { class: "card insights-card",
                    div { class: "card-head",
                        h3 { class: "card-title", "PC tunnel" }
                        if current.relay_online {
                            StatusChip { tone: Tone::Ok, text: "Relaying".to_string() }
                        } else {
                            StatusChip { tone: Tone::Muted, text: "Not relaying".to_string() }
                        }
                    }
                    div { class: "insights-kpis",
                        div { class: "insights-kpi",
                            span { class: "stat-tile-value", "{relayed_share}%" }
                            span { class: "stat-tile-label", "Model calls via this PC" }
                            span { class: "stat-tile-sub", "{group_digits(tunnel.relayed_calls)} of {group_digits(tunnel.total_calls)}" }
                        }
                        div { class: "insights-kpi",
                            span { class: "stat-tile-value", "{human_bytes(tunnel.bytes_from_provider)}" }
                            span { class: "stat-tile-label", "Downloaded" }
                            span { class: "stat-tile-sub", "{human_bytes(tunnel.bytes_to_provider)} sent \u{b7} {group_digits(tunnel.tunnels)} tunnels" }
                        }
                    }
                    if bars.is_empty() {
                        p { class: "tiny", "Nothing went through this PC in this period. Turn it on under Settings \u{2192} Local folders." }
                    } else {
                        div { class: "usage-chart insights-mini-chart", role: "img", "aria-label": "Tunnel traffic per day",
                            for (index, (day, bytes, height)) in bars.into_iter().enumerate() {
                                div { key: "{day}", class: "usage-bar-col", title: "{day}: {human_bytes(bytes)}",
                                    div { class: "usage-bar", style: "height: {height}%; --bar-delay: {index.min(40) * 12}ms;" }
                                }
                            }
                        }
                        div { class: "cat-list",
                            for host in tunnel.per_host.iter().take(5).cloned() {
                                div { key: "{host.host}", class: "cat-row",
                                    span { class: "cat-label", "{host.host}" }
                                    span { class: "tiny", "{group_digits(host.tunnels)} tunnels \u{b7} ~{host.avg_seconds} s" }
                                    span { class: "cat-size", "{human_bytes(host.bytes)}" }
                                }
                            }
                        }
                    }
                }
            }
            div { class: "card insights-card",
                div { class: "card-head",
                    h3 { class: "card-title", "Model speed and reliability" }
                    span { class: "page-count", "{models.len()} models" }
                }
                if models.is_empty() {
                    p { class: "tiny", "No model calls in this period." }
                } else {
                    table { class: "usage-table insights-table",
                        thead {
                            tr {
                                th { scope: "col", "Model" }
                                th { scope: "col", class: "num", "Calls" }
                                th { scope: "col", class: "num", "Average" }
                                th { scope: "col", class: "num", "Slowest 5%" }
                                th { scope: "col", class: "num", "Via PC" }
                                th { scope: "col", class: "num", "Failovers" }
                            }
                        }
                        tbody {
                            for row in models {
                                tr { key: "{row.provider}/{row.model}",
                                    td {
                                        span { class: "insights-model", "{row.model}" }
                                        span { class: "tiny", " {human_provider(&row.provider)}" }
                                    }
                                    td { class: "num", "{group_digits(row.calls)}" }
                                    td { class: "num", "{human_latency(row.avg_latency_ms)}" }
                                    td { class: "num", "{human_latency(row.p95_latency_ms)}" }
                                    td { class: "num", "{share_percent(row.relayed_calls, row.calls)}%" }
                                    td { class: if row.failovers > 0 { "num warn-text" } else { "num" }, "{group_digits(row.failovers)}" }
                                }
                            }
                        }
                    }
                }
            }
            if let Some(samples) = resources {
                ServerLoadCard { samples }
            }
        }
    }
}

#[component]
fn ServerLoadCard(samples: Vec<insights::ResourceSample>) -> Element {
    let cpu: Vec<f32> = samples.iter().map(|s| s.cpu_percent).collect();
    let memory: Vec<f32> = samples.iter().map(|s| s.memory_percent).collect();
    let cpu_points = sparkline_points(&cpu, SPARK_W, SPARK_H);
    let memory_points = sparkline_points(&memory, SPARK_W, SPARK_H);
    let peak_cpu = cpu.iter().copied().fold(0.0f32, f32::max);
    let last_cpu = cpu.last().copied().unwrap_or(0.0);
    let last_memory = memory.last().copied().unwrap_or(0.0);
    let rss = samples.last().map(|s| s.process_rss_bytes).unwrap_or(0);
    let hours = samples.len() as f32 * 5.0 / 60.0;
    rsx! {
        div { class: "card insights-card",
            div { class: "card-head",
                h3 { class: "card-title", "Server load" }
                span { class: "page-count", "last {hours:.0} h \u{b7} owner only" }
            }
            if cpu_points.is_empty() {
                p { class: "tiny", "Sampling every 5 minutes since the server started. Check back soon." }
            } else {
                div { class: "insights-sparks",
                    div { class: "insights-spark",
                        div { class: "insights-spark-head",
                            span { class: "stat-tile-label", "CPU" }
                            span { class: "stat-tile-sub", "now {last_cpu:.0}% \u{b7} peak {peak_cpu:.0}%" }
                        }
                        svg { class: "spark spark-cpu", view_box: "0 0 {SPARK_W} {SPARK_H}", preserve_aspect_ratio: "none", role: "img", "aria-label": "CPU over time",
                            polyline { points: "{cpu_points}", fill: "none" }
                        }
                    }
                    div { class: "insights-spark",
                        div { class: "insights-spark-head",
                            span { class: "stat-tile-label", "Memory" }
                            span { class: "stat-tile-sub", "now {last_memory:.0}% \u{b7} SeraBot {human_bytes(rss as i64)}" }
                        }
                        svg { class: "spark spark-mem", view_box: "0 0 {SPARK_W} {SPARK_H}", preserve_aspect_ratio: "none", role: "img", "aria-label": "Memory over time",
                            polyline { points: "{memory_points}", fill: "none" }
                        }
                    }
                }
            }
        }
    }
}
