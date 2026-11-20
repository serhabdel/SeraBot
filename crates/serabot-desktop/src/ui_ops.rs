//! Alerts and backups cards (0.68) on the Usage tab.
//!
//! - **Alerts** (every member): where SeraBot's own alerts arrive (Telegram,
//!   and optionally one WhatsApp chat) and the last ones sent.
//! - **Backups** (owner): the last nightly backup, its test restore, the
//!   OneDrive copy, the files kept, and "Back up now".

use dioxus::prelude::*;
use serde_json::Value;

use crate::ui::Icon;
use crate::ui_widgets::StatusChip;
use serabot_desktop::ops_view::{self as ops, alert_kind_label, onedrive_line};
use serabot_desktop::session::Session;
use serabot_desktop::theme::Tone;
use serabot_desktop::usage_insights::human_bytes;
use serabot_desktop::worker::Credentials;

async fn call(
    session: Signal<Session>,
    job: impl FnOnce(&Credentials) -> Result<Value, String> + Send + 'static,
) -> Result<Value, String> {
    let Some(credentials) = session.read().credentials.clone() else {
        return Err("Not connected to the server.".into());
    };
    let (tx, rx) = tokio::sync::oneshot::channel();
    let spawned = std::thread::Builder::new()
        .name("serabot-ops".into())
        .spawn(move || {
            let _ = tx.send(job(&credentials));
        });
    if spawned.is_err() {
        return Err("Could not start the request.".into());
    }
    rx.await
        .unwrap_or_else(|_| Err("The request was interrupted.".into()))
}

fn short_time(at: &str) -> String {
    at.get(..16).unwrap_or(at).replace('T', " ")
}

#[component]
pub fn AlertsCard(session: Signal<Session>) -> Element {
    let mut view = use_signal(|| Option::<Value>::None);
    let mut error = use_signal(|| Option::<String>::None);
    let mut busy = use_signal(|| false);
    use_hook(move || {
        spawn(async move {
            match call(session, ops::get_alerts).await {
                Ok(value) => view.set(Some(value)),
                Err(message) => error.set(Some(message)),
            }
        });
    });
    let mut save = move |telegram: bool, whatsapp: Option<String>| {
        busy.set(true);
        spawn(async move {
            let result = call(session, move |credentials| {
                ops::put_alerts(credentials, telegram, whatsapp)
            })
            .await;
            busy.set(false);
            match result {
                Ok(value) => {
                    error.set(None);
                    view.set(Some(value));
                }
                Err(message) => error.set(Some(message)),
            }
        });
    };
    let mut update = move |body: Value| {
        busy.set(true);
        spawn(async move {
            let result = call(session, move |credentials| {
                ops::update_alerts(credentials, body)
            })
            .await;
            busy.set(false);
            match result {
                Ok(value) => {
                    error.set(None);
                    view.set(Some(value));
                }
                Err(message) => error.set(Some(message)),
            }
        });
    };
    let mut note = use_signal(|| Option::<String>::None);
    let preview = move |_| {
        spawn(async move {
            match call(session, ops::digest_now).await {
                Ok(value) => note.set(Some(if value["delivered"].as_bool().unwrap_or(false) {
                    "Sent. Check your Telegram or WhatsApp.".to_string()
                } else {
                    value["text"].as_str().unwrap_or_default().to_string()
                })),
                Err(message) => error.set(Some(message)),
            }
        });
    };
    let Some(current) = view() else {
        return rsx! {
            div { class: "card insights-card",
                div { class: "card-head", h3 { class: "card-title", "Alerts" } }
                if let Some(message) = error() {
                    p { class: "tiny", role: "alert", "{message}" }
                } else {
                    p { class: "tiny", "Loading\u{2026}" }
                }
            }
        };
    };
    let telegram = current["settings"]["telegram"].as_bool().unwrap_or(true);
    let telegram_linked = current["telegram_linked"].as_bool().unwrap_or(false);
    let whatsapp = current["settings"]["whatsapp_conversation_id"]
        .as_str()
        .map(str::to_string);
    let choices: Vec<(String, String)> = current["whatsapp_choices"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|choice| {
            Some((
                choice["conversation_id"].as_str()?.to_string(),
                choice["label"]
                    .as_str()
                    .unwrap_or("WhatsApp chat")
                    .to_string(),
            ))
        })
        .collect();
    let recent: Vec<(String, String, String)> = current["recent"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|alert| {
            (
                alert["at"].as_str().unwrap_or_default().to_string(),
                alert["kind"].as_str().unwrap_or_default().to_string(),
                alert["text"].as_str().unwrap_or_default().to_string(),
            )
        })
        .collect();
    let thresholds = &current["thresholds"];
    let disk = thresholds["disk_percent"].as_u64().unwrap_or(90);
    let failovers = thresholds["failovers_in_15_min"].as_u64().unwrap_or(3);
    let enabled = current["enabled"].as_bool().unwrap_or(true);
    let whatsapp_for_toggle = whatsapp.clone();
    let whatsapp_for_auto = whatsapp.clone();
    let whatsapp_for_digest = whatsapp.clone();
    let auto_send = current["auto_send_files"].as_bool().unwrap_or(true);
    let digest_on = current["weekly_digest"].as_bool().unwrap_or(true);
    let brief_on = current["morning_brief"].as_bool();
    let triage_on = current["email_triage"].as_bool();
    let review_on = current["memory_review"].as_bool();
    let base =
        serde_json::json!({"telegram": telegram, "whatsapp_conversation_id": whatsapp.clone()});
    let with = move |key: &'static str, on: bool| {
        let mut body = base.clone();
        body[key] = serde_json::json!(on);
        body
    };
    let brief_now = move |_| {
        spawn(async move {
            match call(session, ops::brief_now).await {
                Ok(value) => note.set(Some(if value["delivered"].as_bool().unwrap_or(false) {
                    "Brief sent. Check your Telegram or WhatsApp.".to_string()
                } else {
                    value["text"].as_str().unwrap_or_default().to_string()
                })),
                Err(message) => error.set(Some(message)),
            }
        });
    };
    rsx! {
        div { class: "card insights-card",
            div { class: "card-head",
                h3 { class: "card-title", "Alerts" }
                if enabled {
                    StatusChip { tone: Tone::Ok, text: "On".to_string() }
                } else {
                    StatusChip { tone: Tone::Muted, text: "Off on this server".to_string() }
                }
            }
            p { class: "tiny",
                "SeraBot tells you when a model keeps failing over ({failovers} times in 15 minutes), a sign-in expires, the PC tunnel drops during a job, or (owner) the disk passes {disk}% or a backup fails."
            }
            label { class: "insights-label",
                input {
                    r#type: "checkbox",
                    checked: telegram,
                    disabled: busy(),
                    onchange: move |event| save(event.checked(), whatsapp_for_toggle.clone()),
                }
                if telegram_linked {
                    "Send to my Telegram chat"
                } else {
                    "Send to my Telegram chat (link Telegram first)"
                }
            }
            div { class: "insights-label",
                span { "WhatsApp chat" }
                select {
                    class: "input",
                    "aria-label": "WhatsApp chat for alerts",
                    disabled: busy() || choices.is_empty(),
                    onchange: move |event| {
                        let value = event.value();
                        save(telegram, (!value.is_empty()).then_some(value));
                    },
                    option { value: "", selected: whatsapp.is_none(), "None" }
                    for (id, label) in choices.clone() {
                        option { key: "{id}", value: "{id}", selected: whatsapp.as_deref() == Some(id.as_str()), "{label}" }
                    }
                }
            }
            if choices.is_empty() {
                p { class: "tiny", "WhatsApp appears here once one of your allowed chats has talked to the WhatsApp bot." }
            }
            label { class: "insights-label",
                input {
                    r#type: "checkbox",
                    checked: auto_send,
                    disabled: busy(),
                    onchange: move |event| update(serde_json::json!({"telegram": telegram, "whatsapp_conversation_id": whatsapp_for_auto.clone(), "auto_send_files": event.checked()})),
                }
                "Send files to my own chats without asking, when I ask for them"
            }
            label { class: "insights-label",
                input {
                    r#type: "checkbox",
                    checked: digest_on,
                    disabled: busy(),
                    onchange: move |event| update(serde_json::json!({"telegram": telegram, "whatsapp_conversation_id": whatsapp_for_digest.clone(), "weekly_digest": event.checked()})),
                }
                "Weekly digest (usage, cost, failures, backups)"
            }
            if let Some(on) = brief_on {
                label { class: "insights-label",
                    input {
                        r#type: "checkbox",
                        checked: on,
                        disabled: busy(),
                        onchange: {
                            let with = with.clone();
                            move |event: Event<FormData>| update(with("morning_brief", event.checked()))
                        },
                    }
                    "Morning brief (deadlines, calendar, tasks, job matches, mail, budget)"
                }
            }
            if let Some(on) = triage_on {
                label { class: "insights-label",
                    input {
                        r#type: "checkbox",
                        checked: on,
                        disabled: busy(),
                        onchange: {
                            let with = with.clone();
                            move |event: Event<FormData>| update(with("email_triage", event.checked()))
                        },
                    }
                    "Daily email triage by the inbox bot (replies still need your approval)"
                }
            }
            if let Some(on) = review_on {
                label { class: "insights-label",
                    input {
                        r#type: "checkbox",
                        checked: on,
                        disabled: busy(),
                        onchange: {
                            let with = with.clone();
                            move |event: Event<FormData>| update(with("memory_review", event.checked()))
                        },
                    }
                    "Weekly memory review"
                }
            }
            div {
                button { class: "btn ghost sm", r#type: "button", onclick: preview, "Send me the digest now" }
                if brief_on.is_some() {
                    button { class: "btn ghost sm", r#type: "button", onclick: brief_now, "Send me my brief now" }
                }
            }
            if let Some(message) = note() {
                p { class: "tiny", style: "white-space: pre-line;", "{message}" }
            }
            if let Some(message) = error() {
                p { class: "tiny", role: "alert", "{message}" }
            }
            if recent.is_empty() {
                p { class: "tiny", "No alerts yet." }
            } else {
                div { class: "cat-list",
                    for (index, (at, kind, text)) in recent.into_iter().take(6).enumerate() {
                        div { key: "{index}-{at}", class: "cat-row",
                            span { class: "cat-label", "{alert_kind_label(&kind)}" }
                            span { class: "tiny", title: "{text}", "{text}" }
                            span { class: "cat-size", "{short_time(&at)}" }
                        }
                    }
                }
            }
        }
    }
}

#[component]
pub fn BackupsCard(session: Signal<Session>) -> Element {
    let mut view = use_signal(|| Option::<Value>::None);
    let mut error = use_signal(|| Option::<String>::None);
    let refresh = move || {
        spawn(async move {
            match call(session, ops::get_backups).await {
                Ok(value) => {
                    error.set(None);
                    view.set(Some(value));
                }
                Err(message) => error.set(Some(message)),
            }
        });
    };
    use_hook(refresh);
    let in_progress = view()
        .as_ref()
        .is_some_and(|value| value["in_progress"].as_bool().unwrap_or(false));
    // While a backup runs, check again every few seconds.
    use_effect(move || {
        let running = view
            .read()
            .as_ref()
            .is_some_and(|value| value["in_progress"].as_bool().unwrap_or(false));
        if running {
            spawn(async move {
                tokio::time::sleep(std::time::Duration::from_secs(4)).await;
                refresh();
            });
        }
    });
    let run_now = move |_| {
        spawn(async move {
            match call(session, ops::run_backup).await {
                Ok(value) => {
                    error.set(None);
                    view.set(Some(value));
                }
                Err(message) => error.set(Some(message)),
            }
        });
    };
    let Some(current) = view() else {
        return rsx! {
            div { class: "card insights-card",
                div { class: "card-head", h3 { class: "card-title", "Backups" } }
                if let Some(message) = error() {
                    p { class: "tiny", role: "alert", "{message}" }
                } else {
                    p { class: "tiny", "Loading\u{2026}" }
                }
            }
        };
    };
    let last = &current["last"];
    let has_last = last.is_object();
    let ok = last["ok"].as_bool().unwrap_or(false);
    let restore_ok = last["restore_ok"].as_bool().unwrap_or(false);
    let at = short_time(last["at"].as_str().unwrap_or_default());
    let bytes = last["bytes"].as_i64().unwrap_or(0);
    let detail = last["restore_detail"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    let onedrive = onedrive_line(last["onedrive"].as_str().unwrap_or_default());
    let failure = last["error"].as_str().map(str::to_string);
    let hour = current["hour_utc"].as_u64().unwrap_or(3);
    let keep = current["keep"].as_u64().unwrap_or(7);
    let enabled = current["enabled"].as_bool().unwrap_or(false);
    let files: Vec<(String, i64)> = current["files"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|file| {
            (
                file["file"].as_str().unwrap_or_default().to_string(),
                file["bytes"].as_i64().unwrap_or(0),
            )
        })
        .collect();
    rsx! {
        div { class: "card insights-card",
            div { class: "card-head",
                h3 { class: "card-title", "Backups" }
                if in_progress {
                    span { class: "spinner", title: "Backing up" }
                } else if !has_last {
                    StatusChip { tone: Tone::Muted, text: "None yet".to_string() }
                } else if ok {
                    StatusChip { tone: Tone::Ok, text: "Healthy".to_string() }
                } else {
                    StatusChip { tone: Tone::Danger, text: "Last one failed".to_string() }
                }
            }
            p { class: "tiny",
                if enabled {
                    "Every day at {hour}:00 UTC the database is copied, encrypted with the server key and test restored. The last {keep} are kept."
                } else {
                    "Nightly backups are off on this server ([ops] backup_enabled)."
                }
            }
            if has_last {
                div { class: "insights-kpis",
                    div { class: "insights-kpi",
                        span { class: "stat-tile-label", "Last backup" }
                        span { class: "stat-tile-value", "{at}" }
                        span { class: "stat-tile-sub", "{human_bytes(bytes)} \u{b7} {onedrive}" }
                    }
                    div { class: "insights-kpi",
                        span { class: "stat-tile-label", "Test restore" }
                        span { class: if restore_ok { "stat-tile-value" } else { "stat-tile-value warn-text" },
                            if restore_ok { "Passed" } else { "Failed" }
                        }
                        span { class: "stat-tile-sub", "{detail}" }
                    }
                }
            }
            if let Some(message) = failure {
                p { class: "tiny warn-text", role: "alert", "{message}" }
            }
            if let Some(message) = error() {
                p { class: "tiny", role: "alert", "{message}" }
            }
            div {
                button {
                    class: "btn sm",
                    r#type: "button",
                    disabled: in_progress,
                    onclick: run_now,
                    Icon { name: "refresh", size: 14 }
                    if in_progress { "Backing up\u{2026}" } else { "Back up and test now" }
                }
            }
            if !files.is_empty() {
                div { class: "cat-list",
                    for (file, size) in files {
                        div { key: "{file}", class: "cat-row",
                            span { class: "cat-label", "{file}" }
                            span { class: "tiny", "encrypted" }
                            span { class: "cat-size", "{human_bytes(size)}" }
                        }
                    }
                }
            }
        }
    }
}

#[component]
pub fn CostsCard(session: Signal<Session>, is_admin: bool) -> Element {
    let mut view = use_signal(|| Option::<Value>::None);
    let mut error = use_signal(|| Option::<String>::None);
    let mut budget_text = use_signal(String::new);
    let mut prices_text = use_signal(String::new);
    let mut editing = use_signal(|| false);
    let mut plans_text = use_signal(String::new);
    let mut note = use_signal(|| Option::<String>::None);
    let mut apply = move |value: Value| {
        budget_text.set(
            value["budget_usd"]
                .as_f64()
                .map(|budget| format!("{budget}"))
                .unwrap_or_default(),
        );
        let lines: Vec<String> = value["prices"]
            .as_array()
            .into_iter()
            .flatten()
            .map(ops::price_line)
            .collect();
        prices_text.set(lines.join("\n"));
        let plans: Vec<String> = value["plans"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|plan| {
                Some(format!(
                    "{} {}",
                    plan["provider"].as_str()?,
                    plan["monthly_usd"].as_f64()?
                ))
            })
            .collect();
        plans_text.set(plans.join("\n"));
        error.set(None);
        view.set(Some(value));
    };
    use_hook(move || {
        spawn(async move {
            match call(session, ops::get_costs).await {
                Ok(value) => apply(value),
                Err(message) => error.set(Some(message)),
            }
        });
    });
    let save_budget = move |_| {
        let parsed = ops::parse_budget(&budget_text.read());
        match parsed {
            Err(message) => error.set(Some(message)),
            Ok(budget) => {
                spawn(async move {
                    match call(session, move |credentials| {
                        ops::put_budget(credentials, budget)
                    })
                    .await
                    {
                        Ok(value) => apply(value),
                        Err(message) => error.set(Some(message)),
                    }
                });
            }
        }
    };
    let save_prices = move |_| {
        let text = prices_text.read().clone();
        let mut rows = Vec::new();
        for line in text.lines().map(str::trim).filter(|line| !line.is_empty()) {
            match ops::parse_price_line(line) {
                Ok(row) => rows.push(row),
                Err(message) => {
                    error.set(Some(message));
                    return;
                }
            }
        }
        spawn(async move {
            match call(session, move |credentials| {
                ops::put_prices(credentials, Value::Array(rows))
            })
            .await
            {
                Ok(value) => {
                    editing.set(false);
                    apply(value);
                }
                Err(message) => error.set(Some(message)),
            }
        });
    };
    let load_suggested = move |_| {
        spawn(async move {
            match call(session, ops::suggested_prices).await {
                Ok(value) => {
                    let lines: Vec<String> = value["prices"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .map(ops::price_line)
                        .collect();
                    let mut text = prices_text.read().trim().to_string();
                    for line in lines {
                        let key: Vec<&str> = line.split_whitespace().take(2).collect();
                        let exists = text.lines().any(|existing| {
                            existing.split_whitespace().take(2).collect::<Vec<_>>() == key
                        });
                        if !exists {
                            if !text.is_empty() {
                                text.push('\n');
                            }
                            text.push_str(&line);
                        }
                    }
                    prices_text.set(text);
                    editing.set(true);
                    note.set(Some(format!(
                        "Suggested prices from public pages ({}), NOT verified. Check each line against the provider's pricing page, then save.",
                        value["researched_at"].as_str().unwrap_or("recently")
                    )));
                }
                Err(message) => error.set(Some(message)),
            }
        });
    };
    let save_plans = move |_| {
        let text = plans_text.read().clone();
        let mut rows = Vec::new();
        for line in text.lines().map(str::trim).filter(|line| !line.is_empty()) {
            match ops::parse_plan_line(line) {
                Ok(row) => rows.push(row),
                Err(message) => {
                    error.set(Some(message));
                    return;
                }
            }
        }
        spawn(async move {
            match call(session, move |credentials| {
                ops::put_plans(credentials, Value::Array(rows))
            })
            .await
            {
                Ok(value) => apply(value),
                Err(message) => error.set(Some(message)),
            }
        });
    };
    let Some(current) = view() else {
        return rsx! {
            div { class: "card insights-card",
                div { class: "card-head", h3 { class: "card-title", "Costs" } }
                if let Some(message) = error() {
                    p { class: "tiny", role: "alert", "{message}" }
                } else {
                    p { class: "tiny", "Loading\u{2026}" }
                }
            }
        };
    };
    let month = current["month_usd"].as_f64().unwrap_or(0.0);
    let unpriced = current["month_unpriced_tokens"].as_i64().unwrap_or(0);
    let budget = current["budget_usd"].as_f64();
    let level = current["budget_level"].as_u64();
    let percent = budget
        .filter(|budget| *budget > 0.0)
        .map(|budget| ((month / budget) * 100.0).round().min(100.0) as u32);
    let m = |line: &Value, key: &str| line[key].as_f64().unwrap_or(0.0) / 1_000_000.0;
    let lines: Vec<(String, String, String)> = current["month_lines"]
        .as_array()
        .into_iter()
        .flatten()
        .take(8)
        .map(|line| {
            let name = line["model"].as_str().unwrap_or_default().to_string();
            let tokens = format!(
                "{} in \u{b7} {} cached \u{b7} {} out",
                ops::millions(m(line, "prompt_tokens")),
                ops::millions(m(line, "cached_tokens")),
                ops::millions(m(line, "completion_tokens"))
            );
            let cost = if line["plan"].as_bool().unwrap_or(false) {
                match line["api_equivalent_usd"].as_f64() {
                    Some(value) => format!("plan (API {})", ops::usd(value)),
                    None => "plan".to_string(),
                }
            } else {
                match line["usd"].as_f64() {
                    Some(value) => ops::usd(value),
                    None => "no price".to_string(),
                }
            };
            (name, tokens, cost)
        })
        .collect();
    let totals = |period: &str| {
        let t = &current["totals"][period];
        (
            ops::millions(t["input_m"].as_f64().unwrap_or(0.0)),
            ops::millions(t["cached_m"].as_f64().unwrap_or(0.0)),
            ops::millions(t["output_m"].as_f64().unwrap_or(0.0)),
            ops::usd(t["usd"].as_f64().unwrap_or(0.0)),
            ops::usd(t["plan_api_equivalent_usd"].as_f64().unwrap_or(0.0)),
        )
    };
    let month_totals = totals("month");
    let all_totals = totals("all_time");
    let plans: Vec<(String, String, String, String)> = current["plans"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|plan| {
            let provider = plan["provider"].as_str().unwrap_or_default().to_string();
            let paid = plan["monthly_usd"]
                .as_f64()
                .map(ops::usd)
                .unwrap_or_else(|| "not set".into());
            let api = ops::usd(plan["api_equivalent_usd"].as_f64().unwrap_or(0.0));
            let saving = match plan["saving_usd"].as_f64() {
                Some(value) if value >= 0.0 => format!("saved {}", ops::usd(value)),
                Some(value) => format!("cost {} more", ops::usd(-value)),
                None => "set your plan price".into(),
            };
            (provider, paid, api, saving)
        })
        .collect();
    let no_prices = current["prices"]
        .as_array()
        .is_none_or(|prices| prices.is_empty());
    rsx! {
        div { class: "card insights-card",
            div { class: "card-head",
                h3 { class: "card-title", "Costs this month" }
                match level {
                    Some(100) => rsx! { StatusChip { tone: Tone::Danger, text: "Over budget".to_string() } },
                    Some(_) => rsx! { StatusChip { tone: Tone::Warn, text: "Near budget".to_string() } },
                    None => rsx! { span { class: "page-count", "estimate" } },
                }
            }
            div { class: "insights-kpis",
                div { class: "insights-kpi",
                    span { class: "stat-tile-value", "{ops::usd(month)}" }
                    span { class: "stat-tile-label", "Estimated so far" }
                    span { class: "stat-tile-sub",
                        if unpriced > 0 { "{unpriced} tokens have no price yet" } else { "from your owner's price table" }
                    }
                }
                div { class: "insights-kpi",
                    span { class: "stat-tile-label", "Monthly budget (USD)" }
                    div { class: "insights-label",
                        input {
                            class: "input",
                            r#type: "text",
                            inputmode: "decimal",
                            placeholder: "none",
                            "aria-label": "Monthly budget in dollars",
                            value: "{budget_text}",
                            oninput: move |event| budget_text.set(event.value()),
                        }
                        button { class: "btn sm", r#type: "button", onclick: save_budget, "Save" }
                    }
                    if let Some(percent) = percent {
                        div { class: "meter", div { class: if level.is_some() { "meter-fill warn" } else { "meter-fill" }, style: "width: {percent}%;" } }
                    }
                    span { class: "stat-tile-sub", "Alerts at 80% and 100%. Bots keep working." }
                }
            }
            table { class: "usage-table insights-table",
                thead {
                    tr {
                        th { scope: "col", "" }
                        th { scope: "col", class: "num", "Input" }
                        th { scope: "col", class: "num", "Cached" }
                        th { scope: "col", class: "num", "Output" }
                        th { scope: "col", class: "num", "Cost" }
                        th { scope: "col", class: "num", "Plans at API price" }
                    }
                }
                tbody {
                    tr {
                        td { "This month" }
                        td { class: "num", "{month_totals.0}" }
                        td { class: "num", "{month_totals.1}" }
                        td { class: "num", "{month_totals.2}" }
                        td { class: "num", "{month_totals.3}" }
                        td { class: "num", "{month_totals.4}" }
                    }
                    tr {
                        td { "All time" }
                        td { class: "num", "{all_totals.0}" }
                        td { class: "num", "{all_totals.1}" }
                        td { class: "num", "{all_totals.2}" }
                        td { class: "num", "{all_totals.3}" }
                        td { class: "num", "{all_totals.4}" }
                    }
                }
            }
            if !lines.is_empty() {
                div { class: "cat-list",
                    for (index, (name, tokens, cost)) in lines.into_iter().enumerate() {
                        div { key: "{index}-{name}", class: "cat-row",
                            span { class: "cat-label", "{name}" }
                            span { class: "tiny", "{tokens}" }
                            span { class: "cat-size", "{cost}" }
                        }
                    }
                }
            }
            if !plans.is_empty() {
                h4 { class: "health-heading", "Plans vs API" }
                div { class: "cat-list",
                    for (provider, paid, api, saving) in plans {
                        div { key: "{provider}", class: "cat-row",
                            span { class: "cat-label", "{provider}" }
                            span { class: "tiny", "plan {paid} \u{b7} at API prices {api}" }
                            span { class: "cat-size", "{saving}" }
                        }
                    }
                }
                p { class: "tiny", "What you pay per plan, one per line: provider monthly_price (for example google-antigravity 20)." }
                textarea {
                    class: "input",
                    rows: "3",
                    "aria-label": "Plan prices",
                    value: "{plans_text}",
                    oninput: move |event| plans_text.set(event.value()),
                }
                div {
                    button { class: "btn sm", r#type: "button", onclick: save_plans, "Save plan prices" }
                }
            }
            if no_prices {
                p { class: "tiny", "No prices are set yet, so costs show as $0. The owner can add them below (USD per million tokens)." }
            }
            if is_admin {
                if editing() {
                    p { class: "tiny", "One model per line: provider model input cached output (USD per million tokens; - for no cache price). Use * as the model for a provider-wide price." }
                    if let Some(message) = note() {
                        p { class: "tiny warn-text", "{message}" }
                    }
                    textarea {
                        class: "input",
                        rows: "6",
                        "aria-label": "Price table",
                        value: "{prices_text}",
                        oninput: move |event| prices_text.set(event.value()),
                    }
                    div {
                        button { class: "btn primary sm", r#type: "button", onclick: save_prices, "Save prices" }
                        button { class: "btn ghost sm", r#type: "button", onclick: move |_| editing.set(false), "Cancel" }
                    }
                } else {
                    div {
                        button { class: "btn ghost sm", r#type: "button", onclick: move |_| editing.set(true), "Edit prices" }
                        button { class: "btn ghost sm", r#type: "button", onclick: load_suggested, "Load suggested prices" }
                    }
                }
            }
            if let Some(message) = error() {
                p { class: "tiny", role: "alert", "{message}" }
            }
        }
    }
}
