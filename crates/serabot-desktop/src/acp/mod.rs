//! Claude Code on this computer through the Agent Client Protocol (0.54,
//! `docs/specs/claude-code-acp.md`): the person's own Claude login, run by the
//! real Claude Code, driven by the desktop. Nothing here talks to the server.

pub mod client;
pub mod controller;
pub mod protocol;
pub mod setup;
pub mod transcript;

pub use client::{AcpClient, Event, Workspace};
pub use controller::{Controller, Status};
pub use transcript::{Item, Transcript};
