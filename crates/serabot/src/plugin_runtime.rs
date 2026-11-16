//! Plugin tools for a user's bots (v0.37, auth and annotations in v0.51).
//!
//! An installed and enabled `http` plugin exposes the tools its MCP servers
//! offer; every `http` app of a plugin is used. Each tool is named
//! `plugin.<slug>.<tool>` (`plugin.<slug>.<app>.<tool>` when a plugin has more
//! than one http app), so a name never collides with a built-in tool or with
//! another plugin. Everything here is per user: the install, the sealed
//! credential and the audit row all belong to the caller. Nothing is added to
//! the global tool registry; the set is read per turn, so a plugin disabled or
//! uninstalled drops out on the next turn.
//!
//! Credentials come from the manifest's auth entry for the app: an API key
//! sealed in the caller's store (sent in the header the manifest names), an
//! OAuth access token from the caller's marketplace account, or none. The
//! credential is read at call time and handed to the HTTP client only; it never
//! reaches a prompt, a tool description or an error. A `401` on an OAuth app
//! refreshes the token once and retries once.
//!
//! Each request is checked first: https only, no credentials, the host on the
//! manifest's egress list, and every resolved address public. Calls are audited
//! by plugin and tool name only; arguments and results are never written.
//!
//! Approval follows the tool's MCP annotations. A tool the server marks
//! `readOnlyHint` (or the manifest vouches for) runs without approval, still
//! through the gateway, so it is audited and its output is scanned. Every other
//! tool needs approval for each call. A tool marked `destructiveHint` can never
//! be relaxed, and no plugin tool gets a standing grant. The owner can set a
//! per-tool override: `ask` makes a read-only tool ask, `allow` lets a tool that
//! is not marked destructive run without asking. A server's own hints are
//! untrusted claims; the manifest's egress list and the owner's override bound
//! what they can do.

use std::collections::{BTreeMap, HashMap};
use std::future::Future;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use reqwest::Url;
use secrecy::{ExposeSecret, SecretString};
use serde_json::{Value, json};

use crate::error::{Result, SeraError};
use crate::gateway::{ApprovalDetails, Tool, ToolContext, ToolOutput, ToolSpec};
use crate::marketplace::{App, AuthKind, HeaderScheme, PluginManifest, Server};
use crate::mcp_client::{CatalogCache, McpAuth, McpClient, McpError};
use crate::mcp_oauth::{OauthAccounts, OauthNet, RefreshOutcome};
use crate::store::Store;
use crate::user_secrets::UserSecrets;

/// Prefix shared by every plugin tool name.
pub const PREFIX: &str = "plugin.";
/// Longest time one plugin request may take.
pub(crate) const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
/// Largest response read from a plugin server.
pub(crate) const MAX_RESPONSE_BYTES: usize = 2 * 1024 * 1024;
/// Largest text returned to the model from one call.
pub(crate) const MAX_OUTPUT_CHARS: usize = 8_000;
/// Longest description kept from a plugin's tool list.
pub(crate) const MAX_DESCRIPTION_CHARS: usize = 400;
/// Largest input schema passed to the model. A bigger one is skipped, not truncated,
/// since a cut-off schema would describe arguments wrongly.
const MAX_SCHEMA_BYTES: usize = 8 * 1024;

/// Why a plugin could not be used this turn. Shown to the owner (and in a
/// one-line note to the main bot) in fixed words; nothing from the server.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Problem {
    NeedsSetup,
    NeedsSignIn,
    Unreachable,
}

impl Problem {
    pub fn phrase(self) -> &'static str {
        match self {
            Problem::NeedsSetup => "needs setup",
            Problem::NeedsSignIn => "needs sign-in",
            Problem::Unreachable => "is not responding",
        }
    }
}

/// A failed attempt to use one app: the class, and the error to return.
#[derive(Debug)]
struct Failure {
    problem: Problem,
    error: SeraError,
}

impl Failure {
    fn new(problem: Problem, error: SeraError) -> Self {
        Self { problem, error }
    }

    fn of_mcp(slug: &str, error: &McpError, signed_in: bool) -> Self {
        let problem = if error.is_unauthorized() {
            if signed_in {
                Problem::NeedsSignIn
            } else {
                Problem::NeedsSetup
            }
        } else {
            Problem::Unreachable
        };
        Self::new(problem, external(slug, error))
    }

    fn needs(slug: &str, problem: Problem) -> Self {
        Self::new(
            problem,
            SeraError::Validation(format!("plugin {slug} {}", problem.phrase())),
        )
    }
}

/// What a call presents to the server and where it came from.
struct Credential {
    auth: McpAuth,
    secret: SecretString,
    /// The marketplace account behind an OAuth token.
    account: Option<String>,
}

/// The caller's override for one tool.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolMode {
    Ask,
    Allow,
}

impl ToolMode {
    pub fn parse(word: &str) -> Option<Self> {
        match word {
            "ask" => Some(Self::Ask),
            "allow" => Some(Self::Allow),
            _ => None,
        }
    }
}

/// What a tool's MCP annotations say about it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Hints {
    pub read_only: bool,
    pub destructive: bool,
}

/// Read the hints from a tool entry. `destructiveHint: true` wins over a
/// read-only claim, so a contradictory server is treated as destructive.
pub fn hints_of(tool: &Value) -> Hints {
    let annotations = tool.get("annotations");
    let flag = |key: &str| {
        annotations
            .and_then(|found| found.get(key))
            .and_then(Value::as_bool)
    };
    let destructive = flag("destructiveHint") == Some(true);
    Hints {
        read_only: flag("readOnlyHint") == Some(true) && !destructive,
        destructive,
    }
}

/// One tool as the console lists it: the server's hints and the caller's
/// override (`"ask"`, `"allow"` or none).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct ToolRow {
    /// The app the tool belongs to; sent back when setting an override.
    pub app: String,
    pub name: String,
    pub description: String,
    pub read_only: bool,
    pub destructive: bool,
    pub mode: Option<String>,
}

/// Whether a call to a tool with these hints needs approval.
pub(crate) fn approval_needed(hints: Hints, mode: Option<ToolMode>) -> bool {
    match mode {
        Some(ToolMode::Ask) => true,
        Some(ToolMode::Allow) if !hints.destructive => false,
        _ => !hints.read_only,
    }
}

/// The plugins a user can call: installed and enabled, with an http server.
#[derive(Clone)]
pub struct PluginTools {
    store: Store,
    users: Arc<UserSecrets>,
    catalog: Arc<Vec<PluginManifest>>,
    cache: Arc<CatalogCache>,
    oauth: OauthAccounts,
    net: OauthNet,
    /// One token refresh at a time per user and account, so a rotating refresh
    /// token is not spent twice and one slow server stalls no other user.
    refresh_locks: crate::auth_flows::KeyedLocks,
    /// Per user, the plugins that could not be used on the last turn.
    problems: Arc<Mutex<HashMap<String, BTreeMap<String, Problem>>>>,
}

impl PluginTools {
    pub fn new(store: Store, users: Arc<UserSecrets>, catalog: Vec<PluginManifest>) -> Self {
        let net = OauthNet::system();
        Self {
            oauth: OauthAccounts::new(store.clone(), users.clone(), net.clone()),
            net,
            store,
            users,
            catalog: Arc::new(catalog),
            cache: Arc::new(CatalogCache::default()),
            refresh_locks: Default::default(),
            problems: Arc::default(),
        }
    }

    /// Tests only: use a net that accepts a plain-http loopback mock server.
    #[cfg(test)]
    pub(crate) fn with_net(mut self, net: OauthNet) -> Self {
        self.oauth = OauthAccounts::new(self.store.clone(), self.users.clone(), net.clone());
        self.net = net;
        self
    }

    /// The caller's enabled plugins with at least one usable http app. A plugin
    /// with only stdio apps is skipped; install already refuses those.
    fn enabled_for(&self, user_id: &str) -> Result<Vec<&PluginManifest>> {
        let mut plugins = Vec::new();
        for (slug, enabled, _version) in self.store.marketplace_installs(user_id)? {
            if !enabled {
                continue;
            }
            let Some(manifest) = self.catalog.iter().find(|plugin| plugin.slug == slug) else {
                continue;
            };
            if http_apps(manifest).next().is_some() {
                plugins.push(manifest);
            }
        }
        Ok(plugins)
    }

    /// Specs for every tool the caller's enabled plugins currently offer. A plugin
    /// that cannot be used contributes nothing this turn; the reason is kept (see
    /// [`PluginTools::notes_for`]) and audited when it changes, not raised.
    pub async fn specs_for(&self, user_id: &str) -> Vec<ToolSpec> {
        let Ok(plugins) = self.enabled_for(user_id) else {
            return Vec::new();
        };
        let mut specs = Vec::new();
        let mut found: BTreeMap<String, Problem> = BTreeMap::new();
        for manifest in plugins {
            for app in http_apps(manifest) {
                match self.list_app(user_id, manifest, app).await {
                    Ok(tools) => {
                        let space = namespace(manifest, app);
                        specs.extend(tools.iter().filter_map(|tool| spec_of(&space, tool)));
                    }
                    Err(failure) => {
                        tracing::warn!(
                            plugin = %manifest.slug,
                            app = %app.id,
                            problem = failure.problem.phrase(),
                            "plugin tools unavailable"
                        );
                        found
                            .entry(manifest.slug.clone())
                            .or_insert(failure.problem);
                    }
                }
            }
        }
        self.record_problems(user_id, found);
        specs
    }

    /// Replace the caller's remembered problems; audit each one that is new or changed.
    fn record_problems(&self, user_id: &str, found: BTreeMap<String, Problem>) {
        let previous = {
            let mut problems = self.problems.lock().unwrap_or_else(|e| e.into_inner());
            let previous = problems.get(user_id).cloned().unwrap_or_default();
            if found.is_empty() {
                problems.remove(user_id);
            } else {
                problems.insert(user_id.to_owned(), found.clone());
            }
            previous
        };
        for (slug, problem) in &found {
            if previous.get(slug) == Some(problem) {
                continue;
            }
            if let Err(error) = self.store.record_audit(
                user_id,
                "plugin.unavailable",
                None,
                None,
                None,
                None,
                None,
                Some(&json!({"plugin": slug, "problem": problem.phrase()})),
            ) {
                tracing::warn!(error = %error, "plugin audit write failed");
            }
        }
    }

    /// One short line per plugin that could not be used on the caller's last
    /// turn, such as "Canva needs sign-in". Names come from the catalog and the
    /// wording is fixed: nothing here is server text.
    pub fn notes_for(&self, user_id: &str) -> Vec<String> {
        let problems = self.problems.lock().unwrap_or_else(|e| e.into_inner());
        let Some(found) = problems.get(user_id) else {
            return Vec::new();
        };
        found
            .iter()
            .filter_map(|(slug, problem)| {
                let manifest = self.catalog.iter().find(|plugin| &plugin.slug == slug)?;
                Some(format!("{} {}", manifest.name, problem.phrase()))
            })
            .collect()
    }

    /// Drop the caller's cached tool lists for one plugin, for example after a
    /// sign-in or sign-out.
    pub fn forget_cache(&self, user_id: &str, slug: &str) {
        if let Some(manifest) = self.catalog.iter().find(|plugin| plugin.slug == slug) {
            for app in &manifest.apps {
                self.cache.forget(&cache_owner(user_id, slug, &app.id));
            }
        }
    }

    /// The tools one installed plugin offers the caller, with what the server's
    /// hints say and the caller's own override, for the console's per-tool
    /// switches. Plugin text is cut to the same length the model sees. Nothing
    /// here is a secret.
    pub async fn tool_rows(&self, user_id: &str, slug: &str) -> Result<Vec<ToolRow>> {
        let manifest = self
            .catalog
            .iter()
            .find(|plugin| plugin.slug == slug)
            .ok_or_else(|| SeraError::NotFound(format!("plugin {slug}")))?;
        if self.store.marketplace_install(user_id, slug)?.is_none() {
            return Err(SeraError::NotFound(format!(
                "plugin {slug} is not installed"
            )));
        }
        let mut rows = Vec::new();
        for app in http_apps(manifest) {
            let listed = self
                .list_app(user_id, manifest, app)
                .await
                .map_err(|failure| failure.error)?;
            for tool in &listed {
                let Some(name) = tool.get("name").and_then(Value::as_str) else {
                    continue;
                };
                let hints = hints_of(tool);
                let mode = self
                    .store
                    .marketplace_tool_mode(user_id, slug, &mode_key(manifest, app, name))?
                    .filter(|word| ToolMode::parse(word).is_some());
                rows.push(ToolRow {
                    app: app.id.clone(),
                    name: name.to_owned(),
                    description: truncate(
                        tool.get("description")
                            .and_then(Value::as_str)
                            .unwrap_or(""),
                        MAX_DESCRIPTION_CHARS,
                    ),
                    read_only: hints.read_only,
                    destructive: hints.destructive,
                    mode,
                });
            }
        }
        Ok(rows)
    }

    /// The tool behind one `plugin.<slug>.<tool>` name, when the caller may use it.
    pub async fn tool_for(&self, user_id: &str, name: &str) -> Option<PluginTool> {
        let rest = name.strip_prefix(PREFIX)?;
        let plugins = self.enabled_for(user_id).ok()?;
        for manifest in plugins {
            for app in http_apps(manifest) {
                let space = format!("{}.", namespace(manifest, app));
                let Some(tool_name) = rest.strip_prefix(&space) else {
                    continue;
                };
                let listed = self.list_app(user_id, manifest, app).await.ok()?;
                let description = listed
                    .into_iter()
                    .find(|tool| tool.get("name").and_then(Value::as_str) == Some(tool_name))?;
                let mode = self
                    .store
                    .marketplace_tool_mode(
                        user_id,
                        &manifest.slug,
                        &mode_key(manifest, app, tool_name),
                    )
                    .ok()
                    .flatten()
                    .and_then(|word| ToolMode::parse(&word));
                return Some(PluginTool {
                    tools: self.clone(),
                    user_id: user_id.to_owned(),
                    slug: manifest.slug.clone(),
                    app_id: app.id.clone(),
                    tool_name: tool_name.to_owned(),
                    description,
                    mode,
                });
            }
        }
        None
    }

    /// The tool list of one app's server for this caller, cached briefly. The
    /// manifest's tool filters are applied here, and a tool the manifest vouches
    /// for as read only carries `readOnlyHint` from then on.
    async fn list_app(
        &self,
        user_id: &str,
        manifest: &PluginManifest,
        app: &App,
    ) -> std::result::Result<Vec<Value>, Failure> {
        let owner = cache_owner(user_id, &manifest.slug, &app.id);
        if let Some(cached) = self.cache.get(&owner) {
            return Ok(cached.as_ref().clone());
        }
        let listed = self
            .run(user_id, manifest, app, |client, secret| async move {
                client
                    .list_tools(secret.expose_secret(), REQUEST_TIMEOUT)
                    .await
            })
            .await?;
        let total = listed.len();
        let filter = app.tools.clone().unwrap_or_default();
        let mut tools: Vec<Value> = listed
            .into_iter()
            .filter(schema_within_limit)
            .filter(|tool| {
                let Some(name) = tool.get("name").and_then(Value::as_str) else {
                    return false;
                };
                !filter.deny.iter().any(|denied| denied == name)
                    && (filter.allow.is_empty() || filter.allow.iter().any(|ok| ok == name))
            })
            .collect();
        for tool in &mut tools {
            let name = tool.get("name").and_then(Value::as_str).unwrap_or_default();
            if filter.read_only.iter().any(|vouched| vouched == name) {
                vouch_read_only(tool);
            }
        }
        if tools.len() < total {
            tracing::debug!(
                plugin = %manifest.slug,
                skipped = total - tools.len(),
                "plugin tools were skipped (oversized schema or manifest filter)"
            );
        }
        self.cache.put(&owner, Arc::new(tools.clone()));
        Ok(tools)
    }

    /// Run one MCP operation against an app with the caller's credential. On a
    /// `401` for an OAuth account the token is refreshed once and the operation
    /// is tried once more: the server rejected the request before acting, so
    /// the retry cannot repeat an effect.
    async fn run<T, F, Fut>(
        &self,
        user_id: &str,
        manifest: &PluginManifest,
        app: &App,
        op: F,
    ) -> std::result::Result<T, Failure>
    where
        F: Fn(McpClient, SecretString) -> Fut,
        Fut: Future<Output = std::result::Result<T, McpError>>,
    {
        let url = self.checked_url(manifest, app)?;
        let mut rejected: Option<String> = None;
        loop {
            let credential = self
                .credential(user_id, manifest, app, rejected.as_deref())
                .await?;
            // Resolve last: a plugin the caller has not set up never reaches DNS.
            let client = self.connect(url, manifest, credential.auth.clone()).await?;
            let signed_in = credential.account.is_some();
            let used = credential.secret.expose_secret().to_owned();
            match op(client, credential.secret).await {
                Ok(value) => return Ok(value),
                Err(error) if error.is_unauthorized() && signed_in && rejected.is_none() => {
                    rejected = Some(used);
                }
                Err(error) => return Err(Failure::of_mcp(&manifest.slug, &error, signed_in)),
            }
        }
    }

    /// An MCP client for `url`, pinned to the addresses that passed the public-IP
    /// check, so the credential is sent only to them whatever DNS says later.
    async fn connect(
        &self,
        url: &str,
        manifest: &PluginManifest,
        auth: McpAuth,
    ) -> std::result::Result<McpClient, Failure> {
        let unreachable =
            |error: &McpError| Failure::new(Problem::Unreachable, external(&manifest.slug, error));
        if self.net.is_loopback_test_url(url) {
            return McpClient::new(
                "plugin",
                url.to_owned(),
                auth,
                REQUEST_TIMEOUT,
                MAX_RESPONSE_BYTES,
            )
            .map_err(|error| unreachable(&error));
        }
        let pinned = resolve_checked(url, system_lookup)
            .await
            .map_err(|error| Failure::new(Problem::Unreachable, error))?;
        McpClient::new_pinned(
            "plugin",
            url.to_owned(),
            auth,
            REQUEST_TIMEOUT,
            MAX_RESPONSE_BYTES,
            &pinned.host,
            &pinned.addrs,
        )
        .map_err(|error| unreachable(&error))
    }

    /// The credential for one app, read from the caller's own store at call
    /// time. `rejected` is an access token the server just refused: it forces a
    /// refresh unless another call already replaced it.
    async fn credential(
        &self,
        user_id: &str,
        manifest: &PluginManifest,
        app: &App,
        rejected: Option<&str>,
    ) -> std::result::Result<Credential, Failure> {
        let slug = &manifest.slug;
        let Some(entry) = app.auth.first() else {
            return Ok(Credential::none());
        };
        match entry.kind {
            AuthKind::None => Ok(Credential::none()),
            AuthKind::OauthLink => Err(Failure::needs(slug, Problem::NeedsSetup)),
            AuthKind::ApiKey => {
                let field = entry
                    .fields
                    .first()
                    .ok_or_else(|| Failure::needs(slug, Problem::NeedsSetup))?;
                let key_name = format!("marketplace.{slug}.{}", field.secret_key);
                let secret = self
                    .users
                    .get(user_id, &key_name)
                    .map_err(|error| Failure::new(Problem::NeedsSetup, error))?
                    .ok_or_else(|| {
                        Failure::new(
                            Problem::NeedsSetup,
                            SeraError::Validation(format!("plugin {slug} is not set up yet")),
                        )
                    })?;
                let auth = match &field.header {
                    Some(header) => McpAuth::Named {
                        name: header.name.clone(),
                        bearer: header.scheme == HeaderScheme::Bearer,
                    },
                    None => McpAuth::Bearer,
                };
                Ok(Credential {
                    auth,
                    secret,
                    account: None,
                })
            }
            AuthKind::Oauth => self.oauth_credential(user_id, slug, rejected).await,
        }
    }

    /// The access token of the caller's first usable account, refreshed when it
    /// is stale. The outcome table of [`OauthAccounts::refresh_account`] decides
    /// the account's state.
    async fn oauth_credential(
        &self,
        user_id: &str,
        slug: &str,
        rejected: Option<&str>,
    ) -> std::result::Result<Credential, Failure> {
        let sign_in = || Failure::needs(slug, Problem::NeedsSignIn);
        let stored = |error: SeraError| Failure::new(Problem::Unreachable, error);
        let account = self
            .store
            .marketplace_accounts(user_id, slug)
            .map_err(stored)?
            .into_iter()
            .find(|account| account.state != "needs_sign_in")
            .ok_or_else(sign_in)?
            .account_id;
        let now = chrono::Utc::now().timestamp();
        let mut record = self
            .oauth
            .load(user_id, slug, &account)
            .map_err(stored)?
            .ok_or_else(sign_in)?;
        let stale = |record: &crate::mcp_oauth::OauthRecord| {
            record.needs_refresh(now) || rejected == Some(record.access_token.as_str())
        };
        if stale(&record) {
            let lock = self
                .refresh_locks
                .get(&format!("{user_id}\n{slug}\n{account}"));
            let _guard = lock.lock().await;
            // Another call may have refreshed while this one waited.
            record = self
                .oauth
                .load(user_id, slug, &account)
                .map_err(stored)?
                .ok_or_else(sign_in)?;
            if stale(&record) {
                match self
                    .oauth
                    .refresh_account(user_id, slug, &account)
                    .await
                    .map_err(stored)?
                {
                    RefreshOutcome::Ready => {}
                    RefreshOutcome::NeedsSignIn | RefreshOutcome::ClientDropped => {
                        return Err(sign_in());
                    }
                    RefreshOutcome::Unreachable => {
                        return Err(Failure::needs(slug, Problem::Unreachable));
                    }
                }
                record = self
                    .oauth
                    .load(user_id, slug, &account)
                    .map_err(stored)?
                    .ok_or_else(sign_in)?;
            }
        }
        if record.access_token.is_empty() {
            return Err(sign_in());
        }
        Ok(Credential {
            auth: McpAuth::Bearer,
            secret: SecretString::from(record.access_token.clone()),
            account: Some(account),
        })
    }

    /// The app's URL after the egress check. Nothing is resolved or sent yet.
    fn checked_url<'a>(
        &self,
        manifest: &PluginManifest,
        app: &'a App,
    ) -> std::result::Result<&'a str, Failure> {
        let Server::Http { url } = &app.server else {
            return Err(Failure::new(
                Problem::NeedsSetup,
                SeraError::Validation("plugin app has no http server".into()),
            ));
        };
        self.net
            .check_mcp_egress(url, &manifest.permissions.egress)
            .map_err(|error| Failure::new(Problem::NeedsSetup, error))?;
        Ok(url)
    }

    fn record(
        &self,
        user_id: &str,
        event: &str,
        tool: &str,
        task_id: Option<&str>,
        ok: bool,
        read_only: bool,
    ) {
        // An audit write failing must not hide the tool result, so it is logged only.
        if let Err(error) = self.store.record_audit(
            user_id,
            event,
            Some(tool),
            task_id,
            None,
            None,
            None,
            Some(&json!({"ok": ok, "read_only": read_only})),
        ) {
            tracing::warn!(error = %error, "plugin audit write failed");
        }
    }
}

impl Credential {
    fn none() -> Self {
        Self {
            auth: McpAuth::None,
            secret: SecretString::from(String::new()),
            account: None,
        }
    }
}

fn cache_owner(user_id: &str, slug: &str, app_id: &str) -> String {
    format!("{user_id}/{slug}/{app_id}")
}

/// The plugin's http apps, in manifest order.
fn http_apps(manifest: &PluginManifest) -> impl Iterator<Item = &App> {
    manifest
        .apps
        .iter()
        .filter(|app| matches!(app.server, Server::Http { .. }))
}

/// The key an approval override is stored under. A plugin with one http app
/// keys by the tool name alone; with several, by `app/tool`, so a switch set
/// for one app's tool never reaches a same-named tool on another app.
fn mode_key(manifest: &PluginManifest, app: &App, tool: &str) -> String {
    if http_apps(manifest).count() > 1 {
        format!("{}/{tool}", app.id)
    } else {
        tool.to_owned()
    }
}

/// The override key for a console request. `app` is required when the plugin
/// has several http apps and must name one of them.
pub fn tool_mode_key(manifest: &PluginManifest, app: Option<&str>, tool: &str) -> Result<String> {
    let apps: Vec<&App> = http_apps(manifest).collect();
    let chosen = match (apps.as_slice(), app) {
        ([only], None) => *only,
        (apps, Some(id)) => *apps
            .iter()
            .find(|candidate| candidate.id == id)
            .ok_or_else(|| SeraError::Validation("app is not part of this plugin".into()))?,
        _ => {
            return Err(SeraError::Validation(
                "this plugin has several apps: name one".into(),
            ));
        }
    };
    Ok(mode_key(manifest, chosen, tool))
}

/// The name space of an app's tools: the slug, plus the app id when the plugin
/// has more than one http app.
fn namespace(manifest: &PluginManifest, app: &App) -> String {
    if http_apps(manifest).count() > 1 {
        format!("{}.{}", manifest.slug, app.id)
    } else {
        manifest.slug.clone()
    }
}

/// Mark a tool entry read only (the manifest vouches for it).
fn vouch_read_only(tool: &mut Value) {
    if let Some(object) = tool.as_object_mut() {
        let annotations = object.entry("annotations").or_insert_with(|| json!({}));
        if let Some(annotations) = annotations.as_object_mut() {
            annotations.insert("readOnlyHint".into(), json!(true));
        }
    }
}

/// Whether a tool's input schema is small enough to offer the model. A tool
/// without a schema is fine: the default one is used.
pub(crate) fn schema_within_limit(tool: &Value) -> bool {
    match tool.get("inputSchema") {
        None => true,
        Some(schema) => {
            serde_json::to_vec(schema).is_ok_and(|bytes| bytes.len() <= MAX_SCHEMA_BYTES)
        }
    }
}

/// The spec for one MCP tool entry, namespaced to its plugin (and app). A read
/// only tool is not an external effect.
fn spec_of(space: &str, tool: &Value) -> Option<ToolSpec> {
    if !schema_within_limit(tool) {
        return None;
    }
    let name = tool.get("name")?.as_str()?;
    let description = tool
        .get("description")
        .and_then(Value::as_str)
        .unwrap_or("A tool from an installed plugin.");
    Some(ToolSpec {
        name: format!("{PREFIX}{space}.{name}"),
        description: truncate(description, MAX_DESCRIPTION_CHARS),
        parameters: tool
            .get("inputSchema")
            .cloned()
            .unwrap_or_else(|| json!({"type": "object"})),
        external_effect: !hints_of(tool).read_only,
    })
}

/// Map an MCP failure to an error that names the plugin and carries no secret.
fn external(slug: &str, error: &McpError) -> SeraError {
    match error {
        McpError::Http { status, .. } => {
            SeraError::External(format!("plugin {slug} answered with status {status}"))
        }
        McpError::Rpc { message, .. } => SeraError::External(format!(
            "plugin {slug} refused the request: {}",
            truncate(message, 200)
        )),
        McpError::Transport(_) => {
            SeraError::External(format!("plugin {slug} could not be reached"))
        }
        McpError::Other(text) => {
            SeraError::External(format!("plugin {slug}: {}", truncate(text, 200)))
        }
    }
}

/// The egress rule for a plugin request: https only, no credentials, and a host
/// on the manifest's list.
pub fn check_egress(url: &str, allowed_hosts: &[String]) -> Result<()> {
    let parsed =
        Url::parse(url).map_err(|_| SeraError::Validation("plugin URL is invalid".into()))?;
    if parsed.scheme() != "https" {
        return Err(SeraError::Forbidden("plugins may only use https".into()));
    }
    if !parsed.username().is_empty() || parsed.password().is_some() {
        return Err(SeraError::Forbidden(
            "plugin URLs must not carry credentials".into(),
        ));
    }
    let host = parsed
        .host_str()
        .ok_or_else(|| SeraError::Validation("plugin URL has no host".into()))?
        .to_ascii_lowercase();
    if !allowed_hosts
        .iter()
        .any(|allowed| allowed.eq_ignore_ascii_case(&host))
    {
        return Err(SeraError::Forbidden(format!(
            "host {host} is not on this plugin's egress list"
        )));
    }
    Ok(())
}

/// The addresses one plugin connection is pinned to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PinnedAddresses {
    /// The bare host name the connection is pinned for.
    pub host: String,
    /// Every address the host resolved to. All of them passed the check.
    pub addrs: Vec<SocketAddr>,
}

/// The system resolver, as the lookup [`resolve_checked`] uses in production.
pub(crate) async fn system_lookup(host: String, port: u16) -> std::io::Result<Vec<SocketAddr>> {
    Ok(tokio::net::lookup_host((host.as_str(), port))
        .await?
        .collect())
}

/// Resolve the plugin's host once and refuse unless every address is public,
/// the same rule the watcher fetch uses. The returned addresses are the only
/// ones the connection may use, so a DNS answer that changes later cannot
/// send the request, or the plugin key, to an unchecked address.
pub async fn resolve_checked<F, Fut>(url: &str, lookup: F) -> Result<PinnedAddresses>
where
    F: FnOnce(String, u16) -> Fut,
    Fut: Future<Output = std::io::Result<Vec<SocketAddr>>>,
{
    let parsed =
        Url::parse(url).map_err(|_| SeraError::Validation("plugin URL is invalid".into()))?;
    let host = parsed
        .host_str()
        .ok_or_else(|| SeraError::Validation("plugin URL has no host".into()))?
        .to_owned();
    // An IPv6 literal keeps its brackets in the URL; the resolver wants bare text.
    let bare = host
        .trim_start_matches('[')
        .trim_end_matches(']')
        .to_owned();
    let port = parsed.port_or_known_default().unwrap_or(443);
    let addrs = lookup(bare.clone(), port)
        .await
        .map_err(|_| SeraError::External(format!("could not resolve {host}")))?;
    if addrs.is_empty() {
        return Err(SeraError::External(format!("no address for {host}")));
    }
    if addrs
        .iter()
        .any(|address| crate::fetch::is_forbidden_ip(address.ip()))
    {
        return Err(SeraError::Forbidden(format!(
            "{host} resolves to a non-public address"
        )));
    }
    Ok(PinnedAddresses { host: bare, addrs })
}

pub(crate) fn truncate(text: &str, max: usize) -> String {
    text.chars().take(max).collect()
}

/// One plugin tool bound to one caller. A call needs owner approval unless the
/// tool is read only (or the owner allowed it); it never gets a standing grant
/// and is never auto-approved, so the owner decides each use of a tool that acts.
pub struct PluginTool {
    tools: PluginTools,
    user_id: String,
    slug: String,
    app_id: String,
    tool_name: String,
    description: Value,
    mode: Option<ToolMode>,
}

impl PluginTool {
    fn space(&self) -> String {
        // The name space the model saw: recomputed from the catalog so it
        // matches `specs_for` exactly.
        self.tools
            .catalog
            .iter()
            .find(|plugin| plugin.slug == self.slug)
            .and_then(|manifest| {
                manifest
                    .apps
                    .iter()
                    .find(|app| app.id == self.app_id)
                    .map(|app| namespace(manifest, app))
            })
            .unwrap_or_else(|| self.slug.clone())
    }
}

#[async_trait]
impl Tool for PluginTool {
    fn spec(&self) -> ToolSpec {
        spec_of(&self.space(), &self.description).unwrap_or_else(|| ToolSpec {
            name: format!("{PREFIX}{}.{}", self.space(), self.tool_name),
            description: "A tool from an installed plugin.".into(),
            parameters: json!({"type": "object"}),
            external_effect: true,
        })
    }

    fn approval_details(&self, _args: &Value) -> Result<ApprovalDetails> {
        // The summary names the plugin and tool only: never the arguments.
        Ok(ApprovalDetails {
            summary: format!("Run {} from the {} plugin", self.tool_name, self.slug),
            destination: None,
            artifact_hash: None,
        })
    }

    fn requires_approval(&self, _args: &Value) -> bool {
        approval_needed(hints_of(&self.description), self.mode)
    }

    async fn auto_approve_allowed(&self, _args: &Value, _context: &ToolContext) -> bool {
        false
    }

    fn allow_standing_grant(&self, _args: &Value) -> bool {
        false
    }

    async fn execute(&self, args: Value, context: &ToolContext) -> Result<ToolOutput> {
        if context.user_id != self.user_id {
            return Err(SeraError::Forbidden(
                "plugin tool belongs to another user".into(),
            ));
        }
        let manifest = self
            .tools
            .enabled_for(&self.user_id)?
            .into_iter()
            .find(|plugin| plugin.slug == self.slug)
            .cloned()
            .ok_or_else(|| SeraError::NotFound(format!("plugin {} is not enabled", self.slug)))?;
        let outcome = self.call(&manifest, &args).await;
        let tool = format!("{PREFIX}{}.{}", self.space(), self.tool_name);
        self.tools.record(
            &self.user_id,
            "plugin.call",
            &tool,
            context.task_id.as_deref(),
            outcome.is_ok(),
            hints_of(&self.description).read_only,
        );
        outcome
    }
}

impl PluginTool {
    async fn call(&self, manifest: &PluginManifest, args: &Value) -> Result<ToolOutput> {
        let app = manifest
            .apps
            .iter()
            .find(|app| app.id == self.app_id)
            .ok_or_else(|| SeraError::NotFound(format!("plugin {} app", self.slug)))?;
        let result = self
            .tools
            .run(&self.user_id, manifest, app, |client, secret| {
                let name = self.tool_name.clone();
                let args = args.clone();
                async move {
                    client
                        .call_tool(secret.expose_secret(), &name, &args, REQUEST_TIMEOUT)
                        .await
                }
            })
            .await
            .map_err(|failure| failure.error)?;
        Ok(ToolOutput {
            content: truncate(&result_text(&result), MAX_OUTPUT_CHARS),
            ..Default::default()
        })
    }
}

/// The text blocks of an MCP tool result, joined in order.
pub(crate) fn result_text(result: &Value) -> String {
    result
        .get("content")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter(|item| item.get("type").and_then(Value::as_str) == Some("text"))
                .filter_map(|item| item.get("text").and_then(Value::as_str))
                .collect::<Vec<_>>()
                .join(
                    "
",
                )
        })
        .unwrap_or_default()
}

#[cfg(test)]
pub(crate) mod oauth_tests;

#[cfg(test)]
mod tests {
    use super::*;

    /// The shipped catalog, read from the repository root. No network: every
    /// test below stops before a request leaves the process.
    fn catalog() -> Vec<PluginManifest> {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../plugins");
        let (plugins, problems) = crate::marketplace::load_catalog(&root);
        assert!(problems.is_empty(), "catalog problems: {problems:?}");
        plugins
    }

    fn http_url(manifest: &PluginManifest) -> Option<&str> {
        http_apps(manifest).find_map(|app| match &app.server {
            Server::Http { url } => Some(url.as_str()),
            Server::Stdio { .. } => None,
        })
    }

    fn firecrawl() -> PluginManifest {
        catalog()
            .into_iter()
            .find(|plugin| plugin.slug == "firecrawl")
            .expect("firecrawl manifest")
    }

    fn fixture() -> (PluginTools, Store, String) {
        let store = Store::open_in_memory(384).unwrap();
        let users = Arc::new(UserSecrets::with_master_key(store.clone(), [7u8; 32]).unwrap());
        let user = store
            .create_user("microsoft:plugin-test", "plugin@example.example", "P")
            .unwrap()
            .id;
        let tools = PluginTools::new(store.clone(), users, catalog());
        (tools, store, user)
    }

    #[test]
    fn egress_allows_only_listed_https_hosts() {
        let manifest = firecrawl();
        let url = http_url(&manifest).unwrap();
        assert!(check_egress(url, &manifest.permissions.egress).is_ok());
        let upper = url.replace(
            url.split('/').nth(2).unwrap(),
            &url.split('/').nth(2).unwrap().to_ascii_uppercase(),
        );
        assert!(check_egress(&upper, &manifest.permissions.egress).is_ok());
    }

    #[test]
    fn egress_refuses_other_schemes_credentials_and_hosts() {
        let hosts = vec!["mcp.example.invalid".to_owned()];
        assert!(check_egress("http://mcp.example.invalid/mcp", &hosts).is_err());
        assert!(check_egress("https://user:pw@mcp.example.invalid/mcp", &hosts).is_err());
        assert!(check_egress("https://evil.example.invalid/mcp", &hosts).is_err());
        assert!(check_egress("not a url", &hosts).is_err());
    }

    #[tokio::test]
    async fn resolved_addresses_refuse_non_public_targets() {
        for url in [
            "https://127.0.0.1/mcp",
            "https://10.0.0.5/mcp",
            "https://192.168.1.10/mcp",
            "https://169.254.169.254/latest",
            "https://100.64.0.1/mcp",
            "https://[::1]/mcp",
            "https://[fe80::1]/mcp",
        ] {
            let error = resolve_checked(url, system_lookup).await.err();
            assert!(
                matches!(error, Some(SeraError::Forbidden(_))),
                "{url} should be refused as non-public, got {error:?}"
            );
        }
    }

    #[test]
    fn tool_names_are_namespaced_per_plugin() {
        let tool = serde_json::json!({
            "name": "scrape",
            "description": "Fetch a page",
            "inputSchema": {"type": "object"}
        });
        let spec = spec_of("firecrawl", &tool).unwrap();
        assert_eq!(spec.name, "plugin.firecrawl.scrape");
        assert!(spec.external_effect);
        assert!(spec_of("firecrawl", &serde_json::json!({"description": "no name"})).is_none());
    }

    #[test]
    fn result_text_keeps_only_text_blocks() {
        let result = serde_json::json!({
            "content": [
                {"type": "text", "text": "first"},
                {"type": "image", "data": "AAAA"},
                {"type": "text", "text": "second"}
            ]
        });
        assert_eq!(result_text(&result), "first\nsecond");
        assert_eq!(result_text(&serde_json::json!({})), "");
    }

    #[tokio::test]
    async fn plugin_tools_need_an_installed_and_enabled_plugin() {
        let (tools, store, user) = fixture();
        assert!(tools.enabled_for(&user).unwrap().is_empty());
        assert!(
            tools
                .tool_for(&user, "plugin.firecrawl.scrape")
                .await
                .is_none()
        );

        // A new install starts disabled; only an enabled install counts.
        store
            .marketplace_install_insert(&user, "firecrawl", "0.1.0")
            .unwrap();
        assert!(tools.enabled_for(&user).unwrap().is_empty());
        store
            .marketplace_install_set_enabled(&user, "firecrawl", true)
            .unwrap();
        assert_eq!(tools.enabled_for(&user).unwrap().len(), 1);

        store
            .marketplace_install_set_enabled(&user, "firecrawl", false)
            .unwrap();
        assert!(tools.enabled_for(&user).unwrap().is_empty());
        assert!(
            tools
                .tool_for(&user, "plugin.firecrawl.scrape")
                .await
                .is_none()
        );
    }

    #[tokio::test]
    async fn an_unset_up_plugin_is_not_reached() {
        let (tools, store, user) = fixture();
        store
            .marketplace_install_insert(&user, "firecrawl", "0.1.0")
            .unwrap();
        // No key is stored, so the call fails before any request or DNS lookup.
        let manifest = firecrawl();
        let error = tools
            .credential(&user, &manifest, &manifest.apps[0], None)
            .await
            .err()
            .unwrap()
            .error;
        assert!(error.to_string().contains("not set up"), "{error}");
        assert!(tools.specs_for(&user).await.is_empty());
    }

    #[test]
    fn non_plugin_names_never_resolve() {
        let (tools, store, user) = fixture();
        store
            .marketplace_install_insert(&user, "firecrawl", "0.1.0")
            .unwrap();
        let rt = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        assert!(rt.block_on(tools.tool_for(&user, "scrape")).is_none());
        assert!(
            rt.block_on(tools.tool_for(&user, "plugin.firecrawl"))
                .is_none()
        );
        assert!(
            rt.block_on(tools.tool_for(&user, "plugin.unknown.scrape"))
                .is_none()
        );
    }

    /// A host whose answer changes: public on the first lookup, private after.
    /// The check must ask once, and the connection must use only what it checked.
    #[tokio::test]
    async fn the_checked_answer_is_the_only_one_used() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicUsize, Ordering};
        let calls = Arc::new(AtomicUsize::new(0));
        let counter = calls.clone();
        let lookup = move |_host: String, port: u16| {
            let call = counter.fetch_add(1, Ordering::SeqCst);
            async move {
                let ip = if call == 0 {
                    "93.184.216.34"
                } else {
                    "10.0.0.5"
                };
                Ok::<Vec<SocketAddr>, std::io::Error>(vec![SocketAddr::new(
                    ip.parse().unwrap(),
                    port,
                )])
            }
        };
        let pinned = resolve_checked("https://mcp.example.invalid/mcp", lookup)
            .await
            .expect("public answer passes");
        assert_eq!(calls.load(Ordering::SeqCst), 1, "DNS asked exactly once");
        assert_eq!(pinned.host, "mcp.example.invalid");
        assert_eq!(
            pinned.addrs,
            vec![SocketAddr::new("93.184.216.34".parse().unwrap(), 443)]
        );
    }

    /// One private address in the answer refuses the whole plugin.
    #[tokio::test]
    async fn a_private_address_among_public_ones_is_refused() {
        let lookup = |_host: String, port: u16| async move {
            Ok::<Vec<SocketAddr>, std::io::Error>(vec![
                SocketAddr::new("93.184.216.34".parse().unwrap(), port),
                SocketAddr::new("169.254.169.254".parse().unwrap(), port),
            ])
        };
        let error = resolve_checked("https://mcp.example.invalid/mcp", lookup)
            .await
            .err();
        assert!(matches!(error, Some(SeraError::Forbidden(_))), "{error:?}");
    }

    /// The connection goes only to the pinned address. The host name does not
    /// resolve in DNS (".invalid"), so a request that succeeds proves the pin.
    #[tokio::test]
    async fn a_pinned_connection_reaches_only_the_checked_address() {
        use serde_json::json;
        use wiremock::matchers::method;
        use wiremock::{Mock, MockServer, Request, ResponseTemplate};

        struct Rpc;
        impl wiremock::Respond for Rpc {
            fn respond(&self, request: &Request) -> ResponseTemplate {
                let body: Value = serde_json::from_slice(&request.body).unwrap_or(Value::Null);
                let id = body.get("id").cloned().unwrap_or(Value::Null);
                match body.get("method").and_then(Value::as_str) {
                    Some("initialize") => ResponseTemplate::new(200)
                        .insert_header("content-type", "application/json")
                        .insert_header("mcp-session-id", "synthetic-session")
                        .set_body_json(json!({"jsonrpc": "2.0", "id": id, "result": {
                            "protocolVersion": "2025-06-18"
                        }})),
                    Some("tools/list") => ResponseTemplate::new(200)
                        .insert_header("content-type", "application/json")
                        .set_body_json(json!({"jsonrpc": "2.0", "id": id, "result": {
                            "tools": [{"name": "scrape", "inputSchema": {"type": "object"}}]
                        }})),
                    _ => ResponseTemplate::new(202),
                }
            }
        }

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(Rpc)
            .mount(&server)
            .await;
        let addr = *server.address();
        let url = format!("http://mcp.example.invalid:{}/mcp", addr.port());
        let client = McpClient::new_pinned(
            "plugin",
            url,
            McpAuth::Bearer,
            Duration::from_secs(5),
            1_000_000,
            "mcp.example.invalid",
            &[addr],
        )
        .expect("client builds");
        let tools = client
            .list_tools("synthetic-test-key", Duration::from_secs(5))
            .await
            .expect("pinned address answers");
        assert_eq!(tools[0]["name"], "scrape");
    }

    #[test]
    fn an_oversized_input_schema_is_not_offered_to_the_model() {
        let small = json!({"name": "scrape", "inputSchema": {"type": "object"}});
        assert!(spec_of("firecrawl", &small).is_some());

        let padding = "x".repeat(MAX_SCHEMA_BYTES + 1);
        let big = json!({
            "name": "huge",
            "description": "A tool with a huge schema.",
            "inputSchema": {"type": "object", "description": padding}
        });
        assert!(!schema_within_limit(&big));
        assert!(spec_of("firecrawl", &big).is_none());

        // No schema at all is fine: the default one is used.
        let bare = json!({"name": "bare"});
        assert!(spec_of("firecrawl", &bare).is_some());
    }
}
