use std::{collections::HashMap, sync::Arc};
use uuid::Uuid;

use crate::errors::{AppError, AppResult};
use crate::models::{
    app_state::{SharedStorageOAuthSessionSecret, SharedStorageProfileSecret},
    application_bundle::{SharedStorageProfile, SharedStorageStatus, StorageProvider},
};
use crate::services::app_context::AppContext;

use super::object_storage::{list_all_providers, StorageCredential};

static SHARED_PROFILE_MANAGER: std::sync::OnceLock<Arc<SharedStorageProfileManager>> =
    std::sync::OnceLock::new();

pub fn shared_profile_manager() -> Arc<SharedStorageProfileManager> {
    SHARED_PROFILE_MANAGER
        .get_or_init(|| Arc::new(SharedStorageProfileManager::new()))
        .clone()
}

pub struct SharedStorageProfileManager;

impl SharedStorageProfileManager {
    pub fn new() -> Self {
        Self
    }

    pub fn list_providers(&self) -> Vec<crate::models::application_bundle::ProviderDefinition> {
        list_all_providers()
    }

    pub async fn save_static_credentials(
        &self,
        context: &AppContext,
        provider: &StorageProvider,
        credentials: &StorageCredential,
        provider_fields: &HashMap<String, String>,
        bucket: Option<&str>,
        prefix: Option<&str>,
        display_name: &str,
    ) -> AppResult<SharedStorageProfile> {
        let profile_id = Uuid::new_v4().to_string();
        let repository_id = format!(
            "repo_{}",
            Uuid::new_v4().to_string().replace('-', "")[..12].to_string()
        );

        let stored = SharedStorageProfileSecret {
            credentials: credentials.clone(),
            provider_fields: provider_fields.clone(),
            repository_key_hex: hex::encode(generate_repository_key()),
        };

        context
            .update_state(|state| {
                state
                    .shared_storage_credentials
                    .profiles
                    .insert(profile_id.clone(), stored.clone());
            })
            .await?;

        Ok(SharedStorageProfile {
            id: profile_id.clone(),
            display_name: display_name.to_string(),
            provider: provider.clone(),
            provider_label: provider.label().to_string(),
            bucket: bucket.map(String::from),
            prefix: prefix.map(String::from),
            credential_vault_reference: format!(
                "state.json:sharedStorageCredentials.profiles.{}",
                profile_id
            ),
            repository_id,
            status: SharedStorageStatus::Connected,
            last_verified_at: None,
            protected_bundles_count: 0,
            total_stored_bytes: 0,
        })
    }

    pub async fn save_oauth_credentials(
        &self,
        context: &AppContext,
        provider: &StorageProvider,
        credentials: &StorageCredential,
        provider_fields: &HashMap<String, String>,
        display_name: &str,
    ) -> AppResult<SharedStorageProfile> {
        self.save_static_credentials(
            context,
            provider,
            credentials,
            provider_fields,
            None,
            None,
            display_name,
        )
        .await
    }

    pub async fn retrieve_credentials(
        &self,
        context: &AppContext,
        profile: &SharedStorageProfile,
    ) -> AppResult<StorageCredential> {
        let state = context.load_state().await;
        state
            .shared_storage_credentials
            .profiles
            .get(&profile.id)
            .map(|stored| stored.credentials.clone())
            .ok_or_else(|| AppError::NotFound("Credentials not found in state".to_string()))
    }

    pub async fn retrieve_provider_fields(
        &self,
        context: &AppContext,
        profile: &SharedStorageProfile,
    ) -> AppResult<HashMap<String, String>> {
        let state = context.load_state().await;
        state
            .shared_storage_credentials
            .profiles
            .get(&profile.id)
            .map(|stored| stored.provider_fields.clone())
            .ok_or_else(|| AppError::NotFound("Profile secrets not found in state".to_string()))
    }

    pub async fn retrieve_repository_key(
        &self,
        context: &AppContext,
        profile: &SharedStorageProfile,
    ) -> AppResult<[u8; 32]> {
        let state = context.load_state().await;
        let key_hex = state
            .shared_storage_credentials
            .profiles
            .get(&profile.id)
            .map(|stored| stored.repository_key_hex.clone())
            .ok_or_else(|| AppError::NotFound("Repository key not found in state".to_string()))?;

        let key_bytes: [u8; 32] = hex::decode(&key_hex)
            .map_err(|e| AppError::State(format!("Invalid repository key: {e}")))?
            .try_into()
            .map_err(|_| AppError::State("Repository key has wrong length".to_string()))?;

        Ok(key_bytes)
    }

    pub async fn delete_profile_credentials(
        &self,
        context: &AppContext,
        profile: &SharedStorageProfile,
    ) -> AppResult<()> {
        context
            .update_state(|state| {
                if let Some(stored) = state.shared_storage_credentials.profiles.get_mut(&profile.id) {
                    // Overwrite the credentials with blank ones, but keep the repository_key_hex
                    match &mut stored.credentials {
                        crate::models::application_bundle::StorageCredential::OAuth2 { access_token, refresh_token, expires_at } => {
                            *access_token = String::new();
                            *refresh_token = None;
                            *expires_at = 0;
                        }
                        crate::models::application_bundle::StorageCredential::S3 { access_key_id, secret_access_key, session_token } => {
                            *access_key_id = String::new();
                            *secret_access_key = String::new();
                            *session_token = None;
                        }
                        crate::models::application_bundle::StorageCredential::BackblazeB2 { key_id, application_key } => {
                            *key_id = String::new();
                            *application_key = String::new();
                        }
                        crate::models::application_bundle::StorageCredential::UsernamePassword { username, password } => {
                            *username = String::new();
                            *password = String::new();
                        }
                        crate::models::application_bundle::StorageCredential::SshKey { username, private_key, passphrase } => {
                            *username = String::new();
                            *private_key = String::new();
                            *passphrase = None;
                        }
                        crate::models::application_bundle::StorageCredential::ServiceAccount { json } => {
                            *json = String::new();
                        }
                    }
                }
            })
            .await?;
        Ok(())
    }

    pub async fn store_oauth_session_credentials(
        &self,
        context: &AppContext,
        session_id: &str,
        credentials: &StorageCredential,
    ) -> AppResult<()> {
        context
            .update_state(|state| {
                state.shared_storage_credentials.oauth_sessions.insert(
                    session_id.to_string(),
                    SharedStorageOAuthSessionSecret {
                        credentials: credentials.clone(),
                    },
                );
            })
            .await?;
        Ok(())
    }

    pub async fn retrieve_oauth_session_credentials(
        &self,
        context: &AppContext,
        session_id: &str,
    ) -> AppResult<Option<StorageCredential>> {
        let state = context.load_state().await;
        Ok(state
            .shared_storage_credentials
            .oauth_sessions
            .get(session_id)
            .map(|stored| stored.credentials.clone()))
    }

    pub async fn delete_oauth_session_credentials(
        &self,
        context: &AppContext,
        session_id: &str,
    ) -> AppResult<()> {
        context
            .update_state(|state| {
                state
                    .shared_storage_credentials
                    .oauth_sessions
                    .remove(session_id);
            })
            .await?;
        Ok(())
    }
}

fn generate_repository_key() -> [u8; 32] {
    let mut key = [0u8; 32];
    use rand_core::RngCore;
    rand_core::OsRng.fill_bytes(&mut key);
    key
}
