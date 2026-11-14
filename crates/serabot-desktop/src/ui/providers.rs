use super::*;
use crate::ui_widgets::StatusChip;
use serabot_desktop::provider_presets::{DirectoryFilter, filter_presets, provider_matches_preset};
use serabot_desktop::theme::Tone;
use std::collections::BTreeSet;

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
            "aria-label": "Add provider",
            tabindex: "0",
            onkeydown: move |event: KeyboardEvent| {
                if event.key() == Key::Escape {
                    event.prevent_default();
                    overlay.set(Overlay::None);
                }
            },
            div { class: "sheet-head",
                h2 { class: "sheet-title", "Add provider" }
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
                                    "Sign-in only. Save the provider first, then press Sign in on its card on the Providers page."
                                }
                            }
                        } else if preset.sign_in == "pkce" {
                            div { class: "banner info",
                                span {
                                    "Paste an API key below, or save and use Sign in with {preset.label} on its card."
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
                                    "After saving, use the Sign in button on this provider's card. Tokens are encrypted on the server ({secret_name})."
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
                    "Save provider"
                }
            }
        }
    }
}

#[component]
pub(super) fn ProvidersView(session: Signal<Session>, overlay: Signal<Overlay>) -> Element {
    let mut overlay = overlay;
    let mut dir_query = use_signal(String::new);
    let mut dir_filter = use_signal(DirectoryFilter::default);
    let mut revealed_keys = use_signal(BTreeSet::<String>::new);
    let providers = session.read().state.providers.clone();
    let provider_tests = session.read().state.provider_tests.clone();
    let loaded = session.read().state.providers_loaded;
    let can_save = session.read().can_admin();
    let usage = session.read().state.usage.clone();
    // Per-day rows preformatted: (day label, "N tokens · M calls").
    let usage_days: Vec<(String, String)> = usage
        .as_ref()
        .map(|report| {
            report
                .per_day
                .iter()
                .take(14)
                .map(|bucket| {
                    (
                        bucket.day.clone().unwrap_or_else(|| "—".to_owned()),
                        format!("{} tokens · {} calls", bucket.total_tokens, bucket.calls),
                    )
                })
                .collect()
        })
        .unwrap_or_default();
    let usage_card = rsx! {
        details { class: "card advanced",
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
                if let Some(report) = usage.clone() {
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
                    for (day, summary) in usage_days.clone() {
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
    };
    let provider_cards: Vec<serabot_desktop::ProviderStatus> = providers.clone();
    let mut confirm = use_signal(|| Option::<String>::None);
    let preset_is_connected = |preset: &provider_presets::ProviderPreset| {
        providers.iter().any(|p| {
            provider_matches_preset(&p.id, &p.kind, preset)
                && (p.credential.configured
                    || (p.auth == "oauth" && p.secrets.values().any(|s| s.configured)))
        })
    };
    let filtered_presets = filter_presets(&dir_query(), dir_filter(), preset_is_connected);
    let dir_filtering = !dir_query().trim().is_empty() || dir_filter() != DirectoryFilter::All;
    rsx! {
        div { class: "pane",
            div { class: "pane-head",
                div {
                    h1 { class: "pane-title", "Providers" }
                    p { class: "muted", "Add a provider, pull its live models, and pick the route per conversation." }
                }
                div { class: "row",
                    button {
                        class: "btn primary",
                        r#type: "button",
                        onclick: move |_| {
                            let mut s = session.write();
                            s.provider = Default::default();
                            s.admin_error = None;
                            drop(s);
                            overlay.set(Overlay::AddProvider);
                        },
                        Icon { name: "plus", size: 16 }
                        "Add provider"
                    }
                    button {
                        class: "btn ghost",
                        r#type: "button",
                        onclick: move |_| session.write().fetch_models(),
                        "Fetch models"
                    }
                    button {
                        class: "btn ghost",
                        r#type: "button",
                        onclick: move |_| session.write().refresh_providers(),
                        "Refresh"
                    }
                }
            }
            div { class: "pane-body",
                div { class: "card",
                    ModelBar { session }
                    p { class: "tiny", "Applies to the next turn; a running turn keeps its pinned route." }
                }
                if !loaded {
                    div { class: "card", role: "status", "aria-busy": "true",
                        div { class: "skeleton title" }
                        div { class: "skeleton block" }
                    }
                }
                if loaded && provider_cards.is_empty() {
                    div { class: "empty",
                        div { class: "empty-icon", Icon { name: "providers", size: 20 } }
                        h2 { class: "empty-title", "No AI providers yet" }
                        p { class: "empty-text", "Add a provider and an API key so your bots can answer." }
                        button {
                            class: "btn primary",
                            r#type: "button",
                            onclick: move |_| {
                                let mut s = session.write();
                                s.provider = Default::default();
                                s.admin_error = None;
                                drop(s);
                                overlay.set(Overlay::AddProvider);
                            },
                            Icon { name: "plus", size: 16 }
                            "Add provider"
                        }
                    }
                }
                div { class: "card-grid provider-grid",
                    for provider in provider_cards {
                        {
                            let provider_id = provider.id.clone();
                            let signed_in = provider_signed_in(&provider.kind, &provider.secrets);
                            let conn = provider_view::connection(&provider, signed_in);
                            let is_oauth = provider.auth == "oauth";
                            let preset = provider_presets::presets()
                                .iter()
                                .find(|preset| preset.id == provider.id)
                                .or_else(|| {
                                    provider_presets::presets().iter().find(|preset| {
                                        preset.kind == provider.kind && preset.kind != "openai-compatible"
                                    })
                                })
                                .copied();
                            let brand = if branding::brand_label(&provider.id).is_some() {
                                provider.id.clone()
                            } else {
                                provider.kind.clone()
                            };
                            let title = provider_view::display_name(
                                &provider.id,
                                &provider.kind,
                                branding::brand_label(&provider.kind),
                            );
                            let key_url = preset.map(|p| p.key_url).filter(|url| !url.is_empty());
                            let takes_key = !is_oauth && preset.is_none_or(|p| p.auth != "none");
                            let pkce_sign_in = provider.kind == "openrouter";
                            let signing_in = session.read().oauth_signing_in.as_deref() == Some(provider.id.as_str());
                            let testing = session.read().provider_testing.as_deref() == Some(provider.id.as_str());
                            let test_outcome = provider_tests.get(&provider.id).map(provider_view::format_provider_test);
                            let draft_key = session
                                .read()
                                .provider_key_drafts
                                .get(&provider.id)
                                .cloned()
                                .unwrap_or_default();
                            let has_key_text = !draft_key.trim().is_empty();
                            let key_revealed = has_key_text && revealed_keys.read().contains(&provider.id);
                            let model_line = provider_view::model_line(&provider);
                            let shown: Vec<String> = provider.models.iter().take(6).cloned().collect();
                            let extra = provider.models.len().saturating_sub(shown.len());
                            let star_model = provider
                                .default_model
                                .clone()
                                .or_else(|| provider.models.first().cloned());
                            let is_default = provider.is_default;
                            let managed = provider.managed;
                            let routing = provider.routing_classes.join(", ");
                            let egress = provider.data_egress_approved;
                            let kind_label = provider_view::human_kind(&provider.kind);
                            let sub = if provider.id == provider.kind || provider.id == "default" {
                                kind_label.to_owned()
                            } else {
                                format!("{} ({kind_label})", provider.id)
                            };
                            rsx! {
                                div { class: if is_default { "card provider-card is-default" } else { "card provider-card" }, key: "{provider_id}",
                                    div { class: "card-head",
                                        div { class: "provider-card-head",
                                            BrandLogo { brand: brand.clone(), size: 36 }
                                            div { class: "provider-card-title",
                                                h3 { class: "card-title", "{title}" }
                                                span { class: "card-sub provider-card-sub", "{sub}" }
                                            }
                                        }
                                        if is_default {
                                            span { class: "chip accent default-badge",
                                                Icon { name: "star", size: 12 }
                                                "Default"
                                            }
                                        }
                                    }
                                    div { class: "chips provider-card-badges",
                                        StatusChip { tone: conn.tone(), text: conn.label().to_owned() }
                                        if managed {
                                            span { class: "chip accent", "Custom" }
                                        }
                                        if !egress {
                                            StatusChip { tone: Tone::Warn, text: "Data egress off".to_owned() }
                                        }
                                    }
                                    p { class: "tiny", "{model_line}" }
                                    if let Some((test_tone, test_text)) = test_outcome {
                                        div { class: "provider-test-line", role: "status",
                                            StatusChip { tone: test_tone, text: test_text }
                                        }
                                    }
                                    div { class: "chips",
                                        for model in shown {
                                            span { class: "chip", "{model}" }
                                        }
                                        if extra > 0 {
                                            span { class: "chip muted", "+{extra} more" }
                                        }
                                    }
                                    if !routing.is_empty() {
                                        p { class: "tiny", "Used for: {routing}" }
                                    }
                                    if takes_key {
                                        div { class: "provider-key-row",
                                            div { class: "secret-field",
                                                input {
                                                    class: "input",
                                                    r#type: if key_revealed { "text" } else { "password" },
                                                    autocomplete: "off",
                                                    spellcheck: false,
                                                    "aria-label": "API key for {title}",
                                                    value: "{draft_key}",
                                                    placeholder: if provider.credential.own { "Paste a new key to replace yours" } else { "Paste your API key" },
                                                    oninput: {
                                                        let id = provider.id.clone();
                                                        move |event| {
                                                            let next = event.value();
                                                            if next.is_empty() {
                                                                revealed_keys.write().remove(&id);
                                                            }
                                                            session.write().provider_key_drafts.insert(id.clone(), next);
                                                        }
                                                    },
                                                    onkeydown: {
                                                        let id = provider.id.clone();
                                                        move |event: KeyboardEvent| {
                                                            if event.key() == Key::Enter && can_save && has_key_text {
                                                                event.prevent_default();
                                                                session.write().save_provider_key(id.clone());
                                                            }
                                                        }
                                                    },
                                                }
                                                button {
                                                    class: "btn icon ghost sm secret-toggle",
                                                    r#type: "button",
                                                    title: if key_revealed { "Hide key" } else { "Show key" },
                                                    "aria-label": if key_revealed { format!("Hide API key for {title}") } else { format!("Show API key for {title}") },
                                                    "aria-pressed": if key_revealed { "true" } else { "false" },
                                                    onclick: {
                                                        let id = provider.id.clone();
                                                        move |_| {
                                                            let mut set = revealed_keys.write();
                                                            if !set.remove(&id) {
                                                                set.insert(id.clone());
                                                            }
                                                        }
                                                    },
                                                    Icon { name: if key_revealed { "eye-off".to_owned() } else { "eye".to_owned() }, size: 14 }
                                                }
                                            }
                                            button {
                                                class: "btn primary sm",
                                                r#type: "button",
                                                disabled: !can_save || !has_key_text,
                                                title: "Saved to your account only, encrypted. It is never shown again.",
                                                onclick: {
                                                    let id = provider.id.clone();
                                                    move |_| session.write().save_provider_key(id.clone())
                                                },
                                                if provider.credential.own { "Replace key" } else { "Save key" }
                                            }
                                        }
                                    }
                                    div { class: "card-actions",
                                        if pkce_sign_in {
                                            button {
                                                class: "btn ghost sm",
                                                r#type: "button",
                                                title: "Sign in on openrouter.ai. Your key is created for you and saved to your account.",
                                                disabled: !can_save || signing_in,
                                                onclick: {
                                                    let id = provider.id.clone();
                                                    let kind = provider.kind.clone();
                                                    move |_| session.write().sign_in_provider(id.clone(), kind.clone())
                                                },
                                                BrandLogo { brand: "openrouter".to_owned(), size: 16 }
                                                if signing_in { "Waiting for browser…" } else { "Sign in with OpenRouter" }
                                            }
                                        }
                                        if is_oauth {
                                            button {
                                                class: if conn.is_connected() { "btn ghost sm" } else { "btn primary sm" },
                                                r#type: "button",
                                                title: "Sign in through your browser. Tokens are stored on the server and never shown here.",
                                                disabled: !can_save || signing_in,
                                                onclick: {
                                                    let id = provider.id.clone();
                                                    let kind = provider.kind.clone();
                                                    move |_| session.write().sign_in_provider(id.clone(), kind.clone())
                                                },
                                                BrandLogo { brand: brand.clone(), size: 16 }
                                                if signing_in {
                                                    "Waiting for browser…"
                                                } else if conn.is_connected() {
                                                    "Sign in again"
                                                } else {
                                                    "Sign in with {title}"
                                                }
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
                                        button {
                                            class: "btn ghost sm",
                                            r#type: "button",
                                            title: "Ask the provider for its model list with your key",
                                            disabled: !can_save || testing || !conn.is_connected(),
                                            onclick: {
                                                let id = provider.id.clone();
                                                move |_| session.write().test_provider(id.clone())
                                            },
                                            Icon { name: "refresh", size: 14 }
                                            if testing { "Testing…" } else { "Test" }
                                        }
                                        if !is_default && conn.is_connected() && star_model.is_some() {
                                            button {
                                                class: "btn ghost sm",
                                                r#type: "button",
                                                title: "Use this provider for new chats",
                                                disabled: !can_save,
                                                onclick: {
                                                    let id = provider.id.clone();
                                                    let model = star_model.clone().unwrap_or_default();
                                                    move |_| session.write().set_default_provider(id.clone(), model.clone())
                                                },
                                                Icon { name: "star", size: 14 }
                                                "Set as default"
                                            }
                                        }
                                    }
                                    if managed {
                                        if confirm() == Some(provider_id.clone()) {
                                            div { class: "mp-confirm", role: "alertdialog", "aria-label": "Confirm removing provider",
                                                span { class: "tiny", "Remove {provider_id}? Conversations pinned to it must pick another route." }
                                                button {
                                                    class: "btn danger sm",
                                                    r#type: "button",
                                                    disabled: !can_save,
                                                    onclick: {
                                                        let id = provider_id.clone();
                                                        move |_| {
                                                            session.write().remove_provider(id.clone());
                                                            confirm.set(None);
                                                        }
                                                    },
                                                    "Remove"
                                                }
                                                button {
                                                    class: "btn ghost sm",
                                                    r#type: "button",
                                                    onclick: move |_| confirm.set(None),
                                                    "Cancel"
                                                }
                                            }
                                        } else {
                                            button {
                                                class: "btn ghost sm",
                                                r#type: "button",
                                                onclick: {
                                                    let id = provider_id.clone();
                                                    move |_| confirm.set(Some(id.clone()))
                                                },
                                                "Remove provider"
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
                {usage_card}
                div { class: "card",
                    div { class: "card-head",
                        div {
                            h3 { class: "card-title", "Provider Directory" }
                            p { class: "muted", "Supported model providers and gateways. Connect a preset or customize endpoints." }
                        }
                    }
                    div { class: "provider-dir-toolbar",
                        div { class: "search-wrap",
                            input {
                                class: "input mp-search",
                                r#type: "search",
                                placeholder: "Search providers or models",
                                "aria-label": "Search provider directory",
                                value: "{dir_query}",
                                oninput: move |event| dir_query.set(event.value()),
                                onkeydown: move |event: KeyboardEvent| {
                                    if event.key() == Key::Escape && !dir_query.read().is_empty() {
                                        event.prevent_default();
                                        dir_query.set(String::new());
                                    }
                                },
                            }
                            if !dir_query().trim().is_empty() {
                                button {
                                    class: "btn ghost sm",
                                    r#type: "button",
                                    onclick: move |_| dir_query.set(String::new()),
                                    "Clear"
                                }
                            }
                        }
                        div { class: "seg", role: "group", "aria-label": "Filter providers",
                            for filter_opt in DirectoryFilter::ALL {
                                button {
                                    key: "{filter_opt.label()}",
                                    r#type: "button",
                                    class: if dir_filter() == filter_opt { "seg-btn active" } else { "seg-btn" },
                                    "aria-pressed": if dir_filter() == filter_opt { "true" } else { "false" },
                                    onclick: move |_| dir_filter.set(filter_opt),
                                    "{filter_opt.label()}"
                                }
                            }
                        }
                    }
                    if filtered_presets.is_empty() {
                        div { class: "empty compact",
                            h4 { class: "empty-title", "No matching providers" }
                            p { class: "empty-text", "Try clearing your search or switching the filter." }
                            if dir_filtering {
                                button {
                                    class: "btn ghost sm",
                                    r#type: "button",
                                    onclick: move |_| {
                                        dir_query.set(String::new());
                                        dir_filter.set(DirectoryFilter::All);
                                    },
                                    "Clear filters"
                                }
                            }
                        }
                    } else {
                        div { class: "integration-grid",
                            for preset in filtered_presets {
                                {
                                    let existing = providers
                                        .iter()
                                        .find(|p| provider_matches_preset(&p.id, &p.kind, preset))
                                        .cloned();
                                    let is_connected = existing.as_ref().is_some_and(|p| {
                                        p.credential.configured
                                            || (p.auth == "oauth" && p.secrets.values().any(|s| s.configured))
                                    });
                                    rsx! {
                                        div { class: "integration-card", key: "{preset.id}",
                                            div {
                                                div { class: "card-head",
                                                    div { class: "provider-card-head",
                                                        BrandLogo { brand: preset.id.to_owned(), size: 30 }
                                                        div { class: "provider-card-title",
                                                            h4 { class: "card-title", "{preset.label}" }
                                                            span { class: "card-sub provider-card-sub",
                                                                "{provider_view::human_kind(preset.kind)}"
                                                            }
                                                        }
                                                    }
                                                    span { class: "chip accent", "{preset.badge}" }
                                                }
                                                p { class: "tiny", "{preset.subtitle}" }
                                                div { class: "chips",
                                                    for model in preset.models.iter().take(3) {
                                                        span { class: "chip", "{model}" }
                                                    }
                                                    if preset.models.len() > 3 {
                                                        span { class: "chip muted", "+{preset.models.len() - 3} more" }
                                                    }
                                                }
                                            }
                                            div { class: "card-actions",
                                                StatusChip {
                                                    tone: if is_connected { Tone::Ok } else { Tone::Muted },
                                                    text: if is_connected { "Connected".to_owned() } else { "Not connected".to_owned() },
                                                }
                                                button {
                                                    class: if is_connected { "btn ghost sm" } else { "btn primary sm" },
                                                    r#type: "button",
                                                    onclick: {
                                                        let preset = *preset;
                                                        let existing = existing.clone();
                                                        move |_| {
                                                            let mut draft = session.write();
                                                            if let Some(ref cur) = existing {
                                                                draft.provider.id = cur.id.clone();
                                                                draft.provider.kind = cur.kind.clone();
                                                                draft.provider.base_url = preset.base_url.to_owned();
                                                                draft.provider.secret_name = cur
                                                                    .secrets
                                                                    .values()
                                                                    .find_map(|s| s.name.clone())
                                                                    .filter(|n| !n.is_empty())
                                                                    .unwrap_or_else(|| preset.secret_name.to_owned());
                                                                draft.provider.models = cur.models.join(", ");
                                                                draft.provider.catalog_fetch = cur.catalog_fetch;
                                                                draft.provider.auth = cur.auth.clone();
                                                            } else {
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
                                                            }
                                                            draft.provider.api_key.clear();
                                                            draft.admin_error = None;
                                                            drop(draft);
                                                            overlay.set(Overlay::AddProvider);
                                                        }
                                                    },
                                                    if is_connected { "Configure" } else { "Connect" }
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
}
