#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod commands;
mod errors;
mod input;
mod mic_client;
mod models;
mod moonlight;
mod services;
mod utils;

use std::{fs::OpenOptions, io::Write, path::Path, sync::Arc};

use crate::moonlight::{domain::ColorRange, infrastructure::persistence::MoonlightStateRepository};
use commands::*;
use services::{
    app_config::AppConfig,
    app_context::AppContext,
    mic_passthrough::MicPassthroughService,
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

fn write_startup_diagnostic(stage: &str, message: &str) {
    eprintln!("[startup:{stage}] {message}");

    let path = std::env::temp_dir().join("noland-connect-startup.log");
    if let Ok(mut file) = OpenOptions::new().create(true).append(true).open(path) {
        let _ = writeln!(file, "[startup:{stage}] {message}");
    }
}

fn reset_persisted_state(
    state_store: &Arc<dyn StateStore>,
    state_path: &Path,
    app_data_dir: &Path,
    moonlight_bootstrap_created: &mut bool,
    reason: &str,
) -> crate::models::app_state::PersistedAppState {
    write_startup_diagnostic("state-reset", reason);
    warn!("{reason}");

    if let Err(error) = std::fs::remove_file(state_path) {
        if error.kind() != std::io::ErrorKind::NotFound {
            let message = format!(
                "Failed deleting persisted state file {}: {error}",
                state_path.display()
            );
            write_startup_diagnostic("state-reset", &message);
            warn!("{message}");
        }
    }

    match tauri::async_runtime::block_on(moonlight::composition::bootstrap_default_services(
        state_path.to_path_buf(),
        app_data_dir.to_path_buf(),
    )) {
        Ok(result) => {
            *moonlight_bootstrap_created = result.created;
        }
        Err(error) => {
            let message =
                format!("Moonlight bootstrap after state reset could not complete: {error}");
            write_startup_diagnostic("state-reset", &message);
            warn!("{message}");
            *moonlight_bootstrap_created = false;
        }
    }

    match tauri::async_runtime::block_on(state_store.load_state()) {
        Ok(state) => state,
        Err(error) => {
            let message = format!(
                "Failed recreating persisted state after reset, continuing with defaults: {error}"
            );
            write_startup_diagnostic("state-reset", &message);
            warn!("{message}");
            crate::models::app_state::PersistedAppState::default()
        }
    }
}

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

            let mut moonlight_bootstrap_created = match tauri::async_runtime::block_on(
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
                    result.created
                }
                Err(error) => {
                    warn!("Moonlight bootstrap could not complete: {error}");
                    false
                }
            };

            let mut initial_state = match tauri::async_runtime::block_on(state_store.load_state()) {
                Ok(state) => state,
                Err(error) => reset_persisted_state(
                    &state_store,
                    &state_path,
                    &app_data_dir,
                    &mut moonlight_bootstrap_created,
                    &format!("Failed loading persisted state; deleting and recreating it: {error}"),
                ),
            };
            let mut detected_stream_defaults = detect_client_display_for_provisioning()
                .map(|(width, height, _)| (width, height));

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

            match normalize_wireguard_state_from_disk(&mut initial_state, &app_data_dir) {
                Ok(changed) => {
                    if changed {
                        state_changed = true;
                    }
                }
                Err(error) => {
                    initial_state = reset_persisted_state(
                        &state_store,
                        &state_path,
                        &app_data_dir,
                        &mut moonlight_bootstrap_created,
                        &format!(
                            "Failed normalizing WireGuard state from disk; deleting and recreating persisted state: {error}"
                        ),
                    );
                    state_changed = false;
                }
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
            let (width, height, refresh_hz, source_label) = match initial_state.sunshine.edid_mode {
                crate::models::app_state::EdidMode::Manual => (
                    initial_state.moonlight_preferences.width,
                    initial_state.moonlight_preferences.height,
                    initial_state.sunshine.edid_refresh_rate_hz,
                    "Manual".to_string(),
                ),
                crate::models::app_state::EdidMode::MacHardware => {
                    if let Some((detected_width, detected_height, detected_refresh)) =
                        crate::services::moonlight::detect_hardware_display_for_provisioning()
                    {
                        (
                            detected_width,
                            detected_height,
                            detected_refresh,
                            "Mac Hardware".to_string(),
                        )
                    } else {
                        (1920, 1080, 60, "Fallback 1920x1080@60".to_string())
                    }
                }
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
            let mut candidates = vec![(width, height, refresh_hz, source_label)];
            if initial_state.sunshine.edid_mode == crate::models::app_state::EdidMode::AutoDetect {
                if refresh_hz != 60 {
                    candidates.push((
                        width,
                        height,
                        60,
                        "Auto-Detected (EDID refresh limited to 60 Hz)".to_string(),
                    ));
                }
                candidates.push((
                    3840,
                    2160,
                    60,
                    "Fallback 3840x2160@60 (native timing is not EDID-compatible)".to_string(),
                ));
                candidates.push((1920, 1080, 60, "Fallback 1920x1080@60".to_string()));
            }
            if !candidates.iter().any(|candidate| {
                candidate.0 == 1920 && candidate.1 == 1080 && candidate.2 == 60
            }) {
                candidates.push((1920, 1080, 60, "Emergency fallback 1920x1080@60".to_string()));
            }
            let mut generated = None;
            let mut last_generation_error = None;
            for (candidate_width, candidate_height, candidate_refresh, candidate_source) in
                candidates
            {
                match generate_headless_edid_base64(
                    candidate_width,
                    candidate_height,
                    candidate_refresh,
                ) {
                    Ok(edid) => {
                        generated = Some((edid, candidate_source));
                        break;
                    }
                    Err(error) => last_generation_error = Some(error),
                }
            }
            if let Some((generated_edid, generated_source)) = generated {
                if initial_state.sunshine.headless_edid_base64 != generated_edid
                    || initial_state.sunshine.edid_source_label != generated_source
                {
                    initial_state.sunshine.headless_edid_base64 = generated_edid;
                    initial_state.sunshine.edid_source_label = generated_source;
                    state_changed = true;
                }
            } else if initial_state
                .sunshine
                .headless_edid_base64
                .trim()
                .is_empty()
            {
                let message = format!(
                    "Failed generating startup EDID and no previous EDID exists; continuing without refresh: {}",
                    last_generation_error
                        .map(|error| error.to_string())
                        .unwrap_or_else(|| "no compatible timing".to_string())
                );
                write_startup_diagnostic("edid-generation", &message);
                warn!("{message}");
            } else if let Some(error) = last_generation_error {
                warn!(
                    "Could not refresh the startup EDID; retaining the last valid profile: {}",
                    error
                );
            }

            if detected_stream_defaults.is_none() {
                detected_stream_defaults = Some((width, height));
            }

            if state_changed {
                if let Err(error) = tauri::async_runtime::block_on(state_store.save_state(&initial_state)) {
                    let message = format!(
                        "Failed normalizing persisted state, continuing with in-memory state only: {error}"
                    );
                    write_startup_diagnostic("state-save", &message);
                    warn!("{message}");
                }
            }

            info!("Loaded state for Noland Connect");

            let artwork_config = services::app_config::IgdbConfig {
                twitch_client_id: if initial_state.credentials.twitch_client_id.trim().is_empty() {
                    config.igdb.twitch_client_id.clone()
                } else {
                    Some(initial_state.credentials.twitch_client_id.trim().to_string())
                },
                twitch_client_secret: if initial_state.credentials.twitch_client_secret.trim().is_empty() {
                    config.igdb.twitch_client_secret.clone()
                } else {
                    Some(initial_state.credentials.twitch_client_secret.trim().to_string())
                },
            };
            let artwork_service = services::software_artwork::SoftwareArtworkService::new(
                artwork_config,
                reqwest::Client::new(),
                app_data_dir.join("software-artwork-cache.json"),
            );
            let context = AppContext::new(config, state_store, initial_state);
            app.manage(context.clone());
            app.manage(artwork_service);
            app.manage(moonlight::platform::StreamWindowCloseState::default());
            let moonlight_manager = moonlight::composition::MoonlightManager::new(
                state_path.clone(),
                app_data_dir.clone(),
            );
            if moonlight_bootstrap_created {
                if let Some((default_stream_width, default_stream_height)) = detected_stream_defaults {
                    if let Err(error) = moonlight_manager.repository.update(|configuration| {
                        let video = &mut configuration.defaults.video;
                        video.width = default_stream_width;
                        video.height = default_stream_height;
                        video.color_range = ColorRange::Full;
                        Ok(())
                    }) {
                        let message = format!(
                            "Failed seeding embedded Moonlight defaults, continuing with generated defaults: {error}"
                        );
                        write_startup_diagnostic("moonlight-seed", &message);
                        warn!("{message}");
                    }
                }
            }
            moonlight_manager
                .runtime
                .start_event_bridge(app.handle().clone());
            app.manage(moonlight_manager);

            let app_handle = app.handle().clone();
            let resume_context = context.clone();
            tauri::async_runtime::spawn(async move {
                OrchestrationService::resume_if_needed(&app_handle, &resume_context).await;
            });

            tauri::async_runtime::spawn(async move {
                let mut interval = tokio::time::interval(std::time::Duration::from_secs(3));
                loop {
                    interval.tick().await;
                    MicPassthroughService::maintain_active_sessions().await;
                }
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
            let active_stream_instance_id = moonlight.active_stream_instance_id.clone();
            let mic_context = app.state::<AppContext>().inner().clone();

            tauri::async_runtime::spawn(async move {
                let _ = runtime.stop().await;
                let _ = runtime.detach_surface().await;
                input.end_capture();
                if let Ok(mut active_preferences) = active_session_preferences.lock() {
                    *active_preferences = None;
                }
                let mic_instance_id = active_stream_instance_id
                    .lock()
                    .ok()
                    .and_then(|mut instance| instance.take());
                let _ = moonlight::platform::close_stream_window(&app);
                if let Some(instance_id) = mic_instance_id {
                    if let Err(error) =
                        MicPassthroughService::stop_for_game_stream(&mic_context, instance_id).await
                    {
                        warn!(instance_id, %error, "Microphone stop failed while closing the stream window");
                    }
                }
            });
        })
        .invoke_handler(tauri::generate_handler![
            get_app_state,
            complete_onboarding,
            force_update_state_agent,
            refresh_ip_location,
            set_manual_location,
            set_os_location,
            search_offers,
            select_offer,
            start_play_flow,
            resume_provisioning_existing_instance,
            start_play_existing_instance,
            get_instance_launch_library,
            launch_instance_software,
            get_launch_instance_software_job,
            get_software_artwork,
            update_igdb_credentials,
            local_environment_preflight,
            setup_wireguard_client,
            reconnect_local_wireguard_client_quick,
            disconnect_local_wireguard_client_command,
            setup_wireguard_app_handoff_command,
            verify_wireguard,
            get_setup_status_command,
            verify_sunshine,
            setup_moonlight_sunshine_command,
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
            get_instance_display_status,
            apply_instance_display_mode,
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
            mute_instance_mic,
            unmute_instance_mic,
            get_instance_mic_metrics,
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
            moonlight_get_host_latency_preferences,
            moonlight_update_host_latency_preferences,
            moonlight_forget_host,
            moonlight_get_active_input_mode,
            moonlight_get_input_debug_state,
            moonlight_get_session_state
        ])
        .run(tauri::generate_context!())
        .unwrap_or_else(|error| {
            let message = format!("error while running tauri application: {error}");
            write_startup_diagnostic("tauri-run", &message);
            error!("{message}");
            std::process::exit(1);
        });
}
