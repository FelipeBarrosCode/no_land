use std::collections::HashSet;
use std::future::Future;
use std::sync::Arc;

use parking_lot::Mutex;
use uuid::Uuid;

#[derive(Clone, Default)]
pub struct OperationManager {
    running: Arc<Mutex<HashSet<Uuid>>>,
    execution_gate: Arc<tokio::sync::Mutex<()>>,
}

impl OperationManager {
    pub fn spawn<F, C>(&self, operation_id: Uuid, future: F, on_terminated: C) -> bool
    where
        F: Future<Output = ()> + Send + 'static,
        C: FnOnce(String) + Send + 'static,
    {
        if !self.running.lock().insert(operation_id) {
            return false;
        }

        let running = Arc::clone(&self.running);
        let execution_gate = Arc::clone(&self.execution_gate);
        tokio::spawn(async move {
            let _guard = RunningGuard {
                operation_id,
                running,
            };
            let worker = tokio::spawn(async move {
                let _permit = execution_gate.lock().await;
                future.await;
            });
            if let Err(error) = worker.await {
                on_terminated(format!(
                    "background operation terminated unexpectedly: {error}"
                ));
            }
        });
        true
    }

    pub fn is_running(&self, operation_id: Uuid) -> bool {
        self.running.lock().contains(&operation_id)
    }

    pub async fn run_exclusive<F, T>(&self, future: F) -> T
    where
        F: Future<Output = T>,
    {
        let _permit = self.execution_gate.lock().await;
        future.await
    }
}

struct RunningGuard {
    operation_id: Uuid,
    running: Arc<Mutex<HashSet<Uuid>>>,
}

impl Drop for RunningGuard {
    fn drop(&mut self) {
        self.running.lock().remove(&self.operation_id);
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
