use serde::Serialize;
use thiserror::Error;

pub type AppResult<T> = Result<T, AppError>;

#[derive(Debug, Error)]
pub enum AppError {
    #[error("I/O failure: {0}")]
    Io(String),
    #[error("Serialization failure: {0}")]
    Serialization(String),
    #[error("API request failed: {0}")]
    Api(String),
    #[error("Authentication failed")]
    Authentication,
    #[error("Invalid input: {0}")]
    InvalidInput(String),
    #[error("Not found: {0}")]
    NotFound(String),
    #[error("Command failed: {0}")]
    Command(String),
    #[error("Command timeout: {0}")]
    Timeout(String),
    #[error("Provisioning error: {0}")]
    Provisioning(String),
    #[error("State error: {0}")]
    State(String),
    #[error("NVIDIA driver mismatch: {0}")]
    DriverMismatch(String),
    #[error("Operation cancelled")]
    Cancelled,
}

impl From<std::io::Error> for AppError {
    fn from(value: std::io::Error) -> Self {
        Self::Io(value.to_string())
    }
}

impl From<serde_json::Error> for AppError {
    fn from(value: serde_json::Error) -> Self {
        Self::Serialization(value.to_string())
    }
}

impl From<reqwest::Error> for AppError {
    fn from(value: reqwest::Error) -> Self {
        if value.status() == Some(reqwest::StatusCode::UNAUTHORIZED)
            || value.status() == Some(reqwest::StatusCode::FORBIDDEN)
        {
            return Self::Authentication;
        }

        Self::Api(value.to_string())
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FrontendError {
    pub code: String,
    pub message: String,
    pub details: Option<String>,
    pub retryable: bool,
}

impl From<AppError> for FrontendError {
    fn from(value: AppError) -> Self {
        match value {
            AppError::Authentication => Self {
                code: "auth_failed".to_string(),
                message: "Invalid Vast.ai API key. Update your key in onboarding/settings."
                    .to_string(),
                details: None,
                retryable: true,
            },
            AppError::InvalidInput(message) => Self {
                code: "invalid_input".to_string(),
                message,
                details: None,
                retryable: false,
            },
            AppError::NotFound(message) => Self {
                code: "not_found".to_string(),
                message,
                details: None,
                retryable: false,
            },
            AppError::Timeout(message) => Self {
                code: "timeout".to_string(),
                message: "Operation timed out. You can retry safely.".to_string(),
                details: Some(message),
                retryable: true,
            },
            AppError::Provisioning(message) => Self {
                code: "provisioning_failed".to_string(),
                message: "Provisioning failed. Check diagnostics and retry.".to_string(),
                details: Some(message),
                retryable: true,
            },
            AppError::DriverMismatch(message) => Self {
                code: "driver_mismatch".to_string(),
                message: "NVIDIA driver mismatch detected. Rebooting to fix...".to_string(),
                details: Some(message),
                retryable: true,
            },
            AppError::Cancelled => Self {
                code: "cancelled".to_string(),
                message: "Previous setup cancelled to run the new request.".to_string(),
                details: None,
                retryable: true,
            },
            other => Self {
                code: "internal_error".to_string(),
                message: "Unexpected error. Please retry.".to_string(),
                details: Some(other.to_string()),
                retryable: true,
            },
        }
    }
}
