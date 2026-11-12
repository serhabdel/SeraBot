//! The Marketplace screen's data and rules (v0.37): the plugin catalog, one
//! user's install state, and the config form. Plain data so it can be tested
//! without a window.
//!
//! Secrets: the server never returns a secret value, only whether a field is
//! configured. This module has no field that could hold a stored value; the
//! only secret text on the client is what the user types into a form, and it
//! is cleared as soon as it is sent.
//!
//! Plugins cannot be called yet, and the shipped catalog uses placeholder
//! hosts (`*.example.invalid`). Install and Enable stay off until both are
//! true for that plugin, so a click cannot end at a plugin that looks on and
//! does nothing. Disable and Uninstall stay available so an install made
//! through the API can still be turned off and removed.

use serde::Deserialize;

/// Upper bounds, so a bad response cannot balloon the screen.
pub const MAX_PLUGINS: usize = 500;
pub const MAX_TEXT: usize = 4_000;
pub const MAX_ITEMS: usize = 64;

/// The tool runtime that would call a plugin is not built. While this is
/// false, nothing in the catalog can run, so Install and Enable stay off
/// even for a host that is not a placeholder.
pub const RUNTIME_CALLS_PLUGINS: bool = false;

/// The line at the top of the screen while [`RUNTIME_CALLS_PLUGINS`] is false.
pub const COMING_SOON: &str = "Coming soon. You can browse plugins. Install and Enable stay off until SeraBot can call them, and a plugin whose host is still a placeholder (example.invalid) stays off after that.";

/// What a card shows as its status.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PluginStatus {
    /// Not installed, and not offered yet.
    ComingSoon,
    /// Not installed, and install is allowed.
    Available,
    /// Installed but a required key is missing.
    NeedsSetup,
    /// Installed and ready, turned off.
    Installed,
    /// Installed and turned on.
    Enabled,
}

impl PluginStatus {
    pub fn label(self) -> &'static str {
        match self {
            Self::ComingSoon => "Coming soon",
            Self::Available => "Available",
            Self::NeedsSetup => "Needs setup",
            Self::Installed => "Installed",
            Self::Enabled => "Enabled",
        }
    }
}

/// Reads the server's `status` word together with the install flags. The
/// flags win when they disagree, so a card never claims "Enabled" for a plugin
/// that is not installed.
pub fn plugin_status(installed: bool, enabled: bool, status: &str) -> PluginStatus {
    if !installed {
        return PluginStatus::Available;
    }
    if status == "needs_config" {
        return PluginStatus::NeedsSetup;
    }
    if enabled {
        PluginStatus::Enabled
    } else {
        PluginStatus::Installed
    }
}

/// One card of `GET /v1/marketplace/plugins`.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
pub struct PluginSummary {
    pub slug: String,
    pub name: String,
    #[serde(default)]
    pub tagline: String,
    #[serde(default)]
    pub category: String,
    #[serde(default)]
    pub version: String,
    #[serde(default)]
    pub installed: bool,
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub status: String,
}

impl PluginSummary {
    pub fn state(&self) -> PluginStatus {
        let status = plugin_status(self.installed, self.enabled, &self.status);
        // The list payload has no hosts. Until plugins can be called, an
        // uninstalled card says so instead of offering itself as available.
        if !self.installed && !RUNTIME_CALLS_PLUGINS {
            PluginStatus::ComingSoon
        } else {
            status
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct PluginList {
    #[serde(default)]
    pub plugins: Vec<PluginSummary>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
pub struct Prompt {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub text: String,
}

/// One config field. `configured` says whether the user has set it; the value
/// is never sent to the client.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
pub struct ConfigField {
    pub id: String,
    #[serde(default)]
    pub label: String,
    #[serde(default)]
    pub hint: String,
    #[serde(default)]
    pub docs_url: Option<String>,
    #[serde(default)]
    pub configured: bool,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
pub struct AuthBlock {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub kind: String,
    #[serde(default)]
    pub fields: Vec<ConfigField>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
pub struct PluginApp {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub transport: String,
    #[serde(default)]
    pub auth: Vec<AuthBlock>,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq)]
pub struct Permissions {
    #[serde(default)]
    pub egress: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
pub struct PluginInfo {
    pub slug: String,
    pub name: String,
    #[serde(default)]
    pub tagline: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub category: String,
    #[serde(default)]
    pub version: String,
    /// Install flags live on the plugin object. `GET /v1/marketplace/plugins/{slug}`
    /// returns `{ "plugin": { ..., "installed", "enabled", "status" } }`.
    #[serde(default)]
    pub installed: bool,
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub prompts: Vec<Prompt>,
    #[serde(default)]
    pub apps: Vec<PluginApp>,
    #[serde(default)]
    pub permissions: Permissions,
    #[serde(default)]
    pub website_url: Option<String>,
    #[serde(default)]
    pub privacy_policy_url: Option<String>,
    #[serde(default)]
    pub terms_url: Option<String>,
}

/// `GET /v1/marketplace/plugins/{slug}`.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
pub struct PluginDetail {
    pub plugin: PluginInfo,
}

/// Why Install and Enable must not be sent.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ActivationBlock {
    /// SeraBot cannot call any plugin yet.
    Runtime,
    /// A declared host is `example.invalid`, which is not a service.
    PlaceholderHost,
    /// The server refuses plugins that start a local program.
    LocalProgram,
    /// The page is not loaded, so a placeholder host cannot be ruled out.
    Unchecked,
}

impl ActivationBlock {
    pub fn message(self) -> &'static str {
        match self {
            Self::Runtime => {
                "Coming soon. SeraBot cannot call plugins yet, so this one cannot be installed or turned on."
            }
            Self::PlaceholderHost => {
                "Coming soon. This plugin still points at a placeholder host (example.invalid), so it cannot be installed or turned on."
            }
            Self::LocalProgram => {
                "This plugin starts a program on the server, which is not supported yet."
            }
            Self::Unchecked => "Open the plugin page before installing or turning it on.",
        }
    }
}

/// A host, or a URL whose host, is the reserved placeholder `example.invalid`.
pub fn placeholder_host(value: &str) -> bool {
    fn reserved(host: &str) -> bool {
        let host = host.trim().trim_end_matches('.').to_ascii_lowercase();
        host == "example.invalid" || host.ends_with(".example.invalid")
    }
    let value = value.trim();
    if reserved(value) {
        return true;
    }
    url::Url::parse(value).is_ok_and(|parsed| parsed.host_str().is_some_and(reserved))
}

/// How a failed marketplace call is shown. A missing list route means this
/// server has no marketplace. A missing plugin does not.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MarketplaceProblem {
    Unavailable,
    Message(String),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MarketplaceCall {
    List,
    Detail,
    Write,
}

pub fn marketplace_problem(
    call: MarketplaceCall,
    not_found: bool,
    message: String,
) -> MarketplaceProblem {
    if not_found {
        return match call {
            MarketplaceCall::List => MarketplaceProblem::Unavailable,
            MarketplaceCall::Detail => {
                MarketplaceProblem::Message("That plugin is not in the catalog.".into())
            }
            MarketplaceCall::Write => MarketplaceProblem::Message(
                "That change could not be made. Refresh the catalog and try again.".into(),
            ),
        };
    }
    MarketplaceProblem::Message(plain_failure(message))
}

/// A sentence the screen can show. JSON, the generic "invalid request"
/// placeholder, and "Retrying…" (this screen does not retry by itself) become
/// a line that says what to do.
fn plain_failure(message: String) -> String {
    let trimmed = message.trim();
    let unreadable = trimmed.is_empty()
        || trimmed.chars().count() > 200
        || trimmed.chars().any(char::is_control)
        || trimmed.contains(['{', '}', '[', ']'])
        || serde_json::from_str::<serde_json::Value>(trimmed).is_ok();
    if unreadable {
        return "The server sent an error this app could not read. Retry.".to_owned();
    }
    let lower = trimmed.to_ascii_lowercase();
    if matches!(
        lower.as_str(),
        "invalid request" | "not found" | "forbidden" | "unauthorized" | "internal server error"
    ) {
        return "The server refused that change. Refresh and try again.".to_owned();
    }
    if lower.contains("retrying") {
        return "Can't reach the server. Check the connection, then retry.".to_owned();
    }
    trimmed.to_owned()
}

impl PluginDetail {
    pub fn installed(&self) -> bool {
        self.plugin.installed
    }

    pub fn enabled(&self) -> bool {
        self.plugin.enabled
    }

    pub fn state(&self) -> PluginStatus {
        let status = plugin_status(self.installed(), self.enabled(), &self.plugin.status);
        if !self.installed() && self.activation_block().is_some() {
            PluginStatus::ComingSoon
        } else {
            status
        }
    }

    /// Every config field of every app, in order.
    pub fn fields(&self) -> Vec<&ConfigField> {
        self.plugin
            .apps
            .iter()
            .flat_map(|app| app.auth.iter())
            .flat_map(|auth| auth.fields.iter())
            .collect()
    }

    /// Whether the plugin has anything to configure.
    pub fn needs_config(&self) -> bool {
        !self.fields().is_empty()
    }

    /// Whether every field is set (so Enable makes sense).
    pub fn fully_configured(&self) -> bool {
        self.fields().iter().all(|field| field.configured)
    }

    /// Plugins that start a local program are refused by the server.
    pub fn uses_local_program(&self) -> bool {
        self.plugin.apps.iter().any(|app| app.transport == "stdio")
    }

    /// True when any declared host or link is `example.invalid`. The detail
    /// payload does not include the app URL; egress hosts and the public
    /// links are what the server sends.
    pub fn uses_placeholder_host(&self) -> bool {
        if self
            .plugin
            .permissions
            .egress
            .iter()
            .any(|host| placeholder_host(host))
        {
            return true;
        }
        let links = [
            self.plugin.website_url.as_deref(),
            self.plugin.privacy_policy_url.as_deref(),
            self.plugin.terms_url.as_deref(),
        ];
        if links.into_iter().flatten().any(placeholder_host) {
            return true;
        }
        self.fields()
            .iter()
            .any(|field| field.docs_url.as_deref().is_some_and(placeholder_host))
    }

    /// Why Install and Enable are refused, if they are. Local programs and
    /// placeholder hosts stay refused after the runtime exists.
    pub fn activation_block(&self) -> Option<ActivationBlock> {
        self.activation_block_when(RUNTIME_CALLS_PLUGINS)
    }

    pub fn activation_block_when(&self, runtime_calls: bool) -> Option<ActivationBlock> {
        if self.uses_local_program() {
            return Some(ActivationBlock::LocalProgram);
        }
        if self.uses_placeholder_host() {
            return Some(ActivationBlock::PlaceholderHost);
        }
        if !runtime_calls {
            return Some(ActivationBlock::Runtime);
        }
        None
    }
}

/// What a button does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PluginAction {
    Install,
    Enable,
    Disable,
    Uninstall,
}

impl PluginAction {
    pub fn label(self) -> &'static str {
        match self {
            Self::Install => "Install",
            Self::Enable => "Enable",
            Self::Disable => "Disable",
            Self::Uninstall => "Uninstall",
        }
    }
}

/// The buttons for a plugin in its current state. Install and Enable are
/// omitted while [`PluginDetail::activation_block`] is set. Disable and
/// Uninstall stay, so an install that already exists can be removed.
pub fn actions_for(detail: &PluginDetail) -> Vec<PluginAction> {
    actions_when(detail, detail.activation_block().is_some())
}

fn actions_when(detail: &PluginDetail, blocked: bool) -> Vec<PluginAction> {
    if !detail.installed() {
        if blocked {
            return Vec::new();
        }
        return vec![PluginAction::Install];
    }
    let mut actions = Vec::new();
    if detail.enabled() {
        actions.push(PluginAction::Disable);
    } else if !blocked && detail.fully_configured() {
        actions.push(PluginAction::Enable);
    }
    actions.push(PluginAction::Uninstall);
    actions
}

/// Case-insensitive search over name, tagline and category, plus an optional
/// category. An empty category means all.
pub fn filter_plugins<'a>(
    plugins: &'a [PluginSummary],
    query: &str,
    category: &str,
) -> Vec<&'a PluginSummary> {
    let needle = query.trim().to_lowercase();
    plugins
        .iter()
        .filter(|plugin| category.is_empty() || plugin.category.eq_ignore_ascii_case(category))
        .filter(|plugin| {
            needle.is_empty()
                || plugin.name.to_lowercase().contains(&needle)
                || plugin.tagline.to_lowercase().contains(&needle)
                || plugin.category.to_lowercase().contains(&needle)
        })
        .collect()
}

/// The categories present, sorted, without repeats or blanks.
pub fn categories(plugins: &[PluginSummary]) -> Vec<String> {
    let mut found: Vec<String> = plugins
        .iter()
        .map(|plugin| plugin.category.trim().to_owned())
        .filter(|category| !category.is_empty())
        .collect();
    found.sort_by_key(|category| category.to_lowercase());
    found.dedup_by(|a, b| a.eq_ignore_ascii_case(b));
    found
}

/// Only plain web links may be opened from a plugin page.
pub fn safe_link(url: &str) -> bool {
    url.len() <= 2_048
        && url::Url::parse(url).is_ok_and(|parsed| {
            parsed.scheme() == "https"
                && parsed.host_str().is_some()
                && parsed.username().is_empty()
                && parsed.password().is_none()
        })
}

/// What a typed value may be: non-empty after trimming, one line, bounded. The
/// value itself is never logged or shown.
pub fn valid_secret_input(value: &str) -> bool {
    let trimmed = value.trim();
    !trimmed.is_empty()
        && trimmed.chars().count() <= 4_096
        && !trimmed.chars().any(char::is_control)
}

/// The values to send: only fields the user filled in. Fields left empty keep
/// their saved value on the server.
pub fn config_values<'a>(
    fields: &[&ConfigField],
    typed: impl Fn(&str) -> &'a str,
) -> Result<Vec<(String, zeroize::Zeroizing<String>)>, String> {
    let mut out = Vec::new();
    for field in fields {
        let value = typed(&field.id);
        if value.trim().is_empty() {
            continue;
        }
        if !valid_secret_input(value) {
            let label = if field.label.is_empty() {
                &field.id
            } else {
                &field.label
            };
            return Err(format!("{label} must be one line of text."));
        }
        out.push((
            field.id.clone(),
            zeroize::Zeroizing::new(value.trim().to_owned()),
        ));
    }
    if out.is_empty() {
        return Err("Fill in at least one field.".to_owned());
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn summary(
        slug: &str,
        name: &str,
        category: &str,
        installed: bool,
        enabled: bool,
        status: &str,
    ) -> PluginSummary {
        PluginSummary {
            slug: slug.into(),
            name: name.into(),
            tagline: format!("{name} tools"),
            category: category.into(),
            version: "1.0.0".into(),
            installed,
            enabled,
            status: status.into(),
        }
    }

    fn detail(installed: bool, enabled: bool, configured: bool, transport: &str) -> PluginDetail {
        PluginDetail {
            plugin: PluginInfo {
                slug: "search".into(),
                name: "Search".into(),
                tagline: String::new(),
                description: String::new(),
                category: "research".into(),
                version: "1.0.0".into(),
                installed,
                enabled,
                status: String::new(),
                prompts: vec![],
                apps: vec![PluginApp {
                    id: "app".into(),
                    name: "App".into(),
                    transport: transport.into(),
                    auth: vec![AuthBlock {
                        id: "key".into(),
                        kind: "api-key".into(),
                        fields: vec![ConfigField {
                            id: "token".into(),
                            label: "API key".into(),
                            hint: String::new(),
                            docs_url: None,
                            configured,
                        }],
                    }],
                }],
                permissions: Permissions::default(),
                website_url: None,
                privacy_policy_url: None,
                terms_url: None,
            },
        }
    }

    #[test]
    fn parses_the_posted_list_and_detail_shapes() {
        let list: PluginList = serde_json::from_str(
            r#"{"plugins":[{"slug":"search","name":"Search","tagline":"t","category":"research",
                "icon_url":"https://x.example/i.png","version":"1.0.0","min_server_version":"0.36.0",
                "installed":true,"enabled":false,"status":"needs_config"}]}"#,
        )
        .unwrap();
        assert_eq!(list.plugins[0].state(), PluginStatus::NeedsSetup);
        assert_eq!(
            summary("a", "Search", "research", false, false, "available").state(),
            PluginStatus::ComingSoon
        );

        // The server nests installed/enabled/status inside `plugin`. Flags
        // sitting beside `plugin` are not the install state.
        let beside: PluginDetail = serde_json::from_str(
            r#"{"plugin":{"slug":"search","name":"Search"},"installed":true,"enabled":true,"status":"installed"}"#,
        )
        .unwrap();
        assert!(!beside.installed());
        assert_eq!(beside.state(), PluginStatus::ComingSoon);

        let detail: PluginDetail = serde_json::from_str(
            r#"{"plugin":{"slug":"search","name":"Search","tagline":"t","description":"d","category":"research",
                "version":"1.0.0","installed":true,"enabled":false,"status":"needs_config",
                "prompts":[{"id":"p","text":"Find"}],
                "apps":[{"id":"a","name":"A","transport":"http","auth":[{"id":"k","kind":"api-key",
                   "fields":[{"id":"token","label":"API key","hint":"h","docs_url":"https://docs.example/keys","configured":false}]}]}],
                "permissions":{"egress":["api.example"]},"website_url":"https://w.example",
                "privacy_policy_url":"https://p.example","terms_url":"https://t.example"}}"#,
        )
        .unwrap();
        assert!(detail.installed());
        assert!(!detail.enabled());
        assert_eq!(detail.state(), PluginStatus::NeedsSetup);
        assert_eq!(detail.fields().len(), 1);
        assert!(!detail.fully_configured());
        assert_eq!(detail.plugin.permissions.egress, vec!["api.example"]);
        // Sparse bodies fall back to safe defaults.
        let sparse: PluginDetail =
            serde_json::from_str(r#"{"plugin":{"slug":"x","name":"X"}}"#).unwrap();
        assert!(!sparse.installed());
        assert!(!sparse.needs_config());
        assert_eq!(sparse.state(), PluginStatus::ComingSoon);
    }

    #[test]
    fn status_follows_the_flags_first() {
        assert_eq!(
            plugin_status(false, true, "needs_config"),
            PluginStatus::Available
        );
        assert_eq!(
            plugin_status(true, false, "needs_config"),
            PluginStatus::NeedsSetup
        );
        assert_eq!(
            plugin_status(true, true, "installed"),
            PluginStatus::Enabled
        );
        assert_eq!(
            plugin_status(true, false, "installed"),
            PluginStatus::Installed
        );
        assert_eq!(PluginStatus::NeedsSetup.label(), "Needs setup");
    }

    #[test]
    fn buttons_match_the_state_and_stay_off_until_a_plugin_can_run() {
        // The runtime is off, so Install and Enable are not offered.
        assert!(actions_for(&detail(false, false, false, "http")).is_empty());
        assert_eq!(
            actions_for(&detail(true, false, true, "http")),
            vec![PluginAction::Uninstall]
        );
        assert_eq!(
            actions_for(&detail(true, true, true, "http")),
            vec![PluginAction::Disable, PluginAction::Uninstall]
        );
        // Once a real host can be called, the buttons follow the flags.
        let ready = detail(true, false, true, "http");
        assert_eq!(
            actions_when(&ready, false),
            vec![PluginAction::Enable, PluginAction::Uninstall]
        );
        assert_eq!(
            actions_when(&detail(false, false, false, "http"), false),
            vec![PluginAction::Install]
        );
        assert_eq!(
            actions_when(&detail(true, false, false, "http"), false),
            vec![PluginAction::Uninstall]
        );
    }

    #[test]
    fn placeholder_hosts_and_local_programs_stay_blocked_after_the_runtime_exists() {
        let mut placeholder = detail(false, false, false, "http");
        placeholder.plugin.permissions.egress = vec!["mcp.firecrawl.example.invalid".into()];
        assert!(placeholder.uses_placeholder_host());
        assert_eq!(
            placeholder.activation_block_when(true),
            Some(ActivationBlock::PlaceholderHost)
        );
        assert!(actions_for(&placeholder).is_empty());
        assert!(placeholder_host("https://docs.example.invalid/keys"));
        assert!(!placeholder_host("api.example"));

        let local = detail(false, false, false, "stdio");
        assert_eq!(
            local.activation_block_when(true),
            Some(ActivationBlock::LocalProgram)
        );
        assert!(
            local
                .activation_block()
                .unwrap()
                .message()
                .contains("program")
        );

        let real = detail(false, false, false, "http");
        assert_eq!(real.activation_block_when(true), None);
        assert_eq!(real.activation_block(), Some(ActivationBlock::Runtime));
    }

    #[test]
    fn a_plugin_with_no_fields_can_be_enabled_once_it_can_run() {
        let mut d = detail(true, false, true, "http");
        d.plugin.apps[0].auth.clear();
        assert!(d.fully_configured());
        assert_eq!(actions_when(&d, false)[0], PluginAction::Enable);
    }

    #[test]
    fn a_missing_plugin_is_not_a_missing_marketplace() {
        assert_eq!(
            marketplace_problem(MarketplaceCall::List, true, "gone".into()),
            MarketplaceProblem::Unavailable
        );
        assert_eq!(
            marketplace_problem(MarketplaceCall::Detail, true, "gone".into()),
            MarketplaceProblem::Message("That plugin is not in the catalog.".into())
        );
        let write = marketplace_problem(MarketplaceCall::Write, true, "gone".into());
        assert!(matches!(write, MarketplaceProblem::Message(ref text) if text.contains("Refresh")));
        assert_eq!(
            marketplace_problem(
                MarketplaceCall::Write,
                false,
                "plugin app files starts a local program, which this server does not run yet"
                    .into()
            ),
            MarketplaceProblem::Message(
                "plugin app files starts a local program, which this server does not run yet"
                    .into()
            )
        );
        let refused = marketplace_problem(MarketplaceCall::Write, false, "invalid request".into());
        let MarketplaceProblem::Message(text) = refused else {
            panic!("a refusal is a sentence");
        };
        assert!(text.contains("Refresh"));
        assert!(!text.contains("invalid request"));
        let raw = marketplace_problem(
            MarketplaceCall::List,
            false,
            r#"{"error":"secret-value"}"#.into(),
        );
        let MarketplaceProblem::Message(text) = raw else {
            panic!("json is a sentence");
        };
        assert!(!text.contains("secret-value"));
        assert!(!text.contains('{'));
        let dropped = marketplace_problem(
            MarketplaceCall::List,
            false,
            "Can't reach SeraBot right now. Retrying…".into(),
        );
        let MarketplaceProblem::Message(text) = dropped else {
            panic!("a dropped connection is a sentence");
        };
        assert!(text.contains("retry"));
        assert!(!text.to_ascii_lowercase().contains("retrying"));
    }

    #[test]
    fn local_program_plugins_are_flagged() {
        assert!(detail(false, false, false, "stdio").uses_local_program());
        assert!(!detail(false, false, false, "http").uses_local_program());
    }

    #[test]
    fn search_and_category_filter_together() {
        let all = vec![
            summary("a", "Firecrawl", "research", false, false, ""),
            summary("b", "Resend", "Email", false, false, ""),
            summary("c", "Brave Search", "research", true, true, ""),
        ];
        assert_eq!(filter_plugins(&all, "", "").len(), 3);
        assert_eq!(filter_plugins(&all, "brave", "").len(), 1);
        // The query also matches the category word.
        assert_eq!(filter_plugins(&all, "search", "").len(), 2);
        assert_eq!(filter_plugins(&all, "", "RESEARCH").len(), 2);
        assert_eq!(filter_plugins(&all, "tools", "email").len(), 1);
        assert!(filter_plugins(&all, "zzz", "").is_empty());
        assert_eq!(categories(&all), vec!["Email", "research"]);
    }

    #[test]
    fn marketplace_layout_rules_stay_in_place() {
        // The card grid keeps its 280 px minimum, and the sheet and cards exist.
        let css = include_str!("../assets/foundation.css");
        for needle in [
            "grid-template-columns: repeat(auto-fill, minmax(280px, 1fr));",
            ".mp-card {",
            ".mp-sheet {",
            ".mp-field {",
            ".mp-toolbar .seg {",
        ] {
            assert!(css.contains(needle), "missing marketplace rule: {needle}");
        }
    }

    #[test]
    fn only_plain_https_links_open() {
        assert!(safe_link("https://example.com/privacy"));
        assert!(!safe_link("http://example.com"));
        assert!(!safe_link("javascript:alert(1)"));
        assert!(!safe_link("file:///etc/passwd"));
        assert!(!safe_link("https://user:pw@example.com"));
        assert!(!safe_link("not a url"));
    }

    #[test]
    fn config_sends_only_filled_fields_and_rejects_bad_input() {
        let field = |id: &str| ConfigField {
            id: id.into(),
            label: id.to_uppercase(),
            hint: String::new(),
            docs_url: None,
            configured: false,
        };
        let (a, b) = (field("a"), field("b"));
        let fields = [&a, &b];
        let values =
            config_values(&fields, |id| if id == "a" { "  secret-1 " } else { "" }).unwrap();
        let plain: Vec<(&str, &str)> = values
            .iter()
            .map(|(k, v)| (k.as_str(), v.as_str()))
            .collect();
        assert_eq!(plain, vec![("a", "secret-1")]);
        assert!(config_values(&fields, |_| "").is_err());
        let err =
            config_values(&fields, |id| if id == "b" { "two\nlines" } else { "" }).unwrap_err();
        assert!(err.contains('B'));
        assert!(!err.contains("two"), "the message must not echo the value");
    }
}
