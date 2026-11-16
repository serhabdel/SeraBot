//! Plain-word view logic for a provider's accounts on the Providers page.
//!
//! Pure functions, no I/O and no UI, so the wording, the ordering and the
//! quota meter are tested on every platform. Nothing here holds a secret: an
//! account is a label, a masked identity and a status.

use crate::api::{ProviderAccount, ProviderStatus};
use crate::provider_presets::ProviderPreset;
use crate::theme::Tone;
use serde_json::Value;

/// The most accounts one provider can hold (the server's cap).
pub const MAX_ACCOUNTS_PER_PROVIDER: usize = 10;

/// Where one account stands, as the server reports it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AccountState {
    Ok,
    NeedsReauth,
    RateLimited,
    QuotaExhausted,
    Disabled,
    /// A status this console does not know (a newer server).
    Unknown,
}

impl AccountState {
    pub fn parse(status: &str) -> Self {
        match status {
            "ok" => Self::Ok,
            "needs_reauth" => Self::NeedsReauth,
            "rate_limited" => Self::RateLimited,
            "quota_exhausted" => Self::QuotaExhausted,
            "disabled" => Self::Disabled,
            _ => Self::Unknown,
        }
    }

    /// The chip word.
    pub fn label(self) -> &'static str {
        match self {
            Self::Ok => "Ready",
            Self::NeedsReauth => "Sign in again",
            Self::RateLimited => "Cooling down",
            Self::QuotaExhausted => "Out of quota",
            Self::Disabled => "Off",
            Self::Unknown => "Unknown",
        }
    }

    pub fn tone(self) -> Tone {
        match self {
            Self::Ok => Tone::Ok,
            Self::NeedsReauth => Tone::Danger,
            Self::RateLimited | Self::QuotaExhausted => Tone::Warn,
            Self::Disabled | Self::Unknown => Tone::Muted,
        }
    }

    /// One line that says what to do, or why it waits.
    pub fn hint(self) -> Option<&'static str> {
        match self {
            Self::NeedsReauth => Some(
                "The provider no longer accepts this account. Sign in again, or replace the key.",
            ),
            Self::RateLimited => {
                Some("The provider asked this account to slow down. It is used again soon.")
            }
            Self::QuotaExhausted => {
                Some("This account used its allowance. It is used again after the reset.")
            }
            Self::Disabled => Some("Switched off. Bots skip it until you turn it on."),
            Self::Ok | Self::Unknown => None,
        }
    }

    /// Whether a bot can use the account right now.
    pub fn usable(self) -> bool {
        matches!(self, Self::Ok)
    }
}

pub fn state_of(account: &ProviderAccount) -> AccountState {
    AccountState::parse(&account.status)
}

/// How the account signs in, in plain words.
pub fn auth_kind_label(kind: &str) -> &'static str {
    match kind {
        "api_key" => "API key",
        "oauth" => "Signed in",
        "device" => "Device sign-in",
        "minted" => "Signed in",
        _ => "Account",
    }
}

/// `p***@example.com · Pro`: what tells two accounts apart. The server masks
/// the email; the console never unmasks it.
pub fn identity_line(account: &ProviderAccount) -> Option<String> {
    let identity = &account.identity;
    let parts: Vec<&str> = [
        identity.email.as_deref(),
        identity.plan.as_deref(),
        identity.tier.as_deref(),
        identity.org.as_deref(),
    ]
    .into_iter()
    .flatten()
    .map(str::trim)
    .filter(|part| !part.is_empty())
    .collect();
    (!parts.is_empty()).then(|| parts.join(" \u{b7} "))
}

/// The account's name in a list: its label, else its kind.
pub fn display_label(account: &ProviderAccount) -> String {
    let label = account.label.trim();
    if label.is_empty() {
        auth_kind_label(&account.auth_kind).to_owned()
    } else {
        label.to_owned()
    }
}

/// A quota bar: how much of the tightest window is used.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct QuotaMeter {
    /// 0 to 100, of the window that has the least left.
    pub percent_used: u32,
    /// `37% of 5h used`.
    pub label: String,
    /// When it resets, when the provider said.
    pub resets_at: Option<String>,
    pub tone: Tone,
}

/// What the quota snapshot says, if anything.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum QuotaView {
    Meter(QuotaMeter),
    /// The provider says the plan is inactive or needs payment.
    Note(String),
}

/// Read the server's quota snapshot (`{state, windows:[{label, used_percent,
/// remaining, resets_at}]}`). Anything else is "no reading": `None`.
pub fn quota_view(quota: &Value) -> Option<QuotaView> {
    match quota.get("state").and_then(Value::as_str) {
        Some("inactive") => {
            return Some(QuotaView::Note(
                "The subscription is not active.".to_owned(),
            ));
        }
        Some("payment_required") => {
            return Some(QuotaView::Note(
                "The provider needs a payment before this account works.".to_owned(),
            ));
        }
        _ => {}
    }
    let windows = quota.get("windows")?.as_array()?;
    // The window with the least left is the one that will stop a turn first.
    let tightest = windows
        .iter()
        .filter_map(|window| {
            let used = window_used_percent(window)?;
            Some((used, window))
        })
        .max_by(|a, b| a.0.total_cmp(&b.0))?;
    let (used, window) = tightest;
    let percent_used = used.round().clamp(0.0, 100.0) as u32;
    let scope = window
        .get("label")
        .or_else(|| window.get("scope"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|text| !text.is_empty() && text.len() <= 32 && !text.chars().any(char::is_control));
    let label = match scope {
        Some(scope) => format!("{percent_used}% of {scope} used"),
        None => format!("{percent_used}% used"),
    };
    let resets_at = window
        .get("resets_at")
        .and_then(Value::as_str)
        .filter(|text| text.len() <= 40)
        .map(str::to_owned);
    let tone = if percent_used >= 100 {
        Tone::Danger
    } else if percent_used >= 80 {
        Tone::Warn
    } else {
        Tone::Ok
    };
    Some(QuotaView::Meter(QuotaMeter {
        percent_used,
        label,
        resets_at,
        tone,
    }))
}

fn window_used_percent(window: &Value) -> Option<f64> {
    if let Some(used) = window.get("used_percent").and_then(Value::as_f64) {
        return Some(used.clamp(0.0, 100.0));
    }
    let remaining = window.get("remaining").and_then(Value::as_f64)?;
    // `remaining` is a fraction from 0 to 1.
    Some(((1.0 - remaining) * 100.0).clamp(0.0, 100.0))
}

/// The accounts in the order bots try them: highest priority first; the
/// virtual `default` account, then the label, break a tie.
pub fn ordered(accounts: &[ProviderAccount]) -> Vec<&ProviderAccount> {
    let mut list: Vec<&ProviderAccount> = accounts.iter().collect();
    list.sort_by(|a, b| {
        b.priority
            .cmp(&a.priority)
            .then_with(|| {
                display_label(a)
                    .to_lowercase()
                    .cmp(&display_label(b).to_lowercase())
            })
            .then_with(|| a.id.cmp(&b.id))
    });
    list
}

/// Which way a row moves in the priority order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Move {
    Up,
    Down,
}

/// The priority writes that move one account a step up or down. Rows that
/// already differ swap priorities; when neighbours tie, the whole list is
/// renumbered so the order the member sees is the order the server keeps.
/// Empty when the account is already at that end or is not in the list.
pub fn priority_moves(
    accounts: &[ProviderAccount],
    id: &str,
    direction: Move,
) -> Vec<(String, i64)> {
    let list = ordered(accounts);
    let Some(at) = list.iter().position(|account| account.id == id) else {
        return Vec::new();
    };
    let to = match direction {
        Move::Up if at > 0 => at - 1,
        Move::Down if at + 1 < list.len() => at + 1,
        _ => return Vec::new(),
    };
    let ties = list
        .windows(2)
        .any(|pair| pair[0].priority == pair[1].priority);
    if !ties {
        let (a, b) = (list[at], list[to]);
        return vec![(a.id.clone(), b.priority), (b.id.clone(), a.priority)];
    }
    let mut order: Vec<&ProviderAccount> = list;
    order.swap(at, to);
    let count = order.len() as i64;
    order
        .iter()
        .enumerate()
        .map(|(index, account)| (account.id.clone(), count - index as i64))
        .filter(|(account_id, wanted)| {
            accounts
                .iter()
                .find(|account| &account.id == account_id)
                .is_none_or(|account| account.priority != *wanted)
        })
        .collect()
}

/// One way to add an account.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AddMethod {
    /// Paste an API key.
    Key,
    /// Sign in through the browser (PKCE, loopback redirect).
    Browser,
    /// Type a one-time code on the provider's page.
    Device,
}

impl AddMethod {
    pub fn label(self) -> &'static str {
        match self {
            Self::Key => "API key",
            Self::Browser => "Sign in with browser",
            Self::Device => "Sign in with a code",
        }
    }

    /// The `flow` word the server takes; `None` for a pasted key.
    pub fn flow(self) -> Option<&'static str> {
        match self {
            Self::Key => None,
            Self::Browser => Some("pkce"),
            Self::Device => Some("device"),
        }
    }
}

/// The ways to add an account to `provider`. The server's own list of
/// sign-in flows wins; an older server that sends none falls back to what
/// the preset documents. A sign-in-only provider offers no key.
pub fn add_methods(provider: &ProviderStatus, preset: Option<&ProviderPreset>) -> Vec<AddMethod> {
    let mut methods = Vec::new();
    let sign_in_only = provider.auth == "oauth" || preset.is_some_and(|p| p.auth == "oauth");
    let takes_key = !sign_in_only && preset.is_none_or(|p| p.auth != "none");
    if takes_key {
        methods.push(AddMethod::Key);
    }
    let flows: Vec<&str> = if provider.signin.is_empty() {
        match preset.map(|p| p.sign_in) {
            Some("pkce" | "pkce_google") => vec!["pkce"],
            Some("device") => vec!["device"],
            _ => Vec::new(),
        }
    } else {
        provider.signin.iter().map(String::as_str).collect()
    };
    for flow in flows {
        match flow {
            "pkce" => methods.push(AddMethod::Browser),
            "device" => methods.push(AddMethod::Device),
            _ => {}
        }
    }
    methods
}

/// The risk line to show beside a plan sign-in. The server's own text wins;
/// otherwise the preset's, and only when a sign-in is actually offered.
pub fn risk_text(provider: &ProviderStatus, preset: Option<&ProviderPreset>) -> Option<String> {
    if let Some(text) = provider
        .signin_risk
        .as_deref()
        .map(str::trim)
        .filter(|text| !text.is_empty())
    {
        return Some(text.to_owned());
    }
    let sign_in_offered = add_methods(provider, preset)
        .iter()
        .any(|method| *method != AddMethod::Key);
    let text = preset.map(|p| p.risk).filter(|text| !text.is_empty())?;
    sign_in_offered.then(|| text.to_owned())
}

/// Whether the server refused a Remove because bots pin the account. It says
/// so in plain words; the page then offers "Remove anyway".
pub fn is_pinned_refusal(message: &str) -> bool {
    message.contains("Remove it anyway")
}

/// Whether one more account fits under the server's cap.
pub fn can_add(provider: &ProviderStatus) -> bool {
    provider.accounts.len() < MAX_ACCOUNTS_PER_PROVIDER
}

/// A short count line for the card: `2 accounts, 1 ready`.
pub fn summary_line(accounts: &[ProviderAccount]) -> String {
    let ready = accounts.iter().filter(|a| state_of(a).usable()).count();
    let noun = if accounts.len() == 1 {
        "account"
    } else {
        "accounts"
    };
    format!("{} {noun}, {ready} ready", accounts.len())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider_presets::preset_for;
    use serde_json::json;

    fn account(id: &str, label: &str, priority: i64, status: &str) -> ProviderAccount {
        ProviderAccount {
            id: id.into(),
            label: label.into(),
            auth_kind: "api_key".into(),
            status: status.into(),
            priority,
            ..ProviderAccount::default()
        }
    }

    fn provider(auth: &str, signin: &[&str]) -> ProviderStatus {
        ProviderStatus {
            id: "p".into(),
            kind: "k".into(),
            auth: auth.into(),
            signin: signin.iter().map(|s| (*s).to_owned()).collect(),
            ..ProviderStatus::default()
        }
    }

    #[test]
    fn statuses_read_in_plain_words_and_only_ready_is_usable() {
        for (word, label, usable) in [
            ("ok", "Ready", true),
            ("needs_reauth", "Sign in again", false),
            ("rate_limited", "Cooling down", false),
            ("quota_exhausted", "Out of quota", false),
            ("disabled", "Off", false),
            ("from_the_future", "Unknown", false),
        ] {
            let state = AccountState::parse(word);
            assert_eq!(state.label(), label, "{word}");
            assert_eq!(state.usable(), usable, "{word}");
        }
        assert_eq!(AccountState::NeedsReauth.tone(), Tone::Danger);
        assert!(
            AccountState::NeedsReauth
                .hint()
                .unwrap()
                .contains("Sign in again")
        );
        assert!(AccountState::Ok.hint().is_none());
    }

    #[test]
    fn identity_joins_the_masked_email_and_plan() {
        let mut one = account("a", "Work", 0, "ok");
        assert_eq!(identity_line(&one), None);
        one.identity.email = Some("p***@example.com".into());
        one.identity.plan = Some("Pro".into());
        assert_eq!(
            identity_line(&one).as_deref(),
            Some("p***@example.com \u{b7} Pro")
        );
        one.label = "  ".into();
        assert_eq!(display_label(&one), "API key");
    }

    #[test]
    fn the_quota_meter_shows_the_tightest_window() {
        let quota = json!({"state":"ok","windows":[
            {"label":"5h","used_percent":37.5,"remaining":0.625,"resets_at":"2099-01-01T00:00:00Z"},
            {"label":"week","remaining":0.1,"resets_at":"2099-01-02T00:00:00Z"}
        ]});
        let Some(QuotaView::Meter(meter)) = quota_view(&quota) else {
            panic!("a meter");
        };
        assert_eq!(meter.percent_used, 90);
        assert_eq!(meter.label, "90% of week used");
        assert_eq!(meter.tone, Tone::Warn);
        assert_eq!(meter.resets_at.as_deref(), Some("2099-01-02T00:00:00Z"));

        let spent = json!({"windows":[{"remaining":0.0}]});
        let Some(QuotaView::Meter(spent)) = quota_view(&spent) else {
            panic!("a meter");
        };
        assert_eq!((spent.percent_used, spent.tone), (100, Tone::Danger));
        assert_eq!(spent.label, "100% used");

        assert!(matches!(
            quota_view(&json!({"state":"payment_required"})),
            Some(QuotaView::Note(text)) if text.contains("payment")
        ));
        assert!(matches!(
            quota_view(&json!({"state":"inactive"})),
            Some(QuotaView::Note(_))
        ));
        // No windows, or nonsense, is "no reading".
        assert_eq!(quota_view(&json!({"state":"ok","windows":[]})), None);
        assert_eq!(quota_view(&json!("text")), None);
        assert_eq!(quota_view(&json!({"windows":[{"label":"5h"}]})), None);
    }

    #[test]
    fn accounts_sort_by_priority_then_name() {
        let list = [
            account("b", "Beta", 1, "ok"),
            account("a", "Alpha", 1, "ok"),
            account("c", "Top", 5, "ok"),
        ];
        let ids: Vec<&str> = ordered(&list).iter().map(|a| a.id.as_str()).collect();
        assert_eq!(ids, ["c", "a", "b"]);
    }

    #[test]
    fn moving_a_row_swaps_priorities_or_renumbers_a_tie() {
        let apart = [
            account("a", "A", 3, "ok"),
            account("b", "B", 2, "ok"),
            account("c", "C", 1, "ok"),
        ];
        assert_eq!(
            priority_moves(&apart, "b", Move::Up),
            vec![("b".to_owned(), 3), ("a".to_owned(), 2)]
        );
        assert!(priority_moves(&apart, "a", Move::Up).is_empty());
        assert!(priority_moves(&apart, "c", Move::Down).is_empty());
        assert!(priority_moves(&apart, "nope", Move::Down).is_empty());

        // Two rows tie at 0: moving one renumbers so the order is exact.
        let tied = [
            account("a", "A", 0, "ok"),
            account("b", "B", 0, "ok"),
            account("c", "C", 0, "ok"),
        ];
        let writes = priority_moves(&tied, "c", Move::Up);
        let mut next: Vec<(String, i64)> =
            tied.iter().map(|a| (a.id.clone(), a.priority)).collect();
        for (id, priority) in &writes {
            next.iter_mut().find(|(row, _)| row == id).unwrap().1 = *priority;
        }
        next.sort_by_key(|(_, priority)| std::cmp::Reverse(*priority));
        let order: Vec<&str> = next.iter().map(|(id, _)| id.as_str()).collect();
        assert_eq!(order, ["a", "c", "b"]);
    }

    #[test]
    fn add_methods_follow_the_server_then_the_preset() {
        // Key-only provider.
        let plain = provider("api_key", &[]);
        assert_eq!(add_methods(&plain, preset_for("openai")), [AddMethod::Key]);
        // OpenRouter: a key or a browser sign-in.
        assert_eq!(
            add_methods(&provider("api_key", &["pkce"]), preset_for("openrouter")),
            [AddMethod::Key, AddMethod::Browser]
        );
        // Antigravity: sign-in only, no key box, even on an older server.
        assert_eq!(
            add_methods(&provider("oauth", &[]), preset_for("antigravity")),
            [AddMethod::Browser]
        );
        // Muse: a device code only.
        assert_eq!(
            add_methods(&provider("oauth", &["device"]), preset_for("muse-code")),
            [AddMethod::Device]
        );
        // Claude: keys, and the plan sign-in only when the server offers it.
        assert_eq!(
            add_methods(&provider("api_key", &[]), preset_for("anthropic")),
            [AddMethod::Key]
        );
        assert_eq!(
            add_methods(&provider("api_key", &["pkce"]), preset_for("anthropic")),
            [AddMethod::Key, AddMethod::Browser]
        );
        // A local server needs nothing.
        assert!(add_methods(&provider("none", &[]), preset_for("ollama")).is_empty());
        assert_eq!(AddMethod::Device.flow(), Some("device"));
        assert_eq!(AddMethod::Key.flow(), None);
    }

    #[test]
    fn a_plan_sign_in_carries_its_risk_and_a_key_does_not() {
        use crate::provider_presets::{PLAN_RISK_CLAUDE, PLAN_RISK_GOOGLE};
        // Antigravity always rides on a Google consumer client.
        assert_eq!(
            risk_text(&provider("oauth", &["pkce"]), preset_for("antigravity")).as_deref(),
            Some(PLAN_RISK_GOOGLE)
        );
        // Claude shows its risk only while the plan sign-in is on offer.
        assert_eq!(
            risk_text(&provider("api_key", &[]), preset_for("anthropic")),
            None
        );
        assert_eq!(
            risk_text(&provider("api_key", &["pkce"]), preset_for("anthropic")).as_deref(),
            Some(PLAN_RISK_CLAUDE)
        );
        // The server's own sentence wins.
        let mut sent = provider("api_key", &["pkce"]);
        sent.signin_risk = Some("Server words.".into());
        assert_eq!(
            risk_text(&sent, preset_for("anthropic")).as_deref(),
            Some("Server words.")
        );
        // OpenRouter and key-only providers carry none.
        assert_eq!(
            risk_text(&provider("api_key", &["pkce"]), preset_for("openrouter")),
            None
        );
        assert_eq!(
            risk_text(&provider("api_key", &[]), preset_for("openai")),
            None
        );
    }

    #[test]
    fn a_pinned_refusal_is_recognised_by_its_own_words() {
        assert!(is_pinned_refusal(
            "These bots use this account: notes. Remove it anyway to let them use any account."
        ));
        assert!(!is_pinned_refusal("That account is gone."));
    }

    #[test]
    fn the_cap_and_the_summary_line() {
        let mut full = provider("api_key", &[]);
        assert!(can_add(&full));
        full.accounts = (0..MAX_ACCOUNTS_PER_PROVIDER)
            .map(|n| account(&format!("a{n}"), "x", 0, "ok"))
            .collect();
        assert!(!can_add(&full));
        assert_eq!(
            summary_line(&[account("a", "A", 0, "ok"), account("b", "B", 0, "disabled")]),
            "2 accounts, 1 ready"
        );
        assert_eq!(
            summary_line(&[account("a", "A", 0, "ok")]),
            "1 account, 1 ready"
        );
    }
}
