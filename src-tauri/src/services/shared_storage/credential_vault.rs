use async_trait::async_trait;
use keyring::Entry;

use crate::errors::{AppError, AppResult};

// ─── Credential Vault ──────────────────────────────────────

#[async_trait]
pub trait CredentialVault: Send + Sync {
    async fn store(&self, key: &str, value: &str) -> AppResult<()>;
    async fn retrieve(&self, key: &str) -> AppResult<Option<String>>;
    async fn delete(&self, key: &str) -> AppResult<()>;
}

// ─── OS Keyring Implementation ────────────────────────────

pub struct KeyringCredentialVault {
    service_name: String,
}

impl KeyringCredentialVault {
    pub fn new(service_name: &str) -> Self {
        tracing::info!("KeyringCredentialVault created: service='{}'", service_name);
        Self {
            service_name: service_name.to_string(),
        }
    }
}

#[async_trait]
impl CredentialVault for KeyringCredentialVault {
    async fn store(&self, key: &str, value: &str) -> AppResult<()> {
        tracing::info!("VAULT STORE key='{}' val_len={}", key, value.len());
        let entry = Entry::new(&self.service_name, key).map_err(|e| {
            tracing::warn!("VAULT STORE Entry::new FAILED: {}", e);
            AppError::State(format!("Failed to create keyring entry: {e}"))
        })?;

        entry.set_password(value).map_err(|e| {
            tracing::warn!("VAULT STORE set_password FAILED: {}", e);
            AppError::State(format!("Failed to store credential in keyring: {e}"))
        })?;
        tracing::info!("VAULT STORE OK key='{}'", key);
        Ok(())
    }

    async fn retrieve(&self, key: &str) -> AppResult<Option<String>> {
        tracing::info!("VAULT GET key='{}'", key);
        let entry = Entry::new(&self.service_name, key).map_err(|e| {
            tracing::warn!("VAULT GET Entry::new FAILED: {}", e);
            AppError::State(format!("Failed to access keyring: {e}"))
        })?;

        match entry.get_password() {
            Ok(value) => {
                tracing::info!("VAULT GET OK: found {} bytes", value.len());
                Ok(Some(value))
            }
            Err(keyring::Error::NoEntry) => {
                tracing::warn!("VAULT GET NoEntry for key='{}'", key);
                Ok(None)
            }
            Err(e) => {
                tracing::warn!("VAULT GET error: {}", e);
                Err(AppError::State(format!(
                    "Failed to retrieve credential: {e}",
                )))
            }
        }
    }

    async fn delete(&self, key: &str) -> AppResult<()> {
        let entry = Entry::new(&self.service_name, key)
            .map_err(|e| AppError::State(format!("Failed to access keyring: {e}")))?;

        match entry.delete_credential() {
            Ok(_) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(e) => Err(AppError::State(format!("Failed to delete credential: {e}"))),
        }
    }
}

// ─── In-Memory Vault (for testing) ─────────────────────────

use std::collections::HashMap;
use tokio::sync::RwLock;

pub struct InMemoryCredentialVault {
    store: RwLock<HashMap<String, String>>,
}

impl InMemoryCredentialVault {
    pub fn new() -> Self {
        Self {
            store: RwLock::new(HashMap::new()),
        }
    }
}

#[async_trait]
impl CredentialVault for InMemoryCredentialVault {
    async fn store(&self, key: &str, value: &str) -> AppResult<()> {
        let mut store = self.store.write().await;
        store.insert(key.to_string(), value.to_string());
        Ok(())
    }

    async fn retrieve(&self, key: &str) -> AppResult<Option<String>> {
        let store = self.store.read().await;
        Ok(store.get(key).cloned())
    }

    async fn delete(&self, key: &str) -> AppResult<()> {
        let mut store = self.store.write().await;
        store.remove(key);
        Ok(())
    }
}
