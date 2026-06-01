use std::sync::Arc;

use axum::{extract::{Path, State}, Json};

use crate::{
    api::{error::ApiError, state::ApiState},
    models::app_state::{BundleIndex, RestoreDryRunResult, RestoreJob, RestoreRequest},
    services::{
        remote_exec::RemoteExec,
        shared_storage::{bundle_indexer::BundleIndexer, bundle_restore::BundleRestoreService},
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
pub struct WrappedRestoreRequest {
    pub payload: RestoreRequest,
}

pub async fn generate_bundle_index(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<bool>, ApiError> {
    let snapshot = state.context.state.read().await.clone();
    let remote = remote_from_active_state(&snapshot, state.context.config.audio_target_user.clone())
        .map_err(|e| ApiError::from_frontend(e.into()))?;
    let instance_id = snapshot.instance.instance_id.ok_or_else(|| {
        ApiError::from_frontend(
            crate::errors::AppError::InvalidInput("No active instance. Start provisioning first.".to_string()).into(),
        )
    })?;
    BundleIndexer::generate_and_upload(&state.context, &remote, instance_id, &state.context.config.audio_target_user)
        .await
        .map_err(|e| ApiError::from_frontend(e.into()))?;
    Ok(Json(true))
}

pub async fn get_instance_restore_bundles(
    State(state): State<Arc<ApiState>>,
    Path(instance_id): Path<u64>,
) -> Result<Json<BundleIndex>, ApiError> {
    let snapshot = state.context.state.read().await.clone();
    let remote = remote_from_active_state(&snapshot, state.context.config.audio_target_user.clone())
        .map_err(|e| ApiError::from_frontend(e.into()))?;
    let bundles = BundleRestoreService::list_bundles(
        &state.context,
        &remote,
        instance_id,
        &state.context.config.audio_target_user,
    )
    .await
    .map_err(|e| ApiError::from_frontend(e.into()))?;
    Ok(Json(bundles))
}

pub async fn dry_run_restore(
    State(state): State<Arc<ApiState>>,
    Path(instance_id): Path<u64>,
    Json(request): Json<WrappedRestoreRequest>,
) -> Result<Json<RestoreDryRunResult>, ApiError> {
    let snapshot = state.context.state.read().await.clone();
    let remote = remote_from_active_state(&snapshot, state.context.config.audio_target_user.clone())
        .map_err(|e| ApiError::from_frontend(e.into()))?;
    let result = BundleRestoreService::dry_run_restore(
        &state.context,
        &remote,
        instance_id,
        &state.context.config.audio_target_user,
        request.payload,
    )
    .await
    .map_err(|e| ApiError::from_frontend(e.into()))?;
    Ok(Json(result))
}

pub async fn restore_bundle(
    State(state): State<Arc<ApiState>>,
    Path(instance_id): Path<u64>,
    Json(request): Json<WrappedRestoreRequest>,
) -> Result<Json<RestoreJob>, ApiError> {
    let snapshot = state.context.state.read().await.clone();
    let remote = remote_from_active_state(&snapshot, state.context.config.audio_target_user.clone())
        .map_err(|e| ApiError::from_frontend(e.into()))?;
    let result = BundleRestoreService::restore_bundle(
        &state.context,
        &remote,
        instance_id,
        &state.context.config.audio_target_user,
        request.payload,
    )
    .await
    .map_err(|e| ApiError::from_frontend(e.into()))?;
    Ok(Json(result))
}

pub async fn get_restore_job(
    Path(job_id): Path<String>,
) -> Result<Json<RestoreJob>, ApiError> {
    let result = BundleRestoreService::get_job(&job_id)
        .await
        .map_err(|e| ApiError::from_frontend(e.into()))?;
    Ok(Json(result))
}
