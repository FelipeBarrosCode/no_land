use std::sync::Arc;

use axum::{extract::State, Json};

use crate::{
    api::{error::ApiError, state::ApiState},
    models::{app_state::PersistedAppState, events::ProvisioningEvent},
    services::{orchestration::OrchestrationService, post_wireguard_setup::retry_setup_stage},
};

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StartExistingRequest {
    pub instance_id: u64,
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PinRequest {
    pub pin: String,
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RetryStageRequest {
    pub stage: crate::models::app_state::SetupStage,
}

pub async fn start_play(State(state): State<Arc<ApiState>>) -> Result<Json<bool>, ApiError> {
    OrchestrationService::start_play_flow(state.app.clone(), state.context.clone())
        .await
        .map_err(|e| ApiError::from_frontend(e.into()))?;
    Ok(Json(true))
}

pub async fn start_play_existing(
    State(state): State<Arc<ApiState>>,
    Json(payload): Json<StartExistingRequest>,
) -> Result<Json<bool>, ApiError> {
    OrchestrationService::start_play_for_existing_instance(
        state.app.clone(),
        state.context.clone(),
        payload.instance_id,
    )
    .await
    .map_err(|e| ApiError::from_frontend(e.into()))?;
    Ok(Json(true))
}

pub async fn submit_pairing_pin(
    State(state): State<Arc<ApiState>>,
    Json(payload): Json<PinRequest>,
) -> Result<Json<PersistedAppState>, ApiError> {
    OrchestrationService::submit_pairing_pin(&state.app, &state.context, payload.pin)
        .await
        .map_err(|e| ApiError::from_frontend(e.into()))?;
    Ok(Json(state.context.state.read().await.clone()))
}

pub async fn skip_pairing(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<PersistedAppState>, ApiError> {
    OrchestrationService::skip_pairing_and_continue(&state.app, &state.context)
        .await
        .map_err(|e| ApiError::from_frontend(e.into()))?;
    Ok(Json(state.context.state.read().await.clone()))
}

pub async fn get_logs(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<Vec<ProvisioningEvent>>, ApiError> {
    Ok(Json(state.context.provisioning_logs.read().await.clone()))
}

pub async fn retry_stage(
    State(state): State<Arc<ApiState>>,
    Json(payload): Json<RetryStageRequest>,
) -> Result<Json<crate::models::app_state::PostWireGuardSetupState>, ApiError> {
    let next = retry_setup_stage(&state.app, &state.context, payload.stage)
        .await
        .map_err(|e| ApiError::from_frontend(e.into()))?;
    Ok(Json(next))
}
