//! Small shared widgets: the status chip and the secret field.

use dioxus::prelude::*;
use serabot_desktop::theme::{Level, Tone};

use crate::ui::Icon;

/// A status chip: colour, icon and word together.
#[component]
pub fn StatusChip(tone: Tone, text: String) -> Element {
    rsx! {
        span { class: "{tone.chip_class()}",
            if let Some(icon) = tone.icon() {
                Icon { name: icon.to_owned(), size: 14 }
            }
            span { "{text}" }
        }
    }
}

/// The chip for a limit level ("OK", "Near limit", "At limit").
#[component]
pub fn LevelChip(level: Level) -> Element {
    rsx! {
        StatusChip { tone: level.tone(), text: level.label().to_owned() }
    }
}

/// A masked input with a Show/Hide toggle. The toggle only reveals what the
/// user typed in this field; saved secrets are never sent back to the app.
/// The field hides itself again whenever it is emptied (for example after Save).
#[component]
pub fn SecretField(
    value: Signal<String>,
    label: String,
    placeholder: String,
    #[props(default = "input".to_owned())] class: String,
    #[props(default = false)] disabled: bool,
    #[props(default = false)] invalid: bool,
    #[props(default)] on_submit: Option<EventHandler<()>>,
) -> Element {
    let mut value = value;
    let mut shown = use_signal(|| false);
    use_effect(move || {
        if value.read().is_empty() {
            shown.set(false);
        }
    });
    let revealed = shown();
    let icon = if revealed { "eye-off" } else { "eye" };
    rsx! {
        div { class: "secret-field",
            input {
                class: "{class}",
                r#type: if revealed { "text" } else { "password" },
                autocomplete: "off",
                spellcheck: false,
                disabled,
                "aria-label": "{label}",
                "aria-invalid": if invalid { "true" } else { "false" },
                placeholder: "{placeholder}",
                value: "{value}",
                oninput: move |event| value.set(event.value()),
                onkeydown: move |event: KeyboardEvent| {
                    if event.key() == Key::Enter
                        && !disabled
                        && !event.is_composing()
                        && let Some(handler) = on_submit
                    {
                        event.prevent_default();
                        handler.call(());
                    }
                },
            }
            button {
                class: "btn ghost sm secret-toggle",
                r#type: "button",
                disabled,
                "aria-pressed": if revealed { "true" } else { "false" },
                "aria-label": if revealed { "Hide what I typed" } else { "Show what I typed" },
                title: if revealed { "Hide" } else { "Show" },
                onclick: move |_| shown.set(!revealed),
                Icon { name: icon.to_owned(), size: 16 }
            }
        }
    }
}
