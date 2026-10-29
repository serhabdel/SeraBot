//! Dioxus Desktop shell. Windows-only; the Rust session owns all network work.

#![cfg_attr(not(windows), allow(dead_code))]

use dioxus::prelude::*;
use serabot_desktop::{
    ApprovalDecision, ConversationKind, Role, Selection, TaskStatus,
    session::{Section, Session, Theme},
};

const CSS: &str = include_str!("../assets/app.css");

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
        let config = dioxus::desktop::Config::new().with_window(
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
    let theme = session.read().theme.as_str();
    let _revision = revision();
    rsx! {
        style { {CSS} }
        div { class: "root", "data-theme": theme,
            if connected {
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
                        Icon { name: "settings".to_string() }
                        span { if theme == Theme::Dark { "Light" } else { "Dark" } }
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
                        "Bots {bots}"
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
fn ConversationsPane(session: Signal<Session>) -> Element {
    let mut search = use_signal(String::new);
    let needle = search().to_lowercase();
    let current = session.read().conversation.clone();
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
                message.created_at.clone(),
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
    let mut confirmed = use_signal(|| false);
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
                input {
                    "type": "checkbox",
                    checked: confirmed(),
                    onchange: move |event| confirmed.set(event.checked()),
                }
                span { class: "muted small", "Provider receives this history" }
                button {
                    class: "btn primary small",
                    disabled: !allowed || !confirmed(),
                    onclick: move |_| {
                        let draft = session.read().state.draft.clone();
                        session.write().apply_model(draft);
                        confirmed.set(false);
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
            div { class: "cards",
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
                if bots_empty {
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
                if approvals_empty {
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
    rsx! {
        div { class: "page",
            div { class: "page-head",
                div {
                    h1 { "Providers" }
                    p { class: "muted", "Select an approved model for this conversation." }
                }
                button {
                    class: "btn ghost",
                    onclick: move |_| session.write().refresh_providers(),
                    "Refresh"
                }
            }
            div { class: "card",
                ModelBar { session }
                p { class: "muted small", "Applies to the next turn; a running turn keeps its pinned route." }
            }
            div { class: "cards",
                for provider in providers {
                    div { class: "card", key: "{provider.id}",
                        div { class: "row between",
                            h3 { "{provider.id}" }
                            span { class: "pill", "{provider.kind}" }
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
                    }
                }
            }
            div { class: "card",
                h3 { "Add or change a provider" }
                p { class: "muted small", "Provider blocks, keys and egress approval stay in /etc/serabot/serabot.toml and the age store." }
                code { "serabot secrets set <api_key_secret>" }
                code { "sudo systemctl restart serabot" }
            }
        }
    }
}

#[component]
fn ConnectorsView(session: Signal<Session>) -> Element {
    let connectors = session.read().state.connectors.clone();
    rsx! {
        div { class: "page",
            div { class: "page-head",
                div {
                    h1 { "Connectors" }
                    p { class: "muted", "Google, Microsoft, Notion and AgentMail status." }
                }
                button {
                    class: "btn ghost",
                    onclick: move |_| session.write().refresh_connectors(),
                    "Refresh"
                }
            }
            div { class: "cards",
                for connector in connectors {
                    div { class: "card", key: "{connector.id}",
                        div { class: "row between",
                            h3 { "{connector.id}" }
                            span { class: if connector.enabled { "pill ok" } else { "pill" },
                                if connector.enabled { "enabled" } else { "disabled" }
                            }
                        }
                        if let Some(uri) = connector.redirect_uri {
                            code { "{uri}" }
                        }
                        div { class: "chips",
                            for (role, secret) in connector.secrets {
                                span { class: if secret.configured { "chip accent" } else { "chip warn" },
                                    "{role}: {secret.name.clone().unwrap_or_default()}"
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
                conversation.updated_at.clone(),
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
    rsx! {
        div { class: "page",
            div { class: "page-head",
                div {
                    h1 { "Settings" }
                    p { class: "muted", "Connection, appearance and updates." }
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
