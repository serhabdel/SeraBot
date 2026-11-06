//! Fake backend/launcher/resolver for synthetic tests (no Chromium).

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use zeroize::Zeroizing;

use super::manager::{BrowserBackend, BrowserLauncher, BrowserManager, PageInfo, SecretResolver};
use super::outline::{Outline, RefTarget};
use crate::error::{Result, SeraError};

pub(crate) const FAKE_SECRET: &str = "s3cr3t-value-123";

pub(crate) struct FakeBackend {
    pub user: String,
    pub calls: std::sync::Mutex<Vec<String>>,
    pub alive: AtomicBool,
    pub shutdown_count: Arc<AtomicUsize>,
    url: std::sync::Mutex<String>,
}

impl FakeBackend {
    fn record(&self, call: impl Into<String>) {
        self.calls.lock().unwrap().push(call.into());
    }
}

#[async_trait]
impl BrowserBackend for FakeBackend {
    async fn navigate(&self, url: &str) -> Result<PageInfo> {
        self.record(format!("navigate {url}"));
        *self.url.lock().unwrap() = url.to_owned();
        Ok(PageInfo {
            url: url.to_owned(),
            title: format!("title of {}", self.user),
        })
    }
    async fn snapshot(&self) -> Result<(PageInfo, Outline)> {
        self.record("snapshot");
        let info = self.info().await?;
        let mut outline = Outline::default();
        outline.refs.push(RefTarget {
            backend_node_id: 7,
            role: "textbox".into(),
            name: "Password".into(),
        });
        // A page that echoes the typed secret back, to exercise redaction.
        outline.text = format!("[e1] textbox \"Password\" value=\"{FAKE_SECRET}\"");
        Ok((info, outline))
    }
    async fn click_ref(&self, reference: &str) -> Result<PageInfo> {
        self.record(format!("click {reference}"));
        self.info().await
    }
    async fn type_ref(&self, reference: &str, text: &str, submit: bool) -> Result<()> {
        self.record(format!("type {reference} {text} {submit}"));
        Ok(())
    }
    async fn press(&self, key: &str) -> Result<()> {
        self.record(format!("press {key}"));
        Ok(())
    }
    async fn click_at(&self, x: f64, y: f64) -> Result<()> {
        self.record(format!("click_at {x} {y}"));
        Ok(())
    }
    async fn insert_text(&self, text: &str) -> Result<()> {
        self.record(format!("insert {text}"));
        Ok(())
    }
    async fn scroll(&self, dy: f64) -> Result<()> {
        self.record(format!("scroll {dy}"));
        Ok(())
    }
    async fn screenshot_jpeg(&self) -> Result<Vec<u8>> {
        // JPEG magic + the owning user, so isolation tests can tell sessions apart.
        let mut bytes = vec![0xff, 0xd8, 0xff, 0xe0];
        bytes.extend_from_slice(self.user.as_bytes());
        Ok(bytes)
    }
    async fn info(&self) -> Result<PageInfo> {
        Ok(PageInfo {
            url: self.url.lock().unwrap().clone(),
            title: format!("title of {}", self.user),
        })
    }
    async fn is_alive(&self) -> bool {
        self.alive.load(Ordering::SeqCst)
    }
    async fn shutdown(&self) {
        self.shutdown_count.fetch_add(1, Ordering::SeqCst);
        self.alive.store(false, Ordering::SeqCst);
    }
}

#[derive(Default)]
pub(crate) struct FakeLauncher {
    launches: std::sync::Mutex<Vec<String>>,
    backends: std::sync::Mutex<Vec<Arc<FakeBackend>>>,
    shutdowns: Arc<AtomicUsize>,
}

impl FakeLauncher {
    pub fn launches(&self) -> Vec<String> {
        self.launches.lock().unwrap().clone()
    }
    pub fn shutdowns(&self) -> usize {
        self.shutdowns.load(Ordering::SeqCst)
    }
    pub fn kill_all(&self) {
        for backend in self.backends.lock().unwrap().iter() {
            backend.alive.store(false, Ordering::SeqCst);
        }
    }
    pub fn calls_for(&self, user: &str) -> Vec<String> {
        self.backends
            .lock()
            .unwrap()
            .iter()
            .filter(|backend| backend.user == user)
            .flat_map(|backend| backend.calls.lock().unwrap().clone())
            .collect()
    }
}

#[async_trait]
impl BrowserLauncher for FakeLauncher {
    async fn launch(&self, user_id: &str) -> Result<Arc<dyn BrowserBackend>> {
        self.launches.lock().unwrap().push(user_id.to_owned());
        let backend = Arc::new(FakeBackend {
            user: user_id.to_owned(),
            calls: Default::default(),
            alive: AtomicBool::new(true),
            shutdown_count: self.shutdowns.clone(),
            url: std::sync::Mutex::new("about:blank".into()),
        });
        self.backends.lock().unwrap().push(backend.clone());
        Ok(backend)
    }
}

pub(crate) fn manager_with(
    max_sessions: usize,
    idle: Duration,
) -> (Arc<BrowserManager>, Arc<FakeLauncher>) {
    let launcher = Arc::new(FakeLauncher::default());
    let manager = Arc::new(BrowserManager::new(launcher.clone(), max_sessions, idle));
    (manager, launcher)
}

/// Mock vault: `bank` is allowed on `*.bank.example` only.
#[derive(Default)]
pub(crate) struct FakeResolver {
    pub requests: std::sync::Mutex<Vec<(String, String, String, String)>>,
}

impl SecretResolver for FakeResolver {
    fn resolve(
        &self,
        user_id: &str,
        name: &str,
        field: &str,
        host: &str,
    ) -> Result<Zeroizing<String>> {
        self.requests.lock().unwrap().push((
            user_id.into(),
            name.into(),
            field.into(),
            host.into(),
        ));
        if name != "bank" {
            return Err(SeraError::NotFound("no such vault entry".into()));
        }
        if !(host == "bank.example" || host.ends_with(".bank.example")) {
            return Err(SeraError::Forbidden(
                "this vault entry is not allowed on this host".into(),
            ));
        }
        Ok(Zeroizing::new(FAKE_SECRET.to_owned()))
    }
}
