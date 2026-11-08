//! Plain-word view logic for the Providers page and the model picker.
//!
//! Pure functions, no I/O and no UI, so the wording and the grouping are
//! tested on every platform.

use crate::api::{CatalogEntry, ProviderStatus};

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
}

/// Decide what a card shows. `oauth_signed_in` is the server-side sign-in
/// state of an OAuth provider (Antigravity, Muse); every other provider is
/// judged by the account's own credential and the last model-list fetch.
pub fn connection(provider: &ProviderStatus, oauth_signed_in: bool) -> Connection {
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
            },
            CatalogEntry {
                provider: "default".into(),
                model: "codestral-latest".into(),
                data_egress_approved: true,
                name: None,
                context_length: None,
                tags: Vec::new(),
            },
        ];
        let rows = picker_rows(&entries, |model| model.replace('-', " "));
        assert_eq!(rows[0].title, "Mistral Large 3");
        assert_eq!(rows[0].context.as_deref(), Some("128K"));
        assert_eq!(rows[0].tags, vec!["Vision", "Tools"]);
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
    }
}
