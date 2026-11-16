use thiserror::Error;

pub type Result<T> = std::result::Result<T, SeraError>;

#[derive(Debug, Error)]
pub enum SeraError {
    #[error("configuration error: {0}")]
    Config(String),
    #[error("database error: {0}")]
    Database(#[from] rusqlite::Error),
    #[error("connection pool error: {0}")]
    Pool(#[from] r2d2::Error),
    #[error("not found: {0}")]
    NotFound(String),
    #[error("forbidden: {0}")]
    Forbidden(String),
    #[error("policy violation")]
    PolicyViolation,
    #[error("approval required")]
    ApprovalRequired { approval_id: String },
    #[error("invalid state: {0}")]
    InvalidState(String),
    #[error("validation error: {0}")]
    Validation(String),
    /// A request the server refused with a server-authored, secret-free
    /// message that is safe to show the caller. `code` is a stable word
    /// clients match on. Never put user content, secrets or provider bodies
    /// in `message`.
    #[error("{message}")]
    Rejected { code: &'static str, message: String },
    #[error("external service error: {0}")]
    External(String),
    /// A third-party service refused or failed a request made for the user.
    /// `status` is the upstream HTTP status (0 when no response arrived),
    /// `code` is a stable word clients match on, and `message` is already
    /// safe to show: no secrets, no control characters, bounded length.
    /// `retry_after` is the server's own back-off hint, when it sent one.
    /// `ambiguous` is set when the request may have reached the service and
    /// acted (a timeout or a lost connection on a mutation); the gateway then
    /// records the outcome as unknown instead of failed, and nothing replays it.
    #[error("{message}")]
    Upstream {
        status: u16,
        code: &'static str,
        message: String,
        retry_after: Option<std::time::Duration>,
        ambiguous: bool,
    },
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("serialization error: {0}")]
    Serialization(#[from] serde_json::Error),
    #[error("secret store error: {0}")]
    Secrets(String),
    #[error("internal error: {0}")]
    Internal(String),
}

impl SeraError {
    /// An upstream failure with no back-off hint that did not reach the service
    /// in an unknown state.
    pub fn upstream(status: u16, code: &'static str, message: impl Into<String>) -> Self {
        Self::Upstream {
            status,
            code,
            message: message.into(),
            retry_after: None,
            ambiguous: false,
        }
    }

    /// An upstream failure whose effect on the service is unknown.
    pub fn upstream_ambiguous(code: &'static str, message: impl Into<String>) -> Self {
        Self::Upstream {
            status: 0,
            code,
            message: message.into(),
            retry_after: None,
            ambiguous: true,
        }
    }
}

impl From<anyhow::Error> for SeraError {
    fn from(value: anyhow::Error) -> Self {
        Self::Internal(value.to_string())
    }
}
