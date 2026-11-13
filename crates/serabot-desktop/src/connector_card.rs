//! What a per-user connector card shows, as plain data.
//!
//! The server reports each account's own state for Outlook, Google and Notion
//! (`personal` in `GET /v1/connectors`). This module maps that state, plus the
//! last error from a click, onto the four things a card can be: not set up yet,
//! ready to connect, connected, or failed. It draws nothing and touches no
//! network, so the mapping is unit-tested; `ui_connectors.rs` only renders it.
//!
//! Links and help text are fixed here. The console never opens an address the
//! server sent for a help link.

use crate::{ConnectorStatus, PersonalStatus};

/// Where a Notion user creates an internal integration and copies its token.
pub const NOTION_HELP_URL: &str = "https://www.notion.so/profile/integrations";
/// One line telling a Notion user what to do after connecting.
pub const NOTION_SHARE_HINT: &str = "Then share each page you want your bots to use with the integration (open the page, choose the \u{2026} menu, then Connections).";
/// What a pasted-token card says: the field name, where the user creates the
/// token, and one line on what to do. Fixed in the console; the server never
/// supplies a link the console opens.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TokenHelp {
    pub label: &'static str,
    pub url: &'static str,
    pub hint: &'static str,
}

/// Help for the connectors that connect with a pasted token.
pub fn token_help(id: &str) -> Option<TokenHelp> {
    Some(match id {
        "notion" => TokenHelp {
            label: "Integration token",
            url: NOTION_HELP_URL,
            hint: NOTION_SHARE_HINT,
        },
        "github" => TokenHelp {
            label: "Fine-grained token",
            url: GITHUB_HELP_URL,
            hint: "Limit it to the repositories you want. Give it Issues (read and write) and Contents (read).",
        },
        "todoist" => TokenHelp {
            label: "API token",
            url: TODOIST_HELP_URL,
            hint: "Open Settings, Integrations, Developer and copy your API token.",
        },
        "slack" => TokenHelp {
            label: "Bot token",
            url: SLACK_HELP_URL,
            hint: "Add the bot scopes channels:read, groups:read, channels:history, groups:history and chat:write, install the app, then invite the bot to each channel.",
        },
        "exa" => TokenHelp {
            label: "API key",
            url: EXA_HELP_URL,
            hint: "Create a key in the Exa dashboard. Connecting runs one small search to check it.",
        },
        _ => return None,
    })
}

/// Where an Exa user creates an API key.
pub const EXA_HELP_URL: &str = "https://dashboard.exa.ai/api-keys";
/// Where a Slack user creates an app and copies its bot token.
pub const SLACK_HELP_URL: &str = "https://api.slack.com/apps";
/// Where a Todoist user finds their API token.
pub const TODOIST_HELP_URL: &str = "https://app.todoist.com/app/settings/integrations/developer";
/// Where a GitHub user creates a fine-grained personal access token.
pub const GITHUB_HELP_URL: &str = "https://github.com/settings/personal-access-tokens/new";
/// Shown when the server gave no admin hint.
pub const DEFAULT_ADMIN_HINT: &str = "Ask your admin to set this up.";

/// The Google Cloud console pages an admin needs, fixed in the console.
pub const GOOGLE_CONSOLE_URL: &str = "https://console.cloud.google.com/apis/credentials";

/// Step-by-step text for the admin "Set up Google" card. `{redirect}` is the
/// exact address to register.
pub const GOOGLE_SETUP_STEPS: [&str; 5] = [
    "In Google Cloud Console, create or pick a project.",
    "Under APIs & Services, Library, enable the Google Drive, Google Sheets, Google Calendar and Gmail APIs.",
    "Under APIs & Services, OAuth consent screen, set the app up and add yourself and your users as test users.",
    "Under Credentials, create an OAuth client ID of type Web application and add the redirect address below.",
    "Paste the client ID and client secret here and save. They are stored on the server and never shown again.",
];

/// What one connector card is showing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CardState {
    /// An admin has to configure the provider first. No sign-in action.
    NotSetUp { hint: String },
    /// This account can connect, by sign-in and/or a pasted token.
    Connect { oauth: bool, token: bool },
    /// This account is connected.
    Connected { account: Option<String> },
    /// The last action failed; `message` is in plain words.
    Error { message: String },
}

impl CardState {
    /// The status pill text.
    pub fn pill(&self) -> &'static str {
        match self {
            Self::NotSetUp { .. } => "Not set up yet",
            Self::Connect { .. } => "Not connected",
            Self::Connected { .. } => "Connected",
            Self::Error { .. } => "Needs attention",
        }
    }

    pub fn is_connected(&self) -> bool {
        matches!(self, Self::Connected { .. })
    }

    /// "Connected as name@example.example", or "Connected" without a name.
    pub fn connected_line(&self) -> Option<String> {
        match self {
            Self::Connected {
                account: Some(account),
            } if !account.trim().is_empty() => Some(format!("Connected as {account}")),
            Self::Connected { .. } => Some("Connected".to_owned()),
            _ => None,
        }
    }
}

/// Map one account's server-reported state and the last error onto a card.
pub fn personal_card_state(personal: &PersonalStatus, error: Option<&str>) -> CardState {
    if let Some(message) = error.map(str::trim).filter(|message| !message.is_empty()) {
        return CardState::Error {
            message: message.to_owned(),
        };
    }
    match personal.state.as_str() {
        "connected" => CardState::Connected {
            account: personal
                .account
                .clone()
                .filter(|account| !account.trim().is_empty()),
        },
        "ready" => CardState::Connect {
            oauth: personal.methods.iter().any(|method| method == "oauth"),
            token: personal.methods.iter().any(|method| method == "token"),
        },
        _ => CardState::NotSetUp {
            hint: personal
                .hint
                .clone()
                .filter(|hint| !hint.trim().is_empty())
                .unwrap_or_else(|| DEFAULT_ADMIN_HINT.to_owned()),
        },
    }
}

/// The card state for a connector that reports per-user status, or `None` for
/// server-wide connectors (they keep their admin card).
pub fn connector_card_state(connector: &ConnectorStatus, error: Option<&str>) -> Option<CardState> {
    connector
        .personal
        .as_ref()
        .map(|personal| personal_card_state(personal, error))
}

/// Secret roles that are OAuth outputs. The server may store them, but a
/// person never types or sees them, so no card draws a field for them.
pub const INTERNAL_SECRET_ROLES: [&str; 2] = ["access_token", "refresh_token"];

/// True for a role the console must never show as an editable field.
pub fn is_internal_role(role: &str) -> bool {
    INTERNAL_SECRET_ROLES.contains(&role)
}

/// Roles a person may type into, in catalog order (internal ones removed).
pub fn editable_roles(connector: &ConnectorStatus) -> Vec<String> {
    connector
        .secrets
        .keys()
        .filter(|role| !is_internal_role(role))
        .cloned()
        .collect()
}

/// A connector the person signs in to (ElevenLabs): the server lists an OAuth
/// sign-in for it.
pub fn supports_sign_in(connector: &ConnectorStatus) -> bool {
    connector.oauth.is_some()
}

/// Whether a server-wide card counts as connected. A sign-in connector reports
/// this account's own sign-in; others keep the server's own answer.
pub fn legacy_connected(connector: &ConnectorStatus) -> bool {
    if supports_sign_in(connector) {
        return connector.connected == Some(true);
    }
    connector.connected.unwrap_or_else(|| {
        connector
            .secrets
            .iter()
            .any(|(role, secret)| !is_internal_role(role) && secret.configured)
    })
}

pub const GOOGLE_IDS: [&str; 4] = ["google_drive", "google_sheets", "google_calendar", "gmail"];

fn google_connectors(connectors: &[ConnectorStatus]) -> impl Iterator<Item = &PersonalStatus> {
    connectors
        .iter()
        .filter(|connector| GOOGLE_IDS.contains(&connector.id.as_str()))
        .filter_map(|connector| connector.personal.as_ref())
}

/// The admin "Set up Google" card is offered to admins only, when the server
/// says Google can be set up from the console.
pub fn google_setup_offered(connectors: &[ConnectorStatus], is_admin: bool) -> bool {
    is_admin
        && google_connectors(connectors)
            .any(|personal| personal.admin_setup.as_deref() == Some("google"))
}

/// Google credentials are already saved on the server.
pub fn google_is_set_up(connectors: &[ConnectorStatus]) -> bool {
    google_connectors(connectors).any(|personal| personal.state != "not_set_up")
}

/// The redirect address to register with Google, as the server computed it.
pub fn google_redirect_uri(connectors: &[ConnectorStatus]) -> Option<String> {
    google_connectors(connectors)
        .find_map(|personal| personal.redirect_uri.clone())
        .filter(|uri| !uri.trim().is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn personal(state: &str) -> PersonalStatus {
        PersonalStatus {
            state: state.into(),
            ..Default::default()
        }
    }

    fn status(id: &str, personal: Option<PersonalStatus>) -> ConnectorStatus {
        ConnectorStatus {
            id: id.into(),
            kind: "oauth".into(),
            personal,
            ..Default::default()
        }
    }

    #[test]
    fn token_connectors_have_fixed_https_help_links() {
        for id in ["notion", "github", "todoist", "slack", "exa"] {
            let help = token_help(id).expect(id);
            assert!(help.url.starts_with("https://"), "{id}");
            assert!(!help.label.is_empty() && !help.hint.is_empty(), "{id}");
        }
        assert!(token_help("gmail").is_none());
        let mut github = personal("ready");
        github.methods = vec!["token".into()];
        assert_eq!(
            personal_card_state(&github, None),
            CardState::Connect {
                oauth: false,
                token: true
            }
        );
    }

    #[test]
    fn not_set_up_shows_the_admin_hint_and_no_sign_in() {
        let mut outlook = personal("not_set_up");
        outlook.hint = Some("Admin: Microsoft sign-in is not configured.".into());
        let state = personal_card_state(&outlook, None);
        assert_eq!(
            state,
            CardState::NotSetUp {
                hint: "Admin: Microsoft sign-in is not configured.".into()
            }
        );
        assert_eq!(state.pill(), "Not set up yet");
        assert!(!matches!(state, CardState::Connect { .. }));
        // A missing or blank hint falls back to a short default.
        for hint in [None, Some("   ".to_owned())] {
            let mut bare = personal("not_set_up");
            bare.hint = hint;
            assert_eq!(
                personal_card_state(&bare, None),
                CardState::NotSetUp {
                    hint: DEFAULT_ADMIN_HINT.into()
                }
            );
        }
        // An unknown state never offers a sign-in either.
        assert!(matches!(
            personal_card_state(&personal("mystery"), None),
            CardState::NotSetUp { .. }
        ));
    }

    #[test]
    fn ready_offers_the_methods_the_server_lists() {
        let mut notion = personal("ready");
        notion.methods = vec!["token".into()];
        assert_eq!(
            personal_card_state(&notion, None),
            CardState::Connect {
                oauth: false,
                token: true
            }
        );
        notion.methods = vec!["token".into(), "oauth".into()];
        assert_eq!(
            personal_card_state(&notion, None),
            CardState::Connect {
                oauth: true,
                token: true
            }
        );
        let mut outlook = personal("ready");
        outlook.methods = vec!["oauth".into()];
        let state = personal_card_state(&outlook, None);
        assert_eq!(state.pill(), "Not connected");
    }

    #[test]
    fn connected_reads_connected_as_the_account() {
        let mut gmail = personal("connected");
        gmail.account = Some("sam@example.example".into());
        let state = personal_card_state(&gmail, None);
        assert!(state.is_connected());
        assert_eq!(state.pill(), "Connected");
        assert_eq!(
            state.connected_line().as_deref(),
            Some("Connected as sam@example.example")
        );
        gmail.account = Some("  ".into());
        assert_eq!(
            personal_card_state(&gmail, None)
                .connected_line()
                .as_deref(),
            Some("Connected")
        );
        assert_eq!(
            personal_card_state(&personal("ready"), None).connected_line(),
            None
        );
    }

    #[test]
    fn an_error_wins_and_is_plain_words() {
        let state = personal_card_state(&personal("ready"), Some("  Not set up yet.  "));
        assert_eq!(
            state,
            CardState::Error {
                message: "Not set up yet.".into()
            }
        );
        assert_eq!(state.pill(), "Needs attention");
        // A blank error is no error.
        assert!(matches!(
            personal_card_state(&personal("ready"), Some("  ")),
            CardState::Connect { .. }
        ));
    }

    /// The account sign-in card: an OAuth block, no server-wide secrets.
    fn elevenlabs_sign_in(connected: Option<bool>) -> ConnectorStatus {
        ConnectorStatus {
            id: "elevenlabs".into(),
            kind: "oauth".into(),
            oauth: Some(crate::ConnectorOauth {
                client_id: "https://example.example/client.json".into(),
                authorize_url: None,
                cimd_path: None,
                callback_uri: Some("http://127.0.0.1:1/cb".into()),
                scope: None,
            }),
            connected,
            ..Default::default()
        }
    }

    #[test]
    fn the_sign_in_card_has_no_editable_fields_and_follows_this_account() {
        let card = elevenlabs_sign_in(Some(true));
        assert!(supports_sign_in(&card));
        assert!(editable_roles(&card).is_empty());
        assert!(legacy_connected(&card));
        assert!(!legacy_connected(&elevenlabs_sign_in(Some(false))));
        assert!(!legacy_connected(&elevenlabs_sign_in(None)));
    }

    #[test]
    fn plain_key_connectors_ignore_internal_roles_when_counting() {
        let mut tavily = status("tavily", None);
        assert!(!supports_sign_in(&tavily));
        assert!(!legacy_connected(&tavily));
        tavily.secrets.insert(
            "api_key".into(),
            crate::SecretStatus {
                name: None,
                configured: true,
            },
        );
        assert!(legacy_connected(&tavily));
        tavily.connected = Some(false);
        assert!(!legacy_connected(&tavily));
    }

    #[test]
    fn server_wide_connectors_have_no_personal_card() {
        assert_eq!(connector_card_state(&status("tavily", None), None), None);
        assert!(connector_card_state(&status("outlook", Some(personal("ready"))), None).is_some());
    }

    #[test]
    fn google_setup_is_for_admins_and_follows_the_servers_state() {
        let mut unset = personal("not_set_up");
        unset.admin_setup = Some("google".into());
        unset.redirect_uri =
            Some("https://bot.example.example/v1/connectors/google/callback".into());
        let list = vec![
            status("gmail", Some(unset.clone())),
            status("notion", Some(personal("ready"))),
        ];
        assert!(google_setup_offered(&list, true));
        assert!(!google_setup_offered(&list, false));
        assert!(!google_is_set_up(&list));
        assert_eq!(
            google_redirect_uri(&list).as_deref(),
            Some("https://bot.example.example/v1/connectors/google/callback")
        );
        let mut ready = unset;
        ready.state = "ready".into();
        let list = vec![status("gmail", Some(ready))];
        assert!(google_is_set_up(&list));
        // Nothing Google in the list: nothing to set up.
        assert!(!google_setup_offered(
            &[status("notion", Some(personal("ready")))],
            true
        ));
    }
}
