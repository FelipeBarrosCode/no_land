use std::{
    env,
    net::{SocketAddr, TcpStream, ToSocketAddrs},
    path::{Path, PathBuf},
    process::Command,
    time::Duration,
};

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
use serde::Serialize;
use tauri::AppHandle;
use tokio::time::{sleep, timeout};
use tracing::{info, warn};

use super::{
    app_context::AppContext,
    moonlight::{MoonlightConfigureOptions, MoonlightService},
    orchestration,
    os_detection::OsDetection,
    remote_exec::RemoteExec,
    ssh_keys::SshKeyService,
};

const TUNNEL_HOST: &str = "10.77.0.1";
const SUNSHINE_API_PORT: u16 = 47990;
const REACHABILITY_PORTS: [u16; 3] = [47990, 47989, 47984];
const IMPORT_FILENAME: &str = "wireguard-app-import.conf";
const SUNSHINE_API_READY_RETRIES: usize = 15;
const SUNSHINE_API_READY_POLL_INTERVAL: Duration = Duration::from_secs(1);
const SUNSHINE_TLS_RENEW_THRESHOLD_DAYS: i64 = 30;
const SUNSHINE_PRE_PIN_VERIFY_TIMEOUT: Duration = Duration::from_secs(60);

fn sunshine_http_client() -> AppResult<reqwest::Client> {
    reqwest::Client::builder()
        .danger_accept_invalid_certs(true)
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|error| AppError::Command(format!("Failed building Sunshine client: {error}")))
}

fn sunshine_api_url(path: &str) -> String {
    format!("https://{}:{}{}", TUNNEL_HOST, SUNSHINE_API_PORT, path)
}

#[derive(Debug, Clone)]
struct SunshineApiResponse {
    status: reqwest::StatusCode,
    location: Option<String>,
}

impl SunshineApiResponse {
    fn welcome_redirect(&self) -> bool {
        self.status.is_redirection()
            && self
                .location
                .as_deref()
                .map(|location| location.contains("/welcome"))
                .unwrap_or(false)
    }
}

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
    let previous_instance_id = {
        context
            .state
            .read()
            .await
            .post_wireguard_setup
            .current_instance_id
    };
    let config_text = std::fs::read_to_string(config_path).map_err(|error| {
        AppError::Command(format!(
            "Failed reading generated WireGuard config {}: {error}",
            config_path.display()
        ))
    })?;
    let export_path = ensure_wireguard_import_copy(context, config_path, instance_id)?;
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

    if previous_instance_id != Some(instance_id) {
        emit_post_wireguard_event(
            app,
            context,
            OrchestrationState::WireGuardConfigGenerated,
            "New instance detected",
            Some(
                "This is a different instance. WireGuard app setup and verification are required again before Sunshine/Moonlight setup."
                    .to_string(),
            ),
            false,
        )
        .await;
    }

    Ok(())
}

pub async fn setup_wireguard_app_handoff(
    app: &AppHandle,
    context: &AppContext,
) -> AppResult<PostWireGuardSetupState> {
    let os = OsDetection::new();
    open_wireguard_app()?;

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
            state.post_wireguard_setup.wireguard_setup_status =
                WireGuardSetupStatus::AppHandoffStarted;
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
        None,
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
        Some(format!(
            "Checking {} on ports {:?}",
            TUNNEL_HOST, REACHABILITY_PORTS
        )),
        false,
    )
    .await;

    let result = tcp_reachability(TUNNEL_HOST, &REACHABILITY_PORTS, Duration::from_secs(2));
    if result.reachable {
        context
            .update_state(|state| {
                state.post_wireguard_setup.stage = SetupStage::WireguardConnected;
                state.post_wireguard_setup.wireguard_setup_status = WireGuardSetupStatus::Connected;
                state.post_wireguard_setup.wireguard_reachable_ports =
                    result.reachable_ports.clone();
                state.orchestration_state = OrchestrationState::MoonlightSunshineReadyToSetup;
                state.last_error = None;
            })
            .await?;

        emit_post_wireguard_event(
            app,
            context,
            OrchestrationState::WireGuardConnected,
            "Connection verified",
            Some(format!(
                "Reachable ports on {}: {:?}",
                TUNNEL_HOST, result.reachable_ports
            )),
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
    let instance_id = active_instance_id(context).await?;
    sync_wireguard_config_snapshot(context, instance_id, &config_path).await?;
    let export_path = ensure_wireguard_import_copy(context, &config_path, instance_id)?;
    let export_text = export_path.display().to_string();
    sync_wireguard_config_snapshot(context, instance_id, &export_path).await?;
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
            if status
                .as_ref()
                .map(|value| value.success())
                .unwrap_or(false)
            {
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
        Some(format!(
            "Checking https://{}:{}/api/config",
            TUNNEL_HOST, SUNSHINE_API_PORT
        )),
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

    if username.trim().is_empty() || password.trim().is_empty() {
        set_setup_failure(
            context,
            SetupStage::SunshineCredentialsConfiguring,
            OrchestrationState::SunshineCredentialsConfiguring,
            "missing_platform_credentials",
            "Sunshine setup requires app username and password from state.json.",
            Some("Set platform credentials in onboarding/settings, then retry.".to_string()),
            true,
        )
        .await?;
        return Err(AppError::InvalidInput(
            "Sunshine setup requires app username/password from state.json.".to_string(),
        ));
    }

    let client = sunshine_http_client()?;
    let response = sunshine_config_response(&client, &username, &password).await;

    let result = match response {
        Ok(response) if response.status.is_success() => SunshineVerificationResult {
            reachable: true,
            authenticated: true,
            host: TUNNEL_HOST.to_string(),
            port: SUNSHINE_API_PORT,
            error: None,
        },
        Ok(response) if response.welcome_redirect() => SunshineVerificationResult {
            reachable: true,
            authenticated: false,
            host: TUNNEL_HOST.to_string(),
            port: SUNSHINE_API_PORT,
            error: Some(
                "Sunshine is still in its first-run welcome flow. Finish Sunshine setup on the host before pairing."
                    .to_string(),
            ),
        },
        Ok(response) if response.status == reqwest::StatusCode::UNAUTHORIZED => {
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
            error: Some(format!(
                "Sunshine API returned status {}{}",
                response.status,
                response
                    .location
                    .as_deref()
                    .map(|location| format!(" (location: {location})"))
                    .unwrap_or_default()
            )),
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
    let executable_path = moonlight
        .detected_executable_path()
        .map(|path| path.display().to_string());
    let installed = executable_path.is_some();
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
        executable_path,
        error: if installed {
            None
        } else {
            Some("Moonlight is not installed on this machine. Install the desktop app from the official website and ensure Moonlight.exe is in PATH or registered in App Paths.".to_string())
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
    let (active_instance_id, setup_instance_id) = {
        let state = context.state.read().await;
        (
            state.instance.instance_id,
            state.post_wireguard_setup.current_instance_id,
        )
    };

    if setup_instance_id.is_none() || active_instance_id != setup_instance_id {
        let error = "WireGuard setup context is for a different instance. Run WireGuard app setup again for this new provisioned instance.".to_string();
        set_setup_failure(
            context,
            SetupStage::WireguardConfigGenerated,
            OrchestrationState::WireGuardConfigGenerated,
            "wireguard_setup_required_for_new_instance",
            &error,
            Some(
                "The active instance changed, so previous WireGuard setup state cannot be reused."
                    .to_string(),
            ),
            true,
        )
        .await?;
        return Err(AppError::Provisioning(error));
    }

    context
        .update_state(|state| {
            state.post_wireguard_setup.stage = SetupStage::SunshineCredentialsConfiguring;
            state.orchestration_state = OrchestrationState::SunshineCredentialsConfiguring;
            state.post_wireguard_setup.wireguard_setup_status = WireGuardSetupStatus::Connected;
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

    let (username, password) = {
        let state = context.state.read().await;
        (
            state.credentials.app_username.clone(),
            state.credentials.app_password.clone(),
        )
    };

    emit_post_wireguard_event(
        app,
        context,
        OrchestrationState::SunshineCredentialsConfiguring,
        "Preparing Sunshine before pairing",
        Some(
            "Reapplying credentials and restarting Sunshine before asking for a Moonlight PIN."
                .to_string(),
        ),
        false,
    )
    .await;

    let sunshine = match timeout(SUNSHINE_PRE_PIN_VERIFY_TIMEOUT, async {
        repair_sunshine_auth_state(app, context, &username, &password, false).await?;

        emit_post_wireguard_event(
            app,
            context,
            OrchestrationState::SunshineVerifying,
            "Checking Sunshine TLS certificate",
            Some(
                "Generating or rotating certificate only if missing or expiring soon (30 days)."
                    .to_string(),
            ),
            false,
        )
        .await;

        let tls_action = ensure_sunshine_tls_certificate_over_ssh(context).await?;
        emit_post_wireguard_event(
            app,
            context,
            OrchestrationState::SunshineVerifying,
            "Sunshine TLS certificate status",
            Some(tls_action.clone()),
            false,
        )
        .await;

        let tls_cert_changed = !tls_action.contains("TLS_ACTION=skipped");
        let tls_config_changed = tls_action.contains("TLS_CONFIG_CHANGED=1");
        if tls_cert_changed || tls_config_changed {
            emit_post_wireguard_event(
                app,
                context,
                OrchestrationState::SunshineVerifying,
                "Restarting Sunshine after TLS update",
                Some(if tls_cert_changed {
                    "Applying new/rotated TLS certificate before pairing.".to_string()
                } else {
                    "Applying Sunshine TLS config changes before pairing.".to_string()
                }),
                false,
            )
            .await;
            restart_sunshine_service_over_ssh(context).await?;
        }

        verify_sunshine_api(app, context).await
    })
    .await
    {
        Ok(result) => result?,
        Err(_) => {
            let error = format!(
                "Sunshine verification took longer than {} seconds. Re-import the current WireGuard config and try again.",
                SUNSHINE_PRE_PIN_VERIFY_TIMEOUT.as_secs()
            );
            reset_to_wireguard_recovery_step(
                context,
                "sunshine_verify_timeout",
                &error,
                Some(
                    "The secure tunnel or Sunshine API did not become ready in time before Moonlight PIN setup."
                        .to_string(),
                ),
            )
            .await?;
            emit_post_wireguard_event(
                app,
                context,
                recovery_orchestration_state(context).await,
                "Sunshine verification timed out",
                Some(error.clone()),
                true,
            )
            .await;
            return Err(AppError::Provisioning(error));
        }
    };

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
    let config_result = moonlight
        .configure_client(MoonlightConfigureOptions {
            apply: true,
            ..Default::default()
        })
        .await;
    if !config_result.success {
        warn!(
            "moonlight auto-config apply returned non-success before pairing: {:?}",
            config_result.error
        );
    }

    let os = OsDetection::new();
    if os.is_windows() {
        if let Err(error) = moonlight.launch_native_client() {
            warn!(
                "windows moonlight auto-launch failed before pairing attempt: {}",
                error
            );
        }

        if let Err(error) = moonlight.pair_host(TUNNEL_HOST) {
            warn!(
                "windows moonlight auto-pair command failed for {}: {}",
                TUNNEL_HOST, error
            );
        }
    }

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
        Some(
            "Moonlight pairing was started for 10.77.0.1. Enter the PIN shown there below."
                .to_string(),
        ),
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

    let client = sunshine_http_client()?;
    let client_name = env::var("COMPUTERNAME")
        .or_else(|_| env::var("HOSTNAME"))
        .unwrap_or_else(|_| "machine".to_string());
    let response = submit_sunshine_pin_request(&client, &username, &password, &pin, &client_name)
        .await
        .map_err(|error| AppError::Api(format!("Failed submitting Sunshine PIN: {error}")))?;

    if response.welcome_redirect() {
        let error = "Sunshine is still in its first-run welcome flow after repair. Finish Sunshine setup on the host before submitting a Moonlight PIN.".to_string();
        let details = response
            .location
            .as_deref()
            .map(|location| format!("Sunshine redirected to {location}"));
        set_pin_retryable_failure(
            context,
            "sunshine_setup_incomplete",
            &error,
            details.clone(),
        )
        .await?;
        emit_post_wireguard_event(
            app,
            context,
            OrchestrationState::MoonlightPinReceived,
            "Sunshine is not ready yet for PIN submission",
            details,
            true,
        )
        .await;
        return Err(AppError::Provisioning(error));
    }

    if !response.status.is_success() {
        let error = format!(
            "Sunshine rejected the PIN request with status {}{}",
            response.status,
            response
                .location
                .as_deref()
                .map(|location| format!(" (location: {location})"))
                .unwrap_or_default()
        );
        set_pin_retryable_failure(
            context,
            &format!("sunshine_pin_rejected_status_{}", response.status.as_u16()),
            &error,
            None,
        )
        .await?;
        emit_post_wireguard_event(
            app,
            context,
            OrchestrationState::MoonlightPinReceived,
            "Sunshine rejected the PIN submission",
            Some(error.clone()),
            true,
        )
        .await;
        return Err(AppError::Provisioning(error));
    }

    {
        let mut pin_memory = context.pairing_pin_in_memory.write().await;
        *pin_memory = Some(pin.clone());
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

    let (instance_id, offer_id, status, ssh_host, ssh_port) = {
        let snapshot = context.state.read().await.clone();
        let instance_id = snapshot
            .post_wireguard_setup
            .current_instance_id
            .or(snapshot.instance.instance_id)
            .ok_or_else(|| {
                AppError::State(
                    "Missing instance id for post-WireGuard completion bookkeeping.".to_string(),
                )
            })?;
        (
            instance_id,
            snapshot.instance.offer_id,
            snapshot.instance.status,
            snapshot.instance.ssh_host,
            snapshot.instance.ssh_port,
        )
    };

    orchestration::mark_server_step_completed(
        context,
        instance_id,
        orchestration::ProvisionStepMarker::PairingCompleted,
        OrchestrationState::Ready,
        &status,
        &ssh_host,
        ssh_port,
        offer_id,
    )
    .await?;

    let (remote, _) = sunshine_ssh_remote(context).await?;
    if let Err(error) =
        orchestration::run_post_provision_step(app, context, &remote, instance_id, offer_id).await
    {
        warn!("Post-provision setup failed (non-fatal): {error}");
    }

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

async fn active_instance_id(context: &AppContext) -> AppResult<u64> {
    let state = context.state.read().await;
    state
        .instance
        .instance_id
        .or(state.post_wireguard_setup.current_instance_id)
        .ok_or_else(|| AppError::State("Missing active instance id for WireGuard flow".to_string()))
}

async fn sync_wireguard_config_snapshot(
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

    context
        .update_state(|state| {
            state.post_wireguard_setup.current_instance_id = Some(instance_id);
            state.post_wireguard_setup.wireguard_config = config_text.clone();
        })
        .await?;

    Ok(())
}

async fn active_wireguard_config_path(context: &AppContext) -> AppResult<PathBuf> {
    let state = context.state.read().await;
    let active_instance_id = state
        .instance
        .instance_id
        .or(state.post_wireguard_setup.current_instance_id)
        .ok_or_else(|| {
            AppError::State(
                "Missing active instance id for WireGuard config selection. Re-run provisioning."
                    .to_string(),
            )
        })?;

    let candidate = if let Some(path) = state
        .provisioned_servers
        .iter()
        .find(|record| record.instance_id == active_instance_id)
        .map(|record| PathBuf::from(record.wireguard_config_path.clone()))
        .filter(|path| path.exists())
    {
        path
    } else {
        return Err(AppError::NotFound(format!(
            "WireGuard config for active instance {} was not found. Regenerate WireGuard for this instance.",
            active_instance_id
        )));
    };

    if candidate.as_os_str().is_empty() || !candidate.exists() {
        return Err(AppError::NotFound(
            "Generated WireGuard config not found. Re-run provisioning from the WireGuard stage."
                .to_string(),
        ));
    }

    let instance_segment = format!("/{}/", active_instance_id);
    let normalized_candidate = candidate.to_string_lossy().replace('\\', "/");
    if !normalized_candidate.contains(&instance_segment) {
        return Err(AppError::Provisioning(format!(
            "WireGuard config {} does not belong to active instance {}. Regenerate WireGuard for this instance.",
            candidate.display(),
            active_instance_id
        )));
    }

    Ok(candidate)
}

fn ensure_wireguard_import_copy(
    _context: &AppContext,
    config_path: &Path,
    instance_id: u64,
) -> AppResult<PathBuf> {
    #[cfg(target_os = "macos")]
    let wireguard_dir_name = "wireguard-local";
    #[cfg(not(target_os = "macos"))]
    let wireguard_dir_name = "wireguard";

    let app_data_dir = dirs::data_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("com.noland.connect")
        .join(wireguard_dir_name)
        .join(instance_id.to_string());
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

async fn sunshine_config_response(
    client: &reqwest::Client,
    username: &str,
    password: &str,
) -> Result<SunshineApiResponse, reqwest::Error> {
    let response = client
        .get(sunshine_api_url("/api/config"))
        .basic_auth(username, Some(password))
        .send()
        .await?;
    Ok(SunshineApiResponse {
        status: response.status(),
        location: response
            .headers()
            .get(reqwest::header::LOCATION)
            .and_then(|value| value.to_str().ok())
            .map(|value| value.to_string()),
    })
}

async fn submit_sunshine_pin_request(
    client: &reqwest::Client,
    username: &str,
    password: &str,
    pin: &str,
    client_name: &str,
) -> Result<SunshineApiResponse, reqwest::Error> {
    let response = client
        .post(sunshine_api_url("/api/pin"))
        .basic_auth(username, Some(password))
        .json(&serde_json::json!({
            "pin": pin,
            "name": format!("Noland Client - {}", client_name)
        }))
        .send()
        .await?;
    Ok(SunshineApiResponse {
        status: response.status(),
        location: response
            .headers()
            .get(reqwest::header::LOCATION)
            .and_then(|value| value.to_str().ok())
            .map(|value| value.to_string()),
    })
}

async fn repair_sunshine_auth_state(
    app: &AppHandle,
    context: &AppContext,
    sunshine_username: &str,
    sunshine_password: &str,
    best_effort: bool,
) -> AppResult<()> {
    emit_post_wireguard_event(
        app,
        context,
        OrchestrationState::SunshineVerifying,
        "Repairing Sunshine auth state",
        Some("Reapplying Sunshine credentials before restarting the service.".to_string()),
        false,
    )
    .await;

    bootstrap_sunshine_credentials_over_ssh(context, sunshine_username, sunshine_password).await?;

    emit_post_wireguard_event(
        app,
        context,
        OrchestrationState::SunshineVerifying,
        "Restarting Sunshine service",
        Some("Refreshing Sunshine so the updated credentials take effect.".to_string()),
        false,
    )
    .await;

    restart_sunshine_service_over_ssh(context).await?;

    emit_post_wireguard_event(
        app,
        context,
        OrchestrationState::SunshineVerifying,
        "Verifying Sunshine API readiness",
        Some("Waiting for Sunshine to stop redirecting to the welcome page.".to_string()),
        false,
    )
    .await;

    let client = sunshine_http_client()?;
    for attempt in 1..=SUNSHINE_API_READY_RETRIES {
        match sunshine_config_response(&client, sunshine_username, sunshine_password).await {
            Ok(response) if response.status.is_success() => {
                info!(attempt, "Sunshine API became ready after restart");
                return Ok(());
            }
            Ok(response) => {
                info!(
                    attempt,
                    status = %response.status,
                    location = response.location.as_deref().unwrap_or(""),
                    "Sunshine API not ready yet after restart"
                );
                if !best_effort
                    && !response.welcome_redirect()
                    && response.status != reqwest::StatusCode::UNAUTHORIZED
                {
                    return Err(AppError::Provisioning(format!(
                        "Sunshine API returned status {}{} while waiting for auth readiness.",
                        response.status,
                        response
                            .location
                            .as_deref()
                            .map(|location| format!(" (location: {location})"))
                            .unwrap_or_default()
                    )));
                }
            }
            Err(error) => {
                info!(attempt, %error, "Sunshine API not reachable yet after restart");
                if !best_effort && attempt == SUNSHINE_API_READY_RETRIES {
                    return Err(AppError::Provisioning(format!(
                        "Sunshine did not become ready after restart: {error}"
                    )));
                }
            }
        }

        sleep(SUNSHINE_API_READY_POLL_INTERVAL).await;
    }

    Err(AppError::Provisioning(
        "Sunshine stayed in its welcome/auth flow after credentials were reapplied and the service restarted."
            .to_string(),
    ))
}

async fn sunshine_ssh_remote(context: &AppContext) -> AppResult<(RemoteExec, String)> {
    let (private_key_path, key_passphrase, ssh_host, ssh_port, ssh_user, sunshine_user) = {
        let state = context.state.read().await;
        let ssh_user = if state.ssh.ssh_username.trim().is_empty() {
            context.config.ssh_user.clone()
        } else {
            state.ssh.ssh_username.clone()
        };
        (
            state.ssh.private_key_path.clone(),
            state.credentials.app_password.clone(),
            state.instance.ssh_host.clone(),
            state.instance.ssh_port,
            ssh_user,
            context.config.audio_target_user.clone(),
        )
    };

    if private_key_path.trim().is_empty() || ssh_host.trim().is_empty() || ssh_port == 0 {
        return Err(AppError::State(
            "Cannot manage Sunshine over SSH because SSH connection details are missing."
                .to_string(),
        ));
    }

    SshKeyService::new("nolandConnectSSH")
        .load_key_into_agent(Path::new(&private_key_path), &key_passphrase)
        .await?;

    Ok((
        RemoteExec {
            ssh_user: sanitize_ssh_user(&ssh_user),
            ssh_host,
            ssh_port,
            private_key_path,
        },
        sanitize_ssh_user(&sunshine_user),
    ))
}

async fn restart_sunshine_service_over_ssh(context: &AppContext) -> AppResult<()> {
    let (remote, _) = sunshine_ssh_remote(context).await?;
    let output = tokio::task::spawn_blocking(move || {
        remote.ssh(
            "systemctl restart sunshine && sleep 2 && systemctl is-active sunshine",
            Duration::from_secs(30),
        )
    })
    .await
    .map_err(|error| {
        AppError::Command(format!("Failed to join Sunshine restart task: {error}"))
    })??;

    if output.status_code != 0 || output.stdout.trim() != "active" {
        return Err(AppError::Provisioning(format!(
            "Failed restarting Sunshine service: stdout: {} | stderr: {}",
            output.stdout.trim(),
            output.stderr.trim()
        )));
    }

    Ok(())
}

async fn ensure_sunshine_tls_certificate_over_ssh(context: &AppContext) -> AppResult<String> {
    let (remote, sunshine_user) = sunshine_ssh_remote(context).await?;
    let command = format!(
        "sudo bash -lc 'set -euo pipefail; TARGET_USER=\"{sunshine_user}\"; TARGET_GROUP=$(id -gn \"$TARGET_USER\"); TARGET_HOME=$(getent passwd \"$TARGET_USER\" | cut -d: -f6); CERT_DIR=\"/etc/sunshine/certs\"; CERT_PATH=\"$CERT_DIR/sunshine.crt\"; KEY_PATH=\"$CERT_DIR/sunshine.key\"; CNF_PATH=\"$CERT_DIR/sunshine-san.cnf\"; THRESHOLD_SECS=$(( {threshold_days} * 86400 )); mkdir -p \"$CERT_DIR\"; chown root:\"$TARGET_GROUP\" \"$CERT_DIR\"; chmod 750 \"$CERT_DIR\"; ACTION=skipped; NEEDS_GEN=0; CONFIG_CHANGED=0; if [ ! -s \"$CERT_PATH\" ] || [ ! -s \"$KEY_PATH\" ]; then NEEDS_GEN=1; ACTION=generated_missing; else END_DATE=$(openssl x509 -in \"$CERT_PATH\" -noout -enddate 2>/dev/null | cut -d= -f2 || true); if [ -z \"$END_DATE\" ]; then NEEDS_GEN=1; ACTION=generated_invalid; else END_EPOCH=$(date -d \"$END_DATE\" +%s 2>/dev/null || echo 0); NOW_EPOCH=$(date +%s); if [ \"$END_EPOCH\" -le 0 ] || [ $((END_EPOCH - NOW_EPOCH)) -lt $THRESHOLD_SECS ]; then NEEDS_GEN=1; ACTION=rotated_expiring; fi; fi; fi; if [ \"$NEEDS_GEN\" = \"1\" ]; then HOSTNAME_SHORT=$(hostname -s 2>/dev/null || echo sunshine-host); cat > \"$CNF_PATH\" <<EOF\n[req]\ndefault_bits       = 4096\nprompt             = no\ndefault_md         = sha256\ndistinguished_name = dn\nx509_extensions    = v3_req\n\n[dn]\nCN = $HOSTNAME_SHORT\n\n[v3_req]\nsubjectAltName = @alt_names\nkeyUsage = critical, digitalSignature, keyEncipherment\nextendedKeyUsage = serverAuth\nbasicConstraints = critical, CA:false\n\n[alt_names]\nDNS.1 = localhost\nDNS.2 = $HOSTNAME_SHORT\nIP.1 = 127.0.0.1\nIP.2 = 10.77.0.1\nEOF\nopenssl req -x509 -nodes -days 825 -newkey rsa:4096 -keyout \"$KEY_PATH\" -out \"$CERT_PATH\" -config \"$CNF_PATH\" >/dev/null 2>&1; fi; chmod 644 \"$CERT_PATH\" \"$CNF_PATH\"; chmod 640 \"$KEY_PATH\"; chown root:\"$TARGET_GROUP\" \"$CERT_PATH\" \"$KEY_PATH\" \"$CNF_PATH\"; SUN_CONF=\"$TARGET_HOME/.config/sunshine/sunshine.conf\"; sudo -u \"$TARGET_USER\" mkdir -p \"$TARGET_HOME/.config/sunshine\"; sudo -u \"$TARGET_USER\" touch \"$SUN_CONF\"; if grep -q \"^bind_address[[:space:]]*=\" \"$SUN_CONF\"; then sed -i \"/^bind_address[[:space:]]*=/d\" \"$SUN_CONF\"; CONFIG_CHANGED=1; fi; if grep -q \"^cert[[:space:]]*=\" \"$SUN_CONF\"; then if ! grep -q \"^cert[[:space:]]*=[[:space:]]*$CERT_PATH$\" \"$SUN_CONF\"; then sed -i \"s|^cert[[:space:]]*=.*|cert = $CERT_PATH|\" \"$SUN_CONF\"; CONFIG_CHANGED=1; fi; else echo \"cert = $CERT_PATH\" >> \"$SUN_CONF\"; CONFIG_CHANGED=1; fi; if grep -q \"^pkey[[:space:]]*=\" \"$SUN_CONF\"; then if ! grep -q \"^pkey[[:space:]]*=[[:space:]]*$KEY_PATH$\" \"$SUN_CONF\"; then sed -i \"s|^pkey[[:space:]]*=.*|pkey = $KEY_PATH|\" \"$SUN_CONF\"; CONFIG_CHANGED=1; fi; else echo \"pkey = $KEY_PATH\" >> \"$SUN_CONF\"; CONFIG_CHANGED=1; fi; chown \"$TARGET_USER:$TARGET_GROUP\" \"$SUN_CONF\"; END_DATE2=$(openssl x509 -in \"$CERT_PATH\" -noout -enddate 2>/dev/null | cut -d= -f2 || true); if [ -n \"$END_DATE2\" ]; then END_EPOCH2=$(date -d \"$END_DATE2\" +%s 2>/dev/null || echo 0); NOW_EPOCH2=$(date +%s); if [ \"$END_EPOCH2\" -gt 0 ]; then DAYS_LEFT=$(( (END_EPOCH2 - NOW_EPOCH2) / 86400 )); else DAYS_LEFT=-1; fi; else DAYS_LEFT=-1; fi; echo \"TLS_ACTION=$ACTION\"; echo \"TLS_CONFIG_CHANGED=$CONFIG_CHANGED\"; echo \"TLS_CERT_PATH=$CERT_PATH\"; echo \"TLS_KEY_PATH=$KEY_PATH\"; echo \"TLS_DAYS_LEFT=$DAYS_LEFT\"; echo \"TLS_BIND_ADDRESS=cleared\"'",
        sunshine_user = sunshine_user,
        threshold_days = SUNSHINE_TLS_RENEW_THRESHOLD_DAYS,
    );

    let output = tokio::task::spawn_blocking(move || remote.ssh(&command, Duration::from_secs(60)))
        .await
        .map_err(|error| {
            AppError::Command(format!("Failed to join Sunshine TLS setup task: {error}"))
        })??;

    if output.status_code != 0 {
        return Err(AppError::Provisioning(format!(
            "Failed ensuring Sunshine TLS certificate: stdout: {} | stderr: {}",
            output.stdout.trim(),
            output.stderr.trim()
        )));
    }

    let summary = output
        .stdout
        .lines()
        .filter(|line| line.starts_with("TLS_"))
        .collect::<Vec<_>>()
        .join(" | ");

    if summary.is_empty() {
        return Err(AppError::Provisioning(
            "Sunshine TLS setup did not produce expected TLS summary output.".to_string(),
        ));
    }

    Ok(summary)
}

async fn bootstrap_sunshine_credentials_over_ssh(
    context: &AppContext,
    sunshine_username: &str,
    sunshine_password: &str,
) -> AppResult<()> {
    let (remote, sunshine_user) = sunshine_ssh_remote(context).await?;
    let escaped_username = shell_single_quote_escape(sunshine_username);
    let escaped_password = shell_single_quote_escape(sunshine_password);
    let command = format!(
        "sudo -u {sunshine_user} bash -lc 'sunshine --creds '\''{username}'\'' '\''{password}'\'''",
        sunshine_user = sunshine_user,
        username = escaped_username,
        password = escaped_password,
    );
    let output = tokio::task::spawn_blocking(move || remote.ssh(&command, Duration::from_secs(30)))
        .await
        .map_err(|error| {
            AppError::Command(format!(
                "Failed to join Sunshine credential bootstrap task: {error}"
            ))
        })??;

    if output.status_code != 0 {
        return Err(AppError::Provisioning(format!(
            "Failed to set Sunshine credentials over SSH: stdout: {} | stderr: {}",
            output.stdout.trim(),
            output.stderr.trim()
        )));
    }

    Ok(())
}

fn sanitize_ssh_user(value: &str) -> String {
    value
        .trim()
        .trim_matches('"')
        .trim_matches('\'')
        .to_string()
}

fn shell_single_quote_escape(content: &str) -> String {
    content.replace('\'', "'\\''")
}

fn recovery_stage_for_mode(mode: WireGuardSetupMode) -> SetupStage {
    match mode {
        WireGuardSetupMode::WireguardAppMacosManual => SetupStage::WireguardWaitingForImport,
        WireGuardSetupMode::WireguardAppWindows | WireGuardSetupMode::WireguardAppLinux => {
            SetupStage::WireguardWaitingForActivation
        }
    }
}

fn recovery_status_for_stage(stage: SetupStage) -> WireGuardSetupStatus {
    match stage {
        SetupStage::WireguardWaitingForImport => WireGuardSetupStatus::WaitingForUserImport,
        SetupStage::WireguardWaitingForActivation => WireGuardSetupStatus::WaitingForUserActivation,
        _ => WireGuardSetupStatus::AppHandoffStarted,
    }
}

fn recovery_orchestration_state_for_stage(stage: SetupStage) -> OrchestrationState {
    match stage {
        SetupStage::WireguardWaitingForImport => OrchestrationState::WireGuardWaitingForImport,
        SetupStage::WireguardWaitingForActivation => {
            OrchestrationState::WireGuardWaitingForActivation
        }
        _ => OrchestrationState::WireGuardAppHandoffStarted,
    }
}

async fn recovery_orchestration_state(context: &AppContext) -> OrchestrationState {
    let mode = context
        .state
        .read()
        .await
        .post_wireguard_setup
        .wireguard_setup_mode;
    recovery_orchestration_state_for_stage(recovery_stage_for_mode(mode))
}

async fn reset_to_wireguard_recovery_step(
    context: &AppContext,
    code: &str,
    message: &str,
    details: Option<String>,
) -> AppResult<()> {
    let error = SetupErrorState {
        code: code.to_string(),
        message: message.to_string(),
        stage: SetupStage::SunshineVerifying,
        retryable: true,
        details: details.clone(),
    };

    context
        .update_state(|state| {
            let recovery_stage =
                recovery_stage_for_mode(state.post_wireguard_setup.wireguard_setup_mode);
            state.post_wireguard_setup.stage = recovery_stage;
            state.post_wireguard_setup.wireguard_setup_status =
                recovery_status_for_stage(recovery_stage);
            state.post_wireguard_setup.last_error = Some(error.clone());
            state.post_wireguard_setup.setup_complete = false;
            state.post_wireguard_setup.paired = false;
            state.orchestration_state = recovery_orchestration_state_for_stage(recovery_stage);
            state.last_error = Some(message.to_string());
        })
        .await?;

    Ok(())
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

async fn set_pin_retryable_failure(
    context: &AppContext,
    code: &str,
    message: &str,
    details: Option<String>,
) -> AppResult<()> {
    let error = SetupErrorState {
        code: code.to_string(),
        message: message.to_string(),
        stage: SetupStage::SunshinePinSubmitting,
        retryable: true,
        details: details.clone(),
    };

    context
        .update_state(|state| {
            state.post_wireguard_setup.stage = SetupStage::MoonlightPinReceived;
            state.post_wireguard_setup.last_error = Some(error.clone());
            state.orchestration_state = OrchestrationState::MoonlightPinReceived;
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
