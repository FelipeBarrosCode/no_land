use std::sync::Arc;

use axum::{extract::State, Json};
use tracing::info;

use crate::api::error::ApiError;
use crate::{
    api::state::ApiState,
    models::app_state::{OnboardingPayload, OrchestrationState, PersistedAppState},
    services::{
        ssh_keys::SshKeyService,
        sunshine::{generate_headless_edid_base64, EDID_MAX_REFRESH_HZ, EDID_MIN_REFRESH_HZ},
        vast_api::VastApiClient,
    },
    utils::redact::redact_secret,
};

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CompleteOnboardingRequest {
    pub payload: OnboardingPayload,
}

pub async fn complete_onboarding(
    State(state): State<Arc<ApiState>>,
    Json(request): Json<CompleteOnboardingRequest>,
) -> Result<Json<crate::models::app_state::PersistedAppState>, ApiError> {
    let payload = request.payload;
    if payload.app_username.trim().len() < 3 {
        return Err(ApiError::from_frontend(crate::errors::AppError::InvalidInput(
            "Username must have at least 3 characters".to_string(),
        )
        .into()));
    }
    if payload.app_password.len() < 6 {
        return Err(ApiError::from_frontend(crate::errors::AppError::InvalidInput(
            "Password must have at least 6 characters".to_string(),
        )
        .into()));
    }
    if payload.vast_api_key.trim().len() < 16 {
        return Err(ApiError::from_frontend(crate::errors::AppError::InvalidInput(
            "Vast API key looks invalid".to_string(),
        )
        .into()));
    }

    info!(
        "onboarding submitted with api key {}",
        redact_secret(&payload.vast_api_key)
    );

    let app_data_root = state
        .context
        .state_store
        .path()
        .parent()
        .ok_or_else(|| ApiError::from_frontend(crate::errors::AppError::State("Unable to resolve app data directory".to_string()).into()))?
        .to_path_buf();

    let vast = VastApiClient::new(
        state.context.http_client.clone(),
        state.context.config.vast_base_url.clone(),
        payload.vast_api_key.clone(),
    );

    let ssh_service = SshKeyService::new("nolandConnectSSH");
    let key_paths = ssh_service
        .ensure_keypair(&app_data_root)
        .await
        .map_err(|e| ApiError::from_frontend(e.into()))?;
    let uploaded = ssh_service
        .upload_public_key_if_missing(&vast, &key_paths.public_key_path)
        .await
        .map_err(|e| ApiError::from_frontend(e.into()))?;

    let current_state = state.context.load_state().await;
    let existing_edid = current_state.sunshine.headless_edid_base64.clone();
    let edid_refresh = current_state
        .sunshine
        .edid_refresh_rate_hz
        .clamp(EDID_MIN_REFRESH_HZ, EDID_MAX_REFRESH_HZ);
    let generated_edid = if existing_edid.trim().is_empty() {
        generate_headless_edid_base64(
            current_state.moonlight_preferences.width,
            current_state.moonlight_preferences.height,
            edid_refresh,
        )
        .map_err(|e| ApiError::from_frontend(e.into()))?
    } else {
        existing_edid
    };

    let next_state: PersistedAppState = state
        .context
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
            state.sunshine.edid_source_label = "State Preferences".to_string();
            state.last_error = None;
        })
        .await
        .map_err(|e| ApiError::from_frontend(e.into()))?;

    Ok(Json(next_state))
}
