//! The composer's route chip, its thinking control, the "Switched to X" rows
//! in the transcript and the "Retry on <fallback>" offer.
//!
//! Pure functions over the catalog, the member's accounts and the server's
//! route events. No I/O and no UI, so each rule is unit-tested.

use crate::api::{RouteEntry, RouteSwitch, RouteView, Selection, TaskStatus, thinking_label};
use crate::route_editor::{Options, route_line, switch_line};

/// The chip text for the open conversation: `provider/account · model · thinking`.
/// `Auto` reads as the approved route. A concrete selection names its account
/// only when the member pinned one.
pub fn chip_label(options: &Options<'_>, selection: &Selection, thinking: Option<&str>) -> String {
    if selection.is_auto() {
        return "Auto \u{b7} approved route".to_owned();
    }
    route_line(
        options,
        &selection.provider,
        selection.account.as_deref(),
        &selection.model,
        thinking,
    )
}

/// One button of the thinking control.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ThinkingSegment {
    /// `""` is the model's default.
    pub value: String,
    pub label: &'static str,
    pub active: bool,
}

/// The segmented thinking control: "Default" first, then the model's levels,
/// weakest first. Empty when the model has no thinking control. A current level
/// the model does not list is shown as "Default", never as a stale choice.
pub fn thinking_segments(levels: &[String], current: &str) -> Vec<ThinkingSegment> {
    if levels.is_empty() {
        return Vec::new();
    }
    let current = if levels.iter().any(|level| level == current) {
        current
    } else {
        ""
    };
    let mut segments = vec![ThinkingSegment {
        value: String::new(),
        label: "Default",
        active: current.is_empty(),
    }];
    segments.extend(levels.iter().map(|level| ThinkingSegment {
        value: level.clone(),
        label: thinking_label(level),
        active: level == current,
    }));
    segments
}

/// The account choices of one provider group in the picker, "Any account"
/// first. Empty (no account step) unless the member has two or more usable
/// accounts there: with one, "any" and that account are the same.
pub fn picker_accounts(options: &Options<'_>, provider: &str) -> Vec<(String, String)> {
    let choices = options.account_choices(provider);
    if choices.len() > 2 {
        choices
    } else {
        Vec::new()
    }
}

/// A "Switched to X (reason)" row of the transcript.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SwitchRow {
    pub id: String,
    pub text: String,
    pub created_at: String,
}

/// Rows for the open conversation's switches, oldest first. The server already
/// scopes the list to the member and the conversation; a row with no target is
/// skipped rather than drawn half empty.
pub fn switch_rows(options: &Options<'_>, events: &[RouteSwitch]) -> Vec<SwitchRow> {
    let mut rows: Vec<SwitchRow> = events
        .iter()
        .filter(|event| event.to_provider.as_deref().is_some_and(|p| !p.is_empty()))
        .map(|event| SwitchRow {
            id: event.id.clone(),
            text: switch_line(options, event),
            created_at: event.created_at.clone(),
        })
        .collect();
    rows.sort_by(|a, b| {
        a.created_at
            .cmp(&b.created_at)
            .then_with(|| a.id.cmp(&b.id))
    });
    rows
}

/// Put each row before the first message made after it, like a resolved card.
/// The result has one slot per message plus one for rows after the last.
pub fn place_rows(message_times: &[String], rows: Vec<SwitchRow>) -> Vec<Vec<SwitchRow>> {
    let mut placed: Vec<Vec<SwitchRow>> = vec![Vec::new(); message_times.len() + 1];
    for row in rows {
        let index = message_times.partition_point(|at| at.as_str() <= row.created_at.as_str());
        placed[index].push(row);
    }
    placed
}

/// A fallback the member can retry on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FallbackOffer {
    pub selection: Selection,
    pub thinking: Option<String>,
    /// The button text: `Retry on Anthropic/Work · claude-sonnet`.
    pub label: String,
}

fn same_route(entry: &RouteEntry, selection: &Selection) -> bool {
    entry.provider == selection.provider
        && entry.model == selection.model
        && entry.account == selection.account
}

/// The next route in the bot's chain after the one the conversation is on.
/// With no known position the first fallback is offered. `None` when the bot
/// lists no fallback, or the conversation is already on the last one.
pub fn fallback_offer(
    options: &Options<'_>,
    chain: Option<&RouteView>,
    current: &Selection,
) -> Option<FallbackOffer> {
    let chain = chain?;
    if chain.fallbacks.is_empty() {
        return None;
    }
    let mut sequence: Vec<&RouteEntry> = chain.route.iter().collect();
    sequence.extend(chain.fallbacks.iter());
    // The chain's own head is "where it started"; any other selection (Auto, a
    // route outside the chain) is not a position in it.
    let at = sequence.iter().position(|entry| same_route(entry, current));
    let next = match at {
        Some(position) => sequence.get(position + 1).copied()?,
        None => chain.fallbacks.first()?,
    };
    let selection = Selection {
        provider: next.provider.clone(),
        model: next.model.clone(),
        account: next.account.clone(),
    };
    Some(FallbackOffer {
        label: format!(
            "Retry on {}",
            route_line(
                options,
                &next.provider,
                next.account.as_deref(),
                &next.model,
                None
            )
        ),
        thinking: next.thinking.clone(),
        selection,
    })
}

/// Whether the failed-turn card offers "Retry on <fallback>": the last turn
/// failed and there is a fallback that is not the route it ran on.
pub fn offer_retry(task_status: Option<TaskStatus>, offer: Option<&FallbackOffer>) -> bool {
    task_status == Some(TaskStatus::Failed) && offer.is_some()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::{
        AccountIdentity, CatalogEntry, FailoverPolicy, Provider, ProviderAccount, ProviderStatus,
    };

    fn world() -> (Vec<CatalogEntry>, Vec<Provider>, Vec<ProviderStatus>) {
        let entry = |provider: &str, model: &str, levels: &[&str]| CatalogEntry {
            provider: provider.into(),
            model: model.into(),
            data_egress_approved: true,
            name: None,
            context_length: None,
            tags: Vec::new(),
            thinking: levels.iter().map(|l| (*l).to_owned()).collect(),
            account_ids: Vec::new(),
        };
        let account = |id: &str, label: &str, status: &str| ProviderAccount {
            id: id.into(),
            label: label.into(),
            status: status.into(),
            // The first account is the one bots try first.
            priority: if id == "a1" { 2 } else { 1 },
            identity: AccountIdentity::default(),
            ..ProviderAccount::default()
        };
        (
            vec![
                entry("openai", "gpt-5", &["low", "high"]),
                entry("anthropic", "claude-sonnet", &[]),
            ],
            vec![
                Provider {
                    id: "openai".into(),
                    models: vec!["gpt-5".into()],
                    data_egress_approved: true,
                },
                Provider {
                    id: "anthropic".into(),
                    models: vec!["claude-sonnet".into()],
                    data_egress_approved: true,
                },
            ],
            vec![
                ProviderStatus {
                    id: "openai".into(),
                    kind: "openai".into(),
                    accounts: vec![account("a1", "Work", "ok"), account("a2", "Spare", "ok")],
                    ..ProviderStatus::default()
                },
                ProviderStatus {
                    id: "anthropic".into(),
                    kind: "anthropic".into(),
                    accounts: vec![account("k1", "Personal", "ok")],
                    ..ProviderStatus::default()
                },
            ],
        )
    }

    fn options<'a>(
        data: &'a (Vec<CatalogEntry>, Vec<Provider>, Vec<ProviderStatus>),
    ) -> Options<'a> {
        Options {
            entries: &data.0,
            catalog_providers: &data.1,
            providers: &data.2,
        }
    }

    fn selection(provider: &str, model: &str, account: Option<&str>) -> Selection {
        Selection {
            provider: provider.into(),
            model: model.into(),
            account: account.map(str::to_owned),
        }
    }

    #[test]
    fn the_chip_reads_provider_account_model_thinking() {
        let data = world();
        let options = options(&data);
        assert_eq!(
            chip_label(
                &options,
                &selection("openai", "gpt-5", Some("a1")),
                Some("high")
            ),
            "OpenAI/Work \u{b7} gpt-5 \u{b7} High"
        );
        assert_eq!(
            chip_label(&options, &selection("openai", "gpt-5", None), None),
            "OpenAI \u{b7} gpt-5"
        );
        assert_eq!(
            chip_label(&options, &Selection::auto(), Some("high")),
            "Auto \u{b7} approved route"
        );
        // An account that is gone is not named.
        assert_eq!(
            chip_label(&options, &selection("openai", "gpt-5", Some("zzz")), None),
            "OpenAI \u{b7} gpt-5"
        );
    }

    #[test]
    fn thinking_is_a_segmented_control_that_never_shows_a_stale_level() {
        let levels = vec!["low".to_owned(), "high".to_owned()];
        let segments = thinking_segments(&levels, "high");
        let labels: Vec<&str> = segments.iter().map(|s| s.label).collect();
        assert_eq!(labels, ["Default", "Low", "High"]);
        assert_eq!(
            segments
                .iter()
                .filter(|s| s.active)
                .map(|s| s.value.as_str())
                .collect::<Vec<_>>(),
            ["high"]
        );
        // The model does not list "max": Default is the active one.
        let stale = thinking_segments(&levels, "max");
        assert!(stale[0].active && stale.iter().skip(1).all(|s| !s.active));
        assert!(thinking_segments(&[], "high").is_empty());
    }

    #[test]
    fn the_account_step_appears_only_with_two_usable_accounts() {
        let data = world();
        let options = options(&data);
        let two = picker_accounts(&options, "openai");
        assert_eq!(
            two,
            vec![
                (String::new(), "Any account".to_owned()),
                ("a1".to_owned(), "Work".to_owned()),
                ("a2".to_owned(), "Spare".to_owned())
            ]
        );
        assert!(picker_accounts(&options, "anthropic").is_empty());
        assert!(picker_accounts(&options, "unknown").is_empty());
    }

    fn event(id: &str, at: &str, provider: Option<&str>, reason: &str) -> RouteSwitch {
        RouteSwitch {
            id: id.into(),
            to_provider: provider.map(str::to_owned),
            to_model: Some("claude-sonnet".into()),
            reason: reason.into(),
            created_at: at.into(),
            ..RouteSwitch::default()
        }
    }

    #[test]
    fn switch_rows_say_where_and_why_and_sit_in_time_order() {
        let data = world();
        let options = options(&data);
        let rows = switch_rows(
            &options,
            &[
                event(
                    "e2",
                    "2026-11-16T10:05:00Z",
                    Some("anthropic"),
                    "quota_exhausted",
                ),
                event(
                    "e1",
                    "2026-11-16T10:01:00Z",
                    Some("anthropic"),
                    "rate_limited",
                ),
                event("bad", "2026-11-16T10:02:00Z", None, "rate_limited"),
            ],
        );
        assert_eq!(rows.len(), 2, "a row with no target is skipped");
        assert_eq!(
            rows[0].text,
            "Switched to Anthropic \u{b7} claude-sonnet (rate limited)"
        );
        assert!(rows[1].text.ends_with("(out of quota)"));
        assert!(rows[0].created_at < rows[1].created_at);
    }

    #[test]
    fn rows_sit_before_the_first_later_message_and_trail_when_last() {
        let times: Vec<String> = ["2026-11-16T10:00:00Z", "2026-11-16T10:10:00Z"]
            .iter()
            .map(|s| (*s).to_owned())
            .collect();
        let row = |id: &str, at: &str| SwitchRow {
            id: id.into(),
            text: id.into(),
            created_at: at.into(),
        };
        let placed = place_rows(
            &times,
            vec![
                row("early", "2026-11-16T09:00:00Z"),
                row("middle", "2026-11-16T10:05:00Z"),
                row("late", "2026-11-16T11:00:00Z"),
            ],
        );
        assert_eq!(placed.len(), 3);
        assert_eq!(placed[0][0].id, "early");
        assert_eq!(placed[1][0].id, "middle");
        assert_eq!(placed[2][0].id, "late");
        assert_eq!(place_rows(&[], Vec::new()).len(), 1);
    }

    fn chain() -> RouteView {
        let entry = |provider: &str, model: &str, account: Option<&str>| RouteEntry {
            provider: provider.into(),
            model: model.into(),
            thinking: None,
            account: account.map(str::to_owned),
        };
        RouteView {
            route: Some(entry("openai", "gpt-5", Some("a1"))),
            fallbacks: vec![
                entry("openai", "gpt-5", Some("a2")),
                entry("anthropic", "claude-sonnet", None),
            ],
            failover: FailoverPolicy::default(),
            effective: None,
            last_switch: None,
        }
    }

    #[test]
    fn the_retry_offer_is_the_next_route_in_the_chain() {
        let data = world();
        let options = options(&data);
        let chain = chain();
        // On the primary: offer fallback 1, with its account named.
        let first = fallback_offer(
            &options,
            Some(&chain),
            &selection("openai", "gpt-5", Some("a1")),
        )
        .unwrap();
        assert_eq!(first.label, "Retry on OpenAI/Spare \u{b7} gpt-5");
        assert_eq!(first.selection, selection("openai", "gpt-5", Some("a2")));
        // On fallback 1: offer fallback 2.
        let second = fallback_offer(
            &options,
            Some(&chain),
            &selection("openai", "gpt-5", Some("a2")),
        )
        .unwrap();
        assert_eq!(second.label, "Retry on Anthropic \u{b7} claude-sonnet");
        // On the last fallback there is nowhere left to go.
        assert_eq!(
            fallback_offer(
                &options,
                Some(&chain),
                &selection("anthropic", "claude-sonnet", None)
            ),
            None
        );
        // Auto (not a position in the chain) offers the first fallback.
        assert_eq!(
            fallback_offer(&options, Some(&chain), &Selection::auto())
                .unwrap()
                .selection,
            selection("openai", "gpt-5", Some("a2"))
        );
        // No chain, or no fallbacks, offers nothing.
        assert_eq!(fallback_offer(&options, None, &Selection::auto()), None);
        let mut bare = chain.clone();
        bare.fallbacks.clear();
        assert_eq!(
            fallback_offer(&options, Some(&bare), &Selection::auto()),
            None
        );
    }

    #[test]
    fn the_failed_turn_card_needs_a_failure_and_a_fallback() {
        let data = world();
        let options = options(&data);
        let offer = fallback_offer(&options, Some(&chain()), &Selection::auto());
        assert!(offer_retry(Some(TaskStatus::Failed), offer.as_ref()));
        assert!(!offer_retry(Some(TaskStatus::Done), offer.as_ref()));
        assert!(!offer_retry(Some(TaskStatus::Running), offer.as_ref()));
        assert!(!offer_retry(None, offer.as_ref()));
        assert!(!offer_retry(Some(TaskStatus::Failed), None));
    }
}
