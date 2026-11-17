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

impl Controller {
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
            }
            Err(error) => {
                self.status = Status::Stopped(error.clone());
                self.notice = Some(error);
            }
        }
    }

    /// Send a message. Returns false (with a notice) when it cannot go.
    pub fn send(&mut self, text: &str) -> bool {
        let Some(client) = &self.client else {
            self.notice = Some("Start Claude Code first.".into());
            return false;
        };
        match client.prompt(text) {
            Ok(()) => {
                self.transcript.push_user(text.trim());
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
        self.status = Status::Idle;
        self.transcript.notice("Claude Code stopped.", false);
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
            }
            Event::AuthRequired => {
                self.status = Status::NeedsSignIn;
                self.asks.clear();
                self.transcript.notice(
                    "Claude Code is not signed in on this computer. Sign in, then press Try again.",
                    true,
                );
            }
            Event::Failed(message) => {
                if self.status == Status::Working {
                    self.status = Status::Ready;
                }
                if self.status == Status::Starting {
                    self.status = Status::Stopped(message.clone());
                }
                self.transcript.notice(message, true);
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
        let mut controller = Controller {
            status: Status::Starting,
            ..Controller::default()
        };
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
}
