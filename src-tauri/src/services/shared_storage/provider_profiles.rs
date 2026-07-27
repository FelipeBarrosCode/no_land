use std::{collections::HashMap, sync::Arc};
use uuid::Uuid;

use crate::errors::{AppError, AppResult};
use crate::models::application_bundle::{
    SharedStorageProfile, SharedStorageStatus, StorageProvider,
};

use super::credential_vault::{CredentialVault, InMemoryCredentialVault};
use super::object_storage::{list_all_providers, StorageCredential};

// ─── Profile Manager ──────────────────────────────────────

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct StoredProfileSecrets {
    credentials: StorageCredential,
    #[serde(default)]
    provider_fields: HashMap<String, String>,
}

static SHARED_VAULT: std::sync::OnceLock<Arc<InMemoryCredentialVault>> = std::sync::OnceLock::new();

pub fn shared_profile_vault() -> Arc<dyn CredentialVault> {
    SHARED_VAULT
        .get_or_init(|| Arc::new(InMemoryCredentialVault::new()))
        .clone()
}

pub fn shared_profile_manager() -> Arc<SharedStorageProfileManager> {
    Arc::new(SharedStorageProfileManager::new(shared_profile_vault()))
}

pub struct SharedStorageProfileManager {
    vault: Arc<dyn CredentialVault>,
}

impl SharedStorageProfileManager {
    pub fn new(vault: Arc<dyn CredentialVault>) -> Self {
        Self { vault }
    }

    /// List all available storage providers for the UI.
    pub fn list_providers(&self) -> Vec<crate::models::application_bundle::ProviderDefinition> {
        list_all_providers()
    }

    /// Save static provider credentials into the OS vault and return a profile.
    pub async fn save_static_credentials(
        &self,
        provider: &StorageProvider,
        credentials: &StorageCredential,
        provider_fields: &HashMap<String, String>,
        bucket: Option<&str>,
        prefix: Option<&str>,
        display_name: &str,
    ) -> AppResult<SharedStorageProfile> {
        let profile_id = Uuid::new_v4().to_string();
        let vault_key = format!("shared-storage:profile:{}:credentials", profile_id);
        let repo_key = format!("shared-storage:profile:{}:repo-key", profile_id);

        let stored = StoredProfileSecrets {
            credentials: credentials.clone(),
            provider_fields: provider_fields.clone(),
        };
        let cred_json = serde_json::to_string(&stored)?;
        self.vault.store(&vault_key, &cred_json).await?;

        // Generate repository encryption key
        let repo_master_key = generate_repository_key();
        self.vault
            .store(&repo_key, &hex::encode(&repo_master_key))
            .await?;

        let repository_id = format!(
            "repo_{}",
            Uuid::new_v4().to_string().replace('-', "")[..12].to_string()
        );

        Ok(SharedStorageProfile {
            id: profile_id,
            display_name: display_name.to_string(),
            provider: provider.clone(),
            provider_label: provider.label().to_string(),
            bucket: bucket.map(String::from),
            prefix: prefix.map(String::from),
            credential_vault_reference: vault_key,
            repository_id,
            status: SharedStorageStatus::Connected,
            last_verified_at: None,
            protected_bundles_count: 0,
            total_stored_bytes: 0,
        })
    }

    /// Save OAuth credentials (after OAuth flow completes).
    pub async fn save_oauth_credentials(
        &self,
        provider: &StorageProvider,
        credentials: &StorageCredential,
        provider_fields: &HashMap<String, String>,
        display_name: &str,
    ) -> AppResult<SharedStorageProfile> {
        self.save_static_credentials(
            provider,
            credentials,
            provider_fields,
            None,
            None,
            display_name,
        )
        .await
    }

    /// Retrieve credentials from vault for a profile.
    pub async fn retrieve_credentials(
        &self,
        profile: &SharedStorageProfile,
    ) -> AppResult<StorageCredential> {
        let cred_json = self
            .vault
            .retrieve(&profile.credential_vault_reference)
            .await?
            .ok_or_else(|| AppError::NotFound("Credentials not found in vault".to_string()))?;

        if let Ok(stored) = serde_json::from_str::<StoredProfileSecrets>(&cred_json) {
            return Ok(stored.credentials);
        }

        let credentials: StorageCredential = serde_json::from_str(&cred_json)?;
        Ok(credentials)
    }

    pub async fn retrieve_provider_fields(
        &self,
        profile: &SharedStorageProfile,
    ) -> AppResult<HashMap<String, String>> {
        let cred_json = self
            .vault
            .retrieve(&profile.credential_vault_reference)
            .await?
            .ok_or_else(|| AppError::NotFound("Profile secrets not found in vault".to_string()))?;

        if let Ok(stored) = serde_json::from_str::<StoredProfileSecrets>(&cred_json) {
            return Ok(stored.provider_fields);
        }

        Ok(HashMap::new())
    }

    /// Retrieve repository key.
    pub async fn retrieve_repository_key(
        &self,
        profile: &SharedStorageProfile,
    ) -> AppResult<[u8; 32]> {
        let repo_key = format!("shared-storage:profile:{}:repo-key", profile.id);
        let key_hex =
            self.vault.retrieve(&repo_key).await?.ok_or_else(|| {
                AppError::NotFound("Repository key not found in vault".to_string())
            })?;

        let key_bytes: [u8; 32] = hex::decode(&key_hex)
            .map_err(|e| AppError::State(format!("Invalid repository key: {e}")))?
            .try_into()
            .map_err(|_| AppError::State("Repository key has wrong length".to_string()))?;

        Ok(key_bytes)
    }

    /// Delete all vault entries for a profile.
    pub async fn delete_profile_credentials(
        &self,
        profile: &SharedStorageProfile,
    ) -> AppResult<()> {
        let _ = self.vault.delete(&profile.credential_vault_reference).await;
        let repo_key = format!("shared-storage:profile:{}:repo-key", profile.id);
        let _ = self.vault.delete(&repo_key).await;
        Ok(())
    }
}

// ─── Repository Key Generation ────────────────────────────

fn generate_repository_key() -> [u8; 32] {
    let mut key = [0u8; 32];
    // Use OS randomness (cryptographically secure)
    use rand_core::RngCore;
    rand_core::OsRng.fill_bytes(&mut key);
    key
}
