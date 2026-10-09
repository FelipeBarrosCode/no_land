use std::{
    collections::{HashMap, HashSet},
    sync::{Arc, Mutex},
};

use async_trait::async_trait;
use keyring::Entry;
use serde::{de::DeserializeOwned, Serialize};
use sha2::{Digest, Sha256};

use crate::{
    errors::{AppError, AppResult},
    models::app_state::PersistedAppState,
};

use super::state_store::StateStore;

const SERVICE_PREFIX: &str = "noland-connect.state-secrets.v1";
const APP_PASSWORD: &str = "credentials.app-password";
const VAST_API_KEY: &str = "credentials.vast-api-key";
const TWITCH_CLIENT_SECRET: &str = "credentials.twitch-client-secret";
const SSH_PASSWORD: &str = "ssh.password";
const LEGACY_B2_KEY_ID: &str = "shared-storage.legacy.backblaze-key-id";
const LEGACY_B2_APPLICATION_KEY: &str = "shared-storage.legacy.backblaze-application-key";
const LEGACY_CRYPT_PASSWORD: &str = "shared-storage.legacy.crypt-password";
const REPOSITORY_KEY: &str = "shared-storage.repository-key";
const WIREGUARD_CONFIG: &str = "wireguard.in-progress-config";
const PROFILE_MANIFEST: &str = "shared-storage.profile-manifest";
const OAUTH_MANIFEST: &str = "shared-storage.oauth-manifest";

trait StateSecretBackend: Send + Sync {
    fn get(&self, account: &str) -> AppResult<Option<String>>;
    fn put(&self, account: &str, value: &str) -> AppResult<()>;
    fn delete(&self, account: &str) -> AppResult<()>;
}

struct KeyringStateSecrets {
    service: String,
    legacy_service: Option<String>,
    cache: Mutex<HashMap<String, Option<String>>>,
}

impl KeyringStateSecrets {
    fn new(namespace: &str, migrate_unnamespaced: bool) -> Self {
        Self {
            service: format!("{SERVICE_PREFIX}.{namespace}"),
            legacy_service: migrate_unnamespaced.then(|| SERVICE_PREFIX.to_string()),
            cache: Mutex::new(HashMap::new()),
        }
    }

    fn entry_for(service: &str, account: &str) -> AppResult<Entry> {
        Entry::new(service, account).map_err(|error| {
            AppError::SecureStorage(format!("Could not access secure state storage: {error}"))
        })
    }

    fn entry(&self, account: &str) -> AppResult<Entry> {
        Self::entry_for(&self.service, account)
    }

    fn cache_lock(&self) -> AppResult<std::sync::MutexGuard<'_, HashMap<String, Option<String>>>> {
        self.cache
            .lock()
            .map_err(|_| AppError::SecureStorage("Secure state cache is poisoned".to_string()))
    }
}

impl StateSecretBackend for KeyringStateSecrets {
    fn get(&self, account: &str) -> AppResult<Option<String>> {
        if let Some(value) = self.cache_lock()?.get(account).cloned() {
            return Ok(value);
        }
        let mut value = match self.entry(account)?.get_password() {
            Ok(value) => Some(value),
            Err(keyring::Error::NoEntry) => None,
            Err(error) => {
                return Err(AppError::SecureStorage(format!(
                    "Could not read protected state from secure storage: {error}"
                )))
            }
        };
        if value.is_none() {
            if let Some(legacy_service) = &self.legacy_service {
                let legacy_entry = Self::entry_for(legacy_service, account)?;
                match legacy_entry.get_password() {
                    Ok(legacy_value) => {
                        self.entry(account)?
                            .set_password(&legacy_value)
                            .map_err(|error| {
                                AppError::SecureStorage(format!(
                                    "Could not migrate protected state to its namespaced secure storage: {error}"
                                ))
                            })?;
                        let verified = self.entry(account)?.get_password().map_err(|error| {
                            AppError::SecureStorage(format!(
                                "Could not verify namespaced protected state: {error}"
                            ))
                        })?;
                        if verified != legacy_value {
                            return Err(AppError::SecureStorage(
                                "Namespaced protected state failed migration verification"
                                    .to_string(),
                            ));
                        }
                        match legacy_entry.delete_credential() {
                            Ok(()) | Err(keyring::Error::NoEntry) => {}
                            Err(error) => {
                                return Err(AppError::SecureStorage(format!(
                                    "Could not remove migrated legacy protected state: {error}"
                                )))
                            }
                        }
                        value = Some(legacy_value);
                    }
                    Err(keyring::Error::NoEntry) => {}
                    Err(error) => {
                        return Err(AppError::SecureStorage(format!(
                            "Could not read legacy protected state: {error}"
                        )))
                    }
                }
            }
        }
        self.cache_lock()?
            .insert(account.to_string(), value.clone());
        Ok(value)
    }

    fn put(&self, account: &str, value: &str) -> AppResult<()> {
        if self.get(account)?.as_deref() == Some(value) {
            return Ok(());
        }
        self.entry(account)?.set_password(value).map_err(|error| {
            AppError::SecureStorage(format!(
                "Could not save protected state in secure storage: {error}"
            ))
        })?;
        let verified = self.entry(account)?.get_password().map_err(|error| {
            AppError::SecureStorage(format!(
                "Could not verify protected state in secure storage: {error}"
            ))
        })?;
        if verified != value {
            return Err(AppError::SecureStorage(
                "Protected state did not match its secure-storage read-back".to_string(),
            ));
        }
        self.cache_lock()?
            .insert(account.to_string(), Some(value.to_string()));
        Ok(())
    }

    fn delete(&self, account: &str) -> AppResult<()> {
        match self.entry(account)?.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => {}
            Err(error) => {
                return Err(AppError::SecureStorage(format!(
                    "Could not remove protected state from secure storage: {error}"
                )))
            }
        }
        self.cache_lock()?.insert(account.to_string(), None);
        Ok(())
    }
}

pub struct KeychainBackedStateStore {
    inner: Arc<dyn StateStore>,
    secrets: Arc<dyn StateSecretBackend>,
}

impl KeychainBackedStateStore {
    pub fn new(inner: Arc<dyn StateStore>, namespace: &str, migrate_unnamespaced: bool) -> Self {
        Self {
            inner,
            secrets: Arc::new(KeyringStateSecrets::new(namespace, migrate_unnamespaced)),
        }
    }

    #[cfg(test)]
    fn with_backend(inner: Arc<dyn StateStore>, secrets: Arc<dyn StateSecretBackend>) -> Self {
        Self { inner, secrets }
    }

    fn hydrate_or_migrate(&self, state: &mut PersistedAppState) -> AppResult<()> {
        hydrate_string(
            self.secrets.as_ref(),
            APP_PASSWORD,
            &mut state.credentials.app_password,
        )?;
        hydrate_string(
            self.secrets.as_ref(),
            VAST_API_KEY,
            &mut state.credentials.vast_api_key,
        )?;
        hydrate_string(
            self.secrets.as_ref(),
            TWITCH_CLIENT_SECRET,
            &mut state.credentials.twitch_client_secret,
        )?;
        hydrate_string(
            self.secrets.as_ref(),
            SSH_PASSWORD,
            &mut state.ssh.ssh_password,
        )?;
        hydrate_string(
            self.secrets.as_ref(),
            LEGACY_B2_KEY_ID,
            &mut state.shared_storage.settings.backblaze_key_id,
        )?;
        hydrate_string(
            self.secrets.as_ref(),
            LEGACY_B2_APPLICATION_KEY,
            &mut state.shared_storage.settings.backblaze_application_key,
        )?;
        hydrate_optional_string(
            self.secrets.as_ref(),
            LEGACY_CRYPT_PASSWORD,
            &mut state.shared_storage.settings.crypt_password,
        )?;
        hydrate_string(
            self.secrets.as_ref(),
            REPOSITORY_KEY,
            &mut state.shared_storage_credentials.repository_key_hex,
        )?;
        hydrate_string(
            self.secrets.as_ref(),
            WIREGUARD_CONFIG,
            &mut state.post_wireguard_setup.wireguard_config,
        )?;

        hydrate_dynamic_map(
            self.secrets.as_ref(),
            PROFILE_MANIFEST,
            "shared-storage.profile",
            &mut state.shared_storage_credentials.profiles,
        )?;
        hydrate_dynamic_map(
            self.secrets.as_ref(),
            OAUTH_MANIFEST,
            "shared-storage.oauth",
            &mut state.shared_storage_credentials.oauth_sessions,
        )?;
        Ok(())
    }

    fn protect_for_save(&self, state: &PersistedAppState) -> AppResult<PersistedAppState> {
        protect_string(
            self.secrets.as_ref(),
            APP_PASSWORD,
            &state.credentials.app_password,
        )?;
        protect_string(
            self.secrets.as_ref(),
            VAST_API_KEY,
            &state.credentials.vast_api_key,
        )?;
        protect_string(
            self.secrets.as_ref(),
            TWITCH_CLIENT_SECRET,
            &state.credentials.twitch_client_secret,
        )?;
        protect_string(self.secrets.as_ref(), SSH_PASSWORD, &state.ssh.ssh_password)?;
        protect_string(
            self.secrets.as_ref(),
            LEGACY_B2_KEY_ID,
            &state.shared_storage.settings.backblaze_key_id,
        )?;
        protect_string(
            self.secrets.as_ref(),
            LEGACY_B2_APPLICATION_KEY,
            &state.shared_storage.settings.backblaze_application_key,
        )?;
        protect_optional_string(
            self.secrets.as_ref(),
            LEGACY_CRYPT_PASSWORD,
            state.shared_storage.settings.crypt_password.as_deref(),
        )?;
        protect_string(
            self.secrets.as_ref(),
            REPOSITORY_KEY,
            &state.shared_storage_credentials.repository_key_hex,
        )?;
        protect_string(
            self.secrets.as_ref(),
            WIREGUARD_CONFIG,
            &state.post_wireguard_setup.wireguard_config,
        )?;
        protect_dynamic_map(
            self.secrets.as_ref(),
            PROFILE_MANIFEST,
            "shared-storage.profile",
            &state.shared_storage_credentials.profiles,
        )?;
        protect_dynamic_map(
            self.secrets.as_ref(),
            OAUTH_MANIFEST,
            "shared-storage.oauth",
            &state.shared_storage_credentials.oauth_sessions,
        )?;

        let mut scrubbed = state.clone();
        scrubbed.credentials.app_password.clear();
        scrubbed.credentials.vast_api_key.clear();
        scrubbed.credentials.twitch_client_secret.clear();
        scrubbed.ssh.ssh_password.clear();
        scrubbed.shared_storage.settings.backblaze_key_id.clear();
        scrubbed
            .shared_storage
            .settings
            .backblaze_application_key
            .clear();
        scrubbed.shared_storage.settings.crypt_password = None;
        scrubbed.shared_storage_credentials.profiles.clear();
        scrubbed.shared_storage_credentials.oauth_sessions.clear();
        scrubbed
            .shared_storage_credentials
            .repository_key_hex
            .clear();
        scrubbed.post_wireguard_setup.wireguard_config.clear();
        Ok(scrubbed)
    }
}

#[async_trait]
impl StateStore for KeychainBackedStateStore {
    async fn load_state(&self) -> AppResult<PersistedAppState> {
        let mut state = self.inner.load_state().await?;
        self.hydrate_or_migrate(&mut state)?;
        let scrubbed = self.protect_for_save(&state)?;
        self.inner.save_state(&scrubbed).await?;
        Ok(state)
    }

    async fn save_state(&self, state: &PersistedAppState) -> AppResult<()> {
        let scrubbed = self.protect_for_save(state)?;
        self.inner.save_state(&scrubbed).await
    }

    fn path(&self) -> &std::path::Path {
        self.inner.path()
    }
}

fn hydrate_string(
    secrets: &dyn StateSecretBackend,
    account: &str,
    value: &mut String,
) -> AppResult<()> {
    if value.is_empty() {
        if let Some(stored) = secrets.get(account)? {
            *value = stored;
        }
    } else {
        secrets.put(account, value)?;
    }
    Ok(())
}

fn hydrate_optional_string(
    secrets: &dyn StateSecretBackend,
    account: &str,
    value: &mut Option<String>,
) -> AppResult<()> {
    match value.as_ref().filter(|value| !value.is_empty()) {
        Some(value) => secrets.put(account, value),
        None => {
            *value = secrets.get(account)?;
            Ok(())
        }
    }
}

fn protect_string(secrets: &dyn StateSecretBackend, account: &str, value: &str) -> AppResult<()> {
    if value.is_empty() {
        secrets.delete(account)
    } else {
        secrets.put(account, value)
    }
}

fn protect_optional_string(
    secrets: &dyn StateSecretBackend,
    account: &str,
    value: Option<&str>,
) -> AppResult<()> {
    match value.filter(|value| !value.is_empty()) {
        Some(value) => secrets.put(account, value),
        None => secrets.delete(account),
    }
}

fn dynamic_account(prefix: &str, id: &str) -> String {
    let digest = Sha256::digest(id.as_bytes());
    format!("{prefix}.{}", hex::encode(digest))
}

fn load_manifest(secrets: &dyn StateSecretBackend, account: &str) -> AppResult<HashSet<String>> {
    secrets
        .get(account)?
        .map(|value| {
            serde_json::from_str::<Vec<String>>(&value)
                .map(|ids| ids.into_iter().collect())
                .map_err(|error| {
                    AppError::SecureStorage(format!("Secure state manifest is invalid: {error}"))
                })
        })
        .transpose()
        .map(|value| value.unwrap_or_default())
}

fn save_manifest(
    secrets: &dyn StateSecretBackend,
    account: &str,
    ids: &HashSet<String>,
) -> AppResult<()> {
    if ids.is_empty() {
        return secrets.delete(account);
    }
    let mut ids = ids.iter().cloned().collect::<Vec<_>>();
    ids.sort();
    secrets.put(account, &serde_json::to_string(&ids)?)
}

fn hydrate_dynamic_map<T>(
    secrets: &dyn StateSecretBackend,
    manifest_account: &str,
    account_prefix: &str,
    values: &mut HashMap<String, T>,
) -> AppResult<()>
where
    T: Clone + Serialize + DeserializeOwned,
{
    let mut ids = load_manifest(secrets, manifest_account)?;
    for (id, value) in values.iter() {
        secrets.put(
            &dynamic_account(account_prefix, id),
            &serde_json::to_string(value)?,
        )?;
        ids.insert(id.clone());
    }
    for id in ids.clone() {
        if values.contains_key(&id) {
            continue;
        }
        if let Some(serialized) = secrets.get(&dynamic_account(account_prefix, &id))? {
            let value = serde_json::from_str(&serialized).map_err(|error| {
                AppError::SecureStorage(format!("Protected state entry is invalid: {error}"))
            })?;
            values.insert(id, value);
        }
    }
    save_manifest(secrets, manifest_account, &ids)
}

fn protect_dynamic_map<T>(
    secrets: &dyn StateSecretBackend,
    manifest_account: &str,
    account_prefix: &str,
    values: &HashMap<String, T>,
) -> AppResult<()>
where
    T: Serialize,
{
    let previous_ids = load_manifest(secrets, manifest_account)?;
    let current_ids = values.keys().cloned().collect::<HashSet<_>>();
    for removed in previous_ids.difference(&current_ids) {
        secrets.delete(&dynamic_account(account_prefix, removed))?;
    }
    for (id, value) in values {
        secrets.put(
            &dynamic_account(account_prefix, id),
            &serde_json::to_string(value)?,
        )?;
    }
    save_manifest(secrets, manifest_account, &current_ids)
}

#[cfg(test)]
mod tests {
    use std::{collections::HashMap, path::PathBuf, sync::Arc};

    use super::*;
    use crate::{
        models::{
            app_state::{
                SharedStorageCredentialState, SharedStorageOAuthSessionSecret,
                SharedStorageProfileSecret,
            },
            application_bundle::StorageCredential,
        },
        services::state_store::JsonStateStore,
    };

    #[derive(Default)]
    struct MemorySecrets(Mutex<HashMap<String, String>>);

    impl StateSecretBackend for MemorySecrets {
        fn get(&self, account: &str) -> AppResult<Option<String>> {
            Ok(self.0.lock().unwrap().get(account).cloned())
        }
        fn put(&self, account: &str, value: &str) -> AppResult<()> {
            self.0
                .lock()
                .unwrap()
                .insert(account.to_string(), value.to_string());
            Ok(())
        }
        fn delete(&self, account: &str) -> AppResult<()> {
            self.0.lock().unwrap().remove(account);
            Ok(())
        }
    }

    fn path(name: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "noland-protected-state-{name}-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&root).unwrap();
        root.join("state.json")
    }

    fn populated_state() -> PersistedAppState {
        let mut state = PersistedAppState::default();
        state.credentials.app_password = "app-password".into();
        state.credentials.vast_api_key = "vast-key".into();
        state.credentials.twitch_client_secret = "twitch-secret".into();
        state.ssh.ssh_password = "ssh-password".into();
        state.shared_storage.settings.backblaze_key_id = "b2-id".into();
        state.shared_storage.settings.backblaze_application_key = "b2-key".into();
        state.shared_storage.settings.crypt_password = Some("crypt-password".into());
        state.shared_storage_credentials = SharedStorageCredentialState {
            repository_key_hex: "ab".repeat(32),
            ..Default::default()
        };
        state.shared_storage_credentials.profiles.insert(
            "profile-1".into(),
            SharedStorageProfileSecret {
                credentials: StorageCredential::UsernamePassword {
                    username: "storage-user".into(),
                    password: "storage-password".into(),
                },
                provider_fields: HashMap::from([("client_secret".into(), "value".into())]),
                repository_key_hex: "cd".repeat(32),
            },
        );
        state.shared_storage_credentials.oauth_sessions.insert(
            "session-1".into(),
            SharedStorageOAuthSessionSecret {
                credentials: StorageCredential::OAuth2 {
                    access_token: "access-token".into(),
                    refresh_token: Some("refresh-token".into()),
                    expires_at: 123,
                },
            },
        );
        state.post_wireguard_setup.wireguard_config =
            "[Interface]\nPrivateKey = private-key".into();
        state
    }

    #[tokio::test]
    async fn persists_no_plaintext_secrets_and_hydrates_them_on_load() {
        let state_path = path("round-trip");
        let backend = Arc::new(MemorySecrets::default());
        let inner: Arc<dyn StateStore> = Arc::new(JsonStateStore::new(state_path.clone(), 4));
        let store = KeychainBackedStateStore::with_backend(inner, backend);
        let expected = populated_state();

        store.save_state(&expected).await.unwrap();
        let json = std::fs::read_to_string(&state_path).unwrap();
        for forbidden in [
            "app-password",
            "vast-key",
            "twitch-secret",
            "ssh-password",
            "b2-id",
            "b2-key",
            "crypt-password",
            "storage-password",
            "access-token",
            "refresh-token",
            "private-key",
            &"ab".repeat(32),
        ] {
            assert!(!json.contains(forbidden), "state JSON leaked {forbidden}");
        }

        let loaded = store.load_state().await.unwrap();
        assert_eq!(
            loaded.credentials.app_password,
            expected.credentials.app_password
        );
        assert_eq!(
            loaded.credentials.vast_api_key,
            expected.credentials.vast_api_key
        );
        assert_eq!(loaded.ssh.ssh_password, expected.ssh.ssh_password);
        assert!(loaded
            .shared_storage_credentials
            .profiles
            .contains_key("profile-1"));
        assert!(loaded
            .shared_storage_credentials
            .oauth_sessions
            .contains_key("session-1"));
        assert_eq!(
            loaded.post_wireguard_setup.wireguard_config,
            expected.post_wireguard_setup.wireguard_config
        );
    }

    #[tokio::test]
    async fn migrates_legacy_plaintext_then_scrubs_the_json() {
        let state_path = path("migration");
        let legacy = populated_state();
        std::fs::write(&state_path, serde_json::to_vec_pretty(&legacy).unwrap()).unwrap();
        let backend = Arc::new(MemorySecrets::default());
        let inner: Arc<dyn StateStore> = Arc::new(JsonStateStore::new(state_path.clone(), 4));
        let store = KeychainBackedStateStore::with_backend(inner, backend);

        let loaded = store.load_state().await.unwrap();
        assert_eq!(loaded.credentials.vast_api_key, "vast-key");
        assert_eq!(loaded.ssh.ssh_password, "ssh-password");
        let json = std::fs::read_to_string(state_path).unwrap();
        assert!(!json.contains("vast-key"));
        assert!(!json.contains("storage-password"));
        assert!(!json.contains("private-key"));
    }

    #[tokio::test]
    async fn removes_deleted_dynamic_credentials_from_secure_storage() {
        let state_path = path("deletion");
        let backend = Arc::new(MemorySecrets::default());
        let inner: Arc<dyn StateStore> = Arc::new(JsonStateStore::new(state_path, 4));
        let store = KeychainBackedStateStore::with_backend(inner, backend.clone());
        let mut state = populated_state();
        store.save_state(&state).await.unwrap();

        state.shared_storage_credentials.profiles.clear();
        state.shared_storage_credentials.oauth_sessions.clear();
        store.save_state(&state).await.unwrap();
        assert!(backend
            .get(&dynamic_account("shared-storage.profile", "profile-1"))
            .unwrap()
            .is_none());
        assert!(backend
            .get(&dynamic_account("shared-storage.oauth", "session-1"))
            .unwrap()
            .is_none());
    }
}
