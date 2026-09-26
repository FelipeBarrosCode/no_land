use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::state::TransportKind;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum NetworkErrorCode {
    InvalidRequest,
    UnsupportedMethod,
    ControlOperationFailed,
    UnsupportedProtocolVersion,
    Unauthorized,
    ReplayedRequest,
    RequestIdConflict,
    StaleRevision,
    TransitionInProgress,
    InvalidEndpoint,
    HelperUnavailable,
    HelperStateMismatch,
    DirectProbeUnavailable,
    TurnCredentialsMissing,
    TurnCredentialsInvalid,
    TurnCredentialsExpired,
    TurnServerUnreachable,
    TurnAllocationFailed,
    TurnAllocationLost,
    TurnPermissionFailed,
    TurnBridgeUnavailable,
    InsufficientSamples,
    TargetValidationFailed,
    TunnelValidationFailed,
    SunshineValidationFailed,
    RollbackFailed,
    StatePersistenceFailed,
    BothTransportsUnavailable,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct NetworkError {
    pub code: NetworkErrorCode,
    pub message: String,
    pub retryable: bool,
    pub transport: Option<TransportKind>,
    pub transition_id: Option<Uuid>,
    pub details: Option<String>,
}
