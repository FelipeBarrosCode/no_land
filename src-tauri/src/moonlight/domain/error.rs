use thiserror::Error;

#[derive(Debug, Error)]
pub enum MoonlightError {
    #[error("validation error: {0}")]
    Validation(String),
    #[error("invalid session transition from {from:?} with signal {signal:?}")]
    InvalidSessionTransition {
        from: crate::moonlight::domain::SessionState,
        signal: crate::moonlight::domain::SessionSignal,
    },
    #[error("persistence error: {0}")]
    Persistence(String),
    #[error("migration error: {0}")]
    Migration(String),
    #[error("secret store error: {0}")]
    SecretStore(String),
    #[error("identity is invalid: {0}")]
    IdentityInvalid(String),
    #[error("serialization error: {0}")]
    Serialization(String),
    #[error("i/o error: {0}")]
    Io(String),
    #[error("native build error: {0}")]
    Native(String),
}

impl From<std::io::Error> for MoonlightError {
    fn from(value: std::io::Error) -> Self {
        Self::Io(value.to_string())
    }
}

impl From<serde_json::Error> for MoonlightError {
    fn from(value: serde_json::Error) -> Self {
        Self::Serialization(value.to_string())
    }
}
