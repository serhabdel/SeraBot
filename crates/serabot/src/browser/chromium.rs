//! Headless Chromium launcher and CDP-driven page backend.
//!
//! One Chromium process per active user, DevTools bound to 127.0.0.1 on an
//! OS-chosen port (`--remote-debugging-port=0`, discovered through the
//! `DevToolsActivePort` file in that user's profile directory). Every request
//! the page makes is paused through the Fetch domain and checked by the
//! navigation guard before it continues.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use base64::Engine;
use serde_json::{Value, json};
use tokio::process::{Child, Command};
use tokio::sync::Mutex;
use tokio::task::JoinHandle;

use super::cdp::{CdpConnection, CdpEvent};
use super::guard::allow_subrequest;
use super::manager::{BrowserBackend, BrowserLauncher, PageInfo, Pointer, TabInfo};
use super::outline::{Outline, build_outline, key_def};
use crate::config::BrowserConfig;
use crate::error::{Result, SeraError};

pub const VIEWPORT_WIDTH: u32 = 1280;
pub const VIEWPORT_HEIGHT: u32 = 800;
pub const MAX_SCREENSHOT_BYTES: usize = 512 * 1024;
const SNAPSHOT_CHARS: usize = 12_000;
const SNAPSHOT_REFS: usize = 200;
const VERDICT_TTL: Duration = Duration::from_secs(30);
/// Most tabs one session may hold.
pub const MAX_TABS: usize = 8;

/// One attached page.
#[derive(Debug, Clone)]
struct Tab {
    label: String,
    target_id: String,
    session_id: String,
}

/// The tabs of one browser and which one the tools act on.
#[derive(Debug, Default)]
struct TabSet {
    tabs: Vec<Tab>,
    active: String,
    next: u32,
}

impl TabSet {
    fn add(&mut self, target_id: String, session_id: String) -> String {
        self.next += 1;
        let label = format!("t{}", self.next);
        self.tabs.push(Tab {
            label: label.clone(),
            target_id,
            session_id,
        });
        self.active = label.clone();
        label
    }

    fn active_tab(&self) -> Option<&Tab> {
        self.tabs.iter().find(|tab| tab.label == self.active)
    }

    fn find(&self, label: &str) -> Option<&Tab> {
        self.tabs.iter().find(|tab| tab.label == label)
    }

    fn owns_session(&self, session_id: &str) -> bool {
        self.tabs.iter().any(|tab| tab.session_id == session_id)
    }

    fn owns_target(&self, target_id: &str) -> bool {
        self.tabs.iter().any(|tab| tab.target_id == target_id)
    }

    fn remove(&mut self, label: &str) {
        self.tabs.retain(|tab| tab.label != label);
        if self.active == label {
            self.active = self
                .tabs
                .first()
                .map(|tab| tab.label.clone())
                .unwrap_or_default();
        }
    }
}

type SharedTabs = Arc<std::sync::Mutex<TabSet>>;

fn lock_tabs(tabs: &SharedTabs) -> std::sync::MutexGuard<'_, TabSet> {
    tabs.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// What to do with a new page that a tab of ours did not open itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PopupDecision {
    /// Register it as a tab (it becomes the active one).
    Adopt,
    /// Close it; the reason is logged.
    Close(&'static str),
}

/// Adopt a new page only when a tab of this browser opened it (its
/// `openerId` is one of our targets) and the tab limit has room. Anything
/// else, including a page with no opener, is closed.
pub fn should_adopt(
    opener_id: Option<&str>,
    known_target_ids: &[&str],
    count: usize,
) -> PopupDecision {
    match opener_id {
        Some(opener) if known_target_ids.contains(&opener) => {
            if count >= MAX_TABS {
                PopupDecision::Close("tab limit reached")
            } else {
                PopupDecision::Adopt
            }
        }
        _ => PopupDecision::Close("not opened by a tab of this browser"),
    }
}

/// Chromium binaries to try, in order, when none is configured.
pub const CHROMIUM_CANDIDATES: &[&str] = &[
    "chromium",
    "chromium-browser",
    "/snap/bin/chromium",
    "google-chrome",
    "google-chrome-stable",
];

fn browser_error(message: impl Into<String>) -> SeraError {
    SeraError::External(format!("browser: {}", message.into()))
}

pub fn resolve_chromium_executable(configured: &str) -> Result<String> {
    if !configured.trim().is_empty() {
        return Ok(configured.trim().to_owned());
    }
    if let Ok(from_env) = std::env::var("CHROME_PATH")
        && !from_env.trim().is_empty()
    {
        return Ok(from_env.trim().to_owned());
    }
    for candidate in CHROMIUM_CANDIDATES {
        let found = if candidate.starts_with('/') {
            Path::new(candidate).exists()
        } else {
            which_available(candidate)
        };
        if found {
            return Ok((*candidate).to_owned());
        }
    }
    Err(SeraError::Config(
        "set browser.chromium_executable or install chromium (see docs/browser.md)".into(),
    ))
}

#[cfg(unix)]
fn which_available(name: &str) -> bool {
    std::process::Command::new("sh")
        .arg("-c")
        .arg(format!("command -v {name}"))
        .stdout(Stdio::null())
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

#[cfg(windows)]
fn which_available(name: &str) -> bool {
    std::process::Command::new("where")
        .arg(name)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

pub fn chromium_args(profile: &Path, no_sandbox: bool) -> Vec<String> {
    let mut args = vec![
        "--headless=new".to_owned(),
        "--remote-debugging-address=127.0.0.1".to_owned(),
        "--remote-debugging-port=0".to_owned(),
        format!("--user-data-dir={}", profile.display()),
        format!("--window-size={VIEWPORT_WIDTH},{VIEWPORT_HEIGHT}"),
        "--no-first-run".to_owned(),
        "--no-default-browser-check".to_owned(),
        "--disable-dev-shm-usage".to_owned(),
        "--disable-extensions".to_owned(),
        "--disable-sync".to_owned(),
        "--disable-background-networking".to_owned(),
        "--disable-component-update".to_owned(),
        "--disable-breakpad".to_owned(),
        "--mute-audio".to_owned(),
        "--password-store=basic".to_owned(),
        "--disable-features=Translate,MediaRouter,OptimizationHints".to_owned(),
        "about:blank".to_owned(),
    ];
    if no_sandbox {
        args.insert(1, "--no-sandbox".to_owned());
    }
    args
}

/// Contents of `DevToolsActivePort`: line 1 port, line 2 browser ws path.
pub fn parse_devtools_active_port(contents: &str) -> Option<(u16, String)> {
    let mut lines = contents.lines();
    let port: u16 = lines.next()?.trim().parse().ok()?;
    let path = lines.next()?.trim().to_owned();
    (port >= 1024 && path.starts_with('/')).then_some((port, path))
}

pub struct ChromiumLauncher {
    executable: String,
    base_dir: PathBuf,
    no_sandbox: bool,
    timeout: Duration,
}

impl ChromiumLauncher {
    /// Fails when no Chromium can be found, so a misconfigured server stops at startup.
    pub fn new(config: &BrowserConfig, base_dir: PathBuf) -> Result<Self> {
        Ok(Self {
            executable: resolve_chromium_executable(&config.chromium_executable)?,
            base_dir,
            no_sandbox: config.no_sandbox,
            timeout: Duration::from_secs(config.request_timeout_secs.clamp(5, 120)),
        })
    }
}

#[async_trait]
impl BrowserLauncher for ChromiumLauncher {
    async fn launch(&self, user_id: &str, profile_id: &str) -> Result<Arc<dyn BrowserBackend>> {
        let profile = crate::browser_profiles::profile_dir(&self.base_dir, user_id, profile_id)?;
        std::fs::create_dir_all(&profile)?;
        let port_file = profile.join("DevToolsActivePort");
        let _ = std::fs::remove_file(&port_file);
        let mut child = Command::new(&self.executable)
            .args(chromium_args(&profile, self.no_sandbox))
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .map_err(|error| {
                browser_error(format!(
                    "failed to start '{}': {error}; see docs/browser.md",
                    self.executable
                ))
            })?;
        let started = Instant::now();
        let (port, path) = loop {
            if let Some(status) = child.try_wait()? {
                return Err(browser_error(format!(
                    "Chromium exited during startup ({status}); with a root service user set browser.no_sandbox or run as a normal user"
                )));
            }
            if let Ok(contents) = std::fs::read_to_string(&port_file)
                && let Some(found) = parse_devtools_active_port(&contents)
            {
                break found;
            }
            if started.elapsed() > Duration::from_secs(20) {
                let _ = child.kill().await;
                return Err(browser_error("Chromium did not expose DevTools in time"));
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        };
        let outline: SharedOutline = Arc::default();
        match ChromiumBackend::setup(port, &path, self.timeout, outline.clone()).await {
            Ok((cdp, tabs, tasks)) => Ok(Arc::new(ChromiumBackend {
                cdp,
                tabs,
                child: Mutex::new(Some(child)),
                outline,
                tasks: std::sync::Mutex::new(tasks),
                pointer: std::sync::Mutex::new(None),
                pointer_seq: AtomicU64::new(0),
            })),
            Err(error) => {
                let _ = child.kill().await;
                Err(error)
            }
        }
    }
}

/// Refs of the active page. Shared with the event loop, which clears them
/// when it makes an adopted popup the active tab.
type SharedOutline = Arc<std::sync::Mutex<Outline>>;

pub struct ChromiumBackend {
    cdp: Arc<CdpConnection>,
    tabs: SharedTabs,
    child: Mutex<Option<Child>>,
    outline: SharedOutline,
    tasks: std::sync::Mutex<Vec<JoinHandle<()>>>,
    pointer: std::sync::Mutex<Option<Pointer>>,
    pointer_seq: AtomicU64,
}

impl ChromiumBackend {
    async fn setup(
        port: u16,
        path: &str,
        timeout: Duration,
        outline: SharedOutline,
    ) -> Result<(Arc<CdpConnection>, SharedTabs, Vec<JoinHandle<()>>)> {
        let cdp = Arc::new(CdpConnection::connect(port, path, timeout).await?);
        let targets = cdp.call(None, "Target.getTargets", json!({})).await?;
        let existing = targets["targetInfos"]
            .as_array()
            .and_then(|infos| infos.iter().find(|info| info["type"] == "page"))
            .and_then(|info| info["targetId"].as_str())
            .map(str::to_owned);
        let target_id = match existing {
            Some(id) => id,
            None => cdp
                .call(None, "Target.createTarget", json!({"url": "about:blank"}))
                .await?["targetId"]
                .as_str()
                .ok_or_else(|| browser_error("no page target"))?
                .to_owned(),
        };
        let tabs: SharedTabs = Arc::default();
        // Subscribe before enabling Fetch so no paused request is missed.
        let events = spawn_event_loop(cdp.clone(), tabs.clone(), outline);
        if let Err(error) = Self::attach_tab(&cdp, &tabs, &target_id).await {
            events.abort();
            return Err(error);
        }
        // From now on every page the browser creates (popups, target=_blank,
        // open_tab) attaches paused. `adopt_child` guards or closes it before
        // it runs. Our own tab is attached again here; that copy is dropped.
        if let Err(error) = cdp
            .call(
                None,
                "Target.setAutoAttach",
                json!({"autoAttach": true, "waitForDebuggerOnStart": true, "flatten": true}),
            )
            .await
        {
            events.abort();
            return Err(error);
        }
        Ok((cdp, tabs, vec![events]))
    }

    /// Attach to a page, register it (so its requests are guarded from the
    /// first one) and configure it. Returns the tab label.
    async fn attach_tab(
        cdp: &Arc<CdpConnection>,
        tabs: &SharedTabs,
        target_id: &str,
    ) -> Result<String> {
        let attached = cdp
            .call(
                None,
                "Target.attachToTarget",
                json!({"targetId": target_id, "flatten": true}),
            )
            .await?;
        let session_id = attached["sessionId"]
            .as_str()
            .ok_or_else(|| browser_error("attach returned no session"))?
            .to_owned();
        let previous = lock_tabs(tabs).active.clone();
        let label = lock_tabs(tabs).add(target_id.to_owned(), session_id.clone());
        if let Err(error) = Self::configure(cdp, &session_id).await {
            let mut set = lock_tabs(tabs);
            set.remove(&label);
            if set.tabs.iter().any(|tab| tab.label == previous) {
                set.active = previous;
            }
            return Err(error);
        }
        Ok(label)
    }

    async fn configure(cdp: &Arc<CdpConnection>, session_id: &str) -> Result<()> {
        let sid = Some(session_id);
        cdp.call(None, "Target.setDiscoverTargets", json!({"discover": true}))
            .await?;
        cdp.call(
            None,
            "Browser.setDownloadBehavior",
            json!({"behavior": "deny"}),
        )
        .await?;
        for method in ["Page.enable", "DOM.enable", "Accessibility.enable"] {
            cdp.call(sid, method, json!({})).await?;
        }
        cdp.call(
            sid,
            "Emulation.setDeviceMetricsOverride",
            json!({"width": VIEWPORT_WIDTH, "height": VIEWPORT_HEIGHT, "deviceScaleFactor": 1, "mobile": false}),
        )
        .await?;
        cdp.call(
            sid,
            "Fetch.enable",
            json!({"patterns": [{"urlPattern": "*"}]}),
        )
        .await?;
        Ok(())
    }

    fn active(&self) -> Result<Tab> {
        lock_tabs(&self.tabs)
            .active_tab()
            .cloned()
            .ok_or_else(|| browser_error("the browser has no open tab"))
    }

    fn sid(&self) -> String {
        self.active().map(|tab| tab.session_id).unwrap_or_default()
    }

    async fn call(&self, method: &str, params: Value) -> Result<Value> {
        let tab = self.active()?;
        self.cdp
            .call(Some(tab.session_id.as_str()), method, params)
            .await
    }

    fn set_pointer(&self, x: f64, y: f64, action: &'static str) {
        let seq = self.pointer_seq.fetch_add(1, Ordering::SeqCst) + 1;
        if let Ok(mut slot) = self.pointer.lock() {
            *slot = Some(Pointer {
                x,
                y,
                action,
                seq,
                vw: VIEWPORT_WIDTH,
                vh: VIEWPORT_HEIGHT,
            });
        }
    }

    async fn click_node(&self, backend_node_id: i64) -> Result<()> {
        let _ = self
            .call(
                "DOM.scrollIntoViewIfNeeded",
                json!({"backendNodeId": backend_node_id}),
            )
            .await;
        let model = self
            .call("DOM.getBoxModel", json!({"backendNodeId": backend_node_id}))
            .await
            .map_err(|_| browser_error("element is not visible; take a new browser_snapshot"))?;
        let quad = model["model"]["content"]
            .as_array()
            .filter(|quad| quad.len() == 8)
            .ok_or_else(|| browser_error("element has no box"))?;
        let coords: Vec<f64> = quad.iter().filter_map(Value::as_f64).collect();
        if coords.len() != 8 {
            return Err(browser_error("element has no box"));
        }
        let x = (coords[0] + coords[2] + coords[4] + coords[6]) / 4.0;
        let y = (coords[1] + coords[3] + coords[5] + coords[7]) / 4.0;
        self.click_at(x, y).await
    }

    /// Refs belong to the page they were taken on.
    fn reset_outline(&self) -> Result<()> {
        *self
            .outline
            .lock()
            .map_err(|_| browser_error("outline lock poisoned"))? = Outline::default();
        Ok(())
    }

    async fn settle(&self) {
        tokio::time::sleep(Duration::from_millis(400)).await;
    }

    fn target_backend_id(&self, reference: &str) -> Result<i64> {
        let outline = self
            .outline
            .lock()
            .map_err(|_| browser_error("outline lock poisoned"))?;
        Ok(outline.target(reference)?.backend_node_id)
    }
}

/// Wait (bounded) for the page session's load event.
async fn wait_for_load(
    mut events: tokio::sync::broadcast::Receiver<CdpEvent>,
    session_id: String,
    limit: Duration,
) {
    let wait = async {
        while let Ok(event) = events.recv().await {
            if event.method == "Page.loadEventFired"
                && event.session_id.as_deref() == Some(session_id.as_str())
            {
                break;
            }
        }
    };
    let _ = tokio::time::timeout(limit, wait).await;
}

/// Handles Fetch pauses (guard), popups and JS dialogs for one page session.
fn spawn_event_loop(
    cdp: Arc<CdpConnection>,
    tabs: SharedTabs,
    outline: SharedOutline,
) -> JoinHandle<()> {
    let mut events = cdp.subscribe();
    let verdicts: Arc<std::sync::Mutex<HashMap<String, (Instant, bool)>>> = Arc::default();
    tokio::spawn(async move {
        loop {
            let event = match events.recv().await {
                Ok(event) => event,
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                Err(_) => break,
            };
            handle_event(&cdp, &tabs, &outline, &verdicts, event);
        }
    })
}

/// A page one of our tabs opened (or a page nobody should have opened) has
/// attached paused. Adopt it as a tab or close it; never leave it paused.
async fn adopt_child(
    cdp: &Arc<CdpConnection>,
    tabs: &SharedTabs,
    outline: &SharedOutline,
    child_session: &str,
    info: &Value,
    waiting: bool,
) {
    let target_id = info["targetId"].as_str().unwrap_or_default().to_owned();
    // Only a session the browser paused is ours to decide. Anything else is
    // an attach we made ourselves, which must be left alone.
    if !waiting || child_session.is_empty() || target_id.is_empty() {
        return;
    }
    // The session of one of our own tabs (the browser reports the attach we
    // made ourselves): it is not ours to detach.
    if lock_tabs(tabs).owns_session(child_session) {
        return;
    }
    // Workers and extension pages run as they would without auto-attach, and
    // a second session to one of our own tabs is not needed: let go of both.
    if info["type"] != "page" || lock_tabs(tabs).owns_target(&target_id) {
        let _ = cdp
            .call(
                Some(child_session),
                "Runtime.runIfWaitingForDebugger",
                json!({}),
            )
            .await;
        let _ = cdp
            .call(
                None,
                "Target.detachFromTarget",
                json!({"sessionId": child_session}),
            )
            .await;
        return;
    }
    // Decide and register under one lock, so the tab limit holds.
    let adopted = {
        let mut set = lock_tabs(tabs);
        let known: Vec<String> = set.tabs.iter().map(|tab| tab.target_id.clone()).collect();
        let known: Vec<&str> = known.iter().map(String::as_str).collect();
        let count = set.tabs.len();
        match should_adopt(info["openerId"].as_str(), &known, count) {
            PopupDecision::Adopt => Some(set.add(target_id.clone(), child_session.to_owned())),
            PopupDecision::Close(reason) => {
                tracing::warn!(reason, "browser: closed a new page");
                None
            }
        }
    };
    let Some(label) = adopted else {
        if info["openerId"].is_string() {
            // A popup some other page opened: not ours.
            let _ = cdp
                .call(None, "Target.closeTarget", json!({"targetId": target_id}))
                .await;
        } else {
            // A page with no opener is not a popup. Our own tabs and the
            // browser's pages land here; the target-created check closes any
            // unrelated page, so only let this copy go.
            let _ = cdp
                .call(
                    Some(child_session),
                    "Runtime.runIfWaitingForDebugger",
                    json!({}),
                )
                .await;
            let _ = cdp
                .call(
                    None,
                    "Target.detachFromTarget",
                    json!({"sessionId": child_session}),
                )
                .await;
        }
        return;
    };
    // Fetch (the guard) is on before the page is allowed to run. Requests the
    // page had already issued are not covered; see docs/browser.md.
    if ChromiumBackend::configure(cdp, child_session)
        .await
        .is_err()
    {
        lock_tabs(tabs).remove(&label);
        let _ = cdp
            .call(None, "Target.closeTarget", json!({"targetId": target_id}))
            .await;
        return;
    }
    let _ = cdp
        .call(
            Some(child_session),
            "Runtime.runIfWaitingForDebugger",
            json!({}),
        )
        .await;
    *outline
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = Outline::default();
    let _ = cdp
        .call(
            None,
            "Target.activateTarget",
            json!({"targetId": target_id}),
        )
        .await;
}

fn handle_event(
    cdp: &Arc<CdpConnection>,
    tabs: &SharedTabs,
    outline: &SharedOutline,
    verdicts: &Arc<std::sync::Mutex<HashMap<String, (Instant, bool)>>>,
    event: CdpEvent,
) {
    let known_session = event
        .session_id
        .as_deref()
        .filter(|sid| lock_tabs(tabs).owns_session(sid))
        .map(str::to_owned);
    match event.method.as_str() {
        "Fetch.requestPaused" if known_session.is_some() => {
            let cdp = cdp.clone();
            let session_id = known_session.unwrap_or_default();
            let verdicts = verdicts.clone();
            tokio::spawn(async move {
                let request_id = event.params["requestId"]
                    .as_str()
                    .unwrap_or_default()
                    .to_owned();
                let url = event.params["request"]["url"].as_str().unwrap_or_default();
                let allowed = cached_verdict(&verdicts, url).await;
                let (method, params) = if allowed {
                    ("Fetch.continueRequest", json!({"requestId": request_id}))
                } else {
                    (
                        "Fetch.failRequest",
                        json!({"requestId": request_id, "errorReason": "AccessDenied"}),
                    )
                };
                let _ = cdp.call(Some(&session_id), method, params).await;
            });
        }
        "Target.attachedToTarget" => {
            // A page the browser created, attached paused (see `setup`). Adopt
            // it, or let go of a copy we do not need.
            let cdp = cdp.clone();
            let tabs = tabs.clone();
            let outline = outline.clone();
            let child = event.params["sessionId"]
                .as_str()
                .unwrap_or_default()
                .to_owned();
            let info = event.params["targetInfo"].clone();
            let waiting = event.params["waitingForDebugger"]
                .as_bool()
                .unwrap_or(false);
            tokio::spawn(async move {
                adopt_child(&cdp, &tabs, &outline, &child, &info, waiting).await;
            });
        }
        "Target.targetCreated" => {
            let info = &event.params["targetInfo"];
            if info["type"] == "page"
                && let Some(id) = info["targetId"].as_str()
            {
                // Safety net for pages nobody adopted: close them unless a tab
                // of ours registered the target (see `adopt_child`) first.
                let cdp = cdp.clone();
                let tabs = tabs.clone();
                let id = id.to_owned();
                tokio::spawn(async move {
                    tokio::time::sleep(Duration::from_millis(600)).await;
                    if !lock_tabs(&tabs).owns_target(&id) {
                        let _ = cdp
                            .call(None, "Target.closeTarget", json!({"targetId": id}))
                            .await;
                    }
                });
            }
        }
        "Page.javascriptDialogOpening" if known_session.is_some() => {
            let cdp = cdp.clone();
            let session_id = known_session.unwrap_or_default();
            tokio::spawn(async move {
                let _ = cdp
                    .call(
                        Some(&session_id),
                        "Page.handleJavaScriptDialog",
                        json!({"accept": false}),
                    )
                    .await;
            });
        }
        _ => {}
    }
}

async fn cached_verdict(
    verdicts: &Arc<std::sync::Mutex<HashMap<String, (Instant, bool)>>>,
    url: &str,
) -> bool {
    let key = url::Url::parse(url)
        .ok()
        .and_then(|parsed| {
            parsed.host_str().map(|host| {
                format!(
                    "{}://{}:{}",
                    parsed.scheme(),
                    host,
                    parsed.port_or_known_default().unwrap_or(0)
                )
            })
        })
        .unwrap_or_else(|| url.chars().take(16).collect());
    let inline = url.starts_with("data:") || url.starts_with("blob:") || url == "about:blank";
    if !inline
        && let Ok(map) = verdicts.lock()
        && let Some((at, allowed)) = map.get(&key)
        && at.elapsed() < VERDICT_TTL
    {
        return *allowed;
    }
    let allowed = allow_subrequest(url).await;
    if !inline && let Ok(mut map) = verdicts.lock() {
        if map.len() > 512 {
            map.clear();
        }
        map.insert(key, (Instant::now(), allowed));
    }
    allowed
}

#[async_trait]
impl BrowserBackend for ChromiumBackend {
    async fn navigate(&self, url: &str) -> Result<PageInfo> {
        let events = self.cdp.subscribe();
        let wanted = self.sid();
        let result = self.call("Page.navigate", json!({"url": url})).await?;
        if let Some(error) = result["errorText"].as_str() {
            return Err(browser_error(format!("navigation failed: {error}")));
        }
        wait_for_load(events, wanted, Duration::from_secs(20)).await;
        self.info().await
    }

    async fn history_step(&self, delta: i64) -> Result<PageInfo> {
        let history = self.call("Page.getNavigationHistory", json!({})).await?;
        let current = history["currentIndex"].as_i64().unwrap_or(-1);
        let entry = usize::try_from(current + delta)
            .ok()
            .and_then(|index| history["entries"].as_array()?.get(index).cloned())
            .ok_or_else(|| SeraError::Validation("there is no page in that direction".into()))?;
        let entry_id = entry["id"]
            .as_i64()
            .ok_or_else(|| browser_error("history entry has no id"))?;
        let events = self.cdp.subscribe();
        let wanted = self.sid();
        self.call("Page.navigateToHistoryEntry", json!({"entryId": entry_id}))
            .await?;
        self.reset_outline()?;
        // A page restored from the back/forward cache fires no load event.
        wait_for_load(events, wanted, Duration::from_secs(8)).await;
        self.info().await
    }

    async fn reload(&self) -> Result<PageInfo> {
        let events = self.cdp.subscribe();
        let wanted = self.sid();
        self.call("Page.reload", json!({})).await?;
        self.reset_outline()?;
        wait_for_load(events, wanted, Duration::from_secs(20)).await;
        self.info().await
    }

    async fn snapshot(&self) -> Result<(PageInfo, Outline)> {
        let tree = self.call("Accessibility.getFullAXTree", json!({})).await?;
        let nodes = tree["nodes"].as_array().cloned().unwrap_or_default();
        let outline = build_outline(&nodes, SNAPSHOT_CHARS, SNAPSHOT_REFS);
        *self
            .outline
            .lock()
            .map_err(|_| browser_error("outline lock poisoned"))? = outline.clone();
        Ok((self.info().await?, outline))
    }

    async fn click_ref(&self, reference: &str) -> Result<PageInfo> {
        let id = self.target_backend_id(reference)?;
        self.click_node(id).await?;
        self.settle().await;
        self.info().await
    }

    async fn type_ref(&self, reference: &str, text: &str, submit: bool) -> Result<()> {
        let id = self.target_backend_id(reference)?;
        self.call("DOM.focus", json!({"backendNodeId": id}))
            .await
            .map_err(|_| browser_error("element cannot take focus; take a new browser_snapshot"))?;
        // Select any existing text so the insert replaces it.
        if let Ok(node) = self
            .call("DOM.resolveNode", json!({"backendNodeId": id}))
            .await
            && let Some(object_id) = node["object"]["objectId"].as_str()
        {
            let _ = self
                .call(
                    "Runtime.callFunctionOn",
                    json!({"objectId": object_id, "functionDeclaration": "function(){ if (this.select) this.select(); }"}),
                )
                .await;
        }
        self.call("Input.insertText", json!({"text": text})).await?;
        if submit {
            self.press("Enter").await?;
            self.settle().await;
        }
        Ok(())
    }

    async fn press(&self, key: &str) -> Result<()> {
        let def = key_def(key)
            .ok_or_else(|| SeraError::Validation(format!("unsupported key '{key}'")))?;
        let mut down = json!({
            "type": if def.text.is_some() { "keyDown" } else { "rawKeyDown" },
            "key": def.key,
            "code": def.code,
            "windowsVirtualKeyCode": def.windows_virtual_key_code,
        });
        if let Some(text) = &def.text {
            down["text"] = json!(text);
        }
        self.call("Input.dispatchKeyEvent", down).await?;
        self.call(
            "Input.dispatchKeyEvent",
            json!({"type": "keyUp", "key": def.key, "code": def.code, "windowsVirtualKeyCode": def.windows_virtual_key_code}),
        )
        .await?;
        Ok(())
    }

    async fn click_at(&self, x: f64, y: f64) -> Result<()> {
        self.set_pointer(x, y, "click");
        for (kind, extra) in [("mouseMoved", 0), ("mousePressed", 1), ("mouseReleased", 1)] {
            self.call(
                "Input.dispatchMouseEvent",
                json!({"type": kind, "x": x, "y": y, "button": if extra == 1 { "left" } else { "none" }, "clickCount": extra}),
            )
            .await?;
        }
        Ok(())
    }

    async fn insert_text(&self, text: &str) -> Result<()> {
        self.call("Input.insertText", json!({"text": text})).await?;
        Ok(())
    }

    async fn scroll(&self, dy: f64) -> Result<()> {
        self.set_pointer(
            f64::from(VIEWPORT_WIDTH / 2),
            f64::from(VIEWPORT_HEIGHT / 2),
            "scroll",
        );
        self.call(
            "Input.dispatchMouseEvent",
            json!({"type": "mouseWheel", "x": VIEWPORT_WIDTH / 2, "y": VIEWPORT_HEIGHT / 2, "deltaX": 0, "deltaY": dy}),
        )
        .await?;
        Ok(())
    }

    async fn screenshot_jpeg(&self) -> Result<Vec<u8>> {
        for quality in [70u32, 50, 35, 20] {
            let shot = self
                .call(
                    "Page.captureScreenshot",
                    json!({"format": "jpeg", "quality": quality}),
                )
                .await?;
            let data = shot["data"]
                .as_str()
                .ok_or_else(|| browser_error("screenshot returned no data"))?;
            let bytes = base64::engine::general_purpose::STANDARD
                .decode(data)
                .map_err(|_| browser_error("screenshot was not valid base64"))?;
            if bytes.len() <= MAX_SCREENSHOT_BYTES {
                return Ok(bytes);
            }
        }
        Err(browser_error("screenshot exceeds the 512 KiB limit"))
    }

    async fn info(&self) -> Result<PageInfo> {
        let info = self
            .cdp
            .call(
                None,
                "Target.getTargetInfo",
                json!({"targetId": self.active()?.target_id}),
            )
            .await?;
        Ok(PageInfo {
            url: info["targetInfo"]["url"]
                .as_str()
                .unwrap_or_default()
                .to_owned(),
            title: info["targetInfo"]["title"]
                .as_str()
                .unwrap_or_default()
                .to_owned(),
        })
    }

    fn pointer(&self) -> Option<Pointer> {
        self.pointer.lock().ok().and_then(|slot| *slot)
    }

    async fn tabs(&self) -> Result<Vec<TabInfo>> {
        let targets = self.cdp.call(None, "Target.getTargets", json!({})).await?;
        let infos = targets["targetInfos"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        let mut set = lock_tabs(&self.tabs);
        // Forget tabs the page closed itself.
        let gone: Vec<String> = set
            .tabs
            .iter()
            .filter(|tab| {
                !infos
                    .iter()
                    .any(|info| info["targetId"].as_str() == Some(tab.target_id.as_str()))
            })
            .map(|tab| tab.label.clone())
            .collect();
        for label in gone {
            set.remove(&label);
        }
        Ok(set
            .tabs
            .iter()
            .map(|tab| {
                let info = infos
                    .iter()
                    .find(|info| info["targetId"].as_str() == Some(tab.target_id.as_str()));
                TabInfo {
                    id: tab.label.clone(),
                    url: info
                        .and_then(|i| i["url"].as_str())
                        .unwrap_or_default()
                        .to_owned(),
                    title: info
                        .and_then(|i| i["title"].as_str())
                        .unwrap_or_default()
                        .to_owned(),
                    active: tab.label == set.active,
                }
            })
            .collect())
    }

    async fn open_tab(&self, url: &str) -> Result<Vec<TabInfo>> {
        let before = lock_tabs(&self.tabs).tabs.len();
        if before >= MAX_TABS {
            return Err(SeraError::Validation(format!(
                "at most {MAX_TABS} tabs; close one first"
            )));
        }
        // The active page opens a blank popup. It is a popup like any other:
        // it attaches paused and `adopt_child` registers it as the active tab.
        self.call(
            "Runtime.evaluate",
            json!({"expression": "window.open('about:blank')", "userGesture": true}),
        )
        .await?;
        let deadline = Instant::now() + Duration::from_secs(5);
        while lock_tabs(&self.tabs).tabs.len() <= before {
            if Instant::now() >= deadline {
                return Err(browser_error("the new tab did not open"));
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        self.reset_outline()?;
        self.navigate(url).await?;
        self.tabs().await
    }

    async fn switch_tab(&self, tab: &str) -> Result<()> {
        let target = {
            let mut set = lock_tabs(&self.tabs);
            let found = set
                .find(tab)
                .cloned()
                .ok_or_else(|| SeraError::Validation(format!("no tab {tab}")))?;
            set.active = found.label.clone();
            found.target_id
        };
        self.reset_outline()?;
        let _ = self
            .cdp
            .call(None, "Target.activateTarget", json!({"targetId": target}))
            .await;
        Ok(())
    }

    async fn close_tab(&self, tab: &str) -> Result<()> {
        let (target, next) = {
            let mut set = lock_tabs(&self.tabs);
            let found = set
                .find(tab)
                .cloned()
                .ok_or_else(|| SeraError::Validation(format!("no tab {tab}")))?;
            if set.tabs.len() <= 1 {
                return Err(SeraError::Validation(
                    "that is the last tab; close the browser instead".into(),
                ));
            }
            set.remove(tab);
            (
                found.target_id,
                set.active_tab().map(|next| next.target_id.clone()),
            )
        };
        self.reset_outline()?;
        let _ = self
            .cdp
            .call(None, "Target.closeTarget", json!({"targetId": target}))
            .await;
        if let Some(next) = next {
            let _ = self
                .cdp
                .call(None, "Target.activateTarget", json!({"targetId": next}))
                .await;
        }
        Ok(())
    }

    async fn is_alive(&self) -> bool {
        if self.cdp.is_closed() {
            return false;
        }
        match self.child.lock().await.as_mut() {
            Some(child) => matches!(child.try_wait(), Ok(None)),
            None => false,
        }
    }

    async fn shutdown(&self) {
        if let Ok(mut tasks) = self.tasks.lock() {
            for task in tasks.drain(..) {
                task.abort();
            }
        }
        let _ = tokio::time::timeout(
            Duration::from_secs(2),
            self.cdp.call(None, "Browser.close", json!({})),
        )
        .await;
        if let Some(mut child) = self.child.lock().await.take() {
            let _ = child.kill().await;
            let _ = child.wait().await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn args_are_loopback_headless_and_sandboxed_by_default() {
        let args = chromium_args(Path::new("/var/lib/serabot/browser/u1"), false);
        assert!(args.contains(&"--headless=new".to_owned()));
        assert!(args.contains(&"--remote-debugging-address=127.0.0.1".to_owned()));
        assert!(args.contains(&"--remote-debugging-port=0".to_owned()));
        assert!(
            args.iter()
                .any(|a| a.starts_with("--user-data-dir=") && a.ends_with("u1"))
        );
        assert!(!args.iter().any(|a| a == "--no-sandbox"));
        assert!(chromium_args(Path::new("/p"), true).contains(&"--no-sandbox".to_owned()));
    }

    #[test]
    fn a_popup_from_one_of_our_tabs_is_adopted_while_there_is_room() {
        let known = ["T1", "T2"];
        assert_eq!(should_adopt(Some("T2"), &known, 2), PopupDecision::Adopt);
        assert_eq!(
            should_adopt(Some("T2"), &known, MAX_TABS - 1),
            PopupDecision::Adopt
        );
    }

    #[test]
    fn a_full_tab_set_closes_the_popup_it_would_have_adopted() {
        let known = ["T1"];
        assert_eq!(
            should_adopt(Some("T1"), &known, MAX_TABS),
            PopupDecision::Close("tab limit reached")
        );
    }

    #[test]
    fn pages_without_one_of_our_openers_are_closed() {
        let known = ["T1"];
        // No opener at all (a page the browser created itself).
        assert!(matches!(
            should_adopt(None, &known, 1),
            PopupDecision::Close(_)
        ));
        // An opener that is not one of our tabs.
        assert!(matches!(
            should_adopt(Some("SOMEONE-ELSE"), &known, 1),
            PopupDecision::Close(_)
        ));
        // A tab that was closed since: its id no longer counts.
        assert!(matches!(
            should_adopt(Some("T9"), &known, 1),
            PopupDecision::Close(_)
        ));
        assert!(matches!(
            should_adopt(Some(""), &[], 0),
            PopupDecision::Close(_)
        ));
    }

    #[test]
    fn parses_devtools_active_port() {
        assert_eq!(
            parse_devtools_active_port("40123\n/devtools/browser/abc-def\n"),
            Some((40123, "/devtools/browser/abc-def".to_owned()))
        );
        assert_eq!(parse_devtools_active_port("80\n/x"), None);
        assert_eq!(parse_devtools_active_port("40123\nnot-a-path"), None);
        assert_eq!(parse_devtools_active_port(""), None);
    }

    #[test]
    fn configured_executable_wins() {
        assert_eq!(
            resolve_chromium_executable("/snap/bin/chromium").unwrap(),
            "/snap/bin/chromium"
        );
    }

    /// Needs a real Chromium: `CHROME_PATH=... cargo test -- --ignored live_`.
    #[tokio::test]
    #[ignore = "requires a local Chromium"]
    async fn live_navigate_snapshot_click_screenshot() {
        let config = BrowserConfig {
            enabled: true,
            no_sandbox: true,
            ..BrowserConfig::default()
        };
        let base = std::env::temp_dir().join(format!("serabot-live-{}", uuid::Uuid::new_v4()));
        let launcher = ChromiumLauncher::new(&config, base.clone()).unwrap();
        let backend = launcher.launch("live-user", "default").await.unwrap();
        backend
            .navigate("data:text/html,<h1>Hi</h1><button onclick=\"document.title='clicked'\">Go</button><input aria-label=name>")
            .await
            .unwrap();
        let (_, outline) = backend.snapshot().await.unwrap();
        assert!(outline.text.contains("button \"Go\""), "{}", outline.text);
        backend.click_ref("e1").await.unwrap();
        assert_eq!(backend.info().await.unwrap().title, "clicked");
        let jpeg = backend.screenshot_jpeg().await.unwrap();
        assert_eq!(&jpeg[..2], &[0xff, 0xd8]);
        assert!(jpeg.len() <= MAX_SCREENSHOT_BYTES);
        backend.shutdown().await;
        let _ = std::fs::remove_dir_all(base);
    }

    /// Needs a real Chromium: `CHROME_PATH=... cargo test -- --ignored live_popup`.
    #[tokio::test]
    #[ignore = "requires a local Chromium"]
    async fn live_popup_opened_by_a_tab_is_adopted_and_active() {
        let config = BrowserConfig {
            enabled: true,
            no_sandbox: true,
            ..BrowserConfig::default()
        };
        let base = std::env::temp_dir().join(format!("serabot-live-{}", uuid::Uuid::new_v4()));
        let launcher = ChromiumLauncher::new(&config, base.clone()).unwrap();
        let backend = launcher.launch("live-user", "popup").await.unwrap();
        backend
            .navigate("data:text/html,<button style=\"position:fixed;left:0;top:0;width:1280px;height:800px\" onclick=\"var w=window.open('about:blank');w.document.title='popup';w.document.body.textContent='hello';\">Open</button>")
            .await
            .unwrap();
        backend.click_at(10.0, 10.0).await.unwrap();
        let mut tabs = Vec::new();
        for _ in 0..50 {
            tabs = backend.tabs().await.unwrap();
            if tabs.len() == 2 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        assert_eq!(tabs.len(), 2, "{tabs:?}");
        assert!(tabs[1].active, "{tabs:?}");
        assert_eq!(backend.info().await.unwrap().title, "popup");
        // open_tab takes the same path: a popup of the active tab.
        let opened = backend
            .open_tab("data:text/html,<title>third</title>")
            .await
            .unwrap();
        assert_eq!(opened.len(), 3, "{opened:?}");
        assert!(opened[2].active, "{opened:?}");
        assert_eq!(backend.info().await.unwrap().title, "third");
        backend.shutdown().await;
        let _ = std::fs::remove_dir_all(base);
    }

    /// Needs a real Chromium: `CHROME_PATH=... cargo test -- --ignored live_popup_first`.
    /// A popup's first request is guarded: a loopback listener (which the
    /// guard refuses) must never see a connection, and the popup is adopted.
    #[tokio::test]
    #[ignore = "requires a local Chromium"]
    async fn live_popup_first_request_is_guarded() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let config = BrowserConfig {
            enabled: true,
            no_sandbox: true,
            ..BrowserConfig::default()
        };
        let base = std::env::temp_dir().join(format!("serabot-live-{}", uuid::Uuid::new_v4()));
        let launcher = ChromiumLauncher::new(&config, base.clone()).unwrap();
        let backend = launcher.launch("live-user", "guard").await.unwrap();
        backend
            .navigate(&format!(
                "data:text/html,<button style=\"position:fixed;left:0;top:0;width:1280px;height:800px\" onclick=\"window.open('http://127.0.0.1:{port}/private')\">Open</button>"
            ))
            .await
            .unwrap();
        backend.click_at(10.0, 10.0).await.unwrap();
        // Chromium may open a bare TCP connection (a preconnect) before the
        // guard sees the request; that is a documented limit. No HTTP request
        // may reach the refused address.
        if let Ok(Ok((mut stream, _peer))) =
            tokio::time::timeout(Duration::from_secs(3), listener.accept()).await
        {
            use tokio::io::AsyncReadExt;
            let mut buf = vec![0u8; 400];
            let n = tokio::time::timeout(Duration::from_secs(2), stream.read(&mut buf))
                .await
                .ok()
                .and_then(|read| read.ok())
                .unwrap_or(0);
            let head = String::from_utf8_lossy(&buf[..n]).to_string();
            assert!(
                !["GET ", "HEAD ", "POST "]
                    .iter()
                    .any(|verb| head.starts_with(verb)),
                "an HTTP request reached a refused address: {head}"
            );
        }
        let tabs = backend.tabs().await.unwrap();
        assert_eq!(tabs.len(), 2, "{tabs:?}");
        backend.shutdown().await;
        let _ = std::fs::remove_dir_all(base);
    }

    /// Needs a real Chromium: `CHROME_PATH=... cargo test -- --ignored live_history`.
    #[tokio::test]
    #[ignore = "requires a local Chromium"]
    async fn live_history_steps_and_reload() {
        let config = BrowserConfig {
            enabled: true,
            no_sandbox: true,
            ..BrowserConfig::default()
        };
        let base = std::env::temp_dir().join(format!("serabot-live-{}", uuid::Uuid::new_v4()));
        let launcher = ChromiumLauncher::new(&config, base.clone()).unwrap();
        let backend = launcher.launch("live-user", "history").await.unwrap();
        backend
            .navigate("data:text/html,<title>one</title>")
            .await
            .unwrap();
        backend
            .navigate("data:text/html,<title>two</title>")
            .await
            .unwrap();
        assert_eq!(backend.history_step(-1).await.unwrap().title, "one");
        assert_eq!(backend.history_step(1).await.unwrap().title, "two");
        assert_eq!(backend.reload().await.unwrap().title, "two");
        assert!(backend.history_step(1).await.is_err(), "no forward entry");
        backend.shutdown().await;
        let _ = std::fs::remove_dir_all(base);
    }
}
