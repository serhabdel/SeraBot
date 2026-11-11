//! The settings panel on the right of the chat (0.16): it replaces the old
//! "Inspector". For a bot chat it is one editable form (picture, name,
//! description, instructions, model, browser profile, abilities) with Save and
//! Cancel; for a group it lists the members; "Files" shows what the bots made.
//!
//! Raw ids, routing and tool names live under "Advanced" only. The state and
//! the validation are in `session/bot_settings.rs`; this file draws.

use crate::ui::{Icon, Overlay, PinToggle, avatar_style, human_model, human_provider, stamp};
use crate::ui_bots::{AvatarPicker, CapabilityToggles, model_groups};
use dioxus::prelude::*;
use serabot_desktop::session::{ImageSlot, Section, Session, SettingsTab, humanize_label};
use serabot_desktop::{
    BotConfigUpdate, DEFAULT_PROFILE, MAX_BOT_SETTINGS_DESCRIPTION_CHARS, can_remove_bot, teams,
    valid_skill_id,
};

/// Cap for the skills array of one bot config.
const MAX_BOT_SKILLS: usize = 16;

#[component]
pub fn SettingsPanel(session: Signal<Session>, overlay: Signal<Overlay>) -> Element {
    let mut session = session;
    let (open, tab, bot_id) = {
        let s = session.read();
        (
            s.bot_settings.open,
            s.bot_settings.tab,
            s.bot_settings.bot.clone(),
        )
    };
    let conversation = session.read().conversation.clone();
    let is_room = teams::is_room(&conversation);
    let title = match &bot_id {
        Some(id) => session.read().bot_display_name(id),
        None => session.read().conversation_title(),
    };
    let subtitle = bot_id.as_ref().map(|id| {
        let s = session.read();
        s.state
            .listed_bots
            .iter()
            .find(|bot| &bot.id == id)
            .map(|bot| humanize_label(&bot.role))
            .unwrap_or_else(|| "Bot".to_owned())
    });
    let avatar = bot_id
        .as_ref()
        .map(|id| session.read().bot_avatar(id))
        .unwrap_or_else(|| {
            title
                .chars()
                .next()
                .map(|c| c.to_uppercase().to_string())
                .unwrap_or_default()
        });
    let head_style = match &bot_id {
        Some(id) => avatar_style(id),
        None => avatar_style(&title),
    };
    let settings_label = if bot_id.is_some() {
        "Settings"
    } else if is_room {
        "Members"
    } else {
        "About"
    };
    let tab = if bot_id.is_none() && !is_room && tab == SettingsTab::Settings {
        // A plain chat has nothing to configure: show its files.
        SettingsTab::Files
    } else {
        tab
    };
    rsx! {
        aside {
            class: if open { "inspector settings-panel open" } else { "inspector settings-panel" },
            "aria-label": "Settings",
            div { class: "settings-head",
                span { class: "avatar lg bot", style: "{head_style}", "{avatar}" }
                div { class: "settings-head-copy",
                    h2 { class: "inspector-title", "{title}" }
                    if let Some(subtitle) = subtitle {
                        span { class: "caption", "{subtitle}" }
                    }
                }
                button {
                    class: "icon-btn",
                    title: "Close",
                    "aria-label": "Close settings",
                    onclick: move |_| session.write().request_close_bot_settings(),
                    Icon { name: "x", size: 16 }
                }
            }
            if open && session.read().bot_settings.confirm_close {
                div { class: "settings-confirm", role: "alertdialog", "aria-label": "Save changes?",
                    span { class: "settings-confirm-title", "Save changes?" }
                    span { class: "caption", "You changed this bot. Save before closing?" }
                    div { class: "row",
                        button {
                            class: "btn primary sm",
                            r#type: "button",
                            onclick: move |_| session.write().save_and_close_bot_settings(),
                            "Save"
                        }
                        button {
                            class: "btn sm",
                            r#type: "button",
                            onclick: move |_| session.write().cancel_bot_settings(),
                            "Discard"
                        }
                        button {
                            class: "btn ghost sm",
                            r#type: "button",
                            onclick: move |_| session.write().keep_editing_bot_settings(),
                            "Keep editing"
                        }
                    }
                }
            }
            if bot_id.is_some() || is_room {
                div { class: "inspector-tabs settings-tabs",
                    button {
                        class: if tab == SettingsTab::Settings { "tab active" } else { "tab" },
                        onclick: move |_| session.write().bot_settings.tab = SettingsTab::Settings,
                        "{settings_label}"
                    }
                    button {
                        class: if tab == SettingsTab::Files { "tab active" } else { "tab" },
                        onclick: move |_| session.write().bot_settings.tab = SettingsTab::Files,
                        "Files"
                    }
                }
            }
            if open {
                {match (tab, bot_id) {
                    (SettingsTab::Files, _) => rsx! { FilesTab { session } },
                    (SettingsTab::Settings, Some(id)) => rsx! {
                        BotForm { session, id: id.clone() }
                        MemoryRefreshCard { session, id }
                    },
                    (SettingsTab::Settings, None) => rsx! { MembersTab { session, overlay } },
                }}
            }
        }
    }
}

/// "Refresh memory every N turns" for one bot (5 to 500, default 40). After N
/// of the owner's turns the server folds them into a short summary and saves
/// the lasting facts, in the background. The setting covers all of the bot's chats.
#[component]
fn MemoryRefreshCard(session: Signal<Session>, id: String) -> Element {
    let mut session = session;
    let mut draft = use_signal(String::new);
    let mut seeded = use_signal(|| false);
    let bot = id.clone();
    use_effect(move || {
        let loaded = session.read().state.bot_chats.contains_key(&bot);
        if !loaded {
            session.write().load_bot_chats(bot.clone());
        }
    });
    let current = session
        .read()
        .state
        .bot_chats
        .get(&id)
        .map(|list| list.refresh_every_turns);
    // Seed the field once the server's value is known.
    if let Some(value) = current
        && !seeded()
    {
        draft.set(value.to_string());
        seeded.set(true);
    }
    let parsed = serabot_desktop::bot_chats::parse_refresh_turns(&draft.read());
    let changed = parsed.is_some() && parsed != current;
    let invalid = !draft.read().trim().is_empty() && parsed.is_none();
    let save_id = id.clone();
    rsx! {
        div { class: "card memory-card",
            div { class: "card-head",
                h3 { class: "card-title", "Memory refresh" }
            }
            p { class: "tiny",
                "Every so many of your messages, this bot summarises the recent turns and saves the lasting facts to its memory, in the background. It covers all of this bot\u{2019}s chats. Default 40."
            }
            label { class: "field",
                span { "Refresh every (turns)" }
                input {
                    class: "input",
                    r#type: "number",
                    min: "{serabot_desktop::bot_chats::MIN_REFRESH_TURNS}",
                    max: "{serabot_desktop::bot_chats::MAX_REFRESH_TURNS}",
                    value: "{draft}",
                    disabled: current.is_none(),
                    "aria-invalid": if invalid { "true" } else { "false" },
                    oninput: move |event| draft.set(event.value()),
                }
            }
            if invalid {
                p { class: "tiny", role: "alert", "Enter a whole number from 5 to 500." }
            }
            div { class: "row-actions",
                button {
                    class: "btn primary small",
                    disabled: !changed,
                    onclick: move |_| {
                        if let Some(every) = parsed {
                            session.write().set_bot_refresh(save_id.clone(), every);
                        }
                    },
                    "Save"
                }
            }
        }
    }
}

/// The bot's form. The picture saves the moment it changes; everything else,
/// the abilities included, waits for Save.
#[component]
fn BotForm(session: Signal<Session>, id: String) -> Element {
    let mut session = session;
    let (draft, dirty, saving, hydrated, error, notice, has_image, letter) = {
        let s = session.read();
        (
            s.bot_settings.draft.clone(),
            s.bot_settings.dirty(),
            s.bot_settings.saving,
            s.bot_settings.hydrated,
            s.bot_settings.error.clone(),
            s.bot_settings.notice.clone(),
            // "bundled-" is the server's stamp for a shipped default picture:
            // there is nothing of the owner's to remove.
            s.state
                .listed_bots
                .iter()
                .find(|bot| bot.id == id)
                .is_some_and(|bot| {
                    bot.avatar_version
                        .as_deref()
                        .is_some_and(|version| !version.starts_with("bundled-"))
                }),
            s.bot_avatar(&id),
        )
    };
    let groups = model_groups(&session.read());
    let thinking_levels = session.read().bot_thinking_levels();
    let profiles = session.read().panel.profiles.clone();
    let name_error = draft.name_error();
    let description_error = draft.description_error();
    let instructions_error = draft.instructions_error();
    let description_left =
        MAX_BOT_SETTINGS_DESCRIPTION_CHARS as i64 - draft.description.chars().count() as i64;
    let can_save = hydrated && dirty && !saving && draft.first_error().is_none();
    // A model the list no longer offers stays visible instead of silently
    // switching to "Default" on save.
    let unknown_model = (!draft.model.is_empty()
        && !groups.iter().any(|(provider, models)| {
            models
                .iter()
                .any(|model| serabot_desktop::session::model_value(provider, model) == draft.model)
        }))
    .then(|| {
        serabot_desktop::session::model_pick(&draft.model)
            .map(|pick| human_model(&pick.model))
            .unwrap_or_else(|| "Current model".to_owned())
    });
    let profile_value = if draft.browser_profile.is_empty() {
        DEFAULT_PROFILE.to_owned()
    } else {
        draft.browser_profile.clone()
    };
    let save_id = id.clone();
    rsx! {
        form {
            class: "settings-form",
            onsubmit: move |event| {
                event.prevent_default();
                session.write().save_bot_settings();
            },
            div { class: "settings-scroll",
                div { class: "field",
                    span { class: "field-label", "Picture" }
                    AvatarPicker {
                        preview: None,
                        letter,
                        style: avatar_style(&id),
                        has_image,
                        on_pick: {
                            let id = id.clone();
                            move |bytes: Vec<u8>| session.write().pick_image(ImageSlot::Bot(id.clone()), bytes)
                        },
                        on_clear: {
                            let id = id.clone();
                            move |_| session.write().clear_image(ImageSlot::Bot(id.clone()))
                        },
                    }
                    span { class: "hint", "The picture is saved as soon as you choose it." }
                }
                div { class: "field",
                    label { class: "field-label", r#for: "bot-name", "Name" }
                    input {
                        id: "bot-name",
                        class: if name_error.is_some() { "input invalid" } else { "input" },
                        value: "{draft.display_name}",
                        disabled: !hydrated,
                        autocomplete: "off",
                        "aria-invalid": if name_error.is_some() { "true" } else { "false" },
                        oninput: move |event| {
                            let mut s = session.write();
                            s.bot_settings.draft.display_name = event.value();
                            s.bot_settings.error = None;
                            s.bot_settings.notice = None;
                        },
                    }
                    if let Some(message) = name_error.clone() {
                        span { class: "field-error", "{message}" }
                    }
                }
                div { class: "field",
                    label { class: "field-label", r#for: "bot-description", "What it's for" }
                    textarea {
                        id: "bot-description",
                        class: if description_error.is_some() { "textarea invalid" } else { "textarea" },
                        rows: "2",
                        value: "{draft.description}",
                        disabled: !hydrated,
                        placeholder: "A short line that explains this bot.",
                        oninput: move |event| {
                            let mut s = session.write();
                            s.bot_settings.draft.description = event.value();
                            s.bot_settings.error = None;
                            s.bot_settings.notice = None;
                        },
                    }
                    if let Some(message) = description_error.clone() {
                        span { class: "field-error", "{message}" }
                    } else if description_left <= 40 {
                        span { class: "hint", "{description_left} characters left" }
                    }
                }
                div { class: "field",
                    label { class: "field-label", r#for: "bot-instructions", "How it should behave" }
                    textarea {
                        id: "bot-instructions",
                        class: if instructions_error.is_some() { "textarea tall invalid" } else { "textarea tall" },
                        rows: "7",
                        value: "{draft.instructions}",
                        disabled: !hydrated,
                        placeholder: "Its role and rules, in your own words. For example: you book venues, always confirm the date first.",
                        oninput: move |event| {
                            let mut s = session.write();
                            s.bot_settings.draft.instructions = event.value();
                            s.bot_settings.error = None;
                            s.bot_settings.notice = None;
                        },
                    }
                    if let Some(message) = instructions_error.clone() {
                        span { class: "field-error", "{message}" }
                    }
                }
                div { class: "field",
                    label { class: "field-label", r#for: "bot-model", "Model" }
                    select {
                        id: "bot-model",
                        class: "select",
                        value: "{draft.model}",
                        disabled: !hydrated,
                        onchange: move |event| {
                            let mut s = session.write();
                            s.bot_settings.draft.model = event.value();
                            s.bot_settings.error = None;
                            s.bot_settings.notice = None;
                        },
                        option { value: "", selected: draft.model.is_empty(), "Default" }
                        if let Some(label) = unknown_model {
                            option { value: "{draft.model}", selected: true, "{label} (not in your list)" }
                        }
                        for (provider, models) in groups {
                            optgroup { key: "{provider}", label: "{human_provider(&provider)}",
                                for item in models {
                                    option {
                                        key: "{item}",
                                        value: "{serabot_desktop::session::model_value(&provider, &item)}",
                                        selected: serabot_desktop::session::model_value(&provider, &item) == draft.model,
                                        "{human_model(&item)}"
                                    }
                                }
                            }
                        }
                    }
                    span { class: "hint", "Default uses the model picked for the chat." }
                }
                if !thinking_levels.is_empty() {
                    div { class: "field",
                        label { class: "field-label", r#for: "bot-thinking", "Thinking" }
                        select {
                            id: "bot-thinking",
                            class: "select",
                            value: "{draft.thinking}",
                            disabled: !hydrated,
                            onchange: move |event| {
                                let mut s = session.write();
                                s.bot_settings.draft.thinking = event.value();
                                s.bot_settings.error = None;
                                s.bot_settings.notice = None;
                            },
                            option { value: "", selected: draft.thinking.is_empty(), "Default" }
                            for level in thinking_levels.clone() {
                                option {
                                    key: "{level}",
                                    value: "{level}",
                                    selected: level == draft.thinking,
                                    "{serabot_desktop::thinking_label(&level)}"
                                }
                            }
                        }
                        span { class: "hint", "Only applies when the model supports it." }
                    }
                }
                div { class: "field",
                    label { class: "field-label", r#for: "bot-profile", "Browser profile" }
                    select {
                        id: "bot-profile",
                        class: "select",
                        value: "{profile_value}",
                        disabled: !hydrated,
                        onchange: move |event| {
                            let picked = event.value();
                            let mut s = session.write();
                            s.bot_settings.draft.browser_profile =
                                if picked == DEFAULT_PROFILE { String::new() } else { picked };
                            s.bot_settings.error = None;
                            s.bot_settings.notice = None;
                        },
                        option { value: "{DEFAULT_PROFILE}", selected: profile_value == DEFAULT_PROFILE, "Default profile" }
                        for profile in profiles.iter().filter(|p| !p.is_default && p.id != DEFAULT_PROFILE) {
                            option {
                                key: "{profile.id}",
                                value: "{profile.id}",
                                selected: profile_value == profile.id,
                                "{profile.name}"
                            }
                        }
                    }
                    span { class: "hint", "Each profile keeps its own logins. Add more on the Browser page." }
                }
                div { class: "field",
                    span { class: "field-label", "What it can do" }
                    CapabilityToggles { session, bot: id.clone() }
                    span { class: "hint", "Press Save to apply. The bot uses the new abilities from its next message." }
                }
                SkillsSection { session, id: id.clone() }
                AdvancedSection { session, id: id.clone() }
                LifecycleSection { session, id: id.clone() }
            }
            div { class: "settings-foot",
                div { class: "settings-status",
                    if let Some(message) = error {
                        span { class: "error-text", role: "alert", "{message}" }
                    } else if let Some(message) = notice {
                        span { class: "settings-saved", role: "status",
                            Icon { name: "check", size: 14 }
                            "{message}"
                        }
                    }
                }
                button {
                    class: "btn ghost",
                    r#type: "button",
                    onclick: move |_| session.write().cancel_bot_settings(),
                    "Cancel"
                }
                button {
                    class: "btn primary",
                    r#type: "submit",
                    disabled: !can_save,
                    "aria-label": "Save settings for {save_id}",
                    if saving { "Saving…" } else { "Save" }
                }
            }
        }
    }
}

/// Skills the bot carries, with a filter and an "add" list.
#[component]
fn SkillsSection(session: Signal<Session>, id: String) -> Element {
    let mut session = session;
    let mut add_query = use_signal(String::new);
    let mut new_id = use_signal(String::new);
    let mut new_desc = use_signal(String::new);
    let mut new_body = use_signal(String::new);
    let attached = session
        .read()
        .state
        .bot_details
        .get(&id)
        .map(|detail| detail.skills.clone())
        .or_else(|| {
            session
                .read()
                .state
                .listed_bots
                .iter()
                .find(|bot| bot.id == id)
                .map(|bot| bot.skills.clone())
        })
        .unwrap_or_default();
    let needle = add_query().trim().to_lowercase();
    let addable: Vec<(String, String, String)> = session
        .read()
        .state
        .listed_skills
        .iter()
        .filter(|skill| valid_skill_id(&skill.id))
        .filter(|skill| !attached.iter().any(|s| s == &skill.id))
        .filter(|skill| {
            needle.is_empty()
                || skill.id.to_lowercase().contains(&needle)
                || skill.name.to_lowercase().contains(&needle)
        })
        .map(|skill| {
            (
                skill.id.clone(),
                skill.name.clone(),
                skill.description.clone(),
            )
        })
        .collect();
    let full = attached.len() >= MAX_BOT_SKILLS;
    rsx! {
        details { class: "advanced skills-box",
            summary { "Skills ({attached.len()})" }
            div { class: "advanced-body",
                if attached.is_empty() {
                    p { class: "hint", "No skills yet." }
                } else {
                    div { class: "chips",
                        for skill in attached.clone() {
                            {
                                let bot_id = id.clone();
                                let all = attached.clone();
                                let this = skill.clone();
                                rsx! {
                                    span { key: "{skill}", class: "chip accent",
                                        "{skill}"
                                        button {
                                            class: "team-chip remove",
                                            title: "Remove {skill}",
                                            r#type: "button",
                                            onclick: move |_| {
                                                let mut next: Vec<String> =
                                                    all.iter().filter(|s| **s != this).cloned().collect();
                                                next.retain(|s| valid_skill_id(s));
                                                session.write().save_bot_config(
                                                    bot_id.clone(),
                                                    BotConfigUpdate { skills: Some(next), ..Default::default() },
                                                );
                                            },
                                            Icon { name: "x", size: 12 }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
                input {
                    class: "input",
                    placeholder: "Add a skill…",
                    value: "{add_query}",
                    oninput: move |event| add_query.set(event.value()),
                }
                if !addable.is_empty() {
                    div { class: "pop-list skill-pop",
                        for (skill_id, name, description) in addable {
                            {
                                let bot_id = id.clone();
                                let all = attached.clone();
                                let pick = skill_id.clone();
                                rsx! {
                                    button {
                                        key: "{skill_id}",
                                        class: "pop-item",
                                        r#type: "button",
                                        disabled: full,
                                        onclick: move |_| {
                                            if full || !valid_skill_id(&pick) || all.contains(&pick) {
                                                return;
                                            }
                                            let mut next = all.clone();
                                            next.push(pick.clone());
                                            session.write().save_bot_config(
                                                bot_id.clone(),
                                                BotConfigUpdate { skills: Some(next), ..Default::default() },
                                            );
                                            add_query.set(String::new());
                                        },
                                        span { "{name}" }
                                        if !description.trim().is_empty() {
                                            span { class: "caption", "{description}" }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
                if full {
                    p { class: "hint", "A bot can carry up to {MAX_BOT_SKILLS} skills." }
                }
                details { class: "advanced",
                    summary { "Write a new skill" }
                    div { class: "advanced-body",
                        input {
                            class: "input",
                            placeholder: "Skill name, like venue-booking",
                            value: "{new_id}",
                            oninput: move |event| new_id.set(event.value()),
                        }
                        input {
                            class: "input",
                            placeholder: "One line about what it does",
                            value: "{new_desc}",
                            oninput: move |event| new_desc.set(event.value()),
                        }
                        textarea {
                            class: "textarea",
                            placeholder: "The steps, in your own words (optional)",
                            value: "{new_body}",
                            oninput: move |event| new_body.set(event.value()),
                        }
                        button {
                            class: "btn sm",
                            r#type: "button",
                            onclick: move |_| {
                                session.write().create_skill(new_id(), new_desc(), new_body());
                                new_id.set(String::new());
                                new_desc.set(String::new());
                                new_body.set(String::new());
                            },
                            "Create skill"
                        }
                    }
                }
            }
        }
    }
}

/// Switch the bot off or on, or delete it (after a confirm step). Never shown
/// for the orchestrator.
#[component]
fn LifecycleSection(session: Signal<Session>, id: String) -> Element {
    let mut session = session;
    let (bot, confirming, busy) = {
        let s = session.read();
        (
            s.state.listed_bots.iter().find(|bot| bot.id == id).cloned(),
            s.bot_settings.confirm_delete.as_deref() == Some(id.as_str()),
            s.bot_settings.lifecycle_busy,
        )
    };
    let Some(bot) = bot else {
        return rsx! {};
    };
    if !can_remove_bot(&bot.id, &bot.role) {
        return rsx! {};
    }
    let enabled = bot.enabled;
    let name = session.read().bot_display_name(&id);
    rsx! {
        div { class: "field lifecycle",
            span { class: "field-label", "Switch off or delete" }
            if confirming {
                div { class: "vault-confirm", role: "group", "aria-label": "Confirm delete",
                    span { class: "caption", "Delete {name}? Its chats and memories go too." }
                    button {
                        class: "btn danger sm",
                        r#type: "button",
                        disabled: busy,
                        onclick: move |_| session.write().confirm_delete_bot(),
                        "Delete"
                    }
                    button {
                        class: "btn ghost sm",
                        r#type: "button",
                        onclick: move |_| session.write().cancel_delete_bot(),
                        "Keep"
                    }
                }
            } else {
                div { class: "row",
                    button {
                        class: "btn sm",
                        r#type: "button",
                        disabled: busy,
                        onclick: {
                            let id = id.clone();
                            move |_| session.write().set_bot_enabled(id.clone(), !enabled)
                        },
                        if enabled { "Switch off" } else { "Switch on" }
                    }
                    button {
                        class: "btn ghost sm danger-text",
                        r#type: "button",
                        disabled: busy,
                        onclick: {
                            let id = id.clone();
                            move |_| session.write().ask_delete_bot(id.clone())
                        },
                        "Delete bot"
                    }
                }
            }
        }
    }
}

/// Ids, routing and the like: read-only, for the curious.
#[component]
fn AdvancedSection(session: Signal<Session>, id: String) -> Element {
    let mut session = session;
    let (detail, bot) = {
        let s = session.read();
        (
            s.state.bot_details.get(&id).cloned(),
            s.state.listed_bots.iter().find(|bot| bot.id == id).cloned(),
        )
    };
    let enabled = bot.as_ref().is_some_and(|bot| bot.enabled);
    let live = bot.as_ref().is_some_and(|bot| bot.live);
    let is_main = bot.as_ref().is_some_and(|bot| bot.role == "orchestrator");
    rsx! {
        details { class: "advanced",
            summary { "Advanced" }
            div { class: "advanced-body",
                div { class: "kv-row",
                    span { class: "kv-key", "Id" }
                    span { class: "kv-val mono", "{id}" }
                }
                div { class: "kv-row",
                    span { class: "kv-key", "Status" }
                    span { class: "kv-val",
                        if !enabled {
                            "Switched off"
                        } else if live {
                            "Running"
                        } else {
                            "Waiting for a server restart"
                        }
                    }
                }
                if let Some(detail) = detail {
                    div { class: "kv-row",
                        span { class: "kv-key", "Provider" }
                        span { class: "kv-val", "{human_provider(&detail.provider)}" }
                    }
                    div { class: "kv-row",
                        span { class: "kv-key", "Model in use" }
                        span { class: "kv-val", "{human_model(&detail.model)}" }
                    }
                    if let Some(provider) = detail.fallback_provider.clone() {
                        div { class: "kv-row",
                            span { class: "kv-key", "Backup" }
                            span { class: "kv-val",
                                "{human_provider(&provider)} · {human_model(&detail.fallback_model.clone().unwrap_or_default())}"
                            }
                        }
                    }
                    if !detail.routing_classes.is_empty() {
                        div { class: "chips",
                            for class in detail.routing_classes.clone() {
                                span { key: "{class}", class: "chip muted", "{class}" }
                            }
                        }
                    }
                    div { class: "kv-row",
                        span { class: "kv-key", "Memories" }
                        span { class: "kv-val", "{detail.memory_count}" }
                    }
                    div { class: "kv-row",
                        span { class: "kv-key", "Files made" }
                        span { class: "kv-val", "{detail.artifact_count}" }
                    }
                }
                div { class: "row",
                    if !enabled && !is_main {
                        button {
                            class: "btn sm",
                            r#type: "button",
                            onclick: {
                                let id = id.clone();
                                move |_| session.write().open_onboarding(id.clone())
                            },
                            "Finish setting up"
                        }
                    }
                    button {
                        class: "btn ghost sm",
                        r#type: "button",
                        onclick: move |_| session.write().open_section(Section::Memory),
                        "Open memory"
                    }
                }
            }
        }
    }
}

#[component]
fn FilesTab(session: Signal<Session>) -> Element {
    let mut session = session;
    let (library, loaded, loading, error, downloading) = {
        let s = session.read();
        (
            s.state.library.clone(),
            s.state.library_loaded,
            s.state.library_loading,
            s.state.library_error.clone(),
            s.downloading,
        )
    };
    rsx! {
        div { class: "settings-scroll",
            PinnedCard { session }
            div { class: "card",
                div { class: "card-head",
                    h3 { class: "card-title", "Files from this chat" }
                    if loading || downloading {
                        span { class: "spinner", title: "Working" }
                    }
                }
                if let Some(error) = error {
                    p { class: "hint", "{error}" }
                }
                if !loaded {
                    p { class: "hint", "Files load together with the chat." }
                } else if library.is_empty() {
                    p { class: "hint", "No files yet. Decks and documents the bots make will show up here." }
                } else {
                    div { class: "roster",
                        for entry in library {
                            div { key: "{entry.action_id}", class: "roster-row",
                                div { class: "thread-copy",
                                    span { class: "thread-title",
                                        "{entry.title.clone().or(entry.filename.clone()).unwrap_or_else(|| entry.hash.clone())}"
                                    }
                                    span { class: "thread-meta",
                                        "{humanize_label(&entry.tool)} · {stamp(&entry.created_at)}"
                                    }
                                }
                                button {
                                    class: "btn icon",
                                    title: "Open",
                                    disabled: downloading,
                                    onclick: {
                                        let hash = entry.hash.clone();
                                        move |_| session.write().open_artifact(hash.clone())
                                    },
                                    Icon { name: "external", size: 15 }
                                }
                                button {
                                    class: "btn icon",
                                    title: "Show in Explorer",
                                    disabled: downloading,
                                    onclick: {
                                        let hash = entry.hash.clone();
                                        move |_| session.write().reveal_artifact(hash.clone())
                                    },
                                    Icon { name: "folder", size: 15 }
                                }
                                button {
                                    class: "btn icon",
                                    title: "Save to Downloads",
                                    disabled: downloading,
                                    onclick: {
                                        let hash = entry.hash.clone();
                                        move |_| session.write().download_artifact(hash.clone())
                                    },
                                    Icon { name: "download", size: 15 }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

/// Chats the owner pinned, one click away.
#[component]
fn PinnedCard(session: Signal<Session>) -> Element {
    let mut session = session;
    let rows: Vec<(String, String, String, bool)> = {
        let s = session.read();
        s.pins
            .iter()
            .map(|pin| {
                (
                    pin.clone(),
                    s.pin_label(pin),
                    s.pin_avatar(pin),
                    s.pin_is_bot(pin),
                )
            })
            .collect()
    };
    if rows.is_empty() {
        return rsx! {};
    }
    rsx! {
        div { class: "card",
            div { class: "card-head",
                h3 { class: "card-title", "Pinned chats" }
            }
            div { class: "pins",
                for (pin, label, avatar, is_bot) in rows {
                    div {
                        key: "{pin}",
                        class: "pin-row",
                        title: "Open {label}",
                        onclick: {
                            let pin = pin.clone();
                            move |_| session.write().open_pin(pin.clone())
                        },
                        if is_bot {
                            span { class: "avatar sm bot", style: avatar_style(&pin), "{avatar}" }
                        } else {
                            span { class: "avatar sm user", "{avatar}" }
                        }
                        span { class: "thread-title", "{label}" }
                        PinToggle { session, id: pin.clone(), pinned: true }
                    }
                }
            }
        }
    }
}

/// A group: who is in it, and its standing rules.
#[component]
fn MembersTab(session: Signal<Session>, overlay: Signal<Overlay>) -> Element {
    let mut session = session;
    let mut overlay = overlay;
    let conversation = session.read().conversation.clone();
    let is_room = teams::is_room(&conversation);
    let rules = session.read().team_rules_draft.clone();
    let (members, loaded, loading) = {
        let s = session.read();
        (
            s.state
                .team
                .as_ref()
                .map(teams::member_ids)
                .unwrap_or_default(),
            s.state.team_loaded,
            s.state.team_loading,
        )
    };
    let rows: Vec<(String, String, String, bool)> = {
        let s = session.read();
        members
            .iter()
            .filter_map(|id| {
                s.state
                    .listed_bots
                    .iter()
                    .find(|bot| &bot.id == id)
                    .map(|bot| {
                        (
                            id.clone(),
                            s.bot_display_name(id),
                            s.bot_avatar(id),
                            bot.enabled && bot.live,
                        )
                    })
            })
            .collect()
    };
    rsx! {
        div { class: "settings-scroll",
            div { class: "card",
                div { class: "card-head",
                    h3 { class: "card-title", "Who is in this chat" }
                    button {
                        class: "btn ghost sm",
                        onclick: move |_| {
                            session.write().refresh_team();
                            overlay.set(Overlay::Team);
                        },
                        "Change"
                    }
                }
                if is_room && !loaded && !loading {
                    button { class: "btn ghost sm", onclick: move |_| session.write().refresh_team(), "Load members" }
                } else if rows.is_empty() {
                    p { class: "hint", "Nobody is picked, so every bot that is switched on can answer." }
                } else {
                    div { class: "roster",
                        for (id, name, avatar, running) in rows {
                            div { key: "{id}", class: "roster-row",
                                span { class: "avatar sm bot", style: avatar_style(&id), "{avatar}" }
                                span { class: "thread-title", "{name}" }
                                span { class: if running { "chip ok" } else { "chip warn" },
                                    if running { "ready" } else { "off" }
                                }
                            }
                        }
                    }
                }
            }
            if is_room {
                div { class: "card",
                    div { class: "card-head",
                        h3 { class: "card-title", "Group rules" }
                    }
                    p { class: "hint", "Rules every bot in this group reads before it answers." }
                    textarea {
                        class: "textarea",
                        value: "{rules}",
                        placeholder: "For example: answer in English, name your sources, keep it short.",
                        oninput: move |event| session.write().team_rules_draft = event.value(),
                    }
                    div { class: "row",
                        button {
                            class: "btn primary sm",
                            onclick: move |_| session.write().save_team_rules(),
                            "Save rules"
                        }
                    }
                }
            }
        }
    }
}
