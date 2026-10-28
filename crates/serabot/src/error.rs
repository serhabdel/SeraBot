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
    #[error("external service error: {0}")]
    External(String),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("serialization error: {0}")]
    Serialization(#[from] serde_json::Error),
    #[error("secret store error: {0}")]
    Secrets(String),
    #[error("internal error: {0}")]
    Internal(String),
}

impl From<anyhow::Error> for SeraError {
    fn from(value: anyhow::Error) -> Self {
        Self::Internal(value.to_string())
    }
}
