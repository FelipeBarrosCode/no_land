use std::{path::PathBuf, sync::Arc};

use super::{
    application::{
        bootstrap::{bootstrap_client_identity, ClientIdentityBootstrapResult},
        pairing::PairingSessionStore,
    },
    infrastructure::{
        persistence::JsonMoonlightStateRepository,
        secrets::{KeyringSecretStore, SecretStore},
    },
    runtime::{spawn_runtime_actor, MoonlightRuntimeHandle},
};

pub async fn bootstrap_default_services(
    state_path: PathBuf,
) -> Result<ClientIdentityBootstrapResult, crate::moonlight::domain::MoonlightError> {
    let repository = JsonMoonlightStateRepository::new(state_path);
    let secret_store = KeyringSecretStore::default();
    bootstrap_client_identity(&repository, &secret_store).await
}

pub struct MoonlightManager {
    pub state_path: PathBuf,
    pub app_data_dir: PathBuf,
    pub repository: Arc<JsonMoonlightStateRepository>,
    pub secret_store: Arc<dyn SecretStore>,
    pub pairing_sessions: PairingSessionStore,
    pub runtime: MoonlightRuntimeHandle,
}

impl MoonlightManager {
    pub fn new(state_path: PathBuf, app_data_dir: PathBuf) -> Self {
        Self {
            state_path: state_path.clone(),
            app_data_dir,
            repository: Arc::new(JsonMoonlightStateRepository::new(state_path)),
            secret_store: Arc::new(KeyringSecretStore::default()),
            pairing_sessions: PairingSessionStore::default(),
            runtime: spawn_runtime_actor(),
        }
    }
}
