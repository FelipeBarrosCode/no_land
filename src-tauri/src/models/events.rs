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

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SharedStorageProgressEvent {
    pub operation_id: String,
    pub instance_id: u64,
    pub kind: String,
    pub state: String,
    pub phase: Option<String>,
    pub message: Option<String>,
    pub completed_units: Option<u64>,
    pub total_units: Option<u64>,
    pub unit: Option<String>,
    pub fraction: Option<f64>,
    pub ready_to_launch: bool,
    pub cancel_requested: bool,
    pub cancellable: bool,
}
