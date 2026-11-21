//! Fake backend/launcher/resolver for synthetic tests (no Chromium).

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use zeroize::Zeroizing;

use super::banners::BannerCandidate;
use super::manager::{
    BrowserBackend, BrowserLauncher, BrowserManager, Download, LoadState, PageInfo, Pointer,
    ScrollState, SecretResolver, TabInfo,
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
    /// Consent banner buttons the page shows; cleared when one is clicked.
    pub banners: std::sync::Mutex<Vec<BannerCandidate>>,
    /// Makes the next click/hover/type fail like a failed actionability check.
    pub fail_next: std::sync::Mutex<Option<String>>,
    /// Paths handed to `upload_ref`, to check they are deleted afterwards.
    pub uploaded: std::sync::Mutex<Vec<std::path::PathBuf>>,
    /// Optional override for `FakeBackend` download tests: `(filename, mime, bytes)`.
    pub fake_download: std::sync::Mutex<Option<(String, String, Vec<u8>)>>,
}

impl FakeBackend {
    fn take_failure(&self) -> Result<()> {
        match self.fail_next.lock().unwrap().take() {
            Some(message) => Err(SeraError::Validation(message)),
            None => Ok(()),
        }
    }

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
        outline.interactive = outline.text.clone();
        Ok((info, outline))
    }
    async fn click_ref(&self, reference: &str) -> Result<PageInfo> {
        self.record(format!("click {reference}"));
        self.take_failure()?;
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
    async fn history_step(&self, delta: i64) -> Result<PageInfo> {
        self.record(format!("history {delta}"));
        self.info().await
    }
    async fn reload(&self) -> Result<PageInfo> {
        self.record("reload");
        self.info().await
    }
    fn pointer(&self) -> Option<Pointer> {
        *self.pointer.lock().unwrap()
    }
    async fn page_text(&self, max_chars: usize) -> Result<String> {
        self.record(format!("page_text {max_chars}"));
        Ok("Readable article text".into())
    }
    async fn hover_ref(&self, reference: &str) -> Result<()> {
        self.record(format!("hover {reference}"));
        self.take_failure()
    }
    async fn select_ref(&self, reference: &str, option: &str) -> Result<String> {
        self.record(format!("select {reference} {option}"));
        self.take_failure()?;
        Ok(option.to_owned())
    }
    async fn set_checked(&self, reference: &str, checked: bool) -> Result<bool> {
        self.record(format!("check {reference} {checked}"));
        self.take_failure()?;
        Ok(checked)
    }
    async fn scroll_by(&self, reference: Option<&str>, dx: f64, dy: f64) -> Result<ScrollState> {
        self.record(format!(
            "scroll_by {} {dx} {dy}",
            reference.unwrap_or("page")
        ));
        Ok(ScrollState {
            x: dx,
            y: dy,
            height: 5000.0,
            view: 800.0,
        })
    }
    async fn scroll_to_ref(&self, reference: &str) -> Result<()> {
        self.record(format!("scroll_to {reference}"));
        Ok(())
    }
    async fn upload_ref(&self, reference: &str, path: &std::path::Path) -> Result<()> {
        let bytes = std::fs::read(path)
            .map_err(|_| SeraError::Validation("fake: upload file missing".into()))?;
        self.uploaded.lock().unwrap().push(path.to_path_buf());
        self.record(format!("upload {reference} {} bytes", bytes.len()));
        self.take_failure()
    }
    async fn wait_load_state(&self, state: LoadState, _timeout: Duration) -> Result<bool> {
        self.record(format!("wait_load {state:?}"));
        Ok(true)
    }
    async fn click_download(
        &self,
        reference: &str,
        _timeout: Duration,
        max_bytes: u64,
    ) -> Result<Download> {
        self.record(format!("download {reference}"));
        self.take_failure()?;
        let (filename, mime, bytes) =
            self.fake_download
                .lock()
                .unwrap()
                .clone()
                .unwrap_or_else(|| {
                    (
                        "report.csv".into(),
                        "text/csv".into(),
                        b"downloaded-bytes".to_vec(),
                    )
                });
        if bytes.len() as u64 > max_bytes {
            return Err(SeraError::Validation("the file is too large".into()));
        }
        Ok(Download {
            filename,
            mime,
            bytes,
        })
    }
    async fn download_url(
        &self,
        url: &str,
        _timeout: Duration,
        max_bytes: u64,
    ) -> Result<Download> {
        self.record(format!("download_url {url}"));
        self.take_failure()?;
        let (filename, mime, bytes) =
            self.fake_download
                .lock()
                .unwrap()
                .clone()
                .unwrap_or_else(|| {
                    (
                        "document.pdf".into(),
                        "application/pdf".into(),
                        b"%PDF-1.4 fake-url".to_vec(),
                    )
                });
        if bytes.len() as u64 > max_bytes {
            return Err(SeraError::Validation("the file is too large".into()));
        }
        Ok(Download {
            filename,
            mime,
            bytes,
        })
    }
    async fn download_current(&self, _timeout: Duration, max_bytes: u64) -> Result<Download> {
        let current = self.url();
        self.record(format!("download_current {current}"));
        self.take_failure()?;
        let (filename, mime, bytes) =
            self.fake_download
                .lock()
                .unwrap()
                .clone()
                .unwrap_or_else(|| {
                    (
                        "current.pdf".into(),
                        "application/pdf".into(),
                        b"%PDF-1.4 fake-current".to_vec(),
                    )
                });
        if bytes.len() as u64 > max_bytes {
            return Err(SeraError::Validation("the file is too large".into()));
        }
        Ok(Download {
            filename,
            mime,
            bytes,
        })
    }
    async fn print_pdf(&self, _max_bytes: usize) -> Result<Vec<u8>> {
        self.record("pdf");
        Ok(b"%PDF-1.4 fake".to_vec())
    }
    async fn banner_candidates(&self) -> Result<Vec<BannerCandidate>> {
        Ok(self.banners.lock().unwrap().clone())
    }
    async fn click_banner(&self, index: usize) -> Result<()> {
        self.record(format!("banner {index}"));
        self.banners.lock().unwrap().clear();
        Ok(())
    }
    fn set_block_heavy(&self, on: bool) {
        self.record(format!("block_heavy {on}"));
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
    /// Point every session of `user` at `url`, as a page that navigates by itself.
    pub fn navigate_user(&self, user: &str, url: &str) {
        for backend in self.backends.lock().unwrap().iter() {
            if backend.user == user {
                backend.set_url(url);
            }
        }
    }
    /// Give every session of `user` a consent banner with these buttons.
    pub fn set_banners(&self, user: &str, buttons: Vec<BannerCandidate>) {
        for backend in self.backends.lock().unwrap().iter() {
            if backend.user == user {
                *backend.banners.lock().unwrap() = buttons.clone();
            }
        }
    }
    /// Make the next click, hover, select, check or upload of `user` fail.
    pub fn fail_next(&self, user: &str, message: &str) {
        for backend in self.backends.lock().unwrap().iter() {
            if backend.user == user {
                *backend.fail_next.lock().unwrap() = Some(message.to_owned());
            }
        }
    }
    /// Set the next download response `(filename, mime, bytes)` for `user`.
    pub fn set_fake_download(&self, user: &str, filename: &str, mime: &str, bytes: Vec<u8>) {
        for backend in self.backends.lock().unwrap().iter() {
            if backend.user == user {
                *backend.fake_download.lock().unwrap() =
                    Some((filename.to_owned(), mime.to_owned(), bytes.clone()));
            }
        }
    }
    pub fn uploaded_paths(&self, user: &str) -> Vec<std::path::PathBuf> {
        self.backends
            .lock()
            .unwrap()
            .iter()
            .filter(|backend| backend.user == user)
            .flat_map(|backend| backend.uploaded.lock().unwrap().clone())
            .collect()
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
            banners: Default::default(),
            fail_next: Default::default(),
            uploaded: Default::default(),
            fake_download: Default::default(),
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
    /// Once set, the next resolve moves the user's pages to the URL after the
    /// host check: a page that navigates during a slow password manager read.
    pub moves_page: std::sync::Mutex<Option<(Arc<FakeLauncher>, String)>>,
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
        let moved = self.moves_page.lock().unwrap().take();
        if let Some((launcher, url)) = moved {
            launcher.navigate_user(user_id, &url);
        }
        Ok(Zeroizing::new(FAKE_SECRET.to_owned()))
    }
}

/// A scripted DevTools server for testing the real CDP code paths without
/// Chromium. `handler(method, params)` returns the result and any events to
/// emit before it (each `{"method","params","sessionId"?}`). Every call is
/// logged as `(method, params)`.
pub(crate) type CdpLog = Arc<std::sync::Mutex<Vec<(String, serde_json::Value)>>>;

pub(crate) async fn scripted_cdp<F>(mut handler: F) -> (Arc<super::cdp::CdpConnection>, CdpLog)
where
    F: FnMut(&str, &serde_json::Value) -> (serde_json::Value, Vec<serde_json::Value>)
        + Send
        + 'static,
{
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    fn frame(payload: &[u8]) -> Vec<u8> {
        let mut out = vec![0x81u8];
        if payload.len() < 126 {
            out.push(payload.len() as u8);
        } else if payload.len() <= u16::MAX as usize {
            out.push(126);
            out.extend_from_slice(&(payload.len() as u16).to_be_bytes());
        } else {
            out.push(127);
            out.extend_from_slice(&(payload.len() as u64).to_be_bytes());
        }
        out.extend_from_slice(payload);
        out
    }
    let (client, mut server) = tokio::io::duplex(1 << 20);
    let log: CdpLog = Arc::default();
    let server_log = log.clone();
    tokio::spawn(async move {
        let mut buf = [0u8; 2048];
        let _ = server.read(&mut buf).await;
        let _ = server
            .write_all(b"HTTP/1.1 101 Switching Protocols\r\n\r\n")
            .await;
        loop {
            let Ok(super::ws::Incoming::Text(text)) = super::ws::read_message(&mut server).await
            else {
                return;
            };
            let value: serde_json::Value = serde_json::from_str(&text).unwrap();
            let method = value["method"].as_str().unwrap_or_default().to_owned();
            server_log
                .lock()
                .unwrap()
                .push((method.clone(), value["params"].clone()));
            let (result, events) = handler(&method, &value["params"]);
            for event in events {
                let _ = server.write_all(&frame(event.to_string().as_bytes())).await;
            }
            let reply = if let Some(message) = result.get("__error").and_then(|m| m.as_str()) {
                serde_json::json!({"id": value["id"], "error": {"message": message}})
            } else {
                serde_json::json!({"id": value["id"], "result": result})
            };
            let _ = server.write_all(&frame(reply.to_string().as_bytes())).await;
        }
    });
    let cdp =
        super::cdp::CdpConnection::from_stream(client, "127.0.0.1:1", "/x", Duration::from_secs(2))
            .await
            .unwrap();
    (Arc::new(cdp), log)
}
