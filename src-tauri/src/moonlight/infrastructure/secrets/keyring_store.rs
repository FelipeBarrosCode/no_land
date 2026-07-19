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
    fn entry(&self, reference: &SecretReference) -> Result<keyring::Entry, MoonlightError> {
        keyring::Entry::new(&self.service_name, &reference.0)
            .map_err(|error| MoonlightError::SecretStore(error.to_string()))
    }
}

#[async_trait]
impl SecretStore for KeyringSecretStore {
    async fn get(
        &self,
        reference: &SecretReference,
    ) -> Result<Option<SecretBytes>, MoonlightError> {
        let entry = self.entry(reference)?;
        match entry.get_secret() {
            Ok(bytes) => Ok(Some(SecretBytes(bytes))),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(error) => Err(MoonlightError::SecretStore(error.to_string())),
        }
    }

    async fn put(
        &self,
        reference: &SecretReference,
        value: SecretBytes,
    ) -> Result<(), MoonlightError> {
        let entry = self.entry(reference)?;
        entry
            .set_secret(&value.0)
            .map_err(|error| MoonlightError::SecretStore(error.to_string()))
    }

    async fn remove(&self, reference: &SecretReference) -> Result<(), MoonlightError> {
        let entry = self.entry(reference)?;
        match entry.delete_credential() {
            Ok(_) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(error) => Err(MoonlightError::SecretStore(error.to_string())),
        }
    }
}
