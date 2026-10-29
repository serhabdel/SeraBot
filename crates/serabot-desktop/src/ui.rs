//! Dioxus Desktop shell. Windows-only; the Rust session owns all network work.

#![cfg_attr(not(windows), allow(dead_code))]

use dioxus::prelude::*;
use serabot_desktop::{
    ApprovalDecision, ConversationKind, Role, SPECIALIST_TOOLS, Selection, TaskStatus,
    parse_skill_list,
    session::{Section, Session, Theme},
};

const CSS: &str = include_str!("../assets/app.css");

fn stamp(raw: &str) -> String {
    raw.chars().take(16).collect::<String>().replace('T', " ")
}

const NAV: [(Section, &str, &str); 7] = [
    (Section::Chat, "Chat", "chat"),
    (Section::Conversations, "Threads", "threads"),
    (Section::Memory, "Memory", "memory"),
    (Section::Providers, "Providers", "providers"),
    (Section::Connectors, "Connectors", "connectors"),
    (Section::Approvals, "Approvals", "approvals"),
    (Section::Fleet, "Fleet", "fleet"),
];

type BotRow = (
    String,
    String,
    bool,
    bool,
    Vec<String>,
    Vec<String>,
    Vec<String>,
);

pub fn run() {
    #[cfg(windows)]
    {
        let config = dioxus::desktop::Config::new()
            .with_menu(Option::<dioxus::desktop::muda::Menu>::None)
            .with_window(
                dioxus::desktop::WindowBuilder::new()
                    .with_title("SeraBot Owner")
                    .with_inner_size(dioxus::desktop::LogicalSize::new(1280.0, 820.0))
                    .with_min_inner_size(dioxus::desktop::LogicalSize::new(920.0, 620.0)),
            );
        dioxus::LaunchBuilder::desktop()
            .with_cfg(config)
            .launch(App);
    }
}

#[component]
fn App() -> Element {
    let mut session = use_signal(Session::new);
    let mut revision = use_signal(|| 0u64);
    use_coroutine(move |_: UnboundedReceiver<()>| async move {
        loop {
            #[cfg(windows)]
            tokio::time::sleep(std::time::Duration::from_millis(120)).await;
            #[cfg(not(windows))]
            std::future::pending::<()>().await;
            session.with_mut(|session| session.tick());
            revision.set(revision() + 1);
        }
    });
    let connected = session.read().state.connected;
    let restarting = session.read().restarting;
    let theme = session.read().theme.as_str();
    let _revision = revision();
    rsx! {
        style { {CSS} }
        div { class: "root", "data-theme": theme,
            if connected || restarting {
                Shell { session }
            } else {
                ConnectView { session }
            }
        }
    }
}

#[component]
fn Icon(name: String) -> Element {
    let path = match name.as_str() {
        "chat" => "M4 5h16v11H8l-4 4V5z",
        "threads" => "M4 5h11v9H4zM9 9h11v9H9z",
        "memory" => "M12 3c-4 0-7 2-7 5s3 5 7 5 7-2 7-5-3-5-7-5zM5 8v8c0 3 3 5 7 5s7-2 7-5V8",
        "providers" => "M4 6h16M4 12h16M4 18h10",
        "connectors" => "M8 12a4 4 0 1 1 4 4M16 12a4 4 0 1 1-4-4",
        "approvals" => "M12 3a9 9 0 1 0 0 18 9 9 0 0 0 0-18zM8 12l3 3 5-6",
        "fleet" => "M4 4h6v6H4zM14 4h6v6h-6zM4 14h6v6H4zM14 14h6v6h-6z",
        "settings" => {
            "M12 8a4 4 0 1 0 0 8 4 4 0 0 0 0-8zM12 2v3M12 19v3M2 12h3M19 12h3M5 5l2 2M17 17l2 2M19 5l-2 2M7 17l-2 2"
        }
        "theme" => {
            "M12 4v2M12 18v2M4 12h2M18 12h2M6.3 6.3l1.4 1.4M16.3 16.3l1.4 1.4M17.7 6.3l-1.4 1.4M7.7 16.3l-1.4 1.4M16 12a4 4 0 1 1-8 0 4 4 0 0 1 8 0z"
        }
        "send" => "M4 12l16-8-6 16-2-6-8-2z",
        "refresh" => {
            "M4 12a8 8 0 0 1 13.5-5.8L20 8M20 4v4h-4M20 12a8 8 0 0 1-13.5 5.8L4 16M4 20v-4h4"
        }
        _ => "M5 5h14v14H5z",
    };
    rsx! {
        svg {
            "viewBox": "0 0 24 24",
            fill: "none",
            stroke: "currentColor",
            "stroke-width": "1.7",
            "stroke-linecap": "round",
            "stroke-linejoin": "round",
            path { d: "{path}" }
        }
    }
}

#[component]
fn ConnectView(session: Signal<Session>) -> Element {
    let server = session.read().server.clone();
    let token = session.read().token.clone();
    let error = session.read().connect_error.clone();
    let connecting = session.read().connecting;
    let theme = session.read().theme;
    let version = env!("CARGO_PKG_VERSION");
    rsx! {
        div { class: "connect",
            div { class: "connect-card",
                div { class: "brand brand-lg", "S" }
                h1 { "SeraBot" }
                p { class: "muted", "Owner console · connect to your host" }
                label { "Server" }
                input {
                    class: "input",
                    value: "{server}",
                    placeholder: "https://bot.seradyx.tech",
                    oninput: move |event| session.write().server = event.value(),
                }
                label { "Token" }
                input {
                    class: "input",
                    "type": "password",
                    value: "{token}",
                    placeholder: "token minted on the server",
                    oninput: move |event| session.write().token = event.value(),
                }
                if let Some(message) = error {
                    div { class: "error", "{message}" }
                }
                div { class: "row",
                    button {
                        class: "btn primary",
                        disabled: connecting,
                        onclick: move |_| session.write().connect_now(),
                        if connecting { "Connecting…" } else { "Connect" }
                    }
                    button {
                        class: "btn ghost",
                        onclick: move |_| {
                            let next = session.read().theme.toggled();
                            session.write().set_theme(next);
                        },
                        if theme == Theme::Dark { "Light theme" } else { "Dark theme" }
                    }
                }
                p { class: "muted small",
                    "The token is kept in Windows Credential Manager. Only the server origin and theme are written to settings.json."
                }
            }
            p { class: "muted small", "v{version}" }
        }
    }
}

#[component]
fn Shell(session: Signal<Session>) -> Element {
    let section = session.read().section;
    let theme = session.read().theme;
    let chat = section == Section::Chat;
    let shell_class = if chat { "shell" } else { "shell dense" };
    rsx! {
        div { class: "{shell_class}",
            nav { class: "rail",
                div { class: "brand", "S" }
                div { class: "nav",
                    for (target, label, icon) in NAV {
                        button {
                            class: if section == target { "nav-btn active" } else { "nav-btn" },
                            onclick: move |_| session.write().open_section(target),
                            Icon { name: icon.to_string() }
                            span { "{label}" }
                        }
                    }
                }
                div { class: "rail-foot",
                    button {
                        class: "nav-btn",
                        onclick: move |_| {
                            let next = session.read().theme.toggled();
                            session.write().set_theme(next);
                        },
                        Icon { name: "theme".to_string() }
                        span { if theme == Theme::Dark { "Dark" } else { "Light" } }
                    }
                    button {
                        class: if section == Section::Settings { "nav-btn active" } else { "nav-btn" },
                        onclick: move |_| session.write().open_section(Section::Settings),
                        Icon { name: "settings".to_string() }
                        span { "Settings" }
                    }
                    span { class: "version", "v{env!(\"CARGO_PKG_VERSION\")}" }
                }
            }
            if chat {
                ConversationsPane { session }
            }
            main { class: "main",
                Header { session }
                StatusBanner { session }
                UpdateBanner { session }
                div { class: "content",
                    {match section {
                        Section::Chat => rsx! { ChatView { session } },
                        Section::Conversations => rsx! { ConversationsView { session } },
                        Section::Memory => rsx! { MemoryView { session } },
                        Section::Providers => rsx! { ProvidersView { session } },
                        Section::Connectors => rsx! { ConnectorsView { session } },
                        Section::Approvals => rsx! { ApprovalsView { session } },
                        Section::Fleet => rsx! { FleetView { session } },
                        Section::Settings => rsx! { SettingsView { session } },
                    }}
                }
            }
        }
    }
}

#[component]
fn Header(session: Signal<Session>) -> Element {
    let section = session.read().section;
    let title = if section == Section::Chat {
        session.read().conversation_title()
    } else {
        section.label().to_owned()
    };
    let kind = session.read().conversation_kind().map(str::to_owned);
    let connected = session.read().state.connected;
    let bots = session.read().state.listed_bots.len();
    let bots_loaded = session.read().state.bots_loaded;
    let bots_label = if bots_loaded {
        format!("Bots {bots}")
    } else {
        "Bots".to_owned()
    };
    let theme = session.read().theme;
    rsx! {
        header { class: "header",
            div { class: "header-title",
                strong { "{title}" }
                if let Some(kind) = kind {
                    span { class: "pill", "{kind}" }
                }
            }
            div { class: "header-actions",
                if section == Section::Chat {
                    button {
                        class: "btn ghost small",
                        onclick: move |_| session.write().open_section(Section::Fleet),
                        "Bots {bots_label}"
                    }
                }
                button {
                    class: "btn ghost small",
                    onclick: move |_| {
                        let next = session.read().theme.toggled();
                        session.write().set_theme(next);
                    },
                    if theme == Theme::Dark { "Light" } else { "Dark" }
                }
                button {
                    class: "btn ghost small",
                    onclick: move |_| session.write().refresh(),
                    Icon { name: "refresh".to_string() }
                    "Refresh"
                }
                span { class: if connected { "status on" } else { "status" },
                    if connected { "Online" } else { "Offline" }
                }
            }
        }
    }
}

#[component]
fn StatusBanner(session: Signal<Session>) -> Element {
    let error = session.read().state.error.clone();
    let unknown = session.read().state.unknown_outcome;
    let admin_error = session.read().admin_error.clone();
    let admin_notice = session.read().admin_notice.clone();
    let restart_pending = session.read().restart_pending;
    let restarting = session.read().restarting;
    let can_restart = session.read().can_admin();
    let task = session.read().state.task.as_ref().map(|task| {
        (
            task.status,
            task.status.label().to_owned(),
            task.bot.clone(),
            task.id.clone(),
        )
    });
    let error_view = error.map(|message| {
        rsx! {
            div { class: "banner error", span { "{message}" } }
        }
    });
    let unknown_view = unknown.then(|| {
        rsx! {
            div { class: "banner warn",
                span { "A previous action has an unknown outcome. Refresh reads before acting again." }
            }
        }
    });
    let task_view = task.map(|(status, label, bot, id)| {
        rsx! {
            div { class: "banner",
                span { class: "pill", "{label}" }
                span { class: "muted small", "{bot} · {id}" }
                if status == TaskStatus::AwaitingApproval {
                    button {
                        class: "btn ghost small",
                        onclick: move |_| session.write().open_section(Section::Approvals),
                        "Review approvals"
                    }
                }
            }
        }
    });
    let admin_error_view = admin_error.map(|message| {
        rsx! {
            div { class: "banner error",
                span { "{message}" }
            }
        }
    });
    let admin_notice_view = admin_notice.map(|message| {
        rsx! {
            div { class: if restart_pending { "banner accent" } else { "banner" },
                span { "{message}" }
                if restart_pending {
                    button {
                        class: "btn primary small",
                        disabled: !can_restart,
                        onclick: move |_| session.write().request_restart(),
                        "Restart server and apply"
                    }
                }
            }
        }
    });
    let restarting_view = restarting.then(|| {
        rsx! {
            div { class: "banner warn",
                span { "The server is restarting. Reconnecting automatically…" }
                button {
                    class: "btn ghost small",
                    onclick: move |_| session.write().connect_now(),
                    "Reconnect now"
                }
            }
        }
    });
    rsx! {
        {admin_error_view}
        {admin_notice_view}
        {restarting_view}
        {error_view}
        {unknown_view}
        {task_view}
    }
}

fn format_size(bytes: u64) -> String {
    const MB: f64 = 1024.0 * 1024.0;
    if bytes as f64 >= MB {
        format!("{:.1} MB", bytes as f64 / MB)
    } else {
        format!("{} KB", bytes.div_ceil(1024))
    }
}

#[component]
fn UpdateBanner(session: Signal<Session>) -> Element {
    let mut dismissed = use_signal(|| false);
    let info = session.read().update.info.clone();
    let Some(info) = info else {
        return rsx! {};
    };
    if dismissed() {
        return rsx! {};
    }
    let version = info.version.to_string();
    let size = format_size(info.size);
    let notes = info.notes.clone();
    let install = info.clone();
    rsx! {
        div { class: "banner update",
            span { strong { "Update v{version} available" } span { class: "muted small", " · {size}" } }
            div { class: "row",
                button {
                    class: "btn primary small",
                    onclick: move |_| {
                        if session.write().apply_update(&install).is_ok() {
                            std::process::exit(0);
                        }
                    },
                    "Install and restart"
                }
                button {
                    class: "btn ghost small",
                    onclick: move |_| dismissed.set(true),
                    "Later"
                }
            }
            details {
                summary { "Release notes" }
                p { class: "muted small", "{notes}" }
            }
        }
    }
}

#[component]
fn ConversationsPane(session: Signal<Session>) -> Element {
    let mut search = use_signal(String::new);
    let needle = search().to_lowercase();
    let current = session.read().conversation.clone();
    let loaded = session.read().state.conversations_loaded;
    let items: Vec<(String, String, String)> = session
        .read()
        .state
        .conversations
        .iter()
        .filter(|conversation| {
            needle.is_empty()
                || conversation.id.to_lowercase().contains(&needle)
                || conversation
                    .display_title()
                    .to_lowercase()
                    .contains(&needle)
        })
        .map(|conversation| {
            (
                conversation.id.clone(),
                conversation.display_title().to_owned(),
                conversation.normalized_kind().to_owned(),
            )
        })
        .collect();
    rsx! {
        aside { class: "sidebar",
            div { class: "sidebar-head",
                h2 { "Conversations" }
                button {
                    class: "btn primary small",
                    onclick: move |_| {
                        let id = format!("windows-{}", uuid::Uuid::new_v4());
                        session.write().open_conversation(id);
                    },
                    "New"
                }
            }
            input {
                class: "input",
                value: "{search}",
                placeholder: "Search threads",
                oninput: move |event| search.set(event.value()),
            }
            div { class: "thread-list",
                if !loaded {
                    div { class: "muted small", "Loading threads…" }
                }
                for (id, title, kind) in items {
                    button {
                        class: if id == current { "thread active" } else { "thread" },
                        onclick: {
                            let id = id.clone();
                            move |_| session.write().open_conversation(id.clone())
                        },
                        span { class: "thread-title", "{title}" }
                        span { class: "thread-meta", "{kind} · {id}" }
                    }
                }
            }
            button {
                class: "btn ghost small",
                onclick: move |_| session.write().open_section(Section::Conversations),
                "Manage conversations"
            }
        }
    }
}

#[component]
fn ChatView(session: Signal<Session>) -> Element {
    let messages: Vec<(String, bool, String, String, String)> = session
        .read()
        .state
        .messages
        .iter()
        .map(|message| {
            let user = message.role == Role::User;
            let speaker = if user {
                "You".to_owned()
            } else {
                message.name.clone().unwrap_or_else(|| "SeraBot".into())
            };
            (
                message.id.clone(),
                user,
                speaker,
                stamp(&message.created_at),
                message.content.clone(),
            )
        })
        .collect();
    let task = session
        .read()
        .state
        .task
        .as_ref()
        .map(|task| (task.status, task.status.label().to_owned()));
    let composer = session.read().composer.clone();
    let can_send = session.read().state.can_send();
    let accepts = session.read().conversation_accepts_turns();
    let bots: Vec<String> = session
        .read()
        .state
        .listed_bots
        .iter()
        .map(|bot| bot.id.clone())
        .collect();
    use_effect(move || {
        let _ = session.read().state.messages.len();
        let _ = dioxus::document::eval(
            "const el=document.getElementById('messages'); if(el){el.scrollTop=el.scrollHeight;}",
        );
    });
    let empty = messages.is_empty();
    rsx! {
        div { class: "chat",
            ModelBar { session }
            if let Some((status, label)) = task {
                if status == TaskStatus::AwaitingApproval {
                    div { class: "banner warn",
                        span { "{label}" }
                        button {
                            class: "btn ghost small",
                            onclick: move |_| session.write().open_section(Section::Approvals),
                            "Review approvals"
                        }
                    }
                }
            }
            div { class: "messages", id: "messages",
                if empty {
                    div { class: "hero",
                        div { class: "hero-mark", "S" }
                        h2 { if accepts { "Start a quiet, focused turn." } else { "This node is read-only." } }
                        p { class: "muted",
                            if accepts {
                                "Ask the orchestrator, or open a room with your specialists."
                            } else {
                                "Sections and groups organize the tree. Open a direct thread or a room to talk."
                            }
                        }
                        div { class: "roster",
                            for bot in bots {
                                span { class: "avatar", "{bot.chars().next().unwrap_or('?').to_ascii_uppercase()}" }
                                span { class: "muted small", "{bot}" }
                            }
                        }
                    }
                }
                for (id, user, speaker, stamp, content) in messages {
                    div { class: if user { "msg user" } else { "msg" }, key: "{id}",
                        span { class: "avatar", "{speaker.chars().next().unwrap_or('?').to_ascii_uppercase()}" }
                        div { class: "bubble",
                            div { class: "bubble-meta",
                                strong { "{speaker}" }
                                span { "{stamp}" }
                            }
                            div { class: "bubble-text", "{content}" }
                        }
                    }
                }
            }
            div { class: "composer",
                textarea {
                    class: "input composer-input",
                    value: "{composer}",
                    placeholder: if accepts { "Message SeraBot…" } else { "This node does not accept turns" },
                    disabled: !accepts,
                    oninput: move |event| session.write().composer = event.value(),
                }
                div { class: "composer-row",
                    button {
                        class: "btn primary",
                        disabled: !can_send || composer.trim().is_empty(),
                        onclick: move |_| session.write().submit(),
                        Icon { name: "send".to_string() }
                        "Send"
                    }
                    span { class: "muted small", "Ctrl+Enter in the old client · 8,000 character limit" }
                }
            }
        }
    }
}

#[component]
fn ModelBar(session: Signal<Session>) -> Element {
    let (options, current, allowed, saved) = {
        let session = session.read();
        let catalog = session.state.catalog.as_ref();
        let mut options = vec![("auto".to_owned(), String::new())];
        let mut current = "auto".to_owned();
        let mut allowed = false;
        let mut saved = "auto".to_owned();
        if let Some(catalog) = catalog {
            if !catalog.selection.is_auto() {
                current = format!("{}|{}", catalog.selection.provider, catalog.selection.model);
            }
            for entry in &catalog.entries {
                options.push((entry.provider.clone(), entry.model.clone()));
            }
            if catalog.entries.is_empty() {
                for provider in &catalog.providers {
                    for model in &provider.models {
                        options.push((provider.id.clone(), model.clone()));
                    }
                }
            }
            allowed = catalog.allows(&session.state.draft);
            saved = if catalog.selection.is_auto() {
                "auto".into()
            } else {
                format!(
                    "{} / {}",
                    catalog.selection.provider, catalog.selection.model
                )
            };
        }
        (options, current, allowed, saved)
    };
    let value = {
        let draft = session.read().state.draft.clone();
        if draft.is_auto() {
            "auto".to_owned()
        } else {
            format!("{}|{}", draft.provider, draft.model)
        }
    };
    let changed = value != current;
    rsx! {
        div { class: "model-bar",
            label { class: "muted small", "Model" }
            select {
                class: "input model-select",
                value: "{value}",
                onchange: move |event| {
                    let raw = event.value();
                    let next = if raw == "auto" {
                        Selection::auto()
                    } else {
                        let (provider, model) = raw.split_once('|').unwrap_or(("auto", ""));
                        if provider == "auto" {
                            Selection::auto()
                        } else {
                            Selection { provider: provider.to_owned(), model: model.to_owned() }
                        }
                    };
                    session.write().state.draft = next;
                },
                for (provider, model) in options {
                    if provider == "auto" {
                        option { value: "auto", "Auto (approved default)" }
                    } else {
                        option { value: "{provider}|{model}", "{provider} / {model}" }
                    }
                }
            }
            if changed {
                span { class: "muted small", "Egress-approved providers only" }
                button {
                    class: "btn primary small",
                    disabled: !allowed,
                    onclick: move |_| {
                        let draft = session.read().state.draft.clone();
                        session.write().apply_model(draft);
                    },
                    "Apply"
                }
            } else {
                span { class: "muted small", "Saved {saved}" }
            }
        }
    }
}

#[component]
fn FleetView(session: Signal<Session>) -> Element {
    let bots: Vec<BotRow> = session
        .read()
        .state
        .listed_bots
        .iter()
        .map(|bot| {
            (
                bot.id.clone(),
                bot.role.clone(),
                bot.enabled,
                bot.live,
                bot.tools.clone(),
                bot.skills.clone(),
                bot.require_approval.clone(),
            )
        })
        .collect();
    let bots_empty = bots.is_empty();
    let bots_loaded = session.read().state.bots_loaded;
    let mut bot_id = use_signal(String::new);
    let mut bot_desc = use_signal(String::new);
    let mut bot_skills = use_signal(String::new);
    let mut skill_id = use_signal(String::new);
    let mut skill_desc = use_signal(String::new);
    let mut skill_body = use_signal(String::new);
    rsx! {
        div { class: "page",
            div { class: "page-head",
                div {
                    h1 { "Fleet" }
                    p { class: "muted", "Specialists, tools, and approval rules." }
                }
                button {
                    class: "btn ghost",
                    onclick: move |_| session.write().refresh_bots(),
                    Icon { name: "refresh".to_string() }
                    "Refresh"
                }
            }
            details { class: "card form-card",
                summary { "New specialist" }
                div { class: "form-grid",
                    input {
                        class: "input",
                        value: "{bot_id}",
                        placeholder: "bot id (lowercase slug)",
                        oninput: move |event| bot_id.set(event.value()),
                    }
                    input {
                        class: "input",
                        value: "{bot_skills}",
                        placeholder: "skills: ask, notes",
                        oninput: move |event| bot_skills.set(event.value()),
                    }
                }
                textarea {
                    class: "input",
                    value: "{bot_desc}",
                    placeholder: "What this specialist is for",
                    oninput: move |event| bot_desc.set(event.value()),
                }
                details {
                    summary { class: "muted small", "Tools" }
                    div { class: "tool-grid",
                        for (index, name) in SPECIALIST_TOOLS.iter().enumerate() {
                            label { key: "{index}",
                                input {
                                    "type": "checkbox",
                                    checked: session.read().fleet.bot_tools.get(index).copied().unwrap_or(false),
                                    onchange: move |event| {
                                        if let Some(flag) = session.write().fleet.bot_tools.get_mut(index) {
                                            *flag = event.checked();
                                        }
                                    },
                                }
                                "{name}"
                            }
                        }
                    }
                }
                button {
                    class: "btn primary",
                    onclick: move |_| {
                        match parse_skill_list(&bot_skills()) {
                            Ok(skills) => {
                                let tools = session.read().selected_tools();
                                session.write().create_bot(bot_id(), bot_desc(), tools, skills);
                                bot_id.set(String::new());
                                bot_desc.set(String::new());
                                bot_skills.set(String::new());
                            }
                            Err(error) => session.write().state.error = Some(error.to_string()),
                        }
                    },
                    "Create disabled bot"
                }
                p { class: "muted small", "The bot stays disabled until serabot.service restarts." }
            }
            details { class: "card form-card",
                summary { "New skill pack" }
                div { class: "form-grid",
                    input {
                        class: "input",
                        value: "{skill_id}",
                        placeholder: "skill id",
                        oninput: move |event| skill_id.set(event.value()),
                    }
                    input {
                        class: "input",
                        value: "{skill_desc}",
                        placeholder: "One-line description",
                        oninput: move |event| skill_desc.set(event.value()),
                    }
                }
                textarea {
                    class: "input",
                    value: "{skill_body}",
                    placeholder: "Optional markdown body",
                    oninput: move |event| skill_body.set(event.value()),
                }
                button {
                    class: "btn primary",
                    onclick: move |_| {
                        session.write().create_skill(skill_id(), skill_desc(), skill_body());
                        skill_id.set(String::new());
                        skill_desc.set(String::new());
                        skill_body.set(String::new());
                    },
                    "Create skill"
                }
            }
            div { class: "cards",
                if !bots_loaded {
                    div { class: "card empty", h3 { "Loading bots…" } }
                }
                for (id, role, enabled, live, tools, skills, approvals) in bots {
                    div { class: "card bot-card", key: "{id}",
                        div { class: "bot-head",
                            span { class: "avatar", "{id.chars().next().unwrap_or('?').to_ascii_uppercase()}" }
                            div {
                                h3 { "{id}" }
                                span { class: "muted small", "{role}" }
                            }
                            span { class: if enabled { "pill ok" } else { "pill" }, if enabled { "enabled" } else { "disabled" } }
                            if !live {
                                span { class: "pill warn", "restart required" }
                            }
                        }
                        div { class: "chips",
                            for tool in tools {
                                span { class: if approvals.contains(&tool) { "chip warn" } else { "chip" }, "{tool}" }
                            }
                        }
                        div { class: "chips",
                            for skill in skills {
                                span { class: "chip accent", "{skill}" }
                            }
                        }
                    }
                }
                if bots_loaded && bots_empty {
                    div { class: "card empty",
                        h3 { "No bots loaded yet" }
                        p { class: "muted", "Refresh, or create a specialist on the server." }
                    }
                }
            }
        }
    }
}

#[component]
fn ApprovalsView(session: Signal<Session>) -> Element {
    let approvals_loaded = session.read().state.approvals_loaded;
    let approvals: Vec<(String, String, String, String)> = session
        .read()
        .state
        .approvals
        .iter()
        .map(|approval| {
            (
                approval.id.clone(),
                approval.tool.clone(),
                approval.summary.clone(),
                approval.expires_at.clone(),
            )
        })
        .collect();
    let approvals_empty = approvals.is_empty();
    rsx! {
        div { class: "page",
            div { class: "page-head",
                div {
                    h1 { "Approvals" }
                    p { class: "muted", "Each decision binds the exact payload, destination, hash, policy and expiry." }
                }
                button {
                    class: "btn ghost",
                    onclick: move |_| session.write().refresh_approvals(),
                    "Refresh list"
                }
            }
            div { class: "cards",
                if !approvals_loaded {
                    div { class: "card empty", h3 { "Loading approvals…" } }
                }
                for (id, tool, summary, expires) in approvals {
                    div { class: "card", key: "{id}",
                        div { class: "row between",
                            span { class: "pill accent", "{tool}" }
                            span { class: "muted small", "expires {expires}" }
                        }
                        p { class: "approval-text", "{summary}" }
                        div { class: "row",
                            button {
                                class: "btn primary",
                                onclick: {
                                    let id = id.clone();
                                    move |_| session.write().decide(id.clone(), ApprovalDecision::Once)
                                },
                                "Approve once"
                            }
                            button {
                                class: "btn ghost",
                                onclick: {
                                    let id = id.clone();
                                    move |_| session.write().decide(id.clone(), ApprovalDecision::Always)
                                },
                                "Always"
                            }
                            button {
                                class: "btn danger",
                                onclick: {
                                    let id = id.clone();
                                    move |_| session.write().decide(id.clone(), ApprovalDecision::Deny)
                                },
                                "Deny"
                            }
                        }
                    }
                }
                if approvals_loaded && approvals_empty {
                    div { class: "card empty",
                        h3 { "Nothing pending" }
                        p { class: "muted", "Gateway requests appear here." }
                    }
                }
            }
        }
    }
}

#[component]
fn ProvidersView(session: Signal<Session>) -> Element {
    let providers = session.read().state.providers.clone();
    let loaded = session.read().state.providers_loaded;
    let id = session.read().provider.id.clone();
    let kind = session.read().provider.kind.clone();
    let base_url = session.read().provider.base_url.clone();
    let models = session.read().provider.models.clone();
    let secret_name = session.read().provider.secret_name.clone();
    let api_key = session.read().provider.api_key.clone();
    let catalog_fetch = session.read().provider.catalog_fetch;
    let can_save = session.read().can_admin();
    let mut confirm = use_signal(|| Option::<String>::None);
    let kinds = [
        "openai-compatible",
        "mistral",
        "deepseek",
        "minimax",
        "minimax-cn",
        "openai",
        "groq",
        "together",
        "fireworks",
        "openrouter",
        "xai",
        "cerebras",
        "moonshot",
    ];
    rsx! {
        div { class: "page",
            div { class: "page-head",
                div {
                    h1 { "Providers" }
                    p { class: "muted", "Add a provider, pull its live models, and pick the route per conversation." }
                }
                div { class: "row",
                    button {
                        class: "btn ghost",
                        onclick: move |_| session.write().fetch_models(),
                        "Fetch models"
                    }
                    button {
                        class: "btn ghost",
                        onclick: move |_| session.write().refresh_providers(),
                        "Refresh"
                    }
                }
            }
            div { class: "card",
                ModelBar { session }
                p { class: "muted small", "Applies to the next turn; a running turn keeps its pinned route." }
            }
            if !loaded {
                div { class: "card empty", h3 { "Loading providers…" } }
            }
            div { class: "cards",
                for provider in providers {
                    div { class: "card", key: "{provider.id}",
                        div { class: "row between",
                            div {
                                h3 { "{provider.id}" }
                                if provider.managed {
                                    span { class: "pill accent", "managed here" }
                                }
                            }
                            span { class: "pill", "{provider.kind}" }
                        }
                        div { class: "chips",
                            if provider.catalog_verified {
                                span { class: "pill ok", "catalog verified" }
                            } else if provider.catalog_fetch {
                                span { class: "pill warn", "catalog fetch on" }
                            } else {
                                span { class: "pill", "static models" }
                            }
                            if provider.data_egress_approved {
                                span { class: "pill ok", "ready" }
                            } else {
                                span { class: "pill warn", "egress blocked" }
                            }
                        }
                        div { class: "chips",
                            for model in provider.models {
                                span { class: "chip", "{model}" }
                            }
                        }
                        p { class: "muted small", "Routing: {provider.routing_classes.join(\", \")}" }
                        div { class: "chips",
                            for (role, secret) in provider.secrets {
                                span { class: if secret.configured { "chip accent" } else { "chip warn" },
                                    "{role}: {secret.name.clone().unwrap_or_default()}"
                                }
                            }
                        }
                        if provider.managed {
                            if confirm() == Some(provider.id.clone()) {
                                div { class: "row",
                                    span { class: "muted small", "Remove {provider.id}? Conversations pinned to it must pick another route." }
                                    button {
                                        class: "btn danger small",
                                        disabled: !can_save,
                                        onclick: {
                                            let id = provider.id.clone();
                                            move |_| {
                                                session.write().remove_provider(id.clone());
                                                confirm.set(None);
                                            }
                                        },
                                        "Remove"
                                    }
                                    button {
                                        class: "btn ghost small",
                                        onclick: move |_| confirm.set(None),
                                        "Cancel"
                                    }
                                }
                            } else {
                                button {
                                    class: "btn ghost small",
                                    onclick: {
                                        let id = provider.id.clone();
                                        move |_| confirm.set(Some(id.clone()))
                                    },
                                    "Remove provider"
                                }
                            }
                        }
                    }
                }
            }
            div { class: "card form-card",
                h3 { "Add or update a provider" }
                p { class: "muted small",
                    "Saved next to the database, never in serabot.toml. Keys go straight to the encrypted store and are never shown again. Any OpenAI-compatible /chat/completions endpoint works, including gateways that front Anthropic models."
                }
                div { class: "form-grid",
                    input {
                        class: "input",
                        value: "{id}",
                        placeholder: "provider id (for example work)",
                        oninput: move |event| session.write().provider.id = event.value(),
                    }
                    select {
                        class: "input",
                        value: "{kind}",
                        onchange: move |event| session.write().provider.kind = event.value(),
                        for item in kinds {
                            option { value: "{item}", "{item}" }
                        }
                    }
                    input {
                        class: "input",
                        value: "{base_url}",
                        placeholder: "https://api.example.com/v1",
                        oninput: move |event| session.write().provider.base_url = event.value(),
                    }
                    input {
                        class: "input",
                        value: "{models}",
                        placeholder: "model ids, comma separated",
                        oninput: move |event| session.write().provider.models = event.value(),
                    }
                    input {
                        class: "input",
                        value: "{secret_name}",
                        placeholder: "secret name (for example WORK_API_KEY)",
                        oninput: move |event| session.write().provider.secret_name = event.value(),
                    }
                    input {
                        class: "input",
                        type: "password",
                        value: "{api_key}",
                        placeholder: "API key (leave empty if already stored)",
                        oninput: move |event| session.write().provider.api_key = event.value(),
                    }
                }
                label { class: "check",
                    input {
                        "type": "checkbox",
                        checked: catalog_fetch,
                        onchange: move |event| session.write().provider.catalog_fetch = event.checked(),
                    }
                    span { "Fetch the live model list (/models)" }
                }
                button {
                    class: "btn primary",
                    disabled: !can_save,
                    onclick: move |_| session.write().save_provider(),
                    "Save provider"
                }
            }
        }
    }
}

#[component]
fn ConnectorsView(session: Signal<Session>) -> Element {
    let connectors = session.read().state.connectors.clone();
    let loaded = session.read().state.connectors_loaded;
    let can_save = session.read().can_admin();
    rsx! {
        div { class: "page",
            div { class: "page-head",
                div {
                    h1 { "Connectors" }
                    p { class: "muted", "Enable Google, Microsoft, Notion and AgentMail, and store their credentials." }
                }
                button {
                    class: "btn ghost",
                    onclick: move |_| session.write().refresh_connectors(),
                    "Refresh"
                }
            }
            if !loaded {
                div { class: "card empty", h3 { "Loading connectors…" } }
            }
            div { class: "cards",
                for connector in connectors {
                    {
                        let id = connector.id.clone();
                        let enabled = session
                            .read()
                            .connector_drafts
                            .get(&id)
                            .map(|draft| draft.enabled)
                            .unwrap_or(connector.enabled);
                        let values = session
                            .read()
                            .connector_drafts
                            .get(&id)
                            .map(|draft| draft.values.clone())
                            .unwrap_or_default();
                        rsx! {
                            div { class: "card", key: "{id}",
                                div { class: "row between",
                                    div {
                                        h3 { "{id}" }
                                        span { class: "pill", "{connector.kind}" }
                                    }
                                    span { class: if enabled { "pill ok" } else { "pill" },
                                        if enabled { "enabled" } else { "disabled" }
                                    }
                                }
                                if let Some(uri) = connector.redirect_uri.clone() {
                                    code { "{uri}" }
                                }
                                div { class: "chips",
                                    for (role, secret) in &connector.secrets {
                                        span { class: if secret.configured { "chip accent" } else { "chip warn" },
                                            "{role}: {secret.name.clone().unwrap_or_default()}"
                                        }
                                    }
                                }
                                div { class: "form-grid",
                                    for (role, secret) in &connector.secrets {
                                        {
                                            let role = role.clone();
                                            let name = secret.name.clone().unwrap_or_default();
                                            let value = values.get(&role).cloned().unwrap_or_default();
                                            rsx! {
                                                input {
                                                    class: "input",
                                                    "type": "password",
                                                    value: "{value}",
                                                    placeholder: if secret.configured { format!("replace {name}") } else { format!("set {name}") },
                                                    oninput: {
                                                        let id = id.clone();
                                                        let role = role.clone();
                                                        move |event| session.write().set_connector_value(&id, &role, event.value())
                                                    },
                                                }
                                            }
                                        }
                                    }
                                }
                                p { class: "muted small",
                                    "Enabling makes the connector available; grant its tools to a bot in Fleet. Reads and sends stay approval-gated."
                                }
                                div { class: "row",
                                    button {
                                        class: if enabled { "btn ghost" } else { "btn primary" },
                                        onclick: {
                                            let id = id.clone();
                                            move |_| session.write().toggle_connector(&id)
                                        },
                                        if enabled { "Disable" } else { "Enable" }
                                    }
                                    button {
                                        class: "btn ghost",
                                        disabled: !can_save,
                                        onclick: {
                                            let id = id.clone();
                                            move |_| session.write().save_connector(id.clone())
                                        },
                                        "Save connector"
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

#[component]
fn MemoryView(session: Signal<Session>) -> Element {
    let memories = session.read().state.memories.clone();
    let memories_loaded = session.read().state.memories_loaded;
    let kind = session.read().memory.kind.clone();
    let content = session.read().memory.content.clone();
    let source = session.read().memory.source.clone();
    let query = session.read().memory.query.clone();
    let confirm = session.read().memory.delete_confirm.clone();
    rsx! {
        div { class: "page",
            div { class: "page-head",
                div {
                    h1 { "Memory" }
                    p { class: "muted", "Owner-scoped facts. The orchestrator reads the 20 most recent." }
                }
                button {
                    class: "btn ghost",
                    onclick: move |_| session.write().load_memories(),
                    "Refresh"
                }
            }
            div { class: "card",
                div { class: "row",
                    select {
                        class: "input",
                        value: "{kind}",
                        onchange: move |event| session.write().memory.kind = event.value(),
                        for kind in Session::memory_kinds() {
                            option { value: "{kind}", "{kind}" }
                        }
                    }
                    input {
                        class: "input",
                        value: "{source}",
                        placeholder: "source",
                        oninput: move |event| session.write().memory.source = event.value(),
                    }
                }
                textarea {
                    class: "input",
                    value: "{content}",
                    placeholder: "A durable preference, fact, decision, context or note",
                    oninput: move |event| session.write().memory.content = event.value(),
                }
                button {
                    class: "btn primary",
                    onclick: move |_| session.write().add_memory(),
                    "Add memory"
                }
            }
            div { class: "row",
                input {
                    class: "input",
                    value: "{query}",
                    placeholder: "Filter content, kind or source",
                    oninput: move |event| session.write().memory.query = event.value(),
                }
                button {
                    class: "btn ghost",
                    onclick: move |_| session.write().load_memories(),
                    "Search"
                }
            }
            div { class: "cards",
                if !memories_loaded {
                    div { class: "card empty", h3 { "Loading memories…" } }
                }
                for memory in memories {
                    div { class: "card", key: "{memory.id}",
                        div { class: "row between",
                            span { class: "pill accent", "{memory.kind}" }
                            span { class: "muted small", "{memory.created_at} · {memory.source}" }
                        }
                        p { "{memory.content}" }
                        if confirm.as_deref() == Some(memory.id.as_str()) {
                            button {
                                class: "btn danger small",
                                onclick: {
                                    let id = memory.id.clone();
                                    move |_| session.write().delete_memory(id.clone())
                                },
                                "Confirm delete"
                            }
                        } else {
                            button {
                                class: "btn ghost small",
                                onclick: {
                                    let id = memory.id.clone();
                                    move |_| session.write().memory.delete_confirm = Some(id.clone())
                                },
                                "Delete"
                            }
                        }
                    }
                }
            }
        }
    }
}

#[component]
fn ConversationsView(session: Signal<Session>) -> Element {
    let mut rename = use_signal(|| None::<(String, String)>);
    let items: Vec<(String, String, String, String)> = session
        .read()
        .state
        .conversations
        .iter()
        .map(|conversation| {
            (
                conversation.id.clone(),
                conversation.display_title().to_owned(),
                conversation.normalized_kind().to_owned(),
                stamp(&conversation.updated_at),
            )
        })
        .collect();
    let kind = session.read().fleet.conv_kind;
    let slug = session.read().fleet.conv_slug.clone();
    let parent = session.read().fleet.conv_parent.clone();
    let editing = rename();
    rsx! {
        div { class: "page",
            div { class: "page-head",
                div {
                    h1 { "Conversations" }
                    p { class: "muted", "Sections, groups, direct threads and rooms." }
                }
                button {
                    class: "btn ghost",
                    onclick: move |_| session.write().load_conversations(),
                    "Refresh"
                }
            }
            div { class: "card",
                h3 { "New conversation" }
                div { class: "row",
                    select {
                        class: "input",
                        value: "{kind.as_str()}",
                        onchange: move |event| {
                            let next = ConversationKind::ALL
                                .into_iter()
                                .find(|kind| kind.as_str() == event.value())
                                .unwrap_or(ConversationKind::Section);
                            session.write().fleet.conv_kind = next;
                        },
                        for kind in ConversationKind::ALL {
                            option { value: "{kind.as_str()}", "{kind.label()}" }
                        }
                    }
                    input {
                        class: "input",
                        value: "{slug}",
                        placeholder: "slug",
                        oninput: move |event| session.write().fleet.conv_slug = event.value(),
                    }
                    if kind != ConversationKind::Section {
                        input {
                            class: "input",
                            value: "{parent}",
                            placeholder: "parent sec:finance",
                            oninput: move |event| session.write().fleet.conv_parent = event.value(),
                        }
                    }
                    button {
                        class: "btn primary",
                        onclick: move |_| {
                            let kind = session.read().fleet.conv_kind;
                            let slug = session.read().fleet.conv_slug.clone();
                            let parent = session.read().fleet.conv_parent.trim().to_owned();
                            session.write().create_conversation(kind, slug, (!parent.is_empty()).then_some(parent));
                        },
                        "Create"
                    }
                }
            }
            div { class: "cards",
                for (id, title, kind, updated) in items {
                    div { class: "card", key: "{id}",
                        div { class: "row between",
                            div {
                                h3 { "{title}" }
                                span { class: "muted small", "{id} · {kind} · {updated}" }
                            }
                            div { class: "row",
                                button {
                                    class: "btn ghost small",
                                    onclick: {
                                        let id = id.clone();
                                        move |_| session.write().open_conversation(id.clone())
                                    },
                                    "Open"
                                }
                                button {
                                    class: "btn ghost small",
                                    onclick: {
                                        let id = id.clone();
                                        let title = title.clone();
                                        move |_| rename.set(Some((id.clone(), title.clone())))
                                    },
                                    "Rename"
                                }
                                button {
                                    class: "btn danger small",
                                    onclick: {
                                        let id = id.clone();
                                        move |_| session.write().archive_conversation(id.clone())
                                    },
                                    "Archive"
                                }
                            }
                        }
                        if let Some((edit_id, text)) = editing.clone() {
                            if edit_id == id {
                                div { class: "row",
                                    input {
                                        class: "input",
                                        value: "{text}",
                                        oninput: move |event| {
                                            if let Some((_, text)) = rename.write().as_mut() {
                                                *text = event.value();
                                            }
                                        },
                                    }
                                    button {
                                        class: "btn primary small",
                                        onclick: {
                                            let id = id.clone();
                                            move |_| {
                                                if let Some((_, title)) = rename() {
                                                    session.write().rename_conversation(id.clone(), title);
                                                    rename.set(None);
                                                }
                                            }
                                        },
                                        "Save"
                                    }
                                    button {
                                        class: "btn ghost small",
                                        onclick: move |_| rename.set(None),
                                        "Cancel"
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

#[component]
fn SettingsView(session: Signal<Session>) -> Element {
    let theme = session.read().theme;
    let server = session.read().server.clone();
    let version = session.read().state.version.clone();
    let update = session.read().update.info.clone();
    let update_error = session.read().update.error.clone();
    let checking = session.read().update.checking;
    let audio_enabled = session.read().state.audio_enabled;
    let transcription = session
        .read()
        .state
        .audio_transcription_model
        .clone()
        .unwrap_or_default();
    let understanding = session
        .read()
        .state
        .audio_understanding_model
        .clone()
        .unwrap_or_default();
    let firecrawl_enabled = session.read().state.firecrawl_enabled;
    let telegram_enabled = session.read().state.telegram_enabled;
    let catalog_fetch = session.read().state.catalog_fetch;
    let audio_on = session.read().audio.enabled;
    let audio_transcription = session.read().audio.transcription_model.clone();
    let audio_understanding = session.read().audio.understanding_model.clone();
    let can_admin = session.read().can_admin();
    rsx! {
        div { class: "page",
            div { class: "page-head",
                div {
                    h1 { "Settings" }
                    p { class: "muted", "Connection, capabilities, appearance and updates." }
                }
            }
            div { class: "card",
                h3 { "Connection" }
                p { class: "muted small", "{server}" }
                if !version.is_empty() {
                    p { class: "muted small", "Server {version}" }
                }
                div { class: "row",
                    button {
                        class: "btn ghost",
                        onclick: move |_| session.write().forget_token(),
                        "Forget token"
                    }
                    button {
                        class: "btn danger",
                        onclick: move |_| session.write().disconnect(),
                        "Disconnect"
                    }
                }
            }
            div { class: "card",
                h3 { "Capabilities" }
                div { class: "chips",
                    span {
                        class: match audio_enabled { Some(true) => "chip accent", Some(false) => "chip warn", None => "chip" },
                        match audio_enabled { Some(true) => "Audio on", Some(false) => "Audio off", None => "Audio unknown" }
                    }
                    span {
                        class: match firecrawl_enabled { Some(true) => "chip accent", Some(false) => "chip", None => "chip" },
                        match firecrawl_enabled { Some(true) => "Firecrawl on", Some(false) => "Firecrawl off", None => "Firecrawl unknown" }
                    }
                    span {
                        class: match telegram_enabled { Some(true) => "chip accent", Some(false) => "chip", None => "chip" },
                        match telegram_enabled { Some(true) => "Telegram on", Some(false) => "Telegram off", None => "Telegram unknown" }
                    }
                    span {
                        class: match catalog_fetch { Some(true) => "chip accent", Some(false) => "chip", None => "chip" },
                        match catalog_fetch { Some(true) => "Model catalogs live", Some(false) => "Model catalogs static", None => "Model catalogs unknown" }
                    }
                }
                if audio_enabled == Some(true) {
                    p { class: "muted small",
                        "Transcription: {transcription}. Understanding: {understanding}."
                    }
                    p { class: "muted small",
                        "Voice notes sent to the bot's Telegram intake are transcribed with the configured Mistral audio model before the turn runs."
                    }
                } else if audio_enabled == Some(false) {
                    p { class: "muted small", "Audio is disabled on the server (audio.enabled = false)." }
                } else {
                    p { class: "muted small",
                        "This server build does not report capabilities. Update the server to see live status."
                    }
                }
                if catalog_fetch == Some(false) {
                    p { class: "muted small",
                        "Enable catalog_fetch in [llm] and use Fetch models on the Providers page to pull live model lists."
                    }
                }
                div { class: "form-grid",
                    input {
                        class: "input",
                        value: "{audio_transcription}",
                        placeholder: "transcription model",
                        oninput: move |event| session.write().audio.transcription_model = event.value(),
                    }
                    input {
                        class: "input",
                        value: "{audio_understanding}",
                        placeholder: "understanding model",
                        oninput: move |event| session.write().audio.understanding_model = event.value(),
                    }
                }
                p { class: "muted small",
                    "Audio uses the Mistral credentials already stored for the provider. Add a separate audio key in serabot.toml only if you need one."
                }
                label { class: "check",
                    input {
                        "type": "checkbox",
                        checked: audio_on,
                        onchange: move |event| session.write().audio.enabled = event.checked(),
                    }
                    span { "Enable audio transcription and understanding" }
                }
                button {
                    class: "btn primary",
                    disabled: !can_admin,
                    onclick: move |_| session.write().save_audio(),
                    "Save audio settings"
                }
            }
            div { class: "card",
                h3 { "Appearance" }
                div { class: "row",
                    for mode in [Theme::Dark, Theme::Light] {
                        button {
                            class: if theme == mode { "btn primary" } else { "btn ghost" },
                            onclick: move |_| session.write().set_theme(mode),
                            "{mode:?}"
                        }
                    }
                }
            }
            div { class: "card",
                h3 { "Updates" }
                button {
                    class: "btn ghost",
                    disabled: checking,
                    onclick: move |_| session.write().check_updates(),
                    if checking { "Checking…" } else { "Check for updates" }
                }
                if let Some(info) = update {
                    p { class: "accent", "v{info.version} available" }
                    button {
                        class: "btn primary",
                        onclick: {
                            let info = info.clone();
                            move |_| {
                                let _ = session.write().apply_update(&info);
                            }
                        },
                        "Install and restart"
                    }
                }
                if let Some(error) = update_error {
                    p { class: "error", "{error}" }
                }
            }
            div { class: "card",
                h3 { "Keyboard" }
                p { class: "muted small", "Ctrl+Enter sends in the previous shell; the webview uses the Send button." }
            }
        }
    }
}
