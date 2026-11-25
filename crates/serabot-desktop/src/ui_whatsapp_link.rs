//! The "Link a spare WhatsApp number" card (0.58).
//!
//! Talks to `/v1/whatsapp-link` only: shows the risk banner, starts linking
//! (QR code, or a pairing code for a number), shows the state, edits the chats
//! that may reach the bot, and signs the device out. The QR payload is drawn
//! locally as SVG; nothing is sent anywhere else.

use dioxus::prelude::*;
use serde_json::Value;

use crate::ui_widgets::StatusChip;
use serabot_desktop::session::Session;
use serabot_desktop::theme::Tone;
use serabot_desktop::whatsapp_link as wa;
use serabot_desktop::worker::Credentials;

/// Run one API call off the UI thread and hand its result back.
async fn call(
    session: Signal<Session>,
    job: impl FnOnce(&Credentials) -> Result<Value, String> + Send + 'static,
) -> Result<Value, String> {
    let Some(credentials) = session.read().credentials.clone() else {
        return Err("Not connected to the server.".into());
    };
    let (tx, rx) = tokio::sync::oneshot::channel();
    let spawned = std::thread::Builder::new()
        .name("serabot-whatsapp-link".into())
        .spawn(move || {
            let _ = tx.send(job(&credentials));
        });
    if spawned.is_err() {
        return Err("Could not start the request.".into());
    }
    rx.await
        .unwrap_or_else(|_| Err("The request was interrupted.".into()))
}

/// The QR payload as an SVG image, or `None` when it cannot be encoded.
pub fn qr_svg(payload: &str) -> Option<String> {
    let code = qrcode::QrCode::new(payload.as_bytes()).ok()?;
    Some(
        code.render::<qrcode::render::svg::Color>()
            .min_dimensions(220, 220)
            .quiet_zone(true)
            .build(),
    )
}

fn tone_for(status: &str, connected: bool) -> (Tone, &'static str) {
    match (status, connected) {
        ("linked", true) => (Tone::Ok, "Connected"),
        ("linked", false) => (Tone::Warn, "Linked, reconnecting"),
        ("pairing", _) => (Tone::Warn, "Waiting for your phone"),
        _ => (Tone::Muted, "Not linked"),
    }
}

/// Phone numbers from the text box: one per line or comma separated.
pub fn parse_chats(text: &str) -> Vec<String> {
    text.split([',', '\n', ';'])
        .map(str::trim)
        .filter(|entry| !entry.is_empty())
        .map(str::to_owned)
        .collect()
}

/// Display-ready profile summary extracted from `/v1/whatsapp-link`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WaProfileDisplay {
    pub name: String,
    pub initials: String,
    pub phone: String,
    pub connected: bool,
    pub linked_at: Option<String>,
}

/// Format a raw WhatsApp phone number (`"2126..."` or `"+2126..."`), or a
/// syncing placeholder when the client has not reported it yet.
pub fn format_wa_phone(raw: Option<&str>) -> String {
    match raw.map(str::trim).filter(|s| !s.is_empty()) {
        Some(number) => {
            let digits = number.strip_prefix('+').unwrap_or(number);
            format!("+{digits}")
        }
        None => "Phone syncing\u{2026}".to_string(),
    }
}

/// Derive 1–2 uppercase avatar initials from a push name, falling back to `"WA"`.
pub fn profile_initials(push_name: Option<&str>) -> String {
    let Some(name) = push_name.map(str::trim).filter(|s| !s.is_empty()) else {
        return "WA".to_string();
    };
    let mut chars = name
        .split_whitespace()
        .filter_map(|word| word.chars().find(|c| c.is_alphanumeric()));
    match (chars.next(), chars.next()) {
        (Some(a), Some(b)) => format!("{a}{b}").to_uppercase(),
        (Some(a), None) => a.to_uppercase().to_string(),
        _ => "WA".to_string(),
    }
}

/// Build a [`WaProfileDisplay`] from the `/v1/whatsapp-link` JSON payload.
pub fn profile_from_status(value: &Value) -> WaProfileDisplay {
    let push_name = value["profile"]["push_name"]
        .as_str()
        .or_else(|| value["push_name"].as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty());
    let raw_phone = value["profile"]["phone"]
        .as_str()
        .or_else(|| value["phone"].as_str());
    let connected = value["profile"]["connected"]
        .as_bool()
        .or_else(|| value["connected"].as_bool())
        .unwrap_or(false);
    let linked_at = value["profile"]["linked_at"]
        .as_str()
        .or_else(|| value["updated_at"].as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned);
    WaProfileDisplay {
        name: push_name
            .map(str::to_owned)
            .unwrap_or_else(|| "Linked WhatsApp Device".to_string()),
        initials: profile_initials(push_name),
        phone: format_wa_phone(raw_phone),
        connected,
        linked_at,
    }
}

fn allowlist_numbers(value: &Value) -> Vec<String> {
    value["allowlist"]
        .as_array()
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(|jid| {
                    let base = jid.split('@').next().unwrap_or(jid);
                    let digits = base.strip_prefix('+').unwrap_or(base);
                    format!("+{digits}")
                })
                .collect()
        })
        .unwrap_or_default()
}

fn short_timestamp(ts: &str) -> String {
    if ts.len() >= 16 && ts.as_bytes().get(10) == Some(&b'T') {
        format!("{} {}", &ts[..10], &ts[11..16])
    } else {
        ts.to_string()
    }
}

#[component]
pub fn WhatsappLinkCard(session: Signal<Session>) -> Element {
    let mut status = use_signal(|| Option::<Value>::None);
    let mut busy = use_signal(|| false);
    let mut error = use_signal(|| Option::<String>::None);
    let mut phone = use_signal(String::new);
    let mut chats = use_signal(String::new);
    let mut new_chat = use_signal(String::new);
    let mut bulk_edit = use_signal(|| false);
    let mut confirm = use_signal(|| false);

    let refresh = move || {
        spawn(async move {
            match call(session, wa::status).await {
                Ok(value) => {
                    if !*bulk_edit.read() {
                        chats.set(allowlist_numbers(&value).join("\n"));
                    }
                    status.set(Some(value));
                }
                Err(message) => error.set(Some(message)),
            }
        });
    };
    use_hook(refresh);
    // While pairing, the QR code changes every ~20 s: poll the state.
    use_future(move || async move {
        loop {
            #[cfg(windows)]
            tokio::time::sleep(std::time::Duration::from_secs(3)).await;
            #[cfg(not(windows))]
            std::future::pending::<()>().await;
            let pairing = status
                .read()
                .as_ref()
                .is_some_and(|value| value["status"] == "pairing");
            if pairing {
                refresh();
            }
        }
    });

    let status_loaded = status.read().is_some();
    let value = status.read().clone().unwrap_or(Value::Null);
    let enabled = value["enabled"].as_bool().unwrap_or(false);
    let state = value["status"].as_str().unwrap_or("signed_out").to_owned();
    let profile = profile_from_status(&value);
    let (tone, badge) = tone_for(&state, profile.connected);
    let allowed_list = allowlist_numbers(&value);
    let qr = value["pairing"]["qr"].as_str().and_then(qr_svg);
    let code = value["pairing"]["code"].as_str().map(str::to_owned);
    let risk = value["risk"].as_str().unwrap_or_default().to_owned();

    let pair = move |_| {
        busy.set(true);
        error.set(None);
        let number = phone.read().trim().to_owned();
        spawn(async move {
            let result = call(session, move |credentials| {
                wa::pair(credentials, (!number.is_empty()).then_some(number.as_str()))
            })
            .await;
            busy.set(false);
            match result {
                Ok(_) => refresh(),
                Err(message) => error.set(Some(message)),
            }
        });
    };
    let save_allowlist = Callback::new(move |(list, clear_input): (Vec<String>, bool)| {
        busy.set(true);
        error.set(None);
        spawn(async move {
            let result = call(session, move |credentials| {
                wa::set_allowlist(credentials, &list)
            })
            .await;
            match result {
                Ok(_) => {
                    if clear_input {
                        new_chat.set(String::new());
                    }
                    if let Ok(fresh) = call(session, wa::status).await {
                        chats.set(allowlist_numbers(&fresh).join("\n"));
                        status.set(Some(fresh));
                    }
                    bulk_edit.set(false);
                }
                Err(message) => error.set(Some(message)),
            }
            busy.set(false);
        });
    });
    let add_chat = Callback::new(move |()| {
        let candidate = new_chat.read().trim().to_owned();
        if candidate.is_empty() || *busy.read() {
            return;
        }
        let Some(current_status) = status.read().clone() else {
            error.set(Some(
                "Wait for WhatsApp status to load before adding chats.".into(),
            ));
            return;
        };
        let mut list = allowlist_numbers(&current_status);
        let cand_digits: String = candidate.chars().filter(|c| c.is_ascii_digit()).collect();
        let already = !cand_digits.is_empty()
            && list.iter().any(|existing| {
                existing
                    .chars()
                    .filter(|c| c.is_ascii_digit())
                    .collect::<String>()
                    == cand_digits
            });
        if already {
            new_chat.set(String::new());
            return;
        }
        list.push(candidate);
        save_allowlist.call((list, true));
    });
    let save_chats = move |_| {
        let list = parse_chats(&chats.read());
        save_allowlist.call((list, false));
    };
    let toggle_bulk = move |_| {
        let next = !*bulk_edit.read();
        if next {
            let seeded = status
                .read()
                .as_ref()
                .map(allowlist_numbers)
                .unwrap_or_default()
                .join("\n");
            chats.set(seeded);
        }
        bulk_edit.set(next);
    };
    let sign_out = move |_| {
        confirm.set(false);
        busy.set(true);
        spawn(async move {
            let result = call(session, wa::sign_out).await;
            busy.set(false);
            match result {
                Ok(_) => {
                    chats.set(String::new());
                    new_chat.set(String::new());
                    bulk_edit.set(false);
                    refresh();
                }
                Err(message) => error.set(Some(message)),
            }
        });
    };

    rsx! {
        section { class: "github-card wa-link-card", "aria-label": "Link a spare WhatsApp number",
            div { class: "github-card-head",
                h3 { class: "mp-section-title", "WhatsApp Linked Device" }
                StatusChip { tone, text: badge.to_owned() }
            }
            if state != "linked" {
                div { class: "banner warn", role: "note",
                    span { class: "grow",
                        if risk.is_empty() { "Unofficial WhatsApp client: the number can be banned. Use a spare number." } else { "{risk}" }
                    }
                }
            }
            if !enabled && status.read().is_some() {
                p { class: "tiny", "Linking is turned off on this server. The admin sets [whatsapp_link] enabled = true." }
            }
            if let Some(message) = error() {
                div { class: "banner err", role: "alert", span { class: "grow", "{message}" } }
            }
            if state == "linked" {
                div { class: "wa-profile-card",
                    div { class: "wa-avatar", "aria-hidden": "true",
                        span { "{profile.initials}" }
                        span {
                            class: if profile.connected { "wa-status-dot online" } else { "wa-status-dot reconnecting" },
                            title: if profile.connected { "Connected" } else { "Reconnecting" },
                        }
                    }
                    div { class: "wa-profile-meta",
                        div { class: "wa-profile-name", "{profile.name}" }
                        div { class: "wa-profile-phone mono", "{profile.phone}" }
                        div { class: "wa-profile-sub tiny",
                            if profile.connected { "Connected" } else { "Linked, reconnecting\u{2026}" }
                            if let Some(ts) = profile.linked_at.as_deref() {
                                " \u{b7} Updated {short_timestamp(ts)}"
                            }
                        }
                    }
                    button {
                        class: "btn ghost sm",
                        r#type: "button",
                        disabled: busy(),
                        onclick: move |_| {
                            error.set(None);
                            refresh();
                        },
                        "Refresh"
                    }
                }
            }
            if state == "pairing" {
                div { class: "wa-pairing",
                    if let Some(svg) = qr {
                        div { class: "wa-qr", "aria-label": "QR code to scan", dangerous_inner_html: "{svg}" }
                    }
                    if let Some(code) = code {
                        p { class: "mono wa-code", "aria-label": "Pairing code", "{code}" }
                    }
                    p { class: "tiny", "On the spare phone: WhatsApp > Linked devices > Link a device. Scan the code, or choose \u{201c}Link with phone number instead\u{201d} and type the pairing code." }
                }
            }
            if enabled && state != "linked" {
                div { class: "row-actions",
                    input {
                        class: "input",
                        r#type: "tel",
                        placeholder: "Spare number for a pairing code (optional), e.g. +33 6 12 34 56 78",
                        "aria-label": "Spare phone number",
                        value: "{phone}",
                        oninput: move |event| phone.set(event.value()),
                    }
                    button {
                        class: "btn primary",
                        r#type: "button",
                        disabled: busy(),
                        onclick: pair,
                        if state == "pairing" { "Restart linking" } else { "Link a number" }
                    }
                }
            }
            div { class: "wa-allowlist",
                div { class: "wa-allowlist-head",
                    label { class: "tiny", r#for: "wa-chat-add-input", "Allowed chats — only these numbers can talk to your WhatsApp bot." }
                    button {
                        class: "btn ghost sm",
                        r#type: "button",
                        disabled: busy(),
                        onclick: toggle_bulk,
                        if bulk_edit() { "Chip view" } else { "Bulk edit" }
                    }
                }
                if bulk_edit() {
                    textarea {
                        id: "wa-chats",
                        class: "input",
                        rows: 3,
                        placeholder: "+212 6 12 34 56 78",
                        value: "{chats}",
                        oninput: move |event| chats.set(event.value()),
                    }
                    div { class: "row-actions",
                        button {
                            class: "btn primary sm",
                            r#type: "button",
                            disabled: busy(),
                            onclick: save_chats,
                            "Save allowed chats"
                        }
                    }
                } else {
                    if allowed_list.is_empty() {
                        p { class: "tiny muted", "No chats allowed yet. Add a phone number below." }
                    } else {
                        div { class: "wa-chat-chips", role: "list", "aria-label": "Allowed phone numbers",
                            for (idx, number) in allowed_list.iter().enumerate() {
                                {
                                    let list_for_remove = allowed_list.clone();
                                    let label = number.clone();
                                    rsx! {
                                        span { class: "wa-chat-chip", role: "listitem", key: "{label}",
                                            span { class: "mono", "{label}" }
                                            button {
                                                class: "wa-chat-chip-remove",
                                                r#type: "button",
                                                title: "Remove {label}",
                                                "aria-label": "Remove {label}",
                                                disabled: busy(),
                                                onclick: move |_| {
                                                    let next: Vec<String> = list_for_remove
                                                        .iter()
                                                        .enumerate()
                                                        .filter(|(i, _)| *i != idx)
                                                        .map(|(_, item)| item.clone())
                                                        .collect();
                                                    save_allowlist.call((next, false));
                                                },
                                                "\u{00d7}"
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                    div { class: "row-actions wa-chat-add",
                        input {
                            id: "wa-chat-add-input",
                            class: "input",
                            r#type: "tel",
                            placeholder: "Add phone number (e.g. +212 6...)",
                            "aria-label": "Add allowed phone number",
                            value: "{new_chat}",
                            oninput: move |event| new_chat.set(event.value()),
                            onkeydown: move |event| {
                                if event.key() == Key::Enter {
                                    add_chat.call(());
                                }
                            },
                        }
                        button {
                            class: "btn primary sm",
                            r#type: "button",
                            disabled: !status_loaded || busy() || new_chat.read().trim().is_empty(),
                            onclick: move |_| add_chat.call(()),
                            "Add"
                        }
                    }
                }
            }
            if state != "signed_out" {
                if confirm() {
                    div { class: "mp-confirm", role: "alertdialog", "aria-label": "Confirm sign out",
                        p { "Sign the spare number out? The linked device is removed on WhatsApp and its session is deleted here." }
                        div { class: "row-actions",
                            button { class: "btn ghost sm", r#type: "button", autofocus: true, onclick: move |_| confirm.set(false), "Cancel" }
                            button { class: "btn danger sm", r#type: "button", disabled: busy(), onclick: sign_out, "Sign out" }
                        }
                    }
                } else {
                    div { class: "row-actions",
                        button { class: "btn ghost", r#type: "button", disabled: busy(), onclick: move |_| confirm.set(true), "Sign out" }
                    }
                }
            }
            if state == "linked" {
                p { class: "tiny wa-risk-footnote", role: "note",
                    if risk.is_empty() { "Unofficial WhatsApp client: the number can be banned. Use a spare number." } else { "{risk}" }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn chats_are_split_on_lines_commas_and_semicolons() {
        assert_eq!(
            parse_chats("+33 6 12 34 56 78\n, +1 555 0100;\n\n"),
            vec!["+33 6 12 34 56 78".to_string(), "+1 555 0100".to_string()]
        );
    }

    #[test]
    fn a_pairing_payload_renders_as_svg() {
        let svg = qr_svg("2@synthetic-pairing-payload,abc,def").unwrap();
        assert!(svg.starts_with("<?xml") || svg.contains("<svg"));
    }

    #[test]
    fn badges_follow_the_link_state() {
        assert_eq!(tone_for("linked", true).1, "Connected");
        assert_eq!(tone_for("linked", false).1, "Linked, reconnecting");
        assert_eq!(tone_for("pairing", false).1, "Waiting for your phone");
        assert_eq!(tone_for("signed_out", false).1, "Not linked");
    }

    #[test]
    fn profile_display_formats_live_profile_and_fallbacks() {
        let rich = json!({
            "status": "linked",
            "phone": "212612345678",
            "push_name": "Nadia Bennani",
            "connected": true,
            "updated_at": "2026-11-25T18:55:40Z",
            "profile": {
                "push_name": "Nadia Bennani",
                "phone": "212612345678",
                "connected": true,
                "linked_at": "2026-11-25T18:55:40Z"
            }
        });
        let display = profile_from_status(&rich);
        assert_eq!(display.name, "Nadia Bennani");
        assert_eq!(display.initials, "NB");
        assert_eq!(display.phone, "+212612345678");
        assert!(display.connected);
        assert_eq!(display.linked_at.as_deref(), Some("2026-11-25T18:55:40Z"));

        // Fallbacks when push_name and phone are still syncing.
        let syncing = json!({
            "status": "linked",
            "connected": false
        });
        let fallback = profile_from_status(&syncing);
        assert_eq!(fallback.name, "Linked WhatsApp Device");
        assert_eq!(fallback.initials, "WA");
        assert_eq!(fallback.phone, "Phone syncing\u{2026}");
        assert!(!fallback.connected);
        assert_eq!(fallback.linked_at, None);

        assert_eq!(profile_initials(Some("Sera")), "S");
        assert_eq!(format_wa_phone(Some("+33612345678")), "+33612345678");
        assert_eq!(short_timestamp("2026-11-25T18:55:40Z"), "2026-11-25 18:55");
    }
}
