//! The Composio card on the Connectors screen.
//!
//! One key per person, pasted here and kept only on the server. With a key,
//! the card lists Composio's apps with a search box; Connect opens Composio's
//! own sign-in page in the browser, and the card updates by itself once the
//! app is linked. Disconnect unlinks it again.

use crate::ui::{BrandLogo, Icon};
use dioxus::prelude::*;
use serabot_desktop::ConnectorStatus;
use serabot_desktop::session::Session;

const COMPOSIO_HELP_URL: &str = "https://platform.composio.dev/";

#[component]
pub(crate) fn ComposioCard(session: Signal<Session>, connector: ConnectorStatus) -> Element {
    let mut session = session;
    let mut key = use_signal(String::new);
    let mut search = use_signal(String::new);
    let connected = connector.connected.unwrap_or(false);
    let (busy, loading, loaded, error, waiting, apps, accounts) = {
        let state = session.read();
        let flow = &state.panel.composio;
        (
            flow.busy,
            flow.loading,
            flow.loaded,
            flow.error.clone(),
            flow.waiting.clone(),
            flow.apps.clone(),
            flow.accounts.clone(),
        )
    };
    // Read the apps once the key is in.
    use_effect(move || {
        let (loaded, loading, busy) = {
            let state = session.read();
            let flow = &state.panel.composio;
            (flow.loaded || flow.error.is_some(), flow.loading, flow.busy)
        };
        if connected && !loaded && !loading && !busy {
            session.write().panel.composio.load("");
        }
    });
    let pill_class = if connected {
        "status-pill connected"
    } else {
        "status-pill disconnected"
    };
    let pill = if connected {
        "Connected"
    } else {
        "Not connected"
    };
    rsx! {
        div { class: "card connector-card",
            div { class: "card-head",
                div { class: "connector-id",
                    BrandLogo { brand: "composio".to_owned(), size: 28 }
                    div {
                        h3 { class: "card-title", "Composio" }
                        span { class: "card-sub",
                            "Let your bots use hundreds of apps, like Gmail, Slack, and GitHub."
                        }
                    }
                }
                span { class: pill_class, "{pill}" }
            }
            if let Some(message) = error.clone() {
                p { class: "banner err", role: "alert", "{message}" }
            }
            if !connected {
                div { class: "field",
                    label { class: "field-label", "Your Composio key" }
                    input {
                        class: "input",
                        "type": "password",
                        autocomplete: "off",
                        value: "{key}",
                        placeholder: "Paste it here",
                        oninput: move |event| key.set(event.value()),
                    }
                }
                p { class: "tiny",
                    "The key stays on the server and is never shown again. It lets your bots use only the apps you link here."
                }
                div { class: "card-actions",
                    button {
                        class: "btn primary",
                        disabled: busy || key().trim().is_empty(),
                        onclick: move |_| {
                            session.write().panel.composio_save_key(&key());
                            key.set(String::new());
                        },
                        if busy { "Checking…" } else { "Connect" }
                    }
                    button {
                        class: "btn ghost",
                        title: "Open Composio to create an account and copy your key",
                        onclick: move |_| session.write().open_in_browser(COMPOSIO_HELP_URL.to_owned()),
                        Icon { name: "external", size: 14 }
                        "How to get one"
                    }
                }
            } else {
                div { class: "sidebar-search",
                    span { class: "search-glyph", Icon { name: "search", size: 15 } }
                    input {
                        class: "input",
                        value: "{search}",
                        placeholder: "Search apps",
                        "aria-label": "Search Composio apps",
                        oninput: move |event| {
                            search.set(event.value());
                            session.write().panel.composio.load(&event.value());
                        },
                    }
                }
                if let Some(slug) = waiting.clone() {
                    p { class: "tiny", role: "status",
                        "Finish signing in to {slug} in your browser. This card updates by itself."
                    }
                }
                div { class: "stack",
                    if !loaded && loading {
                        div { class: "skeleton row", " " }
                    }
                    if loaded && apps.is_empty() {
                        p { class: "tiny", "No apps match that search." }
                    }
                    for app in apps {
                        {
                            let account = accounts
                                .iter()
                                .find(|a| a.toolkit.eq_ignore_ascii_case(&app.slug))
                                .cloned();
                            let linked = account.as_ref().is_some_and(|a| a.is_active());
                            let pending = account.is_some() && !linked;
                            let slug = app.slug.clone();
                            rsx! {
                                div { class: "vault-row", key: "{app.slug}",
                                    div { class: "vault-row-main",
                                        strong { "{app.name}" }
                                        if !app.description.is_empty() {
                                            span { class: "vault-meta", title: "{app.description}", "{app.description}" }
                                        }
                                    }
                                    div { class: "vault-actions",
                                        if linked {
                                            span { class: "status-pill connected", "Connected" }
                                            button {
                                                class: "btn ghost",
                                                disabled: busy,
                                                onclick: {
                                                    let id = account.as_ref().map(|a| a.id.clone()).unwrap_or_default();
                                                    move |_| session.write().panel.composio_unlink(&id)
                                                },
                                                "Disconnect"
                                            }
                                        } else {
                                            button {
                                                class: "btn ghost",
                                                disabled: busy,
                                                onclick: move |_| session.write().panel.composio_connect(&slug),
                                                if pending { "Connect again" } else { "Connect" }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
                div { class: "card-actions",
                    button {
                        class: "btn ghost",
                        disabled: busy,
                        title: "Forget your Composio key. Your bots lose access to these apps.",
                        onclick: move |_| session.write().panel.composio_remove_key(),
                        "Remove my key"
                    }
                }
            }
        }
    }
}
