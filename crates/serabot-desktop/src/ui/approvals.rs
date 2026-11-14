use super::*;

fn short_hash(hash: &str) -> String {
    let chars: String = hash.chars().take(12).collect();
    if hash.chars().count() > 12 {
        format!("{chars}\u{2026}")
    } else {
        chars
    }
}

fn pulse_busy(mut busy: Signal<Option<String>>, id: String) {
    busy.set(Some(id.clone()));
    spawn(async move {
        let mut done = dioxus::document::eval(CONFIRM_RESET_JS);
        let _ = done.recv::<bool>().await;
        if busy.read().as_deref() == Some(id.as_str()) {
            busy.set(None);
        }
    });
}

#[component]
pub(super) fn ApprovalsView(session: Signal<Session>) -> Element {
    let mut filter = use_signal(String::new);
    let mut confirm_always = use_signal(|| None::<String>);
    let busy_id = use_signal(|| None::<String>);

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
    let grants = session.read().state.standing_grants.grants.clone();
    let grants_loaded = session.read().state.standing_grants_loaded;
    let active_grants: Vec<_> = grants.iter().filter(|g| g.active).cloned().collect();

    let needle = filter().trim().to_lowercase();
    let filtered_approvals: Vec<_> = approvals
        .into_iter()
        .filter(|(_, tool, summary, _)| {
            needle.is_empty()
                || tool.to_lowercase().contains(&needle)
                || summary.to_lowercase().contains(&needle)
        })
        .collect();
    let filtered_grants: Vec<_> = active_grants
        .into_iter()
        .filter(|grant| {
            needle.is_empty()
                || grant.tool.to_lowercase().contains(&needle)
                || grant
                    .destination
                    .as_deref()
                    .is_some_and(|dest| dest.to_lowercase().contains(&needle))
                || grant.canonical_hash.to_lowercase().contains(&needle)
        })
        .collect();

    let approvals_empty = filtered_approvals.is_empty();
    let grants_empty = filtered_grants.is_empty();
    let confirm_id = confirm_always();
    let active_busy = busy_id();
    let has_any = !approvals_loaded || !approvals_empty || !grants_empty || !needle.is_empty();

    rsx! {
        div { class: "pane",
            div { class: "pane-head",
                div {
                    h1 { class: "pane-title", "Approvals" }
                    p { class: "muted", "Each decision binds the exact payload, destination, hash, policy and expiry." }
                }
                button {
                    class: "btn ghost",
                    r#type: "button",
                    onclick: move |_| session.write().refresh_approvals(),
                    Icon { name: "refresh", size: 14 }
                    " Refresh list"
                }
            }
            div { class: "pane-body",
                if has_any {
                    div { class: "approval-filter",
                        input {
                            class: "input",
                            r#type: "search",
                            "aria-label": "Filter approvals and standing grants",
                            placeholder: "Filter by tool, summary, or destination\u{2026}",
                            value: "{filter}",
                            oninput: move |event| filter.set(event.value()),
                            onkeydown: move |event: KeyboardEvent| {
                                if event.key() == Key::Escape && !filter.read().is_empty() {
                                    event.prevent_default();
                                    filter.set(String::new());
                                }
                            },
                        }
                        if !needle.is_empty() {
                            button {
                                class: "btn ghost sm",
                                r#type: "button",
                                onclick: move |_| filter.set(String::new()),
                                "Clear"
                            }
                        }
                    }
                }

                section { class: "ap-section", "aria-label": "Pending approvals",
                    div { class: "row",
                        h2 { class: "section-title", "Waiting for you" }
                        if approvals_loaded && !approvals_empty {
                            span { class: "chip accent", "{filtered_approvals.len()}" }
                        }
                    }
                    if !approvals_loaded {
                        div { class: "ap-list",
                            div { class: "ap-row skeleton block", " " }
                        }
                    } else if approvals_empty {
                        div { class: "empty",
                            div { class: "empty-icon", Icon { name: "shield", size: 20 } }
                            h3 { class: "empty-title",
                                if needle.is_empty() { "Nothing waiting for approval" } else { "No matching approvals" }
                            }
                            p { class: "empty-text",
                                if needle.is_empty() {
                                    "Requests that need your OK appear here first."
                                } else {
                                    "Try clearing the filter to see all pending requests."
                                }
                            }
                        }
                    } else {
                        div { class: "ap-list", role: "list",
                            for (id, tool, summary, expires) in filtered_approvals {
                                {
                                    let row_busy = active_busy.as_deref() == Some(id.as_str());
                                    let confirming = confirm_id.as_deref() == Some(id.as_str());
                                    let exp_label = stamp(&expires);
                                    rsx! {
                                        div { class: "ap-row", key: "{id}", role: "listitem",
                                            span { class: "chip accent ap-tool", "{tool}" }
                                            div { class: "ap-body",
                                                p { class: "ap-summary", title: "{summary}", "{summary}" }
                                                span { class: "tiny muted", title: "{expires}", "Expires {exp_label}" }
                                            }
                                            div { class: "ap-actions",
                                                button {
                                                    class: "btn primary sm",
                                                    r#type: "button",
                                                    disabled: row_busy,
                                                    onclick: {
                                                        let id = id.clone();
                                                        move |_| {
                                                            confirm_always.set(None);
                                                            pulse_busy(busy_id, id.clone());
                                                            session.write().decide(id.clone(), ApprovalDecision::Once);
                                                        }
                                                    },
                                                    "Approve once"
                                                }
                                                if confirming {
                                                    div { class: "conv-confirm",
                                                        button {
                                                            class: "btn primary sm",
                                                            r#type: "button",
                                                            disabled: row_busy,
                                                            onclick: {
                                                                let id = id.clone();
                                                                move |_| {
                                                                    confirm_always.set(None);
                                                                    pulse_busy(busy_id, id.clone());
                                                                    session.write().decide(id.clone(), ApprovalDecision::Always);
                                                                }
                                                            },
                                                            "Confirm Always"
                                                        }
                                                        button {
                                                            class: "btn ghost sm",
                                                            r#type: "button",
                                                            disabled: row_busy,
                                                            onclick: move |_| confirm_always.set(None),
                                                            "Cancel"
                                                        }
                                                    }
                                                } else {
                                                    button {
                                                        class: "btn ghost sm",
                                                        r#type: "button",
                                                        disabled: row_busy,
                                                        onclick: {
                                                            let id = id.clone();
                                                            move |_| confirm_always.set(Some(id.clone()))
                                                        },
                                                        "Always"
                                                    }
                                                }
                                                button {
                                                    class: "btn danger sm",
                                                    r#type: "button",
                                                    disabled: row_busy,
                                                    onclick: {
                                                        let id = id.clone();
                                                        move |_| {
                                                            confirm_always.set(None);
                                                            pulse_busy(busy_id, id.clone());
                                                            session.write().decide(id.clone(), ApprovalDecision::Deny);
                                                        }
                                                    },
                                                    "Deny"
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }

                if grants_loaded {
                    section { class: "card-section",
                        div { class: "row",
                            h2 { class: "section-title", "Standing grants" }
                            if !grants_empty {
                                span { class: "chip", "{filtered_grants.len()}" }
                            }
                        }
                        p { class: "muted", "Tools you chose Always for. Revoke to require approval again." }
                        if grants_empty {
                            p { class: "muted",
                                if needle.is_empty() { "No active standing grants." } else { "No standing grants match this filter." }
                            }
                        } else {
                            div { class: "card-grid",
                                for grant in filtered_grants {
                                    {
                                        let row_busy = active_busy.as_deref() == Some(grant.id.as_str());
                                        let since = stamp(&grant.created_at);
                                        let hash_short = short_hash(&grant.canonical_hash);
                                        rsx! {
                                            div { class: "card", key: "{grant.id}",
                                                div { class: "card-head",
                                                    span { class: "chip accent", "{grant.tool}" }
                                                    span { class: "tiny muted", title: "{grant.created_at}", "since {since}" }
                                                }
                                                if let Some(dest) = grant.destination.as_deref() {
                                                    p { class: "card-sub", "Destination: {dest}" }
                                                }
                                                p { class: "tiny muted",
                                                    "Hash "
                                                    span { class: "chip mono", title: "{grant.canonical_hash}", "{hash_short}" }
                                                }
                                                div { class: "card-actions",
                                                    button {
                                                        class: "btn danger sm",
                                                        r#type: "button",
                                                        disabled: row_busy,
                                                        onclick: {
                                                            let id = grant.id.clone();
                                                            move |_| {
                                                                pulse_busy(busy_id, id.clone());
                                                                session.write().revoke_standing_grant(id.clone());
                                                            }
                                                        },
                                                        if row_busy { "Revoking\u{2026}" } else { "Revoke" }
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
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_hash_truncates_long_hashes() {
        assert_eq!(short_hash("abcdef123456"), "abcdef123456");
        assert_eq!(short_hash("abcdef1234567890"), "abcdef123456\u{2026}");
    }
}
