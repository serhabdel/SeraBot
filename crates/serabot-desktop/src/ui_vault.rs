//! The Vault page (design B, contract section 3; a rail item since 0.16).
//!
//! One vault per user, shared by all of that user's bots. The page lists
//! entries (never a value), adds or replaces one through a form whose value field is
//! write-only, and deletes after a confirm step. Requests run on the panel
//! worker (`panel.rs`); this file only draws and forwards clicks.

use crate::ui::Icon;
use dioxus::prelude::*;
use serabot_desktop::panel::last_used_label;
use serabot_desktop::session::Session;
use serabot_desktop::{PasswordManager, PasswordManagerProbe, VaultEntry, VaultKind, VaultSource};
use zeroize::{Zeroize, Zeroizing};

fn kind_icon(kind: VaultKind) -> &'static str {
    match kind {
        VaultKind::Password => "lock",
        VaultKind::ApiKey => "key",
        VaultKind::Note => "edit",
    }
}

/// The Vault page: the passwords and keys list, or the add / replace form.
/// One vault per user, shared by all of that user's bots; the page lists
/// entries (never a value), adds or replaces one through a form whose value
/// field is write-only, and deletes after a confirm step.
#[component]
pub fn VaultPage(session: Signal<Session>) -> Element {
    let mut session = session;

    let (loaded, loading, error, notice, entries, has_form) = {
        let s = session.read();
        let v = &s.panel.vault;
        (
            v.loaded,
            v.loading,
            v.error.clone(),
            v.notice.clone(),
            v.entries.clone(),
            v.form.is_some(),
        )
    };

    rsx! {
        div {
            class: "pane vault-page",
            onkeydown: move |event: KeyboardEvent| {
                if event.key() == Key::Escape && session.read().panel.vault.form.is_some() {
                    event.stop_propagation();
                    session.write().panel.close_form();
                }
            },
            div { class: "pane-head",
                div {
                    h1 { class: "pane-title", "Vault" }
                    p { class: "muted",
                        "Passwords and keys your bots can use. They never see the values."
                    }
                }
                if !has_form && loaded && !entries.is_empty() {
                    button {
                        class: "btn primary",
                        onclick: move |_| session.write().panel.open_add_form(),
                        Icon { name: "plus", size: 15 }
                        "Add"
                    }
                }
            }
            div { class: "pane-body",
                div { class: "vault-panel",
                    if let Some(message) = error {
                        div { class: "error-text vault-error", role: "alert", "{message}" }
                    }
                    if has_form {
                        VaultFormView { session }
                    } else {
                        if let Some(message) = notice {
                            div { class: "vault-notice", role: "status",
                                Icon { name: "check", size: 14 }
                                span { "{message}" }
                            }
                        }
                        PasswordManagersPanel { session }
                        if loading && !loaded {
                            div { class: "vault-list", "aria-busy": "true",
                                div { class: "skeleton row", " " }
                                div { class: "skeleton row", " " }
                            }
                        } else if loaded && entries.is_empty() {
                            div { class: "vault-empty",
                                div { class: "empty-icon", Icon { name: "key", size: 20 } }
                                h2 { class: "empty-title", "Nothing saved yet" }
                                p { class: "empty-text",
                                    "Add a login or an API key. Your bots can use it to sign in for you, but never see the actual value."
                                }
                                button {
                                    class: "btn primary",
                                    onclick: move |_| session.write().panel.open_add_form(),
                                    "Add"
                                }
                            }
                        } else if !loaded {
                            // The first load failed: say so, offer a retry.
                            div { class: "vault-empty",
                                button {
                                    class: "btn",
                                    onclick: move |_| session.write().panel.ask_vault_list(),
                                    "Try again"
                                }
                            }
                        } else {
                            ul { class: "vault-list", role: "list",
                                for entry in entries {
                                    VaultRow { key: "{entry.name}", session, entry }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

#[component]
fn VaultRow(session: Signal<Session>, entry: VaultEntry) -> Element {
    let mut session = session;
    let (confirming, deleting) = {
        let s = session.read();
        (
            s.panel.vault.confirm_delete.as_deref() == Some(entry.name.as_str()),
            s.panel.vault.deleting.as_deref() == Some(entry.name.as_str()),
        )
    };
    let mut meta = vec![entry.kind.label().to_owned()];
    if entry.source.is_reference() {
        meta.push(entry.source.label().to_owned());
    }
    if let Some(user) = entry.username.as_deref().filter(|u| !u.is_empty()) {
        meta.push(user.to_owned());
    }
    let meta = meta.join(" \u{b7} ");
    let used = last_used_label(entry.last_used_at.as_deref());
    let name = entry.name.clone();
    let name_replace = entry.name.clone();
    let name_delete = entry.name.clone();
    rsx! {
        li { class: if deleting { "vault-row busy" } else { "vault-row" },
            span { class: "vault-kind", title: "{entry.kind.label()}",
                Icon { name: kind_icon(entry.kind), size: 15 }
            }
            div { class: "vault-row-main",
                strong { class: "vault-name", "{name}" }
                span { class: "vault-meta caption", "{meta}" }
                if !entry.domains.is_empty() {
                    div { class: "vault-chips",
                        for domain in entry.domains.iter() {
                            span { key: "{domain}", class: "chip mono", "{domain}" }
                        }
                    }
                }
                span { class: "vault-used tiny", "{used}" }
            }
            if confirming {
                div { class: "vault-confirm", role: "group", "aria-label": "Confirm delete",
                    span { class: "caption", "Delete?" }
                    button {
                        class: "btn danger sm",
                        onclick: move |_| session.write().panel.confirm_delete(),
                        "Delete"
                    }
                    button {
                        class: "btn ghost sm",
                        onclick: move |_| session.write().panel.cancel_delete(),
                        "Cancel"
                    }
                }
            } else {
                div { class: "vault-actions",
                    button {
                        class: "icon-btn",
                        title: "Edit",
                        "aria-label": "Edit {name_replace}",
                        disabled: deleting,
                        onclick: move |_| session.write().panel.open_replace_form(&name_replace),
                        Icon { name: "edit", size: 15 }
                    }
                    button {
                        class: "icon-btn",
                        title: "Delete",
                        "aria-label": "Delete {name_delete}",
                        disabled: deleting,
                        onclick: move |_| session.write().panel.ask_delete(&name_delete),
                        Icon { name: "trash", size: 15 }
                    }
                }
            }
        }
    }
}

/// Add / Replace. The value input is uncontrolled and never given a `value`
/// attribute, so nothing the user typed is ever rendered back; the form is
/// dropped (and the value wiped) as soon as the save succeeds.
#[component]
fn VaultFormView(session: Signal<Session>) -> Element {
    let mut session = session;
    let (
        replacing,
        name,
        kind,
        username,
        domains,
        domain_draft,
        note,
        error,
        domain_error,
        saving,
        name_hint,
    ) = {
        let s = session.read();
        let Some(form) = s.panel.vault.form.as_ref() else {
            return rsx! {};
        };
        (
            form.replacing,
            form.name.clone(),
            form.kind,
            form.username.clone(),
            form.domains.clone(),
            form.domain_draft.clone(),
            form.note.clone(),
            form.error.clone(),
            form.domain_error.clone(),
            form.saving,
            form.name_hint(),
        )
    };
    let (source, reference) = {
        let s = session.read();
        match s.panel.vault.form.as_ref() {
            Some(form) => (form.source, form.reference.clone()),
            None => (VaultSource::Local, String::new()),
        }
    };
    let value_label = match kind {
        VaultKind::Password => "Password",
        VaultKind::ApiKey => "API key",
        VaultKind::Note => "Note text",
    };
    let title = if replacing {
        "Edit saved item"
    } else {
        "Add a password or key"
    };
    rsx! {
        form {
            class: "vault-form",
            onsubmit: move |event| {
                event.prevent_default();
                session.write().panel.save_vault_form();
            },
            h3 { class: "vault-form-title", "{title}" }
            div { class: "vault-form-body",
                div { class: "field",
                    label { class: "field-label", r#for: "vault-name", "Name" }
                    input {
                        id: "vault-name",
                        class: "input",
                        value: "{name}",
                        readonly: replacing,
                        autocomplete: "off",
                        placeholder: "e.g. venue-portal",
                        "aria-invalid": if name_hint.is_some() { "true" } else { "false" },
                        oninput: move |event| {
                            if let Some(form) = session.write().panel.vault.form.as_mut() {
                                form.name = event.value();
                            }
                        },
                    }
                    if let Some(hint) = name_hint {
                        span { class: "error-text tiny", "{hint}" }
                    }
                }
                div { class: "field",
                    span { class: "field-label", "Value comes from" }
                    div { class: "seg", role: "radiogroup", "aria-label": "Value comes from",
                        for option in VaultSource::ALL {
                            button {
                                key: "{option.label()}",
                                r#type: "button",
                                role: "radio",
                                class: if option == source { "seg-btn active" } else { "seg-btn" },
                                "aria-checked": if option == source { "true" } else { "false" },
                                onclick: move |_| {
                                    if let Some(form) = session.write().panel.vault.form.as_mut() {
                                        form.source = option;
                                    }
                                },
                                "{option.label()}"
                            }
                        }
                    }
                }
                div { class: "field",
                    span { class: "field-label", "Type" }
                    div { class: "seg", role: "radiogroup", "aria-label": "Type",
                        for option in VaultKind::ALL {
                            button {
                                key: "{option.as_str()}",
                                r#type: "button",
                                role: "radio",
                                class: if option == kind { "seg-btn active" } else { "seg-btn" },
                                "aria-checked": if option == kind { "true" } else { "false" },
                                onclick: move |_| {
                                    if let Some(form) = session.write().panel.vault.form.as_mut() {
                                        form.kind = option;
                                    }
                                },
                                "{option.label()}"
                            }
                        }
                    }
                }
                if kind == VaultKind::Password {
                    div { class: "field",
                        label { class: "field-label", r#for: "vault-user", "Username" }
                        input {
                            id: "vault-user",
                            class: "input",
                            value: "{username}",
                            autocomplete: "off",
                            placeholder: "you@example.com",
                            oninput: move |event| {
                                if let Some(form) = session.write().panel.vault.form.as_mut() {
                                    form.username = event.value();
                                }
                            },
                        }
                    }
                }
                if source.is_reference() {
                    div { class: "field",
                        label { class: "field-label", r#for: "vault-reference", "Item reference" }
                        input {
                            id: "vault-reference",
                            class: "input mono",
                            value: "{reference}",
                            autocomplete: "off",
                            spellcheck: false,
                            placeholder: if source == VaultSource::ProtonPass { "pass://Vault/Item/password" } else { "op://Vault/Item/password" },
                            oninput: move |event| {
                                if let Some(form) = session.write().panel.vault.form.as_mut() {
                                    form.reference = event.value();
                                }
                            },
                        }
                        span { class: "tiny vault-hint",
                            "The password is read from {source.label()} each time a bot types it. Nothing is copied here."
                        }
                    }
                } else {
                div { class: "field",
                    label { class: "field-label", r#for: "vault-value", "{value_label}" }
                    input {
                        id: "vault-value",
                        class: "input",
                        r#type: "password",
                        autocomplete: "new-password",
                        spellcheck: false,
                        placeholder: if replacing { "Enter the new value" } else { "Stored encrypted, never shown again" },
                        oninput: move |event| {
                            if let Some(form) = session.write().panel.vault.form.as_mut() {
                                let typed = event.value();
                                (*form.value).zeroize();
                                form.value.push_str(&typed);
                            }
                        },
                    }
                    span { class: "tiny vault-hint",
                        if replacing {
                            "Saved securely. You can't view it again, so enter the new one."
                        } else {
                            "Saved securely. You can't view it again."
                        }
                    }
                }
                }
                div { class: "field",
                    label { class: "field-label", r#for: "vault-domains", "Websites" }
                    div { class: "vault-chips editable",
                        for domain in domains.iter() {
                            span { key: "{domain}", class: "chip mono removable",
                                "{domain}"
                                button {
                                    r#type: "button",
                                    class: "chip-x",
                                    title: "Remove {domain}",
                                    "aria-label": "Remove {domain}",
                                    onclick: {
                                        let domain = domain.clone();
                                        move |_| {
                                            if let Some(form) = session.write().panel.vault.form.as_mut() {
                                                form.remove_domain(&domain);
                                            }
                                        }
                                    },
                                    Icon { name: "x", size: 11 }
                                }
                            }
                        }
                        input {
                            id: "vault-domains",
                            class: "chip-input",
                            value: "{domain_draft}",
                            autocomplete: "off",
                            placeholder: if domains.is_empty() { "example.com" } else { "add another" },
                            "aria-invalid": if domain_error.is_some() { "true" } else { "false" },
                            oninput: move |event| {
                                if let Some(form) = session.write().panel.vault.form.as_mut() {
                                    form.domain_draft = event.value();
                                }
                            },
                            onkeydown: move |event: KeyboardEvent| {
                                let key = event.key();
                                if key == Key::Enter || key == Key::Character(",".into()) || key == Key::Character(" ".into()) {
                                    event.prevent_default();
                                    event.stop_propagation();
                                    if let Some(form) = session.write().panel.vault.form.as_mut() {
                                        form.commit_domain();
                                    }
                                }
                            },
                            onblur: move |_| {
                                if let Some(form) = session.write().panel.vault.form.as_mut() {
                                    form.commit_domain();
                                }
                            },
                        }
                    }
                    if let Some(message) = domain_error.clone() {
                        span { class: "error-text tiny", "{message}" }
                    } else {
                        span { class: "tiny vault-hint",
                            "Required: the sites this login may be typed into. example.com also covers its subdomains."
                        }
                    }
                }
                div { class: "field",
                    label { class: "field-label", r#for: "vault-note", "Note (optional)" }
                    input {
                        id: "vault-note",
                        class: "input",
                        value: "{note}",
                        autocomplete: "off",
                        placeholder: "What is this for?",
                        oninput: move |event| {
                            if let Some(form) = session.write().panel.vault.form.as_mut() {
                                form.note = event.value();
                            }
                        },
                    }
                }
            }
            if let Some(message) = error {
                div { class: "error-text", role: "alert", "{message}" }
            }
            div { class: "vault-form-foot",
                button {
                    r#type: "button",
                    class: "btn ghost",
                    disabled: saving,
                    onclick: move |_| session.write().panel.close_form(),
                    "Cancel"
                }
                button { r#type: "submit", class: "btn primary", disabled: saving,
                    if saving { "Saving\u{2026}" } else { "Save" }
                }
            }
        }
    }
}

/// The source a password manager's id stands for, for its import button.
fn source_of(id: &str) -> Option<VaultSource> {
    match id {
        "onepassword" => Some(VaultSource::OnePassword),
        "protonpass" => Some(VaultSource::ProtonPass),
        _ => None,
    }
}

fn entry_count_for(source: VaultSource, entries: &[VaultEntry]) -> usize {
    entries
        .iter()
        .filter(|entry| entry.source == source)
        .count()
}

/// Short connection status for the Connected vaults summary. Never a token.
fn provider_status_label(manager: &PasswordManager) -> &'static str {
    if !manager.enabled {
        "off on this server"
    } else if manager.configured && manager.cli_found {
        "ready"
    } else if manager.configured {
        "token saved · CLI missing"
    } else if manager.cli_found {
        "CLI found · no token"
    } else {
        "not connected"
    }
}

fn entries_label(count: usize) -> String {
    match count {
        0 => "no entries".to_owned(),
        1 => "1 entry".to_owned(),
        n => format!("{n} entries"),
    }
}

/// Password managers (0.23): each one's token and CLI status, and an import
/// from this PC. A token is write-only, like a vault value: it is sent and
/// never shown again. Requests run on the panel worker.
#[component]
fn PasswordManagersPanel(session: Signal<Session>) -> Element {
    let (
        loaded,
        vault_loaded,
        entries,
        providers,
        tests,
        error,
        importing,
        import_error,
        picker_open,
    ) = {
        let s = session.read();
        let v = &s.panel.vault;
        (
            v.providers_loaded,
            v.loaded,
            v.entries.clone(),
            v.providers.clone(),
            v.provider_tests.clone(),
            v.provider_error.clone(),
            v.import_loading,
            v.import_error.clone(),
            v.import.is_some(),
        )
    };
    let local_count = vault_loaded.then(|| entry_count_for(VaultSource::Local, &entries));
    rsx! {
        section { class: "vault-providers", "aria-label": "Password managers",
            h2 { class: "vault-section-title", "Password managers" }
            p { class: "tiny vault-hint",
                "Use a login from 1Password or Proton Pass without copying it. The password stays in your manager and is read only when a bot types it."
            }
            if loaded || vault_loaded {
                ConnectedVaultsSummary {
                    providers: providers.clone(),
                    tests: tests.clone(),
                    entries: if vault_loaded { Some(entries.clone()) } else { None },
                    local_count,
                }
            }
            if let Some(message) = error {
                div { class: "error-text tiny", role: "alert", "{message}" }
            }
            if !loaded {
                span { class: "caption", "Checking password managers\u{2026}" }
            }
            for manager in providers {
                ProviderRow { key: "{manager.id}", session, manager }
            }
            if let Some(source) = importing {
                span { class: "caption", "Reading the logins from {source.label()}\u{2026}" }
            }
            if let Some(message) = import_error {
                div { class: "error-text tiny", role: "alert", "{message}" }
            }
            if picker_open {
                ImportPickerView { session }
            }
        }
    }
}

/// Providers status, last Test result, and entry counts — never a token or value.
#[component]
fn ConnectedVaultsSummary(
    providers: Vec<PasswordManager>,
    tests: std::collections::BTreeMap<String, PasswordManagerProbe>,
    entries: Option<Vec<VaultEntry>>,
    local_count: Option<usize>,
) -> Element {
    rsx! {
        div { class: "vault-connected", role: "status", "aria-label": "Connected vaults",
            h3 { class: "vault-connected-title", "Connected vaults" }
            ul { class: "vault-connected-list",
                if let Some(count) = local_count {
                    li { class: "vault-connected-row",
                        span { class: "vault-connected-name", "Stored here" }
                        span { class: "chip", "{entries_label(count)}" }
                    }
                }
                for manager in providers {
                    ConnectedProviderStatus {
                        key: "{manager.id}",
                        manager: manager.clone(),
                        probe: tests.get(&manager.id).cloned(),
                        entry_count: entries.as_ref().and_then(|list| {
                            source_of(&manager.id).map(|source| entry_count_for(source, list))
                        }),
                    }
                }
            }
        }
    }
}

#[component]
fn ConnectedProviderStatus(
    manager: PasswordManager,
    probe: Option<PasswordManagerProbe>,
    entry_count: Option<usize>,
) -> Element {
    let status = provider_status_label(&manager);
    let chip = if !manager.enabled {
        "chip muted"
    } else if manager.configured && manager.cli_found {
        "chip ok"
    } else if manager.configured || manager.cli_found {
        "chip warn"
    } else {
        "chip muted"
    };
    rsx! {
        li { class: "vault-connected-row",
            span { class: "vault-connected-name", "{manager.name}" }
            span { class: "{chip}", "{status}" }
            if let Some(count) = entry_count {
                span { class: "chip", "{entries_label(count)}" }
            }
            if let Some(PasswordManagerProbe { ok, message, .. }) = probe {
                span {
                    class: if ok { "tiny ok" } else { "tiny err" },
                    "Last test: {message}"
                }
            }
        }
    }
}

#[component]
fn ProviderRow(session: Signal<Session>, manager: PasswordManager) -> Element {
    let mut session = session;
    let mut token = use_signal(String::new);
    let id = manager.id.clone();
    let busy = session.read().panel.vault.provider_busy.as_deref() == Some(id.as_str());
    let testing = session.read().panel.vault.provider_testing.as_deref() == Some(id.as_str());
    let probe = session.read().panel.vault.provider_tests.get(&id).cloned();
    let filled = !token.read().is_empty();
    let token_line = if manager.configured {
        "Access token saved on this server"
    } else {
        "No access token saved"
    };
    let cli_line = if !manager.enabled {
        "Off on this server (experimental): the server admin sets protonpass_enabled in [vault]."
            .to_owned()
    } else if manager.cli_found {
        "CLI found on this server".to_owned()
    } else {
        format!(
            "CLI not installed on this server. Install it from {}",
            manager.install_url
        )
    };
    let save_id = manager.id.clone();
    let clear_id = manager.id.clone();
    let source = source_of(&manager.id);
    let input_label = format!("{} access token", manager.name);
    rsx! {
        div { class: "vault-provider-row",
            div { class: "vault-row-main",
                strong { class: "vault-name", "{manager.name}" }
                span { class: "caption", "{token_line}" }
                span { class: "tiny vault-hint", "{cli_line}" }
            }
            div { class: "vault-provider-token",
                input {
                    class: "input",
                    r#type: "password",
                    autocomplete: "off",
                    spellcheck: false,
                    "aria-label": "{input_label}",
                    placeholder: "Access token",
                    value: "{token}",
                    oninput: move |event| token.set(event.value()),
                }
                button {
                    class: "btn primary sm",
                    disabled: busy || testing || !filled,
                    onclick: move |_| {
                        // Move the token out of the signal: nothing is left behind.
                        let secret = Zeroizing::new(std::mem::take(&mut *token.write()));
                        if !secret.is_empty() {
                            session.write().panel.save_password_manager(save_id.clone(), secret);
                        }
                    },
                    "Save token"
                }
                if manager.configured {
                    button {
                        class: "btn ghost sm",
                        disabled: busy || testing,
                        onclick: move |_| session.write().panel.clear_password_manager(&clear_id),
                        "Remove"
                    }
                }
            }
            div { class: "vault-provider-actions",
                button {
                    class: "btn ghost sm",
                    title: "Check the saved token against the CLI on the server",
                    disabled: busy || testing,
                    onclick: {
                        let test_id = id.clone();
                        move |_| session.write().panel.test_password_manager(test_id.clone())
                    },
                    Icon { name: "refresh", size: 14 }
                    if testing { "Testing\u{2026}" } else { "Test" }
                }
                if let Some(source) = source {
                    button {
                        class: "btn sm",
                        disabled: busy || testing,
                        onclick: move |_| session.write().panel.start_import(source),
                        "Import from this PC"
                    }
                }
            }
            if let Some(PasswordManagerProbe { ok, message, .. }) = probe {
                p {
                    class: if ok { "tiny ok" } else { "tiny err" },
                    role: "status",
                    "{message}"
                }
            }
        }
    }
}

/// The import picker: the logins listed on this PC, with their titles,
/// usernames and sites. Picking one creates a reference, never a copy.
#[component]
fn ImportPickerView(session: Signal<Session>) -> Element {
    let mut session = session;
    let (source, items, picked, saving, error) = {
        let s = session.read();
        match s.panel.vault.import.as_ref() {
            Some(picker) => (
                picker.source,
                picker.items.clone(),
                picker.picked.clone(),
                picker.saving,
                picker.error.clone(),
            ),
            None => return rsx! {},
        }
    };
    let count = picked.iter().filter(|chosen| **chosen).count();
    let label = source.label();
    let action = if saving {
        "Importing\u{2026}".to_owned()
    } else {
        format!("Import {count}")
    };
    rsx! {
        section { class: "vault-import", "aria-label": "Import from {label}",
            h3 { class: "vault-form-title", "Import from {label}" }
            p { class: "tiny vault-hint",
                "Each login becomes a reference: the password stays in {label} and is read only when a bot types it."
            }
            if items.is_empty() {
                p { class: "caption", "No logins were listed." }
            }
            ul { class: "vault-list", role: "list",
                for (index, item) in items.iter().enumerate() {
                    li { key: "{item.reference}", class: "vault-row",
                        label { class: "vault-import-pick",
                            input {
                                r#type: "checkbox",
                                checked: picked.get(index).copied().unwrap_or(false),
                                onchange: move |_| session.write().panel.toggle_import_item(index),
                            }
                            div { class: "vault-row-main",
                                strong { class: "vault-name", "{item.title}" }
                                span { class: "vault-meta caption",
                                    {
                                        [item.username.clone(), Some(item.vault.clone()).filter(|v| !v.is_empty())]
                                            .into_iter()
                                            .flatten()
                                            .collect::<Vec<_>>()
                                            .join(" \u{b7} ")
                                    }
                                }
                                if let Some(domain) = item.domain.clone() {
                                    span { class: "chip mono", "{domain}" }
                                } else {
                                    span { class: "tiny vault-hint", "No site in this item. Add one after importing." }
                                }
                            }
                        }
                    }
                }
            }
            if let Some(message) = error {
                div { class: "error-text", role: "alert", "{message}" }
            }
            div { class: "vault-form-foot",
                button {
                    r#type: "button",
                    class: "btn ghost",
                    disabled: saving,
                    onclick: move |_| session.write().panel.close_import(),
                    "Cancel"
                }
                button {
                    r#type: "button",
                    class: "btn primary",
                    disabled: saving || count == 0,
                    onclick: move |_| session.write().panel.import_picked(),
                    "{action}"
                }
            }
        }
    }
}
