//! "Bot health" card on the Usage page (0.70): for each bot, success rate,
//! average and slowest turn time, the tool that failed most, and a
//! "Needs attention" chip with the reason. Data: `GET /v1/bots/health`.

use dioxus::prelude::*;

use crate::ui_usage::group_digits;
use crate::ui_widgets::StatusChip;
use serabot_desktop::bot_health_view::{
    self as health, BotHealthReport, human_duration, success_label, top_tool_label, window_days,
};
use serabot_desktop::session::Session;
use serabot_desktop::theme::Tone;

async fn load(session: Signal<Session>, days: u32) -> Result<BotHealthReport, String> {
    let Some(credentials) = session.read().credentials.clone() else {
        return Err("Not connected to the server.".into());
    };
    let (tx, rx) = tokio::sync::oneshot::channel();
    let spawned = std::thread::Builder::new()
        .name("serabot-bot-health".into())
        .spawn(move || {
            let _ = tx.send(health::fetch(&credentials, days));
        });
    if spawned.is_err() {
        return Err("Could not start the request.".into());
    }
    rx.await
        .unwrap_or_else(|_| Err("The request was interrupted.".into()))
}

#[component]
pub fn BotHealthCard(session: Signal<Session>, days: u32) -> Element {
    let window = window_days(days);
    let mut data = use_signal(|| Option::<BotHealthReport>::None);
    let mut error = use_signal(|| Option::<String>::None);
    let mut loaded_for = use_signal(|| 0u32);
    if loaded_for() != window {
        loaded_for.set(window);
        spawn(async move {
            match load(session, window).await {
                Ok(value) => {
                    error.set(None);
                    data.set(Some(value));
                }
                Err(message) => error.set(Some(message)),
            }
        });
    }
    rsx! {
        section { class: "usage-section", "aria-labelledby": "usage-bot-health-title",
            div { class: "page-section-head",
                div {
                    h2 { class: "usage-section-title", id: "usage-bot-health-title", "Bot health" }
                    p { class: "usage-section-sub", "How each of your bots did over the last {window} days." }
                }
            }
            div { class: "card insights-card",
                if let Some(report) = data() {
                    div { class: "card-head",
                        h3 { class: "card-title", "Your bots" }
                        if report.needs_attention > 0 {
                            StatusChip { tone: Tone::Warn, text: format!("{} need attention", report.needs_attention) }
                        } else {
                            StatusChip { tone: Tone::Ok, text: "All fine".to_string() }
                        }
                    }
                    if report.bots.is_empty() {
                        p { class: "tiny", "No bot turns in this period." }
                    } else {
                        table { class: "usage-table insights-table",
                            thead {
                                tr {
                                    th { scope: "col", "Bot" }
                                    th { scope: "col", class: "num", "Turns" }
                                    th { scope: "col", class: "num", "Success" }
                                    th { scope: "col", class: "num", "Average" }
                                    th { scope: "col", class: "num", "Slowest 5%" }
                                    th { scope: "col", "Top failing tool" }
                                    th { scope: "col", "Status" }
                                }
                            }
                            tbody {
                                for bot in report.bots.clone() {
                                    tr { key: "{bot.bot}",
                                        td { span { class: "insights-model", "{bot.bot}" } }
                                        td { class: "num", "{group_digits(bot.turns)}" }
                                        td { class: "num", "{success_label(bot.success_percent)}" }
                                        td { class: "num", "{human_duration(bot.avg_ms)}" }
                                        td { class: "num", "{human_duration(bot.p95_ms)}" }
                                        td { class: "tiny", "{top_tool_label(&bot)}" }
                                        td {
                                            if bot.needs_attention {
                                                StatusChip {
                                                    tone: Tone::Warn,
                                                    text: format!("Needs attention: {}", bot.reason.clone().unwrap_or_default()),
                                                }
                                            } else {
                                                StatusChip { tone: Tone::Ok, text: "OK".to_string() }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                } else if let Some(message) = error() {
                    p { class: "tiny", role: "alert", "Bot health could not be loaded: {message}" }
                } else {
                    div { class: "usage-tiles", role: "status", "aria-busy": "true", "aria-label": "Loading bot health",
                        for n in 0..2 {
                            div { key: "{n}", class: "stat-tile skeleton usage-skeleton", " " }
                        }
                    }
                }
            }
        }
    }
}
