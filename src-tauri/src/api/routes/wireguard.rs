use std::{path::Path as FsPath, sync::Arc};

use axum::{extract::{Path, State}, Json};

use crate::{
    api::{error::ApiError, state::ApiState},
    errors::AppError,
    models::app_state::PostWireGuardSetupState,
    services::{
        os_detection::OsDetection,
        post_wireguard_setup::{
            download_wireguard_config, get_setup_status, open_wireguard_app,
            setup_wireguard_app_handoff, verify_wireguard_connection, ReachabilityResult,
        },
    },
};

fn resolve_config_path(state: &crate::models::app_state::PersistedAppState) -> String {
    if let Some(instance_id) = state.instance.instance_id {
        if let Some(path) = state
            .provisioned_servers
            .iter()
            .find(|record| record.instance_id == instance_id)
            .map(|record| record.wireguard_config_path.clone())
            .filter(|path| FsPath::new(path).exists())
        {
            return path;
        }
    }
    state.wireguard.config_path.clone()
}

pub async fn setup_wireguard_client(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<String>, ApiError> {
    let config_path = resolve_config_path(&state.context.state.read().await.clone());
    if config_path.trim().is_empty() {
        return Err(ApiError::from_frontend(
            AppError::InvalidInput("WireGuard client config path is empty. Run provisioning first.".to_string()).into(),
        ));
    }
    if !FsPath::new(&config_path).exists() {
        return Err(ApiError::from_frontend(
            AppError::NotFound(format!("WireGuard client config not found at {}", config_path)).into(),
        ));
    }
    open_wireguard_app().map_err(|e| ApiError::from_frontend(e.into()))?;
    Ok(Json("WireGuard app opened. Import and activate the generated tunnel there.".to_string()))
}

pub async fn reconnect_local_wireguard_client(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<String>, ApiError> {
    let config_path = resolve_config_path(&state.context.state.read().await.clone());
    if config_path.trim().is_empty() {
        return Err(ApiError::from_frontend(
            AppError::InvalidInput("WireGuard client config path is empty. Run provisioning first.".to_string()).into(),
        ));
    }
    if !FsPath::new(&config_path).exists() {
        return Err(ApiError::from_frontend(
            AppError::NotFound(format!("WireGuard client config not found at {}", config_path)).into(),
        ));
    }
    open_wireguard_app().map_err(|e| ApiError::from_frontend(e.into()))?;
    Ok(Json("WireGuard app opened. Use it to reconnect or toggle the tunnel.".to_string()))
}

pub async fn setup_handoff(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<PostWireGuardSetupState>, ApiError> {
    let result = setup_wireguard_app_handoff(&state.app, &state.context)
        .await
        .map_err(|e| ApiError::from_frontend(e.into()))?;
    Ok(Json(result))
}

pub async fn verify_wireguard(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<ReachabilityResult>, ApiError> {
    let result = verify_wireguard_connection(&state.app, &state.context)
        .await
        .map_err(|e| ApiError::from_frontend(e.into()))?;
    Ok(Json(result))
}

pub async fn open_wireguard_app_route() -> Result<Json<bool>, ApiError> {
    open_wireguard_app().map_err(|e| ApiError::from_frontend(e.into()))?;
    Ok(Json(true))
}

pub async fn download_wireguard_config_route(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<String>, ApiError> {
    let path = download_wireguard_config(&state.context)
        .await
        .map_err(|e| ApiError::from_frontend(e.into()))?;
    Ok(Json(path))
}

pub async fn get_setup_status_route(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<PostWireGuardSetupState>, ApiError> {
    Ok(Json(get_setup_status(&state.context).await))
}

pub async fn download_url(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<String>, ApiError> {
    let os = OsDetection::new();
    if os.is_windows() {
        Ok(Json(state.context.config.wireguard_download_url_windows.clone()))
    } else if os.is_macos() {
        Ok(Json(state.context.config.wireguard_download_url_macos.clone()))
    } else {
        Ok(Json(state.context.config.wireguard_download_url_linux.clone()))
    }
}

pub async fn reconnect_instance_wireguard(
    Path(_instance_id): Path<u64>,
) -> Result<Json<String>, ApiError> {
    open_wireguard_app().map_err(|e| ApiError::from_frontend(e.into()))?;
    Ok(Json("Opened WireGuard app.".to_string()))
}
