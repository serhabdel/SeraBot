use super::*;

#[component]
pub(super) fn ApprovalsView(session: Signal<Session>) -> Element {
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
    let grants = session.read().state.standing_grants.grants.clone();
    let grants_loaded = session.read().state.standing_grants_loaded;
    let active_grants: Vec<_> = grants.iter().filter(|g| g.active).cloned().collect();
    rsx! {
        div { class: "pane",
            div { class: "pane-head",
                div {
                    h1 { class: "pane-title", "Approvals" }
                    p { class: "muted", "Each decision binds the exact payload, destination, hash, policy and expiry." }
                }
                button {
                    class: "btn ghost",
                    onclick: move |_| session.write().refresh_approvals(),
                    "Refresh list"
                }
            }
            div { class: "pane-body",
                if grants_loaded {
                    section { class: "card-section",
                        h2 { class: "section-title", "Standing grants" }
                        p { class: "muted", "Tools you chose Always for. Revoke to require approval again." }
                        if active_grants.is_empty() {
                            p { class: "muted", "No active standing grants." }
                        } else {
                            div { class: "card-grid",
                                for grant in active_grants {
                                    div { class: "card", key: "{grant.id}",
                                        div { class: "card-head",
                                            span { class: "chip accent", "{grant.tool}" }
                                            span { class: "tiny", "since {grant.created_at}" }
                                        }
                                        if let Some(dest) = grant.destination.as_deref() {
                                            p { class: "card-sub", "Destination: {dest}" }
                                        }
                                        p { class: "tiny muted", "Hash {grant.canonical_hash}" }
                                        div { class: "card-actions",
                                            button {
                                                class: "btn danger sm",
                                                onclick: {
                                                    let id = grant.id.clone();
                                                    move |_| session.write().revoke_standing_grant(id.clone())
                                                },
                                                "Revoke"
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
                div { class: "card-grid",
                    if !approvals_loaded {
                        div { class: "card",
                            div { class: "skeleton title" }
                            div { class: "skeleton block" }
                        }
                    }
                    for (id, tool, summary, expires) in approvals {
                        div { class: "card", key: "{id}",
                            div { class: "card-head",
                                span { class: "chip accent", "{tool}" }
                                span { class: "tiny", "expires {expires}" }
                            }
                            p { class: "card-sub", "{summary}" }
                            div { class: "card-actions",
                                button {
                                    class: "btn primary sm",
                                    onclick: {
                                        let id = id.clone();
                                        move |_| session.write().decide(id.clone(), ApprovalDecision::Once)
                                    },
                                    "Approve once"
                                }
                                button {
                                    class: "btn ghost sm",
                                    onclick: {
                                        let id = id.clone();
                                        move |_| session.write().decide(id.clone(), ApprovalDecision::Always)
                                    },
                                    "Always"
                                }
                                button {
                                    class: "btn danger sm",
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
                        div { class: "empty",
                            div { class: "empty-icon", Icon { name: "shield", size: 20 } }
                            h3 { class: "empty-title", "Nothing pending" }
                            p { class: "empty-text", "Requests that need your OK appear here." }
                        }
                    }
                }
            }
        }
    }
}
