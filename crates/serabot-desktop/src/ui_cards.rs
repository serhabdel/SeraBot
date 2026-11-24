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

/// Tool category badge shown on approval cards.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ToolCategory {
    Mail,
    OneDrive,
    Browser,
    ProjectCheck,
    Tool,
}

impl ToolCategory {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Mail => "Mail",
            Self::OneDrive => "OneDrive",
            Self::Browser => "Browser",
            Self::ProjectCheck => "Project Check",
            Self::Tool => "Tool",
        }
    }

    pub(crate) fn class_suffix(self) -> &'static str {
        match self {
            Self::Mail => "mail",
            Self::OneDrive => "onedrive",
            Self::Browser => "browser",
            Self::ProjectCheck => "project-check",
            Self::Tool => "tool",
        }
    }
}

/// Map a tool identifier (e.g. `mail_send`, `onedrive_upload`, `browser_upload`,
/// `project_check`) to a short category badge.
pub(crate) fn tool_category_badge(tool: &str) -> ToolCategory {
    let lower = tool.trim().to_ascii_lowercase();
    if lower == "project_check" || lower.starts_with("project_") {
        ToolCategory::ProjectCheck
    } else if lower.starts_with("mail_")
        || lower.starts_with("agentmail_")
        || lower.starts_with("outlook.")
        || lower.starts_with("gmail.")
    {
        ToolCategory::Mail
    } else if lower.starts_with("onedrive_") || lower.contains("onedrive") {
        ToolCategory::OneDrive
    } else if lower.starts_with("browser_") || lower.starts_with("web_") {
        ToolCategory::Browser
    } else {
        ToolCategory::Tool
    }
}

/// Tools that must never offer "Always allow" in the UI (AGENTS.md +
/// destructive/vault/upload/mail_send policy).
pub(crate) fn tool_allows_standing_grant(tool: &str, summary: &str) -> bool {
    let lower = tool.trim().to_ascii_lowercase();
    !(matches!(
        lower.as_str(),
        "memory_delete"
            | "agentmail_send"
            | "telegram_send_document"
            | "onedrive_upload"
            | "local_write"
            | "local_export"
            | "github_create_issue"
            | "github_comment_issue"
            | "todoist_add_task"
            | "todoist_complete_task"
            | "slack_post_message"
            | "whatsapp_send_message"
            | "whatsapp_send_template"
            | "browser_upload"
            | "browser_forget_logins"
            | "project_check"
            | "mail_send"
            | "mail_flag"
            | "mail_move"
            | "mail_delete"
    ) || lower.starts_with("vault_")
        || lower.starts_with("plugin.")
        || lower.starts_with("github_")
        || lower.contains("upload")
        || lower.contains("secret")
        || (lower == "browser_type" && summary.to_ascii_lowercase().contains("secret '")))
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

/// Extract an optional destination chip from an approval summary (host, email,
/// OneDrive path, or target after " -> " / " to " / " into " / " in ").
pub(crate) fn approval_destination(summary: &str) -> Option<String> {
    if let Some((_, dest)) = summary
        .split_once(" -> ")
        .or_else(|| summary.split_once(" to "))
        .or_else(|| summary.split_once(" into "))
    {
        let cleaned = clean_destination(dest);
        if !cleaned.is_empty() {
            return Some(cleaned);
        }
    }
    // Fall back to scanning tokens for an explicit URL host, email, or path.
    for token in summary.split_whitespace() {
        let trimmed = token.trim_matches(|c: char| {
            matches!(c, ',' | ';' | ')' | '(' | '"' | '\'' | '<' | '>' | '.')
        });
        if trimmed.contains("://") {
            let host = trimmed
                .split_once("://")
                .map(|(_, rest)| rest.split(&['/', '?', '#'][..]).next().unwrap_or(rest))
                .unwrap_or(trimmed);
            if !host.is_empty() {
                return Some(host.to_owned());
            }
        }
        if let Some((local, domain)) = trimmed.split_once('@')
            && !local.is_empty()
            && domain.contains('.')
        {
            return Some(trimmed.to_owned());
        }
    }
    None
}

fn clean_destination(raw: &str) -> String {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return String::new();
    }
    let first = trimmed.split(['\n', '\r']).next().unwrap_or(trimmed).trim();
    // Strip trailing prose clauses like " in OneDrive" or " on the current page"
    // only when preceded by a standalone token, otherwise keep full path/target.
    if first.starts_with("http://") || first.starts_with("https://") {
        let token = first.split_whitespace().next().unwrap_or(first);
        return token
            .split_once("://")
            .map(|(_, rest)| rest.split(&['/', '?', '#'][..]).next().unwrap_or(rest))
            .unwrap_or(token)
            .to_owned();
    }
    first.trim_end_matches(['.', ',', ';']).to_owned()
}

/// Extract a 64-char SHA-256 artifact hash if mentioned in an approval summary
/// (for older/compact approvals before `Approval.artifact_hash` is populated).
pub(crate) fn approval_artifact_hash(summary: &str) -> Option<String> {
    for token in summary.split(|c: char| !c.is_ascii_hexdigit()) {
        if token.len() == 64 {
            return Some(token.to_ascii_lowercase());
        }
    }
    None
}

fn current_epoch_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Format an expiry timestamp into a short countdown pill label relative to
/// `now_secs` (e.g. `"Expires in 14m"`, `"Expires in 45s"`, `"Expired"`),
/// falling back to a compact UTC stamp if `expires_at` cannot be parsed.
pub(crate) fn format_expiry_countdown(expires_at: &str, now_secs: i64) -> String {
    let trimmed = expires_at.trim();
    if trimmed.is_empty() {
        return "Pending approval".to_owned();
    }
    if let Some(exp_secs) = serabot_desktop::workflows::epoch_secs(trimmed) {
        let remaining = exp_secs - now_secs;
        if remaining <= 0 {
            return "Expired".to_owned();
        }
        if remaining < 60 {
            return format!("Expires in {remaining}s");
        }
        if remaining < 3_600 {
            let mins = remaining / 60;
            return format!("Expires in {mins}m");
        }
        if remaining < 86_400 {
            let hours = remaining / 3_600;
            let mins = (remaining % 3_600) / 60;
            if mins > 0 {
                return format!("Expires in {hours}h {mins}m");
            }
            return format!("Expires in {hours}h");
        }
        let days = remaining / 86_400;
        return format!("Expires in {days}d");
    }
    let fallback = stamp(trimmed);
    if fallback.is_empty() {
        "Pending approval".to_owned()
    } else {
        format!("Expires {fallback}")
    }
}

fn trigger_decision(
    mut session: Signal<Session>,
    mut confirm_always: Signal<bool>,
    mut local_busy: Signal<bool>,
    on_decide: Option<EventHandler<ApprovalDecision>>,
    id: String,
    decision: ApprovalDecision,
) {
    confirm_always.set(false);
    if let Some(handler) = on_decide {
        handler.call(decision);
    } else {
        local_busy.set(true);
        session.write().decide(id, decision);
    }
}

/// Unified approval card used in both inline chat approvals and the Approvals
/// page. Supports a `compact` mode for secondary/global approvals.
#[component]
pub(crate) fn ApprovalCard(
    mut session: Signal<Session>,
    id: String,
    tool: String,
    summary: String,
    expires_at: String,
    #[props(default = false)] compact: bool,
    #[props(default)] bot_id: Option<String>,
    #[props(default)] artifact_hash: Option<String>,
    #[props(default = false)] allow_standing_grant: bool,
    #[props(default = false)] busy: bool,
    #[props(default)] on_decide: Option<EventHandler<ApprovalDecision>>,
) -> Element {
    let mut confirm_always = use_signal(|| false);
    let local_busy = use_signal(|| false);
    let is_busy = busy || local_busy();

    let (bot_name, bot_avatar, bot_style) = {
        let state = session.read();
        let resolved_bot = bot_id
            .as_deref()
            .filter(|id| !id.trim().is_empty())
            .map(str::to_owned)
            .or_else(|| state.conversation_bot(&state.conversation))
            .unwrap_or_else(|| "main".to_owned());
        (
            state.bot_display_name(&resolved_bot),
            state.bot_avatar(&resolved_bot),
            avatar_style(&resolved_bot),
        )
    };

    let category = tool_category_badge(&tool);
    let category_label = category.label();
    let category_class = format!("chip ap-cat ap-cat-{}", category.class_suffix());
    let countdown = format_expiry_countdown(&expires_at, current_epoch_secs());
    let expired = countdown == "Expired";
    let dest = approval_destination(&summary);
    let hash = artifact_hash
        .filter(|h| h.len() == 64 && h.bytes().all(|b| b.is_ascii_hexdigit()))
        .or_else(|| approval_artifact_hash(&summary));
    let can_always = allow_standing_grant && tool_allows_standing_grant(&tool, &summary);

    let card_class = if compact {
        "chat-card approval-card compact"
    } else {
        "chat-card approval-card"
    };

    if compact {
        return rsx! {
            section { class: "{card_class}", key: "approval-{id}", "aria-label": "Pending approval from {bot_name}",
                div { class: "approval-card-compact-row",
                    span { class: "avatar bot sm", style: "{bot_style}", title: "{bot_name}", "{bot_avatar}" }
                    span { class: "approval-card-bot", "{bot_name}" }
                    span { class: "{category_class}", "{category_label}" }
                    span { class: "chip accent ap-tool", "{tool}" }
                    span { class: "approval-card-summary", title: "{summary}", "{summary}" }
                    if let Some(dest) = dest {
                        span { class: "chip mono ap-dest-chip", title: "{dest}", "{dest}" }
                    }
                    if let Some(hash) = hash {
                        button {
                            class: "btn ghost sm ap-artifact-link",
                            r#type: "button",
                            title: "Preview artifact {hash}",
                            onclick: move |_| session.write().open_preview_dock(&hash),
                            Icon { name: "external", size: 12 }
                            "Preview"
                        }
                    }
                    span {
                        class: if expired { "pill ap-expiry expired" } else { "pill ap-expiry" },
                        title: "{expires_at}",
                        "{countdown}"
                    }
                    div { class: "approval-card-actions",
                        button {
                            class: "btn primary sm",
                            r#type: "button",
                            disabled: is_busy,
                            onclick: {
                                let id = id.clone();
                                move |_| {
                                    trigger_decision(
                                        session,
                                        confirm_always,
                                        local_busy,
                                        on_decide,
                                        id.clone(),
                                        ApprovalDecision::Once,
                                    );
                                }
                            },
                            "Approve"
                        }
                        if can_always {
                            if confirm_always() {
                                div { class: "conv-confirm",
                                    button {
                                        class: "btn primary sm",
                                        r#type: "button",
                                        disabled: is_busy,
                                        onclick: {
                                            let id = id.clone();
                                            move |_| {
                                                trigger_decision(
                                                    session,
                                                    confirm_always,
                                                    local_busy,
                                                    on_decide,
                                                    id.clone(),
                                                    ApprovalDecision::Always,
                                                );
                                            }
                                        },
                                        "Confirm Always"
                                    }
                                    button {
                                        class: "btn ghost sm",
                                        r#type: "button",
                                        disabled: is_busy,
                                        onclick: move |_| confirm_always.set(false),
                                        "Cancel"
                                    }
                                }
                            } else {
                                button {
                                    class: "btn ghost sm",
                                    r#type: "button",
                                    disabled: is_busy,
                                    onclick: move |_| confirm_always.set(true),
                                    "Always allow"
                                }
                            }
                        }
                        button {
                            class: "btn danger sm",
                            r#type: "button",
                            disabled: is_busy,
                            onclick: {
                                let id = id.clone();
                                move |_| {
                                    trigger_decision(
                                        session,
                                        confirm_always,
                                        local_busy,
                                        on_decide,
                                        id.clone(),
                                        ApprovalDecision::Deny,
                                    );
                                }
                            },
                            "Reject"
                        }
                    }
                }
            }
        };
    }

    rsx! {
        section { class: "{card_class}", key: "approval-{id}", "aria-label": "Pending approval from {bot_name}",
            div { class: "approval-card-head",
                div { class: "approval-card-who",
                    span { class: "avatar bot sm", style: "{bot_style}", "{bot_avatar}" }
                    span { class: "approval-card-bot", "{bot_name}" }
                    span { class: "{category_class}", "{category_label}" }
                    span { class: "chip accent ap-tool", "{tool}" }
                }
                span {
                    class: if expired { "pill ap-expiry expired" } else { "pill ap-expiry" },
                    title: "{expires_at}",
                    "{countdown}"
                }
            }
            p { class: "approval-card-summary", "{summary}" }
            if dest.is_some() || hash.is_some() {
                div { class: "approval-card-meta",
                    if let Some(dest) = dest {
                        div { class: "approval-card-dest",
                            span { class: "tiny muted", "Destination" }
                            span { class: "chip mono ap-dest-chip", title: "{dest}", "{dest}" }
                        }
                    }
                    if let Some(hash) = hash {
                        {
                            let short = format!("{}\u{2026}", &hash[..12.min(hash.len())]);
                            rsx! {
                                button {
                                    class: "btn ghost sm ap-artifact-link",
                                    r#type: "button",
                                    title: "Preview artifact {hash}",
                                    onclick: move |_| session.write().open_preview_dock(&hash),
                                    Icon { name: "external", size: 13 }
                                    "Preview artifact ({short})"
                                }
                            }
                        }
                    }
                }
            }
            div { class: "approval-card-actions",
                button {
                    class: "btn primary sm",
                    r#type: "button",
                    disabled: is_busy,
                    onclick: {
                        let id = id.clone();
                        move |_| {
                            trigger_decision(
                                session,
                                confirm_always,
                                local_busy,
                                on_decide,
                                id.clone(),
                                ApprovalDecision::Once,
                            );
                        }
                    },
                    "Approve"
                }
                if can_always {
                    if confirm_always() {
                        div { class: "conv-confirm",
                            button {
                                class: "btn primary sm",
                                r#type: "button",
                                disabled: is_busy,
                                onclick: {
                                    let id = id.clone();
                                    move |_| {
                                        trigger_decision(
                                            session,
                                            confirm_always,
                                            local_busy,
                                            on_decide,
                                            id.clone(),
                                            ApprovalDecision::Always,
                                        );
                                    }
                                },
                                "Confirm Always"
                            }
                            button {
                                class: "btn ghost sm",
                                r#type: "button",
                                disabled: is_busy,
                                onclick: move |_| confirm_always.set(false),
                                "Cancel"
                            }
                        }
                    } else {
                        button {
                            class: "btn ghost sm",
                            r#type: "button",
                            disabled: is_busy,
                            onclick: move |_| confirm_always.set(true),
                            "Always allow"
                        }
                    }
                }
                button {
                    class: "btn danger sm",
                    r#type: "button",
                    disabled: is_busy,
                    onclick: {
                        let id = id.clone();
                        move |_| {
                            trigger_decision(
                                session,
                                confirm_always,
                                local_busy,
                                on_decide,
                                id.clone(),
                                ApprovalDecision::Deny,
                            );
                        }
                    },
                    "Reject"
                }
            }
        }
    }
}

/// Rich inline approval cards rendered directly above the composer in ChatView.
#[component]
pub(crate) fn InlineApprovals(mut session: Signal<Session>) -> Element {
    let busy_id = use_signal(|| Option::<String>::None);
    let (local_approvals, global_approvals, active_bot) = {
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
        let active_bot = state
            .state
            .task
            .as_ref()
            .filter(|task| task.status == TaskStatus::AwaitingApproval)
            .map(|task| task.bot.clone())
            .or_else(|| state.conversation_bot(conv));
        (
            local.into_iter().cloned().collect::<Vec<_>>(),
            global.into_iter().cloned().collect::<Vec<_>>(),
            active_bot,
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
                    let bot_id = active_bot.clone();
                    rsx! {
                        ApprovalCard {
                            key: "{id}",
                            session,
                            id: approval.id,
                            tool: approval.tool,
                            summary: approval.summary,
                            expires_at: approval.expires_at,
                            compact: false,
                            bot_id,
                            allow_standing_grant: false,
                            busy: row_busy,
                            on_decide: {
                                let id = id.clone();
                                let mut busy_id = busy_id;
                                move |decision| {
                                    busy_id.set(Some(id.clone()));
                                    session.write().decide(id.clone(), decision);
                                }
                            },
                        }
                    }
                }
            }
            for approval in global_approvals {
                {
                    let id = approval.id.clone();
                    let row_busy = active_busy.as_deref() == Some(id.as_str());
                    rsx! {
                        ApprovalCard {
                            key: "global-{id}",
                            session,
                            id: approval.id,
                            tool: approval.tool,
                            summary: approval.summary,
                            expires_at: approval.expires_at,
                            compact: true,
                            allow_standing_grant: false,
                            busy: row_busy,
                            on_decide: {
                                let id = id.clone();
                                let mut busy_id = busy_id;
                                move |decision| {
                                    busy_id.set(Some(id.clone()));
                                    session.write().decide(id.clone(), decision);
                                }
                            },
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
    fn tool_category_badge_maps_known_tools() {
        assert_eq!(tool_category_badge("mail_send"), ToolCategory::Mail);
        assert_eq!(tool_category_badge("mail_delete"), ToolCategory::Mail);
        assert_eq!(tool_category_badge("agentmail_send"), ToolCategory::Mail);
        assert_eq!(
            tool_category_badge("onedrive_upload"),
            ToolCategory::OneDrive
        );
        assert_eq!(tool_category_badge("browser_upload"), ToolCategory::Browser);
        assert_eq!(tool_category_badge("browser_type"), ToolCategory::Browser);
        assert_eq!(
            tool_category_badge("project_check"),
            ToolCategory::ProjectCheck
        );
        assert_eq!(
            tool_category_badge("slack_post_message"),
            ToolCategory::Tool
        );
        assert_eq!(ToolCategory::ProjectCheck.label(), "Project Check");
    }

    #[test]
    fn approval_destination_extracts_target() {
        assert_eq!(
            approval_destination("Send email to ops@example.com").as_deref(),
            Some("ops@example.com")
        );
        assert_eq!(
            approval_destination("Post -> #general").as_deref(),
            Some("#general")
        );
        assert_eq!(
            approval_destination("Send report to https://api.example.com/v1/hook").as_deref(),
            Some("api.example.com")
        );
        assert_eq!(
            approval_destination("Upload report.pdf to /SeraBot Library/Q4").as_deref(),
            Some("/SeraBot Library/Q4")
        );
        assert_eq!(
            approval_destination("Notify alice@example.com about invoice").as_deref(),
            Some("alice@example.com")
        );
        assert_eq!(approval_destination("Run local check"), None);
    }

    #[test]
    fn format_expiry_countdown_handles_future_expired_and_fallback() {
        let target = "2026-11-24T20:00:00Z";
        let target_secs = serabot_desktop::workflows::epoch_secs(target).unwrap();

        assert_eq!(
            format_expiry_countdown(target, target_secs - 45),
            "Expires in 45s"
        );
        assert_eq!(
            format_expiry_countdown(target, target_secs - 15 * 60),
            "Expires in 15m"
        );
        assert_eq!(
            format_expiry_countdown(target, target_secs - (2 * 3_600 + 5 * 60)),
            "Expires in 2h 5m"
        );
        assert_eq!(format_expiry_countdown(target, target_secs + 1), "Expired");
        assert_eq!(format_expiry_countdown("", target_secs), "Pending approval");
        assert_eq!(
            format_expiry_countdown("not-a-timestamp", target_secs),
            "Expires not-a-timestamp"
        );
    }

    #[test]
    fn destructive_vault_upload_and_mail_send_never_allow_standing_grant() {
        assert!(!tool_allows_standing_grant(
            "mail_send",
            "Send email to a@b.com"
        ));
        assert!(!tool_allows_standing_grant("browser_upload", "Upload file"));
        assert!(!tool_allows_standing_grant(
            "onedrive_upload",
            "Upload to /Docs"
        ));
        assert!(!tool_allows_standing_grant(
            "project_check",
            "Run cargo test"
        ));
        assert!(!tool_allows_standing_grant("vault_get", "Read secret"));
        assert!(!tool_allows_standing_grant(
            "browser_type",
            "Type vault secret 'bank' into e1"
        ));
        assert!(tool_allows_standing_grant("browser_click", "Click submit"));
    }

    #[test]
    fn approval_artifact_hash_finds_sha256() {
        let hash = "a".repeat(64);
        assert_eq!(
            approval_artifact_hash(&format!("Upload artifact {hash} to OneDrive")),
            Some(hash)
        );
        assert_eq!(approval_artifact_hash("No hash here"), None);
    }
}
