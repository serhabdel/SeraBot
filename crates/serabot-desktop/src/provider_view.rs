//! Plain-word view logic for the Providers page and the model picker.
//!
//! Pure functions, no I/O and no UI, so the wording and the grouping are
//! tested on every platform.

use crate::api::{CatalogEntry, ProviderStatus, ProviderTest};
use crate::provider_presets::{ProviderPreset, presets, provider_matches_preset};
use crate::theme::Tone;
use std::collections::BTreeSet;

/// How a provider card describes the connection.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Connection {
    Connected,
    NotConnected,
    KeyRejected,
    /// A sign-in provider (browser or device flow) with no sign-in yet.
    NotSignedIn,
    SignedIn,
}

impl Connection {
    /// The pill text, in plain words.
    pub fn label(self) -> &'static str {
        match self {
            Self::Connected => "Connected",
            Self::NotConnected => "Not connected",
            Self::KeyRejected => "Key rejected",
            Self::NotSignedIn => "Not signed in",
            Self::SignedIn => "Signed in",
        }
    }

    /// The pill style: `connected`, `disconnected` or `rejected`.
    pub fn class(self) -> &'static str {
        match self {
            Self::Connected | Self::SignedIn => "status-pill connected",
            Self::NotConnected | Self::NotSignedIn => "status-pill disconnected",
            Self::KeyRejected => "status-pill rejected",
        }
    }

    pub fn is_connected(self) -> bool {
        matches!(self, Self::Connected | Self::SignedIn)
    }

    /// Status tone (colour + icon pairing) for `StatusChip`.
    pub fn tone(self) -> Tone {
        match self {
            Self::Connected | Self::SignedIn => Tone::Ok,
            Self::NotConnected | Self::NotSignedIn => Tone::Warn,
            Self::KeyRejected => Tone::Danger,
        }
    }
}

/// Human-friendly provider kind label.
pub fn human_kind(kind: &str) -> &str {
    match kind {
        "openai" => "OpenAI",
        "openai-codex" => "ChatGPT (Codex)",
        "github-copilot" => "GitHub Copilot",
        "openai-compatible" => "OpenAI-compatible",
        "anthropic" => "Anthropic",
        "google" | "gemini" => "Google Gemini",
        "google-antigravity" => "Google Cloud Code",
        "mistral" => "Mistral AI",
        "groq" => "Groq",
        "deepseek" => "DeepSeek",
        "openrouter" => "OpenRouter",
        "ollama" => "Ollama (Local)",
        "together" => "Together AI",
        "fireworks" => "Fireworks AI",
        "perplexity" => "Perplexity",
        "xai" => "xAI",
        "minimax" => "MiniMax",
        other => other,
    }
}

/// Render a `ProviderTest` result as a status tone and plain-word summary.
pub fn format_provider_test(test: &ProviderTest) -> (Tone, String) {
    if test.ok {
        let summary = if test.model_count > 0 {
            let noun = if test.model_count == 1 {
                "model"
            } else {
                "models"
            };
            format!("Check passed \u{b7} {} live {noun} found", test.model_count)
        } else {
            "Check passed".to_owned()
        };
        (Tone::Ok, summary)
    } else {
        let message = match test.status.as_str() {
            "key_rejected" => "Provider rejected the key.".to_owned(),
            "unreachable" => "Provider did not answer.".to_owned(),
            "" => "Check failed.".to_owned(),
            other => format!("Check failed: {}", other.replace('_', " ")),
        };
        (Tone::Danger, message)
    }
}

/// Inline validation result for the "Add provider" sheet.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ProviderDraftCheck {
    pub id_error: Option<&'static str>,
    pub kind_error: Option<&'static str>,
    pub base_url_error: Option<&'static str>,
    pub models_error: Option<&'static str>,
    pub secret_error: Option<&'static str>,
    pub duplicate_id_warning: bool,
}

impl ProviderDraftCheck {
    pub fn is_valid(&self) -> bool {
        self.id_error.is_none()
            && self.kind_error.is_none()
            && self.base_url_error.is_none()
            && self.models_error.is_none()
            && self.secret_error.is_none()
    }
}

/// Validate provider draft fields inline before calling `Session::save_provider`.
pub fn validate_provider_draft(
    id: &str,
    kind: &str,
    base_url: &str,
    models_raw: &str,
    secret_name: &str,
    existing_ids: &[&str],
) -> ProviderDraftCheck {
    let id = id.trim();
    let kind = kind.trim();
    let base_url = base_url.trim();
    let secret = secret_name.trim();
    let models: Vec<&str> = models_raw
        .split([',', '\n'])
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect();

    let id_error = (!crate::valid_provider_id(id))
        .then_some("Provider id must be 1\u{2013}64 letters, digits, '_' or '-'.");
    let kind_error = (kind.is_empty() || kind.len() > 32).then_some("Choose a provider kind.");
    let base_url_error = if base_url.is_empty() || base_url.len() > 2_048 {
        Some("Enter the provider base URL.")
    } else if !url::Url::parse(base_url)
        .is_ok_and(|u| matches!(u.scheme(), "http" | "https") && u.host_str().is_some())
    {
        Some("Enter a valid http:// or https:// URL.")
    } else {
        None
    };
    let models_error = if models.is_empty() || models.len() > 256 {
        Some("List at least one model id.")
    } else if models.iter().any(|m| !crate::valid_model_id(m)) {
        Some("Model ids cannot be empty or contain control characters.")
    } else if models.iter().collect::<BTreeSet<_>>().len() != models.len() {
        Some("Each model id can be listed once.")
    } else {
        None
    };
    let secret_error = (!crate::valid_secret_name(secret))
        .then_some("Secret name must be 1\u{2013}128 letters, digits, '_' or '-'.");
    let duplicate_id_warning = !id.is_empty() && existing_ids.contains(&id);

    ProviderDraftCheck {
        id_error,
        kind_error,
        base_url_error,
        models_error,
        secret_error,
        duplicate_id_warning,
    }
}

/// Decide what a card shows. `oauth_signed_in` is the server-side sign-in
/// state of an OAuth provider (Antigravity, Muse); every other provider is
/// judged by the account's own credential and the last model-list fetch.
pub fn connection(provider: &ProviderStatus, oauth_signed_in: bool) -> Connection {
    // Accounts decide first: one that works is a connection; accounts that
    // all need a new key or sign-in say so, in the provider's own words.
    if !provider.accounts.is_empty() {
        let state =
            |account: &crate::api::ProviderAccount| crate::provider_accounts::state_of(account);
        let any_usable = provider
            .accounts
            .iter()
            .any(|account| state(account).usable());
        let any_stale = provider
            .accounts
            .iter()
            .any(|account| state(account) == crate::provider_accounts::AccountState::NeedsReauth);
        let signs_in = provider.auth == "oauth";
        return match (any_usable, any_stale, signs_in) {
            (true, _, true) => Connection::SignedIn,
            (true, _, false) => Connection::Connected,
            (false, true, true) => Connection::NotSignedIn,
            (false, true, false) => Connection::KeyRejected,
            (false, false, true) => Connection::SignedIn,
            (false, false, false) => Connection::Connected,
        };
    }
    if provider.auth == "oauth" {
        return if oauth_signed_in {
            Connection::SignedIn
        } else {
            Connection::NotSignedIn
        };
    }
    if !provider.credential.configured {
        return Connection::NotConnected;
    }
    if provider.status == "key_rejected" {
        return Connection::KeyRejected;
    }
    Connection::Connected
}

/// One short line under the card title that says where the models come from.
pub fn model_line(provider: &ProviderStatus) -> String {
    let count = provider.model_count.max(provider.models.len());
    let noun = if count == 1 { "model" } else { "models" };
    match (provider.model_source.as_str(), provider.status.as_str()) {
        ("live", _) => format!("{count} {noun}, live from the provider"),
        (_, "key_rejected") => "The provider refused the key. Paste a new one.".to_owned(),
        (_, "unreachable") => {
            format!("{count} {noun} from the saved list. The provider did not answer.")
        }
        _ if !provider.credential.configured => format!("{count} {noun} in the saved list"),
        _ => format!("{count} {noun} in the saved list"),
    }
}

/// A readable provider name: the brand label for its kind, else the id.
pub fn display_name(id: &str, kind: &str, brand_label: Option<&str>) -> String {
    match brand_label {
        Some(label) if id == "default" || id == kind => label.to_owned(),
        _ => id.to_owned(),
    }
}

/// `131072` becomes `128K`, `1000000` becomes `1M`.
pub fn format_context(tokens: u64) -> String {
    if tokens >= 1_000_000 {
        let tenths = tokens * 10 / 1_000_000;
        if tenths.is_multiple_of(10) {
            format!("{}M", tenths / 10)
        } else {
            format!("{}.{}M", tenths / 10, tenths % 10)
        }
    } else if tokens >= 1_000 {
        format!("{}K", (tokens + 512) / 1_024)
    } else {
        tokens.to_string()
    }
}

/// Friendly tag text for a capability tag the server sends.
pub fn tag_label(tag: &str) -> Option<&'static str> {
    match tag {
        "vision" => Some("Vision"),
        "tools" => Some("Tools"),
        "reasoning" => Some("Reasoning"),
        "free" => Some("Free"),
        "cheap" => Some("Low cost"),
        _ => None,
    }
}

/// One row of the model picker.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PickerRow {
    /// `provider|model`, the value the picker stores.
    pub value: String,
    pub provider: String,
    /// The provider's own friendly name when it gave one, else a tidied id.
    pub title: String,
    /// The raw model id, shown small when it differs from the title.
    pub model: String,
    pub context: Option<String>,
    pub tags: Vec<&'static str>,
    /// The thinking levels the model accepts, weakest first.
    pub thinking: Vec<String>,
    /// The member's usable account ids for this provider, best first.
    pub account_ids: Vec<String>,
}

/// Rows for the picker, in server order, each tagged with its provider so the
/// view can draw a group header whenever the provider changes.
pub fn picker_rows(entries: &[CatalogEntry], tidy: impl Fn(&str) -> String) -> Vec<PickerRow> {
    entries
        .iter()
        .map(|entry| PickerRow {
            value: format!("{}|{}", entry.provider, entry.model),
            provider: entry.provider.clone(),
            title: entry.name.clone().unwrap_or_else(|| tidy(&entry.model)),
            model: entry.model.clone(),
            context: entry.context_length.map(format_context),
            tags: entry.tags.iter().filter_map(|tag| tag_label(tag)).collect(),
            thinking: entry.thinking.clone(),
            account_ids: entry.account_ids.clone(),
        })
        .collect()
}

/// Whether a row matches the picker's search box: any part of the title, the
/// raw id, the provider id or a tag.
pub fn row_matches(row: &PickerRow, needle: &str) -> bool {
    let needle = needle.trim().to_lowercase();
    needle.is_empty()
        || row.title.to_lowercase().contains(&needle)
        || row.model.to_lowercase().contains(&needle)
        || row.provider.to_lowercase().contains(&needle)
        || row
            .tags
            .iter()
            .any(|tag| tag.to_lowercase().contains(&needle))
}

/// The preset a saved provider was made from: same id first, then the same
/// kind (a generic OpenAI-compatible kind never matches by kind alone).
pub fn preset_of(provider: &ProviderStatus) -> Option<&'static ProviderPreset> {
    presets()
        .iter()
        .find(|preset| preset.id == provider.id)
        .or_else(|| {
            presets()
                .iter()
                .find(|preset| provider_matches_preset(&provider.id, &provider.kind, preset))
        })
}

/// What a provider card is called: the preset's name when the provider is the
/// preset's own (saved under its default id), the brand for a server's
/// built-in provider, and the owner's own id for anything they named.
pub fn card_title(provider: &ProviderStatus, brand_label: Option<&str>) -> String {
    if let Some(preset) = preset_of(provider)
        && (provider.id == preset.id
            || provider.id == preset.default_id
            || provider.id == provider.kind)
    {
        return preset.label.to_owned();
    }
    match brand_label {
        Some(label) if provider.id == "default" || provider.id == provider.kind => label.to_owned(),
        _ if provider.id == "default" => human_kind(&provider.kind).to_owned(),
        _ => provider.id.clone(),
    }
}

/// Whether the member can use this provider now: a working account, the
/// server's sign-in, or a key.
pub fn provider_usable(provider: &ProviderStatus, oauth_signed_in: bool) -> bool {
    connection(provider, oauth_signed_in).is_connected()
}

/// The order of the cards: the default first, then the ones that work, then
/// the rest, each group by title.
pub fn card_order(
    providers: &[ProviderStatus],
    usable: impl Fn(&ProviderStatus) -> bool,
) -> Vec<usize> {
    let mut order: Vec<usize> = (0..providers.len()).collect();
    order.sort_by_key(|&index| {
        let provider = &providers[index];
        (
            !provider.is_default,
            !usable(provider),
            card_title(provider, None).to_lowercase(),
        )
    });
    order
}

/// Whether a preset adds an account by signing in (on its own or beside a key).
pub fn preset_signs_in(preset: &ProviderPreset) -> bool {
    preset.auth == "oauth" || !preset.sign_in.is_empty()
}

/// The presets the admin can still add, split into sign-in presets and key
/// (or local, or custom) presets. A preset already saved on the server is a
/// card above, so it is not offered twice. `query` matches name, summary, kind
/// and model ids.
pub fn presets_to_add(
    saved: &[ProviderStatus],
    query: &str,
) -> (Vec<&'static ProviderPreset>, Vec<&'static ProviderPreset>) {
    let needle = query.trim().to_lowercase();
    let matches = |preset: &ProviderPreset| {
        needle.is_empty()
            || preset.label.to_lowercase().contains(&needle)
            || preset.subtitle.to_lowercase().contains(&needle)
            || preset.kind.to_lowercase().contains(&needle)
            || preset
                .models
                .iter()
                .any(|model| model.to_lowercase().contains(&needle))
    };
    let unsaved = |preset: &ProviderPreset| {
        preset.id == "custom"
            || !saved
                .iter()
                .any(|provider| provider_matches_preset(&provider.id, &provider.kind, preset))
    };
    presets()
        .iter()
        .filter(|preset| unsaved(preset) && matches(preset))
        .partition(|preset| preset_signs_in(preset))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::CredentialState;

    fn provider(auth: &str, configured: bool, status: &str, source: &str) -> ProviderStatus {
        ProviderStatus {
            id: "mistral".into(),
            kind: "mistral".into(),
            auth: auth.into(),
            status: status.into(),
            model_source: source.into(),
            credential: CredentialState {
                configured,
                own: configured,
                granted: false,
            },
            ..ProviderStatus::default()
        }
    }

    #[test]
    fn connection_wording_is_plain_and_honest() {
        let cases = [
            (
                provider("api_key", false, "unchecked", "fallback"),
                false,
                "Not connected",
            ),
            (provider("api_key", true, "ok", "live"), false, "Connected"),
            (
                provider("api_key", true, "key_rejected", "fallback"),
                false,
                "Key rejected",
            ),
            (
                provider("api_key", true, "unreachable", "fallback"),
                false,
                "Connected",
            ),
            (
                provider("oauth", false, "unchecked", "fallback"),
                false,
                "Not signed in",
            ),
            (
                provider("oauth", false, "unchecked", "fallback"),
                true,
                "Signed in",
            ),
        ];
        for (provider, signed_in, expected) in cases {
            assert_eq!(connection(&provider, signed_in).label(), expected);
        }
        assert_eq!(Connection::KeyRejected.class(), "status-pill rejected");
        assert!(!Connection::KeyRejected.is_connected());
    }

    #[test]
    fn accounts_decide_the_connection_before_the_legacy_key() {
        use crate::api::ProviderAccount;
        let with = |auth: &str, statuses: &[&str]| {
            let mut p = provider(auth, false, "unchecked", "fallback");
            p.accounts = statuses
                .iter()
                .enumerate()
                .map(|(n, status)| ProviderAccount {
                    id: format!("a{n}"),
                    status: (*status).into(),
                    ..ProviderAccount::default()
                })
                .collect();
            p
        };
        // One working account is a connection even with no legacy key.
        assert_eq!(
            connection(&with("api_key", &["ok", "needs_reauth"]), false).label(),
            "Connected"
        );
        assert_eq!(
            connection(&with("oauth", &["ok"]), false).label(),
            "Signed in"
        );
        // Every account stale: ask for a new key or a new sign-in.
        assert_eq!(
            connection(&with("api_key", &["needs_reauth"]), false).label(),
            "Key rejected"
        );
        assert_eq!(
            connection(&with("oauth", &["needs_reauth"]), false).label(),
            "Not signed in"
        );
        // Cooling or out of quota is still a connection; the row says why.
        assert_eq!(
            connection(&with("api_key", &["rate_limited", "disabled"]), false).label(),
            "Connected"
        );
    }

    #[test]
    fn model_line_says_where_the_list_came_from() {
        let mut live = provider("api_key", true, "ok", "live");
        live.model_count = 42;
        assert_eq!(model_line(&live), "42 models, live from the provider");
        let mut one = provider("api_key", true, "ok", "live");
        one.model_count = 1;
        assert_eq!(model_line(&one), "1 model, live from the provider");
        let mut down = provider("api_key", true, "unreachable", "fallback");
        down.model_count = 6;
        assert!(model_line(&down).contains("did not answer"));
        let rejected = provider("api_key", true, "key_rejected", "fallback");
        assert!(model_line(&rejected).contains("refused the key"));
    }

    #[test]
    fn names_and_context_sizes_read_naturally() {
        assert_eq!(
            display_name("default", "mistral", Some("Mistral AI")),
            "Mistral AI"
        );
        assert_eq!(
            display_name("mistral", "mistral", Some("Mistral AI")),
            "Mistral AI"
        );
        assert_eq!(display_name("work", "mistral", Some("Mistral AI")), "work");
        assert_eq!(display_name("default", "weird", None), "default");
        assert_eq!(format_context(131_072), "128K");
        assert_eq!(format_context(32_000), "31K");
        assert_eq!(format_context(1_000_000), "1M");
        assert_eq!(format_context(2_500_000), "2.5M");
        assert_eq!(format_context(512), "512");
    }

    #[test]
    fn picker_rows_carry_names_context_tags_and_search() {
        let entries = vec![
            CatalogEntry {
                provider: "default".into(),
                model: "mistral-large-latest".into(),
                data_egress_approved: true,
                name: Some("Mistral Large 3".into()),
                context_length: Some(131_072),
                tags: vec!["vision".into(), "tools".into(), "mystery".into()],
                thinking: Vec::new(),
                account_ids: Vec::new(),
            },
            CatalogEntry {
                provider: "default".into(),
                model: "codestral-latest".into(),
                data_egress_approved: true,
                name: None,
                context_length: None,
                tags: Vec::new(),
                thinking: Vec::new(),
                account_ids: Vec::new(),
            },
        ];
        let rows = picker_rows(&entries, |model| model.replace('-', " "));
        assert_eq!(rows[0].title, "Mistral Large 3");
        assert_eq!(rows[0].context.as_deref(), Some("128K"));
        assert_eq!(rows[0].tags, vec!["Vision", "Tools"]);
        assert!(rows[0].thinking.is_empty() && rows[0].account_ids.is_empty());
        assert_eq!(rows[0].value, "default|mistral-large-latest");
        assert_eq!(rows[1].title, "codestral latest");
        assert!(rows[1].context.is_none());
        assert!(row_matches(&rows[0], "large 3"));
        assert!(row_matches(&rows[0], "vision"));
        assert!(row_matches(&rows[1], "CODESTRAL"));
        assert!(row_matches(&rows[1], ""));
        assert!(!row_matches(&rows[1], "gpt"));
    }

    #[test]
    fn server_replies_parse_with_the_new_fields_and_without_them() {
        let status: ProviderStatus = serde_json::from_str(
            r#"{"id":"default","kind":"mistral","models":["a"],"auth":"api_key",
                "status":"key_rejected","model_source":"fallback","model_count":1,
                "credential":{"configured":true,"own":true,"granted":false}}"#,
        )
        .unwrap();
        assert_eq!(connection(&status, false).label(), "Key rejected");
        assert!(
            !status.is_default,
            "the star is set by the console, not the server"
        );
        // An older server sends none of the new fields.
        let old: ProviderStatus = serde_json::from_str(r#"{"id":"x","kind":"openai"}"#).unwrap();
        assert_eq!(connection(&old, false).label(), "Not connected");
        let entry: CatalogEntry = serde_json::from_str(
            r#"{"provider":"default","model":"m","name":"M","context_length":8192,"tags":["tools"]}"#,
        )
        .unwrap();
        assert_eq!(entry.context_length, Some(8192));
        let bare: CatalogEntry =
            serde_json::from_str(r#"{"provider":"default","model":"m"}"#).unwrap();
        assert!(bare.tags.is_empty() && bare.name.is_none());
        // The new fields arrive with the account-aware server and default without it.
        let rich: CatalogEntry = serde_json::from_str(
            r#"{"provider":"p","model":"m","thinking_levels":["low"],"thinking":["low"],"account_ids":["a1","a2"]}"#,
        )
        .unwrap();
        assert_eq!(rich.account_ids, ["a1", "a2"]);
        assert_eq!(picker_rows(&[rich], |m| m.to_owned())[0].thinking, ["low"]);
        assert!(bare.account_ids.is_empty());
    }

    #[test]
    fn tones_test_formatting_and_draft_validation() {
        assert_eq!(Connection::Connected.tone(), Tone::Ok);
        assert_eq!(Connection::NotConnected.tone(), Tone::Warn);
        assert_eq!(Connection::KeyRejected.tone(), Tone::Danger);
        assert_eq!(human_kind("openai-compatible"), "OpenAI-compatible");

        let ok = ProviderTest {
            ok: true,
            status: "ok".into(),
            model_count: 12,
        };
        assert_eq!(
            format_provider_test(&ok),
            (
                Tone::Ok,
                "Check passed \u{b7} 12 live models found".to_owned()
            )
        );
        let rejected = ProviderTest {
            ok: false,
            status: "key_rejected".into(),
            model_count: 0,
        };
        assert_eq!(
            format_provider_test(&rejected),
            (Tone::Danger, "Provider rejected the key.".to_owned())
        );

        let valid = validate_provider_draft(
            "work",
            "openai",
            "https://api.openai.com/v1",
            "gpt-5, gpt-5-mini",
            "WORK_API_KEY",
            &["work"],
        );
        assert!(valid.is_valid());
        assert!(valid.duplicate_id_warning);

        let bad = validate_provider_draft("", "openai", "not-a-url", "m1, m1", "bad name", &[]);
        assert!(!bad.is_valid());
        assert!(bad.id_error.is_some());
        assert!(bad.base_url_error.is_some());
        assert!(bad.models_error.is_some());
        assert!(bad.secret_error.is_some());
    }

    fn saved(id: &str, kind: &str) -> ProviderStatus {
        ProviderStatus {
            id: id.into(),
            kind: kind.into(),
            ..ProviderStatus::default()
        }
    }

    #[test]
    fn a_card_is_named_after_its_preset_unless_the_owner_named_it() {
        assert_eq!(
            card_title(&saved("antigravity", "google-antigravity"), None),
            "Google Antigravity"
        );
        assert_eq!(
            card_title(&saved("codex", "openai-codex"), None),
            "ChatGPT (Codex)"
        );
        assert_eq!(
            card_title(&saved("anthropic", "anthropic"), None),
            "Anthropic (Claude)"
        );
        assert_eq!(card_title(&saved("work", "openai"), None), "work");
        assert_eq!(card_title(&saved("default", "xyz"), Some("Brand")), "Brand");
    }

    #[test]
    fn the_add_section_offers_only_presets_not_saved_and_splits_sign_in_from_keys() {
        let providers = vec![saved("codex", "openai-codex"), saved("openai", "openai")];
        let (sign_in, keys) = presets_to_add(&providers, "");
        let ids = |list: &[&ProviderPreset]| list.iter().map(|p| p.id).collect::<Vec<_>>();
        let sign_in = ids(&sign_in);
        let keys = ids(&keys);
        assert!(!sign_in.contains(&"chatgpt-codex"), "{sign_in:?}");
        assert!(sign_in.contains(&"anthropic") && sign_in.contains(&"antigravity"));
        assert!(sign_in.contains(&"github-copilot"));
        assert!(!keys.contains(&"openai") && keys.contains(&"mistral"));
        assert!(
            keys.contains(&"custom"),
            "a custom endpoint can always be added"
        );
        let (sign_in, keys) = presets_to_add(&providers, "claude");
        assert!(ids(&sign_in).contains(&"anthropic"));
        assert!(!ids(&keys).contains(&"mistral"));
    }

    #[test]
    fn cards_put_the_default_first_then_the_working_ones() {
        let mut providers = vec![
            saved("zeta", "openai"),
            saved("alpha", "openai"),
            saved("mid", "openai"),
        ];
        providers[2].is_default = true;
        let order = card_order(&providers, |p| p.id == "zeta");
        assert_eq!(order, vec![2, 0, 1]);
    }
}
