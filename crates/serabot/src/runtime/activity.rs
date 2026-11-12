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

/// A plain label and icon name for a tool call.
pub(super) fn describe_step(tool: &str, args: &Value, bot_label: Option<&str>) -> StepView {
    let (icon, label): (&'static str, String) = match tool {
        "browser_navigate" => (
            "globe",
            host_of(args).map_or_else(|| "Opening a page".into(), |h| format!("Opening {h}")),
        ),
        "browser_snapshot" => ("eye", "Reading the page".into()),
        "browser_click" => ("pointer", "Clicking on the page".into()),
        "browser_type" => ("pointer", "Filling in a form".into()),
        "browser_press" => ("pointer", "Pressing a key".into()),
        "browser_screenshot" => ("image", "Taking a screenshot".into()),
        "browser_close" => ("globe", "Closing the browser".into()),
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
        "onedrive_list" => ("file", "Listing files".into()),
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
    Running,
    Done,
    Failed,
    WaitingApproval,
}

impl StepPhase {
    fn as_str(self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Done => "done",
            Self::Failed => "failed",
            Self::WaitingApproval => "waiting_approval",
        }
    }
}

/// Optional fields on a `turn.step` event beyond the stable running/done phases.
#[derive(Debug, Clone, Default)]
pub(crate) struct StepExtras {
    pub duration_ms: Option<u64>,
    pub result_preview: Option<String>,
    pub error_message: Option<String>,
    pub structured: Option<Value>,
}

const MAX_PREVIEW_CHARS: usize = 240;

pub(crate) fn preview_tool_output(output: &crate::gateway::ToolOutput) -> Option<String> {
    if let Some(structured) = output.data.as_ref().and_then(|d| d.get("structured"))
        && let Some(preview) = structured.get("preview").and_then(Value::as_str)
    {
        return Some(truncate_chars(preview, MAX_PREVIEW_CHARS));
    }
    let text = output.content.trim();
    if text.is_empty() {
        return None;
    }
    Some(truncate_chars(text, MAX_PREVIEW_CHARS))
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
        extras: StepExtras,
    ) {
        let bot_label = (call.name == DELEGATE_TOOL)
            .then(|| call.arguments.get("to_bot").and_then(Value::as_str))
            .flatten()
            .filter(|bot| safe_bot_label(bot))
            .map(str::to_owned);
        let mut view = describe_step(&call.name, &call.arguments, bot_label.as_deref());
        // The label is derived, not copied, but it still passes the blocklist.
        if !self.gateway.policy().is_allowed(&view.label) {
            view = StepView {
                icon: "bolt",
                label: "Working".into(),
            };
        }
        let input_summary = describe_step(&call.name, &call.arguments, bot_label.as_deref()).label;
        let mut payload = json!({
            "task_id": task.id,
            "turn_id": turn.id,
            "conversation_id": turn.conversation_id,
            "bot": turn.bot,
            "tool": call.name,
            "call_id": truncate_chars(&call.id, 64),
            "step": ordinal,
            "phase": phase.as_str(),
            "icon": view.icon,
            "label": view.label,
            "input_summary": input_summary,
        });
        if let Some(id) = approval_id {
            payload["approval_id"] = json!(id);
        }
        if let Some(ms) = extras.duration_ms {
            payload["duration_ms"] = json!(ms);
        }
        if let Some(preview) = extras.result_preview
            && self.gateway.policy().is_allowed(&preview)
        {
            payload["result_preview"] = json!(truncate_chars(&preview, MAX_PREVIEW_CHARS));
        }
        if let Some(error) = extras.error_message {
            payload["error"] = json!(truncate_chars(&error, MAX_PREVIEW_CHARS));
        }
        if let Some(structured) = extras.structured {
            payload["structured"] = structured;
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
