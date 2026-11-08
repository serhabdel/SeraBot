//! Per-user server browser: headless Chromium driven over CDP on loopback.
//!
//! Layout: `guard` (navigation/SSRF), `ws` + `cdp` (hand-rolled DevTools
//! client), `outline` (accessibility outline and refs), `manager` (sessions,
//! controller, capacity), `chromium` (launcher + backend), `tools` (gateway
//! tools). Page content returned to the model is untrusted data.

pub mod cdp;
pub mod chromium;
pub mod guard;
pub mod manager;
pub mod outline;
pub mod tools;
pub mod ws;

#[cfg(test)]
pub(crate) mod testing;

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use zeroize::Zeroizing;

use crate::config::Config;
use crate::error::{Result, SeraError};
use crate::user_secrets::UserSecrets;
use crate::vault_providers::Providers;

pub use manager::{BrowserInfo, BrowserManager, Controller, Driver, SecretResolver};
pub use tools::{BrowserToolDeps, register_browser_tools};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BrowserProvider {
    /// Per-user headless Chromium started by SeraBot (default).
    Spawn,
    RemoteCdp,
    BrowserOsNeo,
}

impl BrowserProvider {
    pub fn parse(raw: &str) -> Result<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "spawn" | "spawn_cdp" => Ok(Self::Spawn),
            "remote_cdp" => Ok(Self::RemoteCdp),
            "browseros_neo" => Ok(Self::BrowserOsNeo),
            other => Err(SeraError::Config(format!(
                "browser.provider must be spawn, remote_cdp, or browseros_neo (got '{other}')"
            ))),
        }
    }
}

/// Production secret resolver: the acting user's vault, by name and host. A
/// reference entry is read through its password manager at fill time.
pub struct VaultResolver {
    users: Arc<UserSecrets>,
    providers: Arc<Providers>,
}

impl VaultResolver {
    pub fn new(users: Arc<UserSecrets>, providers: Arc<Providers>) -> Self {
        Self { users, providers }
    }
}

#[async_trait]
impl SecretResolver for VaultResolver {
    fn authorize(&self, user_id: &str, name: &str, field: &str, host: &str) -> Result<()> {
        let field = crate::vault::VaultField::parse(field)?;
        crate::vault::authorize_fill(&self.users, user_id, name, host, field).map(|_| ())
    }

    async fn resolve(
        &self,
        user_id: &str,
        name: &str,
        field: &str,
        host: &str,
    ) -> Result<Zeroizing<String>> {
        let field = crate::vault::VaultField::parse(field)?;
        crate::vault_providers::resolve_for_browser(
            &self.users,
            &self.providers,
            user_id,
            name,
            host,
            field,
        )
        .await
    }
}

/// Build the manager for `[browser]`. Only the `spawn` provider supports
/// per-user sessions; the others are refused with a clear error.
pub fn build_manager(config: &Config) -> Result<Arc<BrowserManager>> {
    let browser = &config.browser;
    if BrowserProvider::parse(&browser.provider)? != BrowserProvider::Spawn {
        return Err(SeraError::Config(
            "browser.provider must be \"spawn\": per-user server browser sessions only run on a Chromium started by SeraBot".into(),
        ));
    }
    let base = if browser.user_data_dir.trim().is_empty() {
        config
            .database_path()
            .parent()
            .map(|parent| parent.join("browser"))
            .unwrap_or_else(|| config.path("browser"))
    } else {
        config.path(&browser.user_data_dir)
    };
    std::fs::create_dir_all(&base)?;
    let launcher = chromium::ChromiumLauncher::new(browser, base.clone())?;
    Ok(Arc::new(
        BrowserManager::new(
            Arc::new(launcher),
            browser.max_sessions,
            Duration::from_secs(browser.idle_timeout_secs),
        )
        .with_base_dir(base),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_providers() {
        assert_eq!(
            BrowserProvider::parse("spawn").unwrap(),
            BrowserProvider::Spawn
        );
        assert_eq!(
            BrowserProvider::parse("spawn_cdp").unwrap(),
            BrowserProvider::Spawn
        );
        assert_eq!(
            BrowserProvider::parse("Remote_CDP").unwrap(),
            BrowserProvider::RemoteCdp
        );
        assert!(BrowserProvider::parse("lynx").is_err());
    }

    #[tokio::test]
    async fn stub_vault_fails_closed_through_the_real_resolver_path() {
        // The vault stub errors; the resolver must surface that, not a value.
        // (Real secret typing is tested with the mocked resolver in tools.rs.)
        let store = crate::store::Store::open(
            std::env::temp_dir().join(format!("serabot-vr-{}.db", uuid::Uuid::new_v4())),
            384,
            5000,
        )
        .unwrap();
        let users = Arc::new(UserSecrets::with_master_key(store, [3u8; 32]).unwrap());
        let providers = Arc::new(Providers::new(&crate::config::VaultConfig::default()));
        let resolver = VaultResolver::new(users, providers);
        assert!(
            resolver
                .resolve("u", "bank", "username", "bank.example")
                .await
                .is_err()
        );
        assert!(
            resolver
                .authorize("u", "bank", "username", "bank.example")
                .is_err()
        );
    }
}
