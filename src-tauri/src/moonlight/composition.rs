use std::{
    path::PathBuf,
    sync::{Arc, Mutex},
};

use crate::input::manager::InputManager;

use super::{
    application::{
        bootstrap::{bootstrap_client_identity, ClientIdentityBootstrapResult},
        pairing::PairingSessionStore,
    },
    domain::StreamPreferences,
    infrastructure::{
        persistence::JsonMoonlightStateRepository,
        secrets::{FileSecretStore, SecretStore},
    },
    runtime::{spawn_runtime_actor, MoonlightRuntimeHandle},
};

pub async fn bootstrap_default_services(
    state_path: PathBuf,
    app_data_dir: PathBuf,
) -> Result<ClientIdentityBootstrapResult, crate::moonlight::domain::MoonlightError> {
    let repository = JsonMoonlightStateRepository::new(state_path);
    let secret_store = FileSecretStore::new(app_data_dir.join("moonlight").join("identity"));
    bootstrap_client_identity(&repository, &secret_store).await
}

pub struct MoonlightManager {
    pub state_path: PathBuf,
    pub app_data_dir: PathBuf,
    pub repository: Arc<JsonMoonlightStateRepository>,
    pub secret_store: Arc<dyn SecretStore>,
    pub pairing_sessions: PairingSessionStore,
    pub runtime: MoonlightRuntimeHandle,
    pub input: Arc<InputManager>,
    pub active_session_preferences: Arc<Mutex<Option<StreamPreferences>>>,
    /// Instance whose independent microphone path should be stopped when the
    /// current game stream ends.
    pub active_stream_instance_id: Arc<Mutex<Option<u64>>>,
}

impl MoonlightManager {
    pub fn new(state_path: PathBuf, app_data_dir: PathBuf) -> Self {
        let identity_dir = app_data_dir.join("moonlight").join("identity");
        let runtime = spawn_runtime_actor();
        let input = InputManager::new(runtime.clone());
        Self {
            state_path: state_path.clone(),
            app_data_dir,
            repository: Arc::new(JsonMoonlightStateRepository::new(state_path)),
            secret_store: Arc::new(FileSecretStore::new(identity_dir)),
            pairing_sessions: PairingSessionStore::default(),
            runtime,
            input,
            active_session_preferences: Arc::new(Mutex::new(None)),
            active_stream_instance_id: Arc::new(Mutex::new(None)),
        }
    }
}
