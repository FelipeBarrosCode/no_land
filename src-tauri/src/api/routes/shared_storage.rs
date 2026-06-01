use std::sync::Arc;

use axum::{extract::{Path, State}, Json};

use crate::{
    api::{error::ApiError, state::ApiState},
    models::app_state::{
        BackupStatusResponse, SharedStorageInstanceStatus, SharedStorageObjectEntry,
        SharedStorageSettingsResponse, SharedStorageSettingsUpdate, SharedStorageSyncSelectionRequest,
    },
    services::{
        instance_lifecycle::InstanceLifecycleService,
        remote_exec::RemoteExec,
        shared_storage::shared_storage_manager::SharedStorageManager,
    },
};

fn remote_from_active_state(state: &crate::models::app_state::PersistedAppState, target_user: String) -> Result<RemoteExec, crate::errors::AppError> {
    if state.ssh.private_key_path.trim().is_empty() {
        return Err(crate::errors::AppError::InvalidInput(
            "SSH private key path is empty. Run provisioning first.".to_string(),
        ));
    }
    if state.instance.ssh_host.trim().is_empty() || state.instance.ssh_port == 0 {
        return Err(crate::errors::AppError::InvalidInput(
            "Instance SSH details are not available. Ensure the instance is running.".to_string(),
        ));
    }
    Ok(RemoteExec {
        ssh_user: if state.ssh.ssh_username.trim().is_empty() { target_user } else { state.ssh.ssh_username.clone() },
        ssh_host: state.instance.ssh_host.clone(),
        ssh_port: state.instance.ssh_port,
        private_key_path: state.ssh.private_key_path.clone(),
    })
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WrappedSharedStorageSettingsRequest {
    pub payload: SharedStorageSettingsUpdate,
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WrappedSelectionRequest {
    pub payload: SharedStorageSyncSelectionRequest,
}

pub async fn get_shared_storage_settings(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<SharedStorageSettingsResponse>, ApiError> {
    let settings = SharedStorageManager::get_settings(&state.context)
        .await
        .map_err(|e| ApiError::from_frontend(e.into()))?;
    Ok(Json(settings))
}

pub async fn save_shared_storage_settings(
    State(state): State<Arc<ApiState>>,
    Json(request): Json<WrappedSharedStorageSettingsRequest>,
) -> Result<Json<crate::models::app_state::PersistedAppState>, ApiError> {
    SharedStorageManager::save_settings(&state.context, request.payload)
        .await
        .map_err(|e| ApiError::from_frontend(e.into()))?;
    Ok(Json(state.context.load_state().await))
}

pub async fn test_shared_storage_config(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<String>, ApiError> {
    let snapshot = state.context.state.read().await.clone();
    let remote = remote_from_active_state(&snapshot, state.context.config.audio_target_user.clone())
        .map_err(|e| ApiError::from_frontend(e.into()))?;
    SharedStorageManager::test_configuration(&state.context, &remote, &state.context.config.audio_target_user)
        .await
        .map_err(|e| ApiError::from_frontend(e.into()))?;
    Ok(Json("Backblaze B2 configuration is valid".to_string()))
}

pub async fn trigger_instance_backup(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<BackupStatusResponse>, ApiError> {
    let snapshot = state.context.state.read().await.clone();
    let remote = remote_from_active_state(&snapshot, state.context.config.audio_target_user.clone())
        .map_err(|e| ApiError::from_frontend(e.into()))?;
    let instance_id = snapshot.instance.instance_id.ok_or_else(|| {
        ApiError::from_frontend(
            crate::errors::AppError::InvalidInput("No active instance. Start provisioning first.".to_string()).into(),
        )
    })?;
    SharedStorageManager::trigger_manual_backup(
        &state.context,
        &remote,
        instance_id,
        &state.context.config.audio_target_user,
    )
    .await
    .map_err(|e| ApiError::from_frontend(e.into()))?;
    let status = SharedStorageManager::get_backup_status(&state.context)
        .await
        .map_err(|e| ApiError::from_frontend(e.into()))?;
    Ok(Json(status))
}

pub async fn trigger_instance_backup_for(
    State(state): State<Arc<ApiState>>,
    Path(instance_id): Path<u64>,
) -> Result<Json<BackupStatusResponse>, ApiError> {
    let status = InstanceLifecycleService::save_instance_to_shared_storage(&state.context, instance_id)
        .await
        .map_err(|e| ApiError::from_frontend(e.into()))?;
    Ok(Json(status))
}

pub async fn sync_instance_from_shared_storage(
    State(state): State<Arc<ApiState>>,
    Path(instance_id): Path<u64>,
) -> Result<Json<String>, ApiError> {
    let message = InstanceLifecycleService::sync_instance_from_shared_storage(&state.context, instance_id)
        .await
        .map_err(|e| ApiError::from_frontend(e.into()))?;
    Ok(Json(message))
}

pub async fn list_instance_shared_storage_objects(
    State(state): State<Arc<ApiState>>,
    Path(instance_id): Path<u64>,
) -> Result<Json<Vec<SharedStorageObjectEntry>>, ApiError> {
    let items = InstanceLifecycleService::list_shared_storage_objects(&state.context, instance_id)
        .await
        .map_err(|e| ApiError::from_frontend(e.into()))?;
    Ok(Json(items))
}

pub async fn sync_instance_from_shared_storage_selected(
    State(state): State<Arc<ApiState>>,
    Path(instance_id): Path<u64>,
    Json(request): Json<WrappedSelectionRequest>,
) -> Result<Json<String>, ApiError> {
    let message = InstanceLifecycleService::sync_instance_from_shared_storage_selected(
        &state.context,
        instance_id,
        request.payload.selected_paths,
    )
    .await
    .map_err(|e| ApiError::from_frontend(e.into()))?;
    Ok(Json(message))
}

pub async fn list_instance_exportable_storage_objects(
    State(state): State<Arc<ApiState>>,
    Path(instance_id): Path<u64>,
) -> Result<Json<Vec<SharedStorageObjectEntry>>, ApiError> {
    let items = InstanceLifecycleService::list_instance_exportable_objects(&state.context, instance_id)
        .await
        .map_err(|e| ApiError::from_frontend(e.into()))?;
    Ok(Json(items))
}

pub async fn save_instance_to_shared_storage_selected(
    State(state): State<Arc<ApiState>>,
    Path(instance_id): Path<u64>,
    Json(request): Json<WrappedSelectionRequest>,
) -> Result<Json<String>, ApiError> {
    let message = InstanceLifecycleService::save_instance_to_shared_storage_selected(
        &state.context,
        instance_id,
        request.payload.selected_paths,
    )
    .await
    .map_err(|e| ApiError::from_frontend(e.into()))?;
    Ok(Json(message))
}

pub async fn get_instance_backup_status(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<SharedStorageInstanceStatus>, ApiError> {
    let instance_id = state
        .context
        .state
        .read()
        .await
        .instance
        .instance_id
        .ok_or_else(|| ApiError::from_frontend(crate::errors::AppError::InvalidInput("No active instance.".to_string()).into()))?;
    let status = SharedStorageManager::get_instance_backup_status(&state.context, instance_id)
        .await
        .map_err(|e| ApiError::from_frontend(e.into()))?;
    Ok(Json(status))
}

pub async fn setup_instance_backup_schedule(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<String>, ApiError> {
    let snapshot = state.context.state.read().await.clone();
    let remote = remote_from_active_state(&snapshot, state.context.config.audio_target_user.clone())
        .map_err(|e| ApiError::from_frontend(e.into()))?;
    let instance_id = snapshot.instance.instance_id.ok_or_else(|| ApiError::from_frontend(
        crate::errors::AppError::InvalidInput("No active instance. Start provisioning first.".to_string()).into(),
    ))?;
    SharedStorageManager::setup_scheduled_backup(
        &state.context,
        &remote,
        instance_id,
        &state.context.config.audio_target_user,
    )
    .await
    .map_err(|e| ApiError::from_frontend(e.into()))?;
    Ok(Json("Scheduled backups are disabled".to_string()))
}

pub async fn remove_instance_backup_schedule(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<String>, ApiError> {
    let snapshot = state.context.state.read().await.clone();
    let remote = remote_from_active_state(&snapshot, state.context.config.audio_target_user.clone())
        .map_err(|e| ApiError::from_frontend(e.into()))?;
    SharedStorageManager::remove_scheduled_backup(&remote, &state.context.config.audio_target_user)
        .await
        .map_err(|e| ApiError::from_frontend(e.into()))?;
    Ok(Json("Scheduled backups are disabled".to_string()))
}
