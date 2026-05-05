use std::{path::Path, process::Command, time::Duration};

use tauri::{AppHandle, State};

use crate::{
    errors::{AppError, FrontendError},
    models::{
        app_state::{
            LocationSource, ManualLocationInput, MoonlightPreferences, OnboardingPayload,
            OrchestrationState, PersistedAppState, RentedInstanceSummary, ServerPreferencesUpdate,
            SharedStorageSettingsResponse, SharedStorageSettingsUpdate, BackupStatusResponse,
            SharedStorageInstanceStatus, RestoreRequest, RestoreDryRunResult, RestoreJob,
            BundleIndex, InstanceMicConfig, InstanceMicRuntimeStatus, MicSessionResponse,
            MicSettingsUpdate, MicQualityProfile
        },
        events::ProvisioningEvent,
    },
    services::{
        app_context::AppContext, instance_lifecycle::InstanceLifecycleService,
        location::LocationService, offer_selector::OfferSelector,
        orchestration::OrchestrationService, reboot_helper::RebootHelperService,
        remote_exec::RemoteExec, ssh_keys::SshKeyService,
        shared_storage::shared_storage_manager::SharedStorageManager,
        shared_storage::bundle_indexer::BundleIndexer,
        shared_storage::bundle_restore::BundleRestoreService,
        mic_passthrough::MicPassthroughService,
        vast_api::VastApiClient,
        wireguard::setup_local_wireguard_client,
    },
    utils::redact::redact_secret,
};
use tracing::{info, warn};

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
    let sanitized_ssh_username = sanitize_ssh_username(&payload.app_username);

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
            state.ssh.ssh_username = sanitized_ssh_username.clone();
            state.ssh.ssh_password = payload.app_password.clone();
            state.orchestration_state = OrchestrationState::Idle;
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
        min_reliability: state_snapshot.server_preferences.min_reliability,
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
    OrchestrationService::start_play_flow(app, context.inner().clone()).await?;
    Ok(())
}

#[tauri::command]
pub async fn start_play_existing_instance(
    app: AppHandle,
    instance_id: u64,
    context: State<'_, AppContext>,
) -> Result<(), FrontendError> {
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
    let config_path = {
        let state = context.state.read().await;
        state.wireguard.config_path.clone()
    };

    if config_path.trim().is_empty() {
        return Err(AppError::InvalidInput(
            "WireGuard client config path is empty. Run provisioning first.".to_string(),
        )
        .into());
    }

    sync_wireguard_endpoint_from_vast(context.inner(), Path::new(&config_path)).await?;

    let message = setup_local_wireguard_client(Path::new(&config_path))?;
    let tunnel_server_ip = {
        let state = context.state.read().await;
        state.wireguard.server_ip.clone()
    };

    if let Ok(local_show) = Command::new("wg").arg("show").output() {
        let local_stdout = String::from_utf8_lossy(&local_show.stdout);
        if let Some(local_snapshot) = parse_wg_show(&local_stdout) {
            if !local_snapshot.allowed_ips.contains("10.77.0.1/32") {
                return Err(AppError::Provisioning(format!(
                    "Local WireGuard tunnel is not scoped to 10.77.0.1/32 (found: {})",
                    local_snapshot.allowed_ips
                ))
                .into());
            }

            if local_snapshot
                .latest_handshake
                .to_ascii_lowercase()
                .contains("never")
            {
                return Err(AppError::Provisioning(
                    "WireGuard tunnel is up but handshake has not completed yet. Check endpoint/port and firewall, then retry."
                        .to_string(),
                )
                .into());
            }

            let _ = context
                .update_state(|state| {
                    if !local_snapshot.interface_public_key.is_empty() {
                        state.wireguard.client_public_key =
                            local_snapshot.interface_public_key.clone();
                    }
                    if !local_snapshot.peer_public_key.is_empty() {
                        state.wireguard.server_public_key = local_snapshot.peer_public_key.clone();
                    }
                })
                .await;
        }
    }

    if !tunnel_server_ip.trim().is_empty() {
        validate_wireguard_ping(&tunnel_server_ip)?;
    }

    if let Err(error) = sync_server_wireguard_keys(context.inner()).await {
        warn!("best-effort server WireGuard key sync failed: {}", error);
    }

    let refreshed_state = context.state.read().await;
    Ok(format!(
        "{} | client_pub={} | server_pub={}",
        message,
        refreshed_state.wireguard.client_public_key,
        refreshed_state.wireguard.server_public_key
    ))
}

async fn sync_wireguard_endpoint_from_vast(
    context: &AppContext,
    config_path: &Path,
) -> Result<(), AppError> {
    let (instance_id, api_key) = {
        let state = context.state.read().await;
        (
            state.instance.instance_id,
            state.credentials.vast_api_key.clone(),
        )
    };

    let Some(instance_id) = instance_id else {
        return Ok(());
    };
    if api_key.trim().is_empty() {
        return Ok(());
    }

    let vast = VastApiClient::new(
        context.http_client.clone(),
        context.config.vast_base_url.clone(),
        api_key,
    );

    let instance = vast.get_instance(instance_id).await?;
    let endpoint_host = instance.wireguard_endpoint_host();

    if endpoint_host.trim().is_empty() {
        return Ok(());
    }

    if instance.wireguard_port == 0 {
        return Err(AppError::Provisioning(format!(
            "Vast instance {} does not expose a 51820/udp HostPort mapping. Pick a VM-enabled host with direct UDP ports and retry.",
            instance.id
        )));
    }

    let endpoint_line = format!("Endpoint = {}:{}", endpoint_host, instance.wireguard_port);
    rewrite_wireguard_peer_endpoint(config_path, &endpoint_line)?;
    Ok(())
}

fn rewrite_wireguard_peer_endpoint(
    config_path: &Path,
    endpoint_line: &str,
) -> Result<(), AppError> {
    let original = std::fs::read_to_string(config_path).map_err(|error| {
        AppError::Command(format!(
            "Failed reading WireGuard client config {}: {error}",
            config_path.display()
        ))
    })?;

    let mut in_peer_section = false;
    let mut endpoint_replaced = false;
    let mut lines = Vec::with_capacity(original.lines().count() + 1);

    for line in original.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') && trimmed.ends_with(']') {
            in_peer_section = trimmed.eq_ignore_ascii_case("[Peer]");
        }

        if in_peer_section && trimmed.to_ascii_lowercase().starts_with("endpoint") {
            lines.push(endpoint_line.to_string());
            endpoint_replaced = true;
        } else {
            lines.push(line.to_string());
        }
    }

    if !endpoint_replaced {
        return Err(AppError::InvalidInput(format!(
            "WireGuard client config {} is missing Endpoint in [Peer] section",
            config_path.display()
        )));
    }

    let mut updated = lines.join("\n");
    if original.ends_with('\n') {
        updated.push('\n');
    }

    if updated != original {
        std::fs::write(config_path, updated).map_err(|error| {
            AppError::Command(format!(
                "Failed writing WireGuard client config {}: {error}",
                config_path.display()
            ))
        })?;
    }

    Ok(())
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
    #[cfg(target_os = "windows")]
    let args = ["-n", "3", server_ip];

    #[cfg(not(target_os = "windows"))]
    let args = ["-c", "3", "-W", "2", server_ip];

    let ping = Command::new("ping").args(args).output().map_err(|error| {
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
    if cfg!(target_os = "windows") {
        return Ok(context.config.moonlight_download_url_windows.clone());
    }

    if cfg!(target_os = "macos") {
        return Ok(context.config.moonlight_download_url_macos.clone());
    }

    Ok(context.config.moonlight_download_url_linux.clone())
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
        Ok(instances) => instances,
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

fn sanitize_ssh_username(value: &str) -> String {
    value
        .trim()
        .trim_matches('"')
        .trim_matches('\'')
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

#[tauri::command]
pub async fn get_shared_storage_settings(
    context: State<'_, AppContext>,
) -> Result<SharedStorageSettingsResponse, FrontendError> {
    SharedStorageManager::get_settings(context.inner()).await.map_err(Into::into)
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
        state.instance.instance_id.ok_or_else(|| AppError::InvalidInput(
            "No active instance. Start provisioning first.".to_string(),
        ))?
    };
    SharedStorageManager::trigger_manual_backup(context.inner(), &remote, instance_id, &target_user).await?;
    SharedStorageManager::get_backup_status(context.inner()).await.map_err(Into::into)
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
        state.instance.instance_id.ok_or_else(|| AppError::InvalidInput(
            "No active instance.".to_string(),
        ))?
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
        state.instance.instance_id.ok_or_else(|| AppError::InvalidInput(
            "No active instance. Start provisioning first.".to_string(),
        ))?
    };
    SharedStorageManager::setup_scheduled_backup(context.inner(), &remote, instance_id, &target_user).await?;
    Ok("Hourly backup schedule configured".to_string())
}

#[tauri::command]
pub async fn remove_instance_backup_schedule(
    context: State<'_, AppContext>,
) -> Result<String, FrontendError> {
    let remote = build_remote_exec_from_state(context.inner()).await?;
    let target_user = context.config.audio_target_user.clone();
    SharedStorageManager::remove_scheduled_backup(&remote, &target_user).await?;
    Ok("Backup schedule removed".to_string())
}

#[tauri::command]
pub async fn get_instance_sunshine_settings(
    context: State<'_, AppContext>,
    instance_id: u64,
) -> Result<crate::services::instance_lifecycle::SunshineSettingsResponse, FrontendError> {
    InstanceLifecycleService::get_sunshine_settings(context.inner(), instance_id)
        .await
        .map_err(Into::into)
}

#[tauri::command]
pub async fn update_instance_sunshine_settings(
    context: State<'_, AppContext>,
    instance_id: u64,
    settings: std::collections::HashMap<String, serde_json::Value>,
) -> Result<(), FrontendError> {
    InstanceLifecycleService::update_sunshine_settings(
        context.inner(),
        instance_id,
        crate::services::instance_lifecycle::SunshineSettingsUpdatePayload { settings },
    )
    .await
    .map_err(Into::into)
}

#[tauri::command]
pub async fn reconnect_instance_wireguard(
    context: State<'_, AppContext>,
    instance_id: u64,
) -> Result<String, FrontendError> {
    InstanceLifecycleService::reconnect_wireguard(context.inner(), instance_id)
        .await
        .map_err(Into::into)
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
    _instance_id: u64,
) -> Result<String, FrontendError> {
    let remote = build_remote_exec_from_state(context.inner()).await?;
    let target_user = context.config.audio_target_user.clone();
    RebootHelperService::reboot_and_reinitialize(&remote, &target_user)
        .await
        .map_err(Into::into)
}

#[tauri::command]
pub async fn generate_bundle_index(
    context: State<'_, AppContext>,
) -> Result<(), FrontendError> {
    let remote = build_remote_exec_from_state(context.inner()).await?;
    let target_user = context.config.audio_target_user.clone();
    let instance_id = {
        let state = context.state.read().await;
        state.instance.instance_id.ok_or_else(|| AppError::InvalidInput(
            "No active instance. Start provisioning first.".to_string(),
        ))?
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
    BundleRestoreService::dry_run_restore(context.inner(), &remote, instance_id, &target_user, payload)
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
    BundleRestoreService::restore_bundle(context.inner(), &remote, instance_id, &target_user, payload)
        .await
        .map_err(Into::into)
}

#[tauri::command]
pub async fn get_restore_job(
    job_id: String,
) -> Result<RestoreJob, FrontendError> {
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
