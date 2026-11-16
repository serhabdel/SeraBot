//! The failure and wait rows of the chat (v0.50): the failed-turn card with
//! its reason chips and ways forward, the waiting-for-a-model banner with a
//! countdown, and the inline compaction note.
//!
//! Every word here is fixed text or a chip from a fixed vocabulary; provider
//! text never reaches these rows.

use super::*;
use serabot_desktop::chat_failure::FailedCard;

/// The failed-turn card: why it failed, and Retry on the next route, Retry
/// on another route, or Ask main. One primary action: the fallback when
/// there is one, otherwise Ask main.
#[component]
pub(crate) fn FailedTurnCard(
    session: Signal<Session>,
    card: FailedCard,
    can_act: bool,
    ondismiss: EventHandler<()>,
) -> Element {
    let mut picking = use_signal(|| false);
    let has_fallback = card.fallback.is_some();
    let line = if has_fallback {
        chat_failure::FAILED_TURN_WITH_FALLBACK
    } else {
        chat_failure::FAILED_TURN
    };
    let has_alternatives = !card.alternatives.is_empty();
    rsx! {
        div { class: "banner err failed-card", role: "alert",
            div { class: "failed-body",
                span { class: "failed-line", "{line}" }
                if !card.reasons.is_empty() {
                    div { class: "reason-chips", "aria-label": "Why it failed",
                        for reason in card.reasons.iter() {
                            span { key: "{reason}", class: "reason-chip", "{reason}" }
                        }
                    }
                }
            }
            div { class: "failed-actions",
                if let Some(offer) = card.fallback.clone() {
                    button {
                        class: "btn primary sm",
                        r#type: "button",
                        disabled: !can_act,
                        onclick: {
                            let pick = offer.clone();
                            move |_| session.write().retry_on_fallback(pick.clone())
                        },
                        "{offer.label}"
                    }
                }
                if has_alternatives {
                    button {
                        class: "btn ghost sm",
                        r#type: "button",
                        "aria-expanded": if picking() { "true" } else { "false" },
                        "aria-haspopup": "listbox",
                        disabled: !can_act,
                        onclick: move |_| picking.set(!picking()),
                        "Retry on\u{2026}"
                    }
                }
                if card.ask_main {
                    button {
                        class: if has_fallback { "btn ghost sm" } else { "btn primary sm" },
                        r#type: "button",
                        disabled: !can_act,
                        onclick: move |_| session.write().ask_main(),
                        "Ask main"
                    }
                }
                button {
                    class: "btn ghost sm chat-banner-close",
                    r#type: "button",
                    title: "Dismiss",
                    "aria-label": "Dismiss",
                    onclick: move |_| ondismiss.call(()),
                    Icon { name: "x", size: 13 }
                }
            }
            if picking() && has_alternatives {
                ul { class: "failed-alt", role: "listbox", "aria-label": "Retry on another route",
                    for offer in card.alternatives.iter().cloned() {
                        li { key: "{offer.label}", role: "option",
                            button {
                                class: "failed-alt-item",
                                r#type: "button",
                                onclick: {
                                    let offer = offer.clone();
                                    move |_| {
                                        picking.set(false);
                                        session.write().retry_on_fallback(offer.clone());
                                    }
                                },
                                "{offer.label}"
                            }
                        }
                    }
                }
            }
        }
    }
}

/// "Waiting for a model. Trying again in 4:07." The line is rebuilt every
/// second by the session's repaint, and goes away when the turn moves on.
#[component]
pub(crate) fn WaitBanner(line: String) -> Element {
    rsx! {
        div { class: "banner wait-banner", role: "status", "aria-live": "polite",
            span { class: "wait-dot", "aria-hidden": "true" }
            span { "{line}" }
        }
    }
}

/// One quiet "Earlier messages were summarized" line in the transcript.
#[component]
pub(crate) fn CompactionNote(text: String) -> Element {
    rsx! {
        div { class: "compaction-note", role: "status",
            Icon { name: "sparkles", size: 12 }
            span { "{text}" }
        }
    }
}
