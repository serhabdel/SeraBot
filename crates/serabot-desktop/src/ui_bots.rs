//! Create-a-bot and create-a-group sheets, the onboarding chat, the avatar
//! picker and the plain-language capability toggles (0.15).
//!
//! Nothing here asks for an id or a tag: the owner types a name, and the
//! server derives the id. Ids appear only read-only under "Advanced".

use crate::ui::{Icon, Overlay, avatar_style, group_avatar_style, human_model, human_provider};
use dioxus::prelude::*;
use serabot_desktop::capabilities;
use serabot_desktop::session::{ImageSlot, Section, Session};
use serabot_desktop::{AVATAR_FILE_ACCEPT, MAX_AVATAR_UPLOAD_BYTES, MAX_TEAM_MEMBERS, preview_id};

/// Choose an image from disk. The bytes are read off the UI event and handed
/// to `on_pick`; the session checks the type and size.
#[component]
pub fn AvatarPicker(
    preview: Option<String>,
    letter: String,
    style: String,
    has_image: bool,
    on_pick: EventHandler<Vec<u8>>,
    on_clear: EventHandler<()>,
    #[props(default = "Choose image".to_owned())] label: String,
) -> Element {
    rsx! {
        div { class: "avatar-pick",
            span { class: "avatar xl bot", style: "{style}",
                if let Some(src) = preview {
                    img { class: "avatar-img", src: "{src}", alt: "" }
                } else {
                    "{letter}"
                }
            }
            div { class: "avatar-pick-copy",
                div { class: "row",
                    label { class: "btn ghost sm file-btn",
                        input {
                            class: "file-input",
                            "type": "file",
                            accept: AVATAR_FILE_ACCEPT,
                            "aria-label": "{label}",
                            onchange: move |event| {
                                let files = event.files();
                                spawn(async move {
                                    let Some(file) = files.into_iter().next() else {
                                        return;
                                    };
                                    // Refuse before reading a huge file into memory.
                                    if file.size() > MAX_AVATAR_UPLOAD_BYTES as u64 {
                                        on_pick.call(vec![0; MAX_AVATAR_UPLOAD_BYTES + 1]);
                                        return;
                                    }
                                    if let Ok(bytes) = file.read_bytes().await {
                                        on_pick.call(bytes.to_vec());
                                    }
                                });
                            },
                        }
                        Icon { name: "image", size: 14 }
                        "{label}"
                    }
                    if has_image {
                        button {
                            class: "btn ghost sm",
                            onclick: move |_| on_clear.call(()),
                            "Remove"
                        }
                    }
                }
                span { class: "hint", "PNG, JPEG or WebP, up to 1 MB." }
            }
        }
    }
}

/// Model options grouped by provider, from the live catalog.
pub(crate) fn model_groups(session: &Session) -> Vec<(String, Vec<String>)> {
    session
        .state
        .catalog
        .as_ref()
        .map(|catalog| {
            catalog
                .providers
                .iter()
                .filter(|provider| provider.data_egress_approved && !provider.models.is_empty())
                .map(|provider| (provider.id.clone(), provider.models.clone()))
                .collect()
        })
        .unwrap_or_default()
}

#[component]
pub fn NewBotSheet(session: Signal<Session>, overlay: Signal<Overlay>) -> Element {
    let mut overlay = overlay;
    let name = session.read().new_bot.name.clone();
    let model = session.read().new_bot.model.clone();
    let group = session.read().new_bot.group.clone();
    let error = session.read().new_bot.error.clone();
    let preview = session
        .read()
        .new_bot
        .avatar
        .as_ref()
        .map(|image| image.preview.clone());
    let groups = model_groups(&session.read());
    let rooms: Vec<(String, String)> = {
        let state = session.read();
        state
            .state
            .conversations
            .iter()
            .filter(|conversation| conversation.normalized_kind() == "room")
            .map(|conversation| {
                (
                    conversation.id.clone(),
                    state.conversation_label(conversation),
                )
            })
            .collect()
    };
    let letter = name
        .trim()
        .chars()
        .next()
        .map(|c| c.to_uppercase().to_string())
        .unwrap_or_else(|| "B".into());
    let id_preview = preview_id(&name, "bot");
    let can_create = serabot_desktop::valid_display_name(&name);
    rsx! {
        div { class: "scrim open", onclick: move |_| overlay.set(Overlay::None) }
        div {
            class: "sheet open",
            role: "dialog",
            "aria-label": "New bot",
            onkeydown: move |event: KeyboardEvent| {
                if event.key() == Key::Escape {
                    overlay.set(Overlay::None);
                }
            },
            div { class: "sheet-head",
                h2 { class: "sheet-title", "New bot" }
                button {
                    class: "icon-btn escape-close",
                    title: "Close",
                    "aria-label": "Close",
                    onclick: move |_| overlay.set(Overlay::None),
                    Icon { name: "x", size: 16 }
                }
            }
            div { class: "sheet-body",
                if let Some(message) = error {
                    div { class: "banner err", span { "{message}" } }
                }
                div { class: "field",
                    label { class: "field-label", "Name" }
                    input {
                        class: "input",
                        autofocus: true,
                        value: "{name}",
                        placeholder: "For example Recruiter",
                        oninput: move |event| session.write().new_bot.name = event.value(),
                        onkeydown: move |event: KeyboardEvent| {
                            if event.key() == Key::Enter && session.write().submit_new_bot() {
                                overlay.set(Overlay::None);
                            }
                        },
                    }
                }
                div { class: "field",
                    label { class: "field-label", "Picture (optional)" }
                    AvatarPicker {
                        preview: preview.clone(),
                        letter: letter.clone(),
                        style: avatar_style(&name),
                        has_image: preview.is_some(),
                        on_pick: move |bytes: Vec<u8>| session.write().pick_image(ImageSlot::NewBot, bytes),
                        on_clear: move |_| session.write().clear_image(ImageSlot::NewBot),
                    }
                }
                div { class: "field",
                    label { class: "field-label", "Model" }
                    select {
                        class: "select",
                        value: "{model}",
                        "aria-label": "Model",
                        onchange: move |event| session.write().new_bot.model = event.value(),
                        option { value: "", "Use my default" }
                        for (provider, models) in groups {
                            optgroup { key: "{provider}", label: "{human_provider(&provider)}",
                                for item in models {
                                    option { key: "{item}", value: "{item}", "{human_model(&item)}" }
                                }
                            }
                        }
                    }
                }
                if !rooms.is_empty() {
                    div { class: "field",
                        label { class: "field-label", "Add to a group (optional)" }
                        select {
                            class: "select",
                            value: "{group}",
                            "aria-label": "Group",
                            onchange: move |event| session.write().new_bot.group = event.value(),
                            option { value: "", "No group" }
                            for (id, label) in rooms {
                                option { key: "{id}", value: "{id}", "{label}" }
                            }
                        }
                    }
                }
                details { class: "advanced",
                    summary { "Advanced" }
                    div { class: "advanced-body",
                        div { class: "kv-row",
                            span { class: "kv-key", "Id" }
                            span { class: "kv-val mono",
                                if id_preview.is_empty() { "made from the name" } else { "{id_preview}" }
                            }
                        }
                        p { class: "hint", "Made from the name for you. It gets a number if it is taken." }
                    }
                }
            }
            div { class: "sheet-foot",
                button { class: "btn ghost", onclick: move |_| overlay.set(Overlay::None), "Cancel" }
                button {
                    class: "btn primary",
                    disabled: !can_create,
                    onclick: move |_| {
                        if session.write().submit_new_bot() {
                            overlay.set(Overlay::None);
                        }
                    },
                    "Create bot"
                }
            }
        }
    }
}

#[component]
pub fn NewGroupSheet(session: Signal<Session>, overlay: Signal<Overlay>) -> Element {
    let mut overlay = overlay;
    let mut query = use_signal(String::new);
    let name = session.read().new_group.name.clone();
    let description = session.read().new_group.description.clone();
    let rules = session.read().new_group.rules.clone();
    let model = session.read().new_group.model.clone();
    let members = session.read().new_group.members.clone();
    let error = session.read().new_group.error.clone();
    let preview = session
        .read()
        .new_group
        .avatar
        .as_ref()
        .map(|image| image.preview.clone());
    let groups = model_groups(&session.read());
    let needle = query().trim().to_lowercase();
    let bots: Vec<(String, String, String, String, bool)> = {
        let state = session.read();
        state
            .state
            .listed_bots
            .iter()
            .filter(|bot| bot.enabled && bot.role != "orchestrator")
            .map(|bot| {
                (
                    bot.id.clone(),
                    state.bot_display_name(&bot.id),
                    state.bot_avatar(&bot.id),
                    bot.role.clone(),
                    members.contains(&bot.id),
                )
            })
            .filter(|(_, label, ..)| needle.is_empty() || label.to_lowercase().contains(&needle))
            .collect()
    };
    let full = members.len() >= MAX_TEAM_MEMBERS;
    let letter = name
        .trim()
        .chars()
        .next()
        .map(|c| c.to_uppercase().to_string())
        .unwrap_or_else(|| "G".into());
    let id_preview = preview_id(&name, "group");
    let can_create = serabot_desktop::valid_display_name(&name);
    let count = members.len();
    rsx! {
        div { class: "scrim open", onclick: move |_| overlay.set(Overlay::None) }
        div {
            class: "sheet open wide",
            role: "dialog",
            "aria-label": "New group",
            onkeydown: move |event: KeyboardEvent| {
                if event.key() == Key::Escape {
                    overlay.set(Overlay::None);
                }
            },
            div { class: "sheet-head",
                h2 { class: "sheet-title", "New group" }
                button {
                    class: "icon-btn escape-close",
                    title: "Close",
                    "aria-label": "Close",
                    onclick: move |_| overlay.set(Overlay::None),
                    Icon { name: "x", size: 16 }
                }
            }
            div { class: "sheet-body",
                if let Some(message) = error {
                    div { class: "banner err", span { "{message}" } }
                }
                div { class: "field",
                    label { class: "field-label", "Name" }
                    input {
                        class: "input",
                        autofocus: true,
                        value: "{name}",
                        placeholder: "For example Hiring team",
                        oninput: move |event| session.write().new_group.name = event.value(),
                    }
                }
                div { class: "field",
                    label { class: "field-label", "Description (optional)" }
                    input {
                        class: "input",
                        value: "{description}",
                        placeholder: "What is this group for?",
                        oninput: move |event| session.write().new_group.description = event.value(),
                    }
                }
                div { class: "field",
                    label { class: "field-label", "Picture (optional)" }
                    AvatarPicker {
                        preview: preview.clone(),
                        letter: letter.clone(),
                        style: group_avatar_style(&name),
                        has_image: preview.is_some(),
                        on_pick: move |bytes: Vec<u8>| session.write().pick_image(ImageSlot::NewGroup, bytes),
                        on_clear: move |_| session.write().clear_image(ImageSlot::NewGroup),
                    }
                }
                div { class: "field",
                    label { class: "field-label", "Members" }
                    span { class: "member-count", "{count} of {MAX_TEAM_MEMBERS} chosen" }
                    input {
                        class: "input",
                        placeholder: "Search bots",
                        value: "{query}",
                        oninput: move |event| query.set(event.value()),
                    }
                    div { class: "check-list",
                        for (id, label, avatar, role, on) in bots {
                            label {
                                key: "{id}",
                                class: if on { "check-row on" } else { "check-row" },
                                input {
                                    "type": "checkbox",
                                    checked: on,
                                    disabled: !on && full,
                                    onchange: {
                                        let id = id.clone();
                                        move |event| {
                                            let mut state = session.write();
                                            state.new_group.members.retain(|m| *m != id);
                                            if event.checked() && state.new_group.members.len() < MAX_TEAM_MEMBERS {
                                                state.new_group.members.push(id.clone());
                                            }
                                        }
                                    },
                                }
                                span { class: "avatar sm bot", style: avatar_style(&id), "{avatar}" }
                                span { class: "thread-title", "{label}" }
                                span { class: "thread-meta", "{role}" }
                            }
                        }
                        if session.read().state.listed_bots.iter().all(|bot| !bot.enabled || bot.role == "orchestrator") {
                            p { class: "hint", "No bots yet. Create a bot first, then add it here." }
                        }
                    }
                }
                div { class: "field",
                    label { class: "field-label", "Custom rules (optional)" }
                    textarea {
                        class: "textarea",
                        value: "{rules}",
                        placeholder: "Anything every member should always follow",
                        oninput: move |event| session.write().new_group.rules = event.value(),
                    }
                }
                div { class: "field",
                    label { class: "field-label", "Model" }
                    select {
                        class: "select",
                        value: "{model}",
                        "aria-label": "Model",
                        onchange: move |event| session.write().new_group.model = event.value(),
                        option { value: "", "Use my default" }
                        for (provider, models) in groups {
                            optgroup { key: "{provider}", label: "{human_provider(&provider)}",
                                for item in models {
                                    option { key: "{item}", value: "{provider}|{item}", "{human_model(&item)}" }
                                }
                            }
                        }
                    }
                }
                details { class: "advanced",
                    summary { "Advanced" }
                    div { class: "advanced-body",
                        div { class: "kv-row",
                            span { class: "kv-key", "Id" }
                            span { class: "kv-val mono",
                                if id_preview.is_empty() { "made from the name" } else { "room:{id_preview}" }
                            }
                        }
                        p { class: "hint", "Made from the name for you. It gets a number if it is taken." }
                    }
                }
            }
            div { class: "sheet-foot",
                button { class: "btn ghost", onclick: move |_| overlay.set(Overlay::None), "Cancel" }
                button {
                    class: "btn primary",
                    disabled: !can_create,
                    onclick: move |_| {
                        if session.write().submit_new_group() {
                            overlay.set(Overlay::None);
                        }
                    },
                    "Create group"
                }
            }
        }
    }
}

/// The chat that opens right after a bot is created.
#[component]
pub fn OnboardingView(session: Signal<Session>) -> Element {
    let ui = {
        let state = session.read();
        (
            state.onboarding_ui.bot.clone(),
            state.onboarding_ui.input.clone(),
            state.onboarding_ui.waiting,
            state.onboarding_ui.error.clone(),
        )
    };
    let (bot, input, waiting, error) = ui;
    let (name, avatar) = {
        let state = session.read();
        (state.bot_display_name(&bot), state.bot_avatar(&bot))
    };
    let (lines, can_confirm, done) = {
        let state = session.read();
        match state.state.onboarding.as_ref().filter(|o| o.bot == bot) {
            Some(onboarding) => (
                onboarding
                    .messages
                    .iter()
                    .map(|line| (line.from == "user", line.text.clone()))
                    .collect::<Vec<_>>(),
                onboarding.can_confirm,
                onboarding.done,
            ),
            None => (Vec::new(), false, false),
        }
    };
    let loading = lines.is_empty() && error.is_none();
    rsx! {
        header { class: "topbar chat-header",
            span { class: "avatar bot", style: avatar_style(&bot), "{avatar}" }
            div { class: "chat-header-copy",
                strong { "{name}" }
                span {
                    if done { "Ready" } else { "Getting set up" }
                }
            }
            span { class: if done { "pill ok" } else { "pill" }, role: "status",
                if done { "saved" } else { "new bot" }
            }
            div { class: "chat-header-actions",
                button {
                    class: "btn ghost sm",
                    onclick: move |_| session.write().close_onboarding(),
                    if done { "Close" } else { "Skip for now" }
                }
            }
        }
        div { class: "pane",
            div { class: "pane-body chat-body",
                div { class: "chat-column",
                    div { class: "messages", id: "messages",
                        if loading {
                            div { class: "empty",
                                div { class: "empty-icon", Icon { name: "robot", size: 20 } }
                                h2 { class: "empty-title", "Waking up {name}" }
                            }
                        }
                        for (index, (user, text)) in lines.into_iter().enumerate() {
                            div {
                                key: "{index}",
                                class: if user { "msg user speaker-change" } else { "msg bot speaker-change" },
                                span {
                                    class: if user { "msg-avatar avatar user" } else { "msg-avatar avatar bot" },
                                    style: if user { String::new() } else { avatar_style(&bot) },
                                    if user { "Y" } else { "{avatar}" }
                                }
                                div { class: "msg-main",
                                    if !user {
                                        div { class: "msg-head", span { class: "msg-name", "{name}" } }
                                    }
                                    div { class: "msg-body",
                                        div { class: "md-body",
                                            for (n, paragraph) in text.split("\n\n").map(str::to_owned).collect::<Vec<_>>().into_iter().enumerate() {
                                                p { key: "{n}", class: "md-p", style: "white-space: pre-wrap;", "{paragraph}" }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                        if waiting && !loading {
                            div { class: "tool-row", role: "status",
                                span { class: "spinner" }
                                span { class: "tool-name", "{name} is thinking" }
                            }
                        }
                        if let Some(message) = error {
                            div { class: "banner err", span { "{message}" } }
                        }
                        if can_confirm && !waiting {
                            div { class: "onboard-confirm",
                                button {
                                    class: "btn primary",
                                    onclick: move |_| session.write().confirm_onboarding(),
                                    Icon { name: "check", size: 15 }
                                    "Looks right, save it"
                                }
                                span { class: "hint", "Or type what to change below." }
                            }
                        }
                        if done {
                            div { class: "onboard-confirm",
                                button {
                                    class: "btn primary",
                                    onclick: {
                                        let bot = bot.clone();
                                        move |_| {
                                            session.write().close_onboarding();
                                            session.write().open_bot_chat(bot.clone());
                                        }
                                    },
                                    "Start chatting"
                                }
                            }
                        }
                    }
                    if !done {
                        div { class: "composer-wrap",
                            div { class: "composer",
                                textarea {
                                    class: "composer-input",
                                    rows: 1,
                                    "aria-label": "Your answer",
                                    value: "{input}",
                                    placeholder: "Answer {name}",
                                    disabled: waiting,
                                    oninput: move |event| session.write().onboarding_ui.input = event.value(),
                                    onkeydown: move |event: KeyboardEvent| {
                                        if event.key() == Key::Enter && !event.modifiers().contains(Modifiers::SHIFT) {
                                            event.prevent_default();
                                            session.write().send_onboarding_reply();
                                        }
                                    },
                                }
                                div { class: "composer-row",
                                    div { class: "team-row" }
                                    div { class: "composer-actions",
                                        button {
                                            class: "btn primary send-btn",
                                            title: "Send",
                                            "aria-label": "Send answer",
                                            disabled: waiting || input.trim().is_empty(),
                                            onclick: move |_| session.write().send_onboarding_reply(),
                                            Icon { name: "send", size: 16 }
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
}

/// Plain-language ability switches for one bot, under short headings. They
/// edit the settings draft (Save applies them). An ability that needs a
/// connector that is not connected says so and offers "Connect". The raw tool
/// ids are only under "Advanced".
#[component]
pub fn CapabilityToggles(session: Signal<Session>, bot: String) -> Element {
    let _ = &bot;
    let tools = session.read().bot_settings.draft.tools.clone();
    let hydrated = session.read().bot_settings.hydrated;
    let mut last_group = "";
    let mut rows = Vec::new();
    for (index, capability) in capabilities::CAPABILITIES.iter().enumerate() {
        let heading = (capability.group != last_group).then_some(capability.group);
        last_group = capability.group;
        rows.push((
            index,
            heading,
            capability.label,
            capability.hint,
            capabilities::is_on(capability, &tools),
            session.read().capability_missing(index),
        ));
    }
    let raw = tools.clone();
    let uncovered = capabilities::uncovered(&tools);
    rsx! {
        div { class: "cap-list",
            for (index, heading, label, hint, on, missing) in rows {
                if let Some(heading) = heading {
                    h3 { key: "h{index}", class: "cap-group", "{heading}" }
                }
                if let Some(missing) = missing {
                    div { key: "{index}", class: "cap-row cap-needs",
                        span { class: "cap-copy",
                            span { class: "cap-label", "{label}" }
                            span { class: "cap-hint", "Needs {missing.name}" }
                        }
                        button {
                            class: "btn sm",
                            r#type: "button",
                            "aria-label": "Connect {missing.name} for {label}",
                            onclick: move |_| {
                                let section = match missing.page {
                                    capabilities::MissingPage::Connectors => Section::Connectors,
                                    capabilities::MissingPage::Settings => Section::Settings,
                                };
                                session.write().open_section(section);
                            },
                            "Connect"
                        }
                    }
                } else {
                    label { key: "{index}", class: "cap-row",
                        span { class: "cap-copy",
                            span { class: "cap-label", "{label}" }
                            span { class: "cap-hint", "{hint}" }
                        }
                        input {
                            class: "switch",
                            "type": "checkbox",
                            role: "switch",
                            "aria-label": "{label}",
                            checked: on,
                            disabled: !hydrated,
                            onchange: move |event| {
                                session.write().toggle_draft_capability(index, event.checked())
                            },
                        }
                    }
                }
            }
        }
        p { class: "approval-note", Icon { name: "shield", size: 14 } "Asks before deleting or sending anything" }
        details { class: "advanced",
            summary { "Advanced" }
            div { class: "advanced-body",
                p { class: "hint", "The exact tools this bot has. Use the switches above to change them." }
                div { class: "chips",
                    for tool in raw {
                        span { key: "{tool}", class: "chip mono", "{tool}" }
                    }
                    if uncovered.is_empty() && tools.is_empty() {
                        span { class: "hint", "None" }
                    }
                }
            }
        }
    }
}
