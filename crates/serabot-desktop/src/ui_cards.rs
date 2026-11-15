//! Chat cards (0.23) drawn inline in the chat: a question with option buttons
//! and optional free text, and a secret request with a masked input that goes
//! straight to the server's vault. A resolved card is one quiet line.
//!
//! A secret value lives only in this component's input signal. It is moved out
//! of the signal into a zeroizing buffer on submit, so the signal is empty
//! before the request starts, and it never enters chat state or messages.

use crate::ui::{Icon, avatar_style, stamp};
use crate::ui_files::CONFIRM_RESET_JS;
use dioxus::prelude::*;
use serabot_desktop::chat_cards::{
    self, CardBody, CardState, CardView, MAX_ANSWER, QuestionCard, SecretCard,
};
use serabot_desktop::session::{Section, Session};
use serabot_desktop::{Approval, ApprovalDecision, TaskStatus};
use zeroize::Zeroizing;

/// One card at the foot of the feed. A pending card takes its full form; a
/// resolved one is a single line with its prompt context.
#[component]
pub(crate) fn ChatCardRow(mut session: Signal<Session>, card: CardView) -> Element {
    if card.state != CardState::Pending {
        let avatar = session.read().bot_avatar(&card.bot);
        let style = avatar_style(&card.bot);
        let line = chat_cards::resolved_line(&card);
        let prompt = chat_cards::resolved_prompt(&card);
        return rsx! {
            div { class: "chat-card-resolved", role: "status",
                span { class: "avatar bot sm", style: "{style}", "{avatar}" }
                div { class: "chat-card-resolved-copy",
                    if let Some(prompt) = prompt {
                        span { class: "tiny muted", "{prompt}" }
                        " · "
                    }
                    span { class: "chat-card-resolved-text", "{line}" }
                }
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

fn pulse_submitting(mut submitting: Signal<bool>) {
    submitting.set(true);
    spawn(async move {
        let mut done = dioxus::document::eval(CONFIRM_RESET_JS);
        let _ = done.recv::<bool>().await;
        submitting.set(false);
    });
}

#[component]
fn QuestionPrompt(
    mut session: Signal<Session>,
    id: String,
    bot: String,
    question: QuestionCard,
) -> Element {
    let mut free = use_signal(String::new);
    let mut input_focused = use_signal(|| false);
    let submitting = use_signal(|| false);
    let text = free();
    let chars = text.chars().count();
    let over_limit = chars > MAX_ANSWER;
    let sendable = !submitting() && chat_cards::clean_answer(&text).is_some();
    let name = session.read().bot_display_name(&bot);
    let avatar = session.read().bot_avatar(&bot);
    let style = avatar_style(&bot);
    let aria = format!("Question from {name}");
    let options = question.options.clone();
    rsx! {
        section {
            class: "chat-card question",
            "aria-label": "{aria}",
            onkeydown: {
                let id = id.clone();
                let options = options.clone();
                move |event: KeyboardEvent| {
                    if submitting() || input_focused() || event.is_composing() {
                        return;
                    }
                    let mods = event.modifiers();
                    if mods.ctrl() || mods.meta() || mods.alt() {
                        return;
                    }
                    if let Key::Character(ch) = event.key()
                        && let Some(digit) = ch.chars().next().filter(|c| ('1'..='6').contains(c))
                    {
                        let idx = (digit as usize) - ('1' as usize);
                        if let Some(option) = options.get(idx) {
                            event.prevent_default();
                            pulse_submitting(submitting);
                            session.write().answer_card(id.clone(), option.clone());
                        }
                    }
                }
            },
            div { class: "chat-card-head",
                span { class: "avatar bot sm", style: "{style}", "{avatar}" }
                span { class: "chat-card-title", "{name} asks" }
            }
            p { class: "chat-card-question", "{question.question}" }
            if let Some(context) = question.context.clone() {
                p { class: "chat-card-context", "{context}" }
            }
            if !options.is_empty() {
                div { class: "chat-card-options", role: "group", "aria-label": "Answers",
                    for (idx, option) in options.iter().enumerate() {
                        button {
                            key: "{option}",
                            class: "btn sm chat-card-option",
                            r#type: "button",
                            disabled: submitting(),
                            title: "Choose option {idx + 1}",
                            onclick: {
                                let id = id.clone();
                                let option = option.clone();
                                move |_| {
                                    pulse_submitting(submitting);
                                    session.write().answer_card(id.clone(), option.clone());
                                }
                            },
                            span { class: "kbd", "{idx + 1}" }
                            " {option}"
                        }
                    }
                }
            }
            if question.allow_free_text {
                div { class: "chat-card-free",
                    input {
                        class: if over_limit { "chat-card-input invalid" } else { "chat-card-input" },
                        r#type: "text",
                        disabled: submitting(),
                        "aria-label": "Your answer",
                        "aria-invalid": if over_limit { "true" } else { "false" },
                        placeholder: "Type another answer",
                        value: "{text}",
                        onfocus: move |_| input_focused.set(true),
                        onblur: move |_| input_focused.set(false),
                        oninput: move |event| free.set(event.value()),
                        onkeydown: {
                            let id = id.clone();
                            move |event: KeyboardEvent| {
                                if event.key() == Key::Enter && !event.is_composing() && !submitting() {
                                    // The read guard ends with this statement, before the write.
                                    let answer = chat_cards::clean_answer(&free.read());
                                    if let Some(answer) = answer {
                                        free.set(String::new());
                                        pulse_submitting(submitting);
                                        session.write().answer_card(id.clone(), answer);
                                    }
                                }
                            }
                        },
                    }
                    button {
                        class: "btn primary sm",
                        r#type: "button",
                        disabled: !sendable,
                        onclick: {
                            let id = id.clone();
                            move |_| {
                                let answer = chat_cards::clean_answer(&free.read());
                                if let Some(answer) = answer {
                                    free.set(String::new());
                                    pulse_submitting(submitting);
                                    session.write().answer_card(id.clone(), answer);
                                }
                            }
                        },
                        if submitting() { "Sending\u{2026}" } else { "Send" }
                    }
                }
                if chars >= (MAX_ANSWER * 4) / 5 {
                    p {
                        class: if over_limit { "field-error" } else { "tiny muted" },
                        if over_limit {
                            "Answer is {chars - MAX_ANSWER} characters over the {MAX_ANSWER}-character limit."
                        } else {
                            "{chars} / {MAX_ANSWER} characters"
                        }
                    }
                }
            }
            div { class: "chat-card-actions",
                button {
                    class: "btn ghost sm",
                    r#type: "button",
                    disabled: submitting(),
                    onclick: move |_| {
                        pulse_submitting(submitting);
                        session.write().decline_card(id.clone());
                    },
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
    let submitting = use_signal(|| false);
    let filled = !value.read().is_empty() && !submitting();
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
                span { class: "secret-prompt-badge",
                    Icon { name: "shield", size: 12 }
                    " Write-only encrypted \u{2022} Never visible to bots or AI models"
                }
            }
            p { class: "chat-card-facts",
                span { "Key: {secret.name}" }
                span { "Kind: {kind}" }
                if let Some(username) = secret.username.clone() {
                    span { "Account: {username}" }
                }
            }
            if secret.replaces {
                p { class: "chat-card-note", "This replaces your existing \u{201C}{secret.name}\u{201D} entry in Vault." }
            }
            if !secret.domains.is_empty() {
                div { class: "chat-card-domains", "aria-label": "Provider sites",
                    span { class: "tiny muted", "Provider / Sites:" }
                    for domain in secret.domains.clone() {
                        span { key: "{domain}", class: "chip", "{domain}" }
                    }
                }
            }
            if !secret.reason.is_empty() {
                p { class: "chat-card-reason", "{secret.reason}" }
            }
            p { class: "chat-card-note",
                "Stored write-only in your encrypted Vault. Neither {name} nor any AI model can read the raw value."
            }
            div { class: "chat-card-field",
                input {
                    class: "chat-card-input",
                    r#type: "{input_type}",
                    disabled: submitting(),
                    autocomplete: "off",
                    spellcheck: "false",
                    placeholder: "Paste or type secret value\u{2026}",
                    "aria-label": "Secret value",
                    value: "{shown}",
                    oninput: move |event| value.set(event.value()),
                    onkeydown: {
                        let id = id.clone();
                        move |event: KeyboardEvent| {
                            if event.key() == Key::Enter && !event.is_composing() && !submitting() {
                                let secret = Zeroizing::new(std::mem::take(&mut *value.write()));
                                if !secret.is_empty() {
                                    reveal.set(false);
                                    pulse_submitting(submitting);
                                    session.write().submit_secret_card(id.clone(), secret);
                                }
                            }
                        }
                    },
                }
                button {
                    class: "btn ghost sm",
                    r#type: "button",
                    disabled: submitting(),
                    "aria-pressed": if reveal() { "true" } else { "false" },
                    "aria-label": if reveal() { "Hide what I typed" } else { "Show what I typed" },
                    title: "{reveal_label}",
                    onclick: move |_| reveal.set(!reveal()),
                    Icon { name: if reveal() { "eye-off" } else { "eye" }, size: 15 }
                }
            }
            div { class: "chat-card-actions",
                button {
                    class: "btn primary sm",
                    r#type: "button",
                    disabled: !filled,
                    onclick: {
                        let id = id.clone();
                        move |_| {
                            // Move the value out of the signal: nothing is left behind.
                            let secret = Zeroizing::new(std::mem::take(&mut *value.write()));
                            if !secret.is_empty() {
                                reveal.set(false);
                                pulse_submitting(submitting);
                                session.write().submit_secret_card(id.clone(), secret);
                            }
                        }
                    },
                    if submitting() { "Saving\u{2026}" } else { "Save to vault" }
                }
                button {
                    class: "btn ghost sm",
                    r#type: "button",
                    disabled: submitting(),
                    onclick: move |_| {
                        value.set(String::new());
                        reveal.set(false);
                        pulse_submitting(submitting);
                        session.write().decline_card(id.clone());
                    },
                    "Decline"
                }
                button {
                    class: "btn ghost sm",
                    r#type: "button",
                    disabled: submitting(),
                    title: "Inspect or manage saved secrets in Vault",
                    onclick: move |_| session.write().open_section(Section::Vault),
                    Icon { name: "external", size: 13 }
                    " Open Vault"
                }
            }
        }
    }
}

/// Partition pending approvals into `(for_current_conversation, global_other)`
/// so the active conversation shows full inline cards and Main also surfaces
/// any other pending approvals as compact cards.
pub(crate) fn partition_inline_approvals<'a>(
    approvals: &'a [Approval],
    step_approval_ids: &[&str],
    is_main: bool,
    active_task_awaiting: bool,
) -> (Vec<&'a Approval>, Vec<&'a Approval>) {
    let mut local = Vec::new();
    let mut global = Vec::new();
    for approval in approvals {
        if step_approval_ids.contains(&approval.id.as_str())
            || (active_task_awaiting && step_approval_ids.is_empty())
        {
            local.push(approval);
        } else if is_main {
            global.push(approval);
        }
    }
    (local, global)
}

/// Extract an optional destination hint from an approval summary (e.g. URL or
/// recipient after " to " / " -> "), if present.
fn approval_destination(summary: &str) -> Option<&str> {
    summary
        .split_once(" -> ")
        .or_else(|| summary.split_once(" to "))
        .map(|(_, dest)| dest.trim())
        .filter(|dest| !dest.is_empty())
}

/// Rich inline approval cards rendered directly above the composer in ChatView.
#[component]
pub(crate) fn InlineApprovals(mut session: Signal<Session>) -> Element {
    let busy_id = use_signal(|| Option::<String>::None);
    let (local_approvals, global_approvals) = {
        let state = session.read();
        if state.state.approvals.is_empty() {
            return rsx! {};
        }
        let conv = state.conversation.as_str();
        let is_main = conv == "dir:main";
        let steps = &state.state.activity.steps;
        let step_ids: Vec<&str> = steps
            .iter()
            .filter_map(|step| step.approval_id.as_deref())
            .collect();
        let active_task_awaiting = state.state.task.as_ref().is_some_and(|task| {
            task.status == TaskStatus::AwaitingApproval
                && state.conversation_bot(conv).as_deref() == Some(task.bot.as_str())
        });
        let (local, global) = partition_inline_approvals(
            &state.state.approvals,
            &step_ids,
            is_main,
            active_task_awaiting,
        );
        (
            local.into_iter().cloned().collect::<Vec<_>>(),
            global.into_iter().cloned().collect::<Vec<_>>(),
        )
    };
    if local_approvals.is_empty() && global_approvals.is_empty() {
        return rsx! {};
    }
    let active_busy = busy_id();
    rsx! {
        div { class: "inline-approvals", role: "region", "aria-label": "Pending approvals",
            for approval in local_approvals {
                {
                    let id = approval.id.clone();
                    let row_busy = active_busy.as_deref() == Some(id.as_str());
                    let exp_label = stamp(&approval.expires_at);
                    let dest = approval_destination(&approval.summary).map(str::to_owned);
                    rsx! {
                        section { class: "chat-card approval-card", key: "{id}",
                            div { class: "approval-card-head",
                                span { class: "chip accent ap-tool", "{approval.tool}" }
                                span { class: "tiny muted", title: "{approval.expires_at}",
                                    if exp_label.is_empty() { "Pending approval" } else { "Expires {exp_label}" }
                                }
                            }
                            p { class: "approval-card-summary", "{approval.summary}" }
                            if let Some(dest) = dest {
                                div { class: "approval-card-dest",
                                    span { class: "tiny muted", "Destination:" }
                                    span { class: "tiny mono", "{dest}" }
                                }
                            }
                            div { class: "approval-card-actions",
                                button {
                                    class: "btn primary sm",
                                    r#type: "button",
                                    disabled: row_busy,
                                    onclick: {
                                        let id = id.clone();
                                        let mut busy_id = busy_id;
                                        move |_| {
                                            busy_id.set(Some(id.clone()));
                                            session.write().decide(id.clone(), ApprovalDecision::Once);
                                        }
                                    },
                                    "Approve"
                                }
                                button {
                                    class: "btn danger sm",
                                    r#type: "button",
                                    disabled: row_busy,
                                    onclick: {
                                        let id = id.clone();
                                        let mut busy_id = busy_id;
                                        move |_| {
                                            busy_id.set(Some(id.clone()));
                                            session.write().decide(id.clone(), ApprovalDecision::Deny);
                                        }
                                    },
                                    "Deny"
                                }
                            }
                        }
                    }
                }
            }
            for approval in global_approvals {
                {
                    let id = approval.id.clone();
                    let row_busy = active_busy.as_deref() == Some(id.as_str());
                    let exp_label = stamp(&approval.expires_at);
                    rsx! {
                        section { class: "chat-card approval-card compact", key: "global-{id}",
                            div { class: "approval-card-compact-row",
                                span { class: "chip accent ap-tool", "{approval.tool}" }
                                span { class: "approval-card-summary", title: "{approval.summary}", "{approval.summary}" }
                                if !exp_label.is_empty() {
                                    span { class: "tiny muted", title: "{approval.expires_at}", "Expires {exp_label}" }
                                }
                                div { class: "approval-card-actions",
                                    button {
                                        class: "btn primary sm",
                                        r#type: "button",
                                        disabled: row_busy,
                                        onclick: {
                                            let id = id.clone();
                                            let mut busy_id = busy_id;
                                            move |_| {
                                                busy_id.set(Some(id.clone()));
                                                session.write().decide(id.clone(), ApprovalDecision::Once);
                                            }
                                        },
                                        "Approve"
                                    }
                                    button {
                                        class: "btn danger sm",
                                        r#type: "button",
                                        disabled: row_busy,
                                        onclick: {
                                            let id = id.clone();
                                            let mut busy_id = busy_id;
                                            move |_| {
                                                busy_id.set(Some(id.clone()));
                                                session.write().decide(id.clone(), ApprovalDecision::Deny);
                                            }
                                        },
                                        "Deny"
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

#[cfg(test)]
mod tests {
    use super::*;

    fn approval(id: &str) -> Approval {
        Approval {
            id: id.into(),
            action_id: format!("act-{id}"),
            tool: "http.post".into(),
            summary: "Send report to https://example.com".into(),
            expires_at: "2026-11-15T19:00:00Z".into(),
        }
    }

    #[test]
    fn inline_approvals_partition_local_and_global_for_main() {
        let items = vec![approval("a1"), approval("a2")];
        let (local, global) = partition_inline_approvals(&items, &["a1"], true, false);
        assert_eq!(local.len(), 1);
        assert_eq!(local[0].id, "a1");
        assert_eq!(global.len(), 1);
        assert_eq!(global[0].id, "a2");

        // Non-main conversations do not show unrelated global approvals.
        let (local_other, global_other) = partition_inline_approvals(&items, &["a1"], false, false);
        assert_eq!(local_other.len(), 1);
        assert!(global_other.is_empty());
    }

    #[test]
    fn approval_destination_extracts_target() {
        assert_eq!(
            approval_destination("Send email to ops@example.com"),
            Some("ops@example.com")
        );
        assert_eq!(approval_destination("Post -> #general"), Some("#general"));
        assert_eq!(approval_destination("Run local check"), None);
    }
}
