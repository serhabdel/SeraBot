//! Plugin accounts, per-tool switches and the GitHub card (v0.51).
//!
//! The plugin page shows who is signed in, with a state badge per account, a
//! Sign in button that opens the system browser, and one switch per tool. The
//! GitHub card signs in with a one-time code. Everything is drawn from what
//! the server reports; no token is ever on this screen.
//!
//! Feel: every press answers at once (a switch moves before the server
//! replies, a button shows its busy state), every wait can be cancelled, and
//! each state change is a short transition that a press interrupts. All
//! motion is off for reduced motion (see `marketplace.css`), and every control
//! keeps its focus ring.

use crate::ui::Icon;
use crate::ui_files::copy_script;
use crate::ui_widgets::StatusChip;
use dioxus::prelude::*;
use serabot_desktop::integrations::{account_tone, github_tone};
use serabot_desktop::marketplace::{self, AccountAction, PluginAccount, PluginDetail};
use serabot_desktop::provider_flow::{self, FlowPhase, ProviderFlow};
use serabot_desktop::session::{Session, ToolsState};
use std::time::Duration;

/// The sign-in panel: the page or the code, a countdown, a clear end state.
/// Cancel is always one press away; a finished panel offers Try again and Done.
#[component]
fn McpFlowPanel(
    flow: ProviderFlow,
    elapsed: Duration,
    can_start: bool,
    on_retry: EventHandler<()>,
    on_cancel: EventHandler<()>,
    on_dismiss: EventHandler<()>,
    on_open: EventHandler<String>,
) -> Element {
    let waiting = flow.phase == FlowPhase::Waiting;
    let starting = flow.phase == FlowPhase::Starting;
    let ok = matches!(flow.phase, FlowPhase::Done { .. });
    let bad = matches!(
        flow.phase,
        FlowPhase::Denied(_) | FlowPhase::Failed(_) | FlowPhase::Expired
    );
    let class = if ok {
        "flow-panel is-ok"
    } else if bad {
        "flow-panel is-bad"
    } else {
        "flow-panel"
    };
    let device = flow.kind == provider_flow::FlowKind::Device;
    let remaining = flow.remaining_secs(elapsed);
    let percent = flow.elapsed_percent(elapsed);
    let code = flow.user_code.clone();
    let url = flow.url.clone();
    let headline = flow.headline();
    let detail = flow.detail();
    rsx! {
        div { class: "{class}", "aria-live": "polite",
            p { class: "flow-title", "{headline}" }
            if starting {
                span { class: "flow-wait", role: "status",
                    span { class: "flow-wait-dot", "aria-hidden": "true" }
                    "One moment"
                }
            }
            if waiting {
                if let Some(code) = code.clone() {
                    div { class: "device-code", "aria-label": "One-time code", "{code}" }
                }
            }
            if let Some(text) = detail {
                p { class: "muted", "{text}" }
            }
            if waiting {
                if device {
                    div { class: "flow-count",
                        span { "{provider_flow::countdown_text(remaining)} left" }
                        div { class: "flow-bar", "aria-hidden": "true",
                            div { class: "flow-bar-fill", style: "width: {percent}%;" }
                        }
                    }
                }
                span { class: "flow-wait", role: "status",
                    span { class: "flow-wait-dot", "aria-hidden": "true" }
                    if device { "Waiting for approval\u{2026}" } else { "Waiting for the browser\u{2026}" }
                }
                div { class: "route-actions",
                    if let Some(page) = url.clone() {
                        button {
                            class: "btn primary sm",
                            r#type: "button",
                            autofocus: true,
                            onclick: {
                                let page = page.clone();
                                move |_| on_open.call(page.clone())
                            },
                            Icon { name: "external", size: 14 }
                            "Open page"
                        }
                    }
                    if let Some(code) = code {
                        button {
                            class: "btn ghost sm",
                            r#type: "button",
                            onclick: move |_| {
                                let _ = dioxus::document::eval(&copy_script(&code));
                            },
                            Icon { name: "copy", size: 14 }
                            "Copy code"
                        }
                    }
                    button {
                        class: "btn ghost sm",
                        r#type: "button",
                        onclick: move |_| on_cancel.call(()),
                        "Cancel sign-in"
                    }
                }
                if !device {
                    if let Some(page) = url {
                        p { class: "flow-link", "{page}" }
                    }
                }
            }
            if bad || flow.phase == FlowPhase::Cancelled {
                div { class: "route-actions",
                    button {
                        class: "btn primary sm",
                        r#type: "button",
                        autofocus: true,
                        disabled: !can_start,
                        onclick: move |_| on_retry.call(()),
                        "Try again"
                    }
                    button {
                        class: "btn ghost sm",
                        r#type: "button",
                        onclick: move |_| on_dismiss.call(()),
                        "Dismiss"
                    }
                }
            }
            if ok {
                div { class: "route-actions",
                    button {
                        class: "btn sm",
                        r#type: "button",
                        autofocus: true,
                        onclick: move |_| on_dismiss.call(()),
                        "Done"
                    }
                }
            }
        }
    }
}

/// One account row: its name, a state badge, one sentence, and its actions.
#[component]
fn AccountRow(
    session: Signal<Session>,
    slug: String,
    account: PluginAccount,
    confirming: bool,
    removing: bool,
    can_start: bool,
) -> Element {
    let mut session = session;
    let state = account.state();
    let title = account.title().to_owned();
    let id = account.account_id.clone();
    let line = account
        .error_line()
        .unwrap_or_else(|| state.hint().to_owned());
    let class = match (removing, state) {
        (true, _) => "mp-account is-busy",
        (_, marketplace::AccountState::Ready) => "mp-account",
        _ => "mp-account needs-attention",
    };
    rsx! {
        li { class: "{class}", "aria-busy": if removing { "true" } else { "false" },
            div { class: "mp-account-main",
                div { class: "mp-account-name",
                    span { class: "mp-account-title", title: "{title}", "{title}" }
                    StatusChip { tone: account_tone(state), text: state.label().to_owned() }
                }
                p { class: "tiny", "{line}" }
            }
            if confirming {
                div { class: "mp-confirm", role: "alertdialog", "aria-label": "Confirm sign-out",
                    p { "Sign out of {title}? The service is asked to revoke access, and the sign-in is deleted here either way." }
                    div { class: "row-actions",
                        button {
                            class: "btn ghost sm",
                            r#type: "button",
                            autofocus: true,
                            onclick: move |_| session.write().cancel_remove_plugin_account(),
                            "Cancel"
                        }
                        button {
                            class: "btn danger sm",
                            r#type: "button",
                            onclick: {
                                let (slug, id) = (slug.clone(), id.clone());
                                move |_| session.write().remove_plugin_account(&slug, &id)
                            },
                            "Sign out"
                        }
                    }
                }
            } else {
                div { class: "mp-account-actions",
                    match state.action() {
                        AccountAction::SignIn => rsx! {
                            button {
                                class: "btn primary sm",
                                r#type: "button",
                                disabled: !can_start,
                                onclick: {
                                    let (slug, id) = (slug.clone(), id.clone());
                                    move |_| session.write().start_plugin_signin(slug.clone(), id.clone(), None, true)
                                },
                                "Sign in again"
                            }
                        },
                        AccountAction::Recheck => rsx! {
                            button {
                                class: "btn sm",
                                r#type: "button",
                                onclick: {
                                    let slug = slug.clone();
                                    move |_| session.write().recheck_plugin(&slug)
                                },
                                "Check again"
                            }
                        },
                        AccountAction::None => rsx! {},
                    }
                    button {
                        class: "btn ghost sm",
                        r#type: "button",
                        disabled: removing,
                        "aria-label": "Sign out of {title}",
                        onclick: {
                            let (slug, id) = (slug.clone(), id.clone());
                            move |_| session.write().ask_remove_plugin_account(&slug, &id)
                        },
                        if removing { "Signing out\u{2026}" } else { "Remove" }
                    }
                }
            }
        }
    }
}

/// The accounts of an OAuth plugin: the list, the buttons that add one, and
/// the sign-in panel while one runs.
#[component]
pub fn AccountsSection(session: Signal<Session>, detail: PluginDetail) -> Element {
    let mut session = session;
    let mut label = use_signal(String::new);
    let slug = detail.plugin.slug.clone();
    let (flow, elapsed, flow_running, confirm, removing, notice, error) = {
        let state = session.read();
        (
            state.mcp_ui.plugin_flow(&slug).cloned(),
            state.mcp_ui.flow_elapsed(),
            state.mcp_ui.flow_running(),
            state.mcp_ui.confirm_remove.clone(),
            state.mcp_ui.removing.clone(),
            state.mcp_ui.notice.clone(),
            state.mcp_ui.error.clone(),
        )
    };
    let can_start = !flow_running;
    let accounts: Vec<PluginAccount> = detail.accounts().to_vec();
    let first = detail
        .sign_in_offer()
        .filter(|_| accounts.is_empty())
        .map(|offer| (offer.label, offer.account_id, offer.renew));
    let add = detail
        .add_account_offer()
        .map(|offer| (offer.label, offer.account_id));
    let known: Vec<String> = accounts.iter().map(|a| a.account_id.clone()).collect();
    let label_ok = marketplace::valid_account_label(&label());
    let retry_account = session.read().mcp_ui.flow_account().map(str::to_owned);
    rsx! {
        div { class: "mp-accounts-section",
            h3 { class: "mp-section-title", "Accounts" }
            if accounts.is_empty() {
                p { class: "tiny", "No account is signed in. Sign in so your bots can use {detail.plugin.name}." }
            } else {
                ul { class: "mp-accounts", "aria-label": "Signed-in accounts",
                    for account in accounts {
                        AccountRow {
                            key: "{account.account_id}",
                            session,
                            slug: slug.clone(),
                            confirming: confirm
                                .as_ref()
                                .is_some_and(|(s, a)| s == &slug && a == &account.account_id),
                            removing: removing
                                .as_ref()
                                .is_some_and(|(s, a)| s == &slug && a == &account.account_id),
                            can_start,
                            account,
                        }
                    }
                }
            }
            if let Some(message) = notice {
                p { class: "tiny mp-notice", role: "status", "{message}" }
            }
            if let Some(message) = error {
                p { class: "tiny mp-problem", role: "alert", "{message}" }
            }
            if let Some(flow) = flow {
                McpFlowPanel {
                    flow,
                    elapsed,
                    can_start,
                    on_retry: {
                        let slug = slug.clone();
                        move |_| {
                            let id = retry_account
                                .clone()
                                .unwrap_or_else(|| marketplace::DEFAULT_ACCOUNT.to_owned());
                            let renew = known.contains(&id);
                            session.write().start_plugin_signin(slug.clone(), id, None, renew)
                        }
                    },
                    on_cancel: move |_| session.write().cancel_mcp_signin(),
                    on_dismiss: move |_| session.write().dismiss_mcp_flow(),
                    on_open: move |page: String| session.write().open_in_browser(page),
                }
            } else {
                if let Some((first_label, first_id, first_renew)) = first {
                    div { class: "row-actions",
                        button {
                            class: "btn primary",
                            r#type: "button",
                            disabled: !can_start,
                            onclick: {
                                let slug = slug.clone();
                                move |_| {
                                    session.write().start_plugin_signin(
                                        slug.clone(),
                                        first_id.clone(),
                                        None,
                                        first_renew,
                                    )
                                }
                            },
                            Icon { name: "external", size: 14 }
                            "{first_label}"
                        }
                        span { class: "tiny", "Opens your browser. Your password never reaches SeraBot." }
                    }
                }
                if let Some((add_label, add_id)) = add {
                    div { class: "mp-add-account",
                        input {
                            class: "input",
                            r#type: "text",
                            placeholder: "Label, for example Work (optional)",
                            "aria-label": "Account label",
                            "aria-invalid": if label_ok { "false" } else { "true" },
                            maxlength: "60",
                            value: "{label}",
                            oninput: move |event| label.set(event.value()),
                        }
                        button {
                            class: "btn sm",
                            r#type: "button",
                            disabled: !can_start || !label_ok,
                            onclick: {
                                let slug = slug.clone();
                                move |_| {
                                    let text = label.read().trim().to_owned();
                                    session.write().start_plugin_signin(
                                        slug.clone(),
                                        add_id.clone(),
                                        (!text.is_empty()).then_some(text),
                                        false,
                                    );
                                    label.set(String::new());
                                }
                            },
                            Icon { name: "plus", size: 14 }
                            "{add_label}"
                        }
                    }
                }
            }
        }
    }
}

/// One switch per tool: whether a call runs without asking first. A switch
/// moves at once; a refusal puts it back and says why.
#[component]
pub fn ToolsSection(session: Signal<Session>, slug: String) -> Element {
    let mut session = session;
    use_hook({
        let slug = slug.clone();
        move || session.write().load_plugin_tools(slug)
    });
    let view = session
        .read()
        .mcp_ui
        .tools
        .clone()
        .filter(|view| view.slug == slug);
    let Some(view) = view else {
        return rsx! {};
    };
    let free = view
        .rows
        .iter()
        .filter(|row| row.runs_without_asking())
        .count();
    let total = view.rows.len();
    let loading = view.state == ToolsState::Loading;
    rsx! {
        div { class: "mp-tools-section",
            h3 { class: "mp-section-title", "Tools" }
            p { class: "tiny",
                "Choose which tools your bots may run without asking you. Anything that changes data asks first unless you switch it on, and a tool that can delete always asks."
            }
            if let Some(message) = view.error.clone() {
                p { class: "tiny mp-problem", role: "alert", "{message}" }
            }
            match view.state.clone() {
                ToolsState::Failed(message) => rsx! {
                    div { class: "banner err", role: "alert",
                        span { class: "grow", "{message}" }
                        button {
                            class: "btn ghost sm",
                            r#type: "button",
                            onclick: {
                                let slug = slug.clone();
                                move |_| session.write().load_plugin_tools(slug.clone())
                            },
                            "Retry"
                        }
                    }
                },
                _ if total == 0 && loading => rsx! {
                    div { role: "status", "aria-busy": "true",
                        p { class: "tiny", "Loading tools\u{2026}" }
                        for index in 0..3 {
                            div { key: "{index}", class: "mp-tool skeleton", "aria-hidden": "true", " " }
                        }
                    }
                },
                _ if total == 0 => rsx! {
                    p { class: "tiny", "This plugin lists no tools." }
                },
                _ => rsx! {
                    p { class: "tiny", role: "status", "{free} of {total} run without asking." }
                    ul { class: "mp-tool-list", "aria-label": "Tools",
                        for row in view.rows.clone() {
                            li {
                                key: "{row.name}",
                                class: if row.locked() { "mp-tool is-locked" } else { "mp-tool" },
                                div { class: "mp-tool-main",
                                    span { class: "mp-tool-name mono", "{row.name}" }
                                    span { class: "mp-tool-note", "{row.note()}" }
                                    if !row.description.is_empty() {
                                        span { class: "mp-tool-desc", "{row.description}" }
                                    }
                                }
                                label { class: "mp-tool-switch",
                                    span { class: "tiny",
                                        if row.runs_without_asking() { "Runs freely" } else { "Asks first" }
                                    }
                                    input {
                                        class: "switch",
                                        r#type: "checkbox",
                                        role: "switch",
                                        "aria-label": "Run {row.name} without asking",
                                        checked: row.runs_without_asking(),
                                        disabled: row.locked() || loading,
                                        onchange: {
                                            let (slug, name) = (slug.clone(), row.name.clone());
                                            move |_| session.write().flip_plugin_tool(&slug, &name)
                                        },
                                    }
                                }
                            }
                        }
                    }
                },
            }
        }
    }
}

/// The GitHub card: sign in with a one-time code, see who is connected, and
/// disconnect. The pasted-token connector below it stays as the fallback.
#[component]
pub fn GithubCard(session: Signal<Session>) -> Element {
    let mut session = session;
    use_hook(move || session.write().load_github());
    let (view, flow, elapsed, running, busy, error, confirm, notice) = {
        let state = session.read();
        (
            state.mcp_ui.github.clone(),
            state.mcp_ui.github_flow().cloned(),
            state.mcp_ui.flow_elapsed(),
            state.mcp_ui.flow_running(),
            state.mcp_ui.github_busy,
            state.mcp_ui.github_error.clone(),
            state.mcp_ui.confirm_disconnect,
            state.mcp_ui.notice.clone(),
        )
    };
    let can_start = !running && !busy;
    rsx! {
        section { class: "github-card", "aria-label": "Sign in with GitHub",
            div { class: "github-card-head",
                h3 { class: "mp-section-title", "Sign in with GitHub" }
                if let Some(view) = view.clone() {
                    StatusChip { tone: github_tone(view.state()), text: view.badge().to_owned() }
                } else if busy {
                    span { class: "tiny", role: "status", "Checking\u{2026}" }
                }
            }
            if let Some(view) = view.clone() {
                p { class: "tiny", "{view.hint()}" }
                if let Some(login) = view.login_line() {
                    p { class: "github-login mono", "{login}" }
                }
            }
            if let Some(message) = error {
                div { class: "banner err", role: "alert",
                    span { class: "grow", "{message}" }
                    button {
                        class: "btn ghost sm",
                        r#type: "button",
                        onclick: move |_| session.write().load_github(),
                        "Retry"
                    }
                }
            }
            if let Some(flow) = flow {
                McpFlowPanel {
                    flow,
                    elapsed,
                    can_start,
                    on_retry: move |_| session.write().start_github_signin(),
                    on_cancel: move |_| session.write().cancel_mcp_signin(),
                    on_dismiss: move |_| session.write().dismiss_mcp_flow(),
                    on_open: move |page: String| session.write().open_in_browser(page),
                }
            } else if let Some(view) = view {
                if let Some(message) = notice {
                    p { class: "tiny mp-notice", role: "status", "{message}" }
                }
                if confirm {
                    div { class: "mp-confirm", role: "alertdialog", "aria-label": "Confirm disconnect",
                        p { "Disconnect GitHub? Your sign-in is deleted here, and GitHub's page for removing the app opens so you can finish there." }
                        div { class: "row-actions",
                            button {
                                class: "btn ghost sm",
                                r#type: "button",
                                autofocus: true,
                                onclick: move |_| session.write().cancel_disconnect_github(),
                                "Cancel"
                            }
                            button {
                                class: "btn danger sm",
                                r#type: "button",
                                disabled: busy,
                                onclick: move |_| session.write().disconnect_github(),
                                "Disconnect"
                            }
                        }
                    }
                } else {
                    div { class: "row-actions",
                        if view.can_sign_in() {
                            button {
                                class: "btn primary",
                                r#type: "button",
                                disabled: !can_start,
                                onclick: move |_| session.write().start_github_signin(),
                                Icon { name: "external", size: 14 }
                                if view.login_line().is_some() { "Sign in again" } else { "Get a code" }
                            }
                        }
                        if view.can_disconnect() {
                            button {
                                class: "btn ghost",
                                r#type: "button",
                                disabled: busy,
                                onclick: move |_| session.write().ask_disconnect_github(),
                                "Disconnect"
                            }
                        }
                    }
                }
            }
        }
    }
}
