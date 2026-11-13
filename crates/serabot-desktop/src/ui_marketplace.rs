//! The Marketplace screen (v0.37): a grid of plugins with search and a
//! category filter, and a page per plugin. Install and Enable stay off while
//! a plugin cannot be called. Everything goes through the session worker.
//!
//! Secrets: the server only says whether a field is "configured". The setup
//! form's input is masked, lives in this component only, is moved into the
//! request when sent and wiped, and is never shown again.

use crate::ui::Icon;
use crate::ui_widgets::{SecretField, StatusChip};
use dioxus::prelude::*;
use serabot_desktop::marketplace::{
    self, ConfigField, PluginAction, PluginDetail, PluginStatus, PluginSummary,
};
use serabot_desktop::session::Session;
use serabot_desktop::theme::Tone;
use zeroize::Zeroizing;

fn tone_of(status: PluginStatus) -> Tone {
    match status {
        PluginStatus::ComingSoon | PluginStatus::Available | PluginStatus::Installed => Tone::Muted,
        PluginStatus::NeedsSetup => Tone::Warn,
        PluginStatus::Enabled => Tone::Ok,
    }
}

#[component]
fn PluginCard(session: Signal<Session>, plugin: PluginSummary) -> Element {
    let mut session = session;
    let state = plugin.state();
    let slug = plugin.slug.clone();
    rsx! {
        button {
            class: "mp-card",
            r#type: "button",
            "aria-label": "{plugin.name}, {state.label()}",
            onclick: move |_| session.write().open_plugin(slug.clone()),
            div { class: "mp-card-head",
                span { class: "mp-card-name", title: "{plugin.name}", "{plugin.name}" }
                StatusChip { tone: tone_of(state), text: state.label().to_owned() }
            }
            if !plugin.tagline.is_empty() {
                span { class: "mp-card-tagline", "{plugin.tagline}" }
            }
            div { class: "mp-card-meta",
                if !plugin.category.is_empty() {
                    span { class: "chip muted", "{plugin.category}" }
                }
                if !plugin.version.is_empty() {
                    span { class: "tiny", "v{plugin.version}" }
                }
            }
        }
    }
}

/// One setup field: its state (set or not), a hint, and a masked input.
#[component]
fn FieldRow(field: ConfigField, value: Signal<String>, session: Signal<Session>) -> Element {
    let mut session = session;
    let label = if field.label.is_empty() {
        field.id.clone()
    } else {
        field.label.clone()
    };
    let docs = field
        .docs_url
        .clone()
        .filter(|url| marketplace::safe_link(url));
    rsx! {
        div { class: "mp-field",
            div { class: "mp-field-head",
                span { class: "mp-field-label", "{label}" }
                if field.configured {
                    StatusChip { tone: Tone::Ok, text: "Configured".to_owned() }
                } else {
                    StatusChip { tone: Tone::Warn, text: "Not set".to_owned() }
                }
            }
            if !field.hint.is_empty() {
                p { class: "tiny", "{field.hint}" }
            }
            SecretField {
                value,
                label: label.clone(),
                placeholder: if field.configured {
                    "Paste a new value to replace it".to_owned()
                } else {
                    "Paste the value".to_owned()
                },
            }
            if let Some(url) = docs {
                button {
                    class: "btn ghost sm mp-link",
                    r#type: "button",
                    onclick: move |_| session.write().open_in_browser(url.clone()),
                    Icon { name: "external", size: 14 }
                    "Where do I find this?"
                }
            }
        }
    }
}

/// The setup form of one plugin. Keyed by plugin, so switching plugin starts
/// with empty inputs.
#[component]
fn ConfigForm(session: Signal<Session>, slug: String, fields: Vec<ConfigField>) -> Element {
    let mut session = session;
    let mut problem = use_signal(|| Option::<String>::None);
    let signals: Vec<(String, Signal<String>)> = use_hook({
        let fields = fields.clone();
        move || {
            fields
                .iter()
                .map(|field| (field.id.clone(), Signal::new(String::new())))
                .collect()
        }
    });
    let busy = session.read().marketplace_busy;
    let rows: Vec<(ConfigField, Signal<String>)> = fields
        .iter()
        .cloned()
        .zip(signals.iter().map(|(_, signal)| *signal))
        .collect();
    rsx! {
        div { class: "mp-config",
            h3 { class: "mp-section-title", "Setup" }
            p { class: "tiny",
                "Values are sent to the server and kept in your private storage. They are never shown again here."
            }
            for (field, value) in rows {
                FieldRow { key: "{field.id}", field, value, session }
            }
            if let Some(message) = problem() {
                p { class: "tiny mp-problem", role: "alert", "{message}" }
            }
            div { class: "row-actions",
                button {
                    class: "btn primary sm",
                    r#type: "button",
                    disabled: busy,
                    onclick: {
                        let slug = slug.clone();
                        let fields = fields.clone();
                        move |_| {
                            // Move the typed text out of the inputs (no copies).
                            let mut taken: Vec<(String, Zeroizing<String>)> = signals
                                .iter()
                                .map(|(id, signal)| {
                                    let mut guard = signal.write_unchecked();
                                    (id.clone(), Zeroizing::new(std::mem::take(&mut *guard)))
                                })
                                .collect();
                            let refs: Vec<&ConfigField> = fields.iter().collect();
                            let checked = marketplace::config_values(&refs, |id| {
                                taken
                                    .iter()
                                    .find(|(key, _)| key == id)
                                    .map(|(_, value)| value.as_str())
                                    .unwrap_or("")
                            });
                            match checked {
                                Ok(values) => {
                                    problem.set(None);
                                    session.write().save_plugin_config(slug.clone(), values);
                                }
                                Err(message) => {
                                    // Give the text back so nothing typed is lost.
                                    for ((_, signal), (_, value)) in signals.iter().zip(taken.iter_mut()) {
                                        *signal.write_unchecked() = std::mem::take(&mut **value);
                                    }
                                    problem.set(Some(message));
                                }
                            }
                        }
                    },
                    "Save"
                }
            }
        }
    }
}

#[component]
fn PluginPage(session: Signal<Session>, detail: PluginDetail) -> Element {
    let mut session = session;
    let mut confirming = use_signal(|| false);
    let busy = session.read().marketplace_busy;
    let error = session.read().state.marketplace_error.clone();
    let state = detail.state();
    let slug = detail.plugin.slug.clone();
    let actions = marketplace::actions_for(&detail);
    let fields: Vec<ConfigField> = detail.fields().into_iter().cloned().collect();
    let links: Vec<(&str, String)> = [
        ("Website", detail.plugin.website_url.clone()),
        ("Privacy", detail.plugin.privacy_policy_url.clone()),
        ("Terms", detail.plugin.terms_url.clone()),
    ]
    .into_iter()
    .filter_map(|(label, url)| {
        url.filter(|url| marketplace::safe_link(url))
            .map(|url| (label, url))
    })
    .collect();
    let block = detail.activation_block();
    rsx! {
        div { class: "mp-sheet-scrim", onclick: move |_| session.write().close_plugin() }
        aside {
            class: "mp-sheet",
            role: "dialog",
            "aria-modal": "true",
            "aria-label": "{detail.plugin.name}",
            tabindex: "0",
            onkeydown: move |event: KeyboardEvent| {
                if event.key() == Key::Escape {
                    event.prevent_default();
                    session.write().close_plugin();
                }
            },
            div { class: "mp-sheet-head",
                div {
                    h2 { class: "mp-sheet-title", "{detail.plugin.name}" }
                    if !detail.plugin.tagline.is_empty() {
                        p { class: "muted", "{detail.plugin.tagline}" }
                    }
                }
                button {
                    class: "icon-btn",
                    r#type: "button",
                    title: "Close",
                    "aria-label": "Close",
                    autofocus: !confirming(),
                    onclick: move |_| session.write().close_plugin(),
                    Icon { name: "x", size: 16 }
                }
            }
            div { class: "mp-sheet-body",
                div { class: "mp-status-line",
                    StatusChip { tone: tone_of(state), text: state.label().to_owned() }
                    if !detail.plugin.category.is_empty() {
                        span { class: "chip muted", "{detail.plugin.category}" }
                    }
                    if !detail.plugin.version.is_empty() {
                        span { class: "tiny", "v{detail.plugin.version}" }
                    }
                }
                if let Some(message) = error {
                    div { class: "banner err mp-soon", role: "alert", span { "{message}" } }
                }
                if let Some(reason) = block {
                    p { class: "mp-soon-note", "{reason.message()}" }
                }
                if !detail.plugin.description.is_empty() {
                    p { class: "mp-description", "{detail.plugin.description}" }
                }
                if !detail.plugin.prompts.is_empty() {
                    h3 { class: "mp-section-title", "Example prompts" }
                    p { class: "tiny", "Examples only. The bot cannot run these yet." }
                    ul { class: "mp-prompts",
                        for prompt in detail.plugin.prompts.iter().cloned() {
                            li { key: "{prompt.id}{prompt.text}", "{prompt.text}" }
                        }
                    }
                }
                if !detail.plugin.permissions.egress.is_empty() {
                    h3 { class: "mp-section-title", "Sites it can reach" }
                    div { class: "mp-egress",
                        for host in detail.plugin.permissions.egress.iter().cloned() {
                            span { key: "{host}", class: "chip mono", "{host}" }
                        }
                    }
                }
                if !links.is_empty() {
                    div { class: "mp-links",
                        for (label, url) in links {
                            button {
                                key: "{label}",
                                class: "btn ghost sm mp-link",
                                r#type: "button",
                                onclick: move |_| session.write().open_in_browser(url.clone()),
                                Icon { name: "external", size: 14 }
                                "{label}"
                            }
                        }
                    }
                }
                if block.is_none() && detail.installed() && !fields.is_empty() {
                    ConfigForm { key: "{slug}", session, slug: slug.clone(), fields }
                }
                if confirming() {
                    div { class: "mp-confirm", role: "alertdialog", "aria-label": "Confirm uninstall",
                        p { "Uninstall {detail.plugin.name}? Its saved values are removed." }
                        div { class: "row-actions",
                            button {
                                class: "btn ghost sm",
                                r#type: "button",
                                autofocus: true,
                                onclick: move |_| confirming.set(false),
                                "Cancel"
                            }
                            button {
                                class: "btn danger sm",
                                r#type: "button",
                                disabled: busy,
                                onclick: {
                                    let slug = slug.clone();
                                    move |_| {
                                        confirming.set(false);
                                        session.write().plugin_action(slug.clone(), PluginAction::Uninstall);
                                    }
                                },
                                "Uninstall"
                            }
                        }
                    }
                } else if !actions.is_empty() {
                    div { class: "mp-actions",
                        for action in actions {
                            button {
                                key: "{action.label()}",
                                r#type: "button",
                                class: match action {
                                    PluginAction::Install | PluginAction::Enable => "btn primary",
                                    PluginAction::Disable => "btn",
                                    PluginAction::Uninstall => "btn danger",
                                },
                                disabled: busy,
                                onclick: {
                                    let slug = slug.clone();
                                    move |_| {
                                        if action == PluginAction::Uninstall {
                                            confirming.set(true);
                                        } else {
                                            session.write().plugin_action(slug.clone(), action);
                                        }
                                    }
                                },
                                "{action.label()}"
                            }
                        }
                        if busy {
                            span { class: "tiny", role: "status", "Working\u{2026}" }
                            span { class: "spinner", "aria-hidden": "true" }
                        }
                    }
                }
            }
        }
    }
}

#[component]
pub fn MarketplaceView(session: Signal<Session>) -> Element {
    let mut session = session;
    let mut query = use_signal(String::new);
    let mut category = use_signal(String::new);
    let (plugins, loaded, unavailable, error, detail, open, connected) = {
        let s = session.read();
        (
            s.state.marketplace.clone(),
            s.state.marketplace_loaded,
            s.state.marketplace_unavailable,
            s.state.marketplace_error.clone(),
            s.state.marketplace_detail.clone(),
            s.marketplace_open.clone(),
            s.state.connected,
        )
    };
    let categories = marketplace::categories(&plugins);
    let shown: Vec<PluginSummary> = marketplace::filter_plugins(&plugins, &query(), &category())
        .into_iter()
        .cloned()
        .collect();
    let filtering = !query().trim().is_empty() || !category().is_empty();
    rsx! {
        div { class: "pane mp-page",
            div { class: "pane-head",
                div {
                    h1 { class: "pane-title", "Marketplace" }
                    p { class: "muted", "Plugins your bots will be able to use. Browse the catalog. They cannot be turned on yet." }
                }
                button {
                    class: "btn ghost sm",
                    r#type: "button",
                    onclick: move |_| session.write().refresh_marketplace(),
                    Icon { name: "refresh", size: 14 }
                    "Refresh"
                }
            }
            div { class: "pane-body",
                if unavailable {
                    div { class: "card", role: "status",
                        p { class: "card-sub", "Marketplace unavailable on this server." }
                        p { class: "tiny", "This server has no plugin catalog. Update the server, then check again." }
                        button {
                            class: "btn sm",
                            r#type: "button",
                            onclick: move |_| session.write().refresh_marketplace(),
                            "Check again"
                        }
                    }
                } else if !connected && !loaded {
                    div { class: "card",
                        p { class: "card-sub", "Sign in to see plugins." }
                    }
                } else if !loaded && error.is_none() {
                    div { role: "status", "aria-busy": "true",
                        p { class: "tiny", "Loading plugins\u{2026}" }
                        div { class: "mp-grid", "aria-hidden": "true",
                            for index in 0..6 {
                                div { key: "{index}", class: "mp-card skeleton", " " }
                            }
                        }
                    }
                } else if let (false, Some(message)) = (loaded, error.clone()) {
                    div { class: "banner err", role: "alert",
                        span { "{message}" }
                        button {
                            class: "btn ghost sm",
                            r#type: "button",
                            onclick: move |_| session.write().refresh_marketplace(),
                            "Retry"
                        }
                    }
                } else {
                    if !marketplace::RUNTIME_CALLS_PLUGINS {
                        div { class: "banner mp-soon", role: "status",
                            span { "{marketplace::COMING_SOON}" }
                        }
                    }
                    if let Some(message) = error.clone() {
                        div { class: "banner err", role: "alert",
                            span { "{message}" }
                            button {
                                class: "btn ghost sm",
                                r#type: "button",
                                onclick: move |_| session.write().refresh_marketplace(),
                                "Retry"
                            }
                        }
                    }
                    if plugins.is_empty() {
                        div { class: "card",
                            p { class: "card-sub", "No plugins in this catalog yet." }
                            p { class: "tiny", "Ask the server admin to add plugins to the catalog." }
                        }
                    } else {
                        div { class: "mp-toolbar",
                            input {
                                class: "input mp-search",
                                r#type: "search",
                                placeholder: "Search plugins",
                                "aria-label": "Search plugins",
                                value: "{query}",
                                oninput: move |event| query.set(event.value()),
                            }
                            if !categories.is_empty() {
                                div { class: "seg", role: "group", "aria-label": "Category",
                                    button {
                                        class: if category().is_empty() { "seg-btn active" } else { "seg-btn" },
                                        r#type: "button",
                                        "aria-pressed": if category().is_empty() { "true" } else { "false" },
                                        onclick: move |_| category.set(String::new()),
                                        "All"
                                    }
                                    for name in categories.clone() {
                                        button {
                                            key: "{name}",
                                            r#type: "button",
                                            class: if category().eq_ignore_ascii_case(&name) { "seg-btn active" } else { "seg-btn" },
                                            "aria-pressed": if category().eq_ignore_ascii_case(&name) { "true" } else { "false" },
                                            onclick: {
                                                let name = name.clone();
                                                move |_| category.set(name.clone())
                                            },
                                            "{name}"
                                        }
                                    }
                                }
                            }
                        }
                        if shown.is_empty() {
                            div { class: "card",
                                p { class: "card-sub", "No plugins match." }
                                if filtering {
                                    button {
                                        class: "btn sm",
                                        r#type: "button",
                                        onclick: move |_| {
                                            query.set(String::new());
                                            category.set(String::new());
                                        },
                                        "Clear filters"
                                    }
                                }
                            }
                        } else {
                            div { class: "mp-grid", role: "list", "aria-label": "Plugins",
                                for plugin in shown {
                                    div { key: "{plugin.slug}", role: "listitem",
                                        PluginCard { session, plugin }
                                    }
                                }
                            }
                        }
                    }
                }
            }
            if let Some(slug) = open {
                if let Some(detail) = detail {
                    PluginPage { key: "{detail.plugin.slug}", session, detail }
                } else if error.is_none() {
                    div { class: "mp-sheet-scrim", onclick: move |_| session.write().close_plugin() }
                    aside {
                        class: "mp-sheet",
                        role: "dialog",
                        "aria-modal": "true",
                        "aria-label": "Loading plugin",
                        onkeydown: move |event: KeyboardEvent| {
                            if event.key() == Key::Escape {
                                session.write().close_plugin();
                            }
                        },
                        div { class: "mp-sheet-head",
                            h2 { class: "mp-sheet-title", "Loading" }
                            button {
                                class: "icon-btn",
                                r#type: "button",
                                title: "Close",
                                "aria-label": "Close",
                                autofocus: true,
                                onclick: move |_| session.write().close_plugin(),
                                Icon { name: "x", size: 16 }
                            }
                        }
                        div { class: "mp-sheet-body", p { class: "muted", "Loading\u{2026}" } }
                    }
                } else if let Some(message) = error.clone() {
                    div { class: "mp-sheet-scrim", onclick: move |_| session.write().close_plugin() }
                    aside {
                        class: "mp-sheet",
                        role: "alertdialog",
                        "aria-modal": "true",
                        "aria-label": "Plugin could not be opened",
                        onkeydown: move |event: KeyboardEvent| {
                            if event.key() == Key::Escape {
                                session.write().close_plugin();
                            }
                        },
                        div { class: "mp-sheet-head",
                            h2 { class: "mp-sheet-title", "Plugin" }
                            button {
                                class: "icon-btn",
                                r#type: "button",
                                title: "Close",
                                "aria-label": "Close",
                                autofocus: true,
                                onclick: move |_| session.write().close_plugin(),
                                Icon { name: "x", size: 16 }
                            }
                        }
                        div { class: "mp-sheet-body",
                            div { class: "banner err", role: "alert",
                                span { "{message}" }
                                button {
                                    class: "btn ghost sm",
                                    r#type: "button",
                                    onclick: move |_| session.write().open_plugin(slug.clone()),
                                    "Retry"
                                }
                            }
                            button {
                                class: "btn sm",
                                r#type: "button",
                                onclick: move |_| session.write().close_plugin(),
                                "Close"
                            }
                        }
                    }
                }
            }
        }
    }
}
