//! Create-a-bot and create-a-group sheets, the onboarding chat, the avatar
//! picker and the plain-language capability toggles (0.15).
//!
//! Nothing here asks for an id or a tag: the owner types a name, and the
//! server derives the id. Ids appear only read-only under "Advanced".

use crate::ui::{Icon, Overlay, avatar_style, group_avatar_style, human_model, human_provider};
use dioxus::prelude::*;
use serabot_desktop::capabilities;
use serabot_desktop::route_chip;
use serabot_desktop::route_editor::{
    self, MAX_FALLBACKS, MAX_SWITCHES, ModelChoice, ProviderChoice, RouteCheck, RouteSlot,
};
use serabot_desktop::session::{ImageSlot, RouteChange, Section, Session};
use serabot_desktop::state::{meet_order, shipped_label};
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
    let mut size_error = use_signal(|| Option::<String>::None);
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
                                        size_error.set(Some("Image must be 1 MB or smaller.".into()));
                                        return;
                                    }
                                    size_error.set(None);
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
                            r#type: "button",
                            onclick: move |_| {
                                size_error.set(None);
                                on_clear.call(());
                            },
                            "Remove"
                        }
                    }
                }
                if let Some(err) = size_error() {
                    span { class: "error-text", role: "alert", "{err}" }
                } else {
                    span { class: "hint", "PNG, JPEG or WebP, up to 1 MB." }
                }
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
    let busy = !session.read().ready();
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
                            if event.key() == Key::Enter && !event.is_composing() && session.write().submit_new_bot() {
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
                button { class: "btn ghost", r#type: "button", onclick: move |_| overlay.set(Overlay::None), "Cancel" }
                button {
                    class: "btn primary",
                    r#type: "button",
                    disabled: !can_create || busy,
                    onclick: move |_| {
                        if session.write().submit_new_bot() {
                            overlay.set(Overlay::None);
                        }
                    },
                    if busy { "Creating\u{2026}" } else { "Create bot" }
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
    let busy = !session.read().ready();
    let count = members.len();
    let selected_members: Vec<(String, String)> = {
        let state = session.read();
        members
            .iter()
            .map(|id| (id.clone(), state.bot_display_name(id)))
            .collect()
    };
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
                    div { class: "row-between",
                        span { class: "member-count", "{count} of {MAX_TEAM_MEMBERS} chosen" }
                        if !selected_members.is_empty() {
                            button {
                                class: "btn ghost sm",
                                r#type: "button",
                                onclick: move |_| session.write().new_group.members.clear(),
                                "Clear all"
                            }
                        }
                    }
                    if !selected_members.is_empty() {
                        div { class: "chips",
                            for (mid, mlabel) in selected_members {
                                span { key: "{mid}", class: "chip",
                                    "{mlabel}"
                                    button {
                                        class: "btn ghost sm chip-remove",
                                        r#type: "button",
                                        "aria-label": "Remove {mlabel}",
                                        onclick: move |_| {
                                            session.write().new_group.members.retain(|m| *m != mid);
                                        },
                                        "\u{00d7}"
                                    }
                                }
                            }
                        }
                    }
                    input {
                        class: "input",
                        r#type: "search",
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
                button { class: "btn ghost", r#type: "button", onclick: move |_| overlay.set(Overlay::None), "Cancel" }
                button {
                    class: "btn primary",
                    r#type: "button",
                    disabled: !can_create || busy,
                    onclick: move |_| {
                        if session.write().submit_new_group() {
                            overlay.set(Overlay::None);
                        }
                    },
                    if busy { "Creating\u{2026}" } else { "Create group" }
                }
            }
        }
    }
}

/// "Meet your team": every shipped bot, with add, switch and remove for the
/// owner's team. The same list is the Bots tab of the Integrations screen.
#[component]
pub fn MeetTeamSheet(session: Signal<Session>, overlay: Signal<Overlay>) -> Element {
    let mut overlay = overlay;
    let mut query = use_signal(String::new);
    let total_count = session.read().state.shipped_bots.len();
    rsx! {
        div { class: "scrim open", onclick: move |_| overlay.set(Overlay::None) }
        div {
            class: "sheet open wide",
            role: "dialog",
            "aria-label": "Meet your team",
            onkeydown: move |event: KeyboardEvent| {
                if event.key() == Key::Escape {
                    overlay.set(Overlay::None);
                }
            },
            div { class: "sheet-head",
                h2 { class: "sheet-title", "Meet your team" }
                button {
                    class: "icon-btn escape-close",
                    title: "Close",
                    "aria-label": "Close",
                    onclick: move |_| overlay.set(Overlay::None),
                    Icon { name: "x", size: 16 }
                }
            }
            div { class: "sheet-body",
                p { class: "hint", "The bots SeraBot ships. Each one you add becomes your own bot." }
                if total_count > 3 {
                    input {
                        class: "input",
                        r#type: "search",
                        placeholder: "Search shipped bots\u{2026}",
                        "aria-label": "Search shipped bots",
                        value: "{query}",
                        oninput: move |e| query.set(e.value()),
                    }
                }
                ShippedList { session, query: query(), overlay: Some(overlay) }
            }
            div { class: "sheet-foot",
                button { class: "btn ghost", r#type: "button", onclick: move |_| overlay.set(Overlay::None), "Done" }
            }
        }
    }
}

/// Every shipped bot with the owner's state for it: "Add to my team" when not
/// installed, otherwise switch on/off and remove (the orchestrator is
/// required and has neither). Used by the sheet and the Bots tab.
#[component]
pub fn ShippedList(
    session: Signal<Session>,
    query: String,
    overlay: Option<Signal<Overlay>>,
) -> Element {
    let mut confirming = use_signal(|| Option::<String>::None);
    let needle = query.trim().to_ascii_lowercase();
    let (rows, total_count, loaded, installing, error, busy) = {
        let state = session.read();
        let all_rows = meet_order(&state.state.shipped_bots)
            .into_iter()
            .map(|bot| {
                let name = shipped_label(bot);
                let letter = name
                    .chars()
                    .next()
                    .map(|c| c.to_uppercase().to_string())
                    .unwrap_or_else(|| "B".into());
                (
                    bot.id.clone(),
                    name,
                    letter,
                    bot.role.clone(),
                    bot.description.clone().unwrap_or_default(),
                    bot.installed,
                    bot.enabled,
                    bot.catalog_only,
                )
            })
            .collect::<Vec<_>>();
        let total = all_rows.len();
        let filtered = all_rows
            .into_iter()
            .filter(|(_, name, _, role, desc, _, _, _)| {
                needle.is_empty()
                    || name.to_ascii_lowercase().contains(&needle)
                    || role.to_ascii_lowercase().contains(&needle)
                    || desc.to_ascii_lowercase().contains(&needle)
            })
            .collect::<Vec<_>>();
        (
            filtered,
            total,
            state.state.shipped_loaded,
            state.state.shipped_installing.clone(),
            state.state.shipped_error.clone(),
            state.bot_settings.lifecycle_busy,
        )
    };
    rsx! {
        if let Some(message) = error {
            div { class: "banner err", span { "{message}" } }
        }
        if !loaded {
            div { class: "row",
                span { class: "spinner", title: "Loading the team" }
                span { class: "hint", "Loading the team\u{2026}" }
            }
        } else if total_count == 0 {
            p { class: "hint", "No shipped bots are available on this server." }
        } else if rows.is_empty() {
            p { class: "hint", "No shipped bots match \"{query}\"." }
        }
        div { class: "roster",
            for (id, name, letter, role, description, installed, enabled, catalog_only) in rows {
                div { key: "{id}", class: "roster-row",
                    span { class: "avatar lg bot", style: avatar_style(&id), "{letter}" }
                    span { class: "meet-copy",
                        span { class: "thread-title",
                            "{name}"
                            if catalog_only && !installed {
                                span { class: "chip accent meet-new", title: "Not on your team yet: add it here", "New" }
                            }
                        }
                        span { class: "thread-meta", "{role}" }
                        if !description.is_empty() {
                            span { class: "hint meet-desc", "{description}" }
                        }
                    }
                    if installed {
                        div { class: "row-actions",
                            if !serabot_desktop::can_remove_bot(&id, &role) {
                                span { class: "chip ok", "Required" }
                            } else if enabled {
                                span { class: "chip ok", "Enabled" }
                            } else {
                                span { class: "chip", "Disabled" }
                            }
                            button {
                                class: "btn ghost sm",
                                r#type: "button",
                                onclick: {
                                    let id = id.clone();
                                    move |_| {
                                        if let Some(mut overlay) = overlay {
                                            overlay.set(Overlay::None);
                                        }
                                        session.write().open_bot_chat(id.clone());
                                    }
                                },
                                "Chat"
                            }
                            if serabot_desktop::can_remove_bot(&id, &role) {
                                button {
                                    class: "btn ghost sm",
                                    r#type: "button",
                                    disabled: busy,
                                    onclick: {
                                        let id = id.clone();
                                        move |_| session.write().set_bot_enabled(id.clone(), !enabled)
                                    },
                                    if enabled { "Disable" } else { "Enable" }
                                }
                                if confirming().as_deref() == Some(id.as_str()) {
                                    button {
                                        class: "btn danger sm",
                                        r#type: "button",
                                        disabled: busy,
                                        onclick: {
                                            let id = id.clone();
                                            move |_| {
                                                confirming.set(None);
                                                session.write().remove_shipped_bot(id.clone());
                                            }
                                        },
                                        "Confirm remove"
                                    }
                                    button {
                                        class: "btn ghost sm",
                                        r#type: "button",
                                        onclick: move |_| confirming.set(None),
                                        "Cancel"
                                    }
                                } else {
                                    button {
                                        class: "btn ghost sm",
                                        r#type: "button",
                                        disabled: busy,
                                        onclick: {
                                            let id = id.clone();
                                            move |_| confirming.set(Some(id.clone()))
                                        },
                                        "Remove"
                                    }
                                }
                            }
                        }
                    } else {
                        button {
                            class: "btn primary sm",
                            r#type: "button",
                            disabled: installing.is_some(),
                            onclick: {
                                let id = id.clone();
                                move |_| session.write().install_shipped_bot(id.clone())
                            },
                            if installing.as_deref() == Some(id.as_str()) {
                                "Adding\u{2026}"
                            } else {
                                "Add to my team"
                            }
                        }
                    }
                }
            }
        }
    }
}

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
    let (name, avatar, user_initial) = {
        let state = session.read();
        let initial = state
            .state
            .user
            .as_ref()
            .and_then(|u| u.label().chars().next())
            .map(|c| c.to_uppercase().to_string())
            .unwrap_or_else(|| "Y".into());
        (
            state.bot_display_name(&bot),
            state.bot_avatar(&bot),
            initial,
        )
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
                                    if user { "{user_initial}" } else { "{avatar}" }
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
                                    rows: "{input.lines().count().clamp(1, 5)}",
                                    "aria-label": "Your answer",
                                    value: "{input}",
                                    placeholder: "Answer {name}",
                                    disabled: waiting,
                                    oninput: move |event| session.write().onboarding_ui.input = event.value(),
                                    onkeydown: move |event: KeyboardEvent| {
                                        if event.key() == Key::Enter
                                            && !event.is_composing()
                                            && !event.modifiers().contains(Modifiers::SHIFT)
                                        {
                                            event.prevent_default();
                                            session.write().send_onboarding_reply();
                                        }
                                    },
                                }
                                div { class: "composer-row",
                                    div { class: "team-row",
                                        span { class: "hint", "Enter to send · Shift+Enter for new line" }
                                    }
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
    let mut filter = use_signal(String::new);
    let tools = session.read().bot_settings.draft.tools.clone();
    let hydrated = session.read().bot_settings.hydrated;
    let needle = filter().trim().to_ascii_lowercase();
    let mut last_group = "";
    let mut rows = Vec::new();
    for (index, capability) in capabilities::CAPABILITIES.iter().enumerate() {
        if !needle.is_empty()
            && !capability.label.to_ascii_lowercase().contains(&needle)
            && !capability.hint.to_ascii_lowercase().contains(&needle)
            && !capability.group.to_ascii_lowercase().contains(&needle)
        {
            continue;
        }
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
        div { class: "cap-filter",
            input {
                class: "input",
                r#type: "search",
                placeholder: "Filter abilities\u{2026}",
                "aria-label": "Filter abilities",
                value: "{filter}",
                oninput: move |e| filter.set(e.value()),
                onkeydown: move |e: KeyboardEvent| {
                    if e.key() == Key::Escape {
                        filter.set(String::new());
                    }
                },
            }
        }
        div { class: "cap-list",
            if rows.is_empty() {
                p { class: "hint", "No abilities match \"{filter}\"." }
            }
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
                                    capabilities::MissingPage::Connectors => Section::Integrations,
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

/// Everything one row of the route editor draws, prepared as plain data so the
/// markup holds no borrow of the session.
#[derive(Clone, PartialEq)]
struct RouteRowView {
    row: usize,
    title: &'static str,
    slot: RouteSlot,
    providers: Vec<ProviderChoice>,
    accounts: Vec<(String, String)>,
    models: Vec<ModelChoice>,
    levels: Vec<String>,
    error: Option<String>,
    /// This fallback names another provider than the primary.
    consent_row: bool,
    consent: bool,
    provider_label: String,
}

fn route_row_views(session: &Session) -> (Vec<RouteRowView>, RouteCheck) {
    let options = session.route_options();
    let draft = &session.route_editor.draft;
    // Until the catalog arrives every provider would read "not available".
    let check = if session.state.catalog.is_some() {
        draft.check(&options)
    } else {
        RouteCheck::default()
    };
    let rows = (0..=MAX_FALLBACKS)
        .map(|row| {
            let slot = match row {
                0 => draft.primary.clone(),
                other => draft.fallbacks[other - 1].clone(),
            };
            let (title, error) = match row {
                0 => ("Primary", check.primary.clone()),
                1 => ("Fallback 1", check.fallbacks[0].clone()),
                _ => ("Fallback 2", check.fallbacks[1].clone()),
            };
            let chosen = !slot.is_empty();
            RouteRowView {
                row,
                title,
                providers: options.provider_choices(),
                accounts: if chosen {
                    options.account_choices(&slot.provider)
                } else {
                    Vec::new()
                },
                models: if chosen {
                    options.model_choices(&slot.provider)
                } else {
                    Vec::new()
                },
                levels: options.thinking_levels(&slot.provider, &slot.model),
                consent_row: row > 0 && check.needs_consent[row - 1],
                consent: row > 0 && draft.consent[row - 1],
                provider_label: options.provider_label(&slot.provider),
                error,
                slot,
            }
        })
        .collect();
    (rows, check)
}

/// The bot's "Model route": a primary route and up to two fallbacks, each with
/// a provider, an account (or any), a model and a thinking level, plus the
/// auto-switch switches. Saved on its own with the Save route button; the rest
/// of the settings form keeps its own Save.
#[component]
pub fn RouteEditor(session: Signal<Session>, bot: String) -> Element {
    let acp_on = session.read().bot_uses_claude_agent(&bot);
    let bot_for_server = bot.clone();
    let bot_for_acp = bot.clone();
    let (rows, check, hydrated, saving, dirty, error, notice, failover, effective, last_switch) = {
        let s = session.read();
        let (rows, check) = route_row_views(&s);
        let options = s.route_options();
        let view = s
            .bot_settings
            .bot
            .as_deref()
            .and_then(|id| s.state.bot_details.get(id))
            .and_then(|detail| detail.route.as_ref());
        (
            rows,
            check,
            s.route_editor.hydrated,
            s.route_editor.saving,
            s.route_editor.dirty(),
            s.route_editor.error.clone(),
            s.route_editor.notice.clone(),
            s.route_editor.draft.failover,
            view.and_then(|view| view.effective.as_ref())
                .map(|effective| route_editor::effective_line(&options, effective)),
            view.and_then(|view| view.last_switch.as_ref())
                .map(|switch| route_editor::switch_line(&options, switch)),
        )
    };
    let can_save = hydrated && dirty && !saving && check.is_ok();
    rsx! {
        div { class: "field route-field",
            span { class: "field-label", "Model & agent route" }
            div { class: "route-block", role: "group", "aria-label": "Model and agent route",
                div { class: "route-slot",
                    div { class: "route-slot-title", span { "Execution engine" } }
                    div { class: "seg", role: "group", "aria-label": "Execution engine",
                        button {
                            class: if !acp_on { "seg-btn active" } else { "seg-btn" },
                            r#type: "button",
                            "aria-pressed": if !acp_on { "true" } else { "false" },
                            onclick: move |_| session.write().set_bot_claude_agent(&bot_for_server, false),
                            "Server model route"
                        }
                        button {
                            class: if acp_on { "seg-btn active" } else { "seg-btn" },
                            r#type: "button",
                            "aria-pressed": if acp_on { "true" } else { "false" },
                            onclick: move |_| session.write().set_bot_claude_agent(&bot_for_acp, true),
                            "Claude Code (Local ACP)"
                        }
                    }
                    span { class: "hint",
                        if acp_on {
                            "This bot runs locally through Claude Code (ACP) in an allowed desktop folder with per-action approval."
                        } else {
                            "Uses the server provider chain below (and can reach allowed local folders via the relay)."
                        }
                    }
                }
                if !hydrated {
                    div { class: "banner info", role: "status",
                        span { class: "spinner" }
                        span { "Loading the route\u{2026}" }
                    }
                }
                for view in rows {
                    RouteRow { session, view, hydrated }
                }
                div { class: "route-slot",
                    div { class: "route-slot-title", span { "When a route fails" } }
                    label { class: "check",
                        input {
                            class: "switch",
                            r#type: "checkbox",
                            checked: failover.enabled,
                            disabled: !hydrated,
                            onchange: move |event| {
                                let mut s = session.write();
                                s.route_editor.draft.failover.enabled = event.checked();
                                s.route_editor.error = None;
                                s.route_editor.notice = None;
                            },
                        }
                        span { "Switch automatically" }
                    }
                    label { class: "check",
                        input {
                            class: "switch",
                            r#type: "checkbox",
                            checked: failover.switch_account_first,
                            disabled: !hydrated || !failover.enabled,
                            onchange: move |event| {
                                let mut s = session.write();
                                s.route_editor.draft.failover.switch_account_first = event.checked();
                                s.route_editor.error = None;
                                s.route_editor.notice = None;
                            },
                        }
                        span { "Try the provider's other accounts first" }
                    }
                    div { class: "field",
                        label { class: "field-label", r#for: "route-max-switches", "Most switches per turn" }
                        select {
                            id: "route-max-switches",
                            class: "select",
                            value: "{failover.max_switches}",
                            disabled: !hydrated || !failover.enabled,
                            onchange: move |event| {
                                if let Ok(count) = event.value().parse::<u8>() {
                                    let mut s = session.write();
                                    s.route_editor.draft.failover.max_switches = count.clamp(1, MAX_SWITCHES);
                                    s.route_editor.error = None;
                                    s.route_editor.notice = None;
                                }
                            },
                            for count in 1..=MAX_SWITCHES {
                                option { key: "{count}", value: "{count}", selected: count == failover.max_switches, "{count}" }
                            }
                        }
                    }
                }
                if effective.is_some() || last_switch.is_some() {
                    div { class: "route-effective", role: "status",
                        if let Some(line) = effective {
                            span { "Starts on: {line}" }
                        }
                        if let Some(line) = last_switch {
                            span { "Last switch: {line}" }
                        }
                    }
                }
                if let Some(message) = error {
                    div { class: "banner err", role: "alert", span { "{message}" } }
                }
                if let Some(message) = notice {
                    div { class: "banner ok", role: "status", span { "{message}" } }
                }
                div { class: "route-actions",
                    button {
                        class: "btn primary sm",
                        r#type: "button",
                        disabled: !can_save,
                        onclick: move |_| session.write().save_bot_route(),
                        if saving { "Saving\u{2026}" } else { "Save route" }
                    }
                    button {
                        class: "btn ghost sm",
                        r#type: "button",
                        disabled: !dirty || saving,
                        onclick: move |_| session.write().reset_route_editor(),
                        "Reset"
                    }
                    if dirty {
                        span { class: "hint", role: "status", "Unsaved route changes. Save route to keep them." }
                    } else {
                        span { class: "hint", "Applies to the next turn. A turn already running keeps its route." }
                    }
                }
            }
        }
    }
}

#[component]
fn RouteRow(session: Signal<Session>, view: RouteRowView, hydrated: bool) -> Element {
    let row = view.row;
    let primary = row == 0;
    let chosen = !view.slot.is_empty();
    let segments = route_chip::thinking_segments(&view.levels, &view.slot.thinking);
    let class = if view.error.is_some() {
        "route-slot has-error"
    } else {
        "route-slot"
    };
    let none_label = if primary {
        "Follow my default route"
    } else {
        "No fallback"
    };
    let id = |part: &str| format!("route-{row}-{part}");
    rsx! {
        div { class: "{class}",
            div { class: "route-slot-title",
                span { "{view.title}" }
                if chosen && !primary {
                    button {
                        class: "btn ghost sm",
                        r#type: "button",
                        "aria-label": "Remove {view.title}",
                        onclick: move |_| session.write().route_change(row, RouteChange::Provider(String::new())),
                        "Remove"
                    }
                }
            }
            div { class: "route-grid",
                div { class: "field",
                    label { class: "field-label", r#for: "{id(\"provider\")}", "Provider" }
                    select {
                        id: "{id(\"provider\")}",
                        class: "select",
                        value: "{view.slot.provider}",
                        disabled: !hydrated,
                        onchange: move |event| session.write().route_change(row, RouteChange::Provider(event.value())),
                        option { value: "", selected: !chosen, "{none_label}" }
                        for choice in view.providers.clone() {
                            option {
                                key: "{choice.id}",
                                value: "{choice.id}",
                                selected: choice.id == view.slot.provider,
                                disabled: !choice.egress_approved,
                                if choice.egress_approved {
                                    "{choice.label}"
                                } else {
                                    "{choice.label} (no data-egress approval)"
                                }
                            }
                        }
                    }
                }
                if chosen {
                    div { class: "field",
                        label { class: "field-label", r#for: "{id(\"account\")}", "Account" }
                        select {
                            id: "{id(\"account\")}",
                            class: "select",
                            value: "{view.slot.account}",
                            disabled: !hydrated,
                            onchange: move |event| session.write().route_change(row, RouteChange::Account(event.value())),
                            for (account_id, label) in view.accounts.clone() {
                                option {
                                    key: "{account_id}",
                                    value: "{account_id}",
                                    selected: account_id == view.slot.account,
                                    "{label}"
                                }
                            }
                        }
                    }
                    div { class: "field",
                        label { class: "field-label", r#for: "{id(\"model\")}", "Model" }
                        select {
                            id: "{id(\"model\")}",
                            class: "select",
                            value: "{view.slot.model}",
                            disabled: !hydrated,
                            onchange: move |event| session.write().route_change(row, RouteChange::Model(event.value())),
                            if !view.slot.model.is_empty()
                                && !view.models.iter().any(|choice| choice.model == view.slot.model)
                            {
                                option { value: "{view.slot.model}", selected: true, "{view.slot.model} (not in your list)" }
                            }
                            for choice in view.models.clone() {
                                option {
                                    key: "{choice.model}",
                                    value: "{choice.model}",
                                    selected: choice.model == view.slot.model,
                                    "{choice.title}"
                                }
                            }
                        }
                    }
                    if !segments.is_empty() {
                        div { class: "field",
                            span { class: "field-label", "Thinking" }
                            div { class: "seg", role: "group", "aria-label": "Thinking level for {view.title}",
                                for segment in segments {
                                    button {
                                        key: "{segment.label}",
                                        r#type: "button",
                                        class: if segment.active { "seg-btn active" } else { "seg-btn" },
                                        "aria-pressed": if segment.active { "true" } else { "false" },
                                        disabled: !hydrated,
                                        onclick: {
                                            let value = segment.value.clone();
                                            move |_| session.write().route_change(row, RouteChange::Thinking(value.clone()))
                                        },
                                        "{segment.label}"
                                    }
                                }
                            }
                        }
                    }
                }
            }
            if view.consent_row {
                label { class: "route-consent",
                    input {
                        class: "switch",
                        r#type: "checkbox",
                        checked: view.consent,
                        disabled: !hydrated,
                        onchange: move |event| {
                            let mut s = session.write();
                            if let Some(slot) = s.route_editor.draft.consent.get_mut(row - 1) {
                                *slot = event.checked();
                            }
                            s.route_editor.error = None;
                            s.route_editor.notice = None;
                        },
                    }
                    span { "If the first route fails, allow sending my prompts to {view.provider_label}." }
                }
            }
            if let Some(message) = view.error.clone() {
                p { class: "field-error", role: "alert", "{message}" }
            }
        }
    }
}
