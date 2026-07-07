use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use tauri::{AppHandle, Emitter};
use tokio::sync::{Mutex, RwLock};
use tracing::{error, warn};

use crate::{
    errors::AppResult,
    models::{
        app_state::{OfferCandidate, PairingContext, PersistedAppState},
        events::ProvisioningEvent,
    },
};

use super::{app_config::AppConfig, state_store::StateStore};

#[derive(Clone)]
pub struct AppContext {
    pub config: AppConfig,
    pub state_store: Arc<dyn StateStore>,
    pub state: Arc<RwLock<PersistedAppState>>,
    pub http_client: reqwest::Client,
    pub provisioning_logs: Arc<RwLock<Vec<ProvisioningEvent>>>,
    pub offer_cache: Arc<RwLock<Vec<OfferCandidate>>>,
    pub pairing_context: Arc<RwLock<Option<PairingContext>>>,
    pub pairing_pin_in_memory: Arc<RwLock<Option<String>>>,
    pub orchestration_guard: Arc<Mutex<bool>>,
    pub cancel_requested: Arc<AtomicBool>,
    pub pending_start: Arc<Mutex<Option<OrchestrationStartRequest>>>,
    pub wireguard_mutation_in_progress: Arc<AtomicBool>,
}

pub struct WireGuardMutationGuard {
    flag: Arc<AtomicBool>,
}

impl Drop for WireGuardMutationGuard {
    fn drop(&mut self) {
        self.flag.store(false, Ordering::SeqCst);
    }
}

#[derive(Debug, Clone, Copy)]
pub enum OrchestrationStartRequest {
    SelectedOffer,
    ExistingInstance(u64),
}

impl AppContext {
    pub fn new(
        config: AppConfig,
        state_store: Arc<dyn StateStore>,
        initial_state: PersistedAppState,
    ) -> Self {
        Self {
            config,
            state_store,
            state: Arc::new(RwLock::new(initial_state)),
            http_client: reqwest::Client::new(),
            provisioning_logs: Arc::new(RwLock::new(Vec::new())),
            offer_cache: Arc::new(RwLock::new(Vec::new())),
            pairing_context: Arc::new(RwLock::new(None)),
            pairing_pin_in_memory: Arc::new(RwLock::new(None)),
            orchestration_guard: Arc::new(Mutex::new(false)),
            cancel_requested: Arc::new(AtomicBool::new(false)),
            pending_start: Arc::new(Mutex::new(None)),
            wireguard_mutation_in_progress: Arc::new(AtomicBool::new(false)),
        }
    }

    pub fn begin_wireguard_mutation(&self) -> WireGuardMutationGuard {
        self.wireguard_mutation_in_progress
            .store(true, Ordering::SeqCst);
        WireGuardMutationGuard {
            flag: self.wireguard_mutation_in_progress.clone(),
        }
    }

    pub fn wireguard_mutation_in_progress(&self) -> bool {
        self.wireguard_mutation_in_progress.load(Ordering::SeqCst)
    }

    pub async fn update_state<F>(&self, update: F) -> AppResult<PersistedAppState>
    where
        F: FnOnce(&mut PersistedAppState),
    {
        let next_state = {
            let mut state = self.state.write().await;
            update(&mut state);
            state.clone()
        };

        self.state_store.save_state(&next_state).await?;
        Ok(next_state)
    }

    pub async fn load_state(&self) -> PersistedAppState {
        self.state.read().await.clone()
    }

    pub async fn reload_state_from_disk(&self) -> AppResult<PersistedAppState> {
        let fresh_state = self.state_store.load_state().await?;
        {
            let mut state = self.state.write().await;
            *state = fresh_state.clone();
        }
        Ok(fresh_state)
    }

    pub async fn emit_progress(&self, app: &AppHandle, event: ProvisioningEvent) {
        {
            let mut logs = self.provisioning_logs.write().await;
            logs.insert(0, event.clone());
            logs.truncate(500);
        }

        if let Err(error) = app.emit("orchestration:progress", event.clone()) {
            warn!("failed to emit orchestration progress: {error}");
        }

        if event.is_error {
            error!("{}", event.message);
            if let Some(details) = event.details {
                error!("{details}");
            }
        }
    }
}
