use std::sync::Arc;

use axum::{extract::State, Json};

use crate::{
    api::{error::ApiError, state::ApiState},
    commands::{EdidSettingsUpdate, PlatformCredentialsUpdate, SshCredentialsUpdate},
    errors::AppError,
    models::app_state::{MoonlightPreferences, PersistedAppState, ServerPreferencesUpdate},
    services::sunshine::{generate_headless_edid_base64, EDID_MAX_REFRESH_HZ, EDID_MIN_REFRESH_HZ},
};

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VastApiKeyRequest {
    pub api_key: String,
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WrappedPlatformCredentialsRequest {
    pub payload: PlatformCredentialsUpdate,
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WrappedServerPreferencesRequest {
    pub payload: ServerPreferencesUpdate,
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WrappedMoonlightPreferencesRequest {
    pub payload: MoonlightPreferences,
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WrappedEdidRequest {
    pub payload: EdidSettingsUpdate,
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WrappedSshCredentialsRequest {
    pub payload: SshCredentialsUpdate,
}

pub async fn update_vast_api_key(
    State(state): State<Arc<ApiState>>,
    Json(payload): Json<VastApiKeyRequest>,
) -> Result<Json<PersistedAppState>, ApiError> {
    let trimmed = payload.api_key.trim().to_string();
    if trimmed.len() < 16 {
        return Err(ApiError::from_frontend(
            AppError::InvalidInput("Vast API key looks invalid".to_string()).into(),
        ));
    }
    let next_state = state
        .context
        .update_state(|state| {
            state.credentials.vast_api_key = trimmed;
            state.last_error = None;
        })
        .await
        .map_err(|e| ApiError::from_frontend(e.into()))?;
    Ok(Json(next_state))
}

pub async fn update_platform_credentials(
    State(state): State<Arc<ApiState>>,
    Json(request): Json<WrappedPlatformCredentialsRequest>,
) -> Result<Json<PersistedAppState>, ApiError> {
    let payload = request.payload;
    if payload.app_username.trim().len() < 3 {
        return Err(ApiError::from_frontend(
            AppError::InvalidInput("Username must have at least 3 characters".to_string()).into(),
        ));
    }
    if payload.app_password.len() < 6 {
        return Err(ApiError::from_frontend(
            AppError::InvalidInput("Password must have at least 6 characters".to_string()).into(),
        ));
    }
    let app_username = payload.app_username.trim().to_string();
    let app_password = payload.app_password;
    let next_state = state
        .context
        .update_state(|state| {
            state.credentials.app_username = app_username.clone();
            state.credentials.app_password = app_password.clone();
            state.last_error = None;
        })
        .await
        .map_err(|e| ApiError::from_frontend(e.into()))?;
    Ok(Json(next_state))
}

pub async fn update_server_preferences(
    State(state): State<Arc<ApiState>>,
    Json(request): Json<WrappedServerPreferencesRequest>,
) -> Result<Json<PersistedAppState>, ApiError> {
    let payload = request.payload;
    if payload.min_reliability < 0.8 || payload.min_reliability > 1.0 {
        return Err(ApiError::from_frontend(
            AppError::InvalidInput("Min reliability must be between 0.8 and 1".to_string()).into(),
        ));
    }
    if payload.storage_gb < 30 {
        return Err(ApiError::from_frontend(
            AppError::InvalidInput("Storage must be at least 30GB".to_string()).into(),
        ));
    }
    let next_state = state
        .context
        .update_state(|state| {
            state.server_preferences.min_reliability = payload.min_reliability;
            state.server_preferences.storage_gb = payload.storage_gb;
            state.server_preferences.template_hash = payload.template_hash.trim().to_string();
            state.server_preferences.max_hourly_price = payload.max_hourly_price.max(0.0);
            state.server_preferences.min_hourly_price = payload.min_hourly_price.max(0.0);
            state.server_preferences.require_verified = payload.require_verified;
            state.server_preferences.require_datacenter = payload.require_datacenter;
            state.server_preferences.include_on_demand = payload.include_on_demand;
            state.server_preferences.include_interruptible = payload.include_interruptible;
            state.server_preferences.include_reserved = payload.include_reserved;
            state.server_preferences.require_static_ip = payload.require_static_ip;
            state.server_preferences.require_avx = payload.require_avx;
            state.server_preferences.min_gpu_count = 1;
            state.server_preferences.min_gpu_ram_gb = payload.min_gpu_ram_gb;
            state.server_preferences.min_cpu_cores = payload.min_cpu_cores.max(0.0);
            state.server_preferences.min_inet_down_mbps = payload.min_inet_down_mbps.max(0.0);
            state.server_preferences.min_inet_up_mbps = payload.min_inet_up_mbps.max(0.0);
            state.server_preferences.geolocation_country_code =
                payload.geolocation_country_code.trim().to_uppercase();
            state.last_error = None;
        })
        .await
        .map_err(|e| ApiError::from_frontend(e.into()))?;
    Ok(Json(next_state))
}

pub async fn update_moonlight_preferences(
    State(state): State<Arc<ApiState>>,
    Json(request): Json<WrappedMoonlightPreferencesRequest>,
) -> Result<Json<PersistedAppState>, ApiError> {
    let next_state = state
        .context
        .update_state(|state| {
            state.moonlight_preferences = request.payload.clone();
            state.last_error = None;
        })
        .await
        .map_err(|e| ApiError::from_frontend(e.into()))?;
    Ok(Json(next_state))
}

pub async fn regenerate_edid(
    State(state): State<Arc<ApiState>>,
    Json(request): Json<WrappedEdidRequest>,
) -> Result<Json<PersistedAppState>, ApiError> {
    let payload = request.payload;
    if !(EDID_MIN_REFRESH_HZ..=EDID_MAX_REFRESH_HZ).contains(&payload.refresh_rate_hz) {
        return Err(ApiError::from_frontend(
            AppError::InvalidInput(format!(
                "EDID refresh rate must be between {} and {} Hz",
                EDID_MIN_REFRESH_HZ, EDID_MAX_REFRESH_HZ
            ))
            .into(),
        ));
    }
    let snapshot = state.context.load_state().await;
    let generated = generate_headless_edid_base64(
        snapshot.moonlight_preferences.width,
        snapshot.moonlight_preferences.height,
        payload.refresh_rate_hz,
    )
    .map_err(|e| ApiError::from_frontend(e.into()))?;
    let next_state = state
        .context
        .update_state(|state| {
            state.sunshine.edid_mode = payload.mode;
            state.sunshine.edid_refresh_rate_hz = payload.refresh_rate_hz;
            state.sunshine.headless_edid_base64 = generated.clone();
            state.last_error = None;
        })
        .await
        .map_err(|e| ApiError::from_frontend(e.into()))?;
    Ok(Json(next_state))
}

pub async fn update_ssh_credentials(
    State(state): State<Arc<ApiState>>,
    Json(request): Json<WrappedSshCredentialsRequest>,
) -> Result<Json<PersistedAppState>, ApiError> {
    let payload = request.payload;
    if payload.ssh_username.trim().is_empty() {
        return Err(ApiError::from_frontend(
            AppError::InvalidInput("SSH username cannot be empty".to_string()).into(),
        ));
    }
    if payload.ssh_password.len() < 4 {
        return Err(ApiError::from_frontend(
            AppError::InvalidInput("SSH password must have at least 4 characters".to_string()).into(),
        ));
    }
    let ssh_username = payload
        .ssh_username
        .trim()
        .trim_matches('"')
        .trim_matches('\'')
        .to_lowercase()
        .to_string();
    let next_state = state
        .context
        .update_state(|state| {
            state.ssh.ssh_username = ssh_username.clone();
            state.ssh.ssh_password = payload.ssh_password.clone();
            state.last_error = None;
        })
        .await
        .map_err(|e| ApiError::from_frontend(e.into()))?;
    Ok(Json(next_state))
}
