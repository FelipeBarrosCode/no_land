use tauri::State;

use crate::{
    errors::{AppError, FrontendError},
    models::app_state::{AutoShutdownSettings, AutoShutdownState, PersistedAppState},
    services::{
        app_context::AppContext,
        lifecycle_agent::{LifecycleAgentProvisioner, LifecycleAgentStatus},
    },
};

fn validate_auto_shutdown_request(
    settings: &AutoShutdownSettings,
    state: &PersistedAppState,
) -> Result<(), AppError> {
    if !settings.inactivity_hours.is_finite()
        || !(1.0 / 12.0..=24.0).contains(&settings.inactivity_hours)
    {
        return Err(AppError::InvalidInput(
            "Inactivity timeout must be a finite number between 5 minutes and 24 hours."
                .to_string(),
        ));
    }

    if !(1..=10).contains(&settings.backup_app_limit) {
        return Err(AppError::InvalidInput(
            "Backup app limit must be between 1 and 10.".to_string(),
        ));
    }

    if !settings.enabled {
        return Ok(());
    }

    let active_profile = state
        .shared_storage_profiles
        .iter()
        .find(|profile| profile.active)
        .ok_or_else(|| {
            AppError::InvalidInput(
                "Connect and activate a shared-storage profile before enabling automatic backup and shutdown."
                    .to_string(),
            )
        })?;

    let profile_secrets = state
        .shared_storage_credentials
        .profiles
        .get(&active_profile.id);
    let repository_key_available = !state
        .shared_storage_credentials
        .repository_key_hex
        .trim()
        .is_empty()
        || profile_secrets.is_some_and(|secret| !secret.repository_key_hex.trim().is_empty());

    if !repository_key_available {
        return Err(AppError::InvalidInput(
            "The active shared-storage profile does not have an available repository key. Reconnect the profile before enabling automatic backup and shutdown."
                .to_string(),
        ));
    }

    if profile_secrets.is_none() {
        return Err(AppError::InvalidInput(
            "Credentials for the active shared-storage profile are unavailable. Reconnect the profile before enabling automatic backup and shutdown."
                .to_string(),
        ));
    }

    if state.credentials.vast_api_key.trim().is_empty() {
        return Err(AppError::InvalidInput(
            "Add your Vast.ai API key before enabling automatic backup and shutdown.".to_string(),
        ));
    }

    if state.provisioned_servers.is_empty() {
        return Err(AppError::InvalidInput(
            "Provision at least one server before enabling automatic backup and shutdown."
                .to_string(),
        ));
    }

    Ok(())
}

#[tauri::command]
pub async fn get_auto_shutdown_settings(
    context: State<'_, AppContext>,
) -> Result<AutoShutdownState, FrontendError> {
    Ok(context.load_state().await.auto_shutdown)
}

#[tauri::command]
pub async fn save_auto_shutdown_settings(
    settings: AutoShutdownSettings,
    context: State<'_, AppContext>,
) -> Result<PersistedAppState, FrontendError> {
    let current_state = context.load_state().await;
    validate_auto_shutdown_request(&settings, &current_state)?;
    let instance_ids = current_state
        .provisioned_servers
        .iter()
        .map(|server| server.instance_id)
        .collect::<Vec<_>>();

    for &instance_id in &instance_ids {
        let apply_result =
            match super::build_remote_exec_for_instance(context.inner(), instance_id).await {
                Ok(remote) => {
                    LifecycleAgentProvisioner::configure_for_instance_settings(
                        context.inner(),
                        &remote,
                        instance_id,
                        &settings,
                    )
                    .await
                }
                Err(error) => Err(error),
            };
        if let Err(error) = apply_result {
            return fail_safe_disable_after_settings_error(
                context.inner(),
                &instance_ids,
                &settings,
                format!("Could not apply automatic shutdown settings: {error}"),
            )
            .await;
        }
    }

    match context
        .update_state(|state| {
            state.auto_shutdown.settings = settings.clone();
            state.auto_shutdown.last_status = if settings.enabled {
                "monitoring".to_string()
            } else {
                "disabled".to_string()
            };
            state.auto_shutdown.last_error = None;
            state.last_error = None;
        })
        .await
    {
        Ok(next_state) => Ok(next_state),
        Err(error) => {
            fail_safe_disable_after_settings_error(
                context.inner(),
                &instance_ids,
                &settings,
                format!("Could not persist automatic shutdown settings: {error}"),
            )
            .await
        }
    }
}

async fn fail_safe_disable_after_settings_error(
    context: &AppContext,
    instance_ids: &[u64],
    requested_settings: &AutoShutdownSettings,
    original_error: String,
) -> Result<PersistedAppState, FrontendError> {
    let mut disabled_settings = requested_settings.clone();
    disabled_settings.enabled = false;
    let mut disable_failures = Vec::new();

    for &instance_id in instance_ids {
        let result = match super::build_remote_exec_for_instance(context, instance_id).await {
            Ok(remote) => {
                LifecycleAgentProvisioner::configure_for_instance_settings(
                    context,
                    &remote,
                    instance_id,
                    &disabled_settings,
                )
                .await
            }
            Err(error) => Err(error),
        };
        if let Err(error) = result {
            disable_failures.push(format!("instance {instance_id}: {error}"));
        }
    }

    let status = if disable_failures.is_empty() {
        "disabled_after_error"
    } else {
        "disable_pending"
    };
    let message = if disable_failures.is_empty() {
        format!("{original_error}. All provisioned instances were disabled as a safety fallback.")
    } else {
        format!(
            "{original_error}. Automatic shutdown could not be confirmed disabled everywhere: {}",
            disable_failures.join("; ")
        )
    };

    let persist_result = context
        .update_state(|state| {
            state.auto_shutdown.settings = disabled_settings;
            state.auto_shutdown.last_status = status.to_string();
            state.auto_shutdown.last_error = Some(message.clone());
            state.last_error = Some(message.clone());
        })
        .await;
    let final_message = match persist_result {
        Ok(_) => message,
        Err(error) => {
            format!("{message}. The fail-safe disabled state also could not be persisted: {error}")
        }
    };
    Err(AppError::Provisioning(final_message).into())
}

#[tauri::command]
pub async fn get_instance_auto_shutdown_status(
    instance_id: u64,
    context: State<'_, AppContext>,
) -> Result<LifecycleAgentStatus, FrontendError> {
    let remote = super::build_remote_exec_for_instance(context.inner(), instance_id).await?;
    let status = LifecycleAgentProvisioner::get_status(&remote).await?;
    let last_status = status.state.to_ascii_lowercase();
    let last_error = status.last_error.clone();
    let last_run_at = status.timeout_reached_at.map(|value| value.to_rfc3339());
    let persisted = context.load_state().await.auto_shutdown;
    let effective_last_run = last_run_at
        .clone()
        .or_else(|| persisted.last_run_at.clone());
    if persisted.last_status != last_status
        || persisted.last_error != last_error
        || persisted.last_run_at != effective_last_run
    {
        context
            .update_state(|state| {
                state.auto_shutdown.last_status = last_status.clone();
                state.auto_shutdown.last_error = last_error.clone();
                state.auto_shutdown.last_run_at = effective_last_run.clone();
            })
            .await?;
    }
    Ok(status)
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use crate::models::{
        app_state::{
            AutoShutdownSettings, PersistedAppState, ProvisionedServerState,
            SharedStorageProfileSecret,
        },
        application_bundle::{ProfileReference, StorageCredential, StorageProvider},
    };

    use super::validate_auto_shutdown_request;

    #[test]
    fn settings_validation_accepts_boundaries() {
        let state = PersistedAppState::default();

        for inactivity_hours in [1.0 / 12.0, 24.0] {
            for backup_app_limit in [1, 10] {
                let settings = AutoShutdownSettings {
                    enabled: false,
                    inactivity_hours,
                    backup_app_limit,
                };
                assert!(validate_auto_shutdown_request(&settings, &state).is_ok());
            }
        }
    }

    #[test]
    fn settings_validation_rejects_non_finite_and_out_of_range_values() {
        let state = PersistedAppState::default();

        for inactivity_hours in [f32::NAN, f32::INFINITY, 1.0 / 12.0 - 0.001, 24.01] {
            let settings = AutoShutdownSettings {
                enabled: false,
                inactivity_hours,
                backup_app_limit: 3,
            };
            assert!(validate_auto_shutdown_request(&settings, &state).is_err());
        }

        for backup_app_limit in [0, 11] {
            let settings = AutoShutdownSettings {
                enabled: false,
                inactivity_hours: 3.0,
                backup_app_limit,
            };
            assert!(validate_auto_shutdown_request(&settings, &state).is_err());
        }
    }

    #[test]
    fn enabling_requires_all_local_prerequisites() {
        let settings = AutoShutdownSettings {
            enabled: true,
            ..AutoShutdownSettings::default()
        };
        let mut state = PersistedAppState::default();

        assert!(validate_auto_shutdown_request(&settings, &state).is_err());

        let profile_id = "test-profile".to_string();
        state.shared_storage_profiles.push(ProfileReference {
            id: profile_id.clone(),
            display_name: "Test storage".to_string(),
            provider_label: "Test provider".to_string(),
            provider: Some(StorageProvider::GenericS3),
            bucket: None,
            prefix: None,
            active: true,
        });
        state.shared_storage_credentials.repository_key_hex = "ab".repeat(32);
        state.shared_storage_credentials.profiles.insert(
            profile_id,
            SharedStorageProfileSecret {
                credentials: StorageCredential::UsernamePassword {
                    username: "test".to_string(),
                    password: "test".to_string(),
                },
                provider_fields: HashMap::new(),
                repository_key_hex: String::new(),
            },
        );
        state.credentials.vast_api_key = "test-vast-api-key".to_string();
        state
            .provisioned_servers
            .push(ProvisionedServerState::new(1));

        assert!(validate_auto_shutdown_request(&settings, &state).is_ok());
    }
}
