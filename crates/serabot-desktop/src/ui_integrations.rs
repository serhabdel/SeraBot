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

use crate::ui::Icon;
use crate::ui_composio::ComposioCard;
use crate::ui_connectors::{ConnectorCard, GoogleSetupCard};
use crate::ui_marketplace::{MarketplaceTab, PluginSheet};
use crate::ui_widgets::StatusChip;
use dioxus::prelude::*;
use serabot_desktop::connector_card::{
    GOOGLE_IDS, google_is_set_up, google_redirect_uri, google_setup_offered,
};
use serabot_desktop::integrations::{self, COMPOSIO_ID, Entry, Source, Tab, connector_title};
use serabot_desktop::session::Session;

/// The whole screen: header, tabs and search, the active tab, and the sheets.
#[component]
pub fn IntegrationsView(session: Signal<Session>) -> Element {
    let mut session = session;
    let mut tab = use_signal(Tab::default);
    let mut query = use_signal(String::new);
    let (open_connector, has_plugin_sheet) = {
        let state = session.read();
        (
            state.integration_open.clone(),
            state.marketplace_open.is_some(),
        )
    };
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
                    title: "Check for changes",
                    onclick: move |_| {
                        session.write().refresh_connectors();
                        session.write().refresh_marketplace();
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
                    input {
                        class: "input mp-search",
                        r#type: "search",
                        placeholder: "Search integrations",
                        "aria-label": "Search integrations",
                        value: "{query}",
                        oninput: move |event| query.set(event.value()),
                    }
                }
                {match tab() {
                    Tab::Installed => rsx! { InstalledTab { session, query, tab } },
                    Tab::Marketplace => rsx! { MarketplaceTab { session, query } },
                    Tab::Composio => rsx! { ComposioTab { session } },
                }}
            }
            if let Some(id) = open_connector {
                ConnectorSheet { key: "{id}", session, id }
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
    rsx! {
        button {
            class: "mp-card integration-card",
            r#type: "button",
            "aria-label": "{label}",
            onclick: move |_| match &source {
                Source::Connector(id) => session.write().open_connector_sheet(id.clone()),
                Source::Plugin(slug) => session.write().open_plugin(slug.clone()),
            },
            div { class: "mp-card-head",
                span { class: "mp-card-name", title: "{entry.name}", "{entry.name}" }
                StatusChip { tone: entry.tone, text: entry.pill.clone() }
            }
            if !entry.blurb.is_empty() {
                span { class: "mp-card-tagline", "{entry.blurb}" }
            }
            div { class: "mp-card-meta",
                span { class: "tiny", "{entry.meta}" }
            }
        }
    }
}

/// The Installed tab: what is set up, then the connectors still open to set up,
/// by suite. The search filters both.
#[component]
fn InstalledTab(session: Signal<Session>, query: Signal<String>, tab: Signal<Tab>) -> Element {
    let (connectors, plugins, connectors_loaded, marketplace_loaded, errors) = {
        let state = session.read();
        (
            state.state.connectors.clone(),
            state.state.marketplace.clone(),
            state.state.connectors_loaded,
            state.state.marketplace_loaded,
            state.panel.connectors.errors.clone(),
        )
    };
    let mut tab = tab;
    let mut query = query;
    let needle = query();
    let lookup = |id: &str| errors.get(id).cloned();
    let view = integrations::installed(&connectors, &plugins, &lookup);
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
    let loading = !connectors_loaded && !marketplace_loaded;
    rsx! {
        if loading {
            div { role: "status", "aria-busy": "true",
                p { class: "tiny", "Loading integrations\u{2026}" }
                div { class: "mp-grid", "aria-hidden": "true",
                    for index in 0..6 {
                        div { key: "{index}", class: "mp-card skeleton", " " }
                    }
                }
            }
        } else if nothing && needle.trim().is_empty() {
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
        } else if nothing {
            div { class: "card",
                p { class: "card-sub", "No integrations match that search." }
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

/// The Composio tab: its key form when there is none, then its apps.
#[component]
fn ComposioTab(session: Signal<Session>) -> Element {
    let connector = session
        .read()
        .state
        .connectors
        .iter()
        .find(|connector| connector.id == COMPOSIO_ID)
        .cloned();
    match connector {
        Some(connector) => rsx! { ComposioCard { session, connector } },
        None => rsx! {
            div { class: "card",
                p { class: "card-sub", "Composio is not listed on this server." }
            }
        },
    }
}

/// A connector's card in the detail sheet: the shared frame, then the
/// connector's own screen. A Google connector also shows the Google setup card
/// while an admin has yet to set it up.
#[component]
fn ConnectorSheet(session: Signal<Session>, id: String) -> Element {
    let mut session = session;
    let (connector, connectors, is_admin) = {
        let state = session.read();
        (
            state.state.connectors.iter().find(|c| c.id == id).cloned(),
            state.state.connectors.clone(),
            state.is_admin(),
        )
    };
    let title = connector_title(&id);
    let close = move |_| session.write().close_connector_sheet();
    rsx! {
        div { class: "mp-sheet-scrim", onclick: close }
        aside {
            class: "mp-sheet",
            role: "dialog",
            "aria-modal": "true",
            "aria-label": "{title}",
            onkeydown: move |event: KeyboardEvent| {
                if event.key() == Key::Escape {
                    session.write().close_connector_sheet();
                }
            },
            div { class: "mp-sheet-head",
                h2 { class: "mp-sheet-title", "{title}" }
                button {
                    class: "icon-btn",
                    r#type: "button",
                    title: "Close",
                    "aria-label": "Close",
                    autofocus: true,
                    onclick: close,
                    Icon { name: "x", size: 16 }
                }
            }
            div { class: "mp-sheet-body",
                match connector {
                    None => rsx! { p { class: "muted", "This connector is no longer listed." } },
                    Some(connector) if connector.id == COMPOSIO_ID => rsx! {
                        ComposioCard { session, connector }
                    },
                    Some(connector) => {
                        let google = GOOGLE_IDS.contains(&connector.id.as_str());
                        let offered = google && google_setup_offered(&connectors, is_admin);
                        let configured = google_is_set_up(&connectors);
                        let redirect = google_redirect_uri(&connectors);
                        rsx! {
                            if offered {
                                GoogleSetupCard { session, configured, redirect }
                            }
                            ConnectorCard { session, connector }
                        }
                    }
                }
            }
        }
    }
}
