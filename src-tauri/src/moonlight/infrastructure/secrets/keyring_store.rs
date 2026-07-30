use async_trait::async_trait;

use super::secret_store::{SecretBytes, SecretStore};
use crate::moonlight::domain::{MoonlightError, SecretReference};

#[derive(Debug, Clone)]
pub struct KeyringSecretStore {
    service_name: String,
}

impl Default for KeyringSecretStore {
    fn default() -> Self {
        Self {
            service_name: "noland-connect.moonlight".to_string(),
        }
    }
}

impl KeyringSecretStore {
    fn candidate_accounts(reference: &SecretReference) -> Vec<String> {
        let raw = reference.0.trim();
        let mut candidates = Vec::new();

        if !raw.is_empty() {
            candidates.push(raw.to_string());
        }

        if let Some(stripped) = raw.strip_prefix("os-keychain://") {
            let stripped = stripped.trim_matches('/');
            if !stripped.is_empty() {
                candidates.push(stripped.to_string());
                if let Some(last_segment) = stripped.rsplit('/').next() {
                    let last_segment = last_segment.trim();
                    if !last_segment.is_empty() {
                        candidates.push(last_segment.to_string());
                    }
                }
            }
        }

        candidates.sort();
        candidates.dedup();
        candidates
    }

    fn entry_for_account(&self, account: &str) -> Result<keyring::Entry, MoonlightError> {
        keyring::Entry::new(&self.service_name, account)
            .map_err(|error| MoonlightError::SecretStore(error.to_string()))
    }
}

#[async_trait]
impl SecretStore for KeyringSecretStore {
    async fn get(
        &self,
        reference: &SecretReference,
    ) -> Result<Option<SecretBytes>, MoonlightError> {
        for account in Self::candidate_accounts(reference) {
            let entry = self.entry_for_account(&account)?;
            match entry.get_password() {
                Ok(value) => return Ok(Some(SecretBytes(value.into_bytes()))),
                Err(keyring::Error::NoEntry) => continue,
                Err(error) => return Err(MoonlightError::SecretStore(error.to_string())),
            }
        }

        Ok(None)
    }

    async fn put(
        &self,
        reference: &SecretReference,
        value: SecretBytes,
    ) -> Result<(), MoonlightError> {
        let accounts = Self::candidate_accounts(reference);
        if accounts.is_empty() {
            return Err(MoonlightError::SecretStore(
                "secret reference is empty".to_string(),
            ));
        }

        let string_value = String::from_utf8(value.0)
            .map_err(|error| MoonlightError::SecretStore(error.to_string()))?;

        for account in accounts {
            let entry = self.entry_for_account(&account)?;
            entry
                .set_password(&string_value)
                .map_err(|error| MoonlightError::SecretStore(error.to_string()))?;
        }

        Ok(())
    }

    async fn remove(&self, reference: &SecretReference) -> Result<(), MoonlightError> {
        for account in Self::candidate_accounts(reference) {
            let entry = self.entry_for_account(&account)?;
            match entry.delete_credential() {
                Ok(_) | Err(keyring::Error::NoEntry) => {}
                Err(error) => return Err(MoonlightError::SecretStore(error.to_string())),
            }
        }

        Ok(())
    }
}
