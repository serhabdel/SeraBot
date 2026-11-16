//! What to tell the owner when a chat send fails, and how to keep the text
//! they typed. The message is a sentence. Raw JSON is never shown.

use crate::ApiError;
use crate::api::{RouteSwitch, RouteView, Selection};
use crate::route_chip::FallbackOffer;
use crate::route_editor::{Options, route_line};

/// The server's code when no model route in a bot's chain could answer.
pub const ALL_ROUTES_FAILED: &str = "all_routes_failed";

/// The line on the failed-turn card that offers "Retry on <fallback>".
pub const FAILED_TURN_WITH_FALLBACK: &str =
    "That turn failed. Your message is still above; send it again on the next route.";

/// The line on the failed-turn card when no fallback route is left.
pub const FAILED_TURN: &str = "That turn failed. Your message is still above.";

/// The button next to a send failure.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChatAction {
    /// Send the same text again.
    Retry,
    /// The event stream or the server is unreachable.
    Reconnect,
    /// A provider key needs attention.
    OpenKeys,
    /// Nothing to press; the sentence is enough.
    None,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChatFailure {
    pub message: String,
    pub action: ChatAction,
}

/// A sentence and an action for a failed `POST /v1/messages`.
pub fn classify_send_error(error: &ApiError) -> ChatFailure {
    match error {
        ApiError::Transport => ChatFailure {
            message: "Can't reach SeraBot. Your message is still in the box. Check the connection, then send it again.".into(),
            action: ChatAction::Retry,
        },
        ApiError::Http(401 | 403) => ChatFailure {
            message: "Your session is no longer valid. Your message is still in the box. Reconnect, then send it again.".into(),
            action: ChatAction::Reconnect,
        },
        ApiError::Http(404) => ChatFailure {
            message: "This conversation could not be found. Your text is still in the box. Try switching chats or starting a new one.".into(),
            action: ChatAction::None,
        },
        ApiError::Http(413) => ChatFailure {
            message: "That message or attachment is too large for the server. Shorten it or remove a file, then try again.".into(),
            action: ChatAction::None,
        },
        ApiError::Http(408 | 504) => ChatFailure {
            message: "The request timed out. Your message is still in the box. Send it again.".into(),
            action: ChatAction::Retry,
        },
        ApiError::Http(500..=599) => ChatFailure {
            message: "SeraBot had a problem sending that message. Your text is still in the box. Try again.".into(),
            action: ChatAction::Retry,
        },
        ApiError::Http(429) => ChatFailure {
            message: "Too many requests right now. Your message is still in the box. Wait a moment, then send it again.".into(),
            action: ChatAction::Retry,
        },
        // Every route of the bot's chain failed or none was ready: the
        // server sends this code with stable reason words only.
        ApiError::Coded { code, .. } if code == ALL_ROUTES_FAILED => ChatFailure {
            message: "None of this bot's routes could answer. Your message is still in the box. Check the accounts on AI keys, then send it again.".into(),
            action: ChatAction::OpenKeys,
        },
        ApiError::Coded { message, code, .. } => from_sentence(message, Some(code)),
        ApiError::Rejected(message) => from_sentence(message, None),
        other => from_sentence(&other.to_string(), None),
    }
}

fn from_sentence(message: &str, code: Option<&str>) -> ChatFailure {
    if looks_like_payload(message) {
        return ChatFailure {
            message: "Something went wrong sending that message. Your text is still in the box. Try again.".into(),
            action: ChatAction::Retry,
        };
    }
    if keyish(message) || code.is_some_and(keyish) {
        return keys_failure();
    }
    ChatFailure {
        message: message.to_owned(),
        action: ChatAction::None,
    }
}

fn keys_failure() -> ChatFailure {
    ChatFailure {
        message: "The provider rejected the API key. Open AI keys and check it. Your message is still in the box.".into(),
        action: ChatAction::OpenKeys,
    }
}

/// Text to put back in an empty composer after a send that did not land.
/// `None` when the composer already has something, so a newer draft is kept.
pub fn restore_unsent_draft(composer: &str, optimistic: Option<&str>) -> Option<String> {
    if !composer.trim().is_empty() {
        return None;
    }
    let text = optimistic?.trim();
    if text.is_empty() {
        None
    } else {
        Some(text.to_owned())
    }
}

fn keyish(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    lower.contains("api key")
        || lower.contains("api_key")
        || lower.contains("provider key")
        || lower.contains("invalid key")
        || lower.contains("missing key")
}

fn looks_like_payload(text: &str) -> bool {
    let trimmed = text.trim_start();
    let lower = trimmed.to_ascii_lowercase();
    trimmed.starts_with('{')
        || trimmed.starts_with('[')
        || lower.starts_with("<!doctype")
        || lower.starts_with("<html")
}

// ---- The failed-turn card (v0.50) ----

/// Most other routes "Retry on..." lists.
pub const MAX_ALTERNATIVES: usize = 6;

/// What the failed-turn card shows: why, and the ways forward.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FailedCard {
    /// Reason words, newest first, from a fixed vocabulary.
    pub reasons: Vec<String>,
    /// "Retry on <fallback>": the next route of the bot's chain.
    pub fallback: Option<FallbackOffer>,
    /// "Retry on...": other routes the member may pick.
    pub alternatives: Vec<FallbackOffer>,
    /// "Ask main": hand the request to the orchestrator.
    pub ask_main: bool,
}

/// Reason words of the conversation's newest route switches. A switch records
/// why the route before it failed, which is the closest the server comes to
/// saying why a turn did not finish. Provider text never gets here: only the
/// fixed reason codes become chips.
pub fn recent_reasons(events: &[RouteSwitch]) -> Vec<String> {
    let mut ordered: Vec<&RouteSwitch> = events.iter().collect();
    ordered.sort_by(|a, b| {
        b.created_at
            .cmp(&a.created_at)
            .then_with(|| b.id.cmp(&a.id))
    });
    let mut out: Vec<String> = Vec::new();
    for event in ordered {
        for chip in crate::runs::reason_chips(&event.reason) {
            if !out.contains(&chip) && out.len() < 4 {
                out.push(chip);
            }
        }
    }
    out
}

fn same(entry: &crate::api::RouteEntry, selection: &Selection) -> bool {
    entry.provider == selection.provider
        && entry.model == selection.model
        && entry.account == selection.account
}

/// Routes for "Retry on...": the bot's own chain first, then approved models
/// from the live catalog. The route the conversation is on, and the one the
/// "Retry on <fallback>" button already offers, are left out.
pub fn retry_alternatives(
    options: &Options<'_>,
    chain: Option<&RouteView>,
    current: &Selection,
    skip: Option<&FallbackOffer>,
) -> Vec<FallbackOffer> {
    let mut out: Vec<FallbackOffer> = Vec::new();
    let mut push = |selection: Selection, thinking: Option<String>| {
        if out.len() >= MAX_ALTERNATIVES
            || selection.is_auto()
            || selection == *current
            || skip.is_some_and(|offer| offer.selection == selection)
            || out.iter().any(|offer| offer.selection == selection)
        {
            return;
        }
        let label = route_line(
            options,
            &selection.provider,
            selection.account.as_deref(),
            &selection.model,
            None,
        );
        out.push(FallbackOffer {
            selection,
            thinking,
            label,
        });
    };
    if let Some(chain) = chain {
        for entry in chain.route.iter().chain(chain.fallbacks.iter()) {
            if same(entry, current) {
                continue;
            }
            push(
                Selection {
                    provider: entry.provider.clone(),
                    model: entry.model.clone(),
                    account: entry.account.clone(),
                },
                entry.thinking.clone(),
            );
        }
    }
    for entry in options.entries.iter().filter(|e| e.data_egress_approved) {
        push(
            Selection {
                provider: entry.provider.clone(),
                model: entry.model.clone(),
                account: None,
            },
            None,
        );
    }
    out
}

/// The card for a failed turn.
pub fn failed_card(
    options: &Options<'_>,
    chain: Option<&RouteView>,
    current: &Selection,
    events: &[RouteSwitch],
    fallback: Option<FallbackOffer>,
    ask_main: bool,
) -> FailedCard {
    FailedCard {
        reasons: recent_reasons(events),
        alternatives: retry_alternatives(options, chain, current, fallback.as_ref()),
        fallback,
        ask_main,
    }
}

/// The message "Ask main" sends: fixed words plus the reason chips, so a
/// provider's text can never ride along into the orchestrator's prompt.
pub fn ask_main_text(reasons: &[String]) -> String {
    let mut text = String::from("My last request did not finish because a model route failed.");
    if !reasons.is_empty() {
        text.push_str(" Reasons: ");
        text.push_str(&reasons.join(", "));
        text.push('.');
    }
    text.push_str(
        " Please take over: try it again on another route or hand it to the right specialist, and tell me what you did.",
    );
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transport_timeout_and_server_errors_keep_a_next_step() {
        let lost = classify_send_error(&ApiError::Transport);
        assert_eq!(lost.action, ChatAction::Retry);
        assert!(lost.message.contains("still in the box"));
        assert!(!lost.message.contains('{'));

        let timed = classify_send_error(&ApiError::Http(504));
        assert!(timed.message.contains("timed out"));
        assert_eq!(timed.action, ChatAction::Retry);

        let down = classify_send_error(&ApiError::Http(502));
        assert_eq!(down.action, ChatAction::Retry);
        assert!(down.message.contains("SeraBot"));
    }

    #[test]
    fn a_rejected_key_points_at_ai_keys_and_json_is_not_shown() {
        let key = classify_send_error(&ApiError::Rejected("missing api key".into()));
        assert_eq!(key.action, ChatAction::OpenKeys);
        assert!(key.message.contains("AI keys"));

        let dumped = classify_send_error(&ApiError::Rejected(
            "{\"error\":\"boom\",\"api_key\":\"x\"}".into(),
        ));
        assert!(!dumped.message.contains("api_key"));
        assert!(!dumped.message.contains('{'));
        assert_eq!(dumped.action, ChatAction::Retry);
    }

    #[test]
    fn every_route_failing_points_at_the_accounts_not_at_json() {
        let all_failed = classify_send_error(&ApiError::Coded {
            status: 422,
            code: ALL_ROUTES_FAILED.into(),
            message: "No model route could answer (rate_limited; quota_exhausted)".into(),
        });
        assert_eq!(all_failed.action, ChatAction::OpenKeys);
        assert!(all_failed.message.contains("routes could answer"));
        assert!(!all_failed.message.contains("rate_limited"));
        assert!(FAILED_TURN_WITH_FALLBACK.contains("next route"));
    }

    #[test]
    fn an_empty_composer_gets_the_unsent_text_back() {
        assert_eq!(
            restore_unsent_draft("", Some("hello")),
            Some("hello".into())
        );
        assert_eq!(restore_unsent_draft("already typing", Some("hello")), None);
        assert_eq!(restore_unsent_draft("", Some("  ")), None);
        assert_eq!(restore_unsent_draft("", None), None);
    }

    #[test]
    fn auth_size_missing_and_html_errors_map_cleanly() {
        let auth = classify_send_error(&ApiError::Http(401));
        assert_eq!(auth.action, ChatAction::Reconnect);
        assert!(auth.message.contains("still in the box"));

        let forbidden = classify_send_error(&ApiError::Http(403));
        assert_eq!(forbidden.action, ChatAction::Reconnect);

        let missing = classify_send_error(&ApiError::Http(404));
        assert_eq!(missing.action, ChatAction::None);
        assert!(missing.message.contains("not be found"));

        let large = classify_send_error(&ApiError::Http(413));
        assert_eq!(large.action, ChatAction::None);
        assert!(large.message.contains("too large"));

        let html = classify_send_error(&ApiError::Rejected(
            "<!DOCTYPE html><html>502</html>".into(),
        ));
        assert_eq!(html.action, ChatAction::Retry);
        assert!(!html.message.contains('<'));
    }

    fn switch(id: &str, at: &str, reason: &str) -> RouteSwitch {
        RouteSwitch {
            id: id.into(),
            created_at: at.into(),
            reason: reason.into(),
            to_provider: Some("anthropic".into()),
            ..RouteSwitch::default()
        }
    }

    #[test]
    fn reason_chips_come_from_the_newest_switches_and_known_words_only() {
        let events = vec![
            switch("a", "2026-11-16T10:00:00Z", "quota_exhausted"),
            switch("b", "2026-11-16T10:05:00Z", "rate_limited"),
            switch("c", "2026-11-16T10:06:00Z", "<script>alert(1)</script>"),
            switch("d", "2026-11-16T10:07:00Z", "rate_limited"),
        ];
        assert_eq!(
            recent_reasons(&events),
            vec!["Rate limited".to_owned(), "Quota used up".to_owned()]
        );
        assert!(recent_reasons(&[]).is_empty());
    }

    fn world() -> (
        Vec<crate::api::CatalogEntry>,
        Vec<crate::api::Provider>,
        Vec<crate::api::ProviderStatus>,
    ) {
        let entry = |provider: &str, model: &str, approved: bool| crate::api::CatalogEntry {
            provider: provider.into(),
            model: model.into(),
            data_egress_approved: approved,
            name: None,
            context_length: None,
            tags: Vec::new(),
            thinking: Vec::new(),
            account_ids: Vec::new(),
        };
        (
            vec![
                entry("openai", "gpt-5", true),
                entry("anthropic", "claude-sonnet", true),
                entry("unapproved", "x", false),
            ],
            Vec::new(),
            Vec::new(),
        )
    }

    fn selection(provider: &str, model: &str) -> Selection {
        Selection {
            provider: provider.into(),
            model: model.into(),
            account: None,
        }
    }

    #[test]
    fn retry_on_lists_other_approved_routes_without_the_current_or_offered_one() {
        let data = world();
        let options = Options {
            entries: &data.0,
            catalog_providers: &data.1,
            providers: &data.2,
        };
        let current = selection("openai", "gpt-5");
        let offered = FallbackOffer {
            selection: selection("anthropic", "claude-sonnet"),
            thinking: None,
            label: "Retry on Anthropic".into(),
        };
        let none = retry_alternatives(&options, None, &current, Some(&offered));
        assert!(none.is_empty(), "{none:?}");
        let some = retry_alternatives(&options, None, &current, None);
        assert_eq!(some.len(), 1);
        assert_eq!(some[0].selection, selection("anthropic", "claude-sonnet"));
        // A route that is not egress-approved is never offered.
        assert!(
            some.iter()
                .all(|offer| offer.selection.provider != "unapproved"),
        );
    }

    #[test]
    fn retry_alternatives_are_bounded_and_the_chain_comes_first() {
        let mut entries = Vec::new();
        for i in 0..20 {
            entries.push(crate::api::CatalogEntry {
                provider: format!("p{i}"),
                model: "m".into(),
                data_egress_approved: true,
                name: None,
                context_length: None,
                tags: Vec::new(),
                thinking: Vec::new(),
                account_ids: Vec::new(),
            });
        }
        let options = Options {
            entries: &entries,
            catalog_providers: &[],
            providers: &[],
        };
        let chain = RouteView {
            route: Some(crate::api::RouteEntry {
                provider: "p0".into(),
                model: "m".into(),
                ..Default::default()
            }),
            fallbacks: vec![crate::api::RouteEntry {
                provider: "p19".into(),
                model: "m".into(),
                ..Default::default()
            }],
            ..RouteView::default()
        };
        let current = selection("p0", "m");
        let alternatives = retry_alternatives(&options, Some(&chain), &current, None);
        assert_eq!(alternatives.len(), MAX_ALTERNATIVES);
        assert_eq!(alternatives[0].selection.provider, "p19");
    }

    #[test]
    fn ask_main_carries_only_fixed_words_and_reason_chips() {
        let text = ask_main_text(&["Rate limited".into(), "Quota used up".into()]);
        assert!(text.contains("Reasons: Rate limited, Quota used up."));
        assert!(text.contains("take over"));
        assert!(!ask_main_text(&[]).contains("Reasons"));
    }

    #[test]
    fn the_failed_card_bundles_reasons_and_actions() {
        let data = world();
        let options = Options {
            entries: &data.0,
            catalog_providers: &data.1,
            providers: &data.2,
        };
        let card = failed_card(
            &options,
            None,
            &selection("openai", "gpt-5"),
            &[switch("a", "2026-11-16T10:00:00Z", "overloaded")],
            None,
            true,
        );
        assert_eq!(card.reasons, vec!["Overloaded"]);
        assert!(card.fallback.is_none());
        assert_eq!(card.alternatives.len(), 1);
        assert!(card.ask_main);
    }
}
