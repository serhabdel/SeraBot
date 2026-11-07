//! The Connectors screen.
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
use dioxus::prelude::*;
use serabot_desktop::ConnectorStatus;
use serabot_desktop::connector_card::{
    CardState, DEFAULT_ADMIN_HINT, GOOGLE_CONSOLE_URL, GOOGLE_SETUP_STEPS, NOTION_SHARE_HINT,
    connector_card_state, google_is_set_up, google_redirect_uri, google_setup_offered, token_help,
};
use serabot_desktop::session::{Session, humanize_label};

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
fn connector_action(
    connector: &ConnectorStatus,
    connected: bool,
    auth_mode: &str,
) -> Option<ConnectorAction> {
    if connector.id == "onedrive" {
        return Some(if connected {
            ConnectorAction::DisconnectOneDrive
        } else {
            ConnectorAction::ConnectOneDrive
        });
    }
    if connector.oauth.is_some()
        && auth_mode == "oauth"
        && connector.auth_modes.iter().any(|mode| mode == "oauth")
    {
        return Some(ConnectorAction::SignIn);
    }
    // A pasted API key beats a sign-in page: those pages only help you find one.
    let takes_key = connector.secrets.keys().any(|role| role == "api_key");
    if !takes_key
        && let Some(url) = connector_auth_url(&connector.id, connector.redirect_uri.as_deref())
    {
        return Some(ConnectorAction::Authorize(url));
    }
    if !connector.secrets.is_empty() {
        return Some(if connected {
            ConnectorAction::ReplaceKey
        } else {
            ConnectorAction::AddKey
        });
    }
    None
}

fn connector_title(id: &str) -> String {
    match id {
        "onedrive" => "OneDrive",
        "google_drive" => "Google Drive",
        "google_sheets" => "Google Sheets",
        "google_calendar" => "Google Calendar",
        "gmail" => "Gmail",
        "outlook" => "Outlook",
        "outlook_calendar" => "Outlook Calendar",
        "notion" => "Notion",
        "github" => "GitHub",
        "todoist" => "Todoist",
        "slack" => "Slack",
        "agentmail" => "AgentMail",
        "tavily" => "Tavily Search",
        "firecrawl" => "Firecrawl",
        "elevenlabs" => "ElevenLabs",
        other => return humanize_label(other),
    }
    .to_owned()
}

fn connector_blurb(id: &str) -> &'static str {
    match id {
        "onedrive" => "Save and read files in your OneDrive.",
        "google_drive" => "Files in your Google Drive.",
        "google_sheets" => "Your Google spreadsheets.",
        "google_calendar" => "Your Google calendar.",
        "gmail" => "Read and send email.",
        "outlook" => "Read and send email.",
        "outlook_calendar" => "Your Outlook calendar.",
        "notion" => "Your notes and pages.",
        "github" => {
            "Search repositories, read issues and files, and file issues with your approval."
        }
        "todoist" => "Your to-do list.",
        "slack" => "Read channels and post with your approval.",
        "agentmail" => "A mailbox for your bots.",
        "tavily" | "firecrawl" => "Let bots search and read the web.",
        "elevenlabs" => "Voices and sound.",
        _ => "",
    }
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
pub(crate) fn ConnectorsView(session: Signal<Session>) -> Element {
    let connectors = session.read().state.connectors.clone();
    let loaded = session.read().state.connectors_loaded;
    let is_admin = session.read().is_admin();
    let google_offered = google_setup_offered(&connectors, is_admin);
    let google_ready = google_is_set_up(&connectors);
    let google_redirect = google_redirect_uri(&connectors);
    rsx! {
        div { class: "pane",
            div { class: "pane-head",
                div {
                    h1 { class: "pane-title", "Connectors" }
                    p { class: "muted", "Let your bots use your accounts and tools." }
                }
                button {
                    class: "btn ghost",
                    title: "Check for changes",
                    onclick: move |_| session.write().refresh_connectors(),
                    Icon { name: "refresh", size: 15 }
                    "Refresh"
                }
            }
            div { class: "pane-body",
                if !loaded {
                    div { class: "card",
                        div { class: "skeleton title" }
                        div { class: "skeleton block" }
                    }
                }
                if loaded && connectors.is_empty() {
                    div { class: "empty",
                        div { class: "empty-icon", Icon { name: "connectors", size: 20 } }
                        h2 { class: "empty-title", "No connectors available" }
                        p { class: "empty-text", "The server did not list any connectors. Check the connection and try again." }
                        button {
                            class: "btn primary",
                            onclick: move |_| session.write().refresh_connectors(),
                            "Refresh"
                        }
                    }
                }
                for (suite_title, suite_ids) in [
                    ("Microsoft", &["onedrive", "outlook", "outlook_calendar"][..]),
                    ("Google", &["google_drive", "google_sheets", "google_calendar", "gmail"][..]),
                    ("Notes and email", &["notion", "agentmail"][..]),
                    ("Code and tasks", &["github", "todoist"][..]),
                    ("Messages", &["slack"][..]),
                    ("Web search", &["tavily", "firecrawl"][..]),
                    ("Voice", &["elevenlabs"][..]),
                    ("Apps and tools", &["composio"][..]),
                ] {
                    {
                        let suite_connectors: Vec<_> = suite_ids
                            .iter()
                            .filter_map(|id| connectors.iter().find(|c| c.id == *id))
                            .cloned()
                            .collect();
                        let show_google_setup = suite_title == "Google" && google_offered;
                        let google_redirect = google_redirect.clone();
                        if !suite_connectors.is_empty() {
                            rsx! {
                                h2 { class: "connector-suite-title", "{suite_title}" }
                                if show_google_setup {
                                    GoogleSetupCard { session, configured: google_ready, redirect: google_redirect }
                                }
                                div { class: "card-grid",
                                    for connector in suite_connectors {
                                        ConnectorCard { key: "{connector.id}", session, connector }
                                    }
                                }
                            }
                        } else {
                            rsx! {}
                        }
                    }
                }
            }
        }
    }
}

/// One connector. Per-user products draw their own account's state; the rest
/// keep the server-wide card.
#[component]
fn ConnectorCard(session: Signal<Session>, connector: ConnectorStatus) -> Element {
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
    let pill_class = if state.is_connected() {
        "status-pill connected"
    } else {
        "status-pill disconnected"
    };
    let redirect = connector
        .personal
        .as_ref()
        .and_then(|personal| personal.redirect_uri.clone());
    let is_notion = id == "notion";
    let help = token_help(&id);
    let token_label = help.map_or("Token", |help| help.label);
    let token_hint = help.map_or("", |help| help.hint);
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
                span { class: pill_class, "{state.pill()}" }
            }
            {match state.clone() {
                CardState::NotSetUp { hint } => rsx! {
                    p { class: "tiny", role: "status", "Not set up yet. {hint}" }
                },
                CardState::Connect { oauth, token: takes_token } => rsx! {
                    if takes_token {
                        div { class: "field",
                            label { class: "field-label", "{token_label}" }
                            input {
                                class: "input",
                                "type": "password",
                                autocomplete: "off",
                                value: "{token}",
                                placeholder: "Paste it here",
                                oninput: move |event| token.set(event.value()),
                            }
                        }
                        if !token_hint.is_empty() {
                            p { class: "tiny", "{token_hint}" }
                        }
                    }
                    if waiting {
                        p { class: "tiny", role: "status",
                            "Finish signing in in your browser, then press Refresh."
                        }
                    }
                    div { class: "card-actions",
                        if takes_token {
                            button {
                                class: "btn primary",
                                disabled: busy || token().trim().is_empty(),
                                onclick: {
                                    let id = id.clone();
                                    move |_| {
                                        session.write().panel.connect_connector_token(&id, &token());
                                        token.set(String::new());
                                    }
                                },
                                if busy { "Connecting…" } else { "Connect" }
                            }
                            if let Some(help) = help {
                                button {
                                    class: "btn ghost",
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
                    div { class: "card-actions",
                        button {
                            class: "btn ghost",
                            disabled: busy,
                            onclick: {
                                let id = id.clone();
                                move |_| session.write().panel.disconnect_connector(&id)
                            },
                            if busy { "Working…" } else { "Disconnect" }
                        }
                    }
                },
                CardState::Error { message } => rsx! {
                    p { class: "banner err", role: "alert", "{message}" }
                    div { class: "card-actions",
                        button {
                            class: "btn ghost",
                            onclick: {
                                let id = id.clone();
                                move |_| {
                                    session.write().panel.clear_connector_error(&id);
                                    session.write().refresh_connectors();
                                }
                            },
                            "Try again"
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
                            code { class: "tiny", "{uri}" }
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
fn GoogleSetupCard(
    session: Signal<Session>,
    configured: bool,
    redirect: Option<String>,
) -> Element {
    let mut session = session;
    let mut client_id = use_signal(String::new);
    let mut client_secret = use_signal(String::new);
    let (busy, error, saved) = {
        let state = session.read();
        let flow = &state.panel.connectors;
        (
            flow.google_busy,
            flow.google_error.clone(),
            flow.google_saved,
        )
    };
    let pill_class = if configured {
        "status-pill connected"
    } else {
        "status-pill disconnected"
    };
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
                span { class: pill_class, if configured { "Set up" } else { "Not set up yet" } }
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
                        code { class: "tiny", "{uri}" }
                    }
                    div { class: "card-actions",
                        button {
                            class: "btn ghost sm",
                            onclick: move |_| session.write().open_in_browser(GOOGLE_CONSOLE_URL.to_owned()),
                            Icon { name: "external", size: 14 }
                            "Open Google Cloud Console"
                        }
                    }
                    div { class: "form-grid",
                        div { class: "field",
                            label { class: "field-label", "Client ID" }
                            input {
                                class: "input",
                                "type": "password",
                                autocomplete: "off",
                                value: "{client_id}",
                                placeholder: "Paste it here",
                                oninput: move |event| client_id.set(event.value()),
                            }
                        }
                        div { class: "field",
                            label { class: "field-label", "Client secret" }
                            input {
                                class: "input",
                                "type": "password",
                                autocomplete: "off",
                                value: "{client_secret}",
                                placeholder: "Paste it here",
                                oninput: move |event| client_secret.set(event.value()),
                            }
                        }
                    }
                    if let Some(message) = error {
                        p { class: "banner err", role: "alert", "{message}" }
                    }
                    if saved {
                        p { class: "banner ok", role: "status", "Saved. People can connect Google now." }
                    }
                    div { class: "card-actions",
                        button {
                            class: "btn primary",
                            disabled: !can_save,
                            onclick: move |_| {
                                session.write().panel.save_google_setup(&client_id(), &client_secret());
                                client_id.set(String::new());
                                client_secret.set(String::new());
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
    let can_save = session.read().can_admin();
    let is_admin = session.read().is_admin();
    let id = connector.id.clone();
    let connected = connector
        .connected
        .unwrap_or_else(|| connector.secrets.values().any(|s| s.configured));
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
    let auth_mode = session
        .read()
        .connector_drafts
        .get(&id)
        .and_then(|draft| draft.auth.clone())
        .or_else(|| connector.auth.clone())
        .unwrap_or_default();
    let test_result = session.read().state.connector_tests.get(&id).cloned();
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
    let action = connector_action(&connector, connected, &auth_mode);
    let key_inputs = matches!(
        action,
        Some(ConnectorAction::AddKey | ConnectorAction::ReplaceKey)
    ) && is_admin;
    let needs_admin = !is_admin
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
                span {
                    class: if connected { "status-pill connected" } else { "status-pill disconnected" },
                    if connected { "Connected" } else { "Not connected" }
                }
            }
            if key_inputs {
                div { class: "form-grid",
                    for (role, secret) in &connector.secrets {
                        {
                            let role = role.clone();
                            let label = secret_label(&role);
                            let value = values.get(&role).cloned().unwrap_or_default();
                            rsx! {
                                div { class: "field", key: "{role}",
                                    label { class: "field-label", "{label}" }
                                    input {
                                        class: "input",
                                        "type": "password",
                                        value: "{value}",
                                        autocomplete: "off",
                                        placeholder: if secret.configured { "Paste a new one to replace it" } else { "Paste it here" },
                                        oninput: {
                                            let id = id.clone();
                                            let role = role.clone();
                                            move |event| session.write().set_connector_value(&id, &role, event.value())
                                        },
                                    }
                                }
                            }
                        }
                    }
                }
            }
            if od_waiting && id == "onedrive" && !connected {
                p { class: "tiny", role: "status",
                    "Finish signing in with Microsoft in your browser, then press Refresh."
                }
            }
            if id == "onedrive" {
                if let Some(message) = od_error {
                    p { class: "banner err", role: "alert", "{message}" }
                }
            }
            if needs_admin && !connected {
                p { class: "tiny", "Your admin sets this one up." }
            }
            if let Some(result) = test_result {
                p { class: if result.ok { "banner ok" } else { "banner err" }, "{result.message}" }
            }
            div { class: "card-actions",
                {match action.clone() {
                    Some(ConnectorAction::ConnectOneDrive) => rsx! {
                        button {
                            class: "btn primary",
                            disabled: od_busy,
                            onclick: move |_| session.write().panel.connect_onedrive(),
                            if od_busy { "OpeningÔÇª" } else { "Connect OneDrive" }
                        }
                    },
                    Some(ConnectorAction::DisconnectOneDrive) => rsx! {
                        button {
                            class: "btn ghost",
                            disabled: od_busy,
                            onclick: move |_| session.write().panel.disconnect_onedrive(),
                            "Disconnect"
                        }
                    },
                    Some(ConnectorAction::SignIn) if is_admin => {
                        let callback_uri = connector
                            .oauth
                            .as_ref()
                            .and_then(|o| o.callback_uri.clone())
                            .unwrap_or_default();
                        let id = id.clone();
                        rsx! {
                            button {
                                class: if connected { "btn ghost" } else { "btn primary" },
                                title: "Sign in with your account in your browser",
                                disabled: signing_in || !can_save,
                                onclick: move |_| session.write().sign_in_connector(id.clone(), callback_uri.clone()),
                                if signing_in { "Signing inÔÇª" } else if connected { "Sign in again" } else { "Sign in" }
                            }
                        }
                    },
                    Some(ConnectorAction::Authorize(url)) if is_admin => rsx! {
                        button {
                            class: if connected { "btn ghost" } else { "btn primary" },
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
                                disabled: !can_save || !has_values,
                                title: if has_values { "Save the key" } else { "Paste the key first" },
                                onclick: move |_| session.write().add_connector_key(id.clone()),
                                if connected { "Replace key" } else { "Add key" }
                            }
                            if let Some(url) = key_page {
                                button {
                                    class: "btn ghost",
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
            if is_admin {
                details { class: "advanced",
                    summary { "Advanced" }
                    div { class: "advanced-body",
                        div { class: "chips",
                            span { class: "chip mono", "{connector.kind}" }
                            span { class: if enabled { "chip ok" } else { "chip muted" },
                                if enabled { "enabled" } else { "disabled" }
                            }
                            if let Some(oauth) = connector.oauth.as_ref() {
                                span { class: "chip mono accent", "client_id: {oauth.client_id}" }
                            }
                            for (role, secret) in &connector.secrets {
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
                            code { class: "tiny", "{uri}" }
                        }
                        if connector.auth_modes.len() > 1 {
                            div { class: "field",
                                label { class: "field-label", "How it signs in" }
                                select {
                                    class: "input",
                                    value: "{auth_mode}",
                                    onchange: {
                                        let id = id.clone();
                                        move |event| session.write().set_connector_auth(&id, event.value())
                                    },
                                    for mode in &connector.auth_modes {
                                        option { value: "{mode}", "{mode}" }
                                    }
                                }
                            }
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
                                    onclick: {
                                        let id = id.clone();
                                        move |_| session.write().toggle_connector(&id)
                                    },
                                    if enabled { "Turn off" } else { "Turn on" }
                                }
                                button {
                                    class: "btn ghost sm",
                                    disabled: !can_save,
                                    onclick: {
                                        let id = id.clone();
                                        move |_| session.write().save_connector(id.clone())
                                    },
                                    "Save settings"
                                }
                                button {
                                    class: "btn ghost sm",
                                    title: "Run a read-only check of the saved credentials",
                                    disabled: !can_save || testing,
                                    onclick: {
                                        let id = id.clone();
                                        move |_| session.write().test_connector(id.clone())
                                    },
                                    if testing { "TestingÔÇª" } else { "Test" }
                                }
                            }
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
            connector_action(&onedrive, false, ""),
            Some(ConnectorAction::ConnectOneDrive)
        );
        assert_eq!(
            connector_action(&onedrive, true, ""),
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
            connector_action(&notion, false, ""),
            Some(ConnectorAction::AddKey)
        );
        assert_eq!(
            connector_action(&notion, true, ""),
            Some(ConnectorAction::ReplaceKey)
        );
        let gmail = connector("gmail");
        assert!(matches!(
            connector_action(&gmail, false, ""),
            Some(ConnectorAction::Authorize(_))
        ));
        assert_eq!(connector_action(&connector("unknown"), false, ""), None);
        assert_eq!(secret_label("api_key"), "API key");
        assert_eq!(secret_label("refresh_token"), "Refresh token");
        assert_eq!(connector_title("onedrive"), "OneDrive");
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
