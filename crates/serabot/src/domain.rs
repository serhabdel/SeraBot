use std::fmt;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub type Id = String;

pub fn new_id() -> Id {
    uuid::Uuid::now_v7().to_string()
}

pub fn utc_now() -> DateTime<Utc> {
    Utc::now()
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    System,
    #[default]
    User,
    Assistant,
    Tool,
}

impl Role {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::System => "system",
            Self::User => "user",
            Self::Assistant => "assistant",
            Self::Tool => "tool",
        }
    }
}

impl fmt::Display for Role {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl std::str::FromStr for Role {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "system" => Ok(Self::System),
            "user" => Ok(Self::User),
            "assistant" => Ok(Self::Assistant),
            "tool" => Ok(Self::Tool),
            other => Err(format!("unknown message role: {other}")),
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Language {
    It,
    Fr,
    En,
}

impl Language {
    pub fn code(self) -> &'static str {
        match self {
            Self::It => "it",
            Self::Fr => "fr",
            Self::En => "en",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredMessage {
    pub id: Id,
    pub conversation_id: Id,
    pub turn_id: Option<Id>,
    pub role: Role,
    pub content: String,
    pub name: Option<String>,
    pub tool_call_id: Option<String>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub arguments: Value,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct LlmMessage {
    pub role: Role,
    pub content: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tool_calls: Vec<ToolCall>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

impl LlmMessage {
    pub fn system(content: impl Into<String>) -> Self {
        Self {
            role: Role::System,
            content: Some(content.into()),
            ..Self::default()
        }
    }

    pub fn user(content: impl Into<String>) -> Self {
        Self {
            role: Role::User,
            content: Some(content.into()),
            ..Self::default()
        }
    }

    pub fn assistant(content: Option<String>, tool_calls: Vec<ToolCall>) -> Self {
        Self {
            role: Role::Assistant,
            content,
            tool_calls,
            ..Self::default()
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskStatus {
    Queued,
    Running,
    AwaitingApproval,
    Done,
    Failed,
    Cancelled,
}

impl TaskStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Running => "running",
            Self::AwaitingApproval => "awaiting_approval",
            Self::Done => "done",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Task {
    pub id: Id,
    pub kind: String,
    pub bot: String,
    pub payload: Value,
    pub status: String,
    pub priority: i64,
    pub attempts: i64,
    pub max_attempts: i64,
    pub run_after: DateTime<Utc>,
    pub lease_until: Option<DateTime<Utc>>,
    pub worker_id: Option<String>,
    pub fencing_token: i64,
    pub uncertain: bool,
    pub last_error: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl Task {
    pub fn status(&self) -> Option<TaskStatus> {
        match self.status.as_str() {
            "queued" => Some(TaskStatus::Queued),
            "running" => Some(TaskStatus::Running),
            "awaiting_approval" => Some(TaskStatus::AwaitingApproval),
            "done" => Some(TaskStatus::Done),
            "failed" => Some(TaskStatus::Failed),
            "cancelled" => Some(TaskStatus::Cancelled),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Turn {
    pub id: Id,
    pub task_id: Id,
    pub bot: String,
    pub conversation_id: Id,
    pub status: String,
    pub state: Value,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalState {
    Pending,
    ApprovedOnce,
    ApprovedAlways,
    Denied,
    Expired,
    Invalidated,
}

impl ApprovalState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::ApprovedOnce => "approved_once",
            Self::ApprovedAlways => "approved_always",
            Self::Denied => "denied",
            Self::Expired => "expired",
            Self::Invalidated => "invalidated",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Action {
    pub id: Id,
    pub tool: String,
    pub task_id: Option<Id>,
    pub turn_id: Option<Id>,
    pub canonical_args: String,
    pub canonical_hash: String,
    pub destination: Option<String>,
    pub artifact_hash: Option<String>,
    pub policy_version: String,
    pub status: String,
    pub outcome: Option<Value>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Approval {
    pub id: Id,
    pub action_id: Id,
    pub tool: String,
    pub summary: String,
    pub state: String,
    pub scope: Option<String>,
    pub expires_at: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
    pub decided_at: Option<DateTime<Utc>>,
    pub note: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditEntry {
    pub id: i64,
    pub at: DateTime<Utc>,
    pub actor: String,
    pub event: String,
    pub tool: Option<String>,
    pub task_id: Option<Id>,
    pub approval_id: Option<Id>,
    pub action_id: Option<Id>,
    pub policy_version: Option<String>,
    pub details: Option<Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Event {
    pub id: i64,
    pub at: DateTime<Utc>,
    pub kind: String,
    pub payload: Value,
}
