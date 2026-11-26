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
                        } else if matches!(preset.sign_in, "pkce" | "browser") {
                            div { class: "banner info",
                                span {
                                    if preset.kind == "anthropic" {
                                        "Paste an API key below, or leave it empty and press Save and sign in to sign in with Claude in your browser (port 54545; you can also paste the callback URL or code#state)."
                                    } else {
                                        "Paste an API key below, or leave it empty and press Save and sign in to sign in with your browser."
                                    }
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
    let (providers, loaded, can_write, is_admin, section_load_error) = {
        let s = session.read();
        (
            s.state.providers.clone(),
            s.state.providers_loaded,
            s.can_admin(),
            s.is_admin(),
            s.section_load_error.clone(),
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
                    if let Some(err) = section_load_error {
                        div { class: "empty prov-empty", role: "alert",
                            div { class: "empty-icon", Icon { name: "alert", size: 20 } }
                            h2 { class: "empty-title", "Could not load AI providers" }
                            p { class: "empty-text", "{err}" }
                            button {
                                class: "btn ghost sm",
                                r#type: "button",
                                onclick: move |_| session.write().retry_section_load(),
                                "Retry"
                            }
                        }
                    } else {
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
                    }
                    span { class: "prov-card-sub",
                        if title != kind_label && !title.contains(&kind_label) {
                            "{kind_label} · "
                        }
                        "{model_line}"
                    }
                }
                div { class: "prov-card-head-right",
                    if provider.is_default {
                        span { class: "prov-badge accent",
                            Icon { name: "star", size: 11 }
                            "Default"
                        }
                    }
                    StatusChip { tone: conn.tone(), text: conn.label().to_owned() }
                }
            }
            if !shown.is_empty() {
                div { class: "prov-models-wrap",
                    span { class: "prov-models-label", "Models" }
                    div { class: "prov-models", "aria-label": "Models",
                        for model in shown {
                            span { class: "prov-model", "{model}" }
                        }
                        if extra > 0 {
                            span { class: "prov-model more", "+{extra} more" }
                        }
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
    let takes_key = matches!(preset.auth, "api_key" | "oauth_or_key");
    let button_label = match preset.kind {
        "anthropic" => "Sign in with Claude",
        "openai-codex" => "Sign in with ChatGPT",
        "google-antigravity" => "Sign in with Google",
        "github-copilot" => "Sign in with GitHub",
        "mistral" => "Sign in with Mistral",
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

struct BotChainItemView {
    id: String,
    name: String,
    avatar: String,
    primary_label: String,
    fallback_count: usize,
    is_selected: bool,
    is_dirty: bool,
}

#[derive(Clone, PartialEq)]
struct ChainSlotView {
    row: usize,
    title: &'static str,
    slot: serabot_desktop::route_editor::RouteSlot,
    providers: Vec<serabot_desktop::route_editor::ProviderChoice>,
    accounts: Vec<(String, String)>,
    models: Vec<serabot_desktop::route_editor::ModelChoice>,
    levels: Vec<String>,
    error: Option<String>,
    consent_row: bool,
    consent: bool,
}

fn build_chain_slots(
    session: &Session,
) -> (
    Vec<ChainSlotView>,
    serabot_desktop::route_editor::RouteCheck,
) {
    use serabot_desktop::route_editor::{MAX_FALLBACKS, RouteCheck};
    let options = session.route_options();
    let draft = &session.route_editor.draft;
    let check = if session.state.catalog.is_some() {
        draft.check(&options)
    } else {
        RouteCheck::default()
    };
    let rows = (0..=MAX_FALLBACKS)
        .map(|row| {
            let slot = match row {
                0 => draft.primary.clone(),
                other => draft.fallbacks[other - 1].clone(),
            };
            let (title, error) = match row {
                0 => ("Primary model", check.primary.clone()),
                1 => ("Fallback 1", check.fallbacks[0].clone()),
                _ => ("Fallback 2", check.fallbacks[1].clone()),
            };
            let chosen = !slot.is_empty();
            ChainSlotView {
                row,
                title,
                providers: options.provider_choices(),
                accounts: if chosen {
                    options.account_choices(&slot.provider)
                } else {
                    Vec::new()
                },
                models: if chosen {
                    options.model_choices(&slot.provider)
                } else {
                    Vec::new()
                },
                levels: options.thinking_levels(&slot.provider, &slot.model),
                consent_row: row > 0 && check.needs_consent[row - 1],
                consent: row > 0 && draft.consent[row - 1],
                error,
                slot,
            }
        })
        .collect();
    (rows, check)
}

#[component]
fn BotChainSlotCard(
    session: Signal<Session>,
    view: ChainSlotView,
    hydrated: bool,
    can_write: bool,
    can_move_up: bool,
    can_move_down: bool,
) -> Element {
    let row = view.row;
    let primary = row == 0;
    let chosen = !view.slot.is_empty();
    let segments =
        serabot_desktop::route_chip::thinking_segments(&view.levels, &view.slot.thinking);
    let class = match (primary, view.error.is_some()) {
        (_, true) => "bot-chain-slot has-error",
        (true, false) => "bot-chain-slot is-primary",
        (false, false) => "bot-chain-slot",
    };
    let none_label = if primary {
        "Inherit global default"
    } else {
        "Choose a provider\u{2026}"
    };
    let id = |part: &str| format!("chain-{row}-{part}");
    rsx! {
        div { class: "{class}",
            div { class: "bot-chain-slot-head",
                div { class: "bot-chain-slot-title",
                    span { class: if primary { "chip accent" } else { "chip muted" }, "{view.title}" }
                    if primary && !chosen {
                        span { class: "tiny muted", "Uses the default model strip above" }
                    }
                }
                if !primary {
                    div { class: "bot-chain-slot-actions",
                        button {
                            class: "btn icon ghost sm",
                            r#type: "button",
                            title: "Move fallback earlier",
                            "aria-label": "Move {view.title} up",
                            disabled: !hydrated || !can_write || !can_move_up,
                            onclick: move |_| {
                                let fb_idx = row - 1;
                                if fb_idx > 0 {
                                    session.write().swap_route_fallbacks(fb_idx - 1, fb_idx);
                                }
                            },
                            Icon { name: "chevron-up", size: 14 }
                        }
                        button {
                            class: "btn icon ghost sm",
                            r#type: "button",
                            title: "Move fallback later",
                            "aria-label": "Move {view.title} down",
                            disabled: !hydrated || !can_write || !can_move_down,
                            onclick: move |_| {
                                let fb_idx = row - 1;
                                session.write().swap_route_fallbacks(fb_idx, fb_idx + 1);
                            },
                            Icon { name: "chevron-down", size: 14 }
                        }
                        button {
                            class: "btn ghost sm",
                            r#type: "button",
                            "aria-label": "Remove {view.title}",
                            disabled: !hydrated || !can_write,
                            onclick: move |_| {
                                session.write().remove_route_fallback(row - 1);
                            },
                            Icon { name: "trash", size: 13 }
                            "Remove"
                        }
                    }
                }
            }
            div { class: "route-grid",
                div { class: "field",
                    label { class: "field-label", r#for: "{id(\"provider\")}", "Provider" }
                    select {
                        id: "{id(\"provider\")}",
                        class: "select",
                        value: "{view.slot.provider}",
                        disabled: !hydrated || !can_write,
                        onchange: move |event| {
                            session.write().route_change(
                                row,
                                serabot_desktop::session::RouteChange::Provider(event.value()),
                            );
                        },
                        option { value: "", selected: !chosen, "{none_label}" }
                        for choice in view.providers.clone() {
                            option {
                                key: "{choice.id}",
                                value: "{choice.id}",
                                selected: choice.id == view.slot.provider,
                                disabled: !choice.egress_approved,
                                if choice.egress_approved {
                                    "{choice.label}"
                                } else {
                                    "{choice.label} (no data-egress approval)"
                                }
                            }
                        }
                    }
                }
                if chosen {
                    div { class: "field",
                        label { class: "field-label", r#for: "{id(\"account\")}", "Account" }
                        select {
                            id: "{id(\"account\")}",
                            class: "select",
                            value: "{view.slot.account}",
                            disabled: !hydrated || !can_write,
                            onchange: move |event| {
                                session.write().route_change(
                                    row,
                                    serabot_desktop::session::RouteChange::Account(event.value()),
                                );
                            },
                            for (account_id, label) in view.accounts.clone() {
                                option {
                                    key: "{account_id}",
                                    value: "{account_id}",
                                    selected: account_id == view.slot.account,
                                    "{label}"
                                }
                            }
                        }
                    }
                    div { class: "field",
                        label { class: "field-label", r#for: "{id(\"model\")}", "Model" }
                        select {
                            id: "{id(\"model\")}",
                            class: "select",
                            value: "{view.slot.model}",
                            disabled: !hydrated || !can_write,
                            onchange: move |event| {
                                session.write().route_change(
                                    row,
                                    serabot_desktop::session::RouteChange::Model(event.value()),
                                );
                            },
                            if !view.slot.model.is_empty()
                                && !view.models.iter().any(|choice| choice.model == view.slot.model)
                            {
                                option {
                                    value: "{view.slot.model}",
                                    selected: true,
                                    "{view.slot.model} (not in your list)"
                                }
                            }
                            for choice in view.models.clone() {
                                option {
                                    key: "{choice.model}",
                                    value: "{choice.model}",
                                    selected: choice.model == view.slot.model,
                                    "{choice.title}"
                                }
                            }
                        }
                    }
                    if !segments.is_empty() {
                        div { class: "field",
                            span { class: "field-label", "Thinking" }
                            div { class: "seg", role: "group", "aria-label": "Thinking level for {view.title}",
                                for segment in segments {
                                    button {
                                        key: "{segment.label}",
                                        r#type: "button",
                                        class: if segment.active { "seg-btn active" } else { "seg-btn" },
                                        "aria-pressed": if segment.active { "true" } else { "false" },
                                        disabled: !hydrated || !can_write,
                                        onclick: {
                                            let value = segment.value.clone();
                                            move |_| {
                                                session.write().route_change(
                                                    row,
                                                    serabot_desktop::session::RouteChange::Thinking(value.clone()),
                                                );
                                            }
                                        },
                                        "{segment.label}"
                                    }
                                }
                            }
                        }
                    }
                }
            }
            if view.consent_row {
                label { class: "route-consent",
                    input {
                        class: "switch",
                        r#type: "checkbox",
                        checked: view.consent,
                        disabled: !hydrated || !can_write,
                        onchange: move |event| {
                            let mut s = session.write();
                            if let Some(slot) = s.route_editor.draft.consent.get_mut(row - 1) {
                                *slot = event.checked();
                            }
                            s.route_editor.error = None;
                            s.route_editor.notice = None;
                        },
                    }
                    span { "Send prompts to this provider when the primary fails." }
                }
            }
            if let Some(message) = view.error {
                p { class: "field-error", role: "alert", "{message}" }
            }
        }
    }
}

/// Per-bot model & fallback chain editor on the AI Providers page.
#[component]
fn BotRoutingSection(session: Signal<Session>) -> Element {
    use serabot_desktop::route_editor::{self, MAX_FALLBACKS, MAX_SWITCHES};

    let first_bot_id = {
        let s = session.read();
        s.state
            .listed_bots
            .iter()
            .find(|bot| bot.enabled)
            .map(|bot| bot.id.clone())
    };
    let Some(first_bot_id) = first_bot_id else {
        return rsx! {};
    };

    use_effect(move || {
        let needs_select = {
            let s = session.read();
            let current = s.route_editor.bot.as_deref();
            !s.state
                .listed_bots
                .iter()
                .any(|b| b.enabled && Some(b.id.as_str()) == current)
        };
        if needs_select {
            session.write().select_route_bot(&first_bot_id);
        }
    });

    let (
        items,
        selected_id,
        selected_name,
        selected_avatar,
        slots,
        check,
        hydrated,
        saving,
        dirty,
        error,
        notice,
        failover,
        effective,
        last_switch,
        can_write,
        first_approved_provider,
    ) = {
        let s = session.read();
        let enabled_bots: Vec<_> = s
            .state
            .listed_bots
            .iter()
            .filter(|bot| bot.enabled)
            .collect();
        let selected_id = s
            .route_editor
            .bot
            .clone()
            .filter(|id| enabled_bots.iter().any(|b| &b.id == id))
            .unwrap_or_else(|| enabled_bots[0].id.clone());
        let editor_dirty = s.route_editor.dirty();
        let items: Vec<BotChainItemView> = enabled_bots
            .iter()
            .map(|bot| {
                let is_selected = bot.id == selected_id;
                let detail = s.state.bot_details.get(&bot.id);
                let route_primary = detail
                    .and_then(|d| d.route.as_ref())
                    .and_then(|r| r.route.as_ref());
                let primary_label = if is_selected && s.route_editor.hydrated {
                    if s.route_editor.draft.primary.is_empty() {
                        "Default".to_owned()
                    } else {
                        human_model(&s.route_editor.draft.primary.model)
                    }
                } else if let Some(primary) = route_primary {
                    human_model(&primary.model)
                } else if let Some(model_id) = bot.model.as_deref().filter(|m| !m.is_empty()) {
                    human_model(model_id)
                } else {
                    "Default".to_owned()
                };
                let fallback_count = if is_selected && s.route_editor.hydrated {
                    s.route_editor
                        .draft
                        .fallbacks
                        .iter()
                        .filter(|slot| !slot.is_empty())
                        .count()
                } else {
                    detail
                        .and_then(|d| d.route.as_ref())
                        .map(|r| r.fallbacks.len())
                        .unwrap_or(0)
                };
                BotChainItemView {
                    id: bot.id.clone(),
                    name: s.bot_display_name(&bot.id),
                    avatar: s.bot_avatar(&bot.id),
                    primary_label,
                    fallback_count,
                    is_selected,
                    is_dirty: is_selected && editor_dirty,
                }
            })
            .collect();

        let (slots, check) = build_chain_slots(&s);
        let options = s.route_options();
        let view = s
            .state
            .bot_details
            .get(&selected_id)
            .and_then(|detail| detail.route.as_ref());
        let first_approved_provider = options
            .provider_choices()
            .into_iter()
            .find(|p| p.egress_approved)
            .map(|p| p.id);

        (
            items,
            selected_id.clone(),
            s.bot_display_name(&selected_id),
            s.bot_avatar(&selected_id),
            slots,
            check,
            s.route_editor.hydrated && s.route_editor.bot.as_deref() == Some(selected_id.as_str()),
            s.route_editor.saving,
            editor_dirty,
            s.route_editor.error.clone(),
            s.route_editor.notice.clone(),
            s.route_editor.draft.failover,
            view.and_then(|v| v.effective.as_ref())
                .map(|eff| route_editor::effective_line(&options, eff)),
            view.and_then(|v| v.last_switch.as_ref())
                .map(|sw| route_editor::switch_line(&options, sw)),
            s.can_admin(),
            first_approved_provider,
        )
    };

    let count = items.len();
    let primary_slot = slots.first().cloned();
    let active_fallbacks: Vec<ChainSlotView> = slots
        .iter()
        .skip(1)
        .filter(|slot| !slot.slot.is_empty() || slot.error.is_some())
        .cloned()
        .collect();
    let active_fallback_count = active_fallbacks.len();
    let can_add_fallback = hydrated
        && can_write
        && active_fallback_count < MAX_FALLBACKS
        && first_approved_provider.is_some();
    let can_save = hydrated && can_write && dirty && !saving && check.is_ok();

    // Summary pills for the visual chain strip
    let primary_pill_text = primary_slot
        .as_ref()
        .filter(|s| !s.slot.is_empty())
        .map(|s| human_model(&s.slot.model))
        .unwrap_or_else(|| "Global default".to_owned());

    rsx! {
        section { class: "prov-section bot-routing-section", "aria-labelledby": "prov-bot-routing",
            div { class: "prov-section-head",
                div {
                    h2 { id: "prov-bot-routing", class: "prov-section-title", "Bot routing & fallback chains" }
                    p { class: "tiny", "Pick a bot to configure its primary model, fallback chain, and automatic failover rules." }
                }
                span { class: "prov-count", "{count} enabled" }
            }
            div { class: "bot-chain-shell",
                aside { class: "bot-chain-sidebar", "aria-label": "Bots",
                    div { class: "bot-chain-sidebar-head",
                        span { "Bots" }
                        span { "{count}" }
                    }
                    div { class: "bot-chain-list", role: "listbox", "aria-label": "Select a bot",
                        for item in items {
                            {
                                let bid = item.id.clone();
                                rsx! {
                                    button {
                                        key: "{item.id}",
                                        class: if item.is_selected { "bot-chain-item active" } else { "bot-chain-item" },
                                        r#type: "button",
                                        role: "option",
                                        "aria-selected": if item.is_selected { "true" } else { "false" },
                                        onclick: move |_| session.write().select_route_bot(&bid),
                                        span { class: "avatar sm bot", style: avatar_style(&item.id), "{item.avatar}" }
                                        div { class: "bot-chain-item-copy",
                                            div { class: "bot-chain-item-top",
                                                span { class: "bot-chain-item-name", "{item.name}" }
                                                if item.is_dirty {
                                                    span { class: "bot-chain-dirty-dot", title: "Unsaved changes" }
                                                }
                                            }
                                            div { class: "bot-chain-item-sub",
                                                span { "{item.primary_label}" }
                                                if item.fallback_count > 0 {
                                                    span { "\u{b7} +{item.fallback_count} fallback" }
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
                div { class: "bot-chain-editor",
                    div { class: "bot-chain-editor-head",
                        div { class: "bot-chain-editor-title",
                            span { class: "avatar sm bot", style: avatar_style(&selected_id), "{selected_avatar}" }
                            div {
                                h3 { "{selected_name}" }
                                if let Some(line) = effective {
                                    span { class: "tiny muted", "Starts on: {line}" }
                                } else {
                                    span { class: "tiny muted", "Model route & fallback chain" }
                                }
                            }
                        }
                        button {
                            class: "btn ghost sm",
                            r#type: "button",
                            onclick: {
                                let bid = selected_id.clone();
                                move |_| session.write().open_bot_settings(bid.clone())
                            },
                            Icon { name: "settings", size: 13 }
                            "Bot settings"
                        }
                    }
                    if !hydrated {
                        div { class: "banner info", role: "status",
                            span { class: "spinner" }
                            span { "Loading route for {selected_name}\u{2026}" }
                        }
                    } else {
                        div { class: "bot-chain-pills", "aria-label": "Active route chain",
                            span { class: "bot-chain-pill is-primary",
                                span { class: "bot-chain-pill-role", "Primary" }
                                span { "{primary_pill_text}" }
                            }
                            for (idx, fb) in active_fallbacks.iter().enumerate() {
                                span { key: "arrow-{idx}", class: "bot-chain-arrow", "\u{2192}" }
                                span { class: "bot-chain-pill",
                                    span { class: "bot-chain-pill-role", "Fallback {idx + 1}" }
                                    span {
                                        if fb.slot.model.is_empty() {
                                            "Select model"
                                        } else {
                                            "{human_model(&fb.slot.model)}"
                                        }
                                    }
                                }
                            }
                        }
                        if let Some(primary) = primary_slot {
                            BotChainSlotCard {
                                session,
                                view: primary,
                                hydrated,
                                can_write,
                                can_move_up: false,
                                can_move_down: false,
                            }
                        }
                        div { class: "bot-chain-fallbacks-head",
                            span { class: "bot-chain-fallbacks-title",
                                "Fallback chain ({active_fallback_count} of {MAX_FALLBACKS})"
                            }
                            if can_add_fallback {
                                button {
                                    class: "btn ghost sm",
                                    r#type: "button",
                                    onclick: {
                                        let next_row = active_fallback_count + 1;
                                        let provider = first_approved_provider.clone().unwrap_or_default();
                                        move |_| {
                                            session.write().route_change(
                                                next_row,
                                                serabot_desktop::session::RouteChange::Provider(provider.clone()),
                                            );
                                        }
                                    },
                                    Icon { name: "plus", size: 13 }
                                    "Add fallback"
                                }
                            }
                        }
                        if active_fallbacks.is_empty() {
                            p { class: "tiny muted",
                                "No fallbacks configured. Add a fallback to automatically try another model or provider if the primary fails."
                            }
                        } else {
                            for (idx, fb) in active_fallbacks.into_iter().enumerate() {
                                BotChainSlotCard {
                                    key: "fb-{fb.row}",
                                    session,
                                    view: fb,
                                    hydrated,
                                    can_write,
                                    can_move_up: idx > 0,
                                    can_move_down: idx + 1 < active_fallback_count,
                                }
                            }
                        }
                        div { class: "bot-chain-policy",
                            div { class: "bot-chain-policy-toggles",
                                label { class: "check",
                                    input {
                                        class: "switch",
                                        r#type: "checkbox",
                                        checked: failover.enabled,
                                        disabled: !hydrated || !can_write,
                                        onchange: move |event| {
                                            let mut s = session.write();
                                            s.route_editor.draft.failover.enabled = event.checked();
                                            s.route_editor.error = None;
                                            s.route_editor.notice = None;
                                        },
                                    }
                                    span { "Switch automatically" }
                                }
                                label { class: "check",
                                    input {
                                        class: "switch",
                                        r#type: "checkbox",
                                        checked: failover.switch_account_first,
                                        disabled: !hydrated || !can_write || !failover.enabled,
                                        onchange: move |event| {
                                            let mut s = session.write();
                                            s.route_editor.draft.failover.switch_account_first = event.checked();
                                            s.route_editor.error = None;
                                            s.route_editor.notice = None;
                                        },
                                    }
                                    span { "Try other accounts first" }
                                }
                            }
                            div { class: "bot-chain-policy-limit",
                                label { class: "tiny muted", r#for: "chain-max-switches", "Max switches" }
                                select {
                                    id: "chain-max-switches",
                                    class: "select",
                                    value: "{failover.max_switches}",
                                    disabled: !hydrated || !can_write || !failover.enabled,
                                    onchange: move |event| {
                                        if let Ok(count) = event.value().parse::<u8>() {
                                            let mut s = session.write();
                                            s.route_editor.draft.failover.max_switches = count.clamp(1, MAX_SWITCHES);
                                            s.route_editor.error = None;
                                            s.route_editor.notice = None;
                                        }
                                    },
                                    for count in 1..=MAX_SWITCHES {
                                        option { key: "{count}", value: "{count}", selected: count == failover.max_switches, "{count}" }
                                    }
                                }
                            }
                        }
                        if let Some(line) = last_switch {
                            p { class: "tiny muted", "Last automatic switch: {line}" }
                        }
                        if let Some(message) = error {
                            div { class: "banner err", role: "alert", span { "{message}" } }
                        }
                        if let Some(message) = notice {
                            div { class: "banner ok", role: "status", span { "{message}" } }
                        }
                        div { class: "bot-chain-foot",
                            div { class: "bot-chain-foot-status",
                                if dirty {
                                    span { class: "chip accent", role: "status", "Unsaved changes" }
                                } else {
                                    span { class: "tiny muted", "Applies to the next turn. A turn already running keeps its route." }
                                }
                            }
                            div { class: "bot-chain-foot-actions",
                                button {
                                    class: "btn ghost sm",
                                    r#type: "button",
                                    disabled: !dirty || saving,
                                    onclick: move |_| session.write().reset_route_editor(),
                                    "Reset"
                                }
                                button {
                                    class: "btn primary sm",
                                    r#type: "button",
                                    disabled: !can_save,
                                    onclick: move |_| session.write().save_bot_route(),
                                    if saving { "Saving\u{2026}" } else { "Save route" }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}
