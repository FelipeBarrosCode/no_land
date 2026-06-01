use std::sync::Arc;

use axum::{extract::State, Json};

use crate::{
    api::{error::ApiError, state::ApiState},
    models::app_state::{LocationSource, ManualLocationInput, PersistedAppState},
    services::location::LocationService,
};

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WrappedManualLocationRequest {
    pub payload: ManualLocationInput,
}

pub async fn refresh_ip_location(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<PersistedAppState>, ApiError> {
    let location_service = LocationService::new(state.context.http_client.clone());
    let detected = location_service
        .detect_ip_location()
        .await
        .map_err(|e| ApiError::from_frontend(e.into()))?;
    let next_state = state
        .context
        .update_state(|state| {
            state.location = detected;
            state.last_error = None;
        })
        .await
        .map_err(|e| ApiError::from_frontend(e.into()))?;
    Ok(Json(next_state))
}

pub async fn set_manual_location(
    State(state): State<Arc<ApiState>>,
    Json(request): Json<WrappedManualLocationRequest>,
) -> Result<Json<PersistedAppState>, ApiError> {
    let location = LocationService::from_manual(request.payload)
        .map_err(|e| ApiError::from_frontend(e.into()))?;
    let next_state = state
        .context
        .update_state(|state| {
            state.location = location;
            state.last_error = None;
        })
        .await
        .map_err(|e| ApiError::from_frontend(e.into()))?;
    Ok(Json(next_state))
}

pub async fn set_os_location(
    State(state): State<Arc<ApiState>>,
    Json(request): Json<WrappedManualLocationRequest>,
) -> Result<Json<PersistedAppState>, ApiError> {
    let location_service = LocationService::new(state.context.http_client.clone());
    let mut location = location_service
        .resolve_os_location(request.payload)
        .await
        .map_err(|e| ApiError::from_frontend(e.into()))?;
    location.source = LocationSource::Os;
    let next_state = state
        .context
        .update_state(|state| {
            state.location = location;
            state.last_error = None;
        })
        .await
        .map_err(|e| ApiError::from_frontend(e.into()))?;
    Ok(Json(next_state))
}
