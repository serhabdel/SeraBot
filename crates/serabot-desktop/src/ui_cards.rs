//! Chat cards (0.23) drawn inline in the chat: a question with option buttons
//! and optional free text, and a secret request with a masked input that goes
//! straight to the server's vault. A resolved card is one quiet line.
//!
//! A secret value lives only in this component's input signal. It is moved out
//! of the signal into a zeroizing buffer on submit, so the signal is empty
//! before the request starts, and it never enters chat state or messages.

use crate::ui::{Icon, avatar_style};
use dioxus::prelude::*;
use serabot_desktop::chat_cards::{self, CardBody, CardState, CardView, QuestionCard, SecretCard};
use serabot_desktop::session::Session;
use zeroize::Zeroizing;

/// One card at the foot of the feed. A pending card takes its full form; a
/// resolved one is a single line.
#[component]
pub(crate) fn ChatCardRow(mut session: Signal<Session>, card: CardView) -> Element {
    if card.state != CardState::Pending {
        let avatar = session.read().bot_avatar(&card.bot);
        let style = avatar_style(&card.bot);
        let line = chat_cards::resolved_line(&card);
        return rsx! {
            div { class: "chat-card-resolved", role: "status",
                span { class: "avatar bot sm", style: "{style}", "{avatar}" }
                span { class: "chat-card-resolved-text", "{line}" }
            }
        };
    }
    match card.body {
        CardBody::Question(question) => rsx! {
            QuestionPrompt { session, id: card.id.clone(), bot: card.bot.clone(), question }
        },
        CardBody::Secret(secret) => rsx! {
            SecretPrompt { session, id: card.id.clone(), bot: card.bot.clone(), secret }
        },
    }
}

#[component]
fn QuestionPrompt(
    mut session: Signal<Session>,
    id: String,
    bot: String,
    question: QuestionCard,
) -> Element {
    let mut free = use_signal(String::new);
    let text = free();
    let sendable = chat_cards::clean_answer(&text).is_some();
    let name = session.read().bot_display_name(&bot);
    let avatar = session.read().bot_avatar(&bot);
    let style = avatar_style(&bot);
    let aria = format!("Question from {name}");
    rsx! {
        section { class: "chat-card question", "aria-label": "{aria}",
            div { class: "chat-card-head",
                span { class: "avatar bot sm", style: "{style}", "{avatar}" }
                span { class: "chat-card-title", "{name} asks" }
            }
            p { class: "chat-card-question", "{question.question}" }
            if let Some(context) = question.context.clone() {
                p { class: "chat-card-context", "{context}" }
            }
            if !question.options.is_empty() {
                div { class: "chat-card-options", role: "group", "aria-label": "Answers",
                    for option in question.options.clone() {
                        button {
                            key: "{option}",
                            class: "btn sm chat-card-option",
                            onclick: {
                                let id = id.clone();
                                let option = option.clone();
                                move |_| session.write().answer_card(id.clone(), option.clone())
                            },
                            "{option}"
                        }
                    }
                }
            }
            if question.allow_free_text {
                div { class: "chat-card-free",
                    input {
                        class: "chat-card-input",
                        r#type: "text",
                        "aria-label": "Your answer",
                        placeholder: "Type another answer",
                        value: "{text}",
                        oninput: move |event| free.set(event.value()),
                        onkeydown: {
                            let id = id.clone();
                            move |event: KeyboardEvent| {
                                if event.key() == Key::Enter {
                                    // The read guard ends with this statement, before the write.
                                    let answer = chat_cards::clean_answer(&free.read());
                                    if let Some(answer) = answer {
                                        free.set(String::new());
                                        session.write().answer_card(id.clone(), answer);
                                    }
                                }
                            }
                        },
                    }
                    button {
                        class: "btn primary sm",
                        disabled: !sendable,
                        onclick: {
                            let id = id.clone();
                            move |_| {
                                let answer = chat_cards::clean_answer(&free.read());
                                if let Some(answer) = answer {
                                    free.set(String::new());
                                    session.write().answer_card(id.clone(), answer);
                                }
                            }
                        },
                        "Send"
                    }
                }
            }
            div { class: "chat-card-actions",
                button {
                    class: "btn ghost sm",
                    onclick: move |_| session.write().decline_card(id.clone()),
                    "Skip"
                }
            }
        }
    }
}

#[component]
fn SecretPrompt(
    mut session: Signal<Session>,
    id: String,
    bot: String,
    secret: SecretCard,
) -> Element {
    let mut value = use_signal(String::new);
    let mut reveal = use_signal(|| false);
    let filled = !value.read().is_empty();
    let shown = value();
    let input_type = if reveal() { "text" } else { "password" };
    let reveal_label = if reveal() { "Hide" } else { "Show" };
    let name = session.read().bot_display_name(&bot);
    let avatar = session.read().bot_avatar(&bot);
    let style = avatar_style(&bot);
    let title = chat_cards::secret_title(secret.purpose);
    let kind = chat_cards::secret_kind_label(secret.kind);
    let aria = format!("Secret request from {name}");
    rsx! {
        section { class: "chat-card secret", "aria-label": "{aria}",
            div { class: "chat-card-head",
                span { class: "avatar bot sm", style: "{style}", "{avatar}" }
                span { class: "chat-card-title",
                    Icon { name: "lock", size: 14 }
                    " {title}"
                }
            }
            p { class: "chat-card-facts",
                span { "Name: {secret.name}" }
                span { "Kind: {kind}" }
                if let Some(username) = secret.username.clone() {
                    span { "Username: {username}" }
                }
            }
            if secret.replaces {
                p { class: "chat-card-note", "This replaces your existing “{secret.name}” entry." }
            }
            if !secret.domains.is_empty() {
                div { class: "chat-card-domains", "aria-label": "Sites",
                    for domain in secret.domains.clone() {
                        span { key: "{domain}", class: "chip", "{domain}" }
                    }
                }
            }
            if !secret.reason.is_empty() {
                p { class: "chat-card-reason", "{secret.reason}" }
            }
            p { class: "chat-card-note",
                "The value goes straight into your encrypted vault. The bot never sees it."
            }
            div { class: "chat-card-field",
                input {
                    class: "chat-card-input",
                    r#type: "{input_type}",
                    autocomplete: "off",
                    spellcheck: "false",
                    "aria-label": "Secret value",
                    value: "{shown}",
                    oninput: move |event| value.set(event.value()),
                }
                button {
                    class: "btn ghost sm",
                    onclick: move |_| reveal.set(!reveal()),
                    "{reveal_label}"
                }
            }
            div { class: "chat-card-actions",
                button {
                    class: "btn primary sm",
                    disabled: !filled,
                    onclick: {
                        let id = id.clone();
                        move |_| {
                            // Move the value out of the signal: nothing is left behind.
                            let secret = Zeroizing::new(std::mem::take(&mut *value.write()));
                            if !secret.is_empty() {
                                session.write().submit_secret_card(id.clone(), secret);
                            }
                        }
                    },
                    "Save to vault"
                }
                button {
                    class: "btn ghost sm",
                    onclick: move |_| {
                        value.set(String::new());
                        session.write().decline_card(id.clone());
                    },
                    "Decline"
                }
            }
        }
    }
}
