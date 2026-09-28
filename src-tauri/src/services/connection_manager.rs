use std::{collections::HashMap, net::SocketAddr, path::PathBuf, time::Duration};

use chrono::{Duration as ChronoDuration, Utc};
use noland_network_contracts::{
    control::{
        AbortConnectionProfileRequest, CommitConnectionProfileRequest, InstallProbeSessionRequest,
        PrepareConnectionProfileRequest, PrepareTurnRequest,
    },
    errors::{NetworkError, NetworkErrorCode},
    evaluation::{evaluate_paths, EvaluationMode, EvaluationPolicy},
    probe::{PacketType, ProbeDirection, ProbePacket, ProbePath},
    state::{
        ConnectionEvaluation, ConnectionProfile, ConnectionTransition, EvaluationReason,
        InstanceNetworkState, MeasurementMethod, NetworkEndpoint, PacketLimits, PathAvailability,
        PathMetrics, TransitionPhase, TransportKind,
    },
};
use rand::{rngs::OsRng, RngCore};
use tokio::{
    net::UdpSocket,
    time::{interval, sleep_until, timeout, Instant, MissedTickBehavior},
};
use uuid::Uuid;

use crate::{
    errors::{AppError, AppResult},
    models::app_state::{PersistedAppState, ProvisionedServerState},
};

use super::{
    app_context::AppContext,
    cloudflare_turn,
    network_control::{call_local_control_via_ssh, HostNetworkStatus, NetworkControlClient},
    remote_exec::RemoteExec,
    wireguard::{
        get_managed_gotatun_runtime, set_managed_gotatun_mtu, set_managed_gotatun_peer_endpoint,
        ManagedTunnelRuntime,
    },
};

const TURN_EFFECTIVE_MTU: u16 = 1200;
const DIRECT_PROBE_PORT: u16 = 6201;
const TARGET_PROBE_ATTEMPTS: u64 = 3;
const TUNNEL_VALIDATION_TIMEOUT: Duration = Duration::from_secs(15);
const EVALUATION_SAMPLE_COUNT: u64 = 60;
const EVALUATION_SAMPLE_INTERVAL: Duration = Duration::from_millis(100);
const EVALUATION_DRAIN_TIME: Duration = Duration::from_secs(1);
const WIREGUARD_DATA_OVERHEAD: u16 = 32;
const MIN_STREAMING_INNER_MTU: u16 = 576;
const PAYLOAD_PROBE_TIMEOUT: Duration = Duration::from_millis(500);
const QUALITY_SWITCH_WIN_STREAK: u8 = 3;
const QUALITY_SWITCH_MINIMUM_DWELL: Duration = Duration::from_secs(90);

pub fn turn_switching_enabled() -> bool {
    std::env::var("NOLAND_ENABLE_VERIFIED_TURN_SWITCHING").as_deref() == Ok("1")
}

pub fn automatic_selection_enabled() -> bool {
    turn_switching_enabled()
        && std::env::var("NOLAND_ENABLE_AUTOMATIC_TRANSPORT_SELECTION").as_deref() == Ok("1")
}

pub async fn run_connection_maintenance(context: AppContext) {
    let mut ticker = interval(Duration::from_secs(30));
    ticker.set_missed_tick_behavior(MissedTickBehavior::Skip);
    // Let startup and tunnel restoration settle before the first evaluation.
    ticker.tick().await;
    let mut maintenance_round = 0_u8;
    loop {
        ticker.tick().await;
        maintenance_round = maintenance_round.wrapping_add(1);
        let state = context.load_state().await;
        let Some(instance_id) = state.instance.instance_id else {
            continue;
        };
        let Some(network) = state
            .provisioned_servers
            .iter()
            .find(|server| server.instance_id == instance_id)
            .map(|server| server.network.clone())
        else {
            continue;
        };
        if network
            .last_transition
            .as_ref()
            .is_some_and(|transition| transition.phase == TransitionPhase::Committing)
        {
            if let Err(error) =
                ConnectionManager::reconcile_committed_profile(&context, instance_id).await
            {
                tracing::warn!(instance_id, %error, "connection profile reconciliation failed");
            }
        }
        // Allocation replacement mutates the live peer endpoint. Defer it for
        // manually pinned streams; Auto is the explicit opt-in for mid-stream
        // transport management.
        let may_mutate_active_stream = !context.is_stream_network_active()
            || network.preference == noland_network_contracts::state::ConnectionPreference::Auto;
        if may_mutate_active_stream
            && network.active_transport == Some(TransportKind::CloudflareTurn)
            && maintenance_round % 10 == 1
        {
            if let Err(error) = ConnectionManager::maintain_active_turn(&context, instance_id).await
            {
                tracing::warn!(instance_id, %error, "active TURN allocation maintenance failed");
            }
        }
        if !automatic_selection_enabled()
            || network.preference != noland_network_contracts::state::ConnectionPreference::Auto
        {
            continue;
        }
        if let Err(error) =
            ConnectionManager::evaluate_and_apply_automatic(&context, instance_id).await
        {
            tracing::warn!(instance_id, %error, "automatic transport evaluation failed");
        }
    }
}

pub struct ConnectionManager;

struct TargetPlan {
    endpoint: NetworkEndpoint,
    probe_endpoint: NetworkEndpoint,
    effective_mtu: u16,
    relay_metadata: Option<RelayMetadata>,
    packet_limits: PacketLimits,
}

#[derive(Clone)]
struct RelayMetadata {
    allocation_generation: u64,
    allocation_expires_at: Option<String>,
    credential_expires_at: String,
}

enum HostControl {
    Tunnel(NetworkControlClient),
    Ssh(RemoteExec),
}

impl ConnectionManager {
    pub async fn reconcile_committed_profile(
        context: &AppContext,
        instance_id: u64,
    ) -> AppResult<InstanceNetworkState> {
        let state = context.load_state().await;
        let server = state
            .provisioned_servers
            .iter()
            .find(|server| server.instance_id == instance_id)
            .cloned()
            .ok_or_else(|| {
                AppError::NotFound(format!("Instance {instance_id} is not provisioned"))
            })?;
        let config_path = PathBuf::from(&server.wireguard_config_path);
        let runtime = managed_runtime(config_path).await?;
        let remote = remote_for_server(context, &state, &server);
        let mut control =
            HostControl::connect(instance_id, &server.wireguard_server_ip, remote.as_ref()).await?;
        let status = control.get_status().await?;
        validate_host_identity(instance_id, &status)?;
        let profile = status
            .state
            .committed_profile
            .ok_or_else(|| AppError::State("Host has no committed connection profile".into()))?;
        let runtime_endpoint = parse_runtime_endpoint(&runtime.endpoint)?;
        if runtime.mtu != profile.inner_mtu || runtime_endpoint != profile.endpoint {
            return Err(AppError::State(format!(
                "Committed host profile does not match the client runtime (host MTU {}, client MTU {}; host endpoint {:?}, client endpoint {:?})",
                profile.inner_mtu, runtime.mtu, profile.endpoint, runtime_endpoint
            )));
        }
        let completed_at = Utc::now().to_rfc3339();
        let next = context
            .update_state(|state| {
                if let Some(server) = state
                    .provisioned_servers
                    .iter_mut()
                    .find(|server| server.instance_id == instance_id)
                {
                    let previous_transport = server.network.active_transport;
                    server.network.client_revision =
                        server.network.client_revision.saturating_add(1);
                    server.network.updated_at = Some(completed_at.clone());
                    server.network.active_transport = Some(profile.desired_transport);
                    server.network.connection_profile = Some(profile.clone());
                    server.network.last_transition = Some(ConnectionTransition {
                        transition_id: profile.transition_id,
                        requested_transport: profile.desired_transport,
                        previous_transport,
                        effective_transport: Some(profile.desired_transport),
                        phase: TransitionPhase::Completed,
                        started_at: profile.created_at.clone(),
                        completed_at: Some(completed_at.clone()),
                        error: None,
                    });
                    match profile.desired_transport {
                        TransportKind::Direct => {
                            server.network.direct.effective_mtu = Some(profile.inner_mtu);
                            server.network.direct.availability = PathAvailability::Ready;
                        }
                        TransportKind::CloudflareTurn => {
                            server.network.cloudflare_turn.effective_mtu = Some(profile.inner_mtu);
                            server.network.cloudflare_turn.relay_endpoint =
                                Some(profile.endpoint.clone());
                            server.network.cloudflare_turn.allocation_generation =
                                profile.allocation_generation.unwrap_or_default();
                            server.network.cloudflare_turn.availability = PathAvailability::Ready;
                        }
                    }
                }
            })
            .await?;
        Ok(next
            .provisioned_servers
            .iter()
            .find(|server| server.instance_id == instance_id)
            .expect("reconciled instance remains present")
            .network
            .clone())
    }

    pub async fn repair(context: &AppContext, instance_id: u64) -> AppResult<InstanceNetworkState> {
        let state = context.load_state().await;
        let network = state
            .provisioned_servers
            .iter()
            .find(|server| server.instance_id == instance_id)
            .map(|server| server.network.clone())
            .ok_or_else(|| {
                AppError::NotFound(format!("Instance {instance_id} is not provisioned"))
            })?;
        let requested_transport = match network.preference {
            noland_network_contracts::state::ConnectionPreference::Direct => TransportKind::Direct,
            noland_network_contracts::state::ConnectionPreference::CloudflareTurn => {
                TransportKind::CloudflareTurn
            }
            noland_network_contracts::state::ConnectionPreference::Auto => {
                network.active_transport.unwrap_or(TransportKind::Direct)
            }
        };
        Self::switch_inner(context, instance_id, requested_transport, true).await
    }

    pub async fn maintain_active_turn(
        context: &AppContext,
        instance_id: u64,
    ) -> AppResult<InstanceNetworkState> {
        let state = context.load_state().await;
        let server = state
            .provisioned_servers
            .iter()
            .find(|server| server.instance_id == instance_id)
            .cloned()
            .ok_or_else(|| {
                AppError::NotFound(format!("Instance {instance_id} is not provisioned"))
            })?;
        if server.network.active_transport != Some(TransportKind::CloudflareTurn) {
            return Ok(server.network);
        }
        let remote = remote_for_server(context, &state, &server);
        let public_ip = cloudflare_turn::discover_client_public_ip(&context.http_client).await?;
        let healthy =
            match HostControl::connect(instance_id, &server.wireguard_server_ip, remote.as_ref())
                .await
            {
                Ok(mut control) => control.get_status().await.is_ok_and(|status| {
                    turn_status_is_healthy(&server.network, &status, public_ip)
                }),
                Err(_) => false,
            };
        if healthy {
            return Ok(server.network);
        }
        Self::switch_inner(context, instance_id, TransportKind::CloudflareTurn, true).await
    }

    pub async fn stop_turn(
        context: &AppContext,
        instance_id: u64,
    ) -> AppResult<InstanceNetworkState> {
        let _allocation_guard = context.network_allocation_lock.lock().await;
        let initial = context.load_state().await;
        let server = initial
            .provisioned_servers
            .iter()
            .find(|server| server.instance_id == instance_id)
            .cloned()
            .ok_or_else(|| {
                AppError::NotFound(format!("Instance {instance_id} is not provisioned"))
            })?;
        if server.network.active_transport == Some(TransportKind::CloudflareTurn) {
            return Err(AppError::State(
                "Switch the active tunnel to Direct before stopping its TURN allocation"
                    .to_string(),
            ));
        }
        let remote = remote_for_server(context, &initial, &server);
        let mut control =
            HostControl::connect(instance_id, &server.wireguard_server_ip, remote.as_ref()).await?;
        control.stop_turn().await?;
        let updated_at = Utc::now().to_rfc3339();
        let next = context
            .update_state(|state| {
                if let Some(server) = state
                    .provisioned_servers
                    .iter_mut()
                    .find(|server| server.instance_id == instance_id)
                {
                    server.network.client_revision =
                        server.network.client_revision.saturating_add(1);
                    server.network.updated_at = Some(updated_at);
                    server.network.cloudflare_turn.relay_endpoint = None;
                    server.network.cloudflare_turn.allocation_expires_at = None;
                    server.network.cloudflare_turn.credential_expires_at = None;
                    server.network.cloudflare_turn.availability = PathAvailability::Unavailable;
                }
            })
            .await?;
        Ok(next
            .provisioned_servers
            .iter()
            .find(|server| server.instance_id == instance_id)
            .expect("validated instance remains present")
            .network
            .clone())
    }

    pub async fn evaluate_runtime(
        context: &AppContext,
        instance_id: u64,
    ) -> AppResult<ConnectionEvaluation> {
        let _allocation_guard = context.network_allocation_lock.lock().await;
        let initial = context.load_state().await;
        let server = initial
            .provisioned_servers
            .iter()
            .find(|server| server.instance_id == instance_id)
            .cloned()
            .ok_or_else(|| {
                AppError::NotFound(format!("Instance {instance_id} is not provisioned"))
            })?;
        if server.wireguard_server_ip.trim().is_empty() {
            return Err(AppError::State(
                "WireGuard host address is unavailable for this instance".to_string(),
            ));
        }
        let remote = remote_for_server(context, &initial, &server);
        let mut control =
            HostControl::connect(instance_id, &server.wireguard_server_ip, remote.as_ref()).await?;
        let host_status = control.get_status().await?;
        if host_status.state.instance_id != instance_id.to_string() {
            return Err(AppError::State(
                "Host control returned a different instance identity".to_string(),
            ));
        }

        let direct_endpoint = server.network.direct.probe_endpoint.clone();
        let relay_target = if server.network.cloudflare_turn.enabled {
            match ensure_relay_target(context, &server.network, &mut control, &host_status, false)
                .await
            {
                Ok(target) => Some(target),
                Err(error) => {
                    tracing::warn!(instance_id, %error, "TURN path preparation failed during transport evaluation");
                    None
                }
            }
        } else {
            None
        };

        let direct_probe = install_evaluation_probe(&mut control, TransportKind::Direct).await;
        let relay_probe = if relay_target.is_some() {
            install_evaluation_probe(&mut control, TransportKind::CloudflareTurn).await
        } else {
            Err(AppError::State(
                "TURN path is not available for evaluation".to_string(),
            ))
        };

        let direct_future = sample_if_ready(direct_endpoint, ProbePath::Direct, direct_probe);
        let relay_future = sample_if_ready(
            relay_target
                .as_ref()
                .map(|target| target.probe_endpoint.clone()),
            ProbePath::CloudflareTurn,
            relay_probe,
        );
        let (direct_sample, relay_sample) = tokio::join!(direct_future, relay_future);
        let direct_available = direct_sample
            .as_ref()
            .is_ok_and(|metrics| metrics.median_rtt_ms.is_some());
        let relay_available = relay_sample
            .as_ref()
            .is_ok_and(|metrics| metrics.median_rtt_ms.is_some());
        let direct_metrics = direct_sample.unwrap_or_default();
        let relay_metrics = relay_sample.unwrap_or_default();
        let mode = match server.network.active_transport {
            Some(current) => EvaluationMode::Runtime { current },
            None => EvaluationMode::Provisioning,
        };
        let evaluation = evaluate_paths(
            direct_metrics,
            relay_metrics,
            direct_available,
            relay_available,
            mode,
            Utc::now().to_rfc3339(),
            EvaluationPolicy::default(),
        );
        persist_evaluation(context, instance_id, &evaluation, relay_target.as_ref()).await?;
        Ok(evaluation)
    }

    pub async fn evaluate_and_apply_automatic(
        context: &AppContext,
        instance_id: u64,
    ) -> AppResult<InstanceNetworkState> {
        if !automatic_selection_enabled() {
            return Err(AppError::InvalidInput(
                "Automatic transport selection is disabled until manual Direct and TURN switching are verified"
                    .to_string(),
            ));
        }
        let before = context.load_state().await;
        let network_before = before
            .provisioned_servers
            .iter()
            .find(|server| server.instance_id == instance_id)
            .map(|server| server.network.clone())
            .ok_or_else(|| {
                AppError::NotFound(format!("Instance {instance_id} is not provisioned"))
            })?;
        let evaluation = match Self::evaluate_runtime(context, instance_id).await {
            Ok(evaluation) => evaluation,
            Err(error)
                if network_before.preference
                    == noland_network_contracts::state::ConnectionPreference::Auto
                    && network_before.active_transport == Some(TransportKind::CloudflareTurn)
                    && network_before.direct.availability == PathAvailability::Ready =>
            {
                tracing::warn!(
                    instance_id,
                    %error,
                    "transport evaluation failed on TURN; attempting client-local Direct recovery"
                );
                Self::switch(context, instance_id, TransportKind::Direct).await?;
                let updated_at = Utc::now().to_rfc3339();
                let recovered = context
                    .update_state(|state| {
                        if let Some(server) = state
                            .provisioned_servers
                            .iter_mut()
                            .find(|server| server.instance_id == instance_id)
                        {
                            server.network.client_revision =
                                server.network.client_revision.saturating_add(1);
                            server.network.updated_at = Some(updated_at);
                            server.network.fallback_reason =
                                Some(EvaluationReason::EmergencyFailover);
                        }
                    })
                    .await?;
                return Ok(recovered
                    .provisioned_servers
                    .iter()
                    .find(|server| server.instance_id == instance_id)
                    .expect("recovered instance remains present")
                    .network
                    .clone());
            }
            Err(error) => return Err(error),
        };
        let current = context.load_state().await;
        let network = current
            .provisioned_servers
            .iter()
            .find(|server| server.instance_id == instance_id)
            .map(|server| server.network.clone())
            .ok_or_else(|| {
                AppError::NotFound(format!("Instance {instance_id} is not provisioned"))
            })?;
        if network.preference != noland_network_contracts::state::ConnectionPreference::Auto {
            return Ok(network);
        }
        let Some(selected) = evaluation.selected else {
            return Ok(network);
        };
        if network.active_transport == Some(selected) {
            let mut decisions = context.connection_decisions.lock().await;
            let decision = decisions.entry(instance_id).or_default();
            decision.observed_active = network.active_transport;
            decision.candidate = None;
            decision.consecutive_wins = 0;
            return Ok(network);
        }
        let should_switch = {
            let now = std::time::Instant::now();
            let mut decisions = context.connection_decisions.lock().await;
            let decision = decisions.entry(instance_id).or_default();
            if decision.observed_active != network.active_transport {
                decision.observed_active = network.active_transport;
                decision.candidate = None;
                decision.consecutive_wins = 0;
                decision.last_transport_change = Some(now);
            }
            if decision.candidate == Some(selected) {
                decision.consecutive_wins = decision.consecutive_wins.saturating_add(1);
            } else {
                decision.candidate = Some(selected);
                decision.consecutive_wins = 1;
            }
            let dwell_elapsed = decision
                .last_transport_change
                .is_none_or(|changed| now.duration_since(changed) >= QUALITY_SWITCH_MINIMUM_DWELL);
            decision.consecutive_wins >= QUALITY_SWITCH_WIN_STREAK && dwell_elapsed
        };
        if !should_switch {
            return Ok(network);
        }
        let switched = Self::switch(context, instance_id, selected).await?;
        let mut decisions = context.connection_decisions.lock().await;
        let decision = decisions.entry(instance_id).or_default();
        decision.observed_active = Some(selected);
        decision.candidate = None;
        decision.consecutive_wins = 0;
        decision.last_transport_change = Some(std::time::Instant::now());
        Ok(switched)
    }

    pub async fn switch(
        context: &AppContext,
        instance_id: u64,
        requested_transport: TransportKind,
    ) -> AppResult<InstanceNetworkState> {
        Self::switch_inner(context, instance_id, requested_transport, false).await
    }

    async fn switch_inner(
        context: &AppContext,
        instance_id: u64,
        requested_transport: TransportKind,
        force: bool,
    ) -> AppResult<InstanceNetworkState> {
        // Allocation replacement always precedes endpoint mutation. No code
        // may acquire these locks in the opposite order.
        let _allocation_guard = context.network_allocation_lock.lock().await;
        let _mutation_guard = context.begin_wireguard_mutation()?;
        let initial = context.load_state().await;
        let server = initial
            .provisioned_servers
            .iter()
            .find(|server| server.instance_id == instance_id)
            .cloned()
            .ok_or_else(|| {
                AppError::NotFound(format!("Instance {instance_id} is not provisioned"))
            })?;
        let remote = remote_for_server(context, &initial, &server);
        if !force && server.network.active_transport == Some(requested_transport) {
            return Ok(server.network);
        }
        let config_path = PathBuf::from(&server.wireguard_config_path);
        if server.wireguard_config_path.trim().is_empty() || !config_path.is_file() {
            return Err(AppError::State(
                "Managed WireGuard configuration is unavailable for this instance".to_string(),
            ));
        }
        if server.wireguard_server_ip.trim().is_empty() {
            return Err(AppError::State(
                "WireGuard host address is unavailable for this instance".to_string(),
            ));
        }

        let transition_id = Uuid::new_v4();
        let started_at = Utc::now().to_rfc3339();
        let previous_transport = server.network.active_transport;
        persist_transition(
            context,
            instance_id,
            ConnectionTransition {
                transition_id,
                requested_transport,
                previous_transport,
                effective_transport: previous_transport,
                phase: TransitionPhase::ValidatingTarget,
                started_at: started_at.clone(),
                completed_at: None,
                error: None,
            },
        )
        .await?;

        let runtime_before = managed_runtime(config_path.clone()).await.map_err(|error| {
            transition_error(NetworkErrorCode::HelperUnavailable, error.to_string())
        });
        let runtime_before = match runtime_before {
            Ok(runtime) => runtime,
            Err(error) => {
                persist_failed_transition(
                    context,
                    instance_id,
                    transition_id,
                    requested_transport,
                    previous_transport,
                    started_at,
                    error.clone(),
                )
                .await?;
                return Err(AppError::Command(error.message));
            }
        };
        let previous_endpoint = parse_runtime_endpoint(&runtime_before.endpoint)?;
        let previous_mtu = runtime_before.mtu.max(576);

        let target = match prepare_target(
            context,
            instance_id,
            &server.wireguard_server_ip,
            &server.network,
            requested_transport,
            previous_mtu,
            remote.as_ref(),
        )
        .await
        {
            Ok(target) => target,
            Err(error) => {
                let network_error =
                    transition_error(NetworkErrorCode::TargetValidationFailed, error.to_string());
                persist_failed_transition(
                    context,
                    instance_id,
                    transition_id,
                    requested_transport,
                    previous_transport,
                    started_at,
                    network_error,
                )
                .await?;
                return Err(error);
            }
        };

        let mut host_control =
            match HostControl::connect(instance_id, &server.wireguard_server_ip, remote.as_ref())
                .await
            {
                Ok(control) => control,
                Err(error) => {
                    let network_error = transition_error(
                        NetworkErrorCode::ControlOperationFailed,
                        format!("Host profile transaction is unavailable: {error}"),
                    );
                    persist_failed_transition(
                        context,
                        instance_id,
                        transition_id,
                        requested_transport,
                        previous_transport,
                        started_at,
                        network_error,
                    )
                    .await?;
                    return Err(error);
                }
            };
        let host_status = host_control.get_status().await?;
        validate_host_identity(instance_id, &host_status)?;
        let profile_revision = host_status
            .state
            .committed_profile
            .as_ref()
            .map(|profile| profile.profile_revision)
            .unwrap_or_default()
            .saturating_add(1);
        let operation_id = Uuid::new_v4();
        let mut profile = ConnectionProfile::new(
            instance_id.to_string(),
            profile_revision,
            transition_id,
            requested_transport,
            target.endpoint.clone(),
            target.effective_mtu,
            Utc::now().to_rfc3339(),
        );
        profile.allocation_generation = target
            .relay_metadata
            .as_ref()
            .map(|metadata| metadata.allocation_generation);
        profile.packet_limits = PacketLimits {
            verified_inner_mtu: Some(target.effective_mtu),
            observed_client_mtu: Some(previous_mtu),
            observed_host_mtu: host_status.state.link_state.observed_mtu,
            ..target.packet_limits.clone()
        };
        let prepared_profile = match host_control
            .prepare_connection_profile(&PrepareConnectionProfileRequest {
                operation_id,
                expected_profile_revision: profile_revision.saturating_sub(1),
                lease_expires_at: (Utc::now() + ChronoDuration::minutes(2)).to_rfc3339(),
                profile: profile.clone(),
            })
            .await
        {
            Ok(prepared) if prepared.link_state.observed_mtu == Some(target.effective_mtu) => {
                prepared
            }
            Ok(prepared) => {
                let observed = prepared.link_state.observed_mtu;
                let _ = host_control
                    .abort_connection_profile(&AbortConnectionProfileRequest {
                        operation_id,
                        transition_id,
                        profile_revision,
                        reason: Some("host MTU readback mismatch".to_string()),
                    })
                    .await;
                return Err(AppError::State(format!(
                    "Host MTU readback mismatch: requested {}, observed {:?}",
                    target.effective_mtu, observed
                )));
            }
            Err(error) => {
                // Prepare journals before mutating the host MTU. A transport
                // error can therefore arrive after the mutation succeeded;
                // issue an idempotent abort instead of waiting for lease expiry.
                let _ = host_control
                    .abort_connection_profile(&AbortConnectionProfileRequest {
                        operation_id,
                        transition_id,
                        profile_revision,
                        reason: Some(error.to_string()),
                    })
                    .await;
                let network_error =
                    transition_error(NetworkErrorCode::ControlOperationFailed, error.to_string());
                persist_failed_transition(
                    context,
                    instance_id,
                    transition_id,
                    requested_transport,
                    previous_transport,
                    started_at,
                    network_error,
                )
                .await?;
                return Err(error);
            }
        };
        debug_assert_eq!(prepared_profile.profile, profile);

        persist_transition_phase(
            context,
            instance_id,
            transition_id,
            TransitionPhase::ApplyingEndpoint,
            previous_transport,
            None,
        )
        .await?;

        if target.effective_mtu != previous_mtu {
            let path = config_path.clone();
            let mtu = target.effective_mtu;
            let mtu_result = tokio::task::spawn_blocking(move || {
                set_managed_gotatun_mtu(&path, mtu, transition_id)
            })
            .await
            .map_err(|error| AppError::Command(format!("Tunnel MTU task failed: {error}")))?;
            if let Err(error) = mtu_result {
                let _ = host_control
                    .abort_connection_profile(&AbortConnectionProfileRequest {
                        operation_id,
                        transition_id,
                        profile_revision,
                        reason: Some(error.to_string()),
                    })
                    .await;
                let network_error =
                    transition_error(NetworkErrorCode::HelperUnavailable, error.to_string());
                persist_failed_transition(
                    context,
                    instance_id,
                    transition_id,
                    requested_transport,
                    previous_transport,
                    started_at,
                    network_error,
                )
                .await?;
                return Err(error);
            }
        }
        let endpoint_result = {
            let path = config_path.clone();
            let endpoint = target.endpoint.clone();
            tokio::task::spawn_blocking(move || {
                set_managed_gotatun_peer_endpoint(&path, endpoint, transition_id)
            })
            .await
            .map_err(|error| AppError::Command(format!("Tunnel endpoint task failed: {error}")))?
        };
        let endpoint_update = match endpoint_result {
            Ok(update) => update,
            Err(error) => {
                let _ = rollback_tunnel(
                    config_path.clone(),
                    previous_endpoint.clone(),
                    previous_mtu,
                    transition_id,
                )
                .await;
                let _ = host_control
                    .abort_connection_profile(&AbortConnectionProfileRequest {
                        operation_id,
                        transition_id,
                        profile_revision,
                        reason: Some(error.to_string()),
                    })
                    .await;
                let network_error =
                    transition_error(NetworkErrorCode::HelperUnavailable, error.to_string());
                persist_failed_transition(
                    context,
                    instance_id,
                    transition_id,
                    requested_transport,
                    previous_transport,
                    started_at,
                    network_error,
                )
                .await?;
                return Err(error);
            }
        };

        if let Err(error) = persist_transition_phase(
            context,
            instance_id,
            transition_id,
            TransitionPhase::ValidatingTunnel,
            previous_transport,
            None,
        )
        .await
        {
            let _ =
                rollback_tunnel(config_path, previous_endpoint, previous_mtu, transition_id).await;
            let _ = host_control
                .abort_connection_profile(&AbortConnectionProfileRequest {
                    operation_id,
                    transition_id,
                    profile_revision,
                    reason: Some(error.to_string()),
                })
                .await;
            return Err(error);
        }
        let validation = validate_tunnel(
            instance_id,
            &server.wireguard_server_ip,
            &config_path,
            &endpoint_update.active_endpoint,
            &runtime_before,
            requested_transport,
        )
        .await;
        if let Err(validation_error) = validation {
            let _ = persist_transition_phase(
                context,
                instance_id,
                transition_id,
                TransitionPhase::RollingBack,
                previous_transport,
                None,
            )
            .await;
            let rollback = rollback_tunnel(
                config_path.clone(),
                previous_endpoint,
                previous_mtu,
                transition_id,
            )
            .await;
            let _ = host_control
                .abort_connection_profile(&AbortConnectionProfileRequest {
                    operation_id,
                    transition_id,
                    profile_revision,
                    reason: Some(validation_error.to_string()),
                })
                .await;
            let network_error = match rollback {
                Ok(()) => transition_error(
                    NetworkErrorCode::TunnelValidationFailed,
                    validation_error.to_string(),
                ),
                Err(rollback_error) => transition_error(
                    NetworkErrorCode::RollbackFailed,
                    format!(
                        "target validation failed: {validation_error}; rollback failed: {rollback_error}"
                    ),
                ),
            };
            persist_failed_transition(
                context,
                instance_id,
                transition_id,
                requested_transport,
                previous_transport,
                started_at,
                network_error,
            )
            .await?;
            return Err(validation_error);
        }

        if let Err(error) = persist_transition_phase(
            context,
            instance_id,
            transition_id,
            TransitionPhase::Committing,
            Some(requested_transport),
            None,
        )
        .await
        {
            let _ =
                rollback_tunnel(config_path, previous_endpoint, previous_mtu, transition_id).await;
            let _ = host_control
                .abort_connection_profile(&AbortConnectionProfileRequest {
                    operation_id,
                    transition_id,
                    profile_revision,
                    reason: Some(error.to_string()),
                })
                .await;
            return Err(error);
        }
        if let Err(error) = host_control
            .commit_connection_profile(&CommitConnectionProfileRequest {
                operation_id,
                transition_id,
                profile_revision,
            })
            .await
        {
            let _ = rollback_tunnel(
                config_path.clone(),
                previous_endpoint.clone(),
                previous_mtu,
                transition_id,
            )
            .await;
            let _ = host_control
                .abort_connection_profile(&AbortConnectionProfileRequest {
                    operation_id,
                    transition_id,
                    profile_revision,
                    reason: Some(error.to_string()),
                })
                .await;
            return Err(error);
        }
        let completed_at = Utc::now().to_rfc3339();
        let relay = target.relay_metadata;
        let committed = context
            .update_state(|state| {
                if let Some(server) = state
                    .provisioned_servers
                    .iter_mut()
                    .find(|server| server.instance_id == instance_id)
                {
                    server.network.client_revision =
                        server.network.client_revision.saturating_add(1);
                    server.network.updated_at = Some(completed_at.clone());
                    server.network.active_transport = Some(requested_transport);
                    server.network.connection_profile = Some(profile.clone());
                    server.network.last_transition = Some(ConnectionTransition {
                        transition_id,
                        requested_transport,
                        previous_transport,
                        effective_transport: Some(requested_transport),
                        phase: TransitionPhase::Completed,
                        started_at: started_at.clone(),
                        completed_at: Some(completed_at.clone()),
                        error: None,
                    });
                    match requested_transport {
                        TransportKind::Direct => {
                            server.network.direct.availability = PathAvailability::Ready;
                            server.network.direct.effective_mtu = Some(target.effective_mtu);
                        }
                        TransportKind::CloudflareTurn => {
                            server.network.cloudflare_turn.availability = PathAvailability::Ready;
                            server.network.cloudflare_turn.relay_endpoint =
                                Some(target.endpoint.clone());
                            server.network.cloudflare_turn.effective_mtu =
                                Some(target.effective_mtu);
                            if let Some(relay) = &relay {
                                server.network.cloudflare_turn.allocation_generation =
                                    relay.allocation_generation;
                                server.network.cloudflare_turn.allocation_expires_at =
                                    relay.allocation_expires_at.clone();
                                server.network.cloudflare_turn.credential_expires_at =
                                    Some(relay.credential_expires_at.clone());
                            }
                        }
                    }
                }
            })
            .await;
        match committed {
            Ok(state) => Ok(state
                .provisioned_servers
                .iter()
                .find(|server| server.instance_id == instance_id)
                .expect("committed instance remains present")
                .network
                .clone()),
            Err(error) => {
                // The host profile is already durably committed. Keep the
                // verified endpoint/MTU active rather than creating a
                // one-sided rollback. The persisted transition remains in
                // Committing and Repair connection can reconcile it.
                Err(error)
            }
        }
    }
}

async fn prepare_target(
    context: &AppContext,
    instance_id: u64,
    host: &str,
    network: &InstanceNetworkState,
    transport: TransportKind,
    current_mtu: u16,
    remote: Option<&RemoteExec>,
) -> AppResult<TargetPlan> {
    match transport {
        TransportKind::Direct => {
            let endpoint = network.direct.endpoint.clone().ok_or_else(|| {
                AppError::State("Direct WireGuard endpoint is unavailable".to_string())
            })?;
            // Vast does not publicly expose every internal port. If there is
            // no explicit UDP mapping, validate the agent through the
            // currently active WireGuard path and let post-switch handshake
            // validation verify the public Direct endpoint.
            let probe_endpoint =
                network
                    .direct
                    .probe_endpoint
                    .clone()
                    .unwrap_or_else(|| NetworkEndpoint {
                        host: "10.77.0.1".to_string(),
                        port: DIRECT_PROBE_PORT,
                    });
            let mut target = TargetPlan {
                endpoint,
                probe_endpoint,
                effective_mtu: network.direct.effective_mtu.unwrap_or(current_mtu),
                relay_metadata: None,
                packet_limits: PacketLimits {
                    verified_inner_mtu: network.direct.effective_mtu,
                    confidence: 0.25,
                    measurement_method: MeasurementMethod::ConfiguredFallback,
                    ..PacketLimits::default()
                },
            };
            match HostControl::connect(instance_id, host, remote).await {
                Ok(mut control) => {
                    let host_status = control.get_status().await?;
                    validate_host_identity(instance_id, &host_status)?;
                    let (probe_session_id, token) =
                        install_evaluation_probe(&mut control, TransportKind::Direct).await?;
                    validate_probe_target(
                        &target.probe_endpoint,
                        ProbePath::Direct,
                        probe_session_id,
                        &token,
                    )
                    .await?;
                    // While repairing an already-active Direct path, the
                    // private probe traverses that exact tunnel and can safely
                    // re-measure the inner packet ceiling after a Wi-Fi/5G
                    // path change. During a TURN -> Direct switch it would
                    // still measure TURN, so require an explicit public probe.
                    if network.direct.probe_endpoint.is_some()
                        || network.active_transport == Some(TransportKind::Direct)
                    {
                        apply_discovered_payload_limit(
                            &mut target,
                            ProbePath::Direct,
                            probe_session_id,
                            &token,
                            0,
                        )
                        .await;
                    }
                }
                Err(error) if network.direct.availability == PathAvailability::Ready => {
                    tracing::warn!(
                        instance_id,
                        %error,
                        "using previously verified Direct endpoint for client-local recovery"
                    );
                }
                Err(error) => return Err(error),
            }
            Ok(target)
        }
        TransportKind::CloudflareTurn => {
            if !network.cloudflare_turn.enabled {
                return Err(AppError::InvalidInput(
                    "Cloudflare TURN is disabled for this instance".to_string(),
                ));
            }
            let mut control = HostControl::connect(instance_id, host, remote).await?;
            let host_status = control.get_status().await?;
            validate_host_identity(instance_id, &host_status)?;
            let mut target =
                ensure_relay_target(context, network, &mut control, &host_status, false).await?;
            let (probe_session_id, token) =
                install_evaluation_probe(&mut control, TransportKind::CloudflareTurn).await?;
            if let Err(first_error) = validate_probe_target(
                &target.probe_endpoint,
                ProbePath::CloudflareTurn,
                probe_session_id,
                &token,
            )
            .await
            {
                // A TURN allocation can become unusable while its temporary
                // credential is still fresh (for example after a relay or
                // NAT path interruption). Do not repeatedly probe a dead
                // endpoint: replace it once, then report the original and
                // replacement failures together if the fresh allocation also
                // cannot be validated.
                tracing::warn!(
                    instance_id,
                    %first_error,
                    endpoint = ?target.probe_endpoint,
                    "TURN probe failed; replacing the allocation before rollback"
                );
                target = ensure_relay_target(
                    context,
                    network,
                    &mut control,
                    &host_status,
                    true,
                )
                .await
                .map_err(|error| {
                    AppError::Timeout(format!(
                        "Initial TURN probe failed ({first_error}); fresh allocation failed: {error}"
                    ))
                })?;
                let (fresh_probe_session_id, fresh_token) =
                    install_evaluation_probe(&mut control, TransportKind::CloudflareTurn).await?;
                validate_probe_target(
                    &target.probe_endpoint,
                    ProbePath::CloudflareTurn,
                    fresh_probe_session_id,
                    &fresh_token,
                )
                .await
                .map_err(|error| {
                    AppError::Timeout(format!(
                        "Initial TURN probe failed ({first_error}); fresh TURN probe failed: {error}"
                    ))
                })?;
            }
            if let Some(generation) = target
                .relay_metadata
                .as_ref()
                .map(|metadata| metadata.allocation_generation)
            {
                let (payload_session_id, payload_token) =
                    install_evaluation_probe(&mut control, TransportKind::CloudflareTurn).await?;
                apply_discovered_payload_limit(
                    &mut target,
                    ProbePath::CloudflareTurn,
                    payload_session_id,
                    &payload_token,
                    generation,
                )
                .await;
            }
            Ok(target)
        }
    }
}

fn validate_host_identity(instance_id: u64, status: &HostNetworkStatus) -> AppResult<()> {
    if status.state.instance_id != instance_id.to_string() {
        return Err(AppError::State(
            "Host control returned a different instance identity".to_string(),
        ));
    }
    Ok(())
}

fn turn_status_is_healthy(
    network: &InstanceNetworkState,
    status: &HostNetworkStatus,
    public_ip: std::net::IpAddr,
) -> bool {
    let credential_is_fresh = status
        .state
        .credential_expires_at
        .as_deref()
        .and_then(|value| chrono::DateTime::parse_from_rfc3339(value).ok())
        .is_some_and(|expires_at| {
            expires_at.with_timezone(&Utc) > Utc::now() + ChronoDuration::minutes(10)
        });
    let endpoint_matches = status
        .state
        .relay_endpoint
        .as_ref()
        .zip(network.cloudflare_turn.relay_endpoint.as_ref())
        .is_some_and(|(host, client)| host == client);
    let bridge_matches = status.bridge.as_ref().is_some_and(|bridge| {
        bridge.allocation_generation == status.state.allocation_generation
            && bridge
                .allowed_peer_ips
                .iter()
                .any(|allowed| allowed == &public_ip.to_string())
    });
    status.state.turn_status == noland_network_contracts::state::TurnRuntimeStatus::Ready
        && status.state.allocation_generation == network.cloudflare_turn.allocation_generation
        && credential_is_fresh
        && endpoint_matches
        && bridge_matches
}

async fn ensure_relay_target(
    context: &AppContext,
    network: &InstanceNetworkState,
    control: &mut HostControl,
    host_status: &HostNetworkStatus,
    force_new_allocation: bool,
) -> AppResult<TargetPlan> {
    let public_ip = cloudflare_turn::discover_client_public_ip(&context.http_client).await?;
    let credential_expires_at = host_status
        .state
        .credential_expires_at
        .as_deref()
        .and_then(|value| chrono::DateTime::parse_from_rfc3339(value).ok())
        .map(|value| value.with_timezone(&Utc));
    let bridge_matches = host_status.bridge.as_ref().is_some_and(|bridge| {
        bridge.allocation_generation == host_status.state.allocation_generation
            && bridge
                .allowed_peer_ips
                .iter()
                .any(|allowed| allowed == &public_ip.to_string())
    });
    let reusable = host_status.state.turn_status
        == noland_network_contracts::state::TurnRuntimeStatus::Ready
        && credential_expires_at
            .is_some_and(|expires_at| expires_at > Utc::now() + ChronoDuration::minutes(10))
        && bridge_matches;
    if !force_new_allocation && reusable {
        if let (Some(endpoint), Some(credential_expires_at)) = (
            host_status.state.relay_endpoint.clone(),
            host_status.state.credential_expires_at.clone(),
        ) {
            return Ok(TargetPlan {
                endpoint: endpoint.clone(),
                probe_endpoint: endpoint,
                effective_mtu: network
                    .cloudflare_turn
                    .effective_mtu
                    .unwrap_or(TURN_EFFECTIVE_MTU),
                relay_metadata: Some(RelayMetadata {
                    allocation_generation: host_status.state.allocation_generation,
                    allocation_expires_at: host_status.state.allocation_expires_at.clone(),
                    credential_expires_at,
                }),
                packet_limits: PacketLimits {
                    verified_inner_mtu: network.cloudflare_turn.effective_mtu,
                    confidence: 0.25,
                    measurement_method: MeasurementMethod::ConfiguredFallback,
                    ..PacketLimits::default()
                },
            });
        }
    }

    let credentials = cloudflare_turn::generate_runtime_credentials(&context.http_client).await?;
    let generation = host_status
        .state
        .allocation_generation
        .max(network.cloudflare_turn.allocation_generation)
        .saturating_add(1);
    let prepared = control
        .prepare_turn(&PrepareTurnRequest {
            operation_id: Uuid::new_v4(),
            requested_generation: generation,
            turn_urls: credentials.urls,
            username: credentials.username,
            credential: credentials.credential,
            credential_expires_at: credentials.expires_at,
            expected_peer_ips: vec![public_ip.to_string()],
            effective_mtu: TURN_EFFECTIVE_MTU,
        })
        .await?;
    Ok(TargetPlan {
        endpoint: prepared.relay_endpoint.clone(),
        probe_endpoint: prepared.relay_endpoint,
        effective_mtu: TURN_EFFECTIVE_MTU,
        relay_metadata: Some(RelayMetadata {
            allocation_generation: prepared.allocation_generation,
            allocation_expires_at: prepared.allocation_expires_at,
            credential_expires_at: prepared.credential_expires_at,
        }),
        packet_limits: PacketLimits {
            verified_inner_mtu: Some(TURN_EFFECTIVE_MTU),
            confidence: 0.25,
            measurement_method: MeasurementMethod::ConfiguredFallback,
            ..PacketLimits::default()
        },
    })
}

async fn apply_discovered_payload_limit(
    target: &mut TargetPlan,
    path: ProbePath,
    session_id: Uuid,
    token: &[u8; 32],
    profile_generation: u64,
) {
    let requested_ceiling = target
        .effective_mtu
        .saturating_add(WIREGUARD_DATA_OVERHEAD)
        .clamp(MIN_STREAMING_INNER_MTU + WIREGUARD_DATA_OVERHEAD, 1500);
    match discover_probe_payload_ceiling(
        &target.probe_endpoint,
        path,
        session_id,
        token,
        profile_generation,
        requested_ceiling,
    )
    .await
    {
        Ok(Some(payload_ceiling)) => {
            let verified_inner_mtu = payload_ceiling
                .saturating_sub(WIREGUARD_DATA_OVERHEAD)
                .clamp(MIN_STREAMING_INNER_MTU, target.effective_mtu);
            target.effective_mtu = verified_inner_mtu;
            target.packet_limits.forward_payload_ceiling = Some(payload_ceiling);
            target.packet_limits.verified_inner_mtu = Some(verified_inner_mtu);
            target.packet_limits.confidence = 0.5;
            target.packet_limits.measurement_method = MeasurementMethod::PacketizationLayerProbe;
        }
        Ok(None) => tracing::warn!(
            ?path,
            endpoint = ?target.probe_endpoint,
            "padded path probe could not establish a streaming payload ceiling; retaining the conservative candidate"
        ),
        Err(error) => tracing::warn!(
            ?path,
            endpoint = ?target.probe_endpoint,
            %error,
            "padded path probe failed; retaining the conservative candidate"
        ),
    }
}

async fn discover_probe_payload_ceiling(
    endpoint: &NetworkEndpoint,
    path: ProbePath,
    session_id: Uuid,
    token: &[u8; 32],
    profile_generation: u64,
    upper_bound: u16,
) -> AppResult<Option<u16>> {
    let destination = resolve_endpoint(endpoint).await?;
    let socket = UdpSocket::bind(if destination.is_ipv4() {
        "0.0.0.0:0"
    } else {
        "[::]:0"
    })
    .await?;
    let minimum = MIN_STREAMING_INNER_MTU + WIREGUARD_DATA_OVERHEAD;
    let upper_bound = upper_bound.max(minimum);
    if !probe_payload_candidate(
        &socket,
        destination,
        path,
        session_id,
        token,
        profile_generation,
        minimum,
        1,
    )
    .await?
    {
        return Ok(None);
    }
    let candidates = (minimum..=upper_bound).step_by(4).collect::<Vec<_>>();
    let mut low = 0usize;
    let mut high = candidates.len().saturating_sub(1);
    let mut sequence = 2_u64;
    while low < high {
        let midpoint = low + (high - low).div_ceil(2);
        let delivered = probe_payload_candidate(
            &socket,
            destination,
            path,
            session_id,
            token,
            profile_generation,
            candidates[midpoint],
            sequence,
        )
        .await?;
        sequence = sequence.saturating_add(1);
        if delivered {
            low = midpoint;
        } else {
            high = midpoint.saturating_sub(1);
        }
    }
    Ok(Some(candidates[low]))
}

#[allow(clippy::too_many_arguments)]
async fn probe_payload_candidate(
    socket: &UdpSocket,
    destination: SocketAddr,
    path: ProbePath,
    session_id: Uuid,
    token: &[u8; 32],
    profile_generation: u64,
    payload_size: u16,
    sequence: u64,
) -> AppResult<bool> {
    let packet = ProbePacket::v3(
        PacketType::Probe,
        path,
        ProbeDirection::ClientToHost,
        sequence,
        session_id,
        monotonic_us(),
        profile_generation,
        payload_size,
    )
    .encode(token);
    if packet.is_empty() {
        return Ok(false);
    }
    socket.send_to(&packet, destination).await?;
    let mut response = [0_u8; 128];
    let Ok(Ok((received, source))) =
        timeout(PAYLOAD_PROBE_TIMEOUT, socket.recv_from(&mut response)).await
    else {
        return Ok(false);
    };
    let Some(ack) = ProbePacket::decode_and_verify(&response[..received], token) else {
        return Ok(false);
    };
    Ok(source == destination
        && ack.version == 3
        && ack.packet_type == PacketType::Ack
        && ack.path == path
        && ack.direction == ProbeDirection::ClientToHost
        && ack.sequence == sequence
        && ack.session_id == session_id
        && ack.profile_generation == profile_generation
        && ack.payload_size == payload_size
        && ack.observed_payload_size == payload_size)
}

async fn install_evaluation_probe(
    control: &mut HostControl,
    transport: TransportKind,
) -> AppResult<(Uuid, [u8; 32])> {
    let probe_session_id = Uuid::new_v4();
    let mut token = [0_u8; 32];
    OsRng.fill_bytes(&mut token);
    control
        .install_probe_session(&InstallProbeSessionRequest {
            probe_session_id,
            token: hex::encode(token),
            expires_at: (Utc::now() + ChronoDuration::minutes(2)).to_rfc3339(),
            max_packets_per_second: 20,
            allowed_paths: vec![transport],
        })
        .await?;
    Ok((probe_session_id, token))
}

async fn sample_if_ready(
    endpoint: Option<NetworkEndpoint>,
    path: ProbePath,
    session: AppResult<(Uuid, [u8; 32])>,
) -> AppResult<PathMetrics> {
    let endpoint = endpoint
        .ok_or_else(|| AppError::State(format!("{path:?} probe endpoint is unavailable")))?;
    let (session_id, token) = session?;
    sample_probe_path(&endpoint, path, session_id, &token).await
}

async fn sample_probe_path(
    endpoint: &NetworkEndpoint,
    path: ProbePath,
    session_id: Uuid,
    token: &[u8; 32],
) -> AppResult<PathMetrics> {
    let destination = resolve_endpoint(endpoint).await?;
    let socket = UdpSocket::bind(if destination.is_ipv4() {
        "0.0.0.0:0"
    } else {
        "[::]:0"
    })
    .await?;
    let started_at = Instant::now();
    let deadline = started_at
        + EVALUATION_SAMPLE_INTERVAL * EVALUATION_SAMPLE_COUNT as u32
        + EVALUATION_DRAIN_TIME;
    let mut ticker = interval(EVALUATION_SAMPLE_INTERVAL);
    ticker.set_missed_tick_behavior(MissedTickBehavior::Delay);
    let mut sent = 0_u64;
    let mut pending = HashMap::new();
    let mut received = Vec::new();
    let mut maximum_received_sequence: Option<u64> = None;
    let mut reordered = 0_usize;
    let mut last_received_at = None;
    let mut buffer = [0_u8; 128];

    loop {
        tokio::select! {
            _ = ticker.tick(), if sent < EVALUATION_SAMPLE_COUNT => {
                sent = sent.saturating_add(1);
                let packet = ProbePacket::v2(
                    PacketType::Probe,
                    path,
                    sent,
                    session_id,
                    monotonic_us(),
                ).encode(token);
                let sent_at = Instant::now();
                socket.send_to(&packet, destination).await?;
                pending.insert(sent, sent_at);
            }
            response = socket.recv_from(&mut buffer) => {
                let (length, source) = response?;
                if source != destination {
                    continue;
                }
                let Some(ack) = ProbePacket::decode_and_verify(&buffer[..length], token) else {
                    continue;
                };
                if ack.packet_type != PacketType::Ack
                    || ack.path != path
                    || ack.session_id != session_id
                {
                    continue;
                }
                let Some(sent_at) = pending.remove(&ack.sequence) else {
                    continue;
                };
                if maximum_received_sequence.is_some_and(|maximum| ack.sequence < maximum) {
                    reordered = reordered.saturating_add(1);
                }
                maximum_received_sequence = Some(
                    maximum_received_sequence
                        .map_or(ack.sequence, |maximum| maximum.max(ack.sequence)),
                );
                received.push((ack.sequence, sent_at.elapsed().as_secs_f64() * 1_000.0));
                last_received_at = Some(Instant::now());
            }
            _ = sleep_until(deadline) => break,
        }
    }

    Ok(path_metrics(
        sent,
        received,
        reordered,
        last_received_at
            .map(|received_at| received_at.elapsed())
            .unwrap_or_else(|| started_at.elapsed()),
    ))
}

fn path_metrics(
    sent: u64,
    mut received: Vec<(u64, f64)>,
    reordered: usize,
    sample_age: Duration,
) -> PathMetrics {
    received.sort_by_key(|(sequence, _)| *sequence);
    let mut sequence_rtts = received.iter().map(|(_, rtt)| *rtt).collect::<Vec<_>>();
    let jitter_ms = ewma_jitter(&sequence_rtts);
    sequence_rtts.sort_by(f64::total_cmp);
    let median_rtt_ms = percentile(&sequence_rtts, 0.5);
    let p95_rtt_ms = percentile(&sequence_rtts, 0.95);
    let p99_rtt_ms = percentile(&sequence_rtts, 0.99);
    let spike_threshold = median_rtt_ms.map(|median| median + 15.0_f64.max(jitter_ms * 3.0));
    let spikes = spike_threshold
        .map(|threshold| sequence_rtts.iter().filter(|rtt| **rtt > threshold).count())
        .unwrap_or_default();
    let received_count = received.len();
    let lost = usize::try_from(sent)
        .unwrap_or(usize::MAX)
        .saturating_sub(received_count);
    PathMetrics {
        sample_count: received_count.try_into().unwrap_or(u32::MAX),
        sent_count: sent.try_into().unwrap_or(u32::MAX),
        received_count: received_count.try_into().unwrap_or(u32::MAX),
        lost_count: lost.try_into().unwrap_or(u32::MAX),
        sample_age_ms: sample_age.as_millis().try_into().unwrap_or(u64::MAX),
        median_rtt_ms,
        p95_rtt_ms,
        p99_rtt_ms,
        jitter_ms,
        loss_percent: percentage(lost, usize::try_from(sent).unwrap_or(usize::MAX)),
        spike_percent: percentage(spikes, received_count),
        reordering_percent: Some(percentage(reordered, received_count)),
    }
}

fn percentile(sorted: &[f64], percentile: f64) -> Option<f64> {
    if sorted.is_empty() {
        return None;
    }
    let rank = (percentile * sorted.len() as f64).ceil() as usize;
    Some(sorted[rank.saturating_sub(1).min(sorted.len() - 1)])
}

fn ewma_jitter(rtts: &[f64]) -> f64 {
    let mut previous = None;
    let mut jitter = 0.0;
    for rtt in rtts {
        if let Some(previous_rtt) = previous {
            let difference: f64 = *rtt - previous_rtt;
            jitter += (difference.abs() - jitter) / 16.0;
        }
        previous = Some(*rtt);
    }
    jitter
}

fn percentage(part: usize, total: usize) -> f64 {
    if total == 0 {
        0.0
    } else {
        part as f64 * 100.0 / total as f64
    }
}

async fn persist_evaluation(
    context: &AppContext,
    instance_id: u64,
    evaluation: &ConnectionEvaluation,
    relay_target: Option<&TargetPlan>,
) -> AppResult<()> {
    let evaluated_at = evaluation.evaluated_at.clone();
    let evaluation = evaluation.clone();
    context
        .update_state(|state| {
            if let Some(server) = state
                .provisioned_servers
                .iter_mut()
                .find(|server| server.instance_id == instance_id)
            {
                server.network.client_revision = server.network.client_revision.saturating_add(1);
                server.network.updated_at = Some(evaluated_at);
                server.network.fallback_reason = Some(evaluation.reason);
                server.network.direct.availability = availability(&evaluation.direct.metrics);
                server.network.cloudflare_turn.availability =
                    availability(&evaluation.cloudflare_turn.metrics);
                if let Some(target) = relay_target {
                    server.network.cloudflare_turn.relay_endpoint = Some(target.endpoint.clone());
                    server.network.cloudflare_turn.effective_mtu = Some(target.effective_mtu);
                    if let Some(metadata) = &target.relay_metadata {
                        server.network.cloudflare_turn.allocation_generation =
                            metadata.allocation_generation;
                        server.network.cloudflare_turn.allocation_expires_at =
                            metadata.allocation_expires_at.clone();
                        server.network.cloudflare_turn.credential_expires_at =
                            Some(metadata.credential_expires_at.clone());
                    }
                }
                server.network.last_evaluation = Some(evaluation);
            }
        })
        .await?;
    Ok(())
}

fn availability(metrics: &PathMetrics) -> PathAvailability {
    if metrics.median_rtt_ms.is_none() {
        PathAvailability::Unavailable
    } else if metrics.sample_count < EvaluationPolicy::default().minimum_samples
        || metrics.loss_percent > 5.0
    {
        PathAvailability::Degraded
    } else {
        PathAvailability::Ready
    }
}

fn remote_for_server(
    context: &AppContext,
    state: &PersistedAppState,
    server: &ProvisionedServerState,
) -> Option<RemoteExec> {
    (!state.ssh.private_key_path.trim().is_empty()
        && !server.ssh_host.trim().is_empty()
        && server.ssh_port != 0)
        .then(|| RemoteExec {
            ssh_user: if state.ssh.ssh_username.trim().is_empty() {
                context.config.audio_target_user.clone()
            } else {
                state.ssh.ssh_username.clone()
            },
            ssh_host: server.ssh_host.clone(),
            ssh_port: server.ssh_port,
            private_key_path: state.ssh.private_key_path.clone(),
            ssh_password: state.ssh.ssh_password.clone(),
        })
}

impl HostControl {
    async fn connect(instance_id: u64, host: &str, remote: Option<&RemoteExec>) -> AppResult<Self> {
        match NetworkControlClient::connect(instance_id, host).await {
            Ok(client) => Ok(Self::Tunnel(client)),
            Err(tunnel_error) => {
                let Some(remote) = remote.cloned() else {
                    return Err(tunnel_error);
                };
                let tunnel_error = tunnel_error.to_string();
                let status: HostNetworkStatus = call_local_control_via_ssh(
                    &remote,
                    "get_network_status",
                    &serde_json::json!({}),
                )
                .await
                .map_err(|ssh_error| {
                    AppError::Command(format!(
                        "Host control is unavailable through both the tunnel ({tunnel_error}) and SSH ({ssh_error})"
                    ))
                })?;
                if status.state.instance_id != instance_id.to_string() {
                    return Err(AppError::State(
                        "SSH host control returned a different instance identity".to_string(),
                    ));
                }
                Ok(Self::Ssh(remote))
            }
        }
    }

    async fn get_status(&mut self) -> AppResult<HostNetworkStatus> {
        match self {
            Self::Tunnel(client) => client.get_status().await,
            Self::Ssh(remote) => {
                call_local_control_via_ssh(remote, "get_network_status", &serde_json::json!({}))
                    .await
            }
        }
    }

    async fn prepare_turn(
        &mut self,
        request: &PrepareTurnRequest,
    ) -> AppResult<noland_network_contracts::control::PrepareTurnResponse> {
        match self {
            Self::Tunnel(client) => client.prepare_turn(request).await,
            Self::Ssh(remote) => call_local_control_via_ssh(remote, "prepare_turn", request).await,
        }
    }

    async fn prepare_connection_profile(
        &mut self,
        request: &PrepareConnectionProfileRequest,
    ) -> AppResult<noland_network_contracts::control::PrepareConnectionProfileResponse> {
        match self {
            Self::Tunnel(client) => client.prepare_connection_profile(request).await,
            Self::Ssh(remote) => {
                call_local_control_via_ssh(remote, "prepare_connection_profile", request).await
            }
        }
    }

    async fn commit_connection_profile(
        &mut self,
        request: &CommitConnectionProfileRequest,
    ) -> AppResult<noland_network_contracts::state::HostLinkState> {
        match self {
            Self::Tunnel(client) => client.commit_connection_profile(request).await,
            Self::Ssh(remote) => {
                call_local_control_via_ssh(remote, "commit_connection_profile", request).await
            }
        }
    }

    async fn abort_connection_profile(
        &mut self,
        request: &AbortConnectionProfileRequest,
    ) -> AppResult<noland_network_contracts::state::HostLinkState> {
        match self {
            Self::Tunnel(client) => client.abort_connection_profile(request).await,
            Self::Ssh(remote) => {
                call_local_control_via_ssh(remote, "abort_connection_profile", request).await
            }
        }
    }

    async fn install_probe_session(
        &mut self,
        request: &InstallProbeSessionRequest,
    ) -> AppResult<()> {
        match self {
            Self::Tunnel(client) => client.install_probe_session(request).await,
            Self::Ssh(remote) => {
                let _: serde_json::Value =
                    call_local_control_via_ssh(remote, "install_probe_session", request).await?;
                Ok(())
            }
        }
    }

    async fn stop_turn(&mut self) -> AppResult<()> {
        match self {
            Self::Tunnel(client) => {
                client.stop_turn().await?;
            }
            Self::Ssh(remote) => {
                let _: noland_network_contracts::state::HostNetworkState =
                    call_local_control_via_ssh(remote, "stop_turn", &serde_json::json!({})).await?;
            }
        }
        Ok(())
    }
}

async fn validate_probe_target(
    endpoint: &NetworkEndpoint,
    path: ProbePath,
    session_id: Uuid,
    token: &[u8; 32],
) -> AppResult<()> {
    let destination = resolve_endpoint(endpoint).await?;
    let bind = if destination.is_ipv4() {
        "0.0.0.0:0"
    } else {
        "[::]:0"
    };
    let socket = UdpSocket::bind(bind).await?;
    for sequence in 1..=TARGET_PROBE_ATTEMPTS {
        let packet = ProbePacket::v2(
            PacketType::Probe,
            path,
            sequence,
            session_id,
            monotonic_us(),
        )
        .encode(token);
        socket.send_to(&packet, destination).await?;
        let mut response = [0_u8; 128];
        if let Ok(Ok((received, source))) =
            timeout(Duration::from_secs(1), socket.recv_from(&mut response)).await
        {
            let acknowledged = ProbePacket::decode_and_verify(&response[..received], token);
            if source == destination
                && acknowledged.is_some_and(|ack| {
                    ack.packet_type == PacketType::Ack
                        && ack.path == path
                        && ack.sequence == sequence
                        && ack.session_id == session_id
                })
            {
                return Ok(());
            }
        }
    }
    Err(AppError::Timeout(format!(
        "No authenticated {:?} probe response arrived from {}:{}",
        path, endpoint.host, endpoint.port
    )))
}

async fn validate_tunnel(
    instance_id: u64,
    host: &str,
    config_path: &PathBuf,
    expected_endpoint: &str,
    before: &ManagedTunnelRuntime,
    transport: TransportKind,
) -> AppResult<()> {
    let deadline = tokio::time::Instant::now() + TUNNEL_VALIDATION_TIMEOUT;
    let mut last_error = "tunnel validation did not run".to_string();
    while tokio::time::Instant::now() < deadline {
        let control_ok = match NetworkControlClient::connect(instance_id, host).await {
            Ok(mut control) => match control.get_status().await {
                Ok(status) => {
                    let observed = status.state.observed_transport;
                    let valid = status.state.instance_id == instance_id.to_string()
                        && (transport != TransportKind::CloudflareTurn
                            || observed == Some(TransportKind::CloudflareTurn));
                    if !valid {
                        last_error = format!(
                                "Host did not confirm the requested {transport:?} transport (observed {observed:?})"
                            );
                    }
                    valid
                }
                Err(error) => {
                    last_error = error.to_string();
                    false
                }
            },
            Err(error) => {
                last_error = error.to_string();
                false
            }
        };
        let sunshine_ok = timeout(
            Duration::from_secs(2),
            tokio::net::TcpStream::connect((host, 47990)),
        )
        .await
        .is_ok_and(|result| result.is_ok());
        let runtime = managed_runtime(config_path.clone()).await;
        let runtime_matches = runtime
            .as_ref()
            .is_ok_and(|runtime| runtime.active && runtime.endpoint == expected_endpoint);
        let runtime_has_fresh_stats = runtime.as_ref().is_ok_and(|runtime| {
            runtime.tx_bytes > before.tx_bytes
                || runtime.rx_bytes > before.rx_bytes
                || runtime
                    .latest_handshake_age_secs
                    .is_some_and(|age| age <= 10)
        });
        if control_ok && sunshine_ok && runtime_matches {
            if !runtime_has_fresh_stats {
                tracing::debug!(
                    ?transport,
                    expected_endpoint,
                    "committing tunnel after end-to-end host checks despite a stale local runtime counter"
                );
            }
            return Ok(());
        }
        if !runtime_matches {
            last_error = match runtime {
                Ok(runtime) if !runtime.active => {
                    "Managed tunnel became inactive during validation".to_string()
                }
                Ok(runtime) => format!(
                    "Managed tunnel endpoint mismatch: expected {expected_endpoint}, observed {}",
                    runtime.endpoint
                ),
                Err(error) => error.to_string(),
            };
        } else if !sunshine_ok {
            last_error = "Sunshine control endpoint is unreachable through the tunnel".to_string();
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    Err(AppError::Timeout(last_error))
}

async fn rollback_tunnel(
    config_path: PathBuf,
    endpoint: NetworkEndpoint,
    mtu: u16,
    transition_id: Uuid,
) -> AppResult<()> {
    let endpoint_path = config_path.clone();
    tokio::task::spawn_blocking(move || {
        set_managed_gotatun_peer_endpoint(&endpoint_path, endpoint, transition_id)
    })
    .await
    .map_err(|error| AppError::Command(format!("Tunnel rollback task failed: {error}")))??;
    tokio::task::spawn_blocking(move || set_managed_gotatun_mtu(&config_path, mtu, transition_id))
        .await
        .map_err(|error| {
            AppError::Command(format!("Tunnel MTU rollback task failed: {error}"))
        })??;
    Ok(())
}

async fn managed_runtime(config_path: PathBuf) -> AppResult<ManagedTunnelRuntime> {
    tokio::task::spawn_blocking(move || get_managed_gotatun_runtime(&config_path))
        .await
        .map_err(|error| AppError::Command(format!("Tunnel runtime task failed: {error}")))?
}

async fn resolve_endpoint(endpoint: &NetworkEndpoint) -> AppResult<SocketAddr> {
    endpoint
        .validate()
        .map_err(|error| AppError::InvalidInput(error.to_string()))?;
    let authority = if endpoint.host.contains(':') {
        format!("[{}]:{}", endpoint.host, endpoint.port)
    } else {
        format!("{}:{}", endpoint.host, endpoint.port)
    };
    tokio::net::lookup_host(authority)
        .await?
        .next()
        .ok_or_else(|| AppError::Command("Network endpoint resolved to no addresses".to_string()))
}

fn parse_runtime_endpoint(value: &str) -> AppResult<NetworkEndpoint> {
    let endpoint = value.parse::<SocketAddr>().map_err(|error| {
        AppError::State(format!(
            "Managed tunnel reported an invalid endpoint `{value}`: {error}"
        ))
    })?;
    Ok(NetworkEndpoint {
        host: endpoint.ip().to_string(),
        port: endpoint.port(),
    })
}

async fn persist_transition(
    context: &AppContext,
    instance_id: u64,
    transition: ConnectionTransition,
) -> AppResult<()> {
    context
        .update_state(|state| {
            if let Some(server) = state
                .provisioned_servers
                .iter_mut()
                .find(|server| server.instance_id == instance_id)
            {
                server.network.updated_at = Some(Utc::now().to_rfc3339());
                server.network.last_transition = Some(transition);
            }
        })
        .await?;
    Ok(())
}

async fn persist_transition_phase(
    context: &AppContext,
    instance_id: u64,
    transition_id: Uuid,
    phase: TransitionPhase,
    effective_transport: Option<TransportKind>,
    error: Option<NetworkError>,
) -> AppResult<()> {
    context
        .update_state(|state| {
            if let Some(server) = state
                .provisioned_servers
                .iter_mut()
                .find(|server| server.instance_id == instance_id)
            {
                server.network.updated_at = Some(Utc::now().to_rfc3339());
                if let Some(transition) = server.network.last_transition.as_mut() {
                    if transition.transition_id == transition_id {
                        transition.phase = phase;
                        transition.effective_transport = effective_transport;
                        transition.error = error;
                    }
                }
            }
        })
        .await?;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn persist_failed_transition(
    context: &AppContext,
    instance_id: u64,
    transition_id: Uuid,
    requested_transport: TransportKind,
    previous_transport: Option<TransportKind>,
    started_at: String,
    mut error: NetworkError,
) -> AppResult<()> {
    error.transition_id = Some(transition_id);
    error.transport = Some(requested_transport);
    let completed_at = Utc::now().to_rfc3339();
    context
        .update_state(|state| {
            if let Some(server) = state
                .provisioned_servers
                .iter_mut()
                .find(|server| server.instance_id == instance_id)
            {
                server.network.client_revision = server.network.client_revision.saturating_add(1);
                server.network.updated_at = Some(completed_at.clone());
                server.network.last_transition = Some(ConnectionTransition {
                    transition_id,
                    requested_transport,
                    previous_transport,
                    effective_transport: previous_transport,
                    phase: TransitionPhase::Failed,
                    started_at,
                    completed_at: Some(completed_at),
                    error: Some(error),
                });
            }
        })
        .await?;
    Ok(())
}

fn transition_error(code: NetworkErrorCode, message: String) -> NetworkError {
    NetworkError {
        code,
        message,
        retryable: true,
        transport: None,
        transition_id: None,
        details: None,
    }
}

fn monotonic_us() -> u64 {
    use std::sync::OnceLock;
    use std::time::Instant;
    static START: OnceLock<Instant> = OnceLock::new();
    START
        .get_or_init(Instant::now)
        .elapsed()
        .as_micros()
        .try_into()
        .unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::network_control::TurnBridgeStatus;
    use noland_network_contracts::{
        state::{HostNetworkState, TurnRuntimeStatus},
        NETWORK_STATE_SCHEMA_VERSION,
    };

    #[test]
    fn parses_ipv4_and_ipv6_helper_endpoints_without_persisting_brackets() {
        assert_eq!(
            parse_runtime_endpoint("198.51.100.8:51820").unwrap(),
            NetworkEndpoint {
                host: "198.51.100.8".into(),
                port: 51820,
            }
        );
        assert_eq!(
            parse_runtime_endpoint("[2001:db8::8]:3478").unwrap(),
            NetworkEndpoint {
                host: "2001:db8::8".into(),
                port: 3478,
            }
        );
    }

    #[test]
    fn path_metrics_include_loss_percentiles_jitter_and_reordering() {
        let metrics = path_metrics(
            5,
            vec![(1, 10.0), (3, 14.0), (2, 12.0), (5, 50.0)],
            1,
            Duration::from_millis(25),
        );
        assert_eq!(metrics.sample_count, 4);
        assert_eq!(metrics.sent_count, 5);
        assert_eq!(metrics.received_count, 4);
        assert_eq!(metrics.lost_count, 1);
        assert_eq!(metrics.sample_age_ms, 25);
        assert_eq!(metrics.loss_percent, 20.0);
        assert_eq!(metrics.median_rtt_ms, Some(12.0));
        assert_eq!(metrics.p95_rtt_ms, Some(50.0));
        assert_eq!(metrics.p99_rtt_ms, Some(50.0));
        assert_eq!(metrics.reordering_percent, Some(25.0));
        assert!(metrics.jitter_ms > 0.0);
        assert_eq!(metrics.spike_percent, 25.0);
    }

    #[test]
    fn path_with_no_acknowledgements_is_unavailable() {
        let metrics = path_metrics(60, Vec::new(), 0, Duration::from_secs(7));
        assert_eq!(metrics.loss_percent, 100.0);
        assert_eq!(metrics.median_rtt_ms, None);
        assert_eq!(availability(&metrics), PathAvailability::Unavailable);
    }

    #[tokio::test]
    async fn padded_probe_finds_size_dependent_delivery_ceiling() {
        let token = [0x51; 32];
        let session_id = Uuid::from_u128(31);
        let server = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let address = server.local_addr().unwrap();
        let task = tokio::spawn(async move {
            let mut buffer = [0_u8; 2_048];
            loop {
                let (received, peer) = server.recv_from(&mut buffer).await.unwrap();
                if received <= 660 {
                    let response = noland_network_contracts::probe::acknowledge_probe(
                        &buffer[..received],
                        &token,
                    )
                    .unwrap();
                    server.send_to(&response, peer).await.unwrap();
                }
            }
        });
        let endpoint = NetworkEndpoint {
            host: address.ip().to_string(),
            port: address.port(),
        };
        let ceiling = discover_probe_payload_ceiling(
            &endpoint,
            ProbePath::Direct,
            session_id,
            &token,
            9,
            700,
        )
        .await
        .unwrap();
        task.abort();
        assert_eq!(ceiling, Some(660));
    }

    #[test]
    fn turn_health_binds_generation_endpoint_expiry_and_public_ip() {
        let endpoint = NetworkEndpoint {
            host: "192.0.2.44".into(),
            port: 3478,
        };
        let mut network = InstanceNetworkState::default();
        network.cloudflare_turn.allocation_generation = 8;
        network.cloudflare_turn.relay_endpoint = Some(endpoint.clone());
        let status = HostNetworkStatus {
            state: HostNetworkState {
                schema_version: NETWORK_STATE_SCHEMA_VERSION,
                host_revision: 2,
                updated_at: Utc::now().to_rfc3339(),
                instance_id: "42".into(),
                session_id: Uuid::new_v4(),
                agent_version: "test".into(),
                turn_status: TurnRuntimeStatus::Ready,
                allocation_generation: 8,
                relay_endpoint: Some(endpoint),
                allocation_expires_at: None,
                credential_expires_at: Some((Utc::now() + ChronoDuration::hours(1)).to_rfc3339()),
                observed_transport: Some(TransportKind::CloudflareTurn),
                link_state: Default::default(),
                committed_profile: None,
                committed_operation_id: None,
                pending_profile: None,
            },
            bridge: Some(TurnBridgeStatus {
                allocation_generation: 8,
                relay_endpoint: Some("192.0.2.44:3478".parse().unwrap()),
                allowed_peer_ips: vec!["198.51.100.9".into()],
                wireguard_packets_received: 1,
                wireguard_packets_sent: 1,
                probe_packets_received: 1,
                dropped_packets: 0,
                last_packet_at_unix_ms: None,
                active_peer_tuple: None,
            }),
        };
        assert!(turn_status_is_healthy(
            &network,
            &status,
            "198.51.100.9".parse().unwrap()
        ));
        assert!(!turn_status_is_healthy(
            &network,
            &status,
            "198.51.100.10".parse().unwrap()
        ));
    }
}
