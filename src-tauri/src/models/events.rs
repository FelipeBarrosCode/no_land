use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use super::app_state::OrchestrationState;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProvisioningEvent {
    pub state: OrchestrationState,
    pub message: String,
    pub details: Option<String>,
    pub timestamp: DateTime<Utc>,
    pub is_error: bool,
}

impl ProvisioningEvent {
    pub fn info(
        state: OrchestrationState,
        message: impl Into<String>,
        details: Option<String>,
    ) -> Self {
        Self {
            state,
            message: message.into(),
            details,
            timestamp: Utc::now(),
            is_error: false,
        }
    }

    pub fn error(
        state: OrchestrationState,
        message: impl Into<String>,
        details: Option<String>,
    ) -> Self {
        Self {
            state,
            message: message.into(),
            details,
            timestamp: Utc::now(),
            is_error: true,
        }
    }
}
