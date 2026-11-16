//! The bot "Model route" editor: a primary route and up to two fallbacks, each
//! with a provider, an account (or "any"), a model and a thinking level, plus
//! the auto-switch policy.
//!
//! Plain data and pure functions, no I/O and no UI. The draft builds the exact
//! `PUT /v1/bots/{id}/route` body, and [`RouteDraft::check`] gives the inline
//! messages, so the editor is unit-tested without a window. The server checks
//! every field again; these messages only spare a round trip.

use crate::api::{
    CatalogEntry, EffectiveRoute, FailoverPolicy, Provider, ProviderStatus, RouteEntry,
    RouteSwitch, RouteUpdate, RouteView, thinking_label,
};
use crate::provider_accounts;

/// The most fallbacks a bot can list.
pub const MAX_FALLBACKS: usize = 2;
/// The most switches one model call may make (the server's own bound).
pub const MAX_SWITCHES: u8 = 8;

/// What the pickers can offer, borrowed from the session.
#[derive(Clone, Copy)]
pub struct Options<'a> {
    /// The live catalog's entries (models with their thinking levels).
    pub entries: &'a [CatalogEntry],
    /// The providers the member may route to, with their egress approval.
    pub catalog_providers: &'a [Provider],
    /// The Providers page's list, for the account labels and the kinds.
    pub providers: &'a [ProviderStatus],
}

/// One provider in a picker.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProviderChoice {
    pub id: String,
    pub label: String,
    pub egress_approved: bool,
}

/// One model in a picker.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ModelChoice {
    pub model: String,
    pub title: String,
}

impl Options<'_> {
    pub fn provider_choices(&self) -> Vec<ProviderChoice> {
        self.catalog_providers
            .iter()
            .filter(|provider| !provider.models.is_empty())
            .map(|provider| ProviderChoice {
                id: provider.id.clone(),
                label: self.provider_label(&provider.id),
                egress_approved: provider.data_egress_approved,
            })
            .collect()
    }

    /// The provider's readable name: its brand, else its id.
    pub fn provider_label(&self, id: &str) -> String {
        let kind = self
            .providers
            .iter()
            .find(|provider| provider.id == id)
            .map(|provider| provider.kind.as_str())
            .unwrap_or(id);
        crate::provider_view::display_name(id, kind, crate::branding::brand_label(kind))
    }

    /// `(id, label)` pairs for the account picker, "Any account" first. A
    /// switched-off account is not offered.
    pub fn account_choices(&self, provider: &str) -> Vec<(String, String)> {
        let mut choices = vec![(String::new(), "Any account".to_owned())];
        if let Some(status) = self.providers.iter().find(|p| p.id == provider) {
            for account in provider_accounts::ordered(&status.accounts) {
                if provider_accounts::state_of(account) == provider_accounts::AccountState::Disabled
                {
                    continue;
                }
                choices.push((
                    account.id.clone(),
                    provider_accounts::display_label(account),
                ));
            }
        }
        choices
    }

    pub fn account_label(&self, provider: &str, account: &str) -> Option<String> {
        self.providers
            .iter()
            .find(|p| p.id == provider)?
            .accounts
            .iter()
            .find(|a| a.id == account)
            .map(provider_accounts::display_label)
    }

    pub fn model_choices(&self, provider: &str) -> Vec<ModelChoice> {
        let from_entries: Vec<ModelChoice> = self
            .entries
            .iter()
            .filter(|entry| entry.provider == provider)
            .map(|entry| ModelChoice {
                model: entry.model.clone(),
                title: entry.name.clone().unwrap_or_else(|| entry.model.clone()),
            })
            .collect();
        if !from_entries.is_empty() {
            return from_entries;
        }
        self.catalog_providers
            .iter()
            .find(|p| p.id == provider)
            .map(|p| {
                p.models
                    .iter()
                    .map(|model| ModelChoice {
                        model: model.clone(),
                        title: model.clone(),
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    /// The thinking levels a model accepts, weakest first; empty when it has
    /// no thinking control (or the catalog does not say).
    pub fn thinking_levels(&self, provider: &str, model: &str) -> Vec<String> {
        self.entries
            .iter()
            .find(|entry| entry.provider == provider && entry.model == model)
            .map(|entry| entry.thinking.clone())
            .unwrap_or_default()
    }

    fn egress_approved(&self, provider: &str) -> Option<bool> {
        self.catalog_providers
            .iter()
            .find(|p| p.id == provider)
            .map(|p| p.data_egress_approved)
    }

    fn account_exists(&self, provider: &str, account: &str) -> bool {
        self.providers
            .iter()
            .find(|p| p.id == provider)
            .is_some_and(|p| p.accounts.iter().any(|a| a.id == account))
    }
}

/// One row of the editor. Empty strings mean "not chosen" (`account`: any
/// account; `thinking`: the model's default).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RouteSlot {
    pub provider: String,
    pub account: String,
    pub model: String,
    pub thinking: String,
}

impl RouteSlot {
    pub fn from_entry(entry: &RouteEntry) -> Self {
        Self {
            provider: entry.provider.clone(),
            account: entry.account.clone().unwrap_or_default(),
            model: entry.model.clone(),
            thinking: entry.thinking.clone().unwrap_or_default(),
        }
    }

    /// An unused slot (no provider chosen).
    pub fn is_empty(&self) -> bool {
        self.provider.is_empty()
    }

    pub fn to_entry(&self) -> Option<RouteEntry> {
        if self.is_empty() {
            return None;
        }
        Some(RouteEntry {
            provider: self.provider.clone(),
            model: self.model.clone(),
            thinking: (!self.thinking.is_empty()).then(|| self.thinking.clone()),
            account: (!self.account.is_empty()).then(|| self.account.clone()),
        })
    }

    /// Pick a provider: the account resets to "any", the model to the
    /// provider's first, and a thinking level stays only when that model takes it.
    pub fn set_provider(&mut self, provider: &str, options: &Options<'_>) {
        if provider.is_empty() {
            *self = Self::default();
            return;
        }
        if self.provider == provider {
            return;
        }
        self.provider = provider.to_owned();
        self.account.clear();
        let first = options
            .model_choices(provider)
            .into_iter()
            .next()
            .map(|choice| choice.model)
            .unwrap_or_default();
        self.set_model(&first, options);
    }

    /// Pick a model: the thinking level stays when the new model supports it,
    /// else it goes back to the model's default.
    pub fn set_model(&mut self, model: &str, options: &Options<'_>) {
        self.model = model.to_owned();
        if !self.thinking.is_empty()
            && !options
                .thinking_levels(&self.provider, model)
                .contains(&self.thinking)
        {
            self.thinking.clear();
        }
    }
}

/// The whole editor: the primary, the fallbacks and the policy.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RouteDraft {
    pub primary: RouteSlot,
    pub fallbacks: [RouteSlot; MAX_FALLBACKS],
    /// A tick per fallback that names another provider than the primary: the
    /// member agrees that prompts may go there when the primary fails.
    pub consent: [bool; MAX_FALLBACKS],
    pub failover: FailoverPolicy,
}

impl Default for RouteDraft {
    fn default() -> Self {
        Self {
            primary: RouteSlot::default(),
            fallbacks: Default::default(),
            consent: [false; MAX_FALLBACKS],
            failover: FailoverPolicy::default(),
        }
    }
}

/// The messages next to each part of the editor.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RouteCheck {
    pub primary: Option<String>,
    pub fallbacks: [Option<String>; MAX_FALLBACKS],
    /// Per fallback: the cross-provider box must be ticked.
    pub needs_consent: [bool; MAX_FALLBACKS],
    pub general: Option<String>,
}

impl RouteCheck {
    pub fn is_ok(&self) -> bool {
        self.primary.is_none()
            && self.fallbacks.iter().all(Option::is_none)
            && self.general.is_none()
    }

    /// The first message, for a single banner.
    pub fn first(&self) -> Option<&str> {
        self.primary
            .as_deref()
            .or_else(|| self.fallbacks.iter().flatten().next().map(String::as_str))
            .or(self.general.as_deref())
    }
}

impl RouteDraft {
    /// Fill the editor from what the server shows. A route the server
    /// accepted with `allow_cross_provider` counts as consented.
    pub fn from_view(view: Option<&RouteView>) -> Self {
        let Some(view) = view else {
            return Self::default();
        };
        let mut draft = Self {
            primary: view
                .route
                .as_ref()
                .map(RouteSlot::from_entry)
                .unwrap_or_default(),
            failover: view.failover,
            ..Self::default()
        };
        for (index, entry) in view.fallbacks.iter().take(MAX_FALLBACKS).enumerate() {
            draft.fallbacks[index] = RouteSlot::from_entry(entry);
            draft.consent[index] = view.failover.allow_cross_provider;
        }
        draft
    }

    /// Row `index` of the editor: 0 is the primary, 1 and 2 the fallbacks.
    pub fn slot_mut(&mut self, index: usize) -> Option<&mut RouteSlot> {
        match index {
            0 => Some(&mut self.primary),
            1 | 2 => self.fallbacks.get_mut(index - 1),
            _ => None,
        }
    }

    /// Whether fallback `index` names another provider than the primary.
    pub fn is_cross_provider(&self, index: usize) -> bool {
        let slot = &self.fallbacks[index];
        !slot.is_empty() && !self.primary.is_empty() && slot.provider != self.primary.provider
    }

    /// The fallbacks that are set, in order. A gap closes: fallback 2 alone
    /// becomes fallback 1 on the wire.
    fn filled_fallbacks(&self) -> Vec<(usize, &RouteSlot)> {
        self.fallbacks
            .iter()
            .enumerate()
            .filter(|(_, slot)| !slot.is_empty())
            .collect()
    }

    /// The exact body to send. `allow_cross_provider` is on only when every
    /// cross-provider fallback has its box ticked.
    pub fn to_update(&self) -> RouteUpdate {
        let cross: Vec<usize> = (0..MAX_FALLBACKS)
            .filter(|index| self.is_cross_provider(*index))
            .collect();
        let mut failover = self.failover;
        failover.allow_cross_provider =
            !cross.is_empty() && cross.iter().all(|index| self.consent[*index]);
        RouteUpdate {
            route: self.primary.to_entry(),
            fallbacks: self
                .filled_fallbacks()
                .into_iter()
                .filter_map(|(_, slot)| slot.to_entry())
                .collect(),
            failover,
        }
    }

    pub fn dirty(&self, original: &Self) -> bool {
        self.to_update() != original.to_update()
    }

    /// Check the draft against what the member can route to.
    pub fn check(&self, options: &Options<'_>) -> RouteCheck {
        let mut check = RouteCheck::default();
        let slots = [
            ("primary", &self.primary),
            ("fallback 1", &self.fallbacks[0]),
            ("fallback 2", &self.fallbacks[1]),
        ];
        if self.primary.is_empty() && !self.filled_fallbacks().is_empty() {
            check.primary = Some("Choose a primary model before adding fallbacks.".to_owned());
        }
        let mut seen: Vec<(&str, &RouteSlot)> = Vec::new();
        for (position, (name, slot)) in slots.iter().enumerate() {
            if slot.is_empty() {
                continue;
            }
            let message = slot_problem(slot, options).or_else(|| {
                seen.iter().find_map(|(other_name, other)| {
                    (other.provider == slot.provider
                        && other.model == slot.model
                        && other.account == slot.account)
                        .then(|| format!("Same route as the {other_name}. Pick something else."))
                })
            });
            if position > 0 {
                let index = position - 1;
                if message.is_none() && self.is_cross_provider(index) && !self.consent[index] {
                    check.needs_consent[index] = true;
                    check.fallbacks[index] = Some(
                        "This is another provider. Tick the box to allow sending your prompts there."
                            .to_owned(),
                    );
                } else {
                    check.fallbacks[index] = message;
                }
                if self.is_cross_provider(index) {
                    check.needs_consent[index] = true;
                }
            } else if check.primary.is_none() {
                check.primary = message;
            }
            seen.push((name, slot));
        }
        if self.failover.max_switches > MAX_SWITCHES {
            check.general = Some(format!("Switch at most {MAX_SWITCHES} times per call."));
        }
        check
    }
}

/// What is wrong with one chosen slot, in a sentence.
fn slot_problem(slot: &RouteSlot, options: &Options<'_>) -> Option<String> {
    let label = options.provider_label(&slot.provider);
    match options.egress_approved(&slot.provider) {
        None => return Some(format!("{label} is not available to you.")),
        Some(false) => {
            return Some(format!(
                "{label} has no data-egress approval, so a route cannot use it."
            ));
        }
        Some(true) => {}
    }
    if slot.model.is_empty() {
        return Some("Choose a model.".to_owned());
    }
    if !options
        .model_choices(&slot.provider)
        .iter()
        .any(|choice| choice.model == slot.model)
    {
        return Some(format!("{label} does not offer that model."));
    }
    if !slot.account.is_empty() && !options.account_exists(&slot.provider, &slot.account) {
        return Some("That account is gone. Choose \"Any account\" or another one.".to_owned());
    }
    if !slot.thinking.is_empty()
        && !options
            .thinking_levels(&slot.provider, &slot.model)
            .contains(&slot.thinking)
    {
        return Some("That thinking level isn't available for this model.".to_owned());
    }
    None
}

/// A route in one line: `OpenAI/Work · gpt-5 · High`.
pub fn route_line(
    options: &Options<'_>,
    provider: &str,
    account: Option<&str>,
    model: &str,
    thinking: Option<&str>,
) -> String {
    let mut line = options.provider_label(provider);
    if let Some(label) = account
        .filter(|account| !account.is_empty())
        .and_then(|account| options.account_label(provider, account))
    {
        line.push('/');
        line.push_str(&label);
    }
    if !model.is_empty() {
        line.push_str(" \u{b7} ");
        line.push_str(model);
    }
    if let Some(level) = thinking.filter(|level| !level.is_empty()) {
        line.push_str(" \u{b7} ");
        line.push_str(thinking_label(level));
    }
    line
}

/// Where a call for the bot would start now.
pub fn effective_line(options: &Options<'_>, effective: &EffectiveRoute) -> String {
    route_line(
        options,
        &effective.provider,
        effective.account.as_deref(),
        &effective.model,
        effective.thinking.as_deref(),
    )
}

/// A plain sentence for a route-switch reason word.
pub fn reason_text(reason: &str) -> &'static str {
    match reason {
        "rate_limited" => "rate limited",
        "overloaded" => "provider overloaded",
        "model_unavailable" => "model unavailable",
        "quota_exhausted" => "out of quota",
        "credentials_rejected" => "sign-in or key rejected",
        "account_forbidden" => "account not allowed",
        "provider_down" => "provider down",
        "transport" => "connection problem",
        "context_too_long" => "conversation too long for the model",
        _ => "the first route failed",
    }
}

/// `Switched to Anthropic/Work · claude-sonnet (rate limited)`.
pub fn switch_line(options: &Options<'_>, switch: &RouteSwitch) -> String {
    let to = route_line(
        options,
        switch.to_provider.as_deref().unwrap_or(""),
        switch.to_account.as_deref(),
        switch.to_model.as_deref().unwrap_or(""),
        None,
    );
    format!("Switched to {to} ({})", reason_text(&switch.reason))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::{AccountIdentity, ProviderAccount};

    fn entry(provider: &str, model: &str, levels: &[&str]) -> CatalogEntry {
        CatalogEntry {
            provider: provider.into(),
            model: model.into(),
            data_egress_approved: true,
            name: None,
            context_length: None,
            tags: Vec::new(),
            thinking: levels.iter().map(|l| (*l).to_owned()).collect(),
            account_ids: Vec::new(),
        }
    }

    fn provider(id: &str, egress: bool, models: &[&str]) -> Provider {
        Provider {
            id: id.into(),
            models: models.iter().map(|m| (*m).to_owned()).collect(),
            data_egress_approved: egress,
        }
    }

    fn status(id: &str, kind: &str, accounts: &[(&str, &str, &str)]) -> ProviderStatus {
        ProviderStatus {
            id: id.into(),
            kind: kind.into(),
            accounts: accounts
                .iter()
                .enumerate()
                .map(|(index, (account, label, state))| ProviderAccount {
                    id: (*account).into(),
                    label: (*label).into(),
                    status: (*state).into(),
                    priority: 10 - index as i64,
                    identity: AccountIdentity::default(),
                    ..ProviderAccount::default()
                })
                .collect(),
            ..ProviderStatus::default()
        }
    }

    struct World {
        entries: Vec<CatalogEntry>,
        catalog: Vec<Provider>,
        providers: Vec<ProviderStatus>,
    }

    impl World {
        fn new() -> Self {
            Self {
                entries: vec![
                    entry("openai", "gpt-5", &["low", "medium", "high"]),
                    entry("openai", "gpt-4o", &[]),
                    entry("anthropic", "claude-sonnet", &["low", "high"]),
                    entry("shut", "m", &[]),
                ],
                catalog: vec![
                    provider("openai", true, &["gpt-5", "gpt-4o"]),
                    provider("anthropic", true, &["claude-sonnet"]),
                    provider("shut", false, &["m"]),
                ],
                providers: vec![
                    status(
                        "openai",
                        "openai",
                        &[("a1", "Work", "ok"), ("a2", "Spare", "disabled")],
                    ),
                    status("anthropic", "anthropic", &[("k1", "Personal", "ok")]),
                ],
            }
        }

        fn options(&self) -> Options<'_> {
            Options {
                entries: &self.entries,
                catalog_providers: &self.catalog,
                providers: &self.providers,
            }
        }
    }

    fn slot(provider: &str, model: &str) -> RouteSlot {
        RouteSlot {
            provider: provider.into(),
            model: model.into(),
            ..RouteSlot::default()
        }
    }

    #[test]
    fn pickers_offer_providers_accounts_models_and_levels() {
        let world = World::new();
        let options = world.options();
        let providers: Vec<String> = options
            .provider_choices()
            .into_iter()
            .map(|p| p.id)
            .collect();
        assert_eq!(providers, ["openai", "anthropic", "shut"]);
        // "Any account" first; the switched-off account is not offered.
        assert_eq!(
            options.account_choices("openai"),
            vec![
                (String::new(), "Any account".to_owned()),
                ("a1".to_owned(), "Work".to_owned())
            ]
        );
        assert_eq!(options.account_choices("nobody").len(), 1);
        let models: Vec<String> = options
            .model_choices("openai")
            .into_iter()
            .map(|m| m.model)
            .collect();
        assert_eq!(models, ["gpt-5", "gpt-4o"]);
        assert_eq!(
            options.thinking_levels("openai", "gpt-5"),
            ["low", "medium", "high"]
        );
        assert!(options.thinking_levels("openai", "gpt-4o").is_empty());
    }

    #[test]
    fn choosing_a_provider_or_model_keeps_only_what_still_fits() {
        let world = World::new();
        let options = world.options();
        let mut slot = RouteSlot {
            provider: "openai".into(),
            account: "a1".into(),
            model: "gpt-5".into(),
            thinking: "high".into(),
        };
        // A model that takes the level keeps it.
        slot.set_model("gpt-5", &options);
        assert_eq!(slot.thinking, "high");
        // A model without levels drops it.
        slot.set_model("gpt-4o", &options);
        assert_eq!(slot.thinking, "");
        // A new provider resets the account and picks its first model.
        slot.thinking = "low".into();
        slot.set_provider("anthropic", &options);
        assert_eq!(
            (
                slot.account.as_str(),
                slot.model.as_str(),
                slot.thinking.as_str()
            ),
            ("", "claude-sonnet", "low")
        );
        // Choosing nothing clears the whole row.
        slot.set_provider("", &options);
        assert!(slot.is_empty() && slot.model.is_empty());
    }

    #[test]
    fn the_draft_builds_the_exact_body_and_closes_gaps() {
        let world = World::new();
        let _ = world.options();
        let mut draft = RouteDraft::default();
        assert_eq!(draft.to_update().route, None);
        draft.primary = RouteSlot {
            provider: "openai".into(),
            account: "a1".into(),
            model: "gpt-5".into(),
            thinking: "high".into(),
        };
        // Fallback 2 alone becomes fallback 1 on the wire.
        draft.fallbacks[1] = slot("openai", "gpt-4o");
        let update = draft.to_update();
        let primary = update.route.unwrap();
        assert_eq!(
            (primary.account.as_deref(), primary.thinking.as_deref()),
            (Some("a1"), Some("high"))
        );
        assert_eq!(update.fallbacks.len(), 1);
        assert_eq!(update.fallbacks[0].model, "gpt-4o");
        assert!(update.fallbacks[0].account.is_none() && update.fallbacks[0].thinking.is_none());
        assert!(!update.failover.allow_cross_provider);
        // The JSON never carries empty optional fields.
        let json = serde_json::to_value(draft.to_update()).unwrap();
        assert!(json["fallbacks"][0].get("account").is_none());
        assert_eq!(json["route"]["account"], "a1");
        assert_eq!(json["failover"]["max_switches"], 3);
    }

    #[test]
    fn cross_provider_is_on_only_when_every_such_fallback_is_ticked() {
        let mut draft = RouteDraft {
            primary: slot("openai", "gpt-5"),
            ..RouteDraft::default()
        };
        draft.fallbacks[0] = slot("anthropic", "claude-sonnet");
        draft.fallbacks[1] = slot("openai", "gpt-4o");
        assert!(draft.is_cross_provider(0) && !draft.is_cross_provider(1));
        assert!(!draft.to_update().failover.allow_cross_provider);
        draft.consent[0] = true;
        assert!(draft.to_update().failover.allow_cross_provider);
        // A second cross-provider fallback needs its own tick.
        draft.fallbacks[1] = slot("shut", "m");
        assert!(!draft.to_update().failover.allow_cross_provider);
        draft.consent[1] = true;
        assert!(draft.to_update().failover.allow_cross_provider);
        // Only same-provider fallbacks: never allowed, whatever was ticked.
        draft.fallbacks[0] = slot("openai", "gpt-4o");
        draft.fallbacks[1] = RouteSlot::default();
        assert!(!draft.to_update().failover.allow_cross_provider);
    }

    #[test]
    fn inline_checks_name_the_problem() {
        let world = World::new();
        let options = world.options();
        let mut draft = RouteDraft::default();
        assert!(
            draft.check(&options).is_ok(),
            "an empty route follows the default"
        );

        draft.fallbacks[0] = slot("openai", "gpt-5");
        assert!(
            draft
                .check(&options)
                .primary
                .unwrap()
                .contains("primary model before")
        );

        draft.primary = slot("openai", "gpt-5");
        let dup = draft.check(&options);
        assert!(
            dup.fallbacks[0]
                .as_deref()
                .unwrap()
                .contains("Same route as the primary")
        );

        draft.fallbacks[0] = slot("shut", "m");
        assert!(
            draft.check(&options).fallbacks[0]
                .as_deref()
                .unwrap()
                .contains("no data-egress approval")
        );

        draft.fallbacks[0] = slot("nowhere", "m");
        assert!(
            draft.check(&options).fallbacks[0]
                .as_deref()
                .unwrap()
                .contains("not available to you")
        );

        draft.fallbacks[0] = slot("openai", "gpt-7");
        assert!(
            draft.check(&options).fallbacks[0]
                .as_deref()
                .unwrap()
                .contains("does not offer that model")
        );

        draft.fallbacks[0] = RouteSlot {
            account: "gone".into(),
            ..slot("openai", "gpt-4o")
        };
        assert!(
            draft.check(&options).fallbacks[0]
                .as_deref()
                .unwrap()
                .contains("account is gone")
        );

        draft.fallbacks[0] = RouteSlot {
            thinking: "high".into(),
            ..slot("openai", "gpt-4o")
        };
        assert!(
            draft.check(&options).fallbacks[0]
                .as_deref()
                .unwrap()
                .contains("thinking level isn't available")
        );

        draft.primary = slot("openai", "");
        assert_eq!(
            draft.check(&options).primary.as_deref(),
            Some("Choose a model.")
        );
    }

    #[test]
    fn a_cross_provider_fallback_asks_for_its_tick_and_then_passes() {
        let world = World::new();
        let options = world.options();
        let mut draft = RouteDraft {
            primary: slot("openai", "gpt-5"),
            ..RouteDraft::default()
        };
        draft.fallbacks[0] = slot("anthropic", "claude-sonnet");
        let unticked = draft.check(&options);
        assert!(!unticked.is_ok());
        assert!(unticked.needs_consent[0] && !unticked.needs_consent[1]);
        assert!(
            unticked.fallbacks[0]
                .as_deref()
                .unwrap()
                .contains("Tick the box")
        );
        draft.consent[0] = true;
        let ticked = draft.check(&options);
        assert!(ticked.is_ok(), "{ticked:?}");
        // The consent line stays visible once ticked.
        assert!(ticked.needs_consent[0]);
        assert!(ticked.first().is_none());
    }

    #[test]
    fn the_draft_round_trips_the_server_view() {
        let view = RouteView {
            route: Some(RouteEntry {
                provider: "openai".into(),
                model: "gpt-5".into(),
                thinking: Some("high".into()),
                account: Some("a1".into()),
            }),
            fallbacks: vec![RouteEntry {
                provider: "anthropic".into(),
                model: "claude-sonnet".into(),
                thinking: None,
                account: None,
            }],
            failover: FailoverPolicy {
                allow_cross_provider: true,
                max_switches: 2,
                ..FailoverPolicy::default()
            },
            effective: None,
            last_switch: None,
        };
        let draft = RouteDraft::from_view(Some(&view));
        assert_eq!(draft.primary.account, "a1");
        assert!(
            draft.consent[0],
            "a saved cross-provider route is already agreed to"
        );
        let update = draft.to_update();
        assert_eq!(update.route, view.route);
        assert_eq!(update.fallbacks, view.fallbacks);
        assert_eq!(update.failover, view.failover);
        assert!(!draft.dirty(&draft.clone()));
        let mut edited = draft.clone();
        edited.failover.enabled = false;
        assert!(edited.dirty(&draft));
        // No route yet: an empty editor that sends a null route.
        let empty = RouteDraft::from_view(None);
        assert!(empty.primary.is_empty());
        assert_eq!(
            serde_json::to_value(empty.to_update()).unwrap()["route"],
            serde_json::Value::Null
        );
    }

    #[test]
    fn route_lines_and_switch_words_read_naturally() {
        let world = World::new();
        let options = world.options();
        assert_eq!(
            route_line(&options, "openai", Some("a1"), "gpt-5", Some("high")),
            "OpenAI/Work \u{b7} gpt-5 \u{b7} High"
        );
        assert_eq!(
            route_line(&options, "anthropic", None, "claude-sonnet", None),
            "Anthropic \u{b7} claude-sonnet"
        );
        let switch = RouteSwitch {
            to_provider: Some("anthropic".into()),
            to_model: Some("claude-sonnet".into()),
            to_account: Some("k1".into()),
            reason: "rate_limited".into(),
            ..RouteSwitch::default()
        };
        assert_eq!(
            switch_line(&options, &switch),
            "Switched to Anthropic/Personal \u{b7} claude-sonnet (rate limited)"
        );
        assert_eq!(reason_text("made_up"), "the first route failed");
        for word in [
            "overloaded",
            "model_unavailable",
            "quota_exhausted",
            "credentials_rejected",
            "account_forbidden",
            "provider_down",
            "transport",
            "context_too_long",
        ] {
            assert_ne!(reason_text(word), "the first route failed", "{word}");
        }
        let effective = EffectiveRoute {
            provider: "openai".into(),
            model: "gpt-5".into(),
            account: None,
            thinking: None,
        };
        assert_eq!(effective_line(&options, &effective), "OpenAI \u{b7} gpt-5");
    }
}
