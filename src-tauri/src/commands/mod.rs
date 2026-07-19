use std::{path::Path, process::Command, time::Duration};

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, State};

use crate::{
    errors::{AppError, FrontendError},
    models::{
        app_state::{
            BackupStatusResponse, BundleIndex, ConnectionProvider, EdidMode, InstanceMicConfig,
            InstanceMicRuntimeStatus, LocationSource, ManualLocationInput, MicQualityProfile,
            MicSessionResponse, MicSettingsUpdate, MoonlightPreferences, OnboardingPayload,
            OrchestrationState, PersistedAppState, PostWireGuardSetupState, RentedInstanceSummary,
            RestoreDryRunResult, RestoreJob, RestoreRequest, ServerPreferencesUpdate, SetupStage,
            SharedStorageInstanceStatus, SharedStorageSettingsResponse,
            SharedStorageSettingsUpdate,
        },
        events::ProvisioningEvent,
    },
    moonlight::{
        application::{
            apps::{self, RefreshPolicy},
            hosts::{self, RegisterHostRequest},
            launch as moonlight_launch,
            pairing::{self, PairingSessionId},
        },
        composition::MoonlightManager,
        domain::{
            AddressType, HostAddresses, HostPorts, MoonlightConfiguration, SessionState,
            StreamPreferences, StreamPreferencesPatch,
        },
        infrastructure::gamestream::ReqwestGameStreamHttpClient,
        platform::{
            close_stream_window, create_or_reuse_stream_window, stream_window_surface_descriptor,
        },
        runtime::NativeStartRequest,
    },
    services::{
        app_context::AppContext,
        instance_lifecycle::InstanceLifecycleService,
        location::LocationService,
        mic_passthrough::MicPassthroughService,
        moonlight::{
            detect_client_display_for_provisioning, MoonlightCodecPreference,
            MoonlightConfigureOptions, MoonlightConfigureResult, MoonlightNetworkPreference,
            MoonlightService,
        },
        offer_selector::OfferSelector,
        orchestration::OrchestrationService,
        os_detection::OsDetection,
        post_wireguard_setup::{
            detect_moonlight_client, download_wireguard_config, get_setup_status,
            open_wireguard_app, retry_setup_stage, setup_moonlight_sunshine,
            setup_wireguard_app_handoff, submit_moonlight_pin_to_sunshine, verify_sunshine_api,
            verify_wireguard_connection, MoonlightDetectionResult, ReachabilityResult,
            SunshineVerificationResult,
        },
        reboot_helper::RebootHelperService,
        remote_exec::RemoteExec,
        shared_storage::bundle_indexer::BundleIndexer,
        shared_storage::bundle_restore::BundleRestoreService,
        shared_storage::shared_storage_manager::SharedStorageManager,
        sleep_inhibit::SleepInhibitService,
        ssh_keys::SshKeyService,
        sunshine::{generate_headless_edid_base64, EDID_MAX_REFRESH_HZ, EDID_MIN_REFRESH_HZ},
        vast_api::VastApiClient,
        wireguard::read_local_wireguard_show_output,
    },
    utils::redact::redact_secret,
};
use tracing::{info, warn};

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolCheck {
    pub tool: String,
    pub found: bool,
    pub path: Option<String>,
    pub required_for: String,
    pub install_hint: String,
    pub install_attempted: bool,
    pub install_error: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LocalEnvironmentCheck {
    pub os: String,
    pub arch: String,
    pub ok: bool,
    pub checks: Vec<ToolCheck>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MoonlightRegisterHostInput {
    pub host_id: String,
    pub display_name: String,
    pub overlay_address: Option<String>,
    pub lan_address: Option<String>,
    pub external_address: Option<String>,
    pub http_port: u16,
    pub https_port: Option<u16>,
    pub explicit_address_type: Option<AddressType>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MoonlightCompletePairingInput {
    pub session_id: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MoonlightListAppsInput {
    pub host_id: String,
    pub force_refresh: bool,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MoonlightStartStreamInput {
    pub host_id: String,
    pub app_id: u32,
    pub replace_existing: bool,
    pub session_preferences: Option<StreamPreferencesPatch>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MoonlightPairingSessionResponse {
    pub session_id: String,
    pub host_id: String,
    pub pin: String,
    pub expires_in_seconds: u64,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MoonlightSessionStateResponse {
    pub state: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MoonlightStartStreamResponse {
    pub operation: String,
    pub state: String,
    pub has_session_url: bool,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MoonlightRelativeMouseInput {
    pub delta_x: i16,
    pub delta_y: i16,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MoonlightAbsoluteMouseInput {
    pub x: i16,
    pub y: i16,
    pub reference_width: i16,
    pub reference_height: i16,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MoonlightMouseButtonInput {
    pub button: u8,
    pub pressed: bool,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MoonlightKeyboardInput {
    pub virtual_key: u16,
    pub pressed: bool,
    pub modifiers: u8,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MoonlightControllerArrivalInput {
    pub controller_number: u8,
    pub active_gamepad_mask: u16,
    pub controller_type: u8,
    pub supported_button_flags: u32,
    pub capabilities: u16,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MoonlightControllerStateInput {
    pub controller_number: i16,
    pub active_gamepad_mask: i16,
    pub button_flags: i32,
    pub left_trigger: u8,
    pub right_trigger: u8,
    pub left_stick_x: i16,
    pub left_stick_y: i16,
    pub right_stick_x: i16,
    pub right_stick_y: i16,
}

fn local_environment_check(attempt_install: bool) -> LocalEnvironmentCheck {
    let os = OsDetection::new();
    let build_check = |tool: &str, required_for: &str| {
        let mut install_attempted = false;
        let mut install_error = None;
        if attempt_install && !os.command_exists(tool) {
            install_attempted = true;
            if let Err(error) = os.try_install_tool(tool) {
                install_error = Some(error);
            }
        }

        ToolCheck {
            tool: tool.to_string(),
            found: os.command_exists(tool),
            path: os.resolve_command_path(tool),
            required_for: required_for.to_string(),
            install_hint: os.install_hint_for_tool(tool),
            install_attempted,
            install_error,
        }
    };

    let mut checks = vec![
        build_check("ssh", "remote commands and provisioning"),
        build_check("ssh-keygen", "SSH key generation"),
        build_check("ssh-add", "SSH agent key loading"),
    ];

    if os.is_windows() {
        let mut install_attempted = false;
        let mut install_error = None;
        if attempt_install && !os.command_exists("wireguard.exe") {
            install_attempted = true;
            if let Err(error) = os.try_install_tool("wireguard.exe") {
                install_error = Some(error);
            }
        }
        checks.push(ToolCheck {
            tool: "wireguard.exe".to_string(),
            found: os.command_exists("wireguard.exe"),
            path: os.resolve_command_path("wireguard.exe"),
            required_for: "WireGuard service integration on Windows".to_string(),
            install_hint: os.install_hint_for_tool("wireguard.exe"),
            install_attempted,
            install_error,
        });
    }

    if os.is_linux() {
        let mut install_attempted = false;
        let mut install_error = None;
        if attempt_install && !os.command_exists("xdg-open") {
            install_attempted = true;
            if let Err(error) = os.try_install_tool("xdg-open") {
                install_error = Some(error);
            }
        }
        checks.push(ToolCheck {
            tool: "xdg-open".to_string(),
            found: os.command_exists("xdg-open"),
            path: os.resolve_command_path("xdg-open"),
            required_for: "Moonlight protocol launch fallback".to_string(),
            install_hint: os.install_hint_for_tool("xdg-open"),
            install_attempted,
            install_error,
        });
    }

    let arch = match os.arch() {
        crate::services::os_detection::ArchKind::X64 => "x64",
        crate::services::os_detection::ArchKind::Arm64 => "arm64",
        crate::services::os_detection::ArchKind::Unknown => "unknown",
    }
    .to_string();

    let ok = checks.iter().all(|check| check.found);
    LocalEnvironmentCheck {
        os: os.platform_display_name().to_string(),
        arch,
        ok,
        checks,
    }
}

#[tauri::command]
pub async fn local_environment_preflight() -> Result<LocalEnvironmentCheck, FrontendError> {
    Ok(local_environment_check(false))
}

fn moonlight_frontend_error(error: crate::moonlight::domain::MoonlightError) -> FrontendError {
    match error {
        crate::moonlight::domain::MoonlightError::Validation(message)
        | crate::moonlight::domain::MoonlightError::IdentityInvalid(message) => FrontendError {
            code: "moonlight_invalid_input".to_string(),
            message,
            details: None,
            retryable: false,
        },
        crate::moonlight::domain::MoonlightError::InvalidSessionTransition { from, signal } => {
            FrontendError {
                code: "moonlight_invalid_state".to_string(),
                message: format!(
                    "Invalid Moonlight session transition from {from:?} with signal {signal:?}"
                ),
                details: None,
                retryable: true,
            }
        }
        other => FrontendError {
            code: "moonlight_error".to_string(),
            message: "Moonlight operation failed".to_string(),
            details: Some(other.to_string()),
            retryable: true,
        },
    }
}

fn session_state_name(state: &SessionState) -> String {
    match state {
        SessionState::Idle => "idle",
        SessionState::Preparing => "preparing",
        SessionState::Launching => "launching",
        SessionState::CreatingSurface => "creating_surface",
        SessionState::Connecting => "connecting",
        SessionState::Streaming => "streaming",
        SessionState::Reconnecting => "reconnecting",
        SessionState::Stopping => "stopping",
    }
    .to_string()
}

#[tauri::command]
pub async fn get_app_state(
    context: State<'_, AppContext>,
) -> Result<PersistedAppState, FrontendError> {
    Ok(context.load_state().await)
}

#[tauri::command]
pub async fn complete_onboarding(
    payload: OnboardingPayload,
    context: State<'_, AppContext>,
) -> Result<PersistedAppState, FrontendError> {
    validate_onboarding_payload(&payload)?;
    info!(
        "onboarding submitted with api key {}",
        redact_secret(&payload.vast_api_key)
    );

    let app_data_root = context
        .state_store
        .path()
        .parent()
        .ok_or_else(|| AppError::State("Unable to resolve app data directory".to_string()))?
        .to_path_buf();

    let vast = VastApiClient::new(
        context.http_client.clone(),
        context.config.vast_base_url.clone(),
        payload.vast_api_key.clone(),
    );

    let ssh_service = SshKeyService::new("nolandConnectSSH");
    let key_paths = ssh_service.ensure_keypair(&app_data_root).await?;
    let uploaded = ssh_service
        .upload_public_key_if_missing(&vast, &key_paths.public_key_path)
        .await?;
    let current_state = context.load_state().await;
    let existing_edid = current_state.sunshine.headless_edid_base64.clone();
    let edid_mode = current_state.sunshine.edid_mode;
    let edid_refresh = current_state
        .sunshine
        .edid_refresh_rate_hz
        .clamp(EDID_MIN_REFRESH_HZ, EDID_MAX_REFRESH_HZ);
    let (edid_width, edid_height, edid_refresh_hz, edid_source_label) =
        resolve_effective_edid_profile(
            edid_mode,
            current_state.moonlight_preferences.width,
            current_state.moonlight_preferences.height,
            edid_refresh,
        );
    let generated_edid = if existing_edid.trim().is_empty() {
        generate_headless_edid_base64(edid_width, edid_height, edid_refresh_hz)?
    } else {
        existing_edid
    };

    let next_state = context
        .update_state(|state| {
            state.onboarding_completed = true;
            state.credentials.app_username = payload.app_username.clone();
            state.credentials.app_password = payload.app_password.clone();
            state.credentials.vast_api_key = payload.vast_api_key.clone();
            if !payload.tailscale_api_key.is_empty() {
                state.credentials.tailscale_api_key = payload.tailscale_api_key.clone();
            }
            state.ssh.key_name = "nolandConnectSSH".to_string();
            state.ssh.private_key_path = key_paths.private_key_path.display().to_string();
            state.ssh.public_key_path = key_paths.public_key_path.display().to_string();
            state.ssh.uploaded_to_vast = uploaded || state.ssh.uploaded_to_vast;
            state.ssh.ssh_username = "root".to_string();
            state.ssh.ssh_password = "user".to_string();
            state.orchestration_state = OrchestrationState::Idle;
            state.sunshine.edid_refresh_rate_hz = edid_refresh;
            state.sunshine.headless_edid_base64 = generated_edid.clone();
            state.sunshine.edid_source_label = edid_source_label.clone();
            state.last_error = None;
        })
        .await?;

    Ok(next_state)
}

#[tauri::command]
pub async fn refresh_ip_location(
    context: State<'_, AppContext>,
) -> Result<PersistedAppState, FrontendError> {
    let location_service = LocationService::new(context.http_client.clone());
    let detected = location_service.detect_ip_location().await?;

    let next_state = context
        .update_state(|state| {
            state.location = detected;
            state.last_error = None;
        })
        .await?;

    Ok(next_state)
}

#[tauri::command]
pub async fn set_manual_location(
    payload: ManualLocationInput,
    context: State<'_, AppContext>,
) -> Result<PersistedAppState, FrontendError> {
    let location = LocationService::from_manual(payload)?;
    let next_state = context
        .update_state(|state| {
            state.location = location;
            state.last_error = None;
        })
        .await?;
    Ok(next_state)
}

#[tauri::command]
pub async fn set_os_location(
    payload: ManualLocationInput,
    context: State<'_, AppContext>,
) -> Result<PersistedAppState, FrontendError> {
    let location_service = LocationService::new(context.http_client.clone());
    let mut location = location_service.resolve_os_location(payload).await?;
    location.source = LocationSource::Os;
    let next_state = context
        .update_state(|state| {
            state.location = location;
            state.last_error = None;
        })
        .await?;
    Ok(next_state)
}

#[tauri::command]
pub async fn search_offers(
    limit: Option<usize>,
    page: Option<usize>,
    page_size: Option<usize>,
    context: State<'_, AppContext>,
) -> Result<Vec<crate::models::app_state::OfferCandidate>, FrontendError> {
    let state_snapshot = context.state.read().await.clone();
    if state_snapshot.credentials.vast_api_key.trim().is_empty() {
        return Err(AppError::InvalidInput(
            "Missing Vast.ai API key. Complete onboarding first.".to_string(),
        )
        .into());
    }

    let requested_page_size = page_size
        .or(limit)
        .unwrap_or(context.config.offers_search_limit)
        .clamp(1, context.config.offers_search_limit);
    let requested_page = page.unwrap_or(1).max(1);
    let page_start = (requested_page - 1).saturating_mul(requested_page_size);
    // Vast bundles API supports `limit` but no native page/offset cursor.
    // To serve page N we fetch the top (offset + page_size + 1) rows then slice locally.
    let needed_rows = page_start
        .saturating_add(requested_page_size)
        .saturating_add(1);
    let fetch_limit = needed_rows.clamp(1, context.config.offers_search_limit);
    if needed_rows > context.config.offers_search_limit {
        warn!(
            "search_offers requested page {} with page_size {}, but offers_search_limit={} caps discoverable pages",
            requested_page,
            requested_page_size,
            context.config.offers_search_limit
        );
    }

    let vast = VastApiClient::new(
        context.http_client.clone(),
        context.config.vast_base_url.clone(),
        state_snapshot.credentials.vast_api_key.clone(),
    );
    let offers = vast
        .search_offers(
            state_snapshot.server_preferences.min_reliability,
            fetch_limit,
            Some(
                state_snapshot
                    .server_preferences
                    .geolocation_country_code
                    .as_str(),
            ),
            state_snapshot.server_preferences.require_verified,
            state_snapshot.server_preferences.require_datacenter,
            state_snapshot.server_preferences.require_avx,
        )
        .await?;

    let selector = OfferSelector {
        scoring: context.config.scoring.clone(),
    };
    let ranked = selector.rank_offers(offers, &state_snapshot.location);

    // Apply price and verification filters
    let filtered: Vec<_> = ranked
        .into_iter()
        .filter(|offer| {
            // Price filter
            let price_ok = if state_snapshot.server_preferences.max_hourly_price > 0.0 {
                offer.hourly_price <= state_snapshot.server_preferences.max_hourly_price
                    && offer.hourly_price >= state_snapshot.server_preferences.min_hourly_price
            } else {
                offer.hourly_price >= state_snapshot.server_preferences.min_hourly_price
            };

            // Verification filter
            let verified_ok =
                !state_snapshot.server_preferences.require_verified || offer.is_verified;

            // Datacenter filter
            let datacenter_ok =
                !state_snapshot.server_preferences.require_datacenter || offer.is_datacenter;

            // Offer type/category filter
            let offer_type = offer.offer_type.to_ascii_lowercase();
            let type_ok = match offer_type.as_str() {
                "on-demand" | "ondemand" => state_snapshot.server_preferences.include_on_demand,
                "interruptible" | "bid" => state_snapshot.server_preferences.include_interruptible,
                "reserved" => state_snapshot.server_preferences.include_reserved,
                _ => state_snapshot.server_preferences.include_on_demand,
            };

            let static_ip_ok =
                !state_snapshot.server_preferences.require_static_ip || offer.has_static_ip;
            let avx_ok = !state_snapshot.server_preferences.require_avx || offer.has_avx;
            let gpu_count_ok = offer.gpu_count == 1;
            let gpu_ram_ok = (offer.gpu_ram_mb as f64 / 1024.0)
                >= state_snapshot.server_preferences.min_gpu_ram_gb as f64;
            let cpu_cores_ok = offer.cpu_cores >= state_snapshot.server_preferences.min_cpu_cores;
            let down_ok =
                offer.internet_down_mbps >= state_snapshot.server_preferences.min_inet_down_mbps;
            let up_ok =
                offer.internet_up_mbps >= state_snapshot.server_preferences.min_inet_up_mbps;

            price_ok
                && verified_ok
                && datacenter_ok
                && type_ok
                && static_ip_ok
                && avx_ok
                && gpu_count_ok
                && gpu_ram_ok
                && cpu_cores_ok
                && down_ok
                && up_ok
        })
        .collect();

    let paged = filtered
        .iter()
        .skip(page_start)
        .take(requested_page_size)
        .cloned()
        .collect::<Vec<_>>();

    {
        let mut cache = context.offer_cache.write().await;
        *cache = paged.clone();
    }

    let _ = context
        .update_state(|state| {
            state.orchestration_state = OrchestrationState::SelectingServer;
        })
        .await?;

    Ok(paged)
}

#[tauri::command]
pub async fn select_offer(
    offer_id: u64,
    storage_gb: u32,
    context: State<'_, AppContext>,
) -> Result<PersistedAppState, FrontendError> {
    if storage_gb < 30 {
        return Err(AppError::InvalidInput("Storage must be at least 30GB".to_string()).into());
    }

    let selected = {
        let cache = context.offer_cache.read().await;
        cache.iter().find(|offer| offer.id == offer_id).cloned()
    }
    .ok_or_else(|| {
        AppError::NotFound(
            "Offer not found in current search results. Refresh offers and try again.".to_string(),
        )
    })?;

    let next_state = context
        .update_state(|state| {
            state.selected_offer = Some(selected);
            state.server_preferences.storage_gb = storage_gb;
            state.orchestration_state = OrchestrationState::ServerSelected;
            state.last_error = None;
        })
        .await?;

    Ok(next_state)
}

#[tauri::command]
pub async fn start_play_flow(
    app: AppHandle,
    context: State<'_, AppContext>,
) -> Result<(), FrontendError> {
    let preflight = local_environment_check(true);
    if !preflight.ok {
        let missing = preflight
            .checks
            .iter()
            .filter(|check| !check.found)
            .map(|check| {
                let install_context = check
                    .install_error
                    .as_ref()
                    .map(|error| format!(" | install error: {error}"))
                    .unwrap_or_default();
                format!("{} ({}){}", check.tool, check.install_hint, install_context)
            })
            .collect::<Vec<_>>()
            .join("; ");
        return Err(AppError::Command(format!(
            "Local environment check failed before provisioning. Missing tools: {missing}"
        ))
        .into());
    }

    OrchestrationService::start_play_flow(app, context.inner().clone()).await?;
    Ok(())
}

#[tauri::command]
pub async fn start_play_existing_instance(
    app: AppHandle,
    instance_id: u64,
    context: State<'_, AppContext>,
) -> Result<(), FrontendError> {
    let preflight = local_environment_check(true);
    if !preflight.ok {
        let missing = preflight
            .checks
            .iter()
            .filter(|check| !check.found)
            .map(|check| {
                let install_context = check
                    .install_error
                    .as_ref()
                    .map(|error| format!(" | install error: {error}"))
                    .unwrap_or_default();
                format!("{} ({}){}", check.tool, check.install_hint, install_context)
            })
            .collect::<Vec<_>>()
            .join("; ");
        return Err(AppError::Command(format!(
            "Local environment check failed before provisioning. Missing tools: {missing}"
        ))
        .into());
    }

    OrchestrationService::start_play_for_existing_instance(
        app,
        context.inner().clone(),
        instance_id,
    )
    .await?;
    Ok(())
}

#[tauri::command]
pub async fn submit_pairing_pin(
    app: AppHandle,
    context: State<'_, AppContext>,
    pin: String,
) -> Result<PersistedAppState, FrontendError> {
    OrchestrationService::submit_pairing_pin(&app, context.inner(), pin).await?;
    Ok(context.state.read().await.clone())
}

#[tauri::command]
pub async fn skip_pairing_and_continue(
    app: AppHandle,
    context: State<'_, AppContext>,
) -> Result<PersistedAppState, FrontendError> {
    OrchestrationService::skip_pairing_and_continue(&app, context.inner()).await?;
    Ok(context.state.read().await.clone())
}

#[tauri::command]
pub async fn setup_wireguard_client(
    context: State<'_, AppContext>,
) -> Result<String, FrontendError> {
    let preflight = local_environment_check(true);
    if !preflight.ok {
        let missing = preflight
            .checks
            .iter()
            .filter(|check| !check.found)
            .map(|check| format!("{} ({})", check.tool, check.install_hint))
            .collect::<Vec<_>>()
            .join("; ");
        return Err(AppError::Command(format!(
            "Local environment check failed. Missing tools: {missing}"
        ))
        .into());
    }

    let config_path = {
        let state = context.state.read().await;
        if let Some(instance_id) = state.instance.instance_id {
            if let Some(path) = state
                .provisioned_servers
                .iter()
                .find(|record| record.instance_id == instance_id)
                .map(|record| record.wireguard_config_path.clone())
                .filter(|path| std::path::Path::new(path).exists())
            {
                path
            } else {
                state.wireguard.config_path.clone()
            }
        } else {
            state.wireguard.config_path.clone()
        }
    };

    if config_path.trim().is_empty() {
        return Err(AppError::InvalidInput(
            "WireGuard client config path is empty. Run provisioning first.".to_string(),
        )
        .into());
    }

    if !Path::new(&config_path).exists() {
        return Err(AppError::NotFound(format!(
            "WireGuard client config not found at {}",
            config_path
        ))
        .into());
    }

    open_wireguard_app()?;

    Ok("WireGuard app opened. Import and activate the generated tunnel there.".to_string())
}

#[tauri::command]
pub async fn reconnect_local_wireguard_client_quick(
    context: State<'_, AppContext>,
) -> Result<String, FrontendError> {
    let preflight = local_environment_check(true);
    if !preflight.ok {
        let missing = preflight
            .checks
            .iter()
            .filter(|check| !check.found)
            .map(|check| format!("{} ({})", check.tool, check.install_hint))
            .collect::<Vec<_>>()
            .join("; ");
        return Err(AppError::Command(format!(
            "Local environment check failed. Missing tools: {missing}"
        ))
        .into());
    }

    let config_path = {
        let state = context.state.read().await;
        if let Some(instance_id) = state.instance.instance_id {
            if let Some(path) = state
                .provisioned_servers
                .iter()
                .find(|record| record.instance_id == instance_id)
                .map(|record| record.wireguard_config_path.clone())
                .filter(|path| std::path::Path::new(path).exists())
            {
                path
            } else {
                state.wireguard.config_path.clone()
            }
        } else {
            state.wireguard.config_path.clone()
        }
    };

    if config_path.trim().is_empty() {
        return Err(AppError::InvalidInput(
            "WireGuard client config path is empty. Run provisioning first.".to_string(),
        )
        .into());
    }

    if !Path::new(&config_path).exists() {
        return Err(AppError::NotFound(format!(
            "WireGuard client config not found at {}",
            config_path
        ))
        .into());
    }

    open_wireguard_app()?;

    Ok("WireGuard app opened. Use it to reconnect or toggle the tunnel.".to_string())
}

#[tauri::command]
pub async fn setup_wireguard_app_handoff_command(
    app: AppHandle,
    context: State<'_, AppContext>,
) -> Result<PostWireGuardSetupState, FrontendError> {
    setup_wireguard_app_handoff(&app, context.inner())
        .await
        .map_err(Into::into)
}

#[tauri::command]
pub async fn verify_wireguard(
    app: AppHandle,
    context: State<'_, AppContext>,
) -> Result<ReachabilityResult, FrontendError> {
    verify_wireguard_connection(&app, context.inner())
        .await
        .map_err(Into::into)
}

#[tauri::command]
pub async fn open_wireguard_app_command() -> Result<(), FrontendError> {
    open_wireguard_app().map_err(Into::into)
}

#[tauri::command]
pub async fn download_wireguard_config_command(
    context: State<'_, AppContext>,
) -> Result<String, FrontendError> {
    download_wireguard_config(context.inner())
        .await
        .map_err(Into::into)
}

#[tauri::command]
pub async fn get_setup_status_command(
    context: State<'_, AppContext>,
) -> Result<PostWireGuardSetupState, FrontendError> {
    Ok(get_setup_status(context.inner()).await)
}

#[tauri::command]
pub async fn verify_sunshine(
    app: AppHandle,
    context: State<'_, AppContext>,
) -> Result<SunshineVerificationResult, FrontendError> {
    verify_sunshine_api(&app, context.inner())
        .await
        .map_err(Into::into)
}

#[tauri::command]
pub async fn detect_moonlight(
    context: State<'_, AppContext>,
) -> Result<MoonlightDetectionResult, FrontendError> {
    detect_moonlight_client(context.inner())
        .await
        .map_err(Into::into)
}

#[tauri::command]
pub async fn setup_moonlight_sunshine_command(
    app: AppHandle,
    context: State<'_, AppContext>,
) -> Result<PostWireGuardSetupState, FrontendError> {
    setup_moonlight_sunshine(&app, context.inner())
        .await
        .map_err(Into::into)
}

#[tauri::command]
pub async fn submit_moonlight_pin_to_sunshine_command(
    app: AppHandle,
    context: State<'_, AppContext>,
    pin: String,
) -> Result<PostWireGuardSetupState, FrontendError> {
    submit_moonlight_pin_to_sunshine(&app, context.inner(), pin)
        .await
        .map_err(Into::into)
}

#[tauri::command]
pub async fn retry_setup_stage_command(
    app: AppHandle,
    context: State<'_, AppContext>,
    stage: SetupStage,
) -> Result<PostWireGuardSetupState, FrontendError> {
    retry_setup_stage(&app, context.inner(), stage)
        .await
        .map_err(Into::into)
}

async fn validate_local_wireguard_tunnel(
    context: &AppContext,
    tunnel_server_ip: &str,
    allow_handshake_retry: bool,
) -> Result<(), FrontendError> {
    let os = OsDetection::new();
    let attempts = if allow_handshake_retry { 15 } else { 3 };
    let retry_delay = if allow_handshake_retry { 3 } else { 2 };

    for attempt in 1..=attempts {
        let local_stdout = read_local_wireguard_show_output()?;
        if let Some(local_snapshot) = parse_wg_show(&local_stdout) {
            if !local_snapshot.allowed_ips.contains("10.77.0.1/32") {
                return Err(AppError::Provisioning(format!(
                    "Local WireGuard tunnel is not scoped to 10.77.0.1/32 (found: {})",
                    local_snapshot.allowed_ips
                ))
                .into());
            }

            sync_local_wireguard_keys(context, &local_snapshot).await;

            let handshake_missing = local_snapshot.latest_handshake.is_empty()
                || local_snapshot
                    .latest_handshake
                    .to_ascii_lowercase()
                    .contains("never");

            if !handshake_missing {
                break;
            }

            if attempt == attempts {
                if os.is_macos() {
                    warn!(
                            "WireGuard handshake is still missing on macOS after reconnect retries, but the local tunnel config is applied; continuing without hard failure"
                        );
                    return Ok(());
                }
                return Err(AppError::Provisioning(
                        "WireGuard tunnel exists, but peer handshake is still not visible. Tunnel state was refreshed, but the server is not responding on the WireGuard session yet. Retry reconnect once more; if it still fails, verify the server-side WireGuard service and Sunshine reachability."
                            .to_string(),
                    )
                    .into());
            }

            std::thread::sleep(Duration::from_secs(retry_delay));
            continue;
        }

        if attempt == attempts {
            if allow_handshake_retry || os.is_macos() {
                warn!(
                    "WireGuard local wg state is unavailable after retries; continuing without hard failure"
                );
                return Ok(());
            }
            return Err(AppError::Provisioning(
                "WireGuard reconnect completed, but no local tunnel state is visible yet. macOS likely detached the interface; retry reconnect once more."
                    .to_string(),
            )
            .into());
        }

        std::thread::sleep(Duration::from_secs(retry_delay));
    }

    if !tunnel_server_ip.trim().is_empty() {
        if let Err(error) = validate_wireguard_ping(tunnel_server_ip) {
            if os.is_macos() {
                warn!(
                    "WireGuard ping validation failed on macOS after reconnect/setup; continuing non-fatally: {}",
                    error
                );
            } else {
                return Err(error.into());
            }
        }
    }

    if let Err(error) = sync_server_wireguard_keys(context).await {
        warn!("best-effort server WireGuard key sync failed: {}", error);
    }

    Ok(())
}

async fn sync_local_wireguard_keys(context: &AppContext, local_snapshot: &WgSnapshot) {
    let _ = context
        .update_state(|state| {
            if !local_snapshot.interface_public_key.is_empty() {
                state.wireguard.client_public_key = local_snapshot.interface_public_key.clone();
            }
            if !local_snapshot.peer_public_key.is_empty() {
                state.wireguard.server_public_key = local_snapshot.peer_public_key.clone();
            }
        })
        .await;
}

#[derive(Debug, Clone)]
struct WgSnapshot {
    interface_public_key: String,
    peer_public_key: String,
    allowed_ips: String,
    latest_handshake: String,
}

fn parse_wg_show(raw: &str) -> Option<WgSnapshot> {
    let mut interface_public_key = String::new();
    let mut peer_public_key = String::new();
    let mut allowed_ips = String::new();
    let mut latest_handshake = String::new();
    let mut in_peer = false;

    for line in raw.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("interface:") {
            in_peer = false;
            continue;
        }
        if let Some(value) = trimmed.strip_prefix("public key:") {
            if in_peer {
                if peer_public_key.is_empty() {
                    peer_public_key = value.trim().to_string();
                }
            } else if interface_public_key.is_empty() {
                interface_public_key = value.trim().to_string();
            }
            continue;
        }
        if trimmed.starts_with("peer:") {
            in_peer = true;
            if peer_public_key.is_empty() {
                peer_public_key = trimmed.trim_start_matches("peer:").trim().to_string();
            }
            continue;
        }
        if let Some(value) = trimmed.strip_prefix("allowed ips:") {
            if in_peer && allowed_ips.is_empty() {
                allowed_ips = value.trim().to_string();
            }
        }
        if let Some(value) = trimmed.strip_prefix("latest handshake:") {
            if in_peer && latest_handshake.is_empty() {
                latest_handshake = value.trim().to_string();
            }
        }
    }

    if interface_public_key.is_empty() && peer_public_key.is_empty() {
        None
    } else {
        Some(WgSnapshot {
            interface_public_key,
            peer_public_key,
            allowed_ips,
            latest_handshake,
        })
    }
}

fn validate_wireguard_ping(server_ip: &str) -> Result<(), AppError> {
    let args = OsDetection::new().ping_args(server_ip);

    let ping = Command::new("ping").args(&args).output().map_err(|error| {
        AppError::Command(format!(
            "Failed to run ping for WireGuard validation: {error}"
        ))
    })?;

    if !ping.status.success() {
        return Err(AppError::Provisioning(format!(
            "WireGuard tunnel validation ping to {} failed: {}",
            server_ip,
            String::from_utf8_lossy(&ping.stderr).trim()
        )));
    }

    Ok(())
}

async fn sync_server_wireguard_keys(context: &AppContext) -> Result<(), AppError> {
    let (private_key_path, app_password, ssh_username, fallback_host, fallback_port) = {
        let state = context.state.read().await;
        (
            state.ssh.private_key_path.clone(),
            state.credentials.app_password.clone(),
            state.ssh.ssh_username.clone(),
            state.instance.ssh_host.clone(),
            state.instance.ssh_port,
        )
    };

    if private_key_path.trim().is_empty() || app_password.trim().is_empty() {
        return Ok(());
    }

    let (ssh_host, ssh_port, ssh_user) = {
        let pairing = context.pairing_context.read().await;
        if let Some(pairing_context) = pairing.as_ref() {
            (
                pairing_context.host.clone(),
                pairing_context.port,
                pairing_context.user.clone(),
            )
        } else {
            let user = if ssh_username.trim().is_empty() {
                context.config.audio_target_user.clone()
            } else {
                ssh_username
            };
            (fallback_host, fallback_port, user)
        }
    };

    if ssh_host.trim().is_empty() || ssh_port == 0 {
        return Ok(());
    }

    let ssh_service = SshKeyService::new("nolandConnectSSH");
    ssh_service
        .load_key_into_agent(Path::new(&private_key_path), &app_password)
        .await?;

    let remote = RemoteExec {
        ssh_user,
        ssh_host,
        ssh_port,
        private_key_path,
    };

    let remote_show = tokio::task::spawn_blocking(move || {
        remote.ssh("sudo wg show wg0", Duration::from_secs(20))
    })
    .await
    .map_err(|error| AppError::Command(format!("join failure: {error}")))??;

    if remote_show.status_code != 0 {
        return Ok(());
    }

    if let Some(server_snapshot) = parse_wg_show(&remote_show.stdout) {
        let _ = context
            .update_state(|state| {
                if !server_snapshot.interface_public_key.is_empty() {
                    state.wireguard.server_public_key =
                        server_snapshot.interface_public_key.clone();
                }
                if !server_snapshot.peer_public_key.is_empty() {
                    state.wireguard.client_public_key = server_snapshot.peer_public_key.clone();
                }
            })
            .await;
    }

    Ok(())
}

#[tauri::command]
pub async fn get_provisioning_logs(
    context: State<'_, AppContext>,
) -> Result<Vec<ProvisioningEvent>, FrontendError> {
    Ok(context.provisioning_logs.read().await.clone())
}

#[tauri::command]
pub async fn get_moonlight_download_url(
    context: State<'_, AppContext>,
) -> Result<String, FrontendError> {
    let os = OsDetection::new();
    if os.is_windows() {
        Ok(context.config.moonlight_download_url_windows.clone())
    } else if os.is_macos() {
        Ok(context.config.moonlight_download_url_macos.clone())
    } else {
        Ok(context.config.moonlight_download_url_linux.clone())
    }
}

#[tauri::command]
pub async fn get_wireguard_download_url(
    context: State<'_, AppContext>,
) -> Result<String, FrontendError> {
    let os = OsDetection::new();
    if os.is_windows() {
        Ok(context.config.wireguard_download_url_windows.clone())
    } else if os.is_macos() {
        Ok(context.config.wireguard_download_url_macos.clone())
    } else {
        Ok(context.config.wireguard_download_url_linux.clone())
    }
}

#[tauri::command]
pub async fn launch_moonlight_client() -> Result<(), FrontendError> {
    let moonlight = MoonlightService;
    moonlight.launch_native_client()?;
    Ok(())
}

#[tauri::command]
pub async fn configure_moonlight_client(
    apply: bool,
    force_close: bool,
    native: bool,
    network: Option<String>,
    prefer_codec: Option<String>,
    max_bitrate: Option<u32>,
    fps: Option<u32>,
    resolution: Option<String>,
) -> Result<MoonlightConfigureResult, FrontendError> {
    let moonlight = MoonlightService;
    let resolution_override = resolution
        .as_deref()
        .and_then(|value| value.split_once('x'))
        .and_then(|(width, height)| {
            Some((width.parse::<u32>().ok()?, height.parse::<u32>().ok()?))
        });

    let network = match network.as_deref() {
        Some("lan") => MoonlightNetworkPreference::Lan,
        Some("wifi") => MoonlightNetworkPreference::Wifi,
        Some("remote") => MoonlightNetworkPreference::Remote,
        _ => MoonlightNetworkPreference::Auto,
    };

    let prefer_codec = match prefer_codec.as_deref() {
        Some("h264") => MoonlightCodecPreference::H264,
        Some("hevc") => MoonlightCodecPreference::Hevc,
        Some("av1") => MoonlightCodecPreference::Av1,
        _ => MoonlightCodecPreference::Auto,
    };

    Ok(moonlight
        .configure_client(MoonlightConfigureOptions {
            apply,
            force_close,
            native,
            network,
            prefer_codec,
            max_bitrate,
            fps_override: fps,
            resolution_override,
            set_overrides: Default::default(),
        })
        .await)
}

#[tauri::command]
pub async fn restore_moonlight_backup(backup_file: String) -> Result<String, FrontendError> {
    let moonlight = MoonlightService;
    Ok(moonlight.restore_backup(&backup_file).await?)
}

#[tauri::command]
pub async fn start_local_sleep_prevention() -> Result<String, FrontendError> {
    SleepInhibitService::ensure_active().map_err(Into::into)
}

#[tauri::command]
pub async fn stop_local_sleep_prevention() -> Result<String, FrontendError> {
    SleepInhibitService::stop().map_err(Into::into)
}

#[tauri::command]
pub async fn get_rented_instances(
    context: State<'_, AppContext>,
) -> Result<Vec<RentedInstanceSummary>, FrontendError> {
    let state = context.state.read().await.clone();
    if state.credentials.vast_api_key.trim().is_empty() {
        return Ok(Vec::new());
    }

    let vast = VastApiClient::new(
        context.http_client.clone(),
        context.config.vast_base_url.clone(),
        state.credentials.vast_api_key,
    );

    let list_result = vast.list_instances().await;
    let instances_source = match list_result {
        Ok(instances) => {
            if let Err(error) =
                InstanceLifecycleService::reconcile_owned_instances(context.inner(), &instances)
                    .await
            {
                warn!(
                    "get_rented_instances local state reconciliation failed (continuing): {}",
                    error
                );
            }
            instances
        }
        Err(error) => {
            info!(
                "get_rented_instances list failed; returning empty list for resilience: {}",
                error
            );
            Vec::new()
        }
    };

    let mut instances = instances_source
        .into_iter()
        .filter(|instance| {
            let status = instance.status.to_ascii_lowercase();
            !status.contains("destroy") && !status.contains("stopped") && !status.contains("exited")
        })
        .map(|instance| RentedInstanceSummary {
            instance_id: instance.id,
            label: if instance.label.is_empty() {
                format!("Instance {}", instance.id)
            } else {
                instance.label
            },
            status: instance.status,
            gpu_name: instance.gpu_name,
            ssh_host: instance.ssh_host,
            ssh_port: instance.ssh_port,
            public_ip: instance.public_ip,
        })
        .collect::<Vec<_>>();

    instances.sort_by(|left, right| right.instance_id.cmp(&left.instance_id));
    Ok(instances)
}

#[tauri::command]
pub async fn update_vast_api_key(
    api_key: String,
    context: State<'_, AppContext>,
) -> Result<PersistedAppState, FrontendError> {
    let trimmed = api_key.trim().to_string();
    if trimmed.len() < 16 {
        return Err(AppError::InvalidInput("Vast API key looks invalid".to_string()).into());
    }

    let next_state = context
        .update_state(|state| {
            state.credentials.vast_api_key = trimmed;
            state.last_error = None;
        })
        .await?;

    Ok(next_state)
}

#[tauri::command]
pub async fn update_tailscale_api_key(
    api_key: String,
    context: State<'_, AppContext>,
) -> Result<PersistedAppState, FrontendError> {
    let trimmed = api_key.trim().to_string();
    validate_tailscale_auth_key(&trimmed)?;
    let next_state = context
        .update_state(|state| {
            state.credentials.tailscale_api_key = trimmed;
            state.last_error = None;
        })
        .await?;
    Ok(next_state)
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConnectionProviderUpdate {
    pub connection_provider: String,
}

#[tauri::command]
pub async fn update_connection_provider(
    payload: ConnectionProviderUpdate,
    context: State<'_, AppContext>,
) -> Result<PersistedAppState, FrontendError> {
    let provider = match payload.connection_provider.as_str() {
        "wireguard" => ConnectionProvider::Wireguard,
        "tailscale" => ConnectionProvider::Tailscale,
        _ => {
            return Err(AppError::InvalidInput(format!(
                "Unknown connection provider: {}",
                payload.connection_provider
            ))
            .into());
        }
    };
    let next_state = context
        .update_state(|state| {
            state.connection_provider = provider;
            state.last_error = None;
        })
        .await?;
    Ok(next_state)
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlatformCredentialsUpdate {
    pub app_username: String,
    pub app_password: String,
}

#[tauri::command]
pub async fn update_platform_credentials(
    payload: PlatformCredentialsUpdate,
    context: State<'_, AppContext>,
) -> Result<PersistedAppState, FrontendError> {
    if payload.app_username.trim().len() < 3 {
        return Err(
            AppError::InvalidInput("Username must have at least 3 characters".to_string()).into(),
        );
    }
    if payload.app_password.len() < 6 {
        return Err(
            AppError::InvalidInput("Password must have at least 6 characters".to_string()).into(),
        );
    }

    let app_username = payload.app_username.trim().to_string();
    let app_password = payload.app_password;

    let next_state = context
        .update_state(|state| {
            state.credentials.app_username = app_username.clone();
            state.credentials.app_password = app_password.clone();
            state.last_error = None;
        })
        .await?;

    Ok(next_state)
}

#[tauri::command]
pub async fn update_server_preferences(
    payload: ServerPreferencesUpdate,
    context: State<'_, AppContext>,
) -> Result<PersistedAppState, FrontendError> {
    if payload.min_reliability < 0.8 || payload.min_reliability > 1.0 {
        return Err(AppError::InvalidInput(
            "Min reliability must be between 0.8 and 1".to_string(),
        )
        .into());
    }
    if payload.storage_gb < 30 {
        return Err(AppError::InvalidInput("Storage must be at least 30GB".to_string()).into());
    }
    if payload.template_hash.trim().is_empty() {
        return Err(AppError::InvalidInput("Template hash cannot be empty".to_string()).into());
    }
    if payload.max_hourly_price > 0.0 && payload.min_hourly_price > payload.max_hourly_price {
        return Err(AppError::InvalidInput(
            "Min price cannot be greater than max price".to_string(),
        )
        .into());
    }
    if !(payload.include_on_demand || payload.include_interruptible || payload.include_reserved) {
        return Err(AppError::InvalidInput(
            "At least one offer category must be enabled (on-demand, interruptible, or reserved)"
                .to_string(),
        )
        .into());
    }

    let min_reliability = payload.min_reliability;
    let storage_gb = payload.storage_gb;
    let template_hash = payload.template_hash.trim().to_string();
    let max_hourly_price = payload.max_hourly_price.max(0.0);
    let min_hourly_price = payload.min_hourly_price.max(0.0);
    let require_verified = payload.require_verified;
    let require_datacenter = payload.require_datacenter;
    let include_on_demand = payload.include_on_demand;
    let include_interruptible = payload.include_interruptible;
    let include_reserved = payload.include_reserved;
    let require_static_ip = payload.require_static_ip;
    let require_avx = payload.require_avx;
    let min_gpu_count = 1;
    let min_gpu_ram_gb = payload.min_gpu_ram_gb;
    let min_cpu_cores = payload.min_cpu_cores.max(0.0);
    let min_inet_down_mbps = payload.min_inet_down_mbps.max(0.0);
    let min_inet_up_mbps = payload.min_inet_up_mbps.max(0.0);
    let geolocation_country_code = payload.geolocation_country_code.trim().to_uppercase();

    let next_state = context
        .update_state(|state| {
            state.server_preferences.min_reliability = min_reliability;
            state.server_preferences.storage_gb = storage_gb;
            state.server_preferences.template_hash = template_hash.clone();
            state.server_preferences.max_hourly_price = max_hourly_price;
            state.server_preferences.min_hourly_price = min_hourly_price;
            state.server_preferences.require_verified = require_verified;
            state.server_preferences.require_datacenter = require_datacenter;
            state.server_preferences.include_on_demand = include_on_demand;
            state.server_preferences.include_interruptible = include_interruptible;
            state.server_preferences.include_reserved = include_reserved;
            state.server_preferences.require_static_ip = require_static_ip;
            state.server_preferences.require_avx = require_avx;
            state.server_preferences.min_gpu_count = min_gpu_count;
            state.server_preferences.min_gpu_ram_gb = min_gpu_ram_gb;
            state.server_preferences.min_cpu_cores = min_cpu_cores;
            state.server_preferences.min_inet_down_mbps = min_inet_down_mbps;
            state.server_preferences.min_inet_up_mbps = min_inet_up_mbps;
            state.server_preferences.geolocation_country_code = geolocation_country_code.clone();
            state.last_error = None;
        })
        .await?;

    Ok(next_state)
}

#[tauri::command]
pub async fn update_moonlight_preferences(
    payload: MoonlightPreferences,
    context: State<'_, AppContext>,
) -> Result<PersistedAppState, FrontendError> {
    let moonlight_preferences = payload;

    let next_state = context
        .update_state(|state| {
            state.moonlight_preferences = moonlight_preferences.clone();
            state.last_error = None;
        })
        .await?;

    Ok(next_state)
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EdidSettingsUpdate {
    pub mode: EdidMode,
    pub refresh_rate_hz: u32,
}

#[tauri::command]
pub async fn regenerate_edid(
    payload: EdidSettingsUpdate,
    context: State<'_, AppContext>,
) -> Result<PersistedAppState, FrontendError> {
    info!(
        "regenerate_edid requested: mode={:?} refresh_hz={}",
        payload.mode, payload.refresh_rate_hz
    );
    if !(EDID_MIN_REFRESH_HZ..=EDID_MAX_REFRESH_HZ).contains(&payload.refresh_rate_hz) {
        return Err(AppError::InvalidInput(format!(
            "EDID refresh rate must be between {} and {} Hz",
            EDID_MIN_REFRESH_HZ, EDID_MAX_REFRESH_HZ
        ))
        .into());
    }

    let snapshot = context.load_state().await;
    let (width, height, refresh_hz, source_label) = resolve_effective_edid_profile(
        payload.mode,
        snapshot.moonlight_preferences.width,
        snapshot.moonlight_preferences.height,
        payload.refresh_rate_hz,
    );
    info!(
        "regenerate_edid resolved profile: width={} height={} refresh_hz={} source='{}'",
        width, height, refresh_hz, source_label
    );
    let generated_edid = generate_headless_edid_base64(width, height, refresh_hz)?;

    let next_state = context
        .update_state(|state| {
            state.sunshine.edid_mode = payload.mode;
            state.sunshine.edid_refresh_rate_hz = payload.refresh_rate_hz;
            state.sunshine.headless_edid_base64 = generated_edid.clone();
            state.sunshine.edid_source_label = source_label.clone();
            state.last_error = None;
        })
        .await?;

    info!(
        "regenerate_edid persisted: mode={:?} refresh_hz={} source='{}' edid_len={}",
        payload.mode,
        payload.refresh_rate_hz,
        source_label,
        generated_edid.len()
    );

    Ok(next_state)
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SshCredentialsUpdate {
    pub ssh_username: String,
    pub ssh_password: String,
}

#[tauri::command]
pub async fn update_ssh_credentials(
    payload: SshCredentialsUpdate,
    context: State<'_, AppContext>,
) -> Result<PersistedAppState, FrontendError> {
    if payload.ssh_username.trim().is_empty() {
        return Err(AppError::InvalidInput("SSH username cannot be empty".to_string()).into());
    }
    if payload.ssh_password.len() < 4 {
        return Err(AppError::InvalidInput(
            "SSH password must have at least 4 characters".to_string(),
        )
        .into());
    }

    let ssh_username = sanitize_ssh_username(&payload.ssh_username);
    let ssh_password = payload.ssh_password;

    let next_state = context
        .update_state(|state| {
            state.ssh.ssh_username = ssh_username.clone();
            state.ssh.ssh_password = ssh_password.clone();
            state.last_error = None;
        })
        .await?;

    Ok(next_state)
}

fn validate_onboarding_payload(payload: &OnboardingPayload) -> Result<(), FrontendError> {
    if payload.app_username.trim().len() < 3 {
        return Err(
            AppError::InvalidInput("Username must have at least 3 characters".to_string()).into(),
        );
    }
    if payload.app_password.len() < 6 {
        return Err(
            AppError::InvalidInput("Password must have at least 6 characters".to_string()).into(),
        );
    }
    if payload.vast_api_key.trim().len() < 16 {
        return Err(AppError::InvalidInput("Vast API key looks invalid".to_string()).into());
    }
    validate_tailscale_auth_key(payload.tailscale_api_key.trim())?;
    Ok(())
}

fn validate_tailscale_auth_key(value: &str) -> Result<(), FrontendError> {
    if value.is_empty() {
        return Ok(());
    }

    if !value.starts_with("tskey-auth-") {
        return Err(AppError::InvalidInput(
            "Tailscale requires an auth key (expected prefix: tskey-auth-), not a Tailscale API key."
                .to_string(),
        )
        .into());
    }

    Ok(())
}

fn resolve_effective_edid_profile(
    mode: EdidMode,
    width: u32,
    height: u32,
    refresh_hz: u32,
) -> (u32, u32, u32, String) {
    match mode {
        EdidMode::Manual => (width, height, refresh_hz, "Manual".to_string()),
        EdidMode::AutoDetect => {
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
    }
}

fn sanitize_ssh_username(value: &str) -> String {
    value
        .trim()
        .trim_matches('"')
        .trim_matches('\'')
        .to_lowercase()
        .to_string()
}

async fn build_remote_exec_from_state(context: &AppContext) -> Result<RemoteExec, AppError> {
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

async fn build_remote_exec_for_instance(
    context: &AppContext,
    instance_id: u64,
) -> Result<RemoteExec, AppError> {
    let state = context.state.read().await.clone();
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

    let target = state
        .provisioned_servers
        .iter()
        .find(|server| server.instance_id == instance_id)
        .map(|server| (server.ssh_host.clone(), server.ssh_port))
        .or_else(|| {
            state.instance.instance_id.and_then(|active_id| {
                (active_id == instance_id)
                    .then(|| (state.instance.ssh_host.clone(), state.instance.ssh_port))
            })
        })
        .ok_or_else(|| {
            AppError::InvalidInput(format!(
                "Instance {} is not tracked locally. Refresh rented instances and try again.",
                instance_id
            ))
        })?;

    let (ssh_host, ssh_port) = target;
    if ssh_host.trim().is_empty() || ssh_port == 0 {
        return Err(AppError::InvalidInput(format!(
            "SSH details are not available for instance {}. Ensure it is running and refreshed.",
            instance_id
        )));
    }

    Ok(RemoteExec {
        ssh_user,
        ssh_host,
        ssh_port,
        private_key_path,
    })
}

#[tauri::command]
pub async fn get_shared_storage_settings(
    context: State<'_, AppContext>,
) -> Result<SharedStorageSettingsResponse, FrontendError> {
    SharedStorageManager::get_settings(context.inner())
        .await
        .map_err(Into::into)
}

#[tauri::command]
pub async fn save_shared_storage_settings(
    payload: SharedStorageSettingsUpdate,
    context: State<'_, AppContext>,
) -> Result<PersistedAppState, FrontendError> {
    SharedStorageManager::save_settings(context.inner(), payload).await?;
    Ok(context.load_state().await)
}

#[tauri::command]
pub async fn test_shared_storage_config(
    context: State<'_, AppContext>,
) -> Result<String, FrontendError> {
    let remote = build_remote_exec_from_state(context.inner()).await?;
    let target_user = context.config.audio_target_user.clone();
    SharedStorageManager::test_configuration(context.inner(), &remote, &target_user).await?;
    Ok("Backblaze B2 configuration is valid".to_string())
}

#[tauri::command]
pub async fn trigger_instance_backup(
    context: State<'_, AppContext>,
) -> Result<BackupStatusResponse, FrontendError> {
    let remote = build_remote_exec_from_state(context.inner()).await?;
    let target_user = context.config.audio_target_user.clone();
    let instance_id = {
        let state = context.state.read().await;
        state.instance.instance_id.ok_or_else(|| {
            AppError::InvalidInput("No active instance. Start provisioning first.".to_string())
        })?
    };
    SharedStorageManager::trigger_manual_backup(
        context.inner(),
        &remote,
        instance_id,
        &target_user,
    )
    .await?;
    SharedStorageManager::get_backup_status(context.inner())
        .await
        .map_err(Into::into)
}

#[tauri::command]
pub async fn trigger_instance_backup_for(
    context: State<'_, AppContext>,
    instance_id: u64,
) -> Result<BackupStatusResponse, FrontendError> {
    InstanceLifecycleService::save_instance_to_shared_storage(context.inner(), instance_id)
        .await
        .map_err(Into::into)
}

#[tauri::command]
pub async fn sync_instance_from_shared_storage(
    context: State<'_, AppContext>,
    instance_id: u64,
) -> Result<String, FrontendError> {
    InstanceLifecycleService::sync_instance_from_shared_storage(context.inner(), instance_id)
        .await
        .map_err(Into::into)
}

#[tauri::command]
pub async fn list_instance_shared_storage_objects(
    context: State<'_, AppContext>,
    instance_id: u64,
) -> Result<Vec<crate::models::app_state::SharedStorageObjectEntry>, FrontendError> {
    InstanceLifecycleService::list_shared_storage_objects(context.inner(), instance_id)
        .await
        .map_err(Into::into)
}

#[tauri::command]
pub async fn sync_instance_from_shared_storage_selected(
    context: State<'_, AppContext>,
    instance_id: u64,
    payload: crate::models::app_state::SharedStorageSyncSelectionRequest,
) -> Result<String, FrontendError> {
    InstanceLifecycleService::sync_instance_from_shared_storage_selected(
        context.inner(),
        instance_id,
        payload.selected_paths,
    )
    .await
    .map_err(Into::into)
}

#[tauri::command]
pub async fn list_instance_exportable_storage_objects(
    context: State<'_, AppContext>,
    instance_id: u64,
) -> Result<Vec<crate::models::app_state::SharedStorageObjectEntry>, FrontendError> {
    InstanceLifecycleService::list_instance_exportable_objects(context.inner(), instance_id)
        .await
        .map_err(Into::into)
}

#[tauri::command]
pub async fn save_instance_to_shared_storage_selected(
    context: State<'_, AppContext>,
    instance_id: u64,
    payload: crate::models::app_state::SharedStorageSyncSelectionRequest,
) -> Result<String, FrontendError> {
    InstanceLifecycleService::save_instance_to_shared_storage_selected(
        context.inner(),
        instance_id,
        payload.selected_paths,
    )
    .await
    .map_err(Into::into)
}

#[tauri::command]
pub async fn get_instance_backup_status(
    context: State<'_, AppContext>,
) -> Result<SharedStorageInstanceStatus, FrontendError> {
    let instance_id = {
        let state = context.state.read().await;
        state
            .instance
            .instance_id
            .ok_or_else(|| AppError::InvalidInput("No active instance.".to_string()))?
    };
    SharedStorageManager::get_instance_backup_status(context.inner(), instance_id)
        .await
        .map_err(Into::into)
}

#[tauri::command]
pub async fn setup_instance_backup_schedule(
    context: State<'_, AppContext>,
) -> Result<String, FrontendError> {
    let remote = build_remote_exec_from_state(context.inner()).await?;
    let target_user = context.config.audio_target_user.clone();
    let instance_id = {
        let state = context.state.read().await;
        state.instance.instance_id.ok_or_else(|| {
            AppError::InvalidInput("No active instance. Start provisioning first.".to_string())
        })?
    };
    SharedStorageManager::setup_scheduled_backup(
        context.inner(),
        &remote,
        instance_id,
        &target_user,
    )
    .await?;
    Ok("Scheduled backups are disabled".to_string())
}

#[tauri::command]
pub async fn remove_instance_backup_schedule(
    context: State<'_, AppContext>,
) -> Result<String, FrontendError> {
    let remote = build_remote_exec_from_state(context.inner()).await?;
    let target_user = context.config.audio_target_user.clone();
    SharedStorageManager::remove_scheduled_backup(&remote, &target_user).await?;
    Ok("Scheduled backups are disabled".to_string())
}

#[tauri::command]
pub async fn get_instance_sunshine_settings(
    context: State<'_, AppContext>,
    instance_id: u64,
    sunshine_username: String,
    sunshine_password: String,
) -> Result<crate::services::instance_lifecycle::SunshineSettingsResponse, FrontendError> {
    InstanceLifecycleService::get_sunshine_settings(
        context.inner(),
        instance_id,
        &sunshine_username,
        &sunshine_password,
    )
    .await
    .map_err(Into::into)
}

#[tauri::command]
pub async fn update_instance_sunshine_settings(
    context: State<'_, AppContext>,
    instance_id: u64,
    settings: std::collections::HashMap<String, serde_json::Value>,
    sunshine_username: String,
    sunshine_password: String,
) -> Result<(), FrontendError> {
    InstanceLifecycleService::update_sunshine_settings(
        context.inner(),
        instance_id,
        crate::services::instance_lifecycle::SunshineSettingsUpdatePayload { settings },
        &sunshine_username,
        &sunshine_password,
    )
    .await
    .map_err(Into::into)
}

#[tauri::command]
pub async fn reset_instance_sunshine_settings(
    context: State<'_, AppContext>,
    instance_id: u64,
    sunshine_username: String,
    sunshine_password: String,
) -> Result<(), FrontendError> {
    InstanceLifecycleService::reset_sunshine_settings(
        context.inner(),
        instance_id,
        &sunshine_username,
        &sunshine_password,
    )
    .await
    .map_err(Into::into)
}

#[tauri::command]
pub async fn reconnect_instance_wireguard(
    _context: State<'_, AppContext>,
    _instance_id: u64,
) -> Result<String, FrontendError> {
    open_wireguard_app().map_err(FrontendError::from)?;
    Ok("Opened WireGuard app.".to_string())
}

#[tauri::command]
pub async fn pause_instance(
    context: State<'_, AppContext>,
    instance_id: u64,
) -> Result<(), FrontendError> {
    InstanceLifecycleService::pause_instance(context.inner(), instance_id)
        .await
        .map_err(Into::into)
}

#[tauri::command]
pub async fn destroy_instance(
    context: State<'_, AppContext>,
    instance_id: u64,
) -> Result<(), FrontendError> {
    InstanceLifecycleService::destroy_instance(context.inner(), instance_id)
        .await
        .map_err(Into::into)
}

#[tauri::command]
pub async fn reboot_instance_services(
    context: State<'_, AppContext>,
    instance_id: u64,
) -> Result<String, FrontendError> {
    let remote = build_remote_exec_for_instance(context.inner(), instance_id).await?;
    let target_user = context.config.audio_target_user.clone();
    RebootHelperService::reboot_and_reinitialize(&remote, &target_user)
        .await
        .map_err(Into::into)
}

#[tauri::command]
pub async fn generate_bundle_index(context: State<'_, AppContext>) -> Result<(), FrontendError> {
    let remote = build_remote_exec_from_state(context.inner()).await?;
    let target_user = context.config.audio_target_user.clone();
    let instance_id = {
        let state = context.state.read().await;
        state.instance.instance_id.ok_or_else(|| {
            AppError::InvalidInput("No active instance. Start provisioning first.".to_string())
        })?
    };
    BundleIndexer::generate_and_upload(context.inner(), &remote, instance_id, &target_user)
        .await
        .map_err(Into::into)
}

#[tauri::command]
pub async fn get_instance_restore_bundles(
    context: State<'_, AppContext>,
    instance_id: u64,
) -> Result<BundleIndex, FrontendError> {
    let remote = build_remote_exec_from_state(context.inner()).await?;
    let target_user = context.config.audio_target_user.clone();
    BundleRestoreService::list_bundles(context.inner(), &remote, instance_id, &target_user)
        .await
        .map_err(Into::into)
}

#[tauri::command]
pub async fn dry_run_restore(
    context: State<'_, AppContext>,
    instance_id: u64,
    payload: RestoreRequest,
) -> Result<RestoreDryRunResult, FrontendError> {
    let remote = build_remote_exec_from_state(context.inner()).await?;
    let target_user = context.config.audio_target_user.clone();
    BundleRestoreService::dry_run_restore(
        context.inner(),
        &remote,
        instance_id,
        &target_user,
        payload,
    )
    .await
    .map_err(Into::into)
}

#[tauri::command]
pub async fn restore_bundle(
    context: State<'_, AppContext>,
    instance_id: u64,
    payload: RestoreRequest,
) -> Result<RestoreJob, FrontendError> {
    let remote = build_remote_exec_from_state(context.inner()).await?;
    let target_user = context.config.audio_target_user.clone();
    BundleRestoreService::restore_bundle(
        context.inner(),
        &remote,
        instance_id,
        &target_user,
        payload,
    )
    .await
    .map_err(Into::into)
}

#[tauri::command]
pub async fn get_restore_job(job_id: String) -> Result<RestoreJob, FrontendError> {
    BundleRestoreService::get_job(&job_id)
        .await
        .map_err(Into::into)
}

#[tauri::command]
pub async fn get_instance_mic_config(
    context: State<'_, AppContext>,
    instance_id: u64,
) -> Result<InstanceMicConfig, FrontendError> {
    MicPassthroughService::get_config(context.inner(), instance_id)
        .await
        .map_err(Into::into)
}

#[tauri::command]
pub async fn update_instance_mic_settings(
    context: State<'_, AppContext>,
    instance_id: u64,
    payload: MicSettingsUpdate,
) -> Result<InstanceMicConfig, FrontendError> {
    MicPassthroughService::update_settings(context.inner(), instance_id, payload.quality_profile)
        .await
        .map_err(Into::into)
}

#[tauri::command]
pub async fn enable_instance_mic(
    context: State<'_, AppContext>,
    instance_id: u64,
    quality_profile: Option<MicQualityProfile>,
) -> Result<MicSessionResponse, FrontendError> {
    MicPassthroughService::enable(context.inner(), instance_id, quality_profile)
        .await
        .map_err(Into::into)
}

#[tauri::command]
pub async fn disable_instance_mic(
    context: State<'_, AppContext>,
    instance_id: u64,
) -> Result<(), FrontendError> {
    MicPassthroughService::disable(context.inner(), instance_id)
        .await
        .map_err(Into::into)
}

#[tauri::command]
pub async fn reconnect_instance_mic(
    context: State<'_, AppContext>,
    instance_id: u64,
) -> Result<MicSessionResponse, FrontendError> {
    MicPassthroughService::reconnect(context.inner(), instance_id)
        .await
        .map_err(Into::into)
}

#[tauri::command]
pub async fn recreate_instance_mic_device(
    context: State<'_, AppContext>,
    instance_id: u64,
) -> Result<(), FrontendError> {
    MicPassthroughService::recreate_device(context.inner(), instance_id)
        .await
        .map_err(Into::into)
}

#[tauri::command]
pub async fn get_instance_mic_status(
    context: State<'_, AppContext>,
    instance_id: u64,
) -> Result<InstanceMicRuntimeStatus, FrontendError> {
    MicPassthroughService::get_status(context.inner(), instance_id)
        .await
        .map_err(Into::into)
}

#[tauri::command]
pub async fn moonlight_get_configuration(
    moonlight: State<'_, MoonlightManager>,
) -> Result<MoonlightConfiguration, FrontendError> {
    crate::moonlight::infrastructure::persistence::MoonlightStateRepository::snapshot(
        moonlight.repository.as_ref(),
    )
    .map_err(moonlight_frontend_error)
}

#[tauri::command]
pub async fn moonlight_register_host(
    moonlight: State<'_, MoonlightManager>,
    input: MoonlightRegisterHostInput,
) -> Result<crate::moonlight::domain::PersistedHost, FrontendError> {
    hosts::register_host(
        moonlight.repository.as_ref(),
        RegisterHostRequest {
            host_id: input.host_id,
            display_name: input.display_name,
            addresses: HostAddresses {
                overlay: input.overlay_address,
                lan: input.lan_address,
                external: input.external_address,
            },
            ports: HostPorts {
                http: input.http_port,
                https: input.https_port,
            },
            explicit_address_type: input.explicit_address_type,
        },
    )
    .await
    .map_err(moonlight_frontend_error)
}

#[tauri::command]
pub async fn moonlight_refresh_host(
    moonlight: State<'_, MoonlightManager>,
    host_id: String,
) -> Result<crate::moonlight::application::hosts::HostStatus, FrontendError> {
    let client = ReqwestGameStreamHttpClient::new().map_err(moonlight_frontend_error)?;
    hosts::refresh_host(moonlight.repository.as_ref(), &client, &host_id)
        .await
        .map_err(moonlight_frontend_error)
}

#[tauri::command]
pub async fn moonlight_begin_pairing(
    moonlight: State<'_, MoonlightManager>,
    host_id: String,
) -> Result<MoonlightPairingSessionResponse, FrontendError> {
    let session = pairing::begin_pairing(
        moonlight.repository.as_ref(),
        &moonlight.pairing_sessions,
        &host_id,
    )
    .await
    .map_err(moonlight_frontend_error)?;

    Ok(MoonlightPairingSessionResponse {
        session_id: session.id.0,
        host_id: session.host_id,
        pin: session.pin,
        expires_in_seconds: 300,
    })
}

#[tauri::command]
pub async fn moonlight_complete_pairing(
    moonlight: State<'_, MoonlightManager>,
    input: MoonlightCompletePairingInput,
) -> Result<crate::moonlight::application::pairing::PairingResult, FrontendError> {
    pairing::complete_pairing(
        moonlight.repository.as_ref(),
        moonlight.secret_store.as_ref(),
        &moonlight.pairing_sessions,
        &PairingSessionId(input.session_id),
    )
    .await
    .map_err(moonlight_frontend_error)
}

#[tauri::command]
pub async fn moonlight_list_apps(
    moonlight: State<'_, MoonlightManager>,
    input: MoonlightListAppsInput,
) -> Result<Vec<crate::moonlight::domain::RemoteApp>, FrontendError> {
    let client = ReqwestGameStreamHttpClient::new().map_err(moonlight_frontend_error)?;
    let refresh_policy = if input.force_refresh {
        RefreshPolicy::ForceRefresh
    } else {
        RefreshPolicy::UseCacheIfFresh
    };
    apps::list_remote_apps(
        moonlight.repository.as_ref(),
        &client,
        &input.host_id,
        refresh_policy,
    )
    .await
    .map_err(moonlight_frontend_error)
}

#[tauri::command]
pub async fn moonlight_start_stream(
    app: AppHandle,
    moonlight: State<'_, MoonlightManager>,
    input: MoonlightStartStreamInput,
) -> Result<MoonlightStartStreamResponse, FrontendError> {
    let client = ReqwestGameStreamHttpClient::new().map_err(moonlight_frontend_error)?;
    let prepared = moonlight_launch::start_stream_request(
        moonlight.repository.as_ref(),
        &client,
        &input.host_id,
        input.app_id,
        input.session_preferences.as_ref(),
        input.replace_existing,
    )
    .await
    .map_err(moonlight_frontend_error)?;

    let stream_window = create_or_reuse_stream_window(
        &app,
        prepared.preferences.video.width,
        prepared.preferences.video.height,
    )
    .map_err(moonlight_frontend_error)?;
    let surface =
        stream_window_surface_descriptor(&stream_window).map_err(moonlight_frontend_error)?;

    if let Err(error) = moonlight.runtime.attach_surface(surface).await {
        let _ = close_stream_window(&app);
        return Err(moonlight_frontend_error(error));
    }

    if let Err(error) = moonlight
        .runtime
        .start(NativeStartRequest {
            host_id: input.host_id.clone(),
            app_id: input.app_id,
            host_address: prepared.host_address.clone(),
            app_version: prepared.app_version.clone(),
            gfe_version: prepared.gfe_version.clone(),
            session_url: prepared.launch_result.rtsp_session_url.clone(),
            server_codec_mode_support: prepared.server_codec_mode_support,
            preferences: prepared.preferences.clone(),
            supported_video_formats: prepared.supported_video_formats,
            remote_input_key: prepared.remote_input_key,
            remote_input_iv: prepared.remote_input_iv,
        })
        .await
    {
        let _ = moonlight.runtime.detach_surface().await;
        let _ = close_stream_window(&app);
        return Err(moonlight_frontend_error(error));
    }

    stream_window.show().map_err(|error| FrontendError {
        code: "moonlight_error".to_string(),
        message: "Moonlight operation failed".to_string(),
        details: Some(error.to_string()),
        retryable: false,
    })?;
    stream_window
        .set_fullscreen(true)
        .map_err(|error| FrontendError {
            code: "moonlight_error".to_string(),
            message: "Moonlight operation failed".to_string(),
            details: Some(error.to_string()),
            retryable: false,
        })?;

    let state = moonlight
        .runtime
        .get_state()
        .await
        .map_err(moonlight_frontend_error)?;
    Ok(MoonlightStartStreamResponse {
        operation: match prepared.launch_result.operation {
            crate::moonlight::domain::LaunchOperation::Launch => "launch",
            crate::moonlight::domain::LaunchOperation::Resume => "resume",
        }
        .to_string(),
        state: session_state_name(&state),
        has_session_url: prepared.launch_result.rtsp_session_url.is_some(),
    })
}

#[tauri::command]
pub async fn moonlight_disconnect_stream(
    app: AppHandle,
    moonlight: State<'_, MoonlightManager>,
) -> Result<MoonlightSessionStateResponse, FrontendError> {
    moonlight
        .runtime
        .stop()
        .await
        .map_err(moonlight_frontend_error)?;
    moonlight
        .runtime
        .detach_surface()
        .await
        .map_err(moonlight_frontend_error)?;
    close_stream_window(&app).map_err(moonlight_frontend_error)?;
    let state = moonlight
        .runtime
        .get_state()
        .await
        .map_err(moonlight_frontend_error)?;
    Ok(MoonlightSessionStateResponse {
        state: session_state_name(&state),
    })
}

#[tauri::command]
pub async fn moonlight_quit_remote_app(
    moonlight: State<'_, MoonlightManager>,
    host_id: String,
) -> Result<(), FrontendError> {
    let client = ReqwestGameStreamHttpClient::new().map_err(moonlight_frontend_error)?;
    moonlight_launch::quit_remote_app(moonlight.repository.as_ref(), &client, &host_id)
        .await
        .map_err(moonlight_frontend_error)
}

#[tauri::command]
pub async fn moonlight_send_relative_mouse(
    moonlight: State<'_, MoonlightManager>,
    input: MoonlightRelativeMouseInput,
) -> Result<(), FrontendError> {
    moonlight
        .runtime
        .send_relative_mouse(input.delta_x, input.delta_y)
        .await
        .map_err(moonlight_frontend_error)
}

#[tauri::command]
pub async fn moonlight_send_absolute_mouse(
    moonlight: State<'_, MoonlightManager>,
    input: MoonlightAbsoluteMouseInput,
) -> Result<(), FrontendError> {
    moonlight
        .runtime
        .send_absolute_mouse(
            input.x,
            input.y,
            input.reference_width,
            input.reference_height,
        )
        .await
        .map_err(moonlight_frontend_error)
}

#[tauri::command]
pub async fn moonlight_send_mouse_button(
    moonlight: State<'_, MoonlightManager>,
    input: MoonlightMouseButtonInput,
) -> Result<(), FrontendError> {
    moonlight
        .runtime
        .send_mouse_button(input.button, input.pressed)
        .await
        .map_err(moonlight_frontend_error)
}

#[tauri::command]
pub async fn moonlight_send_keyboard(
    moonlight: State<'_, MoonlightManager>,
    input: MoonlightKeyboardInput,
) -> Result<(), FrontendError> {
    moonlight
        .runtime
        .send_keyboard(input.virtual_key, input.pressed, input.modifiers)
        .await
        .map_err(moonlight_frontend_error)
}

#[tauri::command]
pub async fn moonlight_send_controller_arrival(
    moonlight: State<'_, MoonlightManager>,
    input: MoonlightControllerArrivalInput,
) -> Result<(), FrontendError> {
    moonlight
        .runtime
        .send_controller_arrival(
            input.controller_number,
            input.active_gamepad_mask,
            input.controller_type,
            input.supported_button_flags,
            input.capabilities,
        )
        .await
        .map_err(moonlight_frontend_error)
}

#[tauri::command]
pub async fn moonlight_send_controller_state(
    moonlight: State<'_, MoonlightManager>,
    input: MoonlightControllerStateInput,
) -> Result<(), FrontendError> {
    moonlight
        .runtime
        .send_controller_state(
            input.controller_number,
            input.active_gamepad_mask,
            input.button_flags,
            input.left_trigger,
            input.right_trigger,
            input.left_stick_x,
            input.left_stick_y,
            input.right_stick_x,
            input.right_stick_y,
        )
        .await
        .map_err(moonlight_frontend_error)
}

#[tauri::command]
pub async fn moonlight_update_preferences(
    moonlight: State<'_, MoonlightManager>,
    defaults: StreamPreferences,
) -> Result<MoonlightConfiguration, FrontendError> {
    crate::moonlight::infrastructure::persistence::MoonlightStateRepository::update(
        moonlight.repository.as_ref(),
        |configuration| {
            configuration.defaults = defaults;
            Ok(configuration.clone())
        },
    )
    .map_err(moonlight_frontend_error)
}

#[tauri::command]
pub async fn moonlight_forget_host(
    moonlight: State<'_, MoonlightManager>,
    host_id: String,
) -> Result<MoonlightConfiguration, FrontendError> {
    crate::moonlight::infrastructure::persistence::MoonlightStateRepository::update(
        moonlight.repository.as_ref(),
        |configuration| {
            configuration.hosts.remove(&host_id);
            if configuration.last_selected_host_id.as_deref() == Some(host_id.as_str()) {
                configuration.last_selected_host_id = None;
            }
            Ok(configuration.clone())
        },
    )
    .map_err(moonlight_frontend_error)
}

#[tauri::command]
pub async fn moonlight_get_session_state(
    moonlight: State<'_, MoonlightManager>,
) -> Result<MoonlightSessionStateResponse, FrontendError> {
    let state = moonlight
        .runtime
        .get_state()
        .await
        .map_err(moonlight_frontend_error)?;
    Ok(MoonlightSessionStateResponse {
        state: session_state_name(&state),
    })
}
