use chrono::Utc;
use serde::{Deserialize, Serialize};
use tauri::State;
use tracing::warn;

use crate::{
    errors::{AppError, FrontendError},
    models::app_state::{ConnectionPreference, InstanceNetworkState},
    services::{
        app_context::AppContext,
        cloudflare_turn::{
            self, CloudflareTurnSettingsResponse, CloudflareTurnSettingsUpdate,
            CloudflareTurnTestResult,
        },
        connection_manager::ConnectionManager,
        network_agent::NetworkAgentProvisioner,
    },
};

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InstanceConnectionStatusResponse {
    pub instance_id: u64,
    pub network: InstanceNetworkState,
    pub manual_turn_switching_enabled: bool,
    pub automatic_selection_enabled: bool,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SetConnectionPreferenceRequest {
    pub instance_id: u64,
    pub preference: ConnectionPreference,
}

async fn deactivate_turn_allocations(context: &AppContext) -> Result<(), FrontendError> {
    let active_turn_instances = context
        .load_state()
        .await
        .provisioned_servers
        .into_iter()
        .filter(|server| {
            server.network.active_transport
                == Some(noland_network_contracts::state::TransportKind::CloudflareTurn)
        })
        .map(|server| server.instance_id)
        .collect::<Vec<_>>();
    for instance_id in active_turn_instances {
        ConnectionManager::switch(
            context,
            instance_id,
            noland_network_contracts::state::TransportKind::Direct,
        )
        .await?;
    }
    let turn_instances = context
        .load_state()
        .await
        .provisioned_servers
        .into_iter()
        .filter(|server| server.network.cloudflare_turn.enabled)
        .map(|server| server.instance_id)
        .collect::<Vec<_>>();
    for instance_id in turn_instances {
        if let Err(error) = ConnectionManager::stop_turn(context, instance_id).await {
            warn!(instance_id, %error, "could not stop remote TURN allocation while disabling TURN");
        }
    }
    Ok(())
}

#[tauri::command]
pub async fn get_instance_connection_status(
    instance_id: u64,
    context: State<'_, AppContext>,
) -> Result<InstanceConnectionStatusResponse, FrontendError> {
    let state = context.load_state().await;
    let turn_credentials_available = tokio::task::spawn_blocking(cloudflare_turn::load_secret)
        .await
        .map_err(|error| AppError::State(format!("Secure storage task failed: {error}")))??
        .is_some();
    let server = state
        .provisioned_servers
        .iter()
        .find(|server| server.instance_id == instance_id)
        .ok_or_else(|| AppError::NotFound(format!("Instance {instance_id} is not provisioned")))?;
    Ok(InstanceConnectionStatusResponse {
        instance_id,
        network: server.network.clone(),
        manual_turn_switching_enabled: state.cloudflare_turn.enabled && turn_credentials_available,
        automatic_selection_enabled:
            crate::services::connection_manager::automatic_selection_enabled(),
    })
}

#[tauri::command]
pub async fn repair_instance_connection(
    instance_id: u64,
    context: State<'_, AppContext>,
) -> Result<InstanceConnectionStatusResponse, FrontendError> {
    let remote = super::build_remote_exec_for_instance(context.inner(), instance_id).await?;
    NetworkAgentProvisioner::ensure(&remote, instance_id).await?;
    let network = ConnectionManager::repair(context.inner(), instance_id).await?;
    let state = context.load_state().await;
    let credentials_available = tokio::task::spawn_blocking(cloudflare_turn::load_secret)
        .await
        .map_err(|error| AppError::State(format!("Secure storage task failed: {error}")))??
        .is_some();
    Ok(InstanceConnectionStatusResponse {
        instance_id,
        network,
        manual_turn_switching_enabled: state.cloudflare_turn.enabled && credentials_available,
        automatic_selection_enabled:
            crate::services::connection_manager::automatic_selection_enabled(),
    })
}

#[tauri::command]
pub async fn set_instance_connection_preference(
    payload: SetConnectionPreferenceRequest,
    context: State<'_, AppContext>,
) -> Result<InstanceConnectionStatusResponse, FrontendError> {
    let current = context.load_state().await;
    if !current
        .provisioned_servers
        .iter()
        .any(|server| server.instance_id == payload.instance_id)
    {
        return Err(AppError::NotFound(format!(
            "Instance {} is not provisioned",
            payload.instance_id
        ))
        .into());
    }
    if payload.preference == ConnectionPreference::CloudflareTurn {
        if !current.cloudflare_turn.enabled {
            return Err(AppError::InvalidInput(
                "Enable and validate Cloudflare TURN before selecting the relay preference"
                    .to_string(),
            )
            .into());
        }
        let secret_set = tokio::task::spawn_blocking(cloudflare_turn::load_secret)
            .await
            .map_err(|error| AppError::State(format!("Secure storage task failed: {error}")))??
            .is_some();
        if !secret_set {
            return Err(AppError::InvalidInput(
                "Cloudflare TURN credentials are missing from secure storage".to_string(),
            )
            .into());
        }
    }

    let updated_at = Utc::now().to_rfc3339();
    let instance_id = payload.instance_id;
    let preference = payload.preference;
    let credential_ref = current.cloudflare_turn.credential_ref;
    let turn_enabled = current.cloudflare_turn.enabled;
    match preference {
        ConnectionPreference::Direct => {
            ConnectionManager::switch(
                context.inner(),
                instance_id,
                noland_network_contracts::state::TransportKind::Direct,
            )
            .await?;
        }
        ConnectionPreference::CloudflareTurn => {
            // Network-agent installation was intentionally non-blocking during
            // provisioning so direct WireGuard could succeed independently.
            // A manual TURN request is the explicit point where the host
            // bridge must become available, so finish that deployment here.
            let remote =
                super::build_remote_exec_for_instance(context.inner(), instance_id).await?;
            NetworkAgentProvisioner::ensure(&remote, instance_id).await?;
            ConnectionManager::switch(
                context.inner(),
                instance_id,
                noland_network_contracts::state::TransportKind::CloudflareTurn,
            )
            .await?;
        }
        ConnectionPreference::Auto => {}
    }
    context
        .update_state(|state| {
            if let Some(server) = state
                .provisioned_servers
                .iter_mut()
                .find(|server| server.instance_id == instance_id)
            {
                server.network.client_revision = server.network.client_revision.saturating_add(1);
                server.network.updated_at = Some(updated_at);
                server.network.preference = preference;
                server.network.cloudflare_turn.enabled = turn_enabled;
                server.network.cloudflare_turn.credential_ref =
                    turn_enabled.then(|| credential_ref.clone());
            }
        })
        .await?;

    if preference == ConnectionPreference::Auto
        && crate::services::connection_manager::automatic_selection_enabled()
    {
        ConnectionManager::evaluate_and_apply_automatic(context.inner(), instance_id).await?;
    }

    let next = context.load_state().await;
    let network = next
        .provisioned_servers
        .iter()
        .find(|server| server.instance_id == instance_id)
        .expect("validated instance must remain in committed state")
        .network
        .clone();
    Ok(InstanceConnectionStatusResponse {
        instance_id,
        network,
        manual_turn_switching_enabled: turn_enabled
            && tokio::task::spawn_blocking(cloudflare_turn::load_secret)
                .await
                .map_err(|error| AppError::State(format!("Secure storage task failed: {error}")))??
                .is_some(),
        automatic_selection_enabled:
            crate::services::connection_manager::automatic_selection_enabled(),
    })
}

#[tauri::command]
pub async fn get_cloudflare_turn_settings(
    context: State<'_, AppContext>,
) -> Result<CloudflareTurnSettingsResponse, FrontendError> {
    let state = context.load_state().await;
    let secret = tokio::task::spawn_blocking(cloudflare_turn::load_secret)
        .await
        .map_err(|error| AppError::State(format!("Secure storage task failed: {error}")))??;
    let key_id_hint = secret
        .as_ref()
        .map(|(key_id, _)| cloudflare_turn::key_id_hint(key_id));
    let token_set = secret.is_some();
    let status = if !state.cloudflare_turn.enabled {
        "disabled"
    } else if token_set {
        "valid"
    } else {
        "missing"
    };
    Ok(CloudflareTurnSettingsResponse {
        enabled: state.cloudflare_turn.enabled,
        status: status.to_string(),
        key_id_hint,
        token_set,
        credential_ref: cloudflare_turn::credential_ref().to_string(),
        last_validated_at: state.cloudflare_turn.last_validated_at,
        last_error: state.cloudflare_turn.last_error,
    })
}

#[tauri::command]
pub async fn test_cloudflare_turn_settings(
    payload: CloudflareTurnSettingsUpdate,
    context: State<'_, AppContext>,
) -> Result<CloudflareTurnTestResult, FrontendError> {
    cloudflare_turn::test_credentials(&context.http_client, &payload.key_id, &payload.api_token)
        .await
        .map_err(FrontendError::from)
}

#[tauri::command]
pub async fn save_cloudflare_turn_settings(
    payload: CloudflareTurnSettingsUpdate,
    context: State<'_, AppContext>,
) -> Result<CloudflareTurnSettingsResponse, FrontendError> {
    cloudflare_turn::test_credentials(&context.http_client, &payload.key_id, &payload.api_token)
        .await?;
    if !payload.enabled {
        deactivate_turn_allocations(context.inner()).await?;
    }

    let previous = tokio::task::spawn_blocking(cloudflare_turn::load_secret)
        .await
        .map_err(|error| AppError::State(format!("Secure storage task failed: {error}")))??;
    let key_id = payload.key_id.trim().to_string();
    let api_token = payload.api_token.trim().to_string();
    let stored_key_id = key_id.clone();
    let stored_token = api_token.clone();
    tokio::task::spawn_blocking(move || {
        cloudflare_turn::store_secret(&stored_key_id, &stored_token)
    })
    .await
    .map_err(|error| AppError::State(format!("Secure storage task failed: {error}")))??;

    let validated_at = Utc::now().to_rfc3339();
    let enabled = payload.enabled;
    if let Err(error) = context
        .update_state(|state| {
            state.cloudflare_turn.enabled = enabled;
            state.cloudflare_turn.credential_ref = cloudflare_turn::credential_ref().to_string();
            state.cloudflare_turn.last_validated_at = Some(validated_at.clone());
            state.cloudflare_turn.last_error = None;
            for server in &mut state.provisioned_servers {
                server.network.client_revision = server.network.client_revision.saturating_add(1);
                server.network.updated_at = Some(validated_at.clone());
                server.network.cloudflare_turn.enabled = enabled;
                server.network.cloudflare_turn.credential_ref =
                    enabled.then(|| cloudflare_turn::credential_ref().to_string());
                if !enabled && server.network.preference == ConnectionPreference::CloudflareTurn {
                    server.network.preference = ConnectionPreference::Auto;
                }
            }
        })
        .await
    {
        let _ = tokio::task::spawn_blocking(move || match previous {
            Some((key_id, token)) => cloudflare_turn::store_secret(&key_id, &token),
            None => cloudflare_turn::delete_secret(),
        })
        .await;
        return Err(error.into());
    }

    Ok(CloudflareTurnSettingsResponse {
        enabled: payload.enabled,
        status: if payload.enabled { "valid" } else { "disabled" }.to_string(),
        key_id_hint: Some(cloudflare_turn::key_id_hint(&key_id)),
        token_set: true,
        credential_ref: cloudflare_turn::credential_ref().to_string(),
        last_validated_at: Some(validated_at),
        last_error: None,
    })
}

#[tauri::command]
pub async fn clear_cloudflare_turn_settings(
    context: State<'_, AppContext>,
) -> Result<CloudflareTurnSettingsResponse, FrontendError> {
    deactivate_turn_allocations(context.inner()).await?;

    let previous = tokio::task::spawn_blocking(cloudflare_turn::load_secret)
        .await
        .map_err(|error| AppError::State(format!("Secure storage task failed: {error}")))??;
    tokio::task::spawn_blocking(cloudflare_turn::delete_secret)
        .await
        .map_err(|error| AppError::State(format!("Secure storage task failed: {error}")))??;

    if let Err(error) = context
        .update_state(|state| {
            state.cloudflare_turn.enabled = false;
            state.cloudflare_turn.last_validated_at = None;
            state.cloudflare_turn.last_error = None;
            let updated_at = Utc::now().to_rfc3339();
            for server in &mut state.provisioned_servers {
                server.network.client_revision = server.network.client_revision.saturating_add(1);
                server.network.updated_at = Some(updated_at.clone());
                server.network.cloudflare_turn.enabled = false;
                server.network.cloudflare_turn.credential_ref = None;
                server.network.cloudflare_turn.availability =
                    noland_network_contracts::state::PathAvailability::Unavailable;
                if server.network.preference == ConnectionPreference::CloudflareTurn {
                    server.network.preference = ConnectionPreference::Auto;
                }
            }
        })
        .await
    {
        if let Some((key_id, token)) = previous {
            let _ =
                tokio::task::spawn_blocking(move || cloudflare_turn::store_secret(&key_id, &token))
                    .await;
        }
        return Err(error.into());
    }

    Ok(CloudflareTurnSettingsResponse {
        enabled: false,
        status: "disabled".to_string(),
        key_id_hint: None,
        token_set: false,
        credential_ref: cloudflare_turn::credential_ref().to_string(),
        last_validated_at: None,
        last_error: None,
    })
}
