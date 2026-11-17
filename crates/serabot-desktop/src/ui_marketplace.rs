//! The Marketplace tab and the plugin page (v0.37, merged into the
//! Integrations screen in v0.38). The tab is a grid of plugin cards with a
//! category filter; the page opens in the shared detail sheet. Install and
//! Enable stay off while a plugin cannot be called. Everything goes through
//! the session worker.
//!
//! Secrets: the server only says whether a field is "configured". The setup
//! form's input is masked, lives in this component only, is moved into the
//! request when sent and wiped, and is never shown again.

use crate::ui::{BrandLogo, Icon};
use crate::ui_integrations::IntegrationCard;
use crate::ui_mcp_accounts::{AccountsSection, ToolsSection};
use crate::ui_widgets::{SecretField, StatusChip};
use dioxus::prelude::*;
use serabot_desktop::integrations::{plugin_entry, plugin_tone};
use serabot_desktop::marketplace::{
    self, ConfigField, PluginAction, PluginDetail, PluginSummary, StatusFilter,
};
use serabot_desktop::session::Session;
use serabot_desktop::theme::Tone;
use zeroize::Zeroizing;

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
                    if confirming() {
                        confirming.set(false);
                    } else {
                        session.write().close_plugin();
                    }
                }
            },
            div { class: "mp-sheet-head",
                div { class: "mp-head",
                    BrandLogo { brand: detail.plugin.slug.clone(), size: 40 }
                    div {
                        h2 { class: "mp-sheet-title", "{detail.plugin.name}" }
                        if !detail.plugin.tagline.is_empty() {
                            p { class: "muted", "{detail.plugin.tagline}" }
                        }
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
                    StatusChip { tone: plugin_tone(state), text: state.label().to_owned() }
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
                    div { class: "banner info mp-soon", role: "status",
                        span { "{reason.message()}" }
                    }
                }
                if !detail.plugin.description.is_empty() {
                    p { class: "mp-description", "{detail.plugin.description}" }
                }
                if !detail.plugin.prompts.is_empty() {
                    h3 { class: "mp-section-title", "Example prompts" }
                    p { class: "tiny", "Ideas for what to ask once the plugin is installed and signed in." }
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
                if block.is_none() && detail.installed() && detail.uses_oauth() {
                    AccountsSection { key: "accounts-{slug}", session, detail: detail.clone() }
                }
                if block.is_none() && detail.installed() && detail.fully_configured() && detail.signed_in() {
                    ToolsSection { key: "tools-{slug}", session, slug: slug.clone() }
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

/// The Marketplace tab's body: the catalogue as cards, with a category filter.
/// The search box above the tabs is shared, and comes in as `query`.
#[component]
pub fn MarketplaceTab(session: Signal<Session>, query: Signal<String>) -> Element {
    let mut category = use_signal(String::new);
    let mut status_filter = use_signal(|| StatusFilter::All);
    let (plugins, loaded, unavailable, error, connected) = {
        let state = session.read();
        (
            state.state.marketplace.clone(),
            state.state.marketplace_loaded,
            state.state.marketplace_unavailable,
            state.state.marketplace_error.clone(),
            state.state.connected,
        )
    };
    let mut session = session;
    let categories = marketplace::category_counts(&plugins);
    let total_plugins = plugins.len();
    let shown: Vec<PluginSummary> =
        marketplace::filter_plugins_with_status(&plugins, &query(), &category(), status_filter())
            .into_iter()
            .cloned()
            .collect();
    let filtering = !query().trim().is_empty()
        || !category().is_empty()
        || status_filter() != StatusFilter::All;
    rsx! {
        if unavailable {
            div { class: "empty compact", role: "status",
                h3 { class: "empty-title", "Marketplace unavailable on this server" }
                p { class: "empty-text", "This server does not expose a plugin catalog yet. Update the server, then check again." }
                button {
                    class: "btn sm",
                    r#type: "button",
                    onclick: move |_| session.write().refresh_marketplace(),
                    "Check again"
                }
            }
        } else if !connected && !loaded {
            div { class: "empty compact", role: "status",
                h3 { class: "empty-title", "Sign in to view plugins" }
                p { class: "empty-text", "The plugin catalog loads once your SeraBot session is connected." }
            }
        } else if !loaded && error.is_none() {
            crate::ui_integrations::SkeletonGrid { label: "Loading plugins", count: 8 }
        } else if let (false, Some(message)) = (loaded, error.clone()) {
            div { class: "banner err", role: "alert",
                span { class: "grow", "{message}" }
                button {
                    class: "btn ghost sm",
                    r#type: "button",
                    onclick: move |_| session.write().refresh_marketplace(),
                    "Retry"
                }
            }
        } else {
            if !marketplace::RUNTIME_CALLS_PLUGINS {
                div { class: "banner info mp-soon", role: "status",
                    span { "{marketplace::COMING_SOON}" }
                }
            }
            if let Some(message) = error.clone() {
                div { class: "banner err", role: "alert",
                    span { class: "grow", "{message}" }
                    button {
                        class: "btn ghost sm",
                        r#type: "button",
                        onclick: move |_| session.write().refresh_marketplace(),
                        "Retry"
                    }
                }
            }
            if plugins.is_empty() {
                div { class: "empty compact",
                    h3 { class: "empty-title", "No plugins in this catalog yet" }
                    p { class: "empty-text", "Ask your server admin to add plugins to the catalog." }
                }
            } else {
                div { class: "mp-toolbar",
                    if !categories.is_empty() {
                        div { class: "seg", role: "group", "aria-label": "Category",
                            button {
                                class: if category().is_empty() { "seg-btn active" } else { "seg-btn" },
                                r#type: "button",
                                "aria-pressed": if category().is_empty() { "true" } else { "false" },
                                onclick: move |_| category.set(String::new()),
                                "All ({total_plugins})"
                            }
                            for (name, count) in categories.clone() {
                                button {
                                    key: "{name}",
                                    r#type: "button",
                                    class: if category().eq_ignore_ascii_case(&name) { "seg-btn active" } else { "seg-btn" },
                                    "aria-pressed": if category().eq_ignore_ascii_case(&name) { "true" } else { "false" },
                                    onclick: {
                                        let name = name.clone();
                                        move |_| category.set(name.clone())
                                    },
                                    "{name} ({count})"
                                }
                            }
                        }
                    }
                    div { class: "seg", role: "group", "aria-label": "Status",
                        for filter_opt in [StatusFilter::All, StatusFilter::Installed, StatusFilter::ComingSoon] {
                            button {
                                key: "{filter_opt.label()}",
                                r#type: "button",
                                class: if status_filter() == filter_opt { "seg-btn active" } else { "seg-btn" },
                                "aria-pressed": if status_filter() == filter_opt { "true" } else { "false" },
                                onclick: move |_| status_filter.set(filter_opt),
                                "{filter_opt.label()}"
                            }
                        }
                    }
                    if filtering {
                        span { class: "tiny muted", role: "status", "{shown.len()} shown" }
                    }
                }
                if shown.is_empty() {
                    div { class: "empty compact",
                        h3 { class: "empty-title", "No plugins match" }
                        p { class: "empty-text", "Try clearing your search or changing the category and status filters." }
                        if filtering {
                            button {
                                class: "btn sm",
                                r#type: "button",
                                onclick: move |_| {
                                    query.set(String::new());
                                    category.set(String::new());
                                    status_filter.set(StatusFilter::All);
                                },
                                "Clear filters"
                            }
                        }
                    }
                } else {
                    div { class: "mp-grid", role: "list", "aria-label": "Plugins",
                        for plugin in shown {
                            div { key: "{plugin.slug}", role: "listitem",
                                IntegrationCard { session, entry: plugin_entry(&plugin) }
                            }
                        }
                    }
                }
            }
        }
    }
}

/// The plugin page in the shared detail sheet: loading, a failure with a
/// retry, or the page itself. Renders nothing while no plugin is open.
#[component]
pub fn PluginSheet(session: Signal<Session>) -> Element {
    let mut session = session;
    let (open, detail, error) = {
        let state = session.read();
        (
            state.marketplace_open.clone(),
            state.state.marketplace_detail.clone(),
            state.state.marketplace_error.clone(),
        )
    };
    let Some(slug) = open else {
        return rsx! {};
    };
    if let Some(detail) = detail {
        return rsx! {
            PluginPage { key: "{detail.plugin.slug}", session, detail }
        };
    }
    let Some(message) = error else {
        return rsx! {
            div { class: "mp-sheet-scrim", onclick: move |_| session.write().close_plugin() }
            aside {
                class: "mp-sheet",
                role: "dialog",
                "aria-modal": "true",
                "aria-label": "Loading plugin",
                tabindex: "0",
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
                div { class: "mp-sheet-body",
                    div { class: "row", role: "status", "aria-busy": "true",
                        span { class: "spinner", "aria-hidden": "true" }
                        span { class: "muted", "Loading plugin details\u{2026}" }
                    }
                }
            }
        };
    };
    rsx! {
        div { class: "mp-sheet-scrim", onclick: move |_| session.write().close_plugin() }
        aside {
            class: "mp-sheet",
            role: "alertdialog",
            "aria-modal": "true",
            "aria-label": "Plugin could not be opened",
            tabindex: "0",
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
                    span { class: "grow", "{message}" }
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
