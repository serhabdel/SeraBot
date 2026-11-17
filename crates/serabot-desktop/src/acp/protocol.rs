//! Agent Client Protocol (ACP) messages: newline-delimited JSON-RPC 2.0 over
//! the agent's stdio. Only the parts SeraBot uses are modelled; everything
//! else an agent sends is ignored or answered "method not found".
//!
//! Everything here is pure, so it is tested without a process.

use serde_json::{Value, json};

/// The ACP protocol version this client speaks.
pub const PROTOCOL_VERSION: u64 = 1;
/// JSON-RPC code ACP uses for "sign in first".
pub const AUTH_REQUIRED: i64 = -32000;
/// JSON-RPC "method not found".
pub const METHOD_NOT_FOUND: i64 = -32601;
/// Longest error text shown from the agent.
const ERROR_CHARS: usize = 400;

/// A JSON-RPC error from the agent.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RpcError {
    pub code: i64,
    pub message: String,
}

/// One line the agent wrote.
#[derive(Clone, Debug, PartialEq)]
pub enum Incoming {
    /// The answer to one of our requests.
    Response {
        id: u64,
        result: Result<Value, RpcError>,
    },
    /// The agent asks the client for something (permission, a file).
    Request {
        id: Value,
        method: String,
        params: Value,
    },
    /// A one-way message (session updates).
    Notification { method: String, params: Value },
}

/// Parse one line. Lines that are not JSON-RPC are dropped.
pub fn parse_line(line: &str) -> Option<Incoming> {
    let value: Value = serde_json::from_str(line.trim()).ok()?;
    let object = value.as_object()?;
    let method = object.get("method").and_then(Value::as_str);
    let id = object.get("id").filter(|id| !id.is_null());
    match (method, id) {
        (Some(method), Some(id)) => Some(Incoming::Request {
            id: id.clone(),
            method: method.to_owned(),
            params: object.get("params").cloned().unwrap_or(Value::Null),
        }),
        (Some(method), None) => Some(Incoming::Notification {
            method: method.to_owned(),
            params: object.get("params").cloned().unwrap_or(Value::Null),
        }),
        (None, Some(id)) => {
            let id = id.as_u64()?;
            let result = match object.get("error") {
                Some(error) => Err(RpcError {
                    code: error.get("code").and_then(Value::as_i64).unwrap_or(0),
                    message: clean_text(
                        error
                            .get("message")
                            .and_then(Value::as_str)
                            .unwrap_or("The agent refused the request."),
                        ERROR_CHARS,
                    ),
                }),
                None => Ok(object.get("result").cloned().unwrap_or(Value::Null)),
            };
            Some(Incoming::Response { id, result })
        }
        (None, None) => None,
    }
}

/// Printable text, at most `max` characters.
pub fn clean_text(text: &str, max: usize) -> String {
    text.chars()
        .filter(|c| !c.is_control() || matches!(c, '\n' | '\t'))
        .take(max)
        .collect()
}

fn line(value: Value) -> String {
    let mut text = value.to_string();
    text.push('\n');
    text
}

/// A request line.
pub fn request(id: u64, method: &str, params: Value) -> String {
    line(json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}))
}

/// A notification line.
pub fn notification(method: &str, params: Value) -> String {
    line(json!({"jsonrpc": "2.0", "method": method, "params": params}))
}

/// The answer to an agent's request.
pub fn response(id: &Value, result: Value) -> String {
    line(json!({"jsonrpc": "2.0", "id": id, "result": result}))
}

/// A refusal of an agent's request.
pub fn error_response(id: &Value, code: i64, message: &str) -> String {
    line(json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}}))
}

/// `initialize`: file reads and writes through the client, no terminal.
pub fn initialize_params(client_version: &str) -> Value {
    json!({
        "protocolVersion": PROTOCOL_VERSION,
        "clientCapabilities": {
            "fs": {"readTextFile": true, "writeTextFile": true},
            "terminal": false
        },
        "clientInfo": {"name": "serabot-desktop", "title": "SeraBot", "version": client_version}
    })
}

/// `session/new` in `cwd`, with no MCP servers.
pub fn new_session_params(cwd: &str) -> Value {
    json!({"cwd": cwd, "mcpServers": []})
}

/// `session/prompt` with one text block.
pub fn prompt_params(session_id: &str, text: &str) -> Value {
    json!({"sessionId": session_id, "prompt": [{"type": "text", "text": text}]})
}

/// `session/cancel`.
pub fn cancel_params(session_id: &str) -> Value {
    json!({"sessionId": session_id})
}

/// One step of a plan.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlanEntry {
    pub content: String,
    /// `pending`, `in_progress` or `completed`.
    pub status: String,
}

/// What a `session/update` carries, reduced to what the chat shows.
#[derive(Clone, Debug, PartialEq)]
pub enum Update {
    AgentText(String),
    Thought(String),
    UserText(String),
    ToolCall {
        id: String,
        title: String,
        kind: String,
        status: String,
    },
    ToolUpdate {
        id: String,
        title: Option<String>,
        status: Option<String>,
    },
    Plan(Vec<PlanEntry>),
    Other,
}

/// Longest single text chunk kept.
const CHUNK_CHARS: usize = 64 * 1024;
/// Longest tool title kept.
const TITLE_CHARS: usize = 200;

fn str_at<'a>(value: &'a Value, key: &str) -> Option<&'a str> {
    value.get(key).and_then(Value::as_str)
}

fn chunk_text(update: &Value) -> String {
    let content = update.get("content").unwrap_or(&Value::Null);
    match str_at(content, "type") {
        Some("text") => clean_text(str_at(content, "text").unwrap_or_default(), CHUNK_CHARS),
        Some(_) => String::new(),
        None => String::new(),
    }
}

/// Parse `session/update` params into the session id and the update.
pub fn parse_update(params: &Value) -> Option<(String, Update)> {
    let session = str_at(params, "sessionId")?.to_owned();
    let update = params.get("update")?;
    let parsed = match str_at(update, "sessionUpdate")? {
        "agent_message_chunk" => Update::AgentText(chunk_text(update)),
        "agent_thought_chunk" => Update::Thought(chunk_text(update)),
        "user_message_chunk" => Update::UserText(chunk_text(update)),
        "tool_call" => Update::ToolCall {
            id: str_at(update, "toolCallId")?.to_owned(),
            title: clean_text(str_at(update, "title").unwrap_or("Tool"), TITLE_CHARS),
            kind: clean_text(str_at(update, "kind").unwrap_or("other"), 32),
            status: clean_text(str_at(update, "status").unwrap_or("pending"), 32),
        },
        "tool_call_update" => Update::ToolUpdate {
            id: str_at(update, "toolCallId")?.to_owned(),
            title: str_at(update, "title").map(|title| clean_text(title, TITLE_CHARS)),
            status: str_at(update, "status").map(|status| clean_text(status, 32)),
        },
        "plan" => Update::Plan(
            update
                .get("entries")
                .and_then(Value::as_array)
                .map(|entries| {
                    entries
                        .iter()
                        .take(50)
                        .map(|entry| PlanEntry {
                            content: clean_text(
                                str_at(entry, "content").unwrap_or_default(),
                                TITLE_CHARS,
                            ),
                            status: clean_text(str_at(entry, "status").unwrap_or("pending"), 32),
                        })
                        .collect()
                })
                .unwrap_or_default(),
        ),
        _ => Update::Other,
    };
    Some((session, parsed))
}

/// One choice the agent offers on a permission request.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PermissionOption {
    pub id: String,
    pub name: String,
    /// `allow_once`, `allow_always`, `reject_once` or `reject_always`.
    pub kind: String,
}

impl PermissionOption {
    pub fn allows(&self) -> bool {
        self.kind.starts_with("allow")
    }
    /// SeraBot never offers "always": every action is approved one at a time.
    pub fn always(&self) -> bool {
        self.kind.ends_with("always")
    }
}

/// `session/request_permission`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PermissionRequest {
    pub session_id: String,
    pub tool_call_id: String,
    pub title: String,
    pub kind: String,
    pub options: Vec<PermissionOption>,
}

impl PermissionRequest {
    /// The choices shown: one-time choices only.
    pub fn shown_options(&self) -> Vec<PermissionOption> {
        self.options
            .iter()
            .filter(|option| !option.always())
            .cloned()
            .collect()
    }
}

pub fn parse_permission(params: &Value) -> Option<PermissionRequest> {
    let tool = params.get("toolCall").unwrap_or(&Value::Null);
    Some(PermissionRequest {
        session_id: str_at(params, "sessionId")?.to_owned(),
        tool_call_id: str_at(tool, "toolCallId").unwrap_or_default().to_owned(),
        title: clean_text(
            str_at(tool, "title").unwrap_or("Claude Code asks to use a tool"),
            TITLE_CHARS,
        ),
        kind: clean_text(str_at(tool, "kind").unwrap_or("other"), 32),
        options: params
            .get("options")
            .and_then(Value::as_array)?
            .iter()
            .take(8)
            .filter_map(|option| {
                Some(PermissionOption {
                    id: str_at(option, "optionId")?.to_owned(),
                    name: clean_text(str_at(option, "name").unwrap_or("Choose"), 60),
                    kind: str_at(option, "kind").unwrap_or_default().to_owned(),
                })
            })
            .collect(),
    })
}

/// The answer to a permission request: the chosen option, or cancelled.
pub fn permission_result(option_id: Option<&str>) -> Value {
    match option_id {
        Some(id) => json!({"outcome": {"outcome": "selected", "optionId": id}}),
        None => json!({"outcome": {"outcome": "cancelled"}}),
    }
}

/// `fs/read_text_file` or `fs/write_text_file` params.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileRequest {
    pub path: String,
    pub line: Option<usize>,
    pub limit: Option<usize>,
    pub content: Option<String>,
}

pub fn parse_file_request(params: &Value) -> Option<FileRequest> {
    Some(FileRequest {
        path: str_at(params, "path")?.to_owned(),
        line: params
            .get("line")
            .and_then(Value::as_u64)
            .map(|n| n as usize),
        limit: params
            .get("limit")
            .and_then(Value::as_u64)
            .map(|n| n as usize),
        content: str_at(params, "content").map(str::to_owned),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lines_split_into_responses_requests_and_notifications() {
        assert_eq!(
            parse_line(r#"{"jsonrpc":"2.0","id":3,"result":{"sessionId":"s"}}"#),
            Some(Incoming::Response {
                id: 3,
                result: Ok(json!({"sessionId": "s"}))
            })
        );
        assert_eq!(
            parse_line(
                r#"{"jsonrpc":"2.0","id":4,"error":{"code":-32000,"message":"Auth\u0007 required"}}"#
            ),
            Some(Incoming::Response {
                id: 4,
                result: Err(RpcError {
                    code: AUTH_REQUIRED,
                    message: "Auth required".into()
                })
            })
        );
        assert!(matches!(
            parse_line(r#"{"jsonrpc":"2.0","id":"x","method":"fs/read_text_file","params":{"path":"/a"}}"#),
            Some(Incoming::Request { method, .. }) if method == "fs/read_text_file"
        ));
        assert!(matches!(
            parse_line(r#"{"jsonrpc":"2.0","method":"session/update","params":{}}"#),
            Some(Incoming::Notification { .. })
        ));
        assert_eq!(parse_line("not json"), None);
        assert_eq!(parse_line(r#"{"jsonrpc":"2.0"}"#), None);
    }

    #[test]
    fn updates_reduce_to_what_the_chat_shows() {
        let text = json!({"sessionId": "s", "update": {"sessionUpdate": "agent_message_chunk",
            "content": {"type": "text", "text": "Hi"}}});
        assert_eq!(
            parse_update(&text),
            Some(("s".into(), Update::AgentText("Hi".into())))
        );
        let tool = json!({"sessionId": "s", "update": {"sessionUpdate": "tool_call",
            "toolCallId": "t1", "title": "Read main.rs", "kind": "read", "status": "pending"}});
        assert!(
            matches!(parse_update(&tool), Some((_, Update::ToolCall { id, .. })) if id == "t1")
        );
        let plan = json!({"sessionId": "s", "update": {"sessionUpdate": "plan",
            "entries": [{"content": "Step", "status": "completed", "priority": "high"}]}});
        assert_eq!(
            parse_update(&plan).unwrap().1,
            Update::Plan(vec![PlanEntry {
                content: "Step".into(),
                status: "completed".into()
            }])
        );
        let other =
            json!({"sessionId": "s", "update": {"sessionUpdate": "available_commands_update"}});
        assert_eq!(parse_update(&other).unwrap().1, Update::Other);
    }

    #[test]
    fn permission_requests_never_show_always() {
        let params = json!({"sessionId": "s", "toolCall": {"toolCallId": "t", "title": "Edit a.rs", "kind": "edit"},
        "options": [
            {"optionId": "a1", "name": "Allow", "kind": "allow_once"},
            {"optionId": "a2", "name": "Always allow", "kind": "allow_always"},
            {"optionId": "r1", "name": "Reject", "kind": "reject_once"}
        ]});
        let request = parse_permission(&params).unwrap();
        let shown: Vec<_> = request
            .shown_options()
            .into_iter()
            .map(|option| option.id)
            .collect();
        assert_eq!(shown, vec!["a1", "r1"]);
        assert_eq!(
            permission_result(Some("a1")),
            json!({"outcome": {"outcome": "selected", "optionId": "a1"}})
        );
        assert_eq!(
            permission_result(None),
            json!({"outcome": {"outcome": "cancelled"}})
        );
    }

    #[test]
    fn outgoing_lines_are_one_json_object_each() {
        let line = request(1, "initialize", initialize_params("0.54.0"));
        assert!(line.ends_with('\n') && !line.trim_end().contains('\n'));
        let value: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(value["params"]["clientCapabilities"]["terminal"], false);
        assert_eq!(value["params"]["protocolVersion"], 1);
    }
}
