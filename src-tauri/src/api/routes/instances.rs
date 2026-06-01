use std::{collections::HashMap, sync::Arc};

use axum::{extract::{Path, State}, Json};

use crate::{
    api::{error::ApiError, state::ApiState},
    models::app_state::RentedInstanceSummary,
    services::{instance_lifecycle::InstanceLifecycleService, reboot_helper::RebootHelperService, vast_api::VastApiClient},
};

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SunshineAuthRequest {
    pub sunshine_username: String,
    pub sunshine_password: String,
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SunshineUpdateRequest {
    pub settings: HashMap<String, serde_json::Value>,
    pub sunshine_username: String,
    pub sunshine_password: String,
}

pub async fn get_rented_instances(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<Vec<RentedInstanceSummary>>, ApiError> {
    let app_state = state.context.state.read().await.clone();
    if app_state.credentials.vast_api_key.trim().is_empty() {
        return Ok(Json(Vec::new()));
    }
    let vast = VastApiClient::new(
        state.context.http_client.clone(),
        state.context.config.vast_base_url.clone(),
        app_state.credentials.vast_api_key,
    );
    let instances_source = vast.list_instances().await.unwrap_or_default();
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
    Ok(Json(instances))
}

pub async fn reboot_instance_services(
    State(state): State<Arc<ApiState>>,
    Path(instance_id): Path<u64>,
) -> Result<Json<String>, ApiError> {
    let app_state = state.context.state.read().await.clone();
    let remote = crate::services::remote_exec::RemoteExec {
        ssh_user: if app_state.ssh.ssh_username.trim().is_empty() {
            state.context.config.audio_target_user.clone()
        } else {
            app_state.ssh.ssh_username
        },
        ssh_host: app_state
            .provisioned_servers
            .iter()
            .find(|server| server.instance_id == instance_id)
            .map(|server| server.ssh_host.clone())
            .or_else(|| app_state.instance.ssh_host.clone().into())
            .unwrap_or_default(),
        ssh_port: app_state
            .provisioned_servers
            .iter()
            .find(|server| server.instance_id == instance_id)
            .map(|server| server.ssh_port)
            .unwrap_or(app_state.instance.ssh_port),
        private_key_path: app_state.ssh.private_key_path,
    };
    let message = RebootHelperService::reboot_and_reinitialize(
        &remote,
        &state.context.config.audio_target_user,
    )
    .await
    .map_err(|e| ApiError::from_frontend(e.into()))?;
    Ok(Json(message))
}

pub async fn pause_instance(
    State(state): State<Arc<ApiState>>,
    Path(instance_id): Path<u64>,
) -> Result<Json<bool>, ApiError> {
    InstanceLifecycleService::pause_instance(&state.context, instance_id)
        .await
        .map_err(|e| ApiError::from_frontend(e.into()))?;
    Ok(Json(true))
}

pub async fn destroy_instance(
    State(state): State<Arc<ApiState>>,
    Path(instance_id): Path<u64>,
) -> Result<Json<bool>, ApiError> {
    InstanceLifecycleService::destroy_instance(&state.context, instance_id)
        .await
        .map_err(|e| ApiError::from_frontend(e.into()))?;
    Ok(Json(true))
}

pub async fn get_instance_sunshine_settings(
    State(state): State<Arc<ApiState>>,
    Path(instance_id): Path<u64>,
    Json(payload): Json<SunshineAuthRequest>,
) -> Result<Json<crate::services::instance_lifecycle::SunshineSettingsResponse>, ApiError> {
    let response = InstanceLifecycleService::get_sunshine_settings(
        &state.context,
        instance_id,
        &payload.sunshine_username,
        &payload.sunshine_password,
    )
    .await
    .map_err(|e| ApiError::from_frontend(e.into()))?;
    Ok(Json(response))
}

pub async fn update_instance_sunshine_settings(
    State(state): State<Arc<ApiState>>,
    Path(instance_id): Path<u64>,
    Json(payload): Json<SunshineUpdateRequest>,
) -> Result<Json<bool>, ApiError> {
    InstanceLifecycleService::update_sunshine_settings(
        &state.context,
        instance_id,
        crate::services::instance_lifecycle::SunshineSettingsUpdatePayload {
            settings: payload.settings,
        },
        &payload.sunshine_username,
        &payload.sunshine_password,
    )
    .await
    .map_err(|e| ApiError::from_frontend(e.into()))?;
    Ok(Json(true))
}

pub async fn reset_instance_sunshine_settings(
    State(state): State<Arc<ApiState>>,
    Path(instance_id): Path<u64>,
    Json(payload): Json<SunshineAuthRequest>,
) -> Result<Json<bool>, ApiError> {
    InstanceLifecycleService::reset_sunshine_settings(
        &state.context,
        instance_id,
        &payload.sunshine_username,
        &payload.sunshine_password,
    )
    .await
    .map_err(|e| ApiError::from_frontend(e.into()))?;
    Ok(Json(true))
}
