pub mod launch_library;
pub mod shared_storage;

pub use self::launch_library::{
    get_instance_launch_library, get_launch_instance_software_job, get_software_artwork,
    launch_instance_software, update_igdb_credentials,
};
pub use self::shared_storage::{
    begin_oauth_authorization, complete_oauth_authorization, disconnect_shared_storage_profile,
    get_shared_storage_profiles, list_storage_providers, save_static_provider_credentials,
    set_active_shared_storage_profile, test_shared_storage_connection,
};

use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tauri::{AppHandle, Manager, State};

use crate::{
    errors::{AppError, FrontendError},
    input::{
        event::{ButtonState, MouseButton},
        state::MouseMode as CaptureMouseMode,
    },
    mic_client::device_list::MicrophoneDevice,
    models::{
        app_state::{
            BackupStatusResponse, ConnectionProvider, EdidMode, InstanceMicConfig,
            InstanceMicRuntimeStatus, LocationSource, ManualLocationInput, MicQualityProfile,
            MicSessionResponse, MicSettingsUpdate, MoonlightPreferences, OfferCandidate,
            OnboardingPayload, OrchestrationState, PersistedAppState, PostWireGuardSetupState,
            RentedInstanceSummary, ServerPreferences, ServerPreferencesUpdate, SetupStage,
            SharedStorageInstanceStatus, SharedStorageSettingsResponse,
            SharedStorageSettingsUpdate,
        },
        events::ProvisioningEvent,
        vast::VastInstance,
    },
    moonlight::{
        application::{
            apps::{self, RefreshPolicy},
            bootstrap::bootstrap_client_identity,
            hosts::{self, RegisterHostRequest},
            launch as moonlight_launch,
            pairing::{self, PairingSessionId},
        },
        composition::MoonlightManager,
        domain::{
            validate_latency_config, AddressType, HostAddresses, HostPorts, MoonlightConfiguration,
            NolandLatencyConfigPatch, PairingStatus, SessionState, StreamPreferences,
            StreamPreferencesPatch,
        },
        infrastructure::gamestream::ReqwestGameStreamHttpClient,
        platform::{
            activate_native_stream_input, close_stream_window, create_or_reuse_stream_window,
            deactivate_native_stream_input, install_native_stream_input,
            set_native_stream_input_debug_overlay_enabled, stream_window_surface_descriptor,
        },
        runtime::NativeStartRequest,
    },
    services::{
        app_context::AppContext,
        diagnostics::{write_diagnostic_report, DiagnosticReportResponse},
        display_profile::{
            build_display_profile, DisplayModeSpec, DisplayProfile, DisplayProfileSource,
        },
        health_check::{run_system_health_report, SystemHealthReport},
        instance_lifecycle::InstanceLifecycleService,
        location::LocationService,
        mic_passthrough::MicPassthroughService,
        moonlight::detect_client_display_for_provisioning,
        offer_selector::OfferSelector,
        orchestration::OrchestrationService,
        os_detection::OsDetection,
        post_wireguard_setup::{
            authorize_sunshine_pin, get_setup_status, retry_setup_stage, setup_moonlight_sunshine,
            setup_wireguard_app_handoff, verify_sunshine_api, verify_wireguard_connection,
            ReachabilityResult, SunshineVerificationResult,
        },
        reboot_helper::RebootHelperService,
        remote_display::{ApplyDisplayModeResult, InstanceDisplayStatus, RemoteDisplayService},
        remote_exec::RemoteExec,
        shared_storage::{
            agent_runtime::{call_agent_raw, ensure_state_agent},
            shared_storage_manager::SharedStorageManager,
        },
        sleep_inhibit::SleepInhibitService,
        ssh_keys::SshKeyService,
        sunshine::{generate_headless_edid_base64, EDID_MAX_REFRESH_HZ, EDID_MIN_REFRESH_HZ},
        vast_api::VastApiClient,
        wireguard::{
            locate_noland_net_helper_binary, locate_wintun_library,
            reconnect_local_wireguard_client, setup_local_wireguard_client,
            teardown_local_wireguard_client, verify_managed_gotatun_tunnel, WireGuardProvisionMode,
            WireGuardService,
        },
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

#[derive(Debug, Default)]
struct RemoteInstanceServiceHealth {
    pub sunshine_active: bool,
    pub wireguard_active: bool,
    pub wireguard_addresses: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VastWalletSummary {
    pub available: bool,
    pub balance_usd: Option<f64>,
    pub display_amount: String,
    pub source: String,
    pub last_updated_at: Option<String>,
}

fn unavailable_vast_wallet_summary() -> VastWalletSummary {
    VastWalletSummary {
        available: false,
        balance_usd: None,
        display_amount: "--".to_string(),
        source: "unavailable".to_string(),
        last_updated_at: None,
    }
}

fn build_vast_wallet_summary(balance_usd: Option<f64>) -> VastWalletSummary {
    VastWalletSummary {
        available: balance_usd.is_some(),
        balance_usd,
        display_amount: balance_usd
            .map(|value| format!("${value:.2}"))
            .unwrap_or_else(|| "--".to_string()),
        source: if balance_usd.is_some() {
            "vast_api".to_string()
        } else {
            "unavailable".to_string()
        },
        last_updated_at: Some(
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .ok()
                .map(|duration| duration.as_secs().to_string())
                .unwrap_or_default(),
        ),
    }
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
pub struct MoonlightActiveInputModeResponse {
    pub mouse_mode: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MoonlightStartStreamResponse {
    pub operation: String,
    pub state: String,
    pub has_session_url: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MoonlightInputDebugStateResponse {
    pub capture_active: bool,
    pub capture_mode: i32,
    pub capture_requests: u64,
    pub native_mouse_moves: u64,
    pub native_mouse_downs: u64,
    pub native_mouse_ups: u64,
    pub native_keys: u64,
    pub rust_relative_callbacks: u64,
    pub rust_absolute_callbacks: u64,
    pub rust_button_callbacks: u64,
    pub rust_key_callbacks: u64,
    pub relative_send_attempts: u64,
    pub absolute_send_attempts: u64,
    pub button_send_attempts: u64,
    pub key_send_attempts: u64,
    pub scroll_send_attempts: u64,
    pub send_errors: u64,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EmbeddedMoonlightInstanceStatus {
    pub instance_id: u64,
    pub enabled: bool,
    pub host_id: String,
    pub paired: bool,
    pub host_address: String,
    pub session_state: String,
    pub last_error: Option<String>,
    pub runtime_connected: bool,
    pub renderer_ready: bool,
    pub video_session_active: bool,
    pub video_frame_count: u64,
    pub renderer_submitted_frame_count: u64,
    pub renderer_dropped_frame_count: u64,
    pub audio_sample_count: u64,
    pub last_runtime_event: Option<String>,
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
pub struct MoonlightStartInputCaptureInput {
    pub mode: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MoonlightVideoGeometryInput {
    pub left: f64,
    pub top: f64,
    pub width: f64,
    pub height: f64,
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

fn local_environment_check(_attempt_install: bool) -> LocalEnvironmentCheck {
    let os = OsDetection::new();
    let bundled_check = |tool: &str, required_for: &str, resolved_path: Option<PathBuf>| {
        ToolCheck {
            tool: tool.to_string(),
            found: resolved_path.is_some(),
            path: resolved_path.map(|path| path.display().to_string()),
            required_for: required_for.to_string(),
            install_hint: "This component is bundled with Noland Connect. Reinstall the app or report the package as incomplete; do not install it manually.".to_string(),
            install_attempted: false,
            install_error: None,
        }
    };

    let managed_ssh =
        os.locate_app_managed_binary("ssh", "NOLAND_SSH_BIN", cfg!(target_os = "windows"));
    let managed_ssh_keygen = os.locate_app_managed_binary(
        "ssh-keygen",
        "NOLAND_SSH_KEYGEN_BIN",
        cfg!(target_os = "windows"),
    );

    let mut checks = vec![
        bundled_check("ssh", "remote commands and provisioning", managed_ssh),
        bundled_check("ssh-keygen", "SSH key generation", managed_ssh_keygen),
    ];

    checks.push(bundled_check(
        "noland-net-helper",
        "embedded GotaTun tunnel engine and network configuration",
        locate_noland_net_helper_binary(),
    ));
    if os.is_windows() {
        checks.push(bundled_check(
            "wintun.dll",
            "Windows virtual network adapter for the embedded GotaTun engine",
            locate_wintun_library(),
        ));
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

#[tauri::command]
pub async fn system_health_check(
    app: AppHandle,
    context: State<'_, AppContext>,
) -> Result<SystemHealthReport, FrontendError> {
    Ok(run_system_health_report(&app, context.inner()).await)
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DiagnosticReportInput {
    pub reason: Option<String>,
    pub frontend_error: Option<String>,
}

#[tauri::command]
pub async fn export_diagnostic_report(
    app: AppHandle,
    context: State<'_, AppContext>,
    input: Option<DiagnosticReportInput>,
) -> Result<DiagnosticReportResponse, FrontendError> {
    let input = input.unwrap_or(DiagnosticReportInput {
        reason: None,
        frontend_error: None,
    });
    write_diagnostic_report(
        &app,
        context.inner(),
        input.reason,
        input.frontend_error,
        None,
    )
    .await
    .map_err(Into::into)
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

fn session_state_name(state: &SessionState) -> &'static str {
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
}

async fn stop_active_stream_if_needed(
    app: &AppHandle,
    moonlight: &MoonlightManager,
) -> Result<(), FrontendError> {
    let state = moonlight
        .runtime
        .get_state()
        .await
        .map_err(moonlight_frontend_error)?;

    if matches!(state, SessionState::Idle) {
        return Ok(());
    }

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
    moonlight.input.end_capture();
    if let Ok(mut active_preferences) = moonlight.active_session_preferences.lock() {
        *active_preferences = None;
    }
    close_stream_window(app).map_err(moonlight_frontend_error)?;
    Ok(())
}

fn parse_capture_mouse_mode(mode: &str) -> Result<CaptureMouseMode, FrontendError> {
    match mode {
        "relative" => Ok(CaptureMouseMode::Relative),
        "absolute" => Ok(CaptureMouseMode::Absolute),
        _ => Err(FrontendError {
            code: "moonlight_error".to_string(),
            message: "Moonlight operation failed".to_string(),
            details: Some(format!("unsupported capture mode: {mode}")),
            retryable: false,
        }),
    }
}

fn map_mouse_button(button: u8) -> Option<MouseButton> {
    match button {
        0x01 => Some(MouseButton::Left),
        0x02 => Some(MouseButton::Middle),
        0x03 => Some(MouseButton::Right),
        0x04 => Some(MouseButton::X1),
        0x05 => Some(MouseButton::X2),
        _ => None,
    }
}

fn button_state(pressed: bool) -> ButtonState {
    if pressed {
        ButtonState::Pressed
    } else {
        ButtonState::Released
    }
}

fn embedded_moonlight_host_id(instance_id: u64) -> String {
    format!("instance-{instance_id}")
}

fn parse_instance_id_from_embedded_host_id(host_id: &str) -> Option<u64> {
    host_id.strip_prefix("instance-")?.parse::<u64>().ok()
}

fn resolve_instance_id_for_embedded_host(state: &PersistedAppState, host_id: &str) -> Option<u64> {
    state
        .provisioned_servers
        .iter()
        .find(|server| {
            let candidate = if server.embedded_moonlight_host_id.trim().is_empty() {
                embedded_moonlight_host_id(server.instance_id)
            } else {
                server.embedded_moonlight_host_id.clone()
            };
            candidate == host_id
        })
        .map(|server| server.instance_id)
        .or_else(|| parse_instance_id_from_embedded_host_id(host_id))
}

fn resolve_embedded_moonlight_host_address(
    state: &PersistedAppState,
    instance_id: u64,
) -> Option<String> {
    let server = state
        .provisioned_servers
        .iter()
        .find(|server| server.instance_id == instance_id)?;

    [
        Some(server.wireguard_server_ip.as_str()),
        Some(server.moonlight_host_address.as_str()),
        Some(state.post_wireguard_setup.moonlight_host.as_str()),
        Some(state.moonlight.host_address.as_str()),
    ]
    .into_iter()
    .flatten()
    .map(str::trim)
    .find(|value| !value.is_empty())
    .map(ToOwned::to_owned)
}

fn resolve_embedded_moonlight_host_ports(state: &PersistedAppState, instance_id: u64) -> HostPorts {
    let post_wireguard_matches_instance = state
        .post_wireguard_setup
        .current_instance_id
        .map(|current| current == instance_id)
        .unwrap_or(false);

    let mut http_port = 47989;
    let mut https_port = None;

    if post_wireguard_matches_instance {
        let reachable = &state.post_wireguard_setup.wireguard_reachable_ports;
        if reachable.contains(&47989) {
            http_port = 47989;
        }
        if reachable.contains(&47984) {
            https_port = Some(47984);
        } else if reachable.contains(&47990) {
            https_port = Some(47990);
        }
    }

    HostPorts {
        http: http_port,
        https: https_port,
    }
}

fn is_missing_embedded_identity_private_key_error(
    error: &crate::moonlight::domain::MoonlightError,
) -> bool {
    matches!(error, crate::moonlight::domain::MoonlightError::IdentityInvalid(message)
        if message.contains("private key is missing for the persisted Moonlight identity"))
}

fn is_missing_embedded_host_error(
    error: &crate::moonlight::domain::MoonlightError,
    host_id: &str,
) -> bool {
    matches!(error, crate::moonlight::domain::MoonlightError::Validation(message)
        if message.contains(&format!("host {host_id} not found")))
}

async fn repair_embedded_identity_after_missing_private_key(
    context: &AppContext,
    moonlight: &MoonlightManager,
) -> Result<(), crate::moonlight::domain::MoonlightError> {
    let previous_identity =
        crate::moonlight::infrastructure::persistence::MoonlightStateRepository::snapshot(
            moonlight.repository.as_ref(),
        )?
        .identity;

    if let Some(identity) = previous_identity.as_ref() {
        let _ = moonlight
            .secret_store
            .remove(&identity.private_key_ref)
            .await;
    }

    crate::moonlight::infrastructure::persistence::MoonlightStateRepository::update(
        moonlight.repository.as_ref(),
        |configuration| {
            configuration.identity = None;
            for host in configuration.hosts.values_mut() {
                host.pairing = None;
            }
            Ok(())
        },
    )?;

    let _ = bootstrap_client_identity(
        moonlight.repository.as_ref(),
        moonlight.secret_store.as_ref(),
    )
    .await?;

    let _ = context
        .update_state(|state| {
            state.post_wireguard_setup.paired = false;
            state.post_wireguard_setup.setup_complete = false;
            for server in state.provisioned_servers.iter_mut() {
                server.embedded_moonlight_paired = false;
            }
        })
        .await;

    Ok(())
}

async fn ensure_embedded_identity_ready_for_explicit_action(
    context: &AppContext,
    moonlight: &MoonlightManager,
) -> Result<(), crate::moonlight::domain::MoonlightError> {
    match bootstrap_client_identity(
        moonlight.repository.as_ref(),
        moonlight.secret_store.as_ref(),
    )
    .await
    {
        Ok(_) => Ok(()),
        Err(error) if is_missing_embedded_identity_private_key_error(&error) => {
            warn!(
                "Embedded Moonlight identity private key is missing locally; resetting embedded identity and stale pairing state before continuing"
            );
            repair_embedded_identity_after_missing_private_key(context, moonlight).await
        }
        Err(error) => Err(error),
    }
}

fn embedded_host_is_paired(
    repository: &crate::moonlight::infrastructure::persistence::JsonMoonlightStateRepository,
    host_id: &str,
) -> bool {
    crate::moonlight::infrastructure::persistence::MoonlightStateRepository::get_host(
        repository, host_id,
    )
    .ok()
    .and_then(|host| host.pairing)
    .map(|pairing| matches!(pairing.status, PairingStatus::Paired))
    .unwrap_or(false)
}

async fn ensure_embedded_moonlight_host(
    repository: &crate::moonlight::infrastructure::persistence::JsonMoonlightStateRepository,
    state: &PersistedAppState,
    instance_id: u64,
) -> Result<String, crate::moonlight::domain::MoonlightError> {
    let host_id = embedded_moonlight_host_id(instance_id);
    let host_address =
        resolve_embedded_moonlight_host_address(state, instance_id).ok_or_else(|| {
            crate::moonlight::domain::MoonlightError::Validation(format!(
                "instance {instance_id} has no Moonlight host address"
            ))
        })?;
    let ports = resolve_embedded_moonlight_host_ports(state, instance_id);

    info!(
        instance_id,
        host_id = %host_id,
        host_address = %host_address,
        http_port = ports.http,
        https_port = ?ports.https,
        "Ensuring embedded Moonlight host"
    );

    if crate::moonlight::infrastructure::persistence::MoonlightStateRepository::get_host(
        repository, &host_id,
    )
    .is_ok()
    {
        crate::moonlight::infrastructure::persistence::MoonlightStateRepository::update(
            repository,
            |configuration| {
                let host = configuration.hosts.get_mut(&host_id).ok_or_else(|| {
                    crate::moonlight::domain::MoonlightError::Validation(format!(
                        "host {host_id} not found"
                    ))
                })?;
                host.display_name = format!("Instance {instance_id}");
                host.addresses.overlay = Some(host_address.clone());
                host.active_address_type = AddressType::Overlay;
                host.ports = ports.clone();
                Ok(())
            },
        )?;
        return Ok(host_id);
    }

    hosts::register_host(
        repository,
        RegisterHostRequest {
            host_id: host_id.clone(),
            display_name: format!("Instance {instance_id}"),
            addresses: HostAddresses {
                overlay: Some(host_address),
                lan: None,
                external: None,
            },
            ports,
            explicit_address_type: Some(AddressType::Overlay),
        },
    )
    .await?;

    Ok(host_id)
}

fn preferred_embedded_app(
    apps: &[crate::moonlight::domain::RemoteApp],
) -> Option<&crate::moonlight::domain::RemoteApp> {
    apps.iter()
        .find(|app| app.name.eq_ignore_ascii_case("Computer"))
        .or_else(|| {
            apps.iter()
                .find(|app| app.name.eq_ignore_ascii_case("Desktop"))
        })
        .or_else(|| {
            apps.iter()
                .find(|app| app.name.to_ascii_lowercase().contains("computer"))
        })
        .or_else(|| {
            apps.iter()
                .find(|app| app.name.to_ascii_lowercase().contains("desktop"))
        })
        .or_else(|| {
            apps.iter()
                .find(|app| app.name.to_ascii_lowercase().contains("steam"))
        })
        .or_else(|| apps.first())
}

fn single_computer_app() -> Vec<crate::moonlight::domain::RemoteApp> {
    vec![crate::moonlight::domain::RemoteApp {
        id: 0,
        name: "Computer".to_string(),
        hdr_supported: false,
    }]
}

fn is_embedded_applist_not_found_error(error: &crate::moonlight::domain::MoonlightError) -> bool {
    matches!(error, crate::moonlight::domain::MoonlightError::Validation(message)
        if message.contains("/applist returned status_code=404"))
}

fn parse_sunshine_api_apps(
    value: Value,
) -> Result<Vec<crate::moonlight::domain::RemoteApp>, FrontendError> {
    let items = match value {
        Value::Array(items) => items,
        Value::Object(mut object) => {
            if let Some(Value::Array(items)) = object.remove("apps") {
                items
            } else if let Some(Value::Array(items)) = object.remove("applications") {
                items
            } else if let Some(Value::Array(items)) = object.remove("list") {
                items
            } else if object.contains_key("name") || object.contains_key("title") {
                vec![Value::Object(object)]
            } else {
                return Ok(single_computer_app());
            }
        }
        _ => return Ok(single_computer_app()),
    };

    let mut apps = Vec::new();
    for item in items {
        let Value::Object(object) = item else {
            continue;
        };

        let name = object
            .get("name")
            .and_then(Value::as_str)
            .or_else(|| object.get("title").and_then(Value::as_str))
            .unwrap_or_default()
            .trim()
            .to_string();

        if name.is_empty() {
            continue;
        }

        let app_id = object
            .get("index")
            .and_then(Value::as_i64)
            .or_else(|| object.get("id").and_then(Value::as_i64))
            .or_else(|| object.get("appId").and_then(Value::as_i64))
            .or_else(|| object.get("appid").and_then(Value::as_i64))
            .and_then(|value| if value >= 0 { Some(value as u32) } else { None })
            .or_else(|| {
                if name.eq_ignore_ascii_case("computer") || name.eq_ignore_ascii_case("desktop") {
                    Some(0)
                } else {
                    None
                }
            });

        let Some(id) = app_id else {
            continue;
        };

        apps.push(crate::moonlight::domain::RemoteApp {
            id,
            name,
            hdr_supported: false,
        });
    }

    if apps.is_empty() {
        Ok(single_computer_app())
    } else {
        Ok(apps)
    }
}

async fn fetch_embedded_apps_via_sunshine_api(
    context: &AppContext,
    moonlight: &MoonlightManager,
    host_id: &str,
    host_address: &str,
) -> Result<Vec<crate::moonlight::domain::RemoteApp>, FrontendError> {
    let state = context.load_state().await;
    let username = state.credentials.app_username.trim().to_string();
    let password = state.credentials.app_password.trim().to_string();
    if username.is_empty() || password.is_empty() {
        return Err(FrontendError {
            code: "moonlight_invalid_input".to_string(),
            message: "Sunshine credentials are missing".to_string(),
            details: Some("Embedded app discovery fallback requires the provisioned Sunshine username and password.".to_string()),
            retryable: false,
        });
    }

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(15))
        .danger_accept_invalid_certs(true)
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|error| FrontendError {
            code: "moonlight_error".to_string(),
            message: "Moonlight operation failed".to_string(),
            details: Some(format!("Failed building Sunshine apps client: {error}")),
            retryable: true,
        })?;

    let response = client
        .get(format!("https://{}:47990/api/apps", host_address))
        .basic_auth(username, Some(password))
        .send()
        .await
        .map_err(|error| FrontendError {
            code: "moonlight_error".to_string(),
            message: "Moonlight operation failed".to_string(),
            details: Some(format!(
                "Failed to fetch Sunshine apps via /api/apps: {error}"
            )),
            retryable: true,
        })?;

    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        return Err(FrontendError {
            code: "moonlight_error".to_string(),
            message: "Moonlight operation failed".to_string(),
            details: Some(format!("Sunshine /api/apps failed with {status}: {body}")),
            retryable: true,
        });
    }

    let payload = response
        .json::<Value>()
        .await
        .map_err(|error| FrontendError {
            code: "moonlight_error".to_string(),
            message: "Moonlight operation failed".to_string(),
            details: Some(format!("Invalid Sunshine /api/apps payload: {error}")),
            retryable: true,
        })?;

    let apps = parse_sunshine_api_apps(payload)?;
    crate::moonlight::infrastructure::persistence::MoonlightStateRepository::update(
        moonlight.repository.as_ref(),
        |configuration| {
            let host = configuration.hosts.get_mut(host_id).ok_or_else(|| {
                crate::moonlight::domain::MoonlightError::Validation(format!(
                    "host {host_id} not found"
                ))
            })?;
            host.apps_cache = Some(crate::moonlight::domain::AppsCache {
                fetched_at: chrono::Utc::now().to_rfc3339(),
                items: apps
                    .iter()
                    .map(|app| crate::moonlight::domain::CachedRemoteApp {
                        id: app.id,
                        name: app.name.clone(),
                        hdr_supported: app.hdr_supported,
                    })
                    .collect(),
            });
            Ok(())
        },
    )
    .map_err(moonlight_frontend_error)?;

    if apps.is_empty() {
        let apps = single_computer_app();
        crate::moonlight::infrastructure::persistence::MoonlightStateRepository::update(
            moonlight.repository.as_ref(),
            |configuration| {
                let host = configuration.hosts.get_mut(host_id).ok_or_else(|| {
                    crate::moonlight::domain::MoonlightError::Validation(format!(
                        "host {host_id} not found"
                    ))
                })?;
                host.apps_cache = Some(crate::moonlight::domain::AppsCache {
                    fetched_at: chrono::Utc::now().to_rfc3339(),
                    items: apps
                        .iter()
                        .map(|app| crate::moonlight::domain::CachedRemoteApp {
                            id: app.id,
                            name: app.name.clone(),
                            hdr_supported: app.hdr_supported,
                        })
                        .collect(),
                });
                Ok(())
            },
        )
        .map_err(moonlight_frontend_error)?;
        return Ok(apps);
    }

    Ok(apps)
}

fn is_retryable_embedded_pairing_error(error: &crate::moonlight::domain::MoonlightError) -> bool {
    match error {
        crate::moonlight::domain::MoonlightError::Persistence(message) => {
            message.contains("error sending request")
                || message.contains("operation timed out")
                || message.contains("timed out")
                || message.contains("connection reset")
                || message.contains("broken pipe")
                || message.contains("deadline has elapsed")
        }
        crate::moonlight::domain::MoonlightError::Validation(message) => {
            message.contains("server likely already pairing with another client")
                || message.contains("failed pairing at stage")
        }
        _ => false,
    }
}

#[cfg(test)]
mod sunshine_api_app_tests {
    use super::parse_sunshine_api_apps;
    use serde_json::json;

    #[test]
    fn falls_back_to_single_computer_when_payload_is_empty() {
        let apps = parse_sunshine_api_apps(json!({ "apps": [] })).expect("apps should parse");
        assert_eq!(apps.len(), 1);
        assert_eq!(apps[0].id, 0);
        assert_eq!(apps[0].name, "Computer");
    }

    #[test]
    fn accepts_alternate_id_fields() {
        let apps = parse_sunshine_api_apps(json!({
            "applications": [
                { "title": "Desktop", "appid": 7 }
            ]
        }))
        .expect("apps should parse");
        assert_eq!(apps.len(), 1);
        assert_eq!(apps[0].id, 7);
        assert_eq!(apps[0].name, "Desktop");
    }

    #[test]
    fn defaults_desktop_without_explicit_id_to_zero() {
        let apps = parse_sunshine_api_apps(json!({
            "name": "Desktop"
        }))
        .expect("apps should parse");
        assert_eq!(apps.len(), 1);
        assert_eq!(apps[0].id, 0);
        assert_eq!(apps[0].name, "Desktop");
    }
}

async fn auto_pair_embedded_host(
    context: &AppContext,
    moonlight: &MoonlightManager,
    host_id: &str,
) -> Result<(), crate::moonlight::domain::MoonlightError> {
    let host = crate::moonlight::infrastructure::persistence::MoonlightStateRepository::get_host(
        moonlight.repository.as_ref(),
        host_id,
    )?;
    let sunshine_host = host
        .addresses
        .overlay
        .clone()
        .or(host.addresses.lan.clone())
        .or(host.addresses.external.clone())
        .ok_or_else(|| {
            crate::moonlight::domain::MoonlightError::Validation(format!(
                "host {host_id} has no usable address"
            ))
        })?;
    let state_snapshot = context.load_state().await;
    let sunshine_username = state_snapshot.credentials.app_username.clone();
    let sunshine_password = state_snapshot.credentials.app_password.clone();

    let first_session = pairing::begin_pairing(
        moonlight.repository.as_ref(),
        &moonlight.pairing_sessions,
        host_id,
    )
    .await?;
    let first_pin = first_session.pin.clone();

    match pairing::complete_pairing_with_stage1_authorization(
        moonlight.repository.as_ref(),
        moonlight.secret_store.as_ref(),
        &moonlight.pairing_sessions,
        &first_session.id,
        || async {
            authorize_sunshine_pin(
                &sunshine_host,
                &sunshine_username,
                &sunshine_password,
                &first_pin,
                Some("Noland Connect"),
            )
            .await
            .map_err(|error| {
                crate::moonlight::domain::MoonlightError::Persistence(error.to_string())
            })
        },
    )
    .await
    {
        Ok(_) => Ok(()),
        Err(error) if is_retryable_embedded_pairing_error(&error) => {
            warn!(
                "Embedded Moonlight pairing failed for host {} with retryable error: {}. Regenerating PIN and retrying once.",
                host_id, error
            );

            let second_session = pairing::begin_pairing(
                moonlight.repository.as_ref(),
                &moonlight.pairing_sessions,
                host_id,
            )
            .await?;
            let second_pin = second_session.pin.clone();

            pairing::complete_pairing_with_stage1_authorization(
                moonlight.repository.as_ref(),
                moonlight.secret_store.as_ref(),
                &moonlight.pairing_sessions,
                &second_session.id,
                || async {
                    authorize_sunshine_pin(
                        &sunshine_host,
                        &sunshine_username,
                        &sunshine_password,
                        &second_pin,
                        Some("Noland Connect"),
                    )
                    .await
                    .map_err(|error| {
                        crate::moonlight::domain::MoonlightError::Persistence(error.to_string())
                    })
                },
            )
            .await
            .map(|_| ())
        }
        Err(error) => Err(error),
    }
}

async fn schedule_microphone_for_game_stream(
    context: &AppContext,
    moonlight: &MoonlightManager,
    host_id: &str,
) {
    let instance_id = MicPassthroughService::instance_id_for_game_stream(context, host_id).await;
    let previous_instance_id =
        moonlight
            .active_stream_instance_id
            .lock()
            .ok()
            .and_then(|mut active_instance| {
                let previous = *active_instance;
                *active_instance = instance_id;
                previous
            });
    let Some(instance_id) = instance_id else {
        return;
    };

    let mic_context = context.clone();
    let active_instance = moonlight.active_stream_instance_id.clone();
    tauri::async_runtime::spawn(async move {
        if let Some(previous_instance_id) =
            previous_instance_id.filter(|previous| *previous != instance_id)
        {
            if let Err(error) =
                MicPassthroughService::stop_for_game_stream(&mic_context, previous_instance_id)
                    .await
            {
                warn!(previous_instance_id, %error, "Failed stopping microphone for replaced game stream");
            }
        }
        let still_active = active_instance
            .lock()
            .ok()
            .is_some_and(|active| *active == Some(instance_id));
        if !still_active {
            return;
        }
        match MicPassthroughService::auto_start_for_game_stream(&mic_context, instance_id).await {
            Ok(true) => {
                let still_active = active_instance
                    .lock()
                    .ok()
                    .is_some_and(|active| *active == Some(instance_id));
                if still_active {
                    info!(instance_id, "Microphone auto-connected for game stream");
                } else if let Err(error) =
                    MicPassthroughService::stop_for_game_stream(&mic_context, instance_id).await
                {
                    warn!(instance_id, %error, "Failed stopping a microphone session whose game stream already ended");
                }
            }
            Ok(false) => {}
            Err(error) => {
                warn!(instance_id, %error, "Microphone auto-connect failed without affecting the game stream")
            }
        }
    });
}

async fn start_embedded_stream_for_host(
    app: &AppHandle,
    context: &AppContext,
    moonlight: &MoonlightManager,
    host_id: String,
    app_id: u32,
) -> Result<MoonlightStartStreamResponse, FrontendError> {
    ensure_embedded_identity_ready_for_explicit_action(context, moonlight)
        .await
        .map_err(moonlight_frontend_error)?;
    let client = ReqwestGameStreamHttpClient::new(moonlight.secret_store.clone())
        .map_err(moonlight_frontend_error)?;
    let prepared = match moonlight_launch::start_stream_request(
        moonlight.repository.as_ref(),
        moonlight.secret_store.as_ref(),
        &client,
        &host_id,
        app_id,
        None,
        false,
    )
    .await
    {
        Ok(prepared) => prepared,
        Err(error)
            if parse_instance_id_from_embedded_host_id(&host_id).is_some()
                && is_missing_embedded_host_error(&error, &host_id) =>
        {
            let instance_id = parse_instance_id_from_embedded_host_id(&host_id)
                .expect("instance id already checked as present");
            warn!(
                instance_id,
                host_id = %host_id,
                "Embedded Moonlight host record was missing at stream start; rebuilding it and retrying once"
            );
            let state_snapshot = context.load_state().await;
            let rebuilt_host_id = ensure_embedded_moonlight_host(
                moonlight.repository.as_ref(),
                &state_snapshot,
                instance_id,
            )
            .await
            .map_err(moonlight_frontend_error)?;
            moonlight_launch::start_stream_request(
                moonlight.repository.as_ref(),
                moonlight.secret_store.as_ref(),
                &client,
                &rebuilt_host_id,
                app_id,
                None,
                false,
            )
            .await
            .map_err(moonlight_frontend_error)?
        }
        Err(error) => return Err(moonlight_frontend_error(error)),
    };

    moonlight
        .input
        .set_mouse_mode(match prepared.preferences.input.mouse_mode {
            crate::moonlight::domain::MouseMode::Relative => {
                crate::input::state::MouseMode::Relative
            }
            crate::moonlight::domain::MouseMode::Absolute => {
                crate::input::state::MouseMode::Absolute
            }
        });
    moonlight.input.set_stream_dimensions(
        prepared.preferences.video.width,
        prepared.preferences.video.height,
    );

    let stream_window = create_or_reuse_stream_window(
        app,
        prepared.preferences.video.width,
        prepared.preferences.video.height,
        &prepared.host_address,
    )
    .map_err(moonlight_frontend_error)?;
    set_native_stream_input_debug_overlay_enabled(
        app.state::<AppContext>()
            .state
            .read()
            .await
            .moonlight_preferences
            .show_input_debug_hud
            != 0,
    );
    install_native_stream_input(&stream_window, moonlight.input.clone())
        .map_err(moonlight_frontend_error)?;
    let surface =
        stream_window_surface_descriptor(&stream_window).map_err(moonlight_frontend_error)?;

    if let Err(error) = moonlight.runtime.attach_surface(surface).await {
        let _ = close_stream_window(app);
        return Err(moonlight_frontend_error(error));
    }

    if let Ok(mut active_preferences) = moonlight.active_session_preferences.lock() {
        *active_preferences = Some(prepared.preferences.clone());
    }

    if let Err(error) = moonlight
        .runtime
        .start(NativeStartRequest {
            host_id: host_id.clone(),
            app_id,
            host_address: prepared.host_address.clone(),
            app_version: prepared.app_version.clone(),
            gfe_version: prepared.gfe_version.clone(),
            session_url: prepared.launch_result.rtsp_session_url.clone(),
            server_codec_mode_support: prepared.server_codec_mode_support,
            preferences: prepared.preferences.clone(),
            supported_video_formats: prepared.supported_video_formats,
            remote_input_key: prepared.remote_input_key,
            remote_input_iv: prepared.remote_input_iv,
            session_generation: 0,
        })
        .await
    {
        let _ = moonlight.runtime.detach_surface().await;
        if let Ok(mut active_preferences) = moonlight.active_session_preferences.lock() {
            *active_preferences = None;
        }
        let _ = close_stream_window(app);
        return Err(moonlight_frontend_error(error));
    }

    schedule_microphone_for_game_stream(context, moonlight, &host_id).await;

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
    stream_window.set_focus().map_err(|error| FrontendError {
        code: "moonlight_error".to_string(),
        message: "Moonlight operation failed".to_string(),
        details: Some(error.to_string()),
        retryable: false,
    })?;
    let preferred_capture_mode = match prepared.preferences.input.mouse_mode {
        crate::moonlight::domain::MouseMode::Relative => CaptureMouseMode::Relative,
        crate::moonlight::domain::MouseMode::Absolute => CaptureMouseMode::Absolute,
    };
    let _ = activate_native_stream_input(&stream_window, preferred_capture_mode);

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
        state: session_state_name(&state).to_string(),
        has_session_url: prepared.launch_result.rtsp_session_url.is_some(),
    })
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
    let country_code = state_snapshot
        .server_preferences
        .geolocation_country_code
        .trim()
        .to_string();
    let geolocation_country_code =
        if country_code.is_empty() || country_code.eq_ignore_ascii_case("GLOBAL") {
            None
        } else {
            Some(country_code.as_str())
        };

    let offers = vast
        .search_offers(
            state_snapshot.server_preferences.min_reliability,
            fetch_limit,
            state_snapshot.server_preferences.storage_gb,
            geolocation_country_code,
            state_snapshot.server_preferences.require_verified,
            state_snapshot.server_preferences.require_datacenter,
            state_snapshot.server_preferences.require_avx,
        )
        .await?;

    let selector = OfferSelector {
        scoring: context.config.scoring.clone(),
    };
    let ranked = selector.rank_offers(offers, &state_snapshot.location);

    let filtered: Vec<_> = ranked
        .into_iter()
        .filter(|offer| offer_matches_server_preferences(offer, &state_snapshot.server_preferences))
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

fn offer_matches_server_preferences(
    offer: &OfferCandidate,
    preferences: &ServerPreferences,
) -> bool {
    let price_ok = if preferences.max_hourly_price > 0.0 {
        offer.hourly_price <= preferences.max_hourly_price
            && offer.hourly_price >= preferences.min_hourly_price
    } else {
        offer.hourly_price >= preferences.min_hourly_price
    };

    let verified_ok = !preferences.require_verified || offer.is_verified;
    let datacenter_ok = !preferences.require_datacenter || offer.is_datacenter;

    let offer_type = offer.offer_type.to_ascii_lowercase();
    let type_ok = match offer_type.as_str() {
        "on-demand" | "ondemand" => preferences.include_on_demand,
        "interruptible" | "bid" => preferences.include_interruptible,
        "reserved" => preferences.include_reserved,
        _ => preferences.include_on_demand,
    };

    let static_ip_ok = !preferences.require_static_ip || offer.has_static_ip;
    let avx_ok = !preferences.require_avx || offer.has_avx;
    let gpu_count_ok = offer.gpu_count == 1;
    let gpu_ram_ok = (offer.gpu_ram_mb as f64 / 1024.0) >= preferences.min_gpu_ram_gb as f64;
    let cpu_cores_ok = offer.cpu_cores >= preferences.min_cpu_cores;
    let down_ok = offer.internet_down_mbps >= preferences.min_inet_down_mbps;
    let up_ok = offer.internet_up_mbps >= preferences.min_inet_up_mbps;

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
}

#[tauri::command]
pub async fn list_available_offer_countries(
    context: State<'_, AppContext>,
) -> Result<Vec<crate::models::app_state::OfferCountryAvailability>, FrontendError> {
    let state_snapshot = context.state.read().await.clone();
    if state_snapshot.credentials.vast_api_key.trim().is_empty() {
        return Err(AppError::InvalidInput(
            "Missing Vast.ai API key. Complete onboarding first.".to_string(),
        )
        .into());
    }

    let vast = VastApiClient::new(
        context.http_client.clone(),
        context.config.vast_base_url.clone(),
        state_snapshot.credentials.vast_api_key.clone(),
    );
    let offers = vast
        .search_offers(
            state_snapshot.server_preferences.min_reliability,
            context.config.offers_search_limit,
            state_snapshot.server_preferences.storage_gb,
            None,
            state_snapshot.server_preferences.require_verified,
            state_snapshot.server_preferences.require_datacenter,
            state_snapshot.server_preferences.require_avx,
        )
        .await?;

    let selector = OfferSelector {
        scoring: context.config.scoring.clone(),
    };
    let ranked = selector.rank_offers(offers, &state_snapshot.location);
    let mut counts = BTreeMap::<String, usize>::new();

    for offer in ranked
        .iter()
        .filter(|offer| offer_matches_server_preferences(offer, &state_snapshot.server_preferences))
    {
        let code = offer.country.trim().to_uppercase();
        if code.len() == 2 && code.chars().all(|ch| ch.is_ascii_alphabetic()) {
            *counts.entry(code).or_default() += 1;
        }
    }

    let mut countries = counts
        .into_iter()
        .map(
            |(code, offer_count)| crate::models::app_state::OfferCountryAvailability {
                code,
                offer_count,
            },
        )
        .collect::<Vec<_>>();
    countries.sort_by(|left, right| {
        right
            .offer_count
            .cmp(&left.offer_count)
            .then_with(|| left.code.cmp(&right.code))
    });

    Ok(countries)
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
    moonlight: State<'_, MoonlightManager>,
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

    stop_active_stream_if_needed(&app, moonlight.inner()).await?;
    OrchestrationService::start_play_flow(app, context.inner().clone()).await?;
    Ok(())
}

#[tauri::command]
pub async fn stop_provisioning_after_current_stage(
    app: AppHandle,
    context: State<'_, AppContext>,
) -> Result<(), FrontendError> {
    OrchestrationService::request_stop_after_current_stage(app, context.inner().clone()).await?;
    Ok(())
}

fn setup_stage_for_orchestration_state(state: OrchestrationState) -> Option<SetupStage> {
    match state {
        OrchestrationState::WireGuardConfigGenerated => Some(SetupStage::WireguardConfigGenerated),
        OrchestrationState::WireGuardAppHandoffStarted => {
            Some(SetupStage::WireguardAppHandoffStarted)
        }
        OrchestrationState::WireGuardWaitingForImport => {
            Some(SetupStage::WireguardWaitingForImport)
        }
        OrchestrationState::WireGuardWaitingForActivation => {
            Some(SetupStage::WireguardWaitingForActivation)
        }
        OrchestrationState::WireGuardVerifying => Some(SetupStage::WireguardVerifying),
        OrchestrationState::WireGuardConnected => Some(SetupStage::WireguardConnected),
        OrchestrationState::MoonlightSunshineReadyToSetup => {
            Some(SetupStage::MoonlightSunshineReadyToSetup)
        }
        OrchestrationState::SunshineCredentialsConfiguring => {
            Some(SetupStage::SunshineCredentialsConfiguring)
        }
        OrchestrationState::SunshineVerifying => Some(SetupStage::SunshineVerifying),
        OrchestrationState::MoonlightDetecting => Some(SetupStage::MoonlightDetecting),
        OrchestrationState::MoonlightPairingStarted
        | OrchestrationState::ConfiguringMoonlight
        | OrchestrationState::AwaitingPairPin
        | OrchestrationState::Pairing => Some(SetupStage::MoonlightPairingStarted),
        OrchestrationState::MoonlightPinReceived => Some(SetupStage::MoonlightPinReceived),
        OrchestrationState::SunshinePinSubmitting => Some(SetupStage::SunshinePinSubmitting),
        OrchestrationState::MoonlightSunshinePaired => Some(SetupStage::MoonlightSunshinePaired),
        OrchestrationState::Ready => Some(SetupStage::SetupComplete),
        OrchestrationState::Error => Some(SetupStage::Failed),
        _ => None,
    }
}

fn orchestration_state_for_setup_stage(stage: SetupStage) -> OrchestrationState {
    match stage {
        SetupStage::PreWireguardExistingFlow => OrchestrationState::ConfiguringRemote,
        SetupStage::WireguardConfigGenerated => OrchestrationState::WireGuardConfigGenerated,
        SetupStage::WireguardAppHandoffStarted => OrchestrationState::WireGuardAppHandoffStarted,
        SetupStage::WireguardWaitingForImport => OrchestrationState::WireGuardWaitingForImport,
        SetupStage::WireguardWaitingForActivation => {
            OrchestrationState::WireGuardWaitingForActivation
        }
        SetupStage::WireguardVerifying => OrchestrationState::WireGuardVerifying,
        SetupStage::WireguardConnected => OrchestrationState::WireGuardConnected,
        SetupStage::MoonlightSunshineReadyToSetup => {
            OrchestrationState::MoonlightSunshineReadyToSetup
        }
        SetupStage::SunshineCredentialsConfiguring => {
            OrchestrationState::SunshineCredentialsConfiguring
        }
        SetupStage::SunshineVerifying => OrchestrationState::SunshineVerifying,
        SetupStage::MoonlightDetecting => OrchestrationState::MoonlightDetecting,
        SetupStage::MoonlightPairingStarted => OrchestrationState::MoonlightPairingStarted,
        SetupStage::MoonlightPinReceived => OrchestrationState::MoonlightPinReceived,
        SetupStage::SunshinePinSubmitting => OrchestrationState::SunshinePinSubmitting,
        SetupStage::MoonlightSunshinePaired => OrchestrationState::MoonlightSunshinePaired,
        SetupStage::SetupComplete => OrchestrationState::Ready,
        SetupStage::Failed => OrchestrationState::Error,
    }
}

fn wireguard_status_for_setup_stage(
    stage: SetupStage,
) -> crate::models::app_state::WireGuardSetupStatus {
    use crate::models::app_state::WireGuardSetupStatus;

    match stage {
        SetupStage::PreWireguardExistingFlow => WireGuardSetupStatus::NotStarted,
        SetupStage::WireguardConfigGenerated => WireGuardSetupStatus::ConfigGenerated,
        SetupStage::WireguardAppHandoffStarted => WireGuardSetupStatus::AppHandoffStarted,
        SetupStage::WireguardWaitingForImport => WireGuardSetupStatus::WaitingForUserImport,
        SetupStage::WireguardWaitingForActivation => WireGuardSetupStatus::WaitingForUserActivation,
        SetupStage::WireguardVerifying => WireGuardSetupStatus::Verifying,
        SetupStage::Failed => WireGuardSetupStatus::Failed,
        _ => WireGuardSetupStatus::Connected,
    }
}

#[tauri::command]
pub async fn resume_provisioning_existing_instance(
    app: AppHandle,
    instance_id: u64,
    context: State<'_, AppContext>,
) -> Result<String, FrontendError> {
    let snapshot = context.reload_state_from_disk().await?;
    let server = snapshot
        .provisioned_servers
        .iter()
        .find(|record| record.instance_id == instance_id)
        .cloned()
        .ok_or_else(|| {
            AppError::NotFound(format!(
                "No persisted provisioning state exists for instance {instance_id}"
            ))
        })?;

    let saved_post_wireguard_stage = setup_stage_for_orchestration_state(server.last_state)
        .filter(|stage| !matches!(stage, SetupStage::Failed | SetupStage::SetupComplete));
    let should_resume_interactive_post_wireguard = matches!(
        saved_post_wireguard_stage,
        Some(
            SetupStage::WireguardConfigGenerated
                | SetupStage::WireguardAppHandoffStarted
                | SetupStage::WireguardWaitingForImport
                | SetupStage::WireguardWaitingForActivation
                | SetupStage::WireguardVerifying
                | SetupStage::WireguardConnected
                | SetupStage::MoonlightSunshineReadyToSetup
                | SetupStage::SunshineCredentialsConfiguring
                | SetupStage::SunshineVerifying
                | SetupStage::MoonlightDetecting
                | SetupStage::MoonlightPairingStarted
                | SetupStage::MoonlightPinReceived
                | SetupStage::SunshinePinSubmitting
                | SetupStage::MoonlightSunshinePaired
        )
    );

    context
        .update_state(|state| {
            state.instance.instance_id = Some(instance_id);
            state.instance.offer_id = server.offer_id;
            state.instance.status = server.status.clone();
            state.instance.ssh_host = server.ssh_host.clone();
            state.instance.ssh_port = server.ssh_port;
            state.instance.ssh_command = server.ssh_command.clone();
            state.wireguard.server_ip = server.wireguard_server_ip.clone();
            state.wireguard.client_ip = server.wireguard_client_ip.clone();
            state.wireguard.server_public_key = server.wireguard_server_public_key.clone();
            state.wireguard.client_public_key = server.wireguard_client_public_key.clone();
            state.wireguard.config_path = server.wireguard_config_path.clone();
            state.moonlight.host_address = server.moonlight_host_address.clone();
            state.last_error = server.last_error.clone();

            if server.steps.wireguard_configured {
                let post_setup_belongs_to_instance =
                    state.post_wireguard_setup.current_instance_id == Some(instance_id);
                state.post_wireguard_setup.current_instance_id = Some(instance_id);
                state.post_wireguard_setup.moonlight_host =
                    if server.moonlight_host_address.trim().is_empty() {
                        server.wireguard_server_ip.clone()
                    } else {
                        server.moonlight_host_address.clone()
                    };
                state.post_wireguard_setup.sunshine_username =
                    state.credentials.app_username.clone();

                let restored_stage = if should_resume_interactive_post_wireguard {
                    if post_setup_belongs_to_instance
                        && !matches!(
                            state.post_wireguard_setup.stage,
                            SetupStage::PreWireguardExistingFlow
                        )
                    {
                        state.post_wireguard_setup.stage
                    } else {
                        saved_post_wireguard_stage.unwrap_or(SetupStage::WireguardConfigGenerated)
                    }
                } else {
                    SetupStage::PreWireguardExistingFlow
                };

                if !post_setup_belongs_to_instance {
                    state.post_wireguard_setup.wireguard_export_path.clear();
                    state.post_wireguard_setup.wireguard_config.clear();
                    state.post_wireguard_setup.wireguard_verified_host.clear();
                    state.post_wireguard_setup.wireguard_reachable_ports.clear();
                    state.post_wireguard_setup.last_error = None;
                    state.post_wireguard_setup.paired = server.embedded_moonlight_paired;
                    state.post_wireguard_setup.setup_complete =
                        matches!(restored_stage, SetupStage::SetupComplete);
                }

                state.post_wireguard_setup.stage = restored_stage;
                state.post_wireguard_setup.wireguard_setup_status =
                    wireguard_status_for_setup_stage(restored_stage);
                state.orchestration_state = orchestration_state_for_setup_stage(restored_stage);
            }
        })
        .await?;

    if server.steps.wireguard_configured && should_resume_interactive_post_wireguard {
        return Ok("provisioning".to_string());
    }

    OrchestrationService::start_play_for_existing_instance(
        app,
        context.inner().clone(),
        instance_id,
    )
    .await?;
    Ok("provisioning".to_string())
}

#[tauri::command]
pub async fn start_play_existing_instance(
    app: AppHandle,
    instance_id: u64,
    context: State<'_, AppContext>,
    moonlight: State<'_, MoonlightManager>,
) -> Result<String, FrontendError> {
    start_launch_pc_for_instance(&app, instance_id, context.inner(), moonlight.inner()).await
}

pub(super) async fn start_launch_pc_for_instance(
    app: &AppHandle,
    instance_id: u64,
    context: &AppContext,
    moonlight: &MoonlightManager,
) -> Result<String, FrontendError> {
    stop_active_stream_if_needed(app, moonlight).await?;
    context
        .update_state(|state| {
            if let Some(server) = state
                .provisioned_servers
                .iter_mut()
                .find(|record| record.instance_id == instance_id)
            {
                server.embedded_moonlight_pipeline_enabled = true;
                if server.embedded_moonlight_host_id.trim().is_empty() {
                    server.embedded_moonlight_host_id = embedded_moonlight_host_id(instance_id);
                }
            }
        })
        .await?;

    {
        ensure_embedded_identity_ready_for_explicit_action(context, moonlight)
            .await
            .map_err(moonlight_frontend_error)?;

        let state_snapshot = context.load_state().await;
        let host_id = ensure_embedded_moonlight_host(
            moonlight.repository.as_ref(),
            &state_snapshot,
            instance_id,
        )
        .await
        .map_err(moonlight_frontend_error)?;
        let client = ReqwestGameStreamHttpClient::new(moonlight.secret_store.clone())
            .map_err(moonlight_frontend_error)?;
        let host_status = hosts::refresh_host(moonlight.repository.as_ref(), &client, &host_id)
            .await
            .map_err(moonlight_frontend_error)?;

        if !matches!(
            host_status
                .host
                .pairing
                .as_ref()
                .map(|pairing| &pairing.status),
            Some(PairingStatus::Paired)
        ) {
            auto_pair_embedded_host(context, moonlight, &host_id)
                .await
                .map_err(moonlight_frontend_error)?;
            hosts::refresh_host(moonlight.repository.as_ref(), &client, &host_id)
                .await
                .map_err(moonlight_frontend_error)?;
            if !embedded_host_is_paired(moonlight.repository.as_ref(), &host_id) {
                return Err(moonlight_frontend_error(
                    crate::moonlight::domain::MoonlightError::Validation(
                        "embedded Moonlight repository did not persist automatic pairing"
                            .to_string(),
                    ),
                ));
            }
        }

        let host_address = resolve_embedded_moonlight_host_address(&state_snapshot, instance_id)
            .unwrap_or_default();
        let apps = match apps::list_remote_apps(
            moonlight.repository.as_ref(),
            moonlight.secret_store.as_ref(),
            &client,
            &host_id,
            RefreshPolicy::ForceRefresh,
        )
        .await
        {
            Ok(apps) => apps,
            Err(error) if is_embedded_applist_not_found_error(&error) => {
                fetch_embedded_apps_via_sunshine_api(context, moonlight, &host_id, &host_address)
                    .await?
            }
            Err(error) => return Err(moonlight_frontend_error(error)),
        };
        let target_app = preferred_embedded_app(&apps).ok_or_else(|| FrontendError {
            code: "moonlight_error".to_string(),
            message: "Moonlight operation failed".to_string(),
            details: Some("No Sunshine applications were reported for this instance".to_string()),
            retryable: true,
        })?;

        let response =
            start_embedded_stream_for_host(app, context, moonlight, host_id, target_app.id).await?;
        context
            .update_state(|state| {
                state.moonlight.configured = true;
                state.moonlight.host_address =
                    resolve_embedded_moonlight_host_address(state, instance_id).unwrap_or_default();
                state.moonlight.session_state = response.state.clone();
                state.moonlight.last_error = None;
                if let Some(server) = state
                    .provisioned_servers
                    .iter_mut()
                    .find(|record| record.instance_id == instance_id)
                {
                    server.embedded_moonlight_host_id = embedded_moonlight_host_id(instance_id);
                    server.embedded_moonlight_paired = true;
                }
            })
            .await?;
        return Ok("embedded".to_string());
    }
}

fn active_instance_wireguard_config_path(state: &PersistedAppState) -> Option<String> {
    let instance_id = state.instance.instance_id?;
    if let Some(path) = state
        .provisioned_servers
        .iter()
        .find(|record| record.instance_id == instance_id)
        .map(|record| record.wireguard_config_path.clone())
        .filter(|path| Path::new(path).exists())
    {
        return Some(path);
    }

    let fallback = state.wireguard.config_path.trim();
    let instance_segment = format!("/{instance_id}/");
    (!fallback.is_empty()
        && Path::new(fallback).exists()
        && fallback.replace('\\', "/").contains(&instance_segment))
    .then(|| fallback.to_string())
}

#[tauri::command]
pub async fn setup_wireguard_client(
    _app: AppHandle,
    context: State<'_, AppContext>,
) -> Result<String, FrontendError> {
    let _wireguard_mutation_guard = context.begin_wireguard_mutation()?;
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
        active_instance_wireguard_config_path(&state).unwrap_or_default()
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

    setup_local_wireguard_client(Path::new(&config_path)).map_err(Into::into)
}

#[tauri::command]
pub async fn reconnect_local_wireguard_client_quick(
    _app: AppHandle,
    context: State<'_, AppContext>,
) -> Result<String, FrontendError> {
    let _wireguard_mutation_guard = context.begin_wireguard_mutation()?;
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
        active_instance_wireguard_config_path(&state).unwrap_or_default()
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

    reconnect_local_wireguard_client(Path::new(&config_path)).map_err(Into::into)
}

#[tauri::command]
pub async fn disconnect_local_wireguard_client_command(
    context: State<'_, AppContext>,
) -> Result<String, FrontendError> {
    let _wireguard_mutation_guard = context.begin_wireguard_mutation()?;
    let config_path = {
        let state = context.state.read().await;
        active_instance_wireguard_config_path(&state).unwrap_or_default()
    };

    if config_path.trim().is_empty() {
        return Err(AppError::InvalidInput(
            "WireGuard client config path is empty. Nothing to disconnect.".to_string(),
        )
        .into());
    }

    let message = teardown_local_wireguard_client(Path::new(&config_path))?;
    context
        .update_state(|state| {
            state.post_wireguard_setup.wireguard_setup_status =
                crate::models::app_state::WireGuardSetupStatus::NotStarted;
            state.post_wireguard_setup.stage = SetupStage::WireguardConfigGenerated;
            state.post_wireguard_setup.wireguard_reachable_ports.clear();
            state.post_wireguard_setup.last_error = None;
            state.wireguard.last_runtime_interface.clear();
            state.orchestration_state = OrchestrationState::WireGuardConfigGenerated;
        })
        .await?;

    Ok(message)
}

#[tauri::command]
pub async fn setup_wireguard_app_handoff_command(
    app: AppHandle,
    context: State<'_, AppContext>,
) -> Result<PostWireGuardSetupState, FrontendError> {
    let _wireguard_mutation_guard = context.begin_wireguard_mutation()?;
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
pub async fn setup_moonlight_sunshine_command(
    app: AppHandle,
    context: State<'_, AppContext>,
) -> Result<PostWireGuardSetupState, FrontendError> {
    setup_moonlight_sunshine(&app, context.inner())
        .await
        .map_err(Into::into)
}

#[tauri::command]
pub async fn retry_setup_stage_command(
    app: AppHandle,
    context: State<'_, AppContext>,
    stage: SetupStage,
) -> Result<PostWireGuardSetupState, FrontendError> {
    let _wireguard_mutation_guard = context.begin_wireguard_mutation()?;
    retry_setup_stage(&app, context.inner(), stage)
        .await
        .map_err(Into::into)
}

#[tauri::command]
pub async fn get_provisioning_logs(
    context: State<'_, AppContext>,
) -> Result<Vec<ProvisioningEvent>, FrontendError> {
    Ok(context.provisioning_logs.read().await.clone())
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
    moonlight: State<'_, MoonlightManager>,
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

    let state = context.state.read().await.clone();
    let mut instances = instances_source
        .into_iter()
        .filter(|instance| {
            let status = instance.status.to_ascii_lowercase();
            !status.contains("destroy") && !status.contains("stopped") && !status.contains("exited")
        })
        .map(|instance| {
            let server = state
                .provisioned_servers
                .iter()
                .find(|record| record.instance_id == instance.id);
            let embedded_enabled = true;
            let host_id = server
                .map(|record| record.embedded_moonlight_host_id.as_str())
                .filter(|host_id| !host_id.trim().is_empty())
                .map(str::to_string)
                .unwrap_or_else(|| embedded_moonlight_host_id(instance.id));
            let paired = Some(embedded_host_is_paired(
                moonlight.repository.as_ref(),
                &host_id,
            ));
            RentedInstanceSummary {
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
                embedded_moonlight_pipeline_enabled: embedded_enabled,
                embedded_moonlight_session_state: None,
                embedded_moonlight_last_error: None,
                embedded_moonlight_last_runtime_event: None,
                embedded_moonlight_runtime_connected: None,
                embedded_moonlight_renderer_ready: None,
                embedded_moonlight_video_session_active: None,
                embedded_moonlight_video_frame_count: None,
                embedded_moonlight_renderer_submitted_frame_count: None,
                embedded_moonlight_renderer_dropped_frame_count: None,
                embedded_moonlight_audio_sample_count: None,
                embedded_moonlight_paired: paired,
            }
        })
        .collect::<Vec<_>>();

    instances.sort_by(|left, right| right.instance_id.cmp(&left.instance_id));
    Ok(instances)
}

#[tauri::command]
pub async fn get_vast_wallet_summary(
    context: State<'_, AppContext>,
) -> Result<VastWalletSummary, FrontendError> {
    let state = context.state.read().await.clone();
    if state.credentials.vast_api_key.trim().is_empty() {
        return Ok(unavailable_vast_wallet_summary());
    }

    let vast = VastApiClient::new(
        context.http_client.clone(),
        context.config.vast_base_url.clone(),
        state.credentials.vast_api_key,
    );

    match vast.get_wallet_summary().await {
        Ok(summary) => Ok(build_vast_wallet_summary(summary.balance_usd)),
        Err(error) => {
            warn!("get_vast_wallet_summary failed: {}", error);
            Ok(unavailable_vast_wallet_summary())
        }
    }
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

    set_native_stream_input_debug_overlay_enabled(
        next_state.moonlight_preferences.show_input_debug_hud != 0,
    );

    Ok(next_state)
}

#[tauri::command]
pub async fn set_instance_moonlight_pipeline_enabled(
    instance_id: u64,
    enabled: bool,
    context: State<'_, AppContext>,
) -> Result<PersistedAppState, FrontendError> {
    let _ = enabled;
    let next_state = context
        .update_state(|state| {
            if let Some(server) = state
                .provisioned_servers
                .iter_mut()
                .find(|record| record.instance_id == instance_id)
            {
                server.embedded_moonlight_pipeline_enabled = true;
                if server.embedded_moonlight_host_id.trim().is_empty() {
                    server.embedded_moonlight_host_id = embedded_moonlight_host_id(instance_id);
                }
            } else {
                state.provisioned_servers.push(
                    crate::models::app_state::ProvisionedServerState::new(instance_id),
                );
            }
            state.last_error = None;
        })
        .await?;
    Ok(next_state)
}

#[tauri::command]
pub async fn moonlight_get_instance_pipeline_status(
    instance_id: u64,
    context: State<'_, AppContext>,
    moonlight: State<'_, MoonlightManager>,
) -> Result<EmbeddedMoonlightInstanceStatus, FrontendError> {
    let state = context.load_state().await;
    let server = state
        .provisioned_servers
        .iter()
        .find(|record| record.instance_id == instance_id)
        .ok_or_else(|| AppError::NotFound(format!("instance {instance_id} not found")))?;
    let host_id = if server.embedded_moonlight_host_id.trim().is_empty() {
        embedded_moonlight_host_id(instance_id)
    } else {
        server.embedded_moonlight_host_id.clone()
    };

    let _ =
        ensure_embedded_moonlight_host(moonlight.repository.as_ref(), &state, instance_id).await;

    let paired = embedded_host_is_paired(moonlight.repository.as_ref(), &host_id);
    let runtime_stats = moonlight.runtime.latest_statistics();
    let runtime_connected = runtime_stats.state == "streaming";
    let latest_event = moonlight.runtime.latest_event();
    let last_runtime_event = latest_event.as_ref().map(|event| {
        if event.message.trim().is_empty() {
            format!("{} ({})", event.kind, event.code)
        } else {
            format!("{}: {} ({})", event.kind, event.message, event.code)
        }
    });
    let runtime_last_error = latest_event
        .as_ref()
        .and_then(|event| match event.kind.as_str() {
            "error" | "stageFailed" | "terminated" => last_runtime_event.clone(),
            _ => None,
        });
    Ok(EmbeddedMoonlightInstanceStatus {
        instance_id,
        enabled: true,
        host_id,
        paired,
        host_address: resolve_embedded_moonlight_host_address(&state, instance_id)
            .unwrap_or_default(),
        session_state: runtime_stats.state.clone(),
        last_error: runtime_last_error.or(state.moonlight.last_error),
        runtime_connected,
        renderer_ready: runtime_stats.renderer_ready,
        video_session_active: runtime_stats.video_session_active,
        video_frame_count: runtime_stats.video_frame_count,
        renderer_submitted_frame_count: runtime_stats.renderer_submitted_frame_count,
        renderer_dropped_frame_count: runtime_stats.renderer_dropped_frame_count,
        audio_sample_count: runtime_stats.audio_sample_count,
        last_runtime_event,
    })
}

#[tauri::command]
pub async fn moonlight_prepare_instance_pairing(
    instance_id: u64,
    context: State<'_, AppContext>,
    moonlight: State<'_, MoonlightManager>,
) -> Result<MoonlightPairingSessionResponse, FrontendError> {
    ensure_embedded_identity_ready_for_explicit_action(context.inner(), moonlight.inner())
        .await
        .map_err(moonlight_frontend_error)?;
    let state = context.load_state().await;
    let host_id =
        ensure_embedded_moonlight_host(moonlight.repository.as_ref(), &state, instance_id)
            .await
            .map_err(moonlight_frontend_error)?;
    let client = ReqwestGameStreamHttpClient::new(moonlight.secret_store.clone())
        .map_err(moonlight_frontend_error)?;
    hosts::refresh_host(moonlight.repository.as_ref(), &client, &host_id)
        .await
        .map_err(moonlight_frontend_error)?;
    let session = pairing::begin_pairing(
        moonlight.repository.as_ref(),
        &moonlight.pairing_sessions,
        &host_id,
    )
    .await
    .map_err(moonlight_frontend_error)?;
    context
        .update_state(|state| {
            if let Some(server) = state
                .provisioned_servers
                .iter_mut()
                .find(|record| record.instance_id == instance_id)
            {
                server.embedded_moonlight_pipeline_enabled = true;
                server.embedded_moonlight_host_id = host_id.clone();
            }
            state.post_wireguard_setup.current_instance_id = Some(instance_id);
            state.post_wireguard_setup.stage = SetupStage::MoonlightPairingStarted;
            state.post_wireguard_setup.paired = false;
            state.post_wireguard_setup.setup_complete = false;
            state.orchestration_state = OrchestrationState::MoonlightPairingStarted;
            state.moonlight.host_address =
                resolve_embedded_moonlight_host_address(state, instance_id).unwrap_or_default();
            state.moonlight.last_error = None;
            state.last_error = None;
        })
        .await?;
    Ok(MoonlightPairingSessionResponse {
        session_id: session.id.0,
        host_id: session.host_id,
        pin: session.pin,
        expires_in_seconds: 300,
    })
}

#[tauri::command]
pub async fn moonlight_complete_instance_pairing(
    app: AppHandle,
    instance_id: u64,
    input: MoonlightCompletePairingInput,
    context: State<'_, AppContext>,
    moonlight: State<'_, MoonlightManager>,
) -> Result<crate::moonlight::application::pairing::PairingResult, FrontendError> {
    let session_id = PairingSessionId(input.session_id.clone());
    let pairing_session = moonlight
        .pairing_sessions
        .get(&session_id)
        .map_err(moonlight_frontend_error)?
        .ok_or_else(|| FrontendError {
            code: "moonlight_error".to_string(),
            message: "Moonlight operation failed".to_string(),
            details: Some("pairing session not found".to_string()),
            retryable: true,
        })?;

    let state_snapshot = context.load_state().await;
    let effective_instance_id =
        resolve_instance_id_for_embedded_host(&state_snapshot, &pairing_session.host_id)
            .unwrap_or(instance_id);
    info!(
        requested_instance_id = instance_id,
        effective_instance_id,
        host_id = %pairing_session.host_id,
        "Completing embedded Moonlight pairing"
    );
    ensure_embedded_moonlight_host(
        moonlight.repository.as_ref(),
        &state_snapshot,
        effective_instance_id,
    )
    .await
    .map_err(moonlight_frontend_error)?;

    let (sunshine_host, sunshine_username, sunshine_password) = {
        let host = resolve_embedded_moonlight_host_address(&state_snapshot, effective_instance_id)
            .ok_or_else(|| FrontendError {
                code: "moonlight_error".to_string(),
                message: "Moonlight operation failed".to_string(),
                details: Some(format!(
                    "instance {} has no Moonlight host address",
                    effective_instance_id
                )),
                retryable: true,
            })?;
        let credentials = &state_snapshot.credentials;
        (
            host,
            credentials.app_username.clone(),
            credentials.app_password.clone(),
        )
    };

    let pairing_pin = pairing_session.pin.clone();
    let result = pairing::complete_pairing_with_stage1_authorization(
        moonlight.repository.as_ref(),
        moonlight.secret_store.as_ref(),
        &moonlight.pairing_sessions,
        &session_id,
        || async {
            authorize_sunshine_pin(
                &sunshine_host,
                &sunshine_username,
                &sunshine_password,
                &pairing_pin,
                Some("Noland Connect"),
            )
            .await
            .map_err(|error| {
                crate::moonlight::domain::MoonlightError::Persistence(error.to_string())
            })
        },
    )
    .await
    .map_err(moonlight_frontend_error)?;
    if !embedded_host_is_paired(moonlight.repository.as_ref(), &result.host_id) {
        return Err(moonlight_frontend_error(
            crate::moonlight::domain::MoonlightError::Validation(
                "embedded Moonlight repository did not persist a paired host".to_string(),
            ),
        ));
    }
    context
        .update_state(|state| {
            if let Some(server) = state
                .provisioned_servers
                .iter_mut()
                .find(|record| record.instance_id == effective_instance_id)
            {
                server.embedded_moonlight_pipeline_enabled = true;
                server.embedded_moonlight_host_id = result.host_id.clone();
                server.embedded_moonlight_paired = true;
            }
            state.post_wireguard_setup.current_instance_id = Some(effective_instance_id);
            state.post_wireguard_setup.stage = SetupStage::SetupComplete;
            state.post_wireguard_setup.paired = true;
            state.post_wireguard_setup.setup_complete = true;
            state.orchestration_state = OrchestrationState::Ready;
            state.has_completed_guided_setup = true;
            state.moonlight.last_error = None;
            state.last_error = None;
        })
        .await?;

    let (offer_id, status, ssh_host, ssh_port) = {
        let snapshot = context.state.read().await.clone();
        if let Some(server) = snapshot
            .provisioned_servers
            .iter()
            .find(|record| record.instance_id == effective_instance_id)
        {
            (
                server.offer_id,
                server.status.clone(),
                server.ssh_host.clone(),
                server.ssh_port,
            )
        } else {
            (
                snapshot.instance.offer_id,
                snapshot.instance.status,
                snapshot.instance.ssh_host,
                snapshot.instance.ssh_port,
            )
        }
    };

    crate::services::orchestration::mark_server_step_completed(
        context.inner(),
        effective_instance_id,
        crate::services::orchestration::ProvisionStepMarker::PairingCompleted,
        OrchestrationState::Ready,
        &status,
        &ssh_host,
        ssh_port,
        offer_id,
    )
    .await
    .map_err(FrontendError::from)?;

    let pairing_event = crate::models::events::ProvisioningEvent::info(
        OrchestrationState::MoonlightSunshinePaired,
        "Moonlight and Sunshine paired",
        Some("Your secure streaming connection is ready.".to_string()),
    );
    context.emit_progress(&app, pairing_event).await;

    let ready_event = crate::models::events::ProvisioningEvent::info(
        OrchestrationState::Ready,
        "Setup complete",
        Some("Your secure streaming connection is ready.".to_string()),
    );
    context.emit_progress(&app, ready_event).await;

    Ok(result)
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
    let (width, height, refresh_hz, mut source_label) = resolve_effective_edid_profile(
        payload.mode,
        snapshot.moonlight_preferences.width,
        snapshot.moonlight_preferences.height,
        payload.refresh_rate_hz,
    );
    info!(
        "regenerate_edid resolved profile: width={} height={} refresh_hz={} source='{}'",
        width, height, refresh_hz, source_label
    );
    let mut effective_width = width;
    let mut effective_height = height;
    let mut effective_refresh_hz = refresh_hz;
    let generated_edid = match generate_headless_edid_base64(width, height, refresh_hz) {
        Ok(edid) => edid,
        Err(_) if payload.mode == EdidMode::AutoDetect && refresh_hz != 60 => {
            effective_refresh_hz = 60;
            source_label = "Auto-Detected (EDID refresh limited to 60 Hz)".to_string();
            generate_headless_edid_base64(width, height, 60).or_else(|_| {
                effective_width = 3840;
                effective_height = 2160;
                source_label =
                    "Fallback 3840x2160@60 (native timing is not EDID-compatible)".to_string();
                generate_headless_edid_base64(effective_width, effective_height, 60)
            })?
        }
        Err(_) if payload.mode == EdidMode::AutoDetect => {
            effective_refresh_hz = 60;
            effective_width = 3840;
            effective_height = 2160;
            source_label =
                "Fallback 3840x2160@60 (native timing is not EDID-compatible)".to_string();
            generate_headless_edid_base64(effective_width, effective_height, 60).or_else(|_| {
                effective_width = 1920;
                effective_height = 1080;
                generate_headless_edid_base64(effective_width, effective_height, 60)
            })?
        }
        Err(error) => return Err(error.into()),
    };

    let next_state = context
        .update_state(|state| {
            state.sunshine.edid_mode = payload.mode;
            state.sunshine.edid_refresh_rate_hz = effective_refresh_hz;
            state.sunshine.headless_edid_base64 = generated_edid.clone();
            state.sunshine.edid_source_label = source_label.clone();
            if payload.mode != EdidMode::Manual {
                state.moonlight_preferences.width = effective_width;
                state.moonlight_preferences.height = effective_height;
                state.moonlight_preferences.fps = effective_refresh_hz;
                state.moonlight_preferences.refresh_rate_mode = effective_refresh_hz.to_string();
            }
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
        EdidMode::MacHardware => {
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
    let ssh_password = state.ssh.ssh_password.clone();
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
        ssh_password,
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
    let ssh_password = state.ssh.ssh_password.clone();

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
        ssh_password,
    })
}

fn parse_wireguard_endpoint_from_config(
    config_path: &Path,
) -> Result<Option<(String, u16)>, AppError> {
    if !config_path.exists() {
        return Ok(None);
    }

    let content = std::fs::read_to_string(config_path).map_err(|error| {
        AppError::Command(format!(
            "Failed reading WireGuard client config {}: {error}",
            config_path.display()
        ))
    })?;

    let endpoint_line = content
        .lines()
        .map(str::trim)
        .find(|line| line.to_ascii_lowercase().starts_with("endpoint ="));
    let Some(endpoint_line) = endpoint_line else {
        return Ok(None);
    };

    let Some((_, endpoint)) = endpoint_line.split_once('=') else {
        return Ok(None);
    };
    let endpoint = endpoint.trim();

    if let Some(rest) = endpoint.strip_prefix('[') {
        if let Some((host, port)) = rest.split_once("]:") {
            if let Ok(port) = port.trim().parse::<u16>() {
                return Ok(Some((host.trim().to_string(), port)));
            }
        }
        return Ok(None);
    }

    let Some((host, port)) = endpoint.rsplit_once(':') else {
        return Ok(None);
    };
    let Ok(port) = port.trim().parse::<u16>() else {
        return Ok(None);
    };

    Ok(Some((host.trim().to_string(), port)))
}

async fn inspect_remote_instance_services(
    remote: &RemoteExec,
) -> Result<RemoteInstanceServiceHealth, AppError> {
    let command = r#"sunshine_status=$(systemctl is-active sunshine.service 2>/dev/null || true)
wireguard_status=$(systemctl is-active wg-quick@wg0.service 2>/dev/null || true)
wireguard_addresses=$(ip -o addr show wg0 2>/dev/null | awk '{print $4}' | paste -sd ',' -)
printf 'sunshine=%s\nwireguard=%s\nwireguard_addresses=%s\n' "$sunshine_status" "$wireguard_status" "$wireguard_addresses""#;
    let remote = remote.clone();
    let output = tokio::task::spawn_blocking(move || remote.ssh(command, Duration::from_secs(20)))
        .await
        .map_err(|error| AppError::Command(format!("join failure: {error}")))??;

    if output.status_code != 0 {
        return Err(AppError::Provisioning(format!(
            "Failed checking remote service health: {}",
            output.stderr.trim()
        )));
    }

    let mut result = RemoteInstanceServiceHealth::default();
    for line in output.stdout.lines() {
        let Some((key, raw_value)) = line.split_once('=') else {
            continue;
        };
        let value = raw_value.trim();
        match key.trim() {
            "sunshine" => {
                result.sunshine_active = value.eq_ignore_ascii_case("active");
            }
            "wireguard" => {
                result.wireguard_active = value.eq_ignore_ascii_case("active");
            }
            "wireguard_addresses" => {
                result.wireguard_addresses = value
                    .split(',')
                    .map(str::trim)
                    .filter(|entry| !entry.is_empty())
                    .map(ToOwned::to_owned)
                    .collect();
            }
            _ => {}
        }
    }

    Ok(result)
}

async fn refresh_instance_connection_state(
    context: &AppContext,
    instance: &VastInstance,
    result: &crate::services::wireguard::WireGuardProvisionResult,
) -> Result<(), AppError> {
    let config_path = result.client_config_path.display().to_string();
    let moonlight_host = if !result.server_ip.trim().is_empty() {
        result.server_ip.clone()
    } else {
        instance.wireguard_host_ip.trim().to_string()
    };

    context
        .update_state(|state| {
            let index = state
                .provisioned_servers
                .iter()
                .position(|record| record.instance_id == instance.id)
                .unwrap_or_else(|| {
                    state.provisioned_servers.push(
                        crate::models::app_state::ProvisionedServerState::new(instance.id),
                    );
                    state.provisioned_servers.len() - 1
                });
            let record = &mut state.provisioned_servers[index];
            record.status = instance.status.clone();
            record.ssh_host = instance.ssh_host.clone();
            record.ssh_port = instance.ssh_port;
            record.ssh_command = instance.ssh_command.clone();
            record.connection_provider = ConnectionProvider::Wireguard;
            record.wireguard_server_ip = result.server_ip.clone();
            record.wireguard_client_ip = result.client_ip.clone();
            record.wireguard_server_public_key = result.server_public_key.clone();
            record.wireguard_client_public_key = result.client_public_key.clone();
            record.wireguard_config_path = config_path.clone();
            if !moonlight_host.trim().is_empty() {
                record.moonlight_host_address = moonlight_host.clone();
            }

            state.instance.instance_id = Some(instance.id);
            state.instance.status = instance.status.clone();
            state.instance.ssh_host = instance.ssh_host.clone();
            state.instance.ssh_port = instance.ssh_port;
            state.instance.ssh_command = instance.ssh_command.clone();
            state.wireguard.server_ip = result.server_ip.clone();
            state.wireguard.client_ip = result.client_ip.clone();
            state.wireguard.server_public_key = result.server_public_key.clone();
            state.wireguard.client_public_key = result.client_public_key.clone();
            state.wireguard.config_path = config_path.clone();
            state.connection_provider = ConnectionProvider::Wireguard;
            state.moonlight.host_address = moonlight_host.clone();
            state.post_wireguard_setup.current_instance_id = Some(instance.id);
            state.post_wireguard_setup.wireguard_config.clear();
            state.post_wireguard_setup.wireguard_export_path.clear();
            state.post_wireguard_setup.wireguard_verified_host = result.server_ip.clone();
            state.post_wireguard_setup.moonlight_host = moonlight_host.clone();
            state.post_wireguard_setup.last_error = None;
            state.last_error = None;
        })
        .await?;

    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum InstanceConnectionSyncMode {
    Interactive,
    StartupRestore,
}

pub(crate) async fn sync_instance_connection_internal(
    app: &AppHandle,
    context: &AppContext,
    instance_id: u64,
    mode: InstanceConnectionSyncMode,
) -> Result<String, AppError> {
    let snapshot = context.load_state().await;

    if mode == InstanceConnectionSyncMode::StartupRestore {
        let config_path = snapshot
            .provisioned_servers
            .iter()
            .find(|record| record.instance_id == instance_id)
            .map(|record| PathBuf::from(record.wireguard_config_path.trim()))
            .filter(|path| !path.as_os_str().is_empty())
            .or_else(|| {
                (snapshot.instance.instance_id == Some(instance_id)
                    && !snapshot.wireguard.config_path.trim().is_empty())
                .then(|| PathBuf::from(snapshot.wireguard.config_path.trim()))
            })
            .ok_or_else(|| {
                AppError::NotFound(format!(
                    "No persisted WireGuard config exists for instance {instance_id}"
                ))
            })?;

        if !config_path.is_file() {
            return Err(AppError::NotFound(format!(
                "Persisted WireGuard config for instance {instance_id} was not found at {}",
                config_path.display()
            )));
        }

        if let Ok(status) = verify_managed_gotatun_tunnel(&config_path) {
            return Ok(format!(
                "Managed connection for active instance {instance_id} was already healthy. {status}"
            ));
        }

        let _wireguard_mutation_guard = context.begin_wireguard_mutation()?;
        let reconnect_message = reconnect_local_wireguard_client(&config_path)?;
        let tunnel_status = verify_managed_gotatun_tunnel(&config_path)?;
        return Ok(format!(
            "Restored local managed connection for active instance {instance_id} without changing the remote server or provisioning state. {reconnect_message} {tunnel_status}"
        ));
    }

    if snapshot.credentials.vast_api_key.trim().is_empty() {
        return Err(AppError::InvalidInput(
            "Vast API key is missing. Add it in settings before syncing the connection."
                .to_string(),
        ));
    }

    let vast = VastApiClient::new(
        context.http_client.clone(),
        context.config.vast_base_url.clone(),
        snapshot.credentials.vast_api_key.clone(),
    );
    let instance = vast.get_instance(instance_id).await.map_err(|error| {
        AppError::Provisioning(format!(
            "Failed to refresh Vast.ai instance {} before syncing the connection: {}",
            instance_id, error
        ))
    })?;

    let endpoint_host = instance.wireguard_endpoint_host();
    let endpoint_port = instance.wireguard_port;
    if endpoint_host.trim().is_empty() || endpoint_port == 0 {
        return Err(AppError::Provisioning(format!(
            "Instance {} does not expose a valid WireGuard endpoint on Vast (host='{}' port={}).",
            instance_id, endpoint_host, endpoint_port
        )));
    }

    let previous_config_path = snapshot
        .provisioned_servers
        .iter()
        .find(|record| record.instance_id == instance_id)
        .map(|record| PathBuf::from(record.wireguard_config_path.clone()))
        .filter(|path| !path.as_os_str().is_empty())
        .or_else(|| {
            if snapshot.instance.instance_id == Some(instance_id)
                && !snapshot.wireguard.config_path.trim().is_empty()
            {
                Some(PathBuf::from(snapshot.wireguard.config_path.clone()))
            } else {
                None
            }
        });
    let previous_endpoint = previous_config_path
        .as_deref()
        .map(parse_wireguard_endpoint_from_config)
        .transpose()?
        .flatten();

    context
        .update_state(|state| {
            let index = state
                .provisioned_servers
                .iter()
                .position(|record| record.instance_id == instance.id)
                .unwrap_or_else(|| {
                    state.provisioned_servers.push(
                        crate::models::app_state::ProvisionedServerState::new(instance.id),
                    );
                    state.provisioned_servers.len() - 1
                });
            let record = &mut state.provisioned_servers[index];
            record.status = instance.status.clone();
            record.ssh_host = instance.ssh_host.clone();
            record.ssh_port = instance.ssh_port;
            record.ssh_command = instance.ssh_command.clone();
            record.hourly_price = instance.hourly_price;
            record.compute_hourly_price = instance.compute_hourly_price;
            record.storage_hourly_price = instance.storage_hourly_price;
            state.instance.instance_id = Some(instance.id);
            state.instance.status = instance.status.clone();
            state.instance.ssh_host = instance.ssh_host.clone();
            state.instance.ssh_port = instance.ssh_port;
            state.instance.ssh_command = instance.ssh_command.clone();
            state.instance.hourly_price = instance.hourly_price;
            state.instance.compute_hourly_price = instance.compute_hourly_price;
            state.instance.storage_hourly_price = instance.storage_hourly_price;
        })
        .await?;

    let remote = build_remote_exec_for_instance(context, instance_id).await?;
    let app_data_dir = context.state_store.path().parent().ok_or_else(|| {
        AppError::State("Could not resolve app data directory from state file path".to_string())
    })?;
    let wireguard = WireGuardService {
        defaults: context.config.wireguard.clone(),
    };
    let _wireguard_mutation_guard = context.begin_wireguard_mutation()?;
    let wireguard_result = wireguard
        .configure(
            &remote,
            app_data_dir,
            instance.id,
            &endpoint_host,
            endpoint_port,
            instance.wireguard_listen_port,
            WireGuardProvisionMode::ReinitializeExisting,
        )
        .await?;
    refresh_instance_connection_state(context, &instance, &wireguard_result).await?;

    let reconnect_message =
        reconnect_local_wireguard_client(wireguard_result.client_config_path.as_path())?;

    let reachability = verify_wireguard_connection(app, context).await?;
    let remote_health = inspect_remote_instance_services(&remote).await?;

    let (sunshine_username, sunshine_password) = {
        let state = context.state.read().await;
        (
            state.credentials.app_username.clone(),
            state.credentials.app_password.clone(),
        )
    };
    let sunshine_summary = if reachability.reachable
        && !sunshine_username.trim().is_empty()
        && !sunshine_password.trim().is_empty()
    {
        let sunshine = verify_sunshine_api(app, context).await?;
        if sunshine.reachable && sunshine.authenticated {
            "Sunshine API reachable and authenticated.".to_string()
        } else if sunshine.reachable {
            format!(
                "Sunshine API reachable, but authentication/setup still needs attention: {}",
                sunshine
                    .error
                    .unwrap_or_else(|| "unknown Sunshine authentication state".to_string())
            )
        } else {
            format!(
                "Sunshine service is active on the remote, but the tunneled API check failed: {}",
                sunshine
                    .error
                    .unwrap_or_else(|| "unknown Sunshine API error".to_string())
            )
        }
    } else if !remote_health.sunshine_active {
        "Sunshine service is not active on the remote instance.".to_string()
    } else if !reachability.reachable {
        "Skipped tunneled Sunshine API verification because 10.77.0.1 is still unreachable locally."
            .to_string()
    } else {
        "Skipped tunneled Sunshine API verification because stored Sunshine credentials are missing."
            .to_string()
    };

    let endpoint_change_summary = match previous_endpoint {
        Some((previous_host, previous_port))
            if previous_host.trim() != endpoint_host.trim() || previous_port != endpoint_port =>
        {
            format!(
                "WireGuard endpoint updated from {}:{} to {}:{}.",
                previous_host, previous_port, endpoint_host, endpoint_port
            )
        }
        Some((previous_host, previous_port)) => format!(
            "WireGuard endpoint still matches {}:{}.",
            previous_host, previous_port
        ),
        None => format!(
            "WireGuard endpoint set to {}:{}.",
            endpoint_host, endpoint_port
        ),
    };

    let remote_wireguard_summary = if remote_health.wireguard_active {
        if remote_health.wireguard_addresses.is_empty() {
            "Remote wg0 is active.".to_string()
        } else {
            format!(
                "Remote wg0 is active with addresses {}.",
                remote_health.wireguard_addresses.join(", ")
            )
        }
    } else {
        "Remote wg0 is not active on the instance.".to_string()
    };

    let summary = format!(
        "Synced connection for instance {}. {} {} {} {} {}",
        instance_id,
        endpoint_change_summary,
        reconnect_message,
        if reachability.reachable {
            "Local tunnel reached 10.77.0.1 successfully."
        } else {
            "Local tunnel still could not reach 10.77.0.1."
        },
        remote_wireguard_summary,
        sunshine_summary,
    );

    if !reachability.reachable {
        let tunnel_error = reachability
            .error
            .unwrap_or_else(|| "WireGuard verification failed after sync".to_string());
        return Err(AppError::Provisioning(format!(
            "{} {}",
            summary, tunnel_error
        )));
    }

    Ok(summary)
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
    payload: crate::models::app_state::SharedStorageBackupSelectionRequest,
) -> Result<String, FrontendError> {
    InstanceLifecycleService::save_instance_to_shared_storage_selected(
        context.inner(),
        instance_id,
        payload.selected_paths,
        payload.performance_mode,
    )
    .await
    .map_err(Into::into)
}

#[tauri::command]
pub async fn cancel_shared_storage_operation(
    context: State<'_, AppContext>,
    instance_id: u64,
) -> Result<String, FrontendError> {
    InstanceLifecycleService::cancel_shared_storage_operation(context.inner(), instance_id)
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
    app: AppHandle,
    context: State<'_, AppContext>,
    instance_id: u64,
) -> Result<String, FrontendError> {
    sync_instance_connection_internal(
        &app,
        context.inner(),
        instance_id,
        InstanceConnectionSyncMode::Interactive,
    )
    .await
    .map_err(Into::into)
}

async fn teardown_local_instance_session(
    app: &AppHandle,
    context: &AppContext,
    moonlight: &MoonlightManager,
    instance_id: u64,
) {
    if let Err(error) = MicPassthroughService::disable(context, instance_id).await {
        if !matches!(error, AppError::InvalidInput(_)) {
            warn!(
                instance_id = instance_id,
                "Failed to disable mic passthrough during teardown: {}", error
            );
        }
    }

    let active_instance_id = context.load_state().await.instance.instance_id;
    if active_instance_id != Some(instance_id) {
        return;
    }

    if let Err(error) = moonlight.runtime.stop().await {
        warn!(
            instance_id = instance_id,
            "Failed to stop embedded Moonlight runtime: {}", error
        );
    }
    if let Err(error) = moonlight.runtime.detach_surface().await {
        warn!(
            instance_id = instance_id,
            "Failed to detach embedded Moonlight surface: {}", error
        );
    }
    moonlight.input.end_capture();
    if let Ok(mut active_preferences) = moonlight.active_session_preferences.lock() {
        *active_preferences = None;
    }
    if let Err(error) = close_stream_window(app) {
        warn!(
            instance_id = instance_id,
            "Failed to close Moonlight stream window: {}", error
        );
    }
}

#[tauri::command]
pub async fn pause_instance(
    app: AppHandle,
    context: State<'_, AppContext>,
    moonlight: State<'_, MoonlightManager>,
    instance_id: u64,
) -> Result<(), FrontendError> {
    teardown_local_instance_session(&app, context.inner(), moonlight.inner(), instance_id).await;

    InstanceLifecycleService::pause_instance(context.inner(), instance_id)
        .await
        .map_err(Into::into)
}

#[tauri::command]
pub async fn destroy_instance(
    app: AppHandle,
    context: State<'_, AppContext>,
    moonlight: State<'_, MoonlightManager>,
    instance_id: u64,
) -> Result<(), FrontendError> {
    teardown_local_instance_session(&app, context.inner(), moonlight.inner(), instance_id).await;

    InstanceLifecycleService::destroy_instance(context.inner(), instance_id)
        .await
        .map_err(Into::into)
}

fn desired_display_profile(
    state: &PersistedAppState,
) -> Result<(DisplayProfile, String), AppError> {
    let (width, height, refresh_hz, source_label) = resolve_effective_edid_profile(
        state.sunshine.edid_mode,
        state.moonlight_preferences.width,
        state.moonlight_preferences.height,
        state.sunshine.edid_refresh_rate_hz,
    );
    let source = match state.sunshine.edid_mode {
        EdidMode::Manual => DisplayProfileSource::Manual,
        EdidMode::MacHardware => DisplayProfileSource::AutoDetected,
        EdidMode::AutoDetect if source_label.starts_with("Fallback") => {
            DisplayProfileSource::Fallback
        }
        EdidMode::AutoDetect => DisplayProfileSource::AutoDetected,
    };
    let mut candidates = vec![(width, height, refresh_hz, source_label)];
    if state.sunshine.edid_mode == EdidMode::AutoDetect {
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

    let mut last_error = None;
    for (candidate_width, candidate_height, candidate_refresh, candidate_label) in candidates {
        match generate_headless_edid_base64(candidate_width, candidate_height, candidate_refresh) {
            Ok(edid) => {
                let mut profile = build_display_profile(
                    DisplayModeSpec::from_hz(candidate_width, candidate_height, candidate_refresh),
                    source,
                )
                .map_err(|error| AppError::InvalidInput(error.to_string()))?;
                profile.source_label = candidate_label;
                return Ok((profile, edid));
            }
            Err(error) => last_error = Some(error),
        }
    }

    Err(last_error.unwrap_or_else(|| {
        AppError::InvalidInput("No EDID-compatible display timing is available".to_string())
    }))
}

#[tauri::command]
pub async fn get_instance_display_status(
    context: State<'_, AppContext>,
    instance_id: u64,
) -> Result<InstanceDisplayStatus, FrontendError> {
    let remote = build_remote_exec_for_instance(context.inner(), instance_id).await?;
    let state = context.load_state().await;
    let (profile, edid) = desired_display_profile(&state)?;
    RemoteDisplayService::status(&remote, profile, &edid)
        .await
        .map_err(Into::into)
}

#[tauri::command]
pub async fn apply_instance_display_mode(
    context: State<'_, AppContext>,
    instance_id: u64,
    mode: DisplayModeSpec,
) -> Result<ApplyDisplayModeResult, FrontendError> {
    InstanceLifecycleService::acquire_lock(instance_id, "display apply").await?;
    let result = async {
        let remote = build_remote_exec_for_instance(context.inner(), instance_id).await?;
        let state = context.load_state().await;
        let (profile, edid) = desired_display_profile(&state)?;
        let target_user = context.config.audio_target_user.clone();
        let result =
            RemoteDisplayService::apply(&remote, &target_user, profile, &edid, mode).await?;
        let status = result.status.clone();
        context
            .update_state(|state| {
                if let Some(server) = state
                    .provisioned_servers
                    .iter_mut()
                    .find(|server| server.instance_id == instance_id)
                {
                    server.display.desired_profile_hash = status.desired_profile_hash.clone();
                    server.display.installed_profile_hash = status.installed_profile_hash.clone();
                    server.display.advertised_modes =
                        status.desired_profile.advertised_modes.clone();
                    server.display.selected_mode = status.selected_mode;
                    server.display.active_mode = status.active_mode;
                    server.display.output_name = status.output_name.clone();
                    server.display.applied_at = Some(chrono::Utc::now().to_rfc3339());
                    server.display.last_apply_error = None;
                }
            })
            .await?;
        Ok::<_, AppError>(result)
    }
    .await;
    if let Err(error) = &result {
        let message = error.to_string();
        let _ = context
            .update_state(|state| {
                if let Some(server) = state
                    .provisioned_servers
                    .iter_mut()
                    .find(|server| server.instance_id == instance_id)
                {
                    server.display.last_apply_error = Some(message.clone());
                }
            })
            .await;
    }
    InstanceLifecycleService::release_lock(instance_id).await;
    result.map_err(Into::into)
}

#[tauri::command]
pub async fn reboot_instance_services(
    context: State<'_, AppContext>,
    instance_id: u64,
) -> Result<String, FrontendError> {
    InstanceLifecycleService::acquire_lock(instance_id, "reboot").await?;
    let result = async {
        let remote = build_remote_exec_for_instance(context.inner(), instance_id).await?;
        let target_user = context.config.audio_target_user.clone();
        let (endpoint_tx, endpoint_rx) = tokio::sync::watch::channel(remote.clone());
        let refresh_context = context.inner().clone();
        let base_remote = remote.clone();
        let endpoint_refresh = tokio::spawn(async move {
            loop {
                let snapshot = refresh_context.load_state().await;
                if !snapshot.credentials.vast_api_key.trim().is_empty() {
                    let vast = VastApiClient::new(
                        refresh_context.http_client.clone(),
                        refresh_context.config.vast_base_url.clone(),
                        snapshot.credentials.vast_api_key,
                    );
                    if let Ok(instances) = vast.list_instances().await {
                        if let Some(instance) =
                            instances.into_iter().find(|item| item.id == instance_id)
                        {
                            if !instance.ssh_host.trim().is_empty() && instance.ssh_port != 0 {
                                let updated_remote = RemoteExec {
                                    ssh_host: instance.ssh_host.clone(),
                                    ssh_port: instance.ssh_port,
                                    ..base_remote.clone()
                                };
                                let _ = endpoint_tx.send(updated_remote);
                                let _ = refresh_context
                                    .update_state(|state| {
                                        if let Some(server) = state
                                            .provisioned_servers
                                            .iter_mut()
                                            .find(|server| server.instance_id == instance_id)
                                        {
                                            server.ssh_host = instance.ssh_host.clone();
                                            server.ssh_port = instance.ssh_port;
                                            server.status = instance.status.clone();
                                        }
                                        if state.instance.instance_id == Some(instance_id) {
                                            state.instance.ssh_host = instance.ssh_host.clone();
                                            state.instance.ssh_port = instance.ssh_port;
                                            state.instance.status = instance.status.clone();
                                        }
                                    })
                                    .await;
                            }
                        }
                    }
                }
                tokio::time::sleep(Duration::from_secs(10)).await;
            }
        });
        let result = RebootHelperService::reboot_and_reinitialize_with_endpoint_updates(
            &remote,
            &target_user,
            endpoint_rx,
        )
        .await;
        endpoint_refresh.abort();
        result
    }
    .await;
    InstanceLifecycleService::release_lock(instance_id).await;
    result.map_err(Into::into)
}

#[tauri::command]
pub async fn list_microphones() -> Result<Vec<MicrophoneDevice>, FrontendError> {
    MicPassthroughService::list_devices().map_err(Into::into)
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
    MicPassthroughService::update_settings(context.inner(), instance_id, payload)
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
pub async fn mute_instance_mic(instance_id: u64) -> Result<(), FrontendError> {
    MicPassthroughService::set_muted(instance_id, true).map_err(Into::into)
}

#[tauri::command]
pub async fn unmute_instance_mic(instance_id: u64) -> Result<(), FrontendError> {
    MicPassthroughService::set_muted(instance_id, false).map_err(Into::into)
}

#[tauri::command]
pub async fn get_instance_mic_metrics(
    instance_id: u64,
) -> Result<serde_json::Value, FrontendError> {
    MicPassthroughService::get_local_metrics(instance_id).map_err(Into::into)
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
    let client = ReqwestGameStreamHttpClient::new(moonlight.secret_store.clone())
        .map_err(moonlight_frontend_error)?;
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
    context: State<'_, AppContext>,
    moonlight: State<'_, MoonlightManager>,
    input: MoonlightCompletePairingInput,
) -> Result<crate::moonlight::application::pairing::PairingResult, FrontendError> {
    ensure_embedded_identity_ready_for_explicit_action(context.inner(), moonlight.inner())
        .await
        .map_err(moonlight_frontend_error)?;
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
    let client = ReqwestGameStreamHttpClient::new(moonlight.secret_store.clone())
        .map_err(moonlight_frontend_error)?;
    let refresh_policy = if input.force_refresh {
        RefreshPolicy::ForceRefresh
    } else {
        RefreshPolicy::UseCacheIfFresh
    };
    apps::list_remote_apps(
        moonlight.repository.as_ref(),
        moonlight.secret_store.as_ref(),
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
    context: State<'_, AppContext>,
    moonlight: State<'_, MoonlightManager>,
    input: MoonlightStartStreamInput,
) -> Result<MoonlightStartStreamResponse, FrontendError> {
    ensure_embedded_identity_ready_for_explicit_action(context.inner(), moonlight.inner())
        .await
        .map_err(moonlight_frontend_error)?;
    let client = ReqwestGameStreamHttpClient::new(moonlight.secret_store.clone())
        .map_err(moonlight_frontend_error)?;
    let prepared = moonlight_launch::start_stream_request(
        moonlight.repository.as_ref(),
        moonlight.secret_store.as_ref(),
        &client,
        &input.host_id,
        input.app_id,
        input.session_preferences.as_ref(),
        input.replace_existing,
    )
    .await
    .map_err(moonlight_frontend_error)?;

    moonlight
        .input
        .set_mouse_mode(match prepared.preferences.input.mouse_mode {
            crate::moonlight::domain::MouseMode::Relative => {
                crate::input::state::MouseMode::Relative
            }
            crate::moonlight::domain::MouseMode::Absolute => {
                crate::input::state::MouseMode::Absolute
            }
        });
    moonlight.input.set_stream_dimensions(
        prepared.preferences.video.width,
        prepared.preferences.video.height,
    );

    let stream_window = create_or_reuse_stream_window(
        &app,
        prepared.preferences.video.width,
        prepared.preferences.video.height,
        &prepared.host_address,
    )
    .map_err(moonlight_frontend_error)?;
    set_native_stream_input_debug_overlay_enabled(
        app.state::<AppContext>()
            .state
            .read()
            .await
            .moonlight_preferences
            .show_input_debug_hud
            != 0,
    );
    install_native_stream_input(&stream_window, moonlight.input.clone())
        .map_err(moonlight_frontend_error)?;
    let surface =
        stream_window_surface_descriptor(&stream_window).map_err(moonlight_frontend_error)?;

    if let Err(error) = moonlight.runtime.attach_surface(surface).await {
        let _ = close_stream_window(&app);
        return Err(moonlight_frontend_error(error));
    }

    if let Ok(mut active_preferences) = moonlight.active_session_preferences.lock() {
        *active_preferences = Some(prepared.preferences.clone());
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
            session_generation: 0,
        })
        .await
    {
        let _ = moonlight.runtime.detach_surface().await;
        if let Ok(mut active_preferences) = moonlight.active_session_preferences.lock() {
            *active_preferences = None;
        }
        let _ = close_stream_window(&app);
        return Err(moonlight_frontend_error(error));
    }

    schedule_microphone_for_game_stream(context.inner(), moonlight.inner(), &input.host_id).await;

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
    stream_window.set_focus().map_err(|error| FrontendError {
        code: "moonlight_error".to_string(),
        message: "Moonlight operation failed".to_string(),
        details: Some(error.to_string()),
        retryable: false,
    })?;
    let preferred_capture_mode = match prepared.preferences.input.mouse_mode {
        crate::moonlight::domain::MouseMode::Relative => CaptureMouseMode::Relative,
        crate::moonlight::domain::MouseMode::Absolute => CaptureMouseMode::Absolute,
    };
    let _ = activate_native_stream_input(&stream_window, preferred_capture_mode);

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
        state: session_state_name(&state).to_string(),
        has_session_url: prepared.launch_result.rtsp_session_url.is_some(),
    })
}

#[tauri::command]
pub async fn moonlight_disconnect_stream(
    app: AppHandle,
    context: State<'_, AppContext>,
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
    moonlight.input.end_capture();
    if let Ok(mut active_preferences) = moonlight.active_session_preferences.lock() {
        *active_preferences = None;
    }
    close_stream_window(&app).map_err(moonlight_frontend_error)?;
    let mic_instance_id = moonlight
        .active_stream_instance_id
        .lock()
        .ok()
        .and_then(|mut instance| instance.take());
    if let Some(instance_id) = mic_instance_id {
        let mic_context = context.inner().clone();
        tauri::async_runtime::spawn(async move {
            if let Err(error) =
                MicPassthroughService::stop_for_game_stream(&mic_context, instance_id).await
            {
                warn!(instance_id, %error, "Microphone stop failed without affecting Moonlight disconnect");
            }
        });
    }
    let state = moonlight
        .runtime
        .get_state()
        .await
        .map_err(moonlight_frontend_error)?;
    Ok(MoonlightSessionStateResponse {
        state: session_state_name(&state).to_string(),
    })
}

#[tauri::command]
pub async fn moonlight_start_input_capture(
    app: AppHandle,
    moonlight: State<'_, MoonlightManager>,
    input: MoonlightStartInputCaptureInput,
) -> Result<bool, FrontendError> {
    let mode = parse_capture_mouse_mode(&input.mode)?;
    moonlight.input.begin_capture(mode);
    let Some(window) = app.get_window(crate::moonlight::platform::STREAM_WINDOW_LABEL) else {
        return Ok(false);
    };
    activate_native_stream_input(&window, mode).map_err(moonlight_frontend_error)
}

#[tauri::command]
pub async fn moonlight_stop_input_capture(
    app: AppHandle,
    moonlight: State<'_, MoonlightManager>,
) -> Result<bool, FrontendError> {
    moonlight.input.end_capture();
    let Some(window) = app.get_window(crate::moonlight::platform::STREAM_WINDOW_LABEL) else {
        return Ok(false);
    };
    deactivate_native_stream_input(&window).map_err(moonlight_frontend_error)
}

#[tauri::command]
pub async fn moonlight_update_video_geometry(
    moonlight: State<'_, MoonlightManager>,
    input: MoonlightVideoGeometryInput,
) -> Result<(), FrontendError> {
    moonlight
        .input
        .update_video_geometry(input.left, input.top, input.width, input.height);
    Ok(())
}

#[tauri::command]
pub async fn moonlight_activate_native_mouse_capture(
    app: AppHandle,
    moonlight: State<'_, MoonlightManager>,
) -> Result<bool, FrontendError> {
    let capture_mode = moonlight
        .active_session_preferences
        .lock()
        .ok()
        .and_then(|preferences| preferences.as_ref().map(|value| value.input.mouse_mode))
        .map(|mode| match mode {
            crate::moonlight::domain::MouseMode::Relative => CaptureMouseMode::Relative,
            crate::moonlight::domain::MouseMode::Absolute => CaptureMouseMode::Absolute,
        })
        .unwrap_or_else(|| moonlight.input.capture_state().mouse_mode);
    moonlight.input.begin_capture(capture_mode);
    let Some(window) = app.get_window(crate::moonlight::platform::STREAM_WINDOW_LABEL) else {
        return Ok(false);
    };
    activate_native_stream_input(&window, capture_mode).map_err(moonlight_frontend_error)
}

#[tauri::command]
pub async fn moonlight_deactivate_native_mouse_capture(
    app: AppHandle,
    moonlight: State<'_, MoonlightManager>,
) -> Result<bool, FrontendError> {
    moonlight.input.end_capture();
    let Some(window) = app.get_window(crate::moonlight::platform::STREAM_WINDOW_LABEL) else {
        return Ok(false);
    };
    deactivate_native_stream_input(&window).map_err(moonlight_frontend_error)
}

#[tauri::command]
pub async fn moonlight_quit_remote_app(
    context: State<'_, AppContext>,
    moonlight: State<'_, MoonlightManager>,
    host_id: String,
) -> Result<(), FrontendError> {
    ensure_embedded_identity_ready_for_explicit_action(context.inner(), moonlight.inner())
        .await
        .map_err(moonlight_frontend_error)?;
    let client = ReqwestGameStreamHttpClient::new(moonlight.secret_store.clone())
        .map_err(moonlight_frontend_error)?;
    moonlight_launch::quit_remote_app(
        moonlight.repository.as_ref(),
        moonlight.secret_store.as_ref(),
        &client,
        &host_id,
    )
    .await
    .map_err(moonlight_frontend_error)
}

#[tauri::command]
pub async fn moonlight_send_relative_mouse(
    moonlight: State<'_, MoonlightManager>,
    input: MoonlightRelativeMouseInput,
) -> Result<(), FrontendError> {
    moonlight
        .input
        .relative_motion(input.delta_x as i32, input.delta_y as i32);
    Ok(())
}

#[tauri::command]
pub async fn moonlight_send_absolute_mouse(
    moonlight: State<'_, MoonlightManager>,
    input: MoonlightAbsoluteMouseInput,
) -> Result<(), FrontendError> {
    moonlight.input.update_video_geometry(
        0.0,
        0.0,
        input.reference_width as f64,
        input.reference_height as f64,
    );
    moonlight
        .input
        .absolute_motion(input.x as f64, input.y as f64);
    Ok(())
}

#[tauri::command]
pub async fn moonlight_send_mouse_button(
    moonlight: State<'_, MoonlightManager>,
    input: MoonlightMouseButtonInput,
) -> Result<(), FrontendError> {
    let Some(button) = map_mouse_button(input.button) else {
        return Ok(());
    };
    moonlight
        .input
        .mouse_button(button, button_state(input.pressed));
    Ok(())
}

#[tauri::command]
pub async fn moonlight_send_keyboard(
    moonlight: State<'_, MoonlightManager>,
    input: MoonlightKeyboardInput,
) -> Result<(), FrontendError> {
    moonlight.input.key(
        input.virtual_key,
        button_state(input.pressed),
        input.modifiers,
        false,
    );
    Ok(())
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

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MoonlightHostLatencyPreferencesResponse {
    pub host_id: String,
    pub effective: StreamPreferences,
    pub overrides: Option<StreamPreferencesPatch>,
}

#[tauri::command]
pub async fn moonlight_get_host_latency_preferences(
    moonlight: State<'_, MoonlightManager>,
    host_id: String,
) -> Result<MoonlightHostLatencyPreferencesResponse, FrontendError> {
    let configuration =
        crate::moonlight::infrastructure::persistence::MoonlightStateRepository::snapshot(
            moonlight.repository.as_ref(),
        )
        .map_err(moonlight_frontend_error)?;

    let host = configuration
        .hosts
        .get(&host_id)
        .ok_or_else(|| {
            crate::moonlight::domain::MoonlightError::Validation(format!(
                "host {host_id} not found"
            ))
        })
        .map_err(moonlight_frontend_error)?;

    let effective = crate::moonlight::domain::merge_preferences(
        &configuration.defaults,
        host.preferences_override.as_ref(),
        None,
    );

    Ok(MoonlightHostLatencyPreferencesResponse {
        host_id,
        effective,
        overrides: host.preferences_override.clone(),
    })
}

#[tauri::command]
pub async fn moonlight_update_host_latency_preferences(
    moonlight: State<'_, MoonlightManager>,
    host_id: String,
    latency: Option<NolandLatencyConfigPatch>,
) -> Result<MoonlightHostLatencyPreferencesResponse, FrontendError> {
    let host = crate::moonlight::infrastructure::persistence::MoonlightStateRepository::update(
        moonlight.repository.as_ref(),
        |configuration| {
            let host = configuration.hosts.get_mut(&host_id).ok_or_else(|| {
                crate::moonlight::domain::MoonlightError::Validation(format!(
                    "host {host_id} not found"
                ))
            })?;

            if let Some(latency_patch) = latency {
                let mut overrides = host.preferences_override.clone().unwrap_or_default();
                overrides.latency = Some(latency_patch);
                host.preferences_override = Some(overrides);
            } else {
                host.preferences_override = None;
            }
            Ok(host.clone())
        },
    )
    .map_err(moonlight_frontend_error)?;

    let configuration =
        crate::moonlight::infrastructure::persistence::MoonlightStateRepository::snapshot(
            moonlight.repository.as_ref(),
        )
        .map_err(moonlight_frontend_error)?;
    let effective = crate::moonlight::domain::merge_preferences(
        &configuration.defaults,
        host.preferences_override.as_ref(),
        None,
    );
    validate_latency_config(&effective).map_err(moonlight_frontend_error)?;

    Ok(MoonlightHostLatencyPreferencesResponse {
        host_id,
        effective,
        overrides: host.preferences_override,
    })
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
pub async fn moonlight_get_active_input_mode(
    moonlight: State<'_, MoonlightManager>,
) -> Result<MoonlightActiveInputModeResponse, FrontendError> {
    let mouse_mode = moonlight
        .active_session_preferences
        .lock()
        .ok()
        .and_then(|preferences| preferences.as_ref().map(|prefs| prefs.input.mouse_mode))
        .map(|mode| match mode {
            crate::moonlight::domain::MouseMode::Relative => "relative".to_string(),
            crate::moonlight::domain::MouseMode::Absolute => "absolute".to_string(),
        });
    Ok(MoonlightActiveInputModeResponse { mouse_mode })
}

#[tauri::command]
pub async fn moonlight_get_input_debug_state(
    _moonlight: State<'_, MoonlightManager>,
) -> Result<MoonlightInputDebugStateResponse, FrontendError> {
    let native = crate::moonlight::platform::desktop_input::desktop_input_debug_snapshot();
    let worker = crate::input::worker::input_worker_debug_snapshot();

    Ok(MoonlightInputDebugStateResponse {
        capture_active: native.capture_active,
        capture_mode: native.capture_mode,
        capture_requests: native.capture_requests,
        native_mouse_moves: native.native_mouse_moves,
        native_mouse_downs: native.native_mouse_downs,
        native_mouse_ups: native.native_mouse_ups,
        native_keys: native.native_keys,
        rust_relative_callbacks: native.rust_relative_callbacks,
        rust_absolute_callbacks: native.rust_absolute_callbacks,
        rust_button_callbacks: native.rust_button_callbacks,
        rust_key_callbacks: native.rust_key_callbacks,
        relative_send_attempts: worker.relative_send_attempts,
        absolute_send_attempts: worker.absolute_send_attempts,
        button_send_attempts: worker.button_send_attempts,
        key_send_attempts: worker.key_send_attempts,
        scroll_send_attempts: worker.scroll_send_attempts,
        send_errors: worker.send_errors,
    })
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
        state: session_state_name(&state).to_string(),
    })
}

#[tauri::command]
pub async fn refresh_state_agent_index(
    context: State<'_, AppContext>,
    instance_id: u64,
) -> Result<serde_json::Value, FrontendError> {
    let api_key = {
        let state = context.state.read().await;
        state.credentials.vast_api_key.clone()
    };
    if api_key.trim().is_empty() {
        return Err(
            crate::errors::AppError::InvalidInput("Vast API key is missing.".to_string()).into(),
        );
    }
    let vast = crate::services::vast_api::VastApiClient::new(
        context.http_client.clone(),
        context.config.vast_base_url.clone(),
        api_key,
    );
    let remote = crate::services::instance_lifecycle::build_remote_exec_for_instance(
        context.inner(),
        &vast,
        instance_id,
    )
    .await
    .map_err(|e| crate::errors::AppError::State(e.to_string()))?;

    let target_user = context.config.audio_target_user.clone();
    ensure_state_agent(&remote, &target_user).await?;
    call_agent_raw(&remote, "RefreshIndex", serde_json::json!({}))
        .await
        .map_err(Into::into)
}
