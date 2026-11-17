//! The Claude Code page (0.54): Claude Code running on this computer with the
//! person's own Claude login, driven over ACP (`serabot_desktop::acp`).
//!
//! Setup card (Node.js, folder, sign-in), the transcript, permission cards and
//! a composer. The agent lives in an app-wide [`Controller`] so it keeps
//! running while other pages are open; the shell polls it.

use crate::ui::{Icon, MarkdownBody};
use dioxus::prelude::*;
use serabot_desktop::acp::controller::folder_label;
use serabot_desktop::acp::setup::NodeCheck;
use serabot_desktop::acp::{Controller, Item, Status};
use serabot_desktop::local_folders::FolderMode;
use serabot_desktop::session::{Section, Session, SettingsSection};

/// The app-wide controller, provided once by the shell.
#[derive(Clone, Copy)]
pub struct ClaudeCode(pub Signal<Controller>);

/// Provide the controller and drain it while the app runs.
pub fn use_claude_code_provider() {
    let controller = use_context_provider(|| ClaudeCode(Signal::new(Controller::default())));
    let mut signal = controller.0;
    use_future(move || async move {
        loop {
            // Drain without subscribing; write (and redraw) only when
            // something arrived.
            let got = signal.peek().take();
            if !got.is_empty() {
                signal.write().absorb(got);
            }
            #[cfg(windows)]
            tokio::time::sleep(std::time::Duration::from_millis(80)).await;
            #[cfg(not(windows))]
            std::future::pending::<()>().await;
        }
    });
}

/// Embedded inside a bot's chat pane when local Claude Agent (ACP) mode is active.
#[component]
pub fn ClaudeAgentBotView(session: Signal<Session>, bot_id: String) -> Element {
    let ClaudeCode(mut controller) = use_context::<ClaudeCode>();
    let mut draft = use_signal(String::new);
    use_hook(move || {
        let mut c = controller.write();
        if c.node.is_none() {
            c.check_node();
        }
    });
    let folders = session.read().local_folders.clone();
    let bot_name = session.read().bot_display_name(&bot_id);
    let c = controller.read();
    let status = c.status.clone();
    let running = status.running();
    let working = status == Status::Working;
    let node = c.node.clone();
    let checking = c.checking_node();
    let node_ready = node.as_ref().is_some_and(|n| n.npx().is_some());
    let selected = c
        .folder_id
        .clone()
        .or_else(|| folders.first().map(|f| f.id.clone()));
    let items = c.transcript.items.clone();
    let asks = c.asks.clone();
    let notice = c.notice.clone();
    let folder_label_now = c.folder_label.clone();
    let writable = c.writable;
    drop(c);
    let status_class = match &status {
        Status::Ready => "cc-status ok",
        Status::Working | Status::Starting => "cc-status busy",
        Status::NeedsSignIn | Status::Stopped(_) => "cc-status warn",
        Status::Idle => "cc-status",
    };
    let can_send = status == Status::Ready && !draft().trim().is_empty();
    let bot_for_toggle = bot_id.clone();

    rsx! {
        div { class: "pane cc-page cc-embedded",
            div { class: "cc-inline-bar",
                div { class: "cc-inline-title",
                    Icon { name: "code", size: 15 }
                    strong { "{bot_name} \u{00b7} Local Claude Agent (ACP)" }
                    span { class: "{status_class}", role: "status", "{status.label()}" }
                }
                div { class: "cc-head-actions",
                    if running {
                        button {
                            class: "btn ghost sm",
                            r#type: "button",
                            onclick: move |_| controller.write().stop(),
                            Icon { name: "stop", size: 14 }
                            "Stop agent"
                        }
                    }
                    button {
                        class: "btn ghost sm",
                        r#type: "button",
                        onclick: move |_| session.write().toggle_bot_claude_agent(&bot_for_toggle),
                        Icon { name: "chat", size: 14 }
                        "Server model chat"
                    }
                }
            }
            div { class: "pane-body cc-body",
                if !running {
                    section { class: "card cc-setup", "aria-labelledby": "cc-setup-title",
                        div { class: "card-head",
                            h2 { class: "card-title", id: "cc-setup-title", "Set up" }
                        }
                        ol { class: "cc-steps",
                            li { class: if node_ready { "cc-step done" } else { "cc-step" },
                                div { class: "cc-step-text",
                                    strong { "Node.js 22 or newer" }
                                    span { class: "muted",
                                        {match (&node, checking) {
                                            (_, true) | (None, _) => "Checking\u{2026}".to_owned(),
                                            (Some(check), false) => check.summary(),
                                        }}
                                    }
                                }
                                if !node_ready && !checking {
                                    div { class: "cc-step-actions",
                                        if matches!(node, Some(NodeCheck::Missing) | Some(NodeCheck::TooOld(_))) {
                                            button {
                                                class: "btn primary sm",
                                                r#type: "button",
                                                onclick: move |_| controller.write().install_node(),
                                                Icon { name: "download", size: 14 }
                                                "Install Node.js"
                                            }
                                        }
                                        button {
                                            class: "btn ghost sm",
                                            r#type: "button",
                                            onclick: move |_| controller.write().check_node(),
                                            Icon { name: "refresh", size: 14 }
                                            "Check again"
                                        }
                                    }
                                }
                            }
                            li { class: if folders.is_empty() { "cc-step" } else { "cc-step done" },
                                div { class: "cc-step-text",
                                    strong { "A folder to work in" }
                                    span { class: "muted",
                                        if folders.is_empty() {
                                            "Allow a folder in Settings, Local folders. Read-write folders let Claude Code edit files."
                                        } else {
                                            "Claude Code sees only this folder."
                                        }
                                    }
                                }
                                if folders.is_empty() {
                                    div { class: "cc-step-actions",
                                        button {
                                            class: "btn sm",
                                            r#type: "button",
                                            onclick: move |_| {
                                                let mut s = session.write();
                                                s.settings_section = SettingsSection::LocalFolders;
                                                s.open_section(Section::Settings);
                                            },
                                            Icon { name: "folder", size: 14 }
                                            "Allow a folder"
                                        }
                                    }
                                } else {
                                    div { class: "cc-folders", role: "radiogroup", "aria-label": "Folder",
                                        for folder in folders.iter().cloned() {
                                            {
                                                let on = selected.as_deref() == Some(folder.id.as_str());
                                                let id = folder.id.clone();
                                                let label = folder_label(&folder);
                                                let mode = if folder.mode == FolderMode::ReadWrite { "read and write" } else { "read only" };
                                                rsx! {
                                                    button {
                                                        key: "{id}",
                                                        class: if on { "cc-folder on" } else { "cc-folder" },
                                                        r#type: "button",
                                                        role: "radio",
                                                        "aria-checked": if on { "true" } else { "false" },
                                                        onclick: move |_| controller.write().folder_id = Some(id.clone()),
                                                        Icon { name: "folder", size: 14 }
                                                        span { class: "cc-folder-name", "{label}" }
                                                        span { class: "cc-folder-mode", "{mode}" }
                                                    }
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                            li { class: "cc-step",
                                div { class: "cc-step-text",
                                    strong { "Your Claude login" }
                                    span { class: "muted",
                                        "Claude Code uses its own sign-in on this computer. SeraBot never sees it. If it asks, sign in once."
                                    }
                                }
                                if node_ready {
                                    div { class: "cc-step-actions",
                                        button {
                                            class: "btn ghost sm",
                                            r#type: "button",
                                            onclick: move |_| controller.write().sign_in(),
                                            Icon { name: "key", size: 14 }
                                            "Sign in to Claude"
                                        }
                                    }
                                }
                            }
                        }
                        div { class: "cc-start",
                            button {
                                class: "btn primary",
                                r#type: "button",
                                disabled: !node_ready || selected.is_none(),
                                onclick: {
                                    let folders = folders.clone();
                                    let selected = selected.clone();
                                    move |_| {
                                        if let Some(folder) = folders
                                            .iter()
                                            .find(|f| Some(f.id.as_str()) == selected.as_deref())
                                        {
                                            controller.write().start(folder);
                                        }
                                    }
                                },
                                Icon { name: "bolt", size: 16 }
                                "Start Claude Code"
                            }
                            p { class: "muted cc-fine",
                                "The first start downloads the Claude Code adapter with npx. Nothing is sent to your SeraBot server."
                            }
                        }
                    }
                } else if let Some(label) = folder_label_now.clone() {
                    div { class: "cc-context",
                        Icon { name: "folder", size: 14 }
                        span { "{label}" }
                        span { class: "cc-folder-mode", if writable { "read and write" } else { "read only" } }
                    }
                }
                if status == Status::NeedsSignIn {
                    div { class: "card cc-signin", role: "alert",
                        p { "Claude Code is not signed in on this computer." }
                        div { class: "cc-step-actions",
                            button {
                                class: "btn primary sm",
                                r#type: "button",
                                onclick: move |_| controller.write().sign_in(),
                                Icon { name: "key", size: 14 }
                                "Sign in to Claude"
                            }
                            button {
                                class: "btn sm",
                                r#type: "button",
                                onclick: move |_| controller.write().retry_session(),
                                Icon { name: "refresh", size: 14 }
                                "Try again"
                            }
                        }
                    }
                }
                if let Some(text) = notice {
                    p { class: "cc-notice", role: "status", "{text}" }
                }
                div { class: "cc-feed", role: "log", "aria-live": "polite",
                    if items.is_empty() && running {
                        p { class: "muted cc-empty", "Ask Claude Code to explain, fix or build something in this folder." }
                    }
                    for (index, item) in items.into_iter().enumerate() {
                        TranscriptRow { key: "{index}", index, item }
                    }
                    if working {
                        div { class: "cc-working", span { class: "cc-dot" } "Working\u{2026}" }
                    }
                }
                for (index, ask) in asks.into_iter().enumerate() {
                    div { key: "{index}", class: "card cc-ask", role: "alertdialog", "aria-label": "Claude Code asks permission",
                        div { class: "cc-ask-head",
                            Icon { name: "shield", size: 16 }
                            strong { "Claude Code asks: {ask.request.title}" }
                        }
                        div { class: "cc-step-actions",
                            for option in ask.request.shown_options() {
                                {
                                    let id = option.id.clone();
                                    let class = if option.allows() { "btn primary sm" } else { "btn sm" };
                                    rsx! {
                                        button {
                                            key: "{id}",
                                            class: "{class}",
                                            r#type: "button",
                                            onclick: move |_| controller.write().answer(index, Some(&id)),
                                            "{option.name}"
                                        }
                                    }
                                }
                            }
                            button {
                                class: "btn ghost sm",
                                r#type: "button",
                                onclick: move |_| controller.write().answer(index, None),
                                "Decline"
                            }
                        }
                    }
                }
                if running {
                    form {
                        class: "cc-composer",
                        onsubmit: move |event| {
                            event.prevent_default();
                            let text = draft();
                            if controller.write().send(&text) {
                                draft.set(String::new());
                            }
                        },
                        textarea {
                            class: "cc-input",
                            rows: "3",
                            placeholder: if status == Status::Ready { "Message Claude Code (Ctrl+Enter to send)" } else { "Claude Code is busy\u{2026}" },
                            "aria-label": "Message Claude Code",
                            value: "{draft}",
                            oninput: move |event| draft.set(event.value()),
                            onkeydown: move |event| {
                                if event.key() == Key::Enter && event.modifiers().ctrl() {
                                    event.prevent_default();
                                    let text = draft();
                                    if controller.write().send(&text) {
                                        draft.set(String::new());
                                    }
                                }
                            },
                        }
                        div { class: "cc-composer-actions",
                            if working {
                                button {
                                    class: "btn sm",
                                    r#type: "button",
                                    onclick: move |_| controller.write().cancel(),
                                    Icon { name: "stop", size: 14 }
                                    "Stop"
                                }
                            }
                            button {
                                class: "btn ghost sm",
                                r#type: "button",
                                onclick: move |_| controller.write().clear(),
                                "Clear"
                            }
                            button {
                                class: "btn primary sm",
                                r#type: "submit",
                                disabled: !can_send,
                                Icon { name: "send", size: 14 }
                                "Send"
                            }
                        }
                    }
                }
            }
        }
    }
}

#[component]
fn TranscriptRow(index: usize, item: Item) -> Element {
    match item {
        Item::User(text) => rsx! {
            div { class: "cc-row cc-user", p { "{text}" } }
        },
        Item::Agent(text) => rsx! {
            div { class: "cc-row cc-agent",
                MarkdownBody { message_id: "cc-{index}", content: text }
            }
        },
        Item::Thought(text) => rsx! {
            details { class: "cc-row cc-thought",
                summary { "Thinking" }
                p { "{text}" }
            }
        },
        Item::Tool {
            title,
            kind,
            status,
            ..
        } => {
            let class = match status.as_str() {
                "completed" => "cc-row cc-tool done",
                "failed" => "cc-row cc-tool failed",
                _ => "cc-row cc-tool",
            };
            rsx! {
                div { class: "{class}",
                    span { class: "cc-tool-kind", "{kind}" }
                    span { class: "cc-tool-title", "{title}" }
                    span { class: "cc-tool-status", "{status}" }
                }
            }
        }
        Item::Plan(entries) => rsx! {
            div { class: "cc-row cc-plan",
                strong { "Plan" }
                ul {
                    for (i, entry) in entries.into_iter().enumerate() {
                        li { key: "{i}", class: "cc-plan-{entry.status}", "{entry.content}" }
                    }
                }
            }
        },
        Item::Notice { text, error } => rsx! {
            p { class: if error { "cc-row cc-note err" } else { "cc-row cc-note" }, "{text}" }
        },
    }
}
