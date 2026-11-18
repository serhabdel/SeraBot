//! The Marketplace screen's data and rules (v0.37): the plugin catalog, one
//! user's install state, and the config form. Plain data so it can be tested
//! without a window.
//!
//! Secrets: the server never returns a secret value, only whether a field is
//! configured. This module has no field that could hold a stored value; the
//! only secret text on the client is what the user types into a form, and it
//! is cleared as soon as it is sent.
//!
//! Plugins on real `http` hosts can be called through the approval-gated
//! runtime; some shipped manifests still use placeholder hosts
//! (`*.example.invalid`). Install and Enable stay off for those, so a click
//! cannot end at a plugin that looks on and does nothing. Disable and Uninstall stay available so an install made
//! through the API can still be turned off and removed.

use serde::Deserialize;

/// Upper bounds, so a bad response cannot balloon the screen.
pub const MAX_PLUGINS: usize = 500;
pub const MAX_TEXT: usize = 4_000;
pub const MAX_ITEMS: usize = 64;

/// Whether the tool runtime can call installed `http` MCP plugins.
/// Placeholder hosts (`*.example.invalid`) and `stdio` local-program plugins
/// still stay blocked even when this is `true`.
pub const RUNTIME_CALLS_PLUGINS: bool = true;

/// The line at the top of the screen while [`RUNTIME_CALLS_PLUGINS`] is false.
pub const COMING_SOON: &str = "Coming soon. You can browse plugins and see what they connect to. Install and Enable stay off until plugin calling is turned on and each plugin points at a live service.";

/// What a card shows as its status.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PluginStatus {
    /// Not installed, and not offered yet.
    ComingSoon,
    /// Not installed, and install is allowed.
    Available,
    /// Installed but a required key is missing.
    NeedsSetup,
    /// Installed, and no account is signed in yet (or it was signed out).
    NeedsSignIn,
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
            Self::NeedsSignIn => "Needs sign-in",
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
    if status == "needs_sign_in" {
        return PluginStatus::NeedsSignIn;
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
    /// Whether this app lets the member sign in with more than one account.
    #[serde(default)]
    pub multiple_accounts: bool,
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
    /// The member's sign-ins for an OAuth plugin. Never holds a token: the
    /// server reports a state word and an optional short error only.
    #[serde(default)]
    pub accounts: Vec<PluginAccount>,
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
                "Not live yet. This plugin does not point at a live service yet, so it cannot be installed or turned on."
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
    } else if !blocked && detail.fully_configured() && detail.signed_in() {
        actions.push(PluginAction::Enable);
    }
    actions.push(PluginAction::Uninstall);
    actions
}

/// Quick status filter for the Marketplace tab.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum StatusFilter {
    #[default]
    All,
    Installed,
    Available,
    ComingSoon,
}

impl StatusFilter {
    pub fn label(self) -> &'static str {
        match self {
            Self::All => "All",
            Self::Installed => "Installed",
            Self::Available => "Available",
            Self::ComingSoon => "Coming soon",
        }
    }

    pub fn matches(self, status: PluginStatus) -> bool {
        match self {
            Self::All => true,
            Self::Installed => matches!(
                status,
                PluginStatus::Installed
                    | PluginStatus::Enabled
                    | PluginStatus::NeedsSetup
                    | PluginStatus::NeedsSignIn
            ),
            Self::Available => status == PluginStatus::Available,
            Self::ComingSoon => status == PluginStatus::ComingSoon,
        }
    }
}

/// Case-insensitive search over name, tagline and category, plus an optional
/// category. An empty category means all.
pub fn filter_plugins<'a>(
    plugins: &'a [PluginSummary],
    query: &str,
    category: &str,
) -> Vec<&'a PluginSummary> {
    filter_plugins_with_status(plugins, query, category, StatusFilter::All)
}

/// Case-insensitive search combined with category and install-status filter.
pub fn filter_plugins_with_status<'a>(
    plugins: &'a [PluginSummary],
    query: &str,
    category: &str,
    status_filter: StatusFilter,
) -> Vec<&'a PluginSummary> {
    let needle = query.trim().to_lowercase();
    plugins
        .iter()
        .filter(|plugin| category.is_empty() || plugin.category.eq_ignore_ascii_case(category))
        .filter(|plugin| status_filter.matches(plugin.state()))
        .filter(|plugin| {
            needle.is_empty()
                || plugin.name.to_lowercase().contains(&needle)
                || plugin.tagline.to_lowercase().contains(&needle)
                || plugin.category.to_lowercase().contains(&needle)
        })
        .collect()
}

/// The categories present, sorted, without repeats or blanks, with counts.
pub fn category_counts(plugins: &[PluginSummary]) -> Vec<(String, usize)> {
    let mut found: Vec<(String, usize)> = Vec::new();
    for plugin in plugins {
        let cat = plugin.category.trim();
        if cat.is_empty() {
            continue;
        }
        if let Some((_, count)) = found
            .iter_mut()
            .find(|(existing, _)| existing.eq_ignore_ascii_case(cat))
        {
            *count += 1;
        } else {
            found.push((cat.to_owned(), 1));
        }
    }
    found.sort_by_key(|(category, _)| category.to_lowercase());
    found
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

// ---- Accounts, sign-in, tool switches and the GitHub card (v0.51) ----

/// The most accounts one plugin lists, and the longest tool list shown.
pub const MAX_ACCOUNTS: usize = 8;
pub const MAX_TOOLS: usize = 200;

/// Where one sign-in of a plugin stands. The server reports the word; an
/// unknown word reads as "needs sign-in", the safe thing to offer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AccountState {
    NeedsSetup,
    NeedsSignIn,
    Ready,
    Unreachable,
}

impl AccountState {
    pub fn parse(word: &str) -> Self {
        match word {
            "needs_setup" => Self::NeedsSetup,
            "ready" => Self::Ready,
            "unreachable" => Self::Unreachable,
            _ => Self::NeedsSignIn,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::NeedsSetup => "Needs setup",
            Self::NeedsSignIn => "Needs sign-in",
            Self::Ready => "Ready",
            Self::Unreachable => "Unreachable",
        }
    }

    /// One calm sentence under the badge.
    pub fn hint(self) -> &'static str {
        match self {
            Self::NeedsSetup => "The sign-in setup was lost. Sign in again to set it up.",
            Self::NeedsSignIn => "Sign in again so your bots can use this account.",
            Self::Ready => "Your bots can use this account.",
            Self::Unreachable => {
                "The service did not answer. Nothing was lost, and it is tried again on the next use."
            }
        }
    }

    /// What the row offers: a new sign-in, or just a re-check.
    pub fn action(self) -> AccountAction {
        match self {
            Self::NeedsSetup | Self::NeedsSignIn => AccountAction::SignIn,
            Self::Unreachable => AccountAction::Recheck,
            Self::Ready => AccountAction::None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AccountAction {
    SignIn,
    Recheck,
    None,
}

/// One sign-in of an OAuth plugin, as the server lists it.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
pub struct PluginAccount {
    pub account_id: String,
    #[serde(default)]
    pub label: String,
    #[serde(default)]
    pub state: String,
    #[serde(default)]
    pub last_error: Option<String>,
    #[serde(default)]
    pub last_ok_at: Option<String>,
}

impl PluginAccount {
    pub fn state(&self) -> AccountState {
        AccountState::parse(&self.state)
    }

    /// The label the member gave it, or its id.
    pub fn title(&self) -> &str {
        let label = self.label.trim();
        if label.is_empty() {
            &self.account_id
        } else {
            label
        }
    }

    /// The last failure as a sentence, only while the account is not ready.
    pub fn error_line(&self) -> Option<String> {
        if self.state() == AccountState::Ready {
            return None;
        }
        let text = self.last_error.as_deref()?.trim();
        if text.is_empty() {
            return None;
        }
        Some(plain_failure(text.to_owned()))
    }
}

/// An account id the server accepts: short, lowercase, safe in a stored key.
pub fn valid_account_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 32
        && id
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'_')
}

/// The id of a plugin's first account.
pub const DEFAULT_ACCOUNT: &str = "default";

/// The id for one more account: the first free `account-N`.
pub fn next_account_id(existing: &[PluginAccount]) -> String {
    if !existing.iter().any(|a| a.account_id == DEFAULT_ACCOUNT) {
        return DEFAULT_ACCOUNT.to_owned();
    }
    (2..)
        .map(|n| format!("account-{n}"))
        .find(|id| !existing.iter().any(|a| &a.account_id == id))
        .unwrap_or_else(|| "account-99".to_owned())
}

/// A label the member typed: one line, bounded. Empty means "use the plugin's
/// name".
pub fn valid_account_label(label: &str) -> bool {
    label.chars().count() <= 60 && !label.chars().any(char::is_control)
}

/// The sign-in button of a plugin page.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SignInOffer {
    /// "Sign in", "Add account" or "Sign in again".
    pub label: &'static str,
    pub account_id: String,
    /// True when it signs an existing account in again.
    pub renew: bool,
}

impl PluginDetail {
    /// The first `http` app that signs in with OAuth.
    pub fn oauth_app(&self) -> Option<&PluginApp> {
        self.plugin
            .apps
            .iter()
            .find(|app| app.transport == "http" && app.auth.iter().any(|a| a.kind == "oauth"))
    }

    pub fn uses_oauth(&self) -> bool {
        self.oauth_app().is_some()
    }

    pub fn accounts(&self) -> &[PluginAccount] {
        &self.plugin.accounts
    }

    /// Whether the app lets the member add a second account.
    pub fn allows_multiple_accounts(&self) -> bool {
        self.oauth_app().is_some_and(|app| app.multiple_accounts)
    }

    /// Whether the plugin has an account that can be used, or has no sign-in at
    /// all. Mirrors the server: `unreachable` still counts, since nothing was lost.
    pub fn signed_in(&self) -> bool {
        !self.uses_oauth()
            || self.plugin.accounts.iter().any(|account| {
                matches!(
                    account.state(),
                    AccountState::Ready | AccountState::Unreachable
                )
            })
    }

    /// The main sign-in button, when one makes sense: an installed OAuth plugin
    /// that can run. Gone while Install and Enable are blocked, so nothing
    /// invites a sign-in for a service that cannot be called.
    pub fn sign_in_offer(&self) -> Option<SignInOffer> {
        if !self.installed() || !self.uses_oauth() || self.activation_block().is_some() {
            return None;
        }
        let accounts = self.accounts();
        match accounts
            .iter()
            .find(|a| a.state().action() == AccountAction::SignIn)
        {
            Some(account) => Some(SignInOffer {
                label: "Sign in again",
                account_id: account.account_id.clone(),
                renew: true,
            }),
            None if accounts.is_empty() => Some(SignInOffer {
                label: "Sign in",
                account_id: DEFAULT_ACCOUNT.to_owned(),
                renew: false,
            }),
            None => None,
        }
    }

    /// The "Add account" button: only for an app that allows several accounts,
    /// once there is a first one, and below the cap.
    pub fn add_account_offer(&self) -> Option<SignInOffer> {
        if !self.installed()
            || !self.allows_multiple_accounts()
            || self.activation_block().is_some()
            || self.accounts().is_empty()
            || self.accounts().len() >= MAX_ACCOUNTS
        {
            return None;
        }
        Some(SignInOffer {
            label: "Add account",
            account_id: next_account_id(self.accounts()),
            renew: false,
        })
    }
}

/// What the owner's switch for one tool sends.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ToolMode {
    Default,
    Ask,
    Allow,
}

impl ToolMode {
    pub fn word(self) -> &'static str {
        match self {
            Self::Default => "default",
            Self::Ask => "ask",
            Self::Allow => "allow",
        }
    }
}

/// One tool of a plugin, with the server's hints and the owner's override.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
pub struct PluginTool {
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub read_only: bool,
    #[serde(default)]
    pub destructive: bool,
    #[serde(default)]
    pub mode: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct ToolList {
    #[serde(default)]
    pub tools: Vec<PluginTool>,
}

/// A tool name the server's route accepts.
pub fn valid_tool_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 128
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.'))
}

impl PluginTool {
    pub fn override_mode(&self) -> ToolMode {
        match self.mode.as_deref() {
            Some("ask") => ToolMode::Ask,
            Some("allow") => ToolMode::Allow,
            _ => ToolMode::Default,
        }
    }

    /// Whether a call runs without asking. The same rule the server applies:
    /// `ask` always asks, `allow` frees a tool that is not destructive, and
    /// otherwise only a read-only tool runs freely.
    pub fn runs_without_asking(&self) -> bool {
        match self.override_mode() {
            ToolMode::Ask => false,
            ToolMode::Allow if !self.destructive => true,
            _ => self.read_only && !self.destructive,
        }
    }

    /// A destructive tool can never be relaxed, so its switch stays off.
    pub fn locked(&self) -> bool {
        self.destructive
    }

    /// What flipping the switch stores: the opposite of today's behaviour, or
    /// `Default` when that is already what the opposite means (so no redundant
    /// override is kept). `None` when the tool is locked and would be relaxed.
    pub fn flip(&self) -> Option<ToolMode> {
        let want_free = !self.runs_without_asking();
        if want_free && self.locked() {
            return None;
        }
        let default_free = self.read_only && !self.destructive;
        Some(if want_free == default_free {
            ToolMode::Default
        } else if want_free {
            ToolMode::Allow
        } else {
            ToolMode::Ask
        })
    }

    /// The same tool after the owner's choice, for an instant redraw.
    pub fn with_mode(&self, mode: ToolMode) -> Self {
        Self {
            mode: match mode {
                ToolMode::Default => None,
                other => Some(other.word().to_owned()),
            },
            ..self.clone()
        }
    }

    /// One line on what the tool can do and what the switch decides.
    pub fn note(&self) -> &'static str {
        if self.destructive {
            "Can delete or overwrite. Always asks first."
        } else if self.read_only {
            "Only reads. Runs without asking unless you switch it off."
        } else {
            "Can change data. Asks first unless you switch it on."
        }
    }
}

/// The tool list as the screen keeps it: bounded, valid names, sorted.
pub fn clean_tools(mut tools: Vec<PluginTool>) -> Vec<PluginTool> {
    tools.retain(|tool| valid_tool_name(&tool.name));
    tools.truncate(MAX_TOOLS);
    for tool in &mut tools {
        if tool.description.chars().count() > 400 {
            tool.description = tool.description.chars().take(400).collect();
        }
    }
    tools
}

/// A started marketplace sign-in: the page to open, the state its redirect
/// must carry back, and where the console listens for it.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
pub struct OauthStarted {
    pub authorize_url: String,
    pub state: String,
    pub account_id: String,
    #[serde(default)]
    pub redirect_uri: String,
}

/// The port of a loopback redirect `http://127.0.0.1:{port}/oauth-callback`,
/// and nothing else: the console never listens anywhere a plugin names.
pub fn loopback_port(redirect_uri: &str) -> Option<u16> {
    let rest = redirect_uri.strip_prefix("http://127.0.0.1:")?;
    let (port, path) = rest.split_once('/')?;
    if format!("/{path}") != crate::oauth::CALLBACK_PATH {
        return None;
    }
    port.parse::<u16>().ok().filter(|port| *port > 0)
}

/// A started sign-in is only used when its page is plain https and its state
/// and redirect are shaped as expected.
pub fn check_oauth_started(started: &OauthStarted) -> bool {
    safe_link(&started.authorize_url)
        && started.authorize_url.len() <= 8_192
        && (8..=256).contains(&started.state.len())
        && started
            .state
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~'))
        && valid_account_id(&started.account_id)
        && loopback_port(&started.redirect_uri).is_some()
}

/// The reply of `.../oauth/complete`. Only the account id is read.
#[derive(Debug, Deserialize)]
pub struct OauthDone {
    pub account_id: String,
}

/// Where the member's native GitHub sign-in stands.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GithubState {
    /// The server has no GitHub app configured.
    NotConfigured,
    /// Ready to sign in; nobody is signed in.
    Ready,
    Connected,
    /// Signed in before, and the sign-in ran out. The login is kept.
    Expired,
}

/// `GET /v1/connectors/github/native`. No token is ever part of it.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
pub struct GithubView {
    #[serde(default)]
    pub state: String,
    #[serde(default)]
    pub configured: bool,
    #[serde(default)]
    pub login: Option<String>,
    #[serde(default)]
    pub pending: bool,
    #[serde(default)]
    pub manage_url: Option<String>,
}

impl GithubView {
    pub fn state(&self) -> GithubState {
        match self.state.as_str() {
            "connected" => GithubState::Connected,
            "expired" => GithubState::Expired,
            "ready" => GithubState::Ready,
            _ if self.configured => GithubState::Ready,
            _ => GithubState::NotConfigured,
        }
    }

    pub fn badge(&self) -> &'static str {
        match self.state() {
            GithubState::NotConfigured => "Not set up",
            GithubState::Ready => "Not connected",
            GithubState::Connected => "Connected",
            GithubState::Expired => "Needs sign-in",
        }
    }

    /// The signed-in login, one short line, or none.
    pub fn login_line(&self) -> Option<String> {
        let login = self.login.as_deref()?.trim();
        let ok = !login.is_empty()
            && login.len() <= 64
            && login
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'));
        ok.then(|| format!("@{login}"))
    }

    /// Sign in is offered unless connected, or the server has no GitHub app.
    pub fn can_sign_in(&self) -> bool {
        matches!(self.state(), GithubState::Ready | GithubState::Expired)
    }

    pub fn can_disconnect(&self) -> bool {
        matches!(self.state(), GithubState::Connected | GithubState::Expired)
    }

    /// The sentence under the badge.
    pub fn hint(&self) -> &'static str {
        match self.state() {
            GithubState::NotConfigured => {
                "This server has no GitHub app yet. Ask the owner to set it up, or use a token below."
            }
            GithubState::Ready => {
                "Sign in with a one-time code. Your bots get read tools at once; changes ask first."
            }
            GithubState::Connected => "Your bots can use GitHub. Changes still ask first.",
            GithubState::Expired => "Your GitHub sign-in ran out. Sign in again to keep going.",
        }
    }
}

/// `POST /v1/connectors/github/device/start`.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
pub struct GithubDeviceStarted {
    pub user_code: String,
    pub verification_uri: String,
    pub expires_in: u64,
    #[serde(default)]
    pub interval: u64,
}

/// A device code is shown and opened only when it is a short plain code and a
/// plain https page.
pub fn check_device_started(started: &GithubDeviceStarted) -> bool {
    !started.user_code.is_empty()
        && started.user_code.len() <= 32
        && !started.user_code.chars().any(char::is_control)
        && safe_link(&started.verification_uri)
        && (1..=3_600).contains(&started.expires_in)
        && started.interval <= 120
}

/// `GET /v1/connectors/github/device/status`.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
pub struct GithubDeviceStatus {
    /// `pending`, `connected`, `denied` or `expired`.
    pub status: String,
    #[serde(default)]
    pub login: Option<String>,
    #[serde(default)]
    pub reason: Option<String>,
}

/// The reply of the disconnect call: the app's page on GitHub where the
/// member can also revoke it.
#[derive(Debug, Deserialize)]
pub struct GithubDisconnected {
    #[serde(default)]
    pub manage_url: Option<String>,
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
                    multiple_accounts: false,
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
                accounts: vec![],
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
            PluginStatus::Available
        );

        // The server nests installed/enabled/status inside `plugin`. Flags
        // sitting beside `plugin` are not the install state.
        let beside: PluginDetail = serde_json::from_str(
            r#"{"plugin":{"slug":"search","name":"Search"},"installed":true,"enabled":true,"status":"installed"}"#,
        )
        .unwrap();
        assert!(!beside.installed());
        assert_eq!(beside.state(), PluginStatus::Available);

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
        assert_eq!(sparse.state(), PluginStatus::Available);
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
    fn buttons_match_the_state_and_stay_off_when_blocked() {
        // When blocked (e.g. placeholder/runtime off), Install and Enable are not offered.
        assert!(actions_when(&detail(false, false, false, "http"), true).is_empty());
        assert_eq!(
            actions_when(&detail(true, false, true, "http"), true),
            vec![PluginAction::Uninstall]
        );
        assert_eq!(
            actions_when(&detail(true, true, true, "http"), true),
            vec![PluginAction::Disable, PluginAction::Uninstall]
        );
        // With runtime enabled and a real host, the buttons follow the flags.
        let ready = detail(true, false, true, "http");
        assert_eq!(
            actions_for(&ready),
            vec![PluginAction::Enable, PluginAction::Uninstall]
        );
        assert_eq!(
            actions_for(&detail(false, false, false, "http")),
            vec![PluginAction::Install]
        );
        assert_eq!(
            actions_for(&detail(true, false, false, "http")),
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
        assert_eq!(real.activation_block(), None);
        assert_eq!(
            real.activation_block_when(false),
            Some(ActivationBlock::Runtime)
        );
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

    fn oauth_detail(installed: bool, accounts: &[(&str, &str)], multiple: bool) -> PluginDetail {
        let mut d = detail(installed, false, true, "http");
        d.plugin.apps[0].multiple_accounts = multiple;
        d.plugin.apps[0].auth = vec![AuthBlock {
            id: "oauth".into(),
            kind: "oauth".into(),
            fields: vec![],
        }];
        d.plugin.status = if accounts.iter().any(|(_, state)| *state == "ready") {
            "installed".into()
        } else {
            "needs_sign_in".into()
        };
        d.plugin.accounts = accounts
            .iter()
            .map(|(id, state)| PluginAccount {
                account_id: (*id).into(),
                label: String::new(),
                state: (*state).into(),
                last_error: None,
                last_ok_at: None,
            })
            .collect();
        d
    }

    #[test]
    fn account_states_have_badges_hints_and_the_right_action() {
        for (word, state, label, action) in [
            ("ready", AccountState::Ready, "Ready", AccountAction::None),
            (
                "needs_sign_in",
                AccountState::NeedsSignIn,
                "Needs sign-in",
                AccountAction::SignIn,
            ),
            (
                "needs_setup",
                AccountState::NeedsSetup,
                "Needs setup",
                AccountAction::SignIn,
            ),
            (
                "unreachable",
                AccountState::Unreachable,
                "Unreachable",
                AccountAction::Recheck,
            ),
        ] {
            assert_eq!(AccountState::parse(word), state);
            assert_eq!(state.label(), label);
            assert_eq!(state.action(), action);
            assert!(!state.hint().is_empty());
        }
        // A word this app does not know asks for a sign-in instead of claiming ready.
        assert_eq!(AccountState::parse("surprise"), AccountState::NeedsSignIn);
        // Unreachable keeps the tokens, so it says nothing was lost.
        assert!(
            AccountState::Unreachable
                .hint()
                .contains("Nothing was lost")
        );
    }

    #[test]
    fn the_server_word_needs_sign_in_has_its_own_badge_and_filter() {
        assert_eq!(
            plugin_status(true, false, "needs_sign_in"),
            PluginStatus::NeedsSignIn
        );
        assert_eq!(PluginStatus::NeedsSignIn.label(), "Needs sign-in");
        // Not installed wins over the word.
        assert_eq!(
            plugin_status(false, false, "needs_sign_in"),
            PluginStatus::Available
        );
        // Setup comes first when both apply.
        assert_eq!(
            plugin_status(true, false, "needs_config"),
            PluginStatus::NeedsSetup
        );
        assert!(StatusFilter::Installed.matches(PluginStatus::NeedsSignIn));
        let list = vec![summary(
            "a",
            "Canva",
            "design",
            true,
            false,
            "needs_sign_in",
        )];
        assert_eq!(
            filter_plugins_with_status(&list, "", "", StatusFilter::Installed).len(),
            1
        );
    }

    #[test]
    fn a_detail_parses_its_accounts_and_never_holds_a_token() {
        let parsed: PluginDetail = serde_json::from_str(
            r#"{"plugin":{"slug":"canva","name":"Canva","installed":true,"enabled":false,
                "status":"needs_sign_in",
                "apps":[{"id":"mcp","name":"Canva","transport":"http","multiple_accounts":true,
                   "auth":[{"id":"o","kind":"oauth","fields":[]}]}],
                "accounts":[{"account_id":"default","label":"Work","state":"unreachable",
                   "last_error":"plugin canva could not be reached","last_ok_at":null}]}}"#,
        )
        .unwrap();
        assert!(parsed.uses_oauth());
        assert!(parsed.allows_multiple_accounts());
        assert_eq!(parsed.state(), PluginStatus::NeedsSignIn);
        let account = &parsed.accounts()[0];
        assert_eq!(account.title(), "Work");
        assert_eq!(account.state(), AccountState::Unreachable);
        assert_eq!(
            account.error_line().as_deref(),
            Some("plugin canva could not be reached")
        );
        // Unreachable counts as signed in, like the server's status word.
        assert!(parsed.signed_in());
        // A ready account shows no error line even if one was left behind.
        let ready = PluginAccount {
            state: "ready".into(),
            last_error: Some("old".into()),
            ..account.clone()
        };
        assert_eq!(ready.error_line(), None);
        // An error that is JSON is replaced by a plain line.
        let raw = PluginAccount {
            last_error: Some(r#"{"error":"x"}"#.into()),
            state: "needs_sign_in".into(),
            ..account.clone()
        };
        assert!(!raw.error_line().unwrap().contains('{'));
    }

    #[test]
    fn sign_in_is_offered_only_where_it_can_work() {
        // Installed, no account: Sign in as the default account.
        let fresh = oauth_detail(true, &[], false);
        let offer = fresh.sign_in_offer().unwrap();
        assert_eq!(
            (offer.label, offer.account_id.as_str(), offer.renew),
            ("Sign in", "default", false)
        );
        // Not installed: nothing to sign in to yet.
        assert_eq!(oauth_detail(false, &[], false).sign_in_offer(), None);
        // A signed-out account is signed in again, in place.
        let out = oauth_detail(true, &[("default", "needs_sign_in")], false);
        let again = out.sign_in_offer().unwrap();
        assert_eq!((again.label, again.renew), ("Sign in again", true));
        // A ready account needs no button; one-account apps offer no second.
        let ready = oauth_detail(true, &[("default", "ready")], false);
        assert_eq!(ready.sign_in_offer(), None);
        assert_eq!(ready.add_account_offer(), None);
        // A multi-account app offers Add account with a fresh id.
        let multi = oauth_detail(true, &[("default", "ready")], true);
        assert_eq!(multi.add_account_offer().unwrap().account_id, "account-2");
        let more = oauth_detail(true, &[("default", "ready"), ("account-2", "ready")], true);
        assert_eq!(more.add_account_offer().unwrap().account_id, "account-3");
        // The cap holds.
        let ids: Vec<(String, &str)> = (0..MAX_ACCOUNTS)
            .map(|n| (format!("a{n}"), "ready"))
            .collect();
        let pairs: Vec<(&str, &str)> = ids.iter().map(|(a, b)| (a.as_str(), *b)).collect();
        assert_eq!(oauth_detail(true, &pairs, true).add_account_offer(), None);
        // A placeholder host gets no sign-in either.
        let mut placeholder = oauth_detail(true, &[], false);
        placeholder.plugin.permissions.egress = vec!["mcp.example.invalid".into()];
        assert_eq!(placeholder.sign_in_offer(), None);
        // A key plugin has no sign-in at all.
        assert_eq!(detail(true, false, true, "http").sign_in_offer(), None);
        assert!(valid_account_id("account-2") && !valid_account_id("Bad Id"));
        assert!(valid_account_label("Work") && !valid_account_label("a\nb"));
    }

    #[test]
    fn enable_waits_for_a_usable_account() {
        let out = oauth_detail(true, &[("default", "needs_sign_in")], false);
        assert_eq!(actions_for(&out), vec![PluginAction::Uninstall]);
        let ready = oauth_detail(true, &[("default", "ready")], false);
        assert_eq!(
            actions_for(&ready),
            vec![PluginAction::Enable, PluginAction::Uninstall]
        );
    }

    fn tool(name: &str, read_only: bool, destructive: bool, mode: Option<&str>) -> PluginTool {
        PluginTool {
            name: name.into(),
            description: String::new(),
            read_only,
            destructive,
            mode: mode.map(str::to_owned),
        }
    }

    #[test]
    fn a_tool_switch_mirrors_the_servers_approval_rule() {
        // Read only: free by default; the switch makes it ask.
        let read = tool("lookup", true, false, None);
        assert!(read.runs_without_asking());
        assert_eq!(read.flip(), Some(ToolMode::Ask));
        // Asking after an override; flipping back clears it.
        let asks = read.with_mode(ToolMode::Ask);
        assert!(!asks.runs_without_asking());
        assert_eq!(asks.flip(), Some(ToolMode::Default));
        // A tool that changes data: asks by default; the switch frees it.
        let write = tool("post", false, false, None);
        assert!(!write.runs_without_asking());
        assert_eq!(write.flip(), Some(ToolMode::Allow));
        let freed = write.with_mode(ToolMode::Allow);
        assert!(freed.runs_without_asking());
        assert_eq!(freed.flip(), Some(ToolMode::Default));
        // A destructive tool cannot be relaxed, even by a stored `allow`.
        let danger = tool("delete", false, true, Some("allow"));
        assert!(!danger.runs_without_asking());
        assert!(danger.locked());
        assert_eq!(danger.flip(), None);
        // A contradictory claim (read only and destructive) is destructive.
        assert!(!tool("liar", true, true, None).runs_without_asking());
        // Overrides round trip through the words the server takes.
        assert_eq!(
            write.with_mode(ToolMode::Allow).mode.as_deref(),
            Some("allow")
        );
        assert_eq!(write.with_mode(ToolMode::Default).mode, None);
        assert_eq!(ToolMode::Ask.word(), "ask");
        assert!(read.note().contains("reads") && danger.note().contains("Always asks"));
    }

    #[test]
    fn the_tool_list_is_cleaned_before_it_is_shown() {
        let parsed: ToolList = serde_json::from_str(
            r#"{"slug":"x","tools":[{"name":"a_b","read_only":true},
                {"name":"bad name"},{"name":"c.d","mode":"ask","destructive":false}]}"#,
        )
        .unwrap();
        let cleaned = clean_tools(parsed.tools);
        assert_eq!(cleaned.len(), 2);
        assert_eq!(cleaned[1].override_mode(), ToolMode::Ask);
        let mut long = tool("x", false, false, None);
        long.description = "é".repeat(450);
        assert_eq!(clean_tools(vec![long])[0].description.chars().count(), 400);
    }

    #[test]
    fn a_started_sign_in_is_used_only_when_it_is_well_formed() {
        let ok = OauthStarted {
            authorize_url: "https://auth.example/authorize?x=1".into(),
            state: "abcdefghijklmnop".into(),
            account_id: "default".into(),
            redirect_uri: "http://127.0.0.1:51122/oauth-callback".into(),
        };
        assert!(check_oauth_started(&ok));
        assert_eq!(loopback_port(&ok.redirect_uri), Some(51122));
        let bad = |edit: &dyn Fn(&mut OauthStarted)| {
            let mut copy = ok.clone();
            edit(&mut copy);
            !check_oauth_started(&copy)
        };
        assert!(bad(&|s| s.authorize_url = "http://auth.example/a".into()));
        assert!(bad(&|s| s.authorize_url = "javascript:alert(1)".into()));
        assert!(bad(&|s| s.state = "short".into()));
        assert!(bad(&|s| s.state = "has space in it!!".into()));
        assert!(bad(&|s| s.account_id = "Bad Id".into()));
        // The console listens only on its own loopback callback.
        assert!(bad(
            &|s| s.redirect_uri = "http://127.0.0.1:51122/other".into()
        ));
        assert!(bad(
            &|s| s.redirect_uri = "http://evil.example:51122/oauth-callback".into()
        ));
        assert!(bad(
            &|s| s.redirect_uri = "http://localhost:51122/oauth-callback".into()
        ));
        assert!(bad(
            &|s| s.redirect_uri = "http://127.0.0.1:0/oauth-callback".into()
        ));
        assert!(bad(&|s| s.redirect_uri.clear()));
    }

    #[test]
    fn the_github_card_states_badges_and_buttons() {
        let view = |state: &str, configured: bool, login: Option<&str>| GithubView {
            state: state.into(),
            configured,
            login: login.map(str::to_owned),
            pending: false,
            manage_url: None,
        };
        let none = view("not_configured", false, None);
        assert_eq!(none.state(), GithubState::NotConfigured);
        assert!(!none.can_sign_in() && !none.can_disconnect());
        assert_eq!(none.badge(), "Not set up");
        let ready = view("ready", true, None);
        assert!(ready.can_sign_in() && !ready.can_disconnect());
        assert_eq!(ready.badge(), "Not connected");
        let on = view("connected", true, Some("octo-cat"));
        assert!(!on.can_sign_in() && on.can_disconnect());
        assert_eq!(on.login_line().as_deref(), Some("@octo-cat"));
        // Expired keeps the login and offers both sign in again and disconnect.
        let old = view("expired", true, Some("octo-cat"));
        assert!(old.can_sign_in() && old.can_disconnect());
        assert_eq!(old.badge(), "Needs sign-in");
        // A login that is not a plain handle is not shown.
        assert_eq!(view("connected", true, Some("a b<")).login_line(), None);
        // The server's extra fields are ignored; no token field exists to read.
        let parsed: GithubView = serde_json::from_str(
            r#"{"state":"connected","configured":true,"login":"o","access_expires_at":"x",
                "pending":false,"manage_url":"https://github.com/settings","tool_prefix":"github."}"#,
        )
        .unwrap();
        assert_eq!(parsed.state(), GithubState::Connected);
    }

    #[test]
    fn a_device_code_is_shown_only_when_it_is_plain() {
        let ok = GithubDeviceStarted {
            user_code: "ABCD-1234".into(),
            verification_uri: "https://github.com/login/device".into(),
            expires_in: 900,
            interval: 5,
        };
        assert!(check_device_started(&ok));
        let bad = |edit: &dyn Fn(&mut GithubDeviceStarted)| {
            let mut copy = ok.clone();
            edit(&mut copy);
            !check_device_started(&copy)
        };
        assert!(bad(&|s| s.user_code.clear()));
        assert!(bad(&|s| s.user_code = "x".repeat(40)));
        assert!(bad(
            &|s| s.verification_uri = "http://github.com/login/device".into()
        ));
        assert!(bad(&|s| s.expires_in = 0));
        assert!(bad(&|s| s.interval = 500));
    }

    #[test]
    fn marketplace_account_styles_keep_focus_rings_and_reduced_motion() {
        let css = include_str!("../assets/marketplace.css");
        for needle in [
            ".mp-account {",
            ".mp-account:focus-within {",
            ".mp-tool {",
            ".github-card {",
            "@media (prefers-reduced-motion: reduce)",
            ".root[data-motion=\"reduce\"] .mp-account",
        ] {
            assert!(css.contains(needle), "missing rule: {needle}");
        }
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

    #[test]
    fn status_filters_and_category_counts_are_accurate() {
        const {
            assert!(RUNTIME_CALLS_PLUGINS);
        }
        assert!(!COMING_SOON.contains("example.invalid"));
        assert!(
            !ActivationBlock::PlaceholderHost
                .message()
                .contains("example.invalid")
        );
        let all = vec![
            summary("a", "Firecrawl", "research", false, false, ""),
            summary("b", "Resend", "Email", true, false, "needs_config"),
            summary("c", "Brave Search", "research", true, true, ""),
        ];
        assert_eq!(
            category_counts(&all),
            vec![("Email".into(), 1), ("research".into(), 2)]
        );
        assert_eq!(
            filter_plugins_with_status(&all, "", "", StatusFilter::Installed).len(),
            2
        );
        assert_eq!(
            filter_plugins_with_status(&all, "", "", StatusFilter::Available).len(),
            1
        );
        assert_eq!(
            filter_plugins_with_status(&all, "", "", StatusFilter::ComingSoon).len(),
            0
        );
    }
}
