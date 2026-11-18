//! The Claude Code page's state, kept for the whole app session so a running
//! agent survives switching pages. The page calls these methods and redraws;
//! [`Controller::poll`] drains what the agent and the setup checks reported.

use std::sync::mpsc::{Receiver, TryRecvError, channel};

use serde_json::Value;

use super::client::{AcpClient, Event, Workspace};
use super::protocol::{PermissionRequest, Update};
use super::setup::{self, NodeCheck};
use super::transcript::Transcript;
use crate::local_folders::{FolderMode, LocalFolder};

/// Where the agent stands.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Status {
    /// Nothing started yet (or stopped by the person).
    Idle,
    Starting,
    Ready,
    Working,
    /// Claude Code is not signed in on this computer.
    NeedsSignIn,
    /// The agent ended or failed; it can be started again.
    Stopped(String),
}

impl Status {
    pub fn label(&self) -> &'static str {
        match self {
            Self::Idle => "Not started",
            Self::Starting => "Starting",
            Self::Ready => "Ready",
            Self::Working => "Working",
            Self::NeedsSignIn => "Sign-in needed",
            Self::Stopped(_) => "Stopped",
        }
    }
    pub fn running(&self) -> bool {
        matches!(self, Self::Starting | Self::Ready | Self::Working)
    }
}

/// Something the agent or a setup check reported.
#[derive(Debug)]
pub enum Inbound {
    Node(NodeCheck),
    Event(Event),
}

/// A permission request waiting for the person.
#[derive(Clone, Debug, PartialEq)]
pub struct Ask {
    pub request_id: Value,
    pub request: PermissionRequest,
}

pub struct Controller {
    pub bot_id: Option<String>,
    pub bot_context: Option<String>,
    pub persona_sent: bool,
    pub node: Option<NodeCheck>,
    node_rx: Option<Receiver<NodeCheck>>,
    pub folder_id: Option<String>,
    /// The folder the running session uses (shown while it runs).
    pub folder_label: Option<String>,
    pub writable: bool,
    pub status: Status,
    pub transcript: Transcript,
    pub asks: Vec<Ask>,
    pub notice: Option<String>,
    client: Option<AcpClient>,
    events: Option<Receiver<Event>>,
}

impl Default for Controller {
    fn default() -> Self {
        Self {
            bot_id: None,
            bot_context: None,
            persona_sent: false,
            node: None,
            node_rx: None,
            folder_id: None,
            folder_label: None,
            writable: false,
            status: Status::Idle,
            transcript: Transcript::default(),
            asks: Vec::new(),
            notice: None,
            client: None,
            events: None,
        }
    }
}

impl Drop for Controller {
    fn drop(&mut self) {
        self.stop_quietly();
    }
}

impl Controller {
    /// Build a controller bound to one bot, loading any saved local transcript
    /// and restoring the last selected folder id if one was saved.
    pub fn for_bot(bot_id: String, folder_id: Option<String>, node: Option<NodeCheck>) -> Self {
        let transcript = crate::creds::settings_dir()
            .map(|dir| Transcript::load_for_bot(&dir, &bot_id))
            .unwrap_or_default();
        let mut controller = Self::default();
        controller.bot_id = Some(bot_id);
        controller.folder_id = folder_id;
        controller.node = node;
        controller.transcript = transcript;
        controller
    }

    pub fn set_bot_context(&mut self, context: Option<String>) {
        self.bot_context = context.and_then(|text| {
            let trimmed = text.trim();
            (!trimmed.is_empty()).then(|| trimmed.to_owned())
        });
    }

    /// Format a prompt for ACP: prepends the bot's persona/instructions once
    /// per live session so Claude Code follows the bot's identity without
    /// polluting the displayed user bubble.
    pub fn format_prompt(&self, user_text: &str) -> String {
        let clean = user_text.trim();
        if !self.persona_sent
            && let Some(ctx) = &self.bot_context
            && !ctx.trim().is_empty()
        {
            return format!(
                "[Bot persona context — follow this role and style]\n{}\n\n[User request]\n{}",
                ctx.trim(),
                clean
            );
        }
        clean.to_owned()
    }

    fn save_transcript(&self) {
        let Some(bot_id) = self.bot_id.as_deref() else {
            return;
        };
        if let Some(dir) = crate::creds::settings_dir() {
            let _ = self.transcript.save_for_bot(&dir, bot_id);
        }
    }

    /// Look for Node.js in the background (once, or again on request).
    pub fn check_node(&mut self) {
        if self.node_rx.is_some() {
            return;
        }
        let (sender, receiver) = channel();
        std::thread::spawn(move || {
            let _ = sender.send(setup::check_node());
        });
        self.node_rx = Some(receiver);
    }

    pub fn checking_node(&self) -> bool {
        self.node_rx.is_some()
    }

    /// Start Claude Code in `folder`.
    pub fn start(&mut self, folder: &LocalFolder) {
        let Some(npx) = self
            .node
            .as_ref()
            .and_then(NodeCheck::npx)
            .map(ToOwned::to_owned)
        else {
            self.notice = Some("Install Node.js 22 or newer first.".into());
            return;
        };
        self.stop_quietly();
        self.persona_sent = false;
        let workspace = Workspace {
            root: std::path::PathBuf::from(&folder.path),
            writable: folder.mode == FolderMode::ReadWrite,
        };
        self.writable = workspace.writable;
        match AcpClient::spawn(&npx, workspace) {
            Ok((client, events)) => {
                self.client = Some(client);
                self.events = Some(events);
                self.folder_id = Some(folder.id.clone());
                self.folder_label = Some(folder_label(folder));
                self.status = Status::Starting;
                self.notice = None;
                self.transcript.notice(
                    format!(
                        "Claude Code started in {} ({}). The first start downloads it, which can take a minute.",
                        folder_label(folder),
                        if self.writable { "read and write" } else { "read only" }
                    ),
                    false,
                );
                self.save_transcript();
            }
            Err(error) => {
                self.status = Status::Stopped(error.clone());
                self.notice = Some(error);
            }
        }
    }

    /// Send a message. Returns false (with a notice) when it cannot go.
    pub fn send(&mut self, text: &str) -> bool {
        self.send_with_images(text, &[])
    }

    /// Send a prompt with optional base64 image blocks (ACP `session/prompt`).
    pub fn send_with_images(
        &mut self,
        text: &str,
        images: &[super::protocol::PromptImage],
    ) -> bool {
        let trimmed = text.trim();
        if trimmed.is_empty() && images.is_empty() {
            return false;
        }
        let wire_prompt = if trimmed.is_empty() {
            String::new()
        } else {
            self.format_prompt(trimmed)
        };
        let Some(client) = &self.client else {
            self.notice = Some("Start Claude Code first.".into());
            return false;
        };
        match client.prompt_with_images(&wire_prompt, images) {
            Ok(()) => {
                self.persona_sent = true;
                let display = if images.is_empty() {
                    trimmed.to_owned()
                } else if trimmed.is_empty() {
                    format!("[{} image(s) attached]", images.len())
                } else {
                    format!("{trimmed}\n\n[{} image(s) attached]", images.len())
                };
                self.transcript.push_user(&display);
                self.save_transcript();
                self.status = Status::Working;
                self.notice = None;
                true
            }
            Err(error) => {
                self.notice = Some(error);
                false
            }
        }
    }

    /// List relative file paths inside `root` matching `query` for `@`-file
    /// mentions in the ACP composer. Bounded in depth and count; skips
    /// symlinks and common build/VCS directories.
    pub fn list_folder_files(root: &std::path::Path, query: &str) -> Vec<String> {
        let needle = query.trim().to_ascii_lowercase();
        let mut out = Vec::new();
        fn walk(
            dir: &std::path::Path,
            prefix: &str,
            needle: &str,
            out: &mut Vec<String>,
            depth: usize,
        ) {
            if depth > 4 || out.len() >= 12 {
                return;
            }
            let Ok(entries) = std::fs::read_dir(dir) else {
                return;
            };
            let mut items: Vec<_> = entries.flatten().collect();
            items.sort_by_key(|e| e.file_name());
            for entry in items {
                if out.len() >= 12 {
                    break;
                }
                let Ok(ft) = entry.file_type() else {
                    continue;
                };
                if ft.is_symlink() {
                    continue;
                }
                let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
                    continue;
                };
                if matches!(
                    name.as_str(),
                    ".git" | "node_modules" | "target" | ".next" | "dist" | "__pycache__"
                ) {
                    continue;
                }
                let rel = if prefix.is_empty() {
                    name
                } else {
                    format!("{prefix}/{name}")
                };
                if ft.is_dir() {
                    walk(&entry.path(), &rel, needle, out, depth + 1);
                } else if ft.is_file()
                    && (needle.is_empty() || rel.to_ascii_lowercase().contains(needle))
                {
                    out.push(rel);
                }
            }
        }
        walk(root, "", &needle, &mut out, 0);
        out
    }

    /// Stop the running message (the agent keeps running).
    pub fn cancel(&mut self) {
        if let Some(client) = &self.client {
            client.cancel();
        }
        self.asks.clear();
    }

    /// End the agent.
    pub fn stop(&mut self) {
        self.stop_quietly();
        self.persona_sent = false;
        self.status = Status::Idle;
        self.transcript.notice("Claude Code stopped.", false);
        self.save_transcript();
    }

    fn stop_quietly(&mut self) {
        if let Some(client) = self.client.take() {
            client.shutdown();
        }
        self.events = None;
        self.asks.clear();
    }

    /// Answer the permission request at `index` with an option, or decline.
    pub fn answer(&mut self, index: usize, option_id: Option<&str>) {
        if index >= self.asks.len() {
            return;
        }
        let ask = self.asks.remove(index);
        if let Some(client) = &self.client {
            client.answer(&ask.request_id, &ask.request, option_id);
        }
        let allowed = option_id.is_some_and(|id| {
            ask.request
                .shown_options()
                .iter()
                .any(|option| option.id == id && option.allows())
        });
        self.transcript.notice(
            format!(
                "{}: {}",
                if allowed { "Allowed" } else { "Declined" },
                ask.request.title
            ),
            false,
        );
    }

    /// Open Claude Code's own sign-in in a console window.
    pub fn sign_in(&mut self) {
        let Some(npx) = self.node.as_ref().and_then(NodeCheck::npx) else {
            self.notice = Some("Install Node.js 22 or newer first.".into());
            return;
        };
        self.notice = Some(match setup::sign_in_command(npx).spawn() {
            Ok(_) => "Finish signing in in the window that opened, then press Try again.".into(),
            Err(error) => format!("Could not open the sign-in: {}", error.kind()),
        });
    }

    /// After signing in: open the session again.
    pub fn retry_session(&mut self) {
        match &self.client {
            Some(client) => match client.new_session() {
                Ok(()) => {
                    self.persona_sent = false;
                    self.status = Status::Starting;
                    self.notice = None;
                }
                Err(error) => self.notice = Some(error),
            },
            None => self.notice = Some("Start Claude Code first.".into()),
        }
    }

    /// Install Node.js with winget in a console window.
    pub fn install_node(&mut self) {
        self.notice = Some(match setup::install_node_command() {
            Some(mut command) => match command.spawn() {
                Ok(_) => "Installing Node.js in the window that opened. When it finishes, restart SeraBot so it sees Node.js.".into(),
                Err(error) => format!("Could not start the installer: {}", error.kind()),
            },
            None => "Install Node.js 22 or newer from nodejs.org, then press Check again.".into(),
        });
    }

    pub fn clear(&mut self) {
        self.transcript = Transcript::default();
        self.persona_sent = false;
        self.save_transcript();
    }

    /// Take what arrived without changing the page state (only the channels
    /// are read), so a caller can skip a redraw when nothing came.
    pub fn take(&self) -> Vec<Inbound> {
        let mut got = Vec::new();
        if let Some(receiver) = &self.node_rx {
            match receiver.try_recv() {
                Ok(check) => got.push(Inbound::Node(check)),
                Err(TryRecvError::Disconnected) => got.push(Inbound::Node(NodeCheck::Missing)),
                Err(TryRecvError::Empty) => {}
            }
        }
        if let Some(events) = &self.events {
            while got.len() < 500 {
                match events.try_recv() {
                    Ok(event) => got.push(Inbound::Event(event)),
                    Err(_) => break,
                }
            }
        }
        got
    }

    /// Apply what [`Controller::take`] returned.
    pub fn absorb(&mut self, inbound: Vec<Inbound>) {
        for item in inbound {
            match item {
                Inbound::Node(check) => {
                    self.node = Some(check);
                    self.node_rx = None;
                }
                Inbound::Event(event) => self.apply(event),
            }
        }
    }

    /// Take and apply. Returns true when something changed.
    pub fn poll(&mut self) -> bool {
        let got = self.take();
        let changed = !got.is_empty();
        self.absorb(got);
        changed
    }

    fn apply(&mut self, event: Event) {
        match event {
            Event::Ready => {
                self.status = Status::Ready;
                self.notice = None;
            }
            Event::Update(update) => {
                if !matches!(update, Update::Other) {
                    self.transcript.apply(update);
                }
            }
            Event::Permission {
                request_id,
                request,
            } => self.asks.push(Ask {
                request_id,
                request,
            }),
            Event::TurnEnded(reason) => {
                self.status = Status::Ready;
                self.asks.clear();
                match reason.as_str() {
                    "end_turn" => {}
                    "cancelled" => self.transcript.notice("Stopped.", false),
                    "max_tokens" => self
                        .transcript
                        .notice("Claude Code reached its answer limit.", true),
                    "refusal" => self.transcript.notice("Claude Code declined.", true),
                    other => self
                        .transcript
                        .notice(format!("The turn ended: {other}."), false),
                }
                self.save_transcript();
            }
            Event::AuthRequired => {
                self.status = Status::NeedsSignIn;
                self.asks.clear();
                self.transcript.notice(
                    "Claude Code is not signed in on this computer. Sign in, then press Try again.",
                    true,
                );
                self.save_transcript();
            }
            Event::Failed(message) => {
                if self.status == Status::Working {
                    self.status = Status::Ready;
                }
                if self.status == Status::Starting {
                    self.status = Status::Stopped(message.clone());
                }
                self.transcript.notice(message, true);
                self.save_transcript();
            }
            Event::FileUsed { path, write, ok } => {
                if !ok {
                    self.transcript.notice(
                        format!(
                            "Refused to {} {path}: outside the folder or not allowed.",
                            if write { "write" } else { "read" }
                        ),
                        true,
                    );
                }
            }
            Event::Exited => {
                self.asks.clear();
                self.client = None;
                self.events = None;
                if self.status.running() || self.status == Status::NeedsSignIn {
                    let message = "Claude Code ended.".to_owned();
                    self.status = Status::Stopped(message.clone());
                    self.transcript.notice(message, true);
                }
                self.save_transcript();
            }
        }
    }
}

/// The folder's last path component, never the full path.
pub fn folder_label(folder: &LocalFolder) -> String {
    std::path::Path::new(&folder.path)
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| folder.id.clone())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::acp::protocol::{PermissionOption, PermissionRequest};
    use crate::acp::transcript::Item;

    fn ask() -> Ask {
        Ask {
            request_id: Value::from(5),
            request: PermissionRequest {
                session_id: "s".into(),
                tool_call_id: "t".into(),
                title: "Run tests".into(),
                kind: "execute".into(),
                options: vec![PermissionOption {
                    id: "ok".into(),
                    name: "Allow".into(),
                    kind: "allow_once".into(),
                }],
            },
        }
    }

    #[test]
    fn events_move_the_status_and_fill_the_transcript() {
        let mut controller = Controller::default();
        controller.status = Status::Starting;
        controller.apply(Event::Ready);
        assert_eq!(controller.status, Status::Ready);
        controller.status = Status::Working;
        controller.apply(Event::Update(Update::AgentText("Hi".into())));
        controller.apply(Event::Permission {
            request_id: Value::from(5),
            request: ask().request,
        });
        assert_eq!(controller.asks.len(), 1);
        controller.answer(0, Some("ok"));
        assert!(controller.asks.is_empty());
        assert!(matches!(
            controller.transcript.items.last(),
            Some(Item::Notice { text, .. }) if text == "Allowed: Run tests"
        ));
        controller.apply(Event::TurnEnded("cancelled".into()));
        assert_eq!(controller.status, Status::Ready);
        controller.apply(Event::AuthRequired);
        assert_eq!(controller.status, Status::NeedsSignIn);
        controller.apply(Event::Exited);
        assert!(matches!(controller.status, Status::Stopped(_)));
    }

    #[test]
    fn nothing_starts_or_sends_without_node_and_an_agent() {
        let mut controller = Controller::default();
        controller.start(&LocalFolder {
            id: "proj".into(),
            path: "/tmp/proj".into(),
            mode: FolderMode::Read,
            bots: Default::default(),
        });
        assert_eq!(controller.status, Status::Idle);
        assert!(controller.notice.as_deref().unwrap().contains("Node.js"));
        assert!(!controller.send("hi"));
        assert!(controller.transcript.items.is_empty());
    }

    #[test]
    fn a_refused_file_is_shown_and_a_declined_ask_says_so() {
        let mut controller = Controller::default();
        controller.apply(Event::FileUsed {
            path: "/elsewhere/a".into(),
            write: true,
            ok: false,
        });
        assert!(matches!(
            controller.transcript.items.last(),
            Some(Item::Notice { error: true, .. })
        ));
        controller.asks.push(ask());
        controller.answer(0, None);
        assert!(matches!(
            controller.transcript.items.last(),
            Some(Item::Notice { text, .. }) if text.starts_with("Declined")
        ));
    }

    #[test]
    fn persona_prompt_prepends_context_until_sent() {
        let mut controller = Controller::default();
        controller.set_bot_context(Some("You are Analyst — concise and cited.".into()));
        let first = controller.format_prompt("Summarize README.md");
        assert!(first.contains("You are Analyst"));
        assert!(first.contains("Summarize README.md"));

        controller.persona_sent = true;
        let second = controller.format_prompt("Next question");
        assert_eq!(second, "Next question");
    }

    #[test]
    fn list_folder_files_filters_and_skips_build_dirs() {
        let root = std::env::temp_dir().join(format!("serabot-acp-files-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::create_dir_all(root.join(".git")).unwrap();
        std::fs::create_dir_all(root.join("target")).unwrap();
        std::fs::write(root.join("README.md"), "# hi").unwrap();
        std::fs::write(root.join("src/main.rs"), "fn main() {}").unwrap();
        std::fs::write(root.join(".git/config"), "ignored").unwrap();
        std::fs::write(root.join("target/out.rs"), "ignored").unwrap();

        let all = Controller::list_folder_files(&root, "");
        assert_eq!(all, vec!["README.md", "src/main.rs"]);

        let filtered = Controller::list_folder_files(&root, "main");
        assert_eq!(filtered, vec!["src/main.rs"]);

        let _ = std::fs::remove_dir_all(&root);
    }
}
