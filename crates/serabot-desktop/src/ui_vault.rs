//! Floating "Passwords & keys" sheet (design B, contract section 3).
//!
//! One vault per user, shared by all of that user's bots. The sheet is
//! anchored to the Vault button in the chat header. It lists entries (never a
//! value), adds or replaces one through a form whose value field is
//! write-only, and deletes after a confirm step. Requests run on the panel
//! worker (`panel.rs`); this file only draws and forwards clicks.

use crate::ui::Icon;
use dioxus::prelude::*;
use serabot_desktop::panel::last_used_label;
use serabot_desktop::session::Session;
use serabot_desktop::{VaultEntry, VaultKind};
use zeroize::Zeroize;

fn kind_icon(kind: VaultKind) -> &'static str {
    match kind {
        VaultKind::Password => "lock",
        VaultKind::ApiKey => "key",
        VaultKind::Note => "edit",
    }
}

/// The Vault button plus its sheet. `open` is owned by the caller so the
/// header and keyboard shortcuts can drive it too.
#[component]
pub fn VaultSheet(open: Signal<bool>, session: Signal<Session>) -> Element {
    let mut open = open;
    let mut session = session;
    let is_open = open();

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
            class: "vault-anchor",
            onkeydown: move |event: KeyboardEvent| {
                if event.key() == Key::Escape && open() {
                    event.stop_propagation();
                    let in_form = session.read().panel.vault.form.is_some();
                    if in_form {
                        session.write().panel.close_form();
                    } else {
                        open.set(false);
                        let _ = dioxus::document::eval(
                            "document.getElementById('vault-trigger')?.focus();",
                        );
                    }
                }
            },
            button {
                id: "vault-trigger",
                class: if is_open { "btn vault-btn active" } else { "btn vault-btn" },
                "aria-haspopup": "dialog",
                "aria-expanded": if is_open { "true" } else { "false" },
                title: "Passwords & keys",
                "aria-label": "Passwords & keys",
                onclick: move |_| {
                    let next = !open();
                    open.set(next);
                    if next {
                        session.write().panel.ask_vault_list();
                        let _ = dioxus::document::eval(
                            "setTimeout(() => document.getElementById('vault-sheet')?.focus(), 30);",
                        );
                    }
                },
                Icon { name: "lock", size: 15 }
                span { class: "btn-label", "Passwords & keys" }
            }
            div {
                class: if is_open { "vault-scrim open" } else { "vault-scrim" },
                onclick: move |_| open.set(false),
            }
            div {
                id: "vault-sheet",
                class: if is_open { "vault-sheet open" } else { "vault-sheet" },
                role: "dialog",
                "aria-label": "Passwords & keys",
                tabindex: -1,
                div { class: "vault-head",
                    Icon { name: "lock", size: 15 }
                    strong { class: "vault-title", "Passwords & keys" }
                    button {
                        class: "icon-btn vault-close",
                        title: "Close",
                        "aria-label": "Close passwords & keys",
                        onclick: move |_| open.set(false),
                        Icon { name: "x", size: 15 }
                    }
                }
                p { class: "vault-sub caption",
                    "Your bots can use these to sign in for you. They never see the actual value."
                }
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
                        div { class: "vault-foot",
                            button {
                                class: "btn primary",
                                onclick: move |_| session.write().panel.open_add_form(),
                                Icon { name: "plus", size: 15 }
                                "Add"
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
                div { class: "field",
                    label { class: "field-label", r#for: "vault-domains", "Websites it works on (optional)" }
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
                            "Leave empty to allow any site. example.com also covers its subdomains."
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
