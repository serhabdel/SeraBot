//! The Vault page (design B, contract section 3; a rail item since 0.16).
//!
//! One vault per user, shared by all of that user's bots. The page lists
//! entries (never a value), adds or replaces one through a form whose value field is
//! write-only, and deletes after a confirm step. Requests run on the panel
//! worker (`panel.rs`); this file only draws and forwards clicks.

use crate::ui::Icon;
use crate::ui_widgets::SecretField;
use dioxus::prelude::*;
use serabot_desktop::panel::last_used_label;
use serabot_desktop::session::Session;
use serabot_desktop::{
    PasswordManager, PasswordManagerProbe, VaultEntry, VaultKind, VaultPendingSecret, VaultSource,
};
use zeroize::{Zeroize, Zeroizing};

fn entry_matches(entry: &VaultEntry, query: &str, kind: Option<VaultKind>) -> bool {
    if let Some(k) = kind
        && entry.kind != k
    {
        return false;
    }
    let needle = query.trim().to_ascii_lowercase();
    if needle.is_empty() {
        return true;
    }
    entry.name.to_ascii_lowercase().contains(&needle)
        || entry
            .username
            .as_deref()
            .is_some_and(|u| u.to_ascii_lowercase().contains(&needle))
        || entry
            .reference
            .as_deref()
            .is_some_and(|r| r.to_ascii_lowercase().contains(&needle))
        || entry
            .domains
            .iter()
            .any(|d| d.to_ascii_lowercase().contains(&needle))
        || entry.kind.label().to_ascii_lowercase().contains(&needle)
}

fn kind_badge_label(kind: VaultKind) -> &'static str {
    match kind {
        VaultKind::Password => "\u{1f511} Password",
        VaultKind::ApiKey => "\u{26a1} API key",
        VaultKind::Note => "\u{1f4dd} Note",
    }
}

/// Summary metrics for the Security Vault hero strip:
/// `(total_credentials, local_sealed, manager_linked, domain_rules)`.
fn vault_stats(entries: &[VaultEntry]) -> (usize, usize, usize, usize) {
    let total = entries.len();
    let local_sealed = entries
        .iter()
        .filter(|e| e.source == VaultSource::Local)
        .count();
    let manager_linked = entries.iter().filter(|e| e.source.is_reference()).count();
    let domain_rules = entries.iter().map(|e| e.domains.len()).sum();
    (total, local_sealed, manager_linked, domain_rules)
}

fn kind_count(entries: &[VaultEntry], kind: VaultKind) -> usize {
    entries.iter().filter(|e| e.kind == kind).count()
}

/// The Vault page: Security Vault Dashboard with overview stats, password
/// manager cards, search & filter toolbar, and credential cards.
#[component]
pub fn VaultPage(session: Signal<Session>) -> Element {
    let mut session = session;
    let mut local_query = use_signal(String::new);
    let mut kind_filter = use_signal(|| Option::<VaultKind>::None);

    use_effect(move || {
        if session.read().panel.has_auto_sync_candidate() {
            session.write().panel.maybe_auto_sync_providers();
        }
    });

    let (loaded, loading, error, notice, entries, pending, domain_filter, has_form) = {
        let s = session.read();
        let v = &s.panel.vault;
        (
            v.loaded,
            v.loading,
            v.error.clone(),
            v.notice.clone(),
            v.entries.clone(),
            v.pending_secrets.clone(),
            v.domain_filter.clone(),
            v.form.is_some(),
        )
    };
    let filtered_entries: Vec<VaultEntry> = entries
        .iter()
        .filter(|entry| entry_matches(entry, &local_query.read(), kind_filter()))
        .cloned()
        .collect();
    let any_filter_active = !domain_filter.trim().is_empty()
        || !local_query.read().trim().is_empty()
        || kind_filter().is_some();
    let (total_creds, local_sealed, manager_linked, domain_rules) = vault_stats(&entries);

    rsx! {
        div {
            class: "pane vault-page",
            tabindex: "-1",
            onkeydown: move |event: KeyboardEvent| {
                if event.key() == Key::Escape && session.read().panel.vault.import.is_some() {
                    event.stop_propagation();
                    session.write().panel.close_import();
                }
            },
            div { class: "pane-head",
                div {
                    h1 { class: "pane-title", "Security Vault" }
                    p { class: "muted",
                        "Passwords and keys your bots can use. Values are write-only and never exposed."
                    }
                }
                if !has_form && loaded {
                    button {
                        class: "btn primary",
                        r#type: "button",
                        onclick: move |_| session.write().panel.open_add_form(),
                        Icon { name: "plus", size: 15 }
                        "Add credential"
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

                        // Security Overview Hero / Stats Strip
                        div { class: "vault-hero", role: "region", "aria-label": "Vault overview",
                            div { class: "vault-stat-card",
                                span { class: "vault-stat-value", "{total_creds}" }
                                span { class: "vault-stat-label", "Total Credentials" }
                                span { class: "vault-stat-sub", "Across all sources" }
                            }
                            div { class: "vault-stat-card",
                                span { class: "vault-stat-value", "{local_sealed}" }
                                span { class: "vault-stat-label", "Local Sealed" }
                                span { class: "vault-stat-sub", "AES-256 envelope" }
                            }
                            div { class: "vault-stat-card",
                                span { class: "vault-stat-value", "{manager_linked}" }
                                span { class: "vault-stat-label", "Manager Linked" }
                                span { class: "vault-stat-sub", "1Password & Proton Pass" }
                            }
                            div { class: "vault-stat-card",
                                span { class: "vault-stat-value", "{domain_rules}" }
                                span { class: "vault-stat-label", "Domain Rules" }
                                span { class: "vault-stat-sub", "Host allowlists" }
                            }
                        }

                        PasswordManagersPanel { session }

                        if loaded || !entries.is_empty() || !pending.is_empty() {
                            div { class: "vault-toolbar",
                                div { class: "vault-filter-row",
                                    input {
                                        class: "input",
                                        r#type: "search",
                                        value: "{local_query}",
                                        placeholder: "Search name, user, reference, or site\u{2026}",
                                        "aria-label": "Search saved entries",
                                        oninput: move |event| local_query.set(event.value()),
                                    }
                                    input {
                                        id: "vault-domain-filter",
                                        class: "input",
                                        r#type: "search",
                                        value: "{domain_filter}",
                                        placeholder: "Server site filter (example.com)",
                                        "aria-label": "Filter on server by site",
                                        oninput: move |event| {
                                            session.write().panel.set_vault_domain_filter(event.value());
                                        },
                                        onkeydown: move |event: KeyboardEvent| {
                                            if event.key() == Key::Enter {
                                                event.prevent_default();
                                                session.write().panel.apply_vault_domain_filter();
                                            }
                                        },
                                    }
                                    button {
                                        class: "btn sm",
                                        r#type: "button",
                                        disabled: loading,
                                        onclick: move |_| session.write().panel.apply_vault_domain_filter(),
                                        "Apply"
                                    }
                                    if any_filter_active {
                                        button {
                                            class: "btn ghost sm",
                                            r#type: "button",
                                            disabled: loading,
                                            onclick: move |_| {
                                                local_query.set(String::new());
                                                kind_filter.set(None);
                                                if !session.read().panel.vault.domain_filter.trim().is_empty() {
                                                    session.write().panel.set_vault_domain_filter(String::new());
                                                    session.write().panel.apply_vault_domain_filter();
                                                }
                                            },
                                            "Clear"
                                        }
                                    }
                                }
                                div { class: "seg", role: "group", "aria-label": "Filter by type",
                                    button {
                                        class: if kind_filter().is_none() { "seg-btn active" } else { "seg-btn" },
                                        r#type: "button",
                                        onclick: move |_| kind_filter.set(None),
                                        "All ({entries.len()})"
                                    }
                                    for option in VaultKind::ALL {
                                        button {
                                            key: "{option.as_str()}",
                                            class: if kind_filter() == Some(option) { "seg-btn active" } else { "seg-btn" },
                                            r#type: "button",
                                            onclick: move |_| kind_filter.set(Some(option)),
                                            "{option.label()} ({kind_count(&entries, option)})"
                                        }
                                    }
                                }
                            }
                        }
                        if !pending.is_empty() {
                            section { class: "vault-pending", "aria-label": "Waiting in chat",
                                h3 { class: "vault-form-title", "Waiting in chat" }
                                div { class: "vault-entry-grid", role: "list",
                                    for item in pending.iter() {
                                        VaultPendingRow { key: "{item.card_id}", item: item.clone() }
                                    }
                                }
                            }
                        }
                        if loading && !loaded {
                            div { class: "vault-entry-grid", "aria-busy": "true",
                                div { class: "skeleton row", " " }
                                div { class: "skeleton row", " " }
                            }
                        } else if loaded && filtered_entries.is_empty() {
                            if any_filter_active {
                                div { class: "vault-empty",
                                    div { class: "empty-icon", Icon { name: "search", size: 20 } }
                                    h2 { class: "empty-title", "No entries match your filter" }
                                    p { class: "empty-text",
                                        "Try clearing your search or site filter."
                                    }
                                    button {
                                        class: "btn",
                                        r#type: "button",
                                        onclick: move |_| {
                                            local_query.set(String::new());
                                            kind_filter.set(None);
                                            if !session.read().panel.vault.domain_filter.trim().is_empty() {
                                                session.write().panel.set_vault_domain_filter(String::new());
                                                session.write().panel.apply_vault_domain_filter();
                                            }
                                        },
                                        "Reset filters"
                                    }
                                }
                            } else {
                                div { class: "vault-empty",
                                    div { class: "empty-icon", Icon { name: "key", size: 20 } }
                                    h2 { class: "empty-title", "Nothing saved yet" }
                                    p { class: "empty-text",
                                        "Add a login or an API key. Your bots can use it to sign in for you, but never see the actual value."
                                    }
                                    button {
                                        class: "btn primary",
                                        r#type: "button",
                                        onclick: move |_| session.write().panel.open_add_form(),
                                        "Add credential"
                                    }
                                }
                            }
                        } else if !loaded {
                            div { class: "vault-empty",
                                p { class: "empty-text", "Could not load your vault entries." }
                                button {
                                    class: "btn",
                                    r#type: "button",
                                    onclick: move |_| session.write().panel.ask_vault_list(),
                                    "Try again"
                                }
                            }
                        } else {
                            div { class: "vault-entry-grid", role: "list",
                                for entry in filtered_entries {
                                    VaultEntryCard { key: "{entry.name}", session, entry }
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
fn VaultPendingRow(item: VaultPendingSecret) -> Element {
    rsx! {
        div { class: "vault-entry-card vault-row-pending", role: "listitem",
            div { class: "vault-entry-head",
                strong { class: "vault-name", "{item.name}" }
                div { class: "vault-entry-badges",
                    span { class: "vault-kind-badge", "{kind_badge_label(item.kind)}" }
                    span { class: "vault-source-badge", "Pending card" }
                }
            }
            if !item.domains.is_empty() {
                div { class: "vault-chips",
                    for domain in item.domains.iter() {
                        span { key: "{domain}", class: "vault-domain-pill mono", "{domain}" }
                    }
                }
            } else {
                span { class: "vault-domain-pill warn", "Any domain" }
            }
            div { class: "vault-entry-foot",
                span { class: "vault-used tiny", "Finish the card in chat" }
            }
        }
    }
}

#[component]
fn VaultEntryCard(session: Signal<Session>, entry: VaultEntry) -> Element {
    let mut session = session;
    let mut copied_ref = use_signal(|| false);
    let (confirming, deleting) = {
        let s = session.read();
        (
            s.panel.vault.confirm_delete.as_deref() == Some(entry.name.as_str()),
            s.panel.vault.deleting.as_deref() == Some(entry.name.as_str()),
        )
    };
    let used = last_used_label(entry.last_used_at.as_deref());
    let name = entry.name.clone();
    let name_replace = entry.name.clone();
    let name_delete = entry.name.clone();
    let source_label = entry.source.label();
    rsx! {
        div {
            class: if deleting { "vault-entry-card busy" } else { "vault-entry-card" },
            role: "listitem",
            div { class: "vault-entry-head",
                strong { class: "vault-name", "{name}" }
                div { class: "vault-entry-badges",
                    span { class: "vault-kind-badge", "{kind_badge_label(entry.kind)}" }
                    span { class: "vault-source-badge", "{source_label}" }
                }
            }

            if let Some(user) = entry.username.as_deref().filter(|u| !u.is_empty()) {
                div { class: "vault-chips",
                    span { class: "vault-user-chip",
                        Icon { name: "user", size: 12 }
                        "{user}"
                    }
                }
            }

            if let Some(reference) = entry.reference.as_deref().filter(|r| !r.is_empty()) {
                {
                    let ref_copy = reference.to_owned();
                    rsx! {
                        button {
                            class: "vault-ref-chip mono",
                            r#type: "button",
                            title: "Copy reference path",
                            onclick: move |_| {
                                #[cfg_attr(not(target_os = "windows"), allow(unused_variables))]
                                let text = ref_copy.clone();
                                copied_ref.set(true);
                                #[cfg(target_os = "windows")]
                                {
                                    let _ = std::process::Command::new("cmd")
                                        .args(["/C", &format!("echo|set /p={text}|clip")])
                                        .spawn();
                                }
                            },
                            Icon { name: if copied_ref() { "check" } else { "link" }, size: 12 }
                            span { "{reference}" }
                        }
                    }
                }
            } else {
                div { class: "vault-secret-preview", "aria-label": "Secret stored encrypted",
                    Icon { name: "lock", size: 12 }
                    span { class: "mono", "\u{2022}\u{2022}\u{2022}\u{2022}\u{2022}\u{2022}\u{2022}\u{2022}" }
                }
            }

            div { class: "vault-chips",
                if entry.domains.is_empty() {
                    span { class: "vault-domain-pill warn", title: "Not restricted to specific hosts",
                        "Any domain"
                    }
                } else {
                    for domain in entry.domains.iter() {
                        span { key: "{domain}", class: "vault-domain-pill mono", "{domain}" }
                    }
                }
            }

            div { class: "vault-entry-foot",
                span { class: "vault-used tiny", "{used}" }
                if confirming {
                    div { class: "vault-confirm", role: "group", "aria-label": "Confirm delete",
                        span { class: "caption", "Delete?" }
                        button {
                            class: "btn danger sm",
                            r#type: "button",
                            onclick: move |_| session.write().panel.confirm_delete(),
                            "Delete"
                        }
                        button {
                            class: "btn ghost sm",
                            r#type: "button",
                            onclick: move |_| session.write().panel.cancel_delete(),
                            "Cancel"
                        }
                    }
                } else {
                    div { class: "vault-actions",
                        button {
                            class: "btn ghost sm",
                            r#type: "button",
                            title: "Edit",
                            "aria-label": "Edit {name_replace}",
                            disabled: deleting,
                            onclick: move |_| session.write().panel.open_replace_form(&name_replace),
                            Icon { name: "edit", size: 13 }
                            "Edit"
                        }
                        button {
                            class: "icon-btn",
                            r#type: "button",
                            title: "Delete",
                            "aria-label": "Delete {name_delete}",
                            disabled: deleting,
                            onclick: move |_| session.write().panel.ask_delete(&name_delete),
                            Icon { name: "trash", size: 14 }
                        }
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
    let mut confirm_discard = use_signal(|| false);
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
        has_secret,
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
            !form.value.is_empty(),
        )
    };
    let (source, reference) = {
        let s = session.read();
        match s.panel.vault.form.as_ref() {
            Some(form) => (form.source, form.reference.clone()),
            None => (VaultSource::Local, String::new()),
        }
    };
    let has_unsaved = has_secret
        || !domain_draft.trim().is_empty()
        || !reference.trim().is_empty()
        || (!replacing
            && (!name.trim().is_empty()
                || !username.trim().is_empty()
                || !domains.is_empty()
                || !note.trim().is_empty()));
    let value_label = match kind {
        VaultKind::Password => "Password",
        VaultKind::ApiKey => "API key",
        VaultKind::Note => "Note text",
    };
    let title = if replacing {
        "Edit saved credential"
    } else {
        "Add a password or key"
    };
    rsx! {
        form {
            class: "vault-form",
            tabindex: "-1",
            onkeydown: move |event: KeyboardEvent| {
                if event.key() == Key::Escape {
                    event.stop_propagation();
                    if confirm_discard() {
                        confirm_discard.set(false);
                    } else if has_unsaved && !saving {
                        confirm_discard.set(true);
                    } else if !saving {
                        session.write().panel.close_form();
                    }
                }
            },
            onsubmit: move |event| {
                event.prevent_default();
                confirm_discard.set(false);
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
                                        if option.is_reference() && form.kind == VaultKind::Note {
                                            form.kind = VaultKind::Password;
                                        }
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
                                disabled: source.is_reference() && option == VaultKind::Note,
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
                    if source.is_reference() {
                        span { class: "tiny vault-hint", "Notes can only be saved in this Vault, not as external references." }
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
            if confirm_discard() {
                div { class: "vault-confirm", role: "alertdialog", "aria-label": "Discard unsaved changes?",
                    span { class: "caption", "Discard this unsaved entry?" }
                    button {
                        r#type: "button",
                        class: "btn danger sm",
                        onclick: move |_| session.write().panel.close_form(),
                        "Discard"
                    }
                    button {
                        r#type: "button",
                        class: "btn ghost sm",
                        onclick: move |_| confirm_discard.set(false),
                        "Keep editing"
                    }
                }
            }
            div { class: "vault-form-foot",
                button {
                    r#type: "button",
                    class: "btn ghost",
                    disabled: saving,
                    onclick: move |_| {
                        if has_unsaved {
                            confirm_discard.set(true);
                        } else {
                            session.write().panel.close_form();
                        }
                    },
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

/// Clear status pill label + modifier class matching server report.
fn provider_status_pill(manager: &PasswordManager) -> (&'static str, &'static str) {
    if !manager.enabled {
        ("Off on this server", "vault-provider-status off")
    } else if manager.configured && manager.cli_found {
        ("Connected", "vault-provider-status ok")
    } else if manager.configured {
        ("Token saved", "vault-provider-status warn")
    } else if manager.cli_found {
        ("CLI ready · no token", "vault-provider-status warn")
    } else {
        ("CLI not installed", "vault-provider-status off")
    }
}

fn provider_token_hint(id: &str) -> &'static str {
    match id {
        "onepassword" => "OP_SERVICE_ACCOUNT_TOKEN",
        "protonpass" => "PROTON_PASS_PERSONAL_ACCESS_TOKEN",
        _ => "Access token",
    }
}

fn provider_ref_snippet(id: &str) -> &'static str {
    match id {
        "protonpass" => "pass://Vault/Item/field",
        _ => "op://Vault/Item/field",
    }
}

/// Password managers (0.23): side-by-side cards for 1Password and Proton Pass.
#[component]
fn PasswordManagersPanel(session: Signal<Session>) -> Element {
    let (loaded, providers, error, importing, import_error, picker_open) = {
        let s = session.read();
        let v = &s.panel.vault;
        (
            v.providers_loaded,
            v.providers.clone(),
            v.provider_error.clone(),
            v.import_loading,
            v.import_error.clone(),
            v.import.is_some(),
        )
    };
    rsx! {
        section { class: "vault-providers", "aria-label": "Password managers",
            if let Some(message) = error {
                div { class: "error-text tiny", role: "alert", "{message}" }
            }
            if !loaded {
                span { class: "caption", "Checking password managers\u{2026}" }
            } else if !providers.is_empty() {
                div { class: "vault-provider-grid",
                    for manager in providers {
                        ProviderCard { key: "{manager.id}", session, manager }
                    }
                }
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

#[component]
fn ProviderCard(session: Signal<Session>, manager: PasswordManager) -> Element {
    let mut session = session;
    let mut token = use_signal(String::new);
    let mut confirm_clear = use_signal(|| false);
    let id = manager.id.clone();
    let busy = session.read().panel.vault.provider_busy.as_deref() == Some(id.as_str());
    let testing = session.read().panel.vault.provider_testing.as_deref() == Some(id.as_str());
    let syncing = session.read().panel.vault.provider_syncing.as_deref() == Some(id.as_str());
    let probe = session.read().panel.vault.provider_tests.get(&id).cloned();
    let filled = !token.read().is_empty();
    let (status_label, status_class) = provider_status_pill(&manager);
    let token_hint = provider_token_hint(&manager.id);
    let ref_snippet = provider_ref_snippet(&manager.id);
    let save_id = manager.id.clone();
    let clear_id = manager.id.clone();
    let source = source_of(&manager.id);
    let input_label = format!("{} access token", manager.name);
    rsx! {
        div { class: "vault-provider-card",
            div { class: "vault-provider-head",
                div { class: "vault-provider-brand",
                    span { class: "vault-provider-icon", "aria-hidden": "true",
                        Icon { name: if manager.id == "onepassword" { "lock" } else { "key" }, size: 16 }
                    }
                    div { class: "vault-provider-title-col",
                        strong { class: "vault-name", "{manager.name}" }
                        span { class: "vault-provider-sub mono", title: "{token_hint}", "{token_hint}" }
                    }
                }
                span { class: "{status_class}", "{status_label}" }
            }

            if manager.enabled && !manager.cli_found {
                p { class: "tiny muted",
                    "The server CLI installs automatically when you test connection or use this provider."
                }
            }

            div { class: "vault-provider-token",
                SecretField {
                    value: token,
                    label: input_label.clone(),
                    placeholder: "Paste token (write-only)".to_owned(),
                }
                button {
                    class: "btn primary sm",
                    r#type: "button",
                    disabled: busy || testing || syncing || !filled,
                    onclick: move |_| {
                        let secret = Zeroizing::new(std::mem::take(&mut *token.write()));
                        if !secret.is_empty() {
                            session.write().panel.save_password_manager(save_id.clone(), secret);
                        }
                    },
                    "Save"
                }
                if manager.configured && !confirm_clear() {
                    button {
                        class: "btn ghost sm",
                        r#type: "button",
                        disabled: busy || testing || syncing,
                        onclick: move |_| confirm_clear.set(true),
                        "Remove"
                    }
                }
            }

            if manager.configured && confirm_clear() {
                div { class: "vault-confirm", role: "alertdialog", "aria-label": "Remove {manager.name} token?",
                    span { class: "caption", "Remove the saved {manager.name} token?" }
                    button {
                        class: "btn danger sm",
                        r#type: "button",
                        disabled: busy || testing || syncing,
                        onclick: move |_| {
                            confirm_clear.set(false);
                            session.write().panel.clear_password_manager(&clear_id);
                        },
                        "Remove"
                    }
                    button {
                        class: "btn ghost sm",
                        r#type: "button",
                        disabled: busy || testing || syncing,
                        onclick: move |_| confirm_clear.set(false),
                        "Keep"
                    }
                }
            }

            div { class: "vault-provider-actions",
                button {
                    class: "btn primary sm",
                    r#type: "button",
                    title: "Sync credentials from {manager.name} on the server",
                    disabled: !manager.enabled || !manager.configured || busy || testing || syncing,
                    onclick: {
                        let mgr = manager.clone();
                        move |_| session.write().panel.sync_provider(mgr.clone())
                    },
                    Icon { name: "refresh", size: 14 }
                    if syncing { "Syncing\u{2026}" } else { "Sync from Server" }
                }
                button {
                    class: "btn ghost sm",
                    r#type: "button",
                    title: "Check the saved token against the CLI on the server",
                    disabled: busy || testing || syncing,
                    onclick: {
                        let test_id = id.clone();
                        move |_| session.write().panel.test_password_manager(test_id.clone())
                    },
                    Icon { name: "check", size: 14 }
                    if testing { "Testing\u{2026}" } else { "Test Connection" }
                }
                if let Some(source) = source {
                    button {
                        class: "btn ghost sm",
                        r#type: "button",
                        disabled: busy || testing || syncing,
                        onclick: move |_| session.write().panel.start_import(source),
                        "Import from this PC"
                    }
                }
            }

            if let Some(probe) = probe {
                ProbeBanner { probe }
            }

            details { class: "vault-provider-snippet",
                summary { "Reference format" }
                code { class: "mono tiny", "{ref_snippet}" }
            }
        }
    }
}

#[component]
fn ProbeBanner(probe: PasswordManagerProbe) -> Element {
    let banner_class = if probe.ok {
        "vault-probe-banner ok"
    } else {
        "vault-probe-banner err"
    };
    let headline = if probe.ok {
        "Connection verified"
    } else {
        "Connection check failed"
    };
    rsx! {
        div { class: "{banner_class}", role: "status",
            Icon { name: if probe.ok { "check" } else { "x" }, size: 13 }
            span { strong { "{headline}" } }
            if !probe.message.is_empty() {
                span { class: "tiny", " — {probe.message}" }
            }
        }
    }
}

/// The import picker: the logins listed on this PC, with their titles,
/// usernames and sites. Picking one creates a reference, never a copy.
#[component]
fn ImportPickerView(session: Signal<Session>) -> Element {
    let mut session = session;
    let mut query = use_signal(String::new);
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
    let filter_text = query.read().clone();
    let matching_indices: Vec<usize> = items
        .iter()
        .enumerate()
        .filter(|(_, item)| item.matches_filter(&filter_text))
        .map(|(i, _)| i)
        .collect();
    let label = source.label();
    let action = if saving {
        "Importing\u{2026}".to_owned()
    } else {
        format!("Import {count}")
    };
    rsx! {
        section {
            class: "vault-import",
            tabindex: "-1",
            "aria-label": "Import from {label}",
            onkeydown: move |event: KeyboardEvent| {
                if event.key() == Key::Escape && !saving {
                    event.stop_propagation();
                    session.write().panel.close_import();
                }
            },
            h3 { class: "vault-form-title", "Import from {label}" }
            p { class: "tiny vault-hint",
                "Each login becomes a reference: the password stays in {label} and is read only when a bot types it."
            }
            if !items.is_empty() {
                div { class: "vault-filter-row",
                    input {
                        class: "input",
                        r#type: "search",
                        placeholder: "Filter logins by title, user, vault, or site\u{2026}",
                        "aria-label": "Filter logins to import",
                        value: "{query}",
                        oninput: move |event| query.set(event.value()),
                    }
                    if !filter_text.trim().is_empty() {
                        button {
                            class: "btn ghost sm",
                            r#type: "button",
                            onclick: move |_| query.set(String::new()),
                            "Clear"
                        }
                    }
                    button {
                        class: "btn ghost sm",
                        r#type: "button",
                        disabled: saving || matching_indices.is_empty(),
                        onclick: {
                            let idxs = matching_indices.clone();
                            move |_| {
                                if let Some(picker) = session.write().panel.vault.import.as_mut() {
                                    for i in &idxs {
                                        if let Some(flag) = picker.picked.get_mut(*i) {
                                            *flag = true;
                                        }
                                    }
                                }
                            }
                        },
                        "Select all"
                    }
                    button {
                        class: "btn ghost sm",
                        r#type: "button",
                        disabled: saving || count == 0,
                        onclick: {
                            let idxs = matching_indices.clone();
                            move |_| {
                                if let Some(picker) = session.write().panel.vault.import.as_mut() {
                                    for i in &idxs {
                                        if let Some(flag) = picker.picked.get_mut(*i) {
                                            *flag = false;
                                        }
                                    }
                                }
                            }
                        },
                        "Select none"
                    }
                }
            }
            if items.is_empty() {
                p { class: "caption", "No logins were listed." }
            } else if matching_indices.is_empty() {
                p { class: "caption", "No logins match \u{201c}{query}\u{201d}." }
            }
            ul { class: "vault-list", role: "list",
                for index in matching_indices {
                    if let Some(item) = items.get(index) {
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

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_entry(
        name: &str,
        kind: VaultKind,
        source: VaultSource,
        domains: Vec<&str>,
    ) -> VaultEntry {
        VaultEntry {
            name: name.into(),
            kind,
            source,
            reference: source.is_reference().then(|| "op://Work/Item/pw".into()),
            username: Some("billing@acme.test".into()),
            domains: domains.into_iter().map(String::from).collect(),
            note: None,
            created_at: "2026-01-01T00:00:00Z".into(),
            updated_at: "2026-01-01T00:00:00Z".into(),
            last_used_at: None,
        }
    }

    #[test]
    fn entry_matches_checks_name_username_domain_and_kind() {
        let entry = sample_entry(
            "stripe-key",
            VaultKind::ApiKey,
            VaultSource::Local,
            vec!["api.stripe.com"],
        );
        assert!(entry_matches(&entry, "", None));
        assert!(entry_matches(&entry, "stripe", Some(VaultKind::ApiKey)));
        assert!(entry_matches(&entry, "billing@", None));
        assert!(entry_matches(&entry, "api.stripe", None));
        assert!(!entry_matches(&entry, "stripe", Some(VaultKind::Password)));
        assert!(!entry_matches(&entry, "github", None));
    }

    #[test]
    fn vault_stats_counts_sources_and_domain_rules() {
        let entries = vec![
            sample_entry(
                "a",
                VaultKind::Password,
                VaultSource::Local,
                vec!["a.com", "b.com"],
            ),
            sample_entry(
                "b",
                VaultKind::Password,
                VaultSource::OnePassword,
                vec!["c.com"],
            ),
            sample_entry("c", VaultKind::Note, VaultSource::Local, vec![]),
        ];
        assert_eq!(vault_stats(&entries), (3, 2, 1, 3));
        assert_eq!(kind_count(&entries, VaultKind::Password), 2);
        assert_eq!(kind_count(&entries, VaultKind::Note), 1);
    }
}
