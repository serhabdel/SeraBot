//! Floating vault sheet (design B, contract section 3).
//!
//! The sheet is anchored to the Vault button in the chat header. This step
//! ships the trigger, the open/close animation, dismissal (Escape, outside
//! click, the button itself) and the empty state. Listing, adding,
//! replacing and deleting secrets arrive with the network follow-up; the
//! value field there is write-only and is never rendered back.

use crate::ui::Icon;
use dioxus::prelude::*;

/// The Vault button plus its sheet. `open` is owned by the caller so the
/// header and keyboard shortcuts can drive it too.
#[component]
pub fn VaultSheet(open: Signal<bool>, bot_name: String) -> Element {
    let mut open = open;
    let is_open = open();
    rsx! {
        div {
            class: "vault-anchor",
            onkeydown: move |event: KeyboardEvent| {
                if event.key() == Key::Escape && open() {
                    event.stop_propagation();
                    open.set(false);
                    let _ = dioxus::document::eval(
                        "document.getElementById('vault-trigger')?.focus();",
                    );
                }
            },
            button {
                id: "vault-trigger",
                class: if is_open { "btn vault-btn active" } else { "btn vault-btn" },
                "aria-haspopup": "dialog",
                "aria-expanded": if is_open { "true" } else { "false" },
                title: "Open the vault",
                onclick: move |_| {
                    let next = !open();
                    open.set(next);
                    if next {
                        let _ = dioxus::document::eval(
                            "setTimeout(() => document.getElementById('vault-sheet')?.focus(), 30);",
                        );
                    }
                },
                Icon { name: "lock", size: 15 }
                "Vault"
            }
            div {
                class: if is_open { "vault-scrim open" } else { "vault-scrim" },
                onclick: move |_| open.set(false),
            }
            div {
                id: "vault-sheet",
                class: if is_open { "vault-sheet open" } else { "vault-sheet" },
                role: "dialog",
                "aria-label": "{bot_name}'s vault",
                tabindex: -1,
                div { class: "vault-head",
                    Icon { name: "lock", size: 15 }
                    strong { class: "vault-title", "{bot_name}'s vault" }
                    button {
                        class: "icon-btn vault-close",
                        title: "Close",
                        "aria-label": "Close the vault",
                        onclick: move |_| open.set(false),
                        Icon { name: "x", size: 15 }
                    }
                }
                div { class: "vault-empty",
                    div { class: "empty-icon", Icon { name: "key", size: 20 } }
                    h2 { class: "empty-title", "Your vault is empty" }
                    p { class: "empty-text",
                        "Secrets you add here are encrypted. A bot can use one to sign in, but never sees its value."
                    }
                    button {
                        class: "btn primary",
                        disabled: true,
                        title: "Adding secrets arrives with the next update",
                        "Add secret"
                    }
                }
            }
        }
    }
}
