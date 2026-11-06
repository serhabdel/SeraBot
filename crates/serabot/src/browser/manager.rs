//! Per-user browser sessions: the backend/launcher seams, the controller
//! state machine (bot or user), capacity, idle reaping and secret redaction.
//!
//! Everything is keyed by the acting user's id. There is no API that takes a
//! user id from the model or from a request body.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use chrono::{DateTime, SecondsFormat, Utc};
use serde::Serialize;
use serde_json::{Value, json};
use tokio::sync::{Mutex, OwnedMutexGuard};
use zeroize::Zeroizing;

use super::outline::Outline;
use crate::error::{Result, SeraError};

pub const MAX_SECRETS_TRACKED: usize = 16;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PageInfo {
    pub url: String,
    pub title: String,
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
}

#[async_trait]
pub trait BrowserLauncher: Send + Sync {
    async fn launch(&self, user_id: &str) -> Result<Arc<dyn BrowserBackend>>;
}

/// Resolves a vault secret for typing. Production: `vault::resolve_for_browser`.
pub trait SecretResolver: Send + Sync {
    fn resolve(
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
}

impl BrowserInfo {
    fn inactive() -> Self {
        Self {
            active: false,
            url: None,
            title: None,
            controller: Controller::Bot,
            updated_at: Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true),
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
    sessions: Mutex<HashMap<String, Arc<Session>>>,
    launch_gate: Mutex<()>,
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
        }
    }

    async fn existing(&self, user_id: &str) -> Option<Arc<Session>> {
        self.sessions.lock().await.get(user_id).cloned()
    }

    async fn drop_dead(&self, user_id: &str, session: &Arc<Session>) {
        let mut map = self.sessions.lock().await;
        if map
            .get(user_id)
            .is_some_and(|current| Arc::ptr_eq(current, session))
        {
            map.remove(user_id);
        }
        drop(map);
        session.backend.shutdown().await;
    }

    async fn live_session(&self, user_id: &str) -> Option<Arc<Session>> {
        let session = self.existing(user_id).await?;
        if session.backend.is_alive().await {
            Some(session)
        } else {
            self.drop_dead(user_id, &session).await;
            None
        }
    }

    async fn launch(&self, user_id: &str) -> Result<Arc<Session>> {
        let _gate = self.launch_gate.lock().await;
        if let Some(session) = self.live_session(user_id).await {
            return Ok(session);
        }
        self.reap_idle().await;
        if self.sessions.lock().await.len() >= self.max_sessions {
            return Err(SeraError::Validation(
                "browser capacity reached; try again when another session has closed".into(),
            ));
        }
        let backend = self.launcher.launch(user_id).await?;
        let session = Arc::new(Session::new(backend));
        self.sessions
            .lock()
            .await
            .insert(user_id.to_owned(), session.clone());
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

    /// Lease for a bot tool. `launch` creates a session when none exists
    /// (navigate only); fails while the user holds control.
    pub async fn bot_lease(&self, user_id: &str, launch: bool) -> Result<Lease> {
        let session = match self.live_session(user_id).await {
            Some(session) => session,
            None if launch => self.launch(user_id).await?,
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
        Ok(lease)
    }

    /// Lease for owner input; only while the user holds control.
    pub async fn user_lease(&self, user_id: &str) -> Result<Lease> {
        let session = self
            .live_session(user_id)
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

    pub async fn info(&self, user_id: &str) -> BrowserInfo {
        let Some(session) = self.live_session(user_id).await else {
            return BrowserInfo::inactive();
        };
        let page = session.backend.info().await.unwrap_or_default();
        let (controller, updated_at) =
            session.with_state(|state| (state.controller, state.updated_at));
        BrowserInfo {
            active: true,
            url: Some(page.url),
            title: Some(page.title),
            controller,
            updated_at: updated_at.to_rfc3339_opts(SecondsFormat::Secs, true),
        }
    }

    pub async fn screenshot(&self, user_id: &str) -> Result<Vec<u8>> {
        let session = self
            .live_session(user_id)
            .await
            .ok_or_else(|| SeraError::NotFound("no browser session".into()))?;
        session.backend.screenshot_jpeg().await
    }

    pub async fn set_controller(&self, user_id: &str, controller: Controller) -> Result<()> {
        let session = self
            .live_session(user_id)
            .await
            .ok_or_else(|| SeraError::NotFound("no browser session".into()))?;
        session.with_state(|state| {
            state.controller = controller;
            state.last_used = Instant::now();
            state.updated_at = Utc::now();
        });
        Ok(())
    }

    /// Close the user's session. Returns whether one existed.
    pub async fn close(&self, user_id: &str) -> bool {
        let removed = self.sessions.lock().await.remove(user_id);
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
        let expired: Vec<(String, Arc<Session>)> = {
            let mut map = self.sessions.lock().await;
            let stale: Vec<String> = map
                .iter()
                .filter(|(_, session)| {
                    session.with_state(|state| state.last_used.elapsed() >= self.idle_timeout)
                })
                .map(|(user, _)| user.clone())
                .collect();
            stale
                .into_iter()
                .filter_map(|user| map.remove(&user).map(|session| (user, session)))
                .collect()
        };
        let count = expired.len();
        for (_, session) in expired {
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

    #[tokio::test]
    async fn bot_lease_requires_launch_and_is_per_user() {
        let (manager, launcher) = manager_with(4, Duration::from_secs(600));
        assert!(manager.bot_lease("alice", false).await.is_err());
        drop(manager.bot_lease("alice", true).await.unwrap());
        drop(manager.bot_lease("bob", true).await.unwrap());
        assert_eq!(launcher.launches(), vec!["alice", "bob"]);
        assert!(manager.info("alice").await.active);
        assert!(!manager.info("carol").await.active);
        assert!(manager.close("alice").await);
        assert!(!manager.info("alice").await.active);
        assert!(manager.info("bob").await.active);
    }

    #[tokio::test]
    async fn controller_state_machine() {
        let (manager, _) = manager_with(4, Duration::from_secs(600));
        assert!(
            manager
                .set_controller("alice", Controller::User)
                .await
                .is_err()
        );
        drop(manager.bot_lease("alice", true).await.unwrap());
        // User input is refused until takeover.
        assert!(manager.user_lease("alice").await.is_err());
        manager
            .set_controller("alice", Controller::User)
            .await
            .unwrap();
        assert_eq!(manager.info("alice").await.controller, Controller::User);
        let err = manager.bot_lease("alice", true).await.err().unwrap();
        assert!(err.to_string().contains("user is in control"), "{err}");
        drop(manager.user_lease("alice").await.unwrap());
        // Another user is unaffected.
        drop(manager.bot_lease("bob", true).await.unwrap());
        assert!(manager.user_lease("bob").await.is_err());
        manager
            .set_controller("alice", Controller::Bot)
            .await
            .unwrap();
        drop(manager.bot_lease("alice", false).await.unwrap());
        assert!(manager.user_lease("alice").await.is_err());
    }

    #[tokio::test]
    async fn capacity_and_idle_reaping() {
        let (manager, launcher) = manager_with(1, Duration::from_millis(50));
        drop(manager.bot_lease("alice", true).await.unwrap());
        let err = manager.bot_lease("bob", true).await.err().unwrap();
        assert!(err.to_string().contains("capacity"), "{err}");
        tokio::time::sleep(Duration::from_millis(80)).await;
        // The idle session is reaped to make room.
        drop(manager.bot_lease("bob", true).await.unwrap());
        assert_eq!(launcher.shutdowns(), 1);
        assert_eq!(manager.session_count().await, 1);
        tokio::time::sleep(Duration::from_millis(80)).await;
        assert_eq!(manager.reap_idle().await, 1);
        assert_eq!(manager.session_count().await, 0);
    }

    #[tokio::test]
    async fn dead_backend_is_replaced() {
        let (manager, launcher) = manager_with(2, Duration::from_secs(600));
        drop(manager.bot_lease("alice", true).await.unwrap());
        launcher.kill_all();
        assert!(!manager.info("alice").await.active);
        drop(manager.bot_lease("alice", true).await.unwrap());
        assert_eq!(launcher.launches().len(), 2);
    }

    #[tokio::test]
    async fn secrets_are_redacted_from_later_text() {
        let (manager, _) = manager_with(2, Duration::from_secs(600));
        let lease = manager.bot_lease("alice", true).await.unwrap();
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
