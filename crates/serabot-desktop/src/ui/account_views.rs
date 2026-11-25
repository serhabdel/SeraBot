use super::*;
use crate::ui_widgets::StatusChip;
use serabot_desktop::provider_accounts::{self as accounts, AddMethod, Move, QuotaView};
use serabot_desktop::provider_flow::{self, FlowKind, FlowPhase};
use serabot_desktop::provider_presets::{ProviderPreset, provider_matches_preset};
use serabot_desktop::theme::Tone;
use serabot_desktop::{ProviderAccount, ProviderStatus};

/// Providers whose servers report a quota the page can read again.
const QUOTA_KINDS: [&str; 3] = ["muse-code", "google-antigravity", "opencode-go"];

fn preset_of(provider: &ProviderStatus) -> Option<&'static ProviderPreset> {
    provider_presets::presets()
        .iter()
        .find(|preset| preset.id == provider.id)
        .or_else(|| {
            provider_presets::presets()
                .iter()
                .find(|preset| provider_matches_preset(&provider.id, &provider.kind, preset))
        })
}

/// A thin quota bar. The width eases only while motion is allowed.
#[component]
fn QuotaBar(percent: u32, label: String, tone: Tone, resets: Option<String>) -> Element {
    let class = match tone {
        Tone::Danger => "meter-fill danger",
        Tone::Warn => "meter-fill warn",
        _ => "meter-fill",
    };
    rsx! {
        div { class: "acct-quota",
            div {
                class: "meter",
                role: "progressbar",
                "aria-label": "{label}",
                "aria-valuemin": "0",
                "aria-valuemax": "100",
                "aria-valuenow": "{percent}",
                div { class: "{class}", style: "width: {percent}%;" }
            }
            div { class: "acct-quota-text",
                span { "{label}" }
                if let Some(at) = resets {
                    span { "Resets {at}" }
                }
            }
        }
    }
}

/// The accounts of one provider: order, switch, status, quota, Test, Remove.
#[component]
pub(super) fn AccountsBlock(
    session: Signal<Session>,
    provider_id: String,
    overlay: Signal<Overlay>,
) -> Element {
    let mut overlay = overlay;
    let Some(provider) = session
        .read()
        .state
        .providers
        .iter()
        .find(|candidate| candidate.id == provider_id)
        .cloned()
    else {
        return rsx! {};
    };
    let preset = preset_of(&provider);
    let methods = accounts::add_methods(&provider, preset);
    let ordered: Vec<ProviderAccount> = accounts::ordered(&provider.accounts)
        .into_iter()
        .cloned()
        .collect();
    let last_index = ordered.len().saturating_sub(1);
    let (can_write, busy, confirm, force_prompt, tests) = {
        let s = session.read();
        (
            s.can_admin(),
            s.accounts_ui.busy.clone(),
            s.accounts_ui.confirm_remove.clone(),
            s.accounts_ui.force_prompt.clone(),
            s.state.account_tests.clone(),
        )
    };
    let signs_in_methods = accounts::first_sign_in_method(&methods).is_some();
    let takes_key = methods.contains(&AddMethod::Key);
    let add_label = accounts::add_button_label(&methods, !ordered.is_empty());
    let shared_key = ordered.is_empty() && provider.credential.configured;
    if methods.is_empty() && ordered.is_empty() {
        return rsx! {};
    }
    let add_enabled = can_write && accounts::can_add(&provider) && !methods.is_empty();
    let add_provider = provider.id.clone();
    let key_provider = provider.id.clone();
    let quota_reads = QUOTA_KINDS.contains(&provider.kind.as_str());
    rsx! {
        div { class: "acct-block",
            div { class: "acct-head",
                span { class: "micro",
                    if ordered.is_empty() { "Accounts" } else { "{accounts::summary_line(&provider.accounts)}" }
                }
                if !methods.is_empty() {
                    div { class: "row",
                        if signs_in_methods && takes_key {
                            button {
                                class: "btn ghost sm",
                                r#type: "button",
                                disabled: !add_enabled,
                                title: "Paste an API key instead of signing in",
                                onclick: move |_| {
                                    let mut s = session.write();
                                    s.open_add_account(&key_provider);
                                    s.set_add_method(AddMethod::Key);
                                    drop(s);
                                    overlay.set(Overlay::AddAccount);
                                },
                                "Use a key"
                            }
                        }
                        button {
                            class: if ordered.is_empty() { "btn primary sm" } else { "btn ghost sm" },
                            r#type: "button",
                            disabled: !add_enabled,
                            title: if !accounts::can_add(&provider) {
                                "This provider holds the most accounts it can"
                            } else if signs_in_methods {
                                "Open the provider's sign-in page in your browser"
                            } else {
                                "Add another account for this provider"
                            },
                            onclick: move |_| {
                                if signs_in_methods {
                                    session.write().sign_in_now(&add_provider);
                                } else {
                                    session.write().open_add_account(&add_provider);
                                }
                                overlay.set(Overlay::AddAccount);
                            },
                            Icon { name: if signs_in_methods { "external".to_owned() } else { "plus".to_owned() }, size: 14 }
                            "{add_label}"
                        }
                    }
                }
            }
            if shared_key {
                div { class: "acct-empty", role: "status",
                    if provider.credential.granted {
                        "Using the key your admin shares. Add your own account to use that first."
                    } else {
                        "Using a saved key. Add an account to manage keys here."
                    }
                }
            } else if ordered.is_empty() {
                div { class: "acct-empty", role: "status",
                    "{accounts::empty_card_text(&methods)}"
                }
            }
            if !ordered.is_empty() {
                ul { class: "acct-list", "aria-label": "Accounts, in the order bots try them",
                    for (index, account) in ordered.into_iter().enumerate() {
                        {
                            let state = accounts::state_of(&account);
                            let label = accounts::display_label(&account);
                            let identity = accounts::identity_line(&account);
                            let kind_label = accounts::auth_kind_label(&account.auth_kind);
                            let enabled = state != accounts::AccountState::Disabled;
                            let key = format!("{}/{}", provider.id, account.id);
                            let working = busy.as_deref() == Some(key.as_str());
                            let test = tests.get(&key).map(provider_view::format_provider_test);
                            let quota = account.quota.as_ref().and_then(accounts::quota_view);
                            let confirming = confirm.as_ref().is_some_and(|(p, a)| {
                                p == &provider.id && a == &account.id
                            });
                            let forcing = force_prompt.as_ref().filter(|(p, a, _)| {
                                p == &provider.id && a == &account.id
                            });
                            let row_class = if state == accounts::AccountState::NeedsReauth {
                                "acct-row needs-attention"
                            } else if !enabled {
                                "acct-row is-off"
                            } else if working {
                                "acct-row is-busy"
                            } else {
                                "acct-row"
                            };
                            let pid = provider.id.clone();
                            let aid = account.id.clone();
                            rsx! {
                                li { class: "{row_class}", key: "{account.id}",
                                    div { class: "acct-order",
                                        button {
                                            class: "btn icon ghost sm",
                                            r#type: "button",
                                            title: "Try this account earlier",
                                            "aria-label": "Move {label} up",
                                            disabled: !can_write || index == 0,
                                            onclick: {
                                                let (pid, aid) = (pid.clone(), aid.clone());
                                                move |_| session.write().move_account(&pid, &aid, Move::Up)
                                            },
                                            Icon { name: "chevron-up", size: 14 }
                                        }
                                        span { class: "acct-rank", "aria-hidden": "true", "{index + 1}" }
                                        button {
                                            class: "btn icon ghost sm",
                                            r#type: "button",
                                            title: "Try this account later",
                                            "aria-label": "Move {label} down",
                                            disabled: !can_write || index == last_index,
                                            onclick: {
                                                let (pid, aid) = (pid.clone(), aid.clone());
                                                move |_| session.write().move_account(&pid, &aid, Move::Down)
                                            },
                                            Icon { name: "chevron-down", size: 14 }
                                        }
                                    }
                                    div { class: "acct-main",
                                        div { class: "acct-name",
                                            span { class: "acct-label", title: "{label}", "{label}" }
                                            StatusChip { tone: state.tone(), text: state.label().to_owned() }
                                        }
                                        div { class: "acct-meta",
                                            span { "{kind_label}" }
                                            if let Some(identity) = identity {
                                                span { class: "chip muted", "{identity}" }
                                            }
                                            if let Some((tone, text)) = test {
                                                StatusChip { tone, text }
                                            }
                                        }
                                    }
                                    div { class: "acct-actions",
                                        input {
                                            class: "switch",
                                            r#type: "checkbox",
                                            checked: enabled,
                                            disabled: !can_write,
                                            title: if enabled { "On: bots may use this account" } else { "Off: bots skip this account" },
                                            "aria-label": "Use {label}",
                                            onchange: {
                                                let (pid, aid) = (pid.clone(), aid.clone());
                                                move |event| {
                                                    session.write().set_account_disabled(&pid, &aid, !event.checked())
                                                }
                                            },
                                        }
                                        button {
                                            class: "btn ghost sm",
                                            r#type: "button",
                                            title: "Ask the provider for its models with this account",
                                            disabled: !can_write || working,
                                            onclick: {
                                                let (pid, aid) = (pid.clone(), aid.clone());
                                                move |_| session.write().test_account(&pid, &aid)
                                            },
                                            Icon { name: "refresh", size: 14 }
                                            if working { "Checking\u{2026}" } else { "Test" }
                                        }
                                        if quota_reads {
                                            button {
                                                class: "btn icon ghost sm",
                                                r#type: "button",
                                                title: "Read the quota again",
                                                "aria-label": "Refresh quota for {label}",
                                                disabled: !can_write || working,
                                                onclick: {
                                                    let (pid, aid) = (pid.clone(), aid.clone());
                                                    move |_| session.write().refresh_account_quota(&pid, &aid)
                                                },
                                                Icon { name: "usage", size: 14 }
                                            }
                                        }
                                        if signs_in_methods && account.auth_kind != "api_key" {
                                            button {
                                                class: if state == accounts::AccountState::NeedsReauth { "btn primary sm" } else { "btn ghost sm" },
                                                r#type: "button",
                                                title: "Sign in to this provider again",
                                                "aria-label": "Sign in again as {label}",
                                                disabled: !can_write,
                                                onclick: {
                                                    let pid = pid.clone();
                                                    move |_| {
                                                        session.write().sign_in_now(&pid);
                                                        overlay.set(Overlay::AddAccount);
                                                    }
                                                },
                                                "Sign in again"
                                            }
                                        }
                                        button {
                                            class: "btn ghost sm",
                                            r#type: "button",
                                            "aria-label": "Remove {label}",
                                            disabled: !can_write,
                                            onclick: {
                                                let (pid, aid) = (pid.clone(), aid.clone());
                                                move |_| session.write().ask_remove_account(&pid, &aid)
                                            },
                                            Icon { name: "trash", size: 14 }
                                            "Remove"
                                        }
                                    }
                                    match quota {
                                        Some(QuotaView::Meter(meter)) => rsx! {
                                            QuotaBar {
                                                percent: meter.percent_used,
                                                label: meter.label,
                                                tone: meter.tone,
                                                resets: meter.resets_at,
                                            }
                                        },
                                        Some(QuotaView::Note(note)) => rsx! {
                                            p { class: "acct-hint", role: "status", "{note}" }
                                        },
                                        None => rsx! {},
                                    }
                                    if let Some(hint) = state.hint() {
                                        if !confirming {
                                            p { class: "acct-hint", "{hint}" }
                                        }
                                    }
                                    if confirming {
                                        div { class: "mp-confirm acct-confirm", role: "alertdialog", "aria-label": "Confirm removing {label}",
                                            span { class: "tiny", "Remove {label}? Its key or sign-in is deleted from your account." }
                                            button {
                                                class: "btn danger sm",
                                                r#type: "button",
                                                autofocus: true,
                                                disabled: !can_write,
                                                onclick: {
                                                    let (pid, aid) = (pid.clone(), aid.clone());
                                                    move |_| session.write().remove_account(&pid, &aid, false)
                                                },
                                                "Remove"
                                            }
                                            button {
                                                class: "btn ghost sm",
                                                r#type: "button",
                                                onclick: move |_| session.write().cancel_remove_account(),
                                                "Cancel"
                                            }
                                        }
                                    }
                                    if let Some((_, _, message)) = forcing {
                                        div { class: "mp-confirm acct-confirm", role: "alertdialog", "aria-label": "Bots use this account",
                                            span { class: "tiny", "{message}" }
                                            button {
                                                class: "btn danger sm",
                                                r#type: "button",
                                                disabled: !can_write,
                                                onclick: {
                                                    let (pid, aid) = (pid.clone(), aid.clone());
                                                    move |_| session.write().remove_account(&pid, &aid, true)
                                                },
                                                "Remove anyway"
                                            }
                                            button {
                                                class: "btn ghost sm",
                                                r#type: "button",
                                                onclick: move |_| session.write().cancel_remove_account(),
                                                "Keep it"
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

/// The "Add account" sheet: a key, a browser sign-in or a device code.
#[component]
pub(super) fn AddAccountSheet(session: Signal<Session>, overlay: Signal<Overlay>) -> Element {
    let mut overlay = overlay;
    let mut show_key = use_signal(|| false);
    // The session closes the sheet (a key was added): follow it.
    use_effect(move || {
        if session.read().accounts_ui.sheet_provider.is_none() {
            overlay.set(Overlay::None);
        }
    });
    let provider_id = session.read().accounts_ui.sheet_provider.clone();
    let Some(provider_id) = provider_id else {
        return rsx! {};
    };
    let Some(provider) = session
        .read()
        .state
        .providers
        .iter()
        .find(|candidate| candidate.id == provider_id)
        .cloned()
    else {
        return rsx! {};
    };
    let preset = preset_of(&provider);
    let methods = accounts::add_methods(&provider, preset);
    let (method, label, key, error, flow, elapsed, can_write, adding) = {
        let s = session.read();
        (
            s.accounts_ui.method,
            s.accounts_ui.label.clone(),
            s.accounts_ui.key.clone(),
            s.accounts_ui.sheet_error.clone(),
            s.accounts_ui.flow.clone(),
            s.accounts_ui.flow_elapsed(),
            s.can_admin(),
            s.accounts_ui
                .busy
                .as_deref()
                .is_some_and(|key| key.ends_with("/new")),
        )
    };
    let title = provider_view::display_name(
        &provider.id,
        &provider.kind,
        branding::brand_label(&provider.kind),
    );
    let risk = accounts::risk_text(&provider, preset);
    let key_url = preset.map(|p| p.key_url).filter(|url| !url.is_empty());
    let key_revealed = show_key() && !key.is_empty();
    let running = flow.as_ref().is_some_and(|f| !f.phase.is_final());
    let kind = provider.kind.clone();
    let pid = provider.id.clone();
    let close = move |_| {
        session.write().close_add_account();
        overlay.set(Overlay::None);
    };
    rsx! {
        div { class: "scrim open", onclick: close }
        div {
            class: "sheet open",
            role: "dialog",
            "aria-modal": "true",
            "aria-label": "Add account to {title}",
            tabindex: "0",
            onkeydown: move |event: KeyboardEvent| {
                if event.key() == Key::Escape {
                    event.prevent_default();
                    session.write().close_add_account();
                    overlay.set(Overlay::None);
                }
            },
            div { class: "sheet-head",
                h2 { class: "sheet-title", "Add account to {title}" }
                button {
                    class: "icon-btn escape-close",
                    r#type: "button",
                    title: "Close",
                    "aria-label": "Close",
                    autofocus: true,
                    onclick: close,
                    Icon { name: "x", size: 16 }
                }
            }
            div { class: "sheet-body",
                if let Some(text) = risk {
                    div { class: "banner warn risk-banner", role: "note",
                        span { "{text}" }
                    }
                }
                if methods.len() > 1 {
                    div { class: "seg method-seg", role: "group", "aria-label": "How to add the account",
                        for choice in methods.clone() {
                            button {
                                key: "{choice.label()}",
                                r#type: "button",
                                class: if method == Some(choice) { "seg-btn active" } else { "seg-btn" },
                                "aria-pressed": if method == Some(choice) { "true" } else { "false" },
                                onclick: move |_| session.write().set_add_method(choice),
                                "{choice.label()}"
                            }
                        }
                    }
                }
                if let Some(message) = error {
                    div { class: "banner err", role: "alert",
                        span { class: "grow", "{message}" }
                    }
                }
                match method {
                    Some(AddMethod::Key) => rsx! {
                        div { class: "form-grid",
                            div { class: "field",
                                label { class: "field-label", r#for: "account-label-input", "Label" }
                                input {
                                    id: "account-label-input",
                                    class: "input",
                                    value: "{label}",
                                    maxlength: "64",
                                    placeholder: "Work, Personal, Spare\u{2026}",
                                    oninput: move |event| session.write().accounts_ui.label = event.value(),
                                }
                                span { class: "hint", "{Session::account_label_hint()}" }
                            }
                            div { class: "field",
                                label { class: "field-label", r#for: "account-key-input", "API key" }
                                div { class: "secret-field",
                                    input {
                                        id: "account-key-input",
                                        class: "input",
                                        r#type: if key_revealed { "text" } else { "password" },
                                        autocomplete: "off",
                                        spellcheck: false,
                                        value: "{key}",
                                        placeholder: "Paste the key",
                                        oninput: move |event| {
                                            let next = event.value();
                                            if next.is_empty() {
                                                show_key.set(false);
                                            }
                                            session.write().accounts_ui.key = next;
                                        },
                                        onkeydown: move |event: KeyboardEvent| {
                                            if event.key() == Key::Enter {
                                                event.prevent_default();
                                                session.write().add_account_with_key();
                                            }
                                        },
                                    }
                                    button {
                                        class: "btn icon ghost sm secret-toggle",
                                        r#type: "button",
                                        title: if key_revealed { "Hide key" } else { "Show key" },
                                        "aria-label": if key_revealed { "Hide API key" } else { "Show API key" },
                                        "aria-pressed": if key_revealed { "true" } else { "false" },
                                        onclick: move |_| show_key.set(!show_key()),
                                        Icon { name: if key_revealed { "eye-off".to_owned() } else { "eye".to_owned() }, size: 14 }
                                    }
                                }
                                span { class: "hint", "Saved to your account only, encrypted. It is never shown again." }
                                if let Some(url) = key_url {
                                    button {
                                        class: "btn ghost sm",
                                        r#type: "button",
                                        onclick: move |_| session.write().open_in_browser(url.to_owned()),
                                        Icon { name: "external", size: 14 }
                                        "Get a key"
                                    }
                                }
                            }
                        }
                    },
                    Some(choice @ (AddMethod::Browser | AddMethod::Device)) => rsx! {
                        FlowPanel {
                            session,
                            flow: flow.clone(),
                            elapsed,
                            method: choice,
                            provider: pid.clone(),
                            kind: kind.clone(),
                            title: title.clone(),
                            can_write,
                        }
                    },
                    None => rsx! {
                        div { class: "empty compact",
                            p { class: "empty-text", "This provider has no way to add an account here." }
                        }
                    },
                }
            }
            div { class: "sheet-foot",
                button {
                    class: "btn ghost",
                    r#type: "button",
                    onclick: close,
                    if flow.as_ref().is_some_and(|f| f.phase.is_final()) { "Done" } else { "Cancel" }
                }
                if method == Some(AddMethod::Key) {
                    button {
                        class: "btn primary",
                        r#type: "button",
                        disabled: !can_write || adding || key.trim().is_empty(),
                        onclick: move |_| session.write().add_account_with_key(),
                        if adding { "Adding..." } else { "Add account" }
                    }
                }
                if running {
                    span { class: "tiny", role: "status", "Signing in\u{2026} you can cancel at any time." }
                }
            }
        }
    }
}

/// The browser or device sign-in: start button, the code, the countdown, and
/// a clear end state. Cancelling is always one press away.
#[component]
fn FlowPanel(
    session: Signal<Session>,
    flow: Option<provider_flow::ProviderFlow>,
    elapsed: std::time::Duration,
    method: AddMethod,
    provider: String,
    kind: String,
    title: String,
    can_write: bool,
) -> Element {
    let mut pasted_callback = use_signal(String::new);
    let mut paste_error = use_signal(|| Option::<String>::None);
    let device = method == AddMethod::Device;
    let is_claude_pkce = !device && kind == "anthropic";
    let is_browser_device = device && kind == "mistral";
    let Some(flow) = flow else {
        return rsx! {
            div { class: "flow-panel",
                p { class: "flow-title",
                    if device && !is_browser_device { "Sign in with a one-time code" } else { "Sign in with your browser" }
                }
                p { class: "muted",
                    if device && !is_browser_device {
                        "You get a short code to type on {title}'s own page. This window updates when it is approved."
                    } else if is_claude_pkce {
                        "Your browser opens {title}'s sign-in page (port 54545). Sign in there, or paste the callback URL or code#state if the redirect does not complete."
                    } else {
                        "Your browser opens {title}'s sign-in page. Sign in there, then come back."
                    }
                }
                div { class: "route-actions",
                    button {
                        class: "btn primary",
                        r#type: "button",
                        disabled: !can_write,
                        autofocus: true,
                        onclick: move |_| {
                            session.write().start_provider_signin(provider.clone(), kind.clone(), method)
                        },
                        if device && !is_browser_device { "Get a code" } else { "Open browser" }
                    }
                }
            }
        };
    };
    let waiting = flow.phase == FlowPhase::Waiting;
    let starting = flow.phase == FlowPhase::Starting;
    let remaining = flow.remaining_secs(elapsed);
    let percent = flow.elapsed_percent(elapsed);
    let ok = matches!(flow.phase, FlowPhase::Done { .. });
    let bad = matches!(
        flow.phase,
        FlowPhase::Denied(_) | FlowPhase::Failed(_) | FlowPhase::Expired
    );
    let panel_class = if ok {
        "flow-panel is-ok"
    } else if bad {
        "flow-panel is-bad"
    } else {
        "flow-panel"
    };
    let headline = flow.headline();
    let detail = flow.detail();
    let url = flow.url.clone();
    let code = flow.user_code.clone();
    let device_flow = flow.kind == FlowKind::Device;
    let retry_provider = provider.clone();
    let retry_kind = kind.clone();
    let submit_pasted = Callback::new(move |()| {
        let raw = pasted_callback();
        let Some((cb_code, cb_state)) = accounts::parse_claude_callback(&raw) else {
            paste_error.set(Some(
                "Paste the full callback URL (with code and state) or code#state.".to_owned(),
            ));
            return;
        };
        paste_error.set(None);
        let path = accounts::claude_callback_path(&cb_code, &cb_state);
        std::thread::spawn(move || {
            use std::io::Write as _;
            let addr = std::net::SocketAddr::from(([127, 0, 0, 1], 54545));
            let stream =
                std::net::TcpStream::connect_timeout(&addr, std::time::Duration::from_secs(2))
                    .or_else(|_| {
                        let v6 = std::net::SocketAddr::from(([0, 0, 0, 0, 0, 0, 0, 1], 54545));
                        std::net::TcpStream::connect_timeout(&v6, std::time::Duration::from_secs(2))
                    });
            if let Ok(mut conn) = stream {
                let req = format!(
                    "GET {path} HTTP/1.1\r\nHost: localhost:54545\r\nConnection: close\r\n\r\n"
                );
                let _ = conn.write_all(req.as_bytes());
            }
        });
    });
    rsx! {
        div { class: "{panel_class}", "aria-live": "polite",
            p { class: "flow-title", "{headline}" }
            if starting {
                span { class: "flow-wait", role: "status",
                    span { class: "flow-wait-dot", "aria-hidden": "true" }
                    "One moment"
                }
            }
            if let Some(code) = code.clone() {
                if waiting {
                    div { class: "device-code", "aria-label": "One-time code", "{code}" }
                }
            }
            if let Some(text) = detail {
                p { class: "muted", "{text}" }
            }
            if waiting {
                if device_flow {
                    div { class: "flow-count",
                        span { "{provider_flow::countdown_text(remaining)} left" }
                        div { class: "flow-bar", "aria-hidden": "true",
                            div { class: "flow-bar-fill", style: "width: {percent}%;" }
                        }
                    }
                    span { class: "flow-wait", role: "status",
                        span { class: "flow-wait-dot", "aria-hidden": "true" }
                        "Waiting for approval\u{2026}"
                    }
                }
                div { class: "route-actions",
                    if let Some(page) = url.clone() {
                        button {
                            class: "btn primary sm",
                            r#type: "button",
                            onclick: {
                                let page = page.clone();
                                move |_| session.write().open_in_browser(page.clone())
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
                        onclick: move |_| session.write().cancel_provider_signin(),
                        "Cancel sign-in"
                    }
                }
                if !device_flow || flow.user_code.is_none() {
                    if let Some(page) = url {
                        p { class: "flow-link", "{page}" }
                    }
                }
                if !device_flow {
                    if is_claude_pkce {
                        div { class: "field",
                            label { class: "field-label", r#for: "claude-callback-input",
                                "Paste callback URL or code#state"
                            }
                            p { class: "hint",
                                "In case port 54545 is blocked or your browser is on another machine, paste the callback URL or code#state from Claude here:"
                            }
                            div { class: "row",
                                input {
                                    id: "claude-callback-input",
                                    class: "input grow",
                                    autocomplete: "off",
                                    spellcheck: false,
                                    value: "{pasted_callback}",
                                    placeholder: "http://localhost:54545/callback?code=...&state=... or code#state",
                                    oninput: move |event| {
                                        pasted_callback.set(event.value());
                                        paste_error.set(None);
                                    },
                                    onkeydown: move |event: KeyboardEvent| {
                                        if event.key() == Key::Enter {
                                            event.prevent_default();
                                            submit_pasted.call(());
                                        }
                                    },
                                }
                                button {
                                    class: "btn primary sm",
                                    r#type: "button",
                                    disabled: pasted_callback().trim().is_empty(),
                                    onclick: move |_| submit_pasted.call(()),
                                    "Complete sign-in"
                                }
                            }
                            if let Some(err) = paste_error() {
                                p { class: "field-error", role: "alert", "{err}" }
                            }
                        }
                    }
                }
            }
            if bad || flow.phase == FlowPhase::Cancelled {
                if is_claude_pkce && bad {
                    p { class: "hint",
                        "Claude redirects to localhost:54545/callback. If port 54545 is blocked, free that port or use an Anthropic API key instead."
                    }
                }
                div { class: "route-actions",
                    button {
                        class: "btn primary sm",
                        r#type: "button",
                        disabled: !can_write,
                        onclick: move |_| {
                            session.write().start_provider_signin(
                                retry_provider.clone(),
                                retry_kind.clone(),
                                method,
                            )
                        },
                        "Try again"
                    }
                }
            }
        }
    }
}
