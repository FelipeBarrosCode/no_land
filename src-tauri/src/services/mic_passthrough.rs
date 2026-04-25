use std::{collections::HashMap, net::SocketAddr, time::Duration};

use tokio::sync::RwLock;
use tracing::{error, info, warn};

use crate::errors::{AppError, AppResult};
use crate::models::app_state::{
    InstanceMicConfig, InstanceMicRuntimeStatus, MicQualityProfile, MicSessionResponse, MicState,
    ProvisionedServerState,
};

use super::{app_context::AppContext, remote_exec::RemoteExec};

/// In-memory mic session tracking per instance.
static MIC_SESSIONS: std::sync::OnceLock<RwLock<HashMap<u64, MicSession>>> =
    std::sync::OnceLock::new();

fn get_mic_sessions() -> &'static RwLock<HashMap<u64, MicSession>> {
    MIC_SESSIONS.get_or_init(|| RwLock::new(HashMap::new()))
}

#[derive(Debug, Clone)]
struct MicSession {
    session_id: String,
    session_token: String,
    ssrc: u32,
    started_at: String,
    quality_profile: MicQualityProfile,
}

/// Microphone passthrough service.
///
/// Manages mic configuration, sessions, and VM agent communication
/// for native microphone passthrough to provisioned instances.
pub struct MicPassthroughService;

impl MicPassthroughService {
    /// Get mic configuration for an instance.
    pub async fn get_config(context: &AppContext, instance_id: u64) -> AppResult<InstanceMicConfig> {
        let state = context.load_state().await;

        // Find provisioned server
        let server = state
            .provisioned_servers
            .iter()
            .find(|s| s.instance_id == instance_id)
            .ok_or_else(|| AppError::NotFound(format!("Instance {} not found", instance_id)))?;

        // Build config from state + defaults
        let mut config = InstanceMicConfig::default();
        config.instance_id = instance_id;
        config.vm_wireguard_ip = server.wireguard_server_ip.clone();

        // If we have an active session, include it
        let sessions = get_mic_sessions().read().await;
        if let Some(session) = sessions.get(&instance_id) {
            config.enabled = true;
            config.session_id = Some(session.session_id.clone());
            config.session_token = Some(session.session_token.clone());
            config.ssrc = Some(session.ssrc);
            config.quality_profile = session.quality_profile.clone();
            config.last_enabled_at = Some(session.started_at.clone());
        }

        Ok(config)
    }

    /// Update mic settings.
    pub async fn update_settings(
        context: &AppContext,
        instance_id: u64,
        quality_profile: Option<MicQualityProfile>,
    ) -> AppResult<InstanceMicConfig> {
        let mut config = Self::get_config(context, instance_id).await?;

        if let Some(profile) = quality_profile {
            config.quality_profile = profile;
        }

        // If currently enabled, update the running session
        let sessions = get_mic_sessions().read().await;
        if sessions.contains_key(&instance_id) {
            drop(sessions);
            info!(
                instance_id = instance_id,
                "Mic settings updated while streaming; will apply on next session"
            );
        }

        Ok(config)
    }

    /// Enable microphone passthrough for an instance.
    pub async fn enable(
        context: &AppContext,
        instance_id: u64,
        requested_profile: Option<MicQualityProfile>,
    ) -> AppResult<MicSessionResponse> {
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

        let profile = requested_profile.unwrap_or(MicQualityProfile::Standard);
        let session_id = uuid::Uuid::new_v4().to_string();
        let session_token = generate_session_token();
        let ssrc: u32 = (uuid::Uuid::new_v4().as_u128() & 0xFFFFFFFF) as u32;
        let rtp_port = 34778u16;
        let started_at = chrono::Local::now().to_rfc3339();

        let session = MicSession {
            session_id: session_id.clone(),
            session_token: session_token.clone(),
            ssrc,
            started_at: started_at.clone(),
            quality_profile: profile.clone(),
        };

        // Store session
        {
            let mut sessions = get_mic_sessions().write().await;
            sessions.insert(instance_id, session);
        }

        // Try to notify VM agent (best effort for MVP)
        if let Ok(remote) = build_remote_exec_for_instance(context, instance_id).await {
            let target_user = context.config.audio_target_user.clone();
            let peer_ip = state.wireguard.client_ip.clone();
            let start_result = Self::call_vm_agent_start_session(
                &remote,
                &target_user,
                &server.wireguard_server_ip,
                &session_id,
                &session_token,
                &peer_ip,
                ssrc,
                rtp_port,
                &profile,
            )
            .await;

            if let Err(e) = start_result {
                warn!("VM agent session start failed (non-fatal for MVP): {}", e);
            }
        }

        info!(
            instance_id = instance_id,
            session_id = %session_id,
            ssrc = ssrc,
            "Microphone passthrough enabled"
        );

        Ok(MicSessionResponse {
            session_id,
            session_token,
            ssrc,
            vm_wireguard_ip: server.wireguard_server_ip.clone(),
            rtp_port,
            sample_rate: 48000,
            channels: 1,
            frame_ms: profile.frame_ms(),
            bitrate_kbps: profile.bitrate_kbps(),
        })
    }

    /// Disable microphone passthrough.
    pub async fn disable(context: &AppContext, instance_id: u64) -> AppResult<()> {
        // Remove local session
        let session = {
            let mut sessions = get_mic_sessions().write().await;
            sessions.remove(&instance_id)
        };

        if session.is_none() {
            return Err(AppError::InvalidInput(
                "Microphone passthrough is not enabled for this instance.".to_string(),
            ));
        }

        // Try to notify VM agent
        if let Ok(remote) = build_remote_exec_for_instance(context, instance_id).await {
            let target_user = context.config.audio_target_user.clone();
            let stop_result = Self::call_vm_agent_stop_session(&remote, &target_user).await;
            if let Err(e) = stop_result {
                warn!("VM agent session stop failed (non-fatal): {}", e);
            }
        }

        info!(instance_id = instance_id, "Microphone passthrough disabled");
        Ok(())
    }

    /// Reconnect microphone (new session).
    pub async fn reconnect(
        context: &AppContext,
        instance_id: u64,
    ) -> AppResult<MicSessionResponse> {
        // Get current profile before removing
        let current_profile = {
            let sessions = get_mic_sessions().read().await;
            sessions.get(&instance_id).map(|s| s.quality_profile.clone())
        };

        // Disable then enable
        let _ = Self::disable(context, instance_id).await;
        Self::enable(context, instance_id, current_profile).await
    }

    /// Recreate the Cloud Mic device on the VM.
    pub async fn recreate_device(context: &AppContext, instance_id: u64) -> AppResult<()> {
        let remote = build_remote_exec_for_instance(context, instance_id).await?;
        let target_user = context.config.audio_target_user.clone();

        Self::call_vm_agent_recreate_device(&remote, &target_user).await?;
        info!(instance_id = instance_id, "Cloud Mic device recreated on VM");
        Ok(())
    }

    /// Get runtime status from VM agent.
    pub async fn get_status(
        context: &AppContext,
        instance_id: u64,
    ) -> AppResult<InstanceMicRuntimeStatus> {
        let remote = build_remote_exec_for_instance(context, instance_id).await?;
        let target_user = context.config.audio_target_user.clone();

        let mut status = InstanceMicRuntimeStatus::default();

        // Check if we have a local session
        {
            let sessions = get_mic_sessions().read().await;
            if let Some(session) = sessions.get(&instance_id) {
                status.enabled = true;
                status.bitrate_kbps = session.quality_profile.bitrate_kbps();
                status.frame_ms = session.quality_profile.frame_ms();
            }
        }

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
        session_token: &str,
        peer_ip: &str,
        ssrc: u32,
        rtp_port: u16,
        profile: &MicQualityProfile,
    ) -> AppResult<()> {
        let body = serde_json::json!({
            "sessionId": session_id,
            "sessionToken": session_token,
            "expectedPeerIp": peer_ip,
            "ssrc": ssrc,
            "rtpPort": rtp_port,
            "codec": "opus",
            "sampleRate": 48000,
            "channels": 1,
            "frameMs": profile.frame_ms(),
            "bitrateKbps": profile.bitrate_kbps(),
        });

        let cmd = format!(
            "sudo -u {user} curl -sf -X POST http://{ip}:34779/session/start \
             -H 'Content-Type: application/json' \
             -d '{body}' 2>&1",
            user = target_user,
            ip = vm_wg_ip,
            body = shell_escape(&body.to_string()),
        );

        let output = {
            let r = remote.clone();
            tokio::task::spawn_blocking(move || r.ssh(&cmd, Duration::from_secs(30)))
                .await
                .map_err(|e| AppError::Command(format!("join failure: {e}")))??
        };

        if output.status_code != 0 {
            return Err(AppError::Provisioning(format!(
                "VM agent session/start failed: {}",
                output.stderr.trim()
            )));
        }

        Ok(())
    }

    async fn call_vm_agent_stop_session(
        remote: &RemoteExec,
        target_user: &str,
    ) -> AppResult<()> {
        let cmd = format!(
            "sudo -u {user} curl -sf -X POST http://localhost:34779/session/stop 2>&1",
            user = target_user,
        );

        let output = {
            let r = remote.clone();
            tokio::task::spawn_blocking(move || r.ssh(&cmd, Duration::from_secs(15)))
                .await
                .map_err(|e| AppError::Command(format!("join failure: {e}")))??
        };

        if output.status_code != 0 {
            return Err(AppError::Provisioning(format!(
                "VM agent session/stop failed: {}",
                output.stderr.trim()
            )));
        }

        Ok(())
    }

    async fn call_vm_agent_recreate_device(
        remote: &RemoteExec,
        target_user: &str,
    ) -> AppResult<()> {
        let cmd = format!(
            "sudo -u {user} curl -sf -X POST http://localhost:34779/device/recreate 2>&1",
            user = target_user,
        );

        let output = {
            let r = remote.clone();
            tokio::task::spawn_blocking(move || r.ssh(&cmd, Duration::from_secs(30)))
                .await
                .map_err(|e| AppError::Command(format!("join failure: {e}")))??
        };

        if output.status_code != 0 {
            return Err(AppError::Provisioning(format!(
                "VM agent device/recreate failed: {}",
                output.stderr.trim()
            )));
        }

        Ok(())
    }

    async fn call_vm_agent_status(
        remote: &RemoteExec,
        target_user: &str,
    ) -> AppResult<VmAgentStatus> {
        let cmd = format!(
            "sudo -u {user} curl -sf http://localhost:34779/status 2>&1",
            user = target_user,
        );

        let output = {
            let r = remote.clone();
            tokio::task::spawn_blocking(move || r.ssh(&cmd, Duration::from_secs(15)))
                .await
                .map_err(|e| AppError::Command(format!("join failure: {e}")))??
        };

        if output.status_code != 0 {
            return Err(AppError::Provisioning(format!(
                "VM agent status check failed: {}",
                output.stderr.trim()
            )));
        }

        let status: VmAgentStatus = serde_json::from_str(&output.stdout).map_err(|e| {
            AppError::Serialization(format!(
                "Failed to parse VM agent status: {e}. Raw: {}",
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

        if !status.vm_agent_reachable {
            return MicState::VmAgentUnreachable;
        }

        if !status.pipewire_connected {
            return MicState::PipewireUnavailable;
        }

        if !status.device_ready {
            return MicState::CloudMicMissing;
        }

        if status.packet_loss_percent > 10.0 {
            return MicState::PacketLossHigh;
        }

        if status.receiving_audio {
            return MicState::Streaming;
        }

        if status.enabled {
            return MicState::NoAudioDetected;
        }

        MicState::Error
    }
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

fn generate_session_token() -> String {
    use base64::Engine;
    let u1 = uuid::Uuid::new_v4();
    let u2 = uuid::Uuid::new_v4();
    let mut bytes = Vec::with_capacity(32);
    bytes.extend_from_slice(u1.as_bytes());
    bytes.extend_from_slice(u2.as_bytes());
    base64::engine::general_purpose::STANDARD.encode(&bytes)
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

fn shell_escape(input: &str) -> String {
    input.replace('\'', "'\"'\"'")
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
        assert_eq!(MicQualityProfile::Standard.frame_ms(), 20);
        assert_eq!(MicQualityProfile::LowLatency.frame_ms(), 10);
        assert_eq!(MicQualityProfile::HighQuality.frame_ms(), 20);
    }

    #[test]
    fn test_map_runtime_state_disabled() {
        let status = InstanceMicRuntimeStatus {
            enabled: false,
            ..Default::default()
        };
        assert_eq!(MicPassthroughService::map_runtime_state(&status), MicState::Disabled);
    }

    #[test]
    fn test_map_runtime_state_vm_unreachable() {
        let status = InstanceMicRuntimeStatus {
            enabled: true,
            vm_agent_reachable: false,
            ..Default::default()
        };
        assert_eq!(MicPassthroughService::map_runtime_state(&status), MicState::VmAgentUnreachable);
    }

    #[test]
    fn test_map_runtime_state_streaming() {
        let status = InstanceMicRuntimeStatus {
            enabled: true,
            vm_agent_reachable: true,
            pipewire_connected: true,
            device_ready: true,
            receiving_audio: true,
            ..Default::default()
        };
        assert_eq!(MicPassthroughService::map_runtime_state(&status), MicState::Streaming);
    }

    #[test]
    fn test_map_runtime_state_packet_loss_high() {
        let status = InstanceMicRuntimeStatus {
            enabled: true,
            vm_agent_reachable: true,
            pipewire_connected: true,
            device_ready: true,
            receiving_audio: true,
            packet_loss_percent: 15.0,
            ..Default::default()
        };
        assert_eq!(MicPassthroughService::map_runtime_state(&status), MicState::PacketLossHigh);
    }

    #[test]
    fn test_map_runtime_state_no_audio() {
        let status = InstanceMicRuntimeStatus {
            enabled: true,
            vm_agent_reachable: true,
            pipewire_connected: true,
            device_ready: true,
            receiving_audio: false,
            ..Default::default()
        };
        assert_eq!(MicPassthroughService::map_runtime_state(&status), MicState::NoAudioDetected);
    }

    #[test]
    fn test_session_token_format() {
        let token = generate_session_token();
        assert!(!token.is_empty());
        assert!(token.len() >= 32);
    }
}
