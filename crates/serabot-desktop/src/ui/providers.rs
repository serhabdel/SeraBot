use super::*;

/// The "Add provider" sheet.
///
/// A preset fills the id, base URL, kind and secret name, and offers its
/// model ids as a starting list. The owner pastes the API key; the write
/// goes through the normal admin path (`Session::save_provider`).
#[component]
pub(super) fn AddProviderSheet(session: Signal<Session>, overlay: Signal<Overlay>) -> Element {
    let mut overlay = overlay;
    let mut selected = use_signal(String::new);
    let choices = provider_presets::presets()
        .iter()
        .map(|preset| (preset.id, preset.label))
        .collect::<Vec<_>>();
    let chosen = provider_presets::preset_for(&selected()).copied();
    let id = session.read().provider.id.clone();
    let kind = session.read().provider.kind.clone();
    let base_url = session.read().provider.base_url.clone();
    let models = session.read().provider.models.clone();
    let secret_name = session.read().provider.secret_name.clone();
    let api_key = session.read().provider.api_key.clone();
    let catalog_fetch = session.read().provider.catalog_fetch;
    let can_save = session.read().can_admin();
    rsx! {
        div {
            class: "scrim open",
            onclick: move |_| overlay.set(Overlay::None),
        }
        div {
            class: "sheet open",
            role: "dialog",
            onkeydown: move |event: KeyboardEvent| {
                if event.key() == Key::Escape {
                    overlay.set(Overlay::None);
                }
            },
            div { class: "sheet-head",
                h2 { class: "sheet-title", "Add provider" }
                button {
                    class: "icon-btn escape-close",
                    title: "Close",
                    "aria-label": "Close",
                    onclick: move |_| overlay.set(Overlay::None),
                    Icon { name: "x", size: 16 }
                }
            }
            div { class: "sheet-body",
                div { class: "field",
                    label { class: "field-label", "Preset" }
                    select {
                        class: "select",
                        value: "{selected()}",
                        onchange: move |event| {
                            let id = event.value();
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
                            div { style: "display: flex; align-items: center; gap: 10px;",
                                BrandLogo { brand: preset.id.to_owned(), size: 28 }
                                div {
                                    h3 { class: "card-title", "{preset.label}" }
                                    span { class: "card-sub", "{preset.subtitle}" }
                                }
                            }
                            span { class: "chip accent", "{preset.badge}" }
                        }
                        if preset.auth == "oauth" {
                            div { class: "banner info", style: "margin: 8px 0; display: flex; flex-direction: column; gap: 6px;",
                                strong { "Sign-in only" }
                                p { class: "tiny", "Save the provider first. Then press Sign in on its card on the Providers page: your browser opens, you approve, and the tokens go straight to the server's encrypted store. There is no API key to paste." }
                            }
                        } else if preset.sign_in == "pkce" {
                            div { class: "banner info", style: "margin: 8px 0; display: flex; flex-direction: column; gap: 6px;",
                                strong { "Two ways to connect" }
                                p { class: "tiny", "Paste an API key below, or save the provider and use Sign in with {preset.label} on its card. Sign-in creates a key for you and saves it to your account." }
                            }
                        } else if preset.auth == "none" {
                            div { class: "banner info", style: "margin: 8px 0;",
                                p { class: "tiny", "This is a local server. It needs no key and no sign-in. Change the address below if it runs on another machine." }
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
                        label { class: "field-label", "Provider id" }
                        input {
                            class: "input",
                            value: "{id}",
                            placeholder: "provider id (for example work)",
                            oninput: move |event| session.write().provider.id = event.value(),
                        }
                    }
                    div { class: "field",
                        label { class: "field-label", "Kind" }
                        select {
                            class: "select",
                            value: "{kind}",
                            onchange: move |event| session.write().provider.kind = event.value(),
                            for item in PROVIDER_KINDS {
                                option { value: "{item}", "{item}" }
                            }
                        }
                    }
                    div { class: "field",
                        label { class: "field-label", "Base URL" }
                        input {
                            class: "input",
                            value: "{base_url}",
                            placeholder: "https://api.example.com/v1",
                            oninput: move |event| session.write().provider.base_url = event.value(),
                        }
                    }
                    div { class: "field",
                        label { class: "field-label", "Models" }
                        input {
                            class: "input",
                            value: "{models}",
                            placeholder: "model ids, comma separated",
                            oninput: move |event| session.write().provider.models = event.value(),
                        }
                        span { class: "hint", "At least one model id is required." }
                    }
                    if chosen.is_some_and(|preset| preset.auth == "oauth") {
                        div { class: "field", style: "grid-column: 1 / -1;",
                            label { class: "field-label", "Sign-in" }
                            div { class: "banner info", style: "display: flex; flex-direction: column; gap: 6px;",
                                strong { "This provider signs in, it has no API key." }
                                p { class: "tiny", "After you save, use the Sign in button on this provider's card on the Providers page. Your browser opens, you approve, and the tokens land in the server's encrypted store ({secret_name})." }
                            }
                        }
                    } else {
                        div { class: "field",
                            label { class: "field-label", "Secret name" }
                            input {
                                class: "input",
                                value: "{secret_name}",
                                placeholder: "secret name (for example WORK_API_KEY)",
                                oninput: move |event| session.write().provider.secret_name = event.value(),
                            }
                        }
                        if chosen.is_none_or(|preset| preset.auth != "none") {
                            div { class: "field",
                                label { class: "field-label", "API key" }
                                input {
                                    class: "input",
                                    "type": "password",
                                    value: "{api_key}",
                                    placeholder: "API key (leave empty if already stored)",
                                    oninput: move |event| session.write().provider.api_key = event.value(),
                                }
                                if let Some(url) = chosen.map(|preset| preset.key_url).filter(|url| !url.is_empty()) {
                                    button {
                                        class: "btn ghost sm",
                                        style: "align-self: flex-start;",
                                        title: "Open the provider's site to create a key",
                                        onclick: move |_| session.write().open_in_browser(url.to_owned()),
                                        Icon { name: "external", size: 14 }
                                        "Get a key"
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
                    onclick: move |_| overlay.set(Overlay::None),
                    "Cancel"
                }
                button {
                    class: "btn primary",
                    disabled: !can_save,
                    onclick: move |_| {
                        session.write().save_provider();
                        overlay.set(Overlay::None);
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
    let providers = session.read().state.providers.clone();
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
        div { class: "card",
            div { class: "card-head",
                h3 { class: "card-title", "Token usage" }
                button {
                    class: "btn ghost sm",
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
    };
    let provider_cards: Vec<serabot_desktop::ProviderStatus> = providers.clone();
    let mut confirm = use_signal(|| Option::<String>::None);
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
                        onclick: move |_| overlay.set(Overlay::AddProvider),
                        Icon { name: "plus", size: 16 }
                        "Add provider"
                    }
                    button {
                        class: "btn ghost",
                        onclick: move |_| session.write().fetch_models(),
                        "Fetch models"
                    }
                    button {
                        class: "btn ghost",
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
                {usage_card}
                if !loaded {
                    div { class: "card",
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
                            onclick: move |_| overlay.set(Overlay::AddProvider),
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
                            let draft_key = session
                                .read()
                                .provider_key_drafts
                                .get(&provider.id)
                                .cloned()
                                .unwrap_or_default();
                            let has_key_text = !draft_key.trim().is_empty();
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
                            let sub = if provider.id == provider.kind || provider.id == "default" {
                                provider.kind.clone()
                            } else {
                                format!("{} ({})", provider.id, provider.kind)
                            };
                            rsx! {
                                div { class: if is_default { "card provider-card is-default" } else { "card provider-card" }, key: "{provider_id}",
                                    div { class: "card-head",
                                        div { style: "display: flex; align-items: center; gap: 12px; min-width: 0;",
                                            BrandLogo { brand: brand.clone(), size: 36 }
                                            div { style: "min-width: 0;",
                                                h3 { class: "card-title", "{title}" }
                                                span { class: "card-sub", "{sub}" }
                                            }
                                        }
                                        if is_default {
                                            span { class: "chip accent default-badge",
                                                Icon { name: "star", size: 12 }
                                                "Default"
                                            }
                                        }
                                    }
                                    div { class: "chips",
                                        span { class: "{conn.class()}", "{conn.label()}" }
                                        if managed {
                                            span { class: "chip accent", "managed" }
                                        }
                                        if !egress {
                                            span { class: "chip warn", "sending blocked" }
                                        }
                                    }
                                    p { class: "tiny", "{model_line}" }
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
                                            input {
                                                class: "input",
                                                "type": "password",
                                                value: "{draft_key}",
                                                placeholder: if provider.credential.own { "Paste a new key to replace yours" } else { "Paste your API key" },
                                                oninput: {
                                                    let id = provider.id.clone();
                                                    move |event| {
                                                        session.write().provider_key_drafts.insert(id.clone(), event.value());
                                                    }
                                                },
                                            }
                                            button {
                                                class: "btn primary sm",
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
                                                title: "Sign in on openrouter.ai. Your key is created for you and saved to your account.",
                                                disabled: !can_save || signing_in,
                                                onclick: {
                                                    let id = provider.id.clone();
                                                    let kind = provider.kind.clone();
                                                    move |_| session.write().sign_in_provider(id.clone(), kind.clone())
                                                },
                                                BrandLogo { brand: "openrouter".to_owned(), size: 16 }
                                                if signing_in { "Waiting for browser..." } else { "Sign in with OpenRouter" }
                                            }
                                        }
                                        if is_oauth {
                                            button {
                                                class: if conn.is_connected() { "btn ghost sm" } else { "btn primary sm" },
                                                title: "Sign in through your browser. Tokens are stored on the server and never shown here.",
                                                disabled: !can_save || signing_in,
                                                onclick: {
                                                    let id = provider.id.clone();
                                                    let kind = provider.kind.clone();
                                                    move |_| session.write().sign_in_provider(id.clone(), kind.clone())
                                                },
                                                BrandLogo { brand: brand.clone(), size: 16 }
                                                if signing_in {
                                                    "Waiting for browser..."
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
                                                title: "Open the provider's site to create a key",
                                                onclick: move |_| session.write().open_in_browser(url.to_owned()),
                                                Icon { name: "external", size: 14 }
                                                "Get a key"
                                            }
                                        }
                                        button {
                                            class: "btn ghost sm",
                                            title: "Ask the provider for its model list with your key",
                                            disabled: !can_save || testing || !conn.is_connected(),
                                            onclick: {
                                                let id = provider.id.clone();
                                                move |_| session.write().test_provider(id.clone())
                                            },
                                            Icon { name: "refresh", size: 14 }
                                            if testing { "Testing..." } else { "Test" }
                                        }
                                        if !is_default && conn.is_connected() && star_model.is_some() {
                                            button {
                                                class: "btn ghost sm",
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
                                            div { class: "row",
                                                span { class: "tiny", "Remove {provider_id}? Conversations pinned to it must pick another route." }
                                                button {
                                                    class: "btn danger sm",
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
                                                    onclick: move |_| confirm.set(None),
                                                    "Cancel"
                                                }
                                            }
                                        } else {
                                            button {
                                                class: "btn ghost sm",
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
                div { class: "card",
                    div { class: "card-head",
                        div {
                            h3 { class: "card-title", "Provider Directory — Available Integrations" }
                            p { class: "muted", "All supported model providers and gateways. Connect in one click or configure credentials." }
                        }
                    }
                    div { class: "integration-grid",
                        for preset in provider_presets::presets() {
                            {
                                let is_connected = providers.iter().any(|p| {
                                    (p.id == preset.id
                                        || (p.kind == preset.kind && preset.kind != "openai-compatible"))
                                        && (p.credential.configured || p.auth == "oauth" && p.secrets.values().any(|s| s.configured))
                                });
                                rsx! {
                                    div { class: "integration-card", key: "{preset.id}",
                                        div {
                                            div { class: "card-head", style: "align-items: flex-start; margin-bottom: 8px;",
                                                div { style: "display: flex; align-items: center; gap: 10px;",
                                                    BrandLogo { brand: preset.id.to_owned(), size: 30 }
                                                    div {
                                                        h4 { style: "margin: 0; font-size: 0.98rem; font-weight: 600;", "{preset.label}" }
                                                        span { class: "card-sub", style: "font-size: 0.78rem;", "{preset.kind}" }
                                                    }
                                                }
                                                span { class: "chip accent", style: "font-size: 0.72rem; padding: 2px 7px;", "{preset.badge}" }
                                            }
                                            p { class: "tiny", style: "margin-bottom: 10px; line-height: 1.45;", "{preset.subtitle}" }
                                            div { class: "chips", style: "margin-bottom: 12px;",
                                                for model in preset.models.iter().take(3) {
                                                    span { class: "chip", style: "font-size: 0.7rem;", "{model}" }
                                                }
                                                if preset.models.len() > 3 {
                                                    span { class: "chip muted", style: "font-size: 0.7rem;", "+{preset.models.len() - 3} more" }
                                                }
                                            }
                                        }
                                        div { style: "display: flex; align-items: center; justify-content: space-between; border-top: 1px solid var(--hairline); padding-top: 10px; margin-top: 6px;",
                                            span {
                                                class: if is_connected { "status-pill connected" } else { "status-pill disconnected" },
                                                if is_connected { "Connected" } else { "Not connected" }
                                            }
                                            button {
                                                class: if is_connected { "btn ghost sm" } else { "btn primary sm" },
                                                onclick: {
                                                    let preset = *preset;
                                                    move |_| {
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
                                                        draft.provider.api_key.clear();
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
