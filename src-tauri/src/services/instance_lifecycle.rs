use std::{
    collections::{HashMap, HashSet},
    path::Path,
    time::Duration,
};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::RwLock;
use tracing::info;

use crate::{
    errors::{AppError, AppResult},
    models::{app_state::ProvisionedServerState, vast::VastInstance},
};

use super::{
    app_context::AppContext,
    remote_exec::RemoteExec,
    shared_storage::shared_storage_manager::SharedStorageManager,
    vast_api::VastApiClient,
    wireguard::{reconnect_local_wireguard_client, remove_local_wireguard_config},
};

/// In-memory tracking of lifecycle actions per instance to prevent overlap.
static LIFECYCLE_ACTIONS: std::sync::OnceLock<RwLock<HashMap<u64, String>>> =
    std::sync::OnceLock::new();

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
    pub category: String,
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
    pub async fn reconcile_owned_instances(
        context: &AppContext,
        owned_instances: &[VastInstance],
    ) -> AppResult<()> {
        let owned_instance_ids = owned_instances.iter().map(|instance| instance.id).collect();
        Self::remove_local_instances_missing_from_owned_set(context, &owned_instance_ids).await
    }

    async fn remove_local_instances_missing_from_owned_set(
        context: &AppContext,
        owned_instance_ids: &HashSet<u64>,
    ) -> AppResult<()> {
        let (removed_records, should_clear_active_instance) = {
            let state = context.state.read().await;
            let removed = state
                .provisioned_servers
                .iter()
                .filter(|record| !owned_instance_ids.contains(&record.instance_id))
                .cloned()
                .collect::<Vec<ProvisionedServerState>>();
            let clear_active = state
                .instance
                .instance_id
                .map(|instance_id| !owned_instance_ids.contains(&instance_id))
                .unwrap_or(false);
            (removed, clear_active)
        };

        for record in &removed_records {
            if record.wireguard_config_path.trim().is_empty() {
                continue;
            }

            remove_local_wireguard_config(Path::new(&record.wireguard_config_path))?;
        }

        if removed_records.is_empty() && !should_clear_active_instance {
            return Ok(());
        }

        context
            .update_state(|state| {
                state
                    .provisioned_servers
                    .retain(|record| owned_instance_ids.contains(&record.instance_id));

                if should_clear_active_instance {
                    state.instance = crate::models::app_state::InstanceState::default();
                    state.wireguard = crate::models::app_state::WireGuardState::default();
                    state.moonlight.host_address.clear();
                }
            })
            .await?;

        Ok(())
    }

    async fn fetch_sunshine_raw_config(
        sunshine_username: &str,
        sunshine_password: &str,
    ) -> AppResult<HashMap<String, Value>> {
        if sunshine_username.trim().is_empty() || sunshine_password.trim().is_empty() {
            return Err(AppError::InvalidInput(
                "Sunshine username and password are required.".to_string(),
            ));
        }

        let client = sunshine_api_client()?;
        let response = client
            .get("https://10.77.0.1:47990/api/config")
            .basic_auth(sunshine_username, Some(sunshine_password))
            .send()
            .await
            .map_err(|error| {
                AppError::Provisioning(format!(
                    "Failed to reach Sunshine config endpoint on https://10.77.0.1:47990: {error}"
                ))
            })?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            return Err(AppError::Provisioning(format!(
                "Sunshine config request failed with {status}: {body}"
            )));
        }

        response.json().await.map_err(|error| {
            AppError::Serialization(format!("Invalid Sunshine config payload: {error}"))
        })
    }

    async fn post_sunshine_raw_config(
        payload: &HashMap<String, Value>,
        sunshine_username: &str,
        sunshine_password: &str,
    ) -> AppResult<()> {
        if sunshine_username.trim().is_empty() || sunshine_password.trim().is_empty() {
            return Err(AppError::InvalidInput(
                "Sunshine username and password are required.".to_string(),
            ));
        }

        let json_body = serde_json::to_string(payload)
            .map_err(|e| AppError::Serialization(format!("Failed to serialize settings: {e}")))?;

        let client = sunshine_api_client()?;
        let response = client
            .post("https://10.77.0.1:47990/api/config")
            .basic_auth(sunshine_username, Some(sunshine_password))
            .header("Content-Type", "application/json")
            .body(json_body)
            .send()
            .await
            .map_err(|error| {
                AppError::Provisioning(format!(
                    "Failed to reach Sunshine config update endpoint on https://10.77.0.1:47990: {error}"
                ))
            })?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            return Err(AppError::Provisioning(format!(
                "Sunshine config update failed with {status}: {body}"
            )));
        }

        Ok(())
    }

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
    pub async fn reconnect_wireguard(context: &AppContext, instance_id: u64) -> AppResult<String> {
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

            let _wireguard_mutation_guard = context.begin_wireguard_mutation();
            let message = reconnect_local_wireguard_client(std::path::Path::new(&config_path))?;

            info!(instance_id = instance_id, "WireGuard reconnect completed");

            Ok(message)
        }
        .await;

        Self::release_lock(instance_id).await;
        result
    }

    /// Pause a provisioned instance. Runs backup first if available.
    pub async fn pause_instance(context: &AppContext, instance_id: u64) -> AppResult<()> {
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
    pub async fn destroy_instance(context: &AppContext, instance_id: u64) -> AppResult<()> {
        Self::acquire_lock(instance_id, "destroy").await?;

        let result = async {
            // Run backup first if shared storage is configured
            Self::maybe_run_backup_first(context, instance_id).await?;

            let (wireguard_config_path, should_clear_active_wireguard_state) = {
                let state = context.state.read().await;
                let record_path = state
                    .provisioned_servers
                    .iter()
                    .find(|record| record.instance_id == instance_id)
                    .map(|record| record.wireguard_config_path.clone())
                    .unwrap_or_default();
                let active_path = state.wireguard.config_path.clone();
                let path = if !record_path.trim().is_empty() {
                    record_path
                } else {
                    active_path.clone()
                };
                let should_clear_active = !path.trim().is_empty() && path == active_path;
                (path, should_clear_active)
            };

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

            match vast.list_instances().await {
                Ok(owned_instances) => {
                    Self::reconcile_owned_instances(context, &owned_instances).await?;
                }
                Err(error) => {
                    info!(
                        instance_id = instance_id,
                        "Destroy succeeded but rented-instance refresh failed; falling back to targeted local cleanup: {}",
                        error
                    );

                    if !wireguard_config_path.trim().is_empty() {
                        remove_local_wireguard_config(Path::new(&wireguard_config_path))?;
                    }

                    let _ = context
                        .update_state(|state| {
                            state.instance = crate::models::app_state::InstanceState::default();
                            if should_clear_active_wireguard_state {
                                state.wireguard = crate::models::app_state::WireGuardState::default();
                                state.moonlight.host_address.clear();
                            }
                            state
                                .provisioned_servers
                                .retain(|s| s.instance_id != instance_id);
                        })
                        .await;
                }
            }

            info!(instance_id = instance_id, "Instance destroyed successfully");
            Ok(())
        }
        .await;

        Self::release_lock(instance_id).await;
        result
    }

    /// Run backup before pause/destroy if shared storage is configured.
    async fn maybe_run_backup_first(context: &AppContext, instance_id: u64) -> AppResult<()> {
        let state = context.state.read().await;
        let ss_enabled = state.shared_storage.settings.enabled;
        let has_credentials = !state
            .shared_storage
            .settings
            .backblaze_key_id
            .trim()
            .is_empty()
            && !state
                .shared_storage
                .settings
                .backblaze_application_key
                .trim()
                .is_empty();
        let api_key = state.credentials.vast_api_key.clone();
        drop(state);

        if !ss_enabled || !has_credentials {
            info!(
                instance_id = instance_id,
                "Shared storage not configured, skipping pre-action backup"
            );
            return Ok(());
        }

        info!(
            instance_id = instance_id,
            "Running backup before instance lifecycle action"
        );

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

        SharedStorageManager::trigger_manual_backup(context, &remote, instance_id, &target_user)
            .await?;

        info!(
            instance_id = instance_id,
            "Pre-action backup completed successfully"
        );
        Ok(())
    }

    /// Get Sunshine settings from the provisioned instance via its REST API.
    pub async fn get_sunshine_settings(
        context: &AppContext,
        _instance_id: u64,
        sunshine_username: &str,
        sunshine_password: &str,
    ) -> AppResult<SunshineSettingsResponse> {
        let server_ip = {
            let state = context.state.read().await;
            state.wireguard.server_ip.clone()
        };

        if server_ip.trim().is_empty() {
            return Err(AppError::InvalidInput(
                "WireGuard tunnel not established. Cannot reach Sunshine API.".to_string(),
            ));
        }

        let raw = Self::fetch_sunshine_raw_config(sunshine_username, sunshine_password).await?;

        let settings = raw
            .iter()
            .map(|(key, value)| SunshineSetting {
                key: key.clone(),
                value: value.clone(),
                label: friendly_label(key),
                description: description_for_key(key).map(|s| s.to_string()),
                category: category_for_key(key).to_string(),
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
        sunshine_username: &str,
        sunshine_password: &str,
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

        Self::post_sunshine_raw_config(&payload.settings, sunshine_username, sunshine_password)
            .await?;

        info!("Sunshine settings updated successfully");
        Ok(())
    }

    pub async fn reset_sunshine_settings(
        context: &AppContext,
        _instance_id: u64,
        sunshine_username: &str,
        sunshine_password: &str,
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

        let mut current =
            Self::fetch_sunshine_raw_config(sunshine_username, sunshine_password).await?;

        current.insert(
            "port".to_string(),
            Value::from(context.config.sunshine.port),
        );
        current.insert("origin_web_ui_allowed".to_string(), Value::from("all"));
        current.insert("system_tray".to_string(), Value::from("disabled"));
        current.insert("upnp".to_string(), Value::from("off"));
        current.insert(
            "encoder".to_string(),
            Value::from(context.config.sunshine.encoder.clone()),
        );
        current.insert(
            "av1_mode".to_string(),
            Value::from(context.config.sunshine.av1_mode),
        );
        current.insert(
            "hevc_mode".to_string(),
            Value::from(context.config.sunshine.hevc_mode),
        );
        current.insert(
            "nvenc_preset".to_string(),
            Value::from(context.config.sunshine.nvenc_preset),
        );
        current.insert(
            "fec_percentage".to_string(),
            Value::from(context.config.sunshine.fec_percentage),
        );
        current.insert(
            "ping_timeout".to_string(),
            Value::from(context.config.sunshine.ping_timeout),
        );

        Self::post_sunshine_raw_config(&current, sunshine_username, sunshine_password).await?;

        info!("Sunshine settings reset to provision defaults successfully");
        Ok(())
    }

    pub async fn save_instance_to_shared_storage(
        context: &AppContext,
        instance_id: u64,
    ) -> AppResult<crate::models::app_state::BackupStatusResponse> {
        let _ = (context, instance_id);
        Err(AppError::InvalidInput(
            "Shared storage now only saves files you explicitly select in the interface. Use Export Selected instead of full backup."
                .to_string(),
        ))
    }

    pub async fn sync_instance_from_shared_storage(
        context: &AppContext,
        instance_id: u64,
    ) -> AppResult<String> {
        let _ = (context, instance_id);
        Err(AppError::InvalidInput(
            "Shared storage now only restores files you explicitly select in the interface. Open Sync and choose the files or folders you want."
                .to_string(),
        ))
    }

    pub async fn list_shared_storage_objects(
        context: &AppContext,
        instance_id: u64,
    ) -> AppResult<Vec<crate::models::app_state::SharedStorageObjectEntry>> {
        info!(
            instance_id = instance_id,
            "instance lifecycle list_shared_storage_objects start"
        );
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

        let remote = build_remote_exec_for_instance(context, &vast, instance_id).await?;
        let target_user = context.config.audio_target_user.clone();
        let result =
            SharedStorageManager::list_remote_objects(context, &remote, &target_user).await;
        if let Ok(entries) = &result {
            info!(
                instance_id = instance_id,
                count = entries.len(),
                "instance lifecycle list_shared_storage_objects complete"
            );
        }
        result
    }

    pub async fn sync_instance_from_shared_storage_selected(
        context: &AppContext,
        instance_id: u64,
        selected_paths: Vec<String>,
    ) -> AppResult<String> {
        info!(
            instance_id = instance_id,
            selected_count = selected_paths.len(),
            "instance lifecycle sync_selected start"
        );
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
            info!(
                instance_id = instance_id,
                "instance lifecycle sync_selected complete"
            );
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
            return Err(AppError::InvalidInput(
                "Vast API key is missing.".to_string(),
            ));
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
            return Err(AppError::InvalidInput(
                "Vast API key is missing.".to_string(),
            ));
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

fn sunshine_api_client() -> AppResult<reqwest::Client> {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(20))
        .danger_accept_invalid_certs(true)
        .build()
        .map_err(|error| AppError::Command(format!("Failed to build Sunshine API client: {error}")))
}

fn category_for_key(key: &str) -> &'static str {
    match key {
        "locale"
        | "sunshine_name"
        | "min_log_level"
        | "global_prep_cmd"
        | "notify_pre_releases"
        | "system_tray" => "General",
        "controller"
        | "gamepad"
        | "ds4_back_as_touchpad_click"
        | "motion_as_ds4"
        | "touchpad_as_ds4"
        | "ds5_inputtino_randomize_mac"
        | "back_button_timeout"
        | "keyboard"
        | "key_repeat_delay"
        | "key_repeat_frequency"
        | "always_send_scancodes"
        | "key_rightalt_to_key_win"
        | "mouse"
        | "high_resolution_scrolling"
        | "native_pen_touch"
        | "keybindings" => "Input",
        "audio_sink"
        | "virtual_sink"
        | "stream_audio"
        | "install_steam_audio_drivers"
        | "adapter_name"
        | "output_name"
        | "dd_configuration_option"
        | "dd_resolution_option"
        | "dd_manual_resolution"
        | "dd_refresh_rate_option"
        | "dd_manual_refresh_rate"
        | "dd_hdr_option"
        | "dd_wa_hdr_toggle_delay"
        | "dd_config_revert_delay"
        | "dd_config_revert_on_disconnect"
        | "dd_mode_remapping"
        | "max_bitrate"
        | "minimum_fps_target" => "Audio/Video",
        "upnp"
        | "address_family"
        | "address"
        | "port"
        | "origin_web_ui_allowed"
        | "external_ip"
        | "lan_encryption_mode"
        | "wan_encryption_mode"
        | "ping_timeout" => "Network",
        "file_apps" | "credentials_file" | "log_path" | "pkey" | "cert" | "file_state" => {
            "Config Files"
        }
        "fec_percentage" | "qp" | "min_threads" | "hevc_mode" | "av1_mode" | "capture"
        | "encoder" => "Advanced",
        "nvenc_preset"
        | "nvenc_twopass"
        | "nvenc_spatial_aq"
        | "nvenc_vbv_increase"
        | "nvenc_realtime_hags"
        | "nvenc_latency_over_power"
        | "nvenc_opengl_vulkan_on_dxgi"
        | "nvenc_h264_cavlc" => "NVIDIA NVENC",
        "qsv_preset" | "qsv_coder" | "qsv_slow_hevc" => "Intel QuickSync",
        "amd_usage" | "amd_rc" | "amd_enforce_hrd" | "amd_quality" | "amd_preanalysis"
        | "amd_vbaq" | "amd_coder" => "AMD AMF",
        "vt_coder" | "vt_software" | "vt_realtime" => "VideoToolbox",
        "vaapi_strict_rc_buffer" => "VA-API",
        "sw_preset" | "sw_tune" => "Software Encoder",
        _ => "Other",
    }
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

    mapping.get(key).map(|s| s.to_string()).unwrap_or_else(|| {
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
        "locale" => Some("Locale used by the Sunshine web UI."),
        "sunshine_name" => Some("Name shown to Moonlight clients."),
        "min_log_level" => Some("Minimum Sunshine log level."),
        "global_prep_cmd" => Some("Commands executed before and after app launch."),
        "notify_pre_releases" => Some("Enable Sunshine pre-release update notifications."),
        "nvenc_preset" => {
            Some("NVENC encoding quality preset (lower = faster, higher = better quality).")
        }
        "nvenc_twopass" => Some("NVENC two-pass mode selection."),
        "nvenc_spatial_aq" => Some("Enable NVENC spatial adaptive quantization."),
        "nvenc_vbv_increase" => Some("Increase NVENC VBV buffer target."),
        "nvenc_realtime_hags" => Some("Enable NVENC real-time scheduling with HAGS on Windows."),
        "nvenc_latency_over_power" => Some("Favor lower latency over power efficiency."),
        "nvenc_opengl_vulkan_on_dxgi" => Some("Use DXGI device for OpenGL/Vulkan encode path."),
        "nvenc_h264_cavlc" => Some("Use CAVLC entropy coding for H.264."),
        "hevc_mode" => Some("HEVC encoding mode: 0 = disabled, 1 = enabled, 2 = auto."),
        "av1_mode" => Some("AV1 encoding mode: 0 = disabled, 1 = enabled, 2 = auto."),
        "capture" => Some("Screen capture backend: nvfbc, kms, or x11."),
        "encoder" => Some("Video encoder: nvenc, vaapi, software."),
        "qsv_preset" => Some("Intel QuickSync preset."),
        "qsv_coder" => Some("Intel QuickSync entropy coder mode."),
        "qsv_slow_hevc" => Some("Use slower HEVC path for QuickSync when enabled."),
        "amd_usage" => Some("AMD AMF usage profile."),
        "amd_rc" => Some("AMD AMF rate-control mode."),
        "amd_enforce_hrd" => Some("Force HRD constraints on AMD AMF encoder."),
        "amd_quality" => Some("AMD AMF quality mode."),
        "amd_preanalysis" => Some("Enable AMD AMF pre-analysis."),
        "amd_vbaq" => Some("Enable AMD variance-based adaptive quantization."),
        "amd_coder" => Some("AMD AMF entropy coder mode."),
        "vt_coder" => Some("VideoToolbox entropy coder mode."),
        "vt_software" => Some("Allow VideoToolbox software encoding fallback."),
        "vt_realtime" => Some("Use VideoToolbox real-time encode mode."),
        "vaapi_strict_rc_buffer" => Some("Use strict VA-API rate-control buffering."),
        "sw_preset" => Some("Software encoder preset."),
        "sw_tune" => Some("Software encoder tuning profile."),
        "audio_sink" => Some("PulseAudio/PipeWire sink name for audio capture."),
        "virtual_sink" => Some("Virtual sink used to stream audio while muting host speakers."),
        "stream_audio" => Some("Enable or disable audio streaming."),
        "install_steam_audio_drivers" => {
            Some("Install Steam Streaming Speakers drivers on Windows.")
        }
        "output_name" => Some("Display output identifier Sunshine should stream."),
        "dd_configuration_option" => Some("Windows display device validation/configuration mode."),
        "dd_resolution_option" => Some("Display resolution management mode."),
        "dd_manual_resolution" => Some("Manual resolution when display mode is set to manual."),
        "dd_refresh_rate_option" => Some("Display refresh-rate management mode."),
        "dd_manual_refresh_rate" => Some("Manual refresh rate when display mode is manual."),
        "dd_hdr_option" => Some("Windows HDR handling mode for streamed display."),
        "dd_wa_hdr_toggle_delay" => Some("Delay before applying HDR toggle workaround."),
        "dd_config_revert_delay" => Some("Delay before reverting temporary display configuration."),
        "dd_config_revert_on_disconnect" => {
            Some("Revert display configuration automatically on disconnect.")
        }
        "dd_mode_remapping" => Some("Custom display mode remapping rules."),
        "ping_timeout" => Some("Milliseconds before disconnecting idle clients."),
        "port" => Some("TCP port Sunshine listens on for Moonlight connections."),
        "address" => Some("Bind address used by Sunshine server."),
        "address_family" => Some("Address family preference (IPv4/IPv6)."),
        "fec_percentage" => Some("Forward Error Correction percentage for stream resilience."),
        "system_tray" => Some("Show Sunshine in the system tray and send desktop notifications."),
        "upnp" => Some("Automatically open ports via UPnP (not recommended for cloud VMs)."),
        "origin_web_ui_allowed" => {
            Some("Which origins can access the Web UI: pc, lan, wan, or all.")
        }
        "external_ip" => Some("External IP override for Sunshine network advertisements."),
        "lan_encryption_mode" => Some("Encryption mode for LAN clients."),
        "wan_encryption_mode" => Some("Encryption mode for WAN clients."),
        "controller" => Some("Allow controller input from clients."),
        "gamepad" => Some("Virtual gamepad type: auto, ds4, ds5, x360, xone, switch."),
        "ds4_back_as_touchpad_click" => Some("Map DS4 back/select to touchpad click."),
        "motion_as_ds4" => Some("Treat motion-capable controllers as DS4 in auto mode."),
        "touchpad_as_ds4" => Some("Treat touchpad-capable controllers as DS4 in auto mode."),
        "ds5_inputtino_randomize_mac" => Some("Randomize virtual DS5 MAC address on Linux."),
        "keyboard" => Some("Allow keyboard input from clients."),
        "always_send_scancodes" => {
            Some("Always send keyboard scancodes (Windows compatibility setting).")
        }
        "key_rightalt_to_key_win" => Some("Map right Alt key to Windows key."),
        "mouse" => Some("Allow mouse input from clients."),
        "max_bitrate" => Some("Maximum streaming bitrate in kbps."),
        "file_apps" => Some("Path to Sunshine apps configuration file."),
        "credentials_file" => Some("Path to Sunshine credentials file."),
        "log_path" => Some("Path for Sunshine logs."),
        "pkey" => Some("TLS private key path."),
        "cert" => Some("TLS certificate path."),
        "file_state" => Some("Path to Sunshine runtime state file."),
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
