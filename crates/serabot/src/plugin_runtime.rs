//! Plugin tools for a user's bots (v0.37).
//!
//! An installed and enabled `http` plugin exposes the tools its MCP server
//! offers. Each tool is named `plugin.<slug>.<tool>`, so a name never collides
//! with a built-in tool or with another plugin. Everything here is per user:
//! the install, the sealed key and the audit row all belong to the caller.
//! Nothing is added to the global tool registry; the set is read per turn, so a
//! plugin disabled or uninstalled drops out on the next turn.
//!
//! Each request is checked first: https only, no credentials, the host on the
//! manifest's egress list, and every resolved address public. Calls are audited
//! by plugin and tool name only; arguments and results are never written.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use reqwest::Url;
use secrecy::{ExposeSecret, SecretString};
use serde_json::{Value, json};

use crate::error::{Result, SeraError};
use crate::gateway::{ApprovalDetails, Tool, ToolContext, ToolOutput, ToolSpec};
use crate::marketplace::{PluginManifest, Server};
use crate::mcp_client::{CatalogCache, McpAuth, McpClient, McpError};
use crate::store::Store;
use crate::user_secrets::UserSecrets;

/// Prefix shared by every plugin tool name.
pub const PREFIX: &str = "plugin.";
/// Longest time one plugin request may take.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
/// Largest response read from a plugin server.
const MAX_RESPONSE_BYTES: usize = 2 * 1024 * 1024;
/// Largest text returned to the model from one call.
const MAX_OUTPUT_CHARS: usize = 8_000;
/// Longest description kept from a plugin's tool list.
const MAX_DESCRIPTION_CHARS: usize = 400;

/// The plugins a user can call: installed and enabled, with an http server.
#[derive(Clone)]
pub struct PluginTools {
    store: Store,
    users: Arc<UserSecrets>,
    catalog: Arc<Vec<PluginManifest>>,
    cache: Arc<CatalogCache>,
}

impl PluginTools {
    pub fn new(store: Store, users: Arc<UserSecrets>, catalog: Vec<PluginManifest>) -> Self {
        Self {
            store,
            users,
            catalog: Arc::new(catalog),
            cache: Arc::new(CatalogCache::default()),
        }
    }

    /// The caller's enabled plugins with a usable http server. A plugin with a
    /// stdio server is skipped; install already refuses those.
    fn enabled_for(&self, user_id: &str) -> Result<Vec<&PluginManifest>> {
        let mut plugins = Vec::new();
        for (slug, enabled, _version) in self.store.marketplace_installs(user_id)? {
            if !enabled {
                continue;
            }
            let Some(manifest) = self.catalog.iter().find(|plugin| plugin.slug == slug) else {
                continue;
            };
            if http_url(manifest).is_some() {
                plugins.push(manifest);
            }
        }
        Ok(plugins)
    }

    /// Specs for every tool the caller's enabled plugins currently offer. A plugin
    /// that cannot be reached contributes nothing this turn, and no error is raised.
    pub async fn specs_for(&self, user_id: &str) -> Vec<ToolSpec> {
        let Ok(plugins) = self.enabled_for(user_id) else {
            return Vec::new();
        };
        let mut specs = Vec::new();
        for manifest in plugins {
            let Ok(tools) = self.list(user_id, manifest).await else {
                continue;
            };
            for tool in &tools {
                if let Some(spec) = spec_of(&manifest.slug, tool) {
                    specs.push(spec);
                }
            }
        }
        specs
    }

    /// The tool behind one `plugin.<slug>.<tool>` name, when the caller may use it.
    pub async fn tool_for(&self, user_id: &str, name: &str) -> Option<PluginTool> {
        let (slug, tool_name) = name.strip_prefix(PREFIX)?.split_once('.')?;
        let plugins = self.enabled_for(user_id).ok()?;
        let manifest = plugins.into_iter().find(|plugin| plugin.slug == slug)?;
        let listed = self.list(user_id, manifest).await.ok()?;
        let description = listed
            .into_iter()
            .find(|tool| tool.get("name").and_then(Value::as_str) == Some(tool_name))?;
        Some(PluginTool {
            tools: self.clone(),
            user_id: user_id.to_owned(),
            slug: slug.to_owned(),
            tool_name: tool_name.to_owned(),
            description,
        })
    }

    /// The tool list of one plugin's server for this caller, cached briefly.
    async fn list(&self, user_id: &str, manifest: &PluginManifest) -> Result<Vec<Value>> {
        let owner = format!("{user_id}/{}", manifest.slug);
        if let Some(cached) = self.cache.get(&owner) {
            return Ok(cached.as_ref().clone());
        }
        let (client, secret) = self.client_for(user_id, manifest).await?;
        let tools = client
            .list_tools(secret.expose_secret(), REQUEST_TIMEOUT)
            .await
            .map_err(|error| external(&manifest.slug, &error))?;
        self.cache.put(&owner, Arc::new(tools.clone()));
        Ok(tools)
    }

    /// The plugin's MCP client and the caller's sealed key, after the egress
    /// checks pass. The key is read from the caller's own store and is never
    /// logged or returned to the model.
    async fn client_for(
        &self,
        user_id: &str,
        manifest: &PluginManifest,
    ) -> Result<(McpClient, SecretString)> {
        let url = http_url(manifest)
            .ok_or_else(|| SeraError::Validation("plugin has no http server".into()))?;
        check_egress(url, &manifest.permissions.egress)?;
        let field = manifest
            .apps
            .iter()
            .flat_map(|app| app.auth.iter())
            .flat_map(|auth| auth.fields.iter())
            .next()
            .ok_or_else(|| SeraError::Validation("plugin declares no key field".into()))?;
        let key_name = format!("marketplace.{}.{}", manifest.slug, field.secret_key);
        let secret = self.users.get(user_id, &key_name)?.ok_or_else(|| {
            SeraError::Validation(format!("plugin {} is not set up yet", manifest.slug))
        })?;
        // Resolve last: a plugin the caller has not set up never reaches DNS.
        check_resolved_addresses(url).await?;
        let client = McpClient::new(
            "plugin",
            url.to_owned(),
            McpAuth::Bearer,
            REQUEST_TIMEOUT,
            MAX_RESPONSE_BYTES,
        )
        .map_err(|error| external(&manifest.slug, &error))?;
        Ok((client, secret))
    }

    fn record(&self, user_id: &str, event: &str, tool: &str, task_id: Option<&str>, ok: bool) {
        // An audit write failing must not hide the tool result, so it is logged only.
        if let Err(error) = self.store.record_audit(
            user_id,
            event,
            Some(tool),
            task_id,
            None,
            None,
            None,
            Some(&json!({"ok": ok})),
        ) {
            tracing::warn!(error = %error, "plugin audit write failed");
        }
    }
}

/// The http URL of a plugin's first http app, if it has one.
fn http_url(manifest: &PluginManifest) -> Option<&str> {
    manifest.apps.iter().find_map(|app| match &app.server {
        Server::Http { url } => Some(url.as_str()),
        Server::Stdio { .. } => None,
    })
}

/// The spec for one MCP tool entry, namespaced to its plugin.
fn spec_of(slug: &str, tool: &Value) -> Option<ToolSpec> {
    let name = tool.get("name")?.as_str()?;
    let description = tool
        .get("description")
        .and_then(Value::as_str)
        .unwrap_or("A tool from an installed plugin.");
    Some(ToolSpec {
        name: format!("{PREFIX}{slug}.{name}"),
        description: truncate(description, MAX_DESCRIPTION_CHARS),
        parameters: tool
            .get("inputSchema")
            .cloned()
            .unwrap_or_else(|| json!({"type": "object"})),
        external_effect: true,
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

/// Resolve the host and refuse any non-public address, the same rule the
/// watcher fetch uses. A DNS answer that changes after this check is not
/// covered; the connection itself is not pinned to the checked address.
pub async fn check_resolved_addresses(url: &str) -> Result<()> {
    let parsed =
        Url::parse(url).map_err(|_| SeraError::Validation("plugin URL is invalid".into()))?;
    let host = parsed
        .host_str()
        .ok_or_else(|| SeraError::Validation("plugin URL has no host".into()))?
        .to_owned();
    // An IPv6 literal keeps its brackets in the URL; the resolver wants bare text.
    let bare = host.trim_start_matches('[').trim_end_matches(']');
    let port = parsed.port_or_known_default().unwrap_or(443);
    let addresses: Vec<std::net::SocketAddr> = tokio::net::lookup_host((bare, port))
        .await
        .map_err(|_| SeraError::External(format!("could not resolve {host}")))?
        .collect();
    if addresses.is_empty() {
        return Err(SeraError::External(format!("no address for {host}")));
    }
    if addresses
        .iter()
        .any(|address| crate::fetch::is_forbidden_ip(address.ip()))
    {
        return Err(SeraError::Forbidden(format!(
            "{host} resolves to a non-public address"
        )));
    }
    Ok(())
}

fn truncate(text: &str, max: usize) -> String {
    text.chars().take(max).collect()
}

/// One plugin tool bound to one caller. It always needs owner approval, never
/// gets a standing grant, and is never auto-approved: a third-party tool is
/// external, so the owner decides each use.
pub struct PluginTool {
    tools: PluginTools,
    user_id: String,
    slug: String,
    tool_name: String,
    description: Value,
}

#[async_trait]
impl Tool for PluginTool {
    fn spec(&self) -> ToolSpec {
        spec_of(&self.slug, &self.description).unwrap_or_else(|| ToolSpec {
            name: format!("{PREFIX}{}.{}", self.slug, self.tool_name),
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
        true
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
        let event = "plugin.call";
        let tool = format!("{PREFIX}{}.{}", self.slug, self.tool_name);
        self.tools.record(
            &self.user_id,
            event,
            &tool,
            context.task_id.as_deref(),
            outcome.is_ok(),
        );
        outcome
    }
}

impl PluginTool {
    async fn call(&self, manifest: &PluginManifest, args: &Value) -> Result<ToolOutput> {
        let (client, secret) = self.tools.client_for(&self.user_id, manifest).await?;
        let result = client
            .call_tool(
                secret.expose_secret(),
                &self.tool_name,
                args,
                REQUEST_TIMEOUT,
            )
            .await
            .map_err(|error| external(&self.slug, &error))?;
        Ok(ToolOutput {
            content: truncate(&result_text(&result), MAX_OUTPUT_CHARS),
            ..Default::default()
        })
    }
}

/// The text blocks of an MCP tool result, joined in order.
fn result_text(result: &Value) -> String {
    result
        .get("content")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter(|item| item.get("type").and_then(Value::as_str) == Some("text"))
                .filter_map(|item| item.get("text").and_then(Value::as_str))
                .collect::<Vec<_>>()
                .join("\n")
        })
        .unwrap_or_default()
}

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
            let error = check_resolved_addresses(url).await.err();
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
        let error = tools.client_for(&user, &manifest).await.err().unwrap();
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
}
