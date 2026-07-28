use std::{collections::HashMap, time::Duration};

use parking_lot::Mutex as SyncMutex;
use tokio::sync::RwLock;
use tracing::{info, warn};

use crate::errors::{AppError, AppResult};
use crate::mic_client::device_list::{self, MicrophoneDevice};
use crate::mic_client::{self, MicClientConfig, MicClientHandle};
use crate::models::app_state::{
    InstanceMicConfig, InstanceMicRuntimeStatus, MicQualityProfile, MicSessionResponse,
    MicSettingsUpdate, MicState,
};

use super::{
    app_context::AppContext, mic_receiver::MicReceiverProvisioner, remote_exec::RemoteExec,
};

/// In-memory mic session tracking per instance.
static MIC_SESSIONS: std::sync::OnceLock<RwLock<HashMap<u64, MicSession>>> =
    std::sync::OnceLock::new();

/// Pipeline handles keyed by instance_id.
static MIC_HANDLES: std::sync::OnceLock<SyncMutex<HashMap<u64, MicClientHandle>>> =
    std::sync::OnceLock::new();

fn get_mic_sessions() -> &'static RwLock<HashMap<u64, MicSession>> {
    MIC_SESSIONS.get_or_init(|| RwLock::new(HashMap::new()))
}

fn get_mic_handles() -> &'static SyncMutex<HashMap<u64, MicClientHandle>> {
    MIC_HANDLES.get_or_init(|| SyncMutex::new(HashMap::new()))
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
    /// List available recording devices on this machine.
    pub fn list_devices() -> AppResult<Vec<MicrophoneDevice>> {
        device_list::list_devices()
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
        config.device_id = normalize_device_id(&server.mic_device_id);
        config.device_name =
            resolved_device_name(&config.device_id, server.mic_device_name.as_str());
        config.quality_profile = server.mic_quality_profile.clone();

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
        payload: MicSettingsUpdate,
    ) -> AppResult<InstanceMicConfig> {
        let current_config = Self::get_config(context, instance_id).await?;
        let device_id = payload
            .device_id
            .as_deref()
            .map(normalize_device_id)
            .unwrap_or_else(|| current_config.device_id.clone());
        let device_name = resolve_selected_device_name(&device_id)?;
        let quality_profile = payload
            .quality_profile
            .unwrap_or_else(|| current_config.quality_profile.clone());

        context
            .update_state(move |state| {
                if let Some(server) = state
                    .provisioned_servers
                    .iter_mut()
                    .find(|server| server.instance_id == instance_id)
                {
                    server.mic_device_id = device_id.clone();
                    server.mic_device_name = device_name.clone();
                    server.mic_quality_profile = quality_profile.clone();
                }
            })
            .await?;

        let was_active = {
            let sessions = get_mic_sessions().read().await;
            sessions.contains_key(&instance_id)
        };

        if was_active {
            info!(
                instance_id = instance_id,
                "Mic settings updated while streaming; reconnecting to apply changes"
            );
            let _ = Self::reconnect(context, instance_id).await?;
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

        let selected_device_id = normalize_device_id(&persisted_config.device_id);
        let selected_device_name =
            resolved_device_name(&selected_device_id, persisted_config.device_name.as_str());
        let capture_device_id = if selected_device_id == "default" {
            None
        } else {
            Some(selected_device_id.clone())
        };

        let session_id = uuid::Uuid::new_v4().to_string();
        let session_token = generate_session_token();
        let ssrc: u32 = (uuid::Uuid::new_v4().as_u128() & 0xFFFFFFFF) as u32;
        let session_id_u64: u64 = (uuid::Uuid::new_v4().as_u128() & 0xFFFFFFFFFFFFFFFF) as u64;
        let receiver_port: u16 = 48020u16;
        let started_at = chrono::Local::now().to_rfc3339();

        // ── Start the audio capture + encode + transport pipeline ──
        let secret_bytes = session_token.as_bytes().to_vec();
        let remote_addr = format!("{}:{}", server.wireguard_server_ip.trim(), receiver_port);

        let pipeline_config = MicClientConfig {
            device_id: capture_device_id,
            quality_profile: profile.clone(),
            session_id: session_id_u64,
            session_secret: secret_bytes,
            ssrc,
            remote_addr: remote_addr.clone(),
        };

        let handle = match mic_client::start_pipeline(pipeline_config) {
            Ok(handle) => handle,
            Err(error) => {
                return Err(AppError::Provisioning(format!(
                    "Failed to start local microphone capture for '{}': {}",
                    selected_device_name, error
                )));
            }
        };
        {
            let mut handles = get_mic_handles().lock();
            handles.insert(instance_id, handle);
        }

        let session = MicSession {
            session_id: session_id.clone(),
            session_token: session_token.clone(),
            ssrc,
            started_at: started_at.clone(),
            quality_profile: profile.clone(),
        };

        {
            let mut sessions = get_mic_sessions().write().await;
            sessions.insert(instance_id, session);
        }

        // Try to notify VM agent and recreate the remote device first.
        if let Ok(remote) = build_remote_exec_for_instance(context, instance_id).await {
            let target_user = context.config.audio_target_user.clone();
            let peer_ip = state.wireguard.client_ip.clone();

            if let Err(error) =
                Self::recreate_or_install_remote_device(&remote, &target_user, instance_id).await
            {
                warn!(
                    instance_id = instance_id,
                    "VM agent device recreation failed before mic start (non-fatal): {}", error
                );
            }

            let start_result = Self::call_vm_agent_start_session(
                &remote,
                &target_user,
                &server.wireguard_server_ip,
                &session_id,
                &session_token,
                &peer_ip,
                ssrc,
                receiver_port,
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
            remote_addr = %remote_addr,
            "Microphone passthrough enabled with audio pipeline"
        );

        Ok(MicSessionResponse {
            session_id,
            session_token,
            ssrc,
            vm_wireguard_ip: server.wireguard_server_ip.clone(),
            rtp_port: receiver_port,
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

        // Stop the audio pipeline
        {
            let mut handles = get_mic_handles().lock();
            if let Some(mut handle) = handles.remove(&instance_id) {
                handle.stop();
                info!(instance_id = instance_id, "Mic audio pipeline stopped");
            }
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
        let current_config = Self::get_config(context, instance_id).await?;

        // Disable then enable using the persisted settings.
        let _ = Self::disable(context, instance_id).await;
        Self::enable(context, instance_id, Some(current_config.quality_profile)).await
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
        _vm_wg_ip: &str,
        _session_id: &str,
        _session_token: &str,
        _peer_ip: &str,
        _ssrc: u32,
        _rtp_port: u16,
        _profile: &MicQualityProfile,
    ) -> AppResult<()> {
        let cmd = remote_user_bus_command(
            target_user,
            "if [[ ! -S \"$bus_path\" ]]; then echo \"user systemd bus unavailable\"; exit 1; fi; run_user systemctl --user daemon-reload; run_user systemctl --user restart noland-mic-receiver.service; run_user systemctl --user is-active --quiet noland-mic-receiver.service; run_user pactl list short sources 2>/dev/null | grep -Eq \"(^|[[:space:]])noland_remote_microphone([[:space:]]|$)\"",
        )?;

        let output = {
            let r = remote.clone();
            tokio::task::spawn_blocking(move || r.ssh(&cmd, Duration::from_secs(30)))
                .await
                .map_err(|e| AppError::Command(format!("join failure: {e}")))??
        };

        if output.status_code != 0 {
            return Err(AppError::Provisioning(format!(
                "Remote mic session/start failed: {} {}",
                output.stderr.trim(),
                output.stdout.trim()
            )));
        }

        Ok(())
    }

    async fn call_vm_agent_stop_session(remote: &RemoteExec, target_user: &str) -> AppResult<()> {
        let cmd = remote_user_bus_command(
            target_user,
            "if [[ -S \"$bus_path\" ]]; then run_user systemctl --user restart noland-mic-receiver.service; fi",
        )?;

        let output = {
            let r = remote.clone();
            tokio::task::spawn_blocking(move || r.ssh(&cmd, Duration::from_secs(15)))
                .await
                .map_err(|e| AppError::Command(format!("join failure: {e}")))??
        };

        if output.status_code != 0 {
            return Err(AppError::Provisioning(format!(
                "Remote mic session/stop failed: {} {}",
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
            "if [[ ! -S \"$bus_path\" ]]; then echo \"user systemd bus unavailable\"; exit 1; fi; run_user systemctl --user daemon-reload; run_user systemctl --user restart noland-mic-receiver.service; run_user systemctl --user is-active --quiet noland-mic-receiver.service; run_user pactl list short sources 2>/dev/null | grep -Eq \"(^|[[:space:]])noland_remote_microphone([[:space:]]|$)\"; run_user pactl set-default-source noland_remote_microphone >/dev/null 2>&1 || true",
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
            "bus_ready=false; if [[ -S \"$bus_path\" ]]; then bus_ready=true; fi; pipewire_connected=false; if [[ \"$bus_ready\" = true ]] && run_user systemctl --user is-active --quiet pipewire.service && run_user systemctl --user is-active --quiet pipewire-pulse.service && run_user systemctl --user is-active --quiet wireplumber.service; then pipewire_connected=true; fi; receiver_active=false; if [[ \"$bus_ready\" = true ]] && run_user systemctl --user is-active --quiet noland-mic-receiver.service; then receiver_active=true; fi; source_present=false; if [[ \"$bus_ready\" = true ]] && run_user pactl list short sources 2>/dev/null | grep -Eq \"(^|[[:space:]])noland_remote_microphone([[:space:]]|$)\"; then source_present=true; fi; default_source=false; if [[ \"$bus_ready\" = true ]] && [[ \"$(run_user pactl get-default-source 2>/dev/null || true)\" = \"noland_remote_microphone\" ]]; then default_source=true; fi; udp_listening=false; if ss -uln | grep -q \":48020 \"; then udp_listening=true; fi; device_ready=false; if [[ \"$receiver_active\" = true ]] && [[ \"$source_present\" = true ]]; then device_ready=true; fi; receiving_audio=false; if [[ \"$device_ready\" = true ]] && [[ \"$udp_listening\" = true ]]; then receiving_audio=true; fi; printf \"{\\\"deviceReady\\\":%s,\\\"receivingAudio\\\":%s,\\\"packetLossPercent\\\":0.0,\\\"jitterMs\\\":0.0,\\\"bufferDepthMs\\\":0.0,\\\"lastPacketMsAgo\\\":null,\\\"pipewireConnected\\\":%s,\\\"defaultSource\\\":%s}\\n\" \"$device_ready\" \"$receiving_audio\" \"$pipewire_connected\" \"$default_source\"",
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

fn resolve_selected_device_name(device_id: &str) -> AppResult<String> {
    if device_id == "default" {
        return Ok("System Default".to_string());
    }

    let devices = device_list::list_devices()?;
    devices
        .into_iter()
        .find(|device| device.id == device_id)
        .map(|device| device.name)
        .ok_or_else(|| {
            AppError::InvalidInput(format!(
                "Microphone device '{}' is no longer available on this machine.",
                device_id
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
        assert_eq!(
            MicPassthroughService::map_runtime_state(&status),
            MicState::Disabled
        );
    }

    #[test]
    fn test_map_runtime_state_vm_unreachable() {
        let status = InstanceMicRuntimeStatus {
            enabled: true,
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
