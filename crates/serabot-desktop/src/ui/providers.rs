use super::*;
use crate::ui_widgets::StatusChip;
use serabot_desktop::ProviderStatus;
use serabot_desktop::provider_accounts::{self as accounts, SigninBlocker};
use serabot_desktop::provider_presets::{ProviderPreset, provider_matches_preset};

/// The "Add provider" sheet.
///
/// A preset fills the id, base URL, kind and secret name, and offers its
/// model ids as a starting list. The owner pastes the API key; the write
/// goes through the normal admin path (`Session::save_provider`).
#[component]
pub(super) fn AddProviderSheet(session: Signal<Session>, overlay: Signal<Overlay>) -> Element {
    let mut overlay = overlay;
    let initial_preset = {
        let draft = &session.read().provider;
        provider_presets::presets()
            .iter()
            .find(|p| !draft.kind.is_empty() && provider_matches_preset(&draft.id, &draft.kind, p))
            .map(|p| p.id.to_owned())
            .unwrap_or_default()
    };
    let mut selected = use_signal(move || initial_preset);
    let mut show_key = use_signal(|| false);
    let mut submit_attempted = use_signal(|| false);
    let choices = provider_presets::presets()
        .iter()
        .map(|preset| (preset.id, preset.label))
        .collect::<Vec<_>>();
    let chosen = provider_presets::preset_for(&selected()).copied();
    let (
        id,
        kind,
        base_url,
        models,
        secret_name,
        api_key,
        catalog_fetch,
        can_save,
        admin_error,
        existing_ids,
        will_sign_in,
        oauth_client_secret,
    ) = {
        let s = session.read();
        (
            s.provider.id.clone(),
            s.provider.kind.clone(),
            s.provider.base_url.clone(),
            s.provider.models.clone(),
            s.provider.secret_name.clone(),
            s.provider.api_key.clone(),
            s.provider.catalog_fetch,
            s.can_admin(),
            s.admin_error.clone(),
            s.state
                .providers
                .iter()
                .map(|p| p.id.clone())
                .collect::<Vec<_>>(),
            s.draft_signs_in(
                s.provider.id.trim(),
                s.provider.kind.trim(),
                s.provider.api_key.trim().is_empty(),
            ),
            s.provider.oauth_client_secret.clone(),
        )
    };
    let existing_refs: Vec<&str> = existing_ids.iter().map(String::as_str).collect();
    let check = provider_view::validate_provider_draft(
        &id,
        &kind,
        &base_url,
        &models,
        &secret_name,
        &existing_refs,
    );
    let show_errors = submit_attempted();
    let show_id_err = (show_errors || !id.trim().is_empty()) && check.id_error.is_some();
    let show_url_err =
        (show_errors || !base_url.trim().is_empty()) && check.base_url_error.is_some();
    let show_models_err =
        (show_errors || !models.trim().is_empty()) && check.models_error.is_some();
    let show_secret_err =
        (show_errors || !secret_name.trim().is_empty()) && check.secret_error.is_some();
    let key_revealed = show_key() && !api_key.is_empty();
    let hide_technical = chosen.is_some_and(|p| p.id != "custom");
    rsx! {
        div {
            class: "scrim open",
            onclick: move |_| overlay.set(Overlay::None),
        }
        div {
            class: "sheet open",
            role: "dialog",
            "aria-modal": "true",
            "aria-label": "Add or configure a provider",
            tabindex: "0",
            onkeydown: move |event: KeyboardEvent| {
                if event.key() == Key::Escape {
                    event.prevent_default();
                    overlay.set(Overlay::None);
                }
            },
            div { class: "sheet-head",
                h2 { class: "sheet-title", if existing_ids.iter().any(|existing| existing == id.trim()) { "Configure provider" } else { "Add provider" } }
                button {
                    class: "icon-btn escape-close",
                    r#type: "button",
                    title: "Close",
                    "aria-label": "Close",
                    autofocus: true,
                    onclick: move |_| overlay.set(Overlay::None),
                    Icon { name: "x", size: 16 }
                }
            }
            div { class: "sheet-body",
                if let Some(message) = admin_error {
                    div { class: "banner err", role: "alert",
                        span { class: "grow", "{message}" }
                        button {
                            class: "btn icon ghost sm banner-close",
                            r#type: "button",
                            title: "Dismiss",
                            "aria-label": "Dismiss error",
                            onclick: move |_| session.write().admin_error = None,
                            Icon { name: "x", size: 13 }
                        }
                    }
                }
                div { class: "field",
                    label { class: "field-label", r#for: "provider-preset-select", "Preset" }
                    select {
                        id: "provider-preset-select",
                        class: "select",
                        value: "{selected()}",
                        onchange: move |event| {
                            let id = event.value();
                            if id.is_empty() {
                                selected.set(String::new());
                                return;
                            }
                            let Some(preset) = provider_presets::preset_for(&id).copied() else {
                                return;
                            };
                            let mut session = session;
                            let mut draft = session.write();
                            draft.provider.id = if preset.default_id.is_empty() {
                                String::new()
                            } else {
                                preset.default_id.to_owned()
                            };
                            draft.provider.kind = preset.kind.to_owned();
                            draft.provider.base_url = preset.base_url.to_owned();
                            draft.provider.secret_name = preset.secret_name.to_owned();
                            draft.provider.models = preset.models.join(", ");
                            draft.provider.catalog_fetch = preset.catalog_fetch;
                            draft.provider.auth = preset.auth.to_owned();
                            // The key belongs to one provider; a new preset starts without one.
                            draft.provider.api_key.clear();
                            draft.provider.oauth_client_secret.clear();
                            draft.admin_error = None;
                            drop(draft);
                            show_key.set(false);
                            submit_attempted.set(false);
                            selected.set(preset.id.to_owned());
                        },
                        option { value: "", "Choose a preset" }
                        for (preset_id, preset_label) in choices {
                            option { value: "{preset_id}", "{preset_label}" }
                        }
                    }
                }
                if let Some(preset) = chosen {
                    div { class: "card",
                        div { class: "card-head",
                            div { class: "provider-card-head",
                                BrandLogo { brand: preset.id.to_owned(), size: 28 }
                                div { class: "provider-card-title",
                                    h3 { class: "card-title", "{preset.label}" }
                                    span { class: "card-sub provider-card-sub", "{preset.subtitle}" }
                                }
                            }
                            span { class: "chip accent", "{preset.badge}" }
                        }
                        if preset.auth == "oauth" {
                            div { class: "banner info",
                                span {
                                    if preset.sign_in == "device" {
                                        "Sign-in only. Press Save and sign in: a code appears here to type on the provider's page. You can sign in with more than one account."
                                    } else {
                                        "Sign-in only. Press Save and sign in: your browser opens the provider's sign-in page and the account appears here once you approve. You can sign in with more than one account."
                                    }
                                }
                            }
                        } else if preset.sign_in == "pkce" {
                            div { class: "banner info",
                                span {
                                    "Paste an API key below, or leave it empty and press Save and sign in to sign in with your browser."
                                }
                            }
                        } else if preset.auth == "none" {
                            div { class: "banner info",
                                span {
                                    "Local server. Needs no key or sign-in; change the address below if it runs on another machine."
                                }
                            }
                        }
                        div { class: "chips",
                            for model in preset.models {
                                span { class: "chip", "{model}" }
                            }
                            if preset.models.is_empty() {
                                span { class: "chip warn", "no model list" }
                            }
                        }
                    }
                }
                div { class: "form-grid",
                    div { class: "field",
                        label { class: "field-label", r#for: "provider-id-input", "Provider id" }
                        input {
                            id: "provider-id-input",
                            class: if show_id_err { "input invalid" } else { "input" },
                            value: "{id}",
                            placeholder: "provider id (for example work)",
                            "aria-invalid": if show_id_err { "true" } else { "false" },
                            oninput: move |event| session.write().provider.id = event.value(),
                        }
                        if show_id_err {
                            if let Some(msg) = check.id_error {
                                p { class: "field-error", role: "alert", "{msg}" }
                            }
                        } else if check.duplicate_id_warning {
                            p { class: "tiny", role: "status", "Updates the existing \"{id.trim()}\" provider." }
                        }
                    }
                    div { class: "field",
                        label { class: "field-label", r#for: "provider-url-input", "Base URL" }
                        input {
                            id: "provider-url-input",
                            class: if show_url_err { "input invalid" } else { "input" },
                            value: "{base_url}",
                            placeholder: "https://api.example.com/v1",
                            "aria-invalid": if show_url_err { "true" } else { "false" },
                            oninput: move |event| session.write().provider.base_url = event.value(),
                        }
                        if show_url_err {
                            if let Some(msg) = check.base_url_error {
                                p { class: "field-error", role: "alert", "{msg}" }
                            }
                        }
                    }
                    div { class: "field",
                        label { class: "field-label", r#for: "provider-models-input", "Models" }
                        input {
                            id: "provider-models-input",
                            class: if show_models_err { "input invalid" } else { "input" },
                            value: "{models}",
                            placeholder: "model ids, comma separated",
                            "aria-invalid": if show_models_err { "true" } else { "false" },
                            oninput: move |event| session.write().provider.models = event.value(),
                        }
                        if show_models_err {
                            if let Some(msg) = check.models_error {
                                p { class: "field-error", role: "alert", "{msg}" }
                            }
                        } else {
                            span { class: "hint", "Comma-separated model ids (at least one)." }
                        }
                    }
                    if chosen.is_some_and(|preset| preset.auth == "oauth") {
                        div { class: "field",
                            span { class: "field-label", "Sign-in" }
                            div { class: "banner info",
                                span {
                                    "Each account's tokens are encrypted for you alone."
                                }
                            }
                        }
                    } else if chosen.is_none_or(|preset| preset.auth != "none") {
                        div { class: "field",
                            label { class: "field-label", r#for: "provider-api-key-input", "API key" }
                            div { class: "secret-field",
                                input {
                                    id: "provider-api-key-input",
                                    class: "input",
                                    r#type: if key_revealed { "text" } else { "password" },
                                    autocomplete: "off",
                                    spellcheck: false,
                                    value: "{api_key}",
                                    placeholder: "API key (leave empty if already stored)",
                                    oninput: move |event| {
                                        let next = event.value();
                                        if next.is_empty() {
                                            show_key.set(false);
                                        }
                                        session.write().provider.api_key = next;
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
                            if let Some(url) = chosen.map(|preset| preset.key_url).filter(|url| !url.is_empty()) {
                                button {
                                    class: "btn ghost sm",
                                    r#type: "button",
                                    title: "Open the provider's site to create a key",
                                    onclick: move |_| session.write().open_in_browser(url.to_owned()),
                                    Icon { name: "external", size: 14 }
                                    "Get a key"
                                }
                            }
                        }
                    }
                }
                details { class: "advanced", open: !hide_technical || show_secret_err,
                    summary { "Advanced settings" }
                    div { class: "advanced-body form-grid",
                        div { class: "field",
                            label { class: "field-label", r#for: "provider-kind-input", "Kind" }
                            select {
                                id: "provider-kind-input",
                                class: "select",
                                value: "{kind}",
                                onchange: move |event| session.write().provider.kind = event.value(),
                                for item in PROVIDER_KINDS {
                                    option { value: "{item}", "{item}" }
                                }
                            }
                        }
                        if chosen.is_some_and(|preset| preset.kind == "google-antigravity") {
                            div { class: "field",
                                label { class: "field-label", r#for: "provider-client-secret-input", "OAuth client secret (optional override)" }
                                input {
                                    id: "provider-client-secret-input",
                                    class: "input",
                                    r#type: "password",
                                    autocomplete: "off",
                                    spellcheck: false,
                                    value: "{oauth_client_secret}",
                                    placeholder: "Leave empty to use the built-in Google client",
                                    oninput: move |event| session.write().provider.oauth_client_secret = event.value(),
                                }
                                span { class: "hint",
                                    "Sign-in works without this. Paste a secret only to replace the built-in Google client; it is stored encrypted on the server and never shown again."
                                }
                            }
                        }
                        if chosen.is_none_or(|preset| preset.auth != "oauth") {
                            div { class: "field",
                                label { class: "field-label", r#for: "provider-secret-input", "Secret name" }
                                input {
                                    id: "provider-secret-input",
                                    class: if show_secret_err { "input invalid" } else { "input" },
                                    value: "{secret_name}",
                                    placeholder: "secret name (for example WORK_API_KEY)",
                                    "aria-invalid": if show_secret_err { "true" } else { "false" },
                                    oninput: move |event| session.write().provider.secret_name = event.value(),
                                }
                                if show_secret_err {
                                    if let Some(msg) = check.secret_error {
                                        p { class: "field-error", role: "alert", "{msg}" }
                                    }
                                }
                            }
                        }
                    }
                }
                label { class: "check",
                    input {
                        "type": "checkbox",
                        checked: catalog_fetch,
                        onchange: move |event| session.write().provider.catalog_fetch = event.checked(),
                    }
                    span { "Fetch the live model list (/models)" }
                }
            }
            div { class: "sheet-foot",
                button {
                    class: "btn ghost",
                    r#type: "button",
                    onclick: move |_| overlay.set(Overlay::None),
                    "Cancel"
                }
                button {
                    class: "btn primary",
                    r#type: "button",
                    disabled: !can_save,
                    onclick: move |_| {
                        submit_attempted.set(true);
                        if !check.is_valid() {
                            return;
                        }
                        session.write().save_provider();
                        if session.read().admin_error.is_none() {
                            overlay.set(Overlay::None);
                        }
                    },
                    if will_sign_in { "Save and sign in" } else { "Save provider" }
                }
            }
        }
    }
}

/// Whether a provider works for the signed-in member right now.
fn usable(provider: &ProviderStatus) -> bool {
    provider_view::provider_usable(
        provider,
        provider_signed_in(&provider.kind, &provider.secrets),
    )
}

/// Open the Add provider sheet on a fresh draft (a custom endpoint).
fn open_custom(mut session: Signal<Session>, mut overlay: Signal<Overlay>) {
    let mut s = session.write();
    s.provider = Default::default();
    s.admin_error = None;
    drop(s);
    overlay.set(Overlay::AddProvider);
}

/// The AI providers page: the providers this server offers (each with the
/// member's own accounts), then, for the admin, the providers that can still
/// be added: sign-in first, then API keys.
#[component]
pub(super) fn ProvidersView(session: Signal<Session>, overlay: Signal<Overlay>) -> Element {
    let mut query = use_signal(String::new);
    let (providers, loaded, can_write, is_admin) = {
        let s = session.read();
        (
            s.state.providers.clone(),
            s.state.providers_loaded,
            s.can_admin(),
            s.is_admin(),
        )
    };
    let order = provider_view::card_order(&providers, usable);
    let usable_count = providers.iter().filter(|provider| usable(provider)).count();
    let (sign_in_presets, key_presets) = provider_view::presets_to_add(&providers, &query());
    let searching = !query().trim().is_empty();
    let nothing_to_add = sign_in_presets.is_empty() && key_presets.is_empty();
    rsx! {
        div { class: "pane providers-page",
            div { class: "pane-head providers-head",
                div { class: "providers-intro",
                    h1 { class: "pane-title", "AI providers" }
                    p { class: "muted",
                        "Connect the models your bots use. Sign in with a plan you already have, or add an API key. Every account is yours alone and stays encrypted."
                    }
                }
                div { class: "row providers-head-actions",
                    button {
                        class: "btn ghost",
                        r#type: "button",
                        title: "Ask every provider for its current model list",
                        disabled: !can_write,
                        // Reloads the provider list once the catalogs are in.
                        onclick: move |_| session.write().fetch_models(),
                        Icon { name: "refresh", size: 15 }
                        "Refresh models"
                    }
                    if is_admin {
                        button {
                            class: "btn primary",
                            r#type: "button",
                            onclick: move |_| open_custom(session, overlay),
                            Icon { name: "plus", size: 15 }
                            "Custom provider"
                        }
                    }
                }
            }
            div { class: "pane-body providers-body",
                section { class: "prov-default", "aria-label": "Default model",
                    div { class: "prov-default-copy",
                        span { class: "prov-eyebrow", "Default for new chats" }
                        span { class: "tiny", "Applies from the next message. A running reply keeps the route it started with." }
                    }
                    div { class: "prov-default-picker", ModelBar { session } }
                }
                BotRoutingSection { session }
                if !loaded {
                    div { class: "prov-grid", role: "status", "aria-busy": "true",
                        div { class: "prov-card skeleton-card",
                            div { class: "skeleton title" }
                            div { class: "skeleton block" }
                        }
                        div { class: "prov-card skeleton-card",
                            div { class: "skeleton title" }
                            div { class: "skeleton block" }
                        }
                    }
                } else if providers.is_empty() {
                    div { class: "empty prov-empty",
                        div { class: "empty-icon", Icon { name: "providers", size: 20 } }
                        h2 { class: "empty-title", "No AI providers yet" }
                        p { class: "empty-text",
                            if is_admin {
                                "Sign in with Claude, ChatGPT, Google Antigravity or GitHub Copilot below, or add an API key, so your bots can answer."
                            } else {
                                "Your admin has not added a provider yet. Ask them to add one, then sign in or add your key here."
                            }
                        }
                    }
                } else {
                    section { class: "prov-section", "aria-labelledby": "prov-yours",
                        div { class: "prov-section-head",
                            h2 { id: "prov-yours", class: "prov-section-title", "Your providers" }
                            span { class: "prov-count", "{usable_count} of {providers.len()} ready" }
                        }
                        div { class: "prov-grid",
                            for index in order {
                                ProviderCard {
                                    key: "{providers[index].id}",
                                    session,
                                    overlay,
                                    provider: providers[index].clone(),
                                }
                            }
                        }
                    }
                }
                if is_admin && loaded {
                    section { class: "prov-section prov-add", "aria-labelledby": "prov-add-title",
                        div { class: "prov-section-head",
                            div {
                                h2 { id: "prov-add-title", class: "prov-section-title", "Add a provider" }
                                p { class: "tiny", "Members sign in or add their own keys once a provider is here." }
                            }
                            div { class: "search-wrap prov-search",
                                span { class: "search-glyph", Icon { name: "search", size: 14 } }
                                input {
                                    class: "input",
                                    r#type: "search",
                                    placeholder: "Search providers or models",
                                    "aria-label": "Search providers to add",
                                    value: "{query}",
                                    oninput: move |event| query.set(event.value()),
                                    onkeydown: move |event: KeyboardEvent| {
                                        if event.key() == Key::Escape && !query.read().is_empty() {
                                            event.prevent_default();
                                            query.set(String::new());
                                        }
                                    },
                                }
                            }
                        }
                        if !sign_in_presets.is_empty() {
                            h3 { class: "prov-sub-title",
                                Icon { name: "external", size: 13 }
                                "Sign in with an account"
                            }
                            div { class: "prov-plan-grid",
                                for preset in sign_in_presets {
                                    PlanTile { key: "{preset.id}", session, overlay, preset: *preset }
                                }
                            }
                        }
                        if !key_presets.is_empty() {
                            h3 { class: "prov-sub-title",
                                Icon { name: "lock", size: 13 }
                                "Use an API key"
                            }
                            div { class: "prov-key-grid",
                                for preset in key_presets {
                                    KeyTile { key: "{preset.id}", session, overlay, preset: *preset }
                                }
                            }
                        }
                        if searching && nothing_to_add {
                            div { class: "empty compact",
                                p { class: "empty-text", "No provider matches \"{query}\"." }
                                button {
                                    class: "btn ghost sm",
                                    r#type: "button",
                                    onclick: move |_| query.set(String::new()),
                                    "Clear search"
                                }
                            }
                        }
                    }
                }
                UsageCard { session }
            }
        }
    }
}

/// One provider the server offers, with the member's accounts on it.
#[component]
fn ProviderCard(
    session: Signal<Session>,
    overlay: Signal<Overlay>,
    provider: ProviderStatus,
) -> Element {
    let mut confirm = use_signal(|| false);
    let (can_write, is_admin, testing, test_outcome) = {
        let s = session.read();
        (
            s.can_admin(),
            s.is_admin(),
            s.provider_testing.as_deref() == Some(provider.id.as_str()),
            s.state
                .provider_tests
                .get(&provider.id)
                .map(provider_view::format_provider_test),
        )
    };
    let signed_in = provider_signed_in(&provider.kind, &provider.secrets);
    let conn = provider_view::connection(&provider, signed_in);
    let preset = provider_view::preset_of(&provider).copied();
    let brand = if branding::brand_label(&provider.id).is_some() {
        provider.id.clone()
    } else {
        preset
            .map(|p| p.id.to_owned())
            .unwrap_or_else(|| provider.kind.clone())
    };
    let title = provider_view::card_title(&provider, branding::brand_label(&provider.kind));
    let kind_label = provider_view::human_kind(&provider.kind).to_owned();
    let model_line = provider_view::model_line(&provider);
    let shown: Vec<String> = provider.models.iter().take(4).cloned().collect();
    let extra = provider.models.len().saturating_sub(shown.len());
    let star_model = provider
        .default_model
        .clone()
        .or_else(|| provider.models.first().cloned());
    let key_url = preset.map(|p| p.key_url).filter(|url| !url.is_empty());
    let blocker = accounts::signin_blocker(&provider, is_admin);
    let id = provider.id.clone();
    let class = match (provider.is_default, conn.is_connected()) {
        (true, _) => "prov-card is-default",
        (false, true) => "prov-card is-ready",
        (false, false) => "prov-card",
    };
    rsx! {
        article { class: "{class}", "aria-label": "{title}",
            header { class: "prov-card-head",
                BrandLogo { brand, size: 36 }
                div { class: "prov-card-title",
                    div { class: "prov-card-name",
                        h3 { "{title}" }
                        if provider.is_default {
                            span { class: "prov-badge accent",
                                Icon { name: "star", size: 11 }
                                "Default"
                            }
                        }
                    }
                    span { class: "prov-card-sub",
                        if title != kind_label && !title.contains(&kind_label) {
                            "{kind_label} · "
                        }
                        "{model_line}"
                    }
                }
                StatusChip { tone: conn.tone(), text: conn.label().to_owned() }
            }
            if !shown.is_empty() {
                div { class: "prov-models", "aria-label": "Models",
                    for model in shown {
                        span { class: "prov-model", "{model}" }
                    }
                    if extra > 0 {
                        span { class: "prov-model more", "+{extra}" }
                    }
                }
            }
            if !provider.data_egress_approved {
                div { class: "prov-note warn", role: "note",
                    Icon { name: "alert", size: 14 }
                    span { "Data egress is not approved for this provider, so bots do not send it anything yet." }
                }
            }
            if let Some(blocker) = blocker {
                SigninNotice { session, provider_id: id.clone(), blocker }
            }
            if provider.pending_restart {
                div { class: "prov-note info", role: "status",
                    Icon { name: "refresh", size: 14 }
                    if is_admin {
                        span { class: "grow", "Saved. Restart the server so your bots can use it." }
                        button {
                            class: "btn primary sm",
                            r#type: "button",
                            disabled: !can_write,
                            onclick: move |_| session.write().request_restart(),
                            "Restart server"
                        }
                    } else {
                        span { "Your bots can use it once the admin restarts the server." }
                    }
                }
            }
            if let Some((tone, text)) = test_outcome {
                div { class: "provider-test-line", role: "status",
                    StatusChip { tone, text }
                }
            }
            AccountsBlock { session, provider_id: id.clone(), overlay }
            footer { class: "prov-card-foot",
                if !provider.is_default && conn.is_connected() && star_model.is_some() {
                    button {
                        class: "btn ghost sm",
                        r#type: "button",
                        title: "Use this provider for new chats",
                        disabled: !can_write,
                        onclick: {
                            let id = id.clone();
                            let model = star_model.clone().unwrap_or_default();
                            move |_| session.write().set_default_provider(id.clone(), model.clone())
                        },
                        Icon { name: "star", size: 14 }
                        "Make default"
                    }
                }
                if provider.accounts.is_empty() && conn.is_connected() {
                    button {
                        class: "btn ghost sm",
                        r#type: "button",
                        title: "Ask the provider for its model list with the saved key",
                        disabled: !can_write || testing,
                        onclick: {
                            let id = id.clone();
                            move |_| session.write().test_provider(id.clone())
                        },
                        Icon { name: "refresh", size: 14 }
                        if testing { "Testing\u{2026}" } else { "Test" }
                    }
                }
                if let Some(url) = key_url {
                    button {
                        class: "btn ghost sm",
                        r#type: "button",
                        title: "Open the provider's site to create a key",
                        onclick: move |_| session.write().open_in_browser(url.to_owned()),
                        Icon { name: "external", size: 14 }
                        "Get a key"
                    }
                }
                span { class: "grow" }
                if provider.managed && is_admin {
                    if confirm() {
                        div { class: "prov-confirm", role: "alertdialog", "aria-label": "Confirm removing provider",
                            span { class: "tiny", "Remove {title} for everyone? Chats pinned to it pick another route." }
                            button {
                                class: "btn danger sm",
                                r#type: "button",
                                disabled: !can_write,
                                onclick: {
                                    let id = id.clone();
                                    move |_| {
                                        session.write().remove_provider(id.clone());
                                        confirm.set(false);
                                    }
                                },
                                "Remove"
                            }
                            button {
                                class: "btn ghost sm",
                                r#type: "button",
                                onclick: move |_| confirm.set(false),
                                "Cancel"
                            }
                        }
                    } else {
                        button {
                            class: "btn ghost sm prov-remove",
                            r#type: "button",
                            title: "Remove this provider from the server",
                            onclick: move |_| confirm.set(true),
                            Icon { name: "trash", size: 14 }
                            "Remove"
                        }
                    }
                }
            }
        }
    }
}

/// Why a provider's sign-in cannot start, and the fix when the viewer can make
/// it: the Antigravity client secret field, or turning a plan sign-in back on.
#[component]
fn SigninNotice(session: Signal<Session>, provider_id: String, blocker: SigninBlocker) -> Element {
    let mut secret = use_signal(String::new);
    let can_write = session.read().can_admin();
    let fix = blocker.fix_label();
    let tone = if fix.is_some() {
        "prov-note action"
    } else {
        "prov-note warn"
    };
    rsx! {
        div { class: "{tone}", role: "note",
            Icon { name: "alert", size: 14 }
            div { class: "prov-note-body",
                span { "{blocker.message()}" }
                match (blocker, fix) {
                    (SigninBlocker::ClientSecret { can_fix: true }, Some(label)) => rsx! {
                        div { class: "prov-secret-row",
                            input {
                                class: "input",
                                r#type: "password",
                                autocomplete: "off",
                                spellcheck: false,
                                placeholder: "Google client secret",
                                "aria-label": "Google Antigravity client secret",
                                value: "{secret}",
                                oninput: move |event| secret.set(event.value()),
                            }
                            button {
                                class: "btn primary sm",
                                r#type: "button",
                                disabled: !can_write || secret().trim().is_empty(),
                                onclick: {
                                    let id = provider_id.clone();
                                    move |_| {
                                        let value = std::mem::take(&mut *secret.write());
                                        session.write().resave_provider(&id, Some(value));
                                    }
                                },
                                "{label}"
                            }
                        }
                        span { class: "tiny", "Stored encrypted on the server and never shown again. It is the public secret of Google's Antigravity desktop client." }
                    },
                    (_, Some(label)) => rsx! {
                        div {
                            button {
                                class: "btn primary sm",
                                r#type: "button",
                                disabled: !can_write,
                                onclick: {
                                    let id = provider_id.clone();
                                    move |_| session.write().resave_provider(&id, None)
                                },
                                "{label}"
                            }
                        }
                    },
                    _ => rsx! {},
                }
            }
        }
    }
}

/// A provider the admin can add by signing in (Claude, ChatGPT, Antigravity,
/// Copilot, Muse, OpenRouter). One press saves it and opens the sign-in.
#[component]
fn PlanTile(session: Signal<Session>, overlay: Signal<Overlay>, preset: ProviderPreset) -> Element {
    let mut overlay = overlay;
    let can_write = session.read().can_admin();
    let takes_key = preset.auth == "api_key";
    let button_label = match preset.kind {
        "anthropic" => "Sign in with Claude",
        "openai-codex" => "Sign in with ChatGPT",
        "google-antigravity" => "Sign in with Google",
        "github-copilot" => "Sign in with GitHub",
        _ => "Sign in",
    };
    let how = match preset.sign_in {
        "device" => "One-time code",
        _ => "Browser sign-in",
    };
    rsx! {
        article { class: "prov-plan", "aria-label": "{preset.label}",
            div { class: "prov-plan-head",
                BrandLogo { brand: preset.id.to_owned(), size: 32 }
                div { class: "prov-plan-title",
                    h4 { "{preset.label}" }
                    span { class: "tiny", "{how}" if takes_key { " or API key" } }
                }
            }
            p { class: "prov-plan-text", "{preset.note}" }
            if !preset.risk.is_empty() {
                p { class: "prov-plan-risk", title: "{preset.risk}",
                    Icon { name: "alert", size: 12 }
                    "Uses the vendor's own app sign-in"
                }
            }
            div { class: "prov-plan-actions",
                button {
                    class: "btn primary sm",
                    r#type: "button",
                    disabled: !can_write,
                    onclick: move |_| {
                        let mut s = session.write();
                        s.fill_provider_draft(&preset, None);
                        // Saves the provider, then opens its sign-in at once.
                        s.save_provider();
                    },
                    Icon { name: "external", size: 13 }
                    "{button_label}"
                }
                if takes_key {
                    button {
                        class: "btn ghost sm",
                        r#type: "button",
                        onclick: move |_| {
                            session.write().fill_provider_draft(&preset, None);
                            overlay.set(Overlay::AddProvider);
                        },
                        "Add a key"
                    }
                }
            }
        }
    }
}

/// A provider the admin can add with an API key (or a local server, or a
/// custom endpoint): opens the Add provider sheet filled from the preset.
#[component]
fn KeyTile(session: Signal<Session>, overlay: Signal<Overlay>, preset: ProviderPreset) -> Element {
    let mut overlay = overlay;
    let label = match preset.auth {
        "none" => "Set up",
        _ if preset.id == "custom" => "Configure",
        _ => "Add key",
    };
    rsx! {
        button {
            class: "prov-key",
            r#type: "button",
            title: "{preset.subtitle}",
            onclick: move |_| {
                if preset.id == "custom" {
                    open_custom(session, overlay);
                } else {
                    session.write().fill_provider_draft(&preset, None);
                    overlay.set(Overlay::AddProvider);
                }
            },
            BrandLogo { brand: preset.id.to_owned(), size: 26 }
            span { class: "prov-key-copy",
                span { class: "prov-key-name", "{preset.label}" }
                span { class: "prov-key-sub", "{preset.note}" }
            }
            span { class: "prov-key-cta", "{label}" }
        }
    }
}

/// The member's recent token use, folded away until asked for.
#[component]
fn UsageCard(session: Signal<Session>) -> Element {
    let usage = session.read().state.usage.clone();
    let usage_days: Vec<(String, String)> = usage
        .as_ref()
        .map(|report| {
            report
                .per_day
                .iter()
                .take(14)
                .map(|bucket| {
                    (
                        bucket.day.clone().unwrap_or_else(|| "\u{2014}".to_owned()),
                        format!(
                            "{} tokens \u{b7} {} calls",
                            bucket.total_tokens, bucket.calls
                        ),
                    )
                })
                .collect()
        })
        .unwrap_or_default();
    rsx! {
        details { class: "card advanced prov-usage",
            summary { "Recent token usage" }
            div { class: "advanced-body",
                div { class: "card-actions",
                    button {
                        class: "btn ghost sm",
                        r#type: "button",
                        onclick: move |_| session.write().load_usage(7),
                        "Load last 7 days"
                    }
                }
                if let Some(report) = usage {
                    div { class: "kv-row",
                        span { class: "kv-key", "Calls" }
                        span { class: "kv-val", "{report.totals.calls}" }
                    }
                    div { class: "kv-row",
                        span { class: "kv-key", "Prompt tokens" }
                        span { class: "kv-val", "{report.totals.prompt_tokens}" }
                    }
                    div { class: "kv-row",
                        span { class: "kv-key", "Completion tokens" }
                        span { class: "kv-val", "{report.totals.completion_tokens}" }
                    }
                    div { class: "kv-row",
                        span { class: "kv-key", "Total tokens" }
                        span { class: "kv-val", "{report.totals.total_tokens}" }
                    }
                    if report.per_day.is_empty() {
                        p { class: "caption", "No calls in this window." }
                    }
                    for (day, summary) in usage_days {
                        div { class: "kv-row",
                            span { class: "kv-key", "{day}" }
                            span { class: "kv-val", "{summary}" }
                        }
                    }
                } else {
                    p { class: "caption", "No usage report loaded yet. Load the last 7 days to see calls and tokens." }
                }
            }
        }
    }
}

struct BotRouteRowView {
    id: String,
    name: String,
    avatar: String,
    current_value: String,
    assigned_label: String,
    thinking_badge: Option<String>,
    fallback_count: usize,
    unlisted_current: bool,
}

fn thinking_badge_label(level: &str) -> &str {
    match level {
        "off" => "Off",
        "low" => "Low",
        "medium" => "Medium",
        "high" => "High",
        "xhigh" => "Extra high",
        other => other,
    }
}

fn quick_set_bot_model(
    mut session: Signal<Session>,
    bot_id: String,
    prev_value: String,
    next_value: String,
) {
    if prev_value == next_value {
        return;
    }
    let mut s = session.write();
    if s.bot_settings.saving || !s.can_admin() {
        return;
    }
    let mut display_name = s.bot_display_name(&bot_id);
    if display_name.trim().is_empty() {
        display_name = bot_id.clone();
    }
    let mut original = serabot_desktop::session::BotDraft {
        display_name: display_name.clone(),
        model: prev_value,
        ..Default::default()
    };
    let mut draft = original.clone();
    draft.model = next_value;
    if original.model == draft.model {
        original.model = if draft.model.is_empty() {
            "__reset__|__reset__".to_owned()
        } else {
            String::new()
        };
    }
    s.bot_settings.bot = Some(bot_id);
    s.bot_settings.original = original;
    s.bot_settings.draft = draft;
    s.bot_settings.hydrated = true;
    s.save_bot_settings();
    // Mark unhydrated so opening the full bot settings sheet later reloads
    // every field cleanly from the server record.
    s.bot_settings.hydrated = false;
}

/// Compact per-bot model routing table on the Providers page.
#[component]
fn BotRoutingSection(session: Signal<Session>) -> Element {
    let (rows, model_groups, can_write, saving_bot, route_error) = {
        let s = session.read();
        let enabled_bots: Vec<_> = s
            .state
            .listed_bots
            .iter()
            .filter(|bot| bot.enabled)
            .collect();
        if enabled_bots.is_empty() {
            return rsx! {};
        }
        let options = s.route_options();
        type ModelGroup = (String, String, Vec<(String, String)>);
        let mut model_groups: Vec<ModelGroup> = Vec::new();
        let mut model_to_value: Vec<(String, String)> = Vec::new();
        let mut ready_values: std::collections::HashSet<String> = std::collections::HashSet::new();
        for prov in options.provider_choices() {
            if !prov.egress_approved {
                continue;
            }
            let prov_ready = s
                .state
                .providers
                .iter()
                .find(|p| p.id == prov.id)
                .is_none_or(usable);
            if !prov_ready {
                continue;
            }
            let models: Vec<(String, String)> = options
                .model_choices(&prov.id)
                .into_iter()
                .map(|choice| {
                    let value = serabot_desktop::session::model_value(&prov.id, &choice.model);
                    model_to_value.push((choice.model.clone(), value.clone()));
                    ready_values.insert(value.clone());
                    (value, human_model(&choice.model))
                })
                .collect();
            if !models.is_empty() {
                model_groups.push((prov.id, prov.label, models));
            }
        }
        let rows: Vec<BotRouteRowView> = enabled_bots
            .into_iter()
            .map(|bot| {
                let detail = s.state.bot_details.get(&bot.id);
                let route_primary = detail
                    .and_then(|d| d.route.as_ref())
                    .and_then(|r| r.route.as_ref());
                let current_value = if let Some(primary) = route_primary {
                    serabot_desktop::session::model_value(&primary.provider, &primary.model)
                } else if let Some(model_id) = bot.model.as_deref().filter(|m| !m.is_empty()) {
                    if let Some(d) =
                        detail.filter(|d| !d.provider.is_empty() && !d.model.is_empty())
                    {
                        serabot_desktop::session::model_value(&d.provider, &d.model)
                    } else {
                        model_to_value
                            .iter()
                            .find(|(raw, _)| raw == model_id)
                            .map(|(_, val)| val.clone())
                            .unwrap_or_else(|| model_id.to_owned())
                    }
                } else {
                    String::new()
                };
                let assigned_label = if let Some(primary) = route_primary {
                    human_model(&primary.model)
                } else if let Some(model_id) = bot.model.as_deref().filter(|m| !m.is_empty()) {
                    human_model(model_id)
                } else {
                    "Default".to_owned()
                };
                let thinking_badge = route_primary
                    .and_then(|p| p.thinking.as_deref())
                    .or_else(|| detail.and_then(|d| d.thinking.as_deref()))
                    .filter(|level| !level.is_empty())
                    .map(|level| thinking_badge_label(level).to_owned());
                let fallback_count = detail
                    .and_then(|d| d.route.as_ref())
                    .map(|r| r.fallbacks.len())
                    .unwrap_or(0);
                let unlisted_current =
                    !current_value.is_empty() && !ready_values.contains(&current_value);
                BotRouteRowView {
                    id: bot.id.clone(),
                    name: s.bot_display_name(&bot.id),
                    avatar: s.bot_avatar(&bot.id),
                    current_value,
                    assigned_label,
                    thinking_badge,
                    fallback_count,
                    unlisted_current,
                }
            })
            .collect();
        let can_write = s.can_admin() && !s.bot_settings.saving;
        let saving_bot = if s.bot_settings.saving {
            s.bot_settings.bot.clone()
        } else {
            None
        };
        let route_error = s.bot_settings.error.clone();
        (rows, model_groups, can_write, saving_bot, route_error)
    };
    let count = rows.len();
    rsx! {
        section { class: "prov-section bot-routing-section", "aria-labelledby": "prov-bot-routing",
            div { class: "prov-section-head",
                div {
                    h2 { id: "prov-bot-routing", class: "prov-section-title", "Bot routing" }
                    p { class: "tiny", "Assign each bot its own model, or leave it on Default to inherit the global route above." }
                }
                span { class: "prov-count", "{count} enabled" }
            }
            if let Some(err) = route_error {
                div { class: "banner err", role: "alert",
                    span { class: "grow", "{err}" }
                }
            }
            div { class: "card bot-routing-card", role: "list", "aria-label": "Bot routing",
                for row in rows {
                    {
                        let bot_id_for_select = row.id.clone();
                        let bot_id_for_btn = row.id.clone();
                        let current_for_select = row.current_value.clone();
                        let is_saving = saving_bot.as_deref() == Some(row.id.as_str());
                        let select_id = format!("bot-route-select-{}", row.id);
                        rsx! {
                            div { key: "{row.id}", class: "bot-route-row", role: "listitem",
                                div { class: "bot-route-info",
                                    span { class: "avatar sm bot", style: avatar_style(&row.id), "{row.avatar}" }
                                    div { class: "bot-route-copy",
                                        span { class: "bot-route-name", "{row.name}" }
                                        div { class: "bot-route-meta",
                                            span { class: "bot-route-assigned", "{row.assigned_label}" }
                                            if let Some(thinking) = row.thinking_badge {
                                                span { class: "chip muted", "Thinking: {thinking}" }
                                            }
                                            if is_saving {
                                                span { class: "chip muted", "Saving\u{2026}" }
                                            }
                                        }
                                    }
                                }
                                div { class: "bot-route-controls",
                                    select {
                                        id: "{select_id}",
                                        class: "select bot-route-select",
                                        "aria-label": "Model for {row.name}",
                                        value: "{row.current_value}",
                                        disabled: !can_write,
                                        onchange: move |event| {
                                            quick_set_bot_model(
                                                session,
                                                bot_id_for_select.clone(),
                                                current_for_select.clone(),
                                                event.value(),
                                            );
                                        },
                                        option {
                                            value: "",
                                            selected: row.current_value.is_empty(),
                                            if row.fallback_count > 0 {
                                                "Default (reset route & fallbacks)"
                                            } else {
                                                "Default (inherit)"
                                            }
                                        }
                                        if row.unlisted_current {
                                            option {
                                                value: "{row.current_value}",
                                                selected: true,
                                                "{row.assigned_label}"
                                            }
                                        }
                                        for (prov_id, prov_label, models) in &model_groups {
                                            optgroup { key: "{prov_id}", label: "{prov_label}",
                                                for (val, label) in models {
                                                    option {
                                                        key: "{val}",
                                                        value: "{val}",
                                                        selected: *val == row.current_value,
                                                        "{label}"
                                                    }
                                                }
                                            }
                                        }
                                    }
                                    button {
                                        class: "btn ghost sm",
                                        r#type: "button",
                                        title: "Configure primary model, fallbacks and failover rules",
                                        onclick: move |_| session.write().open_bot_settings(bot_id_for_btn.clone()),
                                        Icon { name: "settings", size: 13 }
                                        if row.fallback_count > 0 {
                                            "Fallbacks ({row.fallback_count})"
                                        } else {
                                            "Fallbacks"
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
