use thiserror::Error;

#[derive(Debug, Error)]
pub enum StateError {
    #[error("{0}")]
    Message(String),
    #[error("not found: {0}")]
    NotFound(String),
    #[error("invalid argument: {0}")]
    Invalid(String),
    #[error("conflict: {0}")]
    Conflict(String),
    #[error("unsafe path: {0}")]
    UnsafePath(String),
    #[error("integrity: {0}")]
    Integrity(String),
    #[error("crypto: {0}")]
    Crypto(String),
    #[error("storage: {0}")]
    Storage(String),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
    #[error("database: {0}")]
    Database(String),
    #[error("operation {id} failed: {message}")]
    Operation { id: String, message: String },
    #[error("seal required before deletion")]
    SealRequired,
    #[error("incomplete commit is not visible")]
    IncompleteCommit,
}

impl StateError {
    pub fn msg(msg: impl Into<String>) -> Self {
        Self::Message(msg.into())
    }
}

pub type Result<T> = std::result::Result<T, StateError>;
