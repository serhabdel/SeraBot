//! Fake backend/launcher/resolver for synthetic tests (no Chromium).

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use zeroize::Zeroizing;

use super::manager::{
    BrowserBackend, BrowserLauncher, BrowserManager, PageInfo, Pointer, SecretResolver, TabInfo,
};
use super::outline::{Outline, RefTarget};
use crate::error::{Result, SeraError};

pub(crate) const FAKE_SECRET: &str = "s3cr3t-value-123";

/// Tabs as `(label, url)`, the active label, and the next tab number.
type TabState = (Vec<(String, String)>, String, u32);

pub(crate) struct FakeBackend {
    pub user: String,
    pub profile: String,
    pub calls: std::sync::Mutex<Vec<String>>,
    pub alive: AtomicBool,
    pub shutdown_count: Arc<AtomicUsize>,
    /// `(label, url)` per tab and the active label.
    tab_state: std::sync::Mutex<TabState>,
    pointer: std::sync::Mutex<Option<Pointer>>,
}

impl FakeBackend {
    fn url(&self) -> String {
        let state = self.tab_state.lock().unwrap();
        state
            .0
            .iter()
            .find(|(label, _)| *label == state.1)
            .map(|(_, url)| url.clone())
            .unwrap_or_default()
    }

    fn set_url(&self, url: &str) {
        let mut state = self.tab_state.lock().unwrap();
        let active = state.1.clone();
        if let Some(entry) = state.0.iter_mut().find(|(label, _)| *label == active) {
            entry.1 = url.to_owned();
        }
    }

    fn tab_list(&self) -> Vec<TabInfo> {
        let state = self.tab_state.lock().unwrap();
        state
            .0
            .iter()
            .map(|(label, url)| TabInfo {
                id: label.clone(),
                url: url.clone(),
                title: format!("title of {}", self.user),
                active: *label == state.1,
            })
            .collect()
    }
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
        self.set_url(url);
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
        let mut slot = self.pointer.lock().unwrap();
        let seq = slot.map_or(0, |p| p.seq) + 1;
        *slot = Some(Pointer {
            x,
            y,
            action: "click",
            seq,
            vw: 1280,
            vh: 800,
        });
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
        if self.profile != "default" {
            bytes.push(b'/');
            bytes.extend_from_slice(self.profile.as_bytes());
        }
        Ok(bytes)
    }
    async fn info(&self) -> Result<PageInfo> {
        Ok(PageInfo {
            url: self.url(),
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
    async fn tabs(&self) -> Result<Vec<TabInfo>> {
        Ok(self.tab_list())
    }
    async fn open_tab(&self, url: &str) -> Result<Vec<TabInfo>> {
        self.record(format!("open_tab {url}"));
        {
            let mut state = self.tab_state.lock().unwrap();
            if state.0.len() >= 8 {
                return Err(SeraError::Validation("at most 8 tabs".into()));
            }
            state.2 += 1;
            let label = format!("t{}", state.2);
            state.0.push((label.clone(), url.to_owned()));
            state.1 = label;
        }
        Ok(self.tab_list())
    }
    async fn switch_tab(&self, tab: &str) -> Result<()> {
        self.record(format!("switch_tab {tab}"));
        let mut state = self.tab_state.lock().unwrap();
        if !state.0.iter().any(|(label, _)| label == tab) {
            return Err(SeraError::Validation(format!("no tab {tab}")));
        }
        state.1 = tab.to_owned();
        Ok(())
    }
    async fn close_tab(&self, tab: &str) -> Result<()> {
        self.record(format!("close_tab {tab}"));
        let mut state = self.tab_state.lock().unwrap();
        if state.0.len() <= 1 {
            return Err(SeraError::Validation("that is the last tab".into()));
        }
        if !state.0.iter().any(|(label, _)| label == tab) {
            return Err(SeraError::Validation(format!("no tab {tab}")));
        }
        state.0.retain(|(label, _)| label != tab);
        if state.1 == tab {
            state.1 = state.0[0].0.clone();
        }
        Ok(())
    }
    fn pointer(&self) -> Option<Pointer> {
        *self.pointer.lock().unwrap()
    }
}

#[derive(Default)]
pub(crate) struct FakeLauncher {
    launches: std::sync::Mutex<Vec<String>>,
    profile_launches: std::sync::Mutex<Vec<(String, String)>>,
    backends: std::sync::Mutex<Vec<Arc<FakeBackend>>>,
    shutdowns: Arc<AtomicUsize>,
}

impl FakeLauncher {
    pub fn launches(&self) -> Vec<String> {
        self.launches.lock().unwrap().clone()
    }
    /// `(user, profile)` per launch, in order.
    pub fn profile_launches(&self) -> Vec<(String, String)> {
        self.profile_launches.lock().unwrap().clone()
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
    pub fn calls_for_profile(&self, user: &str, profile: &str) -> Vec<String> {
        self.backends
            .lock()
            .unwrap()
            .iter()
            .filter(|backend| backend.user == user && backend.profile == profile)
            .flat_map(|backend| backend.calls.lock().unwrap().clone())
            .collect()
    }
}

#[async_trait]
impl BrowserLauncher for FakeLauncher {
    async fn launch(&self, user_id: &str, profile_id: &str) -> Result<Arc<dyn BrowserBackend>> {
        self.launches.lock().unwrap().push(user_id.to_owned());
        self.profile_launches
            .lock()
            .unwrap()
            .push((user_id.to_owned(), profile_id.to_owned()));
        let backend = Arc::new(FakeBackend {
            user: user_id.to_owned(),
            profile: profile_id.to_owned(),
            calls: Default::default(),
            alive: AtomicBool::new(true),
            shutdown_count: self.shutdowns.clone(),
            tab_state: std::sync::Mutex::new((
                vec![("t1".into(), "about:blank".into())],
                "t1".into(),
                1,
            )),
            pointer: Default::default(),
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

/// Like [`manager_with`], with a profile base directory (for delete tests).
pub(crate) fn manager_with_base(
    max_sessions: usize,
    idle: Duration,
    base: std::path::PathBuf,
) -> (Arc<BrowserManager>, Arc<FakeLauncher>) {
    let launcher = Arc::new(FakeLauncher::default());
    let manager =
        Arc::new(BrowserManager::new(launcher.clone(), max_sessions, idle).with_base_dir(base));
    (manager, launcher)
}

/// Mock vault: `bank` is allowed on `*.bank.example` only.
#[derive(Default)]
pub(crate) struct FakeResolver {
    pub requests: std::sync::Mutex<Vec<(String, String, String, String)>>,
}

impl FakeResolver {
    fn check(&self, name: &str, host: &str) -> Result<()> {
        if name != "bank" {
            return Err(SeraError::NotFound("no such vault entry".into()));
        }
        if !(host == "bank.example" || host.ends_with(".bank.example")) {
            return Err(SeraError::Forbidden(
                "this vault entry is not allowed on this host".into(),
            ));
        }
        Ok(())
    }
}

#[async_trait]
impl SecretResolver for FakeResolver {
    fn authorize(&self, _user_id: &str, name: &str, _field: &str, host: &str) -> Result<()> {
        self.check(name, host)
    }

    async fn resolve(
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
        self.check(name, host)?;
        Ok(Zeroizing::new(FAKE_SECRET.to_owned()))
    }
}
