#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod commands;
mod errors;
mod models;
mod services;
mod utils;

use std::sync::Arc;

use commands::*;
use services::{
    app_config::AppConfig,
    app_context::AppContext,
    moonlight::detect_client_display_for_provisioning,
    orchestration::OrchestrationService,
    ssh_keys::normalize_ssh_state_from_disk,
    state_store::{JsonStateStore, StateStore},
    sunshine::{generate_headless_edid_base64, EDID_MAX_REFRESH_HZ, EDID_MIN_REFRESH_HZ},
    wireguard::{maintain_persisted_local_tunnel, normalize_wireguard_state_from_disk},
};
use tauri::Manager;
use tracing::{error, info};
use utils::logging::init_logging;

fn main() {
    init_logging();

    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .setup(|app| {
            let config = AppConfig::default();

            let app_data_dir = app
                .path()
                .app_data_dir()
                .map_err(|error| format!("Failed to resolve app data directory: {error}"))?;

            std::fs::create_dir_all(&app_data_dir)
                .map_err(|error| format!("Failed to create app data directory: {error}"))?;

            let state_path = app_data_dir.join("state.json");
            let state_store: Arc<dyn StateStore> =
                Arc::new(JsonStateStore::new(state_path, config.state_schema_version));

            let mut initial_state = tauri::async_runtime::block_on(state_store.load_state())
                .map_err(|error| format!("Failed loading persisted state: {error}"))?;

            let mut state_changed = false;
            if initial_state
                .server_preferences
                .template_hash
                .trim()
                .is_empty()
            {
                initial_state.server_preferences.template_hash =
                    config.default_template_hash.clone();
                state_changed = true;
            }

            if initial_state.server_preferences.min_reliability <= 0.0 {
                initial_state.server_preferences.min_reliability = config.min_host_reliability;
                state_changed = true;
            }

            if normalize_ssh_state_from_disk(&mut initial_state, &app_data_dir) {
                state_changed = true;
            }

            if normalize_wireguard_state_from_disk(&mut initial_state, &app_data_dir)
                .map_err(|error| format!("Failed normalizing WireGuard state from disk: {error}"))?
            {
                state_changed = true;
            }

            initial_state.sunshine.edid_refresh_rate_hz = initial_state
                .sunshine
                .edid_refresh_rate_hz
                .clamp(EDID_MIN_REFRESH_HZ, EDID_MAX_REFRESH_HZ);
            if initial_state
                .sunshine
                .headless_edid_base64
                .trim()
                .is_empty()
            {
                let (width, height, refresh_hz, source_label) =
                    match initial_state.sunshine.edid_mode {
                        crate::models::app_state::EdidMode::Manual => (
                            initial_state.moonlight_preferences.width,
                            initial_state.moonlight_preferences.height,
                            initial_state.sunshine.edid_refresh_rate_hz,
                            "Manual".to_string(),
                        ),
                        crate::models::app_state::EdidMode::AutoDetect => {
                            if let Some((detected_width, detected_height, detected_refresh)) =
                                detect_client_display_for_provisioning()
                            {
                                (
                                    detected_width,
                                    detected_height,
                                    detected_refresh,
                                    "Auto-Detected".to_string(),
                                )
                            } else {
                                (1920, 1080, 60, "Fallback 1920x1080@60".to_string())
                            }
                        }
                    };
                initial_state.sunshine.headless_edid_base64 =
                    generate_headless_edid_base64(width, height, refresh_hz)
                        .map_err(|error| format!("Failed generating default EDID: {error}"))?;
                initial_state.sunshine.edid_source_label = source_label;
                state_changed = true;
            }

            if state_changed {
                tauri::async_runtime::block_on(state_store.save_state(&initial_state))
                    .map_err(|error| format!("Failed normalizing persisted state: {error}"))?;
            }

            info!("Loaded state for Noland Connect");

            let context = AppContext::new(config, state_store, initial_state);
            app.manage(context.clone());

            let app_handle = app.handle().clone();
            let resume_context = context.clone();
            tauri::async_runtime::spawn(async move {
                OrchestrationService::resume_if_needed(&app_handle, &resume_context).await;
            });

            let monitor_context = context.clone();
            tauri::async_runtime::spawn(async move {
                let interval = std::time::Duration::from_secs(30);
                loop {
                    tokio::time::sleep(interval).await;
                    if let Err(error) = maintain_persisted_local_tunnel(&monitor_context).await {
                        error!("WireGuard tunnel health monitor failed: {error}");
                    }
                }
            });

            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            get_app_state,
            complete_onboarding,
            refresh_ip_location,
            set_manual_location,
            set_os_location,
            search_offers,
            select_offer,
            start_play_flow,
            start_play_existing_instance,
            submit_pairing_pin,
            skip_pairing_and_continue,
            local_environment_preflight,
            setup_wireguard_client,
            reconnect_local_wireguard_client_quick,
            setup_wireguard_app_handoff_command,
            verify_wireguard,
            open_wireguard_app_command,
            download_wireguard_config_command,
            get_setup_status_command,
            verify_sunshine,
            detect_moonlight,
            setup_moonlight_sunshine_command,
            submit_moonlight_pin_to_sunshine_command,
            retry_setup_stage_command,
            start_local_sleep_prevention,
            stop_local_sleep_prevention,
            get_provisioning_logs,
            get_moonlight_download_url,
            get_wireguard_download_url,
            launch_moonlight_client,
            configure_moonlight_client,
            restore_moonlight_backup,
            get_rented_instances,
            update_vast_api_key,
            update_tailscale_api_key,
            update_connection_provider,
            update_platform_credentials,
            update_server_preferences,
            update_moonlight_preferences,
            regenerate_edid,
            update_ssh_credentials,
            get_shared_storage_settings,
            save_shared_storage_settings,
            test_shared_storage_config,
            trigger_instance_backup,
            trigger_instance_backup_for,
            sync_instance_from_shared_storage,
            list_instance_shared_storage_objects,
            sync_instance_from_shared_storage_selected,
            list_instance_exportable_storage_objects,
            save_instance_to_shared_storage_selected,
            get_instance_backup_status,
            setup_instance_backup_schedule,
            remove_instance_backup_schedule,
            get_instance_sunshine_settings,
            update_instance_sunshine_settings,
            reset_instance_sunshine_settings,
            reconnect_instance_wireguard,
            reboot_instance_services,
            pause_instance,
            destroy_instance,
            generate_bundle_index,
            get_instance_restore_bundles,
            dry_run_restore,
            restore_bundle,
            get_restore_job,
            get_instance_mic_config,
            update_instance_mic_settings,
            enable_instance_mic,
            disable_instance_mic,
            reconnect_instance_mic,
            recreate_instance_mic_device,
            get_instance_mic_status
        ])
        .run(tauri::generate_context!())
        .unwrap_or_else(|error| {
            error!("error while running tauri application: {error}");
            panic!("tauri app failed: {error}");
        });
}
