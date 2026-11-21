//! Live turn activity: one bounded, plain-language event per tool step.
//!
//! The desktop shows what a turn is doing while it runs ("Opening
//! example.com", "Searching the web"). Tool arguments and outputs are
//! untrusted, so the label is built here from the tool name and, at most, the
//! host of a URL argument. Free-form argument text (queries, file contents,
//! credentials) never reaches an event.

use super::*;

/// Longest label an event may carry.
const MAX_LABEL_CHARS: usize = 80;
/// Longest host shown in a label.
const MAX_HOST_CHARS: usize = 48;
/// Longest reasoning text one Thinking step carries.
const THINKING_EVENT_CHARS: usize = 4000;

/// Orchestration event kinds (0.50). They ride the same event stream and SSE
/// feed as `turn.step`. Every payload is small and carries ids and short codes
/// only: no objective, reply or provider text. The desktop looks the rest up by
/// id, so it can coalesce a burst of these into one refresh.
pub(crate) const EVENT_HANDOFF_STARTED: &str = "handoff.started";
pub(crate) const EVENT_HANDOFF_COMPLETED: &str = "handoff.completed";
pub(crate) const EVENT_HANDOFF_FAILED: &str = "handoff.failed";
pub(crate) const EVENT_MODEL_FAILOVER: &str = "model.failover";
pub(crate) const EVENT_HOLD_SET: &str = "provider.hold_set";
pub(crate) const EVENT_HOLD_CLEARED: &str = "provider.hold_cleared";
pub(crate) const EVENT_WAITING_MODEL: &str = "task.waiting_model";

/// Where a handoff event points.
pub(crate) struct HandoffRef<'a> {
    pub handoff_id: &'a str,
    pub child_task_id: &'a str,
    pub parent_task_id: &'a str,
    pub to_bot: &'a str,
    pub conversation_id: &'a str,
}

/// Payload of `handoff.started` and `handoff.completed`.
pub(crate) fn handoff_payload(handoff: &HandoffRef<'_>) -> Value {
    json!({
        "handoff_id": handoff.handoff_id,
        "child_task_id": handoff.child_task_id,
        "parent_task_id": handoff.parent_task_id,
        "to_bot": handoff.to_bot,
        "conversation_id": handoff.conversation_id,
    })
}

/// Payload of `handoff.failed`: the ids plus the outcome, so the sidebar can
/// show "failed, resumable" without another call.
pub(crate) fn handoff_failed_payload(
    handoff: &HandoffRef<'_>,
    status: &str,
    outcome_code: &str,
    effects_unknown: bool,
    resume_count: i64,
) -> Value {
    let mut payload = handoff_payload(handoff);
    payload["status"] = json!(status);
    payload["outcome_code"] = json!(outcome_code);
    payload["effects_unknown"] = json!(effects_unknown);
    payload["resume_count"] = json!(resume_count);
    payload
}

/// Payload of `model.failover`.
pub(crate) fn failover_payload(
    conversation_id: &str,
    task_id: Option<&str>,
    bot: &str,
    from: &ModelSelection,
    to: &ModelSelection,
    reason: &str,
) -> Value {
    json!({
        "conversation_id": conversation_id,
        "task_id": task_id,
        "bot": bot,
        "from": {"provider": from.provider, "model": from.model, "account": from.account},
        "to": {"provider": to.provider, "model": to.model, "account": to.account},
        "reason": reason,
    })
}

/// Payload of `provider.hold_set` and `provider.hold_cleared`. `until` is the
/// hold's expiry for a set hold, and absent when it was cleared.
pub(crate) fn hold_payload(
    provider: &str,
    account_id: &str,
    model: &str,
    kind: &str,
    until: Option<&str>,
) -> Value {
    let mut payload = json!({
        "provider": provider,
        "account_id": account_id,
        "model": model,
        "kind": kind,
    });
    if let Some(until) = until {
        payload["until"] = json!(until);
    }
    payload
}

/// Payload of `task.waiting_model`.
pub(crate) fn waiting_model_payload(
    task_id: &str,
    turn_id: &str,
    conversation_id: &str,
    until: &str,
) -> Value {
    json!({
        "task_id": task_id,
        "turn_id": turn_id,
        "conversation_id": conversation_id,
        "until": until,
    })
}

/// What the desktop renders for one step.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct StepView {
    pub icon: &'static str,
    pub label: String,
}

/// Host part of a `url` argument, or `None` when it is absent or not http(s).
fn host_of(args: &Value) -> Option<String> {
    let raw = args.get("url").and_then(Value::as_str)?;
    let parsed = url::Url::parse(raw.trim()).ok()?;
    if !matches!(parsed.scheme(), "http" | "https") {
        return None;
    }
    let host = parsed.host_str()?.trim_start_matches("www.");
    if host.is_empty() || host.chars().any(char::is_control) {
        return None;
    }
    Some(truncate_chars(host, MAX_HOST_CHARS))
}

fn capitalize(word: &str) -> String {
    let mut chars = word.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

/// A file-type icon (`file:<group>`) for the `path` argument, or `None`.
/// The extension is mapped through a fixed whitelist of known types, so no
/// free-form argument text ever reaches an event.
fn file_icon_of(args: &Value) -> Option<&'static str> {
    let path = args.get("path").and_then(Value::as_str)?;
    let file = path.rsplit(['/', '\\']).next()?;
    let (_, ext) = file.rsplit_once('.')?;
    if ext.is_empty() || ext.len() > 8 || !ext.chars().all(|c| c.is_ascii_alphanumeric()) {
        return None;
    }
    Some(match ext.to_ascii_lowercase().as_str() {
        "pdf" => "file:pdf",
        "doc" | "docx" | "odt" | "rtf" => "file:word",
        "ppt" | "pptx" | "odp" => "file:powerpoint",
        "xls" | "xlsx" | "xlsm" | "ods" => "file:excel",
        "md" | "markdown" => "file:markdown",
        "png" | "jpg" | "jpeg" | "gif" | "webp" | "svg" | "bmp" | "ico" => "file:image",
        "mp4" | "mov" | "webm" | "mkv" | "avi" => "file:video",
        "mp3" | "wav" | "ogg" | "m4a" | "flac" => "file:audio",
        "zip" | "rar" | "7z" | "tar" | "gz" => "file:zip",
        "json" | "yaml" | "yml" | "toml" | "xml" => "file:json",
        "csv" | "tsv" => "file:csv",
        "txt" | "log" => "file:text",
        _ => return None,
    })
}

/// First line of a tool result, bounded and without control characters.
fn summarize_result(content: &str) -> String {
    let line = content
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or("Finished");
    let cleaned: String = line
        .chars()
        .filter(|character| !character.is_control())
        .collect();
    let cleaned = cleaned.trim();
    if cleaned.is_empty() {
        "Finished".into()
    } else {
        truncate_chars(cleaned, 160)
    }
}

/// A short error for a step card. Drops control characters and anything that
/// looks like a credential.
fn safe_error(error: &str) -> String {
    let cleaned: String = error
        .chars()
        .map(|character| {
            if character.is_control() {
                ' '
            } else {
                character
            }
        })
        .collect();
    let lower = cleaned.to_ascii_lowercase();
    if lower.contains("api_key")
        || lower.contains("api key")
        || lower.contains("token")
        || lower.contains("secret")
        || lower.contains("password")
        || lower.contains("bearer ")
    {
        return "The step failed. Check the connection and try again.".into();
    }
    truncate_chars(cleaned.trim(), 200)
}

fn humanize(tool: &str) -> String {
    let words = tool
        .split(['_', '.', '-'])
        .filter(|word| !word.is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    let mut chars = words.chars();
    match chars.next() {
        Some(first) => format!("Using {}{}", first.to_lowercase(), chars.as_str()),
        None => "Working".to_owned(),
    }
}

/// Office tool verbs (0.68): a specific label per document tool.
fn office_label(tool: &str) -> Option<(&'static str, &'static str)> {
    Some(match tool {
        "xlsx_build" => ("file:excel", "Building a spreadsheet"),
        "xlsx_read" => ("file:excel", "Reading a spreadsheet"),
        "xlsx_inspect" => ("file:excel", "Looking through a spreadsheet"),
        "xlsx_edit" => ("file:excel", "Editing a spreadsheet"),
        "xlsx_import" => ("file:excel", "Opening a spreadsheet"),
        "xlsx_analyze" | "timesheet_analyze" => ("file:excel", "Analyzing a spreadsheet"),
        "docx_build" => ("file:word", "Writing a Word document"),
        "research_report_build" => ("file:word", "Building the research report"),
        "docx_read" => ("file:word", "Reading a Word document"),
        "docx_inspect" => ("file:word", "Looking through a Word document"),
        "docx_edit" => ("file:word", "Editing a Word document"),
        "docx_import" => ("file:word", "Opening a Word document"),
        "docx_pdf" => ("file:pdf", "Converting a Word document to PDF"),
        "pptx_edit" => ("file:powerpoint", "Editing a presentation"),
        "pptx_inspect" | "pptx_slides" => ("file:powerpoint", "Looking through a presentation"),
        "pptx_import" => ("file:powerpoint", "Opening a presentation"),
        "pptx_pdf" => ("file:pdf", "Converting a presentation to PDF"),
        "deck_build" => ("file:powerpoint", "Building a presentation"),
        "pdf_edit" => ("file:pdf", "Editing a PDF"),
        "pdf_inspect" => ("file:pdf", "Reading a PDF"),
        "pdf_merge" => ("file:pdf", "Merging PDFs"),
        "images_to_pdf" => ("file:pdf", "Making a PDF from images"),
        "pdf_to_images" => ("file:pdf", "Taking the pages out of a PDF"),
        "image_crop" => ("file:image", "Cropping an image"),
        "image_rotate" => ("file:image", "Rotating an image"),
        "image_adjust" => ("file:image", "Adjusting an image"),
        "image_compress" => ("file:image", "Making an image smaller"),
        "image_resize" => ("file:image", "Resizing an image"),
        "image_convert" => ("file:image", "Converting an image"),
        _ => return None,
    })
}

/// Edit operations a step card may name, with their plural wording. Only
/// these fixed words and counts reach an event, never cell values or text.
const OFFICE_OPS: &[(&str, &str)] = &[
    ("set_value", "values"),
    ("set_formula", "formulas"),
    ("set_number_format", "number formats"),
    ("set_column_width", "column widths"),
    ("set_row_height", "row heights"),
    ("set_cell_style", "styles"),
    ("merge_cells", "merges"),
    ("freeze_panes", "frozen panes"),
    ("add_sheet", "new sheets"),
    ("rename_sheet", "renamed sheets"),
    ("delete_sheet", "deleted sheets"),
    ("insert_rows", "row inserts"),
    ("delete_rows", "row deletes"),
    ("insert_columns", "column inserts"),
    ("delete_columns", "column deletes"),
    ("add_chart", "charts"),
    ("replace_text", "text replacements"),
    ("insert_paragraph", "new paragraphs"),
    ("delete_paragraph", "deleted paragraphs"),
    ("delete_block", "deleted blocks"),
    ("set_style", "style changes"),
    ("insert_table", "new tables"),
    ("insert_image", "images"),
    ("replace_slide_text", "slide text changes"),
    ("add_slide", "new slides"),
    ("delete_slide", "deleted slides"),
];

/// A spreadsheet range or cell such as `B2` or `A1:D20`, or `None`.
fn safe_range(value: &str) -> Option<String> {
    let value = value.trim();
    let cell = |part: &str| {
        let letters = part.chars().take_while(char::is_ascii_uppercase).count();
        let digits = &part[letters..];
        (1..=3).contains(&letters)
            && !digits.is_empty()
            && digits.len() <= 7
            && digits.chars().all(|c| c.is_ascii_digit())
    };
    let ok = match value.split_once(':') {
        Some((start, end)) => cell(start) && cell(end),
        None => cell(value),
    };
    ok.then(|| value.to_string())
}

/// A short, secret-free line on what an office step is doing (0.68):
/// operation counts by kind, a cell range, or a block span. Built only from
/// fixed words, numbers and strictly checked ranges.
pub(super) fn office_detail(tool: &str, args: &Value) -> Option<String> {
    office_label(tool)?;
    let ops = ["edits", "ops", "operations"]
        .iter()
        .find_map(|key| args.get(*key).and_then(Value::as_array));
    if let Some(ops) = ops {
        let mut counts: Vec<(&str, usize)> = Vec::new();
        let mut other = 0usize;
        for op in ops {
            let name = op.get("op").and_then(Value::as_str).unwrap_or("set_value");
            match OFFICE_OPS.iter().find(|(known, _)| *known == name) {
                Some((_, plural)) => match counts.iter_mut().find(|(p, _)| p == plural) {
                    Some(entry) => entry.1 += 1,
                    None => counts.push((plural, 1)),
                },
                None => other += 1,
            }
        }
        if other > 0 {
            counts.push(("other changes", other));
        }
        counts.sort_by_key(|entry| std::cmp::Reverse(entry.1));
        let total = ops.len();
        let parts = counts
            .iter()
            .take(4)
            .map(|(plural, count)| format!("{count} {plural}"))
            .collect::<Vec<_>>()
            .join(", ");
        let noun = if total == 1 { "change" } else { "changes" };
        return Some(truncate_chars(&format!("{total} {noun}: {parts}"), 140));
    }
    if let Some(range) = args
        .get("range")
        .and_then(Value::as_str)
        .and_then(safe_range)
    {
        return Some(format!("Cells {range}"));
    }
    let number = |key: &str| args.get(key).and_then(Value::as_u64);
    if let (Some(start), Some(end)) = (
        number("start").or_else(|| number("from")),
        number("end").or_else(|| number("to")),
    ) {
        return Some(format!("Blocks {start}\u{2013}{end}"));
    }
    if let Some(files) = args.get("artifacts").and_then(Value::as_array) {
        return Some(format!("{} files", files.len()));
    }
    None
}

/// What to do next for a failed file step (0.68), from the error's wording.
/// Fixed sentences only; `None` when the error is not about a file.
pub(super) fn file_error_hint(tool: &str, error: &str) -> Option<&'static str> {
    let file_tool = tool.starts_with("local_")
        || tool.starts_with("onedrive_")
        || office_label(tool).is_some()
        || matches!(tool, "document_index" | "telegram_send_document");
    if !file_tool {
        return None;
    }
    let lower = error.to_ascii_lowercase();
    let has = |needles: &[&str]| needles.iter().any(|needle| lower.contains(needle));
    Some(
        if has(&[
            "resourcelocked",
            "locked",
            "being used by another",
            "sharing violation",
            "http 423",
            "(423",
        ]) {
            "The file is open in another app (Excel, Word or OneDrive). Close it there, then try again."
        } else if has(&[
            "not found",
            "no such file",
            "cannot find",
            "itemnotfound",
            "http 404",
            "(404",
        ]) {
            "The file was moved, renamed or deleted. List the folder again and use the current name."
        } else if has(&[
            "permission denied",
            "access is denied",
            "access denied",
            "forbidden",
            "http 403",
            "(403",
            "outside the allowed",
        ]) {
            "SeraBot may not use that location. Pick a file inside an allowed folder (Settings → Local folders) or reconnect OneDrive."
        } else if has(&[
            "namealreadyexists",
            "already exists",
            "http 409",
            "(409",
            "conflict",
        ]) {
            "A file with that name already exists. Choose another name or say to replace it."
        } else if has(&["too large", "exceeds", "larger than", "over the", "limit"]) {
            "The file is too big for this step. Work on part of it (one sheet or a range) or split it."
        } else if has(&["encrypted", "password"]) {
            "The file is password-protected. Save an unprotected copy and try again."
        } else if has(&["macro"]) {
            "Files with macros are refused. Save it as a plain .xlsx, .docx or .pptx first."
        } else if has(&[
            "invalid zip",
            "not a zip",
            "corrupt",
            "malformed",
            "invalid package",
            "unexpected end",
            "could not parse",
            "invalid file",
        ]) {
            "The file looks damaged or is not the type its name says. Open and re-save it in Office, then try again."
        } else if has(&[
            "desktop is not connected",
            "desktop offline",
            "not connected",
            "is offline",
        ]) {
            "Your PC is not connected. Open the SeraBot desktop app, then try again."
        } else if has(&["timed out", "timeout"]) {
            "The file took too long to reach. Try again; for a large file, work on part of it."
        } else if has(&["unsupported", "not supported", "unknown extension"]) {
            "That file type is not supported for this step. Convert it (for example to .xlsx or .pdf) first."
        } else {
            return None;
        },
    )
}

/// A plain label and icon name for a tool call.
pub(super) fn describe_step(tool: &str, args: &Value, bot_label: Option<&str>) -> StepView {
    if let Some((icon, label)) = office_label(tool) {
        return StepView {
            icon,
            label: label.to_string(),
        };
    }
    let (icon, label): (&'static str, String) = match tool {
        "browser_navigate" => (
            "globe",
            host_of(args).map_or_else(|| "Opening a page".into(), |h| format!("Opening {h}")),
        ),
        "browser_snapshot" => ("eye", "Reading the page".into()),
        "browser_click" => ("pointer", "Clicking on the page".into()),
        "browser_type" => ("pointer", "Filling in a form".into()),
        "browser_fill_credentials" => ("key", "Autofilling login credentials".into()),
        "job_search" => ("search", "Searching job boards".into()),
        "job_dossier_checklist" => ("list", "Preparing application checklist".into()),
        "job_profile_get" => ("list", "Reading your job profile".into()),
        "job_profile_set" => ("check", "Saving your job profile".into()),
        "job_watch_now" => ("search", "Checking job sites".into()),
        "job_sources_list" => ("list", "Listing job sources".into()),
        "job_sources_set" => ("check", "Updating job sources".into()),
        "job_application_add" => ("check", "Adding to your applications".into()),
        "job_application_update" => ("check", "Updating an application".into()),
        "job_applications_list" => ("list", "Checking your applications".into()),
        "onedrive_download" => ("file", "Reading a file from OneDrive".into()),
        "calendar_events" => ("calendar", "Checking calendar events".into()),
        "tasks_list" => ("list", "Checking personal tasks".into()),
        "tasks_create" => ("check", "Creating a personal task".into()),
        "browser_press" => ("pointer", "Pressing a key".into()),
        "browser_screenshot" => ("image", "Taking a screenshot".into()),
        "browser_close" => ("globe", "Closing the browser".into()),
        "browser_find" => ("eye", "Looking for an element on the page".into()),
        "browser_wait" => ("eye", "Waiting for the page".into()),
        "browser_select" => ("pointer", "Choosing an option".into()),
        "browser_check" => ("pointer", "Ticking a box".into()),
        "browser_scroll" => ("pointer", "Scrolling the page".into()),
        "browser_hover" => ("pointer", "Hovering over an element".into()),
        "browser_upload" => ("pointer", "Attaching a file to a form".into()),
        "browser_download" => (
            "file",
            host_of(args).map_or_else(
                || "Downloading a file".into(),
                |h| format!("Downloading from {h}"),
            ),
        ),
        "browser_pdf" => ("file:pdf", "Saving the page as a PDF".into()),
        "browser_dismiss_banners" => ("pointer", "Closing a cookie banner".into()),
        "browser_forget_logins" => ("key", "Signing out of all sites".into()),
        "browser_tabs" => ("globe", "Checking the browser tabs".into()),
        "browser_new_tab" => (
            "globe",
            host_of(args).map_or_else(
                || "Opening a new tab".into(),
                |h| format!("Opening {h} in a new tab"),
            ),
        ),
        "fetch_url" | "firecrawl_scrape" | "firecrawl_extract" => (
            "globe",
            host_of(args).map_or_else(|| "Reading a web page".into(), |h| format!("Reading {h}")),
        ),
        "firecrawl_crawl" | "firecrawl_map" => ("globe", "Exploring a website".into()),
        "exa_get_contents" => ("globe", "Reading web pages".into()),
        "tavily_search" | "firecrawl_search" | "exa_search" => {
            ("search", "Searching the web".into())
        }
        "rag_search" => ("search", "Searching your documents".into()),
        "document_index" => (
            file_icon_of(args).unwrap_or("file"),
            "Reading a document".into(),
        ),
        "memory_save" | "memory_update" => ("brain", "Saving to memory".into()),
        "memory_list" | "memory_search" | "memory_delete" => ("brain", "Checking memory".into()),
        "plan_update" => ("bolt", "Updating the plan".into()),
        "skill_list" | "skill_search" | "skill_load" => ("bolt", "Looking up a skill".into()),
        "watch_now" => ("eye", "Checking watched pages".into()),
        "agentmail_send" | "outlook_send" => ("mail", "Sending an email".into()),
        "agentmail_inboxes_list" => ("mail", "Checking the inbox".into()),
        "github_search_repos" => ("search", "Searching GitHub".into()),
        "github_list_issues" | "github_get_issue" => ("bolt", "Reading a GitHub issue".into()),
        "github_create_issue" | "github_comment_issue" => ("bolt", "Writing to GitHub".into()),
        "slack_list_channels" => ("mail", "Checking Slack channels".into()),
        "slack_read_channel" => ("mail", "Reading a Slack channel".into()),
        "slack_post_message" => ("mail", "Posting to Slack".into()),
        "whatsapp_phone_info" => ("mail", "Checking WhatsApp number".into()),
        "whatsapp_list_templates" => ("mail", "Checking WhatsApp templates".into()),
        "whatsapp_send_message" | "whatsapp_send_template" => {
            ("mail", "Sending a WhatsApp message".into())
        }
        "todoist_list_tasks" => ("bolt", "Checking your tasks".into()),
        "todoist_add_task" | "todoist_complete_task" => ("bolt", "Updating your tasks".into()),
        "github_get_file" => (
            file_icon_of(args).unwrap_or("file"),
            "Reading a file on GitHub".into(),
        ),
        "telegram_send_document" | "onedrive_upload" => (
            file_icon_of(args).unwrap_or("file"),
            "Sending a file".into(),
        ),
        "onedrive_list" => ("file", "Listing OneDrive files".into()),
        "onedrive_search" => ("search", "Searching OneDrive".into()),
        "onedrive_recent" => ("file", "Checking recent OneDrive files".into()),
        "onedrive_info" => ("file", "Checking OneDrive file info".into()),
        "local_list" => ("file", "Looking at a folder on your PC".into()),
        "local_search" => ("search", "Searching a folder on your PC".into()),
        "local_stat" => (
            file_icon_of(args).unwrap_or("file"),
            "Checking a file on your PC".into(),
        ),
        "local_read" => (
            file_icon_of(args).unwrap_or("file"),
            "Reading a file on your PC".into(),
        ),
        "local_read_many" => ("file", "Reading several files on your PC".into()),
        "local_extract" => ("file", "Reading documents on your PC".into()),
        "local_brief" => ("file", "Surveying a folder on your PC".into()),
        "local_write" => (
            file_icon_of(args).unwrap_or("file"),
            "Writing a file on your PC".into(),
        ),
        "local_import" => (
            file_icon_of(args).unwrap_or("file"),
            "Copying a file from your PC".into(),
        ),
        "local_export" => (
            file_icon_of(args).unwrap_or("file"),
            "Saving a file to your PC".into(),
        ),
        "vault_list" => ("lock", "Checking the vault".into()),
        "analyst" => ("chart", "Analyzing data".into()),
        "delegate" => (
            "bot",
            bot_label.map_or_else(
                || "Asking a specialist".into(),
                |b| format!("Asking {}", capitalize(b)),
            ),
        ),
        "team_ask" => ("bot", "Asking the team".into()),
        "team_reply" => ("bot", "Replying to the team".into()),
        other if other.starts_with("pdf_") => ("file:pdf", "Working on a PDF".into()),
        other if other.starts_with("image_") => ("file:image", "Working on an image".into()),
        other if other.starts_with("docx_") => ("file:word", "Working on a Word document".into()),
        other if other.starts_with("xlsx_") || other == "timesheet_analyze" => {
            ("file:excel", "Working on a spreadsheet".into())
        }
        other if other.starts_with("pptx_") || other == "deck_build" => {
            ("file:powerpoint", "Working on a presentation".into())
        }
        other if other.starts_with("elevenlabs_") || other.starts_with("audio_") => {
            ("mic", "Working with audio".into())
        }
        other => ("bolt", humanize(other)),
    };
    StepView {
        icon,
        label: truncate_chars(&label, MAX_LABEL_CHARS),
    }
}

/// Phase of one step as the desktop sees it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum StepPhase {
    /// The tool call has started.
    Running,
    /// The call returned, and the work is still going (a generation, for example).
    Progress,
    Done,
    Failed,
    WaitingApproval,
}

impl StepPhase {
    fn as_str(self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Progress => "progress",
            Self::Done => "done",
            Self::Failed => "failed",
            Self::WaitingApproval => "waiting_approval",
        }
    }
}

impl Runtime {
    /// Best-effort `turn.step` event. A failed append never fails the turn:
    /// the activity feed is a convenience, the durable record is the audit log.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn emit_step(
        &self,
        user_id: &str,
        task: &Task,
        turn: &Turn,
        call: &ToolCall,
        phase: StepPhase,
        approval_id: Option<&str>,
        ordinal: usize,
    ) {
        self.emit_step_detail(
            user_id,
            task,
            turn,
            call,
            phase,
            approval_id,
            ordinal,
            None,
            None,
            None,
        );
    }

    /// `turn.step` with the fields clients use for a step card: tool name,
    /// a secret-free input summary, result summary, error, duration, cards
    /// and the current plan. A failed append never fails the turn.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn emit_step_detail(
        &self,
        user_id: &str,
        task: &Task,
        turn: &Turn,
        call: &ToolCall,
        phase: StepPhase,
        approval_id: Option<&str>,
        ordinal: usize,
        duration: Option<std::time::Duration>,
        output: Option<&crate::gateway::ToolOutput>,
        error: Option<&str>,
    ) {
        let bot_label = (call.name == DELEGATE_TOOL)
            .then(|| call.arguments.get("to_bot").and_then(Value::as_str))
            .flatten()
            .filter(|bot| safe_bot_label(bot))
            .map(str::to_owned);
        let mut view = describe_step(&call.name, &call.arguments, bot_label.as_deref());
        if !self.gateway.policy().is_allowed(&view.label) {
            view = StepView {
                icon: "bolt",
                label: "Working".into(),
            };
        }
        let mut payload = json!({
            "task_id": task.id,
            "turn_id": turn.id,
            "conversation_id": turn.conversation_id,
            "bot": turn.bot,
            "call_id": truncate_chars(&call.id, 64),
            "step": ordinal,
            "phase": phase.as_str(),
            "tool": truncate_chars(&call.name, 64),
            "icon": view.icon,
            "label": view.label,
            "input_summary": view.label,
        });
        // 0.68: office steps say what they are changing (counts and ranges).
        if let Some(detail) = office_detail(&call.name, &call.arguments)
            && self.gateway.scan_egress(user_id, &detail).is_ok()
        {
            payload["input_summary"] = json!(detail);
        }
        if let Some(id) = approval_id {
            payload["approval_id"] = json!(id);
        }
        if let Some(duration) = duration {
            payload["duration_ms"] = json!(u64::try_from(duration.as_millis()).unwrap_or(u64::MAX));
        }
        if let Some(output) = output {
            let summary = summarize_result(&output.content);
            if self.gateway.scan_egress(user_id, &summary).is_ok() {
                payload["result_summary"] = json!(summary);
            }
            let cards = crate::result_cards::cards_from_output(output);
            if !cards.is_empty()
                && let Ok(encoded) = serde_json::to_string(&cards)
                && self.gateway.scan_egress(user_id, &encoded).is_ok()
            {
                payload["cards"] = json!(cards);
            }
        }
        if let Some(error) = error {
            let message = safe_error(error);
            if self.gateway.scan_egress(user_id, &message).is_ok() {
                payload["error"] = json!(message);
            } else {
                payload["error"] = json!("The step failed.");
            }
        }
        if let Ok(Some(plan)) = self.store.get_plan(user_id, &turn.conversation_id) {
            let body = crate::plans::plan_json(&plan);
            let mut text = plan.goal.clone();
            for item in &plan.items {
                text.push('\n');
                text.push_str(&item.text);
            }
            if self.gateway.scan_egress(user_id, &text).is_ok() {
                payload["plan"] = body;
            }
        }
        let _ = self.store.append_event(user_id, "turn.step", &payload);
    }

    /// Best-effort collapsed "Thinking" step carrying the reasoning text a
    /// model returned. The text is untrusted display data: it passes the
    /// blocklist like any outgoing text, is bounded, and is dropped (the step
    /// is not emitted) when it does not.
    pub(super) fn emit_thinking(&self, user_id: &str, task: &Task, turn: &Turn, reasoning: &str) {
        let text = truncate_chars(reasoning.trim(), THINKING_EVENT_CHARS);
        if text.is_empty() || self.gateway.scan_egress(user_id, &text).is_err() {
            return;
        }
        let payload = json!({
            "task_id": task.id,
            "turn_id": turn.id,
            "conversation_id": turn.conversation_id,
            "call_id": format!("thinking-{}", new_id()),
            "step": 0,
            "phase": "done",
            "icon": "brain",
            "label": "Thinking",
            "detail": text,
        });
        let _ = self.store.append_event(user_id, "turn.step", &payload);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn office_steps_say_what_changes_without_content() {
        let args = json!({"source_artifact": "abc", "edits": [
            {"op": "set_value", "sheet": "Secret plan", "address": "A1", "value": {"type": "string", "value": "salary 9000"}},
            {"op": "set_value", "sheet": "x", "address": "A2", "value": {"type": "number", "value": 1}},
            {"op": "set_formula", "sheet": "x", "address": "B1", "formula": "=SUM(A1:A2)"},
            {"op": "weird_new_op"}
        ]});
        let detail = office_detail("xlsx_edit", &args).unwrap();
        assert_eq!(detail, "4 changes: 2 values, 1 formulas, 1 other changes");
        assert!(!detail.contains("salary") && !detail.contains("Secret"));
        assert_eq!(
            office_detail("xlsx_read", &json!({"range": "A1:D20"})).unwrap(),
            "Cells A1:D20"
        );
        assert!(office_detail("xlsx_read", &json!({"range": "A1; DROP"})).is_none());
        assert!(office_detail("browser_navigate", &json!({"edits": []})).is_none());
        assert_eq!(
            describe_step("docx_edit", &json!({}), None).label,
            "Editing a Word document"
        );
        assert_eq!(
            describe_step("pdf_merge", &json!({}), None).icon,
            "file:pdf"
        );
    }

    #[test]
    fn file_errors_name_the_next_step() {
        assert!(
            file_error_hint(
                "local_read",
                "The process cannot access the file because it is being used by another process"
            )
            .unwrap()
            .contains("open in another app")
        );
        assert!(
            file_error_hint(
                "onedrive_upload",
                "Graph returned HTTP 409: nameAlreadyExists"
            )
            .unwrap()
            .contains("already exists")
        );
        assert!(
            file_error_hint("xlsx_import", "invalid zip archive")
                .unwrap()
                .contains("damaged")
        );
        assert!(
            file_error_hint("local_read", "path not found")
                .unwrap()
                .contains("moved")
        );
        assert!(file_error_hint("browser_click", "not found").is_none());
        assert!(file_error_hint("local_read", "something odd").is_none());
    }

    #[test]
    fn navigation_label_shows_only_the_host() {
        let view = describe_step(
            "browser_navigate",
            &json!({"url": "https://www.example.com/a/b?token=secret#frag"}),
            None,
        );
        assert_eq!(view.label, "Opening example.com");
        assert_eq!(view.icon, "globe");
    }

    #[test]
    fn labels_never_echo_free_form_arguments() {
        let view = describe_step(
            "tavily_search",
            &json!({"query": "password hunter2 for bank"}),
            None,
        );
        assert_eq!(view.label, "Searching the web");
        let typed = describe_step("browser_type", &json!({"value": "s3cret"}), None);
        assert!(!typed.label.contains("s3cret"));
    }

    #[test]
    fn non_http_urls_and_junk_fall_back_to_a_generic_label() {
        for url in ["file:///etc/passwd", "javascript:alert(1)", "not a url", ""] {
            let view = describe_step("browser_navigate", &json!({ "url": url }), None);
            assert_eq!(view.label, "Opening a page", "{url}");
        }
        let view = describe_step("fetch_url", &json!({"url": 5}), None);
        assert_eq!(view.label, "Reading a web page");
    }

    #[test]
    fn unknown_tools_get_a_humanized_bounded_label() {
        let view = describe_step("some_new_tool", &json!({}), None);
        assert_eq!(view.label, "Using some new tool");
        let long = "x".repeat(300);
        assert!(describe_step(&long, &json!({}), None).label.chars().count() <= MAX_LABEL_CHARS);
    }

    #[test]
    fn file_steps_carry_a_whitelisted_type_icon() {
        let view = describe_step(
            "local_read",
            &json!({"root": "docs", "path": "reports/Q3.PDF"}),
            None,
        );
        assert_eq!(view.icon, "file:pdf");
        let view = describe_step(
            "local_write",
            &json!({"root": "out", "path": "deck\\slides.pptx", "content": "x"}),
            None,
        );
        assert_eq!(view.icon, "file:powerpoint");
        // No extension, junk extension, or missing path fall back to "file".
        for args in [
            json!({"path": "README"}),
            json!({"path": "a.we!rd"}),
            json!({"path": "a.toolongextension"}),
            json!({}),
            json!({"path": 5}),
        ] {
            let view = describe_step("local_read", &args, None);
            assert_eq!(view.icon, "file", "{args}");
        }
        // The icon is a fixed whitelisted word, never argument text.
        let view = describe_step("local_read", &json!({"path": "evil<script>.pdf"}), None);
        assert_eq!(view.icon, "file:pdf");
    }

    #[test]
    fn delegation_names_the_specialist() {
        let view = describe_step("delegate", &json!({}), Some("analyst"));
        assert_eq!(view.label, "Asking Analyst");
        assert_eq!(
            describe_step("delegate", &json!({}), None).label,
            "Asking a specialist"
        );
    }
}
