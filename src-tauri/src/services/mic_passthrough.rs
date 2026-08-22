use std::{
    collections::HashMap,
    net::{IpAddr, UdpSocket},
    path::Path,
    time::Duration,
};

use parking_lot::Mutex as SyncMutex;
use tokio::sync::RwLock;
use tracing::{info, warn};

use crate::errors::{AppError, AppResult};
use crate::mic_client::{
    self,
    device_list::{list_devices as list_sidecar_devices, MicrophoneDevice},
    MicClientConfig, MicClientHandle,
};
use crate::models::app_state::{
    InstanceMicConfig, InstanceMicRuntimeStatus, MicQualityProfile, MicSessionResponse,
    MicSettingsUpdate, MicState,
};

use super::{
    app_context::AppContext, mic_receiver::MicReceiverProvisioner, remote_exec::RemoteExec,
    wireguard::verify_managed_gotatun_tunnel,
};

/// In-memory mic session tracking per instance.
static MIC_SESSIONS: std::sync::OnceLock<RwLock<HashMap<u64, MicSession>>> =
    std::sync::OnceLock::new();

/// Pipeline handles keyed by instance_id.
static MIC_HANDLES: std::sync::OnceLock<SyncMutex<HashMap<u64, ActiveMicPipeline>>> =
    std::sync::OnceLock::new();

fn get_mic_sessions() -> &'static RwLock<HashMap<u64, MicSession>> {
    MIC_SESSIONS.get_or_init(|| RwLock::new(HashMap::new()))
}

fn get_mic_handles() -> &'static SyncMutex<HashMap<u64, ActiveMicPipeline>> {
    MIC_HANDLES.get_or_init(|| SyncMutex::new(HashMap::new()))
}

#[derive(Debug, Clone)]
struct MicSession {
    session_id: String,
    session_token: String,
    ssrc: u32,
    rtp_port: u16,
    rtcp_port: u16,
    local_rtcp_port: u16,
    started_at: String,
    quality_profile: MicQualityProfile,
    client_config: MicClientConfig,
    reconnect_count: u64,
}

/// Microphone passthrough service.
///
/// Manages mic configuration, sessions, and VM agent communication
/// for native microphone passthrough to provisioned instances.

pub struct ActiveMicPipeline {
    client: MicClientHandle,
}

impl ActiveMicPipeline {
    pub fn stop(&mut self) {
        self.client.stop();
    }

    pub fn is_running(&mut self) -> bool {
        self.client.is_running()
    }
}

/// Microphone passthrough service.
///
/// Manages mic configuration, sessions, and VM agent communication
/// for native microphone passthrough to provisioned instances.
pub struct MicPassthroughService;

impl MicPassthroughService {
    /// List available recording devices on this machine.
    pub fn list_devices() -> AppResult<Vec<MicrophoneDevice>> {
        list_sidecar_devices()
    }

    /// Get mic configuration for an instance.
    pub async fn get_config(
        context: &AppContext,
        instance_id: u64,
    ) -> AppResult<InstanceMicConfig> {
        let state = context.load_state().await;

        // Find provisioned server
        let server = state
            .provisioned_servers
            .iter()
            .find(|s| s.instance_id == instance_id)
            .ok_or_else(|| AppError::NotFound(format!("Instance {} not found", instance_id)))?;

        // Build config from persisted state + defaults
        let mut config = InstanceMicConfig::default();
        config.instance_id = instance_id;
        config.vm_wireguard_ip = server.wireguard_server_ip.clone();
        let stored_device_id = normalize_device_id(&server.mic_device_id);
        let (device_id, device_name) =
            resolve_stored_device(&stored_device_id, server.mic_device_name.as_str());
        config.device_id = device_id;
        config.device_name = device_name;
        config.quality_profile = server.mic_quality_profile.clone();
        config.forwarding_enabled = server.mic_forwarding_enabled;
        config.auto_connect = server.mic_auto_connect;

        // If we have an active session, include it
        let sessions = get_mic_sessions().read().await;
        if let Some(session) = sessions.get(&instance_id) {
            config.enabled = true;
            config.session_id = Some(session.session_id.clone());
            config.session_token = Some(session.session_token.clone());
            config.ssrc = Some(session.ssrc);
            config.rtp_port = session.rtp_port;
            config.rtcp_port = session.rtcp_port;
            config.local_rtcp_port = session.local_rtcp_port;
            config.quality_profile = session.quality_profile.clone();
            config.last_enabled_at = Some(session.started_at.clone());
        }

        Ok(config)
    }

    /// Update mic settings.
    pub async fn update_settings(
        context: &AppContext,
        instance_id: u64,
        payload: MicSettingsUpdate,
    ) -> AppResult<InstanceMicConfig> {
        let current_config = Self::get_config(context, instance_id).await?;
        let requested_device_id = payload
            .device_id
            .as_deref()
            .map(normalize_device_id)
            .unwrap_or_else(|| current_config.device_id.clone());
        let (device_id, device_name) = resolve_selected_device(&requested_device_id)?;
        let quality_profile = payload
            .quality_profile
            .unwrap_or_else(|| current_config.quality_profile.clone());
        let forwarding_enabled = payload
            .forwarding_enabled
            .unwrap_or(current_config.forwarding_enabled);
        let auto_connect = payload.auto_connect.unwrap_or(current_config.auto_connect);

        let persisted_device_id = device_id.clone();
        let persisted_device_name = device_name.clone();
        let persisted_quality_profile = quality_profile.clone();
        context
            .update_state(move |state| {
                if let Some(server) = state
                    .provisioned_servers
                    .iter_mut()
                    .find(|server| server.instance_id == instance_id)
                {
                    server.mic_device_id = persisted_device_id.clone();
                    server.mic_device_name = persisted_device_name.clone();
                    server.mic_quality_profile = persisted_quality_profile.clone();
                    server.mic_forwarding_enabled = forwarding_enabled;
                    server.mic_auto_connect = auto_connect;
                }
            })
            .await?;

        let was_active = {
            let sessions = get_mic_sessions().read().await;
            sessions.contains_key(&instance_id)
        };

        if was_active {
            let active_device_id = (device_id != "default").then_some(device_id.clone());
            {
                let mut handles = get_mic_handles().lock();
                let handle = handles.get_mut(&instance_id).ok_or_else(|| {
                    AppError::Command(
                        "Microphone session exists without an active media sidecar".to_string(),
                    )
                })?;
                handle.client.select_device(active_device_id.as_deref())?;
                handle
                    .client
                    .set_bitrate(quality_profile.bitrate_kbps() * 1000)?;
            }
            if let Some(session) = get_mic_sessions().write().await.get_mut(&instance_id) {
                session.client_config.device_id = active_device_id;
                session.client_config.quality_profile = quality_profile.clone();
                session.quality_profile = quality_profile.clone();
            }
            info!(
                instance_id,
                device_id = %device_id,
                bitrate_kbps = quality_profile.bitrate_kbps(),
                "Applied microphone settings to active media sidecar"
            );
        }

        Self::get_config(context, instance_id).await
    }

    /// Enable microphone passthrough for an instance.
    pub async fn enable(
        context: &AppContext,
        instance_id: u64,
        requested_profile: Option<MicQualityProfile>,
    ) -> AppResult<MicSessionResponse> {
        let persisted_config = Self::get_config(context, instance_id).await?;
        let state = context.load_state().await;

        // Verify instance exists and is running
        let server = state
            .provisioned_servers
            .iter()
            .find(|s| s.instance_id == instance_id)
            .ok_or_else(|| AppError::NotFound(format!("Instance {} not found", instance_id)))?;

        if server.wireguard_server_ip.trim().is_empty() {
            return Err(AppError::Provisioning(
                "WireGuard not configured for this instance. Run provisioning first.".to_string(),
            ));
        }

        // Check if already enabled
        {
            let sessions = get_mic_sessions().read().await;
            if sessions.contains_key(&instance_id) {
                return Err(AppError::InvalidInput(
                    "Microphone passthrough is already enabled for this instance. Use reconnect to refresh the session.".to_string(),
                ));
            }
        }

        let requested_profile_supplied = requested_profile.is_some();
        let profile = requested_profile.unwrap_or_else(|| persisted_config.quality_profile.clone());
        if requested_profile_supplied && profile != server.mic_quality_profile {
            let device_id = server.mic_device_id.clone();
            let device_name = server.mic_device_name.clone();
            let profile_for_save = profile.clone();
            context
                .update_state(move |state| {
                    if let Some(server) = state
                        .provisioned_servers
                        .iter_mut()
                        .find(|server| server.instance_id == instance_id)
                    {
                        server.mic_device_id = device_id.clone();
                        server.mic_device_name = device_name.clone();
                        server.mic_quality_profile = profile_for_save.clone();
                    }
                })
                .await?;
        }

        mic_client::ensure_microphone_access()?;

        let selected_device_id = normalize_device_id(&persisted_config.device_id);
        let capture_device_id = if selected_device_id == "default" {
            None
        } else {
            Some(selected_device_id.clone())
        };
        info!(
            instance_id,
            persisted_device_id = %persisted_config.device_id,
            normalized_device_id = %selected_device_id,
            capture_device_id = ?capture_device_id,
            "enable() resolved microphone device"
        );

        let session_id = uuid::Uuid::new_v4().to_string();
        let session_token = generate_session_token();
        let media_seed = uuid::Uuid::new_v4().as_u128();
        let ssrc = media_seed as u32;
        let sequence_offset = (media_seed >> 32) as u16;
        let timestamp_offset = (media_seed >> 64) as u32;
        let local_rtcp_port = allocate_local_udp_port()?;
        let started_at = chrono::Local::now().to_rfc3339();

        // The authenticated SSH control plane creates the short-lived host
        // endpoint before local capture starts. A host failure remains isolated
        // from Moonlight/Sunshine and simply fails this microphone operation.
        let remote = build_remote_exec_for_instance(context, instance_id).await?;
        let target_user = context.config.audio_target_user.clone();
        let peer_ip = state.wireguard.client_ip.clone();
        let endpoint = match Self::call_vm_agent_start_session(
            &remote,
            &target_user,
            &server.wireguard_server_ip,
            &session_id,
            &peer_ip,
            ssrc,
            local_rtcp_port,
            &profile,
        )
        .await
        {
            Ok(endpoint) => endpoint,
            Err(initial_error) => {
                warn!(instance_id, %initial_error, "Host mic session start failed; reinstalling the host media service once");
                MicReceiverProvisioner::install(&remote, &target_user).await?;
                Self::call_vm_agent_start_session(
                    &remote,
                    &target_user,
                    &server.wireguard_server_ip,
                    &session_id,
                    &peer_ip,
                    ssrc,
                    local_rtcp_port,
                    &profile,
                )
                .await
                .map_err(|retry_error| {
                    AppError::Provisioning(format!(
                        "Host microphone session allocation failed after reinstall. initial_error: {initial_error}; retry_error: {retry_error}"
                    ))
                })?
            }
        };

        if let Err(error) = Self::activate_remote_microphone_source(&remote, &target_user).await {
            let _ = Self::call_vm_agent_stop_session(&remote, &target_user, &session_id).await;
            return Err(error);
        }

        let pipeline_config = MicClientConfig {
            device_id: capture_device_id,
            quality_profile: profile.clone(),
            session_id: session_id.clone(),
            ssrc,
            sequence_offset,
            timestamp_offset,
            remote_host: endpoint.host.clone(),
            rtp_port: endpoint.rtp_port,
            rtcp_port: endpoint.rtcp_port,
            local_rtcp_port,
        };
        let client = match mic_client::start_pipeline(pipeline_config.clone()) {
            Ok(client) => client,
            Err(error) => {
                let _ = Self::call_vm_agent_stop_session(&remote, &target_user, &session_id).await;
                return Err(AppError::Provisioning(format!(
                    "Failed to start local microphone sidecar: {error}"
                )));
            }
        };
        let handle = ActiveMicPipeline { client };
        {
            let mut handles = get_mic_handles().lock();
            if let Some(mut previous) = handles.insert(instance_id, handle) {
                previous.stop();
                warn!(instance_id, "Replaced an existing stale mic sidecar handle");
            }
        }

        let session = MicSession {
            session_id: session_id.clone(),
            session_token: session_token.clone(),
            ssrc,
            rtp_port: endpoint.rtp_port,
            rtcp_port: endpoint.rtcp_port,
            local_rtcp_port,
            started_at: started_at.clone(),
            quality_profile: profile.clone(),
            client_config: pipeline_config,
            reconnect_count: 0,
        };
        get_mic_sessions()
            .write()
            .await
            .insert(instance_id, session);
        context
            .update_state(move |state| {
                if let Some(server) = state
                    .provisioned_servers
                    .iter_mut()
                    .find(|server| server.instance_id == instance_id)
                {
                    server.mic_forwarding_enabled = true;
                }
            })
            .await?;

        info!(
            instance_id,
            session_id = %session_id,
            ssrc,
            remote_host = %endpoint.host,
            rtp_port = endpoint.rtp_port,
            rtcp_port = endpoint.rtcp_port,
            local_rtcp_port,
            "Microphone forwarding is streaming on the independent Noland media path"
        );

        Ok(MicSessionResponse {
            session_id,
            session_token,
            ssrc,
            vm_wireguard_ip: endpoint.host,
            rtp_port: endpoint.rtp_port,
            rtcp_port: endpoint.rtcp_port,
            local_rtcp_port,
            rtp_payload_type: endpoint.payload_type,
            sample_rate: endpoint.clock_rate,
            channels: endpoint.channels,
            frame_ms: endpoint.frame_ms,
            bitrate_kbps: profile.bitrate_kbps(),
        })
    }

    /// Disable microphone forwarding and persist the feature preference.
    pub async fn disable(context: &AppContext, instance_id: u64) -> AppResult<()> {
        Self::stop_runtime_session(context, instance_id, true).await
    }

    /// Stop a stream-owned media session without changing the user's saved
    /// forwarding preference or deleting the persistent PipeWire source.
    pub async fn stop_for_game_stream(context: &AppContext, instance_id: u64) -> AppResult<()> {
        Self::stop_runtime_session(context, instance_id, false).await
    }

    async fn stop_runtime_session(
        context: &AppContext,
        instance_id: u64,
        disable_preference: bool,
    ) -> AppResult<()> {
        let session = get_mic_sessions().write().await.remove(&instance_id);

        {
            let mut handles = get_mic_handles().lock();
            if let Some(mut handle) = handles.remove(&instance_id) {
                handle.stop();
                info!(instance_id, "Mic audio pipeline stopped");
            }
        }

        if disable_preference {
            context
                .update_state(move |state| {
                    if let Some(server) = state
                        .provisioned_servers
                        .iter_mut()
                        .find(|server| server.instance_id == instance_id)
                    {
                        server.mic_forwarding_enabled = false;
                    }
                })
                .await?;
        }

        if let Some(session) = session {
            // Close allocated host ports, but never remove the persistent
            // PipeWire source. Failure remains isolated from game streaming.
            if let Ok(remote) = build_remote_exec_for_instance(context, instance_id).await {
                let target_user = context.config.audio_target_user.clone();
                if let Err(error) =
                    Self::call_vm_agent_stop_session(&remote, &target_user, &session.session_id)
                        .await
                {
                    warn!(instance_id, %error, "Host microphone session stop failed");
                }
                if let Err(error) = Self::ensure_remote_audio_defaults(&remote, &target_user).await
                {
                    warn!(instance_id, %error, "Failed restoring remote audio defaults after microphone stop");
                }
            }
        }

        info!(
            instance_id,
            disable_preference, "Microphone runtime stopped"
        );
        Ok(())
    }

    /// Resolve the Noland instance represented by an embedded Moonlight host.
    pub async fn instance_id_for_game_stream(context: &AppContext, host_id: &str) -> Option<u64> {
        context
            .load_state()
            .await
            .provisioned_servers
            .iter()
            .find(|server| {
                server.embedded_moonlight_host_id == host_id
                    || format!("instance-{}", server.instance_id) == host_id
            })
            .map(|server| server.instance_id)
    }

    /// Start microphone forwarding when the user's persisted feature and
    /// auto-connect preferences allow it.
    pub async fn auto_start_for_game_stream(
        context: &AppContext,
        instance_id: u64,
    ) -> AppResult<bool> {
        let state = context.load_state().await;
        let server = state
            .provisioned_servers
            .iter()
            .find(|server| server.instance_id == instance_id)
            .ok_or_else(|| AppError::NotFound(format!("Instance {instance_id} not found")))?;
        let should_start = server.mic_forwarding_enabled && server.mic_auto_connect;
        let profile = server.mic_quality_profile.clone();
        drop(state);

        if !should_start || get_mic_sessions().read().await.contains_key(&instance_id) {
            return Ok(false);
        }
        Self::enable(context, instance_id, Some(profile)).await?;
        Ok(true)
    }

    /// Supervise active local sidecars independently of UI polling. A dead,
    /// hung, or failed sidecar is replaced with the same negotiated RTP session
    /// so the remote virtual microphone and host receiver remain untouched.
    pub async fn maintain_active_sessions() {
        let sessions = get_mic_sessions()
            .read()
            .await
            .iter()
            .map(|(instance_id, session)| {
                (
                    *instance_id,
                    session.session_id.clone(),
                    session.client_config.clone(),
                )
            })
            .collect::<Vec<_>>();

        for (instance_id, session_id, client_config) in sessions {
            let restart_reason = {
                let mut handles = get_mic_handles().lock();
                match handles.get_mut(&instance_id) {
                    None => Some("sidecar handle missing".to_string()),
                    Some(handle) => {
                        if !handle.is_running() {
                            Some("gstreamer pipeline exited".to_string())
                        } else {
                            None
                        }
                    }
                }
            };

            let Some(reason) = restart_reason else {
                continue;
            };
            warn!(instance_id, session_id = %session_id, %reason, "Restarting microphone media sidecar");

            if let Some(mut stale) = get_mic_handles().lock().remove(&instance_id) {
                stale.stop();
            }

            match mic_client::start_pipeline(client_config) {
                Ok(client) => {
                    let mut replacement = ActiveMicPipeline { client };
                    let session_still_active = get_mic_sessions()
                        .read()
                        .await
                        .get(&instance_id)
                        .is_some_and(|session| session.session_id == session_id);
                    if !session_still_active {
                        replacement.stop();
                        continue;
                    }
                    get_mic_handles().lock().insert(instance_id, replacement);
                    if let Some(session) = get_mic_sessions().write().await.get_mut(&instance_id) {
                        session.reconnect_count = session.reconnect_count.saturating_add(1);
                    }
                    info!(instance_id, session_id = %session_id, "Microphone media sidecar recovered");
                }
                Err(error) => {
                    warn!(instance_id, session_id = %session_id, %error, "Microphone sidecar restart failed; retrying on the next supervisor pass");
                }
            }
        }
    }

    /// Reconnect microphone (new session).
    pub async fn reconnect(
        context: &AppContext,
        instance_id: u64,
    ) -> AppResult<MicSessionResponse> {
        let current_config = Self::get_config(context, instance_id).await?;

        // Restart the runtime while preserving the persisted feature and
        // auto-connect preferences even if the new session cannot start.
        let _ = Self::stop_for_game_stream(context, instance_id).await;
        Self::enable(context, instance_id, Some(current_config.quality_profile)).await
    }

    /// Mute or unmute without tearing down capture, Opus, RTP, or the host device.
    pub fn set_muted(instance_id: u64, muted: bool) -> AppResult<()> {
        let mut handles = get_mic_handles().lock();
        let handle = handles.get_mut(&instance_id).ok_or_else(|| {
            AppError::InvalidInput(
                "Microphone forwarding is not active for this instance.".to_string(),
            )
        })?;
        handle.client.set_muted(muted)?;
        Ok(())
    }

    /// Return detailed local sidecar metrics without exposing GStreamer objects.
    pub fn get_local_metrics(instance_id: u64) -> AppResult<serde_json::Value> {
        let mut handles = get_mic_handles().lock();
        let handle = handles.get_mut(&instance_id).ok_or_else(|| {
            AppError::InvalidInput(
                "Microphone forwarding is not active for this instance.".to_string(),
            )
        })?;

        handle.client.metrics()
    }

    /// Recreate the Cloud Mic device on the VM.
    pub async fn recreate_device(context: &AppContext, instance_id: u64) -> AppResult<()> {
        let remote = build_remote_exec_for_instance(context, instance_id).await?;
        let target_user = context.config.audio_target_user.clone();

        Self::recreate_or_install_remote_device(&remote, &target_user, instance_id).await?;
        info!(
            instance_id = instance_id,
            "Cloud Mic device recreated on VM"
        );
        Ok(())
    }

    /// Get runtime status from VM agent.
    pub async fn get_status(
        context: &AppContext,
        instance_id: u64,
    ) -> AppResult<InstanceMicRuntimeStatus> {
        let mut status = InstanceMicRuntimeStatus::default();

        // Check if we have a local session. When the mic is disabled, avoid any
        // expensive remote SSH status probes entirely.
        {
            let sessions = get_mic_sessions().read().await;
            if let Some(session) = sessions.get(&instance_id) {
                status.enabled = true;
                status.bitrate_kbps = session.quality_profile.bitrate_kbps();
                status.frame_ms = session.quality_profile.frame_ms();
                status.reconnect_count = session.reconnect_count;
            } else {
                status.state = MicState::Disabled;
                return Ok(status);
            }
        }

        {
            let mut handles = get_mic_handles().lock();
            match handles.get_mut(&instance_id) {
                Some(handle) => {
                    if !handle.is_running() {
                        status.sidecar_healthy = false;
                        status.error =
                            Some("Noland microphone media sidecar is not running".to_string());
                    } else {
                        match handle.client.status() {
                            Ok(sidecar_status) => {
                                status.sidecar_healthy = sidecar_status
                                    .get("health")
                                    .and_then(serde_json::Value::as_str)
                                    == Some("healthy")
                                    && sidecar_status
                                        .get("sessionActive")
                                        .and_then(serde_json::Value::as_bool)
                                        .unwrap_or(false);
                                status.muted = sidecar_status
                                    .get("muted")
                                    .and_then(serde_json::Value::as_bool)
                                    .unwrap_or(false);
                                status.capture_sample_rate = sidecar_status
                                    .get("activeSampleRate")
                                    .and_then(serde_json::Value::as_u64)
                                    .unwrap_or(0)
                                    as u32;
                                status.error = sidecar_status
                                    .get("lastError")
                                    .and_then(serde_json::Value::as_str)
                                    .map(ToOwned::to_owned);
                            }
                            Err(error) => {
                                status.sidecar_healthy = false;
                                status.error = Some(error.to_string());
                            }
                        }
                        if let Ok(metrics) = handle.client.metrics() {
                            status.capture_overruns = metrics
                                .get("overruns")
                                .and_then(serde_json::Value::as_u64)
                                .unwrap_or(0);
                            let ring_samples = metrics
                                .get("ringDepthSamples")
                                .and_then(serde_json::Value::as_u64)
                                .unwrap_or(0);
                            if status.capture_sample_rate > 0 {
                                status.ring_fill_ms = ring_samples as f64 * 1000.0
                                    / f64::from(status.capture_sample_rate);
                            }
                            status.appsrc_queue_ms = metrics
                                .get("appsrcQueueMs")
                                .and_then(serde_json::Value::as_u64)
                                .unwrap_or(0)
                                as f64;
                            status.opus_packets_sent = metrics
                                .get("opusPacketsSent")
                                .and_then(serde_json::Value::as_u64)
                                .unwrap_or(0);
                            status.bytes_sent = metrics
                                .get("bytesSent")
                                .and_then(serde_json::Value::as_u64)
                                .unwrap_or(0);
                        }
                    }
                }
                _ => {
                    status.sidecar_healthy = false;
                    status.error =
                        Some("Noland microphone media sidecar is not running".to_string());
                }
            }
        }

        let state = context.load_state().await;
        let wireguard_config_path = state
            .provisioned_servers
            .iter()
            .find(|server| server.instance_id == instance_id)
            .map(|server| server.wireguard_config_path.clone())
            .filter(|path| !path.trim().is_empty())
            .unwrap_or_else(|| state.wireguard.config_path.clone());
        drop(state);
        if wireguard_config_path.trim().is_empty()
            || verify_managed_gotatun_tunnel(Path::new(&wireguard_config_path)).is_err()
        {
            status.state = MicState::WireguardDisconnected;
            status.error = Some(
                "The managed WireGuard path is unavailable; microphone RTP will resume when the tunnel returns."
                    .to_string(),
            );
            return Ok(status);
        }

        let remote = build_remote_exec_for_instance(context, instance_id).await?;
        let target_user = context.config.audio_target_user.clone();

        // Try to get VM agent status
        match Self::call_vm_agent_status(&remote, &target_user).await {
            Ok(agent_status) => {
                status.vm_agent_reachable = true;
                status.device_ready = agent_status.device_ready;
                status.receiving_audio = agent_status.receiving_audio;
                status.packet_loss_percent = agent_status.packet_loss_percent;
                status.jitter_ms = agent_status.jitter_ms;
                status.buffer_depth_ms = agent_status.buffer_depth_ms;
                status.last_packet_ms_ago = agent_status.last_packet_ms_ago;
                status.pipewire_connected = agent_status.pipewire_connected;
                status.default_source = agent_status.default_source;

                // Map to high-level state
                status.state = Self::map_runtime_state(&status);
            }
            Err(e) => {
                warn!("VM agent status check failed: {}", e);
                status.vm_agent_reachable = false;
                status.state = if status.enabled {
                    MicState::VmAgentUnreachable
                } else {
                    MicState::Disabled
                };
            }
        }

        Ok(status)
    }

    // ------------------------------------------------------------------
    // VM agent communication helpers
    // ------------------------------------------------------------------

    async fn call_vm_agent_start_session(
        remote: &RemoteExec,
        target_user: &str,
        vm_wg_ip: &str,
        session_id: &str,
        peer_ip: &str,
        ssrc: u32,
        client_rtcp_port: u16,
        _profile: &MicQualityProfile,
    ) -> AppResult<HostMicEndpoint> {
        validate_control_token("target user", target_user)?;
        validate_control_token("session id", session_id)?;
        vm_wg_ip.parse::<IpAddr>().map_err(|error| {
            AppError::InvalidInput(format!(
                "Invalid host WireGuard address '{vm_wg_ip}': {error}"
            ))
        })?;
        peer_ip.parse::<IpAddr>().map_err(|error| {
            AppError::InvalidInput(format!(
                "Invalid client WireGuard address '{peer_ip}': {error}"
            ))
        })?;
        let cmd = format!(
            "test \"$(cat /etc/noland/microphone-agent.version 2>/dev/null || true)\" = \"5\" || {{ echo 'Noland microphone agent upgrade required' >&2; exit 42; }}; sudo /usr/local/sbin/noland-mic-session-control start --user {target_user} --session-id {session_id} --peer-ip {peer_ip} --bind-address {vm_wg_ip} --interface wg0 --ssrc {ssrc} --client-rtcp-port {client_rtcp_port} --jitter-ms 20"
        );
        let output = {
            let remote = remote.clone();
            tokio::task::spawn_blocking(move || remote.ssh(&cmd, Duration::from_secs(30)))
                .await
                .map_err(|error| AppError::Command(format!("join failure: {error}")))??
        };
        if output.status_code != 0 {
            return Err(AppError::Provisioning(format!(
                "Host mic session allocation failed: {} {}",
                output.stderr.trim(),
                output.stdout.trim()
            )));
        }
        let endpoint: HostMicEndpoint =
            serde_json::from_str(output.stdout.trim()).map_err(|error| {
                AppError::Serialization(format!(
                    "Failed parsing host mic endpoint: {error}. Raw: {}",
                    output.stdout.trim()
                ))
            })?;
        if endpoint.session_id != session_id
            || endpoint.payload_type != 111
            || endpoint.clock_rate != 48_000
            || endpoint.channels != 1
            || endpoint.frame_ms != 10
            || endpoint.rtcp_mux
            || !(10..=60).contains(&endpoint.jitter_ms)
        {
            return Err(AppError::Provisioning(
                "Host returned an incompatible microphone media endpoint".to_string(),
            ));
        }
        Ok(endpoint)
    }

    async fn activate_remote_microphone_source(
        remote: &RemoteExec,
        target_user: &str,
    ) -> AppResult<()> {
        let cmd = remote_user_bus_command(
            target_user,
            "if [[ ! -S \"$bus_path\" ]]; then echo \"user systemd bus unavailable\" >&2; exit 1; fi; current_sink=\"$(run_user pactl get-default-sink 2>/dev/null || true)\"; if [[ -z \"$current_sink\" || \"$current_sink\" == noland_mic_* ]]; then fallback_sink=\"\"; while read -r _ sink _; do if [[ -n \"$sink\" && \"$sink\" != noland_mic_* ]]; then fallback_sink=\"$sink\"; break; fi; done < <(run_user pactl list short sinks 2>/dev/null); if [[ -z \"$fallback_sink\" ]]; then echo \"no safe non-Noland audio sink is available\" >&2; exit 1; fi; run_user pactl set-default-sink \"$fallback_sink\"; fi; run_user pactl list short sources 2>/dev/null | grep -Eq \"(^|[[:space:]])noland_mic_source([[:space:]]|$)\" || { echo \"Noland microphone source is unavailable\" >&2; exit 1; }; run_user pactl set-default-source noland_mic_source; while read -r source_output _; do if [[ -n \"$source_output\" ]]; then run_user pactl move-source-output \"$source_output\" noland_mic_source >/dev/null 2>&1 || true; fi; done < <(run_user pactl list short source-outputs 2>/dev/null); final_source=\"$(run_user pactl get-default-source 2>/dev/null || true)\"; if [[ \"$final_source\" != \"noland_mic_source\" ]]; then echo \"failed to activate Noland microphone source\" >&2; exit 1; fi",
        )?;
        let output = {
            let remote = remote.clone();
            tokio::task::spawn_blocking(move || remote.ssh(&cmd, Duration::from_secs(15)))
                .await
                .map_err(|error| AppError::Command(format!("join failure: {error}")))??
        };
        if output.status_code != 0 {
            return Err(AppError::Provisioning(format!(
                "Failed activating the remote Noland microphone source: {} {}",
                output.stderr.trim(),
                output.stdout.trim()
            )));
        }
        Ok(())
    }

    async fn ensure_remote_audio_defaults(remote: &RemoteExec, target_user: &str) -> AppResult<()> {
        let cmd = remote_user_bus_command(
            target_user,
            "if [[ ! -S \"$bus_path\" ]]; then echo \"user systemd bus unavailable\" >&2; exit 1; fi; current_sink=\"$(run_user pactl get-default-sink 2>/dev/null || true)\"; if [[ -z \"$current_sink\" || \"$current_sink\" == noland_mic_* ]]; then fallback_sink=\"\"; while read -r _ sink _; do if [[ -n \"$sink\" && \"$sink\" != noland_mic_* ]]; then fallback_sink=\"$sink\"; break; fi; done < <(run_user pactl list short sinks 2>/dev/null); if [[ -z \"$fallback_sink\" ]]; then echo \"no safe non-Noland audio sink is available\" >&2; exit 1; fi; run_user pactl set-default-sink \"$fallback_sink\"; current_sink=\"$fallback_sink\"; fi; current_source=\"$(run_user pactl get-default-source 2>/dev/null || true)\"; if [[ -z \"$current_source\" || \"$current_source\" == noland_mic_* ]]; then preferred_source=\"${current_sink}.monitor\"; if run_user pactl list short sources 2>/dev/null | grep -Eq \"(^|[[:space:]])${preferred_source}([[:space:]]|$)\"; then safe_source=\"$preferred_source\"; else safe_source=\"\"; while read -r _ source _; do if [[ -n \"$source\" && \"$source\" != noland_mic_* ]]; then safe_source=\"$source\"; break; fi; done < <(run_user pactl list short sources 2>/dev/null); fi; if [[ -z \"$safe_source\" ]]; then echo \"no safe non-Noland audio source is available\" >&2; exit 1; fi; run_user pactl set-default-source \"$safe_source\"; while read -r source_output _; do if [[ -n \"$source_output\" ]]; then run_user pactl move-source-output \"$source_output\" \"$safe_source\" >/dev/null 2>&1 || true; fi; done < <(run_user pactl list short source-outputs 2>/dev/null); fi; final_sink=\"$(run_user pactl get-default-sink 2>/dev/null || true)\"; final_source=\"$(run_user pactl get-default-source 2>/dev/null || true)\"; if [[ -z \"$final_sink\" || \"$final_sink\" == noland_mic_* || -z \"$final_source\" || \"$final_source\" == noland_mic_* ]]; then echo \"Noland microphone nodes became desktop defaults\" >&2; exit 1; fi",
        )?;
        let output = {
            let remote = remote.clone();
            tokio::task::spawn_blocking(move || remote.ssh(&cmd, Duration::from_secs(15)))
                .await
                .map_err(|error| AppError::Command(format!("join failure: {error}")))??
        };
        if output.status_code != 0 {
            return Err(AppError::Provisioning(format!(
                "Failed restoring safe remote audio defaults: {} {}",
                output.stderr.trim(),
                output.stdout.trim()
            )));
        }
        Ok(())
    }

    async fn call_vm_agent_stop_session(
        remote: &RemoteExec,
        target_user: &str,
        session_id: &str,
    ) -> AppResult<()> {
        validate_control_token("target user", target_user)?;
        validate_control_token("session id", session_id)?;
        let cmd = format!(
            "sudo /usr/local/sbin/noland-mic-session-control stop --user {target_user} --session-id {session_id}"
        );
        let output = {
            let remote = remote.clone();
            tokio::task::spawn_blocking(move || remote.ssh(&cmd, Duration::from_secs(15)))
                .await
                .map_err(|error| AppError::Command(format!("join failure: {error}")))??
        };
        if output.status_code != 0 {
            return Err(AppError::Provisioning(format!(
                "Host mic session stop failed: {} {}",
                output.stderr.trim(),
                output.stdout.trim()
            )));
        }
        Ok(())
    }

    async fn call_vm_agent_recreate_device(
        remote: &RemoteExec,
        target_user: &str,
    ) -> AppResult<()> {
        let cmd = remote_user_bus_command(
            target_user,
            "if [[ ! -S \"$bus_path\" ]]; then echo \"user systemd bus unavailable\"; exit 1; fi; receiver_was_active=false; if run_user systemctl --user is-active --quiet noland-mic-receiver.service; then receiver_was_active=true; fi; run_user systemctl --user restart pipewire.service pipewire-pulse.service wireplumber.service; for _ in 1 2 3 4 5; do if run_user pactl list short sources 2>/dev/null | grep -Eq \"(^|[[:space:]])noland_mic_source([[:space:]]|$)\"; then break; fi; sleep 1; done; run_user pactl list short sources 2>/dev/null | grep -Eq \"(^|[[:space:]])noland_mic_source([[:space:]]|$)\"; if [[ \"$receiver_was_active\" = true ]]; then run_user systemctl --user restart noland-mic-receiver.service; fi",
        )?;

        let output = {
            let r = remote.clone();
            tokio::task::spawn_blocking(move || r.ssh(&cmd, Duration::from_secs(30)))
                .await
                .map_err(|e| AppError::Command(format!("join failure: {e}")))??
        };

        if output.status_code != 0 {
            return Err(AppError::Provisioning(format!(
                "Remote mic device/recreate failed: {} {}",
                output.stderr.trim(),
                output.stdout.trim()
            )));
        }

        Ok(())
    }

    async fn recreate_or_install_remote_device(
        remote: &RemoteExec,
        target_user: &str,
        instance_id: u64,
    ) -> AppResult<()> {
        match Self::call_vm_agent_recreate_device(remote, target_user).await {
            Ok(()) => Ok(()),
            Err(recreate_error) => {
                warn!(
                    instance_id = instance_id,
                    "Remote mic recreate failed; attempting install/build fallback: {}",
                    recreate_error
                );

                MicReceiverProvisioner::install(remote, target_user).await?;
                Self::call_vm_agent_recreate_device(remote, target_user)
                    .await
                    .map_err(|retry_error| {
                        AppError::Provisioning(format!(
                            "Remote mic recreate failed after install/build fallback. initial_error: {}; retry_error: {}",
                            recreate_error, retry_error
                        ))
                    })
            }
        }
    }

    async fn call_vm_agent_status(
        remote: &RemoteExec,
        target_user: &str,
    ) -> AppResult<VmAgentStatus> {
        let cmd = remote_user_bus_command(
            target_user,
            "status_file=/run/noland/noland_remote_microphone.status.json; bus_ready=false; if [[ -S \"$bus_path\" ]]; then bus_ready=true; fi; pipewire_connected=false; if [[ \"$bus_ready\" = true ]] && run_user systemctl --user is-active --quiet pipewire.service && run_user systemctl --user is-active --quiet wireplumber.service; then pipewire_connected=true; fi; source_present=false; if [[ \"$bus_ready\" = true ]] && run_user pactl list short sources 2>/dev/null | grep -Eq \"(^|[[:space:]])noland_mic_source([[:space:]]|$)\"; then source_present=true; fi; default_source=false; if [[ \"$bus_ready\" = true ]] && [[ \"$(run_user pactl get-default-source 2>/dev/null || true)\" = \"noland_mic_source\" ]]; then default_source=true; fi; if [[ -f \"$status_file\" ]]; then status_json=$(cat \"$status_file\"); else status_json=\"{}\"; fi; DEVICE_READY=\"$source_present\" PIPEWIRE_CONNECTED=\"$pipewire_connected\" DEFAULT_SOURCE=\"$default_source\" STATUS_JSON=\"$status_json\" python3 -c \"import json,os; raw=os.environ.get(\\\"STATUS_JSON\\\",\\\"{}\\\"); status=json.loads(raw) if raw.strip() else {}; out={\\\"deviceReady\\\":os.environ.get(\\\"DEVICE_READY\\\",\\\"\\\").lower()==\\\"true\\\",\\\"receivingAudio\\\":bool(status.get(\\\"receivingAudio\\\",False)),\\\"packetLossPercent\\\":float(status.get(\\\"packetLossPercent\\\",0.0) or 0.0),\\\"jitterMs\\\":float(status.get(\\\"jitterMs\\\",0.0) or 0.0),\\\"bufferDepthMs\\\":float(status.get(\\\"bufferDepthMs\\\",0.0) or 0.0),\\\"lastPacketMsAgo\\\":status.get(\\\"lastPacketMsAgo\\\"),\\\"pipewireConnected\\\":os.environ.get(\\\"PIPEWIRE_CONNECTED\\\",\\\"\\\").lower()==\\\"true\\\",\\\"defaultSource\\\":os.environ.get(\\\"DEFAULT_SOURCE\\\",\\\"\\\").lower()==\\\"true\\\"}; print(json.dumps(out,separators=(\\\",\\\",\\\":\\\")))\"",
        )?;

        let output = {
            let r = remote.clone();
            tokio::task::spawn_blocking(move || r.ssh(&cmd, Duration::from_secs(15)))
                .await
                .map_err(|e| AppError::Command(format!("join failure: {e}")))??
        };

        if output.status_code != 0 {
            return Err(AppError::Provisioning(format!(
                "Remote mic status check failed: {} {}",
                output.stderr.trim(),
                output.stdout.trim()
            )));
        }

        let status: VmAgentStatus = serde_json::from_str(&output.stdout).map_err(|e| {
            AppError::Serialization(format!(
                "Failed to parse remote mic status: {e}. Raw: {}",
                &output.stdout[..output.stdout.len().min(200)]
            ))
        })?;

        Ok(status)
    }

    // ------------------------------------------------------------------
    // State mapping
    // ------------------------------------------------------------------

    fn map_runtime_state(status: &InstanceMicRuntimeStatus) -> MicState {
        if !status.enabled {
            return MicState::Disabled;
        }

        if let Some(error) = status.error.as_deref() {
            let normalized = error.to_ascii_lowercase();
            if normalized.contains("no cpal input")
                || normalized.contains("no input device")
                || normalized.contains("no microphone")
            {
                return MicState::NoMicrophone;
            }
        }

        if !status.sidecar_healthy {
            return if status.error.as_deref().is_some_and(|error| {
                let error = error.to_ascii_lowercase();
                ["capture", "microphone", "device", "zero samples"]
                    .iter()
                    .any(|needle| error.contains(needle))
            }) {
                MicState::CaptureFailure
            } else {
                MicState::PipelineFailure
            };
        }

        if !status.vm_agent_reachable {
            return MicState::VmAgentUnreachable;
        }

        if !status.pipewire_connected {
            return MicState::PipewireUnavailable;
        }

        if !status.device_ready {
            return MicState::CloudMicMissing;
        }

        if status.error.is_some() && !status.receiving_audio {
            return MicState::Reconnecting;
        }

        if !status.receiving_audio && status.opus_packets_sent >= 300 {
            return if status.last_packet_ms_ago.is_some() {
                MicState::Reconnecting
            } else {
                MicState::NetworkFailure
            };
        }

        if status.receiving_audio {
            if status.packet_loss_percent > 10.0 {
                return MicState::PacketLossHigh;
            }
            return MicState::Streaming;
        }

        if status.enabled {
            return MicState::NoAudioDetected;
        }

        MicState::Error
    }
}

#[derive(Debug, Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct HostMicEndpoint {
    session_id: String,
    host: String,
    rtp_port: u16,
    rtcp_port: u16,
    payload_type: u8,
    clock_rate: u32,
    channels: u32,
    frame_ms: u32,
    jitter_ms: u32,
    rtcp_mux: bool,
}

/// Parsed VM agent status response.
#[derive(Debug, Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct VmAgentStatus {
    device_ready: bool,
    receiving_audio: bool,
    packet_loss_percent: f64,
    jitter_ms: f64,
    buffer_depth_ms: f64,
    last_packet_ms_ago: Option<u64>,
    pipewire_connected: bool,
    default_source: bool,
}

// ------------------------------------------------------------------
// Helpers
// ------------------------------------------------------------------

fn allocate_local_udp_port() -> AppResult<u16> {
    let socket = UdpSocket::bind(("0.0.0.0", 0)).map_err(|error| {
        AppError::Command(format!("Failed allocating local RTCP UDP port: {error}"))
    })?;
    socket
        .local_addr()
        .map(|address| address.port())
        .map_err(|error| AppError::Command(format!("Failed reading local RTCP port: {error}")))
}

fn validate_control_token(label: &str, value: &str) -> AppResult<()> {
    if !value.is_empty()
        && value
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || "._:-".contains(character))
    {
        Ok(())
    } else {
        Err(AppError::InvalidInput(format!(
            "Invalid microphone control {label} '{value}'."
        )))
    }
}

fn generate_session_token() -> String {
    use base64::Engine;
    let u1 = uuid::Uuid::new_v4();
    let u2 = uuid::Uuid::new_v4();
    let mut bytes = Vec::with_capacity(32);
    bytes.extend_from_slice(u1.as_bytes());
    bytes.extend_from_slice(u2.as_bytes());
    base64::engine::general_purpose::STANDARD.encode(&bytes)
}

fn normalize_device_id(device_id: &str) -> String {
    let trimmed = device_id.trim();
    if trimmed.is_empty() || trimmed.eq_ignore_ascii_case("default") {
        "default".to_string()
    } else {
        trimmed.to_string()
    }
}

fn resolved_device_name(device_id: &str, fallback_name: &str) -> String {
    if device_id == "default" {
        return "System Default".to_string();
    }

    let trimmed = fallback_name.trim();
    if trimmed.is_empty() {
        device_id.to_string()
    } else {
        trimmed.to_string()
    }
}

fn resolve_stored_device(device_id: &str, fallback_name: &str) -> (String, String) {
    if device_id == "default" {
        return ("default".to_string(), "System Default".to_string());
    }

    match MicPassthroughService::list_devices() {
        Ok(devices) => {
            let fallback_name = fallback_name.trim();
            if let Some(device) = devices.into_iter().find(|device| {
                device.id == device_id
                    || device.name == device_id
                    || (!fallback_name.is_empty() && device.name == fallback_name)
            }) {
                return (device.id, device.name);
            }
        }
        Err(error) => {
            warn!(%error, "Could not canonicalize the persisted microphone device");
        }
    }

    (
        device_id.to_string(),
        resolved_device_name(device_id, fallback_name),
    )
}

fn resolve_selected_device(device_id: &str) -> AppResult<(String, String)> {
    if device_id == "default" {
        return Ok(("default".to_string(), "System Default".to_string()));
    }

    let devices = MicPassthroughService::list_devices()?;
    devices
        .into_iter()
        .find(|device| device.id == device_id || device.name == device_id)
        .map(|device| (device.id, device.name))
        .ok_or_else(|| {
            AppError::InvalidInput(format!(
                "Selected microphone device '{device_id}' is no longer available"
            ))
        })
}

async fn build_remote_exec_for_instance(
    context: &AppContext,
    instance_id: u64,
) -> AppResult<RemoteExec> {
    let state = context.load_state().await;

    let server = state
        .provisioned_servers
        .iter()
        .find(|s| s.instance_id == instance_id)
        .ok_or_else(|| AppError::NotFound(format!("Instance {} not found", instance_id)))?;

    let private_key_path = state.ssh.private_key_path.clone();
    if private_key_path.trim().is_empty() {
        return Err(AppError::InvalidInput(
            "SSH private key path is empty. Run provisioning first.".to_string(),
        ));
    }

    let ssh_user = if state.ssh.ssh_username.trim().is_empty() {
        context.config.audio_target_user.clone()
    } else {
        state.ssh.ssh_username.clone()
    };

    if server.ssh_host.trim().is_empty() || server.ssh_port == 0 {
        return Err(AppError::InvalidInput(
            "Instance SSH details are not available.".to_string(),
        ));
    }

    Ok(RemoteExec {
        ssh_user,
        ssh_host: server.ssh_host.clone(),
        ssh_port: server.ssh_port,
        private_key_path,
    })
}

fn remote_user_bus_command(target_user: &str, body: &str) -> AppResult<String> {
    if !target_user
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    {
        return Err(AppError::InvalidInput(format!(
            "Invalid audio target user '{}'.",
            target_user
        )));
    }

    Ok(format!(
        "sudo bash -lc 'set -euo pipefail; USER_NAME=\"{}\"; uid=$(id -u \"$USER_NAME\"); runtime_dir=\"/run/user/$uid\"; bus_path=\"$runtime_dir/bus\"; run_user() {{ sudo -u \"$USER_NAME\" env XDG_RUNTIME_DIR=\"$runtime_dir\" DBUS_SESSION_BUS_ADDRESS=\"unix:path=$bus_path\" \"$@\"; }}; {}'",
        target_user, body
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::app_state::MicQualityProfile;

    #[test]
    fn test_quality_profile_bitrate() {
        assert_eq!(MicQualityProfile::Standard.bitrate_kbps(), 32);
        assert_eq!(MicQualityProfile::LowLatency.bitrate_kbps(), 48);
        assert_eq!(MicQualityProfile::HighQuality.bitrate_kbps(), 64);
    }

    #[test]
    fn test_quality_profile_frame_ms() {
        assert_eq!(MicQualityProfile::Standard.frame_ms(), 10);
        assert_eq!(MicQualityProfile::LowLatency.frame_ms(), 10);
        assert_eq!(MicQualityProfile::HighQuality.frame_ms(), 20);
    }

    #[test]
    fn test_map_runtime_state_disabled() {
        let status = InstanceMicRuntimeStatus {
            enabled: false,
            ..Default::default()
        };
        assert_eq!(
            MicPassthroughService::map_runtime_state(&status),
            MicState::Disabled
        );
    }

    #[test]
    fn test_map_runtime_state_vm_unreachable() {
        let status = InstanceMicRuntimeStatus {
            enabled: true,
            sidecar_healthy: true,
            vm_agent_reachable: false,
            ..Default::default()
        };
        assert_eq!(
            MicPassthroughService::map_runtime_state(&status),
            MicState::VmAgentUnreachable
        );
    }

    #[test]
    fn test_map_runtime_state_streaming() {
        let status = InstanceMicRuntimeStatus {
            enabled: true,
            sidecar_healthy: true,
            vm_agent_reachable: true,
            pipewire_connected: true,
            device_ready: true,
            receiving_audio: true,
            ..Default::default()
        };
        assert_eq!(
            MicPassthroughService::map_runtime_state(&status),
            MicState::Streaming
        );
    }

    #[test]
    fn test_map_runtime_state_packet_loss_high() {
        let status = InstanceMicRuntimeStatus {
            enabled: true,
            sidecar_healthy: true,
            vm_agent_reachable: true,
            pipewire_connected: true,
            device_ready: true,
            receiving_audio: true,
            packet_loss_percent: 15.0,
            ..Default::default()
        };
        assert_eq!(
            MicPassthroughService::map_runtime_state(&status),
            MicState::PacketLossHigh
        );
    }

    #[test]
    fn test_map_runtime_state_no_audio() {
        let status = InstanceMicRuntimeStatus {
            enabled: true,
            sidecar_healthy: true,
            vm_agent_reachable: true,
            pipewire_connected: true,
            device_ready: true,
            receiving_audio: false,
            ..Default::default()
        };
        assert_eq!(
            MicPassthroughService::map_runtime_state(&status),
            MicState::NoAudioDetected
        );
    }

    #[test]
    fn test_session_token_format() {
        let token = generate_session_token();
        assert!(!token.is_empty());
        assert!(token.len() >= 32);
    }
}
