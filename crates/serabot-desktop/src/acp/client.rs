//! The ACP client: one Claude Code agent process, one session, one folder.
//!
//! The desktop starts the pinned adapter (see [`super::setup`]), speaks ACP on
//! its stdio and turns what the agent says into [`Event`]s the page drains.
//! The agent's file requests are answered only inside the chosen local folder,
//! with the same checks as the server-relayed folder tools (no `..`, no links,
//! no credential-like names, writes only in a read-write folder). Every
//! permission request waits for the person; "always" choices are never offered.

use std::collections::{HashMap, HashSet};
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::Child;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};

use serde_json::{Value, json};

use super::protocol::{
    self, AUTH_REQUIRED, FileRequest, Incoming, METHOD_NOT_FOUND, PermissionRequest, Update,
};

/// Largest line read from the agent; longer lines are dropped.
const MAX_LINE: usize = 16 * 1024 * 1024;
/// Largest file the agent may read through the client.
const MAX_READ: u64 = 1024 * 1024;

/// What the page learns from the agent.
#[derive(Clone, Debug, PartialEq)]
pub enum Event {
    /// The session is open; prompts can be sent.
    Ready,
    Update(Update),
    /// The agent asks before acting. Answer with [`AcpClient::answer`].
    Permission {
        request_id: Value,
        request: PermissionRequest,
    },
    /// A prompt finished (`end_turn`, `cancelled`, `max_tokens`, `refusal`, ...).
    TurnEnded(String),
    /// Claude Code is not signed in on this computer.
    AuthRequired,
    Failed(String),
    /// A file the agent read or wrote through SeraBot.
    FileUsed {
        path: String,
        write: bool,
        ok: bool,
    },
    /// The agent process ended.
    Exited,
}

/// The one folder a session may touch.
#[derive(Clone, Debug)]
pub struct Workspace {
    pub root: PathBuf,
    pub writable: bool,
}

#[derive(Clone, Copy, Debug)]
enum Pending {
    Initialize,
    NewSession,
    Prompt,
}

struct Shared {
    writer: Mutex<Option<Box<dyn Write + Send>>>,
    next_id: AtomicU64,
    pending: Mutex<HashMap<u64, Pending>>,
    session: Mutex<Option<String>>,
    busy: AtomicBool,
    open_permissions: Mutex<HashSet<String>>,
    events: Sender<Event>,
    workspace: Workspace,
}

impl Shared {
    fn emit(&self, event: Event) {
        let _ = self.events.send(event);
    }

    fn write_line(&self, line: &str) -> bool {
        let mut guard = self.writer.lock().unwrap_or_else(|e| e.into_inner());
        let Some(writer) = guard.as_mut() else {
            return false;
        };
        writer
            .write_all(line.as_bytes())
            .and_then(|()| writer.flush())
            .is_ok()
    }

    fn send_request(&self, method: &str, params: Value, kind: Pending) -> bool {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        self.pending
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(id, kind);
        if self.write_line(&protocol::request(id, method, params)) {
            true
        } else {
            self.pending
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .remove(&id);
            false
        }
    }

    fn session(&self) -> Option<String> {
        self.session
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }
}

/// One running agent.
pub struct AcpClient {
    shared: Arc<Shared>,
    child: Mutex<Option<Child>>,
}

impl AcpClient {
    /// Start the pinned adapter in `workspace` with the `npx` found by setup.
    pub fn spawn(npx: &Path, workspace: Workspace) -> Result<(Self, Receiver<Event>), String> {
        let mut child = super::setup::adapter_command(npx, &workspace.root)
            .spawn()
            .map_err(|error| format!("Could not start Claude Code: {}", error.kind()))?;
        let stdin = child.stdin.take().ok_or("Could not talk to Claude Code.")?;
        let stdout = child
            .stdout
            .take()
            .ok_or("Could not talk to Claude Code.")?;
        let (client, events) = Self::connect(stdout, stdin, workspace);
        *client.child.lock().unwrap_or_else(|e| e.into_inner()) = Some(child);
        Ok((client, events))
    }

    /// Speak ACP over any pair of streams (the process's stdio, or a test
    /// agent's pipes). Sends `initialize`; `session/new` follows its answer.
    pub fn connect(
        reader: impl Read + Send + 'static,
        writer: impl Write + Send + 'static,
        workspace: Workspace,
    ) -> (Self, Receiver<Event>) {
        let (events, receiver) = channel();
        let shared = Arc::new(Shared {
            writer: Mutex::new(Some(Box::new(writer))),
            next_id: AtomicU64::new(1),
            pending: Mutex::new(HashMap::new()),
            session: Mutex::new(None),
            busy: AtomicBool::new(false),
            open_permissions: Mutex::new(HashSet::new()),
            events,
            workspace,
        });
        let reader_shared = Arc::clone(&shared);
        std::thread::spawn(move || read_loop(reader_shared, reader));
        if !shared.send_request(
            "initialize",
            protocol::initialize_params(env!("CARGO_PKG_VERSION")),
            Pending::Initialize,
        ) {
            shared.emit(Event::Failed("Could not talk to Claude Code.".into()));
        }
        (
            Self {
                shared,
                child: Mutex::new(None),
            },
            receiver,
        )
    }

    pub fn has_session(&self) -> bool {
        self.shared.session().is_some()
    }

    pub fn is_busy(&self) -> bool {
        self.shared.busy.load(Ordering::SeqCst)
    }

    /// Open a new session (after signing in, or to start over).
    pub fn new_session(&self) -> Result<(), String> {
        let cwd = self.shared.workspace.root.to_string_lossy().into_owned();
        if self.shared.send_request(
            "session/new",
            protocol::new_session_params(&cwd),
            Pending::NewSession,
        ) {
            Ok(())
        } else {
            Err("Claude Code is not running.".into())
        }
    }

    /// Send one message. One prompt runs at a time.
    pub fn prompt(&self, text: &str) -> Result<(), String> {
        self.prompt_with_images(text, &[])
    }

    pub fn prompt_with_images(
        &self,
        text: &str,
        images: &[protocol::PromptImage],
    ) -> Result<(), String> {
        let text = text.trim();
        if text.is_empty() && images.is_empty() {
            return Err("Type a message or attach an image first.".into());
        }
        let session = self
            .shared
            .session()
            .ok_or("Claude Code is still starting.")?;
        if self.shared.busy.swap(true, Ordering::SeqCst) {
            return Err("Claude Code is still working on the last message.".into());
        }
        if self.shared.send_request(
            "session/prompt",
            protocol::prompt_params_with_images(&session, text, images),
            Pending::Prompt,
        ) {
            Ok(())
        } else {
            self.shared.busy.store(false, Ordering::SeqCst);
            Err("Claude Code is not running.".into())
        }
    }

    /// Stop the running prompt. Open permission requests are answered
    /// "cancelled", as ACP requires.
    pub fn cancel(&self) {
        let open: Vec<String> = self
            .shared
            .open_permissions
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .drain()
            .collect();
        for id in open {
            if let Ok(id) = serde_json::from_str::<Value>(&id) {
                self.shared
                    .write_line(&protocol::response(&id, protocol::permission_result(None)));
            }
        }
        if let Some(session) = self.shared.session() {
            self.shared.write_line(&protocol::notification(
                "session/cancel",
                protocol::cancel_params(&session),
            ));
        }
    }

    /// Answer a permission request with one of its options, or `None` to
    /// decline. An "always" option is refused: it is never offered.
    pub fn answer(&self, request_id: &Value, request: &PermissionRequest, option_id: Option<&str>) {
        let key = request_id.to_string();
        let was_open = self
            .shared
            .open_permissions
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&key);
        if !was_open {
            return;
        }
        let chosen = option_id.filter(|id| {
            request
                .shown_options()
                .iter()
                .any(|option| option.id == *id)
        });
        self.shared.write_line(&protocol::response(
            request_id,
            protocol::permission_result(chosen),
        ));
    }

    /// End the agent: close its input, then stop the process tree.
    pub fn shutdown(&self) {
        self.shared
            .writer
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take();
        if let Some(mut child) = self.child.lock().unwrap_or_else(|e| e.into_inner()).take() {
            super::setup::stop_tree(&mut child);
        }
    }
}

impl Drop for AcpClient {
    fn drop(&mut self) {
        self.shutdown();
    }
}

fn read_loop(shared: Arc<Shared>, reader: impl Read) {
    let mut reader = BufReader::new(reader);
    let mut buffer = Vec::new();
    loop {
        buffer.clear();
        match (&mut reader)
            .take(MAX_LINE as u64 + 1)
            .read_until(b'\n', &mut buffer)
        {
            Ok(0) | Err(_) => break,
            Ok(_) if buffer.len() > MAX_LINE => {
                // Skip the rest of an oversized line.
                if buffer.last() != Some(&b'\n') {
                    let mut sink = Vec::new();
                    let _ = reader.read_until(b'\n', &mut sink);
                }
                continue;
            }
            Ok(_) => {}
        }
        let Ok(line) = std::str::from_utf8(&buffer) else {
            continue;
        };
        if let Some(incoming) = protocol::parse_line(line) {
            handle(&shared, incoming);
        }
    }
    shared.busy.store(false, Ordering::SeqCst);
    shared
        .pending
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clear();
    shared.emit(Event::Exited);
}

fn handle(shared: &Shared, incoming: Incoming) {
    match incoming {
        Incoming::Response { id, result } => {
            let kind = shared
                .pending
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .remove(&id);
            let Some(kind) = kind else {
                return;
            };
            match (kind, result) {
                (_, Err(error)) if error.code == AUTH_REQUIRED => {
                    shared.busy.store(false, Ordering::SeqCst);
                    shared.emit(Event::AuthRequired);
                }
                (Pending::Prompt, Err(error)) => {
                    shared.busy.store(false, Ordering::SeqCst);
                    shared.emit(Event::Failed(error.message));
                }
                (_, Err(error)) => shared.emit(Event::Failed(error.message)),
                (Pending::Initialize, Ok(result)) => {
                    let version = result
                        .get("protocolVersion")
                        .and_then(Value::as_u64)
                        .unwrap_or(0);
                    if version != protocol::PROTOCOL_VERSION {
                        shared.emit(Event::Failed(format!(
                            "This Claude Code adapter speaks ACP version {version}; SeraBot speaks version {}.",
                            protocol::PROTOCOL_VERSION
                        )));
                        return;
                    }
                    let cwd = shared.workspace.root.to_string_lossy().into_owned();
                    shared.send_request(
                        "session/new",
                        protocol::new_session_params(&cwd),
                        Pending::NewSession,
                    );
                }
                (Pending::NewSession, Ok(result)) => {
                    match result.get("sessionId").and_then(Value::as_str) {
                        Some(session) => {
                            *shared.session.lock().unwrap_or_else(|e| e.into_inner()) =
                                Some(session.to_owned());
                            shared.emit(Event::Ready);
                        }
                        None => {
                            shared.emit(Event::Failed("Claude Code did not open a session.".into()))
                        }
                    }
                }
                (Pending::Prompt, Ok(result)) => {
                    shared.busy.store(false, Ordering::SeqCst);
                    let reason = result
                        .get("stopReason")
                        .and_then(Value::as_str)
                        .unwrap_or("end_turn");
                    shared.emit(Event::TurnEnded(protocol::clean_text(reason, 32)));
                }
            }
        }
        Incoming::Notification { method, params } => {
            if method == "session/update"
                && let Some((session, update)) = protocol::parse_update(&params)
                && shared.session().as_deref() == Some(session.as_str())
            {
                shared.emit(Event::Update(update));
            }
        }
        Incoming::Request { id, method, params } => match method.as_str() {
            "session/request_permission" => match protocol::parse_permission(&params) {
                Some(request) if shared.session().as_deref() == Some(&request.session_id) => {
                    shared
                        .open_permissions
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .insert(id.to_string());
                    shared.emit(Event::Permission {
                        request_id: id,
                        request,
                    });
                }
                _ => {
                    shared.write_line(&protocol::response(&id, protocol::permission_result(None)));
                }
            },
            "fs/read_text_file" | "fs/write_text_file" => {
                let write = method == "fs/write_text_file";
                let answer = protocol::parse_file_request(&params)
                    .ok_or_else(|| "invalid file request".to_string())
                    .and_then(|request| {
                        let shown = request.path.clone();
                        let result = if write {
                            write_file(&shared.workspace, &request).map(|()| Value::Null)
                        } else {
                            read_file(&shared.workspace, &request)
                                .map(|content| json!({ "content": content }))
                        };
                        shared.emit(Event::FileUsed {
                            path: protocol::clean_text(&shown, 300),
                            write,
                            ok: result.is_ok(),
                        });
                        result
                    });
                let line = match answer {
                    Ok(result) => protocol::response(&id, result),
                    Err(message) => protocol::error_response(&id, -32602, &message),
                };
                shared.write_line(&line);
            }
            _ => {
                shared.write_line(&protocol::error_response(
                    &id,
                    METHOD_NOT_FOUND,
                    "SeraBot does not offer this.",
                ));
            }
        },
    }
}

/// `C:\a\b` and `\\?\C:\a\b` name the same place.
fn plain(path: &Path) -> PathBuf {
    let text = path.to_string_lossy();
    PathBuf::from(text.strip_prefix(r"\\?\").unwrap_or(&text).to_string())
}

/// The agent's absolute path as a path relative to the workspace root, with
/// `/` separators, or an error when it is elsewhere.
pub fn relative_in(workspace: &Workspace, path: &str) -> Result<String, String> {
    let requested = plain(Path::new(path));
    if !requested.is_absolute() {
        return Err("the path must be absolute".into());
    }
    let mut roots = vec![plain(&workspace.root)];
    if let Ok(canonical) = workspace.root.canonicalize() {
        roots.push(plain(&canonical));
    }
    for root in roots {
        if let Ok(rest) = requested.strip_prefix(&root) {
            let parts: Vec<String> = rest
                .components()
                .map(|part| part.as_os_str().to_string_lossy().into_owned())
                .collect();
            if parts.is_empty() {
                return Err("the folder itself is not a file".into());
            }
            return Ok(parts.join("/"));
        }
    }
    Err("the path is outside the folder you chose for this chat".into())
}

fn read_file(workspace: &Workspace, request: &FileRequest) -> Result<String, String> {
    let rel = relative_in(workspace, &request.path)?;
    let path = crate::local_folders::resolve(&workspace.root, &rel, true)?;
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_default();
    if crate::local_folders::sensitive_name(name) {
        return Err("credential-like files are never read".into());
    }
    let meta = std::fs::metadata(&path).map_err(|_| "could not be opened".to_string())?;
    if !meta.is_file() {
        return Err("not a file".into());
    }
    if meta.len() > MAX_READ {
        return Err("the file is larger than 1 MiB".into());
    }
    let text = std::fs::read_to_string(&path).map_err(|_| "not a text file".to_string())?;
    Ok(match (request.line, request.limit) {
        (None, None) => text,
        (line, limit) => {
            let start = line.unwrap_or(1).max(1) - 1;
            let lines = text.lines().skip(start);
            match limit {
                Some(limit) => lines.take(limit).collect::<Vec<_>>().join("\n"),
                None => lines.collect::<Vec<_>>().join("\n"),
            }
        }
    })
}

fn write_file(workspace: &Workspace, request: &FileRequest) -> Result<(), String> {
    if !workspace.writable {
        return Err("this folder is read-only for SeraBot".into());
    }
    let rel = relative_in(workspace, &request.path)?;
    let content = request.content.as_deref().unwrap_or_default();
    crate::local_folders::write(&workspace.root, &rel, content).map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader};
    use std::time::Duration;

    fn temp_root(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "serabot-acp-{name}-{}",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir.canonicalize().unwrap()
    }

    fn next(events: &Receiver<Event>) -> Event {
        events.recv_timeout(Duration::from_secs(5)).expect("event")
    }

    /// A synthetic agent on the other end of two pipes.
    struct Agent {
        reader: BufReader<std::io::PipeReader>,
        writer: std::io::PipeWriter,
    }

    impl Agent {
        fn read(&mut self) -> Value {
            let mut line = String::new();
            self.reader.read_line(&mut line).unwrap();
            serde_json::from_str(&line).unwrap()
        }
        fn send(&mut self, value: Value) {
            let mut text = value.to_string();
            text.push('\n');
            self.writer.write_all(text.as_bytes()).unwrap();
            self.writer.flush().unwrap();
        }
    }

    fn start(workspace: Workspace) -> (AcpClient, Receiver<Event>, Agent) {
        let (agent_in, client_out) = std::io::pipe().unwrap();
        let (client_in, agent_out) = std::io::pipe().unwrap();
        let (client, events) = AcpClient::connect(client_in, client_out, workspace);
        let agent = Agent {
            reader: BufReader::new(agent_in),
            writer: agent_out,
        };
        (client, events, agent)
    }

    fn open_session(agent: &mut Agent, events: &Receiver<Event>, root: &Path) {
        let init = agent.read();
        assert_eq!(init["method"], "initialize");
        assert_eq!(init["params"]["clientCapabilities"]["terminal"], false);
        agent.send(json!({"jsonrpc": "2.0", "id": init["id"], "result": {"protocolVersion": 1}}));
        let new = agent.read();
        assert_eq!(new["method"], "session/new");
        assert_eq!(new["params"]["cwd"], root.to_string_lossy().as_ref());
        agent.send(json!({"jsonrpc": "2.0", "id": new["id"], "result": {"sessionId": "s1"}}));
        assert_eq!(next(events), Event::Ready);
    }

    #[test]
    fn a_turn_streams_reads_inside_the_folder_and_waits_for_permission() {
        let root = temp_root("turn");
        std::fs::write(root.join("notes.txt"), "one\ntwo\nthree").unwrap();
        std::fs::write(root.join(".env"), "SECRET=synthetic").unwrap();
        let (client, events, mut agent) = start(Workspace {
            root: root.clone(),
            writable: false,
        });
        open_session(&mut agent, &events, &root);

        client.prompt("summarise notes").unwrap();
        assert!(client.prompt("again").is_err(), "one prompt at a time");
        let prompt = agent.read();
        assert_eq!(prompt["method"], "session/prompt");
        assert_eq!(prompt["params"]["prompt"][0]["text"], "summarise notes");

        agent.send(json!({"jsonrpc": "2.0", "method": "session/update", "params": {"sessionId": "s1",
            "update": {"sessionUpdate": "agent_message_chunk", "content": {"type": "text", "text": "Reading"}}}}));
        assert_eq!(
            next(&events),
            Event::Update(Update::AgentText("Reading".into()))
        );
        // An update for another session is ignored.
        agent.send(json!({"jsonrpc": "2.0", "method": "session/update", "params": {"sessionId": "other",
            "update": {"sessionUpdate": "agent_message_chunk", "content": {"type": "text", "text": "x"}}}}));

        let inside = root.join("notes.txt").to_string_lossy().into_owned();
        agent.send(
            json!({"jsonrpc": "2.0", "id": 90, "method": "fs/read_text_file",
            "params": {"sessionId": "s1", "path": inside, "line": 2, "limit": 1}}),
        );
        assert!(matches!(
            next(&events),
            Event::FileUsed {
                ok: true,
                write: false,
                ..
            }
        ));
        let answer = agent.read();
        assert_eq!(answer["id"], 90);
        assert_eq!(answer["result"]["content"], "two");

        let secret = root.join(".env").to_string_lossy().into_owned();
        agent.send(
            json!({"jsonrpc": "2.0", "id": 91, "method": "fs/read_text_file",
            "params": {"sessionId": "s1", "path": secret}}),
        );
        assert!(matches!(next(&events), Event::FileUsed { ok: false, .. }));
        assert!(agent.read()["error"].is_object());

        let outside = std::env::temp_dir()
            .join("elsewhere.txt")
            .to_string_lossy()
            .into_owned();
        agent.send(
            json!({"jsonrpc": "2.0", "id": 92, "method": "fs/write_text_file",
            "params": {"sessionId": "s1", "path": outside, "content": "x"}}),
        );
        assert!(matches!(
            next(&events),
            Event::FileUsed {
                ok: false,
                write: true,
                ..
            }
        ));
        assert!(agent.read()["error"].is_object());

        agent.send(
            json!({"jsonrpc": "2.0", "id": 93, "method": "session/request_permission",
            "params": {"sessionId": "s1", "toolCall": {"toolCallId": "t", "title": "Run tests"},
                "options": [{"optionId": "ok", "name": "Allow", "kind": "allow_once"},
                            {"optionId": "always", "name": "Always", "kind": "allow_always"}]}}),
        );
        let Event::Permission {
            request_id,
            request,
        } = next(&events)
        else {
            panic!("permission")
        };
        // "Always" is never accepted, even if asked for.
        client.answer(&request_id, &request, Some("always"));
        assert_eq!(
            agent.read()["result"],
            json!({"outcome": {"outcome": "cancelled"}})
        );
        // A second answer to the same request is not sent.
        client.answer(&request_id, &request, Some("ok"));

        agent.send(
            json!({"jsonrpc": "2.0", "id": prompt["id"], "result": {"stopReason": "end_turn"}}),
        );
        assert_eq!(next(&events), Event::TurnEnded("end_turn".into()));
        assert!(!client.is_busy());

        agent.send(json!({"jsonrpc": "2.0", "id": 94, "method": "terminal/create", "params": {}}));
        assert_eq!(agent.read()["error"]["code"], METHOD_NOT_FOUND);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn writes_need_a_read_write_folder_and_stay_inside_it() {
        let root = temp_root("write");
        let (_client, events, mut agent) = start(Workspace {
            root: root.clone(),
            writable: true,
        });
        open_session(&mut agent, &events, &root);
        let target = root.join("out.md").to_string_lossy().into_owned();
        agent.send(
            json!({"jsonrpc": "2.0", "id": 7, "method": "fs/write_text_file",
            "params": {"sessionId": "s1", "path": target, "content": "# hi"}}),
        );
        assert!(matches!(
            next(&events),
            Event::FileUsed {
                ok: true,
                write: true,
                ..
            }
        ));
        assert!(agent.read()["result"].is_null());
        assert_eq!(
            std::fs::read_to_string(root.join("out.md")).unwrap(),
            "# hi"
        );
        let script = root.join("run.ps1").to_string_lossy().into_owned();
        agent.send(
            json!({"jsonrpc": "2.0", "id": 8, "method": "fs/write_text_file",
            "params": {"sessionId": "s1", "path": script, "content": "x"}}),
        );
        assert!(matches!(next(&events), Event::FileUsed { ok: false, .. }));
        assert!(agent.read()["error"].is_object());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn sign_in_and_exit_are_reported() {
        let root = temp_root("auth");
        let (client, events, mut agent) = start(Workspace {
            root: root.clone(),
            writable: false,
        });
        let init = agent.read();
        agent.send(json!({"jsonrpc": "2.0", "id": init["id"], "result": {"protocolVersion": 1}}));
        let new = agent.read();
        agent.send(json!({"jsonrpc": "2.0", "id": new["id"],
            "error": {"code": AUTH_REQUIRED, "message": "Authentication required"}}));
        assert_eq!(next(&events), Event::AuthRequired);
        assert!(!client.has_session());
        assert!(client.prompt("hi").is_err());
        drop(agent);
        assert_eq!(next(&events), Event::Exited);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn a_different_protocol_version_is_refused() {
        let root = temp_root("version");
        let (_client, events, mut agent) = start(Workspace {
            root: root.clone(),
            writable: false,
        });
        let init = agent.read();
        agent.send(json!({"jsonrpc": "2.0", "id": init["id"], "result": {"protocolVersion": 2}}));
        assert!(matches!(next(&events), Event::Failed(message) if message.contains("version 2")));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn paths_resolve_only_inside_the_folder() {
        let root = temp_root("paths");
        let workspace = Workspace {
            root: root.clone(),
            writable: false,
        };
        assert_eq!(
            relative_in(&workspace, &root.join("a").join("b.txt").to_string_lossy()).unwrap(),
            "a/b.txt"
        );
        assert!(relative_in(&workspace, &root.to_string_lossy()).is_err());
        assert!(relative_in(&workspace, "relative.txt").is_err());
        assert!(
            relative_in(
                &workspace,
                &std::env::temp_dir().join("x").to_string_lossy()
            )
            .is_err()
        );
        let _ = std::fs::remove_dir_all(root);
    }
}
