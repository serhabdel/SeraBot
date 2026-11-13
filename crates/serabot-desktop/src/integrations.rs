//! The Integrations screen's model (v0.38): connectors, Composio and
//! marketplace plugins as one list of cards, with tabs and search.
//!
//! Pure functions, so the rules are tested without a window. Secrets never
//! reach this module: it sees only the status the server reports, and the
//! typed values stay in the cards that collect them.

use crate::ConnectorStatus;
use crate::connector_card::{CardState, connector_card_state, legacy_connected};
use crate::marketplace::{PluginStatus, PluginSummary};
use crate::theme::Tone;

/// The connector whose apps the Composio tab lists. It is not an installed
/// integration in the "Set up" list: it has its own tab.
pub const COMPOSIO_ID: &str = "composio";

/// The tabs of the Integrations screen.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Tab {
    /// What is set up or turned on, then what is still open to set up.
    #[default]
    Installed,
    /// The plugin catalogue.
    Marketplace,
    /// Composio's apps, with its key first when there is none.
    Composio,
}

impl Tab {
    pub const ALL: [Self; 3] = [Self::Installed, Self::Marketplace, Self::Composio];

    pub fn label(self) -> &'static str {
        match self {
            Self::Installed => "Installed",
            Self::Marketplace => "Marketplace",
            Self::Composio => "Composio",
        }
    }
}

/// Where a card comes from. Connectors and plugins are kept apart, so a
/// connector and a plugin that share a name never collide.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Source {
    Connector(String),
    Plugin(String),
}

/// One card on the screen, whatever its source.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub source: Source,
    pub name: String,
    pub blurb: String,
    pub pill: String,
    pub tone: Tone,
    /// Set up or turned on for this account. Decides the "Set up" group.
    pub set_up: bool,
    /// A short line about where it comes from: "Your account", "Plugin" and so on.
    pub meta: String,
}

/// The suites that group connectors in the "Available to set up" list, in
/// order. A connector not listed here lands in "More" rather than vanishing.
pub const SUITES: [(&str, &[&str]); 7] = [
    ("Microsoft", &["onedrive", "outlook", "outlook_calendar"]),
    (
        "Google",
        &["google_drive", "google_sheets", "google_calendar", "gmail"],
    ),
    ("Notes and email", &["notion", "agentmail"]),
    ("Code and tasks", &["github", "todoist"]),
    ("Messages", &["slack"]),
    ("Web search", &["tavily", "firecrawl", "exa"]),
    ("Voice", &["elevenlabs", "elevenlabs_api_key"]),
];

pub const MORE_SUITE: &str = "More";

/// The suite a connector belongs to in the "Available to set up" list.
pub fn suite_of(id: &str) -> &'static str {
    SUITES
        .iter()
        .find(|(_, ids)| ids.contains(&id))
        .map_or(MORE_SUITE, |(title, _)| title)
}

pub fn connector_title(id: &str) -> String {
    match id {
        "onedrive" => "OneDrive",
        "google_drive" => "Google Drive",
        "google_sheets" => "Google Sheets",
        "google_calendar" => "Google Calendar",
        "gmail" => "Gmail",
        "outlook" => "Outlook",
        "outlook_calendar" => "Outlook Calendar",
        "notion" => "Notion",
        "github" => "GitHub",
        "todoist" => "Todoist",
        "slack" => "Slack",
        "exa" => "Exa Search",
        "agentmail" => "AgentMail",
        "tavily" => "Tavily Search",
        "firecrawl" => "Firecrawl",
        "elevenlabs" => "ElevenLabs",
        "elevenlabs_api_key" => "ElevenLabs API key",
        "composio" => "Composio",
        other => return humanize(other),
    }
    .to_owned()
}

/// An id such as `some_tool` read as "Some tool", for a connector this build
/// does not name.
fn humanize(id: &str) -> String {
    let words = id.replace(['_', '-'], " ");
    let mut chars = words.trim().chars();
    match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
        None => "Connector".to_owned(),
    }
}

pub fn connector_blurb(id: &str) -> &'static str {
    match id {
        "onedrive" => "Save and read files in your OneDrive.",
        "google_drive" => "Files in your Google Drive.",
        "google_sheets" => "Your Google spreadsheets.",
        "google_calendar" => "Your Google calendar.",
        "gmail" => "Read and send email.",
        "outlook" => "Read and send email.",
        "outlook_calendar" => "Your Outlook calendar.",
        "notion" => "Your notes and pages.",
        "github" => {
            "Search repositories, read issues and files, and file issues with your approval."
        }
        "todoist" => "Your to-do list.",
        "slack" => "Read channels and post with your approval.",
        "agentmail" => "A mailbox for your bots.",
        "tavily" | "firecrawl" | "exa" => "Let bots search and read the web.",
        "elevenlabs" => {
            "Sign in with your ElevenLabs account: voices, music, sound, images, video and agents your plan includes."
        }
        "elevenlabs_api_key" => "Voice, sound and images with the owner's ElevenLabs API key.",
        "composio" => "Connect apps through Composio with your own key.",
        _ => "",
    }
}

fn tone_of_card(state: &CardState) -> Tone {
    match state {
        CardState::Connected { .. } => Tone::Ok,
        CardState::Error { .. } => Tone::Danger,
        CardState::NotSetUp { .. } | CardState::Connect { .. } => Tone::Muted,
    }
}

/// The card for one connector. `error` is the last failure for it, if any.
pub fn connector_entry(connector: &ConnectorStatus, error: Option<&str>) -> Entry {
    let name = connector_title(&connector.id);
    let blurb = connector_blurb(&connector.id).to_owned();
    let (pill, tone, set_up, meta) = if connector.id == COMPOSIO_ID {
        let connected = connector.connected.unwrap_or(false);
        let (pill, tone) = if connected {
            ("Connected", Tone::Ok)
        } else {
            ("Not connected", Tone::Muted)
        };
        (pill, tone, connected, "Apps and tools")
    } else if let Some(state) = connector_card_state(connector, error) {
        let set_up = state.is_connected();
        (state.pill(), tone_of_card(&state), set_up, "Your account")
    } else if legacy_connected(connector) {
        ("Connected", Tone::Ok, true, "Shared with the server")
    } else {
        (
            "Not connected",
            Tone::Muted,
            false,
            "Shared with the server",
        )
    };
    Entry {
        source: Source::Connector(connector.id.clone()),
        name,
        blurb,
        pill: pill.to_owned(),
        tone,
        set_up,
        meta: meta.to_owned(),
    }
}

/// The tone for a plugin's state. Unchanged from the Marketplace screen.
pub fn plugin_tone(status: PluginStatus) -> Tone {
    match status {
        PluginStatus::ComingSoon | PluginStatus::Available | PluginStatus::Installed => Tone::Muted,
        PluginStatus::NeedsSetup => Tone::Warn,
        PluginStatus::Enabled => Tone::Ok,
    }
}

/// The card for one marketplace plugin. A plugin counts as set up once it is
/// installed, whether or not it is turned on yet.
pub fn plugin_entry(plugin: &PluginSummary) -> Entry {
    let state = plugin.state();
    let mut meta = vec!["Plugin".to_owned()];
    if !plugin.category.is_empty() {
        meta.push(plugin.category.clone());
    }
    if !plugin.version.is_empty() {
        meta.push(format!("v{}", plugin.version));
    }
    Entry {
        source: Source::Plugin(plugin.slug.clone()),
        name: plugin.name.clone(),
        blurb: plugin.tagline.clone(),
        pill: state.label().to_owned(),
        tone: plugin_tone(state),
        set_up: plugin.installed,
        meta: meta.join(" · "),
    }
}

/// The "Installed" tab: what is set up, then the connectors still open to set
/// up, grouped by suite. Composio has its own tab, so it is left out here.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Installed {
    pub set_up: Vec<Entry>,
    /// (suite title, cards), in suite order, with empty suites left out.
    pub available: Vec<(&'static str, Vec<Entry>)>,
}

pub fn installed(
    connectors: &[ConnectorStatus],
    plugins: &[PluginSummary],
    errors: &dyn Fn(&str) -> Option<String>,
) -> Installed {
    let mut set_up = Vec::new();
    let mut rest: Vec<(&'static str, Entry)> = Vec::new();
    for connector in connectors.iter().filter(|c| c.id != COMPOSIO_ID) {
        let error = errors(&connector.id);
        let entry = connector_entry(connector, error.as_deref());
        if entry.set_up {
            set_up.push(entry);
        } else {
            rest.push((suite_of(&connector.id), entry));
        }
    }
    set_up.extend(
        plugins
            .iter()
            .filter(|plugin| plugin.installed)
            .map(plugin_entry),
    );
    let mut available: Vec<(&'static str, Vec<Entry>)> = Vec::new();
    let order = SUITES
        .iter()
        .map(|(title, _)| *title)
        .chain(std::iter::once(MORE_SUITE));
    for suite in order {
        let cards: Vec<Entry> = rest
            .iter()
            .filter(|(title, _)| *title == suite)
            .map(|(_, entry)| entry.clone())
            .collect();
        if !cards.is_empty() {
            available.push((suite, cards));
        }
    }
    Installed { set_up, available }
}

/// Whether a card matches the search text. Case-insensitive; an empty search
/// matches every card.
pub fn matches(entry: &Entry, query: &str) -> bool {
    let needle = query.trim().to_lowercase();
    if needle.is_empty() {
        return true;
    }
    [&entry.name, &entry.blurb, &entry.meta, &entry.pill]
        .iter()
        .any(|text| text.to_lowercase().contains(&needle))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::PersonalStatus;
    use crate::marketplace::PluginSummary;
    use std::collections::BTreeMap;

    fn connector(id: &str) -> ConnectorStatus {
        ConnectorStatus {
            id: id.into(),
            kind: "test".into(),
            enabled: true,
            secrets: BTreeMap::new(),
            ..Default::default()
        }
    }

    fn personal(id: &str, state: &str) -> ConnectorStatus {
        let mut connector = connector(id);
        connector.personal = Some(PersonalStatus {
            state: state.into(),
            ..Default::default()
        });
        connector
    }

    fn plugin(slug: &str, installed: bool, enabled: bool, status: &str) -> PluginSummary {
        serde_json::from_value(serde_json::json!({
            "slug": slug,
            "name": format!("{slug} name"),
            "tagline": "A test plugin.",
            "category": "Search",
            "version": "1.0.0",
            "installed": installed,
            "enabled": enabled,
            "status": status,
        }))
        .expect("plugin fixture")
    }

    fn no_errors(_: &str) -> Option<String> {
        None
    }

    #[test]
    fn each_tab_has_a_label_and_the_installed_tab_comes_first() {
        assert_eq!(Tab::default(), Tab::Installed);
        let labels: Vec<_> = Tab::ALL.iter().map(|tab| tab.label()).collect();
        assert_eq!(labels, ["Installed", "Marketplace", "Composio"]);
    }

    #[test]
    fn personal_card_states_map_to_pills_and_tones() {
        let connected = connector_entry(&personal("outlook", "connected"), None);
        assert_eq!(
            (connected.pill.as_str(), connected.tone, connected.set_up),
            ("Connected", Tone::Ok, true)
        );
        let ready = connector_entry(&personal("outlook", "ready"), None);
        assert_eq!(
            (ready.pill.as_str(), ready.tone, ready.set_up),
            ("Not connected", Tone::Muted, false)
        );
        let not_set_up = connector_entry(&personal("outlook", "not_set_up"), None);
        assert_eq!(not_set_up.pill, "Not set up yet");
        let failed = connector_entry(&personal("outlook", "ready"), Some("Sign-in failed."));
        assert_eq!(
            (failed.pill.as_str(), failed.tone),
            ("Needs attention", Tone::Danger)
        );
        assert!(!failed.set_up);
    }

    #[test]
    fn composio_is_a_connector_card_with_its_own_tab_not_a_set_up_item() {
        let mut composio = connector(COMPOSIO_ID);
        composio.connected = Some(true);
        let entry = connector_entry(&composio, None);
        assert_eq!(entry.name, "Composio");
        assert!(entry.set_up);
        let view = installed(&[composio], &[], &no_errors);
        assert!(view.set_up.is_empty());
        assert!(view.available.is_empty());
    }

    #[test]
    fn installed_lists_set_up_first_then_available_by_suite() {
        let connectors = vec![
            personal("outlook", "connected"),
            personal("gmail", "ready"),
            personal("onedrive", "not_set_up"),
            connector("mystery_tool"),
        ];
        let plugins = vec![
            plugin("search", true, true, "installed"),
            plugin("mail", false, false, "available"),
        ];
        let view = installed(&connectors, &plugins, &no_errors);
        let set_up: Vec<_> = view.set_up.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(set_up, ["Outlook", "search name"]);
        let suites: Vec<_> = view.available.iter().map(|(title, _)| *title).collect();
        assert_eq!(suites, ["Microsoft", "Google", MORE_SUITE]);
        let more = &view.available.last().unwrap().1;
        assert_eq!(more[0].name, "Mystery tool");
    }

    #[test]
    fn a_failed_connector_stays_in_available_with_its_error() {
        let connectors = vec![personal("outlook", "connected")];
        let errors = |id: &str| (id == "outlook").then(|| "Sign-in failed.".to_owned());
        let view = installed(&connectors, &[], &errors);
        assert!(view.set_up.is_empty());
        assert_eq!(view.available[0].1[0].pill, "Needs attention");
    }

    #[test]
    fn plugin_cards_carry_the_marketplace_state() {
        let uninstalled = plugin_entry(&plugin("mail", false, false, "available"));
        assert!(!uninstalled.set_up);
        assert_eq!(uninstalled.meta, "Plugin · Search · v1.0.0");
        let installed_off = plugin_entry(&plugin("search", true, false, "installed"));
        assert!(installed_off.set_up);
        assert_eq!(installed_off.pill, PluginStatus::Installed.label());
    }

    #[test]
    fn search_matches_name_blurb_and_meta_ignoring_case() {
        let entry = connector_entry(&personal("gmail", "ready"), None);
        assert!(matches(&entry, ""));
        assert!(matches(&entry, "  MAIL "));
        assert!(matches(&entry, "read and send"));
        assert!(matches(&entry, "your account"));
        assert!(!matches(&entry, "calendar"));
    }

    #[test]
    fn a_connector_without_a_known_name_is_still_readable() {
        assert_eq!(connector_title("some_tool-v2"), "Some tool v2");
        assert_eq!(connector_title(""), "Connector");
        assert_eq!(connector_blurb("some_tool"), "");
    }

    #[test]
    fn connector_labels_have_no_mojibake() {
        // The labels are typed in ui_connectors.rs. A UTF-8 ellipsis read as
        // Windows-1252 shows up as these two sequences.
        let source = include_str!("ui_connectors.rs");
        assert!(!source.contains('\u{00d4}'), "mojibake in ui_connectors.rs");
        assert!(
            !source.contains("\u{00e2}\u{20ac}"),
            "mojibake in ui_connectors.rs"
        );
    }
}
