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
                        Tab::Installed => session.write().refresh_connectors(),
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
                    Tab::Installed => rsx! { InstalledTab { session, query, tab } },
                    Tab::Marketplace => rsx! { MarketplaceTab { session, query } },
                    Tab::Bots => rsx! { BotsTab { session, query } },
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
    let cta = if entry.set_up {
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

/// The Installed tab: what is set up, then the connectors still open to set up,
/// by suite. The search filters both.
#[component]
fn InstalledTab(session: Signal<Session>, query: Signal<String>, tab: Signal<Tab>) -> Element {
    let (
        connectors,
        plugins,
        connectors_loaded,
        marketplace_loaded,
        marketplace_unavailable,
        marketplace_error,
        state_error,
        errors,
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
    let marketplace_pending =
        !marketplace_loaded && !marketplace_unavailable && marketplace_error.is_none();
    let loading = nothing && (!connectors_loaded || marketplace_pending);
    let inline_error = state_error.or(marketplace_error);
    rsx! {
        if let Some(message) = inline_error {
            div { class: "banner err", role: "alert",
                span { class: "grow", "{message}" }
                button {
                    class: "btn ghost sm",
                    r#type: "button",
                    onclick: move |_| session.write().refresh_connectors(),
                    "Retry"
                }
            }
        }
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
        p { class: "hint", "{installed} of {total} built-in bots are on your team. Remove one and add it back here any time." }
        crate::ui_bots::ShippedList { session, query: query(), overlay: None }
    }
}

/// The Composio tab: its key form when there is none, then its apps.
#[component]
fn ComposioTab(session: Signal<Session>) -> Element {
    let (loaded, connector) = {
        let state = session.read();
        (
            state.state.connectors_loaded,
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
        None if !loaded => rsx! {
            div { role: "status", "aria-busy": "true",
                p { class: "tiny", "Loading Composio\u{2026}" }
                div { class: "mp-card skeleton", " " }
            }
        },
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
    let saved_enabled = connector.as_ref().map(|c| c.enabled);
    let title = connector_title(&id);
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
