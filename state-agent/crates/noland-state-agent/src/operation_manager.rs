use std::collections::{HashMap, VecDeque};
use std::future::Future;
use std::sync::Arc;
use std::time::Instant;

use parking_lot::Mutex;
use serde_json::Value;
use tokio::sync::watch;
use uuid::Uuid;

const MAX_RETRY_DESCRIPTORS: usize = 1_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CancelOutcome {
    Requested,
    AlreadyRequested,
    NotRunning,
}

#[derive(Debug, Clone)]
pub struct ManagedOperationSnapshot {
    pub operation_id: Uuid,
    pub cancel_requested: bool,
    pub queued_for_ms: u64,
    pub running_for_ms: Option<u64>,
}

#[derive(Debug, Clone)]
pub struct RetryDescriptor {
    pub method: String,
    pub params: Value,
}

#[derive(Clone, Default)]
pub struct OperationManager {
    state: Arc<Mutex<ManagerState>>,
    execution_gate: Arc<tokio::sync::Mutex<()>>,
}

#[derive(Default)]
struct ManagerState {
    running: HashMap<Uuid, RunningOperation>,
    retry_descriptors: HashMap<Uuid, RetryDescriptor>,
    retry_order: VecDeque<Uuid>,
}

struct RunningOperation {
    cancel: watch::Sender<bool>,
    cancel_requested: bool,
    queued_at: Instant,
    started_at: Option<Instant>,
}

impl OperationManager {
    pub fn spawn<F, C>(&self, operation_id: Uuid, future: F, on_terminated: C) -> bool
    where
        F: Future<Output = ()> + Send + 'static,
        C: FnOnce(String) + Send + 'static,
    {
        self.spawn_cancellable(operation_id, future, || {}, on_terminated)
    }

    pub fn spawn_cancellable<F, K, C>(
        &self,
        operation_id: Uuid,
        future: F,
        on_cancelled: K,
        on_terminated: C,
    ) -> bool
    where
        F: Future<Output = ()> + Send + 'static,
        K: FnOnce() + Send + 'static,
        C: FnOnce(String) + Send + 'static,
    {
        let (cancel, mut cancellation) = watch::channel(false);
        {
            let mut state = self.state.lock();
            if state.running.contains_key(&operation_id) {
                return false;
            }
            state.running.insert(
                operation_id,
                RunningOperation {
                    cancel,
                    cancel_requested: false,
                    queued_at: Instant::now(),
                    started_at: None,
                },
            );
        }

        let state = Arc::clone(&self.state);
        let execution_gate = Arc::clone(&self.execution_gate);
        tokio::spawn(async move {
            let _guard = RunningGuard {
                operation_id,
                state: Arc::clone(&state),
            };

            let permit = tokio::select! {
                biased;
                _ = wait_for_cancellation(&mut cancellation) => {
                    on_cancelled();
                    return;
                }
                permit = Arc::clone(&execution_gate).lock_owned() => permit,
            };
            if let Some(operation) = state.lock().running.get_mut(&operation_id) {
                operation.started_at = Some(Instant::now());
            }

            let worker = tokio::spawn(async move {
                let _permit = permit;
                tokio::select! {
                    biased;
                    _ = wait_for_cancellation(&mut cancellation) => WorkerOutcome::Cancelled,
                    _ = future => WorkerOutcome::Completed,
                }
            });
            match worker.await {
                Ok(WorkerOutcome::Completed) => {}
                Ok(WorkerOutcome::Cancelled) => on_cancelled(),
                Err(error) => on_terminated(format!(
                    "background operation terminated unexpectedly: {error}"
                )),
            }
        });
        true
    }

    pub fn cancel(&self, operation_id: Uuid) -> CancelOutcome {
        let mut state = self.state.lock();
        let Some(operation) = state.running.get_mut(&operation_id) else {
            return CancelOutcome::NotRunning;
        };
        if operation.cancel_requested {
            return CancelOutcome::AlreadyRequested;
        }
        if operation.cancel.send(true).is_err() {
            return CancelOutcome::NotRunning;
        }
        operation.cancel_requested = true;
        CancelOutcome::Requested
    }

    pub fn is_running(&self, operation_id: Uuid) -> bool {
        self.state.lock().running.contains_key(&operation_id)
    }

    pub fn cancel_requested(&self, operation_id: Uuid) -> bool {
        self.state
            .lock()
            .running
            .get(&operation_id)
            .is_some_and(|operation| operation.cancel_requested)
    }

    pub fn running_snapshots(&self) -> Vec<ManagedOperationSnapshot> {
        let now = Instant::now();
        self.state
            .lock()
            .running
            .iter()
            .map(|(operation_id, operation)| ManagedOperationSnapshot {
                operation_id: *operation_id,
                cancel_requested: operation.cancel_requested,
                queued_for_ms: elapsed_ms(operation.queued_at, now),
                running_for_ms: operation.started_at.map(|started| elapsed_ms(started, now)),
            })
            .collect()
    }

    pub fn remember_retry(&self, operation_id: Uuid, method: &str, params: &Value) {
        let mut state = self.state.lock();
        if !state.retry_descriptors.contains_key(&operation_id) {
            state.retry_order.push_back(operation_id);
        }
        state.retry_descriptors.insert(
            operation_id,
            RetryDescriptor {
                method: method.to_string(),
                params: params.clone(),
            },
        );
        while state.retry_order.len() > MAX_RETRY_DESCRIPTORS {
            if let Some(expired) = state.retry_order.pop_front() {
                state.retry_descriptors.remove(&expired);
            }
        }
    }

    pub fn retry_descriptor(&self, operation_id: Uuid) -> Option<RetryDescriptor> {
        self.state
            .lock()
            .retry_descriptors
            .get(&operation_id)
            .cloned()
    }

    pub async fn run_exclusive<F, T>(&self, future: F) -> T
    where
        F: Future<Output = T>,
    {
        let _permit = self.execution_gate.lock().await;
        future.await
    }
}

async fn wait_for_cancellation(cancellation: &mut watch::Receiver<bool>) {
    if *cancellation.borrow() {
        return;
    }
    while cancellation.changed().await.is_ok() {
        if *cancellation.borrow() {
            return;
        }
    }
}

fn elapsed_ms(started: Instant, now: Instant) -> u64 {
    now.duration_since(started)
        .as_millis()
        .min(u64::MAX as u128) as u64
}

enum WorkerOutcome {
    Completed,
    Cancelled,
}

struct RunningGuard {
    operation_id: Uuid,
    state: Arc<Mutex<ManagerState>>,
}

impl Drop for RunningGuard {
    fn drop(&mut self) {
        self.state.lock().running.remove(&self.operation_id);
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use tokio::sync::oneshot;

    use super::*;

    #[tokio::test]
    async fn serializes_catalog_mutating_operations() {
        let manager = OperationManager::default();
        let first_id = Uuid::new_v4();
        let second_id = Uuid::new_v4();
        let (release_first_tx, release_first_rx) = oneshot::channel();
        let (second_started_tx, mut second_started_rx) = oneshot::channel();

        assert!(manager.spawn(
            first_id,
            async move {
                let _ = release_first_rx.await;
            },
            |_| {},
        ));
        assert!(manager.spawn(
            second_id,
            async move {
                let _ = second_started_tx.send(());
            },
            |_| {},
        ));

        assert!(
            tokio::time::timeout(Duration::from_millis(50), &mut second_started_rx)
                .await
                .is_err()
        );
        release_first_tx.send(()).unwrap();
        tokio::time::timeout(Duration::from_secs(1), second_started_rx)
            .await
            .unwrap()
            .unwrap();
    }

    #[tokio::test]
    async fn reports_panicked_background_jobs() {
        let manager = OperationManager::default();
        let operation_id = Uuid::new_v4();
        let (terminated_tx, terminated_rx) = oneshot::channel();

        assert!(manager.spawn(
            operation_id,
            async move { panic!("worker panic") },
            move |error| {
                let _ = terminated_tx.send(error);
            },
        ));

        let error = tokio::time::timeout(Duration::from_secs(1), terminated_rx)
            .await
            .unwrap()
            .unwrap();
        assert!(error.contains("terminated unexpectedly"));
        while manager.is_running(operation_id) {
            tokio::task::yield_now().await;
        }
    }

    #[tokio::test]
    async fn cancellation_is_acknowledged_only_for_managed_jobs() {
        let manager = OperationManager::default();
        let operation_id = Uuid::new_v4();
        let (started_tx, started_rx) = oneshot::channel();
        let (cancelled_tx, cancelled_rx) = oneshot::channel();

        assert_eq!(manager.cancel(operation_id), CancelOutcome::NotRunning);
        assert!(manager.spawn_cancellable(
            operation_id,
            async move {
                let _ = started_tx.send(());
                std::future::pending::<()>().await;
            },
            move || {
                let _ = cancelled_tx.send(());
            },
            |_| {},
        ));
        tokio::time::timeout(Duration::from_secs(1), started_rx)
            .await
            .unwrap()
            .unwrap();

        assert_eq!(manager.cancel(operation_id), CancelOutcome::Requested);
        assert_eq!(
            manager.cancel(operation_id),
            CancelOutcome::AlreadyRequested
        );
        tokio::time::timeout(Duration::from_secs(1), cancelled_rx)
            .await
            .unwrap()
            .unwrap();
        while manager.is_running(operation_id) {
            tokio::task::yield_now().await;
        }
        assert_eq!(manager.cancel(operation_id), CancelOutcome::NotRunning);
    }

    #[test]
    fn retains_opaque_retry_context_without_exposing_it_as_runtime_state() {
        let manager = OperationManager::default();
        let operation_id = Uuid::new_v4();
        let params = serde_json::json!({"provider_specific": {"token": "memory-only"}});

        manager.remember_retry(operation_id, "StartBackup", &params);
        let descriptor = manager.retry_descriptor(operation_id).unwrap();

        assert_eq!(descriptor.method, "StartBackup");
        assert_eq!(descriptor.params, params);
        assert!(manager.running_snapshots().is_empty());
    }

    #[tokio::test]
    async fn tracks_operation_until_background_job_finishes() {
        let manager = OperationManager::default();
        let operation_id = Uuid::new_v4();
        let (release_tx, release_rx) = oneshot::channel();

        assert!(manager.spawn(
            operation_id,
            async move {
                let _ = release_rx.await;
            },
            |_| {},
        ));
        assert!(manager.is_running(operation_id));
        assert!(!manager.spawn(operation_id, async {}, |_| {}));

        release_tx.send(()).unwrap();
        tokio::time::timeout(Duration::from_secs(1), async {
            while manager.is_running(operation_id) {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
    }
}
