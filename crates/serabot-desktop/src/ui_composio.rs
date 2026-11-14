//! The Composio card on the Connectors screen.
//!
//! One key per person, pasted here and kept only on the server. With a key,
//! the card lists Composio's apps with a search box; Connect opens Composio's
//! own sign-in page in the browser, and the card updates by itself once the
//! app is linked. Disconnect unlinks it again.

use crate::ui::{BrandLogo, Icon};
use crate::ui_widgets::{SecretField, StatusChip};
use dioxus::prelude::*;
use serabot_desktop::ConnectorStatus;
use serabot_desktop::composio_flow::key_hint;
use serabot_desktop::session::Session;
use serabot_desktop::theme::Tone;

const COMPOSIO_HELP_URL: &str = "https://platform.composio.dev/";
/// Where a Connect key's apps are managed. Fixed here, never taken from the server.
const COMPOSIO_CONNECT_URL: &str = "https://dashboard.composio.dev/";

#[component]
pub(crate) fn ComposioCard(session: Signal<Session>, connector: ConnectorStatus) -> Element {
    let mut session = session;
    let mut key = use_signal(String::new);
    let mut search = use_signal(String::new);
    let mut confirm_remove_key = use_signal(|| false);
    let mut confirm_unlink = use_signal(|| None::<String>);
    let connected = connector.connected.unwrap_or(false);
    // Clear any local key draft once connected so accepted secrets are never kept in memory.
    use_effect(move || {
        if connected {
            if !key.read().is_empty() {
                key.set(String::new());
            }
            confirm_remove_key.set(false);
            confirm_unlink.set(None);
        }
    });
    // A Connect (ck_) key: its apps are linked on Composio's side.
    let managed = connected && connector.key_kind.as_deref() == Some("consumer");
    let (busy, loading, loaded, error, waiting, timed_out, apps, accounts) = {
        let state = session.read();
        let flow = &state.panel.composio;
        (
            flow.busy,
            flow.loading,
            flow.loaded,
            flow.error.clone(),
            flow.waiting.clone(),
            flow.timed_out.clone(),
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
    let key_empty = key().trim().is_empty();
    let search_trimmed = search().trim().to_owned();
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
                StatusChip {
                    tone: if connected { Tone::Ok } else { Tone::Warn },
                    text: if connected { "Connected".to_owned() } else { "Not connected".to_owned() },
                }
            }
            if let Some(message) = error.clone() {
                div { class: "banner err", role: "alert",
                    span { class: "grow", "{message}" }
                }
            }
            if !connected {
                div { class: "field",
                    span { class: "field-label", "Your Composio key" }
                    SecretField {
                        value: key,
                        label: "Your Composio key".to_owned(),
                        placeholder: "Paste it here".to_owned(),
                        disabled: busy,
                        invalid: error.is_some(),
                        on_submit: move |_| {
                            if !busy && !key().trim().is_empty() {
                                session.write().panel.composio_save_key(&key());
                            }
                        },
                    }
                    p { class: "tiny",
                        "Paste a project key (ak_) from platform.composio.dev, a user key (uak_), or a Connect key (ck_) from Composio Connect."
                    }
                    if !key_empty {
                        p { class: "tiny", role: "status", "{key_hint(&key())}" }
                    }
                }
                p { class: "tiny",
                    "The key stays on the server and is never shown again. It lets your bots use only the apps you link here."
                }
                div { class: "card-actions",
                    button {
                        class: "btn primary",
                        r#type: "button",
                        disabled: busy || key_empty,
                        onclick: move |_| {
                            session.write().panel.composio_save_key(&key());
                        },
                        if busy { "Checking…" } else { "Connect" }
                    }
                    button {
                        class: "btn ghost",
                        r#type: "button",
                        title: "Open Composio to create an account and copy your key",
                        onclick: move |_| session.write().open_in_browser(COMPOSIO_HELP_URL.to_owned()),
                        Icon { name: "external", size: 14 }
                        "How to get one"
                    }
                }
            } else {
                div { class: "search-wrap",
                    input {
                        class: "input",
                        r#type: "search",
                        value: "{search}",
                        placeholder: "Search Composio apps",
                        "aria-label": "Search Composio apps",
                        oninput: move |event| {
                            let next = event.value();
                            search.set(next.clone());
                            session.write().panel.composio.load(&next);
                        },
                        onkeydown: move |event: KeyboardEvent| {
                            if event.key() == Key::Escape && !search.read().is_empty() {
                                event.prevent_default();
                                search.set(String::new());
                                session.write().panel.composio.load("");
                            }
                        },
                    }
                    if !search_trimmed.is_empty() {
                        button {
                            class: "btn ghost sm",
                            r#type: "button",
                            onclick: move |_| {
                                search.set(String::new());
                                session.write().panel.composio.load("");
                            },
                            "Clear"
                        }
                    }
                    button {
                        class: "btn ghost sm",
                        r#type: "button",
                        disabled: busy || loading,
                        title: "Refresh Composio apps and linked accounts",
                        onclick: move |_| session.write().panel.composio.load(&search()),
                        Icon { name: "refresh", size: 14 }
                        if loading { " Loading…" } else { " Refresh" }
                    }
                }
                if managed {
                    p { class: "tiny", role: "status",
                        "Your apps are managed in Composio Connect. Link or remove them there; the tools below are what this key offers."
                    }
                    div { class: "card-actions",
                        button {
                            class: "btn ghost",
                            r#type: "button",
                            title: "Open Composio Connect to manage your apps",
                            onclick: move |_| session.write().open_in_browser(COMPOSIO_CONNECT_URL.to_owned()),
                            Icon { name: "external", size: 14 }
                            "Open Composio Connect"
                        }
                    }
                }
                if let Some(slug) = waiting.clone() {
                    div { class: "banner info", role: "status",
                        span { class: "spinner", title: "Waiting for sign-in" }
                        span { class: "grow",
                            "Finish signing in to {slug} in your browser. SeraBot checks automatically."
                        }
                        button {
                            class: "btn ghost sm",
                            r#type: "button",
                            disabled: loading || busy,
                            onclick: move |_| session.write().panel.composio.load(&search()),
                            "Check now"
                        }
                        button {
                            class: "btn ghost sm",
                            r#type: "button",
                            onclick: move |_| session.write().panel.composio.cancel_wait(),
                            "Cancel"
                        }
                    }
                }
                if let Some(slug) = timed_out.clone() {
                    div { class: "banner warn", role: "status",
                        span { class: "grow",
                            "Sign-in for {slug} timed out after 5 minutes. You can try connecting again."
                        }
                        button {
                            class: "btn ghost sm",
                            r#type: "button",
                            onclick: move |_| session.write().panel.composio.dismiss_timeout(),
                            "Dismiss"
                        }
                    }
                }
                div { class: "stack",
                    if !loaded && loading {
                        div { class: "skeleton row", " " }
                    } else if loaded && apps.is_empty() {
                        div { class: "empty compact",
                            h4 { class: "empty-title",
                                if search_trimmed.is_empty() {
                                    "No apps available"
                                } else {
                                    "No apps match that search"
                                }
                            }
                            p { class: "empty-text",
                                if search_trimmed.is_empty() {
                                    "Composio returned no tools for this key yet."
                                } else {
                                    "Try another app name or clear the search filter."
                                }
                            }
                            if !search_trimmed.is_empty() {
                                button {
                                    class: "btn ghost sm",
                                    r#type: "button",
                                    onclick: move |_| {
                                        search.set(String::new());
                                        session.write().panel.composio.load("");
                                    },
                                    "Clear search"
                                }
                            }
                        }
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
                            let acc_id = account.as_ref().map(|a| a.id.clone()).unwrap_or_default();
                            let confirming_app =
                                !acc_id.is_empty() && confirm_unlink().as_deref() == Some(acc_id.as_str());
                            rsx! {
                                div { class: "vault-row", key: "{app.slug}",
                                    div { class: "vault-row-main",
                                        strong { "{app.name}" }
                                        if !app.description.is_empty() {
                                            span { class: "vault-meta", title: "{app.description}", "{app.description}" }
                                        }
                                    }
                                    div { class: "vault-actions",
                                        if managed {
                                            StatusChip { tone: Tone::Ok, text: "Available".to_owned() }
                                        } else if linked {
                                            StatusChip { tone: Tone::Ok, text: "Connected".to_owned() }
                                            if confirming_app {
                                                button {
                                                    class: "btn danger sm",
                                                    r#type: "button",
                                                    disabled: busy,
                                                    onclick: {
                                                        let id = acc_id.clone();
                                                        move |_| {
                                                            confirm_unlink.set(None);
                                                            session.write().panel.composio_unlink(&id);
                                                        }
                                                    },
                                                    "Confirm disconnect"
                                                }
                                                button {
                                                    class: "btn ghost sm",
                                                    r#type: "button",
                                                    disabled: busy,
                                                    onclick: move |_| confirm_unlink.set(None),
                                                    "Cancel"
                                                }
                                            } else {
                                                button {
                                                    class: "btn ghost sm",
                                                    r#type: "button",
                                                    disabled: busy,
                                                    onclick: {
                                                        let id = acc_id.clone();
                                                        move |_| confirm_unlink.set(Some(id.clone()))
                                                    },
                                                    "Disconnect"
                                                }
                                            }
                                        } else {
                                            if pending {
                                                StatusChip { tone: Tone::Warn, text: "Incomplete".to_owned() }
                                            }
                                            button {
                                                class: "btn ghost sm",
                                                r#type: "button",
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
                    if confirm_remove_key() {
                        div { class: "mp-confirm", role: "alertdialog", "aria-label": "Confirm removing Composio key",
                            span { class: "tiny", "Forget your Composio key? Bots will lose access to these apps." }
                            button {
                                class: "btn danger sm",
                                r#type: "button",
                                disabled: busy,
                                onclick: move |_| {
                                    confirm_remove_key.set(false);
                                    session.write().panel.composio_remove_key();
                                },
                                "Confirm remove"
                            }
                            button {
                                class: "btn ghost sm",
                                r#type: "button",
                                disabled: busy,
                                onclick: move |_| confirm_remove_key.set(false),
                                "Cancel"
                            }
                        }
                    } else {
                        button {
                            class: "btn ghost",
                            r#type: "button",
                            disabled: busy,
                            title: "Forget your Composio key. Your bots lose access to these apps.",
                            onclick: move |_| confirm_remove_key.set(true),
                            "Remove my key"
                        }
                    }
                }
            }
        }
    }
}
