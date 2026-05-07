use std::{collections::HashMap, time::Duration};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::RwLock;
use tracing::{info, warn};

use crate::errors::{AppError, AppResult};

use super::{
    app_context::AppContext,
    remote_exec::RemoteExec,
    shared_storage::shared_storage_manager::SharedStorageManager,
    vast_api::VastApiClient,
    wireguard::{
        read_local_wireguard_show_output, reconnect_local_wireguard_client,
    },
};

/// In-memory tracking of lifecycle actions per instance to prevent overlap.
static LIFECYCLE_ACTIONS: std::sync::OnceLock<RwLock<HashMap<u64, String>>> = std::sync::OnceLock::new();

fn get_lifecycle_actions() -> &'static RwLock<HashMap<u64, String>> {
    LIFECYCLE_ACTIONS.get_or_init(|| RwLock::new(HashMap::new()))
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SunshineSetting {
    pub key: String,
    pub value: Value,
    pub label: String,
    pub description: Option<String>,
    pub value_type: String,
    pub requires_restart: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SunshineSettingsResponse {
    pub settings: Vec<SunshineSetting>,
    pub raw: HashMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SunshineSettingsUpdatePayload {
    pub settings: HashMap<String, Value>,
}

pub struct InstanceLifecycleService;

impl InstanceLifecycleService {
    /// Acquire a lifecycle action lock for an instance.
    async fn acquire_lock(instance_id: u64, action: &str) -> AppResult<()> {
        let mut actions = get_lifecycle_actions().write().await;
        if let Some(running) = actions.get(&instance_id) {
            return Err(AppError::Provisioning(format!(
                "Instance {} is already running action: {}. Please wait.",
                instance_id, running
            )));
        }
        actions.insert(instance_id, action.to_string());
        Ok(())
    }

    async fn release_lock(instance_id: u64) {
        let mut actions = get_lifecycle_actions().write().await;
        actions.remove(&instance_id);
    }

    /// Reconnect WireGuard for a provisioned instance.
    pub async fn reconnect_wireguard(
        context: &AppContext,
        instance_id: u64,
    ) -> AppResult<String> {
        Self::acquire_lock(instance_id, "reconnect").await?;

        let result = async {
            let config_path = {
                let state = context.state.read().await;
                state.wireguard.config_path.clone()
            };

            if config_path.trim().is_empty() {
                return Err(AppError::InvalidInput(
                    "WireGuard client config path is empty. Run provisioning first.".to_string(),
                ));
            }

            let mut message = reconnect_local_wireguard_client(std::path::Path::new(&config_path))?;
            if !local_wireguard_has_peer() {
                warn!(
                    instance_id = instance_id,
                    "WireGuard reconnect completed but local peer state is not visible yet"
                );
                message = format!(
                    "{} (peer state is still initializing; retry in a few seconds if needed)",
                    message
                );
            }

            info!(
                instance_id = instance_id,
                "WireGuard reconnect completed"
            );

            Ok(message)
        }
        .await;

        Self::release_lock(instance_id).await;
        result
    }


    /// Pause a provisioned instance. Runs backup first if available.
    pub async fn pause_instance(
        context: &AppContext,
        instance_id: u64,
    ) -> AppResult<()> {
        Self::acquire_lock(instance_id, "pause").await?;

        let result = async {
            // Run backup first if shared storage is configured
            Self::maybe_run_backup_first(context, instance_id).await?;

            let api_key = {
                let state = context.state.read().await;
                state.credentials.vast_api_key.clone()
            };

            if api_key.trim().is_empty() {
                return Err(AppError::InvalidInput(
                    "Vast API key is missing.".to_string(),
                ));
            }

            let vast = VastApiClient::new(
                context.http_client.clone(),
                context.config.vast_base_url.clone(),
                api_key,
            );

            vast.pause_instance(instance_id).await?;
            info!(instance_id = instance_id, "Instance paused successfully");
            Ok(())
        }
        .await;

        Self::release_lock(instance_id).await;
        result
    }

    /// Destroy a provisioned instance. Runs backup first if available.
    pub async fn destroy_instance(
        context: &AppContext,
        instance_id: u64,
    ) -> AppResult<()> {
        Self::acquire_lock(instance_id, "destroy").await?;

        let result = async {
            // Run backup first if shared storage is configured
            Self::maybe_run_backup_first(context, instance_id).await?;

            let api_key = {
                let state = context.state.read().await;
                state.credentials.vast_api_key.clone()
            };

            if api_key.trim().is_empty() {
                return Err(AppError::InvalidInput(
                    "Vast API key is missing.".to_string(),
                ));
            }

            let vast = VastApiClient::new(
                context.http_client.clone(),
                context.config.vast_base_url.clone(),
                api_key,
            );

            vast.destroy_instance(instance_id).await?;

            // Clean up local state references to the destroyed instance
            let _ = context
                .update_state(|state| {
                    state.instance = crate::models::app_state::InstanceState::default();
                    state.provisioned_servers.retain(|s| s.instance_id != instance_id);
                })
                .await;

            info!(instance_id = instance_id, "Instance destroyed successfully");
            Ok(())
        }
        .await;

        Self::release_lock(instance_id).await;
        result
    }

    /// Run backup before pause/destroy if shared storage is configured.
    async fn maybe_run_backup_first(
        context: &AppContext,
        instance_id: u64,
    ) -> AppResult<()> {
        let state = context.state.read().await;
        let ss_enabled = state.shared_storage.settings.enabled;
        let has_credentials = !state.shared_storage.settings.backblaze_key_id.trim().is_empty()
            && !state.shared_storage.settings.backblaze_application_key.trim().is_empty();
        let api_key = state.credentials.vast_api_key.clone();
        drop(state);

        if !ss_enabled || !has_credentials {
            info!(instance_id = instance_id, "Shared storage not configured, skipping pre-action backup");
            return Ok(());
        }

        info!(instance_id = instance_id, "Running backup before instance lifecycle action");

        if api_key.trim().is_empty() {
            return Err(AppError::InvalidInput(
                "Vast API key is missing for pre-action backup.".to_string(),
            ));
        }

        // Build RemoteExec for the target instance (not global active instance).
        let vast = VastApiClient::new(
            context.http_client.clone(),
            context.config.vast_base_url.clone(),
            api_key,
        );
        let remote = build_remote_exec_for_instance(context, &vast, instance_id).await?;
        let target_user = context.config.audio_target_user.clone();

        SharedStorageManager::trigger_manual_backup(
            context,
            &remote,
            instance_id,
            &target_user,
        )
        .await?;

        info!(instance_id = instance_id, "Pre-action backup completed successfully");
        Ok(())
    }

    /// Get Sunshine settings from the provisioned instance via its REST API.
    pub async fn get_sunshine_settings(
        context: &AppContext,
        _instance_id: u64,
    ) -> AppResult<SunshineSettingsResponse> {
        // Sunshine REST API is available at 10.77.0.1:47990
        // We proxy through the backend to avoid CORS/direct network issues
        let server_ip = {
            let state = context.state.read().await;
            state.wireguard.server_ip.clone()
        };

        if server_ip.trim().is_empty() {
            return Err(AppError::InvalidInput(
                "WireGuard tunnel not established. Cannot reach Sunshine API.".to_string(),
            ));
        }

        // Use curl through SSH to the VM, then to Sunshine localhost
        let remote = build_remote_exec_from_context(context).await?;

        let cmd = format!(
            "curl -k -s --connect-timeout 10 https://localhost:47990/api/config 2>/dev/null || echo '{{}}'"
        );

        let output = {
            let remote = remote.clone();
            tokio::task::spawn_blocking(move || remote.ssh(&cmd, Duration::from_secs(30)))
                .await
                .map_err(|e| AppError::Command(format!("join failure: {e}")))??
        };

        if output.status_code != 0 {
            return Err(AppError::Provisioning(format!(
                "Failed to fetch Sunshine settings: {}",
                output.stderr.trim()
            )));
        }

        let raw: HashMap<String, Value> = serde_json::from_str(&output.stdout)
            .unwrap_or_default();

        let settings = raw
            .iter()
            .map(|(key, value)| SunshineSetting {
                key: key.clone(),
                value: value.clone(),
                label: friendly_label(key),
                description: description_for_key(key).map(|s| s.to_string()),
                value_type: infer_value_type(value),
                requires_restart: requires_restart(key),
            })
            .collect();

        Ok(SunshineSettingsResponse { settings, raw })
    }

    /// Update Sunshine settings on the provisioned instance.
    pub async fn update_sunshine_settings(
        context: &AppContext,
        _instance_id: u64,
        payload: SunshineSettingsUpdatePayload,
    ) -> AppResult<()> {
        let server_ip = {
            let state = context.state.read().await;
            state.wireguard.server_ip.clone()
        };

        if server_ip.trim().is_empty() {
            return Err(AppError::InvalidInput(
                "WireGuard tunnel not established. Cannot reach Sunshine API.".to_string(),
            ));
        }

        let remote = build_remote_exec_from_context(context).await?;

        // Serialize the updated settings
        let json_body = serde_json::to_string(&payload.settings)
            .map_err(|e| AppError::Serialization(format!("Failed to serialize settings: {e}")))?;

        let cmd = format!(
            "curl -k -s -X POST -H 'Content-Type: application/json' -d '{}' https://localhost:47990/api/config 2>/dev/null",
            shell_escape(&json_body)
        );

        let output = {
            let remote = remote.clone();
            tokio::task::spawn_blocking(move || remote.ssh(&cmd, Duration::from_secs(30)))
                .await
                .map_err(|e| AppError::Command(format!("join failure: {e}")))??
        };

        if output.status_code != 0 {
            return Err(AppError::Provisioning(format!(
                "Failed to update Sunshine settings: {}",
                output.stderr.trim()
            )));
        }

        info!("Sunshine settings updated successfully");
        Ok(())
    }

    pub async fn save_instance_to_shared_storage(
        context: &AppContext,
        instance_id: u64,
    ) -> AppResult<crate::models::app_state::BackupStatusResponse> {
        let api_key = {
            let state = context.state.read().await;
            state.credentials.vast_api_key.clone()
        };

        if api_key.trim().is_empty() {
            return Err(AppError::InvalidInput("Vast API key is missing.".to_string()));
        }

        let vast = VastApiClient::new(
            context.http_client.clone(),
            context.config.vast_base_url.clone(),
            api_key,
        );

        let remote = build_remote_exec_for_instance(context, &vast, instance_id).await?;
        let target_user = context.config.audio_target_user.clone();
        SharedStorageManager::trigger_manual_backup(context, &remote, instance_id, &target_user).await?;
        SharedStorageManager::get_backup_status(context).await
    }

    pub async fn sync_instance_from_shared_storage(
        context: &AppContext,
        instance_id: u64,
    ) -> AppResult<String> {
        let api_key = {
            let state = context.state.read().await;
            state.credentials.vast_api_key.clone()
        };

        if api_key.trim().is_empty() {
            return Err(AppError::InvalidInput("Vast API key is missing.".to_string()));
        }

        let vast = VastApiClient::new(
            context.http_client.clone(),
            context.config.vast_base_url.clone(),
            api_key,
        );

        let remote = build_remote_exec_for_instance(context, &vast, instance_id).await?;
        let target_user = context.config.audio_target_user.clone();
        SharedStorageManager::auto_restore_instance(context, &remote, instance_id, &target_user).await?;
        Ok("Shared storage sync completed".to_string())
    }

    pub async fn list_shared_storage_objects(
        context: &AppContext,
        instance_id: u64,
    ) -> AppResult<Vec<crate::models::app_state::SharedStorageObjectEntry>> {
        info!(instance_id = instance_id, "instance lifecycle list_shared_storage_objects start");
        let api_key = {
            let state = context.state.read().await;
            state.credentials.vast_api_key.clone()
        };

        if api_key.trim().is_empty() {
            return Err(AppError::InvalidInput("Vast API key is missing.".to_string()));
        }

        let vast = VastApiClient::new(
            context.http_client.clone(),
            context.config.vast_base_url.clone(),
            api_key,
        );

        let remote = build_remote_exec_for_instance(context, &vast, instance_id).await?;
        let target_user = context.config.audio_target_user.clone();
        let result = SharedStorageManager::list_remote_objects(context, &remote, &target_user).await;
        if let Ok(entries) = &result {
            info!(instance_id = instance_id, count = entries.len(), "instance lifecycle list_shared_storage_objects complete");
        }
        result
    }

    pub async fn sync_instance_from_shared_storage_selected(
        context: &AppContext,
        instance_id: u64,
        selected_paths: Vec<String>,
    ) -> AppResult<String> {
        info!(instance_id = instance_id, selected_count = selected_paths.len(), "instance lifecycle sync_selected start");
        let api_key = {
            let state = context.state.read().await;
            state.credentials.vast_api_key.clone()
        };

        if api_key.trim().is_empty() {
            return Err(AppError::InvalidInput("Vast API key is missing.".to_string()));
        }

        let vast = VastApiClient::new(
            context.http_client.clone(),
            context.config.vast_base_url.clone(),
            api_key,
        );

        let remote = build_remote_exec_for_instance(context, &vast, instance_id).await?;
        let target_user = context.config.audio_target_user.clone();
        let result = SharedStorageManager::restore_selected_paths(
            context,
            &remote,
            instance_id,
            &target_user,
            &selected_paths,
        )
        .await;
        if result.is_ok() {
            info!(instance_id = instance_id, "instance lifecycle sync_selected complete");
        }
        result
    }

    pub async fn list_instance_exportable_objects(
        context: &AppContext,
        instance_id: u64,
    ) -> AppResult<Vec<crate::models::app_state::SharedStorageObjectEntry>> {
        let api_key = {
            let state = context.state.read().await;
            state.credentials.vast_api_key.clone()
        };
        if api_key.trim().is_empty() {
            return Err(AppError::InvalidInput("Vast API key is missing.".to_string()));
        }

        let vast = VastApiClient::new(
            context.http_client.clone(),
            context.config.vast_base_url.clone(),
            api_key,
        );

        let remote = build_remote_exec_for_instance(context, &vast, instance_id).await?;
        let target_user = context.config.audio_target_user.clone();
        SharedStorageManager::list_local_objects(context, &remote, &target_user).await
    }

    pub async fn save_instance_to_shared_storage_selected(
        context: &AppContext,
        instance_id: u64,
        selected_paths: Vec<String>,
    ) -> AppResult<String> {
        let api_key = {
            let state = context.state.read().await;
            state.credentials.vast_api_key.clone()
        };
        if api_key.trim().is_empty() {
            return Err(AppError::InvalidInput("Vast API key is missing.".to_string()));
        }

        let vast = VastApiClient::new(
            context.http_client.clone(),
            context.config.vast_base_url.clone(),
            api_key,
        );

        let remote = build_remote_exec_for_instance(context, &vast, instance_id).await?;
        let target_user = context.config.audio_target_user.clone();
        SharedStorageManager::backup_selected_paths(
            context,
            &remote,
            instance_id,
            &target_user,
            &selected_paths,
        )
        .await
    }
}

fn local_wireguard_has_peer() -> bool {
    let stdout = match read_local_wireguard_show_output() {
        Ok(value) => value,
        Err(_) => return false,
    };

    stdout.lines().any(|line| line.trim_start().starts_with("peer:"))
}

async fn build_remote_exec_for_instance(
    context: &AppContext,
    vast: &VastApiClient,
    instance_id: u64,
) -> AppResult<RemoteExec> {
    let state = context.state.read().await.clone();
    let private_key_path = state.ssh.private_key_path.clone();
    if private_key_path.trim().is_empty() {
        return Err(AppError::InvalidInput(
            "SSH private key path is empty. Run provisioning first.".to_string(),
        ));
    }

    let ssh_user = if state.ssh.ssh_username.trim().is_empty() {
        context.config.ssh_user.clone()
    } else {
        state.ssh.ssh_username.clone()
    };

    let instance = vast.get_instance(instance_id).await?;
    let ssh_host = if instance.public_ip.trim().is_empty() {
        instance.ssh_host.clone()
    } else {
        instance.public_ip.clone()
    };
    if ssh_host.trim().is_empty() || instance.ssh_port == 0 {
        return Err(AppError::InvalidInput(format!(
            "Instance {} SSH details are unavailable.",
            instance_id
        )));
    }

    Ok(RemoteExec {
        ssh_user,
        ssh_host,
        ssh_port: instance.ssh_port,
        private_key_path,
    })
}

/// Build a RemoteExec from the current app context state.
async fn build_remote_exec_from_context(context: &AppContext) -> AppResult<RemoteExec> {
    let state = context.state.read().await.clone();
    let private_key_path = state.ssh.private_key_path.clone();
    if private_key_path.trim().is_empty() {
        return Err(AppError::InvalidInput(
            "SSH private key path is empty. Run provisioning first.".to_string(),
        ));
    }
    let ssh_host = state.instance.ssh_host.clone();
    let ssh_port = state.instance.ssh_port;
    let ssh_user = if state.ssh.ssh_username.trim().is_empty() {
        context.config.audio_target_user.clone()
    } else {
        state.ssh.ssh_username.clone()
    };
    if ssh_host.trim().is_empty() || ssh_port == 0 {
        return Err(AppError::InvalidInput(
            "Instance SSH details are not available. Ensure the instance is running.".to_string(),
        ));
    }
    Ok(RemoteExec {
        ssh_user,
        ssh_host,
        ssh_port,
        private_key_path,
    })
}

fn shell_escape(input: &str) -> String {
    input.replace('\'', "'\"'\"'")
}

fn friendly_label(key: &str) -> String {
    let mapping: HashMap<&str, &str> = [
        ("nvenc_preset", "NVENC Preset"),
        ("hevc_mode", "HEVC Mode"),
        ("av1_mode", "AV1 Mode"),
        ("capture", "Capture Method"),
        ("encoder", "Encoder"),
        ("output_name", "Display Output"),
        ("audio_sink", "Audio Sink"),
        ("ping_timeout", "Ping Timeout"),
        ("port", "Sunshine Port"),
        ("address", "Bind Address"),
        ("system_tray", "System Tray"),
        ("upnp", "UPnP"),
        ("fec_percentage", "FEC Percentage"),
        ("origin_web_ui_allowed", "Web UI Access"),
        ("min_log_level", "Min Log Level"),
        ("sunshine_name", "Sunshine Name"),
        ("key_repeat_delay", "Key Repeat Delay"),
        ("key_repeat_frequency", "Key Repeat Frequency"),
        ("back_button_timeout", "Back Button Timeout"),
        ("high_resolution_scrolling", "High Resolution Scrolling"),
        ("gamepad", "Gamepad Type"),
        ("native_pen_touch", "Native Pen/Touch"),
        ("adapter_name", "Video Adapter"),
        ("dd_configuration_option", "Display Configuration"),
        ("dd_resolution_option", "Display Resolution"),
        ("dd_refresh_rate_option", "Display Refresh Rate"),
        ("max_bitrate", "Max Bitrate"),
        ("minimum_fps_target", "Minimum FPS Target"),
        ("qp", "QP"),
        ("min_threads", "Min Threads"),
        ("locale", "Locale"),
    ]
    .into_iter()
    .collect();

    mapping
        .get(key)
        .map(|s| s.to_string())
        .unwrap_or_else(|| {
            key.split('_')
                .map(|word| {
                    let mut chars = word.chars();
                    match chars.next() {
                        None => String::new(),
                        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
                    }
                })
                .collect::<Vec<_>>()
                .join(" ")
        })
}

fn description_for_key(key: &str) -> Option<&'static str> {
    match key {
        "nvenc_preset" => Some("NVENC encoding quality preset (lower = faster, higher = better quality)."),
        "hevc_mode" => Some("HEVC encoding mode: 0 = disabled, 1 = enabled, 2 = auto."),
        "av1_mode" => Some("AV1 encoding mode: 0 = disabled, 1 = enabled, 2 = auto."),
        "capture" => Some("Screen capture backend: nvfbc, kms, or x11."),
        "encoder" => Some("Video encoder: nvenc, vaapi, software."),
        "audio_sink" => Some("PulseAudio/PipeWire sink name for audio capture."),
        "ping_timeout" => Some("Milliseconds before disconnecting idle clients."),
        "port" => Some("TCP port Sunshine listens on for Moonlight connections."),
        "fec_percentage" => Some("Forward Error Correction percentage for stream resilience."),
        "system_tray" => Some("Show Sunshine in the system tray and send desktop notifications."),
        "upnp" => Some("Automatically open ports via UPnP (not recommended for cloud VMs)."),
        "origin_web_ui_allowed" => Some("Which origins can access the Web UI: pc, lan, wan, or all."),
        "gamepad" => Some("Virtual gamepad type: auto, ds4, ds5, x360, xone, switch."),
        "max_bitrate" => Some("Maximum streaming bitrate in kbps."),
        _ => None,
    }
}

fn infer_value_type(value: &Value) -> String {
    match value {
        Value::Bool(_) => "boolean".to_string(),
        Value::Number(n) if n.is_i64() || n.is_u64() => "integer".to_string(),
        Value::Number(_) => "float".to_string(),
        Value::Array(_) => "array".to_string(),
        _ => "string".to_string(),
    }
}

fn requires_restart(key: &str) -> bool {
    matches!(
        key,
        "port" | "address" | "upnp" | "capture" | "encoder" | "audio_sink" | "adapter_name"
    )
}
