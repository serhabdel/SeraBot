//! Plugin catalog (v0.36): the `plugin.json` schema, its validation, and the
//! catalog loaded from `plugins/<slug>/plugin.json`.
//!
//! Schema version 1 is SeraBot's own format. It was designed with reference to
//! public plugin catalogs, but no text or code is copied from them. A plugin
//! names the secrets it needs (`secretKey`) and the hosts it may reach
//! (`permissions.egress`); secrets are stored per user, never as environment
//! variables on the shared server.

use std::collections::HashSet;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::error::{Result, SeraError};

pub const SCHEMA_VERSION: u32 = 1;
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
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum AuthKind {
    OauthLink,
    ApiKey,
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
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Permissions {
    /// Hosts the plugin may reach, as bare host names.
    pub egress: Vec<String>,
}

/// Validate one manifest. The error names the first field that fails.
pub fn validate(manifest: &PluginManifest, expected_slug: &str) -> Result<()> {
    if manifest.schema_version != SCHEMA_VERSION {
        return Err(invalid("schemaVersion", "must be 1"));
    }
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
        for auth in &app.auth {
            check_slug(&auth.id).map_err(|_| invalid("apps.auth.id", "must be a short id"))?;
            for field in &auth.fields {
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
    let ok = !value.is_empty()
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
                    }],
                }],
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
        manifest.schema_version = 2;
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
}
