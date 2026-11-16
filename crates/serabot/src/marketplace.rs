//! Plugin catalog (v0.36): the `plugin.json` schema, its validation, and the
//! catalog loaded from `plugins/<slug>/plugin.json`.
//!
//! Schema version 1 is SeraBot's own format. It was designed with reference to
//! public plugin catalogs, but no text or code is copied from them. A plugin
//! names the secrets it needs (`secretKey`) and the hosts it may reach
//! (`permissions.egress`); secrets are stored per user, never as environment
//! variables on the shared server.
//!
//! Schema version 2 (v0.51) is additive: `schemaVersion: 2` manifests may use
//! `auth.kind` `oauth` or `none`, an `oauth` block, a per-field request
//! `header`, `tools` filters and `accounts`. A version 1 manifest loads exactly
//! as before and may not use any of those.

use std::collections::HashSet;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::error::{Result, SeraError};

pub const SCHEMA_VERSION: u32 = 1;
/// The additive v0.51 schema (OAuth, header rules, tool filters, accounts).
pub const SCHEMA_VERSION_V2: u32 = 2;
const MAX_FILE_BYTES: u64 = 64 * 1024;
const MAX_SLUG_CHARS: usize = 48;
const MAX_TEXT_CHARS: usize = 2_000;
const MAX_LIST_ITEMS: usize = 16;

/// The `plugin.json` document.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
#[serde(rename_all = "camelCase")]
pub struct PluginManifest {
    pub schema_version: u32,
    pub slug: String,
    pub name: String,
    pub tagline: String,
    pub description: String,
    pub category: String,
    pub icon_url: String,
    pub version: String,
    pub min_server_version: String,
    #[serde(default)]
    pub prompts: Vec<Prompt>,
    pub apps: Vec<App>,
    pub permissions: Permissions,
    pub website_url: String,
    pub privacy_policy_url: String,
    pub terms_url: String,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Prompt {
    pub id: String,
    pub text: String,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct App {
    pub id: String,
    pub name: String,
    pub server: Server,
    #[serde(default)]
    pub auth: Vec<Auth>,
    /// v2: which of the server's tools may be offered, and which are read only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tools: Option<ToolFilter>,
    /// v2: whether a member may hold several accounts for this app.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub accounts: Option<AccountRules>,
}

/// v2 tool filters, by tool name. `deny` wins over `allow`; an empty `allow`
/// means every tool not denied. `read_only` names tools the manifest vouches
/// for as read only, which the runtime may treat as such.
#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
#[serde(rename_all = "camelCase")]
pub struct ToolFilter {
    #[serde(default)]
    pub allow: Vec<String>,
    #[serde(default)]
    pub deny: Vec<String>,
    #[serde(default)]
    pub read_only: Vec<String>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AccountRules {
    #[serde(default)]
    pub multiple: bool,
}

/// How the plugin's tool server is reached. Only `http` runs in this build;
/// `stdio` is declared so the schema is complete, and install refuses it.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
#[serde(tag = "transport", rename_all = "lowercase")]
pub enum Server {
    Http { url: String },
    Stdio { command: String, args: Vec<String> },
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Auth {
    pub id: String,
    pub kind: AuthKind,
    #[serde(default)]
    pub fields: Vec<Field>,
    /// v2, `kind: oauth` only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub oauth: Option<OauthSpec>,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum AuthKind {
    /// v1: the owner links the account in a browser; no secret is stored here.
    OauthLink,
    ApiKey,
    /// v2: MCP OAuth (RFC 9728 discovery, PKCE, resource indicators).
    Oauth,
    /// v2: the server needs no credential.
    None,
}

/// v2 OAuth settings for an MCP server. Discovery finds the endpoints; these
/// only steer it.
#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
#[serde(rename_all = "camelCase")]
pub struct OauthSpec {
    /// Where the protected-resource metadata lives, when it is not at the
    /// well-known path for the server URL.
    #[serde(default)]
    pub resource_metadata_url: Option<String>,
    #[serde(default)]
    pub scopes: Vec<String>,
    /// A pre-registered public client id, used when neither a client metadata
    /// document nor dynamic registration is available.
    #[serde(default)]
    pub client_id: Option<String>,
    /// Offer SeraBot's client metadata document as the client id.
    #[serde(default)]
    pub uses_cimd: bool,
}

/// How an API-key field is sent: `bearer` puts `Bearer ` before the value, `raw`
/// sends the value alone.
#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum HeaderScheme {
    Bearer,
    Raw,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct HeaderSpec {
    pub name: String,
    pub scheme: HeaderScheme,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
#[serde(rename_all = "camelCase")]
pub struct Field {
    pub id: String,
    pub label: String,
    pub secret_key: String,
    #[serde(default)]
    pub hint: String,
    #[serde(default)]
    pub docs_url: Option<String>,
    /// v2: the request header this field's value is sent in.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub header: Option<HeaderSpec>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Permissions {
    /// Hosts the plugin may reach, as bare host names.
    pub egress: Vec<String>,
}

/// Validate one manifest. The error names the first field that fails.
pub fn validate(manifest: &PluginManifest, expected_slug: &str) -> Result<()> {
    if manifest.schema_version != SCHEMA_VERSION && manifest.schema_version != SCHEMA_VERSION_V2 {
        return Err(invalid("schemaVersion", "must be 1 or 2"));
    }
    let v2 = manifest.schema_version == SCHEMA_VERSION_V2;
    check_slug(&manifest.slug)
        .map_err(|_| invalid("slug", "must be lowercase letters, digits and dashes"))?;
    if manifest.slug != expected_slug {
        return Err(invalid("slug", "must match the folder name"));
    }
    check_text("name", &manifest.name, 80)?;
    check_text("tagline", &manifest.tagline, 160)?;
    check_text("description", &manifest.description, MAX_TEXT_CHARS)?;
    check_text("category", &manifest.category, 32)?;
    check_version("version", &manifest.version)?;
    check_version("minServerVersion", &manifest.min_server_version)?;
    check_https("iconUrl", &manifest.icon_url)?;
    check_https("websiteUrl", &manifest.website_url)?;
    check_https("privacyPolicyUrl", &manifest.privacy_policy_url)?;
    check_https("termsUrl", &manifest.terms_url)?;
    if manifest.prompts.len() > MAX_LIST_ITEMS {
        return Err(invalid("prompts", "has too many entries"));
    }
    for prompt in &manifest.prompts {
        check_slug(&prompt.id).map_err(|_| invalid("prompts.id", "must be a short id"))?;
        check_text("prompts.text", &prompt.text, 400)?;
    }
    if manifest.apps.is_empty() || manifest.apps.len() > MAX_LIST_ITEMS {
        return Err(invalid("apps", "must list one to 16 apps"));
    }
    let mut secret_keys = HashSet::new();
    for app in &manifest.apps {
        check_slug(&app.id).map_err(|_| invalid("apps.id", "must be a short id"))?;
        check_text("apps.name", &app.name, 80)?;
        match &app.server {
            Server::Http { url } => check_https("apps.server.url", url)?,
            Server::Stdio { command, args } => {
                check_text("apps.server.command", command, 200)?;
                if args.len() > MAX_LIST_ITEMS {
                    return Err(invalid("apps.server.args", "has too many entries"));
                }
            }
        }
        check_app_v2(app, v2, &manifest.permissions.egress)?;
        for auth in &app.auth {
            check_slug(&auth.id).map_err(|_| invalid("apps.auth.id", "must be a short id"))?;
            check_auth_kind(auth, v2)?;
            for field in &auth.fields {
                check_field_header(field, auth.kind, v2)?;
                check_id(&field.id)
                    .map_err(|_| invalid("apps.auth.fields.id", "must be a short id"))?;
                check_text("apps.auth.fields.label", &field.label, 80)?;
                check_secret_key(&field.secret_key)?;
                if !secret_keys.insert(field.secret_key.clone()) {
                    return Err(invalid(
                        "apps.auth.fields.secretKey",
                        "is used by two fields",
                    ));
                }
                if let Some(url) = &field.docs_url {
                    check_https("apps.auth.fields.docsUrl", url)?;
                }
            }
        }
    }
    if manifest.permissions.egress.is_empty() || manifest.permissions.egress.len() > MAX_LIST_ITEMS
    {
        return Err(invalid("permissions.egress", "must list one to 16 hosts"));
    }
    for host in &manifest.permissions.egress {
        check_host(host)?;
    }
    Ok(())
}

/// Request headers a manifest may never set: they frame the request or carry
/// identity the client owns.
const DENIED_HEADERS: &[&str] = &[
    "host",
    "content-length",
    "cookie",
    "set-cookie",
    "transfer-encoding",
    "connection",
    "upgrade",
    "te",
    "trailer",
    "proxy-authorization",
    "content-type",
    "accept",
    "mcp-session-id",
    "mcp-protocol-version",
];
const MAX_TOOL_NAMES: usize = 64;
const MAX_SCOPES: usize = 16;

fn check_auth_kind(auth: &Auth, v2: bool) -> Result<()> {
    match auth.kind {
        AuthKind::OauthLink | AuthKind::ApiKey => {
            if auth.oauth.is_some() {
                return Err(invalid("apps.auth.oauth", "is only for kind oauth"));
            }
            if v2 && auth.kind == AuthKind::OauthLink {
                return Err(invalid(
                    "apps.auth.kind",
                    "oauth-link is a v1 kind; use oauth",
                ));
            }
            if v2 && auth.kind == AuthKind::ApiKey && auth.fields.is_empty() {
                return Err(invalid("apps.auth.fields", "an api-key needs a field"));
            }
        }
        AuthKind::Oauth | AuthKind::None if !v2 => {
            return Err(invalid(
                "apps.auth.kind",
                "oauth and none need schemaVersion 2",
            ));
        }
        AuthKind::Oauth => {
            if !auth.fields.is_empty() {
                return Err(invalid("apps.auth.fields", "an oauth auth has no fields"));
            }
            let spec = auth
                .oauth
                .as_ref()
                .ok_or_else(|| invalid("apps.auth.oauth", "is required for kind oauth"))?;
            check_oauth(spec)?;
        }
        AuthKind::None => {
            if !auth.fields.is_empty() || auth.oauth.is_some() {
                return Err(invalid("apps.auth", "kind none has no fields or oauth"));
            }
        }
    }
    Ok(())
}

fn check_oauth(spec: &OauthSpec) -> Result<()> {
    if let Some(url) = &spec.resource_metadata_url {
        check_https("apps.auth.oauth.resourceMetadataUrl", url)?;
    }
    if spec.scopes.len() > MAX_SCOPES {
        return Err(invalid("apps.auth.oauth.scopes", "has too many entries"));
    }
    for scope in &spec.scopes {
        let ok = !scope.is_empty()
            && scope.len() <= 100
            && scope
                .bytes()
                .all(|b| b.is_ascii_graphic() && b != b'"' && b != b'\\');
        if !ok {
            return Err(invalid("apps.auth.oauth.scopes", "has an invalid scope"));
        }
    }
    if let Some(id) = &spec.client_id {
        let ok = !id.is_empty() && id.len() <= 200 && id.bytes().all(|b| b.is_ascii_graphic());
        if !ok {
            return Err(invalid("apps.auth.oauth.clientId", "is not a client id"));
        }
    }
    Ok(())
}

fn check_field_header(field: &Field, kind: AuthKind, v2: bool) -> Result<()> {
    let Some(header) = &field.header else {
        return Ok(());
    };
    if !v2 || kind != AuthKind::ApiKey {
        return Err(invalid(
            "apps.auth.fields.header",
            "needs schemaVersion 2 and kind api-key",
        ));
    }
    let name = header.name.to_ascii_lowercase();
    let token = !name.is_empty()
        && name.len() <= 64
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_');
    if !token {
        return Err(invalid(
            "apps.auth.fields.header.name",
            "is not a header name",
        ));
    }
    if DENIED_HEADERS.contains(&name.as_str()) {
        return Err(invalid(
            "apps.auth.fields.header.name",
            "names a header a manifest may not set",
        ));
    }
    Ok(())
}

fn check_app_v2(app: &App, v2: bool, egress: &[String]) -> Result<()> {
    if (app.tools.is_some() || app.accounts.is_some()) && !v2 {
        return Err(invalid("apps", "tools and accounts need schemaVersion 2"));
    }
    if let Some(tools) = &app.tools {
        for (field, names) in [
            ("apps.tools.allow", &tools.allow),
            ("apps.tools.deny", &tools.deny),
            ("apps.tools.readOnly", &tools.read_only),
        ] {
            if names.len() > MAX_TOOL_NAMES {
                return Err(invalid(field, "has too many entries"));
            }
            for name in names {
                let ok = !name.is_empty()
                    && name.len() <= 64
                    && name
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.'));
                if !ok {
                    return Err(invalid(field, "has an invalid tool name"));
                }
            }
        }
        if tools.allow.iter().any(|name| tools.deny.contains(name)) {
            return Err(invalid(
                "apps.tools",
                "lists a tool as both allowed and denied",
            ));
        }
    }
    // The resource metadata document is fetched with the plugin's reach: its
    // host must be one the manifest declares.
    for auth in &app.auth {
        if let Some(url) = auth
            .oauth
            .as_ref()
            .and_then(|spec| spec.resource_metadata_url.as_deref())
        {
            let host = url::Url::parse(url)
                .ok()
                .and_then(|parsed| parsed.host_str().map(str::to_ascii_lowercase));
            if !host.is_some_and(|host| egress.iter().any(|e| e.eq_ignore_ascii_case(&host))) {
                return Err(invalid(
                    "apps.auth.oauth.resourceMetadataUrl",
                    "must use a host on permissions.egress",
                ));
            }
        }
    }
    Ok(())
}

const BUILTIN_MANIFESTS: &[(&str, &str)] = &[
    (
        "brave-search",
        include_str!("../../../plugins/brave-search/plugin.json"),
    ),
    (
        "elevenlabs",
        include_str!("../../../plugins/elevenlabs/plugin.json"),
    ),
    ("exa", include_str!("../../../plugins/exa/plugin.json")),
    (
        "firecrawl",
        include_str!("../../../plugins/firecrawl/plugin.json"),
    ),
    (
        "github",
        include_str!("../../../plugins/github/plugin.json"),
    ),
    (
        "linear",
        include_str!("../../../plugins/linear/plugin.json"),
    ),
    (
        "notion",
        include_str!("../../../plugins/notion/plugin.json"),
    ),
    (
        "resend",
        include_str!("../../../plugins/resend/plugin.json"),
    ),
    (
        "tavily",
        include_str!("../../../plugins/tavily/plugin.json"),
    ),
    (
        "workspace-files",
        include_str!("../../../plugins/workspace-files/plugin.json"),
    ),
];

/// The shipped plugin manifests compiled into the binary as a fallback when no
/// external `plugins/` folder is deployed next to the server, sorted by
/// `(name, slug)`.
pub fn builtin_catalog() -> Vec<PluginManifest> {
    let mut plugins = Vec::with_capacity(BUILTIN_MANIFESTS.len());
    for (slug, raw) in BUILTIN_MANIFESTS {
        let manifest: PluginManifest = serde_json::from_str(raw).unwrap_or_else(|error| {
            panic!("built-in plugin manifest {slug} is invalid JSON: {error}")
        });
        validate(&manifest, slug).unwrap_or_else(|error| {
            panic!("built-in plugin manifest {slug} failed validation: {error}")
        });
        plugins.push(manifest);
    }
    plugins.sort_by(|a, b| a.name.cmp(&b.name).then_with(|| a.slug.cmp(&b.slug)));
    plugins
}

/// Load `plugins/<slug>/plugin.json` from `root` (when `root` exists) and merge
/// any built-in manifests whose `slug` is not already present on disk, sorted
/// by `(name, slug)`.
pub fn load_catalog_with_builtin(root: &Path) -> (Vec<PluginManifest>, Vec<String>) {
    let (mut plugins, problems) = load_catalog(root);
    let present: HashSet<String> = plugins.iter().map(|plugin| plugin.slug.clone()).collect();
    for builtin in builtin_catalog() {
        if !present.contains(&builtin.slug) {
            plugins.push(builtin);
        }
    }
    plugins.sort_by(|a, b| a.name.cmp(&b.name).then_with(|| a.slug.cmp(&b.slug)));
    (plugins, problems)
}

/// Load every `plugins/<slug>/plugin.json` under `root`, sorted by slug. A file
/// that fails to read or validate is reported and skipped; it never stops the
/// others from loading.
pub fn load_catalog(root: &Path) -> (Vec<PluginManifest>, Vec<String>) {
    let mut plugins = Vec::new();
    let mut problems = Vec::new();
    let Ok(entries) = std::fs::read_dir(root) else {
        return (plugins, problems);
    };
    let mut folders: Vec<_> = entries.flatten().map(|entry| entry.path()).collect();
    folders.sort();
    for folder in folders {
        if !folder.is_dir() {
            continue;
        }
        let Some(slug) = folder
            .file_name()
            .and_then(|name| name.to_str())
            .map(str::to_owned)
        else {
            continue;
        };
        let file = folder.join("plugin.json");
        match load_one(&file, &slug) {
            Ok(manifest) => plugins.push(manifest),
            Err(error) => problems.push(format!("{slug}: {error}")),
        }
    }
    (plugins, problems)
}

fn load_one(file: &Path, slug: &str) -> Result<PluginManifest> {
    let meta =
        std::fs::metadata(file).map_err(|_| SeraError::Config("plugin.json is missing".into()))?;
    if meta.len() > MAX_FILE_BYTES {
        return Err(SeraError::Config("plugin.json is too large".into()));
    }
    let text = std::fs::read_to_string(file)
        .map_err(|_| SeraError::Config("plugin.json is unreadable".into()))?;
    let manifest: PluginManifest = serde_json::from_str(&text)
        .map_err(|error| SeraError::Config(format!("plugin.json is not valid: {error}")))?;
    validate(&manifest, slug)?;
    Ok(manifest)
}

fn invalid(field: &str, why: &str) -> SeraError {
    SeraError::Validation(format!("plugin field {field} {why}"))
}

fn check_slug(value: &str) -> std::result::Result<(), ()> {
    let ok = !value.is_empty()
        && value.len() <= MAX_SLUG_CHARS
        && value.bytes().next().is_some_and(|b| b.is_ascii_lowercase())
        && value
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        && !value.ends_with('-');
    if ok { Ok(()) } else { Err(()) }
}

/// A field id may use underscores (for example `api_key`); other ids are slugs.
fn check_id(value: &str) -> std::result::Result<(), ()> {
    let ok = !value.is_empty()
        && value.len() <= MAX_SLUG_CHARS
        && value.bytes().next().is_some_and(|b| b.is_ascii_lowercase())
        && value
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'_');
    if ok { Ok(()) } else { Err(()) }
}

fn check_text(field: &str, value: &str, max: usize) -> Result<()> {
    let trimmed = value.trim();
    if trimmed.is_empty() || trimmed.chars().count() > max || trimmed.chars().any(char::is_control)
    {
        return Err(invalid(
            field,
            &format!("must be 1 to {max} characters with no control characters"),
        ));
    }
    Ok(())
}

fn check_version(field: &str, value: &str) -> Result<()> {
    let parts: Vec<&str> = value.split('.').collect();
    let ok = parts.len() == 3
        && parts.iter().all(|part| {
            !part.is_empty() && part.len() <= 4 && part.bytes().all(|b| b.is_ascii_digit())
        });
    if ok {
        Ok(())
    } else {
        Err(invalid(field, "must be a version like 1.2.3"))
    }
}

fn check_https(field: &str, value: &str) -> Result<()> {
    let parsed = url::Url::parse(value).map_err(|_| invalid(field, "must be an https URL"))?;
    if parsed.scheme() != "https" || !parsed.username().is_empty() || parsed.password().is_some() {
        return Err(invalid(field, "must be an https URL without credentials"));
    }
    if parsed.host_str().is_none() {
        return Err(invalid(field, "must name a host"));
    }
    Ok(())
}

fn check_host(value: &str) -> Result<()> {
    let ok = !value.is_empty()
        && value.len() <= 253
        && value.contains('.')
        && value
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'.')
        && !value.starts_with('.')
        && !value.ends_with('.');
    if ok {
        Ok(())
    } else {
        Err(invalid(
            "permissions.egress",
            "must be bare host names such as api.example.com",
        ))
    }
}

fn check_secret_key(value: &str) -> Result<()> {
    // `marketplace.<slug>.<account>.oauth` is where OAuth records are sealed.
    let ok = !value.is_empty()
        && !value.eq_ignore_ascii_case("oauth")
        && !value.to_ascii_lowercase().ends_with(".oauth")
        && value.len() <= 96
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'_' || b == b'-');
    if ok {
        Ok(())
    } else {
        Err(invalid(
            "apps.auth.fields.secretKey",
            "must be a simple name such as service.api_key",
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> PluginManifest {
        PluginManifest {
            schema_version: 1,
            slug: "sample-plugin".into(),
            name: "Sample".into(),
            tagline: "A sample.".into(),
            description: "A sample plugin used by tests.".into(),
            category: "research".into(),
            icon_url: "https://sample.example.invalid/icon.png".into(),
            version: "1.0.0".into(),
            min_server_version: "0.36.0".into(),
            prompts: vec![Prompt {
                id: "p1".into(),
                text: "Try it.".into(),
            }],
            apps: vec![App {
                id: "main".into(),
                name: "Main".into(),
                server: Server::Http {
                    url: "https://mcp.sample.example.invalid/mcp".into(),
                },
                auth: vec![Auth {
                    id: "key".into(),
                    kind: AuthKind::ApiKey,
                    fields: vec![Field {
                        id: "api_key".into(),
                        label: "API key".into(),
                        secret_key: "sample.api_key".into(),
                        hint: String::new(),
                        docs_url: None,
                        header: None,
                    }],
                    oauth: None,
                }],
                tools: None,
                accounts: None,
            }],
            permissions: Permissions {
                egress: vec!["mcp.sample.example.invalid".into()],
            },
            website_url: "https://sample.example.invalid".into(),
            privacy_policy_url: "https://sample.example.invalid/privacy".into(),
            terms_url: "https://sample.example.invalid/terms".into(),
        }
    }

    #[test]
    fn a_well_formed_manifest_validates() {
        assert!(validate(&sample(), "sample-plugin").is_ok());
    }

    #[test]
    fn a_manifest_must_match_its_folder_and_version() {
        let mut manifest = sample();
        assert!(validate(&manifest, "other-plugin").is_err());
        manifest.schema_version = 3;
        assert!(validate(&manifest, "sample-plugin").is_err());
    }

    #[test]
    fn urls_must_be_https_without_credentials() {
        let mut manifest = sample();
        manifest.website_url = "http://sample.example.invalid".into();
        assert!(validate(&manifest, "sample-plugin").is_err());
        manifest.website_url = "https://user:pass@sample.example.invalid".into();
        assert!(validate(&manifest, "sample-plugin").is_err());
    }

    #[test]
    fn egress_must_be_bare_host_names() {
        let mut manifest = sample();
        manifest.permissions.egress = vec!["https://mcp.example.invalid/mcp".into()];
        assert!(validate(&manifest, "sample-plugin").is_err());
        manifest.permissions.egress = vec!["*.example.invalid".into()];
        assert!(validate(&manifest, "sample-plugin").is_err());
    }

    #[test]
    fn secret_keys_are_simple_and_not_shared_between_fields() {
        let mut manifest = sample();
        manifest.apps[0].auth[0].fields[0].secret_key = "bad key with spaces".into();
        assert!(validate(&manifest, "sample-plugin").is_err());
        let mut manifest = sample();
        manifest.apps[0].auth[0].fields[0].secret_key = "acct.oauth".into();
        assert!(validate(&manifest, "sample-plugin").is_err());
        let mut manifest = sample();
        let duplicate = manifest.apps[0].auth[0].fields[0].clone();
        manifest.apps[0].auth[0].fields.push(duplicate);
        assert!(validate(&manifest, "sample-plugin").is_err());
    }

    #[test]
    fn unknown_fields_are_rejected_on_parse() {
        let mut value = serde_json::to_value(sample()).unwrap();
        value["sneaky"] = serde_json::json!(true);
        assert!(serde_json::from_value::<PluginManifest>(value).is_err());
    }

    #[test]
    fn the_repository_catalog_loads_with_ten_valid_plugins() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../plugins");
        let (plugins, problems) = load_catalog(&root);
        assert!(problems.is_empty(), "{problems:?}");
        assert_eq!(
            plugins.len(),
            10,
            "{:?}",
            plugins.iter().map(|p| &p.slug).collect::<Vec<_>>()
        );
    }

    #[test]
    fn a_bad_plugin_file_is_reported_and_the_rest_still_load() {
        let dir = tempfile::tempdir().unwrap();
        let good = dir.path().join("sample-plugin");
        std::fs::create_dir_all(&good).unwrap();
        std::fs::write(
            good.join("plugin.json"),
            serde_json::to_string(&sample()).unwrap(),
        )
        .unwrap();
        let bad = dir.path().join("broken");
        std::fs::create_dir_all(&bad).unwrap();
        std::fs::write(bad.join("plugin.json"), "{not json").unwrap();
        let (plugins, problems) = load_catalog(dir.path());
        assert_eq!(plugins.len(), 1);
        assert_eq!(problems.len(), 1);
        assert!(problems[0].starts_with("broken:"));
    }

    #[test]
    fn builtin_catalog_parses_all_ten_shipped_manifests() {
        let builtin = builtin_catalog();
        assert_eq!(builtin.len(), 10);
        let slugs: Vec<&str> = builtin.iter().map(|p| p.slug.as_str()).collect();
        for expected in [
            "brave-search",
            "elevenlabs",
            "exa",
            "firecrawl",
            "github",
            "linear",
            "notion",
            "resend",
            "tavily",
            "workspace-files",
        ] {
            assert!(slugs.contains(&expected), "missing built-in {expected}");
        }
        // Sorted by (name, slug).
        let mut sorted = builtin.clone();
        sorted.sort_by(|a, b| a.name.cmp(&b.name).then_with(|| a.slug.cmp(&b.slug)));
        assert_eq!(builtin, sorted);
    }

    #[test]
    fn load_catalog_with_builtin_returns_all_ten_when_dir_is_missing_or_empty() {
        let missing = std::path::Path::new("/nonexistent/serabot-plugins-dir-xyz");
        let (plugins, problems) = load_catalog_with_builtin(missing);
        assert!(problems.is_empty(), "{problems:?}");
        assert_eq!(plugins.len(), 10);

        let empty = tempfile::tempdir().unwrap();
        let (plugins, problems) = load_catalog_with_builtin(empty.path());
        assert!(problems.is_empty(), "{problems:?}");
        assert_eq!(plugins.len(), 10);
    }

    #[test]
    fn load_catalog_with_builtin_merges_and_prefers_disk_override() {
        let dir = tempfile::tempdir().unwrap();
        let override_dir = dir.path().join("tavily");
        std::fs::create_dir_all(&override_dir).unwrap();
        let mut custom = sample();
        custom.slug = "tavily".into();
        custom.name = "Custom Tavily Override".into();
        std::fs::write(
            override_dir.join("plugin.json"),
            serde_json::to_string(&custom).unwrap(),
        )
        .unwrap();

        let (plugins, problems) = load_catalog_with_builtin(dir.path());
        assert!(problems.is_empty(), "{problems:?}");
        assert_eq!(plugins.len(), 10);
        let tavily = plugins.iter().find(|p| p.slug == "tavily").unwrap();
        assert_eq!(tavily.name, "Custom Tavily Override");
    }

    fn sample_v2_oauth() -> PluginManifest {
        let mut manifest = sample();
        manifest.schema_version = 2;
        manifest.apps[0].auth = vec![Auth {
            id: "signin".into(),
            kind: AuthKind::Oauth,
            fields: vec![],
            oauth: Some(OauthSpec {
                resource_metadata_url: Some(
                    "https://mcp.sample.example.invalid/.well-known/oauth-protected-resource"
                        .into(),
                ),
                scopes: vec!["read".into(), "write".into()],
                client_id: None,
                uses_cimd: true,
            }),
        }];
        manifest.apps[0].tools = Some(ToolFilter {
            allow: vec![],
            deny: vec!["delete_everything".into()],
            read_only: vec!["search".into()],
        });
        manifest.apps[0].accounts = Some(AccountRules { multiple: true });
        manifest
    }

    #[test]
    fn a_v1_manifest_still_loads_and_round_trips_without_v2_fields() {
        let text = serde_json::to_string(&sample()).unwrap();
        assert!(!text.contains("oauth"));
        assert!(!text.contains("header"));
        let back: PluginManifest = serde_json::from_str(&text).unwrap();
        assert!(validate(&back, "sample-plugin").is_ok());
        // A v1 file written before v2 existed has none of the new keys.
        let legacy = r#"{"schemaVersion":1,"slug":"sample-plugin","name":"S","tagline":"t",
            "description":"d","category":"c","iconUrl":"https://a.example.invalid/i.png",
            "version":"1.0.0","minServerVersion":"0.36.0",
            "apps":[{"id":"m","name":"M","server":{"transport":"http","url":"https://a.example.invalid/mcp"},
              "auth":[{"id":"link","kind":"oauth-link","fields":[]}]}],
            "permissions":{"egress":["a.example.invalid"]},
            "websiteUrl":"https://a.example.invalid","privacyPolicyUrl":"https://a.example.invalid/p",
            "termsUrl":"https://a.example.invalid/t"}"#;
        let manifest: PluginManifest = serde_json::from_str(legacy).unwrap();
        assert!(validate(&manifest, "sample-plugin").is_ok());
    }

    #[test]
    fn a_v2_oauth_manifest_validates_and_round_trips() {
        let manifest = sample_v2_oauth();
        assert!(validate(&manifest, "sample-plugin").is_ok());
        let text = serde_json::to_string(&manifest).unwrap();
        let back: PluginManifest = serde_json::from_str(&text).unwrap();
        assert_eq!(back, manifest);
    }

    #[test]
    fn v2_features_are_refused_in_a_v1_manifest() {
        let mut manifest = sample_v2_oauth();
        manifest.schema_version = 1;
        assert!(validate(&manifest, "sample-plugin").is_err());
        let mut manifest = sample();
        manifest.apps[0].accounts = Some(AccountRules { multiple: true });
        assert!(validate(&manifest, "sample-plugin").is_err());
        let mut manifest = sample();
        manifest.apps[0].auth[0].fields[0].header = Some(HeaderSpec {
            name: "x-api-key".into(),
            scheme: HeaderScheme::Raw,
        });
        assert!(validate(&manifest, "sample-plugin").is_err());
    }

    #[test]
    fn a_denied_or_malformed_header_is_rejected() {
        for name in ["Host", "content-length", "Cookie", "bad header", "x:y", ""] {
            let mut manifest = sample();
            manifest.schema_version = 2;
            manifest.apps[0].auth[0].fields[0].header = Some(HeaderSpec {
                name: name.into(),
                scheme: HeaderScheme::Raw,
            });
            assert!(validate(&manifest, "sample-plugin").is_err(), "{name}");
        }
        let mut manifest = sample();
        manifest.schema_version = 2;
        manifest.apps[0].auth[0].fields[0].header = Some(HeaderSpec {
            name: "X-Api-Key".into(),
            scheme: HeaderScheme::Raw,
        });
        assert!(validate(&manifest, "sample-plugin").is_ok());
    }

    #[test]
    fn oauth_settings_are_checked() {
        let mut manifest = sample_v2_oauth();
        manifest.apps[0].auth[0].oauth = None;
        assert!(validate(&manifest, "sample-plugin").is_err());
        let mut manifest = sample_v2_oauth();
        manifest.apps[0].auth[0]
            .oauth
            .as_mut()
            .unwrap()
            .resource_metadata_url = Some("https://other.example.invalid/meta".into());
        assert!(validate(&manifest, "sample-plugin").is_err());
        let mut manifest = sample_v2_oauth();
        manifest.apps[0].auth[0].oauth.as_mut().unwrap().scopes = vec!["has space".into()];
        assert!(validate(&manifest, "sample-plugin").is_err());
        let mut manifest = sample_v2_oauth();
        manifest.apps[0].tools = Some(ToolFilter {
            allow: vec!["a".into()],
            deny: vec!["a".into()],
            read_only: vec![],
        });
        assert!(validate(&manifest, "sample-plugin").is_err());
    }
}
