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
    orchestration::OrchestrationService,
    state_store::{JsonStateStore, StateStore},
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

            if state_changed {
                tauri::async_runtime::block_on(state_store.save_state(&initial_state))
                    .map_err(|error| format!("Failed normalizing persisted state: {error}"))?;
            }

            info!("Loaded state for Noland Connect");

            let context = AppContext::new(config, state_store, initial_state);
            app.manage(context.clone());

            let app_handle = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                OrchestrationService::resume_if_needed(&app_handle, &context).await;
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
            setup_wireguard_client,
            get_provisioning_logs,
            get_moonlight_download_url,
            get_rented_instances,
            update_vast_api_key,
            update_platform_credentials,
            update_server_preferences,
            update_moonlight_preferences,
            update_ssh_credentials,
            get_shared_storage_settings,
            save_shared_storage_settings,
            test_shared_storage_config,
            trigger_instance_backup,
            get_instance_backup_status,
            setup_instance_backup_schedule,
            remove_instance_backup_schedule,
            get_instance_sunshine_settings,
            update_instance_sunshine_settings,
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
