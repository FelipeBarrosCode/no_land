#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod commands;
mod errors;
mod input;
mod mic_client;
mod models;
mod moonlight;
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
    wireguard::normalize_wireguard_state_from_disk,
};
use tauri::{Manager, WindowEvent};
use tracing::{error, info, warn};
use utils::logging::init_logging;

fn main() {
    init_logging();

    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .setup(|app| {
            mic_client::configure_embedded_stream_runtime();
            let config = AppConfig::default();

            let app_data_dir = app
                .path()
                .app_data_dir()
                .map_err(|error| format!("Failed to resolve app data directory: {error}"))?;

            std::fs::create_dir_all(&app_data_dir)
                .map_err(|error| format!("Failed to create app data directory: {error}"))?;

            let state_path = app_data_dir.join("state.json");
            let state_store: Arc<dyn StateStore> = Arc::new(JsonStateStore::new(
                state_path.clone(),
                config.state_schema_version,
            ));

            match tauri::async_runtime::block_on(
                moonlight::composition::bootstrap_default_services(
                    state_path.clone(),
                    app_data_dir.clone(),
                ),
            ) {
                Ok(result) => {
                    if result.created {
                        info!(
                            "Bootstrapped Moonlight identity {} and persisted moonligConf",
                            result.identity.unique_id
                        );
                    } else {
                        info!(
                            "Validated existing Moonlight identity {}",
                            result.identity.unique_id
                        );
                    }
                }
                Err(error) => {
                    warn!("Moonlight bootstrap could not complete: {error}");
                }
            }

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

            if !initial_state.has_completed_guided_setup
                && (initial_state.post_wireguard_setup.setup_complete
                    || initial_state.provisioned_servers.iter().any(|server| {
                        server.steps.pairing_completed || server.steps.moonlight_configured
                    }))
            {
                initial_state.has_completed_guided_setup = true;
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
            app.manage(moonlight::platform::StreamWindowCloseState::default());
            let moonlight_manager = moonlight::composition::MoonlightManager::new(
                state_path.clone(),
                app_data_dir.clone(),
            );
            moonlight_manager
                .runtime
                .start_event_bridge(app.handle().clone());
            app.manage(moonlight_manager);

            let app_handle = app.handle().clone();
            let resume_context = context.clone();
            tauri::async_runtime::spawn(async move {
                OrchestrationService::resume_if_needed(&app_handle, &resume_context).await;
            });

            Ok(())
        })
        .on_window_event(|window, event| {
            if window.label() != moonlight::platform::STREAM_WINDOW_LABEL {
                return;
            }

            if let WindowEvent::Focused(focused) = event {
                let moonlight = window.state::<moonlight::composition::MoonlightManager>();
                if *focused {
                    let capture_mode = moonlight
                        .active_session_preferences
                        .lock()
                        .ok()
                        .and_then(|preferences| {
                            preferences.as_ref().map(|value| value.input.mouse_mode)
                        })
                        .map(|mode| match mode {
                            moonlight::domain::MouseMode::Relative => {
                                crate::input::state::MouseMode::Relative
                            }
                            moonlight::domain::MouseMode::Absolute => {
                                crate::input::state::MouseMode::Absolute
                            }
                        });
                    if let Some(capture_mode) = capture_mode {
                        if let Err(error) =
                            moonlight::platform::activate_native_stream_input(window, capture_mode)
                        {
                            warn!("Failed to restore native stream input after focus: {error}");
                        }
                    }
                } else {
                    moonlight.input.set_focus(false);
                    if let Err(error) = moonlight::platform::deactivate_native_stream_input(window)
                    {
                        warn!("Failed to release native stream input after focus loss: {error}");
                    }
                }
                return;
            }

            let WindowEvent::CloseRequested { api, .. } = event else {
                return;
            };

            let close_state = window.state::<moonlight::platform::StreamWindowCloseState>();
            if close_state.allow_close() {
                return;
            }

            api.prevent_close();
            if !close_state.begin_close_intercept() {
                return;
            }

            let app = window.app_handle().clone();
            let moonlight = app.state::<moonlight::composition::MoonlightManager>();
            let runtime = moonlight.runtime.clone();
            let input = moonlight.input.clone();
            let active_session_preferences = moonlight.active_session_preferences.clone();

            tauri::async_runtime::spawn(async move {
                let _ = runtime.stop().await;
                let _ = runtime.detach_surface().await;
                input.end_capture();
                if let Ok(mut active_preferences) = active_session_preferences.lock() {
                    *active_preferences = None;
                }
                let _ = moonlight::platform::close_stream_window(&app);
            });
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
            resume_provisioning_existing_instance,
            start_play_existing_instance,
            submit_pairing_pin,
            skip_pairing_and_continue,
            local_environment_preflight,
            setup_wireguard_client,
            reconnect_local_wireguard_client_quick,
            disconnect_local_wireguard_client_command,
            setup_wireguard_app_handoff_command,
            verify_wireguard,
            get_setup_status_command,
            verify_sunshine,
            detect_moonlight,
            setup_moonlight_sunshine_command,
            submit_moonlight_pin_to_sunshine_command,
            retry_setup_stage_command,
            start_local_sleep_prevention,
            stop_local_sleep_prevention,
            get_provisioning_logs,
            get_rented_instances,
            get_vast_wallet_summary,
            update_vast_api_key,
            update_platform_credentials,
            update_server_preferences,
            update_moonlight_preferences,
            set_instance_moonlight_pipeline_enabled,
            moonlight_get_instance_pipeline_status,
            moonlight_prepare_instance_pairing,
            moonlight_complete_instance_pairing,
            regenerate_edid,
            update_ssh_credentials,
            get_shared_storage_settings,
            save_shared_storage_settings,
            test_shared_storage_config,
            list_storage_providers,
            save_static_provider_credentials,
            test_shared_storage_connection,
            get_shared_storage_profiles,
            set_active_shared_storage_profile,
            disconnect_shared_storage_profile,
            begin_oauth_authorization,
            complete_oauth_authorization,
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
            get_instance_mic_status,
            list_microphones,
            moonlight_get_configuration,
            moonlight_register_host,
            moonlight_refresh_host,
            moonlight_begin_pairing,
            moonlight_complete_pairing,
            moonlight_list_apps,
            moonlight_start_stream,
            moonlight_disconnect_stream,
            moonlight_start_input_capture,
            moonlight_stop_input_capture,
            moonlight_update_video_geometry,
            moonlight_activate_native_mouse_capture,
            moonlight_deactivate_native_mouse_capture,
            moonlight_quit_remote_app,
            moonlight_send_relative_mouse,
            moonlight_send_absolute_mouse,
            moonlight_send_mouse_button,
            moonlight_send_keyboard,
            moonlight_send_controller_arrival,
            moonlight_send_controller_state,
            moonlight_update_preferences,
            moonlight_forget_host,
            moonlight_get_active_input_mode,
            moonlight_get_input_debug_state,
            moonlight_get_session_state
        ])
        .run(tauri::generate_context!())
        .unwrap_or_else(|error| {
            error!("error while running tauri application: {error}");
            panic!("tauri app failed: {error}");
        });
}
