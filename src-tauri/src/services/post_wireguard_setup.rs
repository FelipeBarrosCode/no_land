use std::{
    env,
    net::{SocketAddr, TcpStream, ToSocketAddrs},
    path::{Path, PathBuf},
    process::Command,
    time::Duration,
};

use serde::Serialize;
use tauri::AppHandle;
use crate::{
    errors::{AppError, AppResult},
    models::{
        app_state::{
            OrchestrationState, PostWireGuardSetupState, SetupErrorState, SetupStage,
            WireGuardSetupMode, WireGuardSetupStatus,
        },
        events::ProvisioningEvent,
    },
};

use super::{app_context::AppContext, moonlight::MoonlightService, os_detection::OsDetection};

const TUNNEL_HOST: &str = "10.77.0.1";
const SUNSHINE_API_PORT: u16 = 47990;
const REACHABILITY_PORTS: [u16; 3] = [47990, 47989, 47984];
const IMPORT_FILENAME: &str = "wireguard-app-import.conf";

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReachabilityResult {
    pub reachable: bool,
    pub host: String,
    pub checked_ports: Vec<u16>,
    pub reachable_ports: Vec<u16>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MoonlightDetectionResult {
    pub installed: bool,
    pub launch_kind: String,
    pub executable_path: Option<String>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SunshineVerificationResult {
    pub reachable: bool,
    pub authenticated: bool,
    pub host: String,
    pub port: u16,
    pub error: Option<String>,
}

pub async fn initialize_post_wireguard_flow(
    app: &AppHandle,
    context: &AppContext,
    instance_id: u64,
    config_path: &Path,
) -> AppResult<()> {
    let config_text = std::fs::read_to_string(config_path).map_err(|error| {
        AppError::Command(format!(
            "Failed reading generated WireGuard config {}: {error}",
            config_path.display()
        ))
    })?;
    let export_path = ensure_wireguard_import_copy(context, config_path)?;
    let export_path_text = export_path.display().to_string();
    let mode = platform_wireguard_mode();

    context
        .update_state(|state| {
            state.post_wireguard_setup = PostWireGuardSetupState {
                stage: SetupStage::WireguardConfigGenerated,
                wireguard_setup_mode: mode,
                wireguard_setup_status: WireGuardSetupStatus::ConfigGenerated,
                current_instance_id: Some(instance_id),
                wireguard_export_path: export_path_text.clone(),
                wireguard_config: config_text.clone(),
                wireguard_verified_host: TUNNEL_HOST.to_string(),
                wireguard_reachable_ports: Vec::new(),
                sunshine_username: state.credentials.app_username.clone(),
                moonlight_host: TUNNEL_HOST.to_string(),
                moonlight_installed: false,
                paired: false,
                setup_complete: false,
                last_error: None,
            };
            state.orchestration_state = OrchestrationState::WireGuardConfigGenerated;
            state.last_error = None;
        })
        .await?;

    emit_post_wireguard_event(
        app,
        context,
        OrchestrationState::WireGuardConfigGenerated,
        "WireGuard config generated",
        Some(format!(
            "Do not change provisioning logic before this point. New post-WireGuard setup flow starts here. Config saved at {}",
            export_path.display()
        )),
        false,
    )
    .await;

    Ok(())
}

pub async fn setup_wireguard_app_handoff(
    app: &AppHandle,
    context: &AppContext,
) -> AppResult<PostWireGuardSetupState> {
    let config_path = active_wireguard_config_path(context).await?;
    let export_path = ensure_wireguard_import_copy(context, &config_path)?;
    let export_path_text = export_path.display().to_string();
    open_wireguard_app()?;

    let os = OsDetection::new();
    if os.is_windows() || os.is_linux() {
        let _ = open_path_with_system(&export_path);
    }

    let next_state = if os.is_macos() {
        OrchestrationState::WireGuardWaitingForImport
    } else {
        OrchestrationState::WireGuardWaitingForActivation
    };
    let next_stage = if os.is_macos() {
        SetupStage::WireguardWaitingForImport
    } else {
        SetupStage::WireguardWaitingForActivation
    };
    let next_status = if os.is_macos() {
        WireGuardSetupStatus::WaitingForUserImport
    } else {
        WireGuardSetupStatus::WaitingForUserActivation
    };

    context
        .update_state(|state| {
            state.post_wireguard_setup.stage = SetupStage::WireguardAppHandoffStarted;
            state.post_wireguard_setup.wireguard_setup_status = WireGuardSetupStatus::AppHandoffStarted;
            state.post_wireguard_setup.wireguard_export_path = export_path_text.clone();
            state.orchestration_state = OrchestrationState::WireGuardAppHandoffStarted;
            state.post_wireguard_setup.last_error = None;
            state.last_error = None;
        })
        .await?;

    emit_post_wireguard_event(
        app,
        context,
        OrchestrationState::WireGuardAppHandoffStarted,
        "Opening WireGuard app",
        Some(format!("WireGuard config exported to {}", export_path.display())),
        false,
    )
    .await;

    let app_state = context
        .update_state(|state| {
            state.post_wireguard_setup.stage = next_stage;
            state.post_wireguard_setup.wireguard_setup_status = next_status;
            state.orchestration_state = next_state;
        })
        .await?;

    emit_post_wireguard_event(
        app,
        context,
        next_state,
        if os.is_macos() {
            "Import the generated tunnel into WireGuard, then activate it"
        } else {
            "Activate the imported WireGuard tunnel in the WireGuard app"
        },
        Some(format!("Target host: {}", TUNNEL_HOST)),
        false,
    )
    .await;

    Ok(app_state.post_wireguard_setup)
}

pub async fn verify_wireguard_connection(
    app: &AppHandle,
    context: &AppContext,
) -> AppResult<ReachabilityResult> {
    context
        .update_state(|state| {
            state.post_wireguard_setup.stage = SetupStage::WireguardVerifying;
            state.post_wireguard_setup.wireguard_setup_status = WireGuardSetupStatus::Verifying;
            state.orchestration_state = OrchestrationState::WireGuardVerifying;
            state.post_wireguard_setup.last_error = None;
            state.last_error = None;
        })
        .await?;

    emit_post_wireguard_event(
        app,
        context,
        OrchestrationState::WireGuardVerifying,
        "Verifying secure tunnel reachability",
        Some(format!("Checking {} on ports {:?}", TUNNEL_HOST, REACHABILITY_PORTS)),
        false,
    )
    .await;

    let result = tcp_reachability(TUNNEL_HOST, &REACHABILITY_PORTS, Duration::from_secs(2));
    if result.reachable {
        context
            .update_state(|state| {
                state.post_wireguard_setup.stage = SetupStage::WireguardConnected;
                state.post_wireguard_setup.wireguard_setup_status = WireGuardSetupStatus::Connected;
                state.post_wireguard_setup.wireguard_reachable_ports = result.reachable_ports.clone();
                state.orchestration_state = OrchestrationState::MoonlightSunshineReadyToSetup;
                state.last_error = None;
            })
            .await?;

        emit_post_wireguard_event(
            app,
            context,
            OrchestrationState::WireGuardConnected,
            "Connection verified",
            Some(format!("Reachable ports on {}: {:?}", TUNNEL_HOST, result.reachable_ports)),
            false,
        )
        .await;

        emit_post_wireguard_event(
            app,
            context,
            OrchestrationState::MoonlightSunshineReadyToSetup,
            "Secure tunnel connected",
            Some("Next, set up game streaming.".to_string()),
            false,
        )
        .await;
    } else {
        set_setup_failure(
            context,
            SetupStage::WireguardWaitingForActivation,
            OrchestrationState::WireGuardWaitingForActivation,
            "wireguard_unreachable",
            "We could not reach 10.77.0.1 yet. Make sure the WireGuard tunnel is imported and active.",
            result.error.clone(),
            true,
        )
        .await?;

        emit_post_wireguard_event(
            app,
            context,
            OrchestrationState::WireGuardWaitingForActivation,
            "WireGuard verification failed",
            result.error.clone(),
            true,
        )
        .await;
    }

    Ok(result)
}

pub async fn download_wireguard_config(context: &AppContext) -> AppResult<String> {
    let config_path = active_wireguard_config_path(context).await?;
    let export_path = ensure_wireguard_import_copy(context, &config_path)?;
    let export_text = export_path.display().to_string();
    context
        .update_state(|state| {
            state.post_wireguard_setup.wireguard_export_path = export_text.clone();
        })
        .await?;
    Ok(export_text)
}

pub fn open_wireguard_app() -> AppResult<()> {
    let os = OsDetection::new();
    let status = if os.is_macos() {
        Command::new("open").args(["-a", "WireGuard"]).status()
    } else if os.is_windows() {
        Command::new("cmd")
            .args(["/C", "start", "", "wireguard.exe"])
            .status()
    } else {
        let attempts = [
            ("wireguard", vec![]),
            ("wireguard-ui", vec![]),
            ("flatpak", vec!["run", "com.wireguard.WireGuard"]),
            ("xdg-open", vec!["wireguard://"]),
        ];
        let mut launched = None;
        for (program, args) in attempts {
            let status = Command::new(program).args(args).status();
            if status.as_ref().map(|value| value.success()).unwrap_or(false) {
                launched = Some(Ok(std::process::ExitStatus::default()));
                break;
            }
        }
        launched.unwrap_or_else(|| Err(std::io::Error::other("WireGuard app launch failed")))
    }
    .map_err(|error| AppError::Command(format!("Failed to open WireGuard app: {error}")))?;

    if !status.success() {
        return Err(AppError::Command(
            "WireGuard app did not launch successfully".to_string(),
        ));
    }

    Ok(())
}

pub async fn verify_sunshine_api(
    app: &AppHandle,
    context: &AppContext,
) -> AppResult<SunshineVerificationResult> {
    context
        .update_state(|state| {
            state.post_wireguard_setup.stage = SetupStage::SunshineVerifying;
            state.orchestration_state = OrchestrationState::SunshineVerifying;
            state.post_wireguard_setup.last_error = None;
        })
        .await?;

    emit_post_wireguard_event(
        app,
        context,
        OrchestrationState::SunshineVerifying,
        "Verifying Sunshine over the secure tunnel",
        Some(format!("Checking https://{}:{}/api/config", TUNNEL_HOST, SUNSHINE_API_PORT)),
        false,
    )
    .await;

    let (username, password) = {
        let state = context.state.read().await;
        (
            state.credentials.app_username.clone(),
            state.credentials.app_password.clone(),
        )
    };

    let client = reqwest::Client::builder()
        .danger_accept_invalid_certs(true)
        .build()
        .map_err(|error| AppError::Command(format!("Failed building Sunshine client: {error}")))?;
    let response = client
        .get(format!("https://{}:{}/api/config", TUNNEL_HOST, SUNSHINE_API_PORT))
        .basic_auth(username, Some(password))
        .send()
        .await;

    let result = match response {
        Ok(response) if response.status().is_success() => SunshineVerificationResult {
            reachable: true,
            authenticated: true,
            host: TUNNEL_HOST.to_string(),
            port: SUNSHINE_API_PORT,
            error: None,
        },
        Ok(response) if response.status() == reqwest::StatusCode::UNAUTHORIZED => {
            SunshineVerificationResult {
                reachable: true,
                authenticated: false,
                host: TUNNEL_HOST.to_string(),
                port: SUNSHINE_API_PORT,
                error: Some(
                    "Sunshine is reachable, but the stored credentials were rejected. Create or update your Sunshine login, then retry.".to_string(),
                ),
            }
        }
        Ok(response) => SunshineVerificationResult {
            reachable: false,
            authenticated: false,
            host: TUNNEL_HOST.to_string(),
            port: SUNSHINE_API_PORT,
            error: Some(format!("Sunshine API returned status {}", response.status())),
        },
        Err(error) => SunshineVerificationResult {
            reachable: false,
            authenticated: false,
            host: TUNNEL_HOST.to_string(),
            port: SUNSHINE_API_PORT,
            error: Some(error.to_string()),
        },
    };

    if !result.reachable || !result.authenticated {
        set_setup_failure(
            context,
            SetupStage::SunshineVerifying,
            OrchestrationState::SunshineVerifying,
            "sunshine_verify_failed",
            result
                .error
                .clone()
                .unwrap_or_else(|| "Sunshine verification failed".to_string())
                .as_str(),
            result.error.clone(),
            true,
        )
        .await?;
    }

    Ok(result)
}

pub async fn detect_moonlight_client(context: &AppContext) -> AppResult<MoonlightDetectionResult> {
    let moonlight = MoonlightService;
    let installed = moonlight.is_installed();
    let os = OsDetection::new();
    let launch_kind = if !installed {
        "unknown"
    } else if os.is_linux() {
        "path_lookup"
    } else {
        "native_path"
    };
    let result = MoonlightDetectionResult {
        installed,
        launch_kind: launch_kind.to_string(),
        executable_path: None,
        error: if installed {
            None
        } else {
            Some("Moonlight is not installed on this machine.".to_string())
        },
    };

    context
        .update_state(|state| {
            state.post_wireguard_setup.moonlight_installed = installed;
        })
        .await?;

    Ok(result)
}

pub async fn setup_moonlight_sunshine(
    app: &AppHandle,
    context: &AppContext,
) -> AppResult<PostWireGuardSetupState> {
    let reachability = tcp_reachability(TUNNEL_HOST, &REACHABILITY_PORTS, Duration::from_secs(2));
    if !reachability.reachable {
        set_setup_failure(
            context,
            SetupStage::WireguardConnected,
            OrchestrationState::WireGuardConnected,
            "wireguard_recheck_failed",
            "WireGuard is not reachable anymore. Reconnect the tunnel and retry.",
            reachability.error.clone(),
            true,
        )
        .await?;
        return Err(AppError::Provisioning(
            "WireGuard is not reachable anymore. Reconnect the tunnel and retry.".to_string(),
        ));
    }

    context
        .update_state(|state| {
            state.post_wireguard_setup.stage = SetupStage::SunshineCredentialsConfiguring;
            state.orchestration_state = OrchestrationState::SunshineCredentialsConfiguring;
            state.post_wireguard_setup.sunshine_username = state.credentials.app_username.clone();
            state.post_wireguard_setup.last_error = None;
        })
        .await?;

    emit_post_wireguard_event(
        app,
        context,
        OrchestrationState::SunshineCredentialsConfiguring,
        "Configuring Sunshine credentials",
        Some("Using the current platform username as the preferred Sunshine login.".to_string()),
        false,
    )
    .await;

    let sunshine = verify_sunshine_api(app, context).await?;
    if !sunshine.reachable || !sunshine.authenticated {
        return Err(AppError::Provisioning(
            sunshine
                .error
                .unwrap_or_else(|| "Sunshine verification failed".to_string()),
        ));
    }

    context
        .update_state(|state| {
            state.post_wireguard_setup.stage = SetupStage::MoonlightDetecting;
            state.orchestration_state = OrchestrationState::MoonlightDetecting;
        })
        .await?;

    emit_post_wireguard_event(
        app,
        context,
        OrchestrationState::MoonlightDetecting,
        "Finding Moonlight",
        Some("Looking for a local Moonlight client installation.".to_string()),
        false,
    )
    .await;

    let detection = detect_moonlight_client(context).await?;
    if !detection.installed {
        set_setup_failure(
            context,
            SetupStage::MoonlightDetecting,
            OrchestrationState::MoonlightDetecting,
            "moonlight_not_found",
            "Moonlight is not installed on this machine.",
            detection.error.clone(),
            true,
        )
        .await?;
        return Err(AppError::NotFound(
            "Moonlight is not installed on this machine.".to_string(),
        ));
    }

    let moonlight = MoonlightService;
    let preferences = { context.state.read().await.moonlight_preferences.clone() };
    moonlight
        .patch_local_config(TUNNEL_HOST, SUNSHINE_API_PORT, &preferences)
        .await?;
    let _ = moonlight.launch_native_client();

    let state = context
        .update_state(|state| {
            state.post_wireguard_setup.stage = SetupStage::MoonlightPairingStarted;
            state.orchestration_state = OrchestrationState::MoonlightPairingStarted;
            state.moonlight.host_address = TUNNEL_HOST.to_string();
            state.moonlight.configured = true;
            state.post_wireguard_setup.moonlight_host = TUNNEL_HOST.to_string();
        })
        .await?;

    emit_post_wireguard_event(
        app,
        context,
        OrchestrationState::MoonlightPairingStarted,
        "Starting Moonlight pairing",
        Some("Open Moonlight, add 10.77.0.1 if needed, and enter the PIN shown there below.".to_string()),
        false,
    )
    .await;

    Ok(state.post_wireguard_setup)
}

pub async fn submit_moonlight_pin_to_sunshine(
    app: &AppHandle,
    context: &AppContext,
    pin: String,
) -> AppResult<PostWireGuardSetupState> {
    let pin = pin.trim().to_string();
    if pin.len() < 4 || !pin.chars().all(|value| value.is_ascii_digit()) {
        return Err(AppError::InvalidInput(
            "Enter the PIN shown in Moonlight.".to_string(),
        ));
    }

    let (username, password) = {
        let state = context.state.read().await;
        (
            state.credentials.app_username.clone(),
            state.credentials.app_password.clone(),
        )
    };

    context
        .update_state(|state| {
            state.post_wireguard_setup.stage = SetupStage::MoonlightPinReceived;
            state.orchestration_state = OrchestrationState::MoonlightPinReceived;
        })
        .await?;

    emit_post_wireguard_event(
        app,
        context,
        OrchestrationState::MoonlightPinReceived,
        "Moonlight PIN received",
        Some("Submitting the PIN to Sunshine.".to_string()),
        false,
    )
    .await;

    context
        .update_state(|state| {
            state.post_wireguard_setup.stage = SetupStage::SunshinePinSubmitting;
            state.orchestration_state = OrchestrationState::SunshinePinSubmitting;
        })
        .await?;

    let client = reqwest::Client::builder()
        .danger_accept_invalid_certs(true)
        .build()
        .map_err(|error| AppError::Command(format!("Failed building Sunshine client: {error}")))?;
    let client_name = env::var("COMPUTERNAME")
        .or_else(|_| env::var("HOSTNAME"))
        .unwrap_or_else(|_| "machine".to_string());
    let response = client
        .post(format!("https://{}:{}/api/pin", TUNNEL_HOST, SUNSHINE_API_PORT))
        .basic_auth(username, Some(password))
        .json(&serde_json::json!({
            "pin": pin,
            "name": format!("Noland Client - {}", client_name)
        }))
        .send()
        .await
        .map_err(|error| AppError::Api(format!("Failed submitting Sunshine PIN: {error}")))?;

    if !response.status().is_success() {
        let error = format!("Sunshine rejected the PIN request with status {}", response.status());
        set_setup_failure(
            context,
            SetupStage::SunshinePinSubmitting,
            OrchestrationState::SunshinePinSubmitting,
            "sunshine_pin_rejected",
            &error,
            None,
            true,
        )
        .await?;
        return Err(AppError::Provisioning(error));
    }

    let state = context
        .update_state(|state| {
            state.post_wireguard_setup.stage = SetupStage::SetupComplete;
            state.post_wireguard_setup.paired = true;
            state.post_wireguard_setup.setup_complete = true;
            state.orchestration_state = OrchestrationState::Ready;
            state.last_error = None;
        })
        .await?;

    emit_post_wireguard_event(
        app,
        context,
        OrchestrationState::MoonlightSunshinePaired,
        "Moonlight and Sunshine paired",
        Some("Your secure streaming connection is ready.".to_string()),
        false,
    )
    .await;

    emit_post_wireguard_event(
        app,
        context,
        OrchestrationState::Ready,
        "Setup complete",
        Some("Your secure streaming connection is ready.".to_string()),
        false,
    )
    .await;

    Ok(state.post_wireguard_setup)
}

pub async fn get_setup_status(context: &AppContext) -> PostWireGuardSetupState {
    context.state.read().await.post_wireguard_setup.clone()
}

pub async fn retry_setup_stage(
    app: &AppHandle,
    context: &AppContext,
    stage: SetupStage,
) -> AppResult<PostWireGuardSetupState> {
    match stage {
        SetupStage::WireguardConfigGenerated
        | SetupStage::WireguardAppHandoffStarted
        | SetupStage::WireguardWaitingForImport
        | SetupStage::WireguardWaitingForActivation
        | SetupStage::WireguardVerifying
        | SetupStage::WireguardConnected => {
            let _ = setup_wireguard_app_handoff(app, context).await?;
            let _ = verify_wireguard_connection(app, context).await?;
        }
        SetupStage::MoonlightSunshineReadyToSetup
        | SetupStage::SunshineCredentialsConfiguring
        | SetupStage::SunshineVerifying
        | SetupStage::MoonlightDetecting
        | SetupStage::MoonlightPairingStarted
        | SetupStage::MoonlightPinReceived
        | SetupStage::SunshinePinSubmitting
        | SetupStage::MoonlightSunshinePaired
        | SetupStage::SetupComplete
        | SetupStage::Failed => {
            let _ = setup_moonlight_sunshine(app, context).await?;
        }
        SetupStage::PreWireguardExistingFlow => {}
    }

    Ok(context.state.read().await.post_wireguard_setup.clone())
}

fn platform_wireguard_mode() -> WireGuardSetupMode {
    let os = OsDetection::new();
    if os.is_macos() {
        WireGuardSetupMode::WireguardAppMacosManual
    } else if os.is_windows() {
        WireGuardSetupMode::WireguardAppWindows
    } else {
        WireGuardSetupMode::WireguardAppLinux
    }
}

async fn active_wireguard_config_path(context: &AppContext) -> AppResult<PathBuf> {
    let state = context.state.read().await;
    let mut candidate = PathBuf::from(state.wireguard.config_path.clone());
    if let Some(instance_id) = state.post_wireguard_setup.current_instance_id.or(state.instance.instance_id)
    {
        if let Some(path) = state
            .provisioned_servers
            .iter()
            .find(|record| record.instance_id == instance_id)
            .map(|record| PathBuf::from(record.wireguard_config_path.clone()))
            .filter(|path| path.exists())
        {
            candidate = path;
        }
    }

    if candidate.as_os_str().is_empty() || !candidate.exists() {
        return Err(AppError::NotFound(
            "Generated WireGuard config not found. Re-run provisioning from the WireGuard stage.".to_string(),
        ));
    }

    Ok(candidate)
}

fn ensure_wireguard_import_copy(context: &AppContext, config_path: &Path) -> AppResult<PathBuf> {
    let app_data_dir = dirs::data_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("com.noland.connect")
        .join("wireguard");
    std::fs::create_dir_all(&app_data_dir).map_err(|error| {
        AppError::Io(format!(
            "Failed creating WireGuard export directory {}: {error}",
            app_data_dir.display()
        ))
    })?;
    let export_path = app_data_dir.join(IMPORT_FILENAME);
    std::fs::copy(config_path, &export_path).map_err(|error| {
        AppError::Io(format!(
            "Failed exporting WireGuard config to {}: {error}",
            export_path.display()
        ))
    })?;
    Ok(export_path)
}

fn open_path_with_system(path: &Path) -> AppResult<()> {
    let os = OsDetection::new();
    let status = if os.is_macos() {
        Command::new("open").arg(path).status()
    } else if os.is_windows() {
        Command::new("cmd")
            .args(["/C", "start", "", path.to_string_lossy().as_ref()])
            .status()
    } else {
        Command::new("xdg-open").arg(path).status()
    }
    .map_err(|error| AppError::Command(format!("Failed opening {}: {error}", path.display())))?;

    if !status.success() {
        return Err(AppError::Command(format!(
            "System opener failed for {}",
            path.display()
        )));
    }
    Ok(())
}

fn tcp_reachability(host: &str, ports: &[u16], timeout: Duration) -> ReachabilityResult {
    let mut reachable_ports = Vec::new();
    let mut last_error = None;

    for port in ports {
        let Some(address) = resolve_socket_addr(host, *port) else {
            last_error = Some(format!("Could not resolve {host}:{port}"));
            continue;
        };

        match TcpStream::connect_timeout(&address, timeout) {
            Ok(stream) => {
                let _ = stream.shutdown(std::net::Shutdown::Both);
                reachable_ports.push(*port);
            }
            Err(error) => {
                last_error = Some(format!("{host}:{port} is unreachable: {error}"));
            }
        }
    }

    ReachabilityResult {
        reachable: !reachable_ports.is_empty(),
        host: host.to_string(),
        checked_ports: ports.to_vec(),
        reachable_ports,
        error: last_error,
    }
}

fn resolve_socket_addr(host: &str, port: u16) -> Option<SocketAddr> {
    (host, port).to_socket_addrs().ok()?.next()
}

async fn set_setup_failure(
    context: &AppContext,
    stage: SetupStage,
    orchestration_state: OrchestrationState,
    code: &str,
    message: &str,
    details: Option<String>,
    retryable: bool,
) -> AppResult<()> {
    let error = SetupErrorState {
        code: code.to_string(),
        message: message.to_string(),
        stage,
        retryable,
        details: details.clone(),
    };

    context
        .update_state(|state| {
            state.post_wireguard_setup.stage = SetupStage::Failed;
            state.post_wireguard_setup.last_error = Some(error.clone());
            state.post_wireguard_setup.wireguard_setup_status = if matches!(
                stage,
                SetupStage::WireguardConfigGenerated
                    | SetupStage::WireguardAppHandoffStarted
                    | SetupStage::WireguardWaitingForImport
                    | SetupStage::WireguardWaitingForActivation
                    | SetupStage::WireguardVerifying
                    | SetupStage::WireguardConnected
            ) {
                WireGuardSetupStatus::Failed
            } else {
                state.post_wireguard_setup.wireguard_setup_status
            };
            state.orchestration_state = orchestration_state;
            state.last_error = Some(message.to_string());
        })
        .await?;

    Ok(())
}

async fn emit_post_wireguard_event(
    app: &AppHandle,
    context: &AppContext,
    state: OrchestrationState,
    message: &str,
    details: Option<String>,
    is_error: bool,
) {
    let event = if is_error {
        ProvisioningEvent::error(state, message.to_string(), details)
    } else {
        ProvisioningEvent::info(state, message.to_string(), details)
    };
    context.emit_progress(app, event).await;
}

#[cfg(test)]
mod tests {
    use super::tcp_reachability;
    use std::{net::TcpListener, time::Duration};

    #[test]
    fn reachability_reports_open_port() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind test listener");
        let port = listener.local_addr().expect("listener addr").port();

        let result = tcp_reachability("127.0.0.1", &[port], Duration::from_millis(250));

        assert!(result.reachable);
        assert_eq!(result.reachable_ports, vec![port]);
    }

    #[test]
    fn reachability_reports_closed_port() {
        let result = tcp_reachability("127.0.0.1", &[9], Duration::from_millis(100));

        assert!(!result.reachable);
        assert!(result.reachable_ports.is_empty());
    }
}
