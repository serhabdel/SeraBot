//! The 0.70 browser tools: find, wait, select, check, scroll, hover, upload,
//! download, pdf, dismiss_banners and forget_logins. All of them act on the
//! acting member's own session and profile; refs come from the latest
//! snapshot (or from `browser_find`, which takes a fresh one). Page content is
//! untrusted data, like everywhere in the browser.

use std::time::{Duration, Instant};

use async_trait::async_trait;
use chrono::Utc;
use serde_json::{Value, json};

use super::banners;
use super::chromium::actions::{clean_download_name, resolve_mime};
use super::guard::{host_of, validate_navigation_url, validate_resolved};
use super::manager::{Driver, Lease, LoadState};
use super::outline::find_in_outline;
use super::tools::{BrowserToolDeps, UNTRUSTED_BANNER, opt_str, page_lines, str_arg, text_output};
use crate::domain::Language;
use crate::error::{Result, SeraError};
use crate::gateway::{ApprovalDetails, Tool, ToolContext, ToolOutput, ToolSpec};
use crate::maker::ArtifactMeta;
use crate::util::truncate_chars;

/// Largest file `browser_download` saves, `browser_pdf` prints or
/// `browser_upload` sends.
pub const MAX_FILE_BYTES: u64 = 25 * 1024 * 1024;
const MAX_WAIT_SECS: u64 = 30;
const FIND_LIMIT: usize = 20;
const MAX_ARG_CHARS: usize = 200;
const SCROLL_MAX_PIXELS: f64 = 20_000.0;
const PDF_PREVIEW_CHARS: usize = 1500;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Find,
    Wait,
    Select,
    Check,
    Scroll,
    Hover,
    Upload,
    Download,
    Pdf,
    DismissBanners,
    ForgetLogins,
}

pub(super) fn register_ext_tools(
    registry: &mut crate::gateway::ToolRegistry,
    deps: BrowserToolDeps,
) -> Result<()> {
    for kind in [
        Kind::Find,
        Kind::Wait,
        Kind::Select,
        Kind::Check,
        Kind::Scroll,
        Kind::Hover,
        Kind::Upload,
        Kind::Download,
        Kind::Pdf,
        Kind::DismissBanners,
        Kind::ForgetLogins,
    ] {
        registry.register(ExtTool {
            kind,
            deps: deps.clone(),
        })?;
    }
    Ok(())
}

struct ExtTool {
    kind: Kind,
    deps: BrowserToolDeps,
}

fn bounded(args: &Value, key: &str) -> Result<Option<String>> {
    match opt_str(args, key).map(str::trim).filter(|v| !v.is_empty()) {
        Some(value) if value.chars().count() > MAX_ARG_CHARS => Err(SeraError::Validation(
            format!("{key} is too long (at most {MAX_ARG_CHARS} characters)"),
        )),
        Some(value) => Ok(Some(value.to_owned())),
        None => Ok(None),
    }
}

fn timeout_secs(args: &Value, default: u64) -> Result<u64> {
    match args.get("timeout_secs") {
        None | Some(Value::Null) => Ok(default),
        Some(value) => value
            .as_u64()
            .filter(|secs| (1..=MAX_WAIT_SECS).contains(secs))
            .ok_or_else(|| {
                SeraError::Validation(format!("timeout_secs must be 1 to {MAX_WAIT_SECS}"))
            }),
    }
}

fn ref_arg(args: &Value, key: &str) -> Result<String> {
    let value = str_arg(args, key)?;
    super::outline::parse_ref(value)?;
    Ok(value.to_owned())
}

enum DownloadTarget {
    Ref(String),
    Url(String),
    Current,
}

fn parse_download_target(args: &Value) -> Result<DownloadTarget> {
    let reference = opt_str(args, "ref")
        .map(str::trim)
        .filter(|v| !v.is_empty());
    let url = opt_str(args, "url")
        .map(str::trim)
        .filter(|v| !v.is_empty());
    let current = match args.get("current") {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(_) => {
            return Err(SeraError::Validation("current must be a boolean".into()));
        }
    };
    let modes =
        usize::from(reference.is_some()) + usize::from(url.is_some()) + usize::from(current);
    if modes != 1 {
        return Err(SeraError::Validation(
            "give exactly one of ref, url, or current=true".into(),
        ));
    }
    if let Some(reference) = reference {
        super::outline::parse_ref(reference)?;
        Ok(DownloadTarget::Ref(reference.to_owned()))
    } else if let Some(url) = url {
        validate_navigation_url(url)?;
        Ok(DownloadTarget::Url(url.to_owned()))
    } else {
        Ok(DownloadTarget::Current)
    }
}

fn next_tool_hint(filename: &str, mime: &str, hash: &str) -> String {
    let ext = std::path::Path::new(filename)
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase)
        .unwrap_or_default();
    if ext == "pdf" || mime == "application/pdf" {
        format!(
            "Next: inspect pages and safety with pdf_inspect{{artifact: \"{hash}\"}}, or index it for cited search with document_index."
        )
    } else if ext == "docx"
        || mime == "application/vnd.openxmlformats-officedocument.wordprocessingml.document"
    {
        format!(
            "Next: open and read it with docx_import{{artifact: \"{hash}\"}} (then docx_read / docx_inspect)."
        )
    } else if ext == "xlsx"
        || ext == "csv"
        || mime == "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet"
        || mime == "text/csv"
    {
        format!(
            "Next: open and analyze it with xlsx_import{{artifact: \"{hash}\"}} (then xlsx_read / xlsx_analyze)."
        )
    } else if ext == "pptx"
        || mime == "application/vnd.openxmlformats-officedocument.presentationml.presentation"
    {
        format!(
            "Next: inspect it with pptx_inspect{{artifact: \"{hash}\"}} or pptx_import{{artifact: \"{hash}\"}}."
        )
    } else {
        format!(
            "Next: use artifact \"{hash}\" with a matching document tool or send it with telegram_send_document."
        )
    }
}

enum PdfPreview {
    Text(String),
    EmptyText,
    Unreadable,
}

fn pdf_first_page_preview(bytes: &[u8], max_chars: usize) -> PdfPreview {
    let pages = match crate::documents::extract_pdf_page_texts_bounded(bytes, 1, |_| true) {
        Ok((pages, _)) => pages,
        Err(SeraError::Validation(msg)) if msg.contains("no extractable text") => {
            return PdfPreview::EmptyText;
        }
        Err(_) => return PdfPreview::Unreadable,
    };
    let Some(first) = pages.into_iter().next().map(|(_, text)| text) else {
        return PdfPreview::EmptyText;
    };
    let collapsed = first.split_whitespace().collect::<Vec<_>>().join(" ");
    let trimmed = collapsed.trim();
    if trimmed.is_empty() {
        return PdfPreview::EmptyText;
    }
    let truncated = truncate_chars(trimmed, max_chars);
    if trimmed.chars().count() > max_chars {
        PdfPreview::Text(format!("{truncated}..."))
    } else {
        PdfPreview::Text(truncated)
    }
}

fn save_artifact(
    deps: &BrowserToolDeps,
    lease: &Lease,
    bytes: &[u8],
    filename: &str,
    mime_hint: Option<&str>,
    what: &str,
) -> Result<ToolOutput> {
    let store = deps
        .artifacts
        .as_ref()
        .ok_or_else(|| SeraError::Config(format!("{what} need the artifact store")))?;
    let stored = store.save(
        bytes,
        filename,
        ArtifactMeta {
            title: filename.to_owned(),
            filename: filename.to_owned(),
            language: Language::En,
            source_artifact: None,
            created_at: Utc::now(),
        },
    )?;
    let mime = resolve_mime(mime_hint, &stored.filename, bytes);
    let hint = next_tool_hint(&stored.filename, &mime, &stored.hash);
    let mut content = format!(
        "Saved as artifact {} ({} bytes, mime: {}, artifact_hash: {}).\n{}",
        stored.filename,
        bytes.len(),
        mime,
        stored.hash,
        hint
    );
    if mime == "application/pdf" || bytes.starts_with(b"%PDF") {
        match pdf_first_page_preview(bytes, PDF_PREVIEW_CHARS) {
            PdfPreview::Text(preview) => {
                let safe_preview = lease.redact(&preview);
                if deps.policy.is_allowed(&safe_preview) {
                    content.push_str(&format!(
                        "\n\n{UNTRUSTED_BANNER}\nFirst page text preview:\n{safe_preview}"
                    ));
                }
            }
            PdfPreview::EmptyText => {
                content.push_str(&format!(
                    "\n\nNote: this PDF has no extractable text (it appears scanned or image-only). Use pdf_to_images{{artifact: \"{}\"}} to render pages as images, or document_index with OCR to read it.",
                    stored.hash
                ));
            }
            PdfPreview::Unreadable => {}
        }
    }
    Ok(ToolOutput {
        content,
        data: Some(json!({
            "bytes": bytes.len(),
            "filename": stored.filename,
            "mime": mime,
            "artifact_hash": stored.hash,
        })),
        artifact_path: Some(format!("artifacts/{}", stored.filename)),
        artifact_hash: Some(stored.hash),
        filename: Some(stored.filename),
        destination: None,
        cards: Vec::new(),
    })
}

impl ExtTool {
    fn name(&self) -> &'static str {
        match self.kind {
            Kind::Find => "browser_find",
            Kind::Wait => "browser_wait",
            Kind::Select => "browser_select",
            Kind::Check => "browser_check",
            Kind::Scroll => "browser_scroll",
            Kind::Hover => "browser_hover",
            Kind::Upload => "browser_upload",
            Kind::Download => "browser_download",
            Kind::Pdf => "browser_pdf",
            Kind::DismissBanners => "browser_dismiss_banners",
            Kind::ForgetLogins => "browser_forget_logins",
        }
    }

    /// The host of the page the session is on (any http or https page).
    async fn current_host(&self, context: &ToolContext) -> Result<String> {
        let profile = self
            .deps
            .store
            .bot_browser_profile(&context.user_id, &context.bot)?;
        let lease = self
            .deps
            .manager
            .bot_lease(&context.user_id, profile.as_str(), false, None)
            .await?;
        let info = lease.backend().info().await?;
        host_of(&info.url)
            .ok_or_else(|| SeraError::Validation("open a page before uploading a file".into()))
    }

    /// The artifact a member may upload: theirs, within the size cap and
    /// allowed by the blocklist. Returns `(hash, filename, bytes)`.
    fn upload_source(&self, args: &Value, context: &ToolContext) -> Result<(String, String, u64)> {
        let hash = str_arg(args, "artifact")?.to_owned();
        if !self
            .deps
            .store
            .user_owns_artifact(&context.user_id, &hash)?
        {
            return Err(SeraError::Forbidden(
                "that artifact does not belong to this account".into(),
            ));
        }
        let artifacts = self
            .deps
            .artifacts
            .as_ref()
            .ok_or_else(|| SeraError::Config("uploads need the artifact store".into()))?;
        let meta = artifacts.meta(&hash)?;
        if !self.deps.policy.is_allowed(&meta.filename) {
            return Err(SeraError::PolicyViolation);
        }
        let size = artifacts.load(&hash)?.bytes.len() as u64;
        if size == 0 || size > MAX_FILE_BYTES {
            return Err(SeraError::Validation(
                "the file is empty or larger than 25 MB".into(),
            ));
        }
        Ok((hash, meta.filename, size))
    }
}

#[async_trait]
impl Tool for ExtTool {
    fn spec(&self) -> ToolSpec {
        let reference = json!({"type":"string","maxLength":8});
        let (description, parameters, external_effect) = match self.kind {
            Kind::Find => (
                "Find elements on the current page by ARIA role and accessible name (role=\"button\", name=\"Accept\") or lines by visible text. Takes a fresh snapshot, so the refs it returns are ready for browser_click, browser_type and the other tools. Cheaper than reading a whole snapshot.",
                json!({"type":"object","properties":{
                    "role":{"type":"string","maxLength":40},
                    "name":{"type":"string","maxLength":MAX_ARG_CHARS},
                    "text":{"type":"string","maxLength":MAX_ARG_CHARS},
                    "limit":{"type":"integer","minimum":1,"maximum":FIND_LIMIT}
                },"additionalProperties":false}),
                false,
            ),
            Kind::Wait => (
                "Wait (at most 30 seconds) for something on the current page: text to appear (text), an element to exist (role and/or name; returns its ref), or the page to finish loading (state=load) or go quiet (state=networkidle). Give exactly one of those.",
                json!({"type":"object","properties":{
                    "text":{"type":"string","maxLength":MAX_ARG_CHARS},
                    "role":{"type":"string","maxLength":40},
                    "name":{"type":"string","maxLength":MAX_ARG_CHARS},
                    "state":{"type":"string","enum":["load","networkidle"]},
                    "timeout_secs":{"type":"integer","minimum":1,"maximum":MAX_WAIT_SECS}
                },"additionalProperties":false}),
                false,
            ),
            Kind::Select => (
                "Choose an option of a dropdown (<select>) by its visible label or value. ref comes from the latest browser_snapshot or browser_find.",
                json!({"type":"object","properties":{
                    "ref":reference,
                    "option":{"type":"string","maxLength":MAX_ARG_CHARS}
                },"required":["ref","option"],"additionalProperties":false}),
                true,
            ),
            Kind::Check => (
                "Tick or untick a checkbox, radio button or switch (checked=true by default). Does nothing if it is already in that state.",
                json!({"type":"object","properties":{
                    "ref":reference,
                    "checked":{"type":"boolean"}
                },"required":["ref"],"additionalProperties":false}),
                true,
            ),
            Kind::Scroll => (
                "Scroll the page (or one scrollable element, with ref) by direction and amount in pixels (default 600), or scroll an element into view with to_ref.",
                json!({"type":"object","properties":{
                    "direction":{"type":"string","enum":["up","down","left","right"]},
                    "amount":{"type":"integer","minimum":1,"maximum":SCROLL_MAX_PIXELS as i64},
                    "ref":reference,
                    "to_ref":reference
                },"additionalProperties":false}),
                false,
            ),
            Kind::Hover => (
                "Move the pointer over an element (to open a menu or tooltip). ref comes from the latest browser_snapshot or browser_find.",
                json!({"type":"object","properties":{"ref":reference},"required":["ref"],"additionalProperties":false}),
                false,
            ),
            Kind::Upload => (
                "Set a file input to one of the owner's artifacts (by artifact hash). This sends the owner's file to the website, so it needs the owner's approval for that site. ref is the file field from browser_snapshot or browser_find.",
                json!({"type":"object","properties":{
                    "ref":reference,
                    "artifact":{"type":"string","maxLength":80}
                },"required":["ref","artifact"],"additionalProperties":false}),
                true,
            ),
            Kind::Download => (
                "Download a file (at most 30 seconds, up to 25 MB) into an artifact for the owner and return its hash and next reading tool. Give exactly one of: ref (click a link/button, also saves PDFs that open in a viewer), url (fetch a public http(s) URL in the current browser session with cookies), or current=true (save the PDF/document currently open in the active tab).",
                json!({"type":"object","properties":{
                    "ref":reference,
                    "url":{"type":"string","maxLength":2048},
                    "current":{"type":"boolean"},
                    "timeout_secs":{"type":"integer","minimum":1,"maximum":MAX_WAIT_SECS}
                },"additionalProperties":false}),
                true,
            ),
            Kind::Pdf => (
                "Save the current page as a PDF artifact for the owner, returning its artifact hash and a short preview if text is extractable (use pdf_inspect on the hash for page details).",
                json!({"type":"object","properties":{},"additionalProperties":false}),
                false,
            ),
            Kind::DismissBanners => (
                "Close a cookie or consent banner on the current page: rejects non-essential cookies when offered, else accepts only the necessary ones. Never clicks anything that looks like a purchase or a form submit. Banners are also dismissed automatically after browser_navigate.",
                json!({"type":"object","properties":{},"additionalProperties":false}),
                true,
            ),
            Kind::ForgetLogins => (
                "Sign out of every website in this browser profile: closes the browser and deletes its saved cookies and site data. Needs the owner's approval.",
                json!({"type":"object","properties":{},"additionalProperties":false}),
                true,
            ),
        };
        ToolSpec {
            name: self.name().into(),
            description: description.into(),
            parameters,
            external_effect,
        }
    }

    fn requires_approval(&self, _args: &Value) -> bool {
        matches!(self.kind, Kind::Upload | Kind::ForgetLogins)
    }

    fn approval_details(&self, args: &Value) -> Result<ApprovalDetails> {
        match self.kind {
            Kind::ForgetLogins => Ok(ApprovalDetails {
                summary:
                    "Sign out of all websites in the server browser and delete its saved cookies"
                        .into(),
                destination: None,
                artifact_hash: None,
            }),
            Kind::Upload => {
                let reference = truncate_chars(str_arg(args, "ref")?, 16);
                Ok(ApprovalDetails {
                    summary: format!("Upload a file into {reference} on the current page"),
                    destination: None,
                    artifact_hash: str_arg(args, "artifact").ok().map(str::to_owned),
                })
            }
            Kind::Download => match parse_download_target(args)? {
                DownloadTarget::Ref(reference) => Ok(ApprovalDetails {
                    summary: format!(
                        "Download file from {} in the server browser",
                        truncate_chars(&reference, 16)
                    ),
                    destination: None,
                    artifact_hash: None,
                }),
                DownloadTarget::Url(url) => Ok(ApprovalDetails {
                    summary: format!("Download {url} in the server browser"),
                    destination: Some(url),
                    artifact_hash: None,
                }),
                DownloadTarget::Current => Ok(ApprovalDetails {
                    summary: "Save the current browser document as an artifact".into(),
                    destination: None,
                    artifact_hash: None,
                }),
            },
            _ => Ok(ApprovalDetails::default()),
        }
    }

    async fn approval_details_for(
        &self,
        args: &Value,
        context: &ToolContext,
    ) -> Result<ApprovalDetails> {
        if self.kind != Kind::Upload {
            return self.approval_details(args);
        }
        let reference = truncate_chars(&ref_arg(args, "ref")?, 16);
        let (hash, filename, size) = self.upload_source(args, context)?;
        let host = self.current_host(context).await?;
        Ok(ApprovalDetails {
            summary: format!(
                "Upload \"{}\" ({} KB) into {reference} on {host}",
                truncate_chars(&filename, 80),
                size.div_ceil(1024)
            ),
            destination: Some(host),
            artifact_hash: Some(hash),
        })
    }

    async fn live_destination(
        &self,
        args: &Value,
        context: &ToolContext,
    ) -> Result<Option<String>> {
        if self.kind != Kind::Upload {
            return Ok(None);
        }
        let _ = args;
        Ok(Some(self.current_host(context).await?))
    }

    async fn auto_approve_allowed(&self, _args: &Value, _context: &ToolContext) -> bool {
        // Sending a member file to a site, or wiping logins, is the owner's call.
        !matches!(self.kind, Kind::Upload | Kind::ForgetLogins)
    }

    fn allow_standing_grant(&self, _args: &Value) -> bool {
        !matches!(self.kind, Kind::Upload | Kind::ForgetLogins)
    }

    async fn execute(&self, args: Value, context: &ToolContext) -> Result<ToolOutput> {
        let manager = &self.deps.manager;
        let user = context.user_id.as_str();
        let profile = self.deps.store.bot_browser_profile(user, &context.bot)?;
        let profile = profile.as_str();
        let driver = Some(Driver {
            bot: context.bot.as_str(),
            conversation_id: context.conversation_id.as_deref(),
        });
        if self.kind == Kind::ForgetLogins {
            let existed = manager.forget_logins(user, profile).await?;
            let _ = self.deps.store.touch_browser_profile(user, profile);
            return Ok(ToolOutput {
                content: if existed {
                    "Signed out of every website in this browser profile; the saved cookies are deleted."
                        .into()
                } else {
                    "This browser profile had no saved logins.".into()
                },
                data: Some(json!({"forgotten": existed})),
                ..Default::default()
            });
        }
        let lease = manager.bot_lease(user, profile, false, driver).await?;
        let backend = lease.backend().clone();
        match self.kind {
            Kind::Find => {
                let role = bounded(&args, "role")?;
                let name = bounded(&args, "name")?;
                let text = bounded(&args, "text")?;
                if role.is_none() && name.is_none() && text.is_none() {
                    return Err(SeraError::Validation(
                        "give a role and/or name, or some visible text".into(),
                    ));
                }
                let limit = args
                    .get("limit")
                    .and_then(Value::as_u64)
                    .map_or(10, |n| (n as usize).clamp(1, FIND_LIMIT));
                let (info, outline) = backend.snapshot().await?;
                let hits = find_in_outline(
                    &outline,
                    role.as_deref(),
                    name.as_deref(),
                    text.as_deref(),
                    limit,
                );
                let lines: Vec<String> = hits
                    .iter()
                    .map(|hit| match &hit.reference {
                        Some(reference) => format!("{reference} {}", hit.line),
                        None => format!("(text, not an element) {}", hit.line),
                    })
                    .collect();
                let body = if lines.is_empty() {
                    format!("{}\n---\nno match", page_lines(&info))
                } else {
                    format!("{}\n---\n{}", page_lines(&info), lines.join("\n"))
                };
                Ok(text_output(
                    &lease,
                    body,
                    json!({"matches": hits.len(), "refs": hits.iter().filter(|h| h.reference.is_some()).count()}),
                ))
            }
            Kind::Wait => {
                let text = bounded(&args, "text")?;
                let role = bounded(&args, "role")?;
                let name = bounded(&args, "name")?;
                let state = opt_str(&args, "state");
                let element = role.is_some() || name.is_some();
                let modes = usize::from(text.is_some())
                    + usize::from(element)
                    + usize::from(state.is_some());
                if modes != 1 {
                    return Err(SeraError::Validation(
                        "give exactly one of text, role/name, or state".into(),
                    ));
                }
                let limit = Duration::from_secs(timeout_secs(&args, 10)?);
                let started = Instant::now();
                if let Some(state) = state {
                    let state = match state {
                        "load" => LoadState::Load,
                        "networkidle" => LoadState::NetworkIdle,
                        _ => {
                            return Err(SeraError::Validation(
                                "state must be load or networkidle".into(),
                            ));
                        }
                    };
                    let reached = backend.wait_load_state(state, limit).await?;
                    return Ok(text_output(
                        &lease,
                        if reached {
                            format!(
                                "the page is ready ({:.1}s)",
                                started.elapsed().as_secs_f32()
                            )
                        } else {
                            format!(
                                "timed out after {}s; the page is still busy",
                                limit.as_secs()
                            )
                        },
                        json!({"reached": reached}),
                    ));
                }
                loop {
                    let (info, outline) = backend.snapshot().await?;
                    let found = if let Some(wanted) = &text {
                        outline
                            .text
                            .to_lowercase()
                            .contains(&wanted.to_lowercase())
                            .then(|| "the text is on the page".to_owned())
                    } else {
                        let hits =
                            find_in_outline(&outline, role.as_deref(), name.as_deref(), None, 5);
                        (!hits.is_empty()).then(|| {
                            hits.iter()
                                .filter_map(|hit| {
                                    hit.reference.as_ref().map(|r| format!("{r} {}", hit.line))
                                })
                                .collect::<Vec<_>>()
                                .join("\n")
                        })
                    };
                    if let Some(found) = found {
                        return Ok(text_output(
                            &lease,
                            format!(
                                "found after {:.1}s\n{}\n---\n{found}",
                                started.elapsed().as_secs_f32(),
                                page_lines(&info)
                            ),
                            json!({"found": true}),
                        ));
                    }
                    if started.elapsed() >= limit {
                        return Ok(text_output(
                            &lease,
                            format!(
                                "timed out after {}s; not found\n{}",
                                limit.as_secs(),
                                page_lines(&info)
                            ),
                            json!({"found": false}),
                        ));
                    }
                    tokio::time::sleep(Duration::from_millis(400)).await;
                }
            }
            Kind::Select => {
                let reference = ref_arg(&args, "ref")?;
                let option = bounded(&args, "option")?
                    .ok_or_else(|| SeraError::Validation("option is required".into()))?;
                let chosen = backend.select_ref(&reference, &option).await?;
                Ok(text_output(
                    &lease,
                    format!(
                        "selected \"{}\" in {reference}",
                        truncate_chars(&chosen, 80)
                    ),
                    json!({"selected": true}),
                ))
            }
            Kind::Check => {
                let reference = ref_arg(&args, "ref")?;
                let wanted = args.get("checked").and_then(Value::as_bool).unwrap_or(true);
                let state = backend.set_checked(&reference, wanted).await?;
                Ok(text_output(
                    &lease,
                    format!(
                        "{reference} is now {}",
                        if state { "checked" } else { "unchecked" }
                    ),
                    json!({"checked": state}),
                ))
            }
            Kind::Scroll => {
                if let Some(to) = opt_str(&args, "to_ref") {
                    if args.get("direction").is_some()
                        || args.get("amount").is_some()
                        || args.get("ref").is_some()
                    {
                        return Err(SeraError::Validation(
                            "to_ref cannot be combined with direction, amount or ref".into(),
                        ));
                    }
                    super::outline::parse_ref(to)?;
                    backend.scroll_to_ref(to).await?;
                    return Ok(text_output(
                        &lease,
                        format!("scrolled {to} into view"),
                        json!({"scrolled": true}),
                    ));
                }
                let direction = opt_str(&args, "direction").unwrap_or("down");
                let amount = match args.get("amount") {
                    None | Some(Value::Null) => 600.0,
                    Some(value) => value
                        .as_f64()
                        .filter(|n| (1.0..=SCROLL_MAX_PIXELS).contains(n))
                        .ok_or_else(|| {
                            SeraError::Validation("amount must be 1 to 20000 pixels".into())
                        })?,
                };
                let (dx, dy) = match direction {
                    "down" => (0.0, amount),
                    "up" => (0.0, -amount),
                    "right" => (amount, 0.0),
                    "left" => (-amount, 0.0),
                    other => {
                        return Err(SeraError::Validation(format!(
                            "direction must be up, down, left or right (got '{other}')"
                        )));
                    }
                };
                let reference = match opt_str(&args, "ref") {
                    Some(reference) => {
                        super::outline::parse_ref(reference)?;
                        Some(reference)
                    }
                    None => None,
                };
                let at = backend.scroll_by(reference, dx, dy).await?;
                let bottom = at.y + at.view >= at.height - 2.0;
                Ok(text_output(
                    &lease,
                    format!(
                        "scrolled {direction} {amount:.0}px; now at {:.0} of {:.0}px{}",
                        at.y,
                        at.height,
                        if bottom { " (bottom reached)" } else { "" }
                    ),
                    json!({"y": at.y, "x": at.x, "height": at.height, "at_bottom": bottom}),
                ))
            }
            Kind::Hover => {
                let reference = ref_arg(&args, "ref")?;
                backend.hover_ref(&reference).await?;
                Ok(text_output(
                    &lease,
                    format!("hovering over {reference}"),
                    json!({"hovered": true}),
                ))
            }
            Kind::Upload => {
                let reference = ref_arg(&args, "ref")?;
                let (hash, filename, _) = self.upload_source(&args, context)?;
                let artifacts =
                    self.deps.artifacts.as_ref().ok_or_else(|| {
                        SeraError::Config("uploads need the artifact store".into())
                    })?;
                let stored = artifacts.load(&hash)?;
                // A private temp folder inside the browser's own space, removed
                // whatever happens.
                let dir = manager
                    .temp_dir(user)?
                    .join(format!("ul-{}", uuid::Uuid::new_v4().simple()));
                std::fs::create_dir_all(&dir)?;
                let path = dir.join(clean_download_name(&filename));
                let result = async {
                    std::fs::write(&path, &stored.bytes)?;
                    backend.upload_ref(&reference, &path).await
                }
                .await;
                let _ = std::fs::remove_dir_all(&dir);
                result?;
                Ok(text_output(
                    &lease,
                    format!(
                        "attached \"{}\" to {reference}; submit the form to send it",
                        truncate_chars(&filename, 80)
                    ),
                    json!({"uploaded": true, "artifact": hash}),
                ))
            }
            Kind::Download => {
                let target = parse_download_target(&args)?;
                let limit = Duration::from_secs(timeout_secs(&args, 15)?);
                let file = match target {
                    DownloadTarget::Ref(reference) => {
                        backend
                            .click_download(&reference, limit, MAX_FILE_BYTES)
                            .await?
                    }
                    DownloadTarget::Url(url) => {
                        let resolved = validate_resolved(&url).await?;
                        backend
                            .download_url(resolved.as_str(), limit, MAX_FILE_BYTES)
                            .await?
                    }
                    DownloadTarget::Current => {
                        backend.download_current(limit, MAX_FILE_BYTES).await?
                    }
                };
                if file.bytes.is_empty() {
                    return Err(SeraError::Validation("the downloaded file is empty".into()));
                }
                let filename = clean_download_name(&file.filename);
                if !self.deps.policy.is_allowed(&filename) {
                    return Err(SeraError::PolicyViolation);
                }
                save_artifact(
                    &self.deps,
                    &lease,
                    &file.bytes,
                    &filename,
                    Some(&file.mime),
                    "downloads",
                )
            }
            Kind::Pdf => {
                let bytes = backend.print_pdf(MAX_FILE_BYTES as usize).await?;
                let filename = format!("page-{}.pdf", Utc::now().format("%Y%m%d-%H%M%S"));
                save_artifact(
                    &self.deps,
                    &lease,
                    &bytes,
                    &filename,
                    Some("application/pdf"),
                    "PDFs",
                )
            }
            Kind::DismissBanners => {
                let outcome = banners::dismiss(backend.as_ref(), false).await?;
                let (content, data) = match (outcome.dismissed, outcome.action) {
                    (true, Some(action)) => (format!("banner dismissed: {action}"), json!({"dismissed": true})),
                    _ if outcome.candidates > 0 => (
                        "a banner is showing, but it has no safe button to press (only settings or unclear choices); ask the owner or use browser_find".into(),
                        json!({"dismissed": false, "buttons": outcome.candidates}),
                    ),
                    _ => ("no cookie or consent banner found".into(), json!({"dismissed": false})),
                };
                Ok(text_output(&lease, content, data))
            }
            Kind::ForgetLogins => unreachable!("handled before the lease"),
        }
    }
}

#[cfg(test)]
mod tests;
