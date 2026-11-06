//! Right-hand dock (design B, contract section 3): the live server browser.
//!
//! The dock is always mounted so it can slide in from, and leave towards, the
//! right edge. `PanelState` (see `panel.rs`) polls `GET /v1/browser/sessions` every two
//! seconds while the window is focused, flips the dock open while the acting
//! user has an active session, and polls the screenshot at most twice a
//! second while the dock is visible. While the user holds control, clicks,
//! the wheel and typing on the picture go to `/v1/browser/input`.
//!
//! Pointer and keyboard events are collected by a small script (it needs the
//! rendered size of the picture to scale clicks to screenshot pixels) that
//! reports to the coroutine below; the network calls run on the panel worker.

use crate::ui::Icon;
use dioxus::prelude::*;
use serabot_desktop::panel::address_to_url;
use serabot_desktop::session::Session;
use serabot_desktop::{BrowserController, BrowserInput};

/// Reports focus changes and, while `#dock-stage[data-control=user]`, the
/// user's clicks (scaled to screenshot pixels), wheel, keys and pastes.
const DOCK_JS: &str = r#"
(function () {
  const out = (m) => dioxus.send(m);
  const stage = () => document.getElementById('dock-stage');
  const mine = () => { const s = stage(); return s && s.dataset.control === 'user' ? s : null; };
  const reportFocus = () => out({ t: 'focus', v: document.hasFocus() && !document.hidden });
  window.addEventListener('focus', reportFocus);
  window.addEventListener('blur', reportFocus);
  document.addEventListener('visibilitychange', reportFocus);

  let text = '';
  let textTimer = 0;
  const flushText = () => {
    clearTimeout(textTimer);
    textTimer = 0;
    if (text) { out({ t: 'text', text }); text = ''; }
  };
  let wheel = 0;
  let wheelTimer = 0;
  const flushWheel = () => {
    wheelTimer = 0;
    if (wheel) { out({ t: 'scroll', dy: Math.max(-5000, Math.min(5000, wheel)) }); wheel = 0; }
  };

  document.addEventListener('click', (e) => {
    const s = mine();
    if (!s || !s.contains(e.target)) return;
    s.focus({ preventScroll: true });
    const img = document.getElementById('dock-shot');
    if (!img || e.target !== img || !img.naturalWidth) return;
    const r = img.getBoundingClientRect();
    if (!r.width || !r.height) return;
    const x = (e.clientX - r.left) * img.naturalWidth / r.width;
    const y = (e.clientY - r.top) * img.naturalHeight / r.height;
    flushText();
    out({ t: 'click', x: Math.round(x * 10) / 10, y: Math.round(y * 10) / 10 });
  });
  document.addEventListener('wheel', (e) => {
    const s = mine();
    if (!s || !s.contains(e.target)) return;
    e.preventDefault();
    wheel += e.deltaMode === 1 ? e.deltaY * 16 : e.deltaY;
    if (!wheelTimer) wheelTimer = setTimeout(flushWheel, 80);
  }, { passive: false });
  document.addEventListener('keydown', (e) => {
    const s = mine();
    if (!s || document.activeElement !== s || e.isComposing) return;
    if (e.ctrlKey || e.metaKey || e.altKey) return;
    e.stopPropagation();
    if (e.key.length === 1) {
      e.preventDefault();
      text += e.key;
      if (text.length >= 200) flushText();
      else if (!textTimer) textTimer = setTimeout(flushText, 50);
      return;
    }
    const keys = ['Enter','Tab','Escape','Backspace','Delete','ArrowUp','ArrowDown',
      'ArrowLeft','ArrowRight','Home','End','PageUp','PageDown'];
    if (keys.includes(e.key)) {
      e.preventDefault();
      flushText();
      out({ t: 'key', key: e.key });
    }
  }, true);
  document.addEventListener('paste', (e) => {
    const s = mine();
    if (!s || document.activeElement !== s) return;
    const pasted = (e.clipboardData && e.clipboardData.getData('text')) || '';
    if (pasted) { e.preventDefault(); flushText(); out({ t: 'text', text: pasted.slice(0, 2000) }); }
  });
  reportFocus();
})();
"#;

fn field_f64(value: &serde_json::Value, key: &str) -> Option<f64> {
    value.get(key).and_then(serde_json::Value::as_f64)
}

fn field_str<'a>(value: &'a serde_json::Value, key: &str) -> Option<&'a str> {
    value.get(key).and_then(serde_json::Value::as_str)
}

/// The browser dock.
#[component]
pub fn BrowserDock(session: Signal<Session>) -> Element {
    let mut session = session;
    use_coroutine(move |_: UnboundedReceiver<()>| async move {
        let mut eval = dioxus::document::eval(DOCK_JS);
        while let Ok(message) = eval.recv::<serde_json::Value>().await {
            match field_str(&message, "t") {
                Some("focus") => {
                    let focused = message.get("v").and_then(serde_json::Value::as_bool);
                    if let Some(focused) = focused {
                        session.write().panel.set_focused(focused);
                    }
                }
                Some("click") => {
                    if let (Some(x), Some(y)) = (field_f64(&message, "x"), field_f64(&message, "y"))
                    {
                        session
                            .write()
                            .panel
                            .send_input(BrowserInput::Click { x, y });
                    }
                }
                Some("scroll") => {
                    if let Some(dy) = field_f64(&message, "dy") {
                        session
                            .write()
                            .panel
                            .send_input(BrowserInput::Scroll { dy });
                    }
                }
                Some("text") => {
                    if let Some(text) = field_str(&message, "text") {
                        session.write().panel.send_input(BrowserInput::Text {
                            text: text.chars().take(2000).collect(),
                        });
                    }
                }
                Some("key") => {
                    if let Some(key) = field_str(&message, "key") {
                        session.write().panel.send_input(BrowserInput::Key {
                            key: key.to_owned(),
                        });
                    }
                }
                _ => {}
            }
        }
    });

    let mut editing = use_signal(|| false);
    let mut draft = use_signal(String::new);
    let mut more_open = use_signal(|| false);

    let (open, in_control, busy, info, shot, error, bot_name) = {
        let s = session.read();
        let driving = s
            .panel
            .session(&s.panel.dock_profile)
            .and_then(|session| session.bot.clone())
            .or_else(|| {
                s.state
                    .listed_bots
                    .iter()
                    .find(|bot| bot.role == "orchestrator")
                    .map(|bot| bot.id.clone())
            });
        let bot_name = driving
            .map(|bot| s.bot_display_name(&bot))
            .unwrap_or_else(|| "The bot".to_owned());
        (
            s.panel.open(),
            s.panel.user_in_control(),
            s.panel.control_busy,
            s.panel.info.clone(),
            s.panel.dock_shot(),
            s.panel.browser_error.clone(),
            bot_name,
        )
    };
    let active = open && info.as_ref().is_some_and(|i| i.active);
    let url = info
        .as_ref()
        .and_then(|i| i.url.clone())
        .unwrap_or_default();
    let title = info
        .as_ref()
        .and_then(|i| i.title.clone())
        .filter(|t| !t.trim().is_empty());
    let controller = info.as_ref().map(|i| i.controller);
    let (pill_class, pill_label) = match (active, controller) {
        (true, Some(BrowserController::User)) => ("pill warn", "you"),
        (true, _) => ("pill ok", "bot"),
        _ => ("pill", "idle"),
    };
    let control_text = match (active, controller) {
        (true, Some(BrowserController::User)) => {
            "You're in control: click and type in the page. The bot is paused.".to_owned()
        }
        (true, _) => format!("{bot_name} is using the browser."),
        _ => "Nobody is using the browser.".to_owned(),
    };
    let has_shot = active && shot.is_some();
    let url_shown = if editing() { draft() } else { url.clone() };
    let url_text = if url.is_empty() {
        "No page open".to_owned()
    } else {
        url.clone()
    };

    rsx! {
        aside {
            class: if open { "dock-slot open" } else { "dock-slot" },
            "aria-label": "Browser",
            div { class: "dock",
                div { class: "dock-head",
                    strong { class: "dock-title", "Browser" }
                    span { class: "{pill_class}", role: "status", "{pill_label}" }
                }
                p { class: "dock-status caption", "aria-live": "polite", "{control_text}" }
                if in_control {
                    div { class: "dock-url editable",
                        Icon { name: "globe", size: 14 }
                        input {
                            class: "dock-address",
                            "aria-label": "Address",
                            placeholder: "Type an address and press Enter",
                            value: "{url_shown}",
                            onfocus: move |_| {
                                draft.set(session.read().panel.info.as_ref().and_then(|i| i.url.clone()).unwrap_or_default());
                                editing.set(true);
                            },
                            onblur: move |_| editing.set(false),
                            oninput: move |event| draft.set(event.value()),
                            onkeydown: move |event: KeyboardEvent| {
                                event.stop_propagation();
                                if event.key() == Key::Enter {
                                    match address_to_url(&draft()) {
                                        Some(url) => {
                                            session.write().panel.send_input(BrowserInput::Navigate { url });
                                            editing.set(false);
                                        }
                                        None => {
                                            session.write().panel.browser_error =
                                                Some("Enter a web address.".into());
                                        }
                                    }
                                }
                            },
                        }
                    }
                } else {
                    div { class: "dock-url", title: "{url_text}",
                        Icon { name: "globe", size: 14 }
                        span { class: "dock-url-text", "{url_text}" }
                    }
                }
                if let Some(title) = title.clone() {
                    div { class: "dock-title-line caption", title: "{title}", "{title}" }
                }
                div { class: if has_shot { "dock-view has-shot" } else { "dock-view" },
                    if active && shot.is_some() {
                        div {
                            id: "dock-stage",
                            class: if in_control { "dock-stage control" } else { "dock-stage" },
                            tabindex: if in_control { 0 } else { -1 },
                            "data-control": if in_control { "user" } else { "bot" },
                            "aria-label": if in_control { "Live page. You are in control: click, scroll and type here." } else { "Live page" },
                            img {
                                id: "dock-shot",
                                class: "dock-shot",
                                alt: "Live view of the page",
                                draggable: false,
                                src: shot.clone().unwrap_or_default(),
                            }
                        }
                    } else if active {
                        div { class: "empty",
                            div { class: "skeleton", style: "width: 100%; height: 100%; min-height: 180px;", " " }
                        }
                    } else {
                        div { class: "empty",
                            div { class: "empty-icon", Icon { name: "globe", size: 20 } }
                            h2 { class: "empty-title", "No browser session" }
                            p { class: "empty-text",
                                "When a bot opens a page in the server browser, it shows up here live."
                            }
                        }
                    }
                }
                if let Some(message) = error {
                    div { class: "error-text dock-error", role: "alert", "{message}" }
                }
                div { class: "dock-foot",
                    if in_control {
                        button {
                            class: "btn primary",
                            disabled: busy,
                            onclick: move |_| session.write().panel.hand_back(),
                            "Give control back"
                        }
                    } else {
                        button {
                            class: "btn primary",
                            disabled: !active || busy,
                            onclick: move |_| session.write().panel.take_over(),
                            "Take control"
                        }
                    }
                    button {
                        class: "btn ghost",
                        title: "Hide this panel. The bot keeps working.",
                        onclick: move |_| session.write().panel.hide(),
                        "Hide panel"
                    }
                    div { class: "menu-wrap dock-more",
                        button {
                            class: "icon-btn",
                            title: "More",
                            "aria-label": "More",
                            "aria-haspopup": "menu",
                            "aria-expanded": if more_open() { "true" } else { "false" },
                            onclick: move |_| more_open.set(!more_open()),
                            Icon { name: "dots", size: 16 }
                        }
                        div {
                            class: if more_open() { "menu top right open" } else { "menu top right" },
                            onkeydown: move |event: KeyboardEvent| {
                                if event.key() == Key::Escape {
                                    more_open.set(false);
                                }
                            },
                            button {
                                class: "menu-item danger",
                                disabled: !active,
                                onclick: move |_| {
                                    more_open.set(false);
                                    session.write().panel.close_browser();
                                },
                                "End browser session"
                            }
                        }
                    }
                }
            }
        }
    }
}
