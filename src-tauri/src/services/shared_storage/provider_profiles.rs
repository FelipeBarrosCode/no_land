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

        // One shared repository key covers every provider, so switching or
        // disconnecting a provider never breaks decryption of existing data.
        let repository_key_hex = ensure_repository_key(context).await?;

        let stored = SharedStorageProfileSecret {
            credentials: credentials.clone(),
            provider_fields: provider_fields.clone(),
            // Kept for backward compatibility with profiles persisted before
            // the shared key was hoisted to the credential state root.
            repository_key_hex: repository_key_hex,
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
        let credentials = &state.shared_storage_credentials;

        // Prefer the app-wide shared key. Fall back to the per-profile key
        // for installations that persisted their key before the shared slot
        // existed.
        let key_hex = if !credentials.repository_key_hex.is_empty() {
            credentials.repository_key_hex.clone()
        } else {
            credentials
                .profiles
                .get(&profile.id)
                .map(|stored| stored.repository_key_hex.clone())
                .filter(|key| !key.is_empty())
                .ok_or_else(|| {
                    AppError::NotFound("Repository key not found in state".to_string())
                })?
        };

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
        // Hoist the shared repository key out of this profile (if it was not
        // migrated to the shared slot yet) so deleting the provider never
        // removes the ability to decrypt existing repository data.
        ensure_repository_key(context).await?;

        context
            .update_state(|state| {
                // Fully delete the stored credential entry (including the
                // OAuth token material for OAuth-backed profiles).
                state
                    .shared_storage_credentials
                    .profiles
                    .remove(&profile.id);
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

/// Returns the single app-wide repository key, creating it on first use.
///
/// The key is shared across all storage providers: it encrypts the repository
/// contents regardless of which provider session was used to upload them, so
/// disconnecting a provider must never delete it.
///
/// For state files created before the shared slot existed, the first
/// per-profile key is adopted as the shared key so existing data stays
/// decryptable.
async fn ensure_repository_key(context: &AppContext) -> AppResult<String> {
    let existing_key = {
        let state = context.load_state().await;
        if !state
            .shared_storage_credentials
            .repository_key_hex
            .is_empty()
        {
            return Ok(state.shared_storage_credentials.repository_key_hex.clone());
        }
        state
            .shared_storage_credentials
            .profiles
            .values()
            .find_map(|stored| {
                (!stored.repository_key_hex.is_empty()).then(|| stored.repository_key_hex.clone())
            })
    };

    let key = existing_key.unwrap_or_else(|| hex::encode(generate_repository_key()));

    context
        .update_state(|state| {
            if state
                .shared_storage_credentials
                .repository_key_hex
                .is_empty()
            {
                state.shared_storage_credentials.repository_key_hex = key.clone();
            }
        })
        .await?;

    Ok(key)
}
