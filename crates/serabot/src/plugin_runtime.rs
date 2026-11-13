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

use std::future::Future;
use std::net::SocketAddr;
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
/// Largest input schema passed to the model. A bigger one is skipped, not truncated,
/// since a cut-off schema would describe arguments wrongly.
const MAX_SCHEMA_BYTES: usize = 8 * 1024;

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
        let listed = client
            .list_tools(secret.expose_secret(), REQUEST_TIMEOUT)
            .await
            .map_err(|error| external(&manifest.slug, &error))?;
        let total = listed.len();
        let tools: Vec<Value> = listed.into_iter().filter(schema_within_limit).collect();
        if tools.len() < total {
            tracing::warn!(
                plugin = %manifest.slug,
                skipped = total - tools.len(),
                "plugin tools with an oversized input schema were skipped"
            );
        }
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
        // The client is pinned to the addresses checked here, so the key is
        // sent only to them, whatever DNS says later.
        let pinned = resolve_checked(url, system_lookup).await?;
        let client = McpClient::new_pinned(
            "plugin",
            url.to_owned(),
            McpAuth::Bearer,
            REQUEST_TIMEOUT,
            MAX_RESPONSE_BYTES,
            &pinned.host,
            &pinned.addrs,
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

/// Whether a tool's input schema is small enough to offer the model. A tool
/// without a schema is fine: the default one is used.
fn schema_within_limit(tool: &Value) -> bool {
    match tool.get("inputSchema") {
        None => true,
        Some(schema) => {
            serde_json::to_vec(schema).is_ok_and(|bytes| bytes.len() <= MAX_SCHEMA_BYTES)
        }
    }
}

/// The spec for one MCP tool entry, namespaced to its plugin.
fn spec_of(slug: &str, tool: &Value) -> Option<ToolSpec> {
    if !schema_within_limit(tool) {
        return None;
    }
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

/// The addresses one plugin connection is pinned to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PinnedAddresses {
    /// The bare host name the connection is pinned for.
    pub host: String,
    /// Every address the host resolved to. All of them passed the check.
    pub addrs: Vec<SocketAddr>,
}

/// The system resolver, as the lookup [`resolve_checked`] uses in production.
async fn system_lookup(host: String, port: u16) -> std::io::Result<Vec<SocketAddr>> {
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
