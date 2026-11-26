//! The Integrations screen (v0.38): connectors, Composio and marketplace
//! plugins in one place.
//!
//! Tabs: Installed (what is set up, then what is still open to set up),
//! Marketplace (the plugin catalogue) and Composio (its apps, with the key
//! first when there is none). One search box serves every tab. Every card
//! looks the same, and opens the same detail sheet. The sheet's body is the
//! existing connector, Composio or plugin screen, so their behaviour is kept.
//!
//! Secrets: the cards and sheets only show what the server reports. A typed
//! key is handled by the card that collects it, and is never displayed again.

use crate::ui::{BrandLogo, Icon};
use crate::ui_composio::ComposioCard;
use crate::ui_connectors::{ConnectorCard, GoogleSetupCard, SocialAccountsCard};
use crate::ui_marketplace::{MarketplaceTab, PluginSheet};
use crate::ui_mcp_accounts::GithubCard;
use crate::ui_whatsapp_link::WhatsappLinkCard;
use crate::ui_widgets::StatusChip;
use dioxus::prelude::*;
use serabot_desktop::connector_card::{
    GOOGLE_IDS, google_is_set_up, google_redirect_uri, google_setup_offered,
};
use serabot_desktop::integrations::{self, COMPOSIO_ID, Entry, Source, Tab, connector_title};
use serabot_desktop::marketplace::PluginStatus;
use serabot_desktop::session::Session;

use serabot_desktop::theme::Tone;
use serabot_desktop::worker::Credentials;
use serabot_desktop::{ApiClient, MailStatus};
use std::sync::Arc;

const MAIL_CONNECTOR_ID: &str = "mail";

async fn call_mail<T, F>(session: Signal<Session>, job: F) -> Result<T, String>
where
    T: Send + 'static,
    F: FnOnce(Arc<Credentials>) -> Result<T, String> + Send + 'static,
{
    let creds = session.read().credentials.clone();
    let Some(creds) = creds else {
        return Err("Not signed in.".into());
    };
    let (tx, rx) = tokio::sync::oneshot::channel();
    if let Err(err) = std::thread::Builder::new()
        .name("serabot-mail-status".into())
        .spawn(move || {
            let _ = tx.send(job(creds));
        })
    {
        return Err(format!("Could not start request thread: {err}"));
    }
    rx.await
        .map_err(|_| "Request thread exited unexpectedly.".to_owned())?
}

fn fetch_mail_status(creds: &Credentials) -> Result<MailStatus, String> {
    ApiClient::new(creds.endpoint.clone(), creds.token.clone())
        .and_then(|c| c.mail_status())
        .map_err(|e| e.to_string())
}

fn mail_entry(status: Option<&MailStatus>) -> Entry {
    let connected = status.is_some_and(|s| s.connected || !s.accounts.is_empty());
    let (pill, tone, meta) =
        if let Some(st) = status.filter(|s| s.connected || !s.accounts.is_empty()) {
            let default_user = st
                .username
                .as_deref()
                .map(str::trim)
                .filter(|u| !u.is_empty())
                .map(str::to_owned);
            let meta = if let Some(user) = default_user {
                if st.accounts.len() > 1 {
                    format!("{user} (+{} more)", st.accounts.len() - 1)
                } else {
                    user
                }
            } else if st.accounts.len() > 1 {
                format!("{} mailboxes connected", st.accounts.len())
            } else if let Some(first) = st.accounts.first() {
                let label = first.account.trim();
                let user = first
                    .username
                    .as_deref()
                    .map(str::trim)
                    .filter(|u| !u.is_empty());
                match (label.is_empty(), user) {
                    (false, Some(u)) => format!("{label} ({u})"),
                    (false, None) => label.to_owned(),
                    (true, Some(u)) => u.to_owned(),
                    (true, None) => "1 mailbox connected".to_owned(),
                }
            } else {
                "Connected".to_owned()
            };
            ("Connected".to_owned(), Tone::Ok, meta)
        } else {
            (
                "Not connected".to_owned(),
                Tone::Muted,
                "Your mailbox".to_owned(),
            )
        };
    Entry {
        source: Source::Connector(MAIL_CONNECTOR_ID.to_owned()),
        brand_key: "agentmail".to_owned(),
        name: "Mail (SMTP / IMAP)".to_owned(),
        suite_label: "Mail".to_owned(),
        blurb: "Read and send email from any SMTP/IMAP mailbox (Hostinger, custom domain, etc.)."
            .to_owned(),
        pill,
        tone,
        set_up: connected,
        meta,
    }
}

/// The whole screen: header, tabs and search, the active tab, and the sheets.
#[component]
pub fn IntegrationsView(session: Signal<Session>) -> Element {
    let mut session = session;
    let mut mail_status = use_signal(|| Option::<MailStatus>::None);
    let load_mail = move || {
        spawn(async move {
            if let Ok(st) = call_mail(session, |creds| fetch_mail_status(&creds)).await {
                mail_status.set(Some(st));
            }
        });
    };
    // The GitHub card's badge follows the native sign-in, so read it once.
    use_hook(move || {
        session.write().load_github();
        load_mail();
    });
    let mut tab = use_signal(Tab::default);
    let mut query = use_signal(String::new);
    // The sidebar's Bots link asks for its tab; take the request once.
    let requested = session.read().integrations_tab_request;
    if let Some(requested) = requested {
        session.write().integrations_tab_request = None;
        tab.set(requested);
    }
    let (open_connector, has_plugin_sheet) = {
        let state = session.read();
        (
            state.integration_open.clone(),
            state.marketplace_open.is_some(),
        )
    };
    let show_search = tab() != Tab::Composio;
    let query_empty = query().trim().is_empty();
    rsx! {
        div { class: "pane mp-page integrations",
            div { class: "pane-head",
                div {
                    h1 { class: "pane-title", "Integrations" }
                    p { class: "muted", "Accounts, tools and apps your bots can use." }
                }
                button {
                    class: "btn ghost sm",
                    r#type: "button",
                    title: "Refresh this tab",
                    onclick: move |_| match tab() {
                        Tab::Installed => {
                            let mut s = session.write();
                            s.refresh_connectors();
                            s.load_github();
                            drop(s);
                            load_mail();
                        }
                        Tab::Marketplace => session.write().refresh_marketplace(),
                        Tab::Bots => session.write().open_meet_team(),
                        Tab::Composio => {
                            let mut s = session.write();
                            s.refresh_connectors();
                            s.panel.composio.load("");
                        }
                    },
                    Icon { name: "refresh", size: 14 }
                    "Refresh"
                }
            }
            div { class: "pane-body",
                div { class: "mp-toolbar integrations-bar",
                    div { class: "seg", role: "tablist", "aria-label": "Integrations",
                        for item in Tab::ALL {
                            button {
                                key: "{item.label()}",
                                r#type: "button",
                                role: "tab",
                                class: if tab() == item { "seg-btn active" } else { "seg-btn" },
                                "aria-selected": if tab() == item { "true" } else { "false" },
                                onclick: move |_| tab.set(item),
                                "{item.label()}"
                            }
                        }
                    }
                    if show_search {
                        div { class: "search-wrap",
                            input {
                                class: "input mp-search",
                                r#type: "search",
                                placeholder: if tab() == Tab::Bots { "Search bots" } else if tab() == Tab::Marketplace { "Search plugins" } else { "Search integrations" },
                                "aria-label": if tab() == Tab::Marketplace { "Search plugins" } else { "Search integrations" },
                                value: "{query}",
                                oninput: move |event| query.set(event.value()),
                                onkeydown: move |event: KeyboardEvent| {
                                    if event.key() == Key::Escape && !query.read().is_empty() {
                                        event.prevent_default();
                                        query.set(String::new());
                                    }
                                },
                            }
                            if !query_empty {
                                button {
                                    class: "btn ghost sm",
                                    r#type: "button",
                                    onclick: move |_| query.set(String::new()),
                                    "Clear"
                                }
                            }
                        }
                    }
                }
                {match tab() {
                    Tab::Installed => rsx! { InstalledTab { session, query, tab, mail_status } },
                    Tab::Marketplace => rsx! { MarketplaceTab { session, query } },
                    Tab::Bots => rsx! { BotsTab { session, query } },
                    Tab::Composio => rsx! { ComposioTab { session } },
                }}
            }
            if let Some(id) = open_connector {
                ConnectorSheet { key: "{id}", session, id, on_mail_changed: move |next| mail_status.set(Some(next)) }
            }
            if has_plugin_sheet {
                PluginSheet { session }
            }
        }
    }
}

/// One card, whatever its source. Connector cards open the connector sheet,
/// plugin cards open the plugin page. Both use the same markup and pills.
#[component]
pub fn IntegrationCard(session: Signal<Session>, entry: Entry) -> Element {
    let mut session = session;
    let label = format!("{}, {}", entry.name, entry.pill);
    let source = entry.source.clone();
    let needs_sign_in =
        matches!(source, Source::Plugin(_)) && entry.pill == PluginStatus::NeedsSignIn.label();
    let cta = if needs_sign_in {
        "Sign in \u{2192}"
    } else if entry.set_up {
        "Manage \u{2192}"
    } else if matches!(source, Source::Connector(_)) {
        "Connect \u{2192}"
    } else {
        "Details \u{2192}"
    };
    rsx! {
        button {
            class: "mp-card integration-card",
            r#type: "button",
            "aria-label": "{label}",
            onclick: move |_| match &source {
                Source::Connector(id) => session.write().open_connector_sheet(id.clone()),
                Source::Plugin(slug) => session.write().open_plugin(slug.clone()),
            },
            div { class: "integration-card-head",
                BrandLogo { brand: entry.brand_key.clone(), size: 38 }
                div { class: "integration-card-identity",
                    span { class: "integration-card-name", title: "{entry.name}", "{entry.name}" }
                    if !entry.suite_label.is_empty() {
                        span { class: "integration-card-suite", "{entry.suite_label}" }
                    }
                }
                div { class: "integration-status",
                    StatusChip { tone: entry.tone, text: entry.pill.clone() }
                }
            }
            if !entry.blurb.is_empty() {
                span { class: "integration-card-blurb", "{entry.blurb}" }
            }
            div { class: "integration-card-foot",
                span { class: "integration-card-account", "{entry.meta}" }
                span { class: "integration-card-cta", "{cta}" }
            }
        }
    }
}

/// A placeholder card with the real card's anatomy (logo, name, status,
/// description, footer), shown while the lists load. Hidden from assistive tech.
#[component]
pub fn SkeletonCard() -> Element {
    rsx! {
        div { class: "mp-card integration-card mp-skel", "aria-hidden": "true",
            div { class: "integration-card-head",
                div { class: "skeleton sk-logo" }
                div { class: "integration-card-identity",
                    div { class: "skeleton sk-line sk-w60" }
                    div { class: "skeleton sk-line sk-w40" }
                }
                div { class: "skeleton sk-chip" }
            }
            div { class: "skeleton sk-line sk-w90" }
            div { class: "skeleton sk-line sk-w70" }
            div { class: "integration-card-foot",
                div { class: "skeleton sk-line sk-w30" }
                div { class: "skeleton sk-line sk-w20" }
            }
        }
    }
}

/// A grid of placeholder cards under a polite "Loading" label.
#[component]
pub fn SkeletonGrid(label: String, count: usize) -> Element {
    rsx! {
        div { role: "status", "aria-busy": "true", "aria-label": "{label}",
            div { class: "mp-grid", "aria-hidden": "true",
                for index in 0..count {
                    SkeletonCard { key: "{index}" }
                }
            }
        }
    }
}

/// The Installed tab: what is set up, then the connectors still open to set up,
/// by suite. The search filters both.
#[component]
fn InstalledTab(
    session: Signal<Session>,
    query: Signal<String>,
    tab: Signal<Tab>,
    mail_status: Signal<Option<MailStatus>>,
) -> Element {
    let (
        connectors,
        plugins,
        connectors_loaded,
        marketplace_loaded,
        marketplace_unavailable,
        marketplace_error,
        state_error,
        section_load_error,
        errors,
        github,
    ) = {
        let state = session.read();
        (
            state.state.connectors.clone(),
            state.state.marketplace.clone(),
            state.state.connectors_loaded,
            state.state.marketplace_loaded,
            state.state.marketplace_unavailable,
            state.state.marketplace_error.clone(),
            state.state.error.clone(),
            state.section_load_error.clone(),
            state.panel.connectors.errors.clone(),
            state.mcp_ui.github.clone(),
        )
    };
    let mut tab = tab;
    let mut query = query;
    let needle = query();
    let lookup = |id: &str| errors.get(id).cloned();
    let mut view = integrations::installed(&connectors, &plugins, &lookup);
    if let Some(github) = &github {
        integrations::apply_github(&mut view, github);
    }
    integrations::add_whatsapp_personal(&mut view);
    let mail_card = mail_entry(mail_status().as_ref());
    if mail_card.set_up {
        view.set_up.push(mail_card);
    } else if let Some((_, cards)) = view
        .available
        .iter_mut()
        .find(|(suite, _)| *suite == "Notes and email")
    {
        cards.push(mail_card);
    } else {
        view.available.push(("Notes and email", vec![mail_card]));
    }
    let set_up: Vec<Entry> = view
        .set_up
        .into_iter()
        .filter(|entry| integrations::matches(entry, &needle))
        .collect();
    let available: Vec<(&'static str, Vec<Entry>)> = view
        .available
        .into_iter()
        .map(|(suite, cards)| {
            let cards: Vec<Entry> = cards
                .into_iter()
                .filter(|entry| integrations::matches(entry, &needle))
                .collect();
            (suite, cards)
        })
        .filter(|(_, cards)| !cards.is_empty())
        .collect();
    let nothing = set_up.is_empty() && available.is_empty();
    let marketplace_pending =
        !marketplace_loaded && !marketplace_unavailable && marketplace_error.is_none();
    let loading =
        nothing && (!connectors_loaded || marketplace_pending) && section_load_error.is_none();
    let needle_lc = needle.trim().to_lowercase();
    let show_social = needle_lc.is_empty()
        || needle_lc == "x"
        || [
            "social",
            "browser",
            "linkedin",
            "instagram",
            "twitter",
            "x.com",
            "facebook",
            "youtube",
            "marketer",
        ]
        .iter()
        .any(|kw| kw.contains(&needle_lc) || needle_lc.contains(kw));
    let load_error_card = section_load_error
        .clone()
        .filter(|_| nothing && (!connectors_loaded || !marketplace_loaded));
    let inline_error = if load_error_card.is_some() {
        None
    } else {
        state_error.or(marketplace_error).or(section_load_error)
    };
    rsx! {
        if let Some(message) = inline_error {
            div { class: "banner err", role: "alert",
                span { class: "grow", "{message}" }
                button {
                    class: "btn ghost sm",
                    r#type: "button",
                    onclick: move |_| session.write().retry_section_load(),
                    "Retry"
                }
            }
        }
        if let Some(err) = load_error_card {
            div { class: "empty", role: "alert",
                div { class: "empty-icon", Icon { name: "alert", size: 20 } }
                h2 { class: "empty-title", "Could not load integrations" }
                p { class: "empty-text", "{err}" }
                button {
                    class: "btn ghost sm",
                    r#type: "button",
                    onclick: move |_| session.write().retry_section_load(),
                    "Retry"
                }
            }
        } else if loading {
            SkeletonGrid { label: "Loading integrations", count: 8 }
        } else if nothing && !show_social && needle.trim().is_empty() {
            div { class: "empty",
                div { class: "empty-icon", Icon { name: "connectors", size: 20 } }
                h2 { class: "empty-title", "Nothing set up yet" }
                p { class: "empty-text", "Connect an account, or turn on a plugin from the Marketplace." }
                button {
                    class: "btn primary",
                    r#type: "button",
                    onclick: move |_| tab.set(Tab::Marketplace),
                    "Browse Marketplace"
                }
            }
        } else if nothing && !show_social {
            div { class: "empty compact",
                h3 { class: "empty-title", "No integrations match that search" }
                p { class: "empty-text", "Try another name, category, or suite (like Microsoft or Google)." }
                button {
                    class: "btn sm",
                    r#type: "button",
                    onclick: move |_| query.set(String::new()),
                    "Clear search"
                }
            }
        } else {
            if !set_up.is_empty() {
                h2 { class: "connector-suite-title", "Set up" }
                div { class: "mp-grid", role: "list", "aria-label": "Set up",
                    for entry in set_up {
                        div { key: "{entry_key(&entry)}", role: "listitem",
                            IntegrationCard { session, entry }
                        }
                    }
                }
            }
            if show_social {
                h2 { class: "connector-suite-title", "Social" }
                SocialAccountsCard { session }
            }
            for (suite, cards) in available {
                h2 { key: "{suite}", class: "connector-suite-title", "{suite}" }
                div { class: "mp-grid", role: "list", "aria-label": "{suite}",
                    for entry in cards {
                        div { key: "{entry_key(&entry)}", role: "listitem",
                            IntegrationCard { session, entry }
                        }
                    }
                }
            }
        }
    }
}

/// A stable key for a card: its source, so a reorder does not remount it.
fn entry_key(entry: &Entry) -> String {
    match &entry.source {
        Source::Connector(id) => format!("connector:{id}"),
        Source::Plugin(slug) => format!("plugin:{slug}"),
    }
}

/// The Bots tab: every shipped bot with add, switch and remove. Reads the
/// catalog when it opens, so it is current.
#[component]
fn BotsTab(session: Signal<Session>, query: Signal<String>) -> Element {
    let mut session = session;
    use_hook(move || session.write().open_meet_team());
    let (installed, total) = {
        let state = session.read();
        (
            state
                .state
                .shipped_bots
                .iter()
                .filter(|bot| bot.installed)
                .count(),
            state.state.shipped_bots.len(),
        )
    };
    rsx! {
        div { class: "mp-bots",
            p { class: "hint", "{installed} of {total} bots are on your team. Add any of them; each one you add becomes your own, and you can remove it and add it back here any time." }
            crate::ui_bots::ShippedList { session, query: query(), overlay: None }
        }
    }
}

/// The Composio tab: its key form when there is none, then its apps.
#[component]
fn ComposioTab(session: Signal<Session>) -> Element {
    let (loaded, section_load_error, connector) = {
        let state = session.read();
        (
            state.state.connectors_loaded,
            state.section_load_error.clone(),
            state
                .state
                .connectors
                .iter()
                .find(|connector| connector.id == COMPOSIO_ID)
                .cloned(),
        )
    };
    match connector {
        Some(connector) => rsx! { ComposioCard { session, connector } },
        None if !loaded => {
            if let Some(err) = section_load_error {
                rsx! {
                    div { class: "empty compact", role: "alert",
                        div { class: "empty-icon", Icon { name: "alert", size: 20 } }
                        h3 { class: "empty-title", "Could not load Composio" }
                        p { class: "empty-text", "{err}" }
                        button {
                            class: "btn ghost sm",
                            r#type: "button",
                            onclick: move |_| session.write().retry_section_load(),
                            "Retry"
                        }
                    }
                }
            } else {
                rsx! {
                    SkeletonGrid { label: "Loading Composio", count: 1 }
                }
            }
        }
        None => rsx! {
            div { class: "empty compact", role: "status",
                h3 { class: "empty-title", "Composio is not listed on this server" }
                p { class: "empty-text", "Check with your server admin if Composio should be enabled." }
                button {
                    class: "btn ghost sm",
                    r#type: "button",
                    onclick: move |_| session.write().refresh_connectors(),
                    "Refresh"
                }
            }
        },
    }
}

fn close_connector_with_reset(mut session: Signal<Session>, id: &str, saved_enabled: Option<bool>) {
    let mut s = session.write();
    if let Some(saved) = saved_enabled
        && let Some(draft) = s.connector_drafts.get_mut(id)
    {
        draft.enabled = saved;
        draft.values.clear();
    }
    s.close_connector_sheet();
}

/// A connector's card in the detail sheet: the shared frame, then the
/// connector's own screen. A Google connector also shows the Google setup card
/// while an admin has yet to set it up.
#[component]
fn ConnectorSheet(
    session: Signal<Session>,
    id: String,
    on_mail_changed: EventHandler<MailStatus>,
) -> Element {
    let mut session = session;
    let (connector, connectors, is_admin) = {
        let state = session.read();
        (
            state.state.connectors.iter().find(|c| c.id == id).cloned(),
            state.state.connectors.clone(),
            state.is_admin(),
        )
    };
    let saved_enabled = connector.as_ref().map(|c| c.enabled);
    let title = if id == MAIL_CONNECTOR_ID {
        "Mail (SMTP / IMAP)".to_owned()
    } else {
        connector_title(&id)
    };
    let close_id = id.clone();
    let close_scrim_id = id.clone();
    let close_key_id = id.clone();
    rsx! {
        div {
            class: "mp-sheet-scrim",
            onclick: move |_| close_connector_with_reset(session, &close_scrim_id, saved_enabled),
        }
        aside {
            class: "mp-sheet",
            role: "dialog",
            "aria-modal": "true",
            "aria-label": "{title}",
            tabindex: "0",
            onkeydown: move |event: KeyboardEvent| {
                if event.key() == Key::Escape {
                    event.prevent_default();
                    close_connector_with_reset(session, &close_key_id, saved_enabled);
                }
            },
            div { class: "mp-sheet-head",
                h2 { class: "mp-sheet-title", "{title}" }
                div { class: "row",
                    button {
                        class: "btn ghost sm",
                        r#type: "button",
                        title: "Refresh connector status",
                        onclick: move |_| session.write().refresh_connectors(),
                        Icon { name: "refresh", size: 14 }
                        "Refresh"
                    }
                    button {
                        class: "icon-btn",
                        r#type: "button",
                        title: "Close",
                        "aria-label": "Close",
                        autofocus: true,
                        onclick: move |_| close_connector_with_reset(session, &close_id, saved_enabled),
                        Icon { name: "x", size: 16 }
                    }
                }
            }
            div { class: "mp-sheet-body",
                match connector {
                    // 0.63: the personal number has its own card, apart from
                    // WhatsApp Business.
                    None if id == integrations::WHATSAPP_PERSONAL_ID => rsx! {
                        WhatsappLinkCard { session }
                    },
                    None if id == MAIL_CONNECTOR_ID => rsx! {
                        crate::ui_connectors::MailConnectorCard { session, on_status: on_mail_changed }
                    },
                    None => rsx! { p { class: "muted", "This connector is no longer listed." } },
                    Some(connector) if connector.id == COMPOSIO_ID => rsx! {
                        ComposioCard { session, connector }
                    },
                    Some(connector) => {
                        let is_github = connector.id == "github";
                        let google = GOOGLE_IDS.contains(&connector.id.as_str());
                        let offered = google && google_setup_offered(&connectors, is_admin);
                        let configured = google_is_set_up(&connectors);
                        let redirect = google_redirect_uri(&connectors);
                        rsx! {
                            if offered {
                                GoogleSetupCard { session, configured, redirect }
                            }
                            if is_github {
                                GithubCard { session }
                                p { class: "tiny github-or", "Or connect with a personal access token." }
                            }
                            if connector.id == "whatsapp" {
                                p { class: "tiny github-or", "For a personal number, use the WhatsApp card instead. Business sends always ask you first." }
                            }
                            ConnectorCard { session, connector }
                        }
                    }
                }
            }
        }
    }
}
