//! Right-hand dock (design B, contract section 3).
//!
//! The dock lives on the right edge and holds the 'Browser' panel of the
//! server browser. This step only ships the shell: the slot, its slide
//! animation, the header and the empty state. It stays hidden until the
//! `open` flag in `Shell` is raised; the follow-up that talks to
//! `/v1/browser/*` flips that flag while the acting user has an active
//! browser session and fills `dock-view` with the live screenshot.

use crate::ui::Icon;
use dioxus::prelude::*;

/// The browser dock. Always mounted so it can slide in from, and leave
/// towards, the right edge; `open` only toggles the slot. A closed dock is
/// `visibility: hidden`, so it takes no focus and is not read by screen
/// readers.
#[component]
pub fn BrowserDock(open: bool, on_close: EventHandler<()>) -> Element {
    rsx! {
        aside {
            class: if open { "dock-slot open" } else { "dock-slot" },
            "aria-label": "Browser",
            div { class: "dock",
                div { class: "dock-head",
                    strong { class: "dock-title", "Browser" }
                    span { class: "pill", "idle" }
                    span { class: "dock-control caption", "Nobody is in control" }
                }
                div { class: "dock-url", "aria-hidden": "true",
                    Icon { name: "globe", size: 14 }
                    span { "No page open" }
                }
                div { class: "dock-view",
                    div { class: "empty",
                        div { class: "empty-icon", Icon { name: "globe", size: 20 } }
                        h2 { class: "empty-title", "No browser session" }
                        p { class: "empty-text",
                            "When a bot opens a page in the server browser, it shows up here live."
                        }
                    }
                }
                div { class: "dock-foot",
                    button { class: "btn primary", disabled: true, "Take over" }
                    button { class: "btn ghost", disabled: true, "Hand back" }
                    button {
                        class: "btn ghost",
                        onclick: move |_| on_close.call(()),
                        "Close"
                    }
                }
            }
        }
    }
}
