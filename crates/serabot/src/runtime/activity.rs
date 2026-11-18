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
        "browser_fill_credentials" => ("key", "Autofilling login credentials".into()),
        "job_search" => ("search", "Searching job boards".into()),
        "job_dossier_checklist" => ("list", "Preparing application checklist".into()),
        "calendar_events" => ("calendar", "Checking calendar events".into()),
        "tasks_list" => ("list", "Checking personal tasks".into()),
        "tasks_create" => ("check", "Creating a personal task".into()),
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
