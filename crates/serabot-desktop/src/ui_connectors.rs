//! The connector cards shown in the Integrations screen's detail sheet.
//!
//! Per-user products (Outlook, Google, Notion) draw from the account's own
//! state: Not set up yet (with a short admin hint), Connect, Connected as
//! someone with Disconnect, or an error in plain words. The state mapping lives
//! in `serabot_desktop::connector_card` and is unit-tested there. Server-wide
//! connectors (search, voice, mail API) and OneDrive keep the legacy card.
//!
//! Admins also get a "Set up Google" card that saves the Google OAuth client
//! write-only through the admin connector route.

use crate::ui::{BrandLogo, Icon};
use crate::ui_files::{CONFIRM_RESET_JS, copy_script};
use crate::ui_widgets::{SecretField, StatusChip};
use dioxus::prelude::*;
use serabot_desktop::ConnectorStatus;
use serabot_desktop::connector_card::{
    CardState, DEFAULT_ADMIN_HINT, GOOGLE_CONSOLE_URL, GOOGLE_SETUP_STEPS, NOTION_SHARE_HINT,
    connector_card_state, editable_roles, is_internal_role, legacy_connected, legacy_tone,
    pack_whatsapp_token, supports_sign_in, token_help,
};
use serabot_desktop::integrations::{connector_blurb, connector_title};
use serabot_desktop::session::Session;
use serabot_desktop::theme::Tone;
use std::collections::BTreeSet;

/// Connectors whose saved credentials live in the server-wide store and can
/// be removed from the card (0.26.0). Per-account sign-ins use Disconnect.
const REMOVABLE_SECRET_CONNECTORS: &[&str] = &["tavily", "firecrawl", "agentmail", "notion"];

/// The provider authorization pages the console can open.
///
/// These are the same fixed endpoints the server uses
/// (`crates/serabot/src/connectors.rs`). The console never builds a URL from
/// unvalidated server data: the endpoint comes from this table and only a
/// checked redirect URI is appended.
const CONNECTOR_AUTH_URLS: [(&str, &str); 8] = [
    (
        "google_drive",
        "https://accounts.google.com/o/oauth2/v2/auth",
    ),
    (
        "google_sheets",
        "https://accounts.google.com/o/oauth2/v2/auth",
    ),
    (
        "google_calendar",
        "https://accounts.google.com/o/oauth2/v2/auth",
    ),
    ("gmail", "https://accounts.google.com/o/oauth2/v2/auth"),
    (
        "outlook",
        "https://login.microsoftonline.com/common/oauth2/v2.0/authorize",
    ),
    (
        "outlook_calendar",
        "https://login.microsoftonline.com/common/oauth2/v2.0/authorize",
    ),
    ("notion", "https://api.notion.com/v1/oauth/authorize"),
    ("agentmail", "https://app.agentmail.to/"),
];

/// The authorization page of a connector, with its redirect URI appended.
///
/// `None` when the connector has no known authorization page or when the
/// redirect URI is not a plain HTTP(S) address. The result is always an
/// HTTPS URL built from the fixed table above.
fn connector_auth_url(connector_id: &str, redirect_uri: Option<&str>) -> Option<String> {
    let (_, endpoint) = CONNECTOR_AUTH_URLS
        .iter()
        .find(|(id, _)| *id == connector_id)?;
    let mut url = url::Url::parse(endpoint).ok()?;
    if url.scheme() != "https" {
        return None;
    }
    if let Some(redirect) = redirect_uri
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        if !redirect.starts_with("https://") && !redirect.starts_with("http://") {
            return None;
        }
        // Only a plain address is accepted: no shell metacharacters and no
        // query string, so the opened URL can carry nothing but the address.
        if !redirect
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || "-._~:/?@#%".contains(character))
        {
            return None;
        }
        url.query_pairs_mut().append_pair("redirect_uri", redirect);
    }
    Some(url.to_string())
}

/// What a connector card offers as its one main action.
#[derive(Clone, Debug, PartialEq, Eq)]
enum ConnectorAction {
    ConnectOneDrive,
    DisconnectOneDrive,
    SignIn,
    Authorize(String),
    AddKey,
    ReplaceKey,
}

/// The single primary action for a connector in its current state, or `None`
/// when there is nothing the owner can do from the card.
fn connector_action(connector: &ConnectorStatus, connected: bool) -> Option<ConnectorAction> {
    if connector.id == "onedrive" {
        return Some(if connected {
            ConnectorAction::DisconnectOneDrive
        } else {
            ConnectorAction::ConnectOneDrive
        });
    }
    // Signing in with the account is the one main action. A pasted key lives
    // under Advanced, never next to the button.
    if supports_sign_in(connector) {
        return Some(ConnectorAction::SignIn);
    }
    // A pasted API key beats a sign-in page: those pages only help you find one.
    let takes_key = connector.secrets.keys().any(|role| role == "api_key");
    if !takes_key
        && let Some(url) = connector_auth_url(&connector.id, connector.redirect_uri.as_deref())
    {
        return Some(ConnectorAction::Authorize(url));
    }
    if !editable_roles(connector).is_empty() {
        return Some(if connected {
            ConnectorAction::ReplaceKey
        } else {
            ConnectorAction::AddKey
        });
    }
    None
}

/// A key field name in everyday words ("api_key" becomes "API key").
fn secret_label(role: &str) -> String {
    let words = role.replace(['_', '-'], " ");
    let mut text = words.trim().to_lowercase();
    if let Some(rest) = text.strip_prefix("api ") {
        text = format!("API {rest}");
    }
    let mut chars = text.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
        None => "Key".to_owned(),
    }
}

#[component]
fn CopyUriRow(uri: String) -> Element {
    let mut copied = use_signal(|| false);
    let copy_text = uri.clone();
    rsx! {
        div { class: "copy-row",
            code { class: "tiny", "{uri}" }
            button {
                class: "btn ghost sm",
                r#type: "button",
                onclick: move |_| {
                    let _ = dioxus::document::eval(&copy_script(&copy_text));
                    copied.set(true);
                    spawn(async move {
                        let mut done = dioxus::document::eval(CONFIRM_RESET_JS);
                        let _ = done.recv::<bool>().await;
                        copied.set(false);
                    });
                },
                if copied() { "Copied" } else { "Copy" }
            }
        }
    }
}

/// One connector. Per-user products draw their own account's state; the rest
/// keep the server-wide card.
#[component]
pub(crate) fn ConnectorCard(session: Signal<Session>, connector: ConnectorStatus) -> Element {
    if connector.id == "composio" {
        rsx! { crate::ui_composio::ComposioCard { session, connector } }
    } else if connector.personal.is_some() {
        rsx! { PersonalCard { session, connector } }
    } else {
        rsx! { LegacyConnectorCard { session, connector } }
    }
}

/// An Outlook, Google, or Notion card: this account's own connection.
#[component]
fn PersonalCard(session: Signal<Session>, connector: ConnectorStatus) -> Element {
    let mut session = session;
    let mut token = use_signal(String::new);
    let mut wa_phone_id = use_signal(String::new);
    let mut wa_waba_id = use_signal(String::new);
    let mut wa_local_err = use_signal(|| None::<String>);
    let mut confirm_action = use_signal(|| None::<&'static str>);
    let is_admin = session.read().is_admin();
    let id = connector.id.clone();
    let title = connector_title(&id);
    let blurb = connector_blurb(&id);
    let (busy, waiting, error) = {
        let state = session.read();
        let flow = &state.panel.connectors;
        (
            flow.busy_with(&id),
            flow.waiting.as_deref() == Some(id.as_str()),
            flow.errors.get(&id).cloned(),
        )
    };
    let state = connector_card_state(&connector, error.as_deref()).unwrap_or(CardState::NotSetUp {
        hint: DEFAULT_ADMIN_HINT.to_owned(),
    });
    let connected_now = state.is_connected();
    use_effect(move || {
        if connected_now {
            if !token.read().is_empty() {
                token.set(String::new());
            }
            if !wa_phone_id.read().is_empty() {
                wa_phone_id.set(String::new());
            }
            if !wa_waba_id.read().is_empty() {
                wa_waba_id.set(String::new());
            }
            if wa_local_err.read().is_some() {
                wa_local_err.set(None);
            }
        } else {
            confirm_action.set(None);
        }
    });
    let redirect = connector
        .personal
        .as_ref()
        .and_then(|personal| personal.redirect_uri.clone());
    let is_notion = id == "notion";
    let is_whatsapp = id == "whatsapp";
    let help = token_help(&id);
    let token_label = help.map_or("Token", |help| help.label);
    let token_hint = help.map_or("", |help| help.hint);
    let testing = session.read().connector_testing.as_deref() == Some(id.as_str());
    let can_submit_token = if is_whatsapp {
        !wa_phone_id.read().trim().is_empty() && !token.read().trim().is_empty()
    } else {
        !token.read().trim().is_empty()
    };
    let mut submit_token = move |connector_id: &str| {
        if busy {
            return;
        }
        if connector_id == "whatsapp" {
            match pack_whatsapp_token(&wa_phone_id(), &token(), &wa_waba_id()) {
                Ok(packed) => {
                    wa_local_err.set(None);
                    session
                        .write()
                        .panel
                        .connect_connector_token(connector_id, &packed);
                }
                Err(msg) => {
                    wa_local_err.set(Some(msg.to_owned()));
                }
            }
        } else if !token().trim().is_empty() {
            session
                .write()
                .panel
                .connect_connector_token(connector_id, &token());
        }
    };
    rsx! {
        div { class: "card connector-card",
            div { class: "card-head",
                div { class: "connector-id",
                    BrandLogo { brand: id.clone(), size: 28 }
                    div {
                        h3 { class: "card-title", "{title}" }
                        if !blurb.is_empty() {
                            span { class: "card-sub", "{blurb}" }
                        }
                    }
                }
                StatusChip {
                    tone: state.tone(),
                    text: state.pill().to_owned(),
                }
            }
            {match state.clone() {
                CardState::NotSetUp { hint } => rsx! {
                    p { class: "tiny", role: "status", "Not set up yet. {hint}" }
                },
                CardState::Connect { oauth, token: takes_token } => rsx! {
                    if takes_token {
                        if is_whatsapp {
                            div { class: "field",
                                span { class: "field-label", "Phone Number ID" }
                                input {
                                    class: "input",
                                    r#type: "text",
                                    placeholder: "e.g. 109876543210987",
                                    disabled: busy,
                                    value: "{wa_phone_id}",
                                    oninput: move |e| {
                                        wa_phone_id.set(e.value());
                                        wa_local_err.set(None);
                                    },
                                }
                            }
                        }
                        div { class: "field",
                            span { class: "field-label", "{token_label}" }
                            SecretField {
                                value: token,
                                label: token_label.to_owned(),
                                placeholder: "Paste it here".to_owned(),
                                disabled: busy,
                                on_submit: {
                                    let id = id.clone();
                                    move |_| submit_token(&id)
                                },
                            }
                        }
                        if is_whatsapp {
                            div { class: "field",
                                span { class: "field-label", "WhatsApp Business Account ID (optional, for templates)" }
                                input {
                                    class: "input",
                                    r#type: "text",
                                    placeholder: "e.g. 987654321098765",
                                    disabled: busy,
                                    value: "{wa_waba_id}",
                                    oninput: move |e| {
                                        wa_waba_id.set(e.value());
                                        wa_local_err.set(None);
                                    },
                                }
                            }
                            if let Some(local_err) = wa_local_err() {
                                div { class: "banner err", role: "alert",
                                    span { class: "grow", "{local_err}" }
                                }
                            }
                        }
                        if !token_hint.is_empty() {
                            p { class: "tiny", "{token_hint}" }
                        }
                    }
                    if waiting {
                        div { class: "banner info", role: "status",
                            span { class: "spinner", title: "Waiting for browser sign-in" }
                            span { class: "grow",
                                "Finish signing in in your browser, then check status."
                            }
                            button {
                                class: "btn ghost sm",
                                r#type: "button",
                                disabled: busy,
                                onclick: move |_| session.write().refresh_connectors(),
                                "Check status"
                            }
                        }
                    }
                    div { class: "card-actions",
                        if takes_token {
                            button {
                                class: "btn primary",
                                r#type: "button",
                                disabled: busy || !can_submit_token,
                                onclick: {
                                    let id = id.clone();
                                    move |_| submit_token(&id)
                                },
                                if busy { "Connecting…" } else { "Connect" }
                            }
                            if let Some(help) = help {
                                button {
                                    class: "btn ghost",
                                    r#type: "button",
                                    title: "Open the page where you create this token",
                                    onclick: move |_| session.write().open_in_browser(help.url.to_owned()),
                                    Icon { name: "external", size: 14 }
                                    "How to get one"
                                }
                            }
                        }
                        if oauth {
                            button {
                                class: if takes_token { "btn ghost" } else { "btn primary" },
                                r#type: "button",
                                disabled: busy,
                                onclick: {
                                    let id = id.clone();
                                    move |_| session.write().panel.connect_connector(&id)
                                },
                                if busy {
                                    "Opening…"
                                } else if takes_token {
                                    "Sign in instead"
                                } else {
                                    "Connect"
                                }
                            }
                        }
                    }
                },
                CardState::Connected { account } => rsx! {
                    p { class: "tiny", role: "status",
                        {state.connected_line().unwrap_or_else(|| "Connected".to_owned())}
                    }
                    if account.is_none() && is_notion {
                        p { class: "tiny", "{NOTION_SHARE_HINT}" }
                    }
                    if let Some(action_kind) = confirm_action() {
                        div { class: "mp-confirm", role: "alertdialog", "aria-label": "Confirm connector change",
                            span { class: "tiny",
                                if action_kind == "remove_key" {
                                    "Delete the saved credentials from the server?"
                                } else {
                                    "Disconnect this account?"
                                }
                            }
                            button {
                                class: "btn danger sm",
                                r#type: "button",
                                disabled: busy,
                                onclick: {
                                    let id = id.clone();
                                    move |_| {
                                        confirm_action.set(None);
                                        if action_kind == "remove_key" {
                                            session.write().panel.remove_connector_secrets(&id);
                                        } else {
                                            session.write().panel.disconnect_connector(&id);
                                        }
                                    }
                                },
                                if action_kind == "remove_key" { "Confirm remove" } else { "Confirm disconnect" }
                            }
                            button {
                                class: "btn ghost sm",
                                r#type: "button",
                                disabled: busy,
                                onclick: move |_| confirm_action.set(None),
                                "Cancel"
                            }
                        }
                    } else {
                        div { class: "card-actions",
                            button {
                                class: "btn ghost",
                                r#type: "button",
                                disabled: busy,
                                onclick: move |_| confirm_action.set(Some("disconnect")),
                                if busy { "Working…" } else { "Disconnect" }
                            }
                            button {
                                class: "btn ghost sm",
                                r#type: "button",
                                title: "Run a read-only check against the provider with the saved credentials",
                                disabled: busy || testing,
                                onclick: {
                                    let id = id.clone();
                                    move |_| session.write().test_connector(id.clone())
                                },
                                if testing { "Testing…" } else { "Test" }
                            }
                            if is_admin && REMOVABLE_SECRET_CONNECTORS.contains(&id.as_str()) {
                                button {
                                    class: "btn ghost sm",
                                    r#type: "button",
                                    title: "Delete the saved credentials from the server (the sign-in itself stays per account)",
                                    disabled: busy,
                                    onclick: move |_| confirm_action.set(Some("remove_key")),
                                    "Remove saved key"
                                }
                            }
                        }
                    }
                    if let Some(result) = session.read().state.connector_tests.get(&id) {
                        p { class: if result.ok { "tiny ok" } else { "tiny err" }, role: "status", "{result.message}" }
                    }
                },
                CardState::Error { message } => rsx! {
                    div { class: "banner err", role: "alert",
                        span { class: "grow", "{message}" }
                    }
                    div { class: "card-actions",
                        button {
                            class: "btn ghost",
                            r#type: "button",
                            onclick: {
                                let id = id.clone();
                                move |_| session.write().panel.clear_connector_error(&id)
                            },
                            "Try again"
                        }
                        button {
                            class: "btn ghost sm",
                            r#type: "button",
                            onclick: move |_| session.write().refresh_connectors(),
                            "Refresh"
                        }
                    }
                },
            }}
            if is_admin {
                if let Some(uri) = redirect {
                    details { class: "advanced",
                        summary { "Advanced" }
                        div { class: "advanced-body",
                            p { class: "tiny", "Redirect address registered with the provider:" }
                            CopyUriRow { uri }
                        }
                    }
                }
            }
        }
    }
}

/// Admin only: the steps to create a Google OAuth client, and the two fields
/// that save it on the server. The values are write-only.
#[component]
pub(crate) fn GoogleSetupCard(
    session: Signal<Session>,
    configured: bool,
    redirect: Option<String>,
) -> Element {
    let mut session = session;
    let mut client_id = use_signal(String::new);
    let mut client_secret = use_signal(String::new);
    let mut local_submitted = use_signal(|| false);
    let (busy, error, saved) = {
        let state = session.read();
        let flow = &state.panel.connectors;
        (
            flow.google_busy,
            flow.google_error.clone(),
            flow.google_saved,
        )
    };
    let show_saved = saved && local_submitted();
    use_effect(move || {
        if show_saved {
            if !client_id.read().is_empty() {
                client_id.set(String::new());
            }
            if !client_secret.read().is_empty() {
                client_secret.set(String::new());
            }
        }
    });
    let can_save = !busy && !client_id().trim().is_empty() && !client_secret().trim().is_empty();
    rsx! {
        div { class: "card connector-card",
            div { class: "card-head",
                div { class: "connector-id",
                    BrandLogo { brand: "google_drive".to_owned(), size: 28 }
                    div {
                        h3 { class: "card-title", "Set up Google" }
                        span { class: "card-sub", "One Google app for everyone. Each person then connects their own account." }
                    }
                }
                StatusChip {
                    tone: if configured { Tone::Ok } else { Tone::Muted },
                    text: if configured { "Set up".to_owned() } else { "Not set up yet".to_owned() },
                }
            }
            details { class: "advanced", open: !configured,
                summary { if configured { "Replace the Google app" } else { "Steps" } }
                div { class: "advanced-body",
                    ol { class: "tiny",
                        for step in GOOGLE_SETUP_STEPS {
                            li { "{step}" }
                        }
                    }
                    if let Some(uri) = redirect {
                        p { class: "tiny", "Redirect address to add:" }
                        CopyUriRow { uri }
                    }
                    div { class: "card-actions",
                        button {
                            class: "btn ghost sm",
                            r#type: "button",
                            onclick: move |_| session.write().open_in_browser(GOOGLE_CONSOLE_URL.to_owned()),
                            Icon { name: "external", size: 14 }
                            "Open Google Cloud Console"
                        }
                    }
                    div { class: "form-grid",
                        div { class: "field",
                            span { class: "field-label", "Client ID" }
                            SecretField {
                                value: client_id,
                                label: "Client ID".to_owned(),
                                placeholder: "Paste it here".to_owned(),
                                disabled: busy,
                                invalid: error.is_some(),
                            }
                        }
                        div { class: "field",
                            span { class: "field-label", "Client secret" }
                            SecretField {
                                value: client_secret,
                                label: "Client secret".to_owned(),
                                placeholder: "Paste it here".to_owned(),
                                disabled: busy,
                                invalid: error.is_some(),
                                on_submit: move |_| {
                                    if !busy && !client_id().trim().is_empty() && !client_secret().trim().is_empty() {
                                        local_submitted.set(true);
                                        session.write().panel.save_google_setup(&client_id(), &client_secret());
                                    }
                                },
                            }
                        }
                    }
                    if let Some(message) = error {
                        div { class: "banner err", role: "alert", span { "{message}" } }
                    }
                    if show_saved {
                        div { class: "banner ok", role: "status", span { "Saved. People can connect Google now." } }
                    }
                    div { class: "card-actions",
                        button {
                            class: "btn primary",
                            r#type: "button",
                            disabled: !can_save,
                            onclick: move |_| {
                                local_submitted.set(true);
                                session.write().panel.save_google_setup(&client_id(), &client_secret());
                            },
                            if busy { "Saving…" } else { "Save" }
                        }
                    }
                }
            }
        }
    }
}

/// A server-wide connector (or OneDrive): its state, one main action, and the
/// developer fields behind "Advanced" for administrators.
#[component]
fn LegacyConnectorCard(session: Signal<Session>, connector: ConnectorStatus) -> Element {
    let mut session = session;
    let mut revealed_roles = use_signal(BTreeSet::<String>::new);
    let mut confirm_action = use_signal(|| None::<&'static str>);
    let can_save = session.read().can_admin();
    let is_admin = session.read().is_admin();
    let id = connector.id.clone();
    let title = connector_title(&id);
    let blurb = connector_blurb(&id);
    let enabled = session
        .read()
        .connector_drafts
        .get(&id)
        .map(|draft| draft.enabled)
        .unwrap_or(connector.enabled);
    let testing = session.read().connector_testing.as_deref() == Some(id.as_str());
    let signing_in = session.read().oauth_signing_in.as_deref() == Some(id.as_str());
    let connected = legacy_connected(&connector);
    let signed_in_with_account = supports_sign_in(&connector) && connected;
    let test_result = session.read().state.connector_tests.get(&id).cloned();
    let needs_attention = test_result.as_ref().is_some_and(|result| !result.ok);
    let values = session
        .read()
        .connector_drafts
        .get(&id)
        .map(|draft| draft.values.clone())
        .unwrap_or_default();
    let (od_busy, od_waiting, od_error) = {
        let state = session.read();
        let flow = &state.panel.onedrive;
        (flow.busy, flow.waiting, flow.error.clone())
    };
    let action = connector_action(&connector, connected);
    let can_sign_in_elevenlabs = id == "elevenlabs" && action == Some(ConnectorAction::SignIn);
    let can_act = is_admin || can_sign_in_elevenlabs;
    let can_trigger_sign_in = can_save || can_sign_in_elevenlabs;
    let key_inputs = matches!(
        action,
        Some(ConnectorAction::AddKey | ConnectorAction::ReplaceKey)
    ) && is_admin;
    let needs_admin = !can_act
        && matches!(
            action,
            Some(
                ConnectorAction::SignIn
                    | ConnectorAction::Authorize(_)
                    | ConnectorAction::AddKey
                    | ConnectorAction::ReplaceKey
            )
        );
    let has_values = values.values().any(|value| !value.trim().is_empty());
    let dirty = enabled != connector.enabled || has_values;
    rsx! {
        div { class: "card connector-card",
            div { class: "card-head",
                div { class: "connector-id",
                    BrandLogo { brand: id.clone(), size: 28 }
                    div {
                        h3 { class: "card-title", "{title}" }
                        if !blurb.is_empty() {
                            span { class: "card-sub", "{blurb}" }
                        }
                    }
                }
                StatusChip {
                    tone: legacy_tone(connected, needs_attention),
                    text: if needs_attention {
                        "Needs attention".to_owned()
                    } else if connected {
                        "Connected".to_owned()
                    } else {
                        "Not connected".to_owned()
                    },
                }
            }
            if signed_in_with_account {
                p { class: "tiny", role: "status", "Signed in with your {title} account." }
            }
            if id == "onedrive" {
                p { class: if connected { "tiny" } else { "tiny muted" }, role: "status",
                    if connected {
                        "Connected to your Microsoft OneDrive account. Bots can browse your full drive (list, search, recent, info, read) and upload with your approval."
                    } else {
                        "Once connected, bots can browse your full Microsoft OneDrive (list, search, recent, info, read) and upload with your approval."
                    }
                }
            }
            if key_inputs {
                div { class: "form-grid",
                    for (role, secret) in connector.secrets.iter().filter(|(role, _)| !is_internal_role(role)) {
                        {
                            let role = role.clone();
                            let label = secret_label(&role);
                            let value = values.get(&role).cloned().unwrap_or_default();
                            let input_id = format!("connector-{id}-{role}");
                            let is_revealed = !value.is_empty() && revealed_roles.read().contains(&role);
                            rsx! {
                                div { class: "field", key: "{role}",
                                    label { class: "field-label", r#for: "{input_id}", "{label}" }
                                    div { class: "secret-field",
                                        input {
                                            id: "{input_id}",
                                            class: "input",
                                            r#type: if is_revealed { "text" } else { "password" },
                                            value: "{value}",
                                            autocomplete: "off",
                                            spellcheck: false,
                                            placeholder: if secret.configured { "Paste a new one to replace it" } else { "Paste it here" },
                                            oninput: {
                                                let id = id.clone();
                                                let role = role.clone();
                                                move |event| {
                                                    let next = event.value();
                                                    if next.is_empty() {
                                                        revealed_roles.write().remove(&role);
                                                    }
                                                    session.write().set_connector_value(&id, &role, next);
                                                }
                                            },
                                            onkeydown: {
                                                let id = id.clone();
                                                move |event: KeyboardEvent| {
                                                    if event.key() == Key::Enter && can_save && has_values {
                                                        event.prevent_default();
                                                        session.write().add_connector_key(id.clone());
                                                    }
                                                }
                                            },
                                        }
                                        button {
                                            class: "btn icon ghost sm secret-toggle",
                                            r#type: "button",
                                            title: if is_revealed { "Hide value" } else { "Show value" },
                                            "aria-label": if is_revealed { format!("Hide {label}") } else { format!("Show {label}") },
                                            "aria-pressed": if is_revealed { "true" } else { "false" },
                                            onclick: {
                                                let role = role.clone();
                                                move |_| {
                                                    let mut set = revealed_roles.write();
                                                    if !set.remove(&role) {
                                                        set.insert(role.clone());
                                                    }
                                                }
                                            },
                                            Icon { name: if is_revealed { "eye-off".to_owned() } else { "eye".to_owned() }, size: 14 }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
            if od_waiting && id == "onedrive" {
                div { class: "banner info", role: "status",
                    span { class: "spinner", title: "Waiting for Microsoft sign-in" }
                    span { class: "grow",
                        "Finish signing in with Microsoft in your browser, then check status."
                    }
                    button {
                        class: "btn ghost sm",
                        r#type: "button",
                        disabled: od_busy,
                        onclick: move |_| session.write().refresh_connectors(),
                        "Check status"
                    }
                }
            }
            if id == "onedrive" {
                if let Some(message) = od_error {
                    div { class: "banner err", role: "alert", span { "{message}" } }
                }
            }
            if needs_admin && !connected {
                p { class: "tiny", "Your admin sets this one up." }
            }
            if let Some(result) = test_result {
                div { class: if result.ok { "banner ok" } else { "banner err" }, role: "status",
                    span { "{result.message}" }
                }
            }
            div { class: "card-actions",
                if let Some(kind) = confirm_action() {
                    div { class: "mp-confirm", role: "alertdialog", "aria-label": "Confirm disconnect",
                        span { class: "tiny",
                            if kind == "sign_out" {
                                "Sign out of your {title} account?"
                            } else {
                                "Disconnect OneDrive for this account?"
                            }
                        }
                        button {
                            class: "btn danger sm",
                            r#type: "button",
                            disabled: od_busy || !can_save,
                            onclick: {
                                let id = id.clone();
                                move |_| {
                                    confirm_action.set(None);
                                    if kind == "sign_out" {
                                        session.write().sign_out_connector(id.clone());
                                    } else {
                                        session.write().panel.disconnect_onedrive();
                                    }
                                }
                            },
                            if kind == "sign_out" { "Confirm sign out" } else { "Confirm disconnect" }
                        }
                        button {
                            class: "btn ghost sm",
                            r#type: "button",
                            onclick: move |_| confirm_action.set(None),
                            "Cancel"
                        }
                    }
                } else {
                    {match action.clone() {
                        Some(ConnectorAction::ConnectOneDrive) => rsx! {
                            button {
                                class: "btn primary",
                                r#type: "button",
                                disabled: od_busy,
                                onclick: move |_| session.write().panel.connect_onedrive(),
                                if od_busy { "Opening…" } else { "Connect OneDrive" }
                            }
                        },
                        Some(ConnectorAction::DisconnectOneDrive) => rsx! {
                            button {
                                class: "btn ghost",
                                r#type: "button",
                                title: "Sign in again with Microsoft if your OneDrive token expired",
                                disabled: od_busy,
                                onclick: move |_| session.write().panel.connect_onedrive(),
                                Icon { name: "external", size: 14 }
                                if od_busy { "Opening\u{2026}" } else { "Reconnect" }
                            }
                            button {
                                class: "btn ghost",
                                r#type: "button",
                                disabled: od_busy,
                                onclick: move |_| confirm_action.set(Some("disconnect_od")),
                                "Disconnect"
                            }
                            button {
                                class: "btn ghost",
                                r#type: "button",
                                title: "Check the OneDrive connection",
                                disabled: od_busy || testing,
                                onclick: {
                                    let id = id.clone();
                                    move |_| session.write().test_connector(id.clone())
                                },
                                if testing { "Testing\u{2026}" } else { "Test" }
                            }
                        },
                        Some(ConnectorAction::SignIn) if can_act => {
                            let callback_uri = connector
                                .oauth
                                .as_ref()
                                .and_then(|o| o.callback_uri.clone())
                                .unwrap_or_default();
                            let id = id.clone();
                            rsx! {
                                button {
                                    class: if connected { "btn ghost" } else { "btn primary" },
                                    r#type: "button",
                                    title: "Sign in with your account in your browser",
                                    disabled: signing_in || !can_trigger_sign_in,
                                    onclick: {
                                        let id = id.clone();
                                        move |_| session.write().sign_in_connector(id.clone(), callback_uri.clone())
                                    },
                                    if signing_in { "Signing in…" } else if signed_in_with_account { "Reconnect" } else { "Sign in with {title}" }
                                }
                                if signed_in_with_account {
                                    button {
                                        class: "btn ghost",
                                        r#type: "button",
                                        title: "Stop using this account sign-in",
                                        disabled: !can_trigger_sign_in,
                                        onclick: move |_| confirm_action.set(Some("sign_out")),
                                        "Sign out"
                                    }
                                }
                            }
                        },
                        Some(ConnectorAction::Authorize(url)) if is_admin => rsx! {
                            button {
                                class: if connected { "btn ghost" } else { "btn primary" },
                                r#type: "button",
                                title: "Open the sign-in page in your browser",
                                onclick: move |_| session.write().open_in_browser(url.clone()),
                                Icon { name: "external", size: 14 }
                                if connected { "Reconnect" } else { "Connect" }
                            }
                        },
                        Some(ConnectorAction::AddKey | ConnectorAction::ReplaceKey) if is_admin => {
                            let id = id.clone();
                            let key_page = connector_auth_url(&id, connector.redirect_uri.as_deref());
                            rsx! {
                                button {
                                    class: if connected { "btn ghost" } else { "btn primary" },
                                    r#type: "button",
                                    disabled: !can_save || !has_values,
                                    title: if has_values { "Save the key" } else { "Paste the key first" },
                                    onclick: move |_| session.write().add_connector_key(id.clone()),
                                    if connected { "Replace key" } else { "Add key" }
                                }
                                if let Some(url) = key_page {
                                    button {
                                        class: "btn ghost",
                                        r#type: "button",
                                        title: "Open the provider's site to get a key",
                                        onclick: move |_| session.write().open_in_browser(url.clone()),
                                        Icon { name: "external", size: 14 }
                                        "Get a key"
                                    }
                                }
                            }
                        },
                        _ => rsx! {},
                    }}
                }
            }
            if is_admin {
                details { class: "advanced",
                    summary { "Advanced" }
                    div { class: "advanced-body",
                        div { class: "chips",
                            span { class: "chip mono", "{connector.kind}" }
                            span { class: if enabled { "chip ok" } else { "chip muted" },
                                if enabled { "enabled" } else { "disabled" }
                            }
                            if dirty {
                                StatusChip { tone: Tone::Warn, text: "Unsaved changes".to_owned() }
                            }
                            if let Some(oauth) = connector.oauth.as_ref() {
                                span { class: "chip mono accent", "client_id: {oauth.client_id}" }
                            }
                            for (role, secret) in connector.secrets.iter().filter(|(role, _)| !is_internal_role(role)) {
                                if secret.configured || secret.name.as_deref().is_some_and(|n| !n.is_empty()) {
                                    span {
                                        key: "{role}",
                                        class: if secret.configured { "chip mono accent" } else { "chip mono warn" },
                                        "{role}: {secret.name.clone().unwrap_or_default()}"
                                    }
                                }
                            }
                        }
                        if let Some(uri) = connector.redirect_uri.clone() {
                            CopyUriRow { uri }
                        }
                        if id != "onedrive" {
                            p { class: "tiny",
                                "Turning a connector on makes it available. Give its tools to a bot in Bots. Reading and sending still ask you first."
                            }
                            if connector_auth_url(&id, connector.redirect_uri.as_deref()).is_some() {
                                p { class: "tiny",
                                    "Connect opens the provider's sign-in page. Register the address above with the client id the server stores, then paste the refresh token."
                                }
                            }
                            div { class: "card-actions",
                                button {
                                    class: if enabled { "btn ghost sm" } else { "btn primary sm" },
                                    r#type: "button",
                                    onclick: {
                                        let id = id.clone();
                                        move |_| session.write().toggle_connector(&id)
                                    },
                                    if enabled { "Turn off" } else { "Turn on" }
                                }
                                button {
                                    class: "btn ghost sm",
                                    r#type: "button",
                                    disabled: !can_save,
                                    onclick: {
                                        let id = id.clone();
                                        move |_| session.write().save_connector(id.clone())
                                    },
                                    "Save settings"
                                }
                                if dirty {
                                    button {
                                        class: "btn ghost sm",
                                        r#type: "button",
                                        onclick: {
                                            let id = id.clone();
                                            let saved_enabled = connector.enabled;
                                            move |_| {
                                                let mut s = session.write();
                                                if let Some(draft) = s.connector_drafts.get_mut(&id) {
                                                    draft.enabled = saved_enabled;
                                                    draft.values.clear();
                                                }
                                            }
                                        },
                                        "Discard"
                                    }
                                }
                                button {
                                    class: "btn ghost sm",
                                    r#type: "button",
                                    title: "Run a read-only check of the saved credentials",
                                    disabled: !can_save || testing,
                                    onclick: {
                                        let id = id.clone();
                                        move |_| session.write().test_connector(id.clone())
                                    },
                                    if testing { "Testing…" } else { "Test" }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

async fn run_mail_job<T, F>(session: Signal<Session>, job: F) -> Result<T, String>
where
    T: Send + 'static,
    F: FnOnce(&serabot_desktop::ApiClient) -> Result<T, serabot_desktop::ApiError> + Send + 'static,
{
    let Some(creds) = session.read().credentials.clone() else {
        return Err("Not signed in.".to_owned());
    };
    let (tx, rx) = tokio::sync::oneshot::channel();
    if let Err(err) = std::thread::Builder::new()
        .name("serabot-mail".into())
        .spawn(move || {
            let res = serabot_desktop::ApiClient::new(creds.endpoint.clone(), creds.token.clone())
                .and_then(|client| job(&client))
                .map_err(|e| e.to_string());
            let _ = tx.send(res);
        })
    {
        return Err(format!("Could not start request thread: {err}"));
    }
    rx.await
        .map_err(|_| "Request thread exited unexpectedly.".to_owned())?
}

/// Native per-user SMTP/IMAP Mail connector card (Hostinger preset + live test results).
#[component]
pub(crate) fn MailConnectorCard(
    session: Signal<Session>,
    on_status: EventHandler<serabot_desktop::MailStatus>,
) -> Element {
    use serabot_desktop::{
        MailConnectRequest, MailConnectResponse, MailProbeOutcome, MailSecurity, MailStatus,
        validate_mail_connect,
    };
    let mut status = use_signal(|| Option::<MailStatus>::None);
    let mut loading = use_signal(|| true);
    let mut busy = use_signal(|| false);
    let mut error = use_signal(|| Option::<String>::None);
    let mut test_result = use_signal(|| Option::<MailConnectResponse>::None);
    let mut confirm_disconnect = use_signal(|| false);

    let mut smtp_host = use_signal(String::new);
    let mut smtp_port = use_signal(|| "587".to_owned());
    let mut smtp_security = use_signal(|| MailSecurity::StartTls);
    let mut imap_host = use_signal(String::new);
    let mut imap_port = use_signal(|| "993".to_owned());
    let mut imap_security = use_signal(|| MailSecurity::Ssl);
    let mut username = use_signal(String::new);
    let mut password = use_signal(String::new);
    let mut from_addr = use_signal(String::new);
    let mut account_name = use_signal(String::new);
    let mut signature = use_signal(String::new);
    let mut mail_window = use_signal(|| false);

    let load_status = move || {
        loading.set(true);
        error.set(None);
        spawn(async move {
            let res = run_mail_job(session, |c| c.mail_status()).await;
            loading.set(false);
            match res {
                Ok(st) => {
                    if let Some(h) = st.smtp_host.as_deref().filter(|s| !s.is_empty())
                        && smtp_host.read().is_empty()
                    {
                        smtp_host.set(h.to_owned());
                    }
                    if let Some(h) = st.imap_host.as_deref().filter(|s| !s.is_empty())
                        && imap_host.read().is_empty()
                    {
                        imap_host.set(h.to_owned());
                    }
                    if let Some(u) = st.username.as_deref().filter(|s| !s.is_empty())
                        && username.read().is_empty()
                    {
                        username.set(u.to_owned());
                    }
                    on_status.call(st.clone());
                    status.set(Some(st));
                }
                Err(msg) => error.set(Some(msg)),
            }
        });
    };
    use_hook(load_status);

    let connected = status().as_ref().is_some_and(|s| s.connected);
    let connected_user = status()
        .as_ref()
        .and_then(|s| s.username.clone())
        .unwrap_or_default();
    let connected_smtp = status()
        .as_ref()
        .and_then(|s| s.smtp_host.clone())
        .unwrap_or_default();
    let connected_imap = status()
        .as_ref()
        .and_then(|s| s.imap_host.clone())
        .unwrap_or_default();
    let accounts_line = status()
        .as_ref()
        .map(|s| {
            s.accounts
                .iter()
                .map(|a| {
                    let label = if a.account.is_empty() {
                        "default".to_owned()
                    } else {
                        a.account.clone()
                    };
                    match a.username.as_deref().filter(|u| !u.is_empty()) {
                        Some(u) => format!("{label} ({u})"),
                        None => label,
                    }
                })
                .collect::<Vec<_>>()
                .join(", ")
        })
        .unwrap_or_default();

    let submit_connect = move |_| {
        if busy() {
            return;
        }
        error.set(None);
        test_result.set(None);
        let Ok(sp) = smtp_port().trim().parse::<u16>() else {
            error.set(Some("Enter a valid SMTP port (1–65535).".to_owned()));
            return;
        };
        let Ok(ip) = imap_port().trim().parse::<u16>() else {
            error.set(Some("Enter a valid IMAP port (1–65535).".to_owned()));
            return;
        };
        let from_val = from_addr().trim().to_owned();
        let account_val = account_name().trim().to_owned();
        let sig_val = signature().trim().to_owned();
        let req = MailConnectRequest {
            account: (!account_val.is_empty()).then_some(account_val),
            signature: (!sig_val.is_empty()).then_some(sig_val),
            smtp_host: smtp_host().trim().to_owned(),
            smtp_port: sp,
            smtp_security: smtp_security(),
            username: username().trim().to_owned(),
            password: password(),
            imap_host: imap_host().trim().to_owned(),
            imap_port: ip,
            imap_security: imap_security(),
            from: (!from_val.is_empty()).then_some(from_val),
        };
        if let Err(err) = validate_mail_connect(&req) {
            error.set(Some(err.to_string()));
            return;
        }
        busy.set(true);
        spawn(async move {
            let res = run_mail_job(session, move |c| c.mail_connect(&req)).await;
            busy.set(false);
            match res {
                Ok(reply) => {
                    password.set(String::new());
                    test_result.set(Some(reply));
                    // Re-fetch so the accounts list includes the new mailbox.
                    if let Ok(st) = run_mail_job(session, |c| c.mail_status()).await {
                        on_status.call(st.clone());
                        status.set(Some(st));
                    }
                }
                Err(msg) => error.set(Some(msg)),
            }
        });
    };

    let apply_hostinger = move |_| {
        smtp_host.set("smtp.hostinger.com".to_owned());
        smtp_port.set("587".to_owned());
        smtp_security.set(MailSecurity::StartTls);
        imap_host.set("imap.hostinger.com".to_owned());
        imap_port.set("993".to_owned());
        imap_security.set(MailSecurity::Ssl);
        error.set(None);
    };

    let can_submit = !busy()
        && !smtp_host().trim().is_empty()
        && !imap_host().trim().is_empty()
        && !username().trim().is_empty()
        && !password().is_empty();

    rsx! {
        div { class: "card connector-card mail-connector-card",
            div { class: "card-head",
                div { class: "connector-id",
                    BrandLogo { brand: "agentmail".to_owned(), size: 28 }
                    div {
                        h3 { class: "card-title", "Mail (SMTP / IMAP)" }
                        span { class: "card-sub", "Personal mailbox for reading and sending email (writes ask you first)." }
                    }
                }
                StatusChip {
                    tone: if connected { Tone::Ok } else { Tone::Muted },
                    text: if loading() && status().is_none() {
                        "Checking\u{2026}".to_owned()
                    } else if connected {
                        "Connected".to_owned()
                    } else {
                        "Not connected".to_owned()
                    },
                }
            }

            if connected {
                div { class: "mail-connected-summary", role: "status",
                    if !connected_user.is_empty() {
                        p { class: "tiny",
                            strong { "Account: " }
                            span { class: "mono", "{connected_user}" }
                        }
                    }
                    if !connected_smtp.is_empty() || !connected_imap.is_empty() {
                        p { class: "tiny muted",
                            "SMTP: "
                            span { class: "mono", "{connected_smtp}" }
                            " · IMAP: "
                            span { class: "mono", "{connected_imap}" }
                        }
                    }
                    if !accounts_line.is_empty() {
                        p { class: "tiny muted",
                            "Mailboxes: "
                            span { class: "mono", "{accounts_line}" }
                        }
                    }
                }
                if confirm_disconnect() {
                    div { class: "mp-confirm", role: "alertdialog", "aria-label": "Confirm mail disconnect",
                        span { class: "tiny", "Disconnect this mailbox and delete saved credentials?" }
                        button {
                            class: "btn danger sm",
                            r#type: "button",
                            disabled: busy(),
                            onclick: move |_| {
                                confirm_disconnect.set(false);
                                busy.set(true);
                                error.set(None);
                                test_result.set(None);
                                let acct = account_name().trim().to_owned();
                                let acct = if acct.is_empty() { None } else { Some(acct) };
                                spawn(async move {
                                    let res = run_mail_job(session, move |c| c.mail_disconnect(acct.as_deref())).await;
                                    busy.set(false);
                                    match res {
                                        Ok(_) => {
                                            if let Ok(st) = run_mail_job(session, |c| c.mail_status()).await {
                                                on_status.call(st.clone());
                                                status.set(Some(st));
                                            } else {
                                                let next = MailStatus::default();
                                                on_status.call(next.clone());
                                                status.set(Some(next));
                                            }
                                        }
                                        Err(msg) => error.set(Some(msg)),
                                    }
                                });
                            },
                            "Confirm disconnect"
                        }
                        button {
                            class: "btn ghost sm",
                            r#type: "button",
                            disabled: busy(),
                            onclick: move |_| confirm_disconnect.set(false),
                            "Cancel"
                        }
                    }
                } else {
                    div { class: "card-actions",
                        button {
                            class: "btn ghost",
                            r#type: "button",
                            disabled: busy(),
                            onclick: move |_| mail_window.set(true),
                            "Open inbox"
                        }
                        button {
                            class: "btn ghost",
                            r#type: "button",
                            disabled: busy(),
                            onclick: move |_| confirm_disconnect.set(true),
                            "Disconnect"
                        }
                    }
                }
            }

            if let Some(res) = test_result() {
                div { class: "mail-probe-results", role: "status",
                    div {
                        class: match &res.smtp_test {
                            MailProbeOutcome::Ok => "banner ok",
                            MailProbeOutcome::Error(_) => "banner err",
                        },
                        span { class: "mail-probe-label", "SMTP" }
                        span { class: "grow",
                            match &res.smtp_test {
                                MailProbeOutcome::Ok => "Login OK".to_owned(),
                                MailProbeOutcome::Error(err) => err.clone(),
                            }
                        }
                    }
                    div {
                        class: match &res.imap_test {
                            MailProbeOutcome::Ok => "banner ok",
                            MailProbeOutcome::Error(_) => "banner err",
                        },
                        span { class: "mail-probe-label", "IMAP" }
                        span { class: "grow",
                            match &res.imap_test {
                                MailProbeOutcome::Ok => "Login OK".to_owned(),
                                MailProbeOutcome::Error(err) => err.clone(),
                            }
                        }
                    }
                }
            }

            if let Some(msg) = error() {
                div { class: "banner err", role: "alert", span { class: "grow", "{msg}" } }
            }

            div { class: "mail-preset-row",
                span { class: "tiny muted", "Quick preset:" }
                button {
                    class: "btn ghost sm",
                    r#type: "button",
                    disabled: busy(),
                    onclick: apply_hostinger,
                    "Hostinger"
                }
            }

            div { class: "form-grid mail-form-grid",
                div { class: "field mail-from-field",
                    label { class: "field-label", r#for: "mail-account", "Account name (optional)" }
                    input {
                        id: "mail-account",
                        class: "input",
                        r#type: "text",
                        placeholder: "default (leave empty for the main mailbox)",
                        disabled: busy(),
                        value: "{account_name}",
                        oninput: move |e| account_name.set(e.value()),
                    }
                }
                div { class: "field",
                    label { class: "field-label", r#for: "mail-smtp-host", "SMTP host" }
                    input {
                        id: "mail-smtp-host",
                        class: "input",
                        r#type: "text",
                        placeholder: "smtp.hostinger.com",
                        disabled: busy(),
                        value: "{smtp_host}",
                        oninput: move |e| smtp_host.set(e.value()),
                    }
                }
                div { class: "field",
                    label { class: "field-label", r#for: "mail-smtp-port", "SMTP port" }
                    input {
                        id: "mail-smtp-port",
                        class: "input",
                        r#type: "text",
                        placeholder: "587",
                        disabled: busy(),
                        value: "{smtp_port}",
                        oninput: move |e| smtp_port.set(e.value()),
                    }
                }
                div { class: "field",
                    label { class: "field-label", r#for: "mail-smtp-sec", "SMTP security" }
                    select {
                        id: "mail-smtp-sec",
                        class: "input",
                        disabled: busy(),
                        value: "{smtp_security().as_str()}",
                        onchange: move |e| smtp_security.set(MailSecurity::parse(&e.value())),
                        for sec in MailSecurity::ALL {
                            option { key: "{sec.as_str()}", value: "{sec.as_str()}", "{sec.label()}" }
                        }
                    }
                }
                div { class: "field",
                    label { class: "field-label", r#for: "mail-imap-host", "IMAP host" }
                    input {
                        id: "mail-imap-host",
                        class: "input",
                        r#type: "text",
                        placeholder: "imap.hostinger.com",
                        disabled: busy(),
                        value: "{imap_host}",
                        oninput: move |e| imap_host.set(e.value()),
                    }
                }
                div { class: "field",
                    label { class: "field-label", r#for: "mail-imap-port", "IMAP port" }
                    input {
                        id: "mail-imap-port",
                        class: "input",
                        r#type: "text",
                        placeholder: "993",
                        disabled: busy(),
                        value: "{imap_port}",
                        oninput: move |e| imap_port.set(e.value()),
                    }
                }
                div { class: "field",
                    label { class: "field-label", r#for: "mail-imap-sec", "IMAP security" }
                    select {
                        id: "mail-imap-sec",
                        class: "input",
                        disabled: busy(),
                        value: "{imap_security().as_str()}",
                        onchange: move |e| imap_security.set(MailSecurity::parse(&e.value())),
                        for sec in MailSecurity::ALL {
                            option { key: "{sec.as_str()}", value: "{sec.as_str()}", "{sec.label()}" }
                        }
                    }
                }
                div { class: "field",
                    label { class: "field-label", r#for: "mail-username", "Username / Email" }
                    input {
                        id: "mail-username",
                        class: "input",
                        r#type: "text",
                        placeholder: "you@domain.com",
                        disabled: busy(),
                        value: "{username}",
                        oninput: move |e| username.set(e.value()),
                    }
                }
                div { class: "field",
                    label { class: "field-label", r#for: "mail-password", "Password (write-only)" }
                    SecretField {
                        value: password,
                        label: "Mailbox password".to_owned(),
                        placeholder: if connected { "Enter password to update".to_owned() } else { "Mailbox password".to_owned() },
                        disabled: busy(),
                    }
                }
                div { class: "field mail-from-field",
                    label { class: "field-label", r#for: "mail-from", "From address (optional)" }
                    input {
                        id: "mail-from",
                        class: "input",
                        r#type: "text",
                        placeholder: "Defaults to username",
                        disabled: busy(),
                        value: "{from_addr}",
                        oninput: move |e| from_addr.set(e.value()),
                    }
                }
                div { class: "field mail-from-field",
                    label { class: "field-label", r#for: "mail-signature", "Signature (optional, appended to every send)" }
                    textarea {
                        id: "mail-signature",
                        class: "input",
                        rows: 3,
                        placeholder: "Jane Doe — Acme",
                        disabled: busy(),
                        value: "{signature}",
                        oninput: move |e| signature.set(e.value()),
                    }
                }
            }

            p { class: "tiny muted",
                "Credentials are sealed per user on the server and never sent back to this app. Connecting tests both SMTP and IMAP login."
            }

            div { class: "card-actions",
                button {
                    class: "btn primary",
                    r#type: "button",
                    disabled: !can_submit,
                    onclick: submit_connect,
                    if busy() {
                        "Testing & saving\u{2026}"
                    } else if connected {
                        "Update & test"
                    } else {
                        "Connect & test"
                    }
                }
            }

            if mail_window() && connected {
                MailWindow {
                    session,
                    accounts: status().map(|s| s.accounts.clone()).unwrap_or_default(),
                    on_close: move |_| mail_window.set(false),
                }
            }
        }
    }
}

/// Mail reading window (0.74.5). Rendered only while a mailbox is connected,
/// so the window exists exactly when the mail integration is in use.
#[component]
pub(crate) fn MailWindow(
    session: Signal<Session>,
    accounts: Vec<serabot_desktop::MailAccountStatus>,
    on_close: EventHandler<()>,
) -> Element {
    use serabot_desktop::MailMessageInfo;
    let mut account = use_signal(|| Option::<String>::None);
    let mut folders = use_signal(Vec::<String>::new);
    let mut folder = use_signal(|| "INBOX".to_owned());
    let mut messages = use_signal(Vec::<MailMessageInfo>::new);
    let mut open_msg = use_signal(|| Option::<MailMessageInfo>::None);
    let mut loading = use_signal(|| true);
    let mut error = use_signal(|| Option::<String>::None);

    let reload = move |acct: Option<String>, fold: String| {
        loading.set(true);
        error.set(None);
        open_msg.set(None);
        spawn(async move {
            if let Ok(list) =
                run_mail_job(session, move |c| c.mail_folders(acct.clone().as_deref())).await
            {
                folders.set(list);
            }
            let res = run_mail_job(session, move |c| {
                c.mail_messages(acct.as_deref(), &fold, 30)
            })
            .await;
            loading.set(false);
            match res {
                Ok(list) => messages.set(list),
                Err(msg) => error.set(Some(msg)),
            }
        });
    };
    use_hook(move || reload(account(), folder()));

    let open_message = move |uid: u32| {
        let acct = account();
        let fold = folder();
        error.set(None);
        spawn(async move {
            match run_mail_job(session, move |c| {
                c.mail_message(acct.as_deref(), &fold, uid)
            })
            .await
            {
                Ok(msg) => open_msg.set(Some(msg)),
                Err(msg) => error.set(Some(msg)),
            }
        });
    };

    rsx! {
        div { class: "mp-sheet-scrim", onclick: move |_| on_close.call(()),
        }
        aside {
            class: "mp-sheet mail-window",
            role: "dialog",
            "aria-modal": "true",
            "aria-label": "Mail",
            div { class: "mp-sheet-head",
                h2 { class: "mp-sheet-title", "Mail" }
                button {
                    class: "icon-btn",
                    r#type: "button",
                    title: "Close",
                    "aria-label": "Close",
                    onclick: move |_| on_close.call(()),
                    Icon { name: "x", size: 16 }
                }
            }
            div { class: "mp-sheet-body mail-window-body",
                div { class: "row mail-window-controls",
                    select {
                        class: "input",
                        "aria-label": "Mailbox account",
                        value: "{account().clone().unwrap_or_default()}",
                        onchange: move |e| {
                            let v = e.value();
                            let a = if v.is_empty() { None } else { Some(v) };
                            account.set(a.clone());
                            reload(a, folder());
                        },
                        option { value: "", "default" }
                        for a in accounts.iter() {
                            option {
                                key: "{a.account}",
                                value: "{a.account}",
                                "{a.account} ({a.username.clone().unwrap_or_default()})"
                            }
                        }
                    }
                    select {
                        class: "input",
                        "aria-label": "Folder",
                        value: "{folder}",
                        onchange: move |e| {
                            let f = e.value();
                            folder.set(f.clone());
                            reload(account(), f);
                        },
                        option { value: "INBOX", "INBOX" }
                        for f in folders().iter().filter(|f| f.as_str() != "INBOX") {
                            option { key: "{f}", value: "{f}", "{f}" }
                        }
                    }
                    button {
                        class: "btn ghost sm",
                        r#type: "button",
                        onclick: move |_| reload(account(), folder()),
                        Icon { name: "refresh", size: 14 }
                        "Refresh"
                    }
                }
                if let Some(msg) = error() {
                    div { class: "banner err", role: "alert", span { class: "grow", "{msg}" } }
                }
                div { class: "mail-window-cols",
                    div { class: "mail-list", role: "list",
                        if loading() {
                            p { class: "tiny muted", "Loading…" }
                        } else if messages().is_empty() {
                            p { class: "tiny muted", "No messages." }
                        }
                        for m in messages() {
                            {
                                let uid = m.uid;
                                let selected = open_msg().is_some_and(|o| o.uid == uid);
                                rsx! {
                                    div {
                                        key: "{uid}",
                                        class: if selected { "mail-list-item selected" } else { "mail-list-item" },
                                        role: "listitem",
                                        onclick: move |_| open_message(uid),
                                        div { class: "mail-list-subject", "{m.subject}" }
                                        div { class: "tiny muted", "{m.from} · {m.date} {m.flags.join(\" \")}" }
                                    }
                                }
                            }
                        }
                    }
                    div { class: "mail-read",
                        if let Some(m) = open_msg() {
                            h3 { class: "mail-read-subject", "{m.subject}" }
                            p { class: "tiny muted", "From: {m.from} · {m.date} · {m.flags.join(\" \")}" }
                            if !m.attachments.is_empty() {
                                p { class: "tiny",
                                    "Attachments: "
                                    for a in m.attachments.iter() {
                                        span { class: "mono", "{a.name} ({a.size} B) " }
                                    }
                                }
                            }
                            div { class: "mail-body", "{m.body}" }
                        } else {
                            p { class: "tiny muted", "Select a message to read it." }
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
    use std::collections::BTreeMap;

    #[test]
    fn authorization_urls_are_fixed_https_endpoints() {
        let url = connector_auth_url("gmail", None).expect("gmail has an authorization page");
        assert!(url.starts_with("https://accounts.google.com/"), "{url}");
        assert!(
            connector_auth_url("notion", None)
                .expect("notion has an authorization page")
                .starts_with("https://api.notion.com/"),
        );
        assert!(
            connector_auth_url("outlook", None)
                .expect("outlook has an authorization page")
                .starts_with("https://login.microsoftonline.com/"),
        );
        assert!(connector_auth_url("unknown", None).is_none());
    }

    #[test]
    fn a_redirect_uri_is_appended_only_when_it_is_a_plain_address() {
        let url = connector_auth_url("gmail", Some("http://127.0.0.1:8765/callback"))
            .expect("gmail has an authorization page");
        assert!(url.contains("redirect_uri="), "{url}");
        assert!(url.starts_with("https://"), "{url}");
        assert!(connector_auth_url("gmail", Some("javascript:alert(1)")).is_none());
        assert!(connector_auth_url("gmail", Some("https://ok.example/cb&calc")).is_none());
        assert!(connector_auth_url("gmail", Some("https://ok.example/cb?state=1")).is_none());
        assert!(connector_auth_url("gmail", Some("https://ok.example/\"")).is_none());
        assert!(connector_auth_url("gmail", Some("   ")).is_some());
    }

    fn connector(id: &str) -> ConnectorStatus {
        ConnectorStatus {
            id: id.into(),
            kind: "api_key".into(),
            ..Default::default()
        }
    }

    #[test]
    fn each_connector_shows_one_action_that_fits_its_state() {
        let onedrive = connector("onedrive");
        assert_eq!(
            connector_action(&onedrive, false),
            Some(ConnectorAction::ConnectOneDrive)
        );
        assert_eq!(
            connector_action(&onedrive, true),
            Some(ConnectorAction::DisconnectOneDrive)
        );
        let mut notion = connector("notion");
        notion.secrets = BTreeMap::from([(
            "api_key".to_owned(),
            serabot_desktop::SecretStatus {
                name: Some("notion_key".into()),
                configured: false,
            },
        )]);
        assert_eq!(
            connector_action(&notion, false),
            Some(ConnectorAction::AddKey)
        );
        assert_eq!(
            connector_action(&notion, true),
            Some(ConnectorAction::ReplaceKey)
        );
        let gmail = connector("gmail");
        assert!(matches!(
            connector_action(&gmail, false),
            Some(ConnectorAction::Authorize(_))
        ));
        assert_eq!(connector_action(&connector("unknown"), false), None);
        assert_eq!(secret_label("api_key"), "API key");
        assert_eq!(secret_label("refresh_token"), "Refresh token");
        assert_eq!(connector_title("onedrive"), "OneDrive");
    }

    #[test]
    fn a_sign_in_connector_always_leads_with_sign_in_and_has_no_key_field() {
        let mut voice = connector("elevenlabs");
        voice.oauth = Some(serabot_desktop::ConnectorOauth {
            client_id: "https://example.example/client.json".into(),
            authorize_url: None,
            cimd_path: None,
            callback_uri: Some("http://127.0.0.1:1/cb".into()),
            scope: None,
        });
        assert_eq!(
            connector_action(&voice, false),
            Some(ConnectorAction::SignIn)
        );
        assert_eq!(
            connector_action(&voice, true),
            Some(ConnectorAction::SignIn)
        );
        // The account sign-in never takes a pasted key; that is its own connector.
        assert!(editable_roles(&voice).is_empty());
        let key = connector("elevenlabs_api_key");
        assert!(!supports_sign_in(&key));
    }

    #[test]
    fn per_user_cards_never_fall_back_to_a_sign_in_page() {
        let mut outlook = connector("outlook");
        outlook.personal = Some(serabot_desktop::PersonalStatus {
            state: "not_set_up".into(),
            hint: Some("Admin: Microsoft sign-in is not configured.".into()),
            ..Default::default()
        });
        let state = connector_card_state(&outlook, None).expect("per-user card");
        assert!(matches!(state, CardState::NotSetUp { .. }));
        assert_eq!(state.pill(), "Not set up yet");
    }
}
