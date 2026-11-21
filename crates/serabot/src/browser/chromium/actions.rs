//! The 0.70+ page actions of the Chromium backend: actionability-checked
//! clicks, hover, select, check, scroll, upload, download, PDF, banners, page
//! text and load waiting.
//!
//! CDP used here: `DOM.resolveNode`, `DOM.getBoxModel`,
//! `DOM.scrollIntoViewIfNeeded`, `DOM.setFileInputFiles`,
//! `Runtime.callFunctionOn`, `Runtime.evaluate`, `Input.dispatchMouseEvent`,
//! `Page.printToPDF`, `Page.getFrameTree`, `Network.loadNetworkResource`,
//! `IO.read`, `IO.close`, `Target.getTargetInfo`, `Target.getTargets`,
//! `Browser.setDownloadBehavior`, `Browser.cancelDownload` and the
//! `Browser.downloadWillBegin` / `Browser.downloadProgress` events.

use std::path::Path;
use std::time::{Duration, Instant};

use base64::Engine;
use serde_json::{Value, json};
use url::Url;

use super::{ChromiumBackend, browser_error};
use crate::browser::actionability::{Action, PROBE_JS, Probe, quad_centre};
use crate::browser::banners::{BannerCandidate, COLLECT_JS, click_js};
use crate::browser::guard::{validate_navigation_url, validate_resolved};
use crate::browser::manager::{BrowserBackend, Download, LoadState, ScrollState};
use crate::browser::outline::RefTarget;
use crate::error::{Result, SeraError};
use crate::util::{mime_for_filename, truncate_chars};

const PAGE_TEXT_JS: &str = r#"(() => {
  const pick = document.querySelector('main, article, [role=main]');
  const el = pick && (pick.innerText || '').trim().length > 200 ? pick : document.body;
  const text = (el ? el.innerText : '') || '';
  return text.replace(/[ \t]+/g, ' ').replace(/\n{3,}/g, '\n\n').trim();
})()"#;

const CHECKED_JS: &str = r#"function() {
  const t = (this.type || '').toLowerCase();
  const role = ((this.getAttribute && this.getAttribute('role')) || '').toLowerCase();
  if (this.tagName === 'INPUT' && (t === 'checkbox' || t === 'radio')) {
    return JSON.stringify({checkable: true, checked: !!this.checked, radio: t === 'radio'});
  }
  if (['checkbox', 'switch', 'radio', 'menuitemcheckbox', 'menuitemradio'].includes(role)) {
    return JSON.stringify({checkable: true, checked: this.getAttribute('aria-checked') === 'true', radio: role === 'radio'});
  }
  return JSON.stringify({checkable: false, checked: false, radio: false});
}"#;

const SELECT_JS: &str = r#"function(wanted) {
  if (this.tagName !== 'SELECT') return JSON.stringify({error: 'not_select'});
  const w = String(wanted).trim().toLowerCase();
  const opts = Array.from(this.options);
  let idx = opts.findIndex((o) => o.label.trim().toLowerCase() === w);
  if (idx < 0) idx = opts.findIndex((o) => o.value.toLowerCase() === w);
  if (idx < 0) idx = opts.findIndex((o) => o.label.trim().toLowerCase().includes(w));
  if (idx < 0) return JSON.stringify({error: 'no_option', options: opts.slice(0, 10).map((o) => o.label.trim().slice(0, 40))});
  if (opts[idx].disabled) return JSON.stringify({error: 'option_disabled'});
  this.selectedIndex = idx;
  this.dispatchEvent(new Event('input', {bubbles: true}));
  this.dispatchEvent(new Event('change', {bubbles: true}));
  return JSON.stringify({label: opts[idx].label.trim().slice(0, 80)});
}"#;

const SCROLL_ELEMENT_JS: &str = r#"function(dx, dy) {
  this.scrollBy({left: dx, top: dy, behavior: 'instant'});
  return JSON.stringify({x: this.scrollLeft, y: this.scrollTop, height: this.scrollHeight, view: this.clientHeight});
}"#;

fn scroll_page_js(dx: f64, dy: f64) -> String {
    format!(
        "(() => {{ window.scrollBy({{left: {dx}, top: {dy}, behavior: 'instant'}}); const e = document.scrollingElement || document.documentElement; return JSON.stringify({{x: window.scrollX, y: window.scrollY, height: e.scrollHeight, view: window.innerHeight}}); }})()"
    )
}

const LOAD_STATE_JS: &str =
    "JSON.stringify({rs: document.readyState, n: performance.getEntriesByType('resource').length})";

const IO_READ_CHUNK_BYTES: usize = 64 * 1024;
const CLICK_DOWNLOAD_FALLBACK_POLL: Duration = Duration::from_secs(2);

/// Decides "network idle" from samples of (ready state, resource count): the
/// page is idle once it is loaded and the count has not moved for `quiet`.
pub struct IdleTracker {
    last_count: Option<u64>,
    since: Instant,
    quiet: Duration,
}

impl IdleTracker {
    pub fn new(now: Instant, quiet: Duration) -> Self {
        Self {
            last_count: None,
            since: now,
            quiet,
        }
    }

    pub fn observe(&mut self, ready_state: &str, count: u64, now: Instant) -> bool {
        if self.last_count != Some(count) {
            self.last_count = Some(count);
            self.since = now;
        }
        ready_state == "complete" && now.duration_since(self.since) >= self.quiet
    }
}

/// Folder-safe download id from the browser.
fn safe_guid(guid: &str) -> bool {
    !guid.is_empty()
        && guid.len() <= 64
        && guid
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

/// A bare file name from the name a site suggested.
pub fn clean_download_name(raw: &str) -> String {
    let name = raw
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or("")
        .chars()
        .filter(|c| !c.is_control())
        .collect::<String>();
    let name = name.trim().trim_start_matches('.').to_owned();
    if name.is_empty() {
        "download.bin".into()
    } else {
        truncate_chars(&name, 120)
    }
}

/// Normalize a Content-Type header value into a bare lowercase media type.
pub fn normalize_mime(raw: Option<&str>) -> Option<String> {
    let raw = raw?.split(';').next()?.trim().to_ascii_lowercase();
    (!raw.is_empty()).then_some(raw)
}

/// Map a MIME type to a default file extension (without leading dot).
pub fn extension_for_mime(mime: &str) -> Option<&'static str> {
    let base = mime.split(';').next()?.trim().to_ascii_lowercase();
    match base.as_str() {
        "application/pdf" => Some("pdf"),
        "application/vnd.openxmlformats-officedocument.wordprocessingml.document" => Some("docx"),
        "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet" => Some("xlsx"),
        "application/vnd.openxmlformats-officedocument.presentationml.presentation" => Some("pptx"),
        "text/csv" | "application/csv" => Some("csv"),
        "text/plain" => Some("txt"),
        "text/markdown" => Some("md"),
        "application/json" => Some("json"),
        "application/xml" | "text/xml" => Some("xml"),
        "text/html" | "application/xhtml+xml" => Some("html"),
        "image/png" => Some("png"),
        "image/jpeg" => Some("jpg"),
        "image/webp" => Some("webp"),
        "image/gif" => Some("gif"),
        "application/zip" => Some("zip"),
        _ => None,
    }
}

/// Look up a header value case-insensitively in a CDP `headers` JSON object.
pub fn header_value<'a>(headers: &'a Value, name: &str) -> Option<&'a str> {
    let map = headers.as_object()?;
    map.iter()
        .find(|(k, _)| k.eq_ignore_ascii_case(name))
        .and_then(|(_, v)| v.as_str())
}

/// Extract the filename from a `Content-Disposition` header (`filename*=` or `filename=`).
pub fn filename_from_content_disposition(header: &str) -> Option<String> {
    let mut plain: Option<String> = None;
    for part in header.split(';').skip(1) {
        let part = part.trim();
        let Some((key, raw_val)) = part.split_once('=') else {
            continue;
        };
        let key = key.trim().to_ascii_lowercase();
        let val = raw_val.trim();
        if key == "filename*" {
            let encoded = val
                .split_once("''")
                .map_or(val, |(_, rest)| rest)
                .trim_matches('"');
            let decoded = percent_decode(encoded);
            let cleaned = clean_download_name(&decoded);
            if cleaned != "download.bin" {
                return Some(cleaned);
            }
        } else if key == "filename" && plain.is_none() {
            let unquoted = val.trim_matches('"').trim_matches('\'');
            let cleaned = clean_download_name(unquoted);
            if cleaned != "download.bin" {
                plain = Some(cleaned);
            }
        }
    }
    plain
}

fn percent_decode(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && i + 2 < bytes.len()
            && let (Some(hi), Some(lo)) = (hex_val(bytes[i + 1]), hex_val(bytes[i + 2]))
        {
            out.push((hi << 4) | lo);
            i += 3;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn hex_val(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

/// Derive a safe download filename from `Content-Disposition`, the URL path,
/// and the response MIME type (for example `application/pdf` -> `.pdf`).
pub fn derive_download_filename(
    content_disposition: Option<&str>,
    url: &str,
    mime: Option<&str>,
    bytes: &[u8],
) -> String {
    let inferred_mime = normalize_mime(mime).or_else(|| {
        bytes
            .starts_with(b"%PDF")
            .then(|| "application/pdf".to_owned())
    });
    let default_ext = inferred_mime
        .as_deref()
        .and_then(extension_for_mime)
        .unwrap_or("bin");

    if let Some(cd) = content_disposition
        && let Some(name) = filename_from_content_disposition(cd)
    {
        return ensure_extension(&name, default_ext, inferred_mime.as_deref());
    }

    if let Ok(parsed) = Url::parse(url)
        && let Some(segment) = parsed
            .path_segments()
            .and_then(|mut s| s.rfind(|seg| !seg.is_empty()))
    {
        let decoded = percent_decode(segment);
        let cleaned = clean_download_name(&decoded);
        if cleaned != "download.bin" {
            return ensure_extension(&cleaned, default_ext, inferred_mime.as_deref());
        }
    }

    format!("download.{default_ext}")
}

fn ensure_extension(name: &str, default_ext: &str, mime: Option<&str>) -> String {
    let has_ext = Path::new(name)
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| {
            !e.is_empty() && e.len() <= 10 && e.chars().all(|c| c.is_ascii_alphanumeric())
        });
    if has_ext {
        return name.to_owned();
    }
    if mime == Some("application/pdf") || default_ext != "bin" {
        return truncate_chars(&format!("{name}.{default_ext}"), 120);
    }
    name.to_owned()
}

/// True when the MIME type is a document/file rather than a standard HTML web page.
pub fn is_document_mime(mime: &str) -> bool {
    let base = mime
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    !base.is_empty()
        && !matches!(
            base.as_str(),
            "text/html" | "application/xhtml+xml" | "about:blank"
        )
}

/// True when the URL path ends with a known downloadable document extension.
pub fn path_looks_like_document(url: &str) -> bool {
    let Ok(parsed) = Url::parse(url) else {
        return false;
    };
    let ext = Path::new(parsed.path())
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase);
    matches!(
        ext.as_deref(),
        Some("pdf" | "docx" | "xlsx" | "pptx" | "csv" | "zip" | "txt" | "json" | "xml")
    )
}

/// Determine the final MIME type from response header, magic bytes, or filename.
pub fn resolve_mime(header_mime: Option<&str>, filename: &str, bytes: &[u8]) -> String {
    if bytes.starts_with(b"%PDF") {
        return "application/pdf".to_owned();
    }
    if let Some(mime) = normalize_mime(header_mime)
        && mime != "application/octet-stream"
    {
        return mime;
    }
    let by_name = mime_for_filename(filename);
    if by_name != "application/octet-stream" {
        return by_name.to_owned();
    }
    normalize_mime(header_mime).unwrap_or_else(|| "application/octet-stream".to_owned())
}

impl ChromiumBackend {
    async fn eval_string(&self, expression: &str) -> Result<String> {
        let result = self
            .call(
                "Runtime.evaluate",
                json!({"expression": expression, "returnByValue": true}),
            )
            .await?;
        if result.get("exceptionDetails").is_some() {
            return Err(browser_error("the page script failed"));
        }
        Ok(result["result"]["value"]
            .as_str()
            .unwrap_or_default()
            .to_owned())
    }

    async fn object_id(&self, target: &RefTarget) -> Result<String> {
        let node = self
            .call(
                "DOM.resolveNode",
                json!({"backendNodeId": target.backend_node_id}),
            )
            .await
            .map_err(|_| {
                SeraError::Validation(
                    "the element is no longer on the page; take a new browser_snapshot".into(),
                )
            })?;
        node["object"]["objectId"]
            .as_str()
            .map(str::to_owned)
            .ok_or_else(|| {
                SeraError::Validation(
                    "the element is no longer on the page; take a new browser_snapshot".into(),
                )
            })
    }

    async fn call_on(
        &self,
        object_id: &str,
        function: &str,
        arguments: Vec<Value>,
    ) -> Result<Value> {
        let result = self
            .call(
                "Runtime.callFunctionOn",
                json!({
                    "objectId": object_id,
                    "functionDeclaration": function,
                    "arguments": arguments,
                    "returnByValue": true,
                }),
            )
            .await?;
        if result.get("exceptionDetails").is_some() {
            return Err(browser_error("the element could not be inspected"));
        }
        Ok(result)
    }

    async fn probe(&self, object_id: &str, point: Option<(f64, f64)>) -> Result<Probe> {
        let args = match point {
            Some((x, y)) => vec![json!({"value": x}), json!({"value": y})],
            None => vec![json!({"value": Value::Null}), json!({"value": Value::Null})],
        };
        let result = self.call_on(object_id, PROBE_JS, args).await?;
        Probe::from_call_result(&result)
    }

    /// Attached, visible, enabled and editable as `action` needs; no click
    /// point. Used for typing, selecting and uploading.
    pub(super) async fn check_ready(&self, target: &RefTarget, action: Action) -> Result<()> {
        let object_id = self.object_id(target).await?;
        let probe = self.probe(&object_id, None).await?;
        if let Some(message) = probe.blocker(action, &target.role) {
            return Err(SeraError::Validation(message));
        }
        if action == Action::Edit {
            let _ = self
                .call(
                    "DOM.scrollIntoViewIfNeeded",
                    json!({"backendNodeId": target.backend_node_id}),
                )
                .await;
        }
        Ok(())
    }

    /// Check the element, scroll it into view and return the point to click:
    /// the centre of its box, where `elementFromPoint` must land on it.
    pub(super) async fn actionable_point(
        &self,
        target: &RefTarget,
        action: Action,
    ) -> Result<(f64, f64)> {
        let object_id = self.object_id(target).await?;
        let first = self.probe(&object_id, None).await?;
        if let Some(message) = first.blocker(action, &target.role) {
            return Err(SeraError::Validation(message));
        }
        let _ = self
            .call(
                "DOM.scrollIntoViewIfNeeded",
                json!({"backendNodeId": target.backend_node_id}),
            )
            .await;
        let not_visible = || {
            SeraError::Validation(format!(
                "{} is not visible (it has no size on the page); scroll, open its menu, or take a new browser_snapshot",
                match target.role.as_str() {
                    "button" => "the button",
                    "link" => "the link",
                    "textbox" | "searchbox" => "the field",
                    _ => "the element",
                }
            ))
        };
        let model = self
            .call(
                "DOM.getBoxModel",
                json!({"backendNodeId": target.backend_node_id}),
            )
            .await
            .map_err(|_| not_visible())?;
        let quad = model["model"]["content"]
            .as_array()
            .ok_or_else(not_visible)?;
        let (x, y) = quad_centre(quad).ok_or_else(not_visible)?;
        let second = self.probe(&object_id, Some((x, y))).await?;
        if let Some(message) = second.blocker(action, &target.role) {
            return Err(SeraError::Validation(message));
        }
        Ok((x, y))
    }

    pub(super) async fn do_hover(&self, reference: &str) -> Result<()> {
        let target = self.target_ref(reference)?;
        let (x, y) = self.actionable_point(&target, Action::Hover).await?;
        self.set_pointer(x, y, "hover");
        self.call(
            "Input.dispatchMouseEvent",
            json!({"type": "mouseMoved", "x": x, "y": y, "button": "none"}),
        )
        .await?;
        tokio::time::sleep(Duration::from_millis(250)).await;
        Ok(())
    }

    pub(super) async fn do_select(&self, reference: &str, option: &str) -> Result<String> {
        let target = self.target_ref(reference)?;
        self.check_ready(&target, Action::Edit).await?;
        let object_id = self.object_id(&target).await?;
        let result = self
            .call_on(&object_id, SELECT_JS, vec![json!({"value": option})])
            .await?;
        let raw = result["result"]["value"].as_str().unwrap_or_default();
        let value: Value = serde_json::from_str(raw).unwrap_or(Value::Null);
        match value["error"].as_str() {
            None => Ok(value["label"].as_str().unwrap_or(option).to_owned()),
            Some("not_select") => Err(SeraError::Validation(
                "that element is not a <select> list; click it and click the option instead".into(),
            )),
            Some("option_disabled") => Err(SeraError::Validation("that option is disabled".into())),
            Some(_) => {
                let options = value["options"]
                    .as_array()
                    .map(|list| {
                        list.iter()
                            .filter_map(Value::as_str)
                            .collect::<Vec<_>>()
                            .join(" | ")
                    })
                    .unwrap_or_default();
                Err(SeraError::Validation(format!(
                    "no option matches '{}'; the list has: {options}",
                    truncate_chars(option, 60)
                )))
            }
        }
    }

    async fn checked_state(&self, object_id: &str) -> Result<(bool, bool, bool)> {
        let result = self.call_on(object_id, CHECKED_JS, Vec::new()).await?;
        let value: Value = serde_json::from_str(result["result"]["value"].as_str().unwrap_or("{}"))
            .unwrap_or(Value::Null);
        Ok((
            value["checkable"].as_bool().unwrap_or(false),
            value["checked"].as_bool().unwrap_or(false),
            value["radio"].as_bool().unwrap_or(false),
        ))
    }

    pub(super) async fn do_set_checked(&self, reference: &str, checked: bool) -> Result<bool> {
        let target = self.target_ref(reference)?;
        let object_id = self.object_id(&target).await?;
        let (checkable, current, radio) = self.checked_state(&object_id).await?;
        if !checkable {
            return Err(SeraError::Validation(
                "that element is not a checkbox, radio button or switch".into(),
            ));
        }
        if current == checked {
            return Ok(current);
        }
        if !checked && radio {
            return Err(SeraError::Validation(
                "a radio button cannot be switched off; choose another option instead".into(),
            ));
        }
        let probe = self.probe(&object_id, None).await?;
        let visually_hidden = probe.hidden || probe.display_none;
        if visually_hidden && probe.tag == "input" {
            // A styled checkbox: the input itself is hidden. Toggle it the way
            // a click on its label would.
            if let Some(message) = probe.blocker(Action::Click, &target.role) {
                return Err(SeraError::Validation(message));
            }
            self.call_on(&object_id, "function() { this.click(); }", Vec::new())
                .await?;
        } else {
            let (x, y) = self.actionable_point(&target, Action::Click).await?;
            self.click_at(x, y).await?;
        }
        tokio::time::sleep(Duration::from_millis(150)).await;
        let (_, after, _) = self.checked_state(&object_id).await?;
        if after != checked {
            return Err(SeraError::Validation(
                "the checkbox did not change; the page may lock it".into(),
            ));
        }
        Ok(after)
    }

    pub(super) async fn do_scroll_by(
        &self,
        reference: Option<&str>,
        dx: f64,
        dy: f64,
    ) -> Result<ScrollState> {
        let raw = match reference {
            Some(reference) => {
                let target = self.target_ref(reference)?;
                let object_id = self.object_id(&target).await?;
                let result = self
                    .call_on(
                        &object_id,
                        SCROLL_ELEMENT_JS,
                        vec![json!({"value": dx}), json!({"value": dy})],
                    )
                    .await?;
                result["result"]["value"]
                    .as_str()
                    .unwrap_or_default()
                    .to_owned()
            }
            None => self.eval_string(&scroll_page_js(dx, dy)).await?,
        };
        let value: Value = serde_json::from_str(&raw)
            .map_err(|_| browser_error("the page did not report its scroll position"))?;
        self.set_pointer(
            f64::from(super::VIEWPORT_WIDTH / 2),
            f64::from(super::VIEWPORT_HEIGHT / 2),
            "scroll",
        );
        Ok(ScrollState {
            x: value["x"].as_f64().unwrap_or(0.0),
            y: value["y"].as_f64().unwrap_or(0.0),
            height: value["height"].as_f64().unwrap_or(0.0),
            view: value["view"].as_f64().unwrap_or(0.0),
        })
    }

    pub(super) async fn do_scroll_to(&self, reference: &str) -> Result<()> {
        let target = self.target_ref(reference)?;
        let object_id = self.object_id(&target).await?;
        self.call_on(
            &object_id,
            "function() { this.scrollIntoView({block: 'center', inline: 'center', behavior: 'instant'}); }",
            Vec::new(),
        )
        .await?;
        Ok(())
    }

    pub(super) async fn do_upload(&self, reference: &str, path: &Path) -> Result<()> {
        let target = self.target_ref(reference)?;
        let object_id = self.object_id(&target).await?;
        let probe = self.probe(&object_id, None).await?;
        if let Some(message) = probe.blocker(Action::Upload, &target.role) {
            return Err(SeraError::Validation(message));
        }
        if !(probe.tag == "input" && probe.input_type == "file") {
            return Err(SeraError::Validation(
                "that element is not a file input; look for the field that says choose file".into(),
            ));
        }
        self.call(
            "DOM.setFileInputFiles",
            json!({"files": [path.to_string_lossy()], "backendNodeId": target.backend_node_id}),
        )
        .await?;
        Ok(())
    }

    pub(super) async fn do_wait_load(&self, state: LoadState, timeout: Duration) -> Result<bool> {
        let started = Instant::now();
        let mut idle = IdleTracker::new(started, Duration::from_millis(600));
        loop {
            let raw = self.eval_string(LOAD_STATE_JS).await?;
            let value: Value = serde_json::from_str(&raw).unwrap_or(Value::Null);
            let ready = value["rs"].as_str().unwrap_or_default();
            let count = value["n"].as_u64().unwrap_or(0);
            let quiet = idle.observe(ready, count, Instant::now());
            let reached = match state {
                LoadState::Load => ready == "complete",
                LoadState::NetworkIdle => quiet,
            };
            if reached {
                return Ok(true);
            }
            if started.elapsed() >= timeout {
                return Ok(false);
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
    }

    pub(super) async fn do_download(
        &self,
        reference: &str,
        timeout: Duration,
        max_bytes: u64,
    ) -> Result<Download> {
        let target = self.target_ref(reference)?;
        let dir = self
            .temp_root
            .join(format!("dl-{}", uuid::Uuid::new_v4().simple()));
        std::fs::create_dir_all(&dir)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700));
        }
        let result = self.download_inner(&target, &dir, timeout, max_bytes).await;
        // Downloads go back to "refused" and the temp copy is removed, always.
        let _ = self
            .cdp
            .call(
                None,
                "Browser.setDownloadBehavior",
                json!({"behavior": "deny"}),
            )
            .await;
        let _ = std::fs::remove_dir_all(&dir);
        result
    }

    pub(super) async fn do_download_url(
        &self,
        url: &str,
        timeout: Duration,
        max_bytes: u64,
    ) -> Result<Download> {
        let parsed = validate_resolved(url).await?;
        let tab = self.active()?;
        self.load_network_resource_in_session(&tab.session_id, parsed.as_str(), timeout, max_bytes)
            .await
    }

    pub(super) async fn do_download_current(
        &self,
        timeout: Duration,
        max_bytes: u64,
    ) -> Result<Download> {
        let tab = self.active()?;
        let (url, frame_id, mime) = self.session_document_info(&tab.session_id).await?;
        if url.is_empty() || url == "about:blank" {
            return Err(SeraError::Validation(
                "no document is open in the current tab; open a page or PDF first".into(),
            ));
        }
        let parsed = validate_resolved(&url).await?;
        if let Some(mime) = mime.as_deref()
            && !is_document_mime(mime)
        {
            return Err(SeraError::Validation(format!(
                "the current tab is a web page ({mime}), not a downloadable document; use browser_download with ref or url, or browser_pdf to print the page"
            )));
        }
        self.load_network_resource_with_frame(
            &tab.session_id,
            &frame_id,
            parsed.as_str(),
            timeout,
            max_bytes,
        )
        .await
    }

    async fn clicked_link_url(&self, target: &RefTarget) -> Option<String> {
        let object_id = self.object_id(target).await.ok()?;
        let res = self
            .call_on(
                &object_id,
                "function() { return this.href || (this.closest && this.closest('a') && this.closest('a').href) || ''; }",
                Vec::new(),
            )
            .await
            .ok()?;
        let href = res["result"]["value"].as_str()?.trim().to_owned();
        (!href.is_empty() && !href.starts_with('#')).then_some(href)
    }

    async fn session_document_info(
        &self,
        session_id: &str,
    ) -> Result<(String, String, Option<String>)> {
        let tree = self
            .cdp
            .call(Some(session_id), "Page.getFrameTree", json!({}))
            .await?;
        let frame = &tree["frameTree"]["frame"];
        let frame_id = frame["id"].as_str().unwrap_or_default().to_owned();
        let url = frame["url"].as_str().unwrap_or_default().to_owned();
        let mime = normalize_mime(frame["mimeType"].as_str());
        Ok((url, frame_id, mime))
    }

    async fn active_document_snapshot(&self) -> (String, String, String, Option<String>) {
        let Ok(tab) = self.active() else {
            return (String::new(), String::new(), String::new(), None);
        };
        if let Ok((url, frame_id, mime)) = self.session_document_info(&tab.session_id).await
            && !url.is_empty()
        {
            return (tab.session_id, url, frame_id, mime);
        }
        let url = self.info().await.map(|info| info.url).unwrap_or_default();
        (tab.session_id, url, String::new(), None)
    }

    async fn load_network_resource_in_session(
        &self,
        session_id: &str,
        url: &str,
        timeout: Duration,
        max_bytes: u64,
    ) -> Result<Download> {
        let tree = self
            .cdp
            .call(Some(session_id), "Page.getFrameTree", json!({}))
            .await?;
        let frame_id = tree["frameTree"]["frame"]["id"]
            .as_str()
            .ok_or_else(|| browser_error("active tab has no main frame"))?
            .to_owned();
        self.load_network_resource_with_frame(session_id, &frame_id, url, timeout, max_bytes)
            .await
    }

    async fn load_network_resource_with_frame(
        &self,
        session_id: &str,
        frame_id: &str,
        url: &str,
        timeout: Duration,
        max_bytes: u64,
    ) -> Result<Download> {
        let deadline = Instant::now() + timeout;
        let call_fut = self.cdp.call(
            Some(session_id),
            "Network.loadNetworkResource",
            json!({
                "frameId": frame_id,
                "url": url,
                "options": {"disableCache": false, "includeCredentials": true}
            }),
        );
        let reply = tokio::time::timeout(timeout, call_fut)
            .await
            .map_err(|_| SeraError::Validation("timed out fetching the document".into()))??;
        let resource = &reply["resource"];
        if !resource["success"].as_bool().unwrap_or(false) {
            let status = resource["httpStatusCode"].as_i64();
            let net_err = resource["netErrorName"].as_str().unwrap_or_default();
            if let Some(code) = status.filter(|c| *c >= 400) {
                return Err(SeraError::Validation(format!(
                    "document download failed with HTTP {code}"
                )));
            }
            if !net_err.is_empty() {
                return Err(SeraError::Validation(format!(
                    "document download failed ({net_err})"
                )));
            }
            return Err(SeraError::Validation(
                "document download failed in the browser".into(),
            ));
        }

        let headers = &resource["headers"];
        if let Some(len_str) = header_value(headers, "content-length")
            && let Ok(len) = len_str.trim().parse::<u64>()
            && len > max_bytes
        {
            if let Some(handle) = resource["stream"].as_str() {
                let _ = self
                    .cdp
                    .call(Some(session_id), "IO.close", json!({"handle": handle}))
                    .await;
            }
            return Err(SeraError::Validation(format!(
                "the file is larger than {} MB; not downloading it",
                max_bytes / (1024 * 1024)
            )));
        }

        let handle = resource["stream"]
            .as_str()
            .ok_or_else(|| browser_error("browser returned no stream handle for resource"))?;
        let bytes = self
            .read_io_stream(session_id, handle, deadline, max_bytes)
            .await?;

        let content_disposition = header_value(headers, "content-disposition");
        let content_type = header_value(headers, "content-type");
        let filename = derive_download_filename(content_disposition, url, content_type, &bytes);
        let mime = resolve_mime(content_type, &filename, &bytes);
        Ok(Download {
            filename,
            mime,
            bytes,
        })
    }

    async fn read_io_stream(
        &self,
        session_id: &str,
        handle: &str,
        deadline: Instant,
        max_bytes: u64,
    ) -> Result<Vec<u8>> {
        let mut out = Vec::new();
        let read_result: Result<()> = async {
            loop {
                let left = deadline.saturating_duration_since(Instant::now());
                if left.is_zero() {
                    return Err(SeraError::Validation(
                        "timed out reading the document stream".into(),
                    ));
                }
                let chunk = tokio::time::timeout(
                    left,
                    self.cdp.call(
                        Some(session_id),
                        "IO.read",
                        json!({"handle": handle, "size": IO_READ_CHUNK_BYTES}),
                    ),
                )
                .await
                .map_err(|_| {
                    SeraError::Validation("timed out reading the document stream".into())
                })??;
                let data = chunk["data"].as_str().unwrap_or_default();
                let decoded = if chunk["base64Encoded"].as_bool().unwrap_or(false) {
                    base64::engine::general_purpose::STANDARD
                        .decode(data)
                        .map_err(|_| browser_error("document stream chunk was not valid base64"))?
                } else {
                    data.as_bytes().to_vec()
                };
                if (out.len() as u64).saturating_add(decoded.len() as u64) > max_bytes {
                    return Err(SeraError::Validation(format!(
                        "the file is larger than {} MB; not downloading it",
                        max_bytes / (1024 * 1024)
                    )));
                }
                out.extend_from_slice(&decoded);
                if chunk["eof"].as_bool().unwrap_or(false) {
                    break;
                }
            }
            Ok(())
        }
        .await;
        let _ = self
            .cdp
            .call(Some(session_id), "IO.close", json!({"handle": handle}))
            .await;
        read_result?;
        Ok(out)
    }

    async fn try_click_document_fallback(
        &self,
        before_session: &str,
        before_url: &str,
        clicked_href: Option<&str>,
        deadline: Instant,
        max_bytes: u64,
    ) -> Result<Option<Download>> {
        let (after_session, after_url, after_frame, after_mime) =
            self.active_document_snapshot().await;
        let url_changed = !after_url.is_empty()
            && after_url != "about:blank"
            && (after_url != before_url || after_session != before_session);

        // Case 1: The click navigated (in the same tab or a newly adopted
        // target=_blank tab) directly to a PDF or other non-HTML document.
        if url_changed {
            let looks_doc = after_mime.as_deref().is_some_and(is_document_mime)
                || path_looks_like_document(&after_url);
            if looks_doc {
                let parsed = validate_resolved(&after_url).await?;
                let left = deadline.saturating_duration_since(Instant::now());
                if left.is_zero() {
                    return Ok(None);
                }
                let dl = if !after_frame.is_empty() {
                    self.load_network_resource_with_frame(
                        &after_session,
                        &after_frame,
                        parsed.as_str(),
                        left,
                        max_bytes,
                    )
                    .await?
                } else {
                    self.load_network_resource_in_session(
                        &after_session,
                        parsed.as_str(),
                        left,
                        max_bytes,
                    )
                    .await?
                };
                return Ok(Some(dl));
            }
        }

        // Case 2: The clicked element was a link whose href points to a
        // downloadable document (for example a PDF that didn't finish opening
        // or was prevented from triggering a download event).
        if let Some(href) = clicked_href
            && validate_navigation_url(href).is_ok()
        {
            let left = deadline.saturating_duration_since(Instant::now());
            if !left.is_zero() && !after_session.is_empty() {
                let parsed = validate_resolved(href).await?;
                if let Ok(dl) = self
                    .load_network_resource_in_session(
                        &after_session,
                        parsed.as_str(),
                        left,
                        max_bytes,
                    )
                    .await
                    && is_document_mime(&dl.mime)
                {
                    return Ok(Some(dl));
                }
            }
        }

        Ok(None)
    }

    async fn download_inner(
        &self,
        target: &RefTarget,
        dir: &Path,
        timeout: Duration,
        max_bytes: u64,
    ) -> Result<Download> {
        let (x, y) = self.actionable_point(target, Action::Click).await?;
        let (before_session, before_url, _, _) = self.active_document_snapshot().await;
        let clicked_href = self.clicked_link_url(target).await;
        self.cdp
            .call(
                None,
                "Browser.setDownloadBehavior",
                json!({"behavior": "allowAndName", "downloadPath": dir.to_string_lossy(), "eventsEnabled": true}),
            )
            .await?;
        let mut events = self.cdp.subscribe();
        self.click_at(x, y).await?;
        let deadline = Instant::now() + timeout;
        let fallback_at = (Instant::now() + CLICK_DOWNLOAD_FALLBACK_POLL).min(deadline);
        let mut fallback_checked = false;
        let mut filename = String::from("download.bin");
        let mut download_url = String::new();
        let mut guid = String::new();
        loop {
            let now = Instant::now();
            if now >= deadline {
                if guid.is_empty()
                    && let Some(dl) = self
                        .try_click_document_fallback(
                            &before_session,
                            &before_url,
                            clicked_href.as_deref(),
                            now + Duration::from_secs(5),
                            max_bytes,
                        )
                        .await?
                {
                    return Ok(dl);
                }
                return Err(SeraError::Validation(
                    "no download finished in time; the click may not start a download (if it opened in a viewer, call browser_download with url or current=true)".into(),
                ));
            }
            let wait_until = if guid.is_empty() && !fallback_checked {
                fallback_at.min(deadline)
            } else {
                deadline
            };
            let left = wait_until.saturating_duration_since(now);
            let event = match tokio::time::timeout(left, events.recv()).await {
                Ok(Ok(event)) => event,
                Ok(Err(tokio::sync::broadcast::error::RecvError::Lagged(_))) => continue,
                Ok(Err(_)) => return Err(browser_error("browser connection closed")),
                Err(_) => {
                    if guid.is_empty() && !fallback_checked {
                        fallback_checked = true;
                        if let Some(dl) = self
                            .try_click_document_fallback(
                                &before_session,
                                &before_url,
                                clicked_href.as_deref(),
                                deadline,
                                max_bytes,
                            )
                            .await?
                        {
                            return Ok(dl);
                        }
                        continue;
                    }
                    if guid.is_empty()
                        && let Some(dl) = self
                            .try_click_document_fallback(
                                &before_session,
                                &before_url,
                                clicked_href.as_deref(),
                                Instant::now() + Duration::from_secs(5),
                                max_bytes,
                            )
                            .await?
                    {
                        return Ok(dl);
                    }
                    return Err(SeraError::Validation(
                        "no download finished in time; the click may not start a download (if it opened in a viewer, call browser_download with url or current=true)".into(),
                    ));
                }
            };
            match event.method.as_str() {
                "Browser.downloadWillBegin" => {
                    let url = event.params["url"].as_str().unwrap_or_default();
                    let inline = url.starts_with("blob:") || url.starts_with("data:");
                    let id = event.params["guid"].as_str().unwrap_or_default().to_owned();
                    if !inline && validate_navigation_url(url).is_err() {
                        let _ = self
                            .cdp
                            .call(None, "Browser.cancelDownload", json!({"guid": id}))
                            .await;
                        return Err(SeraError::Validation(
                            "the download came from a non-public address and was refused".into(),
                        ));
                    }
                    download_url = url.to_owned();
                    filename = clean_download_name(
                        event.params["suggestedFilename"]
                            .as_str()
                            .unwrap_or_default(),
                    );
                    guid = id;
                }
                "Browser.downloadProgress"
                    if event.params["guid"].as_str() == Some(guid.as_str()) =>
                {
                    let total = event.params["totalBytes"].as_f64().unwrap_or(0.0);
                    let received = event.params["receivedBytes"].as_f64().unwrap_or(0.0);
                    let limit = max_bytes as f64;
                    if total > limit || received > limit {
                        let _ = self
                            .cdp
                            .call(None, "Browser.cancelDownload", json!({"guid": guid}))
                            .await;
                        return Err(SeraError::Validation(format!(
                            "the file is larger than {} MB; not downloading it",
                            max_bytes / (1024 * 1024)
                        )));
                    }
                    match event.params["state"].as_str() {
                        Some("canceled") => {
                            return Err(SeraError::Validation("the download was cancelled".into()));
                        }
                        Some("completed") => {
                            if !safe_guid(&guid) {
                                return Err(browser_error("unexpected download id"));
                            }
                            let path = dir.join(&guid);
                            let meta = std::fs::metadata(&path)
                                .map_err(|_| browser_error("the downloaded file is missing"))?;
                            if meta.len() > max_bytes {
                                return Err(SeraError::Validation(
                                    "the file is larger than the download limit".into(),
                                ));
                            }
                            let bytes = std::fs::read(&path)?;
                            if filename == "download.bin" && !download_url.is_empty() {
                                filename =
                                    derive_download_filename(None, &download_url, None, &bytes);
                            }
                            let mime = resolve_mime(None, &filename, &bytes);
                            return Ok(Download {
                                filename,
                                mime,
                                bytes,
                            });
                        }
                        _ => {}
                    }
                }
                _ => {}
            }
        }
    }

    pub(super) async fn do_pdf(&self, max_bytes: usize) -> Result<Vec<u8>> {
        let result = self
            .call(
                "Page.printToPDF",
                json!({"printBackground": true, "preferCSSPageSize": true, "transferMode": "ReturnAsBase64"}),
            )
            .await?;
        let data = result["data"]
            .as_str()
            .ok_or_else(|| browser_error("printing returned no data"))?;
        if data.len() / 4 * 3 > max_bytes + 4 {
            return Err(SeraError::Validation(
                "the page is too large to save as a PDF".into(),
            ));
        }
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(data)
            .map_err(|_| browser_error("the PDF was not valid base64"))?;
        if bytes.len() > max_bytes || !bytes.starts_with(b"%PDF") {
            return Err(browser_error("the PDF is empty or too large"));
        }
        Ok(bytes)
    }

    pub(super) async fn do_banner_candidates(&self) -> Result<Vec<BannerCandidate>> {
        let raw = self.eval_string(COLLECT_JS).await?;
        Ok(serde_json::from_str(&raw).unwrap_or_default())
    }

    pub(super) async fn do_click_banner(&self, index: usize) -> Result<()> {
        let clicked = self
            .call(
                "Runtime.evaluate",
                json!({"expression": click_js(index), "returnByValue": true}),
            )
            .await?;
        if clicked["result"]["value"].as_bool() == Some(true) {
            Ok(())
        } else {
            Err(SeraError::Validation("the banner button is gone".into()))
        }
    }

    pub(super) async fn do_page_text(&self, max_chars: usize) -> Result<String> {
        let text = self.eval_string(PAGE_TEXT_JS).await?;
        if text.is_empty() {
            return Ok("(no readable text)".into());
        }
        Ok(truncate_chars(&text, max_chars))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn idle_needs_a_loaded_quiet_page() {
        let start = Instant::now();
        let mut tracker = IdleTracker::new(start, Duration::from_millis(600));
        assert!(!tracker.observe("loading", 3, start));
        assert!(!tracker.observe("complete", 3, start + Duration::from_millis(300)));
        // A new request resets the quiet period.
        assert!(!tracker.observe("complete", 4, start + Duration::from_millis(500)));
        assert!(!tracker.observe("complete", 4, start + Duration::from_millis(900)));
        assert!(tracker.observe("complete", 4, start + Duration::from_millis(1200)));
    }

    #[test]
    fn download_names_and_ids_are_made_safe() {
        assert_eq!(clean_download_name("../../etc/passwd"), "passwd");
        assert_eq!(clean_download_name("C:\\x\\report.pdf"), "report.pdf");
        assert_eq!(clean_download_name(""), "download.bin");
        assert_eq!(clean_download_name(".hidden"), "hidden");
        assert!(safe_guid("1a2b-3c"));
        assert!(!safe_guid("../x"));
        assert!(!safe_guid(""));
    }

    #[test]
    fn derives_filename_from_content_disposition_url_and_mime() {
        assert_eq!(
            derive_download_filename(
                Some("attachment; filename=\"../annual-report.pdf\""),
                "https://example.com/download?id=9",
                Some("application/pdf"),
                b"",
            ),
            "annual-report.pdf"
        );
        assert_eq!(
            derive_download_filename(
                Some("inline; filename*=UTF-8''Q3%20Summary.pdf; filename=\"fallback.pdf\""),
                "https://example.com/view",
                Some("application/pdf"),
                b"",
            ),
            "Q3 Summary.pdf"
        );
        assert_eq!(
            derive_download_filename(
                None,
                "https://example.com/files/tax-form%202026.pdf?token=abc",
                Some("application/pdf"),
                b"",
            ),
            "tax-form 2026.pdf"
        );
        // Extensionless URL path gets .pdf when MIME is application/pdf.
        assert_eq!(
            derive_download_filename(
                None,
                "https://example.com/statement",
                Some("application/pdf; charset=binary"),
                b"",
            ),
            "statement.pdf"
        );
        // Root path with no Content-Disposition falls back to download.<ext>.
        assert_eq!(
            derive_download_filename(None, "https://example.com/", Some("application/pdf"), b"",),
            "download.pdf"
        );
        // Magic %PDF bytes infer .pdf even without headers.
        assert_eq!(
            derive_download_filename(None, "https://example.com/blob", None, b"%PDF-1.4\n"),
            "blob.pdf"
        );
        assert_eq!(
            resolve_mime(Some("application/octet-stream"), "report.xlsx", b"PK"),
            "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet"
        );
        assert!(is_document_mime("application/pdf; charset=utf-8"));
        assert!(!is_document_mime("text/html; charset=utf-8"));
        assert!(!is_document_mime("application/xhtml+xml"));
    }
}

#[cfg(test)]
mod scripted_tests;
