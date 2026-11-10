//! Per-user effective watcher sources (Big Wave B3).
//!
//! Admin sources come from TOML; members toggle a subset and may add RSS feeds
//! only on configured host allowlists. All fetches still go through
//! [`crate::fetch::SafeFetcher`] host restrictions.

use std::collections::{BTreeSet, HashMap};

use reqwest::Url;

use crate::config::{Config, SourceConfig};
use crate::error::{Result, SeraError};
use crate::store::{Store, UserSourceRow};

/// Merge admin `sources.toml` with the acting user's overlay rows.
pub fn effective_sources_for_user(
    config: &Config,
    store: &Store,
    user_id: &str,
) -> Result<Vec<SourceConfig>> {
    let admin = load_sources(config)?;
    let overlays = store.list_user_sources(user_id)?;
    let is_admin = store
        .admin_user_id()
        .map(|id| id == user_id)
        .unwrap_or(false);
    merge_sources(&admin, &overlays, is_admin)
}

/// Hostnames permitted for outbound watcher fetches at process start: every
/// admin source host plus explicit RSS allowlist entries.
pub fn watcher_allow_hosts(config: &Config) -> Result<BTreeSet<String>> {
    let mut hosts = BTreeSet::new();
    for source in load_sources(config)? {
        extend_host_aliases(&mut hosts, &source.url)?;
    }
    for host in &config.watcher.rss_allowlist_hosts {
        let normalized = host.trim().to_ascii_lowercase();
        if !normalized.is_empty() {
            hosts.insert(normalized);
        }
    }
    Ok(hosts)
}

/// Hostnames required for one user's effective source list (subset of the
/// global allow list).
pub fn hosts_for_sources(sources: &[SourceConfig]) -> Result<BTreeSet<String>> {
    let mut hosts = BTreeSet::new();
    for source in sources.iter().filter(|s| s.enabled) {
        extend_host_aliases(&mut hosts, &source.url)?;
    }
    Ok(hosts)
}

pub fn merge_sources(
    admin: &[SourceConfig],
    overlays: &[UserSourceRow],
    admin_user: bool,
) -> Result<Vec<SourceConfig>> {
    let mut by_name: HashMap<String, &UserSourceRow> =
        overlays.iter().map(|row| (row.name.clone(), row)).collect();
    let mut effective = Vec::new();
    for base in admin {
        let mut source = base.clone();
        if let Some(row) = by_name.remove(&base.name) {
            if row.origin != "overlay" {
                return Err(SeraError::Internal(format!(
                    "conflicting user source name {}",
                    base.name
                )));
            }
            source.enabled = row.enabled;
        } else if admin_user {
            // Admin keeps TOML defaults without an overlay row.
        } else {
            source.enabled = false;
        }
        effective.push(source);
    }
    for row in overlays {
        if row.origin != "member" {
            continue;
        }
        let url = row.url.as_deref().ok_or_else(|| {
            SeraError::Internal(format!("member source {} missing url", row.name))
        })?;
        effective.push(SourceConfig {
            name: row.name.clone(),
            url: url.to_owned(),
            kind: row.kind.clone(),
            enabled: row.enabled,
            item_selector: default_rss_selectors().0,
            title_selector: default_rss_selectors().1,
            date_selector: default_rss_selectors().2,
            link_selector: default_rss_selectors().3,
        });
    }
    let mut names = std::collections::HashSet::new();
    for source in &effective {
        if source.name.is_empty() || !names.insert(source.name.clone()) {
            return Err(SeraError::Config("source names must be unique".into()));
        }
        if !matches!(source.kind.as_str(), "html" | "rss") {
            return Err(SeraError::Config(format!(
                "source {}: kind must be html or rss",
                source.name
            )));
        }
    }
    Ok(effective)
}

/// Validate a member-added RSS URL against HTTPS and host policy.
pub fn validate_member_rss_url(config: &Config, url: &str) -> Result<String> {
    let parsed = Url::parse(url.trim())
        .map_err(|_| SeraError::Validation("url must be a valid HTTPS URL".into()))?;
    if parsed.scheme() != "https" {
        return Err(SeraError::Validation("member RSS must use https".into()));
    }
    if parsed.username() != "" || parsed.password().is_some() {
        return Err(SeraError::Validation(
            "url must not embed credentials".into(),
        ));
    }
    let host = parsed
        .host_str()
        .ok_or_else(|| SeraError::Validation("url must include a host".into()))?
        .to_ascii_lowercase();
    if !host_allowed_for_member_rss(config, &host) {
        return Err(SeraError::Validation(
            "rss host is not on the allowlist".into(),
        ));
    }
    Ok(parsed.to_string())
}

fn host_allowed_for_member_rss(config: &Config, host: &str) -> bool {
    let allowlist: Vec<String> = config
        .watcher
        .rss_allowlist_hosts
        .iter()
        .map(|h| h.trim().to_ascii_lowercase())
        .filter(|h| !h.is_empty())
        .collect();
    if allowlist.is_empty() {
        return false;
    }
    allowlist.iter().any(|allowed| host_matches(allowed, host))
}

fn host_matches(allowed: &str, host: &str) -> bool {
    if allowed == host {
        return true;
    }
    if let Some(suffix) = allowed.strip_prefix("*.") {
        return host.ends_with(&format!(".{suffix}")) || host == suffix;
    }
    false
}

fn extend_host_aliases(hosts: &mut BTreeSet<String>, url: &str) -> Result<()> {
    let parsed =
        Url::parse(url).map_err(|_| SeraError::Config(format!("invalid source url: {url}")))?;
    let Some(host) = parsed.host_str() else {
        return Err(SeraError::Config(format!("source url missing host: {url}")));
    };
    let lower = host.to_ascii_lowercase();
    hosts.insert(lower.clone());
    if let Some(stripped) = lower.strip_prefix("www.") {
        hosts.insert(stripped.to_owned());
    } else {
        hosts.insert(format!("www.{lower}"));
    }
    Ok(())
}

fn default_rss_selectors() -> (String, String, String, String) {
    (
        "item".into(),
        "title".into(),
        "pubDate".into(),
        "link".into(),
    )
}

pub use crate::config::load_sources;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::SourceConfig;

    fn admin_source(name: &str, enabled: bool) -> SourceConfig {
        SourceConfig {
            name: name.into(),
            url: format!("https://example.com/{name}.rss"),
            kind: "rss".into(),
            enabled,
            item_selector: "item".into(),
            title_selector: "title".into(),
            date_selector: "pubDate".into(),
            link_selector: "link".into(),
        }
    }

    #[test]
    fn member_defaults_disable_admin_sources() {
        let merged = merge_sources(
            &[admin_source("a", true), admin_source("b", true)],
            &[],
            false,
        )
        .unwrap();
        assert!(merged.iter().all(|s| !s.enabled));
    }

    #[test]
    fn member_overlay_enables_subset() {
        let overlay = UserSourceRow {
            name: "a".into(),
            origin: "overlay".into(),
            url: None,
            kind: "rss".into(),
            enabled: true,
        };
        let merged = merge_sources(&[admin_source("a", true)], &[overlay], false).unwrap();
        assert!(merged[0].enabled);
    }

    #[test]
    fn rss_url_rejects_non_allowlisted_host() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .parent()
            .unwrap();
        let config = Config::load(root.join("serabot.toml")).unwrap();
        assert!(validate_member_rss_url(&config, "https://evil.example/feed").is_err());
    }
}
