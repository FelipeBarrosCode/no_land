use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

use crate::EVENT_SCHEMA_VERSION;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum NetworkEventType {
    StatusUpdated,
    MetricsUpdated,
    EvaluationCompleted,
    TransitionUpdated,
    AllocationUpdated,
    Error,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct NetworkEvent {
    pub schema_version: u16,
    pub event_id: Uuid,
    pub instance_id: u64,
    pub timestamp: String,
    pub event_type: NetworkEventType,
    #[schemars(with = "serde_json::Value")]
    pub payload: Value,
}

impl NetworkEvent {
    pub fn new(
        instance_id: u64,
        timestamp: String,
        event_type: NetworkEventType,
        payload: Value,
    ) -> Self {
        Self {
            schema_version: EVENT_SCHEMA_VERSION,
            event_id: Uuid::new_v4(),
            instance_id,
            timestamp,
            event_type,
            payload,
        }
    }
}
