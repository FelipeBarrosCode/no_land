use std::path::PathBuf;

use super::{
    application::bootstrap::{bootstrap_client_identity, ClientIdentityBootstrapResult},
    infrastructure::{persistence::JsonMoonlightStateRepository, secrets::KeyringSecretStore},
};

pub async fn bootstrap_default_services(
    state_path: PathBuf,
) -> Result<ClientIdentityBootstrapResult, crate::moonlight::domain::MoonlightError> {
    let repository = JsonMoonlightStateRepository::new(state_path);
    let secret_store = KeyringSecretStore::default();
    bootstrap_client_identity(&repository, &secret_store).await
}
