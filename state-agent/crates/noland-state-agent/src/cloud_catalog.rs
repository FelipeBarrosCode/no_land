//! Only overlapping requests share a refresh. There is no cached "latest", no
//! negative cache, and pending-deletion state is always read from the provider.
use std::collections::HashMap;
use std::future::Future;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Weak};
use std::time::Instant;

use chrono::Utc;
use noland_crypto::MasterKey;
use noland_rclone_adapter::EphemeralRcloneSession;
use noland_state_core::{Result, StateError};
use noland_storage::{ProviderRootIdentity, SharedStorageProvider};
use parking_lot::Mutex;
use serde_json::{json, Value};
use tokio::sync::{watch, Semaphore};

use crate::{session_storage::SessionStorage, StateAgent};

type RefreshResult = std::result::Result<Arc<Value>, String>;

struct Flight {
    result: watch::Sender<Option<RefreshResult>>,
    invalidated: AtomicBool,
}

pub(crate) struct CatalogRefreshes {
    flights: Mutex<HashMap<String, Weak<Flight>>>,
    slots: Semaphore,
}

impl Default for CatalogRefreshes {
    fn default() -> Self {
        Self {
            flights: Mutex::new(HashMap::new()),
            slots: Semaphore::new(2),
        }
    }
}

struct Leader<'a> {
    owner: &'a CatalogRefreshes,
    key: String,
    flight: Arc<Flight>,
    completed: bool,
}

impl Drop for Leader<'_> {
    fn drop(&mut self) {
        let mut flights = self.owner.flights.lock();
        if flights
            .get(&self.key)
            .is_some_and(|entry| entry.ptr_eq(&Arc::downgrade(&self.flight)))
        {
            flights.remove(&self.key);
        }
        if !self.completed {
            self.flight.result.send_replace(Some(Err(
                "catalog refresh interrupted; retry the request".into(),
            )));
        }
    }
}

impl CatalogRefreshes {
    pub(crate) fn invalidate(&self) {
        let mut flights = self.flights.lock();
        for flight in flights.values().filter_map(Weak::upgrade) {
            flight.invalidated.store(true, Ordering::Release);
        }
        flights.clear();
    }

    async fn refresh<F, Fut>(&self, key: String, fetch: F) -> Result<Value>
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = Result<Value>>,
    {
        let requested = Instant::now();
        let (flight, leader) = {
            let mut flights = self.flights.lock();
            if let Some(flight) = flights.get(&key).and_then(Weak::upgrade) {
                (flight, false)
            } else {
                flights.retain(|_, entry| entry.strong_count() > 0);
                if flights.len() >= 16 {
                    return Err(StateError::Storage(
                        "catalog refresh queue is full; retry the request".into(),
                    ));
                }
                let (result, _) = watch::channel(None);
                let flight = Arc::new(Flight {
                    result,
                    invalidated: false.into(),
                });
                flights.insert(key.clone(), Arc::downgrade(&flight));
                (flight, true)
            }
        };
        let mut receiver = flight.result.subscribe();
        let result = if leader {
            let mut lease = Leader {
                owner: self,
                key,
                flight,
                completed: false,
            };
            let _slot = self
                .slots
                .acquire()
                .await
                .map_err(|_| StateError::Storage("catalog refresh queue closed".into()))?;
            let queue_ms = elapsed_ms(requested);
            let mut result = fetch().await;
            if lease.flight.invalidated.load(Ordering::Acquire) {
                result = Err(StateError::Conflict(
                    "repository changed during catalog refresh; retry the request".into(),
                ));
            }
            if let Ok(value) = &mut result {
                value["catalog_metrics"]["queue_duration_ms"] = json!(queue_ms);
            }
            // Remove before publishing: requests beginning after this refresh
            // must read external changes, even if an old waiter is still alive.
            {
                let mut flights = self.flights.lock();
                if flights
                    .get(&lease.key)
                    .is_some_and(|entry| entry.ptr_eq(&Arc::downgrade(&lease.flight)))
                {
                    flights.remove(&lease.key);
                }
            }
            // Most requests have no followers. Avoid cloning a potentially large
            // serialized catalog just to publish it to an unused channel.
            if lease.flight.result.receiver_count() > 1 || Arc::strong_count(&lease.flight) > 1 {
                let shared = result
                    .as_ref()
                    .map(|value| Arc::new(value.clone()))
                    .map_err(ToString::to_string);
                lease.flight.result.send_replace(Some(shared));
            }
            tracing::debug!(
                elapsed_ms = elapsed_ms(requested),
                ok = result.is_ok(),
                "catalog refresh finished"
            );
            lease.completed = true;
            result
        } else {
            loop {
                if let Some(result) = receiver.borrow_and_update().clone() {
                    break result
                        .map(|value| (*value).clone())
                        .map_err(StateError::Storage);
                }
                receiver.changed().await.map_err(|_| {
                    StateError::Storage("catalog refresh interrupted; retry the request".into())
                })?;
            }
        };
        result.map(|mut value| {
            value["catalog_freshness"]["coalesced"] = json!(!leader);
            value["catalog_metrics"]["request_duration_ms"] = json!(elapsed_ms(requested));
            value
        })
    }
}

fn elapsed_ms(started: Instant) -> u64 {
    started.elapsed().as_millis().min(u64::MAX as u128) as u64
}

async fn timed<T>(future: impl Future<Output = Result<T>>) -> Result<(T, u64)> {
    let started = Instant::now();
    future.await.map(|value| (value, elapsed_ms(started)))
}

pub(crate) async fn load(
    agent: &StateAgent,
    session: &EphemeralRcloneSession,
    master: &MasterKey,
) -> Result<Value> {
    let key = crate::storage_scope::binding(
        Some(crate::storage_scope::bound_session_identity(
            session, master,
        )),
        master,
    )
    .expect("bound session has an identity");
    agent
        .catalog
        .refresh(key, || async {
            let started = Instant::now();
            let (config, _guard) = noland_storage::write_guarded_ephemeral_session(
                &agent.config.paths.run_root,
                session,
            )?;
            let provider = noland_storage::RcloneStorage::try_from_session(session, &config)?;
            let storage = SessionStorage::new(provider, agent, session, master);
            let setup_ms = elapsed_ms(started);
            load_from_provider(&storage, master, setup_ms)
                .await
                .map(|mut value| {
                    value["catalog_metrics"]["metadata"] = storage.metadata_metrics();
                    value
                })
        })
        .await
}

async fn load_from_provider(
    provider: &dyn SharedStorageProvider,
    master: &MasterKey,
    setup_ms: u64,
) -> Result<Value> {
    let started = Instant::now();
    let started_at = Utc::now();
    let before = provider.operation_metrics();
    let metadata_before = provider.metadata_read_metrics();
    // Exactly two bounded branches, sharing one provider/connection budget.
    let ((catalog, catalog_ms), (pending, pending_ms)) = tokio::try_join!(
        timed(noland_storage::load_catalog(provider, master)),
        timed(noland_storage::pending_deletions(provider, master)),
    )?;
    let mut value = serde_json::to_value(catalog)?;
    value["pending_deletions"] = json!(pending
        .iter()
        .map(|plan| json!({
            "plan_id": plan.plan_id,
            "names": plan.targets.iter().map(|target| &target.name).collect::<Vec<_>>(),
        }))
        .collect::<Vec<_>>());
    value["catalog_freshness"] = json!({
        "source": "live_remote_validation",
        "refresh_started_at": started_at,
        "observed_at": Utc::now(),
        "authoritative_for_execution": false,
    });
    value["catalog_metrics"] = json!({
        "storage_setup_duration_ms": setup_ms,
        "load_catalog_duration_ms": catalog_ms,
        "pending_deletions_duration_ms": pending_ms,
        "refresh_duration_ms": elapsed_ms(started),
        "storage": provider.operation_metrics().saturating_sub(before),
        "metadata_provider_io": provider.metadata_read_metrics().saturating_sub(metadata_before),
    });
    Ok(value)
}

pub(crate) fn invalidate(
    agent: &StateAgent,
    identity: Option<ProviderRootIdentity>,
    master: &MasterKey,
) {
    agent.catalog.invalidate();
    if let Some(binding) = crate::storage_scope::binding(identity, master) {
        if let Err(error) = agent.db.invalidate_cached_metadata(&binding) {
            tracing::warn!(%error, "could not invalidate disposable catalog metadata cache");
        }
    }
}

#[cfg(test)]
mod tests;
