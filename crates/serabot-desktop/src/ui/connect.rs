use super::*;

#[component]
pub(super) fn ConnectView(session: Signal<Session>) -> Element {
    let mut copied_link = use_signal(|| false);
    let server = session.read().server.clone();
    let error = session.read().connect_error.clone();
    let connecting = session.read().connecting;
    let can_retry = session.read().has_saved_session();
    let theme = session.read().theme;
    let version = env!("CARGO_PKG_VERSION");
    let phase = session.read().sign_in.phase;
    let busy = session.read().sign_in.busy() || connecting;
    let sign_in_error = session.read().sign_in.error.clone();
    let sign_in_notice = session.read().sign_in.notice.clone();
    // The link is shown only when the browser did not open; then the user can paste it.
    let sign_in_link = {
        let sign_in = &session.read().sign_in;
        (phase == SignInPhase::Waiting && !sign_in.browser_opened)
            .then(|| sign_in.link.clone())
            .flatten()
    };
    let invite_code = session.read().sign_in.invite_code.clone();
    let show_invite = session.read().sign_in.show_invite;
    let server_empty = server.trim().is_empty();
    // Same check the session runs on connect, shown as the user types.
    let server_invalid =
        !server_empty && serabot_desktop::validate_endpoint(server.trim()).is_err();
    let invite_trimmed = invite_code.trim();
    let invite_invalid = show_invite
        && !invite_trimmed.is_empty()
        && (invite_trimmed.chars().count() > 128 || invite_trimmed.chars().any(char::is_control));
    let can_sign_in = !busy && !server_empty && !server_invalid && !invite_invalid;
    rsx! {
        div { class: "connect",
            div { class: "connect-card",
                div { class: "connect-badge",
                    span { "SeraBot Desktop" }
                    span { "\u{00b7}" }
                    span { "v{version}" }
                }
                div { class: "brand brand-lg", "S" }
                h1 { "SeraBot" }
                p { class: "muted", "Sign in to your private bot workspace" }
                div { class: "field",
                    label { class: "field-label", r#for: "server-input", "Server" }
                    input {
                        class: if server_invalid { "input invalid" } else { "input" },
                        id: "server-input",
                        value: "{server}",
                        placeholder: "https://bot.seradyx.tech",
                        disabled: busy,
                        "aria-invalid": if server_invalid { "true" } else { "false" },
                        "aria-describedby": if server_invalid { Some("server-error") } else { None },
                        oninput: move |event| session.write().server = event.value(),
                        onkeydown: move |event: KeyboardEvent| {
                            if event.key() == Key::Enter && can_sign_in {
                                event.prevent_default();
                                session.write().start_sign_in();
                            }
                        },
                    }
                    if server_invalid {
                        p { id: "server-error", class: "field-error", role: "alert",
                            "Use an https:// address. Plain http works only for localhost."
                        }
                    }
                }
                if connecting && phase == SignInPhase::Idle {
                    div { class: "row", role: "status",
                        span { class: "spinner", title: "Checking your saved session" }
                        p { class: "tiny", "Checking your saved session…" }
                    }
                }
                if phase == SignInPhase::Waiting {
                    div { class: "banner info", role: "status",
                        span { class: "spinner", title: "Waiting for the browser" }
                        span { "Finish signing in in your browser, then come back here." }
                    }
                }
                if let Some(link) = sign_in_link {
                    {
                        let copy_text = link.clone();
                        rsx! {
                            div { class: "field",
                                label { class: "field-label", r#for: "sign-in-link",
                                    "Your browser did not open. Open this link to finish signing in:"
                                }
                                div { class: "copy-row",
                                    input {
                                        class: "input",
                                        id: "sign-in-link",
                                        readonly: true,
                                        value: "{link}",
                                    }
                                    button {
                                        class: "btn ghost sm",
                                        r#type: "button",
                                        onclick: move |_| {
                                            let _ = dioxus::document::eval(&copy_script(&copy_text));
                                            copied_link.set(true);
                                            spawn(async move {
                                                let mut done = dioxus::document::eval(CONFIRM_RESET_JS);
                                                let _ = done.recv::<bool>().await;
                                                copied_link.set(false);
                                            });
                                        },
                                        if copied_link() { "Copied" } else { "Copy link" }
                                    }
                                }
                            }
                        }
                    }
                }
                if let Some(message) = sign_in_notice {
                    div { class: "banner info", role: "status", span { "{message}" } }
                }
                if let Some(message) = sign_in_error {
                    div { class: "banner err", role: "alert", span { "{message}" } }
                }
                if let Some(message) = error {
                    div { class: "banner err", role: "alert", span { "{message}" } }
                }
                if show_invite {
                    div { class: "field",
                        div { class: "row",
                            label { class: "field-label", r#for: "invite-code-input", "Invite code" }
                            if !busy {
                                button {
                                    class: "btn ghost sm",
                                    r#type: "button",
                                    onclick: move |_| {
                                        let mut s = session.write();
                                        s.sign_in.invite_code.clear();
                                        s.sign_in.show_invite = false;
                                    },
                                    "Remove"
                                }
                            }
                        }
                        input {
                            id: "invite-code-input",
                            class: if invite_invalid { "input invalid" } else { "input" },
                            value: "{invite_code}",
                            placeholder: "Only needed the first time",
                            disabled: busy,
                            "aria-invalid": if invite_invalid { "true" } else { "false" },
                            "aria-describedby": if invite_invalid { Some("invite-error") } else { None },
                            oninput: move |event| session.write().sign_in.invite_code = event.value(),
                            onkeydown: move |event: KeyboardEvent| {
                                if event.key() == Key::Enter && can_sign_in {
                                    event.prevent_default();
                                    session.write().start_sign_in();
                                }
                            },
                        }
                        if invite_invalid {
                            p { id: "invite-error", class: "field-error", role: "alert",
                                "Invite codes must be 128 characters or fewer without control characters."
                            }
                        }
                    }
                }
                div { class: "row",
                    button {
                        class: "btn primary",
                        r#type: "button",
                        disabled: !can_sign_in,
                        title: "Open the Microsoft sign-in page in your browser",
                        onclick: move |_| session.write().start_sign_in(),
                        match phase {
                            SignInPhase::Starting => "Opening browser…",
                            SignInPhase::Waiting => "Waiting for browser…",
                            SignInPhase::Idle => "Sign in with Microsoft",
                        }
                    }
                    if phase != SignInPhase::Idle {
                        button {
                            class: "btn ghost",
                            r#type: "button",
                            onclick: move |_| session.write().cancel_sign_in(),
                            "Cancel"
                        }
                    }
                    if can_retry && !connecting && phase == SignInPhase::Idle {
                        button {
                            class: "btn ghost",
                            r#type: "button",
                            onclick: move |_| session.write().connect_now(),
                            "Try again"
                        }
                    }
                }
                if server_empty && !busy {
                    p { class: "tiny", "Enter the server address to sign in." }
                }
                if !show_invite && !busy {
                    button {
                        class: "btn ghost sm",
                        r#type: "button",
                        onclick: move |_| session.write().sign_in.show_invite = true,
                        "I have an invite code"
                    }
                }
                div { class: "row",
                    button {
                        class: "btn ghost sm",
                        r#type: "button",
                        onclick: move |_| {
                            let next = session.read().theme.toggled();
                            session.write().set_theme(next);
                        },
                        if theme == Theme::Dark { "Light theme" } else { "Dark theme" }
                    }
                }
                p { class: "tiny",
                    "Your session is kept in Windows Credential Manager. Only the server address and theme are written to settings.json."
                }
            }
            p { class: "tiny", "v{version}" }
        }
    }
}
