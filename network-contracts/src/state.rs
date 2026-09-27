use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::NETWORK_STATE_SCHEMA_VERSION;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ConnectionPreference {
    #[default]
    Auto,
    Direct,
    CloudflareTurn,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum TransportKind {
    Direct,
    CloudflareTurn,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum PathAvailability {
    #[default]
    Unknown,
    Preparing,
    Ready,
    Degraded,
    Unavailable,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum TransitionPhase {
    #[default]
    Idle,
    ValidatingTarget,
    ApplyingEndpoint,
    ValidatingTunnel,
    Committing,
    RollingBack,
    Completed,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct NetworkEndpoint {
    pub host: String,
    pub port: u16,
}

impl NetworkEndpoint {
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.host.trim().is_empty() {
            return Err("endpoint host must not be empty");
        }
        if self.host.starts_with('[') || self.host.ends_with(']') {
            return Err("endpoint host must not persist IPv6 brackets");
        }
        if self.port == 0 {
            return Err("endpoint port must not be zero");
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct PathMetrics {
    pub sample_count: u32,
    pub sample_age_ms: u64,
    pub median_rtt_ms: Option<f64>,
    pub p95_rtt_ms: Option<f64>,
    pub p99_rtt_ms: Option<f64>,
    pub jitter_ms: f64,
    pub loss_percent: f64,
    pub spike_percent: f64,
    pub reordering_percent: Option<f64>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct PathEvaluation {
    pub metrics: PathMetrics,
    pub stability_penalty: f64,
    pub confidence: f64,
    pub scoreable: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct EvaluationComparison {
    pub latency_advantage: f64,
    pub stability_advantage: f64,
    pub latency_weight: f64,
    pub stability_weight: f64,
    pub confidence: f64,
    pub turn_advantage: f64,
}

impl Default for EvaluationComparison {
    fn default() -> Self {
        Self {
            latency_advantage: 0.0,
            stability_advantage: 0.0,
            latency_weight: 0.0,
            stability_weight: 0.0,
            confidence: 0.0,
            turn_advantage: 0.0,
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum EvaluationReason {
    RelayLowerLatency,
    RelayMoreStable,
    RelayWinsBoth,
    DirectLowerLatency,
    DirectMoreStable,
    DirectWinsBoth,
    #[default]
    PathsEquivalent,
    InsufficientSamples,
    DirectUnavailable,
    RelayUnavailable,
    ManualPreference,
    EmergencyFailover,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ConnectionEvaluation {
    pub evaluation_id: Uuid,
    pub policy_version: String,
    pub evaluated_at: String,
    pub selected: Option<TransportKind>,
    pub reason: EvaluationReason,
    pub direct: PathEvaluation,
    pub cloudflare_turn: PathEvaluation,
    pub comparison: EvaluationComparison,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct DirectPathState {
    pub endpoint: Option<NetworkEndpoint>,
    pub probe_endpoint: Option<NetworkEndpoint>,
    pub effective_mtu: Option<u16>,
    pub availability: PathAvailability,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct CloudflareTurnPathState {
    pub enabled: bool,
    pub credential_ref: Option<String>,
    pub relay_endpoint: Option<NetworkEndpoint>,
    pub allocation_generation: u64,
    pub allocation_expires_at: Option<String>,
    pub credential_expires_at: Option<String>,
    pub effective_mtu: Option<u16>,
    pub availability: PathAvailability,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ConnectionTransition {
    pub transition_id: Uuid,
    pub requested_transport: TransportKind,
    pub previous_transport: Option<TransportKind>,
    pub effective_transport: Option<TransportKind>,
    pub phase: TransitionPhase,
    pub started_at: String,
    pub completed_at: Option<String>,
    pub error: Option<crate::errors::NetworkError>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct InstanceNetworkState {
    pub schema_version: u16,
    pub client_revision: u64,
    pub updated_at: Option<String>,
    pub preference: ConnectionPreference,
    pub active_transport: Option<TransportKind>,
    pub fallback_reason: Option<EvaluationReason>,
    pub direct: DirectPathState,
    pub cloudflare_turn: CloudflareTurnPathState,
    pub last_evaluation: Option<ConnectionEvaluation>,
    pub last_transition: Option<ConnectionTransition>,
}

impl Default for InstanceNetworkState {
    fn default() -> Self {
        Self {
            schema_version: NETWORK_STATE_SCHEMA_VERSION,
            client_revision: 0,
            updated_at: None,
            preference: ConnectionPreference::Auto,
            active_transport: None,
            fallback_reason: None,
            direct: DirectPathState::default(),
            cloudflare_turn: CloudflareTurnPathState::default(),
            last_evaluation: None,
            last_transition: None,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum TurnRuntimeStatus {
    #[default]
    Disabled,
    AwaitingCredentials,
    Preparing,
    Ready,
    Degraded,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct HostNetworkState {
    pub schema_version: u16,
    pub host_revision: u64,
    pub updated_at: String,
    pub instance_id: String,
    pub session_id: Uuid,
    pub agent_version: String,
    pub turn_status: TurnRuntimeStatus,
    pub allocation_generation: u64,
    pub relay_endpoint: Option<NetworkEndpoint>,
    pub allocation_expires_at: Option<String>,
    pub credential_expires_at: Option<String>,
    pub observed_transport: Option<TransportKind>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_use_the_current_nested_schema() {
        let state = InstanceNetworkState::default();
        assert_eq!(state.schema_version, NETWORK_STATE_SCHEMA_VERSION);
        assert_eq!(state.preference, ConnectionPreference::Auto);
        assert_eq!(state.direct.availability, PathAvailability::Unknown);
    }

    #[test]
    fn endpoint_rejects_persisted_ipv6_brackets_and_zero_port() {
        assert!(NetworkEndpoint {
            host: "[::1]".into(),
            port: 51820
        }
        .validate()
        .is_err());
        assert!(NetworkEndpoint {
            host: "::1".into(),
            port: 51820
        }
        .validate()
        .is_ok());
        assert!(NetworkEndpoint {
            host: "example.com".into(),
            port: 0
        }
        .validate()
        .is_err());
    }
}
