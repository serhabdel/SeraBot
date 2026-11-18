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

#[component]
pub fn WhatsappLinkCard(session: Signal<Session>) -> Element {
    let mut status = use_signal(|| Option::<Value>::None);
    let mut busy = use_signal(|| false);
    let mut error = use_signal(|| Option::<String>::None);
    let mut phone = use_signal(String::new);
    let mut chats = use_signal(String::new);
    let mut confirm = use_signal(|| false);

    let refresh = move || {
        spawn(async move {
            match call(session, wa::status).await {
                Ok(value) => {
                    if chats.read().is_empty() {
                        let list = value["allowlist"]
                            .as_array()
                            .map(|items| {
                                items
                                    .iter()
                                    .filter_map(Value::as_str)
                                    .map(|jid| format!("+{}", jid.split('@').next().unwrap_or(jid)))
                                    .collect::<Vec<_>>()
                                    .join("\n")
                            })
                            .unwrap_or_default();
                        chats.set(list);
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

    let value = status.read().clone().unwrap_or(Value::Null);
    let enabled = value["enabled"].as_bool().unwrap_or(false);
    let state = value["status"].as_str().unwrap_or("signed_out").to_owned();
    let connected = value["connected"].as_bool().unwrap_or(false);
    let (tone, badge) = tone_for(&state, connected);
    let linked_phone = value["phone"].as_str().map(|p| format!("+{p}"));
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
    let save_chats = move |_| {
        busy.set(true);
        error.set(None);
        let list = parse_chats(&chats.read());
        spawn(async move {
            let result = call(session, move |credentials| {
                wa::set_allowlist(credentials, &list)
            })
            .await;
            busy.set(false);
            match result {
                Ok(_) => refresh(),
                Err(message) => error.set(Some(message)),
            }
        });
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
                    refresh();
                }
                Err(message) => error.set(Some(message)),
            }
        });
    };

    rsx! {
        section { class: "github-card wa-link-card", "aria-label": "Link a spare WhatsApp number",
            div { class: "github-card-head",
                h3 { class: "mp-section-title", "Link a spare WhatsApp number" }
                StatusChip { tone, text: badge.to_owned() }
            }
            div { class: "banner warn", role: "note",
                span { class: "grow",
                    if risk.is_empty() { "Unofficial WhatsApp client: the number can be banned. Use a spare number." } else { "{risk}" }
                }
            }
            if !enabled && status.read().is_some() {
                p { class: "tiny", "Linking is turned off on this server. The admin sets [whatsapp_link] enabled = true." }
            }
            if let Some(message) = error() {
                div { class: "banner err", role: "alert", span { class: "grow", "{message}" } }
            }
            if let Some(number) = linked_phone.clone() {
                p { class: "github-login mono", "{number}" }
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
                label { class: "tiny", r#for: "wa-chats", "Chats that can talk to your WhatsApp bot (one number per line). Everyone else is ignored." }
                textarea {
                    id: "wa-chats",
                    class: "input",
                    rows: 3,
                    value: "{chats}",
                    oninput: move |event| chats.set(event.value()),
                }
                div { class: "row-actions",
                    button {
                        class: "btn ghost sm",
                        r#type: "button",
                        disabled: busy(),
                        onclick: save_chats,
                        "Save allowed chats"
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
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
        assert_eq!(tone_for("pairing", false).1, "Waiting for your phone");
        assert_eq!(tone_for("signed_out", false).1, "Not linked");
    }
}
