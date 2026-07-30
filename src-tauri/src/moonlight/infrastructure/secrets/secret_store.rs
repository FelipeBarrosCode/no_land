use async_trait::async_trait;

use crate::moonlight::domain::{MoonlightError, SecretReference};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SecretBytes(pub Vec<u8>);

#[async_trait]
pub trait SecretStore: Send + Sync {
    async fn get(&self, reference: &SecretReference)
        -> Result<Option<SecretBytes>, MoonlightError>;

    async fn put(
        &self,
        reference: &SecretReference,
        value: SecretBytes,
    ) -> Result<(), MoonlightError>;

    async fn remove(&self, reference: &SecretReference) -> Result<(), MoonlightError>;
}

#[cfg(test)]
pub mod testsupport {
    use std::{collections::HashMap, sync::Mutex};

    use async_trait::async_trait;

    use super::{SecretBytes, SecretStore};
    use crate::moonlight::domain::{MoonlightError, SecretReference};

    #[derive(Default)]
    pub struct InMemorySecretStore {
        values: Mutex<HashMap<String, Vec<u8>>>,
    }

    #[async_trait]
    impl SecretStore for InMemorySecretStore {
        async fn get(
            &self,
            reference: &SecretReference,
        ) -> Result<Option<SecretBytes>, MoonlightError> {
            Ok(self
                .values
                .lock()
                .map_err(|_| {
                    MoonlightError::SecretStore("secret store mutex poisoned".to_string())
                })?
                .get(&reference.0)
                .cloned()
                .map(SecretBytes))
        }

        async fn put(
            &self,
            reference: &SecretReference,
            value: SecretBytes,
        ) -> Result<(), MoonlightError> {
            self.values
                .lock()
                .map_err(|_| {
                    MoonlightError::SecretStore("secret store mutex poisoned".to_string())
                })?
                .insert(reference.0.clone(), value.0);
            Ok(())
        }

        async fn remove(&self, reference: &SecretReference) -> Result<(), MoonlightError> {
            self.values
                .lock()
                .map_err(|_| {
                    MoonlightError::SecretStore("secret store mutex poisoned".to_string())
                })?
                .remove(&reference.0);
            Ok(())
        }
    }
}
