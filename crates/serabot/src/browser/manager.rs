//! Per-user browser sessions: the backend/launcher seams, the controller
//! state machine (bot or user), capacity, idle reaping and secret redaction.
//!
//! Everything is keyed by the acting user's id. There is no API that takes a
//! user id from the model or from a request body.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use chrono::{DateTime, SecondsFormat, Utc};
use serde::Serialize;
use serde_json::{Value, json};
use tokio::sync::{Mutex, OwnedMutexGuard};
use zeroize::Zeroizing;

use super::outline::Outline;
use super::signin::{SigninBlock, signin_block_hint};
use crate::error::{Result, SeraError};

pub const MAX_SECRETS_TRACKED: usize = 16;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PageInfo {
    pub url: String,
    pub title: String,
}

/// One tab of a session. `id` is a short stable label (`t1`, `t2`, ...).
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct TabInfo {
    pub id: String,
    pub url: String,
    pub title: String,
    pub active: bool,
}

/// Where the bot (or the user) last moved/clicked, in viewport pixels
/// (`vw` x `vh`). `seq` grows with every action so a viewer can tell a new
/// click from the same one it already drew.
#[derive(Debug, Clone, Copy, Serialize, PartialEq)]
pub struct Pointer {
    pub x: f64,
    pub y: f64,
    pub action: &'static str,
    pub seq: u64,
    pub vw: u32,
    pub vh: u32,
}

fn tabs_unsupported() -> SeraError {
    SeraError::Validation("this browser does not support tabs".into())
}

fn history_unsupported() -> SeraError {
    SeraError::Validation("this browser does not support back, forward or reload".into())
}

/// One live browser page. Implemented by the Chromium/CDP backend and by a
/// fake in tests.
#[async_trait]
pub trait BrowserBackend: Send + Sync {
    async fn navigate(&self, url: &str) -> Result<PageInfo>;
    /// Take a snapshot; the backend remembers its refs for `click_ref`/`type_ref`.
    async fn snapshot(&self) -> Result<(PageInfo, Outline)>;
    async fn click_ref(&self, reference: &str) -> Result<PageInfo>;
    async fn type_ref(&self, reference: &str, text: &str, submit: bool) -> Result<()>;
    async fn press(&self, key: &str) -> Result<()>;
    async fn click_at(&self, x: f64, y: f64) -> Result<()>;
    async fn insert_text(&self, text: &str) -> Result<()>;
    async fn scroll(&self, dy: f64) -> Result<()>;
    /// JPEG, at most 1280 px wide and 512 KiB.
    async fn screenshot_jpeg(&self) -> Result<Vec<u8>>;
    async fn info(&self) -> Result<PageInfo>;
    async fn is_alive(&self) -> bool;
    async fn shutdown(&self);
    /// Tabs of this browser, in opening order; empty when unsupported.
    async fn tabs(&self) -> Result<Vec<TabInfo>> {
        Ok(Vec::new())
    }
    /// Open `url` (already checked against the public-URL policy) in a new
    /// active tab.
    async fn open_tab(&self, _url: &str) -> Result<Vec<TabInfo>> {
        Err(tabs_unsupported())
    }
    async fn switch_tab(&self, _tab: &str) -> Result<()> {
        Err(tabs_unsupported())
    }
    /// The last tab cannot be closed; close the session instead.
    async fn close_tab(&self, _tab: &str) -> Result<()> {
        Err(tabs_unsupported())
    }
    /// Step through the active tab's history: -1 back, +1 forward. Fails when
    /// there is no entry that way. The caller checks the URL it lands on.
    async fn history_step(&self, _delta: i64) -> Result<PageInfo> {
        Err(history_unsupported())
    }
    async fn reload(&self) -> Result<PageInfo> {
        Err(history_unsupported())
    }
    /// The last pointer action, if any.
    fn pointer(&self) -> Option<Pointer> {
        None
    }
}

#[async_trait]
pub trait BrowserLauncher: Send + Sync {
    async fn launch(&self, user_id: &str, profile_id: &str) -> Result<Arc<dyn BrowserBackend>>;
}

/// Resolves a vault secret for typing. Production: `vault_providers::resolve_for_browser`.
#[async_trait]
pub trait SecretResolver: Send + Sync {
    /// Whether the entry may be typed into a page on `host`, without reading
    /// its value. Used to refuse before an approval is asked for.
    fn authorize(&self, user_id: &str, name: &str, field: &str, host: &str) -> Result<()>;

    /// The value to type. Fails closed on an unknown name or a host outside
    /// the entry's sites.
    async fn resolve(
        &self,
        user_id: &str,
        name: &str,
        field: &str,
        host: &str,
    ) -> Result<Zeroizing<String>>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Controller {
    Bot,
    User,
}

#[derive(Debug, Clone, Serialize)]
pub struct BrowserInfo {
    pub active: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    pub controller: Controller,
    pub updated_at: String,
    /// Open tabs (empty when the backend has none to report).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub tabs: Vec<TabInfo>,
    /// Last pointer action, for the live view overlay.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pointer: Option<Pointer>,
    /// Bot that last drove this session (sessions API only).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bot: Option<String>,
    /// Conversation of the last bot turn that used this session.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub conversation_id: Option<String>,
    /// Seconds since the last bot or owner action (not since the last poll).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub idle_secs: Option<u64>,
    /// Set while the active tab shows a sign-in refusal from a provider.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub signin_blocked: Option<SigninBlock>,
}

/// Which bot (and conversation) is driving a session.
#[derive(Debug, Clone, Copy)]
pub struct Driver<'a> {
    pub bot: &'a str,
    pub conversation_id: Option<&'a str>,
}

type SessionKey = (String, String);

fn key(user_id: &str, profile_id: &str) -> SessionKey {
    (user_id.to_owned(), profile_id.to_owned())
}

impl BrowserInfo {
    fn inactive() -> Self {
        Self {
            active: false,
            url: None,
            title: None,
            controller: Controller::Bot,
            updated_at: Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true),
            tabs: Vec::new(),
            pointer: None,
            bot: None,
            conversation_id: None,
            idle_secs: None,
            signin_blocked: None,
        }
    }

    pub fn to_json(&self) -> Value {
        json!(self)
    }
}

struct SessionState {
    controller: Controller,
    last_used: Instant,
    updated_at: DateTime<Utc>,
    secrets: Vec<(String, Zeroizing<String>)>,
    bot: Option<String>,
    conversation_id: Option<String>,
}

pub struct Session {
    backend: Arc<dyn BrowserBackend>,
    state: std::sync::Mutex<SessionState>,
    /// Serialises operations (bot tool calls and user input).
    op: Arc<Mutex<()>>,
}

impl Session {
    fn new(backend: Arc<dyn BrowserBackend>) -> Self {
        Self {
            backend,
            state: std::sync::Mutex::new(SessionState {
                controller: Controller::Bot,
                last_used: Instant::now(),
                updated_at: Utc::now(),
                secrets: Vec::new(),
                bot: None,
                conversation_id: None,
            }),
            op: Arc::new(Mutex::new(())),
        }
    }

    fn with_state<T>(&self, f: impl FnOnce(&mut SessionState) -> T) -> T {
        let mut guard = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        f(&mut guard)
    }

    fn touch(&self) {
        self.with_state(|state| {
            state.last_used = Instant::now();
            state.updated_at = Utc::now();
        });
    }

    fn controller(&self) -> Controller {
        self.with_state(|state| state.controller)
    }
}

/// Exclusive access to a session for one operation.
pub struct Lease {
    session: Arc<Session>,
    _guard: OwnedMutexGuard<()>,
}

impl Lease {
    pub fn backend(&self) -> &Arc<dyn BrowserBackend> {
        &self.session.backend
    }

    /// Remember a typed secret so it is scrubbed from later page text.
    pub fn remember_secret(&self, name: &str, value: &Zeroizing<String>) {
        if value.is_empty() {
            return;
        }
        self.session.with_state(|state| {
            state.secrets.retain(|(existing, _)| existing != name);
            if state.secrets.len() >= MAX_SECRETS_TRACKED {
                state.secrets.remove(0);
            }
            state
                .secrets
                .push((name.to_owned(), Zeroizing::new(value.to_string())));
        });
    }

    /// Replace every remembered secret value with `[vault:<name>]`.
    pub fn redact(&self, text: &str) -> String {
        self.session.with_state(|state| {
            let mut out = text.to_owned();
            for (name, value) in &state.secrets {
                if !value.is_empty() {
                    out = out.replace(value.as_str(), &format!("[vault:{name}]"));
                }
            }
            out
        })
    }
}

impl Drop for Lease {
    fn drop(&mut self) {
        self.session.touch();
    }
}

fn user_in_control() -> SeraError {
    SeraError::Validation(
        "the user is in control of the browser right now; wait for them to hand it back".into(),
    )
}

pub struct BrowserManager {
    launcher: Arc<dyn BrowserLauncher>,
    max_sessions: usize,
    idle_timeout: Duration,
    sessions: Mutex<HashMap<SessionKey, Arc<Session>>>,
    launch_gate: Mutex<()>,
    /// Base of `<user_id>/<profile_id>` Chrome directories, when known.
    base_dir: Option<PathBuf>,
}

impl BrowserManager {
    pub fn new(
        launcher: Arc<dyn BrowserLauncher>,
        max_sessions: usize,
        idle_timeout: Duration,
    ) -> Self {
        Self {
            launcher,
            max_sessions: max_sessions.max(1),
            idle_timeout,
            sessions: Mutex::new(HashMap::new()),
            launch_gate: Mutex::new(()),
            base_dir: None,
        }
    }

    /// Where profile directories live; needed to delete a profile's data.
    pub fn with_base_dir(mut self, base_dir: PathBuf) -> Self {
        self.base_dir = Some(base_dir);
        self
    }

    pub fn base_dir(&self) -> Option<&Path> {
        self.base_dir.as_deref()
    }

    async fn existing(&self, user_id: &str, profile_id: &str) -> Option<Arc<Session>> {
        self.sessions
            .lock()
            .await
            .get(&key(user_id, profile_id))
            .cloned()
    }

    async fn drop_dead(&self, user_id: &str, profile_id: &str, session: &Arc<Session>) {
        let mut map = self.sessions.lock().await;
        let k = key(user_id, profile_id);
        if map
            .get(&k)
            .is_some_and(|current| Arc::ptr_eq(current, session))
        {
            map.remove(&k);
        }
        drop(map);
        session.backend.shutdown().await;
    }

    async fn live_session(&self, user_id: &str, profile_id: &str) -> Option<Arc<Session>> {
        let session = self.existing(user_id, profile_id).await?;
        if session.backend.is_alive().await {
            Some(session)
        } else {
            self.drop_dead(user_id, profile_id, &session).await;
            None
        }
    }

    async fn launch(&self, user_id: &str, profile_id: &str) -> Result<Arc<Session>> {
        let _gate = self.launch_gate.lock().await;
        if let Some(session) = self.live_session(user_id, profile_id).await {
            return Ok(session);
        }
        self.reap_idle().await;
        if self.sessions.lock().await.len() >= self.max_sessions {
            return Err(SeraError::Validation(
                "browser capacity reached; try again when another session has closed".into(),
            ));
        }
        let backend = self.launcher.launch(user_id, profile_id).await?;
        let session = Arc::new(Session::new(backend));
        self.sessions
            .lock()
            .await
            .insert(key(user_id, profile_id), session.clone());
        Ok(session)
    }

    async fn lease(session: Arc<Session>) -> Lease {
        let guard = session.op.clone().lock_owned().await;
        session.touch();
        Lease {
            session,
            _guard: guard,
        }
    }

    /// Lease for a bot tool on one profile's session. `launch` creates a
    /// session when none exists (navigate only); fails while the user holds
    /// control. The driving bot and conversation are recorded on the session.
    pub async fn bot_lease(
        &self,
        user_id: &str,
        profile_id: &str,
        launch: bool,
        driver: Option<Driver<'_>>,
    ) -> Result<Lease> {
        let session = match self.live_session(user_id, profile_id).await {
            Some(session) => session,
            None if launch => self.launch(user_id, profile_id).await?,
            None => {
                return Err(SeraError::Validation(
                    "no browser session is open; call browser_navigate first".into(),
                ));
            }
        };
        if session.controller() == Controller::User {
            return Err(user_in_control());
        }
        let lease = Self::lease(session).await;
        // Control may have been taken while waiting for the operation lock.
        if lease.session.controller() == Controller::User {
            return Err(user_in_control());
        }
        if let Some(driver) = driver {
            lease.session.with_state(|state| {
                state.bot = Some(driver.bot.to_owned());
                state.conversation_id = driver.conversation_id.map(str::to_owned);
            });
        }
        Ok(lease)
    }

    /// Lease for owner input; only while the user holds control.
    pub async fn user_lease(&self, user_id: &str, profile_id: &str) -> Result<Lease> {
        let session = self
            .live_session(user_id, profile_id)
            .await
            .ok_or_else(|| SeraError::NotFound("no browser session".into()))?;
        let lease = Self::lease(session).await;
        if lease.session.controller() != Controller::User {
            return Err(SeraError::InvalidState(
                "take over the browser before sending input".into(),
            ));
        }
        Ok(lease)
    }

    async fn info_of(session: &Arc<Session>) -> BrowserInfo {
        let page = session.backend.info().await.unwrap_or_default();
        let (controller, updated_at, bot, conversation_id, idle) = session.with_state(|state| {
            (
                state.controller,
                state.updated_at,
                state.bot.clone(),
                state.conversation_id.clone(),
                state.last_used.elapsed().as_secs(),
            )
        });
        BrowserInfo {
            active: true,
            signin_blocked: signin_block_hint(&page.url),
            url: Some(page.url),
            title: Some(page.title),
            controller,
            updated_at: updated_at.to_rfc3339_opts(SecondsFormat::Secs, true),
            tabs: session.backend.tabs().await.unwrap_or_default(),
            pointer: session.backend.pointer(),
            bot,
            conversation_id,
            idle_secs: Some(idle),
        }
    }

    pub async fn info(&self, user_id: &str, profile_id: &str) -> BrowserInfo {
        match self.live_session(user_id, profile_id).await {
            Some(session) => Self::info_of(&session).await,
            None => BrowserInfo::inactive(),
        }
    }

    /// Live sessions of one user: `(profile id, info)`. Never lists another
    /// user's sessions.
    pub async fn user_sessions(&self, user_id: &str) -> Vec<(String, BrowserInfo)> {
        let profiles: Vec<String> = self
            .sessions
            .lock()
            .await
            .keys()
            .filter(|(user, _)| user == user_id)
            .map(|(_, profile)| profile.clone())
            .collect();
        let mut out = Vec::new();
        for profile in profiles {
            if let Some(session) = self.live_session(user_id, &profile).await {
                out.push((profile, Self::info_of(&session).await));
            }
        }
        out.sort_by(|a, b| a.0.cmp(&b.0));
        out
    }

    pub async fn screenshot(&self, user_id: &str, profile_id: &str) -> Result<Vec<u8>> {
        let session = self
            .live_session(user_id, profile_id)
            .await
            .ok_or_else(|| SeraError::NotFound("no browser session".into()))?;
        session.backend.screenshot_jpeg().await
    }

    pub async fn set_controller(
        &self,
        user_id: &str,
        profile_id: &str,
        controller: Controller,
    ) -> Result<()> {
        let session = self
            .live_session(user_id, profile_id)
            .await
            .ok_or_else(|| SeraError::NotFound("no browser session".into()))?;
        session.with_state(|state| {
            state.controller = controller;
            state.last_used = Instant::now();
            state.updated_at = Utc::now();
        });
        Ok(())
    }

    /// Close one profile's session. Returns whether one existed.
    pub async fn close(&self, user_id: &str, profile_id: &str) -> bool {
        let removed = self.sessions.lock().await.remove(&key(user_id, profile_id));
        match removed {
            Some(session) => {
                session.backend.shutdown().await;
                true
            }
            None => false,
        }
    }

    pub async fn close_all(&self) {
        let drained: Vec<_> = self.sessions.lock().await.drain().collect();
        for (_, session) in drained {
            session.backend.shutdown().await;
        }
    }

    /// Close sessions idle for longer than the configured timeout.
    pub async fn reap_idle(&self) -> usize {
        let expired: Vec<Arc<Session>> = {
            let mut map = self.sessions.lock().await;
            let stale: Vec<SessionKey> = map
                .iter()
                .filter(|(_, session)| {
                    session.with_state(|state| state.last_used.elapsed() >= self.idle_timeout)
                })
                .map(|(k, _)| k.clone())
                .collect();
            stale.into_iter().filter_map(|k| map.remove(&k)).collect()
        };
        let count = expired.len();
        for session in expired {
            session.backend.shutdown().await;
        }
        count
    }

    pub async fn session_count(&self) -> usize {
        self.sessions.lock().await.len()
    }

    /// Background reaper; stops when `shutdown` fires and closes everything.
    pub fn spawn_reaper(
        self: &Arc<Self>,
        shutdown: tokio_util::sync::CancellationToken,
    ) -> tokio::task::JoinHandle<()> {
        let manager = self.clone();
        tokio::spawn(async move {
            let period =
                Duration::from_secs(30).min(manager.idle_timeout.max(Duration::from_secs(1)));
            loop {
                tokio::select! {
                    _ = shutdown.cancelled() => break,
                    _ = tokio::time::sleep(period) => {
                        manager.reap_idle().await;
                    }
                }
            }
            manager.close_all().await;
        })
    }
}

#[cfg(test)]
mod tests {
    use super::super::testing::{FakeLauncher, manager_with};
    use super::*;

    const D: &str = "default";

    #[tokio::test]
    async fn bot_lease_requires_launch_and_is_per_user() {
        let (manager, launcher) = manager_with(4, Duration::from_secs(600));
        assert!(manager.bot_lease("alice", D, false, None).await.is_err());
        drop(manager.bot_lease("alice", D, true, None).await.unwrap());
        drop(manager.bot_lease("bob", D, true, None).await.unwrap());
        assert_eq!(launcher.launches(), vec!["alice", "bob"]);
        assert!(manager.info("alice", D).await.active);
        assert!(!manager.info("carol", D).await.active);
        assert!(manager.close("alice", D).await);
        assert!(!manager.info("alice", D).await.active);
        assert!(manager.info("bob", D).await.active);
    }

    #[tokio::test]
    async fn controller_state_machine() {
        let (manager, _) = manager_with(4, Duration::from_secs(600));
        assert!(
            manager
                .set_controller("alice", D, Controller::User)
                .await
                .is_err()
        );
        drop(manager.bot_lease("alice", D, true, None).await.unwrap());
        // User input is refused until takeover.
        assert!(manager.user_lease("alice", D).await.is_err());
        manager
            .set_controller("alice", D, Controller::User)
            .await
            .unwrap();
        assert_eq!(manager.info("alice", D).await.controller, Controller::User);
        let err = manager
            .bot_lease("alice", D, true, None)
            .await
            .err()
            .unwrap();
        assert!(err.to_string().contains("user is in control"), "{err}");
        drop(manager.user_lease("alice", D).await.unwrap());
        // Another user is unaffected.
        drop(manager.bot_lease("bob", D, true, None).await.unwrap());
        assert!(manager.user_lease("bob", D).await.is_err());
        manager
            .set_controller("alice", D, Controller::Bot)
            .await
            .unwrap();
        drop(manager.bot_lease("alice", D, false, None).await.unwrap());
        assert!(manager.user_lease("alice", D).await.is_err());
    }

    #[tokio::test]
    async fn capacity_and_idle_reaping() {
        let (manager, launcher) = manager_with(1, Duration::from_millis(50));
        drop(manager.bot_lease("alice", D, true, None).await.unwrap());
        let err = manager.bot_lease("bob", D, true, None).await.err().unwrap();
        assert!(err.to_string().contains("capacity"), "{err}");
        tokio::time::sleep(Duration::from_millis(80)).await;
        // The idle session is reaped to make room.
        drop(manager.bot_lease("bob", D, true, None).await.unwrap());
        assert_eq!(launcher.shutdowns(), 1);
        assert_eq!(manager.session_count().await, 1);
        tokio::time::sleep(Duration::from_millis(80)).await;
        assert_eq!(manager.reap_idle().await, 1);
        assert_eq!(manager.session_count().await, 0);
    }

    #[tokio::test]
    async fn dead_backend_is_replaced() {
        let (manager, launcher) = manager_with(2, Duration::from_secs(600));
        drop(manager.bot_lease("alice", D, true, None).await.unwrap());
        launcher.kill_all();
        assert!(!manager.info("alice", D).await.active);
        drop(manager.bot_lease("alice", D, true, None).await.unwrap());
        assert_eq!(launcher.launches().len(), 2);
    }

    #[tokio::test]
    async fn secrets_are_redacted_from_later_text() {
        let (manager, _) = manager_with(2, Duration::from_secs(600));
        let lease = manager.bot_lease("alice", D, true, None).await.unwrap();
        lease.remember_secret("bank", &Zeroizing::new("hunter2-pass".to_owned()));
        assert_eq!(
            lease.redact("field value=\"hunter2-pass\" and hunter2-pass"),
            "field value=\"[vault:bank]\" and [vault:bank]"
        );
        assert_eq!(lease.redact("nothing here"), "nothing here");
    }

    #[test]
    fn launcher_type_is_usable() {
        let _ = FakeLauncher::default();
    }
}
